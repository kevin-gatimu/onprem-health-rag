//! `POST /agents/<kind>` — SSE endpoint for task-aware AI agents.
//!
//! Routes by `AgentKind`:
//! - **Semantic** (Chat, PatientLookup, Summarize): retrieval + `generate_stream_with`.
//!   PatientLookup first attempts a conservative live-SQL overview. Emits: `routed`
//!   -> `citations` -> `token`* -> `done`.
//! - **Structured** (HealthQuery, Trends): `plan_aggregation` -> `validate` -> `execute`
//!   -> grounded narration.  Emits: `routed` -> `spec` -> `rows` -> `pipeline` -> `token`* -> `done`.
//!   Semantic questions on these tabs (no structural intent per the shared router)
//!   fall through to the same retrieval pipeline as the semantic kinds.
//!
//! The SSE event set is a strict superset of `/chat` so the bridge relay can be
//! generalised without breaking existing clients.
//!
//! Everything below happens *before* the stream opens, so live pipeline stages
//! cannot ride on it. Pass `run_id` in the body and subscribe to
//! `GET /runs/<run_id>/progress` for those (see `progress.rs`).
//!
//! # Event contract (SSE event names -> JSON payload shapes)
//!
//! | Event      | Payload                                          | When                         |
//! |------------|--------------------------------------------------|------------------------------|
//! | `routed`   | JSON-encoded string (e.g. `"health_query"`)      | First event, both paths      |
//! | `citations`| `[{id,source_id,row_pk,text,fields,score,...}]`  | Semantic path                |
//! | `spec`     | `RunAggregation` JSON object                     | Structured path, after plan  |
//! | `rows`     | `[{"label": String, "value": f64}]`              | Structured path, after exec  |
//! | `pipeline` | `[{...}]` BSON pipeline stages as JSON           | Structured path, provenance  |
//! | `token`    | JSON-encoded string (streaming token)            | Both paths, generation       |
//! | `error`    | Bare error string                                | On mid-stream failure        |
//! | `done`     | Empty string                                     | Terminal, always emitted     |

use rocket::response::stream::{Event, EventStream};
use rocket::serde::json::Json;
use rocket::{State, post};
use serde::Deserialize;

use crate::aggregation::execute;
use crate::aggregation::intent::{QueryIntent, classify_lexical};
use crate::aggregation::validate;
use crate::answer::{NARRATION_SYSTEM_PROMPT, build_agg_planner_system, build_narration_user};
use crate::auth::guard::AuthUser;
use crate::error::{AppError, AppResult};
use crate::foundry::{GuardedChatStream, router::AgentKind};
use crate::memory::WorkingMemory;
use crate::rag::{
    SYSTEM_PROMPT, apply_context_budget, build_prompt, expand_queries_with, prepare_queries_with,
    rewrite_query_with,
};
use crate::retrieval;
use crate::state::AppState;
use crate::telemetry::{RequestTrace, Stage};

// ---------------------------------------------------------------------------
// Request body
// ---------------------------------------------------------------------------

/// Body for `POST /agents/<kind>`.
#[derive(Debug, Deserialize)]
pub struct AgentRequest {
    /// The user's question or query.
    pub question: String,
    /// When present, the server persists the exchange to this conversation and
    /// loads history from the DB (semantic path). Absent -> stateless (eval-safe).
    #[serde(default)]
    pub conversation_id: Option<String>,
    /// Client-minted id for this run; when present every pipeline stage is
    /// published to `GET /runs/<run_id>/progress` for the activity strip.
    #[serde(default)]
    pub run_id: Option<String>,
}

// ---------------------------------------------------------------------------
// Pre-computed data that flows into the single EventStream! generator
// ---------------------------------------------------------------------------

/// All the data prepared before the SSE generator opens. Keeping async work out
/// of the generator means setup errors become HTTP errors (not mid-stream events)
/// and we avoid smuggling non-`Send` temporaries into the async generator.
enum AgentData {
    SemanticDirect {
        /// Deterministic live-source answer rendered as ordinary agent text.
        answer: String,
    },
    StructuredDirect {
        /// Synthetic aggregation-shaped provenance for the existing agent UI.
        spec_json: String,
        /// SQL rows adapted to chart-ready `AggRow` values.
        rows_json: String,
        /// Live-SQL provenance, carried in the existing query disclosure event.
        pipeline_json: String,
        /// Deterministic result narration; no model call is needed.
        answer: String,
    },
    Structured {
        /// Serialised `RunAggregation` spec (provenance).
        spec_json: String,
        /// Serialised `Vec<AggRow>` for the UI chart/table.
        rows_json: String,
        /// Serialised executed pipeline (provenance).
        pipeline_json: String,
        /// Already-opened narration stream (iterate inside EventStream!).
        narration_stream: GuardedChatStream,
    },
    Semantic {
        /// Serialised `Vec<Passage>` for citations.
        citations_json: String,
        /// `None` when the score gate refuses to answer.
        gen_stream: Option<GuardedChatStream>,
    },
}

