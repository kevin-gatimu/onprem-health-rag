//! Server configuration, loaded from the environment (prefix `ONPREM_`).
//!
//! Values come from real environment variables; in development a root `.env` file is
//! loaded first by `dotenvy` (see `main.rs`). See `.env.example` for the full list.

use std::env;

/// Retrieval mode for the RAG pipeline.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RetrievalMode {
    Vector,
    Hybrid,
}

impl RetrievalMode {
    fn parse(s: &str) -> Self {
        match s.trim().to_ascii_lowercase().as_str() {
            "vector" => RetrievalMode::Vector,
            _ => RetrievalMode::Hybrid,
        }
    }
}

/// Per-role model aliases and device-placement knobs for the task-aware model router.
/// All values come from environment variables; defaults aim at a reasonable on-prem
/// hardware profile (NPU for small single-record roles, GPU for everything else).
#[derive(Debug, Clone)]
pub struct RouterConfig {
    pub chat: String,           // ONPREM_MODEL_CHAT
    pub health_query: String,   // ONPREM_MODEL_HEALTH_QUERY
    pub trends: String,         // ONPREM_MODEL_TRENDS
    pub summarize: String,      // ONPREM_MODEL_SUMMARIZE
    pub lookup: String,         // ONPREM_MODEL_LOOKUP
    pub fast: String,           // ONPREM_MODEL_FAST  (QueryRewrite)
    pub classify: String,       // ONPREM_MODEL_CLASSIFY  (intent router Tier 2)
    pub extractor: String,      // ONPREM_MODEL_EXTRACTOR
    pub verifier: String,       // ONPREM_MODEL_VERIFIER
    /// Maximum number of GPU-class models resident in memory simultaneously.
    /// NPU/CPU models are exempt — they live on separate silicon and keeping
    /// phi-4-mini hot on the NPU is the whole point of the device-placement design.
    pub max_resident_models: usize, // ONPREM_MAX_RESIDENT_MODELS
    /// Whether to attempt NPU placement for eligible roles (Extract, Verify, Classify).
    /// Set false to force CPU for those roles even when an NPU is detected.
    pub npu_enabled: bool,          // ONPREM_NPU_ENABLED
    /// NPU context-length cap in tokens. Variants whose `context_length()` exceeds this
    /// are skipped during NPU selection — prevents OOM on the NPU's constrained VRAM.
    pub npu_ctx_cap: u64,           // ONPREM_NPU_CTX_CAP
    /// Whether the intent router may escalate ambiguous questions to the Tier-2
    /// model classifier. When false the router uses Tiers 0/1 only and falls open
    /// to semantic — no model call, useful when phi-4-mini isn't available.
    pub model_router_enabled: bool, // ONPREM_ROUTER_MODEL_ENABLED
    /// Capacity of the per-process Tier-2 route-decision LRU cache (entries).
    pub router_cache_size: usize,   // ONPREM_ROUTER_CACHE_SIZE

    // --- NL-to-SQL (plan 18) ---
    /// Master switch: when false the router never selects `SourceSql` and the
    /// `/nl2sql/<id>` endpoints still work but are direct-call only (no routing).
    pub text2sql_enabled: bool,      // ONPREM_TEXT2SQL_ENABLED
    /// Model alias for SQL generation (phi-4-mini-instruct works well).
    pub sql_model: String,           // ONPREM_MODEL_TEXT2SQL
    /// Maximum schema cards (table cards) injected into the generation prompt.
    pub nl2sql_tables_max: usize,    // ONPREM_NL2SQL_TABLES_MAX
    /// Few-shot examples per prompt (retrieved by question-vector similarity).
    pub nl2sql_fewshots: usize,      // ONPREM_NL2SQL_FEWSHOTS
    /// Hard cap on rows returned by a SQL query (injected as LIMIT/TOP).
    pub nl2sql_max_rows: i64,        // ONPREM_NL2SQL_MAX_ROWS
    /// Query-level timeout in seconds; queries that exceed it are killed.
    pub nl2sql_timeout_secs: u64,    // ONPREM_NL2SQL_TIMEOUT_SECS
    /// Per-column sample-value count for schema cards (0 disables sampling).
    pub nl2sql_sample_values: usize, // ONPREM_NL2SQL_SAMPLE_VALUES
}

