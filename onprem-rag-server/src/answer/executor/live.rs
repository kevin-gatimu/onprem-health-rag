//! The live [`Rungs`] implementation — the only place in the ladder that touches
//! the database, the model, or the clock.
//!
//! Each rung delegates to the module that already owns that step:
//!
//! | rung | delegate | guard it goes through |
//! |------|----------|-----------------------|
//! | 1 Link              | `nl2sql::prepare::link_auto_source`   | scope narrowing (plan 05 hook 1) |
//! | 2 Deterministic SQL | `nl2sql::prepare::try_deterministic` | `nl2sql::validate::validate_sql` |
//! | 3 Model SQL         | `nl2sql::prepare::prepare_with_cards`| `nl2sql::validate::validate_sql` |
//! | 4 Aggregation       | `aggregation::execute::run_scoped`   | `aggregation::validate::validate` |
//! | 5 List              | `aggregation::list::run`             | `aggregation::list::validate_list` |
//! | 6 Retrieval         | `retrieval::retrieve_observed_filtered` | `RetrievalFilter` |
//!
//! No SQL string is built here, and no pipeline is executed without its
//! validator, because in every case the delegate owns both. That is deliberate:
//! the failure mode this codebase has actually hit is a *new* path that skips an
//! *existing* guard, and the cheapest defence is to have no second path.
//!
//! # Known cost, documented rather than hidden
//!
//! Rung 3 calls `prepare_with_cards`, which internally re-attempts
//! `try_deterministic` before planning with the model. So when rung 2 misses,
//! rung 3 repeats rung 2's attempt once. Avoiding that means either splitting
//! `prepare_with_cards` or duplicating its model-planning half here; the second
//! would duplicate the validate/repair/execute sequence, which is exactly what
//! this module exists to avoid. The cost is one extra deterministic compile —
//! cheap and model-free — and it is visible in `provenance.elapsed_ms`.

use std::sync::Arc;
use std::time::{Duration, Instant};

use mongodb::bson::Document;
use serde_json::Value;

use crate::aggregation::catalog::Catalog;
use crate::aggregation::execute::AggRow;
use crate::aggregation::from_ir::{DocDbSpec, spec_from_query_spec};
use crate::aggregation::intent::QueryIntent;
use crate::aggregation::spec::{RunAggregation, RunList};
use crate::aggregation::{execute, list, validate};
use crate::auth::guard::AuthUser;
use crate::error::AppError;
use crate::foundry::router::ModelRole;
use crate::memory::WorkingMemory;
use crate::nl2sql::ir::spec::{QuerySpec, Shape};
use crate::nl2sql::prepare::{
    PreparedNlQuery, SourceScope, link_auto_source, prepare_with_cards, try_deterministic,
};
use crate::nl2sql::spec::TableCard;
use crate::rag::{expand_queries_with, prepare_queries_with};
use crate::retrieval::{self, Passage, RetrievalFilter};
use crate::router::RouteClass;
use crate::state::AppState;
use crate::telemetry::RequestTrace;

use crate::answer::provenance::{connector_error, validation_error};

use super::{ExecInput, ExecOutcome, LadderEnd, RungReport, Rungs};

/// Live rung implementations plus the payload each one produced.
pub(super) struct LiveRungs<'a> {
    state: &'a AppState,
    user: AuthUser,
    memory: &'a WorkingMemory,
    trace: &'a RequestTrace,
    /// The question after anaphora resolution.
    question: String,
    scope: Vec<String>,
    scope_explicit: bool,
    /// `state.catalog()` narrowed to `scope` — the only catalog the DocDb rungs
    /// see, so a planner can never name a collection outside the allow-list.
    scoped_catalog: Catalog,
    intent: QueryIntent,
    /// The IR spec from Tier 1.5, when the router produced one.
    query_spec: Option<QuerySpec>,
    started: Instant,
    total_budget: Duration,

    // --- payloads -----------------------------------------------------------
    linked: Option<(String, Vec<TableCard>)>,
    sql: Option<PreparedNlQuery>,
    agg: Option<(RunAggregation, Vec<AggRow>, Vec<Document>)>,
    list_out: Option<(RunList, Vec<Value>, u64, String)>,
    passages: Option<Vec<Passage>>,
    filter: RetrievalFilter,
    /// `true` when `filter` narrows on something the router *inferred* (a patient
    /// key) rather than something the user stated. Only an inferred narrowing may
    /// be relaxed — relaxing a cohort filter would answer about other patients.
    filter_inferred: bool,
    cohort: Option<Box<ExecOutcome>>,
    cohort_truncated: Option<(usize, usize)>,
}