// ---------------------------------------------------------------------------
// Route
// ---------------------------------------------------------------------------

/// `POST /agents/<kind>` — task-aware SSE agent endpoint.
///
/// `<kind>` is one of: `auto` (default selector), `health_query`, `trends`,
/// `patient_lookup`, `summarize`, `chat`. `auto` is intercepted before
/// `parse_kind`; truly unknown kinds return `400`.
#[post("/agents/<kind>", data = "<body>")]
pub async fn agent(
    state: &State<AppState>,
    user: AuthUser,
    kind: &str,
    body: Json<AgentRequest>,
) -> AppResult<EventStream![]> {
    let trace = RequestTrace::new("agent");
    if let Some(run_id) = body.run_id.clone() {
        trace.attach_progress(state.run_progress.clone(), run_id);
    }
    let mut generation_permit = None;

    // Resolve the effective kind. `auto` uses the shared router; explicit kinds
    // skip classification and record only the resolved agent role.
    let resolved_kind: AgentKind = if kind.eq_ignore_ascii_case("auto") {
        let decision = trace
            .time(
                Stage::Route,
                crate::router::route(
                    &body.question,
                    false,
                    &state.config,
                    state.foundry().ok(),
                    &state.spec_for(AgentKind::Classify),
                    &state.router_cache,
                ),
            )
            .await;
        trace.set_route(
            decision.route_label(),
            Some(decision.tier),
            Some(decision.cached),
        );
        crate::router::class_to_agent_kind(&decision.class)
    } else {
        parse_kind(kind)?
    };

    let req = body.into_inner();

    // Conversation persistence (pre-stream so errors surface as HTTP status, not
    // mid-stream events).
    let history: Vec<crate::rag::ChatTurn> = trace
        .time(Stage::History, async {
            if let Some(cid) = req.conversation_id.as_deref() {
                use crate::routes::conversations::{
                    load_history, persist_user_message, verify_owned,
                };
                verify_owned(&state.db, cid, &user.id).await?;
                let history = load_history(&state.db, cid, &user.id, 10).await;
                persist_user_message(&state.db, cid, &user.id, &req.question).await?;
                Ok::<Vec<crate::rag::ChatTurn>, crate::error::AppError>(history)
            } else {
                Ok::<Vec<crate::rag::ChatTurn>, crate::error::AppError>(Vec::new())
            }
        })
        .await?;
    let memory = WorkingMemory::from_turns(history.clone());

    let kind_str: String = serde_json::to_value(resolved_kind)
        .ok()
        .and_then(|v| v.as_str().map(str::to_string))
        .unwrap_or_else(|| "chat".to_string());
    if !kind.eq_ignore_ascii_case("auto") {
        trace.set_route(format!("agent_{kind_str}"), None, None);
    }

    let mut persist_passages: Vec<crate::retrieval::Passage> = Vec::new();
    let mut persist_structured_json: Option<String> = None;

    let data: AgentData = match resolved_kind {
        // -----------------------------------------------------------------
        // Patient lookup: prefer a conservative live-source patient overview.
        // A miss or source failure falls through to the unchanged semantic path.
        // -----------------------------------------------------------------
        AgentKind::PatientLookup if should_try_deterministic_sql(resolved_kind, &req.question) => {
            let deterministic = trace
                .time(
                    Stage::Execute,
                    crate::nl2sql::routes::prepare_auto_query_deterministic(
                        state.inner(),
                        &req.question,
                    ),
                )
                .await
                .unwrap_or_else(|error| {
                    tracing::warn!(%error, "patient lookup deterministic SQL failed; using semantic retrieval");
                    None
                });

            if let Some(prepared) = deterministic {
                AgentData::SemanticDirect {
                    answer: narrate_patient_rows(&prepared.columns, &prepared.rows),
                }
            } else {
                build_semantic_agent_data(
                    state.inner(),
                    resolved_kind,
                    &memory,
                    &req.question,
                    None,
                    &trace,
                    &mut generation_permit,
                    &mut persist_passages,
                )
                .await?
            }
        }

        // -----------------------------------------------------------------
        // Summaries of recent records are ordered source queries, not semantic
        // retrieval. Keep this narrow and silently retain the semantic fallback.
        // -----------------------------------------------------------------
        AgentKind::Summarize if should_try_deterministic_sql(resolved_kind, &req.question) => {
            let deterministic = trace
                .time(
                    Stage::Execute,
                    crate::nl2sql::routes::prepare_auto_query_deterministic(
                        state.inner(),
                        &req.question,
                    ),
                )
                .await
                .unwrap_or_else(|error| {
                    tracing::warn!(%error, "summary deterministic SQL failed; using semantic retrieval");
                    None
                });

            if let Some(prepared) = deterministic {
                AgentData::SemanticDirect {
                    answer: narrate_recent_rows(&prepared.columns, &prepared.rows, &prepared.sql),
                }
            } else {
                build_semantic_agent_data(
                    state.inner(),
                    resolved_kind,
                    &memory,
                    &req.question,
                    None,
                    &trace,
                    &mut generation_permit,
                    &mut persist_passages,
                )
                .await?
            }
        }

        // -----------------------------------------------------------------
        // Structured analytical path: HealthQuery + Trends
        // -----------------------------------------------------------------
        AgentKind::HealthQuery | AgentKind::Trends => {
            // Not `state.foundry()?`: a question the live source can answer
            // deterministically needs no model at all, so an unavailable Foundry
            // must not turn it into a 503 here. The paths that do need a model
            // ask for it themselves.
            let foundry_opt = state.foundry().ok();

            // Rewrite against working memory *first*. A follow-up ("list their
            // names") only becomes answerable once it carries the subject of the
            // previous turn, and every decision below — deterministic SQL,
            // routing, planning — reads the resolved form. Doing this after the
            // SQL attempt, as this used to, meant follow-ups could never hit the
            // live source and fell through to a planner that had no way to know
            // what "their" referred to. No-op (no model call) without history.
            let rewrite_spec = state.spec_for(AgentKind::QueryRewrite);
            let standalone = match foundry_opt {
                Some(foundry) => {
                    trace
                        .time(
                            Stage::RewriteExpand,
                            rewrite_query_with(
                                foundry,
                                &rewrite_spec,
                                &memory.rewrite_turns(),
                                &req.question,
                            ),
                        )
                        .await
                }
                None => req.question.clone(),
            };

            // Prefer the same validated live-SQL templates used by `/chat`. A miss or
            // source failure is deliberately non-fatal: the existing DocumentDB
            // aggregation planner remains the fallback.
            let deterministic = if should_try_deterministic_sql(resolved_kind, &standalone) {
                trace
                    .time(
                        Stage::Execute,
                        crate::nl2sql::routes::prepare_auto_query_deterministic(
                            state.inner(),
                            &standalone,
                        ),
                    )
                    .await
                    .unwrap_or_else(|error| {
                        tracing::warn!(%error, "agent deterministic SQL failed; using aggregation planner");
                        None
                    })
            } else {
                None
            };

            // Second, cheaper chance for pronoun follow-ups the rewrite model left
            // unresolved: ground them on an identifier from a prior turn. Model-free,
            // so a miss costs nothing.
            let deterministic = match deterministic {
                Some(prepared) => Some(prepared),
                None => resolve_followup_sql(state.inner(), &req.question, &memory).await,
            };

            if let Some(prepared) = deterministic {
                trace.stage_detail(
                    Stage::Execute,
                    format!("{} rows from the live database", prepared.rows.len()),
                );
                let (spec_json, rows_json, pipeline_json, answer) =
                    deterministic_structured_result(prepared, resolved_kind);
                persist_structured_json = Some(format!(
                    "{{\"spec\":{spec_json},\"rows\":{rows_json},\"pipeline\":{pipeline_json}}}"
                ));
                AgentData::StructuredDirect {
                    spec_json,
                    rows_json,
                    pipeline_json,
                    answer,
                }
            } else {
                // Unified routing: explicit structured tabs still honour semantic
                // questions through the same tiered router `auto` uses; anything
                // not confidently structural falls through to shared retrieval.
                let decision = trace
                    .time(
                        Stage::Route,
                        crate::router::route(
                            &standalone,
                            !history.is_empty(),
                            &state.config,
                            state.foundry().ok(),
                            &state.spec_for(AgentKind::Classify),
                            &state.router_cache,
                        ),
                    )
                    .await;

                let catalog = state.catalog();
                // With nothing ingested there is no collection the planner could
                // name, so every spec it produces fails validation. Skip straight
                // to retrieval (or, above, the live source) instead of spending a
                // model call to earn a guaranteed error.
                let structured_possible = can_run_docdb_aggregation(&decision.class, &catalog);

                let aggregated = if structured_possible {
                    match run_docdb_aggregation(
                        state.inner(),
                        &user,
                        resolved_kind,
                        &standalone,
                        &catalog,
                        &trace,
                        &mut generation_permit,
                    )
                    .await
                    {
                        Ok((data, structured_json)) => {
                            persist_structured_json = Some(structured_json);
                            Some(data)
                        }
                        // Planning, validation and execution are all best-effort:
                        // a small model emitting a malformed spec, or naming a
                        // collection that was never ingested, used to abort the
                        // whole request with a raw 400 in the chat transcript.
                        // A grounded semantic answer is a far better outcome.
                        Err(error) => {
                            tracing::info!(
                                kind = ?resolved_kind,
                                %error,
                                "agent aggregation failed; falling back to semantic retrieval"
                            );
                            None
                        }
                    }
                } else {
                    None
                };

                match aggregated {
                    Some(data) => data,
                    None => {
                        build_semantic_agent_data(
                            state.inner(),
                            resolved_kind,
                            &memory,
                            &req.question,
                            Some(standalone),
                            &trace,
                            &mut generation_permit,
                            &mut persist_passages,
                        )
                        .await?
                    }
                }
            }
        }

        // -----------------------------------------------------------------
        // Semantic path: Chat, PatientLookup, Summarize (+ other kinds)
        // -----------------------------------------------------------------
        _ => {
            build_semantic_agent_data(
                state.inner(),
                resolved_kind,
                &memory,
                &req.question,
                None,
                &trace,
                &mut generation_permit,
                &mut persist_passages,
            )
            .await?
        }
    };

    let db = state.db.clone();
    // Read before the generator opens (it cannot borrow `&AppState`); picks the
    // honest "no answer" wording — see `rag::no_grounding_message`.
    let store_is_empty = state.catalog().collections.is_empty();
    let uid = user.id.clone();
    let persist_cid = req.conversation_id.clone();
    let foundry_handle = state.foundry_handle();
    let cfg = state.config.clone();
    let compact_spec = state.spec_for(AgentKind::QueryRewrite);

    Ok(EventStream! {
        use futures::StreamExt;

        let _generation_permit = generation_permit;

        yield Event::data(serde_json::to_string(&kind_str).unwrap_or_default())
            .event("routed");

        let mut full_answer = String::new();
        let mut had_error = false;
        let _generation = trace.stage_guard(Stage::Generate);

        match data {
            AgentData::SemanticDirect { answer } => {
                yield Event::data("[]").event("citations");
                trace.record_output(&answer);
                full_answer.push_str(&answer);
                yield token_event(&answer);
            }

            AgentData::StructuredDirect { spec_json, rows_json, pipeline_json, answer } => {
                yield Event::data(spec_json).event("spec");
                yield Event::data(rows_json).event("rows");
                yield Event::data(pipeline_json).event("pipeline");
                trace.record_output(&answer);
                full_answer.push_str(&answer);
                yield token_event(&answer);
            }

            AgentData::Structured { spec_json, rows_json, pipeline_json, mut narration_stream } => {
                yield Event::data(spec_json).event("spec");
                yield Event::data(rows_json).event("rows");
                yield Event::data(pipeline_json).event("pipeline");

                let mut think = crate::foundry::think_filter::ThinkFilter::new();
                while let Some(chunk) = narration_stream.next().await {
                    match chunk {
                        Ok(resp) => {
                            if let Some(token) =
                                resp.choices.first().and_then(|c| c.delta.content.clone())
                            {
                                let visible = think.push(&token);
                                if !visible.is_empty() {
                                    trace.record_output(&visible);
                                    full_answer.push_str(&visible);
                                    yield token_event(&visible);
                                }
                            }
                        }
                        Err(e) => {
                            had_error = true;
                            yield Event::data(format!("Foundry Local: {e}")).event("error");
                            break;
                        }
                    }
                }
                if !had_error {
                    let tail = think.finish();
                    if !tail.is_empty() {
                        trace.record_output(&tail);
                        full_answer.push_str(&tail);
                        yield token_event(&tail);
                    }
                }
            }

            AgentData::Semantic { citations_json, gen_stream } => {
                yield Event::data(citations_json).event("citations");

                match gen_stream {
                    None => {
                        let msg = crate::rag::no_grounding_message(store_is_empty);
                        trace.record_output(msg);
                        full_answer.push_str(msg);
                        yield token_event(msg);
                    }
                    Some(mut stream) => {
                        let mut think = crate::foundry::think_filter::ThinkFilter::new();
                        while let Some(chunk) = stream.next().await {
                            match chunk {
                                Ok(resp) => {
                                    if let Some(token) = resp
                                        .choices
                                        .first()
                                        .and_then(|c| c.delta.content.clone())
                                    {
                                        let visible = think.push(&token);
                                        if !visible.is_empty() {
                                            trace.record_output(&visible);
                                            full_answer.push_str(&visible);
                                            yield token_event(&visible);
                                        }
                                    }
                                }
                                Err(e) => {
                                    had_error = true;
                                    yield Event::data(format!("Foundry Local: {e}"))
                                        .event("error");
                                    break;
                                }
                            }
                        }
                        if !had_error {
                            let tail = think.finish();
                            if !tail.is_empty() {
                                trace.record_output(&tail);
                                full_answer.push_str(&tail);
                                yield token_event(&tail);
                            }
                        }
                    }
                }
            }
        }

        if let Some(cid) = &persist_cid {
            if !had_error {
                use crate::routes::conversations::persist_agent_assistant_message;
                if let Err(e) = trace
                    .time(
                        Stage::Persist,
                        persist_agent_assistant_message(
                            &db,
                            cid.as_str(),
                            &uid,
                            &full_answer,
                            &kind_str,
                            &persist_passages,
                            persist_structured_json.as_deref(),
                        ),
                    )
                    .await
                {
                    tracing::warn!(error = %e, "failed to persist agent assistant message");
                } else {
                    crate::memory::maybe_spawn_compaction(
                        db.clone(), foundry_handle.clone(), cfg.clone(), compact_spec.clone(), cid.clone(),
                    );
                }
            }
        }

        drop(_generation);
        if had_error {
            trace.finish("stream_error", Some("generation"));
        } else {
            trace.finish("ok", None);
        }
        trace.progress_finished();
        yield Event::data("").event("done");
    })
}

