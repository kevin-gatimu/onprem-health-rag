//! Foundry Local integration — **chat only**.
//!
//! Foundry Local's catalog has no embedding models (verified on 0.8.119), so this
//! module handles chat generation + hardware/model management only; embeddings live
//! in `embed/` (fastembed). Chat runs fully in-process through the native engine
//! (`ChatClient` calls the core directly — no HTTP endpoint, no dynamic port).

pub mod hardware;
pub mod router;
pub mod routes;
pub mod think_filter;

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, LazyLock, Mutex};

use async_openai::types::chat::ChatCompletionTool;
use foundry_local_sdk::{
    ChatCompletionMessageToolCalls, ChatCompletionRequestMessage,
    ChatCompletionRequestSystemMessage, ChatCompletionRequestUserMessage, ChatCompletionStream,
    ChatCompletionTools, ChatToolChoice, FoundryLocalConfig, FoundryLocalError,
    FoundryLocalManager, FunctionObject, Model,
};
use serde::Serialize;

use crate::aggregation::RunAggregation;
use crate::aggregation::spec::RunList;
use crate::config::Config;
use crate::error::{AppError, AppResult};
use crate::ingest::extract::ExtractedClinical;
use crate::nl2sql::spec::EmitSqlOutput;
use crate::router::RouteToolOutput;
use crate::verify::VerifyToolOutput;
use router::{Device, ModelSpec};

/// Map an SDK error to our error type. Foundry problems are "service unavailable"
/// (the model host is down / a model op failed) rather than internal server faults.
fn map_err(e: FoundryLocalError) -> AppError {
    AppError::Unavailable(format!("Foundry Local: {e}"))
}

/// Whether an SDK error came from the constrained-decoding grammar compiler rather
/// than from generation itself.
///
/// Matched on the message because the SDK surfaces the .NET exception as an opaque
/// `command execution error` string — there is no typed variant to match. Kept
/// deliberately narrow so an unrelated failure never triggers the retry in
/// `plan_tool`; the text comes from ORT-GenAI's `Error creating grammar:
/// Unsatisfiable schema: ...`.
fn is_grammar_error(e: &FoundryLocalError) -> bool {
    let msg = e.to_string();
    msg.contains("creating grammar") || msg.contains("Unsatisfiable schema")
}

/// An execution provider (GPU/NPU/CPU backend) as reported by Foundry Local, enriched
/// with a human-readable classification so the UI can group backends by accelerator.
#[derive(Debug, Clone, Serialize)]
pub struct ExecutionProvider {
    /// Raw EP name from the SDK (e.g. `OpenVINOExecutionProvider`).
    pub name: String,
    /// Whether Foundry has downloaded + registered this EP's plugin (usable now).
    pub registered: bool,
    /// Coarse accelerator class: `"CPU"`, `"GPU"`, `"NPU"`, or `"Other"`.
    pub device_kind: String,
    /// Friendly label, e.g. `"OpenVINO — Intel GPU/NPU"` or `"CUDA — NVIDIA GPU"`.
    pub label: String,
}

impl ExecutionProvider {
    /// Classify a raw EP name into `(device_kind, label)`. Covers the accelerators
    /// Foundry Local can target: CPU, WebGPU/CUDA/TensorRT/OpenVINO (GPU), and
    /// QNN/Vitis AI (NPU). Unknown EPs fall back to `Other` with the raw name.
    fn classify(name: &str) -> (&'static str, String) {
        // Match on a lowercased, `ExecutionProvider`-stripped stem so minor SDK
        // naming variants (e.g. `NvTensorRtRtx`) still map cleanly.
        let stem = name.to_ascii_lowercase().replace("executionprovider", "");
        let stem = stem.trim().trim_end_matches('_');
        match stem {
            "cpu" => ("CPU", "CPU".to_string()),
            "webgpu" | "dml" | "directml" => ("GPU", "WebGPU / DirectML — GPU".to_string()),
            "cuda" => ("GPU", "CUDA — NVIDIA GPU".to_string()),
            s if s.contains("tensorrt") || s.contains("nvtensorrt") => {
                ("GPU", "TensorRT — NVIDIA GPU".to_string())
            }
            "openvino" => ("GPU", "OpenVINO — Intel GPU/NPU".to_string()),
            "qnn" => ("NPU", "QNN — Qualcomm NPU".to_string()),
            s if s.contains("vitis") => ("NPU", "Vitis AI — AMD/Xilinx NPU".to_string()),
            _ => ("Other", name.to_string()),
        }
    }

    fn new(name: String, registered: bool) -> Self {
        let (device_kind, label) = Self::classify(&name);
        Self {
            name,
            registered,
            device_kind: device_kind.to_string(),
            label,
        }
    }
}

/// Result of registering execution providers with the server's in-process Foundry core.
#[derive(Debug, Clone, Serialize)]
pub struct EpRegistration {
    pub success: bool,
    pub status: String,
    pub registered: Vec<String>,
    pub failed: Vec<String>,
}

/// Hardware summary surfaced to the Settings UI.
#[derive(Debug, Clone, Serialize)]
pub struct HardwareInfo {
    /// Foundry Local execution providers (classified), with registration state.
    pub execution_providers: Vec<ExecutionProvider>,
    /// Physical accelerators detected at the OS level (independent of EP registration),
    /// so the UI can explain a present-but-unregistered device. Empty if detection
    /// isn't available on this host.
    pub detected_hardware: Vec<hardware::DetectedDevice>,
    /// The chat model the server will use for generation (alias or variant id).
    pub current_chat_model: String,
}

/// A catalog model entry, flattened for the Settings UI.
#[derive(Debug, Clone, Serialize)]
pub struct ModelSummary {
    pub alias: String,
    pub id: String,
    pub capabilities: Option<String>,
    pub input_modalities: Option<String>,
    pub output_modalities: Option<String>,
    pub context_length: Option<u64>,
    pub cached: bool,
    pub loaded: bool,
}

/// A single downloadable/loadable variant of a catalog model, for the "Models this
/// app uses" Settings table — one role can offer several variants (its assigned
/// alias plus higher-parameter siblings in the same family).
#[derive(Debug, Clone, Serialize)]
pub struct VariantInfo {
    pub id: String,
    pub alias: String,
    pub accelerator: String,
    pub supports_tool_calling: bool,
    pub cached: bool,
    pub loaded: bool,
    pub current: bool,
    pub context_length: Option<u64>,
}

/// Progress/status messages streamed from `spawn_pull` back to the SSE route.
pub enum PullMsg {
    /// Download progress, 0.0..100.0.
    Progress(f64),
    /// Free-text status transition (e.g. `"loading"`, `"loaded"`).
    Status(String),
    Done,
    Error(String),
}

/// Serialises native `load()`/`unload()` calls process-wide: the native core
/// (OpenVINO/ONNX) segfaults (STATUS_ACCESS_VIOLATION) when two loads run
/// concurrently, e.g. two "Load" clicks in the UI. Downloads stay parallel.
static LOAD_GATE: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

/// Models with an in-flight generation (variant id → active request count).
/// Unloading a model mid-generation fail-fasts the native core
/// (STATUS_STACK_BUFFER_OVERRUN, 0xc0000409), so LRU eviction and manual
/// unload/delete consult this before touching a model.
static BUSY_MODELS: LazyLock<Mutex<HashMap<String, usize>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

/// Whether a variant currently has at least one in-flight generation.
fn is_busy(variant_id: &str) -> bool {
    BUSY_MODELS
        .lock()
        .expect("busy-models lock poisoned")
        .get(variant_id)
        .is_some_and(|n| *n > 0)
}

/// RAII marker for an in-flight generation: increments the model's busy count on
/// creation and decrements on drop. Held by `GuardedChatStream` so the count
/// stays accurate for the full lifetime of a streamed response, including when
/// the client disconnects and the stream is dropped early.
struct BusyGuard(String);

