//! Binder — resolves logical `QuerySpec` (concept + role refs) to physical
//! table/column names using a `SchemaBinding`.
//!
//! # Inputs
//! * `spec` — a `QuerySpec` where all `ColumnRef.physical` are `None`.
//! * `binding` — the per-source `SchemaBinding` from plan 01.
//! * `cards` — `TableCard` slice from the schema catalog; used to resolve FK
//!   paths between tables that are not on the patient path.
//! * `scope` — if non-empty, only tables named here may appear in the output.
//!
//! # Outputs
//! A new `QuerySpec` where every `ColumnRef.physical` is `Some`, or a `BindError`
//! describing what could not be resolved.

use thiserror::Error;

use crate::nl2sql::spec::TableCard;
use crate::ontology::binding::{JoinHop, SchemaBinding, TableBinding};
use crate::ontology::concepts::EntityConcept;
use crate::ontology::roles::ColumnRole;

use super::spec::{
    ColumnRef, Dimension, Filter, JoinKind, JoinRef, Measure, MeasureOp, Order, OrderTarget,
    QuerySpec, Subject, TimeScope,
};

// ---------------------------------------------------------------------------
// Errors
// ---------------------------------------------------------------------------

#[derive(Debug, Error, Clone, PartialEq)]
pub enum BindError {
    #[error("concept {0:?} has no bound table in this schema (or is EntityConcept::Unknown)")]
    NoSubject(EntityConcept),

    #[error("concept {concept:?} has no column with role {role:?}")]
    NoRole { concept: EntityConcept, role: ColumnRole },

    #[error("no FK join path from {0} to {1}")]
    NoJoinPath(String, String),

    #[error("role {role:?} on concept {concept:?} is ambiguous: {candidates:?}")]
    Ambiguous {
        concept: EntityConcept,
        role: ColumnRole,
        candidates: Vec<String>,
    },

    #[error("table {0} is outside the allowed scope")]
    OutOfScope(String),
}

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

/// Resolve all logical `ColumnRef`s in `spec` to physical `(table, column)` pairs.
pub fn bind(
    spec: QuerySpec,
    binding: &SchemaBinding,
    cards: &[TableCard],
    scope: &[String],
) -> Result<QuerySpec, BindError> {
    let binder = Binder { binding, cards, scope };
    binder.bind(spec)
}

// ---------------------------------------------------------------------------
// Internal binder
// ---------------------------------------------------------------------------

struct Binder<'a> {
    binding: &'a SchemaBinding,
    cards: &'a [TableCard],
    scope: &'a [String],
}

impl<'a> Binder<'a> {
    fn bind(&self, mut spec: QuerySpec) -> Result<QuerySpec, BindError> {
        // 1. Resolve subject.
        let subject_table = self.resolve_subject(&spec.subject)?;
        spec.subject.table = Some(subject_table.clone());

        // 2. Resolve all column refs in filters, measures, dimensions, time, order, projection.
        //    Collect required joins as we go.
        let mut joins: Vec<JoinRef> = Vec::new();

        spec.filters = spec.filters.into_iter().map(|f| self.resolve_filter(f, &subject_table, &mut joins)).collect::<Result<_, _>>()?;

        spec.measures = spec.measures.into_iter().map(|m| self.resolve_measure(m, &subject_table, &mut joins)).collect::<Result<_, _>>()?;

        spec.dimensions = spec.dimensions.into_iter().map(|d| self.resolve_dimension(d, &subject_table, &mut joins)).collect::<Result<_, _>>()?;

        if let Some(ts) = spec.time {
            spec.time = Some(self.resolve_time_scope(ts, &subject_table, &mut joins)?);
        }

        spec.order = spec.order.into_iter().map(|o| self.resolve_order(o, &subject_table, &mut joins)).collect::<Result<_, _>>()?;

        // Projection columns are best-effort: silently drop any that can't be resolved
        // (missing role on subject table or its FK neighbours). The compiler falls back
        // to "t0.*" when the projection is empty, so the query remains valid.
        spec.projection = spec.projection.into_iter()
            .filter_map(|cr| self.resolve_col_ref(cr, &subject_table, &mut joins).ok())
            .collect();

        spec.joins = joins;

        Ok(spec)
    }

    // -----------------------------------------------------------------------
    // Subject
    // -----------------------------------------------------------------------