/// Whether the DocumentDB aggregation planner is worth invoking: the router must
/// have called the question structural *and* something must actually be ingested.
/// An empty catalog rejects every collection a planner could name, so running it
/// there buys a guaranteed validation error in place of an answer.
fn can_run_docdb_aggregation(
    class: &crate::router::RouteClass,
    catalog: &crate::aggregation::catalog::Catalog,
) -> bool {
    matches!(class, crate::router::RouteClass::Structured { .. }) && !catalog.collections.is_empty()
}

fn should_try_deterministic_sql(kind: AgentKind, question: &str) -> bool {
    if question.trim().is_empty() {
        return false;
    }
    match kind {
        AgentKind::PatientLookup => crate::nl2sql::routes::is_patient_overview_question(question),
        // Only structurally-marked questions; semantic follow-ups ("what are they
        // about") must reach the retrieval pipeline instead.
        AgentKind::HealthQuery | AgentKind::Trends => matches!(
            classify_lexical(question),
            Some(QueryIntent::Aggregation | QueryIntent::Trend | QueryIntent::Enumeration)
        ),
        AgentKind::Summarize => crate::nl2sql::routes::is_recent_records_question(question),
        _ => false,
    }
}

/// Plan → validate → execute a DocumentDB aggregation and open its narration
/// stream. Extracted from the route so every failure inside it is one `Err` the
/// caller can trade for a semantic answer; inline `?`s here used to abort the
/// whole request with a raw `400` in the middle of a conversation.
///
/// Returns the SSE payload plus the JSON blob persisted with the message.
#[allow(clippy::too_many_arguments)]
async fn run_docdb_aggregation(
    state: &AppState,
    user: &AuthUser,
    kind: AgentKind,
    standalone: &str,
    catalog: &crate::aggregation::catalog::Catalog,
    trace: &RequestTrace,
    generation_permit: &mut Option<tokio::sync::OwnedSemaphorePermit>,
) -> AppResult<(AgentData, String)> {
    let foundry = state.foundry()?;
    *generation_permit = Some(state.admission.generation().await?);
    let spec = state.spec_for(kind);
    let intent = if kind == AgentKind::Trends {
        QueryIntent::Trend
    } else {
        QueryIntent::Aggregation
    };
    let planner_system = build_agg_planner_system(catalog, intent);

    // 1. Plan: model emits a RunAggregation tool call.
    let planned = trace
        .time(
            Stage::Plan,
            foundry.plan_aggregation(&spec, &planner_system, standalone),
        )
        .await?;

    // 2. Validate + sanitize against the current catalog.
    let validated = trace.time_sync(Stage::Validate, || validate(&planned, catalog))?;

    // 3. Execute pipeline against DocumentDB.
    let (rows, pipeline_docs) = trace
        .time(Stage::Execute, execute::run(&state.db, user, &validated))
        .await?;
    trace.stage_detail(
        Stage::Execute,
        format!("{} result rows from ingested records", rows.len()),
    );

    let spec_json = serde_json::to_string(&validated).unwrap_or_else(|_| "{}".to_string());
    let rows_json = serde_json::to_string(&rows).unwrap_or_else(|_| "[]".to_string());
    let pipeline_json = serde_json::to_string(
        &pipeline_docs
            .iter()
            .map(|d| mongodb::bson::Bson::Document(d.clone()).into_relaxed_extjson())
            .collect::<Vec<_>>(),
    )
    .unwrap_or_else(|_| "[]".to_string());

    let structured_json =
        format!("{{\"spec\":{spec_json},\"rows\":{rows_json},\"pipeline\":{pipeline_json}}}");

    // 4. Open the grounded narration stream.
    let narration_user = build_narration_user(standalone, &rows);
    let mut narration_spec = state.spec_for(kind);
    narration_spec.tools = false; // narration never calls tools
    let narration_stream = trace
        .time(
            Stage::Narrate,
            foundry.generate_stream_with(&narration_spec, NARRATION_SYSTEM_PROMPT, &narration_user),
        )
        .await?;

    Ok((
        AgentData::Structured {
            spec_json,
            rows_json,
            pipeline_json,
            narration_stream,
        },
        structured_json,
    ))
}

