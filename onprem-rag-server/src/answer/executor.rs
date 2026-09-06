//! The one structured-execution ladder shared by `/chat` and every
//! `/agents/<kind>` (plan 04 §2).
//!
//! ```text
//! link source -> deterministic SQL -> model SQL -> validate -> execute
//!             -> validated DocDb aggregation -> validated DocDb list
//!             -> filtered semantic retrieval
//! ```
//!
//! Before this module the ladder existed three times over (`rag/routes.rs::chat`,
//! `agents/routes.rs::agent`, `resolve_followup_sql`), each choosing slightly
//! different fallbacks and emitting slightly different SSE, with provenance
//! inferred client-side from *which* events happened to arrive. Now the ladder is
//! written once and reports what it did explicitly.
//!
//! # Models propose, Rust decides
//!
//! Every SQL statement executed from here goes through the single
//! `nl2sql::validate::validate_sql` guard, because every SQL rung delegates to
//! `nl2sql::prepare` — which owns that guard — instead of re-implementing
//! compile/validate/execute. There is deliberately **no** SQL string built in
//! this file. A new code path that skips an existing guard is a recognised defect
//! class in this codebase, and the cheapest way to not have it is to have no
//! second path.
//!
//! # Testability: the `Rungs` seam
//!
//! [`Rungs`] separates *what each rung does* from *how the ladder branches*.
//! [`run_ladder`] contains all the branching and no I/O, so plan 04 §8's state
//! machine test drives it with a mock that records the order rungs were asked
//! for. `LiveRungs` is the only implementation that touches the database.
//!
//! # Bounded failure (plan 04 §2.1, plan 04a §5)
//!
//! A rung *misses* on: parse miss, bind error, validation error, connector error,
//! timeout, a zero-row **scalar** result when the question implied existence, or
//! a cost-preflight rejection. Zero rows on a `List` or `Grouped` shape is a
//! **hit with an empty result** — the narrator says "none", and no fallback runs.
//! Swapping an exact "none" for a fuzzy narrative is a clinical-safety defect,
//! so the distinction lives in the [`ExecOutcome`] variant rather than being
//! inferred from an empty vector.

use std::time::{Duration, Instant};

use mongodb::bson::Document;
use serde_json::Value;

use crate::agents::kind::AgentMode;
use crate::aggregation::execute::AggRow;
use crate::aggregation::intent::QueryIntent;
use crate::aggregation::spec::{RunAggregation, RunList};
use crate::auth::guard::AuthUser;
use crate::error::AppResult;
use crate::memory::WorkingMemory;
use crate::nl2sql::ir::spec::MissingSlot;
use crate::retrieval::{Passage, RetrievalFilter};
use crate::router::focus::ConversationFocus;
use crate::router::{RouteClass, RouteDecision, StructuredBackend};
use crate::state::AppState;
use crate::telemetry::RequestTrace;

use super::provenance::{
    BACKEND_DOCUMENT_DB, BACKEND_HYBRID, BACKEND_NONE, BACKEND_SEMANTIC, BACKEND_SOURCE_SQL,
    Provenance, Rung, RungResult,
};

// ---------------------------------------------------------------------------
// Input
// ---------------------------------------------------------------------------

/// Everything the ladder needs about one turn.
pub struct ExecInput<'a> {
    pub decision: &'a RouteDecision,
    /// `decision.resolved_question` — anaphora already resolved by the router.
    pub question: &'a str,
    pub user: &'a AuthUser,
    pub memory: &'a WorkingMemory,
    pub focus: &'a ConversationFocus,
    pub mode: AgentMode,
    /// Read allow-list for this turn (the agent's scope, else `decision.scope`).
    pub scope: &'a [String],
    /// `true` when the scope was *stated* — the user picked a service-line agent
    /// tab — so an empty result is an honest "not in this department's data".
    /// `false` for Ask, where the scope is advisory. Mirrors
    /// `AgentKind::scope_is_explicit`; passed in rather than re-derived so the
    /// executor does not need to know about `AgentKind`.
    pub scope_explicit: bool,
    pub trace: &'a RequestTrace,
    pub budget: ExecBudget,
}

