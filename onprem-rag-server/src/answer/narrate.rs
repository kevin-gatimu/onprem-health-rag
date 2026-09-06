//! Narration planning (plan 04 §2.2) — turning an [`ExecOutcome`] into the exact
//! prompt pair that will produce the user-facing answer.
//!
//! This is the *only* place that decides how a result becomes prose. Before it,
//! each route built its own narration prompt inline, which is how the same
//! aggregation could be narrated one way from `/chat` and another from
//! `/agents/maternity`, and how the Handover mode's SBAR framing drifted away from
//! the shape the clinicians agreed to.
//!
//! Two things it deliberately does **not** do:
//!
//! - It never calls a model. It returns a [`NarrationPlan`]; the route streams it.
//!   That keeps every branch here unit-testable and keeps admission control at the
//!   call site, where the permit already lives.
//! - It never widens the result. The prompt says what the rows are and asks for
//!   prose about *those* rows. An empty exact answer stays an empty exact answer —
//!   see [`empty_answer`].
//!
//! # Divergence from the plan's signature
//!
//! Plan 04 §2.2 sketches `narrate(outcome, mode, focus) -> NarrationPlan`. The real
//! signature also takes `state` and `question`: the `spec` field is a `ModelSpec`,
//! which only `AppState::spec_for` can build (it reads the configured alias and the
//! device placement), and every narration prompt needs the question it is answering.
//! Neither is reachable from the three sketched arguments.

// Plan-04 §2.2 subsystem: narration planning (NarrationPlan, plan_narration),
// complete and unit-tested but not yet reachable from the live request path.
// Wiring is gated on the golden suite reaching 55/60 (currently 28/62); see
// TODO(plan03-live) in `nl2sql::prepare`. Until then every public item here is
// dead from the binary's point of view, and the resulting warning wall drowns out
// real signal — so the gate is recorded here instead of in build output.
#![allow(dead_code)]

use crate::agents::kind::AgentMode;
use crate::foundry::router::{ModelRole, ModelSpec};
use crate::router::focus::ConversationFocus;
use crate::state::AppState;

use super::executor::ExecOutcome;
use super::{
    NARRATION_SYSTEM_PROMPT, build_list_narration_user, build_narration_user,
};

/// Row/column limits under which a SQL result needs no model at all.
///
/// A one-cell answer ("42") read back through an 8B model is slower, costs a GPU
/// slot, and can only make the number less exact. `summarize_result` already
/// renders small results as a sentence, so use it directly.
const DIRECT_MAX_ROWS: usize = 3;
const DIRECT_MAX_COLUMNS: usize = 3;

/// The prompt pair (plus model spec) that will produce the answer — or a finished
/// answer that needs no model.
pub struct NarrationPlan {
    pub system: String,
    pub user: String,
    pub spec: ModelSpec,
    /// `Some` when the answer is already exact and complete. The route streams it
    /// verbatim and makes no model call.
    pub direct: Option<String>,
}

impl NarrationPlan {
    /// A plan that needs no model call.
    fn direct(state: &AppState, answer: String) -> Self {
        NarrationPlan {
            system: NARRATION_SYSTEM_PROMPT.to_string(),
            user: String::new(),
            spec: narration_spec(state, false),
            direct: Some(answer),
        }
    }
}

/// The Narrate-role spec, with tool calling off.
///
/// Narration must never emit a tool call: the numbers are already computed, and a
/// model that decides to "check" them would be planning a second query the
/// validator never saw.
fn narration_spec(state: &AppState, thinking: bool) -> ModelSpec {
    let mut spec = state.spec_for(ModelRole::Narrate);
    spec.tools = false;
    spec.thinking = thinking;
    spec
}

/// SBAR is the handover format clinicians already use at shift change, so the
/// answer drops into an existing workflow instead of asking them to read a new one.
/// The section names are fixed; inventing a fact to fill a section is worse than an
/// explicit "nothing recorded".
const HANDOVER_SYSTEM_PROMPT: &str = "\
You are writing a shift-handover summary for clinical staff from health-record data. \
Use the SBAR structure with exactly these four headings, in this order: \
`Situation`, `Background`, `Assessment`, `Recommendation`. \
Situation: what the data shows right now, with the numbers as given. \
Background: the relevant history visible in the data. \
Assessment: what stands out — outliers, trends, gaps — stated as observations, not diagnoses. \
Recommendation: what the incoming shift should check or follow up on. \
Rules: use only the supplied data; never infer a diagnosis; never invent a value, a date, \
or a patient. If a section has nothing to report from the data, write \
`Nothing recorded in the supplied data.` under that heading and move on. \
Keep each section to at most three short sentences. Do not add headings of your own.";

