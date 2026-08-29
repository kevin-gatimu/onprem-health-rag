//! Task-aware model router: `AgentKind` → `ModelSpec`.
//!
//! The router is stateless — it maps a task kind to a spec (alias + device preference
//! + generation params). The `FoundryManager` resolves the spec to a concrete device
//! variant at call time (`resolve_variant`, `ensure_loaded_lru`). Keeping routing
//! separate from loading makes each independently testable and lets the routing table
//! evolve without touching the load/LRU machinery.

use serde::{Deserialize, Serialize};

use crate::config::Config;

/// The kind of agent task being performed. Controls model selection, device placement,
/// and generation params (temperature, thinking mode, tool use).
/// `snake_case` serialization matches the `POST /agents/<kind>` URL segment (Phase 3).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentKind {
    PatientLookup,
    HealthQuery,
    Trends,
    Summarize,
    Chat,
    MultiHop,
    QueryRewrite,
    Classify,
    Extract,
    Verify,
}

/// Coarse accelerator target used to select a device variant at load time.
/// The router's `device_pref` is an ordered list; `resolve_variant` tries each in
/// order and picks the first matching variant.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Device {
    Npu,
    Gpu,
    Cpu,
}

impl Device {
    /// Substrings matched against Foundry variant ids to identify the device class.
    /// Matching is case-sensitive substring (e.g. `"generic-gpu"` in
    /// `"qwen3-8b-generic-gpu:3"`). The slice is checked with `any`, so ordering
    /// within a slice doesn't affect which variant wins — it only matters which
    /// `Device` appears first in `device_pref`.
    pub fn tokens(self) -> &'static [&'static str] {
        match self {
            Device::Npu => &["openvino-npu", "qnn-npu", "-npu"],
            Device::Gpu => &["openvino-gpu", "generic-gpu", "cuda-gpu", "-gpu"],
            Device::Cpu => &["generic-cpu", "-cpu"],
        }
    }
}

/// Per-call generation spec: which model to use, on which device, with which params.
/// Produced by `ModelSpec::for_kind`; consumed by `FoundryManager::generate_stream_with`
/// and `FoundryManager::complete_with`.
#[derive(Debug, Clone)]
pub struct ModelSpec {
    /// Model alias (e.g. `"qwen3-8b"`) or pinned variant id. Resolved to a concrete
    /// variant by `resolve_variant` at call time, honouring `device_pref`.
    pub alias: String,
    /// Enable Qwen3 chain-of-thought. When `false`, `/no_think` is appended to the
    /// system prompt to suppress reasoning traces and save tokens on tasks that don't
    /// need deliberate reasoning.
    pub thinking: bool,
    pub temperature: f32,
    /// Whether this spec intends to use tool-calling (reserved for Phase 3).
    /// Phase 1/2 always passes `None` tools to `complete_streaming_chat`; this field
    /// is carried here so Phase 3 can read it without changing the spec shape.
    pub tools: bool,
    /// Ordered device preference. `resolve_variant` walks this list and returns the
    /// first matching variant. `[Gpu]` = GPU-only; `[Npu, Cpu]` = NPU with CPU
    /// fallback; `[Gpu, Cpu]` = GPU with CPU fallback for the fast lane.
    pub device_pref: Vec<Device>,
    /// Optional max-tokens cap passed to `ChatClient::max_tokens`. `None` leaves it
    /// at the model's default context window, which is correct for most tasks.
    pub max_tokens: Option<u32>,
}