/// Per-rung deadlines. Defaults come from config (plan 04 §2.1).
#[derive(Debug, Clone, Copy)]
pub struct ExecBudget {
    pub sql_plan: Duration,
    pub sql_exec: Duration,
    pub agg: Duration,
    pub total: Duration,
}

impl ExecBudget {
    /// Budget from the runtime config: `sql_plan` =
    /// `ONPREM_NL2SQL_PLAN_TIMEOUT_SECS`, `sql_exec` =
    /// `ONPREM_NL2SQL_TIMEOUT_SECS`, `agg` = 30 s (the pipeline's own
    /// `maxTimeMS`), `total` = `ONPREM_EXEC_TOTAL_TIMEOUT_SECS`.
    pub fn from_config(config: &crate::config::Config) -> Self {
        ExecBudget {
            sql_plan: Duration::from_secs(config.router.nl2sql_plan_timeout_secs),
            sql_exec: Duration::from_secs(config.router.nl2sql_timeout_secs),
            agg: Duration::from_secs(30),
            total: Duration::from_secs(config.router.exec_total_timeout_secs),
        }
    }
}

// ---------------------------------------------------------------------------
// Outcome
// ---------------------------------------------------------------------------

/// What the ladder produced. Each variant carries everything the SSE contract
/// (plan 04 §6) needs to emit, so the routes hold no execution logic of their own.
pub enum ExecOutcome {
    SourceSql {
        source_id: String,
        sql: String,
        /// The IR spec behind the SQL, pre-serialised for the `sql` SSE payload.
        ///
        /// **Divergence from plan 04 §2's prose**, which types this
        /// `Option<QuerySpec>`: the deterministic rung's spec is a `QuerySpec`
        /// (from `decision.query_spec`) while the model rung's is a `PlannedSpec`
        /// DTO (`PreparedNlQuery::spec`). They are different Rust types with the
        /// same job, and the bridge mirror already types the field
        /// `Option<serde_json::Value>` (`commands.rs` `SqlPayload::spec`). Storing
        /// the serialised form keeps one field instead of two and matches the wire
        /// contract exactly.
        spec: Option<Value>,
        columns: Vec<String>,
        rows: Vec<Vec<Value>>,
        explanation: String,
    },
    DocDbAgg {
        spec: RunAggregation,
        rows: Vec<AggRow>,
        pipeline: Vec<Document>,
    },
    DocDbList {
        spec: RunList,
        rows: Vec<Value>,
        total: u64,
        /// Rows rendered as citations so the UI shows them as sources.
        /// Pre-serialised by `list::rows_to_citations_json` — list rows are
        /// records, not chunks, so they are not `Passage`s.
        citations_json: String,
    },
    Semantic {
        passages: Vec<Passage>,
        filter: RetrievalFilter,
    },
    Hybrid {
        /// The cohort query result (a `SourceSql` or `DocDbList`).
        cohort: Box<ExecOutcome>,
        passages: Vec<Passage>,
        filter: RetrievalFilter,
        /// `Some((kept, total))` when the cohort was larger than
        /// `ONPREM_HYBRID_COHORT_MAX` and the narrator must say "first N of M".
        truncated: Option<(usize, usize)>,
    },
    Clarify {
        question: String,
        slot: MissingSlot,
        /// Concrete choices to offer. Empty when the router had none to offer —
        /// the clarifying question still stands on its own.
        options: Vec<String>,
    },
    Conversational,
}

