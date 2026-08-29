//! Foundry Local HTTP routes: hardware/EP enumeration, model listing/selection,
//! and a streaming test-generation endpoint (SSE).

use futures::StreamExt;
use rocket::response::stream::{Event, EventStream};
use rocket::serde::json::Json;
use rocket::{State, get, post, put};
use serde::{Deserialize, Serialize};

use super::router::{AgentKind, Device, ModelSpec};
use super::{EpRegistration, ExecutionProvider, HardwareInfo, ModelSummary, VariantInfo};
use crate::auth::guard::AuthUser;
use crate::error::AppResult;
use crate::state::AppState;

/// Ordered model families (small → large params). Used to offer a role's assigned
/// model plus its higher-parameter siblings in the Settings dropdown.
const MODEL_FAMILIES: &[&[&str]] = &[
    &["qwen3-4b", "qwen3-8b", "qwen3-14b"],
    &["phi-4-mini-instruct", "phi-4"],
    &["phi-4-mini-reasoning", "phi-4-reasoning"],
    &["mistral-nemo-12b-instruct"],
];
/// The assigned alias plus every higher-parameter sibling in its family (assigned first).
/// Aliases not in any family return just themselves.
fn family_and_higher(alias: &str) -> Vec<String> {
    for fam in MODEL_FAMILIES {
        if let Some(i) = fam.iter().position(|a| *a == alias) {
            return fam[i..].iter().map(|s| s.to_string()).collect();
        }
    }
    vec![alias.to_string()]
}

/// Rank a variant id by how preferred its device is for a role (lower = more preferred).
/// Variants whose device matches none of the role's preferences sort last.
fn device_rank(id: &str, pref: &[Device]) -> usize {
    for (i, dev) in pref.iter().enumerate() {
        if dev.tokens().iter().any(|t| id.contains(t)) {
            return i;
        }
    }
    pref.len()
}

/// `GET /hardware` — execution providers + the current chat model. Any authenticated user.
#[get("/hardware")]
pub async fn hardware(state: &State<AppState>, _user: AuthUser) -> AppResult<Json<HardwareInfo>> {
    Ok(Json(state.foundry()?.hardware()?))
}

/// `POST /hardware/register-eps` — download + register all available execution providers
/// into the server's Foundry core. Admin only. Watch progress in the live log stream.
#[post("/hardware/register-eps")]
pub async fn register_eps(
    state: &State<AppState>,
    user: AuthUser,
) -> AppResult<Json<EpRegistration>> {
    user.require_admin()?;
    Ok(Json(state.foundry()?.register_eps().await?))
}

/// `GET /models` — the Foundry Local catalog with cached/loaded state. Any authenticated user.
#[get("/models")]
pub async fn list_models(
    state: &State<AppState>,
    _user: AuthUser,
) -> AppResult<Json<Vec<ModelSummary>>> {
    Ok(Json(state.foundry()?.list_models().await?))
}

#[derive(Debug, Deserialize)]
pub struct SelectModelRequest {
    /// Model alias (e.g. `qwen2.5-7b`) or full variant id (pins the execution provider).
    pub model: String,
}

#[derive(Debug, Serialize)]
pub struct SelectModelResponse {
    /// Resolved variant id now loaded and selected.
    pub model: String,
}

/// `POST /models/select` — download (if needed), load, and select a chat model. Admin only.
#[post("/models/select", data = "<body>")]
pub async fn select_model(
    state: &State<AppState>,
    user: AuthUser,
    body: Json<SelectModelRequest>,
) -> AppResult<Json<SelectModelResponse>> {
    user.require_admin()?;
    let model = state.foundry()?.select_model(&body.model).await?;
    Ok(Json(SelectModelResponse { model }))
}

#[derive(Debug, Deserialize)]
pub struct GenerateRequest {
    pub prompt: String,
}

/// `POST /generate` — stream a completion from the current chat model as SSE.
/// A plumbing/smoke test for model + streaming; the grounded RAG `/chat` lands in WS6.
#[post("/generate", data = "<body>")]
pub async fn generate(
    state: &State<AppState>,
    _user: AuthUser,
    body: Json<GenerateRequest>,
) -> AppResult<EventStream![]> {
    // Open the stream up front so a setup failure surfaces as a normal HTTP error
    // (the SSE body itself can't carry a status code).
    let mut stream = state
        .foundry()?
        .generate_stream("You are a helpful assistant. Answer concisely.", &body.prompt)
        .await?;

    Ok(EventStream! {
        let mut think = super::think_filter::ThinkFilter::new();
        while let Some(chunk) = stream.next().await {
            match chunk {
                Ok(resp) => {
                    if let Some(token) = resp.choices.first().and_then(|c| c.delta.content.clone()) {
                        let visible = think.push(&token);
                        if !visible.is_empty() {
                            yield token_event(&visible);
                        }
                    }
                }
                Err(e) => {
                    yield Event::data(format!("Foundry Local: {e}")).event("error");
                    break;
                }
            }
        }
        let tail = think.finish();
        if !tail.is_empty() { yield token_event(&tail); }
        yield Event::data("").event("done");
    })
}

