//! Backend selector for the v3 intent router.
//!
//! Examines the available `SchemaBinding`s and the `Catalog` collections to
//! choose where a structured query should be executed.
//!
//! **Routing decides; it never executes.**  `select_backend` returns a
//! `V3Backend` variant.  Nothing in this file runs SQL.

use std::collections::HashMap;
use std::sync::Arc;

use crate::aggregation::catalog::Catalog;
use crate::config::Config;
use crate::ontology::{binding::SchemaBinding, service_line::ServiceLine};
use crate::router::entities::RouteEntities;

// ---------------------------------------------------------------------------
// V3Backend
// ---------------------------------------------------------------------------

/// The structured-query backend selected by the v3 router.
#[derive(Debug, Clone, PartialEq)]
pub enum V3Backend {
    /// Execute against a live SQL source.
    SourceSql {
        /// Matches `SourceSpec::id` / `SchemaBinding::source_id`.
        source_id: String,
        /// Physical table names the planner may reference for this line.
        scope: Vec<String>,
    },
    /// Execute as a DocumentDB aggregation over ingested records.
    DocDb {
        /// Catalog collection names the planner may reference.
        scope: Vec<String>,
    },
    /// No suitable structured backend; fall through to Semantic.
    None,
}

// ---------------------------------------------------------------------------
// Selection logic
// ---------------------------------------------------------------------------

/// Choose the best backend for a structured question.
///
/// Priority:
/// 1. **SourceSql** — a registered `SchemaBinding` exists whose binding for
///    `line` is usable (≥1 owned concept at confidence ≥ 0.55).
/// 2. **DocDb** — the `Catalog` has at least one collection whose name overlaps
///    with `entities.tables` or `line`'s concept names.
/// 3. **None** — falls through to semantic retrieval.
///
/// `config.binding_min_confidence` gates which service lines count as usable.
pub fn select_backend(
    bindings: &HashMap<String, Arc<SchemaBinding>>,
    catalog: &Catalog,
    config: &Config,
    line: Option<ServiceLine>,
    entities: &RouteEntities,
) -> V3Backend {
    let min_conf = config.binding_min_confidence;
    // 1 — try SourceSql: prefer the binding whose line is usable, then by table
    //     coverage regardless of line.
    for (source_id, binding) in bindings {
        let usable = line
            .map(|l| binding.line_is_usable(l, min_conf))
            .unwrap_or_else(|| !binding.usable_lines(min_conf).is_empty());

        if usable {
            let scope: Vec<String> = match line {
                Some(l) => binding
                    .tables_for_line(l)
                    .into_iter()
                    .map(|t| t.table_name.clone())
                    .collect(),
                None => binding.tables.iter().map(|t| t.table_name.clone()).collect(),
            };
            return V3Backend::SourceSql {
                source_id: source_id.clone(),
                scope,
            };
        }
    }

    // 2 — try DocDb: look for catalog collection overlap.
    let docdb_scope: Vec<String> = {
        let mut scope: Vec<String> = Vec::new();

        // Tables from entity hints
        for t in &entities.tables {
            if catalog.has_collection(t) {
                scope.push(t.clone());
            }
        }

        // Concept singular/plural names vs catalog keys
        if scope.is_empty() {
            use crate::ontology::concepts::DESCRIPTORS;
            for desc in DESCRIPTORS.iter() {
                if catalog.has_collection(desc.singular) {
                    scope.push(desc.singular.to_string());
                } else if catalog.has_collection(desc.plural) {
                    scope.push(desc.plural.to_string());
                }
            }
        }

        scope
    };

    if !docdb_scope.is_empty() {
        return V3Backend::DocDb { scope: docdb_scope };
    }

    V3Backend::None
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;
    use crate::ontology::{
        binding::{ColumnBinding, SchemaBinding, TableBinding},
        concepts::EntityConcept,
        roles::ColumnRole,
    };
    use chrono::Utc;

    fn empty_entities() -> RouteEntities {
        RouteEntities::default()
    }

    #[test]
    fn usable_binding_gives_source_sql() {
        let mut bindings = HashMap::new();
        let binding = SchemaBinding {
            source_id: "src1".into(),
            bound_at: Utc::now(),
            tables: vec![TableBinding {
                table_name: "prescriptions".into(),
                concept: EntityConcept::Prescription,
                confidence: 0.9,
                service_lines: vec![ServiceLine::Pharmacy],
                columns: vec![ColumnBinding {
                    column_name: "id".into(),
                    role: ColumnRole::PrimaryKey,
                    is_pii: false,
                    enum_values: vec![],
                }],
                patient_path: None,
                event_time_col: None,
                degraded: false,
            }],
            degraded: false,
            override_version: 0,
        };
        bindings.insert("src1".into(), Arc::new(binding));
        let config = Config::from_env();
        let catalog = Catalog::empty();

        let backend = select_backend(
            &bindings,
            &catalog,
            &config,
            Some(ServiceLine::Pharmacy),
            &empty_entities(),
        );
        assert!(
            matches!(backend, V3Backend::SourceSql { ref source_id, .. } if source_id == "src1"),
            "expected SourceSql for usable Pharmacy binding, got {:?}",
            backend
        );
    }

    #[test]
    fn no_binding_and_no_catalog_gives_none() {
        let bindings = HashMap::new();
        let catalog = Catalog::empty();
        let config = Config::from_env();
        let backend = select_backend(&bindings, &catalog, &config, None, &empty_entities());
        assert_eq!(backend, V3Backend::None);
    }

    #[test]
    fn unusable_line_falls_through_to_none_when_catalog_empty() {
        // Binding has only a Patient table (shared concept) → not usable for Maternity.
        let mut bindings = HashMap::new();
        let binding = SchemaBinding {
            source_id: "src1".into(),
            bound_at: Utc::now(),
            tables: vec![TableBinding {
                table_name: "patients".into(),
                concept: EntityConcept::Patient,
                confidence: 0.9,
                service_lines: vec![],
                columns: vec![],
                patient_path: Some(vec![]),
                event_time_col: None,
                degraded: false,
            }],
            degraded: false,
            override_version: 0,
        };
        bindings.insert("src1".into(), Arc::new(binding));
        let catalog = Catalog::empty();
        let config = Config::from_env();
        // Maternity is not usable → no SourceSql, no DocDb collections → None
        let backend = select_backend(
            &bindings,
            &catalog,
            &config,
            Some(ServiceLine::Maternity),
            &empty_entities(),
        );
        assert_eq!(backend, V3Backend::None);
    }
}
