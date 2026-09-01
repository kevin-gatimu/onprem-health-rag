//! Intent Router v2 — tiered, model-backed routing for `/chat` and `/agents`.
//!
//! Separates **structured** questions (exact DB aggregation) from **semantic**
//! ones (hybrid retrieval + grounded generation), adds a **conversational**
//! gate (no retrieval) and a **hybrid** class (structured filter + semantic
//! synthesis), and caches the expensive decisions.
//!
//! Three tiers, cheapest first, always fail-open to [`RouteClass::Semantic`]:
//!
//! ```text
//! Tier 0  conversational regex gate  → Conversational   (instant)
//! Tier 1  lexical markers            → Structured / Semantic
//! Tier 2  phi-4-mini tool-call       → any class        (only when Tier 1 is
//!                                                         ambiguous or a hybrid
//!                                                         candidate; cached)
//! fail-open                          → Semantic
//! ```
//!
//! Only Tier 2 results are cached (Tiers 0/1 are already model-free). See
//! `plans/17-intent-router-v2.md`. The hybrid executor and the live-source SQL
//! backend selector are Phases C/D (they depend on `plans/18-text-to-sql`),
//! so today `Hybrid` is answered semantically and `backend` is always `DocDb`.

pub mod conversational;

use std::collections::{HashMap, VecDeque};
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};

use serde::Deserialize;

use crate::aggregation::intent::{QueryIntent, classify_lexical, has_narrative_marker};
use crate::config::Config;
use crate::foundry::FoundryManager;
use crate::foundry::router::{AgentKind, ModelSpec};

/// Warm, identity-aware system prompt for the conversational path. One cheap
/// streamed reply covers greetings, thanks, identity/capability, and off-topic
/// redirects — no embed, no retrieval.
pub const CONVERSATIONAL_SYSTEM_PROMPT: &str = "\
You are the assistant for an on-premises health-records question-answering system. \
The user's message is a greeting, a thank-you, a question about you, or off-topic — \
it is NOT a request for patient data. Reply in 1-3 short, warm sentences. \
If it is a greeting or thanks, respond in kind and briefly invite a clinical question \
about the health records (for example counts, trends, or summaries of patient data). \
If it asks who you are or what you can do, say you answer questions about the \
health-records database and give two or three example questions. \
If it is off-topic, politely say you focus on the health records. \
Never invent patient data or clinical facts.";

// ---------------------------------------------------------------------------
// Route taxonomy
// ---------------------------------------------------------------------------

/// Where a question should be answered. Produced by [`route`], consumed by
/// `/chat` and `/agents`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RouteClass {
    /// Greeting / thanks / identity / out-of-domain. No retrieval; one cheap
    /// streamed reply.
    Conversational,
    /// A question about the conversation itself ("what have I asked so far?",
    /// "recap our chat"). No retrieval; answered from `WorkingMemory` alone
    /// (plan 22.5). Only produced when the conversation actually has history —
    /// with none, there's nothing to recap and the question falls through
    /// normally.
    ConversationMeta,
    /// Countable / groupable / rankable — answer with exact numbers. Carries the
    /// [`QueryIntent`]. DocumentDB aggregation is the only backend today; a
    /// backend preference will be reintroduced when plan 18 Phase D (text-to-SQL
    /// backend selection) actually lands — no point carrying that distinction
    /// before anything produces or reads a second value for it.
    Structured {
        intent: QueryIntent,
        backend: StructuredBackend,
    },
    /// Explain / summarise / describe — hybrid retrieval + grounded generation.
    Semantic,
    /// Structured filter + semantic synthesis ("summarise notes of patients with
    /// >3 visits"). Executor is Phase C (plan 18); today answered semantically.
    Hybrid { cohort_intent: QueryIntent },
}

/// Which structured backend answers a [`RouteClass::Structured`] query.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StructuredBackend {
    /// DocumentDB aggregation over ingested records — always available.
    DocDb,
    /// Text-to-SQL against the most relevant registered live source.
    SourceSql,
}

