//! Metadata catalog for the aggregation query planner.
//!
//! Provides two things:
//! 1. **Validator allow-list** — which collections (logical entities) and fields the
//!    planner is permitted to reference. Any name not in this catalog causes
//!    `validate` to return `AppError::BadRequest`, blocking misconfigured model output
//!    and injection attempts.
//! 2. **Planner grounding context** — a compact summary of schema and code vocabularies
//!    inserted into the planner system prompt so the model emits real field names.
//!
//! # Dynamic catalog
//!
//! The catalog is built at boot from ingested data via `build_from_store` and stored
//! in `AppState` behind `Arc<RwLock<Arc<Catalog>>>`. It is rebuilt whenever ingest
//! reaches a terminal status or records are deleted, without requiring a server restart.
//!
//! Fields are discovered by sampling up to 5 docs per table from the `records`
//! collection and inferring BSON type. A small hand-maintained augmentation layer
//! (`augmentation_layer`) adds code_vocab and synonyms that schema sampling cannot
//! express.

use std::collections::HashMap;

use crate::ontology::concepts::EntityConcept;
use crate::ontology::service_line::ServiceLine;

// ---------------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------------

/// Coarse type of a field, used by the planner prompt to hint the model about
/// which operators are appropriate (numeric vs. string vs. date).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FieldType {
    /// Free-text or categorical string.
    String,
    /// Numeric (integer or float); supports Sum/Avg/Min/Max.
    Numeric,
    /// ISO-8601 date or BSON Date; supports time bucketing.
    Date,
    /// Opaque identifier (UUID, etc.); supports equality + distinct count.
    Id,
}

impl FieldType {
    pub fn label(self) -> &'static str {
        match self {
            FieldType::String => "string",
            FieldType::Numeric => "numeric",
            FieldType::Date => "date",
            FieldType::Id => "id",
        }
    }
}

/// One field in the catalog.
#[derive(Debug, Clone)]
pub struct FieldMeta {
    pub name: String,
    pub field_type: FieldType,
    /// Optional human-readable description for the planner prompt.
    /// Dynamic fields carry no static description; `None` omits the annotation.
    pub description: Option<String>,
}

#[cfg(test)]
impl FieldMeta {
    // Only used by `build_hardcoded()` below (the real catalog builder constructs
    // `FieldMeta` via struct literal directly) — `cfg(test)` so a production build
    // doesn't carry test-fixture-only constructors.
    #[cfg(test)]
    pub fn new(name: impl Into<String>, t: FieldType) -> Self {
        FieldMeta {
            name: name.into(),
            field_type: t,
            description: None,
        }
    }
    #[cfg(test)]
    pub fn with_desc(name: impl Into<String>, t: FieldType, desc: impl Into<String>) -> Self {
        FieldMeta {
            name: name.into(),
            field_type: t,
            description: Some(desc.into()),
        }
    }
}

/// Metadata for one logical collection (entity type / physical table).
#[derive(Debug, Clone)]
pub struct CollectionMeta {
    /// Human-readable label shown in the planner prompt.
    pub label: String,
    /// All allow-listed field names for this collection.
    pub fields: Vec<FieldMeta>,
    /// Ontology concept bound to this collection by the schema binder, if any.
    /// `None` when the catalog was built without binding data (e.g. degraded mode
    /// or before the first binding run).
    pub concept: Option<EntityConcept>,
    /// Service lines that claim ownership of this collection via the bound concept.
    /// Empty when `concept` is `None` or binding has not been run.
    pub service_lines: Vec<ServiceLine>,
}

impl CollectionMeta {
    /// Return `true` if `name` is an allow-listed field for this collection.
    pub fn has_field(&self, name: &str) -> bool {
        self.fields.iter().any(|f| f.name == name)
    }
}

/// The metadata catalog: collections, code vocabularies, and synonyms.
#[derive(Debug)]
pub struct Catalog {
    /// Map from collection name (physical table) to metadata. Only names in this
    /// map are accepted by `validate`; all others are rejected as `BadRequest`.
    pub collections: HashMap<String, CollectionMeta>,