/// Ground an anaphoric follow-up ("list their names") on an identifier from a
/// prior turn and try the deterministic SQL compiler only. Mirrors `/chat`'s
/// resolver: model-free, so it is safe to attempt on every follow-up.
async fn resolve_followup_sql(
    state: &AppState,
    question: &str,
    memory: &WorkingMemory,
) -> Option<crate::nl2sql::routes::PreparedNlQuery> {
    let augmented = crate::nl2sql::routes::resolve_followup_question(
        question,
        memory.tail.iter().map(|turn| turn.content.as_str()),
    )?;
    match crate::nl2sql::routes::prepare_auto_query_deterministic(state, &augmented).await {
        Ok(prepared) => prepared,
        Err(error) => {
            tracing::debug!(%error, "agent follow-up SQL resolution failed; continuing");
            None
        }
    }
}

async fn build_semantic_agent_data(
    state: &AppState,
    kind: AgentKind,
    memory: &WorkingMemory,
    question: &str,
    prepared_standalone: Option<String>,
    trace: &RequestTrace,
    generation_permit: &mut Option<tokio::sync::OwnedSemaphorePermit>,
    persist_passages: &mut Vec<crate::retrieval::Passage>,
) -> AppResult<AgentData> {
    // Release any permit the caller already holds (structured path that failed and
    // fell through to here) before asking for another — assigning would evaluate
    // the new acquire while the old permit is still alive, deadlocking a
    // single-permit semaphore.
    generation_permit.take();
    *generation_permit = Some(state.admission.generation().await?);
    let foundry = state.foundry()?;
    let generation_spec = state.spec_for(kind);
    let rewrite_spec = state.spec_for(AgentKind::QueryRewrite);
    // Callers that already rewrote the question (structured fall-through) skip
    // the second rewrite model call and only expand.
    let (standalone, queries) = match prepared_standalone {
        Some(standalone) => {
            let queries = trace
                .time(
                    Stage::RewriteExpand,
                    expand_queries_with(foundry, &rewrite_spec, &state.config, &standalone),
                )
                .await;
            (standalone, queries)
        }
        None => {
            let rewrite_turns = memory.rewrite_turns();
            trace
                .time(
                    Stage::RewriteExpand,
                    prepare_queries_with(
                        foundry,
                        &rewrite_spec,
                        &state.config,
                        &rewrite_turns,
                        question,
                    ),
                )
                .await
        }
    };
    let broad = retrieval::is_broad_question(&standalone);
    let _retrieval_permit = state.admission.retrieval().await?;
    let passages = retrieval::retrieve_observed(
        &state.db,
        &state.config,
        &queries,
        state.config.retrieval_mode,
        state.config.rerank_enabled && !broad,
        state.config.context_top_k,
        trace,
    )
    .await?;
    let refuse = trace.time_sync(Stage::Gate, || {
        retrieval::should_refuse_semantic(
            &standalone,
            passages.first().map(|passage| passage.score),
            state.config.score_gate,
        )
    });
    trace.set_gated(refuse);
    let passages = apply_context_budget(
        passages,
        state.config.context_total_tokens,
        state.config.context_per_row_tokens,
    );
    let citations_json = serde_json::to_string(&passages).unwrap_or_else(|_| "[]".to_string());
    *persist_passages = passages.clone();
    let gen_stream = if refuse {
        None
    } else {
        let prompt = trace.time_sync(Stage::Prompt, || {
            build_prompt(&passages, memory, &standalone)
        });
        Some(
            trace
                .time(
                    Stage::Generate,
                    foundry.generate_stream_with(&generation_spec, SYSTEM_PROMPT, &prompt),
                )
                .await?,
        )
    };
    Ok(AgentData::Semantic {
        citations_json,
        gen_stream,
    })
}

