//! Plan 04 §8 bullet 1 — the ladder as a state machine, with mocked rungs.
//!
//! These tests exist because the ladder's *branching* is the part that regressed
//! repeatedly when it lived three times over in the route handlers. They drive
//! [`run_ladder`] with a `Rungs` mock that records the order rungs were asked for
//! and returns scripted reports, so every assertion is about control flow — never
//! about a database, a model, or a clock.
//!
//! They also carry plan 04a §5 and §6:
//! - §5: "ran correctly, found nothing" stays distinguishable from "failed to
//!   run" — an empty List is a `Hit` and stops the ladder.
//! - §6: a budget skip is visible in the response, as `Skipped("budget")`.

use std::cell::RefCell;

use crate::aggregation::intent::QueryIntent;
use crate::answer::provenance::{Provenance, RungResult};

use super::{LadderEnd, LadderStart, RungReport, Rungs, run_ladder};

/// A scripted rung set. Each field is what that rung will report; `calls` records
/// the order they were actually asked for.
struct MockRungs {
    intent: QueryIntent,
    aggregation_in_scope: bool,
    budget_exhausted_after: Option<usize>,
    may_relax: bool,
    retrieval_emptied_by_filter: bool,

    link: RungReport,
    deterministic_sql: RungReport,
    model_sql: RungReport,
    aggregation: RungReport,
    list: RungReport,
    /// Reports for successive `retrieval` calls; the last one repeats.
    retrieval: Vec<RungReport>,
    cohort: RungReport,

    calls: RefCell<Vec<String>>,
    retrieval_calls: RefCell<usize>,
}

impl Default for MockRungs {
    fn default() -> Self {
        MockRungs {
            intent: QueryIntent::Aggregation,
            aggregation_in_scope: true,
            budget_exhausted_after: None,
            may_relax: false,
            retrieval_emptied_by_filter: false,
            link: RungReport::Hit,
            deterministic_sql: RungReport::Hit,
            model_sql: RungReport::Hit,
            aggregation: RungReport::Hit,
            list: RungReport::Hit,
            retrieval: vec![RungReport::Hit],
            cohort: RungReport::Hit,
            calls: RefCell::new(Vec::new()),
            retrieval_calls: RefCell::new(0),
        }
    }
}

impl MockRungs {
    fn record(&self, name: &str) {
        self.calls.borrow_mut().push(name.to_string());
    }

    fn calls(&self) -> Vec<String> {
        self.calls.borrow().clone()
    }
}

impl Rungs for MockRungs {
    fn intent(&self) -> QueryIntent {
        self.intent
    }

    fn aggregation_in_scope(&self) -> bool {
        self.aggregation_in_scope
    }

    fn budget_exhausted(&self) -> bool {
        match self.budget_exhausted_after {
            // "Exhausted after N rungs have run" — the check happens *before* each
            // rung, so this makes the (N+1)-th rung the one that gets skipped.
            Some(n) => self.calls.borrow().len() >= n,
            None => false,
        }
    }

    fn may_relax_filter(&self) -> bool {
        self.may_relax
    }

    fn retrieval_was_emptied_by_filter(&self) -> bool {
        self.retrieval_emptied_by_filter
    }

    async fn link(&mut self) -> RungReport {
        self.record("link");
        self.link.clone()
    }

    async fn deterministic_sql(&mut self) -> RungReport {
        self.record("deterministic_sql");
        self.deterministic_sql.clone()
    }

    async fn model_sql(&mut self) -> RungReport {
        self.record("model_sql");
        self.model_sql.clone()
    }

    async fn aggregation(&mut self) -> RungReport {
        self.record("aggregation");
        self.aggregation.clone()
    }

    async fn list(&mut self) -> RungReport {
        self.record("list");
        self.list.clone()
    }

    async fn retrieval(&mut self, relaxed: bool) -> RungReport {
        self.record(if relaxed { "retrieval(relaxed)" } else { "retrieval" });
        let index = {
            let mut count = self.retrieval_calls.borrow_mut();
            let index = *count;
            *count += 1;
            index
        };
        self.retrieval
            .get(index)
            .or_else(|| self.retrieval.last())
            .cloned()
            .unwrap_or(RungReport::Hit)
    }

    async fn cohort(&mut self) -> RungReport {
        self.record("cohort");
        self.cohort.clone()
    }
}