impl ExecOutcome {
    /// The `Provenance::backend` label for this outcome.
    pub fn backend(&self) -> &'static str {
        match self {
            ExecOutcome::SourceSql { .. } => BACKEND_SOURCE_SQL,
            ExecOutcome::DocDbAgg { .. } | ExecOutcome::DocDbList { .. } => BACKEND_DOCUMENT_DB,
            ExecOutcome::Semantic { .. } => BACKEND_SEMANTIC,
            ExecOutcome::Hybrid { .. } => BACKEND_HYBRID,
            ExecOutcome::Clarify { .. } | ExecOutcome::Conversational => BACKEND_NONE,
        }
    }

    /// `true` when the rung ran correctly and found nothing.
    ///
    /// Plan 04a §5: "ran correctly, found nothing" must be distinguishable from
    /// "failed to run" **without** inspecting a vector's length at the call site.
    /// A miss never reaches here — it is a different variant or a different
    /// provenance path — so this is unambiguous.
    pub fn is_empty_result(&self) -> bool {
        match self {
            ExecOutcome::SourceSql { rows, .. } => rows.is_empty(),
            ExecOutcome::DocDbAgg { rows, .. } => rows.is_empty(),
            ExecOutcome::DocDbList { rows, .. } => rows.is_empty(),
            ExecOutcome::Semantic { passages, .. } => passages.is_empty(),
            ExecOutcome::Hybrid { passages, .. } => passages.is_empty(),
            ExecOutcome::Clarify { .. } | ExecOutcome::Conversational => false,
        }
    }
}

// ---------------------------------------------------------------------------
// The `Rungs` seam
// ---------------------------------------------------------------------------

/// How one rung ended, with no payload — the payload stays inside the
/// implementation so the state machine has nothing to carry and the mock has
/// nothing to fabricate.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RungReport {
    /// Ran and produced a result — including a correctly empty one.
    Hit,
    /// Could not produce a statement at all (parse miss, bind error, no cards).
    Miss(String),
    /// Produced a statement that `validate_sql` (or `aggregation::validate`)
    /// rejected.
    ValidateMiss(String),
    /// Validated, but execution failed: connector error, timeout, cost preflight,
    /// or a zero-row scalar where the question implied existence.
    ExecuteMiss(String),
}

impl RungReport {
    fn reason(&self) -> Option<&str> {
        match self {
            RungReport::Hit => None,
            RungReport::Miss(r) | RungReport::ValidateMiss(r) | RungReport::ExecuteMiss(r) => {
                Some(r.as_str())
            }
        }
    }

    fn is_hit(&self) -> bool {
        matches!(self, RungReport::Hit)
    }
}

/// Where the ladder starts, derived from `RouteDecision::class`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LadderStart {
    /// `Conversational | ConversationMeta`.
    Conversational,
    /// `Clarify { .. }`.
    Clarify,
    /// `Semantic` — rung 6 only.
    Retrieval,
    /// `Structured { backend: SourceSql }` — rungs 1,2,3,4,5,6.
    SourceSql,
    /// `Structured { backend: DocDb }` — rungs 4,5,6.
    DocDb,
    /// `Hybrid` — rungs 1..4 for the cohort, then rung 6 with the cohort filter.
    Hybrid,
}

impl LadderStart {
    /// Plan 04 §2.1's `match decision.class`.
    pub fn from_class(class: &RouteClass) -> Self {
        match class {
            RouteClass::Conversational
            | RouteClass::ConversationMeta
            | RouteClass::Capability => LadderStart::Conversational,
            RouteClass::Clarify { .. } => LadderStart::Clarify,
            RouteClass::Semantic => LadderStart::Retrieval,
            RouteClass::Structured { backend, .. } => match backend {
                StructuredBackend::SourceSql => LadderStart::SourceSql,
                StructuredBackend::DocDb => LadderStart::DocDb,
            },
            RouteClass::Hybrid { .. } => LadderStart::Hybrid,
        }
    }
}

/// Which rung produced the answer, or that none did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LadderEnd {
    Conversational,
    Clarify,
    DeterministicSql,
    ModelSql,
    Aggregation,
    List,
    Retrieval,
    /// Every rung the ladder was allowed to try missed. The caller answers with
    /// a refusal rather than inventing content.
    Exhausted,
}