impl BusyGuard {
    fn new(variant_id: String) -> Self {
        *BUSY_MODELS
            .lock()
            .expect("busy-models lock poisoned")
            .entry(variant_id.clone())
            .or_insert(0) += 1;
        Self(variant_id)
    }
}

impl Drop for BusyGuard {
    fn drop(&mut self) {
        let mut map = BUSY_MODELS.lock().expect("busy-models lock poisoned");
        if let Some(n) = map.get_mut(&self.0) {
            *n -= 1;
            if *n == 0 {
                map.remove(&self.0);
            }
        }
    }
}

/// A chat completion stream that keeps its model marked busy until dropped, so
/// eviction/unload can't rip the weights out from under an active generation.
pub struct GuardedChatStream {
    inner: ChatCompletionStream,
    _busy: BusyGuard,
}

impl futures::Stream for GuardedChatStream {
    type Item = <ChatCompletionStream as futures::Stream>::Item;

    fn poll_next(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Option<Self::Item>> {
        std::pin::Pin::new(&mut self.inner).poll_next(cx)
    }
}

/// Wraps the process-global Foundry Local singleton and tracks the selected chat model.
pub struct FoundryManager {
    /// `&'static` because `FoundryLocalManager::create` returns a reference to an
    /// internal `OnceLock` singleton (the native engine is one-per-process).
    manager: &'static FoundryLocalManager,
    /// Currently selected chat model (alias or variant id). Guarded for `POST /models/select`.
    current_chat_model: Mutex<String>,
    /// LRU-ordered list of currently resident GPU-class model ids (oldest first; index 0 = LRU).
    /// Only GPU-class models count — NPU/CPU models live on separate silicon and are exempt
    /// from the cap, so phi-4-mini can stay hot on the NPU without displacing chat models.
    lru_residents: Mutex<Vec<String>>,
    /// Cap from `RouterConfig::max_resident_models`.
    max_resident_models: usize,
    /// Whether NPU placement is enabled (`RouterConfig::npu_enabled`).
    npu_enabled: bool,
    /// NPU context-length guard (`RouterConfig::npu_ctx_cap`).
    npu_ctx_cap: u64,
}

impl FoundryManager {
    /// Initialise the native engine and seed the selected model from config.
    /// Loads native libraries; callers treat failure as non-fatal (chat features
    /// become unavailable, but the rest of the server still boots).
    pub fn init(config: &Config) -> AppResult<Self> {
        let mut fc = FoundryLocalConfig::new("onprem-rag-server");
        // Our core is separate from the `foundry` CLI service, so the cache directory has to
        // be handed over explicitly — otherwise it defaults to `~/.foundry/cache` and keeps
        // reporting the leftover model index there as cached after the cache is moved.
        if let Some(dir) = &config.foundry_cache_dir {
            tracing::info!(cache_dir = %dir, "Foundry Local: using model cache directory");
            fc = fc.model_cache_dir(dir.clone());
        } else {
            tracing::warn!(
                "Foundry Local: no model cache directory resolved; using the SDK default. Set ONPREM_FOUNDRY_CACHE_DIR if the real cache lives elsewhere."
            );
        }
        let manager = FoundryLocalManager::create(fc).map_err(map_err)?;
        match manager.discover_eps() {
            Ok(eps) => tracing::info!(
                providers = ?eps.iter().map(|e| &e.name).collect::<Vec<_>>(),
                "Foundry Local ready; execution providers discovered"
            ),
            Err(e) => {
                tracing::warn!(error = %e, "Foundry Local: could not discover execution providers")
            }
        }
        Ok(Self {
            manager,
            current_chat_model: Mutex::new(config.chat_model.clone()),
            lru_residents: Mutex::new(Vec::new()),
            max_resident_models: config.router.max_resident_models,
            npu_enabled: config.router.npu_enabled,
            npu_ctx_cap: config.router.npu_ctx_cap,
        })
    }

    /// The chat model the server will generate with.
    pub fn current_model(&self) -> String {
        self.current_chat_model
            .lock()
            .expect("chat-model lock poisoned")
            .clone()
    }

    /// Directly set the current chat model id without downloading/loading — used when
    /// persisting a "chat" role override that was already loaded via `spawn_pull`.
    pub fn set_current_model(&self, id: String) {
        *self
            .current_chat_model
            .lock()
            .expect("chat-model lock poisoned") = id;
    }

    /// Download (if needed) and register all execution providers available for this
    /// hardware into the server's in-process core, so GPU/NPU backends become usable.
    /// The programmatic equivalent of the `foundry model list` first-run EP download,
    /// but scoped to our core (the Foundry CLI service is a separate instance).
    pub async fn register_eps(&self) -> AppResult<EpRegistration> {
        tracing::info!("registering Foundry execution providers (downloading plugins if needed)…");
        let r = self
            .manager
            .download_and_register_eps(None)
            .await
            .map_err(map_err)?;
        if r.failed_eps.is_empty() {
            tracing::info!(registered = ?r.registered_eps, "execution providers registered");
        } else {
            tracing::warn!(registered = ?r.registered_eps, failed = ?r.failed_eps, status = %r.status, "some execution providers failed to register");
        }
        Ok(EpRegistration {
            success: r.success,
            status: r.status,
            registered: r.registered_eps,
            failed: r.failed_eps,
        })
    }

    /// Load a cached routed model during startup so execution-provider graph setup is
    /// paid before the first request. Startup never downloads a missing model.
    pub async fn warm_cached_model(&self, spec: &ModelSpec) -> AppResult<bool> {
        let model = self.resolve_variant(&spec.alias, &spec.device_pref).await?;
        if !model.is_cached().await.map_err(map_err)? {
            tracing::info!(
                model = model.id(),
                "startup: core LLM is not cached; skipping preload"
            );
            return Ok(false);
        }
        self.ensure_loaded_lru(&model).await?;
        // Sync the legacy current-model slot so `/generate` and un-routed callers
        // use the same model the router warmed — divergence here causes LRU thrash.
        self.set_current_model(model.id().to_string());
        tracing::info!(model = model.id(), "startup: core LLM loaded");
        Ok(true)
    }

    /// Fire-and-forget EP registration at startup so accelerators come online without a
    /// manual step. Non-blocking (may download plugin binaries); logs the outcome to the
    /// live log stream. Safe/idempotent when EPs are already registered.
    pub fn spawn_startup_registration(&self) {
        let manager = self.manager; // &'static, Copy
        tokio::spawn(async move {
            match manager.download_and_register_eps(None).await {
                Ok(r) if r.failed_eps.is_empty() => {
                    tracing::info!(registered = ?r.registered_eps, "startup: execution providers registered");
                }
                Ok(r) => {
                    tracing::warn!(registered = ?r.registered_eps, failed = ?r.failed_eps, "startup: some execution providers failed to register")
                }
                Err(e) => tracing::warn!(error = %e, "startup: EP registration failed"),
            }
        });
    }

    /// Execution providers, de-duplicated by name (the SDK can list a provider twice),
    /// each classified into an accelerator kind, plus OS-level physical devices.
    pub fn hardware(&self) -> AppResult<HardwareInfo> {
        let mut seen = HashSet::new();
        let mut eps = Vec::new();
        for ep in self.manager.discover_eps().map_err(map_err)? {
            if seen.insert(ep.name.clone()) {
                eps.push(ExecutionProvider::new(ep.name, ep.is_registered));
            }
        }
        Ok(HardwareInfo {
            execution_providers: eps,
            detected_hardware: hardware::detect().to_vec(),
            current_chat_model: self.current_model(),
        })
    }

