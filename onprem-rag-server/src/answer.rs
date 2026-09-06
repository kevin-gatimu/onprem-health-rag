//! Shared plan-to-narrate helpers used by both `/chat` and `/agents/<kind>`.
//!
//! Centralising the intent-classify -> plan -> validate -> execute -> narrate
//! pipeline here keeps the two routes in sync and avoids duplication.
//!
//! The route files own their SSE contracts; this module owns the model and DB
//! work. Keeping it at the crate root avoids a `rag` -> `agents` import cycle.

use crate::aggregation::catalog::Catalog;
use crate::aggregation::execute;
use crate::aggregation::intent::QueryIntent;
use crate::aggregation::list;
use crate::aggregation::spec::{Metric, MetricOp, RunAggregation};
use crate::aggregation::validate;
use crate::auth::guard::AuthUser;
use crate::documentdb::DocumentDb;
use crate::error::AppResult;
use crate::foundry::router::ModelRole;
use crate::foundry::{FoundryManager, GuardedChatStream};
use crate::state::AppState;
use crate::telemetry::{RequestTrace, Stage};

// ---------------------------------------------------------------------------
// Result envelope
// ---------------------------------------------------------------------------

/// Pre-computed structured answer ready to stream from an SSE route.
#[allow(dead_code)]
pub enum Structured {
    /// Exact answer that does not require an LLM narration pass.
    Direct { answer: String },
    /// Aggregation (Count, Group, Trend) answer.
    Aggregate {
        /// Already-opened narration stream (consumed inside the EventStream!).
        narration_stream: GuardedChatStream,
    },
    /// Enumeration (list all, who are the …) answer.
    List {
        /// Serialised `Vec<Passage>` synthesised from the rows for the
        /// `citations` event.
        citations_json: String,
        /// Already-opened narration stream.
        narration_stream: GuardedChatStream,
    },
}

// ---------------------------------------------------------------------------
// System prompts
// ---------------------------------------------------------------------------

/// Strict narration prompt for the grounded narration step of structured queries.
/// The model must narrate ONLY the numbers from the database rows.
pub const NARRATION_SYSTEM_PROMPT: &str = "\
You are a clinical data analyst presenting exact query results from a health-records database. \
RULES: \
(1) Narrate ONLY the numbers provided in the data rows. \
(2) Do NOT invent, extrapolate, or estimate any values not in the rows. \
(3) Do NOT make clinical recommendations or diagnoses. \
(4) State findings clearly and concisely in 2-4 sentences. \
(5) If the data is empty, say the query returned no results.";

// ---------------------------------------------------------------------------
// Planner system-prompt builders
// ---------------------------------------------------------------------------

/// Build the aggregation planner system prompt grounded by the catalog.
///
/// `intent` controls whether the model is instructed to include a `time_bucket`
/// (Trend) or not (Aggregation).
pub fn build_agg_planner_system(catalog: &Catalog, intent: QueryIntent) -> String {
    let kind_instruction = if intent == QueryIntent::Trend {
        "The user wants a TREND query — always include a `time_bucket` field in your \
         response to bucket results by a calendar unit (month is usually appropriate). \
         `time_bucket` must be an object whose `field` is a real date column and whose \
         `unit` is the granularity, for example: \
         `\"time_bucket\": {\"field\":\"encounter_date\",\"unit\":\"month\"}`. \
         NEVER put `time_bucket` in `group_by` or use `time_bucket` as a field name."
    } else {
        "The user wants a HEALTH QUERY — aggregate the records. \
         Do NOT include a `time_bucket` unless the question explicitly asks for a trend."
    };
    format!(
        "You are an aggregation query planner for a health-records system.\n\
         Your job: given a user question, call the `run_aggregation` tool with a valid JSON spec.\n\
         Use ONLY the field names listed in the schema below — do not invent new field names.\n\
         Map natural-language terms to ICD-10 prefixes using the code vocabulary.\n\
         Preserve every qualifier in the question as a `filter` (for example abnormal, active, \
         failed, positive, male/female, or a status value). Prefer an allowed boolean or flag \
         field such as `is_abnormal` when one exists. NEVER count all rows for a qualified question.\n\
         {kind_instruction}\n\n\
         {}",
        catalog.planner_context()
    )
}

/// Build the list-records planner system prompt grounded by the catalog.
pub fn build_list_planner_system(catalog: &Catalog) -> String {
    format!(
        "You are a record-listing query planner for a health-records system.\n\
         Your job: given a user question, produce a valid `run_list_records` JSON spec.\n\
         Use ONLY the collection and field names listed in the schema below.\n\
         Choose the collection that best matches the question.\n\
         For pagination: use `offset` 0 for the first page; include `limit` (default 50).\n\n\
         {}",
        catalog.planner_context()
    )
}

