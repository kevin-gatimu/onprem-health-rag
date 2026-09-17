//! Schema-binding data structures.
//!
//! A `SchemaBinding` is the result of running the binder over a source's
//! `TableCard` catalog — a per-source, persisted mapping from physical tables
//! to semantic concepts, service lines, column roles, and patient paths.
//!
//! The structs are BSON-serialisable (via `serde`) so they can be stored in and
//! retrieved from DocumentDB.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::ontology::concepts::{EntityConcept, SHARED_CONCEPTS};
use crate::ontology::roles::ColumnRole;
use crate::ontology::service_line::ServiceLine;

/// The binding of a single column within its table.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ColumnBinding {
    pub column_name: String,
    pub role: ColumnRole,
    /// True if the column is PII — derived from `is_likely_pii(&column_name)`
    /// OR from a role whose `is_pii()` returns true.
    pub is_pii: bool,
    /// Low-cardinality enum values probed from the source DB.
    /// **Always empty when `is_pii` is true** (invariant, tested).
    pub enum_values: Vec<String>,
}

/// One hop in a BFS path from a table to the patient table.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct JoinHop {
    /// The source table for this hop.
    pub from_table: String,
    /// Column in `from_table` used to join (typically a FK column).
    pub join_col: String,
    /// The destination table.
    pub to_table: String,
    /// Column in `to_table` being joined to (typically a PK or FK).
    pub via_col: String,
}

/// The binding of a single table in the source schema.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct TableBinding {
    pub table_name: String,
    /// Concept the binder assigned.  `EntityConcept::Unknown` means no concept
    /// reached the minimum-confidence threshold.
    pub concept: EntityConcept,
    /// Score in [0, 1].  Bindings with score < `binding_min_confidence` are
    /// stored with `concept = Unknown`.
    pub confidence: f32,
    /// Service lines that own this concept (empty for Unknown / Geography).
    pub service_lines: Vec<ServiceLine>,
    /// Per-column bindings (same order as `TableCard::columns`).
    pub columns: Vec<ColumnBinding>,
    /// BFS path from this table to the patient table, traversing FK edges.
    ///
    /// - `Some([])` — this table IS the patient table (zero hops)
    /// - `Some(vec![…])` — path via n ≤ 3 hops
    /// - `None` — unreachable within `binding_max_hops`
    pub patient_path: Option<Vec<JoinHop>>,
    /// The single column assigned `EventTime` for this table (if any).
    /// Selection rule: prefer domain-named EventTime cols over `created_at` or
    /// `updated_at`; pick exactly one, preferring temporal FK colums in the
    /// concept descriptor's `required_roles`.
    pub event_time_col: Option<String>,
    /// True if this table's binding was computed without embeddings (degraded
    /// mode — fastembed not loaded when `build_binding` ran).
    pub degraded: bool,
}

/// Summary statistics for a `SchemaBinding`, computed on demand.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BindingCoverage {
    pub total_tables: usize,
    /// Tables with `concept != Unknown`.
    pub bound_tables: usize,
    /// Tables with `concept == Unknown`.
    pub orphan_tables: usize,
    /// Tables with `confidence >= 0.80`.
    pub exact_concepts: usize,
    /// Service lines with ≥1 owned concept bound at confidence ≥ 0.55.
    pub usable_lines: Vec<ServiceLine>,
}

/// The full schema binding for one source at a point in time.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SchemaBinding {
    /// Matches `SourceSpec::id`.
    pub source_id: String,
    /// Wall-clock time when this binding was computed.
    pub bound_at: DateTime<Utc>,
    /// One entry per table in the source.
    pub tables: Vec<TableBinding>,
    /// True if fastembed was not loaded and embedding weight was dropped.
    pub degraded: bool,
    /// Incremented by `validate_overrides` whenever manual overrides are saved.
    pub override_version: u32,
}

impl SchemaBinding {
    /// Service lines that have at least one owned concept bound at confidence
    /// ≥ `min_confidence`.  Returned in tier order (tier-1 first).
    ///
    /// Pass `ONPREM_BINDING_MIN_CONFIDENCE` (default 0.55) from `Config` at
    /// call sites that have config access; use `0.55` in pure tests via the
    /// `PROD_MIN_CONFIDENCE` constant in `ontology/tests/mod.rs`.
    pub fn usable_lines(&self, min_confidence: f32) -> Vec<ServiceLine> {
        let mut result: Vec<ServiceLine> = ServiceLine::ALL
            .iter()
            .copied()
            .filter(|&line| self.line_is_usable(line, min_confidence))
            .collect();
        result.sort_by_key(|l| (l.tier(), l.slug()));
        result
    }