    /// All catalog models with cached/loaded state. Three catalog calls total
    /// (list + cached + loaded) rather than a per-model probe.
    pub async fn list_models(&self) -> AppResult<Vec<ModelSummary>> {
        let catalog = self.manager.catalog();
        let models = catalog.get_models().await.map_err(map_err)?;
        let cached = id_set(catalog.get_cached_models().await.map_err(map_err)?);
        let loaded = id_set(catalog.get_loaded_models().await.map_err(map_err)?);

        Ok(models
            .iter()
            .map(|m| {
                let id = m.id().to_string();
                ModelSummary {
                    alias: m.alias().to_string(),
                    cached: cached.contains(&id),
                    loaded: loaded.contains(&id),
                    capabilities: m.capabilities().map(str::to_string),
                    input_modalities: m.input_modalities().map(str::to_string),
                    output_modalities: m.output_modalities().map(str::to_string),
                    context_length: m.context_length(),
                    id,
                }
            })
            .collect())
    }

    /// Variant-level detail for a set of candidate aliases (e.g. a role's assigned
    /// alias plus its higher-parameter family siblings), for the "Models this app
    /// uses" Settings table. One catalog lookup per alias, plus a single cached/loaded
    /// snapshot shared across all of them.
    pub async fn variants_for(&self, aliases: &[String]) -> AppResult<Vec<VariantInfo>> {
        let catalog = self.manager.catalog();
        let cached = id_set(catalog.get_cached_models().await.map_err(map_err)?);
        let loaded = id_set(catalog.get_loaded_models().await.map_err(map_err)?);
        let current = self.current_model();

        let mut deduped_aliases: Vec<String> = aliases.to_vec();
        deduped_aliases.sort();
        deduped_aliases.dedup();

        let mut out: Vec<VariantInfo> = Vec::new();
        let mut seen_ids = HashSet::new();
        for alias in &deduped_aliases {
            let Ok(model) = catalog.get_model(alias).await else {
                continue;
            };
            let supports_tool_calling = model
                .capabilities()
                .is_some_and(|value| value.split(',').any(|item| item.trim() == "tool-calling"));
            for v in model.variants() {
                let id = v.id().to_string();
                if !seen_ids.insert(id.clone()) {
                    continue;
                }
                out.push(VariantInfo {
                    cached: cached.contains(&id),
                    loaded: loaded.contains(&id),
                    current: id == current,
                    context_length: v.context_length(),
                    alias: v.alias().to_string(),
                    accelerator: crate::foundry::routes::accelerator_label(&id).to_string(),
                    supports_tool_calling,
                    id,
                });
            }
        }
        Ok(out)
    }

    /// Download (if needed) and optionally load a variant, streaming progress/status
    /// back over `tx`. Runs detached (`tokio::spawn`) so the SSE route can start
    /// yielding events immediately. Only `&'static` / owned values cross the spawn
    /// boundary — no `&self` or borrowed `Arc<Model>` from the caller.
    pub fn spawn_pull(
        &self,
        variant_id: String,
        load: bool,
        tx: tokio::sync::mpsc::UnboundedSender<PullMsg>,
    ) {
        let manager = self.manager; // &'static, Copy
        tokio::spawn(async move {
            let model = match manager.catalog().get_model_variant(&variant_id).await {
                Ok(m) => m,
                Err(e) => {
                    let _ = tx.send(PullMsg::Error(format!("Foundry Local: {e}")));
                    return;
                }
            };

            match model.is_cached().await {
                Ok(true) => {}
                Ok(false) => {
                    tracing::info!(model = %variant_id, "downloading model variant");
                    let txp = tx.clone();
                    let cb = move |pct: f64| {
                        let _ = txp.send(PullMsg::Progress(pct));
                    };
                    if let Err(e) = model.download(Some(cb)).await {
                        let _ = tx.send(PullMsg::Error(format!("Foundry Local: {e}")));
                        return;
                    }
                }
                Err(e) => {
                    let _ = tx.send(PullMsg::Error(format!("Foundry Local: {e}")));
                    return;
                }
            }

            if load {
                let _ = tx.send(PullMsg::Status("loading".into()));
                let mut corrupt = false;
                {
                    let _gate = LOAD_GATE.lock().await;
                    if !model.is_loaded().await.unwrap_or(false) {
                        if let Err(e) = model.load().await {
                            if is_corrupt_weights_error(&e.to_string()) {
                                tracing::warn!(model = %variant_id, error = %e, "load hit corrupt cached weights; purging and re-downloading");
                                corrupt = true;
                            } else {
                                let _ = tx.send(PullMsg::Error(format!("Foundry Local: {e}")));
                                return;
                            }
                        }
                    }
                } // gate released during the re-download
                if corrupt {
                    let _ = tx.send(PullMsg::Status("cache corrupt — re-downloading".into()));
                    let txp = tx.clone();
                    let cb = move |pct: f64| {
                        let _ = txp.send(PullMsg::Progress(pct));
                    };
                    if let Err(e) = heal_and_load(&model, Some(cb)).await {
                        let _ = tx.send(PullMsg::Error(format!("Foundry Local: {e}")));
                        return;
                    }
                }
                tracing::info!(model = %variant_id, "model variant loaded");
                let _ = tx.send(PullMsg::Status("loaded".into()));
            }

            let _ = tx.send(PullMsg::Done);
        });
    }

    /// Unload (if resident) and delete a variant's weights from the Foundry model cache.
    ///
    /// Unload comes first: removing files under a loaded model leaves the engine holding
    /// handles to weights that no longer exist. A failed unload is logged and the cache
    /// removal still proceeds — the variant may simply not have been resident. The LRU
    /// list is pruned too, so a deleted model stops occupying a resident slot. The SDK
    /// invalidates its own catalog cache on removal, so the next `list_models` /
    /// `variants_for` reports the variant as not downloaded.
    pub async fn delete_model(&self, variant_id: &str) -> AppResult<()> {
        if is_busy(variant_id) {
            return Err(AppError::Unavailable(format!(
                "{variant_id} is currently generating a response — try again in a moment"
            )));
        }
        let catalog = self.manager.catalog();
        let model = catalog
            .get_model_variant(variant_id)
            .await
            .map_err(map_err)?;

        if model.is_loaded().await.unwrap_or(false) {
            let _gate = LOAD_GATE.lock().await;
            if let Err(e) = model.unload().await {
                tracing::warn!(model = %variant_id, error = %e, "delete: unload failed (continuing to cache removal)");
            }
        }
        if let Ok(mut lru) = self.lru_residents.lock() {
            lru.retain(|x| x != variant_id);
        }

        let detail = model.remove_from_cache().await.map_err(map_err)?;
        tracing::info!(model = %variant_id, detail = %detail, "deleted model variant from cache");
        Ok(())
    }

    /// Unload a variant from memory **without** deleting its weights.
    ///
    /// Mirrors the front half of `delete_model` (resolve variant → unload if resident
    /// → prune the LRU list) but stops short of the cache removal, so the weights stay
    /// on disk and can be reloaded instantly. Idempotent: unloading a model that isn't
    /// resident is a no-op success, and a failed unload is logged and still returns Ok
    /// — matching how `delete_model` treats unload failures as non-fatal.
    pub async fn unload_model(&self, variant_id: &str) -> AppResult<()> {
        if is_busy(variant_id) {
            return Err(AppError::Unavailable(format!(
                "{variant_id} is currently generating a response — try again in a moment"
            )));
        }
        let catalog = self.manager.catalog();
        let model = catalog
            .get_model_variant(variant_id)
            .await
            .map_err(map_err)?;

        if model.is_loaded().await.unwrap_or(false) {
            let _gate = LOAD_GATE.lock().await;
            if let Err(e) = model.unload().await {
                tracing::warn!(model = %variant_id, error = %e, "unload failed (treated as no-op success)");
            }
        }
        if let Ok(mut lru) = self.lru_residents.lock() {
            lru.retain(|x| x != variant_id);
        }
        Ok(())
    }

