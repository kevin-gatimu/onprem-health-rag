//! Shared plan-to-narrate helpers used by both `/chat` and `/agents/<kind>`.
//!
//! Centralising the intent-classify -> plan -> validate -> execute -> narrate
//! pipeline here keeps the two routes in sync and avoids duplication.
//!
//! The route files own their SSE contracts; this module owns the model and DB
//! work. Keeping it at the crate root avoids a `rag` -> `agents` import cycle.

use foundry_local_sdk::ChatCompletionStream;
use serde::de::DeserializeOwned;

use crate::aggregation::catalog::Catalog;
use crate::aggregation::execute;
use crate::aggregation::intent::QueryIntent;
use crate::aggregation::list;
use crate::aggregation::spec::RunAggregation;
use crate::aggregation::validate;
use crate::auth::guard::AuthUser;
use crate::documentdb::DocumentDb;
use crate::error::AppResult;
use crate::foundry::router::AgentKind;
use crate::foundry::FoundryManager;
use crate::state::AppState;

// ---------------------------------------------------------------------------
// Result envelope
// ---------------------------------------------------------------------------

/// Pre-computed structured answer ready to stream from an SSE route.
pub enum Structured {
    /// Aggregation (Count, Group, Trend) answer.
    Aggregate {
        /// Serialised `RunAggregation` for the provenance `spec` event.
        spec_json: String,
        /// Serialised `Vec<AggRow>` for the UI chart/table.
        rows_json: String,
        /// Serialised executed pipeline for the provenance `pipeline` event.
        pipeline_json: String,
        /// Already-opened narration stream (consumed inside the EventStream!).
        narration_stream: ChatCompletionStream,
    },
    /// Enumeration (list all, who are the …) answer.
    List {
        /// Serialised `Vec<serde_json::Value>` rows for this page.
        rows_json: String,
        /// Total deduped row count (before pagination).
        total: u64,
        /// Serialised `Vec<Passage>` synthesised from the rows for the
        /// `citations` event.
        citations_json: String,
        /// Already-opened narration stream.
        narration_stream: ChatCompletionStream,
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
         response to bucket results by a calendar unit (month is usually appropriate)."
    } else {
        "The user wants a HEALTH QUERY — aggregate the records. \
         Do NOT include a `time_bucket` unless the question explicitly asks for a trend."
    };
    format!(
        "You are an aggregation query planner for a health-records system.\n\
         Your job: given a user question, call the `run_aggregation` tool with a valid JSON spec.\n\
         Use ONLY the field names listed in the schema below — do not invent new field names.\n\
         Map natural-language terms to ICD-10 prefixes using the code vocabulary.\n\
         {kind_instruction}\n\n\
         {}",
        catalog.planner_context()
    )
}