    /// Natural-language disease/condition terms to ICD-10 code prefixes.
    /// Used by the planner prompt to anchor code-based filters.
    pub code_vocab: HashMap<String, String>,

    /// Field name synonyms to canonical name. Lets the model use common aliases
    /// without generating an unknown-field error. `validate` resolves synonyms
    /// before the allow-list check.
    pub synonyms: HashMap<String, String>,
}

impl Catalog {
    /// Return an empty catalog. The validator rejects all collection names against
    /// an empty catalog, so this fails closed rather than panicking on boot before
    /// the first successful DB-backed build.
    pub fn empty() -> Self {
        Catalog {
            collections: HashMap::new(),
            code_vocab: HashMap::new(),
            synonyms: HashMap::new(),
        }
    }

    /// Narrow the catalog to a service line's read allow-list (plan 05 §3, hook 2).
    ///
    /// The returned catalog is used for **both** the planner prompt and
    /// validation, so a model that hallucinates an out-of-scope collection is
    /// rejected by the same `validate` guard that already rejects unknown ones —
    /// no second allow-list, no second decision point.
    ///
    /// An empty `scope` means "no narrowing" (the Ask agent) and returns a full
    /// clone. A scope that matches nothing returns an empty catalog, which fails
    /// closed: every collection name the planner could emit is rejected.
    ///
    /// This is a *read* allow-list. It says what an agent may look at; it says
    /// nothing about what a user is permitted to do (see `src/auth/`).
    pub fn scoped(&self, scope: &[String]) -> Catalog {
        let collections = if scope.is_empty() {
            self.collections.clone()
        } else {
            self.collections
                .iter()
                .filter(|(name, _)| scope.iter().any(|t| t == *name))
                .map(|(name, meta)| (name.clone(), meta.clone()))
                .collect()
        };
        Catalog {
            collections,
            code_vocab: self.code_vocab.clone(),
            synonyms: self.synonyms.clone(),
        }
    }

    /// Return `true` if `collection` is allow-listed.
    pub fn has_collection(&self, collection: &str) -> bool {
        self.collections.contains_key(collection)
    }

    /// Return `true` if `field` is allow-listed for `collection`.
    pub fn has_field(&self, collection: &str, field: &str) -> bool {
        self.collections
            .get(collection)
            .map_or(false, |c| c.has_field(field))
    }

    /// Resolve a field name through the synonym map; returns owned canonical name.
    pub fn resolve_synonym_owned(&self, name: &str) -> String {
        self.synonyms
            .get(name)
            .cloned()
            .unwrap_or_else(|| name.to_string())
    }

    /// Build a compact schema summary string for the planner system prompt.
    pub fn planner_context(&self) -> String {
        let mut out = String::from("## Available collections and fields\n");
        let mut coll_names: Vec<&String> = self.collections.keys().collect();
        coll_names.sort();
        for name in coll_names {
            let meta = &self.collections[name];
            out.push_str(&format!("\n**{}** — {}\n", name, meta.label));
            for f in &meta.fields {
                let desc = f
                    .description
                    .as_deref()
                    .map(|d| format!(" — {d}"))
                    .unwrap_or_default();
                out.push_str(&format!(
                    "  - `{}` ({}){}\n",
                    f.name,
                    f.field_type.label(),
                    desc
                ));
            }
        }
        if !self.code_vocab.is_empty() {
            out.push_str("\n## Code vocabulary (natural language to ICD-10 prefix)\n");
            let mut terms: Vec<(&String, &String)> = self.code_vocab.iter().collect();
            terms.sort_by_key(|(k, _)| k.as_str());
            for (term, prefix) in terms {
                out.push_str(&format!("  - \"{term}\" -> prefix `{prefix}`\n"));
            }
        }
        if !self.synonyms.is_empty() {
            out.push_str("\n## Field synonyms (alias to canonical name)\n");
            let mut syns: Vec<(&String, &String)> = self.synonyms.iter().collect();
            syns.sort_by_key(|(k, _)| k.as_str());
            for (alias, canonical) in syns {
                out.push_str(&format!("  - `{alias}` -> `{canonical}`\n"));
            }
        }
        out
    }
}