    /// Resolve a name to a model, trying alias first then variant id. On a miss,
    /// surface the available aliases so the caller (and the UI) can recover rather
    /// than see an opaque "unknown variant" — this is the common failure when a
    /// configured default points at a variant that isn't in the current catalog.
    async fn resolve(&self, name: &str) -> AppResult<Arc<Model>> {
        let catalog = self.manager.catalog();
        if let Ok(m) = catalog.get_model(name).await {
            return Ok(m);
        }
        if let Ok(m) = catalog.get_model_variant(name).await {
            return Ok(m);
        }
        // Neither an alias nor a variant id matched — build a helpful error.
        let available = match catalog.get_models().await {
            Ok(models) => {
                let mut aliases: Vec<String> =
                    models.iter().map(|m| m.alias().to_string()).collect();
                aliases.sort();
                aliases.dedup();
                aliases.join(", ")
            }
            Err(e) => return Err(map_err(e)),
        };
        Err(AppError::Unavailable(format!(
            "Foundry Local: no chat model matches '{name}'. Available aliases: {available}. \
             Load a variant from Settings, or set ONPREM_CHAT_MODEL to one of these."
        )))
    }

    /// Download (if needed) and load a chat model, making it the current selection.
    /// Returns the resolved variant id and whether a corrupt cache was healed en route.
    pub async fn select_model(&self, name: &str) -> AppResult<(String, bool)> {
        let model = self.resolve(name).await?;
        let repaired = ensure_loaded(&model).await?;
        let id = model.id().to_string();
        *self
            .current_chat_model
            .lock()
            .expect("chat-model lock poisoned") = id.clone();
        tracing::info!(model = %id, "chat model selected and loaded");
        Ok((id, repaired))
    }

    // -------------------------------------------------------------------------
    // Phase 2 — device placement + LRU resident cap
    // -------------------------------------------------------------------------

    /// Resolve an alias (or pinned variant id) to the best available device variant
    /// according to the ordered preference list.
    ///
    /// Walk `pref` in order; for each `Device`:
    /// - Skip `Npu` if `npu_enabled` is false or the OS has no NPU detected.
    /// - Pick the first variant whose id contains any of `dev.tokens()` AND (for NPU
    ///   only) whose `context_length()` is within `npu_ctx_cap`.
    ///
    /// If `alias` isn't a multi-variant catalog entry (e.g. it's already a pinned full
    /// variant id), fall back to `resolve()` which handles variant ids and surfaces a
    /// helpful error listing available aliases on a total miss.
    async fn resolve_variant(&self, alias: &str, pref: &[Device]) -> AppResult<Arc<Model>> {
        let catalog = self.manager.catalog();
        let base = match catalog.get_model(alias).await {
            Ok(m) => m,
            // Not a known alias — treat as a pinned variant id (or unknown); the
            // existing resolve() handles both and emits a good error on a miss.
            Err(_) => return self.resolve(alias).await,
        };

        let variants = base.variants();
        let has_npu = hardware::detect().iter().any(|d| d.kind == "NPU");

        for dev in pref {
            if *dev == Device::Npu && !(self.npu_enabled && has_npu) {
                // No NPU, or NPU disabled by config — skip NPU preference.
                continue;
            }
            let tokens = dev.tokens();
            for variant in &variants {
                let id = variant.id();
                if !tokens.iter().any(|t| id.contains(t)) {
                    continue;
                }
                // NPU-only guard: reject variants whose context window exceeds the cap.
                // Phi-4-mini on the NPU is 4224 tokens; larger windows OOM the NPU VRAM.
                if *dev == Device::Npu {
                    if let Some(ctx) = variant.context_length() {
                        if ctx > self.npu_ctx_cap {
                            continue;
                        }
                    }
                    // None = unspecified; treat as within cap (conservative).
                }
                return Ok(variant.clone());
            }
        }
        // Nothing in `pref` matched any variant — return the alias default (same as
        // `get_model` alone would give us).
        Ok(base)
    }

    /// Load a model, enforcing the GPU-class resident cap via LRU eviction.
    ///
    /// Only GPU-class models count against `max_resident_models`; NPU and CPU models
    /// are exempt because they live on separate silicon. This lets phi-4-mini stay hot
    /// on the NPU without displacing any GPU chat model.
    ///
    /// On cap: the LRU entry (oldest, index 0 in the vec) is unloaded before the new
    /// model is loaded. The caller's NPU-load-failure fallback is handled one level up
    /// in `generate_stream_with` / `complete_with`.
    async fn ensure_loaded_lru(&self, model: &Model) -> AppResult<()> {
        let id = model.id().to_string();
        let is_gpu = Device::Gpu.tokens().iter().any(|t| id.contains(t));

        if is_gpu {
            let victim = {
                let mut lru = self
                    .lru_residents
                    .lock()
                    .expect("lru-residents lock poisoned");
                // Remove existing entry so we can re-insert at MRU position regardless
                // of whether the model is already loaded (this also updates recency).
                lru.retain(|x| x != &id);
                // Only evict if we're still at or over the cap after removing ourselves.
                let victim = if lru.len() >= self.max_resident_models && !lru.is_empty() {
                    // Never evict a model with an in-flight generation — the native
                    // core fail-fasts (0xc0000409) if weights vanish mid-generate.
                    // Prefer the oldest non-busy resident; if every resident is busy,
                    // temporarily exceed the cap instead of crashing.
                    match lru.iter().position(|x| !is_busy(x)) {
                        Some(i) => Some(lru.remove(i)),
                        None => {
                            tracing::warn!(
                                "resident cap reached but every GPU resident is mid-generation; \
                                 skipping eviction (cap temporarily exceeded)"
                            );
                            None
                        }
                    }
                } else {
                    None
                };
                lru.push(id.clone()); // re-insert as MRU (back of vec)
                victim
            }; // lock released

            if let Some(victim_id) = victim {
                let catalog = self.manager.catalog();
                match catalog.get_model_variant(&victim_id).await {
                    Ok(victim_model) => {
                        let _gate = LOAD_GATE.lock().await;
                        if let Err(e) = victim_model.unload().await {
                            tracing::warn!(model = %victim_id, error = %e, "LRU eviction: unload failed (continuing anyway)");
                        } else {
                            tracing::info!(model = %victim_id, "LRU unloaded GPU-class resident");
                        }
                    }
                    Err(e) => tracing::warn!(
                        model = %victim_id, error = %e,
                        "LRU eviction: could not resolve victim; skipping unload"
                    ),
                }
            }
        }
        // Proceed with download-if-needed + load regardless of GPU/NPU/CPU class.
        ensure_loaded(model).await.map(|_| ())
    }

    // -------------------------------------------------------------------------
    // Phase 1 — spec-routed generation + thin wrappers
    // -------------------------------------------------------------------------

