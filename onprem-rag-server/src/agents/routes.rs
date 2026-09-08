//! `POST /agents/<kind>` — SSE endpoint for the hospital agent roster.
//!
//! One shared flow serves every agent (plan 05 §2). The old file branched per
//! mechanism-named kind (`health_query`, `trends`, `patient_lookup`,
//! `summarize`); those branches are gone. What an agent *is* now only decides
//! two things: the read scope handed to the four ownership hooks, and the
//! persona the narrator is given.
//!
//! ```text
//! parse kind + mode  →  load memory + focus
//!                    →  capability answer (no model) | out-of-scope redirect
//!                    →  otherwise route (fixed_line = kind.line(), mode)
//!                    →  execute: deterministic SQL | DocDb aggregation | retrieval
//!                    →  narrate with the binding-derived persona
//!                    →  persist (agent_kind = slug, mode)
//! ```
//!
//! **Scope is a read allow-list, not a security boundary.** It decides what an
//! agent may look at. Whether *this user* may do anything at all is decided in
//! `src/auth/` (the `AuthUser` guard on this route) and nowhere else.
//!
//! Every SQL statement still passes the single `nl2sql::validate::validate_sql`
//! guard; scoping narrows the `allowed_tables` that guard is given rather than
//! adding a second check.
//!
//! # Event contract (SSE event names -> JSON payload shapes)
//!
//! | Event      | Payload                                          | When                         |
//! |------------|--------------------------------------------------|------------------------------|
//! | `routed`   | Route decision JSON (`RouteDecision::to_sse_json`)| First event, every path      |
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

use crate::agents::kind::{AgentKind, AgentMode};
use crate::agents::persona;
use crate::aggregation::execute;
use crate::aggregation::intent::QueryIntent;
use crate::aggregation::validate;
use crate::answer::{build_agg_planner_system, build_narration_user};
use crate::auth::guard::AuthUser;
use crate::error::{AppError, AppResult};
use crate::foundry::GuardedChatStream;
use crate::foundry::router::ModelRole;
use crate::memory::WorkingMemory;
use crate::ontology::binding::SchemaBinding;
use crate::rag::{apply_context_budget, build_prompt, expand_queries_with, prepare_queries_with};
use crate::retrieval::{self, RetrievalFilter};
use crate::router::focus::ConversationFocus;
use crate::router::{RouteClass, RouteDecision};
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
    /// loads history from the DB. Absent -> stateless (eval-safe).
    #[serde(default)]
    pub conversation_id: Option<String>,
    /// Client-minted id for this run; when present every pipeline stage is
    /// published to `GET /runs/<run_id>/progress` for the activity strip.
    #[serde(default)]
    pub run_id: Option<String>,
    /// Trends / Handover toggles from the UI. Defaults to `Ask`.
    #[serde(default)]
    pub mode: AgentMode,
    /// Optional source pin when several sources are bound.
    #[serde(default)]
    pub source_id: Option<String>,
}

// ---------------------------------------------------------------------------
// Pre-computed data that flows into the single EventStream! generator
// ---------------------------------------------------------------------------

/// All the data prepared before the SSE generator opens. Keeping async work out
/// of the generator means setup errors become HTTP errors (not mid-stream events)
/// and we avoid smuggling non-`Send` temporaries into the async generator.
enum AgentData {
    /// A complete, model-free answer (capability, redirect, clarify,
    /// deterministic record narration).
    Direct { answer: String },
    /// Deterministic live-SQL result rendered in the structured shape.
    StructuredDirect {
        spec_json: String,
        rows_json: String,
        pipeline_json: String,
        answer: String,
    },
    /// DocumentDB aggregation with a streamed narration.
    Structured {
        spec_json: String,
        rows_json: String,
        pipeline_json: String,
        narration_stream: GuardedChatStream,
    },
    /// Retrieval + grounded generation.
    Semantic {
        citations_json: String,
        /// `None` when the score gate refuses to answer.
        gen_stream: Option<GuardedChatStream>,
    },
}