// ---------------------------------------------------------------------------
// Dynamic build from DocumentDB
// ---------------------------------------------------------------------------

/// Build the catalog from the ingested data in DocumentDB.
///
/// Tables are discovered from `indexed_tables` (status = "indexed"), falling back
/// to distinct `table` values on the `records` collection if that index is empty.
/// For each table, up to 5 records are sampled to union column names and infer types.
/// The augmentation layer (code vocab + synonyms) is merged in from static knowledge.
///
/// Any DB error returns an empty catalog (fail closed, no panic).
pub async fn build_from_store(db: &crate::documentdb::DocumentDb) -> Catalog {
    match build_from_store_inner(db).await {
        Ok(cat) => {
            tracing::info!(tables = cat.collections.len(), "catalog built from store");
            cat
        }
        Err(e) => {
            tracing::warn!(error = %e, "catalog build from store failed; using empty catalog");
            Catalog::empty()
        }
    }
}

async fn build_from_store_inner(
    db: &crate::documentdb::DocumentDb,
) -> crate::error::AppResult<Catalog> {
    use futures::TryStreamExt;
    use mongodb::bson::{Document, doc};

    // 1. Enumerate tables from indexed_tables (indexed status only).
    let mut table_names: Vec<String> = {
        let mut cursor = db
            .indexed_tables()
            .find(doc! { "status": "indexed" })
            .projection(doc! { "source_table": 1, "_id": 0 })
            .await?;
        let mut names = Vec::new();
        while let Some(d) = cursor.try_next().await? {
            if let Ok(name) = d.get_str("source_table") {
                if !name.is_empty() {
                    names.push(name.to_string());
                }
            }
        }
        names.sort();
        names.dedup();
        names
    };

    // Fall back to scanning distinct table values in records.
    if table_names.is_empty() {
        let pipeline: Vec<Document> = vec![
            doc! { "$match": { "active": true } },
            doc! { "$group": { "_id": "$table" } },
            doc! { "$sort": { "_id": 1 } },
        ];
        let mut cursor = db.records().aggregate(pipeline).await?;
        while let Some(d) = cursor.try_next().await? {
            if let Ok(name) = d.get_str("_id") {
                if !name.is_empty() {
                    table_names.push(name.to_string());
                }
            }
        }
    }

    let mut collections: HashMap<String, CollectionMeta> = HashMap::new();

    for table in &table_names {
        // Sample up to 5 docs per table to discover and type the fields.
        let mut cursor = db
            .records()
            .find(doc! { "table": table, "active": true })
            .projection(doc! { "fields": 1, "_id": 0 })
            .limit(5)
            .await?;

        let mut field_map: HashMap<String, FieldType> = HashMap::new();

        while let Some(d) = cursor.try_next().await? {
            if let Ok(fields_doc) = d.get_document("fields") {
                for (key, value) in fields_doc.iter() {
                    // Union keys; keep first inferred type (first non-null doc wins).
                    field_map
                        .entry(key.clone())
                        .or_insert_with(|| infer_field_type(key, value));
                }
            }
        }

        if field_map.is_empty() {
            continue; // no docs found for this table; skip
        }

        let mut fields: Vec<FieldMeta> = field_map
            .into_iter()
            .map(|(name, ft)| FieldMeta {
                name,
                field_type: ft,
                description: None,
            })
            .collect();
        // Stable sort so planner_context output is deterministic.
        fields.sort_by(|a, b| a.name.cmp(&b.name));

        collections.insert(
            table.clone(),
            CollectionMeta {
                label: format!("{table} records"),
                fields,
                concept: None,
                service_lines: Vec::new(),
            },
        );
    }

    let (code_vocab, synonyms) = augmentation_layer();
    Ok(Catalog {
        collections,
        code_vocab,
        synonyms,
    })
}