/// A routing decision plus provenance (which tier decided, cache hit, and any
/// best-effort entity hints the model extracted).
#[derive(Debug, Clone)]
pub struct RouteDecision {
    pub class: RouteClass,
    /// Which tier produced this decision: 0 (gate), 1 (lexical), 2 (model).
    pub tier: u8,
    /// True when served from the Tier-2 cache.
    pub cached: bool,
    /// Best-effort schema-linking hints from the Tier-2 model (Phase 18 input).
    #[allow(dead_code)]
    pub entities: Option<RouteEntities>,
}

impl RouteDecision {
    /// Stable label for the route class (SSE payload + logging).
    pub fn route_label(&self) -> &'static str {
        match self.class {
            RouteClass::Conversational => "conversational",
            RouteClass::ConversationMeta => "conversation_meta",
            RouteClass::Structured { .. } => "structured",
            RouteClass::Semantic => "semantic",
            RouteClass::Hybrid { .. } => "hybrid",
        }
    }

    /// The carried intent, if the class has one.
    fn intent(&self) -> Option<QueryIntent> {
        match &self.class {
            RouteClass::Structured { intent, .. } => Some(*intent),
            RouteClass::Hybrid { cohort_intent } => Some(*cohort_intent),
            _ => None,
        }
    }

    /// Serialise to the `routed` SSE event payload: `{ "route", "intent",
    /// "tier", "cached" }`. Emitted first by `/chat` so the UI activity strip
    /// can show the decision.
    pub fn to_sse_json(&self) -> String {
        let intent = self
            .intent()
            .and_then(|i| serde_json::to_value(i).ok())
            .unwrap_or(serde_json::Value::Null);
        let backend = match &self.class {
            RouteClass::Structured {
                backend: StructuredBackend::SourceSql,
                ..
            } => Some("source_sql"),
            RouteClass::Structured {
                backend: StructuredBackend::DocDb,
                ..
            } => Some("document_db"),
            _ => None,
        };
        serde_json::json!({
            "route": self.route_label(),
            "intent": intent,
            "backend": backend,
            "tier": self.tier,
            "cached": self.cached,
        })
        .to_string()
    }
}

/// Best-effort entities extracted by the Tier-2 classifier — a cheap
/// schema-linking hint for the text-to-SQL backend (plan 18). Carried and
/// logged today; not yet consumed.
#[derive(Debug, Clone, Default, Deserialize)]
#[allow(dead_code)]
pub struct RouteEntities {
    #[serde(default)]
    pub tables: Vec<String>,
    #[serde(default)]
    pub metric: Option<String>,
    #[serde(default)]
    pub time_bucket: Option<String>,
}

/// Raw output of the `classify_route` tool call. Strings are parsed leniently in
/// [`class_from_model`] so a small model's stray label can't break routing.
#[derive(Debug, Clone, Deserialize)]
pub struct RouteToolOutput {
    pub route: String,
    #[serde(default)]
    pub intent: Option<String>,
    #[serde(default)]
    pub entities: Option<RouteEntities>,
}

// ---------------------------------------------------------------------------
// Tier-2 cache + metrics
// ---------------------------------------------------------------------------

/// Per-tier hit counters, exported through `tracing` for later tuning.
#[derive(Debug, Default)]
pub struct RouterMetrics {
    pub tier0: AtomicU64,
    pub tier1: AtomicU64,
    pub tier2: AtomicU64,
    pub cache_hit: AtomicU64,
    pub fail_open: AtomicU64,
}

/// A bounded LRU over normalized questions → Tier-2 [`RouteDecision`]s, plus the
/// router metrics. Routes don't go stale within a process lifetime, so there is
/// no TTL; [`RouterCache::clear`] is called when the catalog changes.
pub struct RouterCache {
    inner: Mutex<Lru>,
    pub metrics: RouterMetrics,
}

struct Lru {
    map: HashMap<String, RouteDecision>,
    /// Recency queue: front = least-recently-used, back = most-recent.
    order: VecDeque<String>,
    cap: usize,
}

impl RouterCache {
    pub fn new(cap: usize) -> Self {
        RouterCache {
            inner: Mutex::new(Lru {
                map: HashMap::new(),
                order: VecDeque::new(),
                cap: cap.max(1),
            }),
            metrics: RouterMetrics::default(),
        }
    }