/// Drive the ladder to completion on the current thread. The mock never awaits
/// anything real, so a block-on executor is unnecessary — polling once is enough,
/// but `futures::executor` is not a dependency here, so use Tokio's current-thread
/// runtime, which the crate already has.
fn drive(mock: &mut MockRungs, start: LadderStart) -> (LadderEnd, Provenance) {
    let mut prov = Provenance::default();
    let runtime = tokio::runtime::Builder::new_current_thread()
        .build()
        .expect("current-thread runtime");
    let end = runtime.block_on(run_ladder(mock, start, &mut prov));
    (end, prov)
}

/// The names in `provenance.path`, in order.
fn path(prov: &Provenance) -> Vec<&'static str> {
    prov.path.iter().map(|rung| rung.name()).collect()
}

// ---------------------------------------------------------------------------
// The documented order (plan 04 §2.1)
// ---------------------------------------------------------------------------

#[test]
fn deterministic_sql_hit_stops_the_ladder() {
    let mut mock = MockRungs::default();
    let (end, prov) = drive(&mut mock, LadderStart::SourceSql);

    assert_eq!(end, LadderEnd::DeterministicSql);
    assert_eq!(mock.calls(), vec!["link", "deterministic_sql"]);
    // A hit expands into the documented sub-rungs so the UI can show where it ran.
    assert_eq!(
        path(&prov),
        vec!["Link", "DeterministicSql", "Validate", "Execute"]
    );
    assert!(!prov.had_miss(), "a clean hit must record no miss");
}

#[test]
fn deterministic_miss_falls_to_model_sql_then_stops() {
    let mut mock = MockRungs {
        deterministic_sql: RungReport::Miss("no rule matched".into()),
        ..Default::default()
    };
    let (end, prov) = drive(&mut mock, LadderStart::SourceSql);

    assert_eq!(end, LadderEnd::ModelSql);
    assert_eq!(
        mock.calls(),
        vec!["link", "deterministic_sql", "model_sql"],
        "the model rung must be tried exactly once, and only after the deterministic one"
    );
    assert!(prov.had_miss());
    assert_eq!(
        path(&prov),
        vec!["Link", "DeterministicSql", "ModelSql", "Validate", "Execute"]
    );
}

#[test]
fn a_link_miss_skips_both_sql_rungs() {
    let mut mock = MockRungs {
        link: RungReport::Miss("no source in scope has linkable schema cards".into()),
        ..Default::default()
    };
    let (end, _prov) = drive(&mut mock, LadderStart::SourceSql);

    // With no source and no cards, SQL is impossible — not failed. Spending a
    // model call on it would be spending it on nothing.
    assert_eq!(end, LadderEnd::Aggregation);
    assert_eq!(mock.calls(), vec!["link", "aggregation"]);
}

#[test]
fn a_validation_rejection_is_recorded_as_a_validate_miss() {
    let mut mock = MockRungs {
        deterministic_sql: RungReport::Hit,
        ..Default::default()
    };
    mock.deterministic_sql = RungReport::ValidateMiss("validation error: unknown column".into());
    let (end, prov) = drive(&mut mock, LadderStart::SourceSql);

    assert_eq!(end, LadderEnd::ModelSql);
    // The rung produced a statement (hit) that the guard rejected — that is a
    // different fact from "the compiler produced nothing", and the path says so.
    assert_eq!(
        path(&prov),
        vec![
            "Link",
            "DeterministicSql",
            "Validate",
            "ModelSql",
            "Validate",
            "Execute"
        ]
    );
    let validate_miss = prov
        .path
        .iter()
        .find(|rung| rung.name() == "Validate" && rung.is_miss())
        .expect("a Validate miss must be in the path");
    assert_eq!(
        validate_miss.result().reason(),
        Some("validation error: unknown column")
    );
}

#[test]
fn both_sql_rungs_missing_falls_through_to_aggregation() {
    let mut mock = MockRungs {
        deterministic_sql: RungReport::Miss("no rule matched".into()),
        model_sql: RungReport::ValidateMiss("validation error: table not allowed".into()),
        ..Default::default()
    };
    let (end, _prov) = drive(&mut mock, LadderStart::SourceSql);

    assert_eq!(end, LadderEnd::Aggregation);
    assert_eq!(
        mock.calls(),
        vec!["link", "deterministic_sql", "model_sql", "aggregation"]
    );
}