#[cfg(test)]
mod planner_prompt_tests {
    use super::build_agg_planner_system;
    use crate::aggregation::catalog::Catalog;
    use crate::aggregation::intent::QueryIntent;

    #[test]
    fn aggregation_trend_prompt_explains_time_bucket_shape() {
        let prompt = build_agg_planner_system(&Catalog::empty(), QueryIntent::Trend);

        assert!(
            prompt.contains("\"time_bucket\": {\"field\":\"encounter_date\",\"unit\":\"month\"}")
        );
        assert!(prompt.contains("NEVER put `time_bucket` in `group_by`"));
        assert!(prompt.contains("use `time_bucket` as a field name"));
    }

    #[test]
    fn aggregation_prompt_requires_filters_for_qualified_questions() {
        let prompt = build_agg_planner_system(&Catalog::empty(), QueryIntent::Aggregation);

        assert!(prompt.contains("Preserve every qualifier"));
        assert!(prompt.contains("`is_abnormal`"));
        assert!(prompt.contains("NEVER count all rows for a qualified question"));
    }
}

// ---------------------------------------------------------------------------
// Narration-user message builders
// ---------------------------------------------------------------------------

/// Build the narration user message for an aggregation result.
pub fn build_narration_user(question: &str, rows: &[execute::AggRow]) -> String {
    let mut msg = format!("Question: {question}\n\nQuery results from database:\n");
    if rows.is_empty() {
        msg.push_str("(no rows returned)\n");
    } else {
        msg.push_str("Label | Value\n");
        msg.push_str("------|------\n");
        for row in rows {
            msg.push_str(&format!("{} | {}\n", row.label, row.value));
        }
    }
    msg.push_str("\nNarrate these results.");
    msg
}

/// Build the narration user message for a list result.
pub fn build_list_narration_user(
    question: &str,
    rows: &[serde_json::Value],
    total: u64,
    offset: u32,
    _limit: u32,
) -> String {
    let shown = rows.len() as u64;
    let end = offset as u64 + shown;
    let mut msg = format!(
        "Question: {question}\n\nQuery results (showing {}-{} of {total} total records):\n",
        offset + 1,
        end,
    );
    if rows.is_empty() {
        msg.push_str("(no rows returned)\n");
    } else {
        for row in rows {
            msg.push_str(&format!(
                "- {}\n",
                serde_json::to_string(row).unwrap_or_default()
            ));
        }
    }
    if total > end {
        msg.push_str(&format!(
            "\n[Note: {} more records exist. Ask for the next page or add a filter to narrow the results.]\n",
            total - end,
        ));
    }
    msg.push_str("\nNarrate these results briefly.");
    msg
}

// Model-based intent classification now lives in the intent router
// (`router::route`, Tier 2 — `FoundryManager::plan_route`), which uses a forced
// tool call rather than free-text JSON parsing. See `plans/17-intent-router-v2.md`.

// ---------------------------------------------------------------------------
// Deterministic structured plans
// ---------------------------------------------------------------------------