    fn resolve_subject(&self, subject: &Subject) -> Result<String, BindError> {
        if subject.concept == EntityConcept::Unknown {
            return Err(BindError::NoSubject(EntityConcept::Unknown));
        }
        let tb = self
            .binding
            .table_for_concept(subject.concept)
            .ok_or(BindError::NoSubject(subject.concept))?;
        if !self.scope_ok(&tb.table_name) {
            return Err(BindError::OutOfScope(tb.table_name.clone()));
        }
        Ok(tb.table_name.clone())
    }

    // -----------------------------------------------------------------------
    // Column reference resolution
    // -----------------------------------------------------------------------

    fn resolve_col_ref(
        &self,
        mut cr: ColumnRef,
        subject_table: &str,
        joins: &mut Vec<JoinRef>,
    ) -> Result<ColumnRef, BindError> {
        if cr.physical.is_some() {
            return Ok(cr); // already bound (hand-built spec)
        }

        // Try the subject table first.
        if let Some(resolved) = self.find_col_in_table(subject_table, cr.concept, cr.role, cr.name_hint.as_deref()) {
            cr.physical = Some(resolved);
            return Ok(cr);
        }

        // Search one-hop FK neighbours.
        if let Some((hop_tbl, resolved_col)) =
            self.find_col_via_fk(subject_table, cr.concept, cr.role, cr.name_hint.as_deref(), joins)?
        {
            cr.physical = Some((hop_tbl, resolved_col));
            return Ok(cr);
        }

        // Patient path as fallback for Patient-scoped roles.
        if cr.concept == EntityConcept::Patient {
            if let Some(patient_tbl) = self.binding.patient_table() {
                let patient_name = patient_tbl.table_name.clone();
                if let Some(resolved) = self.find_col_in_table(&patient_name, cr.concept, cr.role, cr.name_hint.as_deref()) {
                    // Add a join via patient_path if not already present.
                    if subject_table != patient_name {
                        self.ensure_patient_join(subject_table, joins)?;
                    }
                    cr.physical = Some(resolved);
                    return Ok(cr);
                }
            }
        }

        Err(BindError::NoRole { concept: cr.concept, role: cr.role })
    }

    fn find_col_in_table(
        &self,
        table_name: &str,
        concept: EntityConcept,
        role: ColumnRole,
        hint: Option<&str>,
    ) -> Option<(String, String)> {
        let tb = self.binding.tables.iter().find(|t| t.table_name.eq_ignore_ascii_case(table_name))?;
        // Only use the table if its concept matches or if we're searching by table name directly.
        let col = self.binding.column_with_role_hint_in_table(tb, role, hint)?;
        Some((table_name.to_string(), col.column_name.clone()))
    }

    fn find_col_via_fk(
        &self,
        subject_table: &str,
        concept: EntityConcept,
        role: ColumnRole,
        hint: Option<&str>,
        joins: &mut Vec<JoinRef>,
    ) -> Result<Option<(String, String)>, BindError> {
        // Look for a FK from the subject table to a table bound to `concept`.
        // Use TableCard.fk_edges which has the FK relationships.
        let card = self.cards.iter().find(|c| c.table_name.eq_ignore_ascii_case(subject_table));
        let Some(card) = card else { return Ok(None) };

        // Determine the subject table's own concept so we can distinguish:
        //   (a) same-concept lookup  → role absent on subject → Ok(None) → caller emits NoRole
        //   (b) cross-concept lookup → no FK edge reaches concept → Err(NoJoinPath)
        let subject_concept = self.binding.tables.iter()
            .find(|t| t.table_name.eq_ignore_ascii_case(subject_table))
            .map(|tb| tb.concept)
            .unwrap_or(EntityConcept::Unknown);
        let is_cross_concept = concept != EntityConcept::Unknown && concept != subject_concept;

        // Track whether any FK edge leads to a table bound to the wanted concept.
        let mut concept_reachable = false;

        for edge in &card.fk_edges {
            // Find the target table.
            let target_binding = self.binding.tables.iter()
                .find(|t| t.table_name.eq_ignore_ascii_case(&edge.ref_table));
            let Some(target_tb) = target_binding else { continue };

            // Check if target matches the concept we're looking for.
            if target_tb.concept != concept && concept != EntityConcept::Unknown {
                continue;
            }
            if !self.scope_ok(&target_tb.table_name) {
                continue;
            }

            // A table bound to the wanted concept is reachable via this FK edge.
            concept_reachable = true;

            // Check if the target has the role we need.
            let col = self.binding.column_with_role_hint_in_table(target_tb, role, hint);
            let Some(col) = col else { continue };

            // Add a join if not already present.
            let hop = JoinHop {
                from_table: subject_table.to_string(),
                join_col: edge.column.clone(),
                to_table: target_tb.table_name.clone(),
                via_col: edge.ref_column.clone(),
            };
            if !joins.iter().any(|j| j.hops.iter().any(|h| h.to_table == target_tb.table_name)) {
                joins.push(JoinRef {
                    hops: vec![hop],
                    alias: format!("t{}", joins.len() + 1),
                    kind: JoinKind::Inner,
                });
            }
            return Ok(Some((target_tb.table_name.clone(), col.column_name.clone())));
        }

        // For cross-concept lookups: if FK edges exist but none leads to a table
        // bound to the wanted concept, the join path is genuinely absent.
        // Distinguish this from "role not found on a reachable table" (Ok(None)).
        if is_cross_concept && !concept_reachable && !card.fk_edges.is_empty() {
            return Err(BindError::NoJoinPath(
                subject_table.to_string(),
                format!("{:?}", concept),
            ));
        }

        Ok(None)
    }