// ---------------------------------------------------------------------------
// Route
// ---------------------------------------------------------------------------

/// `POST /agents/<kind>` — the hospital agent endpoint.
///
/// `<kind>` is `ask`, any service-line slug (`maternity`, `ward_board`, …), or —
/// for one release — a legacy name (`health_query`, `trends`, `patient_lookup`,
/// `summarize`, `chat`) which maps to `Ask` with a mode. Unknown kinds are a
/// `400`, never a silently substituted neighbour.
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

    let (agent_kind, legacy_mode) = parse_kind(kind)?;
    let req = body.into_inner();
    // An explicit body mode wins; a legacy path name supplies one when the body
    // has none (old clients never send `mode`).
    let mode = if req.mode == AgentMode::Ask {
        legacy_mode
    } else {
        req.mode
    };

    let kind_slug = agent_kind.slug().to_string();
    trace.set_route(format!("agent_{kind_slug}"), None, None);

    // Binding for this turn: the pinned source if one was given, else the
    // richest available. Personas, scope and redirects all read from it.
    let bindings = state.bindings();
    let binding: Option<std::sync::Arc<SchemaBinding>> = match req.source_id.as_deref() {
        Some(pin) => bindings.get(pin).cloned(),
        None => bindings
            .values()
            .max_by_key(|b| b.usable_lines(state.config.binding_min_confidence).len())
            .cloned(),
    };

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
                Ok::<Vec<crate::rag::ChatTurn>, AppError>(history)
            } else {
                Ok::<Vec<crate::rag::ChatTurn>, AppError>(Vec::new())
            }
        })
        .await?;
    let memory = WorkingMemory::from_turns(history.clone());

    // Focus is populated by plan 06; the line comes from the tab so the persona
    // and the router agree about which department is answering.
    let focus = ConversationFocus {
        line: agent_kind.line(),
        ..ConversationFocus::default()
    };

    let catalog = state.catalog();

    // -----------------------------------------------------------------
    // Two model-free short circuits, then routing.
    //
    // 1. A capability question is answered from the binding, so it is truthful
    //    about *this* deployment and costs no model call.
    // 2. A question that plainly names another department's data is redirected
    //    by name. Ownership here decides only what this agent *reads*; whether
    //    the user may read anything at all was settled by the `AuthUser` guard.
    // -----------------------------------------------------------------
    let (decision, direct_answer): (RouteDecision, Option<String>) =
        if crate::router::conversational::is_capability(&req.question) {
            (
                capability_decision(agent_kind, &req.question),
                Some(persona::capability_answer(agent_kind, binding.as_deref())),
            )
        } else if let Some(redirect) = crate::agents::kind::out_of_scope_redirect(
            agent_kind,
            &req.question,
            binding.as_deref(),
            crate::agents::kind::scope_widening_key(&req.question).as_deref(),
        ) {
            let message = redirect.message.clone();
            (
                redirect_decision(agent_kind, &req.question, &redirect),
                Some(message),
            )
        } else {
            // Route once, for every agent, with the tab's line fixed.
            let decision = trace
                .time(
                    Stage::Route,
                    crate::router::route_dispatch(
                        &req.question,
                        !history.is_empty(),
                        &state.config,
                        state.foundry().ok(),
                        &state.spec_for(ModelRole::Classify),
                        &state.router_cache,
                        &bindings,
                        &catalog,
                        Some(&focus),
                        agent_kind.line(),
                        mode,
                    ),
                )
                .await;
            trace.set_route(
                decision.route_label(),
                Some(decision.tier),
                Some(decision.cached),
            );
            (decision, None)
        };

    // The agent's read allow-list. A line agent narrows to its own tables; Ask
    // takes whatever the router resolved (possibly nothing = whole corpus).
    let scope: Vec<String> = {
        let own = agent_kind.scope(binding.as_deref(), decision.entities.patient_key.as_deref());
        if own.is_empty() { decision.scope.clone() } else { own }
    };

    let system_prompt = persona::system_prompt(agent_kind, mode, binding.as_deref(), &focus);

    let mut generation_permit = None;
    let mut persist_passages: Vec<crate::retrieval::Passage> = Vec::new();
    let mut persist_structured_json: Option<String> = None;

    let data: AgentData = match direct_answer {
        Some(answer) => AgentData::Direct { answer },
        None => match &decision.class {
        // Model-free classes: the answer text is already decided.
        RouteClass::Capability => AgentData::Direct {
            answer: persona::capability_answer(agent_kind, binding.as_deref()),
        },
        RouteClass::Clarify { question, .. } => AgentData::Direct {
            answer: question.clone(),
        },

        // Structured: live SQL first, DocumentDB aggregation second, retrieval last.
        RouteClass::Structured { intent, .. } => {
            let source_scope = crate::nl2sql::routes::SourceScope {
                tables: scope.clone(),
                pin_source: req
                    .source_id
                    .clone()
                    .or_else(|| decision.source_id.clone()),
            };
            let deterministic = trace
                .time(
                    Stage::Execute,
                    crate::nl2sql::routes::prepare_auto_query_deterministic(
                        state.inner(),
                        &decision.resolved_question,
                        Some(&source_scope),
                    ),
                )
                .await
                .unwrap_or_else(|error| {
                    tracing::warn!(%error, "agent deterministic SQL failed; using aggregation planner");
                    None
                });

            // Cheap, model-free second chance for pronoun follow-ups.
            let deterministic = match deterministic {
                Some(prepared) => Some(prepared),
                None => {
                    resolve_followup_sql(state.inner(), &req.question, &memory, &source_scope).await
                }
            };

            match deterministic {
                Some(prepared) => {
                    trace.stage_detail(
                        Stage::Execute,
                        format!("{} rows from the live database", prepared.rows.len()),
                    );
                    let (spec_json, rows_json, pipeline_json, answer) =
                        deterministic_structured_result(prepared, mode);
                    persist_structured_json = Some(format!(
                        "{{\"spec\":{spec_json},\"rows\":{rows_json},\"pipeline\":{pipeline_json}}}"
                    ));
                    AgentData::StructuredDirect {
                        spec_json,
                        rows_json,
                        pipeline_json,
                        answer,
                    }
                }
                None => {
                    // Hook 2: the planner prompt AND the validator see the same
                    // scoped catalog, so an out-of-scope collection is rejected
                    // by the existing guard.
                    let scoped = catalog.scoped(&scope);
                    let aggregated = if scoped.collections.is_empty() {
                        // Nothing in scope is ingested; a planner call here would
                        // buy a guaranteed validation error instead of an answer.
                        None
                    } else {
                        match run_docdb_aggregation(
                            state.inner(),
                            &user,
                            *intent,
                            mode,
                            &decision.resolved_question,
                            &scoped,
                            &system_prompt,
                            &trace,
                            &mut generation_permit,
                        )
                        .await
                        {
                            Ok((data, structured_json)) => {
                                persist_structured_json = Some(structured_json);
                                Some(data)
                            }
                            Err(error) => {
                                tracing::info!(
                                    kind = %kind_slug,
                                    %error,
                                    "agent aggregation failed; falling back to semantic retrieval"
                                );
                                None
                            }
                        }
                    };
                    match aggregated {
                        Some(data) => data,
                        None => {
                            build_semantic_agent_data(
                                state.inner(),
                                agent_kind,
                                &memory,
                                &decision,
                                &scope,
                                &system_prompt,
                                &trace,
                                &mut generation_permit,
                                &mut persist_passages,
                            )
                            .await?
                        }
                    }
                }
            }
        }

        // Everything else answers from retrieval with the persona and the filter.
        _ => {
            build_semantic_agent_data(
                state.inner(),
                agent_kind,
                &memory,
                &decision,
                &scope,
                &system_prompt,
                &trace,
                &mut generation_permit,
                &mut persist_passages,
            )
            .await?
        }
        },
    };

    let db = state.db.clone();
    // Read before the generator opens (it cannot borrow `&AppState`); picks the
    // honest "no answer" wording — see `rag::no_grounding_message`.
    let store_is_empty = catalog.collections.is_empty();
    let uid = user.id.clone();
    let persist_cid = req.conversation_id.clone();
    let foundry_handle = state.foundry_handle();
    let cfg = state.config.clone();
    let compact_spec = state.spec_for(ModelRole::Compact);
    let routed_json = decision.to_sse_json();
    let mode_slug = mode.slug().to_string();
    // Plan 05 section 7: an Ask thread that the router sent to a department is
    // titled with that department. A fixed tab already says which one it is.
    let title_line: Option<&'static str> = match agent_kind {
        AgentKind::Ask => decision.service_line.map(|l| l.label()),
        AgentKind::Line(_) => None,
    };

    Ok(EventStream! {
        use futures::StreamExt;

        let _generation_permit = generation_permit;

        yield Event::data(routed_json).event("routed");

        let mut full_answer = String::new();
        let mut had_error = false;
        let _generation = trace.stage_guard(Stage::Generate);

        match data {
            AgentData::Direct { answer } => {
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
                            &kind_slug,
                            &mode_slug,
                            &persist_passages,
                            persist_structured_json.as_deref(),
                        ),
                    )
                    .await
                {
                    tracing::warn!(error = %e, "failed to persist agent assistant message");
                } else {
                    if let Some(label) = title_line {
                        if let Err(e) = crate::routes::conversations::prefix_title_with_line(
                            &db, cid.as_str(), label,
                        )
                        .await
                        {
                            tracing::debug!(error = %e, "could not prefix conversation title");
                        }
                    }
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

// ---------------------------------------------------------------------------
// Model-free answers
// ---------------------------------------------------------------------------

/// A Tier-0 decision for a capability answer, so the UI's activity strip shows
/// the same shape it does for every other route.
fn capability_decision(kind: AgentKind, question: &str) -> RouteDecision {
    RouteDecision {
        class: RouteClass::Capability,
        tier: 0,
        cached: false,
        tier2_attempted: false,
        entities: Default::default(),
        deterministic: false,
        service_line: kind.line(),
        source_id: None,
        scope: vec![],
        query_spec: None,
        resolved_question: question.to_string(),
    }
}

/// A Tier-0 clarify decision naming the department that owns the data.
fn redirect_decision(
    kind: AgentKind,
    question: &str,
    redirect: &crate::agents::kind::ScopeRedirect,
) -> RouteDecision {
    RouteDecision {
        class: RouteClass::Clarify {
            question: redirect.message.clone(),
            slot: crate::nl2sql::ir::spec::MissingSlot::Subject,
        },
        tier: 0,
        cached: false,
        tier2_attempted: false,
        entities: Default::default(),
        deterministic: false,
        service_line: kind.line(),
        source_id: None,
        scope: vec![redirect.table.clone()],
        query_spec: None,
        resolved_question: question.to_string(),
    }
}

// ---------------------------------------------------------------------------
// Structured execution
// ---------------------------------------------------------------------------

/// Plan → validate → execute a DocumentDB aggregation and open its narration
/// stream. Every failure inside is one `Err` the caller can trade for a semantic
/// answer rather than aborting the request mid-conversation.
///
/// `catalog` is already scoped to the agent's allow-list, and it is the same
/// catalog handed to `validate` — narrowing what the planner may name and what
/// the validator will accept in one move, through the existing guard.
#[allow(clippy::too_many_arguments)]
async fn run_docdb_aggregation(
    state: &AppState,
    user: &AuthUser,
    intent: QueryIntent,
    mode: AgentMode,
    question: &str,
    catalog: &crate::aggregation::catalog::Catalog,
    system_prompt: &str,
    trace: &RequestTrace,
    generation_permit: &mut Option<tokio::sync::OwnedSemaphorePermit>,
) -> AppResult<(AgentData, String)> {
    let foundry = state.foundry()?;
    *generation_permit = Some(state.admission.generation().await?);

    // Trends mode is a trend question whatever the lexical intent said.
    let intent = if mode == AgentMode::Trends {
        QueryIntent::Trend
    } else {
        intent
    };
    let planner_system = build_agg_planner_system(catalog, intent);
    let planner_spec = state.spec_for(ModelRole::PlanSpec);

    // 1. Plan: model emits a RunAggregation tool call.
    let planned = trace
        .time(
            Stage::Plan,
            foundry.plan_aggregation(&planner_spec, &planner_system, question),
        )
        .await?;

    // 2. Validate + sanitize against the scoped catalog.
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

    // 4. Open the grounded narration stream, using the agent's persona.
    let narration_user = build_narration_user(question, &rows);
    let mut narration_spec = state.spec_for(ModelRole::Narrate);
    narration_spec.tools = false; // narration never calls tools
    // Trends is the one place chain-of-thought earns its cost: describing a
    // direction over buckets is reasoning, not transcription.
    narration_spec.thinking = mode == AgentMode::Trends;
    let narration_stream = trace
        .time(
            Stage::Narrate,
            foundry.generate_stream_with(&narration_spec, system_prompt, &narration_user),
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
/// prior turn and try the deterministic SQL compiler only. Model-free, so it is
/// safe to attempt on every follow-up.
async fn resolve_followup_sql(
    state: &AppState,
    question: &str,
    memory: &WorkingMemory,
    scope: &crate::nl2sql::routes::SourceScope,
) -> Option<crate::nl2sql::routes::PreparedNlQuery> {
    let augmented = crate::nl2sql::routes::resolve_followup_question(
        question,
        memory.tail.iter().map(|turn| turn.content.as_str()),
    )?;
    match crate::nl2sql::routes::prepare_auto_query_deterministic(state, &augmented, Some(scope))
        .await
    {
        Ok(prepared) => prepared,
        Err(error) => {
            tracing::debug!(%error, "agent follow-up SQL resolution failed; continuing");
            None
        }
    }
}

// ---------------------------------------------------------------------------
// Semantic execution
// ---------------------------------------------------------------------------

#[allow(clippy::too_many_arguments)]
async fn build_semantic_agent_data(
    state: &AppState,
    kind: AgentKind,
    memory: &WorkingMemory,
    decision: &RouteDecision,
    scope: &[String],
    system_prompt: &str,
    trace: &RequestTrace,
    generation_permit: &mut Option<tokio::sync::OwnedSemaphorePermit>,
    persist_passages: &mut Vec<crate::retrieval::Passage>,
) -> AppResult<AgentData> {
    // Release any permit the caller already holds (structured path that failed
    // and fell through) before asking for another — assigning would evaluate the
    // new acquire while the old permit is alive, deadlocking a single-permit
    // semaphore.
    generation_permit.take();
    *generation_permit = Some(state.admission.generation().await?);
    let foundry = state.foundry()?;
    let generation_spec = state.spec_for(ModelRole::Grounded);
    let rewrite_spec = state.spec_for(ModelRole::Rewrite);

    // The router already resolved anaphora; expanding that form avoids a second
    // rewrite model call on the same question.
    let standalone = decision.resolved_question.clone();
    let queries = trace
        .time(
            Stage::RewriteExpand,
            expand_queries_with(foundry, &rewrite_spec, &state.config, &standalone),
        )
        .await;
    let queries = if queries.is_empty() {
        trace
            .time(
                Stage::RewriteExpand,
                prepare_queries_with(
                    foundry,
                    &rewrite_spec,
                    &state.config,
                    &memory.rewrite_turns(),
                    &standalone,
                ),
            )
            .await
            .1
    } else {
        queries
    };

    let broad = retrieval::is_broad_question(&standalone);
    let _retrieval_permit = state.admission.retrieval().await?;

    // Hook 3: the agent's read allow-list, applied to retrieval candidates.
    // `explicit` is false for Ask, which reads the whole corpus by design.
    let mut filter = RetrievalFilter::for_scope(scope.to_vec(), kind.scope_is_explicit());
    filter.patient_key = decision.entities.patient_key.clone();

    let passages = retrieval::retrieve_observed_filtered(
        &state.db,
        &state.config,
        &queries,
        state.config.retrieval_mode,
        state.config.rerank_enabled && !broad,
        state.config.context_top_k,
        trace,
        &filter,
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
                    foundry.generate_stream_with(&generation_spec, system_prompt, &prompt),
                )
                .await?,
        )
    };
    Ok(AgentData::Semantic {
        citations_json,
        gen_stream,
    })
}

// ---------------------------------------------------------------------------
// Live-SQL result adaptation
// ---------------------------------------------------------------------------

/// Adapt a validated live-SQL result to the structured-agent SSE shape. Scalar
/// results become one `(all)` bar; grouped/trend results use the first column as
/// the label and the rightmost numeric column as the value.
fn deterministic_structured_result(
    prepared: crate::nl2sql::routes::PreparedNlQuery,
    mode: AgentMode,
) -> (String, String, String, String) {
    let rows = sql_rows_to_agg_rows(&prepared.rows);
    let time_bucket = if mode == AgentMode::Trends {
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

/// Parse the URL path segment into `(AgentKind, AgentMode)`. Unknown values are
/// a `400` — never resolved to whichever agent happens to be first in a list.
fn parse_kind(s: &str) -> AppResult<(AgentKind, AgentMode)> {
    AgentKind::parse(s).ok_or_else(|| {
        let valid = AgentKind::all()
            .iter()
            .map(|k| k.slug())
            .collect::<Vec<_>>()
            .join(", ");
        AppError::BadRequest(format!("unknown agent kind '{s}'; valid: {valid}"))
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
    use super::*;
    use crate::ontology::service_line::ServiceLine;

    #[test]
    fn every_agent_slug_parses_and_unknown_ones_are_rejected() {
        for kind in AgentKind::all() {
            let (parsed, _) = parse_kind(kind.slug()).expect("roster slug must parse");
            assert_eq!(parsed, kind);
        }
        assert!(parse_kind("not_a_department").is_err());
    }

    #[test]
    fn legacy_paths_still_resolve_to_ask() {
        for (name, expected_mode) in [
            ("health_query", AgentMode::Ask),
            ("trends", AgentMode::Trends),
            ("patient_lookup", AgentMode::Ask),
            ("summarize", AgentMode::Handover),
            ("chat", AgentMode::Ask),
            ("auto", AgentMode::Ask),
        ] {
            let (kind, mode) = parse_kind(name).expect("legacy name must parse");
            assert_eq!(kind, AgentKind::Ask, "legacy '{name}' maps to Ask");
            assert_eq!(mode, expected_mode, "legacy '{name}' mode");
        }
    }

    #[test]
    fn deterministic_scalar_sql_result_preserves_exact_count_for_chart() {
        let rows = sql_rows_to_agg_rows(&[vec![serde_json::json!(16)]]);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].label, "(all)");
        assert_eq!(rows[0].value, 16.0);
    }

    #[test]
    fn line_agents_filter_retrieval_but_ask_does_not() {
        let scope = vec!["deliveries".to_string()];
        let line = RetrievalFilter::for_scope(
            scope.clone(),
            AgentKind::Line(ServiceLine::Maternity).scope_is_explicit(),
        );
        let ask = RetrievalFilter::for_scope(scope, AgentKind::Ask.scope_is_explicit());
        assert!(line.explicit);
        assert!(!ask.explicit);
    }
}