impl<'a> LiveRungs<'a> {
    pub(super) fn new(state: &'a AppState, input: &ExecInput<'a>) -> Self {
        let catalog: Arc<Catalog> = state.catalog();
        let scoped_catalog = catalog.scoped(input.scope);

        // Hook 3 (plan 05 §3): the read allow-list applied to retrieval candidates.
        let mut filter = RetrievalFilter::for_scope(input.scope.to_vec(), input.scope_explicit);
        if let Some(source_id) = &input.decision.source_id {
            filter.source_ids = vec![source_id.clone()];
        }
        let mut filter_inferred = false;
        if let Some(patient_key) = &input.decision.entities.patient_key {
            filter.patient_key = Some(patient_key.clone());
            // A patient key narrows hard even on Ask (precision first), but it was
            // inferred from the question text, so it is the one thing rung 6 may
            // drop if it empties the result set.
            filter.explicit = true;
            filter_inferred = true;
        }

        LiveRungs {
            state,
            user: input.user.clone(),
            memory: input.memory,
            trace: input.trace,
            question: input.question.to_string(),
            scope: input.scope.to_vec(),
            scope_explicit: input.scope_explicit,
            scoped_catalog,
            intent: routed_intent(input),
            query_spec: input.decision.query_spec.clone(),
            started: Instant::now(),
            total_budget: input.budget.total,
            linked: None,
            sql: None,
            agg: None,
            list_out: None,
            passages: None,
            filter,
            filter_inferred,
            cohort: None,
            cohort_truncated: None,
        }
    }

    /// The `SourceScope` for SQL linking and planning: the agent's tables, plus
    /// the router's source pin when it named one.
    fn source_scope(&self) -> SourceScope {
        SourceScope {
            tables: self.scope.clone(),
            pin_source: self.filter.source_ids.first().cloned(),
        }
    }

    /// Turn the stashed payload into the outcome the routes will render.
    ///
    /// `LadderEnd::Exhausted` deliberately yields `Semantic` with whatever rung 6
    /// returned (usually nothing): the caller then refuses honestly instead of
    /// narrating from an empty context. Inventing a separate variant here would
    /// move that decision somewhere less visible, not remove it.
    pub(super) fn into_outcome(self, end: LadderEnd, input: &ExecInput<'_>) -> ExecOutcome {
        match end {
            LadderEnd::Conversational => ExecOutcome::Conversational,
            LadderEnd::Clarify => match &input.decision.class {
                RouteClass::Clarify { question, slot } => ExecOutcome::Clarify {
                    question: question.clone(),
                    slot: slot.clone(),
                    options: Vec::new(),
                },
                // `LadderStart::Clarify` only comes from the Clarify class, so this
                // is unreachable; answer conversationally rather than panic if the
                // router's taxonomy changes.
                _ => ExecOutcome::Conversational,
            },
            LadderEnd::DeterministicSql | LadderEnd::ModelSql => match self.sql {
                Some(prepared) => ExecOutcome::SourceSql {
                    spec: prepared
                        .spec
                        .as_ref()
                        .and_then(|spec| serde_json::to_value(spec).ok()),
                    source_id: prepared.source_id,
                    sql: prepared.sql,
                    columns: prepared.columns,
                    rows: prepared.rows,
                    explanation: prepared.explanation,
                },
                None => ExecOutcome::Conversational,
            },
            LadderEnd::Aggregation => match self.agg {
                Some((spec, rows, pipeline)) => ExecOutcome::DocDbAgg {
                    spec,
                    rows,
                    pipeline,
                },
                None => ExecOutcome::Conversational,
            },
            LadderEnd::List => match self.list_out {
                Some((spec, rows, total, citations_json)) => ExecOutcome::DocDbList {
                    spec,
                    rows,
                    total,
                    citations_json,
                },
                None => ExecOutcome::Conversational,
            },
            LadderEnd::Retrieval | LadderEnd::Exhausted => {
                let passages = self.passages.unwrap_or_default();
                match self.cohort {
                    Some(cohort) => ExecOutcome::Hybrid {
                        cohort,
                        passages,
                        filter: self.filter,
                        truncated: self.cohort_truncated,
                    },
                    None => ExecOutcome::Semantic {
                        passages,
                        filter: self.filter,
                    },
                }
            }
        }
    }