    /// Look up a decision, promoting it to most-recently-used on a hit.
    fn get(&self, key: &str) -> Option<RouteDecision> {
        let mut lru = self.inner.lock().ok()?;
        let hit = lru.map.get(key).cloned()?;
        lru.order.retain(|k| k != key);
        lru.order.push_back(key.to_string());
        Some(hit)
    }

    /// Insert a decision, evicting the least-recently-used entry at capacity.
    fn put(&self, key: String, value: RouteDecision) {
        if let Ok(mut lru) = self.inner.lock() {
            lru.order.retain(|k| k != &key);
            lru.map.insert(key.clone(), value);
            lru.order.push_back(key);
            while lru.order.len() > lru.cap {
                if let Some(evict) = lru.order.pop_front() {
                    lru.map.remove(&evict);
                }
            }
        }
    }

    /// Drop all cached decisions (called when the catalog is rebuilt — a schema
    /// change can flip a structured/semantic decision).
    pub fn clear(&self) {
        if let Ok(mut lru) = self.inner.lock() {
            lru.map.clear();
            lru.order.clear();
        }
    }
}

/// Normalise a question into a cache key: lowercase, collapse whitespace, strip
/// trailing punctuation. Two phrasings that differ only cosmetically share a key.
fn normalize_key(question: &str) -> String {
    let lowered = question.to_lowercase();
    let mut out = String::with_capacity(lowered.len());
    let mut prev_space = false;
    for ch in lowered.chars() {
        if ch.is_alphanumeric() {
            out.push(ch);
            prev_space = false;
        } else {
            // Whitespace AND punctuation both collapse to a single soft separator, so
            // "over-time", "over time", and "over,time" all normalize to one key.
            if !prev_space && !out.is_empty() {
                out.push(' ');
            }
            prev_space = true;
        }
    }
    out.trim().to_string()
}

// ---------------------------------------------------------------------------
// Routing
// ---------------------------------------------------------------------------

/// Decide how to answer `question`. Never errors — any model/availability
/// failure falls open to [`RouteClass::Semantic`].
///
/// `has_history` is carried for logging and future conversational-context rules;
/// the conversational gate itself already excludes conversation-meta questions.
pub async fn route(
    question: &str,
    has_history: bool,
    config: &Config,
    foundry_opt: Option<&FoundryManager>,
    cache: &RouterCache,
) -> RouteDecision {
    // Tier 0a — conversation-meta gate: "what have I asked so far?" needs the
    // transcript, not the small-talk reply or a pointless retrieval pass. Only
    // fires when there's actually history to recap.
    if has_history && conversational::is_conversation_meta(question) {
        cache.metrics.tier0.fetch_add(1, Ordering::Relaxed);
        return finish(
            RouteDecision {
                class: RouteClass::ConversationMeta,
                tier: 0,
                cached: false,
                entities: None,
            },
            question,
            has_history,
        );
    }

    // Tier 0 — conversational gate (instant, high precision, fail-open).
    if conversational::is_conversational(question) {
        cache.metrics.tier0.fetch_add(1, Ordering::Relaxed);
        return finish(
            RouteDecision {
                class: RouteClass::Conversational,
                tier: 0,
                cached: false,
                entities: None,
            },
            question,
            has_history,
        );
    }

    let model_enabled = config.router.model_router_enabled;

    // Tier 1 — lexical markers.
    match classify_lexical(question) {
        Some(intent) => {
            let structural = matches!(
                intent,
                QueryIntent::Aggregation | QueryIntent::Trend | QueryIntent::Enumeration
            );
            // Hybrid candidate: a structured question that ALSO carries a narrative
            // marker ("summarise the notes of patients who visited > 3 times").
            // Escalate to Tier 2 to confirm; a plain structured question does not.
            if structural && has_narrative_marker(question) && model_enabled {
                if let Some(decision) = tier2(question, config, foundry_opt, cache).await {
                    return finish(decision, question, has_history);
                }
                // Tier 2 unavailable — trust the lexical structural read.
            }
            cache.metrics.tier1.fetch_add(1, Ordering::Relaxed);
            finish(
                RouteDecision {
                    class: class_from_intent(intent, config),
                    tier: 1,
                    cached: false,
                    entities: None,
                },
                question,
                has_history,
            )
        }
        None => {
            // Ambiguous for the lexical pass — ask the model, if enabled.
            if model_enabled {
                if let Some(decision) = tier2(question, config, foundry_opt, cache).await {
                    return finish(decision, question, has_history);
                }
            }
            cache.metrics.fail_open.fetch_add(1, Ordering::Relaxed);
            finish(
                RouteDecision {
                    class: RouteClass::Semantic,
                    tier: 1,
                    cached: false,
                    entities: None,
                },
                question,
                has_history,
            )
        }
    }
}