/// Trend narration asks for direction and magnitude over time, which is a
/// different reading task from summarising a flat table.
const TRENDS_SYSTEM_PROMPT: &str = "\
You are describing a trend in health-record data. \
State the direction of change (rising, falling, flat, or irregular), the size of the change \
between the first and last bucket, and any bucket that breaks the pattern. \
Use only the supplied numbers and bucket labels; never extrapolate beyond the last bucket \
and never predict. If there are fewer than three buckets, say the series is too short to \
describe a trend, and report the values instead.";

/// Build the narration plan for an executed outcome.
pub fn narrate(
    state: &AppState,
    question: &str,
    outcome: &ExecOutcome,
    mode: AgentMode,
    focus: &ConversationFocus,
) -> NarrationPlan {
    // An outcome that ran correctly and found nothing gets an exact sentence, not a
    // model call. Plan 04a §5: "none" must survive to the user unchanged.
    if let Some(answer) = empty_answer(outcome, question) {
        return NarrationPlan::direct(state, answer);
    }

    match outcome {
        ExecOutcome::SourceSql {
            columns,
            rows,
            explanation,
            ..
        } => {
            // Small, exact results need no model (see DIRECT_MAX_*).
            if rows.len() <= DIRECT_MAX_ROWS && columns.len() <= DIRECT_MAX_COLUMNS {
                return NarrationPlan::direct(
                    state,
                    crate::nl2sql::text::summarize_result(columns, rows),
                );
            }
            let system = mode_system(mode);
            let user = sql_narration_user(question, columns, rows, explanation, focus);
            NarrationPlan {
                system,
                user,
                spec: narration_spec(state, mode == AgentMode::Trends),
                direct: None,
            }
        }

        ExecOutcome::DocDbAgg { rows, spec, .. } => {
            let system = mode_system(mode);
            let mut user = build_narration_user(question, rows);
            if let Some(line) = focus_note(focus) {
                user.push_str(&line);
            }
            NarrationPlan {
                system,
                user,
                // A bucketed result is a trend regardless of the requested mode:
                // reasoning about direction is what makes the answer useful.
                spec: narration_spec(
                    state,
                    mode == AgentMode::Trends || spec.time_bucket.is_some(),
                ),
                direct: None,
            }
        }

        ExecOutcome::DocDbList {
            spec, rows, total, ..
        } => {
            let system = mode_system(mode);
            let mut user = build_list_narration_user(
                question,
                rows,
                *total,
                spec.offset,
                spec.limit
                    .unwrap_or(crate::aggregation::list::DEFAULT_LIST_LIMIT),
            );
            if let Some(line) = focus_note(focus) {
                user.push_str(&line);
            }
            NarrationPlan {
                system,
                user,
                spec: narration_spec(state, false),
                direct: None,
            }
        }

        ExecOutcome::Semantic { passages, .. } => {
            // The grounded path builds its own context-budgeted prompt from the
            // passages; narration here only supplies the framing, so the route can
            // reuse `rag::build_prompt` unchanged.
            NarrationPlan {
                system: mode_system(mode),
                user: passage_narration_user(question, passages.len(), focus),
                spec: narration_spec(state, mode == AgentMode::Trends),
                direct: None,
            }
        }

        ExecOutcome::Hybrid {
            cohort,
            passages,
            truncated,
            ..
        } => {
            let mut user = passage_narration_user(question, passages.len(), focus);
            if let Some((kept, total)) = truncated {
                // The user asked about a cohort; the answer covers part of it. Saying
                // so is not a caveat, it is the difference between a true and a false
                // statement about the whole cohort.
                user.push_str(&format!(
                    "\n\nCohort note: this answer covers the first {kept} of {total} matching \
                     records. Say so in your answer, and do not describe it as covering all of them."
                ));
            }
            if let ExecOutcome::DocDbList { total, .. } = cohort.as_ref() {
                user.push_str(&format!("\n\nCohort size: {total} records matched the filter."));
            }
            NarrationPlan {
                system: mode_system(mode),
                user,
                spec: narration_spec(state, mode == AgentMode::Trends),
                direct: None,
            }
        }

        ExecOutcome::Clarify { question, .. } => {
            // The clarifying question is already written and already safe. Handing it
            // to a model could only rephrase it into something less precise.
            NarrationPlan::direct(state, question.clone())
        }

        ExecOutcome::Conversational => NarrationPlan {
            system: crate::router::CONVERSATIONAL_SYSTEM_PROMPT.to_string(),
            user: question.to_string(),
            spec: narration_spec(state, false),
            direct: None,
        },
    }
}