    fn ensure_patient_join(
        &self,
        subject_table: &str,
        joins: &mut Vec<JoinRef>,
    ) -> Result<(), BindError> {
        if let Some(hops) = self.binding.patient_path_for(subject_table) {
            if !hops.is_empty() {
                // Add a join for the patient path if not already present.
                if let Some(first_hop) = hops.first() {
                    let target = first_hop.to_table.clone();
                    if !joins.iter().any(|j| j.hops.iter().any(|h| h.to_table == target)) {
                        joins.push(JoinRef {
                            hops: hops.clone(),
                            alias: format!("t{}", joins.len() + 1),
                            kind: JoinKind::Left,
                        });
                    }
                }
            }
        }
        Ok(())
    }

    // -----------------------------------------------------------------------
    // Specialized resolution methods
    // -----------------------------------------------------------------------

    fn resolve_filter(&self, mut f: Filter, subject_table: &str, joins: &mut Vec<JoinRef>) -> Result<Filter, BindError> {
        f.column = self.resolve_col_ref(f.column, subject_table, joins)?;
        Ok(f)
    }

    fn resolve_measure(&self, mut m: Measure, subject_table: &str, joins: &mut Vec<JoinRef>) -> Result<Measure, BindError> {
        if let Some(target) = m.target {
            m.target = Some(self.resolve_col_ref(target, subject_table, joins)?);
        }
        // For Rate, also resolve the numerator filter.
        if let MeasureOp::Rate { numerator } = m.op {
            let resolved_num = self.resolve_filter(*numerator, subject_table, joins)?;
            m.op = MeasureOp::Rate { numerator: Box::new(resolved_num) };
        }
        Ok(m)
    }

    fn resolve_dimension(&self, mut d: Dimension, subject_table: &str, joins: &mut Vec<JoinRef>) -> Result<Dimension, BindError> {
        d.column = self.resolve_col_ref(d.column, subject_table, joins)?;
        Ok(d)
    }

    fn resolve_time_scope(&self, mut ts: TimeScope, subject_table: &str, joins: &mut Vec<JoinRef>) -> Result<TimeScope, BindError> {
        // Default: use EventTime column of subject if not already bound.
        if ts.column.physical.is_none() {
            // Try EventTime first, then StartTime/EndTime.
            let roles = [ColumnRole::EventTime, ColumnRole::StartTime, ColumnRole::EndTime];
            let mut resolved = None;
            for role in roles {
                let cr = ColumnRef {
                    concept: ts.column.concept,
                    role,
                    name_hint: ts.column.name_hint.clone(),
                    physical: None,
                };
                if let Ok(r) = self.resolve_col_ref(cr, subject_table, joins) {
                    if r.physical.is_some() {
                        resolved = Some(r);
                        break;
                    }
                }
            }
            ts.column = resolved.ok_or(BindError::NoRole {
                concept: ts.column.concept,
                role: ColumnRole::EventTime,
            })?;
        }
        Ok(ts)
    }

    fn resolve_order(&self, mut o: Order, subject_table: &str, joins: &mut Vec<JoinRef>) -> Result<Order, BindError> {
        if let OrderTarget::Column(cr) = o.target {
            o.target = OrderTarget::Column(self.resolve_col_ref(cr, subject_table, joins)?);
        }
        Ok(o)
    }

    // -----------------------------------------------------------------------
    // Helpers
    // -----------------------------------------------------------------------

    fn scope_ok(&self, table_name: &str) -> bool {
        if self.scope.is_empty() {
            return true;
        }
        self.scope.iter().any(|s| s.eq_ignore_ascii_case(table_name))
    }
}