impl RouterConfig {
    fn from_env() -> Self {
        RouterConfig {
            chat: env_or("ONPREM_MODEL_CHAT", "qwen3-8b"),
            health_query: env_or("ONPREM_MODEL_HEALTH_QUERY", "qwen3-8b"),
            trends: env_or("ONPREM_MODEL_TRENDS", "qwen3-8b"),
            summarize: env_or("ONPREM_MODEL_SUMMARIZE", "mistral-nemo-12b-instruct"),
            lookup: env_or("ONPREM_MODEL_LOOKUP", "qwen3-4b"),
            fast: env_or("ONPREM_MODEL_FAST", "qwen3-4b"),
            classify: env_or("ONPREM_MODEL_CLASSIFY", "phi-4-mini-instruct"),
            extractor: env_or("ONPREM_MODEL_EXTRACTOR", "phi-4-mini-instruct"),
            verifier: env_or("ONPREM_MODEL_VERIFIER", "phi-4-mini-reasoning"),
            max_resident_models: env_parse("ONPREM_MAX_RESIDENT_MODELS", 2_usize),
            npu_enabled: env_parse("ONPREM_NPU_ENABLED", true),
            npu_ctx_cap: env_parse("ONPREM_NPU_CTX_CAP", 4224_u64),
            model_router_enabled: env_parse("ONPREM_ROUTER_MODEL_ENABLED", true),
            router_cache_size: env_parse("ONPREM_ROUTER_CACHE_SIZE", 512_usize),
            text2sql_enabled: env_parse("ONPREM_TEXT2SQL_ENABLED", false),
            sql_model: env_or("ONPREM_MODEL_TEXT2SQL", "phi-4-mini-instruct"),
            nl2sql_tables_max: env_parse("ONPREM_NL2SQL_TABLES_MAX", 4_usize),
            nl2sql_fewshots: env_parse("ONPREM_NL2SQL_FEWSHOTS", 3_usize),
            nl2sql_max_rows: env_parse("ONPREM_NL2SQL_MAX_ROWS", 500_i64),
            nl2sql_timeout_secs: env_parse("ONPREM_NL2SQL_TIMEOUT_SECS", 30_u64),
            nl2sql_sample_values: env_parse("ONPREM_NL2SQL_SAMPLE_VALUES", 10_usize),
        }
    }
}

/// Fully-resolved server configuration.
#[derive(Debug, Clone)]
pub struct Config {
    // Networking
    pub bind_address: String,
    pub port: u16,
    /// Whether the server is running in a production deployment. Set via `ONPREM_ENV=production`
    /// (or `prod`). When true, `security_issues()` violations abort boot; when false they are
    /// warnings so development still works with convenient defaults.
    pub production: bool,

    // DocumentDB
    pub documentdb_uri: String,
    pub documentdb_db: String,

    // Auth
    pub jwt_secret: String,
    pub jwt_ttl_hours: i64,
    pub credentials_key: Option<String>,
    pub admin_username: String,
    pub admin_password: String,

    // Foundry Local (chat only — Foundry Local has no embedding models)
    pub chat_model: String,
    /// Model-cache directory handed to the in-process Foundry core. The server embeds
    /// its own core (it does not talk to the `foundry` CLI service), so it does **not**
    /// pick up `foundry cache location` on its own — without this it silently falls back
    /// to the SDK default (`~/.foundry/cache`) and reports models there as downloaded
    /// even after the real cache moved. `None` = let the SDK choose.
    pub foundry_cache_dir: Option<String>,

    // Embeddings (fastembed / ONNX Runtime, local — shares the reranker's stack)
    pub embedding_model: String,
    pub embedding_dims: usize,

