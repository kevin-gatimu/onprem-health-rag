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

use std::collections::HashSet;

use thiserror::Error;

use crate::nl2sql::spec::{CardFkEdge, TableCard};
use crate::ontology::binding::{JoinHop, SchemaBinding, TableBinding};
use crate::ontology::concepts::{descriptor, EntityConcept};
use crate::ontology::roles::{self, ColumnRole};

use super::spec::{
    ColumnRef, DerivedDuration, Dimension, Filter, FilterOp, FilterValue, JoinKind, JoinRef,
    Measure, MeasureOp, Order, OrderTarget, QuerySpec, RelatedPhysical, RelatedScope, Shape,
    Subject, TimeScope, ValueExpr,
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

    /// More than one column on the target table satisfies `role` (and `hint`, if
    /// given).  Per plans/new/03b-defect-closure-brief.md and the follow-up defect
    /// closure task: ambiguity is a refusal condition, not a tie to break by
    /// column declaration order.  Never widen this to "pick the first" — that is
    /// exactly the defect class this variant exists to close (see `qs-01`:
    /// `incident_reports` has both `occurred_at` and `reported_at`; picking
    /// whichever came first in the table produced a wrong answer that looked
    /// plausible).
    #[error("role {role:?} on concept {concept:?} is ambiguous: {candidates:?}")]
    Ambiguous {
        concept: EntityConcept,
        role: ColumnRole,
        candidates: Vec<String>,
    },

    #[error("table {0} is outside the allowed scope")]
    OutOfScope(String),

    /// Every candidate value in an `In(...)` filter is absent from the column's
    /// known domain (`enum_values` is non-empty and the intersection is empty).
    /// Returning this instead of emitting an impossible `IN ()` prevents the
    /// pipeline from producing a query that looks correct but returns zero rows.
    #[error("In filter on {concept:?}.{role:?} has no values in the column's domain")]
    UnsatisfiableFilter { concept: EntityConcept, role: ColumnRole },
}

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

/// Resolve all logical `ColumnRef`s in `spec` to physical `(table, column)` pairs.
///
/// `question` is the raw natural-language question that produced `spec`. It is
/// used ONLY as disambiguation evidence when a role is otherwise ambiguous on a
/// table (see `column_with_role_hint_in_table`'s stage A) — never to select a
/// table/concept, and never compared against any single fixture question (that
/// would be the question-text special-casing the defect-closure brief forbids).
/// Passing `""` disables stage A cleanly (no tokens survive), which is what
/// hand-built specs in tests want.
pub fn bind(
    spec: QuerySpec,
    binding: &SchemaBinding,
    cards: &[TableCard],
    scope: &[String],
    question: &str,
) -> Result<QuerySpec, BindError> {
    let binder = Binder { binding, cards, scope, question };
    binder.bind(spec)
}

// ---------------------------------------------------------------------------
// Internal binder
// ---------------------------------------------------------------------------

