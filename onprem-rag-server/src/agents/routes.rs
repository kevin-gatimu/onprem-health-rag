//! `POST /agents/<kind>` — SSE endpoint for task-aware AI agents.
//!
//! Routes by `AgentKind`:
//! - **Semantic** (Chat, PatientLookup, Summarize): retrieval + `generate_stream_with`.
//!   Emits: `routed` -> `citations` -> `token`* -> `done`.
//! - **Structured** (HealthQuery, Trends): `plan_aggregation` -> `validate` -> `execute`
//!   -> grounded narration.  Emits: `routed` -> `spec` -> `rows` -> `pipeline` -> `token`* -> `done`.
//!
//! The SSE event set is a strict superset of `/chat` so the bridge relay can be
//! generalised without breaking existing clients.
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

use foundry_local_sdk::ChatCompletionStream;
use rocket::response::stream::{Event, EventStream};
use rocket::serde::json::Json;
use rocket::{State, post};
use serde::Deserialize;

use crate::aggregation::execute;
use crate::aggregation::intent::QueryIntent;
use crate::aggregation::validate;
use crate::answer::{NARRATION_SYSTEM_PROMPT, build_agg_planner_system, build_narration_user};
use crate::auth::guard::AuthUser;
use crate::error::{AppError, AppResult};
use crate::foundry::router::AgentKind;
use crate::rag::{SYSTEM_PROMPT, apply_context_budget, build_prompt, prepare_queries};
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
}

// ---------------------------------------------------------------------------
// Pre-computed data that flows into the single EventStream! generator
// ---------------------------------------------------------------------------

/// All the data prepared before the SSE generator opens. Keeping async work out
/// of the generator means setup errors become HTTP errors (not mid-stream events)
/// and we avoid smuggling non-`Send` temporaries into the async generator.
enum AgentData {
    Structured {
        /// Serialised `RunAggregation` spec (provenance).
        spec_json: String,
        /// Serialised `Vec<AggRow>` for the UI chart/table.
        rows_json: String,
        /// Serialised executed pipeline (provenance).
        pipeline_json: String,
        /// Already-opened narration stream (iterate inside EventStream!).
        narration_stream: ChatCompletionStream,
    },
    Semantic {
        /// Serialised `Vec<Passage>` for citations.
        citations_json: String,
        /// `None` when the score gate refuses to answer.
        gen_stream: Option<ChatCompletionStream>,
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
    let generation_permit = state.admission.generation().await?;

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

    let foundry = state.foundry()?;
    let req = body.into_inner();
    let spec = state.spec_for(resolved_kind);

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
        // Structured analytical path: HealthQuery + Trends
        // -----------------------------------------------------------------
        AgentKind::HealthQuery | AgentKind::Trends => {
            let cat = state.catalog();
            let intent = if resolved_kind == AgentKind::Trends {
                QueryIntent::Trend
            } else {
                QueryIntent::Aggregation
            };
            let planner_system = build_agg_planner_system(&cat, intent);

            // 1. Plan: model emits a RunAggregation tool call.
            let planned = trace
                .time(
                    Stage::Plan,
                    foundry.plan_aggregation(&spec, &planner_system, &req.question),
                )
                .await?;

            // 2. Validate + sanitize against the current catalog.
            let validated = trace.time_sync(Stage::Validate, || validate(&planned, &cat))?;

            // 3. Execute pipeline against DocumentDB.
            let (rows, pipeline_docs) = trace
                .time(Stage::Execute, execute::run(&state.db, &user, &validated))
                .await?;

            let spec_json = serde_json::to_string(&validated).unwrap_or_else(|_| "{}".to_string());
            let rows_json = serde_json::to_string(&rows).unwrap_or_else(|_| "[]".to_string());
            let pipeline_json = serde_json::to_string(
                &pipeline_docs
                    .iter()
                    .map(|d| mongodb::bson::Bson::Document(d.clone()).into_relaxed_extjson())
                    .collect::<Vec<_>>(),
            )
            .unwrap_or_else(|_| "[]".to_string());

            persist_structured_json = Some(format!(
                "{{\"spec\":{spec_json},\"rows\":{rows_json},\"pipeline\":{pipeline_json}}}"
            ));

            // 4. Open the grounded narration stream.
            let narration_user = build_narration_user(&req.question, &rows);
            let mut narration_spec = state.spec_for(resolved_kind);
            narration_spec.tools = false; // narration never calls tools
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

            AgentData::Structured {
                spec_json,
                rows_json,
                pipeline_json,
                narration_stream,
            }
        }

        // -----------------------------------------------------------------
        // Semantic path: Chat, PatientLookup, Summarize (+ other kinds)
        // -----------------------------------------------------------------
        _ => {
            let mode = state.config.retrieval_mode;
            let rerank = state.config.rerank_enabled;
            let top_k = state.config.context_top_k;

            let (standalone, queries) = trace
                .time(
                    Stage::RewriteExpand,
                    prepare_queries(foundry, &state.config, &history, &req.question),
                )
                .await;
            let _retrieval_permit = state.admission.retrieval().await?;
            let passages = retrieval::retrieve_observed(
                &state.db,
                &state.config,
                &queries,
                mode,
                rerank,
                top_k,
                &trace,
            )
            .await?;

            let refuse = trace.time_sync(Stage::Gate, || {
                passages.is_empty()
                    || state
                        .config
                        .score_gate
                        .is_some_and(|floor| passages[0].score < floor)
            });
            trace.set_gated(refuse);

            let passages = apply_context_budget(
                passages,
                state.config.context_total_tokens,
                state.config.context_per_row_tokens,
            );
            let citations_json =
                serde_json::to_string(&passages).unwrap_or_else(|_| "[]".to_string());

            persist_passages = passages.clone();

            let gen_stream = if refuse {
                None
            } else {
                let prompt =
                    trace.time_sync(Stage::Prompt, || build_prompt(&passages, &standalone));
                Some(
                    trace
                        .time(
                            Stage::Generate,
                            foundry.generate_stream_with(&spec, SYSTEM_PROMPT, &prompt),
                        )
                        .await?,
                )
            };

            AgentData::Semantic {
                citations_json,
                gen_stream,
            }
        }
    };

    let db = state.db.clone();
    let uid = user.id.clone();
    let persist_cid = req.conversation_id.clone();

    Ok(EventStream! {
        use futures::StreamExt;

        let _generation_permit = generation_permit;

        yield Event::data(serde_json::to_string(&kind_str).unwrap_or_default())
            .event("routed");

        let mut full_answer = String::new();
        let mut had_error = false;
        let _generation = trace.stage_guard(Stage::Generate);

        match data {
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
                        let msg = "I don't have relevant records to answer that question.";
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
                }
            }
        }

        drop(_generation);
        if had_error {
            trace.finish("stream_error", Some("generation"));
        } else {
            trace.finish("ok", None);
        }
        yield Event::data("").event("done");
    })
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