    /// Expand the question into the multi-query set rung 6 searches with.
    /// Falls back to the history-aware rewriter, then to the question itself, so
    /// retrieval never runs on an empty query list.
    async fn retrieval_queries(&self) -> Vec<String> {
        let Ok(foundry) = self.state.foundry() else {
            return vec![self.question.clone()];
        };
        let rewrite_spec = self.state.spec_for(ModelRole::Rewrite);
        let queries =
            expand_queries_with(foundry, &rewrite_spec, &self.state.config, &self.question).await;
        if !queries.is_empty() {
            return queries;
        }
        let (_standalone, queries) = prepare_queries_with(
            foundry,
            &rewrite_spec,
            &self.state.config,
            &self.memory.rewrite_turns(),
            &self.question,
        )
        .await;
        if queries.is_empty() {
            vec![self.question.clone()]
        } else {
            queries
        }
    }
}

/// The intent the ladder should assume. `RouteDecision::intent` is private, so
/// read it off the class the same way it does.
fn routed_intent(input: &ExecInput<'_>) -> QueryIntent {
    match &input.decision.class {
        RouteClass::Structured { intent, .. } => *intent,
        RouteClass::Hybrid { cohort_intent } => *cohort_intent,
        // A class with no intent never reaches rung 4 or 5 (`aggregation_in_scope`
        // and the intent gate both refuse), so the value only has to be inert.
        _ => QueryIntent::Narrative,
    }
}

// ---------------------------------------------------------------------------
// The rungs
// ---------------------------------------------------------------------------