    // Retrieval
    pub retrieval_mode: RetrievalMode,
    pub rrf_k: f64,
    pub rerank_enabled: bool,
    pub rerank_model: String,
    pub retrieve_per_side: i64,
    pub rerank_top_n: usize,
    pub context_top_k: usize,
    /// Rerank-score floor (anti-hallucination gate): if the top passage scores below
    /// this after sigmoid normalisation (plan 19.1), `/chat` refuses to generate.
    /// Scores are in (0, 1) after sigmoid; 0.30 is the production default.
    /// Set `ONPREM_SCORE_GATE=0` to disable.
    pub score_gate: Option<f64>,
    /// Pre-load fastembed embedder and reranker at boot so the first real request
    /// does not pay the ONNX model-load latency.
    pub warmup_enabled: bool,

    // Query expansion
    pub multi_query_enabled: bool,
    pub multi_query_count: usize,

    // Chunking (per-passage embedding of long free-text fields)
    pub chunk_enabled: bool,
    pub chunk_size_tokens: usize,
    pub chunk_overlap_tokens: usize,

    // Model router (task-aware model selection + device placement)
    pub router: RouterConfig,
}

impl Config {
    /// Load configuration from the environment, applying sensible development defaults.
    pub fn from_env() -> Self {
        Config {
            bind_address: env_or("ONPREM_BIND_ADDRESS", "0.0.0.0"),
            port: env_parse("ONPREM_PORT", 8000),
            production: matches!(
                env::var("ONPREM_ENV")
                    .unwrap_or_default()
                    .trim()
                    .to_ascii_lowercase()
                    .as_str(),
                "production" | "prod"
            ),

            documentdb_uri: env_or(
                "ONPREM_DOCUMENTDB_URI",
                "mongodb://docadmin:ChangeMe_Doc123@localhost:10260/?tls=true&tlsAllowInvalidCertificates=true&retrywrites=false",
            ),
            documentdb_db: env_or("ONPREM_DOCUMENTDB_DB", "onprem_rag"),

            jwt_secret: env_or("ONPREM_JWT_SECRET", "dev-only-change-me"),
            jwt_ttl_hours: env_parse("ONPREM_JWT_TTL_HOURS", 8),
            credentials_key: env::var("ONPREM_CREDENTIALS_KEY").ok().filter(|s| !s.is_empty()),
            admin_username: env_or("ONPREM_ADMIN_USERNAME", "admin"),
            admin_password: env_or("ONPREM_ADMIN_PASSWORD", "password"),

            // Bare alias (not a pinned variant id): the SDK resolves it to the best
            // variant for the registered EPs, so this stays valid as EPs come online.
            // Pin a full variant id here only to force a specific execution provider.
            // qwen3-8b is preferred (generic-gpu variant); openvino-npu variants are
            // 4224-ctx capped and should not be used for multi-turn chat.
            chat_model: env_or("ONPREM_CHAT_MODEL", "qwen3-8b"),
            // Explicit env wins; otherwise follow whatever `foundry cache location` set,
            // so moving the cache doesn't leave the server reading a stale model index.
            foundry_cache_dir: env::var("ONPREM_FOUNDRY_CACHE_DIR")
                .ok()
                .filter(|s| !s.is_empty())
                .or_else(foundry_cli_cache_dir),
            // fastembed EmbeddingModel variant name (ORT path). BGE-M3 is 1024-dim,
            // multilingual, prefix-free, and GPU/DirectML-capable on this host.
            embedding_model: env_or("ONPREM_EMBEDDING_MODEL", "bge-m3"),
            embedding_dims: env_parse("ONPREM_EMBEDDING_DIMS", 1024),

            retrieval_mode: RetrievalMode::parse(&env_or("ONPREM_RETRIEVAL_MODE", "hybrid")),
            rrf_k: env_parse("ONPREM_RRF_K", 60.0),
            rerank_enabled: env_parse("ONPREM_RERANK_ENABLED", true),
            rerank_model: env_or("ONPREM_RERANK_MODEL", "bge-reranker-v2-m3"),
            retrieve_per_side: env_parse("ONPREM_RETRIEVE_PER_SIDE", 50),
            rerank_top_n: env_parse("ONPREM_RERANK_TOP_N", 30),
            context_top_k: env_parse("ONPREM_CONTEXT_TOP_K", 6),
            // Default gate 0.30 (post-sigmoid scale). Env var overrides; set to 0 to disable.
            score_gate: env::var("ONPREM_SCORE_GATE")
                .ok()
                .and_then(|v| v.parse().ok())
                .or(Some(0.30)),
            warmup_enabled: env_parse("ONPREM_WARMUP_ENABLED", true),

            multi_query_enabled: env_parse("ONPREM_MULTI_QUERY_ENABLED", true),
            multi_query_count: env_parse("ONPREM_MULTI_QUERY_COUNT", 3),

            chunk_enabled: env_parse("ONPREM_CHUNK_ENABLED", true),
            chunk_size_tokens: env_parse("ONPREM_CHUNK_SIZE_TOKENS", 384),
            chunk_overlap_tokens: env_parse("ONPREM_CHUNK_OVERLAP_TOKENS", 64),

            router: RouterConfig::from_env(),
        }
    }

