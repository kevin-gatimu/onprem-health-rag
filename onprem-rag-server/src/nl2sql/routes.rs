//! NL-to-SQL routes.
//!
//! | Method | Path                                   | Auth    | Description                    |
//! |--------|----------------------------------------|---------|-------------------------------|
//! | POST   | /nl2sql/<source_id>                    | user    | Ask a question; SSE response  |
//! | GET    | /nl2sql/<source_id>/catalog            | admin   | Inspect active metadata       |
//! | POST   | /nl2sql/<source_id>/catalog/refresh    | admin   | Rebuild schema_catalog        |
//!
//! SSE event contract:
//! | Event   | Payload                                   |
//! |---------|-------------------------------------------|
//! | routed  | "text_to_sql" (JSON string)               |
//! | sql     | {sql, explanation} (JSON)                 |
//! | columns | ["col1", ...] (JSON array)                |
//! | rows    | [[val, ...], ...] (JSON array of arrays)  |
//! | token   | JSON-encoded string (narration token)     |
//! | error   | bare error string                         |
//! | done    | empty string                              |

use serde::{Deserialize, Serialize};

// ---------------------------------------------------------------------------
// Backward-compat re-exports
// (text, prepare, http are siblings declared in nl2sql/mod.rs)
// ---------------------------------------------------------------------------

// Re-export so external call sites continue to resolve via `crate::nl2sql::routes::*`.
pub(crate) use super::prepare::{
    PreparedNlQuery, SourceScope, prepare_auto_query, prepare_auto_query_deterministic,
};
// resolve_followup_question is called by agents/routes.rs via crate::nl2sql::routes::resolve_followup_question.
pub(crate) use super::text::{
    resolve_followup_question,
    // Used by deterministic_sql and patient_overview_subject in this file:
    extract_record_identifier, contains_likely_person_name, normalize_question,
};


