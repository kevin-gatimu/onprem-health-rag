# 17 — Intent Router v2 (structured vs semantic vs hybrid, model-backed)

> **Status: PHASE A + B COMPLETE.** Phase A+B implemented in `onprem-rag-server`: new module
> `src/router/mod.rs` with `RouteClass`, `route()` three-tier classifier (Tier 0 conversational regex
> gate, Tier 1 lexical, Tier 2 phi-4-mini tool-call), LRU `RouterCache`, `RouterMetrics`; wired into
> `/chat` and `/agents` with SSE `routed` event. Phase C (hybrid executor) and Phase D (backend
> selection) deferred pending `plans/18-text-to-sql-harness.md` and `schema_catalog` implementation.
> Companion plans: `18-text-to-sql-harness.md` (structured-SQL backend),
> `19-retrieval-and-faithfulness.md`, `20-performance-fast-path.md`.

## Problem

Today (`rag/routes.rs` + `aggregation/intent.rs`):

- `/chat` auto mode is **lexical-only** (`classify_lexical`). Marker lists catch obvious phrasings but
  miss paraphrases ("what's the male/female split?" has no marker) and mislabel others ("count on me
  summarising this patient" → Aggregation).
- `classify_with_model` exists (`answer.rs:188`) but is only used by `/agents` fallback, parses
  free-text JSON (brittle), and is not cached.
- There is no notion of **hybrid** ("summarize the complaints of patients who visited more than 3
  times") — needs a structured filter *and* semantic synthesis.
- Every message — including "hi", "thanks" — runs the full pipeline. The old Electron app solved this
  with a front-of-app conversational gate (`docs/intent-router.md`); the Tauri stack never got one.
- Once plan 18 lands there are **two structured backends** (live-source SQL vs DocumentDB
  aggregation); nothing decides between them today.

## Design

### Route taxonomy (new `RouteDecision`)

```rust
/// Where a question should be answered. Produced by the router, consumed by /chat + /agents.
pub enum RouteClass {
    /// Greeting / thanks / identity / out-of-domain. No retrieval; one cheap streamed reply.
    Conversational,
    /// Countable/groupable/rankable — answer with exact numbers. Carries the existing QueryIntent
    /// (Aggregation | Trend | Enumeration | Lookup) plus a backend preference.
    Structured { intent: QueryIntent, backend: StructuredBackend },
    /// Explain/summarise/describe — hybrid retrieval + grounded generation.
    Semantic,
    /// Structured filter + semantic synthesis ("summarise notes of patients with >3 visits").
    Hybrid { cohort_intent: QueryIntent },
}

pub enum StructuredBackend {
    /// DocumentDB aggregation over ingested records (existing path — always available).
    DocDb,
    /// Text-to-SQL against a live registered source (plan 18 — only when catalog says the
    /// referenced tables exist on a reachable source AND the feature flag is on).
    SourceSql,
}
```

### Three-tier classification (cheapest first, fail-open)

```
question ──► Tier 0: conversational regex gate ──► Conversational (instant)
        └──► Tier 1: lexical markers (existing classify_lexical, extended) ──► Structured/Semantic
        └──► Tier 2: phi-4-mini tool-call classify (only when Tier 1 returns None
                     or the question matches BOTH structured and semantic markers)
        └──► fail-open: Semantic
```

- **Tier 0 — conversational gate.** Port the proven anchored-regex design from the Electron app
  (`On-premise-Rag-system-for-Health-Records/docs/intent-router.md`): greetings, thanks,
  identity/capability, obvious out-of-domain. Negative guard: conversation-meta questions ("what
  have I asked?") are *excluded* (they need history → Semantic). Zero latency, high precision,
  fail-open (no match ⇒ continue).
- **Tier 1 — lexical (existing, extended).** Keep `classify_lexical` priority order
  (Trend > Enumeration > Aggregation > Lookup > Narrative). Extend markers with distribution
  vocabulary: `"distribution"`, `"breakdown"`, `"split"`, `"histogram"`, `"percentile"`, `"median"`,
  `"ratio"`, `"proportion"`, `"by gender"`, `"by age group"` → Aggregation. Add hybrid detector: a
  structured marker **plus** a narrative marker in the same question ⇒ candidate Hybrid, escalate to
  Tier 2 to confirm.
- **Tier 2 — model classify (phi-4-mini, tool-calling).** Replace the free-text JSON parse in
  `classify_with_model` with the same native tool-call pattern `plan_aggregation` already uses
  (`tool_choice=Function`, strict JSON Schema) so malformed output can't break the router:

  ```jsonc
  // classify_route tool schema (strict)
  {
    "route": "conversational | structured | semantic | hybrid",
    "intent": "lookup | aggregation | trend | enumeration | narrative | null",
    "entities": { "tables": ["..."], "metric": "...", "time_bucket": "..." } // best-effort
  }
  ```

  `entities` doubles as a cheap schema-linking hint for plan 18 (phi-4-mini is strong enough for
  light extraction alongside the label — this is why phi-4-mini and not qwen3-0.6b). The classify
  `ModelSpec` moves from `ONPREM_MODEL_FAST` to a dedicated `ONPREM_MODEL_CLASSIFY`
  (default `phi-4-mini-instruct`), device pref `[Npu, Cpu]` so it stays hot on the NPU and never
  evicts the GPU chat model (LRU already exempts NPU/CPU residents).

### Structured backend selection

When `RouteClass::Structured` and plan 18 is enabled:

1. Take entity hints (Tier 2) or noun phrases from the question; look them up in the
   `schema_catalog` collection (plan 18) via `$text` + vector match on table cards.
2. If ≥1 catalog table matches with score above threshold **and** its source is marked reachable →
   `SourceSql`. Else → `DocDb` (existing aggregation over ingested records).
3. `SourceSql` failure at any stage (generate/validate/execute) falls back to `DocDb`, which in turn
   falls back to Semantic — the existing fail-open chain is preserved and extended one level.

### Caching

- **In-memory LRU** (`lru` crate or hand-rolled `HashMap+VecDeque` behind a `Mutex`), key =
  normalized question (lowercase, collapse whitespace, strip punctuation), value = `RouteDecision`,
  cap 512 entries, TTL none (routes don't go stale within a process lifetime; catalog changes call
  `router_cache.clear()`).
- Only Tier 2 results are cached (Tiers 0/1 are already ~free).
- Hit-rate counter exported through the existing `tracing` fields for later tuning.

## Implementation phases

### Phase A — module + conversational gate
1. New `onprem-rag-server/src/router/mod.rs`: `RouteClass`, `RouteDecision`, `route(question,
   has_history, config, foundry_opt, catalog) -> RouteDecision`, plus `conversational.rs`
   (regex sets + unit tests: ≥20 positive / ≥20 negative fixtures ported from the Electron app doc).
2. `/chat` handles `Conversational`: skip rewrite/expand/retrieve entirely; stream one
   `generate_stream_with` call using a tiny warm system prompt (`temperature 0.4`,
   `max_tokens 160`), still emitting the standard `citations(empty)/token/done` SSE contract so the
   app needs no changes.

### Phase B — Tier 2 tool-call classify + cache
3. `foundry/mod.rs`: add `classify_route_tool()` (FunctionObject, strict schema) +
   `plan_route(&ModelSpec, question) -> RouteToolOutput`, mirroring `plan_aggregation`'s
   one-reprompt + JSON-content fallback.
4. `router/mod.rs`: Tier 1 → Tier 2 escalation rules (None, or structured∧narrative both matched);
   LRU cache; `RouterMetrics` counters (`tier0/tier1/tier2/cache_hit/fail_open`) logged per request.
5. `config.rs`: `ONPREM_MODEL_CLASSIFY=phi-4-mini-instruct`, `ONPREM_ROUTER_MODEL_ENABLED=true`
   (kill-switch back to lexical-only), `ONPREM_ROUTER_CACHE_SIZE=512`. Mirror in `.env.example`.
6. Replace the inline lexical dispatch in `rag/routes.rs` and the `classify_with_model` call in
   `agents/routes.rs` with `router::route(...)`. Delete nothing yet — `answer::classify_with_model`
   stays as the deprecated shim until `/agents` is migrated, then remove.

### Phase C — hybrid route (after plan 18 Phase 1–3)
7. Hybrid executor in `answer.rs`: run the cohort step (structured: SQL or DocDb aggregation
   returning `row_pk`s / entity keys) → inject as `$match` pre-filter into the semantic retrieval
   (`retrieval::retrieve` already supports metadata filters per plan 19) → grounded generation over
   the cohort's passages. SSE: emit `spec`+`rows` (cohort) then `citations`+`token`.
8. Cap cohort size (`ONPREM_HYBRID_COHORT_MAX=500`); if exceeded, answer structurally and say so.

### Phase D — backend selection
9. `router/backend.rs`: catalog probe described above (needs plan 18's `schema_catalog`).
   `ONPREM_TEXT2SQL_ENABLED=false` default until plan 18 gates pass.

## SSE / API changes

- `/chat` body unchanged. New SSE event `routed` (already used by `/agents`):
  `{ "route": "semantic", "intent": null, "tier": 1, "cached": false, "backend": null }` — emitted
  first so the UI activity strip can show the decision.
- Bridge: add `chat://routed` event relay (one-line change in `commands.rs`); `bridge.ts` callback
  optional — UI ignores unknown events today, so this is non-breaking.

## Testing gates

- `cargo test`: conversational gate fixtures; lexical extension fixtures (distribution vocabulary);
  Tier-2 escalation rules (mock Foundry via trait or feature-gated stub); cache hit/eviction;
  fail-open on model error → Semantic.
- Manual: "hi" answers in <1s with no retrieval; "distribution of patients by blood type" →
  Structured/Aggregation; "summarize the notes of diabetic patients" → Hybrid; kill-switch env
  reverts to current behavior.

## Risks / notes

- phi-4-mini tool-calling reliability: `plan_aggregation` already proves the pattern on this stack;
  keep the one-reprompt + fail-open chain regardless.
- Latency: Tier 2 adds one small NPU call (~100–300 ms) only for ambiguous questions; the cache and
  Tier 0/1 keep the common path model-free. Do NOT run Tier 2 on every message.
- Never route to `SourceSql` when the question has no table-ish entity match — a wrong SQL answer is
  worse than a semantic one (numbers invent authority).
