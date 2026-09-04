# Agents, routing, and aggregation (Phases 0–3 shipped)

> Design note recording Phases 0–3 of the model-router + aggregation plan: versatile default → per-task routing → NPU-preferred placement + LRU lifecycle → tool-calling + structured path. All gates clean: server cargo check + cargo test 22/22, app npx tsc --noEmit + bridge cargo check. Shipped 2026-08-23.
>
> Companion docs: `plans/docs/model-selection-and-routing.md`, `plans/docs/aggregation-aware-retrieval.md`, `plans/docs/app-feature-surface-and-requirements.md`.

## What shipped (executive summary)

### Phase 0 — versatile default
- `onprem-rag-server/src/config.rs`: `chat_model` default → **`qwen3-8b`** (was `qwen2.5-7b`). `.env.example` updated. Fixes the 4224-ctx NPU trap immediately.

### Phase 1 — router + per-call generation params
- New `onprem-rag-server/src/foundry/router.rs`: `AgentKind` enum (10 variants: PatientLookup, HealthQuery, Trends, Summarize, Chat, MultiHop, QueryRewrite, Classify, Extract, Verify). `ModelSpec { alias, thinking, temperature, tools, device_pref, max_tokens }`.
- `config.rs`: nested **`RouterConfig`** (11 env-backed fields: `chat`, `health_query`, `trends`, `summarize`, `lookup`, `fast`, `extractor`, `verifier`, `max_resident_models=2`, `npu_enabled=true`, `npu_ctx_cap=4224`). Env vars `ONPREM_MODEL_CHAT`…`ONPREM_NPU_CTX_CAP` (mirrored in `.env.example`).
- `foundry/mod.rs`: `generate_stream_with(&ModelSpec, system, user)` + `complete_with(...)`. Build messages appends `/no_think` when thinking=false. Old `generate_stream`/`complete` are now thin wrappers (callers in `rag/` unchanged).

### Phase 2 — NPU-preferred / CPU-fallback placement + LRU cap
- `foundry/mod.rs`: **`resolve_variant(alias, device_pref)`** walks device priority list, gates NPU on `hardware::detect()` + `npu_enabled` + `context_length() <= npu_ctx_cap`, falls back to CPU or GPU. Retries same alias's CPU if NPU load fails.
- **`ensure_loaded_lru(model)`**: only GPU-class models count against `max_resident_models` (NPU/CPU exempt — keeps phi-4-mini hot on the NPU). Unloads LRU victim via `get_model_variant(victim).unload()` when cap reached.
- Router assigns: Extract/Verify → `[Npu, Cpu]`; PatientLookup/QueryRewrite/Classify → `[Gpu, Cpu]`; context-heavy (HealthQuery/Trends/Chat) → `[Gpu]`.

### Phase 3 — tool-calling / structured aggregation path

#### Server side
- New **`onprem-rag-server/src/aggregation/`** module:
  - `spec.rs`: `RunAggregation` spec (collection, filter, group_by, metric, time_bucket, sort, top_n); `MAX_TOP_N=500`.
  - `catalog.rs`: `OnceLock` catalog, hardcoded 6 collections (records, patients, encounters, prescriptions, lab_orders, vital_signs) with ~30 fields, 12 disease→ICD-10 prefixes, 11 field synonyms. `planner_context()` grounds the planner.
  - `validate.rs`: rejects unknown collection/field, blocks dangerous operators (`$where/$function/$accumulator/$expr`), clamps top_n≤500.
  - `execute.rs`: pipeline `$match(authz ∧ filter)` → **`$group{_id:$row_pk, doc:$first}` dedup** → optional `$addFields(time)` → metric `$group` → `$sort` → `$limit` → `$project`. Returns `AggRow{label, value:f64}`. `maxTimeMS=30000`.
  - `intent.rs`: `QueryIntent` enum + lexical `classify_lexical()` (full-model fallback deferred to Phase 4).

- `foundry/mod.rs`: 
  - `run_aggregation_tool()` — FunctionObject + JSON Schema (strict mode).
  - `plan_aggregation(...)` — native tool-calling: `tool_choice=Function("run_aggregation")`, `response_format=JsonSchema`, one reprompt, JSON-content fallback.
  - `generate_stream_with` now passes `Some(&[tool])` when `spec.tools=true`.

- New **`agents/routes.rs`**: 
  - `POST /agents/<kind>` (AuthUser-guarded). Structured kinds (HealthQuery, Trends, Summarize, PatientLookup): plan→validate→execute→narrate. Semantic kinds (Chat, Lookup): retrieve→generate.
  - Returns `{ question, intent, citations, spec, rows, pipeline, token, done, error }`.
  - Mounted in `main.rs`. `Cargo.toml` gained `async-openai` (chat-completion-types only).