impl ModelSpec {
    /// Build a `ModelSpec` for a given agent kind, reading model aliases from `cfg`.
    ///
    /// Device placement rationale:
    /// - `[Npu, Cpu]` only for `Extract`/`Verify` — small single-record jobs that fit
    ///   the NPU's 4224-token cap and free the iGPU for concurrent chat.
    /// - `[Gpu, Cpu]` for the fast lane (QueryRewrite/Classify/PatientLookup) — GPU
    ///   preferred but CPU is an acceptable fallback for short outputs.
    /// - `[Gpu]` for everything else — context-heavy tasks need the full iGPU window;
    ///   NPU placement is structurally impossible here (router never sets it).
    pub fn for_kind(kind: AgentKind, cfg: &Config) -> ModelSpec {
        let r = &cfg.router;
        match kind {
            AgentKind::PatientLookup => ModelSpec {
                alias: r.lookup.clone(),
                thinking: false,
                temperature: 0.1,
                tools: true,
                device_pref: vec![Device::Gpu, Device::Cpu],
                max_tokens: None,
            },
            AgentKind::HealthQuery => ModelSpec {
                alias: r.health_query.clone(),
                thinking: false,
                temperature: 0.2,
                tools: true,
                device_pref: vec![Device::Gpu],
                max_tokens: None,
            },
            AgentKind::Trends => ModelSpec {
                alias: r.trends.clone(),
                thinking: true,
                temperature: 0.3,
                tools: true,
                device_pref: vec![Device::Gpu],
                max_tokens: None,
            },
            AgentKind::Summarize => ModelSpec {
                alias: r.summarize.clone(),
                thinking: false,
                temperature: 0.2,
                tools: false,
                device_pref: vec![Device::Gpu],
                max_tokens: None,
            },
            AgentKind::Chat => ModelSpec {
                alias: r.chat.clone(),
                thinking: false,
                temperature: 0.3,
                tools: false,
                device_pref: vec![Device::Gpu],
                max_tokens: None,
            },
            AgentKind::MultiHop => ModelSpec {
                alias: r.chat.clone(),
                thinking: true,
                temperature: 0.3,
                tools: true,
                device_pref: vec![Device::Gpu],
                max_tokens: None,
            },
            // QueryRewrite is a short-output fast-lane task: low temperature for
            // determinism, no thinking, GPU preferred but CPU is fine as a fallback.
            AgentKind::QueryRewrite => ModelSpec {
                alias: r.fast.clone(),
                thinking: false,
                temperature: 0.1,
                tools: false,
                device_pref: vec![Device::Gpu, Device::Cpu],
                max_tokens: None,
            },
            // Classify is the intent router's Tier-2 tool call: phi-4-mini on the NPU
            // (CPU fallback) so it stays hot beside the GPU chat model. Deterministic
            // (temp 0.0), tool-calling on, tiny output — a single small JSON object.
            AgentKind::Classify => ModelSpec {
                alias: r.classify.clone(),
                thinking: false,
                temperature: 0.0,
                tools: true,
                device_pref: vec![Device::Npu, Device::Cpu],
                max_tokens: Some(128),
            },
            AgentKind::Extract => ModelSpec {
                alias: r.extractor.clone(),
                thinking: false,
                temperature: 0.1,
                tools: true,
                // Small single-record extraction: prefer the NPU to keep the iGPU free
                // for concurrent chat. CPU is the fallback when no NPU is present or if
                // the NPU load fails (load-failure fallback in FoundryManager).
                device_pref: vec![Device::Npu, Device::Cpu],
                max_tokens: None,
            },
            AgentKind::Verify => ModelSpec {
                alias: r.verifier.clone(),
                thinking: false,
                temperature: 0.1,
                tools: true,
                device_pref: vec![Device::Npu, Device::Cpu],
                max_tokens: None,
            },
        }
    }
}

/// The settings/override key for a kind (matches the role keys in `GET /models/roles`
/// and the ones persisted by `PUT /settings/router`). `Chat` and `MultiHop` share the
/// `"chat"` role since they're both the conversational path; `QueryRewrite` keeps the
/// `"fast"` fast-lane role while `Classify` gets its own `"classify"` role (the intent
/// router's Tier-2 model, placed on the NPU).
pub fn override_key(kind: AgentKind) -> &'static str {
    match kind {
        AgentKind::Chat | AgentKind::MultiHop => "chat",
        AgentKind::HealthQuery => "health_query",
        AgentKind::Trends => "trends",
        AgentKind::Summarize => "summarize",
        AgentKind::PatientLookup => "lookup",
        AgentKind::QueryRewrite => "fast",
        AgentKind::Classify => "classify",
        AgentKind::Extract => "extractor",
        AgentKind::Verify => "verifier",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;

    fn test_cfg() -> Config {
        // from_env() uses defaults when vars are absent — no .env required for unit tests.
        Config::from_env()
    }

    #[test]
    fn extract_prefers_npu_then_cpu() {
        let cfg = test_cfg();
        let spec = ModelSpec::for_kind(AgentKind::Extract, &cfg);
        assert_eq!(spec.device_pref, vec![Device::Npu, Device::Cpu]);
        assert_eq!(spec.alias, cfg.router.extractor);
        assert!(!spec.thinking, "Extract should not use chain-of-thought");
    }

    #[test]
    fn chat_prefers_gpu_only() {
        let cfg = test_cfg();
        let spec = ModelSpec::for_kind(AgentKind::Chat, &cfg);
        assert_eq!(spec.device_pref, vec![Device::Gpu]);
        assert_eq!(spec.alias, cfg.router.chat);
    }

    #[test]
    fn verify_matches_extract_placement() {
        let cfg = test_cfg();
        let spec = ModelSpec::for_kind(AgentKind::Verify, &cfg);
        assert_eq!(spec.device_pref, vec![Device::Npu, Device::Cpu]);
    }

    #[test]
    fn trends_uses_thinking() {
        let cfg = test_cfg();
        let spec = ModelSpec::for_kind(AgentKind::Trends, &cfg);
        assert!(spec.thinking);
    }
}