/// Infer a `FieldType` from a BSON value and key name.
///
/// - `id` / `*_id` keys are `Id` regardless of value type.
/// - Numeric BSON variants are `Numeric`.
/// - Strings matching the ISO-8601 date prefix (`YYYY-MM-DD`) are `Date`.
/// - BSON DateTime is `Date`.
/// - Everything else is `String`.
fn infer_field_type(key: &str, value: &mongodb::bson::Bson) -> FieldType {
    use mongodb::bson::Bson;

    // Id detection by key name.
    if key == "id" || key.ends_with("_id") {
        return FieldType::Id;
    }

    match value {
        Bson::Double(_) | Bson::Int32(_) | Bson::Int64(_) => FieldType::Numeric,
        Bson::String(s) => {
            if looks_like_date(s) {
                FieldType::Date
            } else {
                FieldType::String
            }
        }
        Bson::DateTime(_) => FieldType::Date,
        _ => FieldType::String,
    }
}

/// Conservative ISO-8601 date detection. Returns true only when the string starts
/// with `YYYY-MM-DD` — common for Postgres `to_jsonb` date serialisation.
fn looks_like_date(s: &str) -> bool {
    let b = s.as_bytes();
    if b.len() < 10 {
        return false;
    }
    b[0..4].iter().all(|c| c.is_ascii_digit())
        && b[4] == b'-'
        && b[5..7].iter().all(|c| c.is_ascii_digit())
        && b[7] == b'-'
        && b[8..10].iter().all(|c| c.is_ascii_digit())
}

// ---------------------------------------------------------------------------
// Augmentation layer (static domain knowledge)
// ---------------------------------------------------------------------------

/// Return the hand-maintained code vocabulary and field synonyms.
///
/// These encode domain knowledge that schema sampling cannot infer: ICD-10 code
/// mappings and common natural-language aliases for field names. They are merged
/// into every dynamically-built catalog.
fn augmentation_layer() -> (HashMap<String, String>, HashMap<String, String>) {
    let mut code_vocab: HashMap<String, String> = HashMap::new();
    code_vocab.insert("diabetes".into(), "E11".into());
    code_vocab.insert("type 2 diabetes".into(), "E11".into());
    code_vocab.insert("hypertension".into(), "I10".into());
    code_vocab.insert("high blood pressure".into(), "I10".into());
    code_vocab.insert("malaria".into(), "B50".into());
    code_vocab.insert("pneumonia".into(), "J18".into());
    code_vocab.insert("tuberculosis".into(), "A15".into());
    code_vocab.insert("tb".into(), "A15".into());
    code_vocab.insert("hiv".into(), "B20".into());
    code_vocab.insert("anemia".into(), "D64".into());
    code_vocab.insert("asthma".into(), "J45".into());
    code_vocab.insert("sepsis".into(), "A41".into());

    let mut synonyms: HashMap<String, String> = HashMap::new();
    // Common natural-language aliases for typical EMR column names.
    synonyms.insert("dob".into(), "date_of_birth".into());
    synonyms.insert("birthdate".into(), "date_of_birth".into());
    synonyms.insert("birth_date".into(), "date_of_birth".into());
    synonyms.insert("diag_code".into(), "icd10_code".into());
    synonyms.insert("icd_code".into(), "icd10_code".into());
    synonyms.insert("diagnosis_code".into(), "icd10_code".into());
    synonyms.insert("visit_date".into(), "encounter_date".into());
    synonyms.insert("appointment_date".into(), "encounter_date".into());
    synonyms.insert("facility".into(), "clinic".into());
    synonyms.insert("hospital".into(), "clinic".into());

    (code_vocab, synonyms)
}

// ---------------------------------------------------------------------------
// Hardcoded catalog (for unit tests and as a known-good fallback reference)
// ---------------------------------------------------------------------------

