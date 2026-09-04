//! RAG HTTP routes: `POST /search` (debug — ranked passages, no generation) and
//! `POST /chat` (SSE — grounded, streamed answer with citations).
//!
//! Both accept per-request retrieval overrides (mode, rerank, top_k) so the Chat UI's
//! toggles work without a server restart; unset fields fall back to config defaults.
//!
//! Stage C intent routing: before falling through to the semantic retrieval path,
//! `/chat` classifies the question with a fast lexical pass. Aggregation, Trend, and
//! Enumeration intents are handled by `answer::run_structured`; all others use the
//! existing semantic retrieval + RAG generation path. Any `run_structured` failure
//! falls back silently to the semantic path — users get a correct (if less precise)
//! answer rather than an error.

use std::sync::Arc;

use rocket::response::stream::{Event, EventStream};
use rocket::serde::json::Json;
use rocket::{State, post};
use serde::{Deserialize, Serialize};

use super::{ChatTurn, SYSTEM_PROMPT, apply_context_budget, build_prompt, prepare_queries_with};
use crate::answer::{Structured, run_structured};
use crate::auth::guard::AuthUser;
use crate::config::{Config, RetrievalMode};
use crate::documentdb::DocumentDb;
use crate::error::AppResult;
use crate::foundry::{FoundryManager, GuardedChatStream};
use crate::memory::{WorkingMemory, load_working_memory};
use crate::retrieval::{self, Passage};
use crate::state::AppState;
use crate::telemetry::{RequestTrace, Stage};

/// Shared retrieval knobs accepted by both endpoints.
#[derive(Debug, Deserialize)]
pub struct RetrievalOpts {
    /// "vector" or "hybrid"; defaults to the server's configured mode.
    pub mode: Option<String>,
    /// Toggle cross-encoder reranking; defaults to the server's configured value.
    pub rerank: Option<bool>,
    /// Passages fed to the model; defaults to `context_top_k`.
    pub top_k: Option<usize>,
}

/// Resolve per-request opts against config defaults into concrete values.
fn resolve(state: &AppState, opts: &RetrievalOpts) -> (RetrievalMode, bool, usize) {
    let mode = match opts
        .mode
        .as_deref()
        .map(str::trim)
        .map(str::to_ascii_lowercase)
        .as_deref()
    {
        Some("vector") => RetrievalMode::Vector,
        Some("hybrid") => RetrievalMode::Hybrid,
        _ => state.config.retrieval_mode,
    };
    let rerank = opts.rerank.unwrap_or(state.config.rerank_enabled);
    let top_k = opts.top_k.unwrap_or(state.config.context_top_k);
    (mode, rerank, top_k)
}

#[derive(Debug, Deserialize)]
pub struct RouteRequest {
    pub question: String,
    #[serde(default)]
    pub has_history: bool,
}

#[derive(Debug, Serialize)]
pub struct RouteResponse {
    pub route: &'static str,
    pub intent: Option<crate::aggregation::intent::QueryIntent>,
    pub tier: u8,
    pub cached: bool,
    pub backend: Option<&'static str>,
}

/// `POST /route` — classify a question without executing retrieval or generation.
/// Used by the local deterministic evaluation runner and protected by normal auth.
#[post("/route", data = "<body>")]
pub async fn route(
    state: &State<AppState>,
    _user: AuthUser,
    body: Json<RouteRequest>,
) -> AppResult<Json<RouteResponse>> {
    let _generation_permit = state.admission.generation().await?;
    let foundry = state.foundry().ok();
    let decision = crate::router::route(
        &body.question,
        body.has_history,
        &state.config,
        foundry,
        &state.spec_for(crate::foundry::router::AgentKind::Classify),
        &state.router_cache,
    )
    .await;

    let (intent, backend) = match &decision.class {
        crate::router::RouteClass::Structured { intent, backend } => (
            Some(*intent),
            Some(match backend {
                crate::router::StructuredBackend::DocDb => "doc_db",
                crate::router::StructuredBackend::SourceSql => "source_sql",
            }),
        ),
        crate::router::RouteClass::Hybrid { cohort_intent } => (Some(*cohort_intent), None),
        crate::router::RouteClass::Conversational
        | crate::router::RouteClass::ConversationMeta
        | crate::router::RouteClass::Semantic => (None, None),
    };

    Ok(Json(RouteResponse {
        route: decision.route_label(),
        intent,
        tier: decision.tier,
        cached: decision.cached,
        backend,
    }))
}