impl Rungs for LiveRungs<'_> {
    fn intent(&self) -> QueryIntent {
        self.intent
    }

    fn aggregation_in_scope(&self) -> bool {
        if !matches!(
            self.intent,
            QueryIntent::Aggregation | QueryIntent::Trend | QueryIntent::MultiHop
        ) {
            return false;
        }
        // A scope with nothing ingested must not consume a rung: planning a
        // pipeline over an empty catalog can only produce an invalid spec, and the
        // model call would be spent for nothing.
        !self.scoped_catalog.collections.is_empty()
    }

    fn budget_exhausted(&self) -> bool {
        self.started.elapsed() >= self.total_budget
    }

    fn may_relax_filter(&self) -> bool {
        self.state.config.router.retrieval_filter_relax && self.filter_inferred
    }

    // --- rung 1 -------------------------------------------------------------
    async fn link(&mut self) -> RungReport {
        let scope = self.source_scope();
        match link_auto_source(self.state, &self.question, Some(&scope)).await {
            Ok(Some((source_id, cards))) if !cards.is_empty() => {
                self.linked = Some((source_id, cards));
                RungReport::Hit
            }
            // No candidate source, or a source with no usable cards. Both mean
            // "SQL is not possible here", not "SQL failed".
            Ok(_) => RungReport::Miss("no source in scope has linkable schema cards".into()),
            Err(error) => RungReport::Miss(connector_error(error.kind_label())),
        }
    }

    // --- rung 2 -------------------------------------------------------------
    async fn deterministic_sql(&mut self) -> RungReport {
        let Some((source_id, cards)) = self.linked.clone() else {
            return RungReport::Miss("link rung produced no cards".into());
        };
        let source_kind = match crate::connectors::routes::load_spec(
            &self.state.db,
            &self.state.config,
            &source_id,
        )
        .await
        {
            Ok(spec) => spec.kind,
            Err(error) => return RungReport::Miss(connector_error(error.kind_label())),
        };
        let allowed_tables: Vec<String> =
            cards.iter().map(|card| card.table_name.clone()).collect();

        let prepared = try_deterministic(
            self.state,
            &source_id,
            &self.question,
            &cards,
            source_kind,
            &allowed_tables,
        )
        .await;

        match prepared {
            Some(prepared) => {
                self.sql = Some(prepared);
                RungReport::Hit
            }
            // `try_deterministic` returns `Option`, collapsing "the compiler
            // produced no SQL", "validate_sql rejected it", and "the connector
            // failed" into one `None`. A plain `Miss` is the honest report;
            // claiming `ValidateMiss` would invent a distinction the delegate does
            // not expose. Splitting it means changing `try_deterministic`'s return
            // type, which belongs to plan 03's file, not this one.
            None => {
                RungReport::Miss("deterministic compile, validation, or execution missed".into())
            }
        }
    }

    // --- rung 3 -------------------------------------------------------------
    async fn model_sql(&mut self) -> RungReport {
        let Some((source_id, cards)) = self.linked.clone() else {
            return RungReport::Miss("link rung produced no cards".into());
        };
        match prepare_with_cards(self.state, &source_id, &self.question, cards).await {
            Ok(prepared) => {
                self.sql = Some(prepared);
                RungReport::Hit
            }
            Err(error) => {
                // `prepare_with_cards` surfaces validation rejections as
                // `BadRequest` after its one repair attempt; anything else is a
                // planning or connector failure.
                let label = error.kind_label();
                if matches!(error, AppError::BadRequest(_)) {
                    RungReport::ValidateMiss(validation_error(label))
                } else {
                    RungReport::ExecuteMiss(connector_error(label))
                }
            }
        }
    }

    // --- rung 4 -------------------------------------------------------------
    async fn aggregation(&mut self) -> RungReport {
        // Deterministic first: when Tier 1.5 parsed the question into IR, the
        // aggregation is a translation, not a guess — and it agrees with the SQL
        // rung by construction, because both read the same spec.
        let deterministic = self
            .query_spec
            .as_ref()
            .and_then(|spec| spec_from_query_spec(spec, &self.scoped_catalog));

        let planned: RunAggregation = match deterministic {
            Some(DocDbSpec::Aggregation(agg)) => agg,
            // The IR translated to a *list*, not an aggregation: that is rung 5's
            // job, so fall back to the planner for an aggregation view.
            Some(DocDbSpec::List(_)) | None => {
                let Ok(foundry) = self.state.foundry() else {
                    return RungReport::Miss("no local model instance for planning".into());
                };
                let planner_spec = self.state.spec_for(ModelRole::PlanSpec);
                let planner_system =
                    crate::answer::build_agg_planner_system(&self.scoped_catalog, self.intent);
                match foundry
                    .plan_aggregation(&planner_spec, &planner_system, &self.question)
                    .await
                {
                    Ok(planned) => planned,
                    Err(error) => return RungReport::Miss(error.kind_label().to_string()),
                }
            }
        };

        // Models propose, Rust decides — the same validator the `/aggregate` route
        // uses, against the *scoped* catalog.
        let validated = match validate::validate(&planned, &self.scoped_catalog) {
            Ok(validated) => validated,
            Err(error) => return RungReport::ValidateMiss(validation_error(error.kind_label())),
        };

        let scope_sources = self.filter.source_ids.clone();
        match execute::run_scoped(&self.state.db, &self.user, &validated, &scope_sources).await {
            Ok((rows, pipeline)) => {
                // Bounded failure (plan 04 §2.1): a Grouped/Trend aggregation that
                // legitimately has no buckets is a **hit with an empty result**. A
                // *scalar* with no row means the pipeline produced nothing at all,
                // which is a miss.
                let scalar = validated.group_by.is_empty() && validated.time_bucket.is_none();
                let empty_scalar = scalar && rows.is_empty();
                self.agg = Some((validated, rows, pipeline));
                if empty_scalar {
                    RungReport::ExecuteMiss("aggregation produced no scalar row".into())
                } else {
                    RungReport::Hit
                }
            }
            Err(error) => RungReport::ExecuteMiss(connector_error(error.kind_label())),
        }
    }

    // --- rung 5 -------------------------------------------------------------
    async fn list(&mut self) -> RungReport {
        let deterministic = self
            .query_spec
            .as_ref()
            .and_then(|spec| spec_from_query_spec(spec, &self.scoped_catalog));

        let planned: RunList = match deterministic {
            Some(DocDbSpec::List(list_spec)) => list_spec,
            Some(DocDbSpec::Aggregation(_)) | None => {
                let Ok(foundry) = self.state.foundry() else {
                    return RungReport::Miss("no local model instance for planning".into());
                };
                let planner_spec = self.state.spec_for(ModelRole::PlanSpec);
                let planner_system =
                    crate::answer::build_list_planner_system(&self.scoped_catalog);
                match foundry
                    .plan_list(&planner_spec, &planner_system, &self.question)
                    .await
                {
                    Ok(planned) => planned,
                    Err(error) => return RungReport::Miss(error.kind_label().to_string()),
                }
            }
        };

        let validated = match list::validate_list(&planned, &self.scoped_catalog) {
            Ok(validated) => validated,
            Err(error) => return RungReport::ValidateMiss(validation_error(error.kind_label())),
        };

        match list::run(&self.state.db, &self.user, &validated).await {
            Ok((rows, total, _pipeline)) => {
                // Zero rows on a List is a **Hit with an empty result** — the
                // narrator says "none matched". Falling through to retrieval here
                // would swap an exact "none" for a fuzzy narrative, which is a
                // clinical-safety defect rather than a nicety (plan 04 §2.1).
                let citations_json = list::rows_to_citations_json(&rows, &validated.collection);
                self.list_out = Some((validated, rows, total, citations_json));
                RungReport::Hit
            }
            Err(error) => RungReport::ExecuteMiss(connector_error(error.kind_label())),
        }
    }

    // --- rung 6 -------------------------------------------------------------
    async fn retrieval(&mut self, relaxed: bool) -> RungReport {
        let queries = self.retrieval_queries().await;
        let broad = retrieval::is_broad_question(&self.question);

        let mut filter = self.filter.clone();
        if relaxed {
            // Drop only the inferred narrowing. The scope stays: it is the agent's
            // read allow-list, not a guess.
            filter.patient_key = None;
            filter.explicit = self.scope_explicit;
        }

        let permit = match self.state.admission.retrieval().await {
            Ok(permit) => permit,
            Err(error) => return RungReport::Miss(connector_error(error.kind_label())),
        };
        let result = retrieval::retrieve_observed_filtered(
            &self.state.db,
            &self.state.config,
            &queries,
            self.state.config.retrieval_mode,
            self.state.config.rerank_enabled && !broad,
            self.state.config.context_top_k,
            self.trace,
            &filter,
        )
        .await;
        drop(permit);

        match result {
            Ok(passages) => {
                self.passages = Some(passages);
                RungReport::Hit
            }
            Err(error) => RungReport::Miss(connector_error(error.kind_label())),
        }
    }

    fn retrieval_was_emptied_by_filter(&self) -> bool {
        // "Emptied by the filter" is inferred, not measured: running the
        // unfiltered search a second time purely to attribute the emptiness would
        // double the embed + search cost of every zero-result turn. The only case
        // that matters is an inferred narrowing that removed everything, and that
        // is exactly this condition.
        self.filter_inferred
            && self
                .passages
                .as_ref()
                .map(|passages| passages.is_empty())
                .unwrap_or(false)
    }

    // --- Hybrid cohort (plan 04 §5) -----------------------------------------
    async fn cohort(&mut self) -> RungReport {
        let max = self.state.config.router.hybrid_cohort_max;

        // The cohort is a *list of keys*, whatever the question's own shape.
        let Some(cohort_spec) = self.query_spec.as_ref().map(|spec| {
            let mut cohort = spec.clone();
            cohort.shape = Shape::List;
            cohort.measures.clear();
            cohort.dimensions.clear();
            cohort.order.clear();
            // One over the cap, so `total > rows.len()` reveals truncation without
            // a second count query.
            cohort.limit = Some(max as u32);
            cohort
        }) else {
            return RungReport::Miss("hybrid cohort needs a parsed spec".into());
        };

        let translated = spec_from_query_spec(&cohort_spec, &self.scoped_catalog);
        let Some(DocDbSpec::List(list_spec)) = translated else {
            return RungReport::Miss("cohort spec does not translate to a list".into());
        };

        let validated = match list::validate_list(&list_spec, &self.scoped_catalog) {
            Ok(validated) => validated,
            Err(error) => return RungReport::ValidateMiss(validation_error(error.kind_label())),
        };

        let (rows, total, _pipeline) =
            match list::run(&self.state.db, &self.user, &validated).await {
                Ok(triple) => triple,
                Err(error) => return RungReport::ExecuteMiss(connector_error(error.kind_label())),
            };

        if rows.is_empty() {
            // An empty cohort is not a Hybrid answer. Saying so keeps the narrator
            // from summarising the whole corpus as if it were the cohort.
            return RungReport::ExecuteMiss("cohort matched no rows".into());
        }

        // Restrict rung 6 to the cohort's rows. This narrowing is *stated* — the
        // user asked for those patients — so `filter_inferred` stays false and it
        // is never relaxed.
        let row_pks: Vec<String> = rows
            .iter()
            .filter_map(|row| row.get("row_pk").and_then(Value::as_str))
            .map(str::to_string)
            .take(max)
            .collect();
        if !row_pks.is_empty() {
            self.filter.row_pks = row_pks;
            self.filter.explicit = true;
        }
        if total as usize > rows.len() {
            self.cohort_truncated = Some((rows.len(), total as usize));
        }

        let citations_json = list::rows_to_citations_json(&rows, &validated.collection);
        self.cohort = Some(Box::new(ExecOutcome::DocDbList {
            spec: validated,
            rows,
            total,
            citations_json,
        }));
        RungReport::Hit
    }
}