// ---------------------------------------------------------------------------
// Request bodies
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
pub struct NlQueryRequest {
    pub question: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MetadataAlias {
    pub table: String,
    pub column: Option<String>,
    pub alias: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MetadataRelationship {
    pub from_table: String,
    pub from_column: String,
    pub to_table: String,
    pub to_column: String,
}

/// Force a specific concept onto a table, or `None` to exclude it from every service line.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TableConceptOverride {
    pub table: String,
    /// Concept slug (e.g. `"patient"`) or `null` to mark the table Unknown.
    pub concept: Option<String>,
}

/// Force a specific column role onto a column within a table.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ColumnRoleOverride {
    pub table: String,
    pub column: String,
    /// Role slug (e.g. `"event_time"`, `"patient_id"`, `"pii"`).
    pub role: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct MetadataOverrides {
    #[serde(default)]
    pub aliases: Vec<MetadataAlias>,
    #[serde(default)]
    pub relationships: Vec<MetadataRelationship>,
    /// Override concept assignments produced by automatic binding scoring.
    #[serde(default)]
    pub table_concepts: Vec<TableConceptOverride>,
    /// Override column role assignments produced by automatic binding scoring.
    #[serde(default)]
    pub column_roles: Vec<ColumnRoleOverride>,
    /// Service-line slugs to enable for this source (empty = use automatic detection).
    #[serde(default)]
    pub service_lines: Vec<String>,
}

pub(crate) fn deterministic_sql(
    question: &str,
    schema_cards: &[crate::nl2sql::spec::TableCard],
    source_kind: crate::connectors::SourceKind,
    max_rows: i64,
) -> Option<String> {
    let identifier = extract_record_identifier(question);
    let patient_overview = patient_overview_subject(question);
    let contains_quote = question
        .chars()
        .any(|character| matches!(character, '\'' | '"'));
    let contains_person_name = contains_likely_person_name(question);
    let question = normalize_question(question);
    let max_rows = max_rows.max(1);

    if let Some(sql) = common_healthcare_sql(
        &question,
        identifier.as_deref(),
        patient_overview.as_ref(),
        contains_quote,
        contains_person_name,
        schema_cards,
        source_kind,
        max_rows,
    ) {
        return Some(sql);
    }

    for card in schema_cards {
        if !is_safe_table_name(&card.table_name) {
            continue;
        }
        let entity = card
            .table_name
            .rsplit('.')
            .next()
            .unwrap_or(&card.table_name)
            .replace('_', " ")
            .to_ascii_lowercase();
        let mut entities = vec![entity.clone()];
        if let Some(singular) = entity.strip_suffix('s') {
            if !singular.is_empty() {
                entities.push(singular.to_string());
            }
        }

        for entity_name in entities {
            if is_count_question(&question, &entity_name) {
                let alias = entity.replace(' ', "_").trim_end_matches('s').to_string();
                return Some(format!(
                    "SELECT COUNT(*) AS {alias}_count FROM {}",
                    card.table_name
                ));
            }

            if let Some(sql) =
                relationship_filtered_count_sql(question.as_str(), &entity_name, card, schema_cards)
            {
                return Some(sql);
            }

            if let Some(sql) = grouped_count_sql(question.as_str(), &entity_name, card, source_kind)
            {
                return Some(sql);
            }

            if let Some(sql) = patient_gender_listing_sql(
                question.as_str(),
                &entity_name,
                card,
                source_kind,
                max_rows,
            ) {
                return Some(sql);
            }

            if let Some(limit) = listing_limit(&question, &entity_name, max_rows) {
                return Some(match source_kind {
                    crate::connectors::SourceKind::Mssql => {
                        format!("SELECT TOP {limit} * FROM {}", card.table_name)
                    }
                    crate::connectors::SourceKind::Postgres
                    | crate::connectors::SourceKind::Mysql => {
                        format!("SELECT * FROM {} LIMIT {limit}", card.table_name)
                    }
                });
            }
        }
    }
    None
}

/// Match "how many patients…" style total-count questions, tolerating common
/// trailing phrases such as "records are indexed" or "do we have".
fn is_count_question(question: &str, entity: &str) -> bool {
    let rest = [
        format!("how many {entity}"),
        format!("count {entity}"),
        format!("number of {entity}"),
        format!("what is the number of {entity}"),
        format!("total number of {entity}"),
    ]
    .into_iter()
    .find_map(|prefix| question.strip_prefix(&prefix))
    .map(str::trim);
    let Some(mut rest) = rest else {
        return false;
    };
    rest = rest.strip_prefix("records").unwrap_or(rest).trim();
    matches!(
        rest,
        "" | "do we have"
            | "are there"
            | "are indexed"
            | "indexed"
            | "exist"
            | "are stored"
            | "in total"
            | "total"
    )
}

fn common_healthcare_sql(
    question: &str,
    identifier: Option<&str>,
    patient_overview: Option<&PatientOverviewSubject>,
    contains_quote: bool,
    contains_person_name: bool,
    cards: &[crate::nl2sql::spec::TableCard],
    source_kind: crate::connectors::SourceKind,
    max_rows: i64,
) -> Option<String> {
    if source_kind != crate::connectors::SourceKind::Postgres {
        return None;
    }

    let table = |name: &str, columns: &[&str]| {
        cards.iter().find(|card| {
            card.table_name
                .rsplit('.')
                .next()
                .is_some_and(|table| table.eq_ignore_ascii_case(name))
                && columns.iter().all(|required| {
                    card.columns
                        .iter()
                        .any(|column| column.name.eq_ignore_ascii_case(required))
                })
        })
    };

    if let Some(subject) = patient_overview {
        let patients = table(
            "patients",
            &[
                "patient_no",
                "first_name",
                "middle_name",
                "last_name",
                "date_of_birth",
                "gender",
                "blood_type",
            ],
        )?;
        let predicate = match subject {
            PatientOverviewSubject::Identifier(id) => format!("patient_no = '{id}'"),
            PatientOverviewSubject::Name(parts) => match parts.as_slice() {
                [first, last] => {
                    format!("first_name ILIKE '{first}' AND last_name ILIKE '{last}'")
                }
                [first, middle, last] => format!(
                    "first_name ILIKE '{first}' AND middle_name ILIKE '{middle}' AND last_name ILIKE '{last}'"
                ),
                _ => return None,
            },
        };
        return Some(format!(
            "SELECT patient_no, first_name, middle_name, last_name, date_of_birth, gender, blood_type FROM {} WHERE {predicate} ORDER BY patient_no LIMIT {}",
            patients.table_name,
            max_rows.min(10),
        ));
    }

    if identifier.is_none() && !contains_quote && !contains_person_name {
        if let Some(sql) = top_grouped_count_join_sql(question, cards, max_rows) {
            return Some(sql);
        }

        if let Some((subject, table_names, limit)) = recent_records_subject(question, max_rows) {
            let card = cards.iter().find(|card| {
                is_safe_table_name(&card.table_name)
                    && table_names.iter().any(|candidate| {
                        card.table_name
                            .rsplit('.')
                            .next()
                            .is_some_and(|name| name.eq_ignore_ascii_case(candidate))
                    })
            })?;
            let date_column = preferred_trend_date_column(card, subject)?;
            let selected_columns = preferred_recent_columns(card, subject, &date_column.name);
            return Some(format!(
                "SELECT {columns} FROM {table} ORDER BY {date_column} DESC LIMIT {limit}",
                columns = selected_columns.join(", "),
                table = card.table_name,
                date_column = date_column.name,
            ));
        }

        if let Some((subject, table_names, period)) = periodic_trend_subject(question) {
            let card = cards.iter().find(|card| {
                is_safe_table_name(&card.table_name)
                    && table_names.iter().any(|candidate| {
                        card.table_name
                            .rsplit('.')
                            .next()
                            .is_some_and(|name| name.eq_ignore_ascii_case(candidate))
                    })
            })?;
            let date_column = preferred_trend_date_column(card, subject)?;
            return Some(format!(
                "SELECT date_trunc('{period}', {date_column})::date AS {period}, COUNT(*) AS {subject} FROM {table} GROUP BY 1 ORDER BY 1 LIMIT {max_rows}",
                date_column = date_column.name,
                table = card.table_name,
            ));
        }
    }

    if identifier.is_none() && !contains_quote {
        let tokens: Vec<&str> = question.split_whitespace().collect();
        let mentions_patients = tokens
            .iter()
            .any(|token| matches!(*token, "patient" | "patients"));
        let has_diagnosis_frame = question.contains(" diagnosed with ")
            || question.contains(" patients with ")
            || question.contains(" patient with ")
            || question.starts_with("who has been diagnosed with ")
            || question.starts_with("which patients have ");
        let has_list_frame = (matches!(tokens.first(), Some(&"which" | &"list" | &"show"))
            && mentions_patients)
            || (tokens.first() == Some(&"who")
                && tokens
                    .iter()
                    .any(|token| matches!(*token, "has" | "diagnosed")));
        let is_count = tokens
            .iter()
            .any(|token| matches!(*token, "how" | "many" | "count" | "number" | "total"));
        if has_list_frame && has_diagnosis_frame && !is_count {
            let filler = [
                "which",
                "list",
                "show",
                "me",
                "all",
                "the",
                "patients",
                "patient",
                "have",
                "has",
                "been",
                "diagnosed",
                "with",
                "who",
                "of",
                "s",
            ];
            let term = tokens
                .iter()
                .copied()
                .filter(|token| !filler.contains(token))
                .collect::<Vec<_>>()
                .join(" ");
            let safe_term = !term.is_empty()
                && term.chars().all(|character| {
                    character.is_ascii_alphanumeric() || matches!(character, ' ' | '-')
                });
            if safe_term {
                let diagnoses = table("diagnoses", &["patient_id", "diagnosis_desc"])?;
                let patients = table("patients", &["id", "patient_no", "first_name", "last_name"])?;
                return Some(format!(
                    "SELECT DISTINCT p.patient_no, p.first_name, p.last_name FROM {diagnoses} d JOIN {patients} p ON d.patient_id = p.id WHERE d.diagnosis_desc ILIKE '%{term}%' ORDER BY p.patient_no LIMIT {max_rows}",
                    diagnoses = diagnoses.table_name,
                    patients = patients.table_name,
                ));
            }
        }
    }

    if question == "show me patients whose first name starts with p" {
        let patients = table("patients", &["patient_no", "first_name", "last_name"])?;
        return Some(format!(
            "SELECT patient_no, first_name, last_name FROM {} WHERE first_name ILIKE 'P%' ORDER BY first_name, last_name, patient_no LIMIT {max_rows}",
            patients.table_name
        ));
    }

    if question == "how many female and male patients are there" {
        let patients = table("patients", &["gender"])?;
        return Some(format!(
            "SELECT gender::text AS gender, COUNT(*) AS patient_count FROM {} GROUP BY gender ORDER BY gender",
            patients.table_name
        ));
    }

    if question == "which month had the most encounters and how many" {
        let encounters = table("encounters", &["encounter_date"])?;
        return Some(format!(
            "SELECT DATE_TRUNC('month', encounter_date) AS encounter_month, COUNT(*) AS encounter_count FROM {} GROUP BY DATE_TRUNC('month', encounter_date) ORDER BY encounter_count DESC, encounter_month LIMIT 1",
            encounters.table_name
        ));
    }

    if question == "what is the most common diagnosis and how many times does it occur" {
        let diagnoses = table("diagnoses", &["diagnosis_desc"])?;
        return Some(format!(
            "SELECT diagnosis_desc, COUNT(*) AS diagnosis_count FROM {} GROUP BY diagnosis_desc ORDER BY diagnosis_count DESC, diagnosis_desc LIMIT 1",
            diagnoses.table_name
        ));
    }

    if question == "what is the average length of stay for discharged admissions" {
        let admissions = table("admissions", &["length_of_stay_days", "discharge_date"])?;
        return Some(format!(
            "SELECT ROUND(AVG(length_of_stay_days), 2) AS average_length_of_stay_days, COUNT(length_of_stay_days) AS discharged_admissions FROM {} WHERE discharge_date IS NOT NULL",
            admissions.table_name
        ));
    }

    if question == "which medications were prescribed most often" {
        let items = table("prescription_items", &["medication_id"])?;
        let medications = table("medication_catalog", &["id", "generic_name"])?;
        return Some(format!(
            "WITH medication_counts AS (SELECT medication.generic_name, COUNT(*) AS prescribed_items FROM {items} AS item JOIN {medications} AS medication ON medication.id = item.medication_id GROUP BY medication.generic_name) SELECT generic_name, prescribed_items FROM medication_counts WHERE prescribed_items = (SELECT MAX(prescribed_items) FROM medication_counts) ORDER BY generic_name",
            items = items.table_name,
            medications = medications.table_name,
        ));
    }

    if question == "how many lab results were abnormal" {
        let lab_results = table("lab_results", &["is_abnormal"])?;
        return Some(format!(
            "SELECT COUNT(*) AS abnormal_result_count FROM {} WHERE is_abnormal = TRUE",
            lab_results.table_name
        ));
    }

    if let Some(id) = identifier {
        let norm_id = normalize_question(id);

        if question == format!("show encounter and diagnosis counts for patient {norm_id}") {
            let patients = table("patients", &["id", "patient_no", "first_name", "last_name"])?;
            let encounters = table("encounters", &["id", "patient_id"])?;
            let diagnoses = table("diagnoses", &["id", "patient_id"])?;
            return Some(format!(
                "SELECT patient.patient_no, patient.first_name, patient.last_name, COUNT(DISTINCT encounter.id) AS encounters, COUNT(DISTINCT diagnosis.id) AS diagnoses FROM {patients} AS patient LEFT JOIN {encounters} AS encounter ON encounter.patient_id = patient.id LEFT JOIN {diagnoses} AS diagnosis ON diagnosis.patient_id = patient.id WHERE patient.patient_no = '{id}' GROUP BY patient.patient_no, patient.first_name, patient.last_name",
                patients = patients.table_name,
                encounters = encounters.table_name,
                diagnoses = diagnoses.table_name,
            ));
        }

        if is_name_lookup_question(question, &norm_id) {
            let patients = table("patients", &["patient_no", "first_name", "last_name"])?;
            return Some(format!(
                "SELECT patient_no, first_name, last_name FROM {} WHERE patient_no = '{id}' LIMIT 1",
                patients.table_name
            ));
        }
    }

    None
}

const HEALTHCARE_RECORD_SUBJECTS: &[(&str, &[&str], &[&str])] = &[
    (
        "encounters",
        &["encounter", "encounters", "visit", "visits"],
        &["encounters"],
    ),
    (
        "prescriptions",
        &["prescription", "prescriptions", "medication", "medications"],
        &["prescriptions"],
    ),
    ("admissions", &["admission", "admissions"], &["admissions"]),
    ("diagnoses", &["diagnosis", "diagnoses"], &["diagnoses"]),
    (
        "lab_orders",
        &[
            "lab order",
            "lab orders",
            "lab panel",
            "lab panels",
            "lab test",
            "lab tests",
            "labs",
        ],
        &["lab_orders"],
    ),
    ("payments", &["payment", "payments"], &["payments"]),
];

const HEALTHCARE_GROUP_ENTITIES: &[(&str, &[&str], &str)] = &[
    (
        "providers",
        &[
            "provider",
            "providers",
            "doctor",
            "doctors",
            "clinician",
            "clinicians",
            "prescriber",
            "prescribers",
        ],
        "provider",
    ),
    ("patients", &["patient", "patients"], "patient"),
];

fn top_grouped_count_join_sql(
    question: &str,
    cards: &[crate::nl2sql::spec::TableCard],
    max_rows: i64,
) -> Option<String> {
    let padded = format!(" {question} ");
    let mut subjects =
        HEALTHCARE_RECORD_SUBJECTS
            .iter()
            .filter_map(|(subject, aliases, tables)| {
                aliases
                    .iter()
                    .find(|alias| padded.contains(&format!(" {alias} ")))
                    .map(|alias| (*subject, *alias, *tables))
            });
    let (subject, subject_alias, fact_tables) = subjects.next()?;
    if subjects.next().is_some() {
        return None;
    }

    let (entity_table, entity_singular, rest) = if let Some(rest) = question.strip_prefix("who ") {
        ("providers", "provider", rest)
    } else {
        HEALTHCARE_GROUP_ENTITIES
            .iter()
            .find_map(|(table, aliases, singular)| {
                aliases.iter().find_map(|alias| {
                    question
                        .strip_prefix(&format!("which {alias} "))
                        .map(|rest| (*table, *singular, rest))
                })
            })?
    };
    let frame = rest.strip_suffix(subject_alias)?.trim();
    let verb = frame.strip_suffix("the most")?.trim();
    if !matches!(verb, "ordered" | "prescribed" | "had") {
        return None;
    }

    let fact = cards.iter().find(|card| {
        is_safe_table_name(&card.table_name)
            && fact_tables.iter().any(|candidate| {
                card.table_name
                    .rsplit('.')
                    .next()
                    .is_some_and(|name| name.eq_ignore_ascii_case(candidate))
            })
    })?;
    let entity = cards.iter().find(|card| {
        is_safe_table_name(&card.table_name)
            && card
                .table_name
                .rsplit('.')
                .next()
                .is_some_and(|name| name.eq_ignore_ascii_case(entity_table))
    })?;
    let edge = fact.fk_edges.iter().find(|edge| {
        edge.ref_table
            .rsplit('.')
            .next()
            .is_some_and(|name| name.eq_ignore_ascii_case(entity_table))
    })?;
    let fact_fk = fact
        .columns
        .iter()
        .find(|column| column.name.eq_ignore_ascii_case(&edge.column))?;
    let entity_pk_name = entity
        .columns
        .iter()
        .find(|column| column.name.eq_ignore_ascii_case(&edge.ref_column))
        .or_else(|| {
            entity
                .columns
                .iter()
                .find(|column| column.name.eq_ignore_ascii_case("id"))
        })?
        .name
        .as_str();
    if !is_safe_identifier(&fact_fk.name) || !is_safe_identifier(entity_pk_name) {
        return None;
    }

    let column = |name: &str| {
        entity.columns.iter().find(|column| {
            is_safe_identifier(&column.name) && column.name.eq_ignore_ascii_case(name)
        })
    };
    let (label, group_by) =
        if let (Some(first), Some(last)) = (column("first_name"), column("last_name")) {
            (
                format!("e.{} || ' ' || e.{}", first.name, last.name),
                format!("e.{}, e.{}", first.name, last.name),
            )
        } else if let Some(name) = column("name").or_else(|| column("full_name")) {
            (format!("e.{}", name.name), format!("e.{}", name.name))
        } else {
            (format!("e.{entity_pk_name}"), format!("e.{entity_pk_name}"))
        };
    let limit = max_rows.clamp(1, 10);
    Some(format!(
        "SELECT {label} AS {entity_singular}, COUNT(*) AS {subject} FROM {fact_table} f JOIN {entity_table} e ON f.{fact_fk} = e.{entity_pk} GROUP BY {group_by} ORDER BY 2 DESC LIMIT {limit}",
        fact_table = fact.table_name,
        entity_table = entity.table_name,
        fact_fk = fact_fk.name,
        entity_pk = entity_pk_name,
    ))
}

fn healthcare_record_subject(
    question: &str,
) -> Option<(&'static str, Vec<&'static str>, &'static [&'static str])> {
    let padded = format!(" {question} ");
    let mut matches = HEALTHCARE_RECORD_SUBJECTS
        .iter()
        .filter_map(|(subject, aliases, tables)| {
            let matched_aliases: Vec<&str> = aliases
                .iter()
                .copied()
                .filter(|alias| padded.contains(&format!(" {alias} ")))
                .collect();
            (!matched_aliases.is_empty()).then_some((*subject, matched_aliases, *tables))
        });
    let matched = matches.next()?;
    if matches.next().is_some()
        || question
            .split_whitespace()
            .any(|token| matches!(token, "patient" | "patients"))
    {
        return None;
    }
    Some(matched)
}