    /// Streaming generation against a fully-resolved `ModelSpec`.
    ///
    /// Resolves the spec's alias + device preference to a concrete variant, enforces
    /// the LRU cap, and opens a streaming chat completion with the spec's generation
    /// params. NPU load failure triggers a transparent CPU fallback before surfacing
    /// any error — the NPU might be busy or incompletely initialised at the time of
    /// the first call.
    pub async fn generate_stream_with(
        &self,
        spec: &ModelSpec,
        system: &str,
        user: &str,
    ) -> AppResult<GuardedChatStream> {
        let model = self.resolve_variant(&spec.alias, &spec.device_pref).await?;

        // NPU load-failure fallback: if we resolved an NPU variant but loading it fails,
        // retry with CPU so the request succeeds on hardware without a working NPU.
        let model = {
            let id = model.id().to_string();
            let is_npu = Device::Npu.tokens().iter().any(|t| id.contains(t));
            match self.ensure_loaded_lru(&model).await {
                Ok(()) => model,
                Err(e) if is_npu => {
                    tracing::warn!(
                        model = %id, error = %e,
                        "NPU load failed; falling back to CPU variant for this request"
                    );
                    let cpu = self.resolve_variant(&spec.alias, &[Device::Cpu]).await?;
                    self.ensure_loaded_lru(&cpu).await?;
                    cpu
                }
                Err(e) => return Err(e),
            }
        };

        let msgs = build_messages(system, user, spec.thinking);
        let client = {
            let c = model
                .create_chat_client()
                .temperature(spec.temperature as f64);
            if let Some(mt) = spec.max_tokens {
                c.max_tokens(mt)
            } else {
                c
            }
        };
        // Phase-3 tool seam: when the spec declares tools (HealthQuery, Trends,
        // PatientLookup, Extract, Verify, MultiHop), expose the run_aggregation tool
        // so the model can invoke structured data operations mid-stream.
        let tools = if spec.tools {
            Some(vec![run_aggregation_tool()])
        } else {
            None
        };
        let tools_ref: Option<&[ChatCompletionTools]> = tools.as_deref();
        let busy = BusyGuard::new(model.id().to_string());
        let inner = client
            .complete_streaming_chat(&msgs, tools_ref)
            .await
            .map_err(map_err)?;
        Ok(GuardedChatStream { inner, _busy: busy })
    }

    /// Non-streaming completion against a fully-resolved `ModelSpec`. Drains
    /// `generate_stream_with` and concatenates the tokens.
    pub async fn complete_with(
        &self,
        spec: &ModelSpec,
        system: &str,
        user: &str,
    ) -> AppResult<String> {
        use futures::StreamExt;
        let mut stream = self.generate_stream_with(spec, system, user).await?;
        let mut out = String::new();
        let mut think = think_filter::ThinkFilter::new();
        while let Some(chunk) = stream.next().await {
            let resp = chunk.map_err(map_err)?;
            if let Some(token) = resp.choices.first().and_then(|c| c.delta.content.clone()) {
                out.push_str(&think.push(&token));
            }
        }
        out.push_str(&think.finish());
        Ok(out)
    }