/// Encode a streamed token as an SSE `token` event. JSON-encoding protects the
/// token's leading/trailing spaces, which the SSE spec would otherwise strip from a
/// bare `data:` field — fusing words together on the client.
fn token_event(text: &str) -> Event {
    Event::data(serde_json::to_string(text).unwrap_or_else(|_| "\"\"".to_string())).event("token")
}

// ---------------------------------------------------------------------------
// Model role manifest — pure config read, no Foundry call required.
// ---------------------------------------------------------------------------

/// One entry in the model-role manifest returned by `GET /models/roles`.
/// Describes which local model handles a specific task, how it's placed on
/// hardware, and whether it's active in the current build.
#[derive(Debug, Clone, Serialize)]
pub struct ModelRole {
    /// Stable identifier used in ONPREM_MODEL_* env vars (e.g. `"chat"`).
    pub role: String,
    /// Human-readable display label shown in the Settings table.
    pub label: String,
    /// Resolved model alias (follows ONPREM_MODEL_* overrides).
    pub model: String,
    /// Inference engine: `"Foundry Local"` or `"fastembed (ONNX)"`.
    pub engine: String,
    /// Resolved device placement, e.g. `"GPU"`, `"GPU → CPU"`, `"ORT (CPU/GPU)"`.
    pub device: String,
    /// One-sentence description of what this role does.
    pub usage: String,
    /// `Some(true/false)` for Foundry chat roles (Qwen3 thinking mode); `None` for
    /// non-Foundry roles (embeddings, reranker) where the concept doesn't apply.
    pub thinking: Option<bool>,
    /// `"active"` if wired up in the current build, `"planned"` if reserved for a
    /// later workstream.
    pub status: String,
    /// Whether this role is served by Foundry Local (downloadable/loadable variants)
    /// vs. a bundled fastembed model (`false`, e.g. embeddings/reranker).
    pub managed: bool,
    /// Downloadable/loadable variants for this role: the assigned model plus any
    /// higher-parameter family siblings. Empty for non-managed (fastembed) roles.
    pub variants: Vec<crate::foundry::VariantInfo>,
    /// The persisted routing override for this role, if an admin has saved one via
    /// `PUT /settings/router` (wins over the `ONPREM_MODEL_*` env default). `None`
    /// for fastembed roles, where the concept doesn't apply.
    pub override_variant: Option<String>,
}

/// Format an ordered device-preference list as a readable fallback chain,
/// e.g. `[Npu, Cpu]` → `"NPU → CPU"`.
fn format_device_pref(pref: &[Device]) -> String {
    pref.iter()
        .map(|d| match d {
            Device::Npu => "NPU",
            Device::Gpu => "GPU",
            Device::Cpu => "CPU",
        })
        .collect::<Vec<_>>()
        .join(" → ")
}

