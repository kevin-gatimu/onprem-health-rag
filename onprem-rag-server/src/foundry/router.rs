//! Task-aware model router: `ModelRole` → `ModelSpec`.
//!
//! The router is stateless — it maps a *model role* (what a model call is for) to a
//! spec (alias + device preference + generation params). The `FoundryManager` resolves
//! the spec to a concrete device variant at call time (`resolve_variant`,
//! `ensure_loaded_lru`). Keeping routing separate from loading makes each
//! independently testable and lets the routing table evolve without touching the
//! load/LRU machinery.
//!
//! `ModelRole` is deliberately *not* the product identity. Which agent the user is
//! talking to lives in `agents::kind::AgentKind`; this enum only says what a given
//! model call is being asked to do. Before plan 05 the two were fused into one
//! overloaded `AgentKind`, which meant adding a hospital service line implied adding
//! a model role.

use serde::{Deserialize, Serialize};

use crate::config::Config;

/// What a single model call is for. Controls model selection, device placement, and
/// generation params (temperature, thinking mode, tool use).
///
/// `snake_case` serialization matches the `role` ids used by `GET /models/roles` and
/// the persisted override keys in `settings.rs`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ModelRole {
    /// Grounded conversational answer over retrieved passages.
    Grounded,
    /// Narrate already-computed rows (aggregation results, record lists).
    Narrate,
    /// History-aware query rewrite and multi-query expansion.
    Rewrite,
    /// Intent classification (router Tier 2) — forced tool call, deterministic.
    Classify,
    /// Clinical entity extraction from free text at ingest.
    Extract,
    /// Post-answer faithfulness verification.
    Verify,
    /// NL-to-SQL generation: deterministic, plain SQL output.
    TextToSql,
    /// Aggregation / `QuerySpec` planning — emits a spec, never prose.
    PlanSpec,
    /// Conversation-history compaction.
    Compact,
}

impl ModelRole {
    /// Every role, for exhaustive tests and admin listings.
    pub const ALL: &'static [ModelRole] = &[
        ModelRole::Grounded,
        ModelRole::Narrate,
        ModelRole::Rewrite,
        ModelRole::Classify,
        ModelRole::Extract,
        ModelRole::Verify,
        ModelRole::TextToSql,
        ModelRole::PlanSpec,
        ModelRole::Compact,
    ];
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
/// Produced by `ModelSpec::for_role`; consumed by `FoundryManager::generate_stream_with`
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
    /// Build a `ModelSpec` for a model role, reading model aliases from `cfg`.
    ///
    /// Temperature table (plan 05 §1): Grounded 0.3, Narrate 0.2, Rewrite 0.1,
    /// Classify 0.0, TextToSql 0.0, PlanSpec 0.0, Extract 0.1, Verify 0.1, Compact 0.2.
    /// Thinking is off for every role by default; `AgentMode::Trends` turns it on for
    /// `Narrate` at the call site, which is the only place the mode is known.
    ///
    /// Device placement rationale: all roles use one shared Qwen GPU variant so the
    /// resident cap bounds memory without cross-family model swaps.
    pub fn for_role(role: ModelRole, cfg: &Config) -> ModelSpec {
        let r = &cfg.router;
        match role {
            ModelRole::Grounded => ModelSpec {
                alias: r.chat.clone(),
                thinking: false,
                temperature: 0.3,
                tools: false,
                device_pref: vec![Device::Gpu],
                max_tokens: None,
            },
            ModelRole::Narrate => ModelSpec {
                alias: r.summarize.clone(),
                thinking: false,
                temperature: 0.2,
                tools: false,
                device_pref: vec![Device::Gpu],
                max_tokens: None,
            },
            // Rewrite is a short-output fast-lane task: low temperature for
            // determinism, no thinking.
            ModelRole::Rewrite => ModelSpec {
                alias: r.fast.clone(),
                thinking: false,
                temperature: 0.1,
                tools: false,
                device_pref: vec![Device::Gpu],
                max_tokens: None,
            },
            // Classification shares the same GPU resident as every other role.
            ModelRole::Classify => ModelSpec {
                alias: r.classify.clone(),
                thinking: false,
                temperature: 0.0,
                tools: true,
                device_pref: vec![Device::Gpu],
                max_tokens: Some(128),
            },
            ModelRole::Extract => ModelSpec {
                alias: r.extractor.clone(),
                thinking: false,
                temperature: 0.1,
                tools: true,
                device_pref: vec![Device::Gpu],
                max_tokens: None,
            },
            ModelRole::Verify => ModelSpec {
                alias: r.verifier.clone(),
                thinking: false,
                temperature: 0.1,
                tools: true,
                device_pref: vec![Device::Gpu],
                max_tokens: None,
            },
            // SQL is emitted directly rather than through constrained tool grammar,
            // which is unsupported by some local ONNX model variants.
            ModelRole::TextToSql => ModelSpec {
                alias: r.sql_model.clone(),
                thinking: false,
                temperature: 0.0,
                tools: false,
                device_pref: vec![Device::Gpu],
                max_tokens: Some(256),
            },
            // Spec planning is schema-bound and deterministic — the same discipline as
            // SQL — but the output is a tool call, so `tools` stays on.
            ModelRole::PlanSpec => ModelSpec {
                alias: r.plan_spec.clone(),
                thinking: false,
                temperature: 0.0,
                tools: true,
                device_pref: vec![Device::Gpu],
                max_tokens: None,
            },
            ModelRole::Compact => ModelSpec {
                alias: r.fast.clone(),
                thinking: false,
                temperature: 0.2,
                tools: false,
                device_pref: vec![Device::Gpu],
                max_tokens: None,
            },
        }
    }
}