#[derive(Debug, Deserialize)]
pub struct SearchRequest {
    pub query: String,
    #[serde(flatten)]
    pub opts: RetrievalOpts,
    /// Optional prior turns for history-aware rewrite (usually empty for debug search).
    #[serde(default)]
    pub history: Vec<ChatTurn>,
}

#[derive(Debug, Serialize)]
pub struct SearchResponse {
    /// The standalone query actually used (after history-aware rewrite).
    pub query: String,
    /// All queries issued to retrieval (primary + expansions).
    pub queries: Vec<String>,
    pub passages: Vec<Passage>,
}

/// `POST /search` — run the retrieval pipeline and return the ranked passages without
/// generation. The debugging window into fusion + reranking (per the WS6 gate).
#[post("/search", data = "<body>")]
pub async fn search(
    state: &State<AppState>,
    _user: AuthUser,
    body: Json<SearchRequest>,
) -> AppResult<Json<SearchResponse>> {
    let trace = RequestTrace::new("search");
    trace.set_route("search", None, None);
    let (mode, rerank, top_k) = resolve(state, &body.opts);
    let foundry = state.foundry()?;
    let generation_permit = state.admission.generation().await?;

    let (standalone, queries) = trace
        .time(
            Stage::RewriteExpand,
            prepare_queries_with(
                foundry,
                &state.spec_for(crate::foundry::router::AgentKind::QueryRewrite),
                &state.config,
                &body.history,
                &body.query,
            ),
        )
        .await;
    drop(generation_permit);
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

    trace.finish("ok", None);
    Ok(Json(SearchResponse {
        query: standalone,
        queries,
        passages,
    }))
}

#[derive(Debug, Deserialize)]
pub struct ChatRequest {
    pub question: String,
    #[serde(default)]
    pub history: Vec<ChatTurn>,
    #[serde(flatten)]
    pub opts: RetrievalOpts,
    /// When present, the server persists messages to this conversation and loads
    /// history from the DB instead of `history`. Absent → stateless (no persistence).
    #[serde(default)]
    pub conversation_id: Option<String>,
    /// Client-minted id for this run. When present, every pipeline stage is
    /// published to `GET /runs/<run_id>/progress` so the UI can show what the
    /// server is actually doing while it works. Absent → no progress fan-out.
    #[serde(default)]
    pub run_id: Option<String>,
}

// ---------------------------------------------------------------------------
// Pre-computed data for the /chat EventStream! generator (Stage C)
// ---------------------------------------------------------------------------

/// All data prepared before the EventStream! generator opens, so every async
/// DB/model call is done and errors surface as HTTP errors rather than
/// mid-stream events.
enum ChatData {
    /// Conversational (Tier 0): greeting / thanks / identity / out-of-domain.
    /// One cheap streamed reply, no retrieval and no citations.
    Conversational { reply_stream: GuardedChatStream },
    /// Deterministic structured answer, emitted without a model narration pass.
    DirectStructured { answer: String },
    /// Structured query executed against the selected operational SQL source.
    SourceSql {
        source_id: String,
        sql: String,
        explanation: String,
        columns: Vec<String>,
        rows: Vec<Vec<serde_json::Value>>,
        answer: String,
    },
    /// Structured query (Aggregation, Trend, or Enumeration): narrate the exact
    /// DB rows; no semantic retrieval. The `citations_json` is either an empty
    /// array (aggregate path) or a synthesized list of row passages (list path).
    Structured {
        citations_json: String,
        narration_stream: GuardedChatStream,
        passages_for_persist: Vec<Passage>,
    },
    /// Semantic retrieval + RAG generation (existing path).
    Semantic {
        citations: String,
        stream: Option<GuardedChatStream>,
        passages_for_persist: Vec<Passage>,
    },
}