/// `GET /models/roles` — the roles this deployment uses and which models serve them.
///
/// Calls `state.foundry()` to enrich the 8 Foundry roles with downloadable/loadable
/// variant info (so the endpoint now requires Foundry Local to be up). The Foundry
/// roles derive their alias, device, and thinking flag directly from
/// `ModelSpec::for_kind`, keeping this endpoint in sync with the router without
/// duplication.
#[get("/models/roles")]
pub async fn model_roles(
    state: &State<AppState>,
    _user: AuthUser,
) -> AppResult<Json<Vec<ModelRole>>> {
    let cfg = &state.config;

    // Closure avoids repeating the Foundry boilerplate for the 8 chat-engine roles.
    // cfg is &Config which is Copy, so the closure captures it without issue.
    // `variants` is filled in a second pass below, once the union lookup is done.
    // Stashes each role's `device_pref` (keyed by role id) so the second pass can
    // order variants by the role's actual device preference without re-deriving
    // `AgentKind` from the role string.
    let mut device_prefs: std::collections::HashMap<String, Vec<Device>> =
        std::collections::HashMap::new();
    let mut foundry_role =
        |role: &str, label: &str, kind: AgentKind, usage: &str, status: &str| -> ModelRole {
            let spec = ModelSpec::for_kind(kind, cfg);
            device_prefs.insert(role.to_string(), spec.device_pref.clone());
            ModelRole {
                role: role.to_string(),
                label: label.to_string(),
                model: spec.alias,
                engine: "Foundry Local".to_string(),
                device: format_device_pref(&spec.device_pref),
                usage: usage.to_string(),
                thinking: Some(spec.thinking),
                status: status.to_string(),
                managed: true,
                variants: vec![],
                override_variant: state.router_override(role),
            }
        };

    let mut roles = vec![
        foundry_role(
            "chat",
            "Chat",
            AgentKind::Chat,
            "Grounded conversational Q&A over your records (semantic RAG).",
            "active",
        ),
        foundry_role(
            "health_query",
            "Health Query",
            AgentKind::HealthQuery,
            "Plans and narrates exact aggregations (counts, group-by) — the structured analytics path.",
            "active",
        ),
        foundry_role(
            "trends",
            "Trends",
            AgentKind::Trends,
            "Time-bucketed trend aggregations with temporal interpretation.",
            "active",
        ),
        foundry_role(
            "summarize",
            "Summarize",
            AgentKind::Summarize,
            "Long-context faithful summaries of a patient history or cohort.",
            "active",
        ),
        foundry_role(
            "lookup",
            "Patient Lookup",
            AgentKind::PatientLookup,
            "Fast literal record lookup; refuses when the record is absent.",
            "active",
        ),
        foundry_role(
            "fast",
            "Query rewrite & expansion",
            AgentKind::QueryRewrite,
            "Internal only: history-aware query rewrite, multi-query expansion, intent classify. Never user-facing.",
            "active",
        ),
        foundry_role(
            "extractor",
            "Ingestion extractor",
            AgentKind::Extract,
            "Pulls structured fields from free-text notes at ingest and maps to ICD-10/LOINC. Runs on the NPU to free the iGPU. (Wired in Phase 4.)",
            "planned",
        ),
        foundry_role(
            "verifier",
            "Faithfulness verifier",
            AgentKind::Verify,
            "Safety pass: checks every clinical claim is supported by the retrieved passages. (Wired in Phase 4.)",
            "planned",
        ),
        // fastembed / ONNX Runtime roles — not served by Foundry Local.
        ModelRole {
            role: "embeddings".to_string(),
            label: "Embeddings".to_string(),
            model: cfg.embedding_model.clone(),
            engine: "fastembed (ONNX)".to_string(),
            device: "ORT (CPU/GPU)".to_string(),
            usage: "Vectorizes records and queries for semantic search (1024-dim, prefix-free)."
                .to_string(),
            thinking: None,
            status: "active".to_string(),
            managed: false,
            variants: vec![],
            override_variant: None,
        },
        ModelRole {
            role: "reranker".to_string(),
            label: "Reranker".to_string(),
            model: cfg.rerank_model.clone(),
            engine: "fastembed (ONNX)".to_string(),
            device: "ORT (CPU/GPU)".to_string(),
            usage: "Cross-encoder that reorders hybrid-retrieval hits before generation."
                .to_string(),
            thinking: None,
            status: "active".to_string(),
            managed: false,
            variants: vec![],
            override_variant: None,
        },
    ];

    // Second pass: fill `variants` for the managed (Foundry) roles. Collect the union
    // of candidate aliases (assigned + higher-parameter family siblings) across all
    // managed roles, resolve variant info once, then assemble each role's list in
    // ORDER: outer loop over the role's candidate aliases in `family_and_higher` order
    // (assigned/lowest-param first), and within each alias, sort matching variants by
    // device preference (the role's preferred device first) so `variants[0]` is always
    // the router's real default — the assigned alias on its preferred device.
    let candidates_by_role: Vec<(usize, Vec<String>, Vec<Device>)> = roles
        .iter()
        .enumerate()
        .filter(|(_, r)| r.managed)
        .map(|(i, r)| {
            let device_pref = device_prefs.get(&r.role).cloned().unwrap_or_default();
            (i, family_and_higher(&r.model), device_pref)
        })
        .collect();
    let union: Vec<String> = {
        let mut all: Vec<String> = candidates_by_role
            .iter()
            .flat_map(|(_, c, _)| c.iter().cloned())
            .collect();
        all.sort();
        all.dedup();
        all
    };
    let variant_pool = state.foundry()?.variants_for(&union).await?;
    for (i, candidates, device_pref) in candidates_by_role {
        let mut ordered: Vec<VariantInfo> = Vec::new();
        for alias in &candidates {
            let mut matching: Vec<VariantInfo> = variant_pool
                .iter()
                .filter(|v| &v.alias == alias)
                .cloned()
                .collect();
            matching.sort_by(|a, b| {
                (device_rank(&a.id, &device_pref), &a.id).cmp(&(device_rank(&b.id, &device_pref), &b.id))
            });
            ordered.extend(matching);
        }
        roles[i].variants = ordered;
    }

    Ok(Json(roles))
}