/// The system prompt for the requested mode.
fn mode_system(mode: AgentMode) -> String {
    match mode {
        AgentMode::Ask => NARRATION_SYSTEM_PROMPT.to_string(),
        AgentMode::Trends => TRENDS_SYSTEM_PROMPT.to_string(),
        AgentMode::Handover => HANDOVER_SYSTEM_PROMPT.to_string(),
    }
}

/// The exact sentence for an outcome that ran correctly and matched nothing.
///
/// Returns `None` for anything that has content, and for the non-executing
/// outcomes (`Clarify`, `Conversational`) whose emptiness means something else.
pub fn empty_answer(outcome: &ExecOutcome, _question: &str) -> Option<String> {
    match outcome {
        ExecOutcome::SourceSql { rows, .. } if rows.is_empty() => {
            Some("No records in the queried source matched that.".to_string())
        }
        ExecOutcome::DocDbAgg { rows, .. } if rows.is_empty() => {
            Some("No records matched, so there is nothing to count.".to_string())
        }
        ExecOutcome::DocDbList { rows, total, .. } if rows.is_empty() && *total == 0 => {
            Some("No records matched that filter.".to_string())
        }
        // Semantic/Hybrid emptiness is a *retrieval* miss, not an exact "none": the
        // corpus may hold the answer in a chunk the search did not reach. The route's
        // refusal gate owns that case, and overriding it here would turn "I could not
        // find it" into the much stronger "it is not there".
        _ => None,
    }
}

/// A short reminder of the conversation's focus, so a follow-up answer stays on the
/// same patient / period instead of silently broadening.
fn focus_note(focus: &ConversationFocus) -> Option<String> {
    let mut parts: Vec<String> = Vec::new();
    if let Some(patient) = &focus.patient_key {
        parts.push(format!("patient {patient}"));
    }
    if let Some(line) = focus.line {
        parts.push(format!("the {} service line", line.slug()));
    }
    if parts.is_empty() {
        None
    } else {
        Some(format!(
            "\n\nConversation focus: {}. Keep the answer scoped to that.",
            parts.join(" and ")
        ))
    }
}

/// User prompt for a SQL result too wide or too long for the direct shortcut.
fn sql_narration_user(
    question: &str,
    columns: &[String],
    rows: &[Vec<serde_json::Value>],
    explanation: &str,
    focus: &ConversationFocus,
) -> String {
    let mut out = String::with_capacity(512);
    out.push_str("Question: ");
    out.push_str(question);
    out.push_str("\n\nThe query returned these rows. Every number is exact — repeat them as given, \
                  do not round, and do not compute new totals.\n\nColumns: ");
    out.push_str(&columns.join(" | "));
    out.push('\n');
    for row in rows {
        let cells: Vec<String> = row
            .iter()
            .map(|cell| match cell {
                serde_json::Value::String(s) => s.clone(),
                serde_json::Value::Null => String::new(),
                other => other.to_string(),
            })
            .collect();
        out.push_str(&cells.join(" | "));
        out.push('\n');
    }
    out.push_str("\nHow the query was built: ");
    out.push_str(explanation);
    out.push_str("\n\nAnswer the question from these rows only.");
    if let Some(note) = focus_note(focus) {
        out.push_str(&note);
    }
    out
}