    /// Open a streaming chat completion against the current model with a system + user
    /// message pair. Backward-compatible wrapper over `generate_stream_with` using a
    /// default GPU spec — callers in `rag/routes.rs` are unchanged.
    pub async fn generate_stream(&self, system: &str, user: &str) -> AppResult<GuardedChatStream> {
        let spec = ModelSpec {
            alias: self.current_model(),
            thinking: false,
            temperature: 0.2,
            tools: false,
            device_pref: vec![Device::Gpu, Device::Cpu],
            max_tokens: None,
        };
        self.generate_stream_with(&spec, system, user).await
    }
}

/// Collect variant ids into a set for cheap membership tests.
fn id_set(models: Vec<Arc<Model>>) -> HashSet<String> {
    models.iter().map(|m| m.id().to_string()).collect()
}

// ---------------------------------------------------------------------------
// Phase 3 — tool-calling: run_aggregation tool definition + planner
// ---------------------------------------------------------------------------

/// Build the `run_aggregation` tool descriptor passed to `complete_chat`.
///
/// The JSON Schema in `parameters` constrains the model's output so it always
/// produces a valid `RunAggregation` object.  `strict: true` asks the engine
/// to enforce the schema — best-effort on small models but combined with the
/// `JsonSchema` response format and our post-parse validation it is sufficient.
pub fn run_aggregation_tool() -> ChatCompletionTools {
    ChatCompletionTools::Function(ChatCompletionTool {
        function: FunctionObject {
            name: "run_aggregation".to_string(),
            description: Some(
                "Execute a constrained aggregation over the health-records store and return \
                 chart-ready rows.  Call this whenever the question requires counting, \
                 grouping, summing, averaging, ranking, or trending health data."
                    .to_string(),
            ),
            parameters: Some(run_aggregation_schema()),
            strict: Some(true),
        },
    })
}

/// JSON Schema for `RunAggregation` — used both as the tool parameter schema
/// and as the `response_format: JsonSchema(…)` constraint in `plan_aggregation`.
pub fn run_aggregation_schema() -> serde_json::Value {
    serde_json::json!({
        "$schema": "http://json-schema.org/draft-07/schema#",
        "type": "object",
        "required": ["collection", "metric"],
        "properties": {
            "collection": {
                "type": "string",
                "description": "Logical collection name (e.g. 'records', 'encounters', 'patients', 'prescriptions', 'lab_orders', 'vital_signs')"
            },
            "filter": {
                "type": "object",
                "description": "Field-level filter. Keys are bare column names (no 'fields.' prefix). Values are plain scalars or operator objects. Use {\"$prefix\": \"E11\"} for ICD-code prefix matching.",
                "additionalProperties": true
            },
            "group_by": {
                "type": "array",
                "items": { "type": "string" },
                "description": "Column names to group by (no 'fields.' prefix)"
            },
            "metric": {
                "type": "object",
                "required": ["op"],
                "properties": {
                    "op": {
                        "type": "string",
                        "enum": ["count", "sum", "avg", "min", "max", "distinct"]
                    },
                    "field": {
                        "type": "string",
                        "description": "Column to aggregate (required for sum/avg/min/max/distinct)"
                    }
                }
            },
            "time_bucket": {
                "type": "object",
                "required": ["field", "unit"],
                "properties": {
                    "field": { "type": "string", "description": "Date/time column name" },
                    "unit": { "type": "string", "enum": ["day", "week", "month", "quarter", "year"] }
                }
            },
            "sort": {
                "type": "object",
                "required": ["by", "dir"],
                "properties": {
                    "by": { "type": "string", "description": "'value' or a dimension column name" },
                    "dir": { "type": "string", "enum": ["asc", "desc"] }
                }
            },
            "top_n": {
                "type": "integer",
                "minimum": 1,
                "maximum": 500,
                "description": "Maximum number of result rows"
            }
        }
    })
}

/// Build the `run_list_records` tool descriptor passed to `complete_chat`.
///
/// The schema mirrors `RunList` — collection name, optional JSON filter,
/// desired columns, sort, limit, and offset.  `strict: true` is best-effort
/// on small models; the planner's post-parse `validate_list` call is the
/// safety net that actually enforces the allow-list.
pub fn run_list_tool() -> ChatCompletionTools {
    ChatCompletionTools::Function(ChatCompletionTool {
        function: FunctionObject {
            name: "run_list_records".to_string(),
            description: Some(
                "Fetch a paginated list of individual records from the health-records store.  \
                 Call this when the user wants to enumerate, list, or display rows rather than \
                 compute an aggregate statistic."
                    .to_string(),
            ),
            parameters: Some(run_list_schema()),
            strict: Some(true),
        },
    })
}

/// JSON Schema for `RunList` — used as the tool parameter schema and as the
/// `response_format: JsonSchema(…)` constraint in `plan_list`.
pub fn run_list_schema() -> serde_json::Value {
    serde_json::json!({
        "$schema": "http://json-schema.org/draft-07/schema#",
        "type": "object",
        "required": ["collection"],
        "properties": {
            "collection": {
                "type": "string",
                "description": "Physical table name to list (e.g. 'patients', 'encounters', 'diagnoses')"
            },
            "filter": {
                "type": "object",
                "description": "Field-level filter (same semantics as run_aggregation). Keys are bare column names, no 'fields.' prefix.",
                "additionalProperties": true
            },
            "columns": {
                "type": "array",
                "items": { "type": "string" },
                "description": "Columns to return (bare names, no 'fields.' prefix). Omit for catalog defaults."
            },
            "sort": {
                "type": "object",
                "required": ["by", "dir"],
                "properties": {
                    "by": { "type": "string", "description": "Column name to sort by" },
                    "dir": { "type": "string", "enum": ["asc", "desc"] }
                }
            },
            "limit": {
                "type": "integer",
                "minimum": 1,
                "maximum": 200,
                "description": "Maximum rows to return (default 50, hard cap 200)"
            },
            "offset": {
                "type": "integer",
                "minimum": 0,
                "description": "Rows to skip for pagination (default 0)"
            }
        }
    })
}

/// Build the `classify_route` tool descriptor — the intent router's Tier-2 call.
///
/// Forces the small model (phi-4-mini) to emit exactly one routing label plus an
/// optional intent, rather than free-text the router would have to parse
/// loosely. `strict: true` is best-effort on small models; the router maps the
/// string fields leniently and falls open to semantic on any surprise.
pub fn classify_route_tool() -> ChatCompletionTools {
    ChatCompletionTools::Function(ChatCompletionTool {
        function: FunctionObject {
            name: "classify_route".to_string(),
            description: Some(
                "Classify how a user's message to a health-records assistant should be \
                 answered. Choose exactly one route: 'conversational' (a greeting, thanks, \
                 or a question about the assistant — no data needed), 'structured' (needs \
                 exact numbers: a count, a group-by, a ranking, or a trend over time), \
                 'semantic' (needs to read and summarise record text: explain, describe, \
                 summarise), or 'hybrid' (filter a cohort by an exact condition AND then \
                 summarise their records)."
                    .to_string(),
            ),
            parameters: Some(classify_route_schema()),
            strict: Some(true),
        },
    })
}

/// JSON Schema for the `classify_route` tool output. Mirrors
/// `router::RouteToolOutput`: a required `route` label plus an optional coarse
/// `intent`. Both string fields — the router parses them into enums leniently
/// so a stray label can never break routing.
pub fn classify_route_schema() -> serde_json::Value {
    serde_json::json!({
        "$schema": "http://json-schema.org/draft-07/schema#",
        "type": "object",
        "required": ["route"],
        "properties": {
            "route": {
                "type": "string",
                "enum": ["conversational", "structured", "semantic", "hybrid"],
                "description": "How to answer the message."
            },
            "intent": {
                "type": "string",
                "enum": ["lookup", "narrative", "aggregation", "trend", "enumeration", "multi_hop"],
                "description": "Finer-grained shape of a structured or hybrid query (optional)."
            }
        }
    })
}

/// Build the `extract_clinical` tool descriptor - the ingestion extractor's forced call.
///
/// The three arrays are split by vocabulary rather than carrying a `system` field per
/// term: which codeset applies is a property of the array, so the model never has to
/// choose one and cannot get it wrong. `ExtractedClinical::stamp_systems` fills the
/// field in afterwards.
pub fn extract_clinical_tool() -> ChatCompletionTools {
    ChatCompletionTools::Function(ChatCompletionTool {
        function: FunctionObject {
            name: "extract_clinical".to_string(),
            description: Some(
                "Record the clinical entities that literally appear in a health record's \
                 text: conditions (ICD-10), medications (RxNorm), and labs or observations \
                 (LOINC). Only report entities the text actually states."
                    .to_string(),
            ),
            parameters: Some(extract_clinical_schema()),
            strict: Some(true),
        },
    })
}

/// JSON Schema for `ExtractedClinical`. Each entity is `{text, code}` - `text` is the
/// wording from the note, `code` the standard code or an empty string when the model
/// is unsure. An empty code is a correct answer; a guessed one is a defect.
pub fn extract_clinical_schema() -> serde_json::Value {
    // One shared item shape for all three arrays, inlined per array rather than shared
    // via `$ref` because small models handle a flat schema far more reliably.
    let term = |code_desc: &str| {
        serde_json::json!({
            "type": "object",
            "required": ["text", "code"],
            "properties": {
                "text": {
                    "type": "string",
                    "description": "The entity exactly as it is worded in the record text."
                },
                "code": { "type": "string", "description": code_desc }
            }
        })
    };
    serde_json::json!({
        "$schema": "http://json-schema.org/draft-07/schema#",
        "type": "object",
        "required": ["conditions", "medications", "labs"],
        "properties": {
            "conditions": {
                "type": "array",
                "description": "Diagnoses and problems stated in the text.",
                "items": term("ICD-10 code, or an empty string if not known with confidence.")
            },
            "medications": {
                "type": "array",
                "description": "Drugs stated in the text.",
                "items": term("RxNorm code, or an empty string if not known with confidence.")
            },
            "labs": {
                "type": "array",
                "description": "Laboratory results, vitals, and observations stated in the text.",
                "items": term("LOINC code, or an empty string if not known with confidence.")
            }
        }
    })
}

/// Build the `verify_claims` tool descriptor - the faithfulness verifier's forced call.
///
/// Asking for a claim list rather than a single verdict is deliberate: a bare pass/fail
/// from a 3.8B model is noise, whereas per-claim rows with passage numbers can be shown
/// to the user and checked against the passages they already have.
pub fn verify_claims_tool() -> ChatCompletionTools {
    ChatCompletionTools::Function(ChatCompletionTool {
        function: FunctionObject {
            name: "verify_claims".to_string(),
            description: Some(
                "Record whether each factual clinical claim in an answer is supported by \
                 the numbered passages retrieved from the patient records. Judge support \
                 only from the passages, never from outside medical knowledge."
                    .to_string(),
            ),
            parameters: Some(verify_claims_schema()),
            strict: Some(true),
        },
    })
}

/// JSON Schema for `VerifyToolOutput`. Passage numbers are 1-based to match the
/// `[1]`-style citation markers the answer itself uses; `VerifyReport::from_claims`
/// discards any that fall outside the evidence actually supplied.
pub fn verify_claims_schema() -> serde_json::Value {
    serde_json::json!({
        "$schema": "http://json-schema.org/draft-07/schema#",
        "type": "object",
        "required": ["claims"],
        "properties": {
            "claims": {
                "type": "array",
                "description": "One entry per factual clinical claim in the answer.",
                "items": {
                    "type": "object",
                    "required": ["claim", "supported"],
                    "properties": {
                        "claim": {
                            "type": "string",
                            "description": "The claim, restated in one short sentence."
                        },
                        "supported": {
                            "type": "boolean",
                            "description": "True only if the passages state this claim."
                        },
                        "passages": {
                            "type": "array",
                            "items": { "type": "integer" },
                            "description": "1-based numbers of the passages that state the claim. Empty when unsupported."
                        }
                    }
                }
            }
        }
    })
}

impl FoundryManager {
    /// Classify a user message for the intent router (Tier 2). Asks the model to
    /// emit a `classify_route` tool call and returns the parsed
    /// [`RouteToolOutput`]. The caller (`router::route`) maps it to a concrete
    /// route and falls open to semantic on any error.
    pub async fn plan_route(&self, spec: &ModelSpec, question: &str) -> AppResult<RouteToolOutput> {
        let system = "You are the routing classifier for an on-premises health-records \
                      question-answering assistant. Given the user's latest message, call \
                      the classify_route tool with exactly one route. Prefer 'structured' \
                      whenever the answer is a number, a count, a breakdown, a ranking, or a \
                      trend. Use 'semantic' when the answer requires reading and summarising \
                      record text. Use 'conversational' only for greetings, thanks, or \
                      questions about the assistant itself. /no_think";
        // Skip the forced tool-call path: phi-4-mini's ONNX grammar compiler rejects
        // the classify_route schema outright ("Unsatisfiable schema"), which costs a
        // full request round-trip (~11s observed) before plan_tool's grammar-error
        // retry kicks in. Same fix already applied to plan_list — go straight to
        // validated JSON content instead of paying for a doomed grammar compile.
        self.plan_tool::<RouteToolOutput>(
            spec,
            system,
            question,
            "classify_route",
            classify_route_tool(),
            classify_route_schema(),
            false,
        )
        .await
    }