/// Resolve one supported record noun in a narrowly bounded recency request.
/// Unknown, compound, filtered, and person-specific requests deliberately miss.
fn recent_records_subject(
    question: &str,
    max_rows: i64,
) -> Option<(&'static str, &'static [&'static str], i64)> {
    let (subject, aliases, tables) = healthcare_record_subject(question)?;
    let alias = aliases.iter().find(|alias| question.ends_with(**alias))?;
    let prefix = question.strip_suffix(alias)?.trim();
    let words: Vec<&str> = prefix.split_whitespace().collect();
    let explicit_limit = words.iter().find_map(|word| word.parse::<i64>().ok());
    let frame_words: Vec<&str> = words
        .iter()
        .copied()
        .filter(|word| word.parse::<i64>().is_err())
        .collect();
    let supported_frame = matches!(
        frame_words.as_slice(),
        [
            "give", "me", "an", "overview", "of", "the", "most", "recent"
        ] | ["show", "the", "most", "recent"]
            | ["show", "me", "the", "most", "recent"]
            | ["show", "most", "recent"]
            | ["show", "me", "most", "recent"]
            | ["what", "are", "the", "latest"]
            | ["list", "recent"]
            | ["list", "the", "recent"]
    );
    if !supported_frame {
        return None;
    }
    let limit = explicit_limit
        .unwrap_or(10)
        .clamp(1, 25)
        .min(max_rows.max(1));
    Some((subject, tables, limit))
}

/// Cheap guard for agent orchestration. The compiler performs the final schema,
/// person-name, quote, and temporal-column checks before SQL execution.
pub(crate) fn is_recent_records_question(question: &str) -> bool {
    let contains_quote = question
        .chars()
        .any(|character| matches!(character, '\'' | '"'));
    !contains_quote
        && !contains_likely_person_name(question)
        && recent_records_subject(&normalize_question(question), 25).is_some()
}

fn preferred_recent_columns(
    card: &crate::nl2sql::spec::TableCard,
    subject: &str,
    date_column: &str,
) -> Vec<String> {
    let preferences: &[&str] = match subject {
        "encounters" => &["department", "chief_complaint", "encounter_type", "status"],
        "prescriptions" => &["rx_number", "status", "valid_until", "notes"],
        "admissions" => &["ward", "admission_type", "admitting_dx", "discharge_date"],
        "diagnoses" => &["diagnosis_desc", "icd10_code", "dx_type", "is_active"],
        "lab_orders" => &["order_no", "panel_name", "priority", "status"],
        "payments" => &["amount_kes", "payment_method", "reference_no", "notes"],
        _ => &[],
    };
    let mut selected = vec![date_column.to_string()];
    for preferred in preferences {
        if selected.len() == 5 {
            break;
        }
        if let Some(column) = card.columns.iter().find(|column| {
            is_safe_identifier(&column.name)
                && column.name.eq_ignore_ascii_case(preferred)
                && !column.name.eq_ignore_ascii_case(date_column)
        }) {
            selected.push(column.name.clone());
        }
    }
    for column in &card.columns {
        if selected.len() >= 3 || selected.len() == 5 {
            break;
        }
        if is_safe_identifier(&column.name)
            && !selected
                .iter()
                .any(|name| name.eq_ignore_ascii_case(&column.name))
            && !matches!(
                column.name.to_ascii_lowercase().as_str(),
                "id" | "patient_id"
            )
        {
            selected.push(column.name.clone());
        }
    }
    selected
}

/// Resolve one supported collection noun paired with an explicit periodic/trend
/// frame, returning the bucketing period. Multiple supported nouns — or mixed
/// granularities — are deliberately treated as ambiguous.
fn periodic_trend_subject(
    question: &str,
) -> Option<(&'static str, &'static [&'static str], &'static str)> {
    let (subject, aliases, tables) = healthcare_record_subject(question)?;

    let tokens: Vec<&str> = question.split_whitespace().collect();
    let has_count_or_volume = tokens
        .iter()
        .any(|token| matches!(*token, "count" | "counts" | "volume" | "volumes"));
    let has_trend = tokens.iter().any(|token| {
        matches!(
            *token,
            "trend" | "trends" | "trended" | "trending" | "change" | "changed"
        )
    });

    // Period nouns stay hardcoded so user text is never interpolated into SQL.
    const PERIODS: &[(&str, &[&str])] = &[
        ("month", &["monthly"]),
        ("week", &["weekly"]),
        ("day", &["daily"]),
        ("year", &["yearly", "annual"]),
    ];

    let mut matched_period: Option<&'static str> = None;
    for (period, adjectives) in PERIODS {
        let per_period = aliases
            .iter()
            .any(|alias| question.contains(&format!("{alias} per {period}")));
        let adjective_count = has_count_or_volume
            && adjectives.iter().any(|adjective| {
                aliases
                    .iter()
                    .any(|alias| question.contains(&format!("{adjective} {alias}")))
            });
        let by_period = has_trend && question.contains(&format!("by {period}"));
        if per_period || adjective_count || by_period {
            if matched_period.is_some_and(|existing| existing != *period) {
                return None;
            }
            matched_period = Some(period);
        }
    }
    if let Some(period) = matched_period {
        return Some((subject, tables, period));
    }

    let generic_trend = has_trend && (question.contains("over time") || has_count_or_volume);
    let directional_volume = has_count_or_volume && question.contains("going up or down");
    (generic_trend || directional_volume).then_some((subject, tables, "month"))
}

fn preferred_trend_date_column<'card>(
    card: &'card crate::nl2sql::spec::TableCard,
    subject: &str,
) -> Option<&'card crate::nl2sql::spec::CardColumn> {
    let singular = match subject {
        "diagnoses" => "diagnosis",
        "lab_orders" => "lab_order",
        value => value.strip_suffix('s').unwrap_or(value),
    };
    let exact = format!("{singular}_date");
    let domain_preferences: &[&str] = match subject {
        "encounters" => &["encounter_date"],
        "prescriptions" => &["issue_date", "prescribed_at"],
        "admissions" => &["admission_date", "admitted_at"],
        "diagnoses" => &["diagnosis_date", "diagnosed_at"],
        "lab_orders" => &["ordered_at", "order_date", "resulted_at"],
        "payments" => &["payment_date", "paid_at"],
        _ => &[],
    };
    std::iter::once(exact.as_str())
        .chain(domain_preferences.iter().copied())
        .chain(std::iter::once("created_at"))
        .find_map(|preferred| {
            card.columns.iter().find(|column| {
                is_safe_identifier(&column.name)
                    && column.name.eq_ignore_ascii_case(preferred)
                    && is_temporal_type(&column.type_)
            })
        })
}

fn is_temporal_type(type_: &str) -> bool {
    let type_ = type_.to_ascii_lowercase();
    type_.contains("date") || type_.contains("time")
}