/// The rungs, as behaviour the ladder can ask for. Implemented once for real
/// (`LiveRungs`) and once per test (plan 04 §8).
///
/// Every method takes `&mut self` so an implementation can stash the payload it
/// produced; the ladder never sees payloads.
#[allow(async_fn_in_trait)]
pub trait Rungs {
    /// The routed intent — decides whether rungs 4 and 5 apply at all.
    fn intent(&self) -> QueryIntent;

    /// `true` when rung 4 is applicable: intent is `Aggregation` or `Trend`
    /// **and** the catalog has at least one collection inside the scope. A
    /// scope with nothing ingested must not consume a rung.
    fn aggregation_in_scope(&self) -> bool;

    /// `true` once `ExecBudget::total` has elapsed. Checked before each rung so
    /// the skip is visible as `Skipped("budget")` (plan 04a §6).
    fn budget_exhausted(&self) -> bool;

    /// `true` when a retrieval filter that empties the result set may be relaxed:
    /// the scope was *inferred*, not stated by the user, and
    /// `ONPREM_RETRIEVAL_FILTER_RELAX` is on.
    fn may_relax_filter(&self) -> bool;

    /// Rung 1 — pick one source and link its schema cards, scope-filtered.
    async fn link(&mut self) -> RungReport;
    /// Rung 2 — deterministic compile from the IR spec.
    async fn deterministic_sql(&mut self) -> RungReport;
    /// Rung 3 — model SQL proposal (still validated by the same guard).
    async fn model_sql(&mut self) -> RungReport;
    /// Rung 4 — validated DocumentDB aggregation.
    async fn aggregation(&mut self) -> RungReport;
    /// Rung 5 — validated DocumentDB list.
    async fn list(&mut self) -> RungReport;
    /// Rung 6 — filtered semantic retrieval. `relaxed` drops the inferred filter.
    async fn retrieval(&mut self, relaxed: bool) -> RungReport;
    /// `true` when the last `retrieval` call returned no passages *because of* the
    /// filter (so relaxing it could help).
    fn retrieval_was_emptied_by_filter(&self) -> bool;
    /// Rung 1..4 for a Hybrid cohort; `Hit` only when the cohort yielded rows.
    async fn cohort(&mut self) -> RungReport;
}

// ---------------------------------------------------------------------------
// The state machine
// ---------------------------------------------------------------------------

/// Drive the ladder. Pure control flow: every side effect is behind [`Rungs`].
///
/// Records each attempt on `prov` in the order it happened, so `provenance.path`
/// is the literal history rather than a reconstruction.
pub async fn run_ladder<R: Rungs>(
    rungs: &mut R,
    start: LadderStart,
    prov: &mut Provenance,
) -> LadderEnd {
    match start {
        LadderStart::Conversational => LadderEnd::Conversational,

        LadderStart::Clarify => {
            prov.note(Rung::Clarify);
            LadderEnd::Clarify
        }

        LadderStart::Retrieval => retrieval_rung(rungs, prov).await,

        LadderStart::DocDb => docdb_rungs(rungs, prov).await,

        LadderStart::SourceSql => {
            // --- rung 1: link -------------------------------------------------
            if skip_for_budget(rungs, prov, Rung::Link(RungResult::Skipped(BUDGET))) {
                return retrieval_rung(rungs, prov).await;
            }
            let started = Instant::now();
            let link = rungs.link().await;
            record_rung(prov, Rung::Link, &link, started.elapsed());
            if !link.is_hit() {
                // No source, no cards: SQL is impossible, not failed. Rung 4 next.
                return docdb_rungs(rungs, prov).await;
            }

            // --- rung 2: deterministic SQL ------------------------------------
            if skip_for_budget(
                rungs,
                prov,
                Rung::DeterministicSql(RungResult::Skipped(BUDGET)),
            ) {
                return retrieval_rung(rungs, prov).await;
            }
            let started = Instant::now();
            let det = rungs.deterministic_sql().await;
            record_sql_rung(prov, Rung::DeterministicSql, &det, started.elapsed());
            if det.is_hit() {
                return LadderEnd::DeterministicSql;
            }

            // --- rung 3: model SQL, tried exactly once ------------------------
            if skip_for_budget(rungs, prov, Rung::ModelSql(RungResult::Skipped(BUDGET))) {
                return retrieval_rung(rungs, prov).await;
            }
            let started = Instant::now();
            let model = rungs.model_sql().await;
            record_sql_rung(prov, Rung::ModelSql, &model, started.elapsed());
            if model.is_hit() {
                return LadderEnd::ModelSql;
            }

            docdb_rungs(rungs, prov).await
        }

        LadderStart::Hybrid => {
            if skip_for_budget(rungs, prov, Rung::Link(RungResult::Skipped(BUDGET))) {
                return retrieval_rung(rungs, prov).await;
            }
            // The cohort *must* yield rows - a Hybrid answer with no cohort is just
            // a semantic answer, and labelling it Hybrid would overstate it.
            let started = Instant::now();
            let cohort = rungs.cohort().await;
            record_sql_rung(prov, Rung::List, &cohort, started.elapsed());
            // Rung 6 runs either way; on a cohort hit it runs with the cohort keys
            // as an explicit filter, and on a miss the miss is on the record so the
            // answer is never presented as being about the cohort.
            retrieval_rung(rungs, prov).await
        }
    }
}