// ---------------------------------------------------------------------------
// SchemaBinding extension: column lookup within a TableBinding
// ---------------------------------------------------------------------------

/// Helper method used only by the binder; added as a free function over a
/// `TableBinding` reference since we cannot call `self.binding.*` from within
/// a closure that also borrows `self`.
impl SchemaBinding {
    /// Find the first column in `tb` that has `role`.
    /// If `hint` is given, prefer a column whose name contains the hint (case-insensitive substring);
    /// fall back to the first role match when no column contains the hint.
    pub(crate) fn column_with_role_hint_in_table<'a>(
        &self,
        tb: &'a TableBinding,
        role: ColumnRole,
        hint: Option<&str>,
    ) -> Option<&'a crate::ontology::binding::ColumnBinding> {
        let mut first_match = None;
        for cb in &tb.columns {
            if cb.role == role {
                if let Some(h) = hint {
                    if cb.column_name.to_ascii_lowercase().contains(&h.to_ascii_lowercase()) {
                        return Some(cb);
                    }
                }
                if first_match.is_none() {
                    first_match = Some(cb);
                }
            }
        }
        first_match
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ontology::binding::{ColumnBinding, JoinHop, SchemaBinding, TableBinding};
    use crate::ontology::service_line::ServiceLine;
    use chrono::Utc;

    use super::super::spec::*;

    fn make_binding() -> SchemaBinding {
        SchemaBinding {
            source_id: "test".into(),
            bound_at: Utc::now(),
            tables: vec![
                TableBinding {
                    table_name: "encounters".to_string(),
                    concept: EntityConcept::Encounter,
                    confidence: 0.95,
                    service_lines: vec![ServiceLine::PatientChart],
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
                        ColumnBinding {
                            column_name: "encounter_date".into(),
                            role: ColumnRole::EventTime,
                            is_pii: false,
                            enum_values: vec![],
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
                },
                TableBinding {
                    table_name: "patients".to_string(),
                    concept: EntityConcept::Patient,
                    confidence: 0.99,
                    service_lines: vec![],
                    columns: vec![
                        ColumnBinding {
                            column_name: "id".into(),
                            role: ColumnRole::PrimaryKey,
                            is_pii: false,
                            enum_values: vec![],
                        },
                        ColumnBinding {
                            column_name: "gender".into(),
                            role: ColumnRole::Gender,
                            is_pii: false,
                            enum_values: vec!["male".into(), "female".into()],
                        },
                    ],
                    patient_path: Some(vec![]),
                    event_time_col: None,
                    degraded: false,
                },
            ],
            degraded: false,
            override_version: 0,
        }
    }

    #[test]
    fn binds_simple_count_spec() {
        let binding = make_binding();
        let spec = QuerySpec {
            subject: Subject { concept: EntityConcept::Encounter, table: None },
            shape: Shape::Scalar,
            measures: vec![Measure { op: MeasureOp::Count, target: None, alias: "count".into() }],
            dimensions: vec![],
            filters: vec![],
            time: None,
            order: vec![],
            limit: None,
            joins: vec![],
            projection: vec![],
            provenance: SpecProvenance { rule: "R1".into(), focus_subs: vec![] },
        };
        let bound = bind(spec, &binding, &[], &[]).unwrap();
        assert_eq!(bound.subject.table.as_deref(), Some("encounters"));
    }

    #[test]
    fn binds_filter_column_on_subject_table() {
        let binding = make_binding();
        let spec = QuerySpec {
            subject: Subject { concept: EntityConcept::Encounter, table: None },
            shape: Shape::Scalar,
            measures: vec![Measure { op: MeasureOp::Count, target: None, alias: "count".into() }],
            dimensions: vec![],
            filters: vec![Filter {
                column: ColumnRef::logical(EntityConcept::Encounter, ColumnRole::Status),
                op: FilterOp::Eq,
                value: FilterValue::Str("active".into()),
            }],
            time: None,
            order: vec![],
            limit: None,
            joins: vec![],
            projection: vec![],
            provenance: SpecProvenance { rule: "R1".into(), focus_subs: vec![] },
        };
        let bound = bind(spec, &binding, &[], &[]).unwrap();
        let (tbl, col) = bound.filters[0].column.physical.as_ref().unwrap();
        assert_eq!(tbl, "encounters");
        assert_eq!(col, "status");
    }

    #[test]
    fn rejects_unknown_concept_as_subject() {
        let binding = make_binding();
        let spec = QuerySpec {
            subject: Subject { concept: EntityConcept::Unknown, table: None },
            shape: Shape::Scalar,
            measures: vec![],
            dimensions: vec![],
            filters: vec![],
            time: None,
            order: vec![],
            limit: None,
            joins: vec![],
            projection: vec![],
            provenance: SpecProvenance { rule: "R1".into(), focus_subs: vec![] },
        };
        let err = bind(spec, &binding, &[], &[]).unwrap_err();
        assert!(matches!(err, BindError::NoSubject(_)));
    }

    /// §0.6 acceptance test: `NoJoinPath` is emitted when a cross-concept join
    /// is required but no FK edge from the subject table reaches a table bound
    /// to the wanted concept.  The subject table DOES have FK edges (to Patient),
    /// but none lead to Department — so the error must be `NoJoinPath`, not
    /// `NoRole` (which would fire if the concept table existed but lacked the role).
    #[test]
    fn no_join_path_when_fk_does_not_reach_concept() {
        use crate::nl2sql::spec::TableCard;
        use crate::ontology::binding::ColumnBinding;

        // Build a binding: Encounter → Patient (FK exists), but no Department table.
        let binding = SchemaBinding {
            source_id: "test".into(),
            bound_at: chrono::Utc::now(),
            tables: vec![
                TableBinding {
                    table_name: "encounters".to_string(),
                    concept: EntityConcept::Encounter,
                    confidence: 0.95,
                    service_lines: vec![],
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
                            column_name: "encounter_date".into(),
                            role: ColumnRole::EventTime,
                            is_pii: false,
                            enum_values: vec![],
                        },
                    ],
                    patient_path: None,
                    event_time_col: Some("encounter_date".into()),
                    degraded: false,
                },
                TableBinding {
                    table_name: "patients".to_string(),
                    concept: EntityConcept::Patient,
                    confidence: 0.99,
                    service_lines: vec![],
                    columns: vec![ColumnBinding {
                        column_name: "id".into(),
                        role: ColumnRole::PrimaryKey,
                        is_pii: false,
                        enum_values: vec![],
                    }],
                    patient_path: Some(vec![]),
                    event_time_col: None,
                    degraded: false,
                },
            ],
            degraded: false,
            override_version: 0,
        };

        // Cards: encounters has a FK to patients but NOT to any department table.
        let cards = vec![TableCard {
            source_id: "test".into(),
            table_name: "encounters".into(),
            row_count: 100,
            columns: vec![],
            fk_edges: vec![crate::nl2sql::spec::CardFkEdge {
                column: "patient_id".into(),
                ref_table: "patients".into(),
                ref_column: "id".into(),
            }],
            card_vector: None,
            card_text: "encounters".into(),
        }];

        // Ask for a Department.Description column from the Encounter subject.
        // Since no FK from encounters leads to a Department-bound table,
        // we expect NoJoinPath — not NoRole.
        let spec = QuerySpec {
            subject: Subject { concept: EntityConcept::Encounter, table: None },
            shape: Shape::Grouped,
            measures: vec![Measure { op: MeasureOp::Count, target: None, alias: "count".into() }],
            dimensions: vec![Dimension {
                column: ColumnRef::logical(EntityConcept::Department, ColumnRole::Description),
                label: "department".into(),
            }],
            filters: vec![],
            time: None,
            order: vec![],
            limit: None,
            joins: vec![],
            projection: vec![],
            provenance: SpecProvenance { rule: "R1".into(), focus_subs: vec![] },
        };

        let allowed: Vec<String> = cards.iter().map(|c| c.table_name.clone()).collect();
        let err = bind(spec, &binding, &cards, &allowed).unwrap_err();
        assert!(
            matches!(err, BindError::NoJoinPath(_, _)),
            "expected NoJoinPath for Encounter→Department with no FK path; got: {:?}",
            err
        );
    }

    #[test]
    fn respects_scope() {
        let binding = make_binding();
        let spec = QuerySpec {
            subject: Subject { concept: EntityConcept::Encounter, table: None },
            shape: Shape::Scalar,
            measures: vec![],
            dimensions: vec![],
            filters: vec![],
            time: None,
            order: vec![],
            limit: None,
            joins: vec![],
            projection: vec![],
            provenance: SpecProvenance { rule: "R1".into(), focus_subs: vec![] },
        };
        // Scope does not include "encounters" → should fail.
        let err = bind(spec, &binding, &[], &["patients".to_string()]).unwrap_err();
        assert!(matches!(err, BindError::OutOfScope(_)));
    }
}