/// User prompt framing for the grounded/semantic path. The passages themselves are
/// appended by `rag::build_prompt` at the call site, which owns the context budget.
fn passage_narration_user(question: &str, passage_count: usize, focus: &ConversationFocus) -> String {
    let mut out = format!(
        "Question: {question}\n\nAnswer only from the {passage_count} record excerpts supplied \
         below. If they do not contain the answer, say so plainly rather than filling the gap."
    );
    if let Some(note) = focus_note(focus) {
        out.push_str(&note);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::aggregation::spec::{Metric, MetricOp, RunAggregation, RunList};
    use serde_json::json;

    fn sql_outcome(columns: &[&str], rows: Vec<Vec<serde_json::Value>>) -> ExecOutcome {
        ExecOutcome::SourceSql {
            source_id: "src-1".to_string(),
            sql: "SELECT 1".to_string(),
            spec: None,
            columns: columns.iter().map(|c| c.to_string()).collect(),
            rows,
            explanation: "compiled deterministically".to_string(),
        }
    }

    fn agg_spec() -> RunAggregation {
        RunAggregation {
            collection: "encounters".to_string(),
            filter: json!({}),
            group_by: Vec::new(),
            metric: Metric {
                op: MetricOp::Count,
                field: None,
            },
            time_bucket: None,
            sort: None,
            top_n: None,
        }
    }

    fn list_spec() -> RunList {
        RunList {
            collection: "encounters".to_string(),
            filter: json!({}),
            columns: Vec::new(),
            sort: None,
            limit: None,
            offset: 0,
        }
    }

    /// Plan 04a §5: an exact "none" must stay exact — never handed to a model that
    /// could turn it into a hedge.
    #[test]
    fn an_empty_exact_result_gets_an_exact_sentence() {
        let outcome = sql_outcome(&["total"], Vec::new());
        let answer = empty_answer(&outcome, "how many admissions last month?")
            .expect("an empty SQL result must have an exact answer");
        assert!(answer.contains("No records"), "{answer}");

        let empty_agg = ExecOutcome::DocDbAgg {
            spec: agg_spec(),
            rows: Vec::new(),
            pipeline: Vec::new(),
        };
        assert!(empty_agg.is_empty_result());
        assert!(empty_answer(&empty_agg, "count them").is_some());

        let empty_list = ExecOutcome::DocDbList {
            spec: list_spec(),
            rows: Vec::new(),
            total: 0,
            citations_json: "[]".to_string(),
        };
        assert!(empty_answer(&empty_list, "list them").is_some());
    }

    /// An empty *retrieval* is a search miss, not proof of absence.
    #[test]
    fn an_empty_retrieval_does_not_claim_absence() {
        let outcome = ExecOutcome::Semantic {
            passages: Vec::new(),
            filter: Default::default(),
        };
        assert!(
            empty_answer(&outcome, "summarise her notes").is_none(),
            "retrieval emptiness must fall to the refusal gate, not assert absence"
        );
    }

    #[test]
    fn handover_mode_uses_the_sbar_template() {
        let system = mode_system(AgentMode::Handover);
        for heading in ["Situation", "Background", "Assessment", "Recommendation"] {
            assert!(system.contains(heading), "SBAR heading {heading} missing");
        }
        assert!(
            system.contains("Nothing recorded in the supplied data."),
            "SBAR must have an explicit empty-section phrase rather than inviting invention"
        );
        assert_ne!(mode_system(AgentMode::Ask), system);
        assert_ne!(mode_system(AgentMode::Trends), system);
    }

    #[test]
    fn a_wide_sql_result_is_narrated_rather_than_summarised_directly() {
        let wide = sql_outcome(
            &["a", "b", "c", "d"],
            vec![vec![json!(1), json!(2), json!(3), json!(4)]],
        );
        // Four columns is past DIRECT_MAX_COLUMNS, so it needs prose.
        let user = sql_narration_user(
            "what is in there?",
            &["a", "b", "c", "d"].map(str::to_string),
            &[vec![json!(1), json!(2), json!(3), json!(4)]],
            "compiled deterministically",
            &ConversationFocus::default(),
        );
        assert!(user.contains("a | b | c | d"));
        assert!(
            user.contains("do not round"),
            "the prompt must forbid restating numbers loosely"
        );
        assert!(!wide.is_empty_result());
    }

    #[test]
    fn the_focus_note_scopes_a_follow_up() {
        let focus = ConversationFocus {
            patient_key: Some("PT-0042".to_string()),
            ..Default::default()
        };
        let note = focus_note(&focus).expect("a focus with a patient must produce a note");
        assert!(note.contains("PT-0042"));
        assert!(focus_note(&ConversationFocus::default()).is_none());
    }
}