fn narrate_patient_rows(columns: &[String], rows: &[Vec<serde_json::Value>]) -> String {
    if rows.is_empty() {
        return "No matching patient record was found.".to_string();
    }
    rows.iter()
        .map(|row| {
            columns
                .iter()
                .zip(row)
                .filter(|(_, value)| !value.is_null())
                .map(|(column, value)| {
                    let label = column.replace('_', " ");
                    let value = value
                        .as_str()
                        .map(str::to_string)
                        .unwrap_or_else(|| value.to_string());
                    format!("{label}: {value}")
                })
                .collect::<Vec<_>>()
                .join(", ")
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn narrate_recent_rows(columns: &[String], rows: &[Vec<serde_json::Value>], sql: &str) -> String {
    if rows.is_empty() {
        return "No recent records were found.".to_string();
    }
    let limit = sql
        .rsplit_once(" LIMIT ")
        .and_then(|(_, value)| value.parse::<usize>().ok())
        .unwrap_or(rows.len());
    let table = sql
        .split_once(" FROM ")
        .and_then(|(_, rest)| rest.split_whitespace().next())
        .and_then(|name| name.rsplit('.').next())
        .unwrap_or("records")
        .replace('_', " ");
    let lines = rows
        .iter()
        .map(|row| {
            columns
                .iter()
                .zip(row)
                .filter(|(_, value)| !value.is_null())
                .map(|(_, value)| json_label(value))
                .collect::<Vec<_>>()
                .join(" — ")
        })
        .collect::<Vec<_>>()
        .join("\n");
    format!("The {limit} most recent {table}:\n{lines}")
}

/// Adapt a validated live-SQL result to the established structured-agent SSE
/// shape. Scalar results become one `(all)` bar; grouped/trend results use the
/// first column as the label and the rightmost numeric column as the value.
fn deterministic_structured_result(
    prepared: crate::nl2sql::routes::PreparedNlQuery,
    kind: AgentKind,
) -> (String, String, String, String) {
    let rows = sql_rows_to_agg_rows(&prepared.rows);
    let time_bucket = if kind == AgentKind::Trends {
        prepared
            .columns
            .first()
            .map(|field| serde_json::json!({ "field": field, "unit": "month" }))
            .unwrap_or(serde_json::Value::Null)
    } else {
        serde_json::Value::Null
    };
    let spec_json = serde_json::json!({
        "collection": "live_source",
        "filter": {},
        "group_by": [],
        "metric": { "op": "count", "field": null },
        "time_bucket": time_bucket,
        "sort": null,
        "top_n": rows.len(),
    })
    .to_string();
    let rows_json = serde_json::to_string(&rows).unwrap_or_else(|_| "[]".to_string());
    let pipeline_json = serde_json::json!([{
        "backend": "source_sql",
        "source_id": prepared.source_id,
        "sql": prepared.sql,
        "columns": prepared.columns,
    }])
    .to_string();
    (spec_json, rows_json, pipeline_json, prepared.answer)
}

fn sql_rows_to_agg_rows(rows: &[Vec<serde_json::Value>]) -> Vec<execute::AggRow> {
    rows.iter()
        .filter_map(|row| {
            let (value_index, value) = row
                .iter()
                .enumerate()
                .rev()
                .find_map(|(index, value)| json_number(value).map(|number| (index, number)))?;
            let label = if row.len() == 1 {
                "(all)".to_string()
            } else {
                row.iter()
                    .take(value_index)
                    .find(|candidate| !candidate.is_null())
                    .map(json_label)
                    .unwrap_or_else(|| "(all)".to_string())
            };
            Some(execute::AggRow { label, value })
        })
        .collect()
}

fn json_number(value: &serde_json::Value) -> Option<f64> {
    value
        .as_f64()
        .or_else(|| value.as_str().and_then(|text| text.parse::<f64>().ok()))
}

fn json_label(value: &serde_json::Value) -> String {
    match value {
        serde_json::Value::String(text) => text.clone(),
        other => other.to_string(),
    }
}

// ---------------------------------------------------------------------------
// Kind parsing
// ---------------------------------------------------------------------------

/// Parse the URL path segment into `AgentKind`. Returns `BadRequest` for unknown
/// values so the error is an HTTP response, not a mid-stream `error` event.
/// `"auto"` is handled before this function is called and will never reach it.
fn parse_kind(s: &str) -> AppResult<AgentKind> {
    serde_json::from_value(serde_json::Value::String(s.to_string())).map_err(|_| {
        AppError::BadRequest(format!(
            "unknown agent kind '{s}'; valid: auto, patient_lookup, health_query, trends, \
             summarize, chat, multi_hop, query_rewrite, classify, extract, verify"
        ))
    })
}

/// Encode a streamed token as an SSE `token` event. JSON-encoding protects the
/// token's leading/trailing spaces, which the SSE spec would otherwise strip from a
/// bare `data:` field — fusing words together on the client.
fn token_event(text: &str) -> Event {
    Event::data(serde_json::to_string(text).unwrap_or_else(|_| "\"\"".to_string())).event("token")
}

#[cfg(test)]
mod tests {
    use super::{
        can_run_docdb_aggregation, narrate_patient_rows, narrate_recent_rows,
        should_try_deterministic_sql, sql_rows_to_agg_rows,
    };
    use crate::aggregation::catalog::{Catalog, CollectionMeta};
    use crate::aggregation::intent::QueryIntent;
    use crate::config::Config;
    use crate::foundry::router::{AgentKind, ModelSpec};

    #[test]
    fn health_query_dispatches_to_deterministic_sql_first() {
        let question = "How many lab results were abnormal?";
        assert!(should_try_deterministic_sql(
            AgentKind::HealthQuery,
            question
        ));
        assert!(!should_try_deterministic_sql(
            AgentKind::PatientLookup,
            question
        ));
        assert!(!should_try_deterministic_sql(
            AgentKind::Summarize,
            question
        ));
    }

    #[test]
    fn aggregation_planner_is_skipped_until_something_is_ingested() {
        let structured = crate::router::RouteClass::Structured {
            intent: QueryIntent::Aggregation,
            backend: crate::router::StructuredBackend::DocDb,
        };
        let empty = Catalog::empty();
        assert!(!can_run_docdb_aggregation(&structured, &empty));

        let mut populated = Catalog::empty();
        populated.collections.insert(
            "patients".to_string(),
            CollectionMeta {
                label: "Patients".to_string(),
                fields: Vec::new(),
                concept: None,
                service_lines: Vec::new(),
            },
        );
        assert!(can_run_docdb_aggregation(&structured, &populated));
        // Semantic questions never reach the planner, ingested data or not.
        assert!(!can_run_docdb_aggregation(
            &crate::router::RouteClass::Semantic,
            &populated
        ));
    }

    #[test]
    fn semantic_followups_skip_deterministic_sql_for_structured_agents() {
        for question in ["What are they about?", "Explain what those results mean."] {
            assert!(!should_try_deterministic_sql(
                AgentKind::HealthQuery,
                question
            ));
            assert!(!should_try_deterministic_sql(AgentKind::Trends, question));
        }
    }

    #[test]
    fn patient_lookup_dispatches_to_deterministic_sql_first() {
        assert!(should_try_deterministic_sql(
            AgentKind::PatientLookup,
            "Find Jane Chebet's record."
        ));
    }

    #[test]
    fn summarize_dispatches_to_deterministic_recent_records() {
        assert!(should_try_deterministic_sql(
            AgentKind::Summarize,
            "Give me an overview of the most recent encounters."
        ));
        assert!(!should_try_deterministic_sql(
            AgentKind::Summarize,
            "Summarize the encounter notes for Jane Chebet."
        ));
    }

    #[test]
    fn deterministic_recent_records_are_narrated_as_readable_lines() {
        let answer = narrate_recent_rows(
            &[
                "encounter_date".into(),
                "department".into(),
                "chief_complaint".into(),
            ],
            &[vec![
                serde_json::json!("2026-08-25"),
                serde_json::json!("Surgery"),
                serde_json::json!("Weight loss"),
            ]],
            "SELECT encounter_date, department, chief_complaint FROM encounters ORDER BY encounter_date DESC LIMIT 10",
        );
        assert_eq!(
            answer,
            "The 10 most recent encounters:\n2026-08-25 — Surgery — Weight loss"
        );
    }

    #[test]
    fn patient_lookup_generation_uses_configured_lookup_role() {
        let mut config = Config::from_env();
        config.router.lookup = "configured-lookup-model".to_string();
        let spec = ModelSpec::for_kind(AgentKind::PatientLookup, &config);
        assert_eq!(spec.alias, "configured-lookup-model");
    }

    #[test]
    fn deterministic_patient_lookup_narrates_non_null_fields() {
        let answer = narrate_patient_rows(
            &[
                "patient_no".into(),
                "first_name".into(),
                "middle_name".into(),
            ],
            &[vec![
                serde_json::json!("SYN-2024-0001"),
                serde_json::json!("Jane"),
                serde_json::Value::Null,
            ]],
        );
        assert_eq!(answer, "patient no: SYN-2024-0001, first name: Jane");
    }

    #[test]
    fn deterministic_scalar_sql_result_preserves_exact_count_for_chart() {
        let rows = sql_rows_to_agg_rows(&[vec![serde_json::json!(16)]]);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].label, "(all)");
        assert_eq!(rows[0].value, 16.0);
    }
}