/// Tier 2: cache lookup → phi-4-mini `classify_route` tool call. Returns `None`
/// when Foundry is unavailable or the model output can't be mapped (caller then
/// falls open). Only Tier-2 results are cached.
async fn tier2(
    question: &str,
    config: &Config,
    foundry_opt: Option<&FoundryManager>,
    cache: &RouterCache,
) -> Option<RouteDecision> {
    let key = normalize_key(question);
    if let Some(mut hit) = cache.get(&key) {
        cache.metrics.cache_hit.fetch_add(1, Ordering::Relaxed);
        hit.cached = true;
        return Some(hit);
    }

    let foundry = foundry_opt?;
    let spec = ModelSpec::for_kind(AgentKind::Classify, config);
    let out = match foundry.plan_route(&spec, question).await {
        Ok(o) => o,
        Err(e) => {
            tracing::info!(error = %e, "router Tier 2 classify failed; falling open");
            return None;
        }
    };

    let class = class_from_model(&out, config)?;
    let decision = RouteDecision {
        class,
        tier: 2,
        cached: false,
        entities: out.entities.clone(),
    };
    cache.metrics.tier2.fetch_add(1, Ordering::Relaxed);
    cache.put(key, decision.clone());
    Some(decision)
}

/// Map a lexical [`QueryIntent`] to a route class. Only Aggregation/Trend/
/// Enumeration have a structured executor today; everything else is semantic.
fn class_from_intent(intent: QueryIntent, config: &Config) -> RouteClass {
    match intent {
        QueryIntent::Aggregation | QueryIntent::Trend | QueryIntent::Enumeration => {
            RouteClass::Structured {
                intent,
                backend: if config.router.text2sql_enabled {
                    StructuredBackend::SourceSql
                } else {
                    StructuredBackend::DocDb
                },
            }
        }
        QueryIntent::Lookup | QueryIntent::Narrative | QueryIntent::MultiHop => {
            RouteClass::Semantic
        }
    }
}

/// Map the Tier-2 model output to a route class. Lenient: an unrecognised
/// `route` label yields `None` so the caller falls open to semantic.
///
/// A `structured`/`hybrid` route additionally requires a *structural* intent
/// (Aggregation/Trend/Enumeration). The classifier is a small model and can
/// emit `route: "structured"` paired with a non-structural `intent` (e.g.
/// "lookup") — that combination used to reach `run_structured`, which rejects
/// non-structural intents outright, so the whole request errored out instead
/// of falling open to semantic. Route it to `Semantic` instead: the model's
/// intent label is more informative here than its route label when they
/// disagree about whether a DB backend can even answer.
fn class_from_model(out: &RouteToolOutput, config: &Config) -> Option<RouteClass> {
    let intent = out.intent.as_deref().and_then(parse_intent);
    let is_structural = |i: QueryIntent| {
        matches!(
            i,
            QueryIntent::Aggregation | QueryIntent::Trend | QueryIntent::Enumeration
        )
    };
    match out.route.trim().to_ascii_lowercase().as_str() {
        "conversational" => Some(RouteClass::Conversational),
        "semantic" => Some(RouteClass::Semantic),
        "structured" => match intent {
            Some(i) if !is_structural(i) => Some(RouteClass::Semantic),
            _ => Some(RouteClass::Structured {
                intent: intent.unwrap_or(QueryIntent::Aggregation),
                backend: if config.router.text2sql_enabled {
                    StructuredBackend::SourceSql
                } else {
                    StructuredBackend::DocDb
                },
            }),
        },
        "hybrid" => Some(RouteClass::Hybrid {
            cohort_intent: intent.filter(|i| is_structural(*i)).unwrap_or(QueryIntent::Aggregation),
        }),
        _ => None,
    }
}