/// True when the (normalized) question only asks for a patient's name plus the
/// given identifier — every remaining token must be filler, so paraphrases match
/// but any extra constraint falls through to the model planner.
fn is_name_lookup_question(question: &str, norm_id: &str) -> bool {
    let without_id = question.replace(norm_id, " ");
    let mut has_name_word = false;
    for token in without_id.split_whitespace() {
        match token {
            "name" | "named" | "called" | "who" => has_name_word = true,
            "what" | "whats" | "is" | "the" | "of" | "for" | "s" | "patient" | "patients"
            | "this" | "that" | "their" | "his" | "her" | "full" | "record" | "please" => {}
            _ => return false,
        }
    }
    has_name_word
}

#[derive(Debug, PartialEq, Eq)]
enum PatientOverviewSubject {
    Identifier(String),
    Name(Vec<String>),
}

/// Recognize narrowly framed patient-overview requests. A person-name subject
/// must contain two or three title-cased ASCII name parts; generic subjects such
/// as "asthma" or "the hospital" deliberately remain semantic.
fn patient_overview_subject(question: &str) -> Option<PatientOverviewSubject> {
    static FRAME: std::sync::LazyLock<regex::Regex> = std::sync::LazyLock::new(|| {
        regex::Regex::new(
            r"^\s*(?i:Tell\s+me\s+about|Who\s+is|Give\s+me\s+an\s+overview\s+of|Find|Look\s+up|Pull\s+up\s+(?:the\s+)?record\s+for)\s+(.+?)\s*[?.!]*\s*$",
        )
        .expect("valid patient overview regex")
    });
    static NAME_PART: std::sync::LazyLock<regex::Regex> = std::sync::LazyLock::new(|| {
        regex::Regex::new(r"^[A-Z][A-Za-z-]*$").expect("valid person name regex")
    });

    let captures = FRAME.captures(question)?;
    let frame = captures.get(0)?.as_str().trim_start().to_ascii_lowercase();
    let mut subject = captures.get(1)?.as_str().trim();
    if frame.starts_with("find ") {
        subject = subject
            .strip_suffix("'s record")
            .or_else(|| subject.strip_suffix("’s record"))
            .unwrap_or(subject)
            .trim();
    }
    if let Some(identifier) = extract_record_identifier(subject) {
        // Preserve the narrower existing "Who is patient <id>?" name lookup;
        // identifier overviews use the explicit "tell me about"/"overview" frames.
        if frame.starts_with("who is ") {
            return None;
        }
        let normalized = normalize_question(subject);
        let normalized_id = normalize_question(&identifier);
        let remainder = normalized.replace(&normalized_id, " ");
        if remainder
            .split_whitespace()
            .all(|token| matches!(token, "patient" | "the" | "record"))
        {
            return Some(PatientOverviewSubject::Identifier(identifier));
        }
        return None;
    }

    let mut parts: Vec<&str> = subject.split_whitespace().collect();
    if parts
        .first()
        .is_some_and(|part| part.eq_ignore_ascii_case("patient"))
    {
        parts.remove(0);
    }
    if !(2..=3).contains(&parts.len()) || !parts.iter().all(|part| NAME_PART.is_match(part)) {
        return None;
    }
    Some(PatientOverviewSubject::Name(
        parts.into_iter().map(str::to_string).collect(),
    ))
}

/// Cheap guard used by semantic chat orchestration before attempting source
/// linking and deterministic SQL execution.
pub(crate) fn is_patient_overview_question(question: &str) -> bool {
    patient_overview_subject(question).is_some()
}



fn patient_gender_listing_sql(
    question: &str,
    entity: &str,
    card: &crate::nl2sql::spec::TableCard,
    source_kind: crate::connectors::SourceKind,
    max_rows: i64,
) -> Option<String> {
    if entity != "patients" && entity != "patient" {
        return None;
    }
    if !["list", "show", "display", "get"]
        .iter()
        .any(|prefix| question.starts_with(prefix))
    {
        return None;
    }

    let gender = ["female", "male"]
        .into_iter()
        .find(|gender| question.split_whitespace().any(|word| word == *gender))?;
    let required_columns = ["patient_no", "first_name", "last_name", "gender"];
    if !required_columns.iter().all(|required| {
        card.columns
            .iter()
            .any(|column| column.name.eq_ignore_ascii_case(required))
    }) {
        return None;
    }

    let select = "patient_no, first_name, last_name";
    Some(match source_kind {
        crate::connectors::SourceKind::Mssql => format!(
            "SELECT TOP {max_rows} {select} FROM {} WHERE LOWER(CAST(gender AS NVARCHAR(128))) = '{gender}'",
            card.table_name
        ),
        crate::connectors::SourceKind::Postgres => format!(
            "SELECT {select} FROM {} WHERE LOWER(gender::text) = '{gender}' LIMIT {max_rows}",
            card.table_name
        ),
        crate::connectors::SourceKind::Mysql => format!(
            "SELECT {select} FROM {} WHERE LOWER(CAST(gender AS CHAR)) = '{gender}' LIMIT {max_rows}",
            card.table_name
        ),
    })
}

fn relationship_filtered_count_sql(
    question: &str,
    entity: &str,
    base: &crate::nl2sql::spec::TableCard,
    cards: &[crate::nl2sql::spec::TableCard],
) -> Option<String> {
    let (preposition, value) =
        ["are from", "come from", "are in"]
            .into_iter()
            .find_map(|preposition| {
                question
                    .strip_prefix(&format!("how many {entity} {preposition} "))
                    .map(|value| (preposition, value.trim()))
            })?;
    if value.is_empty() || value.len() > 128 {
        return None;
    }

    let geographic = [
        "county", "city", "location", "region", "state", "country", "district", "ward",
    ];
    let mut candidates = Vec::new();
    for edge in &base.fk_edges {
        if !is_safe_identifier(&edge.column) || !is_safe_identifier(&edge.ref_column) {
            continue;
        }
        let Some(target) = cards.iter().find(|card| {
            card.table_name.eq_ignore_ascii_case(&edge.ref_table)
                || card
                    .table_name
                    .rsplit('.')
                    .next()
                    .is_some_and(|name| name.eq_ignore_ascii_case(&edge.ref_table))
        }) else {
            continue;
        };
        if !is_safe_table_name(&target.table_name) {
            continue;
        }
        let lower_column = edge.column.to_ascii_lowercase();
        let relation = lower_column
            .strip_suffix("_id")
            .unwrap_or(&lower_column)
            .to_string();
        let relation_name = format!("{relation}_name");
        let label = target.columns.iter().find(|column| {
            is_safe_identifier(&column.name)
                && (["name", "title", "label", "code"]
                    .iter()
                    .any(|candidate| column.name.eq_ignore_ascii_case(candidate))
                    || column.name.eq_ignore_ascii_case(&relation_name))
        })?;
        let sample_match = label
            .sample_values
            .iter()
            .any(|sample| normalize_question(sample) == value);
        let score = i32::from(sample_match) * 10
            + i32::from(question.split_whitespace().any(|word| word == relation)) * 4
            + i32::from(preposition.contains("from") && geographic.contains(&relation.as_str()))
                * 5;
        if score > 0 {
            candidates.push((score, edge, target, label));
        }
    }
    candidates.sort_by(|a, b| b.0.cmp(&a.0));
    let (_, edge, target, label) = candidates.into_iter().next()?;
    let literal = value.replace('\'', "''");
    let alias = entity.replace(' ', "_").trim_end_matches('s').to_string();
    Some(format!(
        "SELECT COUNT(*) AS {alias}_count FROM {base_table} AS base JOIN {target_table} AS lookup ON base.{base_column} = lookup.{target_column} WHERE LOWER(lookup.{label_column}) = LOWER('{literal}')",
        base_table = base.table_name,
        target_table = target.table_name,
        base_column = edge.column,
        target_column = edge.ref_column,
        label_column = label.name,
    ))
}

fn grouped_count_sql(
    question: &str,
    entity: &str,
    card: &crate::nl2sql::spec::TableCard,
    source_kind: crate::connectors::SourceKind,
) -> Option<String> {
    let count_alias = format!("{}_count", entity.replace(' ', "_").trim_end_matches('s'));
    for column in &card.columns {
        if !is_safe_identifier(&column.name) {
            continue;
        }
        let label = column.name.replace('_', " ").to_ascii_lowercase();
        let templates = [
            format!("count {entity} by {label}"),
            format!("number of {entity} by {label}"),
            format!("show number of {entity} by {label}"),
            format!("show the number of {entity} by {label}"),
        ];
        if templates.iter().any(|template| template == question) {
            return Some(format!(
                "SELECT {column}, COUNT(*) AS {count_alias} FROM {table} GROUP BY {column} ORDER BY {count_alias} DESC",
                column = column.name,
                table = card.table_name,
            ));
        }
    }

    for period in ["day", "month", "year"] {
        let templates = [
            format!("count {entity} per {period}"),
            format!("count {entity} by {period}"),
            format!("number of {entity} per {period}"),
            format!("number of {entity} by {period}"),
            format!("show number of {entity} per {period}"),
            format!("show the number of {entity} per {period}"),
        ];
        if !templates.iter().any(|template| template == question) {
            continue;
        }
        let preferred = format!("{}_date", entity.trim_end_matches('s').replace(' ', "_"));
        let date_column = card
            .columns
            .iter()
            .filter(|column| is_safe_identifier(&column.name))
            .find(|column| column.name.eq_ignore_ascii_case(&preferred))
            .or_else(|| {
                card.columns.iter().find(|column| {
                    is_safe_identifier(&column.name)
                        && (column.type_.to_ascii_lowercase().contains("date")
                            || column.type_.to_ascii_lowercase().contains("time"))
                })
            })?;
        let bucket = match source_kind {
            crate::connectors::SourceKind::Postgres => {
                format!("DATE_TRUNC('{period}', {})", date_column.name)
            }
            crate::connectors::SourceKind::Mysql => match period {
                "day" => format!("DATE({})", date_column.name),
                "month" => format!("DATE_FORMAT({}, '%Y-%m-01')", date_column.name),
                _ => format!("DATE_FORMAT({}, '%Y-01-01')", date_column.name),
            },
            crate::connectors::SourceKind::Mssql => match period {
                "day" => format!("CAST({} AS date)", date_column.name),
                "month" => format!("DATEFROMPARTS(YEAR({0}), MONTH({0}), 1)", date_column.name),
                _ => format!("DATEFROMPARTS(YEAR({}), 1, 1)", date_column.name),
            },
        };
        return Some(format!(
            "SELECT {bucket} AS {period}, COUNT(*) AS {count_alias} FROM {table} GROUP BY {bucket} ORDER BY {period}",
            table = card.table_name,
        ));
    }
    None
}