    /// Plan an aggregation as validated JSON content. Foundry Local's ONNX grammar
    /// compiler cannot compile this tool schema and may leave the native runtime
    /// unstable after rejecting it, so this path must never attempt a tool call.
    /// The caller MUST validate the returned spec before passing it to execution.
    pub async fn plan_aggregation(
        &self,
        spec: &ModelSpec,
        system: &str,
        user: &str,
    ) -> AppResult<RunAggregation> {
        self.plan_tool::<RunAggregation>(
            spec,
            system,
            user,
            "run_aggregation",
            run_aggregation_tool(),
            run_aggregation_schema(),
            false,
        )
        .await
    }

    /// Generate one SQL statement without constrained decoding. Some local ONNX
    /// variants cannot compile the `emit_sql` grammar, so the caller MUST enforce
    /// safety with `nl2sql::validate::validate_sql` before execution.
    pub async fn plan_sql(
        &self,
        spec: &ModelSpec,
        system: &str,
        user: &str,
    ) -> AppResult<EmitSqlOutput> {
        let raw = self.complete_with(spec, system, user).await?;
        Ok(EmitSqlOutput {
            sql: extract_sql_statement(&raw)?,
        })
    }

    /// Plan a list-records query as JSON content, avoiding Foundry's unsupported
    /// grammar for arbitrary filter properties. The caller MUST run
    /// `list::validate_list` before passing the result to `list::run`.
    pub async fn plan_list(
        &self,
        spec: &ModelSpec,
        system: &str,
        user: &str,
    ) -> AppResult<RunList> {
        self.plan_tool::<RunList>(
            spec,
            system,
            user,
            "run_list_records",
            run_list_tool(),
            run_list_schema(),
            false,
        )
        .await
    }

    /// Extract clinical entities from one record's text (`ModelRole::Extract`).
    ///
    /// The caller is `ingest::extract`, which stamps the code systems and discards an
    /// empty result. Errors are expected and handled there - a row that cannot be
    /// annotated is simply stored without an annotation.
    pub async fn plan_extraction(
        &self,
        spec: &ModelSpec,
        system: &str,
        user: &str,
    ) -> AppResult<ExtractedClinical> {
        self.plan_tool::<ExtractedClinical>(
            spec,
            system,
            user,
            "extract_clinical",
            extract_clinical_tool(),
            extract_clinical_schema(),
            true,
        )
        .await
    }

    /// Check an answer's claims against its retrieved passages (`ModelRole::Verify`).
    ///
    /// The caller is `verify::check`, which folds the per-claim verdicts into an
    /// overall status and falls open to `skipped` on any error.
    pub async fn plan_verification(
        &self,
        spec: &ModelSpec,
        system: &str,
        user: &str,
    ) -> AppResult<VerifyToolOutput> {
        self.plan_tool::<VerifyToolOutput>(
            spec,
            system,
            user,
            "verify_claims",
            verify_claims_tool(),
            verify_claims_schema(),
            true,
        )
        .await
    }

    /// Generic structured planner. Uses a forced tool call when its schema is
    /// compatible with Foundry's grammar compiler, otherwise requests JSON content.
    /// On first parse failure the request is reprompted once; a second failure
    /// returns `AppError::BadRequest`.
    async fn plan_tool<T: serde::de::DeserializeOwned>(
        &self,
        spec: &ModelSpec,
        system: &str,
        user: &str,
        tool_name: &str,
        tool: ChatCompletionTools,
        schema: serde_json::Value,
        prefer_tool_call: bool,
    ) -> AppResult<T> {
        // Resolve + load the model (same logic as generate_stream_with).
        let model = self.resolve_variant(&spec.alias, &spec.device_pref).await?;
        let model = {
            let id = model.id().to_string();
            let is_npu = Device::Npu.tokens().iter().any(|t| id.contains(t));
            match self.ensure_loaded_lru(&model).await {
                Ok(()) => model,
                Err(e) if is_npu => {
                    tracing::warn!(
                        model = %id, error = %e,
                        "NPU load failed; falling back to CPU variant for plan_tool"
                    );
                    let cpu = self.resolve_variant(&spec.alias, &[Device::Cpu]).await?;
                    self.ensure_loaded_lru(&cpu).await?;
                    cpu
                }
                Err(e) => return Err(e),
            }
        };
        // Keep the model marked busy across both completion calls so it can't be
        // evicted/unloaded mid-generation (native core fail-fasts on that).
        let _busy = BusyGuard::new(model.id().to_string());

        let schema_str = schema.to_string();
        let msgs = build_messages(system, user, spec.thinking);

        // A forced tool and a bare response schema describe incompatible envelopes on
        // ORT-GenAI, so never combine them. Schemas with arbitrary object properties
        // (notably list filters) are known to be rejected by its grammar compiler and
        // go directly through JSON content plus mandatory post-parse validation.
        let json_prompt = || {
            format!(
                "{user}\n\nReturn ONLY the JSON object for `{tool_name}`. No markdown or explanation.\nJSON schema: {schema_str}"
            )
        };
        let resp = if prefer_tool_call {
            let tool_client = model
                .create_chat_client()
                .temperature(spec.temperature as f64)
                .tool_choice(ChatToolChoice::Function(tool_name.to_string()));
            match tool_client.complete_chat(&msgs, Some(&[tool])).await {
                Ok(response) => response,
                Err(error) if is_grammar_error(&error) => {
                    tracing::warn!(
                        tool = tool_name,
                        error = %error,
                        "backend rejected tool grammar; retrying as validated JSON content"
                    );
                    let fallback_msgs = build_messages(system, &json_prompt(), spec.thinking);
                    model
                        .create_chat_client()
                        .temperature(0.0)
                        .complete_chat(&fallback_msgs, None)
                        .await
                        .map_err(map_err)?
                }
                Err(error) => return Err(map_err(error)),
            }
        } else {
            let json_msgs = build_messages(system, &json_prompt(), spec.thinking);
            model
                .create_chat_client()
                .temperature(0.0)
                .complete_chat(&json_msgs, None)
                .await
                .map_err(map_err)?
        };

        match try_parse_tool::<T>(&resp) {
            Ok(result) => return Ok(result),
            Err(parse_err) => {
                tracing::warn!(tool = tool_name, error = %parse_err, "plan_tool: first parse failed; reprompting");
                let reprompt = format!(
                    "{user}\n\nYour previous response could not be parsed. Return ONLY a valid JSON object for `{tool_name}`. No markdown or explanation.\nJSON schema: {schema_str}"
                );
                let msgs2 = build_messages(system, &reprompt, spec.thinking);
                let resp2 = model
                    .create_chat_client()
                    .temperature(0.0)
                    .complete_chat(&msgs2, None)
                    .await
                    .map_err(map_err)?;
                try_parse_tool::<T>(&resp2).map_err(|e| {
                    AppError::BadRequest(format!(
                        "{tool_name} planner could not produce a valid spec after retry: {e}"
                    ))
                })
            }
        }
    }
}

/// Generic tool-call response parser. Priority: `tool_calls[0].function.arguments`
/// then `message.content` (for models that return JSON text directly).
fn try_parse_tool<T: serde::de::DeserializeOwned>(
    resp: &foundry_local_sdk::CreateChatCompletionResponse,
) -> Result<T, String> {
    let first = resp
        .choices
        .first()
        .ok_or_else(|| "model returned no choices".to_string())?;

    // 1. Try tool_calls
    if let Some(tcs) = &first.message.tool_calls {
        for tc in tcs {
            if let ChatCompletionMessageToolCalls::Function(call) = tc {
                match serde_json::from_str::<T>(&call.function.arguments) {
                    Ok(result) => return Ok(result),
                    Err(e) => {
                        return Err(format!(
                            "tool_call arguments parse failed: {e}; raw={}",
                            &call.function.arguments
                        ));
                    }
                }
            }
        }
    }

    // 2. Fall back to message content. Qwen3 may emit an empty reasoning block
    // even under `/no_think`, so normalize that envelope before deserializing.
    let content = first
        .message
        .content
        .as_deref()
        .ok_or_else(|| "no tool_calls and no content in response".to_string())?;
    let json = normalize_json_content(content)?;

    serde_json::from_str::<T>(&json).map_err(|e| format!("content parse failed: {e}; raw={json}"))
}

fn normalize_json_content(content: &str) -> Result<String, String> {
    let content = content.trim_start_matches('\u{feff}');
    let mut think = think_filter::ThinkFilter::new();
    let mut visible = think.push(content);
    visible.push_str(&think.finish());

    let trimmed = visible.trim();
    let json = if let Some(rest) = trimmed
        .strip_prefix("```json")
        .or_else(|| trimmed.strip_prefix("```JSON"))
        .or_else(|| trimmed.strip_prefix("```"))
    {
        rest.trim()
            .strip_suffix("```")
            .map(str::trim)
            .ok_or_else(|| "JSON markdown fence is not closed".to_string())?
    } else {
        trimmed
    };

    if !json.starts_with('{') || !json.ends_with('}') {
        return Err("structured response must contain only one JSON object".to_string());
    }
    Ok(json.to_string())
}

fn extract_sql_statement(raw: &str) -> AppResult<String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Err(AppError::BadRequest(
            "SQL planner returned an empty response".into(),
        ));
    }

    let sql = if trimmed.starts_with("```") {
        let mut lines = trimmed.lines();
        let opening = lines.next().unwrap_or_default();
        let language = opening.trim_start_matches("```").trim();
        if !language.is_empty() && !language.eq_ignore_ascii_case("sql") {
            return Err(AppError::BadRequest(
                "SQL planner returned an unsupported code block".into(),
            ));
        }

        let mut body = Vec::new();
        let mut closed = false;
        for line in lines {
            if line.trim() == "```" {
                closed = true;
                continue;
            }
            if closed && !line.trim().is_empty() {
                return Err(AppError::BadRequest(
                    "SQL planner returned text outside the SQL block".into(),
                ));
            }
            if !closed {
                body.push(line);
            }
        }
        if !closed {
            return Err(AppError::BadRequest(
                "SQL planner returned an unterminated SQL block".into(),
            ));
        }
        body.join("\n").trim().to_string()
    } else {
        trimmed.to_string()
    };

    if sql.is_empty() {
        return Err(AppError::BadRequest(
            "SQL planner returned an empty statement".into(),
        ));
    }
    Ok(sql)
}