#[test]
fn aggregation_miss_reaches_list_only_for_list_shaped_intents() {
    // Aggregation intent: no list rung, straight to retrieval.
    let mut agg = MockRungs {
        intent: QueryIntent::Aggregation,
        aggregation: RungReport::ExecuteMiss("connector error: database error".into()),
        ..Default::default()
    };
    let (end, prov) = drive(&mut agg, LadderStart::DocDb);
    assert_eq!(end, LadderEnd::Retrieval);
    assert_eq!(agg.calls(), vec!["aggregation", "retrieval"]);
    // "Not applicable" must be visible and must not read like a failure.
    let list_rung = prov
        .path
        .iter()
        .find(|rung| rung.name() == "List")
        .expect("the skipped List rung must still be recorded");
    assert_eq!(
        list_rung.result(),
        &RungResult::Skipped("intent is not a list")
    );

    // Enumeration intent: the list rung applies.
    let mut enumeration = MockRungs {
        intent: QueryIntent::Enumeration,
        aggregation_in_scope: false,
        ..Default::default()
    };
    let (end, _prov) = drive(&mut enumeration, LadderStart::DocDb);
    assert_eq!(end, LadderEnd::List);
    assert_eq!(enumeration.calls(), vec!["list"]);
}

#[test]
fn retrieval_is_the_last_rung_and_a_miss_exhausts_the_ladder() {
    let mut mock = MockRungs {
        intent: QueryIntent::Narrative,
        aggregation_in_scope: false,
        retrieval: vec![RungReport::Miss("connector error: service unavailable".into())],
        ..Default::default()
    };
    let (end, prov) = drive(&mut mock, LadderStart::Retrieval);

    assert_eq!(
        end,
        LadderEnd::Exhausted,
        "with every rung missed the caller must refuse, not narrate from nothing"
    );
    assert_eq!(mock.calls(), vec!["retrieval"]);
    assert!(prov.had_miss());
}

// ---------------------------------------------------------------------------
// Plan 04a §5 — empty is not the same as failed
// ---------------------------------------------------------------------------

#[test]
fn an_empty_list_is_a_hit_and_does_not_fall_through() {
    // The mock reports `Hit` for a list that ran and matched nothing; the point of
    // the test is that the ladder must not treat that as a reason to keep going.
    let mut mock = MockRungs {
        intent: QueryIntent::Enumeration,
        aggregation_in_scope: false,
        list: RungReport::Hit,
        ..Default::default()
    };
    let (end, prov) = drive(&mut mock, LadderStart::DocDb);

    assert_eq!(end, LadderEnd::List);
    assert!(
        !mock.calls().contains(&"retrieval".to_string()),
        "an exact empty answer must never be replaced by a fuzzy narrative"
    );
    assert!(!prov.had_miss());
}

// ---------------------------------------------------------------------------
// Plan 04a §6 — a budget skip is visible
// ---------------------------------------------------------------------------

#[test]
fn budget_exhaustion_before_the_first_rung_skips_to_retrieval() {
    let mut mock = MockRungs {
        budget_exhausted_after: Some(0),
        ..Default::default()
    };
    let (end, prov) = drive(&mut mock, LadderStart::SourceSql);

    assert_eq!(end, LadderEnd::Retrieval);
    assert_eq!(
        mock.calls(),
        vec!["retrieval"],
        "no rung may run once the total budget is spent"
    );
    assert!(
        prov.had_budget_skip(),
        "the skip must be visible in the response, not a silent downgrade: {}",
        prov.to_json()
    );
    assert_eq!(
        prov.path.first().map(|rung| rung.name()),
        Some("Link"),
        "the skip is attributed to the rung that was skipped"
    );
}

#[test]
fn budget_exhaustion_mid_ladder_skips_the_remaining_sql_rungs() {
    let mut mock = MockRungs {
        // Link runs, then the budget is gone.
        budget_exhausted_after: Some(1),
        deterministic_sql: RungReport::Miss("no rule matched".into()),
        ..Default::default()
    };
    let (end, prov) = drive(&mut mock, LadderStart::SourceSql);

    assert_eq!(end, LadderEnd::Retrieval);
    assert_eq!(mock.calls(), vec!["link", "retrieval"]);
    assert!(prov.had_budget_skip());
    let skipped = prov
        .path
        .iter()
        .find(|rung| rung.result() == &RungResult::Skipped("budget"))
        .expect("a budget skip must be in the path");
    assert_eq!(skipped.name(), "DeterministicSql");
    // Distinguishable from a miss: a skip is not a failure of the rung.
    assert!(!skipped.is_miss());
}

// ---------------------------------------------------------------------------
// Filter relaxation (plan 04 §4)
// ---------------------------------------------------------------------------