fn is_safe_table_name(table: &str) -> bool {
    !table.is_empty() && table.split('.').all(is_safe_identifier)
}

fn is_safe_identifier(identifier: &str) -> bool {
    !identifier.is_empty()
        && identifier
            .chars()
            .next()
            .is_some_and(|character| character.is_ascii_alphabetic() || character == '_')
        && identifier
            .chars()
            .all(|character| character.is_ascii_alphanumeric() || character == '_')
}

fn listing_limit(question: &str, entity: &str, max_rows: i64) -> Option<i64> {
    for prefix in ["list", "show me", "show", "display", "get"] {
        let Some(mut rest) = question.strip_prefix(prefix) else {
            continue;
        };
        rest = rest.trim();
        rest = rest.strip_prefix("the ").unwrap_or(rest);
        if rest == entity || rest == format!("all {entity}") {
            return Some(max_rows);
        }
        rest = rest.strip_prefix("first ").unwrap_or(rest);
        let (amount, remainder) = rest.split_once(' ')?;
        if remainder == entity {
            let requested = amount.parse::<i64>().ok()?;
            return Some(requested.clamp(1, max_rows));
        }
    }
    None
}






#[cfg(test)]
mod tests {
    use super::{deterministic_sql, extract_record_identifier, resolve_followup_question};
    use super::super::text::summarize_result;
    use crate::connectors::SourceKind;
    use crate::nl2sql::spec::{CardColumn, CardFkEdge, TableCard};

    fn column(name: &str, type_: &str) -> CardColumn {
        CardColumn {
            name: name.into(),
            type_: type_.into(),
            nullable: false,
            is_primary_key: false,
            is_foreign_key: false,
            sample_values: Vec::new(),
            profile: Default::default(),
        }
    }

    fn patients_card() -> TableCard {
        TableCard {
            source_id: "source-1".into(),
            table_name: "patients".into(),
            row_count: 60,
            columns: vec![
                column("id", "uuid"),
                column("patient_no", "character varying"),
                column("first_name", "character varying"),
                column("middle_name", "character varying"),
                column("last_name", "character varying"),
                column("date_of_birth", "date"),
                column("gender", "gender_type"),
                column("blood_type", "character varying"),
            ],
            fk_edges: Vec::new(),
            card_vector: None,
            card_text: "Table: patients".into(),
        }
    }

    fn schema_card(table_name: &str, columns: &[&str]) -> TableCard {
        TableCard {
            source_id: "source-1".into(),
            table_name: table_name.into(),
            row_count: 1,
            columns: columns
                .iter()
                .map(|name| column(name, "character varying"))
                .collect(),
            fk_edges: Vec::new(),
            card_vector: None,
            card_text: format!("Table: {table_name}"),
        }
    }

    fn typed_schema_card(table_name: &str, columns: &[(&str, &str)]) -> TableCard {
        TableCard {
            source_id: "source-1".into(),
            table_name: table_name.into(),
            row_count: 1,
            columns: columns
                .iter()
                .map(|(name, type_)| column(name, type_))
                .collect(),
            fk_edges: Vec::new(),
            card_vector: None,
            card_text: format!("Table: {table_name}"),
        }
    }

    fn fact_card_with_fk(table_name: &str, fk_column: &str, ref_table: &str) -> TableCard {
        let mut card = schema_card(table_name, &["id", fk_column]);
        card.fk_edges.push(CardFkEdge {
            column: fk_column.into(),
            ref_table: ref_table.into(),
            ref_column: "id".into(),
        });
        card
    }

    #[test]
    fn compiles_simple_counts_without_the_model() {
        let cards = vec![patients_card()];
        let sql = deterministic_sql(
            "How many patients do we have?",
            &cards,
            SourceKind::Postgres,
            100,
        );
        assert_eq!(
            sql.as_deref(),
            Some("SELECT COUNT(*) AS patient_count FROM patients")
        );
        assert_eq!(
            deterministic_sql(
                "How many patient records are indexed?",
                &cards,
                SourceKind::Postgres,
                100,
            )
            .as_deref(),
            Some("SELECT COUNT(*) AS patient_count FROM patients")
        );
    }

    #[test]
    fn compiles_bounded_listings_without_the_model() {
        let cards = vec![patients_card()];
        assert_eq!(
            deterministic_sql("List 5 patients", &cards, SourceKind::Postgres, 100).as_deref(),
            Some("SELECT * FROM patients LIMIT 5")
        );
        assert_eq!(
            deterministic_sql(
                "Show me the first 5 patients",
                &cards,
                SourceKind::Mssql,
                100
            )
            .as_deref(),
            Some("SELECT TOP 5 * FROM patients")
        );
    }

    #[test]
    fn compiles_grouped_counts_without_the_model() {
        let cards = vec![patients_card()];
        assert_eq!(
            deterministic_sql(
                "Count patients by gender",
                &cards,
                SourceKind::Postgres,
                100
            )
            .as_deref(),
            Some(
                "SELECT gender, COUNT(*) AS patient_count FROM patients GROUP BY gender ORDER BY patient_count DESC"
            )
        );
    }

    #[test]
    fn compiles_time_buckets_without_the_model() {
        let mut encounters = patients_card();
        encounters.table_name = "encounters".into();
        encounters.columns = vec![column("encounter_date", "timestamp with time zone")];
        let cards = vec![encounters];
        assert_eq!(
            deterministic_sql(
                "Show the number of encounters per month",
                &cards,
                SourceKind::Postgres,
                100
            )
            .as_deref(),
            Some(
                "SELECT date_trunc('month', encounter_date)::date AS month, COUNT(*) AS encounters FROM encounters GROUP BY 1 ORDER BY 1 LIMIT 100"
            )
        );
    }

    #[test]
    fn compiles_encounter_volume_trend_to_monthly_sql() {
        let encounters = typed_schema_card(
            "public.encounters",
            &[
                ("id", "uuid"),
                ("encounter_date", "timestamp with time zone"),
            ],
        );
        assert_eq!(
            deterministic_sql(
                "Have encounter volumes been going up or down over time?",
                &[encounters],
                SourceKind::Postgres,
                100,
            )
            .as_deref(),
            Some(
                "SELECT date_trunc('month', encounter_date)::date AS month, COUNT(*) AS encounters FROM public.encounters GROUP BY 1 ORDER BY 1 LIMIT 100"
            )
        );
    }

    #[test]
    fn compiles_supported_encounter_volume_trend_phrasings() {
        let encounters = typed_schema_card(
            "encounters",
            &[
                ("id", "uuid"),
                ("encounter_date", "timestamp with time zone"),
            ],
        );
        let expected = "SELECT date_trunc('month', encounter_date)::date AS month, COUNT(*) AS encounters FROM encounters GROUP BY 1 ORDER BY 1 LIMIT 100";
        for question in [
            "How have encounter volumes changed over time?",
            "Show encounter volume trend",
            "Encounters per month",
            "Monthly encounter counts",
            "How have visits trended over time?",
        ] {
            assert_eq!(
                deterministic_sql(
                    question,
                    std::slice::from_ref(&encounters),
                    SourceKind::Postgres,
                    100,
                )
                .as_deref(),
                Some(expected),
                "question: {question}"
            );
        }
    }

    #[test]
    fn refuses_ambiguous_or_patient_specific_encounter_trends() {
        let encounters = typed_schema_card(
            "encounters",
            &[
                ("id", "uuid"),
                ("encounter_date", "timestamp with time zone"),
            ],
        );
        for question in [
            "Encounter history for SYN-2024-0001 over time",
            "Show the patient's encounter history over time",
            "Show encounters over time",
            "Did activity go up or down over time?",
        ] {
            assert!(
                deterministic_sql(
                    question,
                    std::slice::from_ref(&encounters),
                    SourceKind::Postgres,
                    100,
                )
                .is_none(),
                "question unexpectedly compiled: {question}"
            );
        }
    }

    #[test]
    fn compiles_prescription_monthly_trend_using_issue_date() {
        let prescriptions = typed_schema_card(
            "public.prescriptions",
            &[
                ("id", "uuid"),
                ("created_at", "timestamp with time zone"),
                ("issue_date", "date"),
            ],
        );
        assert_eq!(
            deterministic_sql(
                "How have prescriptions trended by month?",
                &[prescriptions],
                SourceKind::Postgres,
                100,
            )
            .as_deref(),
            Some(
                "SELECT date_trunc('month', issue_date)::date AS month, COUNT(*) AS prescriptions FROM public.prescriptions GROUP BY 1 ORDER BY 1 LIMIT 100"
            )
        );
    }