    /// Collect security problems with the current config. Callers decide fatality:
    /// fatal in production, warnings in development. Returned strings are user-facing.
    pub fn security_issues(&self) -> Vec<String> {
        let mut issues = Vec::new();

        // Weak JWT secret: known dev placeholder strings, or too short to resist brute-force.
        if self.jwt_secret == "dev-only-change-me"
            || self.jwt_secret == "dev-only-change-me-to-a-long-random-string"
            || self.jwt_secret.len() < 32
        {
            issues.push(
                "ONPREM_JWT_SECRET is unset, default, or shorter than 32 chars".to_string(),
            );
        }

        // Default admin password; empty is also rejected even though env_or("password") never yields "".
        if self.admin_password == "password" || self.admin_password.is_empty() {
            issues.push(
                "ONPREM_ADMIN_PASSWORD is unset or equals the default 'password'".to_string(),
            );
        }

        // Credentials key must be present, ≥32 chars, and distinct from the JWT secret
        // (two independent secrets, two independent jobs — sharing them halves the security).
        match &self.credentials_key {
            None => {
                issues.push(
                    "ONPREM_CREDENTIALS_KEY is unset (required in production)".to_string(),
                );
            }
            Some(k) if k.len() < 32 => {
                issues.push(
                    "ONPREM_CREDENTIALS_KEY is shorter than 32 chars".to_string(),
                );
            }
            Some(k) if k == &self.jwt_secret => {
                issues.push(
                    "ONPREM_CREDENTIALS_KEY must differ from ONPREM_JWT_SECRET".to_string(),
                );
            }
            _ => {}
        }

        issues
    }
}

/// The cache directory configured for the `foundry` CLI/service, read from
/// `~/.foundry/foundry.config.json` (`serviceSettings.cacheDirectoryPath`) — the file
/// `foundry cache location <path>` writes. Best-effort: any missing file, unreadable
/// JSON, or absent key yields `None` and the SDK default applies.
fn foundry_cli_cache_dir() -> Option<String> {
    let home = env::var("USERPROFILE").or_else(|_| env::var("HOME")).ok()?;
    let path = std::path::Path::new(&home).join(".foundry").join("foundry.config.json");
    let raw = std::fs::read_to_string(path).ok()?;
    let json: serde_json::Value = serde_json::from_str(&raw).ok()?;
    let dir = json.get("serviceSettings")?.get("cacheDirectoryPath")?.as_str()?.trim();
    (!dir.is_empty()).then(|| dir.to_string())
}

/// Read an env var or fall back to a default string.
fn env_or(key: &str, default: &str) -> String {
    env::var(key).ok().filter(|s| !s.is_empty()).unwrap_or_else(|| default.to_string())
}

/// Read and parse an env var or fall back to a typed default.
fn env_parse<T: std::str::FromStr>(key: &str, default: T) -> T {
    env::var(key).ok().and_then(|v| v.parse().ok()).unwrap_or(default)
}
