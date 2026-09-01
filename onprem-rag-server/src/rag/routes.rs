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

use super::{ChatTurn, build_prompt, prepare_queries, rewrite_query, SYSTEM_PROMPT};
use crate::answer::{Structured, run_structured};
use crate::auth::guard::AuthUser;
use crate::config::{Config, RetrievalMode};
use crate::documentdb::DocumentDb;
use crate::error::AppResult;
use crate::foundry::{FoundryManager, GuardedChatStream};
use crate::memory::{load_working_memory, WorkingMemory};
use crate::retrieval::{self, Passage};
use crate::state::AppState;

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
    let mode = match opts.mode.as_deref().map(str::trim).map(str::to_ascii_lowercase).as_deref() {
        Some("vector") => RetrievalMode::Vector,
        Some("hybrid") => RetrievalMode::Hybrid,
        _ => state.config.retrieval_mode,
    };
    let rerank = opts.rerank.unwrap_or(state.config.rerank_enabled);
    let top_k = opts.top_k.unwrap_or(state.config.context_top_k);
    (mode, rerank, top_k)
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
    let (mode, rerank, top_k) = resolve(state, &body.opts);
    let foundry = state.foundry()?;

    let (standalone, queries) =
        prepare_queries(foundry, &state.config, &body.history, &body.query).await;
    let passages =
        retrieval::retrieve(&state.db, &state.config, &queries, mode, rerank, top_k).await?;

    Ok(Json(SearchResponse { query: standalone, queries, passages }))
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
    Conversational {
        reply_stream: GuardedChatStream,
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
    let (mode, rerank, top_k) = resolve(state, &body.opts);
    let foundry = state.foundry()?;

    // Conversation persistence (pre-stream, so errors stay clean HTTP errors).
    // When conversation_id is absent, fall back to body.history (stateless path).
    let memory: WorkingMemory;
    if let Some(cid) = body.conversation_id.as_deref() {
        use crate::routes::conversations::{clip_message_bytes, persist_user_message, verify_owned};
        verify_owned(&state.db, cid, &user.id).await?;
        memory = load_working_memory(&state.db, cid, &state.config).await;
        let content = clip_message_bytes(&body.question, state.config.message_max_bytes);
        persist_user_message(&state.db, cid, &user.id, &content).await?;
    } else {
        memory = WorkingMemory::from_turns(body.history.clone());
    }

    // Intent Router v2: Tier 0 conversational gate -> Tier 1 lexical -> Tier 2 model
    // classify, fail-open to semantic. Structured routes (Aggregation/Trend/Enumeration)
    // go through plan->validate->execute->narrate; conversational replies skip retrieval;
    // everything else (including hybrid, until its Phase-C executor lands) is semantic.
    // Any structured failure falls back silently to semantic — a correct grounded answer
    // beats an error. See plans/17-intent-router-v2.md.
    let has_history = !memory.is_empty();
    let decision = crate::router::route(
        &body.question,
        has_history,
        &state.config,
        Some(foundry),
        &state.router_cache,
    )
    .await;
    let routed_json = decision.to_sse_json();

    let chat_data: ChatData = match &decision.class {
        crate::router::RouteClass::Conversational => {
            // Cheap, warm, retrieval-free reply. Honour any persisted "chat" role
            // override, but nudge temperature up and cap the length — this is small talk.
            let mut spec = state.spec_for(crate::foundry::router::AgentKind::Chat);
            spec.tools = false;
            spec.temperature = 0.4;
            spec.max_tokens = Some(160);
            let reply_stream = foundry
                .generate_stream_with(
                    &spec,
                    crate::router::CONVERSATIONAL_SYSTEM_PROMPT,
                    &body.question,
                )
                .await?;
            ChatData::Conversational { reply_stream }
        }

        crate::router::RouteClass::ConversationMeta => {
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
                memory.tail.iter().map(|t| format!("{}: {}", t.role, t.content)).collect::<Vec<_>>().join("\n"),
                body.question,
            );
            let reply_stream = foundry.generate_stream_with(&spec, system, &user).await?;
            ChatData::Conversational { reply_stream }
        }

        crate::router::RouteClass::Structured { intent, .. } => {
            let intent = *intent;
            let catalog = state.catalog(); // Arc<Catalog>
            // Rewrite against working memory so follow-up aggregations ("and for
            // females?") plan against a complete question, not just the fragment
            // (plan 22.3). No-op (no model call) when there's no history.
            let standalone = rewrite_query(foundry, &memory.rewrite_turns(), &body.question).await;
            match run_structured(&state.db, foundry, &user, state, &catalog, intent, &standalone).await {
                Ok(structured) => {
                    // Flatten the Structured enum into the simpler ChatData shape.
                    // The /chat SSE contract is citations then token* then done —
                    // we never emit spec/rows/pipeline events here (those are agents-only).
                    let (citations_json, narration_stream) = match structured {
                        Structured::Aggregate { narration_stream } => {
                            // No passages to cite for aggregations; the numbers are the source.
                            ("[]".to_string(), narration_stream)
                        }
                        Structured::List { citations_json, narration_stream } => {
                            (citations_json, narration_stream)
                        }
                    };
                    ChatData::Structured {
                        citations_json,
                        narration_stream,
                        passages_for_persist: vec![],
                    }
                }
                Err(e) => {
                    tracing::info!(
                        intent = ?intent,
                        error = %e,
                        "run_structured failed; falling back to semantic retrieval"
                    );
                    build_semantic_chat_data(foundry, state, &memory, &body.question, mode, rerank, top_k)
                        .await?
                }
            }
        }

        crate::router::RouteClass::Hybrid { .. } => {
            // Phase C (plan 18) will run a structured cohort filter then summarise its
            // records. Until that executor lands, answer hybrids semantically — the
            // grounded path already handles "summarise records matching X" acceptably.
            tracing::debug!("hybrid route answered semantically (Phase C executor pending)");
            build_semantic_chat_data(foundry, state, &memory, &body.question, mode, rerank, top_k).await?
        }

        crate::router::RouteClass::Semantic => {
            build_semantic_chat_data(foundry, state, &memory, &body.question, mode, rerank, top_k).await?
        }
    };

    // Clone owned, Send data into the generator (EventStream! is 'static — no borrows).
    let db = state.db.clone();
    let persist_target = body.conversation_id.clone();
    let uid = user.id.clone();
    let foundry_handle = state.foundry_handle();
    let cfg = state.config.clone();
    // Resolved on the request so a persisted per-role override is honoured; the
    // generator is 'static and cannot borrow `&AppState`.
    let verify_spec = state.spec_for(crate::foundry::router::AgentKind::Verify);

    Ok(EventStream! {
        use futures::StreamExt;

        // Announce the routing decision first so the UI can show the active path.
        // Non-breaking: existing clients ignore unknown SSE event names.
        yield Event::data(routed_json).event("routed");

        let mut full_answer = String::new();
        let mut had_error = false;

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
                        full_answer.push_str(&tail);
                        yield token_event(&tail);
                    }
                }

                // Persist the exchange with no passages (there are none to cite).
                if let Some(cid) = &persist_target {
                    if !had_error {
                        persist_and_compact(
                            &db, cid, &uid, &full_answer, &[], &cfg, &foundry_handle, "conversational",
                        )
                        .await;
                    }
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
                        full_answer.push_str(&tail);
                        yield token_event(&tail);
                    }
                }

                if let Some(cid) = &persist_target {
                    if !had_error {
                        persist_and_compact(
                            &db, cid, &uid, &full_answer, &passages_for_persist, &cfg, &foundry_handle, "structured",
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
                        let msg = "I don't have relevant records to answer that question.";
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
                                full_answer.push_str(&tail);
                                yield token_event(&tail);
                            }
                        }
                    }
                }

                // Persist the assistant reply if a conversation is present and no error.
                // A persistence failure must not crash the stream — log and continue.
                if let Some(cid) = &persist_target {
                    if !had_error {
                        persist_and_compact(
                            &db, cid, &uid, &full_answer, &passages_for_persist, &cfg, &foundry_handle, "semantic",
                        )
                        .await;
                    }
                }

                // Post-stream safety pass (plan 25). Runs only once the answer has
                // finished streaming: the user already has every token, so this delays
                // the verdict badge and nothing else.
                //
                // Two independent checks land in one `verify` event:
                //   - the deterministic marker check ([N] pointing past the citation
                //     list), which costs nothing and always runs;
                //   - the model-backed faithfulness check, which is gated on
                //     ONPREM_VERIFY_ENABLED and falls open to `skipped`.
                if !had_error && !passages_for_persist.is_empty() {
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
                    }
                }
            }
        }

        yield Event::data("").event("done");
    })
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
) -> AppResult<ChatData> {
    let (standalone, queries) =
        prepare_queries(foundry, &state.config, &memory.rewrite_turns(), question).await;
    let passages =
        retrieval::retrieve(&state.db, &state.config, &queries, mode, rerank, top_k).await?;

    // Anti-hallucination gate: refuse when there's nothing relevant, or (if a floor
    // is configured) when the best passage scores below it. PHI — better to decline.
    let refuse = passages.is_empty()
        || state.config.score_gate.is_some_and(|floor| passages[0].score < floor);

    let citations = serde_json::to_string(&passages).unwrap_or_else(|_| "[]".to_string());

    // Open the generation stream only when we intend to answer; opening here (not in
    // the generator) keeps a setup failure an HTTP error.
    let stream = if refuse {
        None
    } else {
        let prompt = build_prompt(&passages, &standalone, memory);
        Some(foundry.generate_stream(SYSTEM_PROMPT, &prompt).await?)
    };
    let passages_for_persist = passages;
    Ok(ChatData::Semantic { citations, stream, passages_for_persist })
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
    cfg: &Config,
    foundry_handle: &Option<Arc<FoundryManager>>,
    kind: &str,
) {
    use crate::routes::conversations::{clip_message_bytes, persist_assistant_message};
    let content = clip_message_bytes(full_answer, cfg.message_max_bytes);
    match persist_assistant_message(db, cid, uid, &content, passages).await {
        Ok(()) => {
            crate::memory::maybe_spawn_compaction(db.clone(), foundry_handle.clone(), cfg.clone(), cid.to_string());
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