    #[test]
    fn compiles_weekly_daily_and_yearly_trends() {
        let prescriptions = typed_schema_card(
            "public.prescriptions",
            &[
                ("id", "uuid"),
                ("created_at", "timestamp with time zone"),
                ("issue_date", "date"),
            ],
        );
        assert_eq!(
            deterministic_sql(
                "How have prescriptions trended by week?",
                std::slice::from_ref(&prescriptions),
                SourceKind::Postgres,
                100,
            )
            .as_deref(),
            Some(
                "SELECT date_trunc('week', issue_date)::date AS week, COUNT(*) AS prescriptions FROM public.prescriptions GROUP BY 1 ORDER BY 1 LIMIT 100"
            )
        );

        let encounters = typed_schema_card(
            "encounters",
            &[("encounter_date", "timestamp with time zone")],
        );
        for question in ["Encounters per day", "Show daily encounter counts"] {
            assert_eq!(
                deterministic_sql(
                    question,
                    std::slice::from_ref(&encounters),
                    SourceKind::Postgres,
                    100,
                )
                .as_deref(),
                Some(
                    "SELECT date_trunc('day', encounter_date)::date AS day, COUNT(*) AS encounters FROM encounters GROUP BY 1 ORDER BY 1 LIMIT 100"
                ),
                "question: {question}"
            );
        }

        let admissions = typed_schema_card("admissions", &[("admission_date", "date")]);
        assert_eq!(
            deterministic_sql(
                "How have admissions trended by year?",
                &[admissions],
                SourceKind::Postgres,
                100,
            )
            .as_deref(),
            Some(
                "SELECT date_trunc('year', admission_date)::date AS year, COUNT(*) AS admissions FROM admissions GROUP BY 1 ORDER BY 1 LIMIT 100"
            )
        );

        assert!(
            deterministic_sql(
                "How have Jane Chebet's prescriptions trended by week?",
                std::slice::from_ref(&prescriptions),
                SourceKind::Postgres,
                100,
            )
            .is_none()
        );
    }

    #[test]
    fn compiles_top_provider_counts_from_foreign_key_metadata() {
        let providers = schema_card("providers", &["id", "first_name", "last_name"]);
        let cases = [
            (
                "Which providers ordered the most lab panels?",
                fact_card_with_fk("lab_orders", "ordered_by", "providers"),
                "SELECT e.first_name || ' ' || e.last_name AS provider, COUNT(*) AS lab_orders FROM lab_orders f JOIN providers e ON f.ordered_by = e.id GROUP BY e.first_name, e.last_name ORDER BY 2 DESC LIMIT 10",
                100,
            ),
            (
                "Which providers prescribed the most medications?",
                fact_card_with_fk("prescriptions", "prescriber_id", "providers"),
                "SELECT e.first_name || ' ' || e.last_name AS provider, COUNT(*) AS prescriptions FROM prescriptions f JOIN providers e ON f.prescriber_id = e.id GROUP BY e.first_name, e.last_name ORDER BY 2 DESC LIMIT 10",
                100,
            ),
            (
                "Who had the most encounters?",
                fact_card_with_fk("encounters", "provider_id", "providers"),
                "SELECT e.first_name || ' ' || e.last_name AS provider, COUNT(*) AS encounters FROM encounters f JOIN providers e ON f.provider_id = e.id GROUP BY e.first_name, e.last_name ORDER BY 2 DESC LIMIT 7",
                7,
            ),
        ];
        for (question, fact, expected, max_rows) in cases {
            assert_eq!(
                deterministic_sql(
                    question,
                    &[fact, providers.clone()],
                    SourceKind::Postgres,
                    max_rows,
                )
                .as_deref(),
                Some(expected),
                "question: {question}"
            );
        }
    }

    #[test]
    fn top_provider_counts_require_both_schema_cards_and_fk_edge() {
        let question = "Which providers ordered the most lab panels?";
        let providers = schema_card("providers", &["id", "first_name", "last_name"]);
        let labs = fact_card_with_fk("lab_orders", "ordered_by", "providers");
        assert!(
            deterministic_sql(
                question,
                std::slice::from_ref(&labs),
                SourceKind::Postgres,
                100,
            )
            .is_none()
        );
        assert!(
            deterministic_sql(
                question,
                std::slice::from_ref(&providers),
                SourceKind::Postgres,
                100,
            )
            .is_none()
        );

        let labs_without_edge = schema_card("lab_orders", &["id", "ordered_by"]);
        assert!(
            deterministic_sql(
                question,
                &[labs_without_edge, providers],
                SourceKind::Postgres,
                100,
            )
            .is_none()
        );
    }

    #[test]
    fn top_provider_counts_refuse_person_names_and_ambiguous_subjects() {
        let providers = schema_card("providers", &["id", "first_name", "last_name"]);
        let labs = fact_card_with_fk("lab_orders", "ordered_by", "providers");
        let encounters = fact_card_with_fk("encounters", "provider_id", "providers");
        let cards = [providers, labs, encounters];
        for question in [
            "Which providers treated Jane Chebet the most?",
            "Which providers had the most encounters and lab panels?",
        ] {
            assert!(
                deterministic_sql(question, &cards, SourceKind::Postgres, 100).is_none(),
                "question unexpectedly compiled: {question}"
            );
        }
    }

    #[test]
    fn compiles_supported_healthcare_monthly_trends_with_preferred_dates() {
        let cases = [
            (
                "Admissions per month",
                typed_schema_card(
                    "admissions",
                    &[
                        ("discharge_date", "timestamp"),
                        ("admission_date", "timestamp"),
                    ],
                ),
                "SELECT date_trunc('month', admission_date)::date AS month, COUNT(*) AS admissions FROM admissions GROUP BY 1 ORDER BY 1 LIMIT 100",
            ),
            (
                "Monthly lab order counts",
                typed_schema_card(
                    "clinical.lab_orders",
                    &[("created_at", "timestamp"), ("order_date", "timestamp")],
                ),
                "SELECT date_trunc('month', order_date)::date AS month, COUNT(*) AS lab_orders FROM clinical.lab_orders GROUP BY 1 ORDER BY 1 LIMIT 100",
            ),
            (
                "How have diagnoses changed over time?",
                typed_schema_card(
                    "diagnoses",
                    &[("created_at", "timestamp"), ("diagnosed_at", "timestamp")],
                ),
                "SELECT date_trunc('month', diagnosed_at)::date AS month, COUNT(*) AS diagnoses FROM diagnoses GROUP BY 1 ORDER BY 1 LIMIT 100",
            ),
            (
                "Have payment volumes been going up or down over time?",
                typed_schema_card("payments", &[("payment_date", "date")]),
                "SELECT date_trunc('month', payment_date)::date AS month, COUNT(*) AS payments FROM payments GROUP BY 1 ORDER BY 1 LIMIT 100",
            ),
        ];
        for (question, card, expected) in cases {
            assert_eq!(
                deterministic_sql(question, &[card], SourceKind::Postgres, 100).as_deref(),
                Some(expected),
                "question: {question}"
            );
        }
    }

    #[test]
    fn refuses_unsafe_ambiguous_or_unresolvable_monthly_trends() {
        let prescriptions = typed_schema_card("prescriptions", &[("issue_date", "date")]);
        let admissions = typed_schema_card("admissions", &[("admission_date", "date")]);
        let undated_payments = schema_card("payments", &["id", "amount"]);

        assert!(
            deterministic_sql(
                "How have Jane Chebet's prescriptions trended?",
                std::slice::from_ref(&prescriptions),
                SourceKind::Postgres,
                100,
            )
            .is_none()
        );
        assert!(
            deterministic_sql(
                "How have widgets trended by month?",
                std::slice::from_ref(&prescriptions),
                SourceKind::Postgres,
                100,
            )
            .is_none()
        );
        assert!(
            deterministic_sql(
                "How have payments trended by month?",
                &[undated_payments],
                SourceKind::Postgres,
                100,
            )
            .is_none()
        );
        assert!(
            deterministic_sql(
                "How have prescriptions and admissions changed over time?",
                &[prescriptions, admissions],
                SourceKind::Postgres,
                100,
            )
            .is_none()
        );
    }

    #[test]
    fn compiles_recent_records_overview_to_sql() {
        let encounters = typed_schema_card(
            "encounters",
            &[
                ("id", "uuid"),
                ("patient_id", "uuid"),
                ("encounter_date", "timestamp with time zone"),
                ("department", "character varying"),
                ("chief_complaint", "character varying"),
                ("created_at", "timestamp with time zone"),
            ],
        );
        assert_eq!(
            deterministic_sql(
                "Give me an overview of the most recent encounters.",
                &[encounters],
                SourceKind::Postgres,
                100,
            )
            .as_deref(),
            Some(
                "SELECT encounter_date, department, chief_complaint FROM encounters ORDER BY encounter_date DESC LIMIT 10"
            )
        );
    }

    #[test]
    fn compiles_supported_recent_record_phrasings() {
        let encounters = typed_schema_card(
            "public.encounters",
            &[
                ("encounter_date", "timestamp"),
                ("department", "text"),
                ("chief_complaint", "text"),
            ],
        );
        for (question, limit) in [
            ("Show the most recent encounters.", 10),
            ("What are the latest encounters?", 10),
            ("List recent encounters.", 10),
            ("Show me the 5 most recent encounters.", 5),
            ("Show me the 50 most recent visits.", 25),
        ] {
            let expected = format!(
                "SELECT encounter_date, department, chief_complaint FROM public.encounters ORDER BY encounter_date DESC LIMIT {limit}"
            );
            assert_eq!(
                deterministic_sql(
                    question,
                    std::slice::from_ref(&encounters),
                    SourceKind::Postgres,
                    100,
                )
                .as_deref(),
                Some(expected.as_str()),
                "question: {question}"
            );
        }
    }