/// Rungs 4 -> 5 -> 6.
async fn docdb_rungs<R: Rungs>(rungs: &mut R, prov: &mut Provenance) -> LadderEnd {
    let intent = rungs.intent();
    let list_applies = matches!(intent, QueryIntent::Enumeration | QueryIntent::Lookup);

    // --- rung 4: aggregation -------------------------------------------------
    if rungs.aggregation_in_scope() {
        if skip_for_budget(rungs, prov, Rung::Aggregation(RungResult::Skipped(BUDGET))) {
            return retrieval_rung(rungs, prov).await;
        }
        let started = Instant::now();
        let agg = rungs.aggregation().await;
        record_sql_rung(prov, Rung::Aggregation, &agg, started.elapsed());
        if agg.is_hit() {
            return LadderEnd::Aggregation;
        }
    } else {
        // "Not applicable" is not "failed": record it, with a reason that cannot be
        // mistaken for an error.
        prov.note(Rung::Aggregation(RungResult::Skipped(
            "intent or scope has no aggregation",
        )));
    }

    // --- rung 5: list --------------------------------------------------------
    if list_applies {
        if skip_for_budget(rungs, prov, Rung::List(RungResult::Skipped(BUDGET))) {
            return retrieval_rung(rungs, prov).await;
        }
        let started = Instant::now();
        let list = rungs.list().await;
        record_sql_rung(prov, Rung::List, &list, started.elapsed());
        if list.is_hit() {
            return LadderEnd::List;
        }
    } else {
        prov.note(Rung::List(RungResult::Skipped("intent is not a list")));
    }

    retrieval_rung(rungs, prov).await
}

/// Rung 6, including the one permitted relaxation retry.
async fn retrieval_rung<R: Rungs>(rungs: &mut R, prov: &mut Provenance) -> LadderEnd {
    let started = Instant::now();
    let first = rungs.retrieval(false).await;
    let elapsed = started.elapsed();

    if !first.is_hit() {
        record_rung(prov, Rung::Retrieval, &first, elapsed);
        return LadderEnd::Exhausted;
    }

    if rungs.retrieval_was_emptied_by_filter() && rungs.may_relax_filter() {
        // The filter, not the corpus, produced the empty set - and the filter was
        // inferred rather than stated. Exactly one unfiltered retry, never a loop.
        prov.push(
            Rung::Retrieval(RungResult::Miss(FILTER_RELAXED.to_string())),
            elapsed.as_millis() as u64,
        );
        let started = Instant::now();
        let retry = rungs.retrieval(true).await;
        record_rung(prov, Rung::Retrieval, &retry, started.elapsed());
        return if retry.is_hit() {
            LadderEnd::Retrieval
        } else {
            LadderEnd::Exhausted
        };
    }

    record_rung(prov, Rung::Retrieval, &first, elapsed);
    LadderEnd::Retrieval
}