/// The settings/override key for a role. One shared Core LLM serves every generative
/// role, so every role resolves through the single `"chat"` override — stale per-role
/// overrides persisted before the unification are deliberately ignored, otherwise a
/// divergent role (e.g. `text_to_sql` pinned at an older model) forces an LRU swap
/// and a multi-second cold start on nearly every request.
pub fn override_key(_role: ModelRole) -> &'static str {
    "chat"
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
    fn extract_prefers_gpu() {
        let cfg = test_cfg();
        let spec = ModelSpec::for_role(ModelRole::Extract, &cfg);
        assert_eq!(spec.device_pref, vec![Device::Gpu]);
        assert_eq!(spec.alias, cfg.router.extractor);
        assert!(!spec.thinking, "Extract should not use chain-of-thought");
    }

    #[test]
    fn grounded_prefers_gpu_only() {
        let cfg = test_cfg();
        let spec = ModelSpec::for_role(ModelRole::Grounded, &cfg);
        assert_eq!(spec.device_pref, vec![Device::Gpu]);
        assert_eq!(spec.alias, cfg.router.chat);
    }

    #[test]
    fn verify_matches_extract_placement() {
        let cfg = test_cfg();
        let spec = ModelSpec::for_role(ModelRole::Verify, &cfg);
        assert_eq!(spec.device_pref, vec![Device::Gpu]);
    }

    /// Plan 05 §1: the per-role temperature table is a contract other modules rely on.
    #[test]
    fn role_temperature_table_matches_plan() {
        let cfg = test_cfg();
        let expected = [
            (ModelRole::Grounded, 0.3_f32),
            (ModelRole::Narrate, 0.2),
            (ModelRole::Rewrite, 0.1),
            (ModelRole::Classify, 0.0),
            (ModelRole::TextToSql, 0.0),
            (ModelRole::PlanSpec, 0.0),
            (ModelRole::Extract, 0.1),
            (ModelRole::Verify, 0.1),
            (ModelRole::Compact, 0.2),
        ];
        for (role, temp) in expected {
            let spec = ModelSpec::for_role(role, &cfg);
            assert!(
                (spec.temperature - temp).abs() < f32::EPSILON,
                "{role:?} temperature should be {temp}, got {}",
                spec.temperature
            );
            assert!(!spec.thinking, "{role:?} should default to thinking off");
        }
        assert_eq!(
            ModelRole::ALL.len(),
            expected.len(),
            "every ModelRole needs a temperature assertion"
        );
    }

    /// PlanSpec follows the SQL model unless explicitly overridden.
    #[test]
    fn plan_spec_defaults_to_sql_model() {
        let cfg = test_cfg();
        let spec = ModelSpec::for_role(ModelRole::PlanSpec, &cfg);
        assert_eq!(spec.alias, cfg.router.sql_model);
    }

    /// Every role resolves through the single shared override key.
    #[test]
    fn every_role_uses_the_chat_override_key() {
        for role in ModelRole::ALL {
            assert_eq!(override_key(*role), "chat");
        }
    }
}