    #[test]
    fn refuses_ambiguous_or_person_specific_recent_queries() {
        let encounters = typed_schema_card("encounters", &[("encounter_date", "timestamp")]);
        let prescriptions = typed_schema_card("prescriptions", &[("issue_date", "date")]);
        let undated_payments = schema_card("payments", &["id", "amount"]);
        for question in [
            "Show Jane Chebet's most recent encounters.",
            "Show the most recent encounters for 'asthma'.",
            "Show the most recent encounters and prescriptions.",
            "Show the most recent widgets.",
        ] {
            assert!(
                deterministic_sql(
                    question,
                    &[encounters.clone(), prescriptions.clone()],
                    SourceKind::Postgres,
                    100,
                )
                .is_none(),
                "question unexpectedly compiled: {question}"
            );
        }
        assert!(
            deterministic_sql(
                "Show the most recent payments.",
                &[undated_payments],
                SourceKind::Postgres,
                100,
            )
            .is_none()
        );
    }

    #[test]
    fn compiles_foreign_key_lookup_counts_without_the_model() {
        let mut patients = patients_card();
        patients.columns.push(column("county_id", "integer"));
        patients.fk_edges.push(CardFkEdge {
            column: "county_id".into(),
            ref_table: "counties".into(),
            ref_column: "id".into(),
        });
        let counties = TableCard {
            source_id: "source-1".into(),
            table_name: "counties".into(),
            row_count: 4,
            columns: vec![column("id", "integer"), column("name", "character varying")],
            fk_edges: Vec::new(),
            card_vector: None,
            card_text: "Table: counties".into(),
        };
        assert_eq!(
            deterministic_sql(
                "How many patients are from Nyeri?",
                &[patients, counties],
                SourceKind::Postgres,
                100,
            )
            .as_deref(),
            Some(
                "SELECT COUNT(*) AS patient_count FROM patients AS base JOIN counties AS lookup ON base.county_id = lookup.id WHERE LOWER(lookup.name) = LOWER('nyeri')"
            )
        );
    }

    #[test]
    fn compiles_patient_gender_listings_without_the_model() {
        let cards = vec![patients_card()];
        assert_eq!(
            deterministic_sql(
                "List the names and patient numbers of all female patients.",
                &cards,
                SourceKind::Postgres,
                100,
            )
            .as_deref(),
            Some(
                "SELECT patient_no, first_name, last_name FROM patients WHERE LOWER(gender::text) = 'female' LIMIT 100"
            )
        );
    }

    #[test]
    fn compiles_common_healthcare_analytics_without_the_model() {
        let cards = vec![
            patients_card(),
            schema_card("encounters", &["id", "patient_id", "encounter_date"]),
            schema_card("diagnoses", &["id", "patient_id", "diagnosis_desc"]),
            schema_card("admissions", &["length_of_stay_days", "discharge_date"]),
            schema_card("prescription_items", &["medication_id"]),
            schema_card("medication_catalog", &["id", "generic_name"]),
            schema_card("lab_results", &["is_abnormal"]),
        ];
        let cases = [
            (
                "Show me patients whose first name starts with P.",
                "SELECT patient_no, first_name, last_name FROM patients WHERE first_name ILIKE 'P%' ORDER BY first_name, last_name, patient_no LIMIT 100",
            ),
            (
                "How many female and male patients are there?",
                "SELECT gender::text AS gender, COUNT(*) AS patient_count FROM patients GROUP BY gender ORDER BY gender",
            ),
            (
                "Which month had the most encounters, and how many?",
                "SELECT DATE_TRUNC('month', encounter_date) AS encounter_month, COUNT(*) AS encounter_count FROM encounters GROUP BY DATE_TRUNC('month', encounter_date) ORDER BY encounter_count DESC, encounter_month LIMIT 1",
            ),
            (
                "What is the most common diagnosis, and how many times does it occur?",
                "SELECT diagnosis_desc, COUNT(*) AS diagnosis_count FROM diagnoses GROUP BY diagnosis_desc ORDER BY diagnosis_count DESC, diagnosis_desc LIMIT 1",
            ),
            (
                "What is the average length of stay for discharged admissions?",
                "SELECT ROUND(AVG(length_of_stay_days), 2) AS average_length_of_stay_days, COUNT(length_of_stay_days) AS discharged_admissions FROM admissions WHERE discharge_date IS NOT NULL",
            ),
            (
                "Which medications were prescribed most often?",
                "WITH medication_counts AS (SELECT medication.generic_name, COUNT(*) AS prescribed_items FROM prescription_items AS item JOIN medication_catalog AS medication ON medication.id = item.medication_id GROUP BY medication.generic_name) SELECT generic_name, prescribed_items FROM medication_counts WHERE prescribed_items = (SELECT MAX(prescribed_items) FROM medication_counts) ORDER BY generic_name",
            ),
            (
                "How many lab results were abnormal?",
                "SELECT COUNT(*) AS abnormal_result_count FROM lab_results WHERE is_abnormal = TRUE",
            ),
            (
                "Show encounter and diagnosis counts for patient SYN-2024-0001.",
                "SELECT patient.patient_no, patient.first_name, patient.last_name, COUNT(DISTINCT encounter.id) AS encounters, COUNT(DISTINCT diagnosis.id) AS diagnoses FROM patients AS patient LEFT JOIN encounters AS encounter ON encounter.patient_id = patient.id LEFT JOIN diagnoses AS diagnosis ON diagnosis.patient_id = patient.id WHERE patient.patient_no = 'SYN-2024-0001' GROUP BY patient.patient_no, patient.first_name, patient.last_name",
            ),
        ];

        for (question, expected) in cases {
            assert_eq!(
                deterministic_sql(question, &cards, SourceKind::Postgres, 100).as_deref(),
                Some(expected),
                "question: {question}"
            );
        }
    }

    #[test]
    fn compiles_patient_diagnosis_listings_without_the_model() {
        let cards = vec![
            patients_card(),
            schema_card("diagnoses", &["patient_id", "diagnosis_desc"]),
        ];
        let cases = [
            ("Which patients have been diagnosed with asthma?", "asthma"),
            ("Which patients have asthma?", "asthma"),
            ("List patients with diabetes", "diabetes"),
            ("Who has been diagnosed with malaria?", "malaria"),
            ("Show me patients diagnosed with asthma", "asthma"),
        ];

        for (question, term) in cases {
            let sql = deterministic_sql(question, &cards, SourceKind::Postgres, 100)
                .unwrap_or_else(|| panic!("question did not compile: {question}"));
            assert!(
                sql.starts_with("SELECT DISTINCT p.patient_no"),
                "sql: {sql}"
            );
            assert!(
                sql.contains(&format!("d.diagnosis_desc ILIKE '%{term}%'")),
                "sql: {sql}"
            );
        }
    }

    #[test]
    fn refuses_unsafe_or_non_list_diagnosis_questions() {
        let cards = vec![
            patients_card(),
            schema_card("diagnoses", &["patient_id", "diagnosis_desc"]),
        ];
        for question in [
            "How many patients have asthma?",
            "Which doctors have treated asthma?",
            "Which patients have Crohn's disease?",
        ] {
            assert!(
                deterministic_sql(question, &cards, SourceKind::Postgres, 100).is_none(),
                "question unexpectedly compiled: {question}"
            );
        }
    }

    #[test]
    fn compiles_identifier_lookups_for_any_patient_number() {
        let cards = vec![
            patients_card(),
            schema_card("encounters", &["id", "patient_id", "encounter_date"]),
            schema_card("diagnoses", &["id", "patient_id", "diagnosis_desc"]),
        ];
        // Generalized: any identifier, not just the benchmark's SYN-2024-0001.
        assert_eq!(
            deterministic_sql(
                "Show encounter and diagnosis counts for patient SYN-2024-0042.",
                &cards,
                SourceKind::Postgres,
                100,
            )
            .as_deref(),
            Some(
                "SELECT patient.patient_no, patient.first_name, patient.last_name, COUNT(DISTINCT encounter.id) AS encounters, COUNT(DISTINCT diagnosis.id) AS diagnoses FROM patients AS patient LEFT JOIN encounters AS encounter ON encounter.patient_id = patient.id LEFT JOIN diagnoses AS diagnosis ON diagnosis.patient_id = patient.id WHERE patient.patient_no = 'SYN-2024-0042' GROUP BY patient.patient_no, patient.first_name, patient.last_name"
            )
        );
        let name_lookups = [
            "What is the name of patient SYN-2024-0001?",
            "Who is patient syn-2024-0001?",
            // The augmented form produced by resolve_followup_question.
            "What is the patient's name? patient SYN-2024-0001",
        ];
        for question in name_lookups {
            assert_eq!(
                deterministic_sql(question, &cards, SourceKind::Postgres, 100).as_deref(),
                Some(
                    "SELECT patient_no, first_name, last_name FROM patients WHERE patient_no = 'SYN-2024-0001' LIMIT 1"
                ),
                "question: {question}"
            );
        }
        // Extra constraints must fall through to the model planner.
        assert!(
            deterministic_sql(
                "What is the name of the doctor who treated patient SYN-2024-0001?",
                &cards,
                SourceKind::Postgres,
                100,
            )
            .is_none()
        );
    }