#[derive(Debug, serde::Deserialize)]
pub struct PullRequest {
    pub variant_id: String,
    #[serde(default)]
    pub load: bool,
}

/// `POST /models/pull` — download (with progress) and optionally load a variant, as SSE.
/// Events: `progress` (percent 0..100 as a bare number string) → `status` → `done`, or `error`.
#[post("/models/pull", data = "<body>")]
pub async fn pull_model(
    state: &State<AppState>,
    user: AuthUser,
    body: Json<PullRequest>,
) -> AppResult<EventStream![]> {
    user.require_admin()?;
    let foundry = state.foundry()?;
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    foundry.spawn_pull(body.variant_id.clone(), body.load, tx);
    Ok(EventStream! {
        use crate::foundry::PullMsg;
        while let Some(msg) = rx.recv().await {
            match msg {
                PullMsg::Progress(p) => yield Event::data(p.to_string()).event("progress"),
                PullMsg::Status(s)   => yield Event::data(s).event("status"),
                PullMsg::Error(e)    => { yield Event::data(e).event("error"); break; }
                PullMsg::Done        => { yield Event::data("").event("done"); break; }
            }
        }
    })
}

#[derive(Debug, serde::Deserialize)]
pub struct SetRouterReq {
    pub role: String,
    pub variant_id: Option<String>,
}

/// `PUT /settings/router` — persist (or clear, when `variant_id` is `null`) a role's
/// model override in DocumentDB, and update the in-memory cache so `state.spec_for`
/// picks it up immediately (no restart required). Admin only.
///
/// The `"chat"` role also drives the legacy `/chat` + `/generate` current model, since
/// those bypass the `AgentKind` router and read `FoundryManager::current_model()` directly.
#[put("/settings/router", data = "<body>")]
pub async fn set_router(
    state: &State<AppState>,
    user: AuthUser,
    body: Json<SetRouterReq>,
) -> AppResult<Json<serde_json::Value>> {
    user.require_admin()?;
    crate::settings::set_router_override(&state.db, &body.role, body.variant_id.as_deref(), &user.username)
        .await?;
    state.set_router_override_cache(&body.role, body.variant_id.clone());
    if body.role == "chat" {
        if let (Ok(f), Some(v)) = (state.foundry(), body.variant_id.as_ref()) {
            f.set_current_model(v.clone());
        }
    }
    Ok(Json(serde_json::json!({ "ok": true })))
}

#[derive(Debug, Deserialize)]
pub struct DeleteModelReq {
    pub variant_id: String,
}

#[derive(Debug, Serialize)]
pub struct DeleteModelResponse {
    /// Roles whose persisted override named the deleted variant and was cleared.
    pub cleared_roles: Vec<String>,
}

/// `POST /models/delete` — unload the variant, delete its weights from the Foundry model
/// cache, and clear any persisted router override that named it. Admin only.
///
/// The two cleanups belong together: an override surviving in DocumentDB would point a
/// role at weights that are no longer on disk, quietly re-downloading them on the next
/// call. A cleared `"chat"` role also resets `current_model()`, which the legacy
/// `/chat` + `/generate` path reads directly instead of going through the router.
#[post("/models/delete", data = "<body>")]
pub async fn delete_model(
    state: &State<AppState>,
    user: AuthUser,
    body: Json<DeleteModelReq>,
) -> AppResult<Json<DeleteModelResponse>> {
    user.require_admin()?;
    state.foundry()?.delete_model(&body.variant_id).await?;

    let cleared_roles = crate::settings::clear_router_overrides_for_variant(
        &state.db,
        &body.variant_id,
        &user.username,
    )
    .await?;
    for role in &cleared_roles {
        state.set_router_override_cache(role, None);
        if role == "chat" {
            if let Ok(f) = state.foundry() {
                f.set_current_model(state.config.chat_model.clone());
            }
        }
    }
    Ok(Json(DeleteModelResponse { cleared_roles }))
}

#[derive(Debug, Deserialize)]
pub struct UnloadModelReq {
    pub variant_id: String,
}