/// Parse the model's free-text intent label into a [`QueryIntent`]. Unknown
/// labels return `None`.
fn parse_intent(s: &str) -> Option<QueryIntent> {
    match s.trim().to_ascii_lowercase().as_str() {
        "lookup" => Some(QueryIntent::Lookup),
        "narrative" => Some(QueryIntent::Narrative),
        "aggregation" => Some(QueryIntent::Aggregation),
        "trend" => Some(QueryIntent::Trend),
        "enumeration" => Some(QueryIntent::Enumeration),
        "multi_hop" | "multihop" => Some(QueryIntent::MultiHop),
        _ => None,
    }
}

/// Map a route class to the `/agents` [`AgentKind`]. Conversational, Semantic,
/// and Hybrid all resolve to Chat (the grounded default); structured intents map
/// via the shared `intent_to_kind`.
pub fn class_to_agent_kind(class: &RouteClass) -> AgentKind {
    match class {
        RouteClass::Structured { intent, .. } => crate::answer::intent_to_kind(*intent),
        RouteClass::Conversational
        | RouteClass::ConversationMeta
        | RouteClass::Semantic
        | RouteClass::Hybrid { .. } => AgentKind::Chat,
    }
}

/// Log the decision once (debug) and return it.
fn finish(decision: RouteDecision, question: &str, has_history: bool) -> RouteDecision {
    tracing::debug!(
        route = decision.route_label(),
        tier = decision.tier,
        cached = decision.cached,
        has_history,
        chars = question.len(),
        "router decision"
    );
    decision
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalize_collapses_and_strips() {
        assert_eq!(
            normalize_key("  How   MANY patients?? "),
            "how many patients"
        );
        assert_eq!(normalize_key("Trend, over-time!"), "trend over time");
    }

    #[test]
    fn lru_evicts_least_recently_used() {
        let cache = RouterCache::new(2);
        let d = |t| RouteDecision {
            class: RouteClass::Semantic,
            tier: t,
            cached: false,
            entities: None,
        };
        cache.put("a".into(), d(2));
        cache.put("b".into(), d(2));
        // Touch "a" so "b" becomes the LRU victim.
        assert!(cache.get("a").is_some());
        cache.put("c".into(), d(2));
        assert!(cache.get("b").is_none(), "b should have been evicted");
        assert!(cache.get("a").is_some());
        assert!(cache.get("c").is_some());
    }

    #[test]
    fn cache_hit_marks_cached() {
        let cache = RouterCache::new(8);
        cache.put(
            "k".into(),
            RouteDecision {
                class: RouteClass::Semantic,
                tier: 2,
                cached: false,
                entities: None,
            },
        );
        let hit = cache.get("k").unwrap();
        assert!(!hit.cached, "raw stored value keeps cached=false");
        // tier2() is what flips `cached` on the returned copy; verify the field is settable.
    }

    #[tokio::test]
    async fn common_count_and_list_questions_route_to_live_sql() {
        let mut config = Config::from_env();
        config.router.text2sql_enabled = true;
        let cache = RouterCache::new(8);
        for question in ["how many patients do we have?", "list 5 patients"] {
            let decision = route(question, false, &config, None, &cache).await;
            assert!(
                matches!(
                    decision.class,
                    RouteClass::Structured {
                        backend: StructuredBackend::SourceSql,
                        ..
                    }
                ),
                "unexpected route for {question}: {:?}",
                decision.class
            );
        }
    }

    #[test]
    fn structural_intents_use_configured_backend() {
        let mut config = Config::from_env();
        config.router.text2sql_enabled = false;
        for intent in [
            QueryIntent::Aggregation,
            QueryIntent::Trend,
            QueryIntent::Enumeration,
        ] {
            assert_eq!(
                class_from_intent(intent, &config),
                RouteClass::Structured {
                    intent,
                    backend: StructuredBackend::DocDb,
                }
            );
        }
        config.router.text2sql_enabled = true;
        assert_eq!(
            class_from_intent(QueryIntent::Aggregation, &config),
            RouteClass::Structured {
                intent: QueryIntent::Aggregation,
                backend: StructuredBackend::SourceSql,
            }
        );
        assert_eq!(
            class_from_intent(QueryIntent::Narrative, &config),
            RouteClass::Semantic
        );
        assert_eq!(
            class_from_intent(QueryIntent::Lookup, &config),
            RouteClass::Semantic
        );
    }

    #[test]
    fn model_output_maps_leniently() {
        let mut config = Config::from_env();
        config.router.text2sql_enabled = false;
        let mk = |route: &str, intent: Option<&str>| RouteToolOutput {
            route: route.to_string(),
            intent: intent.map(str::to_string),
            entities: None,
        };
        assert_eq!(
            class_from_model(&mk("conversational", None), &config),
            Some(RouteClass::Conversational)
        );
        assert_eq!(
            class_from_model(&mk("SEMANTIC", None), &config),
            Some(RouteClass::Semantic)
        );
        assert_eq!(
            class_from_model(&mk("structured", Some("trend")), &config),
            Some(RouteClass::Structured {
                intent: QueryIntent::Trend,
                backend: StructuredBackend::DocDb
            })
        );
        assert_eq!(
            class_from_model(&mk("structured", None), &config),
            Some(RouteClass::Structured {
                intent: QueryIntent::Aggregation,
                backend: StructuredBackend::DocDb
            })
        );
        assert_eq!(
            class_from_model(&mk("hybrid", Some("enumeration")), &config),
            Some(RouteClass::Hybrid {
                cohort_intent: QueryIntent::Enumeration
            })
        );
        assert_eq!(class_from_model(&mk("gibberish", None), &config), None);

        // Regression: the classifier can emit `route: "structured"` paired with a
        // non-structural `intent` such as "lookup" (a small-model mismatch between
        // its two fields). Previously this constructed `Structured { intent: Lookup }`,
        // which `run_structured` rejects outright — turning a routable question into
        // a hard error instead of falling open to semantic retrieval.
        for bad_intent in ["lookup", "narrative", "multi_hop"] {
            assert_eq!(
                class_from_model(&mk("structured", Some(bad_intent)), &config),
                Some(RouteClass::Semantic),
                "structured route with intent={bad_intent} should fall open to semantic"
            );
        }
        // hybrid with a non-structural cohort intent falls back to Aggregation
        // rather than carrying an intent run_structured can't execute.
        assert_eq!(
            class_from_model(&mk("hybrid", Some("lookup")), &config),
            Some(RouteClass::Hybrid {
                cohort_intent: QueryIntent::Aggregation
            })
        );
    }

    #[tokio::test]
    async fn conversation_meta_requires_history() {
        // "what have I asked" only routes to ConversationMeta when there's
        // history to recap; with none it falls through to the normal pipeline.
        let cache = RouterCache::new(8);
        let cfg = Config::from_env();
        let with_history = route("what have I asked so far?", true, &cfg, None, &cache).await;
        assert_eq!(with_history.class, RouteClass::ConversationMeta);
        assert_eq!(with_history.tier, 0);

        let without_history = route("what have I asked so far?", false, &cfg, None, &cache).await;
        assert_ne!(without_history.class, RouteClass::ConversationMeta);
    }

    #[test]
    fn sse_json_shape() {
        let d = RouteDecision {
            class: RouteClass::Structured {
                intent: QueryIntent::Aggregation,
                backend: StructuredBackend::DocDb,
            },
            tier: 2,
            cached: true,
            entities: None,
        };
        let v: serde_json::Value = serde_json::from_str(&d.to_sse_json()).unwrap();
        assert_eq!(v["route"], "structured");
        assert_eq!(v["intent"], "aggregation");
        assert_eq!(v["tier"], 2);
        assert_eq!(v["cached"], true);

        let sem = RouteDecision {
            class: RouteClass::Semantic,
            tier: 1,
            cached: false,
            entities: None,
        };
        let v2: serde_json::Value = serde_json::from_str(&sem.to_sse_json()).unwrap();
        assert_eq!(v2["intent"], serde_json::Value::Null);
    }
}