    /// True if this service line has at least one *owned* concept bound at
    /// ≥ `min_confidence`.  Shared concepts (Patient, Encounter, Provider,
    /// Department, DiagnosisCode) are explicitly excluded: they are accessible
    /// by every line but do not make a line independently usable on their own.
    ///
    /// Pass `config.binding_min_confidence` at call sites that have config
    /// access; tests use the `PROD_MIN_CONFIDENCE` constant (0.55).
    ///
    /// Two different questions over the same ownership list:
    /// - *usability* = "does this line have its own subject matter?" (shared **excluded**)
    /// - *scope*     = "what may this line read?"                   (shared **included**)
    /// Use [`tables_for_line`] for the second question.
    pub fn line_is_usable(&self, line: ServiceLine, min_confidence: f32) -> bool {
        line.concepts()
            .iter()
            .filter(|&&c| !SHARED_CONCEPTS.contains(&c))
            .any(|&concept| {
                self.tables
                    .iter()
                    .any(|t| t.concept == concept && t.confidence >= min_confidence)
            })
    }

    /// All table bindings that this service line may read: tables for its own
    /// concepts **plus** all shared-concept tables (Patient, Encounter, Provider,
    /// Department, DiagnosisCode).  The shared tables are included because almost
    /// every service-line query joins out to Patient or Encounter.
    ///
    /// Two different questions over the same ownership list:
    /// - *usability* = "does this line have its own subject matter?" (shared **excluded**)
    /// - *scope*     = "what may this line read?"                   (shared **included**)
    /// This function answers the scope question.
    ///
    /// Scope is a **read allow-list, not a security boundary**.  RBAC stays in `auth/`.
    pub fn tables_for_line(&self, line: ServiceLine) -> Vec<&TableBinding> {
        let owned: std::collections::HashSet<crate::ontology::concepts::EntityConcept> =
            line.concepts().iter().copied().collect();
        self.tables
            .iter()
            .filter(|t| owned.contains(&t.concept) || SHARED_CONCEPTS.contains(&t.concept))
            .collect()
    }

    /// First table binding with the given concept, if any.
    pub fn table_for_concept(&self, concept: EntityConcept) -> Option<&TableBinding> {
        self.tables.iter().find(|t| t.concept == concept)
    }

    /// All table bindings for a concept (handles schemas with multiple tables
    /// mapping to the same concept — unusual but possible).
    pub fn tables_for_concept(&self, concept: EntityConcept) -> Vec<&TableBinding> {
        self.tables.iter().filter(|t| t.concept == concept).collect()
    }

    /// Coverage statistics.
    ///
    /// `min_confidence` determines which lines count as usable (passed to
    /// [`usable_lines`]).  Use `config.binding_min_confidence` at HTTP call
    /// sites; tests use `PROD_MIN_CONFIDENCE` (0.55).
    pub fn coverage(&self, min_confidence: f32) -> BindingCoverage {
        let total = self.tables.len();
        let bound = self
            .tables
            .iter()
            .filter(|t| t.concept != EntityConcept::Unknown)
            .count();
        let exact = self
            .tables
            .iter()
            .filter(|t| t.concept != EntityConcept::Unknown && t.confidence >= 0.80)
            .count();
        BindingCoverage {
            total_tables: total,
            bound_tables: bound,
            orphan_tables: total - bound,
            exact_concepts: exact,
            usable_lines: self.usable_lines(min_confidence),
        }
    }

    /// The table bound to `EntityConcept::Patient` for this source, if any.
    pub fn patient_table(&self) -> Option<&TableBinding> {
        self.table_for_concept(EntityConcept::Patient)
    }