/// `POST /chat` — grounded RAG answer streamed as SSE.
///
/// Event sequence: one `citations` event (JSON array of the passages backing the
/// answer, in citation order) → zero or more `token` events → a terminal `done`.
/// Retrieval and planning finish before the stream opens, so live pipeline stages
/// travel on `GET /runs/<run_id>/progress` instead — pass `run_id` to enable it.
/// Retrieval failures surface as a normal HTTP error before streaming begins; a
/// generation failure mid-stream is emitted as an `error` event.
///
/// When `conversation_id` is present the server persists the exchange and loads
/// history from the DB. When absent the call is stateless (no persistence) —
/// `/search`, the agents path, and the eval harness rely on this.
///
/// Stage C: before retrieval, classify the intent. Aggregation/Trend/Enumeration
/// questions are answered via `run_structured` (exact DB aggregation / list).
/// Any failure in the structured path falls back silently to the semantic path.
#[post("/chat", data = "<body>")]
pub async fn chat(
    state: &State<AppState>,
    user: AuthUser,
    body: Json<ChatRequest>,
) -> AppResult<EventStream![]> {
    let trace = RequestTrace::new("chat");
    if let Some(run_id) = body.run_id.clone() {
        trace.attach_progress(state.run_progress.clone(), run_id);
    }
    let (mode, rerank, top_k) = resolve(state, &body.opts);
    let foundry = state.foundry()?;
    let mut generation_permit = None;

    // Conversation persistence (pre-stream, so errors stay clean HTTP errors).
    // When conversation_id is absent, fall back to body.history (stateless path).
    let memory: WorkingMemory = trace
        .time(Stage::History, async {
            if let Some(cid) = body.conversation_id.as_deref() {
                use crate::routes::conversations::{persist_user_message, verify_owned};
                verify_owned(&state.db, cid, &user.id).await?;
                let memory = load_working_memory(&state.db, cid, &state.config).await;
                persist_user_message(&state.db, cid, &user.id, &body.question).await?;
                Ok::<WorkingMemory, crate::error::AppError>(memory)
            } else {
                Ok::<WorkingMemory, crate::error::AppError>(WorkingMemory::from_turns(
                    body.history.clone(),
                ))
            }
        })
        .await?;

    // Intent Router v2: Tier 0 conversational gate -> Tier 1 lexical -> Tier 2 model
    // classify, fail-open to semantic. Structured routes (Aggregation/Trend/Enumeration)
    // go through plan->validate->execute->narrate; conversational replies skip retrieval;
    // everything else (including hybrid, until its Phase-C executor lands) is semantic.
    // Any structured failure falls back silently to semantic — a correct grounded answer
    // beats an error. See plans/17-intent-router-v2.md.
    let has_history = !memory.is_empty();
    let decision = trace
        .time(
            Stage::Route,
            crate::router::route(
                &body.question,
                has_history,
                &state.config,
                Some(foundry),
                &state.spec_for(crate::foundry::router::AgentKind::Classify),
                &state.router_cache,
            ),
        )
        .await;
    trace.set_route(
        decision.route_label(),
        Some(decision.tier),
        Some(decision.cached),
    );
    trace.stage_detail(Stage::Route, format!("{} question", decision.route_label()));
    let routed_json = decision.to_sse_json();

    let chat_data: ChatData = match &decision.class {
        crate::router::RouteClass::Conversational => {
            generation_permit = Some(state.admission.generation().await?);
            // Cheap, warm, retrieval-free reply. Honour any persisted "chat" role
            // override, but nudge temperature up and cap the length — this is small talk.
            let mut spec = state.spec_for(crate::foundry::router::AgentKind::Chat);
            spec.tools = false;
            spec.temperature = 0.4;
            spec.max_tokens = Some(160);
            let reply_stream = trace
                .time(
                    Stage::Generate,
                    foundry.generate_stream_with(
                        &spec,
                        crate::router::CONVERSATIONAL_SYSTEM_PROMPT,
                        &body.question,
                    ),
                )
                .await?;
            ChatData::Conversational { reply_stream }
        }

        crate::router::RouteClass::ConversationMeta => {
            generation_permit = Some(state.admission.generation().await?);
            // Answer from working memory alone — no retrieval, no citations. The
            // conversational reply path already covers the empty-citations SSE
            // contract, so this just builds a memory-only prompt and reuses it.
            let mut spec = state.spec_for(crate::foundry::router::AgentKind::Chat);
            spec.tools = false;
            spec.temperature = 0.2;
            let system = "Answer the user's question about THIS conversation's own history, \
                using only the conversation summary and recent turns below. Do not invent \
                anything not present there; if they don't cover the question, say you don't \
                have that in this conversation's history.";
            let user = format!(
                "Conversation summary: {}\nRecent turns:\n{}\n\nQuestion: {}",
                memory.summary.as_deref().unwrap_or("(none)"),
                memory
                    .tail
                    .iter()
                    .map(|t| format!("{}: {}", t.role, t.content))
                    .collect::<Vec<_>>()
                    .join("\n"),
                body.question,
            );
            let reply_stream = foundry.generate_stream_with(&spec, system, &user).await?;
            ChatData::Conversational { reply_stream }
        }

        crate::router::RouteClass::Structured { intent, backend } => {
            let intent = *intent;
            let sql_data = if *backend == crate::router::StructuredBackend::SourceSql {
                match crate::nl2sql::routes::prepare_auto_query(state.inner(), &body.question).await
                {
                    Ok(Some(prepared)) => {
                        trace.stage_detail(
                            Stage::Execute,
                            format!("{} rows from the live database", prepared.rows.len()),
                        );
                        Some(source_sql_chat_data(prepared))
                    }
                    Ok(None) => resolve_followup_sql(state.inner(), &body.question, &memory).await,
                    Err(error) => {
                        tracing::warn!(%error, "live SQL query failed; falling back to ingested records");
                        None
                    }
                }
            } else {
                None
            };

            if let Some(data) = sql_data {
                data
            } else {
                generation_permit = Some(state.admission.generation().await?);
                let catalog = state.catalog(); // Arc<Catalog>
                if catalog.collections.is_empty() {
                    // No ingested collection exists for the planner to name, so every
                    // spec it could produce fails validation. Skip the model call.
                    build_semantic_chat_data(
                        foundry,
                        state,
                        &memory,
                        &body.question,
                        mode,
                        rerank,
                        top_k,
                        &trace,
                    )
                    .await?
                } else {
                    match run_structured(
                        &state.db,
                        foundry,
                        &user,
                        state,
                        &catalog,
                        intent,
                        &body.question,
                        &trace,
                    )
                    .await
                    {
                        Ok(structured) => match structured {
                            Structured::Direct { answer } => ChatData::DirectStructured { answer },
                            Structured::Aggregate { narration_stream } => ChatData::Structured {
                                citations_json: "[]".to_string(),
                                narration_stream,
                                passages_for_persist: Vec::new(),
                            },
                            Structured::List {
                                citations_json,
                                narration_stream,
                            } => {
                                let passages = serde_json::from_str(&citations_json).unwrap_or_else(|error| {
                            tracing::warn!(%error, "failed to decode list citations for persistence");
                            Vec::new()
                        });
                                ChatData::Structured {
                                    citations_json,
                                    narration_stream,
                                    passages_for_persist: passages,
                                }
                            }
                        },
                        Err(e) => {
                            tracing::info!(
                                intent = ?intent,
                                error = %e,
                                "run_structured failed; falling back to semantic retrieval"
                            );
                            build_semantic_chat_data(
                                foundry,
                                state,
                                &memory,
                                &body.question,
                                mode,
                                rerank,
                                top_k,
                                &trace,
                            )
                            .await?
                        }
                    }
                }
            }
        }

        crate::router::RouteClass::Hybrid { .. } => {
            generation_permit = Some(state.admission.generation().await?);
            // Phase C (plan 18) will run a structured cohort filter then summarise its
            // records. Until that executor lands, answer hybrids semantically — the
            // grounded path already handles "summarise records matching X" acceptably.
            tracing::debug!("hybrid route answered semantically (Phase C executor pending)");
            build_semantic_chat_data(
                foundry,
                state,
                &memory,
                &body.question,
                mode,
                rerank,
                top_k,
                &trace,
            )
            .await?
        }

        crate::router::RouteClass::Semantic => {
            // Direct patient overviews and anaphoric structured follow-ups can be
            // answered exactly from the live source. Try only those conservative
            // deterministic shapes before paying for semantic retrieval.
            if let Some(data) = resolve_semantic_sql(state.inner(), &body.question, &memory).await {
                data
            } else {
                generation_permit = Some(state.admission.generation().await?);
                build_semantic_chat_data(
                    foundry,
                    state,
                    &memory,
                    &body.question,
                    mode,
                    rerank,
                    top_k,
                    &trace,
                )
                .await?
            }
        }
    };

    // Clone owned, Send data into the generator (EventStream! is 'static — no borrows).
    let db = state.db.clone();
    // Read before the generator opens: it is 'static and cannot borrow `&AppState`.
    // Decides which "no answer" wording is honest — see `rag::no_grounding_message`.
    let store_is_empty = state.catalog().collections.is_empty();
    let persist_target = body.conversation_id.clone();
    let uid = user.id.clone();
    let foundry_handle = state.foundry_handle();
    let cfg = state.config.clone();
    // Resolved on the request so a persisted per-role override is honoured; the
    // generator is 'static and cannot borrow `&AppState`.
    let verify_spec = state.spec_for(crate::foundry::router::AgentKind::Verify);
    let compact_spec = state.spec_for(crate::foundry::router::AgentKind::QueryRewrite);

    Ok(EventStream! {
        use futures::StreamExt;

        let _generation_permit = generation_permit;

        // Announce the routing decision first so the UI can show the active path.
        // Non-breaking: existing clients ignore unknown SSE event names.
        yield Event::data(routed_json).event("routed");

        let mut full_answer = String::new();
        let mut had_error = false;
        let _generation = trace.stage_guard(Stage::Generate);

        match chat_data {
            ChatData::Conversational { mut reply_stream } => {
                // No sources for small talk — emit empty citations to keep the
                // citations -> token* -> done contract, then stream the reply.
                yield Event::data("[]".to_string()).event("citations");

                let mut think = crate::foundry::think_filter::ThinkFilter::new();
                while let Some(chunk) = reply_stream.next().await {
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

                // Persist the exchange with no passages (there are none to cite).
                if let Some(cid) = &persist_target {
                    if !had_error {
                        trace
                            .time(
                                Stage::Persist,
                                persist_and_compact(
                                    &db,
                                    cid.as_str(),
                                    &uid,
                                    &full_answer,
                                    &[],
                                    None,
                                    &cfg,
                                    &foundry_handle,
                                    &compact_spec,
                                    "conversational",
                                ),
                            )
                            .await;
                    }
                }
            }

            ChatData::SourceSql {
                source_id,
                sql,
                explanation,
                columns,
                rows,
                answer,
            } => {
                yield Event::data("[]".to_string()).event("citations");
                yield Event::data(serde_json::json!({
                    "source_id": &source_id,
                    "sql": &sql,
                    "explanation": &explanation,
                }).to_string()).event("sql");
                yield Event::data(serde_json::to_string(&columns).unwrap_or_else(|_| "[]".to_string())).event("columns");
                yield Event::data(serde_json::to_string(&rows).unwrap_or_else(|_| "[]".to_string())).event("rows");

                trace.record_output(&answer);
                full_answer.push_str(&answer);
                yield token_event(&answer);

                if let Some(cid) = &persist_target {
                    if !had_error {
                        use crate::routes::conversations::{SqlResult, persist_sql_assistant_message};
                        let result = SqlResult {
                            source_id: source_id.clone(),
                            sql: sql.clone(),
                            columns: columns.clone(),
                            rows: rows.clone(),
                        };
                        if let Err(error) = trace
                            .time(
                                Stage::Persist,
                                persist_sql_assistant_message(
                                    &db,
                                    cid.as_str(),
                                    &uid,
                                    &full_answer,
                                    &result,
                                ),
                            )
                            .await
                        {
                            tracing::warn!(%error, %source_id, "failed to persist SQL assistant message");
                        }
                    }
                }
            }

            ChatData::DirectStructured { answer } => {
                yield Event::data("[]".to_string()).event("citations");
                trace.record_output(&answer);
                full_answer.push_str(&answer);
                yield token_event(&answer);

                if let Some(cid) = &persist_target {
                    trace
                        .time(
                            Stage::Persist,
                            persist_and_compact(
                                &db,
                                cid.as_str(),
                                &uid,
                                &full_answer,
                                &[],
                                None,
                                &cfg,
                                &foundry_handle,
                                &compact_spec,
                                "direct_structured",
                            ),
                        )
                        .await;
                }
            }

            ChatData::Structured { citations_json, mut narration_stream, passages_for_persist } => {
                // Always emit citations first (SSE contract), then stream narration.
                yield Event::data(citations_json).event("citations");

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

                if let Some(cid) = &persist_target {
                    if !had_error {
                        trace
                            .time(
                                Stage::Persist,
                                persist_and_compact(
                                    &db,
                                    cid.as_str(),
                                    &uid,
                                    &full_answer,
                                    &passages_for_persist,
                                    None,
                                    &cfg,
                                    &foundry_handle,
                                    &compact_spec,
                                    "structured",
                                ),
                            )
                            .await;
                    }
                }
            }

            ChatData::Semantic { citations, stream, passages_for_persist } => {
                // Always send citations first so the UI can render sources immediately.
                yield Event::data(citations).event("citations");

                match stream {
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
                }

                // Post-stream safety pass (plan 25). Runs only once the answer has
                // finished streaming: the user already has every token, so this delays
                // the verdict badge and nothing else.
                let verification = if !had_error && !passages_for_persist.is_empty() {
                    let n = passages_for_persist.len();
                    let invalid: Vec<usize> = extract_citation_refs(&full_answer)
                        .into_iter()
                        .filter(|&r| r > n)
                        .collect();

                    let report = crate::verify::check(
                        foundry_handle.as_deref(),
                        &verify_spec,
                        &cfg,
                        &full_answer,
                        &passages_for_persist,
                    )
                    .await
                    .with_citation_overflow(invalid);

                    if report.is_reportable() {
                        if let Ok(json) = serde_json::to_string(&report) {
                            yield Event::data(json).event("verify");
                        }
                        Some(report)
                    } else {
                        None
                    }
                } else {
                    None
                };

                // Persist the report on this exact assistant message. This remains safe
                // when multiple runs overlap in one conversation because no later
                // "update latest message" lookup is needed.
                if let Some(cid) = &persist_target {
                    if !had_error {
                        trace
                            .time(
                                Stage::Persist,
                                persist_and_compact(
                                    &db,
                                    cid.as_str(),
                                    &uid,
                                    &full_answer,
                                    &passages_for_persist,
                                    verification.as_ref(),
                                    &cfg,
                                    &foundry_handle,
                                    &compact_spec,
                                    "semantic",
                                ),
                            )
                            .await;
                    }
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

/// Wrap a prepared live-SQL result for the SSE generator.
fn source_sql_chat_data(prepared: crate::nl2sql::routes::PreparedNlQuery) -> ChatData {
    ChatData::SourceSql {
        source_id: prepared.source_id,
        sql: prepared.sql,
        explanation: prepared.explanation,
        columns: prepared.columns,
        rows: prepared.rows,
        answer: prepared.answer,
    }
}

/// Ground an anaphoric follow-up against identifiers from working memory and try
/// the deterministic SQL compiler only — a miss returns `None` without any model
/// call, so this is safe to attempt speculatively on every follow-up.
async fn resolve_followup_sql(
    state: &AppState,
    question: &str,
    memory: &WorkingMemory,
) -> Option<ChatData> {
    let augmented = crate::nl2sql::routes::resolve_followup_question(
        question,
        memory.tail.iter().map(|turn| turn.content.as_str()),
    )?;
    match crate::nl2sql::routes::prepare_auto_query_deterministic(state, &augmented).await {
        Ok(Some(prepared)) => {
            tracing::info!(%augmented, "anaphoric follow-up answered via deterministic SQL");
            Some(source_sql_chat_data(prepared))
        }
        Ok(None) => None,
        Err(error) => {
            tracing::debug!(%error, "follow-up SQL resolution failed; continuing normal path");
            None
        }
    }
}

/// Try deterministic SQL for the narrow direct-overview shape, otherwise retain
/// the existing anaphoric follow-up behavior. Generic semantic questions never
/// incur source linking or SQL work.
async fn resolve_semantic_sql(
    state: &AppState,
    question: &str,
    memory: &WorkingMemory,
) -> Option<ChatData> {
    let candidate = semantic_sql_candidate(
        question,
        memory.tail.iter().map(|turn| turn.content.as_str()),
    )?;
    match crate::nl2sql::routes::prepare_auto_query_deterministic(state, &candidate).await {
        Ok(Some(prepared)) => {
            tracing::info!(%candidate, "semantic question answered via deterministic SQL");
            Some(source_sql_chat_data(prepared))
        }
        Ok(None) => None,
        Err(error) => {
            tracing::debug!(%error, "semantic deterministic SQL failed; continuing retrieval");
            None
        }
    }
}

fn semantic_sql_candidate<'turn>(
    question: &str,
    prior_turns: impl Iterator<Item = &'turn str>,
) -> Option<String> {
    if crate::nl2sql::routes::is_patient_overview_question(question) {
        Some(question.to_string())
    } else {
        crate::nl2sql::routes::resolve_followup_question(question, prior_turns)
    }
}

/// Build `ChatData::Semantic` by running the full retrieval + generation pipeline.
/// Extracted so both the direct semantic branch and the structured fall-through path
/// can share the code without repeating it inline.
async fn build_semantic_chat_data(
    foundry: &crate::foundry::FoundryManager,
    state: &AppState,
    memory: &WorkingMemory,
    question: &str,
    mode: RetrievalMode,
    rerank: bool,
    top_k: usize,
    trace: &RequestTrace,
) -> AppResult<ChatData> {
    let rewrite_turns = memory.rewrite_turns();
    let (standalone, queries) = trace
        .time(
            Stage::RewriteExpand,
            prepare_queries_with(
                foundry,
                &state.spec_for(crate::foundry::router::AgentKind::QueryRewrite),
                &state.config,
                &rewrite_turns,
                question,
            ),
        )
        .await;
    let broad = retrieval::is_broad_question(&standalone);
    let _retrieval_permit = state.admission.retrieval().await?;
    let passages = retrieval::retrieve_observed(
        &state.db,
        &state.config,
        &queries,
        mode,
        rerank && !broad,
        top_k,
        trace,
    )
    .await?;

    // Broad questions use fused RRF ordering and only refuse when retrieval is empty;
    // pointed questions retain the reranker-score anti-hallucination floor.
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
    let citations = serde_json::to_string(&passages).unwrap_or_else(|_| "[]".to_string());

    // Open the generation stream only when we intend to answer; opening here (not in
    // the generator) keeps a setup failure an HTTP error.
    let stream = if refuse {
        None
    } else {
        let prompt = trace.time_sync(Stage::Prompt, || {
            build_prompt(&passages, memory, &standalone)
        });
        Some(
            trace
                .time(
                    Stage::Generate,
                    foundry.generate_stream_with(
                        &state.spec_for(crate::foundry::router::AgentKind::Chat),
                        SYSTEM_PROMPT,
                        &prompt,
                    ),
                )
                .await?,
        )
    };
    let passages_for_persist = passages;
    Ok(ChatData::Semantic {
        citations,
        stream,
        passages_for_persist,
    })
}

/// Clip, persist, and (on success) trigger a write-behind compaction check.
/// Shared by all three `ChatData` branches inside the SSE generator below — the
/// only thing that differs between them is which passages back the answer and
/// what to call it in the failure log.
async fn persist_and_compact(
    db: &DocumentDb,
    cid: &str,
    uid: &str,
    full_answer: &str,
    passages: &[Passage],
    verification: Option<&crate::verify::VerifyReport>,
    cfg: &Config,
    foundry_handle: &Option<Arc<FoundryManager>>,
    compact_spec: &crate::foundry::router::ModelSpec,
    kind: &str,
) {
    use crate::routes::conversations::{clip_message_bytes, persist_assistant_message};
    let content = clip_message_bytes(full_answer, cfg.message_max_bytes);
    match persist_assistant_message(db, cid, uid, &content, passages, verification).await {
        Ok(()) => {
            crate::memory::maybe_spawn_compaction(
                db.clone(),
                foundry_handle.clone(),
                cfg.clone(),
                compact_spec.clone(),
                cid.to_string(),
            );
        }
        Err(e) => tracing::warn!(error = %e, kind, "failed to persist assistant message"),
    }
}

/// Encode a streamed token as an SSE `token` event. JSON-encoding protects the
/// token's leading/trailing spaces, which the SSE spec would otherwise strip from a
/// bare `data:` field — fusing words together on the client.
fn token_event(text: &str) -> Event {
    Event::data(serde_json::to_string(text).unwrap_or_else(|_| "\"\"".to_string())).event("token")
}

#[cfg(test)]
mod tests {
    use super::semantic_sql_candidate;

    #[test]
    fn semantic_precheck_accepts_only_deterministic_patient_shapes() {
        for question in [
            "Tell me about Jane Chebet.",
            "Who is Jane Chebet?",
            "Give me an overview of Jane Chebet",
            "Tell me about patient SYN-2024-0001",
        ] {
            assert_eq!(
                semantic_sql_candidate(question, std::iter::empty()).as_deref(),
                Some(question),
                "question: {question}"
            );
        }

        for question in ["Tell me about asthma", "Tell me about the hospital"] {
            assert!(
                semantic_sql_candidate(question, std::iter::empty()).is_none(),
                "question unexpectedly reached SQL: {question}"
            );
        }
    }

    #[test]
    fn semantic_precheck_retains_anaphoric_followup_resolution() {
        let turns = ["Reviewed patient SYN-2024-0001."];
        assert_eq!(
            semantic_sql_candidate("What is the patient's name?", turns.iter().copied()).as_deref(),
            Some("What is the patient's name? patient SYN-2024-0001")
        );
    }
}

/// Extract every unique 1-based citation index from `[N]` patterns in `text`.
/// Returns a sorted, deduplicated list. Used for the post-stream overflow check.
fn extract_citation_refs(text: &str) -> Vec<usize> {
    let mut refs = Vec::new();
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        if c != '[' {
            continue;
        }
        let mut digits = String::new();
        loop {
            match chars.peek() {
                Some(&d) if d.is_ascii_digit() => {
                    digits.push(d);
                    chars.next();
                }
                Some(&']') => {
                    chars.next();
                    break;
                }
                _ => {
                    digits.clear();
                    break;
                }
            }
        }
        if let Ok(n) = digits.parse::<usize>() {
            if n > 0 {
                refs.push(n);
            }
        }
    }
    refs.sort_unstable();
    refs.dedup();
    refs
}