struct Binder<'a> {
    binding: &'a SchemaBinding,
    cards: &'a [TableCard],
    scope: &'a [String],
    /// Raw question text, carried only for role disambiguation evidence
    /// (see `bind`'s doc comment).
    question: &'a str,
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
        //
        // Exception — Shape::Lookup's implicit Status slot: a `Lookup` ("tell me about
        // X") presents a single record as a definitive answer, unlike `List`'s tabular,
        // lower-stakes decoration. The standard-list-projection template requests
        // `ColumnRole::Status` unconditionally, with no `name_hint` (nothing in the
        // question asked about status) — a generic template guess, not evidence. When
        // that guess resolves on a concept whose ontology descriptor does not list
        // `Status` among `required_roles`, the match came from a bare name-token
        // collision (any `_status`-suffixed column qualifies — see roles.rs) rather than
        // the concept's own definition of what "status" means. Showing it under a
        // generic "Status" heading asserts something the schema doesn't actually
        // support (patients.marital_status is not the patient's operational status).
        // Refuse the whole lookup rather than presenting it — see `pc-02` in the defect
        // closure task. This does not touch `Shape::List`'s Status/EventTime slots
        // (e.g. `Claim` doesn't require Status either, but a decorative status column in
        // a filtered list is not read as "the one fact about this record").
        if spec.shape == Shape::Lookup {
            for cr in spec.projection.iter().filter_map(ValueExpr::as_column) {
                if cr.role == ColumnRole::Status && cr.name_hint.is_none() {
                    let required = descriptor(cr.concept).required_roles;
                    if !required.contains(&ColumnRole::Status)
                        && self.resolve_col_ref(cr.clone(), &subject_table, &mut joins).is_ok()
                    {
                        return Err(BindError::NoRole { concept: cr.concept, role: ColumnRole::Status });
                    }
                }
            }
        }
        // A projection slot asking for the same (concept, role) the question was
        // ALSO filtered on is asking to display the exact fact the query was
        // narrowed by — not a second, independent guess at "some column with
        // this role". Reuse the filter's own literal as stage-B evidence here
        // too, so e.g. a `Status` projection column resolves to the same column
        // the `status = 'pending'` predicate already proved unambiguous, instead
        // of re-deriving it from scratch with no literal to disambiguate by.
        // This is structural (spec-level), not a match on question text.
        let filter_literals: Vec<(EntityConcept, ColumnRole, FilterValue)> = spec.filters.iter()
            .map(|f| (f.column.concept, f.column.role, f.value.clone()))
            .collect();
        // ASYMMETRY — projection silently drops unresolvable columns (including
        // BindError::Ambiguous), while measure and filter resolution (above) both
        // propagate errors and refuse the whole query.
        //
        // This is deliberate: a decorative projected column that cannot be
        // disambiguated is silently dropped rather than failing the query, because
        // the user did not ask for that specific column — it was added by the
        // template to enrich the result set.  A missing decoration is harmless;
        // the query answer is still correct.
        //
        // Measures and filters, by contrast, MUST propagate ambiguity: a measure
        // whose column cannot be chosen unambiguously would produce a wrong
        // aggregate, and a filter whose column is ambiguous changes *which rows*
        // are returned.  In both cases the correct response is to refuse.
        //
        // rev-02 ("Which claims were rejected?") is the worked example: the
        // pipeline requests three projected columns (BusinessId, Status, EventTime)
        // but EventTime on insurance_claims is ambiguous (occurred_at vs
        // reported_at), so that slot is silently dropped and the query emits 2
        // projected columns instead of 3 — still a valid, correct answer.
        //
        // A projected *derived duration* (plan 03g) is NOT covered by that
        // asymmetry and propagates like a measure: it is never template
        // decoration — nothing but the question itself puts an elapsed-time
        // column in a select list — and dropping it would answer a question
        // about length of stay with a list that does not contain the stay.
        let mut projection: Vec<ValueExpr> = Vec::new();
        for v in std::mem::take(&mut spec.projection) {
            match v {
                ValueExpr::Column(cr) => {
                    let literal = filter_literals.iter()
                        .find(|(c, r, _)| *c == cr.concept && *r == cr.role)
                        .map(|(_, _, v)| v);
                    if let Ok(r) = self.resolve_col_ref_ex(cr, &subject_table, &mut joins, literal) {
                        projection.push(ValueExpr::Column(r));
                    }
                }
                ValueExpr::Duration(d) => {
                    let r = self.resolve_duration(d, &subject_table, &mut joins)?;
                    projection.push(ValueExpr::Duration(r));
                }
            }
        }
        spec.projection = projection;

        // Duration thresholds change WHICH ROWS are returned, so an unbound or
        // ambiguous endpoint refuses the whole query (plan 03g §4).
        spec.duration_filters = spec.duration_filters.into_iter()
            .map(|mut df| {
                df.duration = self.resolve_duration(df.duration, &subject_table, &mut joins)?;
                Ok(df)
            })
            .collect::<Result<_, BindError>>()?;

        spec.joins = joins;

        // Resolve RelatedScopes (reverse-FK semi-join / anti-join, plan 03f).
        spec.related = self.bind_related_scopes(spec.related, &subject_table)?;

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
        cr: ColumnRef,
        subject_table: &str,
        joins: &mut Vec<JoinRef>,
    ) -> Result<ColumnRef, BindError> {
        self.resolve_col_ref_ex(cr, subject_table, joins, None)
    }

    /// Same as `resolve_col_ref`, but takes an optional filter literal used as
    /// stage-B (value-domain) disambiguation evidence when the role is
    /// ambiguous on the candidate table. Only `resolve_filter` has a literal
    /// to offer; every other caller goes through `resolve_col_ref` (`None`).
    fn resolve_col_ref_ex(
        &self,
        mut cr: ColumnRef,
        subject_table: &str,
        joins: &mut Vec<JoinRef>,
        literal: Option<&FilterValue>,
    ) -> Result<ColumnRef, BindError> {
        if cr.physical.is_some() {
            return Ok(cr); // already bound (hand-built spec)
        }

        // Try the subject table first.
        if let Some(resolved) = self.find_col_in_table(subject_table, cr.concept, cr.role, cr.name_hint.as_deref(), literal)? {
            cr.physical = Some(resolved);
            return Ok(cr);
        }

        // Search one-hop FK neighbours.
        if let Some((hop_tbl, resolved_col)) =
            self.find_col_via_fk(subject_table, cr.concept, cr.role, cr.name_hint.as_deref(), joins, literal)?
        {
            cr.physical = Some((hop_tbl, resolved_col));
            return Ok(cr);
        }

        // Patient path as fallback for Patient-scoped roles.
        if cr.concept == EntityConcept::Patient {
            if let Some(patient_tbl) = self.binding.patient_table() {
                let patient_name = patient_tbl.table_name.clone();
                if let Some(resolved) = self.find_col_in_table(&patient_name, cr.concept, cr.role, cr.name_hint.as_deref(), literal)? {
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

    /// Look up a `(role, hint)` column within a single named table.
    ///
    /// Returns:
    /// * `Ok(Some(_))` — exactly one column on this table satisfies the role
    ///   (and hint, if given); bind it.
    /// * `Ok(None)` — the table doesn't exist, its concept doesn't match, or zero
    ///   columns satisfy the role/hint; the caller tries the next mechanism
    ///   (FK hop, patient path) as if this role were simply absent here.
    /// * `Err(BindError::Ambiguous)` — more than one column satisfies the role
    ///   (and hint).  This is NOT "try the next mechanism" — ambiguity on the
    ///   table that was actually asked about must refuse, not silently look
    ///   elsewhere for a column that might disambiguate by accident.
    fn find_col_in_table(
        &self,
        table_name: &str,
        concept: EntityConcept,
        role: ColumnRole,
        hint: Option<&str>,
        literal: Option<&FilterValue>,
    ) -> Result<Option<(String, String)>, BindError> {
        let Some(tb) = self.binding.tables.iter().find(|t| t.table_name.eq_ignore_ascii_case(table_name)) else {
            return Ok(None);
        };
        // Only use the table if its concept matches the requested concept, or if
        // the caller has not specified a concept (Unknown = any).  Without this
        // guard the function silently returns a column from a table whose concept
        // does not match, allowing wrong tables to supply measure columns — the
        // original source of AVG(length_of_stay_days) FROM admissions appearing in
        // triage/surgery/lab-turnaround queries (plans/new/03b §3 Family D).
        if concept != EntityConcept::Unknown && tb.concept != concept {
            return Ok(None);
        }
        match self.binding.column_with_role_hint_in_table(tb, role, hint, concept, self.question, literal) {
            RoleLookup::Unique(col) => Ok(Some((table_name.to_string(), col.column_name.clone()))),
            RoleLookup::Absent => Ok(None),
            RoleLookup::Ambiguous(candidates) => Err(BindError::Ambiguous { concept, role, candidates }),
        }
    }

    fn find_col_via_fk(
        &self,
        subject_table: &str,
        concept: EntityConcept,
        role: ColumnRole,
        hint: Option<&str>,
        joins: &mut Vec<JoinRef>,
        literal: Option<&FilterValue>,
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

        // Every FK edge that reaches a table bound to `concept` (in scope) AND
        // has a Unique column for (role, hint) there. More than one such edge
        // is ambiguous about WHICH RELATIONSHIP to follow (e.g. `admissions`
        // has both `admitting_dr` and `attending_dr` pointing at `providers`)
        // — a different question from "which column on the target table", but
        // the same defect class: taking the first-declared edge is exactly the
        // "pick the first" behaviour this module exists to close. Collected
        // rather than returned-on-first-hit so that can be detected.
        let mut reachable: Vec<(&CardFkEdge, &TableBinding, crate::ontology::binding::ColumnBinding)> = Vec::new();

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

            // Check if the target has the role we need.  Ambiguity WITHIN the
            // target table (two columns there both satisfy role+hint) refuses
            // immediately, same as the subject-table case in `find_col_in_table`
            // (stages A-C already ran inside `column_with_role_hint_in_table`;
            // if it still came back Ambiguous, nothing on the target table itself
            // disambiguates it).
            match self.binding.column_with_role_hint_in_table(target_tb, role, hint, concept, self.question, literal) {
                RoleLookup::Unique(col) => reachable.push((edge, target_tb, col.clone())),
                RoleLookup::Absent => continue,
                RoleLookup::Ambiguous(candidates) => {
                    return Err(BindError::Ambiguous { concept, role, candidates });
                }
            }
        }

        let chosen = match reachable.len() {
            0 => None,
            1 => reachable.into_iter().next(),
            _ => {
                // Multiple FK edges reach the wanted concept with the role
                // available on each target. Disambiguate by the SAME
                // question-evidence mechanism as stage A, applied to the
                // edge's OWN column name (e.g. "admitting_dr" vs
                // "attending_dr") instead of the target column's name — the
                // question names the RELATIONSHIP ("the admitting doctor"),
                // not a column on `providers`.
                let q_tokens = question_evidence_tokens(self.question);
                let mut matches: Vec<_> = reachable.iter()
                    .filter(|(edge, _, _)| {
                        let edge_tokens = roles::column_semantic_tokens(&edge.column);
                        edge_tokens.iter().any(|t| q_tokens.contains(t))
                    })
                    .collect();
                if matches.len() == 1 {
                    let (edge, target_tb, col) = matches.pop().unwrap();
                    Some((*edge, *target_tb, col.clone()))
                } else {
                    let candidates = reachable.iter()
                        .map(|(edge, target_tb, col)| {
                            format!("{}.{} (via {})", target_tb.table_name, col.column_name, edge.column)
                        })
                        .collect();
                    return Err(BindError::Ambiguous { concept, role, candidates });
                }
            }
        };

        if let Some((edge, target_tb, col)) = chosen {
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
    // Reverse-FK scope binding (plan 03f)
    // -----------------------------------------------------------------------

    /// Resolve each `RelatedScope` to its `RelatedPhysical` binding.
    ///
    /// Hard constraints (plan 03f §2):
    /// * Single-hop only: child.fk_col → anchor.pk_col.
    /// * FK must point at the anchor (subject) table's PK — not sideways.
    /// * Prefer-forward guard: if the subject already has a forward FK to the
    ///   child, the normal bind path handles it; refuse the reverse path.
    /// * Unique/Absent/Ambiguous at table granularity (same discipline as
    ///   `find_col_via_fk`).
    /// * Scope check on the child table.
    /// * Filter columns resolved within the child table scope.
    fn bind_related_scopes(
        &self,
        related: Vec<RelatedScope>,
        subject_table: &str,
    ) -> Result<Vec<RelatedScope>, BindError> {
        if related.is_empty() {
            return Ok(related);
        }

        // Build reverse-FK index from all cards:
        //   anchor_table_lower → [(child_table, child_fk_col, anchor_pk_col)]
        let mut rev_idx: std::collections::HashMap<String, Vec<(String, String, String)>> =
            std::collections::HashMap::new();
        for card in self.cards {
            for edge in &card.fk_edges {
                rev_idx
                    .entry(edge.ref_table.to_ascii_lowercase())
                    .or_default()
                    .push((card.table_name.clone(), edge.column.clone(), edge.ref_column.clone()));
            }
        }

        let subject_lower = subject_table.to_ascii_lowercase();
        let reverse_entries: Vec<(String, String, String)> = rev_idx
            .get(&subject_lower)
            .cloned()
            .unwrap_or_default();

        let mut bound = Vec::with_capacity(related.len());
        for mut rs in related {
            // Resolve child table name from concept.
            let child_tb = self
                .binding
                .table_for_concept(rs.concept)
                .ok_or(BindError::NoSubject(rs.concept))?;
            let child_table = child_tb.table_name.clone();

            // Prefer-forward guard: if the subject has a forward FK to the child,
            // the subject→child direction is already handled by the normal bind path
            // (add inline filter, not a subquery).  Refuse the reverse path to
            // avoid confusion — a forward-FK join would not multiply rows either,
            // but if we arrive here the spec was built with a RelatedScope, which
            // means the question parser already decided the child is a separate hop.
            // The correct fix is in parse.rs for that case; here we just refuse.
            let has_forward_fk = self
                .cards
                .iter()
                .find(|c| c.table_name.eq_ignore_ascii_case(subject_table))
                .map(|card| {
                    card.fk_edges
                        .iter()
                        .any(|e| e.ref_table.eq_ignore_ascii_case(&child_table))
                })
                .unwrap_or(false);
            if has_forward_fk {
                return Err(BindError::NoJoinPath(
                    subject_table.to_string(),
                    format!("{:?} (prefer forward FK)", rs.concept),
                ));
            }

            // Find reverse-FK candidates: child_table has an FK pointing at subject_table.
            let candidates: Vec<(String, String)> = reverse_entries
                .iter()
                .filter(|(ct, _, _)| ct.eq_ignore_ascii_case(&child_table))
                .map(|(_, fk_col, pk_col)| (fk_col.clone(), pk_col.clone()))
                .collect();

            let physical = match candidates.len() {
                0 => {
                    return Err(BindError::NoJoinPath(
                        child_table.clone(),
                        subject_table.to_string(),
                    ));
                }
                1 => {
                    let (child_fk_col, anchor_pk_col) = candidates.into_iter().next().unwrap();
                    RelatedPhysical { child_table: child_table.clone(), child_fk_col, anchor_pk_col }
                }
                _ => {
                    // Multiple FKs from child to subject — ambiguous.
                    let candidates_display: Vec<String> = candidates
                        .iter()
                        .map(|(fk, pk)| {
                            format!("{}.{} → {}.{}", child_table, fk, subject_table, pk)
                        })
                        .collect();
                    return Err(BindError::Ambiguous {
                        concept: rs.concept,
                        role: ColumnRole::ForeignRef,
                        candidates: candidates_display,
                    });
                }
            };

            // Scope check for child table.
            if !self.scope_ok(&child_table) {
                return Err(BindError::OutOfScope(child_table));
            }

            // Resolve filter ColumnRefs within child table scope.
            // Only look in the child table itself — single-hop constraint.
            rs.filters = rs
                .filters
                .into_iter()
                .map(|mut f| {
                    if f.column.physical.is_some() {
                        // Column already bound — still apply domain narrowing so that
                        // a pre-bound filter cannot bypass enum-domain validation.
                        return self.apply_in_domain_narrowing(f);
                    }
                    match self.find_col_in_table(
                        &child_table,
                        f.column.concept,
                        f.column.role,
                        f.column.name_hint.as_deref(),
                        Some(&f.value),
                    )? {
                        Some(resolved) => {
                            f.column.physical = Some(resolved);
                            // Apply the same In-list domain narrowing that resolve_filter
                            // applies on the main filter path — prevents enum-invalid
                            // literals from reaching the compiled SQL.
                            self.apply_in_domain_narrowing(f)
                        }
                        None => Err(BindError::NoRole {
                            concept: f.column.concept,
                            role: f.column.role,
                        }),
                    }
                })
                .collect::<Result<_, _>>()?;

            rs.physical = Some(physical);
            bound.push(rs);
        }
        Ok(bound)
    }

    // -----------------------------------------------------------------------
    // Specialized resolution methods
    // -----------------------------------------------------------------------

    fn resolve_filter(&self, mut f: Filter, subject_table: &str, joins: &mut Vec<JoinRef>) -> Result<Filter, BindError> {
        // Pass the filter's own literal as stage-B (value-domain) disambiguation
        // evidence: if the role is ambiguous on the target table, prefer the
        // candidate whose known `enum_values` domain actually contains it.
        f.column = self.resolve_col_ref_ex(f.column, subject_table, joins, Some(&f.value))?;
        self.apply_in_domain_narrowing(f)
    }

    /// Intersect an `In(...)` filter's candidate list against the column's known
    /// `enum_values` domain (if any).
    ///
    /// This is the single implementation of enum-domain narrowing, shared by both
    /// the main filter path (`resolve_filter`) and the related-scope filter path
    /// (`bind_related_scopes`).  Calling it in both places ensures that no `In`
    /// predicate can bypass the domain check regardless of which code path bound
    /// its column.
    ///
    /// Rule:
    ///   • `enum_values` is empty → domain unknown; trust the predicate's list as-is.
    ///   • intersection is non-empty → narrow the `In` list to valid values only.
    ///   • intersection is empty → refuse (`UnsatisfiableFilter`); the pipeline falls
    ///     back to the planner / semantic path rather than emitting a query that looks
    ///     correct but always returns zero rows.
    fn apply_in_domain_narrowing(&self, mut f: Filter) -> Result<Filter, BindError> {
        if let FilterOp::In = f.op {
            if let FilterValue::List(ref candidates) = f.value {
                if let Some((tbl, col)) = f.column.physical.as_ref() {
                    if let Some(enum_vals) = self.enum_values_for(tbl, col) {
                        let narrowed: Vec<FilterValue> = candidates
                            .iter()
                            .filter(|cv| {
                                if let FilterValue::Str(s) = cv {
                                    enum_vals.iter().any(|ev| ev.eq_ignore_ascii_case(s))
                                } else {
                                    false
                                }
                            })
                            .cloned()
                            .collect();

                        if narrowed.is_empty() {
                            return Err(BindError::UnsatisfiableFilter {
                                concept: f.column.concept,
                                role: f.column.role,
                            });
                        }
                        f.value = FilterValue::List(narrowed);
                    }
                }
            }
        }
        Ok(f)
    }

    /// Return the `enum_values` slice for the named column in the named table,
    /// or `None` when the domain is unknown (empty slice means not probed / not
    /// categorical, treated as "trust the predicate's literals as-is").
    fn enum_values_for(&self, table: &str, column: &str) -> Option<&[String]> {
        let tb = self.binding.tables.iter()
            .find(|t| t.table_name.eq_ignore_ascii_case(table))?;
        let cb = tb.columns.iter()
            .find(|c| c.column_name.eq_ignore_ascii_case(column))?;
        if cb.enum_values.is_empty() {
            None
        } else {
            Some(&cb.enum_values)
        }
    }

    /// Resolve a measure/projection target: a stored column or a derived
    /// duration (plan 03g).
    fn resolve_value_expr(&self, v: ValueExpr, subject_table: &str, joins: &mut Vec<JoinRef>) -> Result<ValueExpr, BindError> {
        match v {
            ValueExpr::Column(cr) => Ok(ValueExpr::Column(self.resolve_col_ref(cr, subject_table, joins)?)),
            ValueExpr::Duration(d) => Ok(ValueExpr::Duration(self.resolve_duration(d, subject_table, joins)?)),
        }
    }

    /// Bind both endpoints of a derived duration, or refuse.
    ///
    /// Both endpoints go through the ordinary `resolve_col_ref` path, so each is
    /// chosen by (concept, role) through the three-valued `RoleLookup` — never by
    /// taking the first temporal column in declaration order. Consequences, all
    /// intended (plan 03g §4):
    ///
    /// * end role absent (an admission timestamp but no discharge timestamp) →
    ///   `NoRole`, the row refuses. No nearby temporal column is substituted:
    ///   `EXTRACT(EPOCH FROM created_at - admission_date)` is not a length of stay.
    /// * either role ambiguous (`surgeries` has both `scheduled_start` and
    ///   `actual_start`) → `Ambiguous`, the row refuses. Picking one silently
    ///   changes the number that comes out.
    /// * endpoints landing on different tables → refuse. An FK hop to a 1:N child
    ///   multiplies rows, which silently changes the population being averaged;
    ///   an interval is only well-defined when both instants describe the same
    ///   real-world event record.
    fn resolve_duration(&self, mut d: DerivedDuration, subject_table: &str, joins: &mut Vec<JoinRef>) -> Result<DerivedDuration, BindError> {
        d.start = self.resolve_col_ref(d.start, subject_table, joins)?;
        d.end = self.resolve_col_ref(d.end, subject_table, joins)?;
        let start_tbl = d.start.physical.as_ref().map(|(t, _)| t.clone()).unwrap_or_default();
        let end_tbl = d.end.physical.as_ref().map(|(t, _)| t.clone()).unwrap_or_default();
        if !start_tbl.eq_ignore_ascii_case(&end_tbl) {
            return Err(BindError::NoJoinPath(start_tbl, end_tbl));
        }
        Ok(d)
    }

    fn resolve_measure(&self, mut m: Measure, subject_table: &str, joins: &mut Vec<JoinRef>) -> Result<Measure, BindError> {
        if let Some(target) = m.target {
            m.target = Some(self.resolve_value_expr(target, subject_table, joins)?);
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
            // Try EventTime first, then StartTime/EndTime — but only as a fallback
            // for a role that is genuinely ABSENT on this table.  If EventTime
            // exists but is AMBIGUOUS (e.g. `incident_reports` has both
            // `occurred_at` and `reported_at`), that must refuse immediately, not
            // silently fall through to StartTime/EndTime — those name a different
            // temporal concept (period bounds, not "when this happened"), so
            // substituting one for an ambiguous EventTime would trade one
            // position-based wrong answer for another (e.g. filtering incidents by
            // `closed_at`, the EndTime column, in place of an unresolved
            // occurred_at/reported_at choice — a different question again).
            let roles = [ColumnRole::EventTime, ColumnRole::StartTime, ColumnRole::EndTime];
            let mut resolved = None;
            for role in roles {
                let cr = ColumnRef {
                    concept: ts.column.concept,
                    role,
                    name_hint: ts.column.name_hint.clone(),
                    physical: None,
                };
                match self.resolve_col_ref(cr, subject_table, joins) {
                    Ok(r) => {
                        resolved = Some(r);
                        break;
                    }
                    Err(e @ BindError::Ambiguous { .. }) => return Err(e),
                    Err(_) => continue, // role absent here — try the next tier
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

/// Result of looking up a `(role, hint)` column within one table.
///
/// Deliberately three-valued instead of `Option`: `Absent` and `Ambiguous` are
/// NOT interchangeable to callers.  `Absent` means "this mechanism doesn't
/// apply here, try the next one" (FK hop, patient path, next role tier).
/// `Ambiguous` means "the thing being asked about exists here more than once,
/// under a name that doesn't disambiguate" — a refusal condition, never a
/// signal to keep searching elsewhere for a column that might resolve the tie
/// by accident.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum RoleLookup<'a> {
    /// Exactly one column satisfies `role` (and `hint`, if given).
    Unique(&'a crate::ontology::binding::ColumnBinding),
    /// No column satisfies `role` (and `hint`, if given).
    Absent,
    /// More than one column satisfies `role` (and `hint`, if given); their
    /// names are carried for the `BindError::Ambiguous` message.
    Ambiguous(Vec<String>),
}

/// Helper method used only by the binder; added as a free function over a
/// `TableBinding` reference since we cannot call `self.binding.*` from within
/// a closure that also borrows `self`.
impl SchemaBinding {
    /// Find the column(s) in `tb` that have `role`, optionally constrained by a
    /// name hint.
    ///
    /// Semantics:
    ///   `hint = None`  — collect every column with the matching role.
    ///   `hint = Some`  — collect only columns whose name contains the hint
    ///                    (case-insensitive substring).  A hint is a
    ///                    *requirement*, not a preference: a role match whose
    ///                    name doesn't satisfy the hint is not a candidate.
    ///
    /// Selection **by meaning, never by declaration order**. When more than one
    /// candidate remains after the hint filter, three stages of EVIDENCE are
    /// tried in order — the first that narrows to exactly one candidate wins
    /// (plans/new/03b-defect-closure-brief.md follow-up, "where the question
    /// itself disambiguates, select on that evidence; refuse only when nothing
    /// does"):
    ///
    ///   A. **Name evidence** — does a word in the question match a candidate's
    ///      own (normalised, stemmed) name? ("reported" ↔ `reported_at`).
    ///   B. **Value-domain evidence** — when the caller supplies a filter
    ///      literal, does exactly one candidate's `enum_values` contain it?
    ///      ('pending' is a real `status` label; `outcome` has no label set).
    ///   C. **Concept-name evidence** — does a candidate's name echo the bound
    ///      concept's own descriptor tokens? (`encounter_date` on `Encounter`).
    ///
    /// Still ambiguous after all three → `Ambiguous`, the caller must refuse,
    /// never take the first. There is no "pick the first when tied" path; that
    /// was the defect this function exists to close (`qs-01`, `pc-02` — see
    /// mod.rs docs and the 03b-defect-closure-brief).
    pub(crate) fn column_with_role_hint_in_table<'a>(
        &self,
        tb: &'a TableBinding,
        role: ColumnRole,
        hint: Option<&str>,
        concept: EntityConcept,
        question: &str,
        literal: Option<&FilterValue>,
    ) -> RoleLookup<'a> {
        let mut candidates: Vec<&'a crate::ontology::binding::ColumnBinding> = tb.columns.iter()
            .filter(|cb| cb.role == role)
            .filter(|cb| match hint {
                Some(h) => cb.column_name.to_ascii_lowercase().contains(&h.to_ascii_lowercase()),
                None => true,
            })
            .collect();

        // EventTime with no explicit hint: prefer a purpose-named column over a
        // generic row-bookkeeping timestamp (created_at/updated_at/...) when both
        // exist, mirroring `ontology::binder::select_event_time`'s "prefer domain"
        // rule (see `roles::GENERIC_TIMESTAMP_NAMES`). A generic timestamp remains
        // usable when it is the ONLY EventTime-role column — this narrows a
        // spurious tie, it does not manufacture a candidate that isn't there.
        if role == ColumnRole::EventTime && hint.is_none() && candidates.len() > 1 {
            let purpose_named: Vec<_> = candidates.iter()
                .copied()
                .filter(|cb| !crate::ontology::roles::is_generic_timestamp_name(&cb.column_name))
                .collect();
            if !purpose_named.is_empty() {
                candidates = purpose_named;
            }
        }

        if candidates.len() > 1 {
            if let Some(picked) = disambiguate_by_question_tokens(&candidates, question) {
                candidates = vec![picked];
            } else if let Some(picked) = literal.and_then(|lit| disambiguate_by_value_domain(&candidates, lit)) {
                candidates = vec![picked];
            } else if let Some(picked) = disambiguate_by_concept_name(&candidates, concept) {
                candidates = vec![picked];
            }
        }

        match candidates.len() {
            0 => RoleLookup::Absent,
            1 => RoleLookup::Unique(candidates[0]),
            _ => RoleLookup::Ambiguous(candidates.iter().map(|c| c.column_name.clone()).collect()),
        }
    }
}

// ---------------------------------------------------------------------------
// Role disambiguation stages A-C
// ---------------------------------------------------------------------------
//
// One general mechanism, applied identically regardless of which fixture row
// triggers it: no string here names a specific column, table, or fixture
// question. Every candidate is compared to evidence by the SAME normalised
// token comparison (`roles::column_semantic_tokens` / `roles::word_stems`) —
// stage A's "evidence" is the question's own words, stage C's is the bound
// concept's own descriptor vocabulary; nothing else differs between them.

/// Function words that carry no naming evidence for stage A. Deliberately
/// short and generic (articles, auxiliaries, question words, prepositions) —
/// never a word chosen because a fixture question contains or lacks it.
const EVIDENCE_STOPWORDS: &[&str] = &[
    "the", "a", "an", "is", "are", "was", "were", "this", "that", "these", "those",
    "how", "many", "much", "did", "do", "does", "has", "have", "had", "been", "be",
    "of", "in", "on", "at", "for", "to", "and", "or", "with", "not", "any", "all",
    "we", "our", "their", "its", "who", "whom", "whose", "which", "what", "where",
    "when", "why", "there", "please", "show", "me", "tell", "give", "list",
];

/// Tokenise a raw question into the same stemmed token space as
/// `roles::column_semantic_tokens`, dropping function words that would
/// otherwise create accidental matches against short column-name fragments.
fn question_evidence_tokens(question: &str) -> HashSet<String> {
    let norm = super::parse::normalize(question);
    let mut set = HashSet::new();
    for word in norm.split_whitespace() {
        if EVIDENCE_STOPWORDS.contains(&word) {
            continue;
        }
        for stem in roles::word_stems(word) {
            if stem.len() >= 3 {
                set.insert(stem);
            }
        }
    }
    set
}

/// Stage A: does exactly one candidate's own (normalised) name share a token
/// with the question? Zero or several matches → `None`, try the next stage.
fn disambiguate_by_question_tokens<'a>(
    candidates: &[&'a crate::ontology::binding::ColumnBinding],
    question: &str,
) -> Option<&'a crate::ontology::binding::ColumnBinding> {
    let q_tokens = question_evidence_tokens(question);
    if q_tokens.is_empty() {
        return None;
    }
    let mut matches = candidates.iter().copied().filter(|cb| {
        roles::column_semantic_tokens(&cb.column_name)
            .iter()
            .any(|t| q_tokens.contains(t))
    });
    let first = matches.next()?;
    if matches.next().is_none() {
        Some(first)
    } else {
        None
    }
}

/// Extract the string literal(s) a `Filter` supplies, for stage-B domain
/// matching. Non-string filters (dates, numbers, booleans) carry no domain
/// label evidence and yield nothing.
fn literal_strings(fv: &FilterValue) -> Vec<&str> {
    match fv {
        FilterValue::Str(s) => vec![s.as_str()],
        FilterValue::List(items) => items
            .iter()
            .filter_map(|v| match v {
                FilterValue::Str(s) => Some(s.as_str()),
                _ => None,
            })
            .collect(),
        _ => Vec::new(),
    }
}

/// Stage B: does exactly one candidate's known `enum_values` domain contain
/// the filter's literal? A column with an empty (unprobed) domain is never a
/// match here — silence is not evidence either way.
fn disambiguate_by_value_domain<'a>(
    candidates: &[&'a crate::ontology::binding::ColumnBinding],
    literal: &FilterValue,
) -> Option<&'a crate::ontology::binding::ColumnBinding> {
    let lits = literal_strings(literal);
    if lits.is_empty() {
        return None;
    }
    let mut matches = candidates.iter().copied().filter(|cb| {
        !cb.enum_values.is_empty()
            && lits.iter().any(|l| cb.enum_values.iter().any(|ev| ev.eq_ignore_ascii_case(l)))
    });
    let first = matches.next()?;
    if matches.next().is_none() {
        Some(first)
    } else {
        None
    }
}

/// Stage C: does exactly one candidate's name echo the bound concept's own
/// descriptor vocabulary (its `name_tokens`, `singular`, `plural` — the
/// concept's own name, not the broader `column_tokens`/`synonyms` lists,
/// which are generic enough to match almost anything)?
fn disambiguate_by_concept_name<'a>(
    candidates: &[&'a crate::ontology::binding::ColumnBinding],
    concept: EntityConcept,
) -> Option<&'a crate::ontology::binding::ColumnBinding> {
    if concept == EntityConcept::Unknown {
        return None;
    }
    let desc = descriptor(concept);
    let mut concept_tokens: HashSet<String> = HashSet::new();
    let add = |raw: &str, tokens: &mut HashSet<String>| {
        for part in raw.split('_') {
            for stem in roles::word_stems(part) {
                if stem.len() >= 3 {
                    tokens.insert(stem);
                }
            }
        }
    };
    // `name_tokens` mixes the concept's own name with compound synonym
    // PHRASES ("event_report", "near_miss", "transfer_out" on `Incident`)
    // meant to match a whole TABLE name. Splitting a compound phrase on `_`
    // yields generic standalone words ("report", "miss", "transfer") that are
    // NOT the concept's name and can collide with an unrelated column
    // elsewhere (`incident_reports.reported_at` matching via "report" from
    // "event_report" — the incident *record* being a "report", not the
    // column being about reporting).
    //
    // The same risk applies to a COMPOUND `singular`/`plural`: `LabOrder`'s
    // singular is "lab_order", which splits into "lab" + "order" — and
    // "order" alone spuriously matches `lab_orders.order_date` for a
    // question that names neither column ("How many lab tests were done
    // today?", `dx-02`, the golden suite's deliberate no-evidence control).
    // "order" here means "the record IS an order", not "the column is about
    // ordering" — the same false-positive shape as "event_report", just
    // reached through `singular` instead of `name_tokens`. So: only
    // single-word forms (no `_`) count as "the concept's own name" in EITHER
    // field. A compound `singular`/`plural` contributes nothing, which is
    // the correct, conservative outcome — it leaves stage C silent rather
    // than guessing from a fragment, same as a compound name_token.
    for tok in desc.name_tokens.iter().filter(|t| !t.contains('_')) {
        add(tok, &mut concept_tokens);
    }
    if !desc.singular.contains('_') {
        add(desc.singular, &mut concept_tokens);
    }
    if !desc.plural.contains('_') {
        add(desc.plural, &mut concept_tokens);
    }

    let mut matches = candidates.iter().copied().filter(|cb| {
        roles::column_semantic_tokens(&cb.column_name)
            .iter()
            .any(|t| concept_tokens.contains(t))
    });
    let first = matches.next()?;
    if matches.next().is_none() {
        Some(first)
    } else {
        None
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
            related: vec![],
            duration_filters: vec![],
            provenance: SpecProvenance { rule: "R1".into(), focus_subs: vec![] },
        };
        let bound = bind(spec, &binding, &[], &[], "").unwrap();
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
            related: vec![],
            duration_filters: vec![],
            provenance: SpecProvenance { rule: "R1".into(), focus_subs: vec![] },
        };
        let bound = bind(spec, &binding, &[], &[], "").unwrap();
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
            related: vec![],
            duration_filters: vec![],
            provenance: SpecProvenance { rule: "R1".into(), focus_subs: vec![] },
        };
        let err = bind(spec, &binding, &[], &[], "").unwrap_err();
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
            related: vec![],
            duration_filters: vec![],
            provenance: SpecProvenance { rule: "R1".into(), focus_subs: vec![] },
        };

        let allowed: Vec<String> = cards.iter().map(|c| c.table_name.clone()).collect();
        let err = bind(spec, &binding, &cards, &allowed, "").unwrap_err();
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
            related: vec![],
            duration_filters: vec![],
            provenance: SpecProvenance { rule: "R1".into(), focus_subs: vec![] },
        };
        // Scope does not include "encounters" → should fail.
        let err = bind(spec, &binding, &[], &["patients".to_string()], "").unwrap_err();
        assert!(matches!(err, BindError::OutOfScope(_)));
    }

    // ── In-list grounding regression tests ───────────────────────────────────
    //
    // These tests exercise the enum_values intersection added in Task 1 of
    // plans/new/03b-defect-closure-brief.md.  They use a custom binding with
    // a Bed table whose status column has explicit enum_values, replicating
    // what the live DB probe (`probe_enum_values`) would produce at runtime.

    /// Build a minimal SchemaBinding for Bed with the given status domain.
    fn make_bed_binding(status_enum_values: Vec<String>) -> SchemaBinding {
        use crate::ontology::service_line::ServiceLine;
        SchemaBinding {
            source_id: "test_bed".into(),
            bound_at: Utc::now(),
            tables: vec![
                TableBinding {
                    table_name: "beds".to_string(),
                    concept: EntityConcept::Bed,
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
                            column_name: "status".into(),
                            role: ColumnRole::Status,
                            is_pii: false,
                            enum_values: status_enum_values,
                        },
                    ],
                    patient_path: None,
                    event_time_col: None,
                    degraded: false,
                },
            ],
            degraded: false,
            override_version: 0,
        }
    }

    fn make_in_filter_spec(values: Vec<&str>) -> QuerySpec {
        QuerySpec {
            subject: Subject { concept: EntityConcept::Bed, table: None },
            shape: Shape::Scalar,
            measures: vec![Measure { op: MeasureOp::Count, target: None, alias: "count".into() }],
            dimensions: vec![],
            filters: vec![Filter {
                column: ColumnRef::logical(EntityConcept::Bed, ColumnRole::Status),
                op: FilterOp::In,
                value: FilterValue::List(
                    values.iter().map(|v| FilterValue::Str(v.to_string())).collect()
                ),
            }],
            time: None,
            order: vec![],
            limit: None,
            joins: vec![],
            projection: vec![],
            related: vec![],
            duration_filters: vec![],
            provenance: SpecProvenance { rule: "R8".into(), focus_subs: vec![] },
        }
    }

    /// When ALL candidate values are absent from the column's known domain,
    /// bind must refuse with `UnsatisfiableFilter` — never emit an impossible
    /// `IN ('free', 'vacant')` that returns zero rows and looks like an answer.
    #[test]
    fn in_filter_refuses_when_all_values_outside_domain() {
        // Domain: available, occupied — neither "free" nor "vacant" exists.
        let binding = make_bed_binding(vec!["available".into(), "occupied".into()]);
        let spec = make_in_filter_spec(vec!["free", "vacant"]);
        let err = bind(spec, &binding, &[], &[], "").unwrap_err();
        assert!(
            matches!(err, BindError::UnsatisfiableFilter { .. }),
            "expected UnsatisfiableFilter for In list with no domain overlap; got: {:?}",
            err
        );
    }

    /// When SOME candidate values are in the domain and some are not,
    /// bind must narrow the In list to only the valid subset.
    #[test]
    fn in_filter_narrows_to_valid_subset() {
        // Domain: available, occupied — "available" is valid; "free" and "vacant" are not.
        let binding = make_bed_binding(vec!["available".into(), "occupied".into()]);
        let spec = make_in_filter_spec(vec!["available", "free", "vacant"]);
        let bound = bind(spec, &binding, &[], &[], "").unwrap();
        if let FilterValue::List(values) = &bound.filters[0].value {
            assert_eq!(values.len(), 1, "list must be narrowed to the single valid value");
            assert!(
                matches!(&values[0], FilterValue::Str(s) if s == "available"),
                "narrowed list must contain 'available'"
            );
        } else {
            panic!("expected FilterValue::List after binding");
        }
    }

    /// When `enum_values` is EMPTY (domain not yet probed), the predicate's
    /// candidate list must be trusted as-is — no spurious refusal on unknown domains.
    #[test]
    fn in_filter_trusts_list_when_enum_values_empty() {
        // Domain: empty (unprobed) — "free" and "vacant" are trusted as-is.
        let binding = make_bed_binding(vec![]);
        let spec = make_in_filter_spec(vec!["free", "vacant"]);
        let bound = bind(spec, &binding, &[], &[], "").unwrap();
        if let FilterValue::List(values) = &bound.filters[0].value {
            assert_eq!(values.len(), 2, "unprobed domain must leave the list unchanged");
        } else {
            panic!("expected FilterValue::List after binding");
        }
    }

    // ── Role disambiguation stages A-C (defect-closure follow-up) ────────────
    //
    // These exercise `column_with_role_hint_in_table`'s stages directly through
    // `bind()`, with hand-built bindings so no dev-seed/alt physical name
    // appears outside this `#[cfg(test)]` module.

    /// Two EventTime-role columns on one table ("booked_at", "cancelled_at").
    /// Neither hint nor value evidence is available; only the question can
    /// disambiguate.
    fn make_two_event_time_binding() -> SchemaBinding {
        SchemaBinding {
            source_id: "test_appt".into(),
            bound_at: Utc::now(),
            tables: vec![TableBinding {
                table_name: "appts".to_string(),
                concept: EntityConcept::Appointment,
                confidence: 0.95,
                service_lines: vec![],
                columns: vec![
                    ColumnBinding { column_name: "id".into(), role: ColumnRole::PrimaryKey, is_pii: false, enum_values: vec![] },
                    ColumnBinding { column_name: "booked_at".into(), role: ColumnRole::EventTime, is_pii: false, enum_values: vec![] },
                    ColumnBinding { column_name: "cancelled_at".into(), role: ColumnRole::EventTime, is_pii: false, enum_values: vec![] },
                ],
                patient_path: None,
                event_time_col: None,
                degraded: false,
            }],
            degraded: false,
            override_version: 0,
        }
    }

    /// Same shape as `make_two_event_time_binding`, but with candidate names
    /// that share nothing with `Appointment`'s OWN descriptor vocabulary
    /// either ("booking" is a real Appointment synonym and would legitimately
    /// let stage C resolve "booked_at" on its own — not a useful control for
    /// "no evidence anywhere disambiguates").
    fn make_two_event_time_binding_no_concept_overlap() -> SchemaBinding {
        SchemaBinding {
            source_id: "test_appt2".into(),
            bound_at: Utc::now(),
            tables: vec![TableBinding {
                table_name: "appts2".to_string(),
                concept: EntityConcept::Appointment,
                confidence: 0.95,
                service_lines: vec![],
                columns: vec![
                    ColumnBinding { column_name: "id".into(), role: ColumnRole::PrimaryKey, is_pii: false, enum_values: vec![] },
                    ColumnBinding { column_name: "confirmed_at".into(), role: ColumnRole::EventTime, is_pii: false, enum_values: vec![] },
                    ColumnBinding { column_name: "cancelled_at".into(), role: ColumnRole::EventTime, is_pii: false, enum_values: vec![] },
                ],
                patient_path: None,
                event_time_col: None,
                degraded: false,
            }],
            degraded: false,
            override_version: 0,
        }
    }

    fn make_time_scope_spec() -> QuerySpec {
        QuerySpec {
            subject: Subject { concept: EntityConcept::Appointment, table: None },
            shape: Shape::Scalar,
            measures: vec![Measure { op: MeasureOp::Count, target: None, alias: "count".into() }],
            dimensions: vec![],
            filters: vec![],
            time: Some(TimeScope {
                column: ColumnRef::logical(EntityConcept::Appointment, ColumnRole::EventTime),
                range: None,
                bucket: None,
            }),
            order: vec![],
            limit: None,
            joins: vec![],
            projection: vec![],
            related: vec![],
            duration_filters: vec![],
            provenance: SpecProvenance { rule: "TEST".into(), focus_subs: vec![] },
        }
    }

    /// Stage A: the question names one of two otherwise-tied EventTime
    /// candidates ("booked") — it must win, not refuse.
    #[test]
    fn stage_a_question_evidence_picks_named_column() {
        let binding = make_two_event_time_binding();
        let bound = bind(
            make_time_scope_spec(),
            &binding,
            &[],
            &[],
            "How many appointments are booked this week?",
        )
        .expect("question names 'booked' — must bind, not refuse");
        let (_, col) = bound.time.unwrap().column.physical.unwrap();
        assert_eq!(col, "booked_at");
    }

    /// Control: when nothing in the question names either candidate, the
    /// ambiguity must still refuse — stage A must not guess (mirrors `dx-02`
    /// in the golden suite: "done" names neither `order_date` nor
    /// `collected_at`).
    #[test]
    fn stage_a_no_evidence_still_refuses() {
        let binding = make_two_event_time_binding_no_concept_overlap();
        let err = bind(
            make_time_scope_spec(),
            &binding,
            &[],
            &[],
            "How many lab tests were done today?",
        )
        .unwrap_err();
        assert!(matches!(err, BindError::Ambiguous { .. }), "got {:?}", err);
    }

    /// Two Status-role columns ("status" with a real enum domain, "outcome"
    /// free text with none). Stage B must prefer the column whose domain
    /// actually contains the filter literal.
    #[test]
    fn stage_b_value_domain_picks_column_containing_literal() {
        let binding = SchemaBinding {
            source_id: "test_referral".into(),
            bound_at: Utc::now(),
            tables: vec![TableBinding {
                table_name: "referrals_t".to_string(),
                concept: EntityConcept::Referral,
                confidence: 0.95,
                service_lines: vec![],
                columns: vec![
                    ColumnBinding { column_name: "id".into(), role: ColumnRole::PrimaryKey, is_pii: false, enum_values: vec![] },
                    ColumnBinding {
                        column_name: "status".into(),
                        role: ColumnRole::Status,
                        is_pii: false,
                        enum_values: vec!["pending".into(), "completed".into()],
                    },
                    ColumnBinding {
                        column_name: "outcome".into(),
                        role: ColumnRole::Status,
                        is_pii: false,
                        enum_values: vec![],
                    },
                ],
                patient_path: None,
                event_time_col: None,
                degraded: false,
            }],
            degraded: false,
            override_version: 0,
        };
        let spec = QuerySpec {
            subject: Subject { concept: EntityConcept::Referral, table: None },
            shape: Shape::List,
            measures: vec![],
            dimensions: vec![],
            filters: vec![Filter {
                column: ColumnRef::logical(EntityConcept::Referral, ColumnRole::Status),
                op: FilterOp::Eq,
                value: FilterValue::Str("pending".into()),
            }],
            time: None,
            order: vec![],
            limit: None,
            joins: vec![],
            projection: vec![],
            related: vec![],
            duration_filters: vec![],
            provenance: SpecProvenance { rule: "TEST".into(), focus_subs: vec![] },
        };
        // No question-text evidence ("still pending" doesn't name either column) —
        // this must come from the value domain, not stage A.
        let bound = bind(spec, &binding, &[], &[], "Which referrals are still pending?").unwrap();
        let (_, col) = bound.filters[0].column.physical.as_ref().unwrap();
        assert_eq!(col, "status");
    }

    /// Two EventTime columns on the SAME concept's table where neither is
    /// named by the question: stage C must prefer the one echoing the
    /// concept's own name (`encounter_date` on `Encounter`) over a column
    /// that names an unrelated event (`follow_up_date`).
    #[test]
    fn stage_c_concept_name_picks_primary_attribute() {
        let binding = SchemaBinding {
            source_id: "test_enc".into(),
            bound_at: Utc::now(),
            tables: vec![TableBinding {
                table_name: "enc_t".to_string(),
                concept: EntityConcept::Encounter,
                confidence: 0.95,
                service_lines: vec![],
                columns: vec![
                    ColumnBinding { column_name: "id".into(), role: ColumnRole::PrimaryKey, is_pii: false, enum_values: vec![] },
                    ColumnBinding { column_name: "encounter_date".into(), role: ColumnRole::EventTime, is_pii: false, enum_values: vec![] },
                    ColumnBinding { column_name: "follow_up_date".into(), role: ColumnRole::EventTime, is_pii: false, enum_values: vec![] },
                ],
                patient_path: None,
                event_time_col: None,
                degraded: false,
            }],
            degraded: false,
            override_version: 0,
        };
        let spec = QuerySpec {
            subject: Subject { concept: EntityConcept::Encounter, table: None },
            shape: Shape::Scalar,
            measures: vec![Measure { op: MeasureOp::Count, target: None, alias: "count".into() }],
            dimensions: vec![],
            filters: vec![],
            time: Some(TimeScope {
                column: ColumnRef::logical(EntityConcept::Encounter, ColumnRole::EventTime),
                range: None,
                bucket: None,
            }),
            order: vec![],
            limit: None,
            joins: vec![],
            projection: vec![],
            related: vec![],
            duration_filters: vec![],
            provenance: SpecProvenance { rule: "TEST".into(), focus_subs: vec![] },
        };
        // A question with no naming evidence at all for either column.
        let bound = bind(spec, &binding, &[], &[], "How many happened recently").unwrap();
        let (_, col) = bound.time.unwrap().column.physical.unwrap();
        assert_eq!(col, "encounter_date");
    }

    // ── FK-edge disambiguation ("Also close, while you are in here") ────────

    fn make_admitting_attending_binding() -> (SchemaBinding, Vec<TableCard>) {
        let binding = SchemaBinding {
            source_id: "test_adm".into(),
            bound_at: Utc::now(),
            tables: vec![
                TableBinding {
                    table_name: "admissions_t".to_string(),
                    concept: EntityConcept::Admission,
                    confidence: 0.95,
                    service_lines: vec![],
                    columns: vec![
                        ColumnBinding { column_name: "id".into(), role: ColumnRole::PrimaryKey, is_pii: false, enum_values: vec![] },
                        ColumnBinding { column_name: "admitting_dr".into(), role: ColumnRole::ProviderRef, is_pii: false, enum_values: vec![] },
                        ColumnBinding { column_name: "attending_dr".into(), role: ColumnRole::ProviderRef, is_pii: false, enum_values: vec![] },
                    ],
                    patient_path: None,
                    event_time_col: None,
                    degraded: false,
                },
                TableBinding {
                    table_name: "providers_t".to_string(),
                    concept: EntityConcept::Provider,
                    confidence: 0.99,
                    service_lines: vec![],
                    columns: vec![
                        ColumnBinding { column_name: "id".into(), role: ColumnRole::PrimaryKey, is_pii: false, enum_values: vec![] },
                        ColumnBinding { column_name: "full_name".into(), role: ColumnRole::PersonFullName, is_pii: true, enum_values: vec![] },
                    ],
                    patient_path: None,
                    event_time_col: None,
                    degraded: false,
                },
            ],
            degraded: false,
            override_version: 0,
        };
        let cards = vec![TableCard {
            source_id: "test_adm".into(),
            table_name: "admissions_t".into(),
            row_count: 10,
            columns: vec![],
            fk_edges: vec![
                CardFkEdge { column: "admitting_dr".into(), ref_table: "providers_t".into(), ref_column: "id".into() },
                CardFkEdge { column: "attending_dr".into(), ref_table: "providers_t".into(), ref_column: "id".into() },
            ],
            card_vector: None,
            card_text: "admissions_t".into(),
        }];
        (binding, cards)
    }

    /// Groups admissions by provider name. Uses a `Dimension` (not
    /// `projection`) deliberately: unresolved projection columns are dropped
    /// silently (best-effort decoration — see `bind()`'s doc comment), which
    /// would hide the very ambiguity these tests exist to catch. Dimension
    /// resolution errors propagate through `bind()`'s `Result`.
    fn make_provider_name_lookup_spec() -> QuerySpec {
        QuerySpec {
            subject: Subject { concept: EntityConcept::Admission, table: None },
            shape: Shape::Grouped,
            measures: vec![Measure { op: MeasureOp::Count, target: None, alias: "count".into() }],
            dimensions: vec![Dimension {
                column: ColumnRef::logical(EntityConcept::Provider, ColumnRole::PersonFullName),
                label: "provider".into(),
            }],
            filters: vec![],
            time: None,
            order: vec![],
            limit: None,
            joins: vec![],
            projection: vec![],
            related: vec![],
            duration_filters: vec![],
            provenance: SpecProvenance { rule: "TEST".into(), focus_subs: vec![] },
        }
    }

    /// Two FK edges from the subject table reach the SAME wanted concept
    /// (`admissions_t.admitting_dr` and `.attending_dr`, both → `providers_t`).
    /// With no evidence naming either relationship, this must refuse — the
    /// same "ambiguity across edges" defect the FK-hop lookup previously left
    /// open (it took the first-declared edge unconditionally).
    #[test]
    fn fk_edge_ambiguous_across_edges_without_evidence_refuses() {
        let (binding, cards) = make_admitting_attending_binding();
        // Scope must admit BOTH tables — the FK target ("providers_t") is a
        // separate table from the subject ("admissions_t") reached only via
        // the FK hop under test.
        let allowed: Vec<String> = vec!["admissions_t".to_string(), "providers_t".to_string()];
        let err = bind(make_provider_name_lookup_spec(), &binding, &cards, &allowed, "Who is on this admission?")
            .unwrap_err();
        assert!(matches!(err, BindError::Ambiguous { .. }), "got {:?}", err);
    }

    /// When the question names the relationship ("attending"), the matching
    /// FK edge wins — same question-evidence mechanism as stage A, applied to
    /// the edge's own column name instead of a target column's name.
    #[test]
    fn fk_edge_disambiguated_by_question_naming_relationship() {
        let (binding, cards) = make_admitting_attending_binding();
        let allowed: Vec<String> = vec!["admissions_t".to_string(), "providers_t".to_string()];
        let bound = bind(
            make_provider_name_lookup_spec(),
            &binding,
            &cards,
            &allowed,
            "Who is the attending doctor on this admission?",
        )
        .expect("question names 'attending' — must bind, not refuse");
        let (table, col) = bound.dimensions[0].column.physical.as_ref().unwrap();
        assert_eq!(table, "providers_t");
        assert_eq!(col, "full_name");
    }

    // ── Related-scope enum-domain narrowing tests ─────────────────────────────
    //
    // These verify that `apply_in_domain_narrowing` is called on the reverse-FK
    // filter path (`bind_related_scopes`) as well as the main filter path
    // (`resolve_filter`).  The regression here is that the original code resolved
    // the column in the child table but then returned without intersecting the
    // candidate list against `enum_values`, allowing enum-invalid literals
    // ('dna', 'did_not_attend') to survive into the compiled SQL.
    //
    // Teeth-verification: removing the `self.apply_in_domain_narrowing(f)` call
    // from the `Some(resolved)` arm of `bind_related_scopes` and running
    // `cargo test --bin onprem-server` causes both tests below to fail:
    //   - `related_scope_in_filter_narrows_to_domain` panics because the list
    //     length is 3 (all values pass) instead of 1 (only 'no_show').
    //   - `related_scope_in_filter_refuses_when_unsatisfiable` panics because
    //     bind() returns Ok instead of Err(UnsatisfiableFilter).
    // After restoring the call both tests pass. Teeth confirmed.

    /// Build a minimal binding with Patient (parent) and Appointment (child)
    /// tables.  `appt_status_domain` controls the Appointment.status domain.
    fn make_patient_appointment_binding(appt_status_domain: Vec<String>) -> (SchemaBinding, Vec<TableCard>) {
        use crate::ontology::service_line::ServiceLine;
        let binding = SchemaBinding {
            source_id: "test_pa".into(),
            bound_at: Utc::now(),
            tables: vec![
                TableBinding {
                    table_name: "patients".to_string(),
                    concept: EntityConcept::Patient,
                    confidence: 0.95,
                    service_lines: vec![ServiceLine::PatientChart],
                    columns: vec![
                        ColumnBinding { column_name: "id".into(), role: ColumnRole::PrimaryKey, is_pii: false, enum_values: vec![] },
                        ColumnBinding { column_name: "patient_no".into(), role: ColumnRole::Identifier, is_pii: false, enum_values: vec![] },
                    ],
                    patient_path: Some(vec![]),
                    event_time_col: None,
                    degraded: false,
                },
                TableBinding {
                    table_name: "appointments".to_string(),
                    concept: EntityConcept::Appointment,
                    confidence: 0.95,
                    service_lines: vec![ServiceLine::PatientChart],
                    columns: vec![
                        ColumnBinding { column_name: "id".into(), role: ColumnRole::PrimaryKey, is_pii: false, enum_values: vec![] },
                        ColumnBinding { column_name: "patient_id".into(), role: ColumnRole::ForeignRef, is_pii: false, enum_values: vec![] },
                        ColumnBinding { column_name: "status".into(), role: ColumnRole::Status, is_pii: false, enum_values: appt_status_domain },
                    ],
                    patient_path: None,
                    event_time_col: None,
                    degraded: false,
                },
            ],
            degraded: false,
            override_version: 0,
        };
        let cards = vec![
            TableCard {
                source_id: "test_pa".into(),
                table_name: "patients".into(),
                row_count: 100,
                columns: vec![],
                fk_edges: vec![],
                card_vector: None,
                card_text: "patients".into(),
            },
            TableCard {
                source_id: "test_pa".into(),
                table_name: "appointments".into(),
                row_count: 500,
                columns: vec![],
                fk_edges: vec![CardFkEdge {
                    column: "patient_id".into(),
                    ref_table: "patients".into(),
                    ref_column: "id".into(),
                }],
                card_vector: None,
                card_text: "appointments".into(),
            },
        ];
        (binding, cards)
    }

    /// Build a spec: "Which patients [have status IN list] related appointments?"
    fn make_related_scope_spec(status_values: Vec<&str>) -> QuerySpec {
        QuerySpec {
            subject: Subject { concept: EntityConcept::Patient, table: None },
            shape: Shape::List,
            measures: vec![],
            dimensions: vec![],
            filters: vec![],
            time: None,
            order: vec![],
            limit: None,
            joins: vec![],
            projection: vec![
                ValueExpr::Column(ColumnRef::logical(EntityConcept::Patient, ColumnRole::Identifier)),
            ],
            duration_filters: vec![],
            related: vec![RelatedScope {
                concept: EntityConcept::Appointment,
                filters: vec![Filter {
                    column: ColumnRef::logical(EntityConcept::Appointment, ColumnRole::Status),
                    op: FilterOp::In,
                    value: FilterValue::List(
                        status_values.iter().map(|v| FilterValue::Str(v.to_string())).collect()
                    ),
                }],
                time: None,
                negated: false,
                physical: None,
            }],
            provenance: SpecProvenance { rule: "R8".into(), focus_subs: vec![] },
        }
    }

    /// A related-scope `IN` filter whose candidate list contains both valid and
    /// invalid domain values must be narrowed to only the valid subset.
    /// This ensures the reverse-FK code path goes through `apply_in_domain_narrowing`.
    #[test]
    fn related_scope_in_filter_narrows_to_domain() {
        // Domain: no_show, completed — 'no_show' is valid; 'dna' and 'did_not_attend' are not.
        let (binding, cards) = make_patient_appointment_binding(
            vec!["no_show".into(), "completed".into()],
        );
        let spec = make_related_scope_spec(vec!["no_show", "dna", "did_not_attend"]);
        let all_tables: Vec<String> = binding.tables.iter().map(|t| t.table_name.clone()).collect();
        let bound = bind(spec, &binding, &cards, &all_tables, "").expect(
            "related-scope bind must succeed when at least one value is in the domain"
        );
        let rs = &bound.related[0];
        if let FilterValue::List(values) = &rs.filters[0].value {
            assert_eq!(
                values.len(), 1,
                "related-scope In list must be narrowed to the single valid value; got {} values",
                values.len()
            );
            assert!(
                matches!(&values[0], FilterValue::Str(s) if s == "no_show"),
                "narrowed list must contain only 'no_show'"
            );
        } else {
            panic!("expected FilterValue::List on the related-scope filter after binding");
        }
    }

    /// A related-scope `IN` filter whose entire candidate list is absent from the
    /// column's domain must cause `UnsatisfiableFilter` — the reverse-FK path
    /// must not bypass enum-domain validation.
    #[test]
    fn related_scope_in_filter_refuses_when_unsatisfiable() {
        // Domain: no_show, completed — neither 'dna' nor 'did_not_attend' exists.
        let (binding, cards) = make_patient_appointment_binding(
            vec!["no_show".into(), "completed".into()],
        );
        let spec = make_related_scope_spec(vec!["dna", "did_not_attend"]);
        let all_tables: Vec<String> = binding.tables.iter().map(|t| t.table_name.clone()).collect();
        let err = bind(spec, &binding, &cards, &all_tables, "").unwrap_err();
        assert!(
            matches!(err, BindError::UnsatisfiableFilter { .. }),
            "expected UnsatisfiableFilter when all related-scope filter values are outside \
             the child column's domain; got: {:?}",
            err
        );
    }
}