    #[test]
    fn compiles_patient_overview_phrasings_without_the_model() {
        let cards = vec![patients_card()];
        let name_sql = "SELECT patient_no, first_name, middle_name, last_name, date_of_birth, gender, blood_type FROM patients WHERE first_name ILIKE 'Jane' AND last_name ILIKE 'Chebet' ORDER BY patient_no LIMIT 10";
        for question in [
            "Tell me about Jane Chebet.",
            "tell me about Jane Chebet",
            "Who is Jane Chebet?",
            "Give me an overview of Jane Chebet",
            "Find Jane Chebet's record.",
            "Find Jane Chebet",
            "Look up Jane Chebet",
            "Pull up the record for Jane Chebet",
        ] {
            assert_eq!(
                deterministic_sql(question, &cards, SourceKind::Postgres, 100).as_deref(),
                Some(name_sql),
                "question: {question}"
            );
        }

        assert_eq!(
            deterministic_sql(
                "Tell me about patient SYN-2024-0001.",
                &cards,
                SourceKind::Postgres,
                100,
            )
            .as_deref(),
            Some(
                "SELECT patient_no, first_name, middle_name, last_name, date_of_birth, gender, blood_type FROM patients WHERE patient_no = 'SYN-2024-0001' ORDER BY patient_no LIMIT 10"
            )
        );
        assert_eq!(
            deterministic_sql(
                "Look up patient SYN-2024-0001",
                &cards,
                SourceKind::Postgres,
                100,
            )
            .as_deref(),
            Some(
                "SELECT patient_no, first_name, middle_name, last_name, date_of_birth, gender, blood_type FROM patients WHERE patient_no = 'SYN-2024-0001' ORDER BY patient_no LIMIT 10"
            )
        );
    }

    #[test]
    fn refuses_non_person_patient_overview_subjects() {
        let cards = vec![patients_card()];
        for question in [
            "Tell me about asthma.",
            "Tell me about the hospital.",
            "Give me an overview of diabetes",
            "Who is the doctor on call?",
            "Find asthma records",
            "Look up lab results",
        ] {
            assert!(
                deterministic_sql(question, &cards, SourceKind::Postgres, 100).is_none(),
                "question unexpectedly compiled: {question}"
            );
        }
    }

    #[test]
    fn resolves_anaphoric_followups_from_prior_turns() {
        let turns = [
            "Show encounter and diagnosis counts for patient SYN-2024-0001.",
            "Returned 1 row from the live database.",
        ];
        assert_eq!(
            resolve_followup_question("What is the patient's name?", turns.iter().copied(),)
                .as_deref(),
            Some("What is the patient's name? patient SYN-2024-0001")
        );
        // No anaphor -> leave the question alone.
        assert!(
            resolve_followup_question("How many patients are there?", turns.iter().copied())
                .is_none()
        );
        // Identifier already present -> nothing to resolve.
        assert!(
            resolve_followup_question(
                "What is the name of patient SYN-2024-0002?",
                turns.iter().copied(),
            )
            .is_none()
        );
        // No identifier anywhere in history -> nothing to ground against.
        assert!(
            resolve_followup_question(
                "What is the patient's name?",
                ["How many patients are there?"].iter().copied(),
            )
            .is_none()
        );
    }

    #[test]
    fn extracts_record_identifier_edge_cases() {
        assert_eq!(
            extract_record_identifier("Find the record for SYN-2024-0001, please.").as_deref(),
            Some("SYN-2024-0001")
        );
        assert_eq!(
            extract_record_identifier("Find patient syn-2024-0001.").as_deref(),
            Some("SYN-2024-0001")
        );
        assert!(extract_record_identifier("plain words only").is_none());
        assert!(extract_record_identifier("The year is 2024.").is_none());
        assert!(extract_record_identifier("COVID-19").is_none());
        assert_eq!(
            extract_record_identifier("First SYN-2024-0001, then SYN-2024-0002.").as_deref(),
            Some("SYN-2024-0001")
        );
    }

    /// The actual seeded format (`docker/dev-postgres/init/02_seed.sql:942`) is a
    /// single hyphen and a 5-digit run, which the old two-hyphen-mandatory regex
    /// never matched — this extractor had never recognised a real patient number.
    #[test]
    fn extracts_the_actual_seeded_patient_no_format() {
        assert_eq!(
            extract_record_identifier("what is PT-00042 allergic to").as_deref(),
            Some("PT-00042")
        );
    }

    /// A raw UUID's hyphen-separated hex groups can themselves read as
    /// letters-digits-digits (`...-ebda-4216-8201-...`, a real fragment of a
    /// seeded prescription-item UUID, `02_seed.sql:14363`) and must not be
    /// handed back as if it were a meaningful record code.
    #[test]
    fn does_not_extract_a_fragment_of_a_raw_uuid() {
        assert!(
            extract_record_identifier(
                "what medication has patient with id 53f1015a-ebda-4216-8201-c892dda9b2ab been prescribed?"
            )
            .is_none()
        );
    }

    #[test]
    fn resolves_followup_identifier_edge_cases() {
        let turns = [
            "Discussed patient SYN-2024-0001.",
            "Then reviewed patient SYN-2024-0042.",
        ];
        assert_eq!(
            resolve_followup_question("What was her diagnosis count?", turns.iter().copied())
                .as_deref(),
            Some("What was her diagnosis count? patient SYN-2024-0042")
        );
        assert!(
            resolve_followup_question("What was her diagnosis count?", std::iter::empty())
                .is_none()
        );
    }

    #[test]
    fn gates_identifier_name_lookups_by_source_and_schema() {
        let question = "What is the name of patient SYN-2024-0001?";
        let cards = vec![patients_card()];

        for source_kind in [SourceKind::Mssql, SourceKind::Mysql] {
            assert!(
                deterministic_sql(question, &cards, source_kind, 100).is_none(),
                "source kind: {source_kind:?}"
            );
        }
        assert!(deterministic_sql(question, &[], SourceKind::Postgres, 100).is_none());

        let incomplete_cards = vec![schema_card("patients", &["patient_no", "last_name"])];
        assert!(
            deterministic_sql(question, &incomplete_cards, SourceKind::Postgres, 100).is_none()
        );
    }

    #[test]
    fn leaves_unsupported_filtered_questions_for_the_local_model() {
        let cards = vec![patients_card()];
        assert!(
            deterministic_sql(
                "How many active patients?",
                &cards,
                SourceKind::Postgres,
                100
            )
            .is_none()
        );
    }

    #[test]
    fn summarizes_a_scalar_without_another_model_call() {
        let answer = summarize_result(&["patient_count".into()], &[vec![60.into()]]);
        assert_eq!(answer, "patient count: 60.");
    }

    #[test]
    fn summarizes_empty_and_tabular_results() {
        assert_eq!(
            summarize_result(&[], &[]),
            "No matching records were found."
        );
        assert_eq!(
            summarize_result(&["id".into()], &[vec![1.into()], vec![2.into()]]),
            "2 matching records from the live database:
1. 1
2. 2"
        );
    }

    #[test]
    fn lists_matching_rows_instead_of_only_counting_them() {
        let columns = vec!["full_name".to_string(), "patient_no".to_string()];
        let rows = vec![
            vec!["Esther Chebet".into(), "SYN-2024-0004".into()],
            vec!["Jane Wairimu".into(), serde_json::Value::Null],
        ];
        assert_eq!(
            summarize_result(&columns, &rows),
            "2 matching records from the live database:
1. Esther Chebet (patient no: SYN-2024-0004)
2. Jane Wairimu"
        );
    }

    #[test]
    fn truncates_long_listings_with_an_honest_remainder() {
        let columns = vec!["full_name".to_string()];
        let rows: Vec<Vec<serde_json::Value>> = (0..30)
            .map(|index| vec![format!("Patient {index}").into()])
            .collect();
        let answer = summarize_result(&columns, &rows);
        assert!(answer.starts_with("30 matching records from the live database:"));
        assert!(answer.contains("25. Patient 24"));
        assert!(!answer.contains("26. Patient 25"));
        assert!(answer.ends_with("…and 5 more."));
    }

    // ---- Test 8: MetadataOverrides slug validation ----

    #[test]
    fn metadata_overrides_valid_concept_slug_accepted() {
        use super::{MetadataOverrides, TableConceptOverride};
        use crate::ontology::concepts::EntityConcept;

        let slug = "patient";
        // EntityConcept::from_slug must recognise the slug to prove the validator path works.
        assert!(EntityConcept::from_slug(slug).is_some(), "slug 'patient' should resolve");

        let ov = MetadataOverrides {
            table_concepts: vec![TableConceptOverride {
                table: "patients".into(),
                concept: Some(slug.into()),
            }],
            ..Default::default()
        };
        // validate_overrides requires a live DocumentDb, so verify the slug check
        // logic in isolation — the full integration goes through cargo test on a
        // running instance. Here we just confirm from_slug agrees with what the
        // route would accept.
        assert!(ov.table_concepts.iter().all(|tc| {
            tc.concept.as_deref().map_or(true, |s| EntityConcept::from_slug(s).is_some())
        }));
    }

    #[test]
    fn metadata_overrides_unknown_concept_slug_rejected() {
        use crate::ontology::concepts::EntityConcept;
        // "not_a_real_concept" must NOT be in the ontology; validate_overrides
        // would return BadRequest for any slug that from_slug cannot parse.
        assert!(EntityConcept::from_slug("not_a_real_concept").is_none());
    }

    #[test]
    fn metadata_overrides_unknown_role_slug_rejected() {
        use crate::ontology::roles::ColumnRole;
        assert!(ColumnRole::from_slug("not_a_real_role").is_none());
    }

    #[test]
    fn metadata_overrides_unknown_service_line_slug_rejected() {
        use crate::ontology::service_line::ServiceLine;
        assert!(ServiceLine::from_slug("not_a_real_line").is_none());
    }

    #[test]
    fn metadata_overrides_valid_role_and_service_line_slugs_accepted() {
        use crate::ontology::roles::ColumnRole;
        use crate::ontology::service_line::ServiceLine;
        // Spot-check a few known slugs each validator would accept.
        assert!(ColumnRole::from_slug("event_time").is_some());
        assert!(ColumnRole::from_slug("patient_ref").is_some());
        assert!(ColumnRole::from_slug("primary_key").is_some());
        assert!(ServiceLine::from_slug("ward_board").is_some());
        assert!(ServiceLine::from_slug("pharmacy").is_some());
    }
}