/// Build the catalog from the known AKUH EMR schema. Used in unit tests and as
/// the reference schema. Production code uses `build_from_store` instead.
#[cfg(test)]
pub(crate) fn build_hardcoded() -> Catalog {
    let mut collections: HashMap<String, CollectionMeta> = HashMap::new();

    collections.insert(
        "records".into(),
        CollectionMeta {
            label: "All ingested records from all EMR sources (master collection)".into(),
            fields: vec![
                FieldMeta::with_desc(
                    "source_id",
                    FieldType::Id,
                    "ID of the EMR source (connector)",
                ),
                FieldMeta::with_desc("patient_id", FieldType::Id, "Patient identifier"),
                FieldMeta::with_desc(
                    "encounter_id",
                    FieldType::Id,
                    "Clinical encounter identifier",
                ),
                FieldMeta::with_desc("clinic", FieldType::String, "Clinic or facility name"),
                FieldMeta::with_desc("diagnosis_code", FieldType::String, "ICD-10 diagnosis code"),
                FieldMeta::with_desc(
                    "diagnosis_display",
                    FieldType::String,
                    "Human-readable diagnosis name",
                ),
                FieldMeta::with_desc(
                    "encounter_date",
                    FieldType::Date,
                    "Date of the clinical encounter",
                ),
                FieldMeta::with_desc("gender", FieldType::String, "Patient gender"),
                FieldMeta::with_desc("dob", FieldType::Date, "Patient date of birth"),
                FieldMeta::new("name", FieldType::String),
                FieldMeta::new("phone", FieldType::String),
                FieldMeta::new("address", FieldType::String),
                FieldMeta::with_desc(
                    "registration_date",
                    FieldType::Date,
                    "Patient registration date",
                ),
                FieldMeta::with_desc("heart_rate", FieldType::Numeric, "Heart rate in bpm"),
                FieldMeta::with_desc(
                    "blood_pressure",
                    FieldType::String,
                    "Blood pressure reading",
                ),
                FieldMeta::with_desc("temperature", FieldType::Numeric, "Body temperature (C)"),
                FieldMeta::with_desc("weight", FieldType::Numeric, "Weight in kg"),
                FieldMeta::with_desc("height", FieldType::Numeric, "Height in cm"),
                FieldMeta::with_desc(
                    "recorded_at",
                    FieldType::Date,
                    "Timestamp of vital sign recording",
                ),
                FieldMeta::with_desc(
                    "rx_number",
                    FieldType::String,
                    "Prescription reference number",
                ),
                FieldMeta::with_desc(
                    "prescriber_id",
                    FieldType::Id,
                    "Prescribing provider identifier",
                ),
                FieldMeta::with_desc(
                    "issue_date",
                    FieldType::Date,
                    "Date prescription was issued",
                ),
                FieldMeta::with_desc("valid_until", FieldType::Date, "Prescription expiry date"),
                FieldMeta::with_desc("status", FieldType::String, "Record or prescription status"),
                FieldMeta::with_desc("drug_name", FieldType::String, "Name of prescribed drug"),
                FieldMeta::with_desc("dosage", FieldType::String, "Prescribed dosage"),
                FieldMeta::with_desc("test_name", FieldType::String, "Laboratory test name"),
                FieldMeta::with_desc("ordered_date", FieldType::Date, "Date lab order was placed"),
                FieldMeta::with_desc(
                    "result_value",
                    FieldType::Numeric,
                    "Numeric lab result value",
                ),
                FieldMeta::with_desc("result_unit", FieldType::String, "Unit for the lab result"),
            ],
            concept: None,
            service_lines: Vec::new(),
        },
    );

    collections.insert(
        "patients".into(),
        CollectionMeta {
            label: "Patient demographic records".into(),
            fields: vec![
                FieldMeta::new("patient_id", FieldType::Id),
                FieldMeta::with_desc("name", FieldType::String, "Full patient name"),
                FieldMeta::with_desc("dob", FieldType::Date, "Date of birth"),
                FieldMeta::with_desc("gender", FieldType::String, "Gender"),
                FieldMeta::new("phone", FieldType::String),
                FieldMeta::new("address", FieldType::String),
                FieldMeta::with_desc("registration_date", FieldType::Date, "Registration date"),
                FieldMeta::with_desc("clinic", FieldType::String, "Clinic or facility"),
            ],
            concept: None,
            service_lines: Vec::new(),
        },
    );

    collections.insert(
        "encounters".into(),
        CollectionMeta {
            label: "Clinical encounter records".into(),
            fields: vec![
                FieldMeta::new("encounter_id", FieldType::Id),
                FieldMeta::new("patient_id", FieldType::Id),
                FieldMeta::with_desc("encounter_date", FieldType::Date, "Date of encounter"),
                FieldMeta::with_desc("diagnosis_code", FieldType::String, "ICD-10 code"),
                FieldMeta::with_desc(
                    "diagnosis_display",
                    FieldType::String,
                    "Diagnosis description",
                ),
                FieldMeta::with_desc("clinic", FieldType::String, "Clinic or facility"),
            ],
            concept: None,
            service_lines: Vec::new(),
        },
    );

    collections.insert(
        "prescriptions".into(),
        CollectionMeta {
            label: "Prescription records".into(),
            fields: vec![
                FieldMeta::new("rx_number", FieldType::String),
                FieldMeta::new("encounter_id", FieldType::Id),
                FieldMeta::new("patient_id", FieldType::Id),
                FieldMeta::new("prescriber_id", FieldType::Id),
                FieldMeta::with_desc("issue_date", FieldType::Date, "Issue date"),
                FieldMeta::with_desc("valid_until", FieldType::Date, "Expiry date"),
                FieldMeta::with_desc("status", FieldType::String, "active | expired | cancelled"),
                FieldMeta::with_desc("drug_name", FieldType::String, "Drug name"),
                FieldMeta::with_desc("dosage", FieldType::String, "Dosage"),
            ],
            concept: None,
            service_lines: Vec::new(),
        },
    );

    collections.insert(
        "lab_orders".into(),
        CollectionMeta {
            label: "Laboratory order records".into(),
            fields: vec![
                FieldMeta::new("patient_id", FieldType::Id),
                FieldMeta::new("encounter_id", FieldType::Id),
                FieldMeta::with_desc("test_name", FieldType::String, "Test ordered"),
                FieldMeta::with_desc("ordered_date", FieldType::Date, "Order date"),
                FieldMeta::with_desc(
                    "status",
                    FieldType::String,
                    "pending | completed | cancelled",
                ),
                FieldMeta::with_desc("result_value", FieldType::Numeric, "Numeric result"),
                FieldMeta::with_desc("result_unit", FieldType::String, "Result unit"),
            ],
            concept: None,
            service_lines: Vec::new(),
        },
    );

    collections.insert(
        "vital_signs".into(),
        CollectionMeta {
            label: "Vital sign measurements".into(),
            fields: vec![
                FieldMeta::new("patient_id", FieldType::Id),
                FieldMeta::new("encounter_id", FieldType::Id),
                FieldMeta::with_desc("recorded_at", FieldType::Date, "Recording timestamp"),
                FieldMeta::with_desc("heart_rate", FieldType::Numeric, "bpm"),
                FieldMeta::with_desc("blood_pressure", FieldType::String, "sys/dia"),
                FieldMeta::with_desc("temperature", FieldType::Numeric, "C"),
                FieldMeta::with_desc("weight", FieldType::Numeric, "kg"),
                FieldMeta::with_desc("height", FieldType::Numeric, "cm"),
            ],
            concept: None,
            service_lines: Vec::new(),
        },
    );

    let (code_vocab, mut synonyms) = augmentation_layer();

    // Additional hardcoded synonyms that target the static schema field names.
    synonyms.insert("date_of_birth".into(), "dob".into());
    synonyms.insert("disease".into(), "diagnosis_display".into());
    synonyms.insert("condition".into(), "diagnosis_display".into());
    synonyms.insert("provider".into(), "prescriber_id".into());

    Catalog {
        collections,
        code_vocab,
        synonyms,
    }
}