- **SSE event contract:**
  - `citations`: list of retrieved passages.
  - `spec`: the validated aggregation spec (structured only).
  - `rows`: `[{label, value}]` aggregation result (structured only).
  - `pipeline`: the executed DocumentDB pipeline (structured only; for Audit Log provenance).
  - `token`: one generation token (semantic path) or narration (structured).
  - `error`: if query fails.
  - `done`: stream end marker.
  - Body: `{question, intent}` (intent null for Chat, or one of QueryIntent).

#### Bridge + Frontend
- `onprem-rag-app/src-tauri/src/commands.rs`: **`agent(kind, question, intent)`** command relays SSE as `agent://citations|spec|rows|pipeline|token|error|done` events. Registered in `lib.rs`.
- `src/lib/bridge.ts`: `AgentKind`, `AggRow`, `AggSpec`, **`agent(kind, question, callbacks)`** (returns cleanup fn; unlistens all 7 events).
- `src/App.tsx`: new **`"agents"` tab**. 
  - `src/agents/Agents.tsx`: 4 sub-tabs (Health Query, Trends, Summarize, Patient Lookup). Input field, results table + chart, "Show query" for structured queries, citations for semantic.
  - `src/agents/MiniChart.tsx`: **dependency-free inline SVG**. Bar chart for Health Query (metric values), line/area for Trends (time-series).

## Configuration surface

### Environment variables (all optional with defaults)
```
# Router: which model for each task
ONPREM_MODEL_CHAT=qwen3-8b
ONPREM_MODEL_HEALTH_QUERY=qwen3-8b
ONPREM_MODEL_TRENDS=qwen3-8b
ONPREM_MODEL_SUMMARIZE=mistral-nemo-12b-instruct
ONPREM_MODEL_LOOKUP=qwen3-4b
ONPREM_MODEL_FAST=qwen3-4b
ONPREM_MODEL_EXTRACTOR=phi-4-mini-instruct
ONPREM_MODEL_VERIFIER=phi-4-mini-reasoning

# Lifecycle & hardware
ONPREM_MAX_RESIDENT_MODELS=2
ONPREM_NPU_ENABLED=true
ONPREM_NPU_CTX_CAP=4224
```

All set in `.env.example` with inline doc. `config.rs` loads via `dotenvy` in dev, or environment at runtime.

## What's still open (Phase 4 + follow-ups)

1. **On-demand `mistral-nemo-12b` summarizer** — Summarize tab triggers load; unload after streaming ends.
2. **Agentic multi-hop loop** — agent loop: decompose structured question → retrieve → synthesize (via `search_records` / `run_aggregation` tool calls).
3. **Full-model intent fallback** — Chat "Auto" box currently lexical-only; Phase 4 adds small-model `Classify` intent router.
4. **Ingest-time catalog population** — catalog hardcoded seed today; Phase 4 refreshes schema/counts/vocabularies during/after ingest.
5. **Cohort semantic pre-filter (aggregation-aware retrieval 3b)** — union of vector/text search → `row_pk` cohort → inject into aggregation filter. Gated on ingest-time catalog (Phase 4).
6. **Audit hook** — log `{user, ts, intent, spec, executed_pipeline, row_count}` for the Audit Log screen.
7. **`<think>` stream filtering** — deferred; TODO when thinking-on model routed via `/chat`.

## Testing / verification

- Server: `cargo check` + **`cargo test` 22/22 pass** (all routes, retrieval, aggregation, model selection).
- App: `npx tsc --noEmit` + bridge `cargo check` both clean.
- Manual gates: `/health` responds; `/agents/health_query` accepts structured question; SSE streams events; chart renders.

## Workflow notes

- **Phases 0–3 implemented by:** Sonnet (code). Haiku (doc updates & plans).
- **Phase 4 planning by:** Sonnet (detailed rollout). Implementation TBD.
- All design docs synced to `plans/docs/` for reference; this note is the mind-cache artifact.

## Related files

- `onprem-rag-server/src/config.rs` — RouterConfig defaults + env loading.
- `onprem-rag-server/src/foundry/router.rs` — AgentKind, ModelSpec, resolve_variant, ensure_loaded_lru.
- `onprem-rag-server/src/foundry/mod.rs` — generate_stream_with, plan_aggregation, run_aggregation_tool.
- `onprem-rag-server/src/aggregation/` — spec, catalog, validate, execute, intent.
- `onprem-rag-server/src/agents/routes.rs` — POST /agents/<kind> endpoint.
- `onprem-rag-app/src-tauri/src/commands.rs` — agent command.
- `onprem-rag-app/src/lib/bridge.ts` — bridge layer.
- `onprem-rag-app/src/agents/` — Agents.tsx, MiniChart.tsx.
- `.env.example` — all config keys documented.