/// `POST /models/unload` — unload a variant from memory but keep its weights on disk
/// (the honest analog of the reference's separate "unload" action; "delete weights"
/// stays on `POST /models/delete`). Idempotent, so unloading a not-resident variant
/// succeeds. Admin only.
#[post("/models/unload", data = "<body>")]
pub async fn unload_model(
    state: &State<AppState>,
    user: AuthUser,
    body: Json<UnloadModelReq>,
) -> AppResult<Json<serde_json::Value>> {
    user.require_admin()?;
    state.foundry()?.unload_model(&body.variant_id).await?;
    Ok(Json(serde_json::json!({ "ok": true })))
}

// ---------------------------------------------------------------------------
// System-setup status — one call powering the whole Settings page.
// ---------------------------------------------------------------------------

/// GPU presence + name, from OS-level detection (independent of Foundry EPs).
#[derive(Debug, Clone, Serialize)]
pub struct GpuInfo {
    pub has_gpu: bool,
    pub gpu_name: Option<String>,
}

/// Health of one backing service for the Settings "Service Health" grid.
#[derive(Debug, Clone, Serialize)]
pub struct ServiceStatus {
    pub name: String,
    /// `"ok"` | `"error"` | `"unknown"`.
    pub status: String,
    pub detail: Option<String>,
}

/// Aggregate setup snapshot for the Settings page. Every model/EP field degrades to
/// empty when the Foundry core is down so the whole payload still serializes.
#[derive(Debug, Clone, Serialize)]
pub struct SetupStatus {
    pub gpu: GpuInfo,
    pub active_chat_model: String,
    pub foundry_endpoint: String,
    pub foundry_ready: bool,
    pub services: Vec<ServiceStatus>,
    pub loaded_models: Vec<String>,
    pub cached_models: Vec<String>,
    pub execution_providers: Vec<ExecutionProvider>,
}

/// `GET /setup-status` — one snapshot for the Settings page. Any authenticated user.
///
/// **Always 200, even when Foundry is down** (degraded state): GPU is read from the
/// OS-level `hardware::detect()` (which never touches the core), and every
/// Foundry-derived field is gated behind `if let Ok(f) = state.foundry()`, leaving the
/// model/EP lists empty when the native core failed to initialise at boot.
#[get("/setup-status")]
pub async fn setup_status(state: &State<AppState>, _user: AuthUser) -> Json<SetupStatus> {
    // GPU from OS-level detection so this field is meaningful even when Foundry is down.
    let devices = super::hardware::detect();
    let gpu_dev = devices.iter().find(|d| d.kind == "GPU");
    let gpu = GpuInfo { has_gpu: gpu_dev.is_some(), gpu_name: gpu_dev.map(|d| d.name.clone()) };

    let foundry_ready = state.foundry().is_ok();

    // Model/EP fields are only populated when the core is up; empty (degraded) otherwise.
    let mut active_chat_model = String::new();
    let mut foundry_endpoint = String::new();
    let mut loaded_models: Vec<String> = Vec::new();
    let mut cached_models: Vec<String> = Vec::new();
    let mut execution_providers: Vec<ExecutionProvider> = Vec::new();
    if let Ok(f) = state.foundry() {
        active_chat_model = f.current_model();
        // The native core has no port/URL; report the literal in-process marker.
        foundry_endpoint = "in-process (native SDK)".to_string();
        if let Ok(models) = f.list_models().await {
            loaded_models = models.iter().filter(|m| m.loaded).map(|m| m.id.clone()).collect();
            cached_models = models.iter().filter(|m| m.cached).map(|m| m.id.clone()).collect();
        }
        if let Ok(hw) = f.hardware() {
            execution_providers = hw.execution_providers;
        }
    }

    // Services in a fixed display order. DocumentDB reuses the /health ping.
    let db_ok = state.db.ping().await.is_ok();
    let services = vec![
        ServiceStatus {
            name: "Foundry Local".to_string(),
            status: if foundry_ready { "ok" } else { "error" }.to_string(),
            detail: Some(if foundry_ready {
                active_chat_model.clone()
            } else {
                "native core unavailable".to_string()
            }),
        },
        ServiceStatus {
            name: "DocumentDB".to_string(),
            status: if db_ok { "ok" } else { "error" }.to_string(),
            detail: Some("MongoDB-wire / pgvector".to_string()),
        },
        ServiceStatus {
            name: "Embeddings (fastembed)".to_string(),
            status: "ok".to_string(),
            detail: Some(state.config.embedding_model.clone()),
        },
        ServiceStatus {
            name: "Reranker (fastembed)".to_string(),
            status: "ok".to_string(),
            detail: Some(state.config.rerank_model.clone()),
        },
    ];

    Json(SetupStatus {
        gpu,
        active_chat_model,
        foundry_endpoint,
        foundry_ready,
        services,
        loaded_models,
        cached_models,
        execution_providers,
    })
}