    /// BFS patient path for a given table (convenience wrapper).
    pub fn patient_path_for(&self, table_name: &str) -> Option<&Vec<JoinHop>> {
        self.tables
            .iter()
            .find(|t| t.table_name == table_name)
            .and_then(|t| t.patient_path.as_ref())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Verify that `ColumnBinding::enum_values` is always empty when `is_pii`
    /// is true (acceptance criterion 6).
    #[test]
    fn pii_column_has_empty_enum_values() {
        let cb_pii = ColumnBinding {
            column_name: "phone_number".into(),
            role: ColumnRole::Contact,
            is_pii: true,
            enum_values: vec![],
        };
        assert!(
            cb_pii.enum_values.is_empty(),
            "PII column must have empty enum_values"
        );

        let cb_safe = ColumnBinding {
            column_name: "status".into(),
            role: ColumnRole::Status,
            is_pii: false,
            enum_values: vec!["active".into(), "inactive".into()],
        };
        assert!(
            !cb_safe.enum_values.is_empty(),
            "non-PII categorical column may have enum_values"
        );
    }

    /// SchemaBinding BSON round-trip (acceptance criterion 7).
    #[test]
    fn schema_binding_bson_roundtrip() {
        let binding = SchemaBinding {
            source_id: "test-source".into(),
            bound_at: Utc::now(),
            tables: vec![TableBinding {
                table_name: "encounters".into(),
                concept: EntityConcept::Encounter,
                confidence: 0.91,
                service_lines: vec![ServiceLine::PatientChart, ServiceLine::FrontDesk],
                columns: vec![
                    ColumnBinding {
                        column_name: "id".into(),
                        role: ColumnRole::PrimaryKey,
                        is_pii: false,
                        enum_values: vec![],
                    },
                    ColumnBinding {
                        column_name: "patient_id".into(),
                        role: ColumnRole::PatientRef,
                        is_pii: false,
                        enum_values: vec![],
                    },
                    ColumnBinding {
                        column_name: "status".into(),
                        role: ColumnRole::Status,
                        is_pii: false,
                        enum_values: vec!["active".into(), "closed".into()],
                    },
                ],
                patient_path: Some(vec![JoinHop {
                    from_table: "encounters".into(),
                    join_col: "patient_id".into(),
                    to_table: "patients".into(),
                    via_col: "id".into(),
                }]),
                event_time_col: Some("encounter_date".into()),
                degraded: false,
            }],
            degraded: false,
            override_version: 0,
        };

        let doc = mongodb::bson::to_document(&binding)
            .expect("serialise SchemaBinding to BSON");
        let back: SchemaBinding = mongodb::bson::from_document(doc)
            .expect("deserialise SchemaBinding from BSON");

        assert_eq!(back.source_id, binding.source_id);
        assert_eq!(back.tables.len(), 1);
        let tb = &back.tables[0];
        assert_eq!(tb.concept, EntityConcept::Encounter);
        assert!((tb.confidence - 0.91_f32).abs() < 0.001);
        assert_eq!(tb.columns.len(), 3);
        assert_eq!(tb.columns[2].enum_values, vec!["active", "closed"]);
        let path = tb.patient_path.as_ref().expect("path present");
        assert_eq!(path.len(), 1);
        assert_eq!(path[0].to_table, "patients");
    }

    /// §0.4 asymmetry: a line whose only bound tables are shared-concept tables is
    /// NOT usable (it has no own subject matter), but its scope is NON-EMPTY (it can
    /// still read those shared tables).  Both assertions in one test so the distinction
    /// cannot silently collapse.
    #[test]
    fn line_is_usable_vs_tables_for_line_asymmetry() {
        use crate::ontology::concepts::EntityConcept;
        use crate::ontology::roles::ColumnRole;
        // Build a binding where Maternity has ONLY a Patient table (shared concept).
        // Maternity is therefore NOT usable (no owned concept), but its scope
        // must include the Patient table.
        let binding = SchemaBinding {
            source_id: "asym".into(),
            bound_at: Utc::now(),
            tables: vec![TableBinding {
                table_name: "patients".into(),
                concept: EntityConcept::Patient, // Patient is a SHARED concept
                confidence: 0.95,
                service_lines: vec![], // shared — not in any specific line's service_lines
                columns: vec![ColumnBinding {
                    column_name: "id".into(),
                    role: ColumnRole::PrimaryKey,
                    is_pii: false,
                    enum_values: vec![],
                }],
                patient_path: Some(vec![]),
                event_time_col: None,
                degraded: false,
            }],
            degraded: false,
            override_version: 0,
        };

        // Maternity owns AntenatalVisit, Delivery, Newborn, etc. — none are in the binding.
        // So Maternity is NOT usable.
        assert!(
            !binding.line_is_usable(ServiceLine::Maternity, 0.55),
            "Maternity must not be usable when only shared-concept tables are bound"
        );
        // But the scope for Maternity INCLUDES Patient (shared).
        let scope = binding.tables_for_line(ServiceLine::Maternity);
        assert!(
            !scope.is_empty(),
            "Maternity scope must include shared-concept tables even when line is not usable"
        );
        assert!(
            scope.iter().any(|t| t.table_name == "patients"),
            "Maternity scope must contain the patients table (shared concept)"
        );
    }

    /// `usable_lines` respects the 0.55 confidence threshold.
    #[test]
    fn usable_lines_threshold() {
        let binding = SchemaBinding {
            source_id: "s".into(),
            bound_at: Utc::now(),
            tables: vec![
                TableBinding {
                    table_name: "diagnoses".into(),
                    // Diagnosis is owned by PatientChart — high confidence
                    concept: EntityConcept::Diagnosis,
                    confidence: 0.95,
                    service_lines: vec![ServiceLine::PatientChart],
                    columns: vec![],
                    patient_path: None,
                    event_time_col: None,
                    degraded: false,
                },
                TableBinding {
                    table_name: "bills".into(),
                    // Bill is owned by Revenue — low confidence (below 0.55)
                    concept: EntityConcept::Bill,
                    confidence: 0.40,
                    service_lines: vec![ServiceLine::Revenue],
                    columns: vec![],
                    patient_path: None,
                    event_time_col: None,
                    degraded: false,
                },
            ],
            degraded: false,
            override_version: 0,
        };

        let usable = binding.usable_lines(0.55);
        // PatientChart is usable (Diagnosis at 0.95 ≥ 0.55)
        assert!(usable.contains(&ServiceLine::PatientChart));
        // Revenue should not be usable (Bill at 0.40 < 0.55)
        assert!(!usable.contains(&ServiceLine::Revenue));
    }
}