// --- recording helpers -----------------------------------------------------

/// The one skip reason `Provenance::had_budget_skip` looks for. A constant rather
/// than a literal at five call sites, so the two cannot drift apart.
const BUDGET: &str = "budget";

/// Recorded when rung 6 drops an inferred filter and searches again.
const FILTER_RELAXED: &str = "filter relaxed";

/// Record `skipped` and return `true` when the total budget is spent.
fn skip_for_budget<R: Rungs>(rungs: &R, prov: &mut Provenance, skipped: Rung) -> bool {
    if rungs.budget_exhausted() {
        prov.note(skipped);
        true
    } else {
        false
    }
}

/// Record one rung as a single entry: `Hit`, or `Miss` with its reason.
fn record_rung(
    prov: &mut Provenance,
    wrap: fn(RungResult) -> Rung,
    report: &RungReport,
    elapsed: Duration,
) {
    let result = match report {
        RungReport::Hit => RungResult::Hit,
        other => RungResult::Miss(other.reason().unwrap_or("miss").to_string()),
    };
    prov.push(wrap(result), elapsed.as_millis() as u64);
}

/// Record a rung with the compile -> validate -> execute structure, expanding its
/// report into those sub-rungs so `provenance.path` shows *where* it ended.
///
/// The distinction matters downstream: "produced no statement" is a coverage gap in
/// the compiler, while "produced a statement the guard rejected" is a correctness
/// bug in the planner. Collapsing both to `Miss` would hide which one a deployment
/// is actually hitting.
fn record_sql_rung(
    prov: &mut Provenance,
    wrap: fn(RungResult) -> Rung,
    report: &RungReport,
    elapsed: Duration,
) {
    let ms = elapsed.as_millis() as u64;
    match report {
        RungReport::Hit => {
            prov.push(wrap(RungResult::Hit), ms);
            prov.note(Rung::Validate(RungResult::Hit));
            prov.note(Rung::Execute(RungResult::Hit));
        }
        RungReport::Miss(reason) => {
            prov.push(wrap(RungResult::Miss(reason.clone())), ms);
        }
        RungReport::ValidateMiss(reason) => {
            // The rung produced a statement; the guard rejected it.
            prov.push(wrap(RungResult::Hit), ms);
            prov.note(Rung::Validate(RungResult::Miss(reason.clone())));
        }
        RungReport::ExecuteMiss(reason) => {
            prov.push(wrap(RungResult::Hit), ms);
            prov.note(Rung::Validate(RungResult::Hit));
            prov.note(Rung::Execute(RungResult::Miss(reason.clone())));
        }
    }
}

// ---------------------------------------------------------------------------
// Public entry point
// ---------------------------------------------------------------------------


/// Run the ladder for one turn and return the outcome plus its provenance.
///
/// `Err` is reserved for failures that make *any* answer impossible (no Foundry
/// instance, admission control refused). A rung that cannot answer is a `Miss`
/// in the provenance path, not an `Err`.
pub async fn run<'a>(
    state: &'a AppState,
    input: ExecInput<'a>,
) -> AppResult<(ExecOutcome, Provenance)> {
    let mut prov = Provenance {
        backend: BACKEND_NONE,
        service_line: input.decision.service_line,
        scope: input.scope.to_vec(),
        source_id: input.decision.source_id.clone(),
        ..Default::default()
    };

    let start = LadderStart::from_class(&input.decision.class);
    let mut rungs = live::LiveRungs::new(state, &input);
    let end = run_ladder(&mut rungs, start, &mut prov).await;
    let outcome = rungs.into_outcome(end, &input);
    prov.backend = outcome.backend();
    Ok((outcome, prov))
}

mod live;

#[cfg(test)]
mod ladder_tests;