/// Build the list-records planner system prompt grounded by the catalog.
pub fn build_list_planner_system(catalog: &Catalog) -> String {
    format!(
        "You are a record-listing query planner for a health-records system.\n\
         Your job: given a user question, call the `run_list_records` tool with a valid JSON spec.\n\
         Use ONLY the collection and field names listed in the schema below.\n\
         Choose the collection that best matches the question.\n\
         For pagination: use `offset` 0 for the first page; include `limit` (default 50).\n\n\
         {}",
        catalog.planner_context()
    )
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
    limit: u32,
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

// ---------------------------------------------------------------------------
// Intent-to-kind mapping (shared with agents/routes.rs)
// ---------------------------------------------------------------------------

/// Map a `QueryIntent` to the corresponding `AgentKind`.
///
/// `Enumeration` falls back to `Chat` for the `/agents` path since that route
/// does not yet have a dedicated list handler. `/chat` handles enumeration directly
/// via `run_structured`.
pub fn intent_to_kind(intent: QueryIntent) -> AgentKind {
    match intent {
        QueryIntent::Trend => AgentKind::Trends,
        QueryIntent::Aggregation => AgentKind::HealthQuery,
        QueryIntent::Lookup => AgentKind::PatientLookup,
        QueryIntent::Narrative => AgentKind::Summarize,
        // Enumeration uses the list path in /chat; /agents falls back to semantic.
        QueryIntent::Enumeration | QueryIntent::MultiHop => AgentKind::Chat,
    }
}

// Model-based intent classification now lives in the intent router
// (`router::route`, Tier 2 — `FoundryManager::plan_route`), which uses a forced
// tool call rather than free-text JSON parsing. See `plans/17-intent-router-v2.md`.

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
) -> AppResult<Structured> {
    match intent {
        QueryIntent::Aggregation | QueryIntent::Trend => {
            let kind = if intent == QueryIntent::Trend {
                AgentKind::Trends
            } else {
                AgentKind::HealthQuery
            };
            let spec = state.spec_for(kind);
            let planner_system = build_agg_planner_system(catalog, intent);

            // 1. Plan: model emits a RunAggregation tool call.
            let planned = foundry.plan_aggregation(&spec, &planner_system, question).await?;

            // 2. Validate + sanitize against the current catalog.
            let validated = validate::validate(&planned, catalog)?;

            // 3. Execute pipeline against DocumentDB.
            let (rows, pipeline_docs) = execute::run(db, user, &validated).await?;

            let spec_json =
                serde_json::to_string(&validated).unwrap_or_else(|_| "{}".to_string());
            let rows_json =
                serde_json::to_string(&rows).unwrap_or_else(|_| "[]".to_string());
            let pipeline_json = serde_json::to_string(
                &pipeline_docs
                    .iter()
                    .map(|d| mongodb::bson::Bson::Document(d.clone()).into_relaxed_extjson())
                    .collect::<Vec<_>>(),
            )
            .unwrap_or_else(|_| "[]".to_string());

            // 4. Open grounded narration stream (no tools — model only summarises).
            let narration_user = build_narration_user(question, &rows);
            let mut narration_spec = state.spec_for(kind);
            narration_spec.tools = false; // narration never uses tool calling
            let narration_stream = foundry
                .generate_stream_with(&narration_spec, NARRATION_SYSTEM_PROMPT, &narration_user)
                .await?;

            Ok(Structured::Aggregate {
                spec_json,
                rows_json,
                pipeline_json,
                narration_stream,
            })
        }

        QueryIntent::Enumeration => {
            let spec = state.spec_for(AgentKind::HealthQuery); // planner role
            let planner_system = build_list_planner_system(catalog);

            // 1. Plan: model emits a RunList tool call.
            let planned = foundry.plan_list(&spec, &planner_system, question).await?;

            // 2. Validate against the current catalog.
            let validated = list::validate_list(&planned, catalog)?;

            // 3. Execute paginated list query.
            let (rows, total, _pipeline_docs) =
                list::run(db, user, &validated).await?;

            // 4. Synthesise citations from rows so the UI shows them as sources.
            let citations_json = list::rows_to_citations_json(&rows, &validated.collection);

            // 5. Open narration stream.
            let offset = validated.offset;
            let limit = validated.limit.unwrap_or(list::DEFAULT_LIST_LIMIT);
            let narration_user = build_list_narration_user(question, &rows, total, offset, limit);
            let mut narration_spec = state.spec_for(AgentKind::HealthQuery);
            narration_spec.tools = false;
            let narration_stream = foundry
                .generate_stream_with(&narration_spec, NARRATION_SYSTEM_PROMPT, &narration_user)
                .await?;

            let rows_json =
                serde_json::to_string(&rows).unwrap_or_else(|_| "[]".to_string());

            Ok(Structured::List { rows_json, total, citations_json, narration_stream })
        }

        // Other intents should not reach run_structured; callers guard this.
        other => Err(crate::error::AppError::BadRequest(format!(
            "run_structured called with non-structural intent {other:?}"
        ))),
    }
}