fn normalize_question(question: &str) -> String {
    question
        .chars()
        .map(|ch| {
            if ch.is_alphanumeric() {
                ch.to_ascii_lowercase()
            } else {
                ' '
            }
        })
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

fn simple_patient_count_plan(question: &str, catalog: &Catalog) -> Option<RunAggregation> {
    let normalized = normalize_question(question);
    let is_unfiltered_total = matches!(
        normalized.as_str(),
        "how many patients"
            | "how many patients do we have"
            | "how many patients are there"
            | "number of patients"
            | "what is the number of patients"
            | "total patients"
            | "total number of patients"
            | "what is the total number of patients"
    );
    if !is_unfiltered_total {
        return None;
    }

    let collection = catalog
        .collections
        .keys()
        .find(|name| matches!(name.to_ascii_lowercase().as_str(), "patient" | "patients"))?
        .clone();

    Some(RunAggregation {
        collection,
        filter: serde_json::json!({}),
        group_by: Vec::new(),
        metric: Metric {
            op: MetricOp::Count,
            field: None,
        },
        time_bucket: None,
        sort: None,
        top_n: Some(1),
    })
}

// ---------------------------------------------------------------------------
// Core plan-to-narrate routine
// ---------------------------------------------------------------------------

/// Run the full structured-query pipeline for one question and return a
/// ready-to-stream `Structured` result.
///
/// On plan or validate failure the error propagates as an HTTP error (before any
/// SSE stream opens). The caller should catch and fall back to semantic retrieval
/// when appropriate (as `/chat` does).
pub async fn run_structured(
    db: &DocumentDb,
    foundry: &FoundryManager,
    user: &AuthUser,
    state: &AppState,
    catalog: &Catalog,
    intent: QueryIntent,
    question: &str,
    trace: &RequestTrace,
) -> AppResult<Structured> {
    match intent {
        QueryIntent::Aggregation | QueryIntent::Trend => {
            // Planning a pipeline is the PlanSpec role regardless of intent; the
            // Trend/Aggregation distinction only changes the planner prompt below.
            let spec = state.spec_for(ModelRole::PlanSpec);
            let planner_system = build_agg_planner_system(catalog, intent);

            // Exact unfiltered patient totals do not need model planning. Besides
            // reducing latency, this keeps a basic count available on ORT backends
            // that cannot compile the aggregation tool grammar.
            let deterministic_plan = (intent == QueryIntent::Aggregation)
                .then(|| simple_patient_count_plan(question, catalog))
                .flatten();
            let direct_count = deterministic_plan.is_some();
            let planned = if let Some(plan) = deterministic_plan {
                plan
            } else {
                trace
                    .time(
                        Stage::Plan,
                        foundry.plan_aggregation(&spec, &planner_system, question),
                    )
                    .await?
            };

            // 2. Validate + sanitize against the current catalog.
            let validated =
                trace.time_sync(Stage::Validate, || validate::validate(&planned, catalog))?;

            // 3. Execute pipeline against DocumentDB.
            let (rows, _pipeline_docs) = trace
                .time(Stage::Execute, execute::run(db, user, &validated))
                .await?;

            if direct_count {
                let count = rows
                    .first()
                    .map(|row| row.value.max(0.0) as u64)
                    .unwrap_or(0);
                return Ok(Structured::Direct {
                    answer: format!("There are {count} patients in the indexed records."),
                });
            }

            // 4. Open grounded narration stream (no tools — model only summarises).
            let narration_user = build_narration_user(question, &rows);
            let mut narration_spec = state.spec_for(ModelRole::Narrate);
            narration_spec.tools = false; // narration never uses tool calling
            // Trend narration benefits from deliberate reasoning about direction and
            // magnitude; a flat aggregation does not.
            narration_spec.thinking = intent == QueryIntent::Trend;
            let narration_stream = trace
                .time(
                    Stage::Narrate,
                    foundry.generate_stream_with(
                        &narration_spec,
                        NARRATION_SYSTEM_PROMPT,
                        &narration_user,
                    ),
                )
                .await?;

            Ok(Structured::Aggregate { narration_stream })
        }

        QueryIntent::Enumeration => {
            let spec = state.spec_for(ModelRole::PlanSpec); // planner role
            let planner_system = build_list_planner_system(catalog);

            // 1. Plan: model emits a RunList tool call.
            let planned = trace
                .time(
                    Stage::Plan,
                    foundry.plan_list(&spec, &planner_system, question),
                )
                .await?;

            // 2. Validate against the current catalog.
            let validated =
                trace.time_sync(Stage::Validate, || list::validate_list(&planned, catalog))?;

            // 3. Execute paginated list query.
            let (rows, total, _pipeline_docs) = trace
                .time(Stage::Execute, list::run(db, user, &validated))
                .await?;

            // 4. Synthesise citations from rows so the UI shows them as sources.
            let citations_json = list::rows_to_citations_json(&rows, &validated.collection);

            // 5. Open narration stream.
            let offset = validated.offset;
            let narration_user = build_list_narration_user(
                question,
                &rows,
                total,
                offset,
                validated.limit.unwrap_or(list::DEFAULT_LIST_LIMIT),
            );
            let mut narration_spec = state.spec_for(ModelRole::Narrate);
            narration_spec.tools = false;
            let narration_stream = trace
                .time(
                    Stage::Narrate,
                    foundry.generate_stream_with(
                        &narration_spec,
                        NARRATION_SYSTEM_PROMPT,
                        &narration_user,
                    ),
                )
                .await?;

            Ok(Structured::List {
                citations_json,
                narration_stream,
            })
        }

        // Other intents should not reach run_structured; callers guard this.
        other => Err(crate::error::AppError::BadRequest(format!(
            "run_structured called with non-structural intent {other:?}"
        ))),
    }
}