#[test]
fn an_inferred_filter_that_empties_retrieval_is_relaxed_once() {
    let mut mock = MockRungs {
        intent: QueryIntent::Narrative,
        aggregation_in_scope: false,
        may_relax: true,
        retrieval_emptied_by_filter: true,
        retrieval: vec![RungReport::Hit, RungReport::Hit],
        ..Default::default()
    };
    let (end, prov) = drive(&mut mock, LadderStart::Retrieval);

    assert_eq!(end, LadderEnd::Retrieval);
    assert_eq!(
        mock.calls(),
        vec!["retrieval", "retrieval(relaxed)"],
        "exactly one relaxation retry — never a loop"
    );
    // The relaxation is on the record, so an answer built from a wider set than
    // the user's question implied is never presented as if it were filtered.
    assert!(
        prov.path
            .iter()
            .any(|rung| rung.result().reason() == Some("filter relaxed")),
        "{}",
        prov.to_json()
    );
}

#[test]
fn a_stated_filter_is_never_relaxed() {
    let mut mock = MockRungs {
        intent: QueryIntent::Narrative,
        aggregation_in_scope: false,
        may_relax: false,
        retrieval_emptied_by_filter: true,
        ..Default::default()
    };
    let (end, _prov) = drive(&mut mock, LadderStart::Retrieval);

    assert_eq!(end, LadderEnd::Retrieval);
    assert_eq!(
        mock.calls(),
        vec!["retrieval"],
        "a scope the user stated is an answer boundary, not a hint"
    );
}

// ---------------------------------------------------------------------------
// The non-SQL starts
// ---------------------------------------------------------------------------

#[test]
fn conversational_and_clarify_run_no_rungs() {
    let mut conversational = MockRungs::default();
    let (end, prov) = drive(&mut conversational, LadderStart::Conversational);
    assert_eq!(end, LadderEnd::Conversational);
    assert!(conversational.calls().is_empty());
    assert!(prov.path.is_empty());

    let mut clarify = MockRungs::default();
    let (end, prov) = drive(&mut clarify, LadderStart::Clarify);
    assert_eq!(end, LadderEnd::Clarify);
    assert!(clarify.calls().is_empty());
    assert_eq!(path(&prov), vec!["Clarify"]);
}

#[test]
fn a_hybrid_cohort_hit_still_runs_retrieval() {
    let mut mock = MockRungs {
        intent: QueryIntent::Enumeration,
        ..Default::default()
    };
    let (end, prov) = drive(&mut mock, LadderStart::Hybrid);

    assert_eq!(end, LadderEnd::Retrieval);
    assert_eq!(
        mock.calls(),
        vec!["cohort", "retrieval"],
        "Hybrid is cohort-then-synthesis; retrieval is not optional"
    );
    assert!(!prov.had_miss());
}

#[test]
fn a_hybrid_cohort_miss_degrades_to_plain_retrieval() {
    let mut mock = MockRungs {
        intent: QueryIntent::Enumeration,
        cohort: RungReport::ExecuteMiss("cohort matched no rows".into()),
        ..Default::default()
    };
    let (end, prov) = drive(&mut mock, LadderStart::Hybrid);

    assert_eq!(end, LadderEnd::Retrieval);
    assert_eq!(mock.calls(), vec!["cohort", "retrieval"]);
    // The cohort miss must be on the record: the answer is no longer about the
    // cohort the user asked for, and the UI has to be able to say so.
    assert!(prov.had_miss(), "{}", prov.to_json());
}

#[test]
fn the_ladder_start_is_derived_from_the_route_class() {
    use crate::nl2sql::ir::spec::MissingSlot;
    use crate::router::{RouteClass, StructuredBackend};

    assert_eq!(
        LadderStart::from_class(&RouteClass::Conversational),
        LadderStart::Conversational
    );
    assert_eq!(
        LadderStart::from_class(&RouteClass::ConversationMeta),
        LadderStart::Conversational
    );
    assert_eq!(
        LadderStart::from_class(&RouteClass::Capability),
        LadderStart::Conversational
    );
    assert_eq!(
        LadderStart::from_class(&RouteClass::Semantic),
        LadderStart::Retrieval
    );
    assert_eq!(
        LadderStart::from_class(&RouteClass::Structured {
            intent: QueryIntent::Aggregation,
            backend: StructuredBackend::SourceSql,
        }),
        LadderStart::SourceSql
    );
    assert_eq!(
        LadderStart::from_class(&RouteClass::Structured {
            intent: QueryIntent::Aggregation,
            backend: StructuredBackend::DocDb,
        }),
        LadderStart::DocDb
    );
    assert_eq!(
        LadderStart::from_class(&RouteClass::Hybrid {
            cohort_intent: QueryIntent::Enumeration,
        }),
        LadderStart::Hybrid
    );
    assert_eq!(
        LadderStart::from_class(&RouteClass::Clarify {
            question: "which ward?".to_string(),
            slot: MissingSlot::Subject,
        }),
        LadderStart::Clarify
    );
}