/// Build the system + user messages for a chat completion, applying the Qwen3 soft
/// chain-of-thought switch when `thinking` is false.
///
/// Qwen3 reads `/no_think` in the system prompt as a directive to skip the `<think>`
/// reasoning block, saving tokens and avoiding leaking internal deliberation to the
/// client. When `thinking` is true (e.g. Trends, MultiHop), we leave the system
/// prompt unmodified so the model can use the full CoT budget.
///
/// Stripping of `<think>…</think>` blocks now happens in the route token loops via
/// `ThinkFilter` (`think_filter.rs`), so these messages need no post-processing here.
fn build_messages(system: &str, user: &str, thinking: bool) -> Vec<ChatCompletionRequestMessage> {
    let system_content: String = if thinking {
        system.to_string()
    } else {
        format!("{system}\n/no_think")
    };
    vec![
        ChatCompletionRequestMessage::System(ChatCompletionRequestSystemMessage {
            content: system_content.into(),
            name: None,
        }),
        ChatCompletionRequestMessage::User(ChatCompletionRequestUserMessage {
            content: user.into(),
            name: None,
        }),
    ]
}

/// Ensure a model is cached (download if not) and loaded into memory.
/// Returns `true` when a corrupt cache had to be purged and re-downloaded.
async fn ensure_loaded(model: &Model) -> AppResult<bool> {
    if model.is_loaded().await.map_err(map_err)? {
        return Ok(false);
    }
    if !model.is_cached().await.map_err(map_err)? {
        tracing::info!(model = model.id(), "downloading model (not cached)");
        model.download(None::<fn(f64)>).await.map_err(map_err)?;
    }
    {
        let _gate = LOAD_GATE.lock().await;
        // Re-check under the gate: a queued waiter may find its model already resident.
        if model.is_loaded().await.map_err(map_err)? {
            return Ok(false);
        }
        match model.load().await {
            Ok(()) => return Ok(false),
            Err(e) if is_corrupt_weights_error(&e.to_string()) => {
                tracing::warn!(model = model.id(), error = %e, "load hit corrupt cached weights; purging and re-downloading");
            }
            Err(e) => return Err(map_err(e)),
        }
    } // gate released during the (long) re-download
    heal_and_load(model, None::<fn(f64)>).await?;
    Ok(true)
}

/// Signature of a load failure caused by corrupt/truncated cached weights (e.g. a
/// crash mid-download that Foundry still counts as "cached") — as opposed to
/// transient/resource errors, which must NOT trigger a multi-GB re-download.
fn is_corrupt_weights_error(msg: &str) -> bool {
    let m = msg.to_lowercase();
    m.contains("error parsing")          // OpenVINO IR parse failure
        || m.contains("pugi::")          // OpenVINO XML reader
        || m.contains("input_model")     // openvino frontends/ir/input_model.cpp
        || m.contains("protobuf parsing") // ONNX deserialization failure
        || m.contains("invalid model")
        || m.contains("no such file") // partially-written cache dir
}

/// Recovery path: purge the variant's cached files, re-download (optionally with
/// progress), and retry the load once under the gate. A second failure surfaces.
async fn heal_and_load(
    model: &Model,
    progress: Option<impl Fn(f64) + Send + Sync + 'static>,
) -> AppResult<()> {
    model.remove_from_cache().await.map_err(map_err)?;
    tracing::info!(model = model.id(), "re-downloading model after cache purge");
    model.download(progress).await.map_err(map_err)?;
    let _gate = LOAD_GATE.lock().await;
    model.load().await.map_err(map_err)?;
    tracing::info!(
        model = model.id(),
        "recovered: model re-downloaded and loaded"
    );
    Ok(())
}

#[cfg(test)]
mod sql_response_tests {
    use super::{extract_sql_statement, normalize_json_content};

    #[test]
    fn normalizes_qwen_thinking_before_json() {
        let raw = r#"<think>

</think>

{
  "collection": "patients",
  "columns": ["id", "first_name"],
  "limit": 50,
  "offset": 0
}"#;
        let json = normalize_json_content(raw).unwrap();
        let value: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(value["collection"], "patients");
        assert_eq!(value["limit"], 50);
    }

    #[test]
    fn normalizes_fenced_json() {
        let json = normalize_json_content("```json\n{\"collection\":\"patients\"}\n```").unwrap();
        assert_eq!(json, r#"{"collection":"patients"}"#);
    }

    #[test]
    fn rejects_commentary_and_malformed_json_envelopes() {
        assert!(
            normalize_json_content("Here is the result: {\"collection\":\"patients\"}").is_err()
        );
        assert!(normalize_json_content("```json\n{\"collection\":\"patients\"}").is_err());
        assert!(normalize_json_content("{\"collection\":\"patients\"} done").is_err());
    }

    #[test]
    fn accepts_plain_sql() {
        let sql = extract_sql_statement(" SELECT COUNT(*) FROM patients; ").unwrap();
        assert_eq!(sql, "SELECT COUNT(*) FROM patients;");
    }

    #[test]
    fn accepts_fenced_sql() {
        let sql = extract_sql_statement("```sql\nSELECT id FROM patients\nLIMIT 5;\n```").unwrap();
        assert_eq!(sql, "SELECT id FROM patients\nLIMIT 5;");
    }

    #[test]
    fn rejects_commentary_outside_fence() {
        assert!(extract_sql_statement("```sql\nSELECT 1;\n```\nDone").is_err());
    }
}
