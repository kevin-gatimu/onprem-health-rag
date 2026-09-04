# LLM selection & task-aware routing

> **Superseded.** This file is retained for link/history stability. The authoritative replacement is [Models, Accelerators, and Lifecycle](models-accelerators-and-lifecycle.md). Do not treat the statuses, defaults, or diagrams below as current.

> How we pick local chat LLMs for each kind of question the app supports, and how the server
> routes a request to the right model + generation mode. Design note started 2026-08-23.
> Companion to `plans/05-accelerator-detection.md` (EP/hardware),
> `plans/01-retrieval-design.md` (retrieval pipeline),
> `plans/docs/aggregation-aware-retrieval.md` (the analytical path), and
> `plans/docs/app-feature-surface-and-requirements.md` (the screens these serve).

## Goal

The **AI Agents** screen exposes several task types — **Health Query**, **Summarize**,
**Patient Lookup**, **Trends**, **Ingestion** — plus the generalist **AI Chat**, and a future
**multi-hop** capability. These are not one workload. A patient lookup wants a fast, literal
extractor; a trends question wants temporal reasoning; a summary wants long context and zero
chain-of-thought drift. Forcing one model to do all of it means it is wrong for most of them.

This note defines a **small stable of models** and a **task-aware router** that maps each agent
to a model + generation mode (thinking on/off, temperature, tool-calling), with lazy
load/unload so the shared-memory Intel Arc iGPU is never asked to hold more than it can.

## Hardware constraint (why we can't just run the biggest model)

Target host: **Intel Core Ultra 7 155H** — Arc **iGPU** (shared system RAM, not a discrete
card) + **AI Boost NPU**. Registered EP: WebGPU (works now); OpenVINO available. Implications:

- **7–8B quantized runs comfortably** on the iGPU; **12B is slower**; **14B+ / 20B is painful**.
- **NPU (OpenVINO-npu) variants are context-capped at 4224 tokens** — unusable for RAG that
  stuffs 6 chunks + history. **Always prefer `generic-gpu` variants** for chat/agents.
- **Memory is the real limit**: we cannot keep three models hot. The router keeps a small fast
  model resident and lazily swaps the larger ones (LRU), see *Model lifecycle* below.

## The model stable

Standardize on **three chat models** (plus fastembed BGE-M3 / bge-reranker, which are separate
and unrelated — see `[[embeddings-not-in-foundry-local]]`):

| Role | Model (alias) | Ctx | Capabilities | Why |
| --- | --- | --- | --- | --- |
| **Core / versatile** | `qwen3-8b` (`generic-gpu`) | 40960 | reasoning, tool-calling | Hybrid thinking (on/off per task), native tool-calling, fits the iGPU. The default for chat, health-query, trends, multi-hop. |
| **Fast lane** | `ministral-3-3b-instruct-2512` (or `qwen3-4b`) | 262144 / 40960 | tool-calling (ministral); reasoning+tool-calling (qwen3-4b) | Low-latency literal extraction for Patient Lookup, and all **internal** query-rewrite / multi-query steps. Always thinking-off. |
| **Long summarizer** | `mistral-nemo-12b-instruct` (`generic-gpu`) | 131072 | tool-calling | Pure instruct (no CoT overhead), huge context for whole-history / cohort summaries. Loaded on demand only. |

Escalation model (optional, hardest multi-hop only): `qwen3-14b` (40960, reasoning+tool-calling)
— accept the latency hit; load on demand, unload after.

**Avoid *as the generalist default* on this host:** `gpt-oss-20b` and 14B+ (too slow on the iGPU);
pure-reasoning models with **no** tool-calling and **no** thinking-off escape hatch (`deepseek-r1-*`,
`phi-4-reasoning`) — they lock you into CoT overhead. (`phi-4-reasoning` is still fine in the
specialized verifier role below — just not as the everyday chat model.) `openvino-npu` variants
(4224 ctx trap).

### Health-records-specific roles

Beyond the three generalist tiers, health-record work has specialized jobs that suit **small,
tool-calling / reasoning models** — and these are where the **Phi-4 series earns its place** (small
enough to sit on the **NPU/CPU** and never contend with the iGPU that serves chat):

| Role | Model | Ctx need | Why |
| --- | --- | --- | --- |
| **Ingestion extractor / normalizer / de-id** | `phi-4-mini-instruct` (3.8B, tool-calling) or ministral-3b | small (per-record) | Pull structured fields from free-text notes, map to ICD-10/LOINC/RxNorm, flag/scrub PHI. Runs over many rows → must be fast + reliable structured output. |
| **Aggregation query-planner** | `phi-4-mini-instruct` or `qwen3-8b` | small | Turn an analytical question + schema catalog into a validated `run_aggregation` spec (see `plans/docs/aggregation-aware-retrieval.md`). Precise structured/function output, low latency. |
| **Clinical grounding / faithfulness verifier** | `phi-4-mini-reasoning` (or qwen3-4b thinking-on) | small (answer + passages) | Cheap second pass: is every medication/dose/allergy/contraindication claim supported by the retrieved passages? Safety-critical for PHI. |
| **Intent classifier** | fast lane (3–4B) or `phi-4-mini-instruct` | tiny | Route Chat "Auto" into lookup / narrative / aggregation / trend / multi-hop. Hot path — never the big model. |
| **High-accuracy escalation verifier** (optional) | `phi-4` (14B) / `phi-4-reasoning` | medium | Hardest clinical reasoning checks, on demand; accept the latency. Phi's STEM/logic strength per param suits a checker. |

**So: is there a use case for Phi-4? Yes — three.** `phi-4-mini-instruct` (explicitly tool-calling,
per the Models page) is the **ingestion extractor/normalizer/de-id worker** and the **aggregation
query-planner**; `phi-4-mini-reasoning` is the **grounding/faithfulness verifier**; `phi-4` (14B) is
an optional high-accuracy escalation checker. What Phi-4 is **not**: the generalist chat/RAG model —
that stays `qwen3-8b` (hybrid thinking + long-enough context). The `openvino-npu` variants are
4224-ctx capped, but the extractor / verifier / classifier roles are single-record or
answer+passages, so that small context is fine — which is exactly why parking Phi-4-mini on the
**AI Boost NPU** frees the iGPU for chat.

## Two retrieval paths (this changes the model's job)

The "Most Commonly Diagnosed Diseases" result — counts per diagnosis + a bar chart — is **not**
vector RAG. It is a **structured aggregation** over the records. The app therefore has two paths,
and the LLM plays a different role in each:

1. **Semantic RAG path** — Chat, Patient Lookup (narrative), generalist Q&A.
   Existing pipeline (`plans/01`): rewrite → multi-query → hybrid → RRF → rerank → **grounded
   generation**. The model reads retrieved passages and answers faithfully with a score gate.

2. **Structured / analytical path** — Health Query, Trends.
   The model **emits a tool call** (e.g. `run_aggregation`) → the Rust layer executes it against
   DocumentDB → the model **narrates the returned rows**, and the UI renders the table/chart.
   This path is why **tool-calling** is a hard requirement for Health Query / Trends / Multi-hop,
   and why those agents use `qwen3-8b`, not a small instruct-only model.

## Task → model routing table

| Agent | Path | Model | Thinking | Tools | Temp | Notes |
| --- | --- | --- | --- | --- | --- | --- |
| **Patient Lookup** | Semantic | fast lane (3–4B) | off | fetch_patient | ~0.1 | Literal, fast, refuse-if-absent. |
| **Health Query** | Structured | `qwen3-8b` | off | run_aggregation | ~0.2 | Text→aggregation + narration + chart. |
| **Trends** | Structured | `qwen3-8b` | **on** | run_aggregation | ~0.3 | Temporal multi-hop interpretation. |
| **Summarize** | Semantic | `mistral-nemo-12b` (fast lane if short) | off | – | ~0.2 | Long-context faithful compression. |
| **AI Chat** (generalist) | Semantic | `qwen3-8b` | auto/off | optional | ~0.3 | Hybrid default. |
| **Multi-hop** (future) | Structured+Semantic | `qwen3-8b` → `qwen3-14b` | **on** | search_records, run_aggregation, fetch_patient | ~0.3 | Agent loop: decompose → retrieve → synthesize. |
| *internal* rewrite/expansion | – | fast lane (3–4B) | off | – | ~0.1 | Must be fast + plain-text; never CoT. |

## How to hook it into the app

### Server (`onprem-rag-server/src/`)

1. **Agent/task type** — `enum AgentKind { PatientLookup, HealthQuery, Trends, Summarize, Chat,
   MultiHop }` (+ an internal `QueryRewrite`). Serde-tagged so the frontend/bridge can name it.
2. **Router config** (`config.rs`) — a default `AgentKind → ModelSpec` map, each overridable by
   env: `ONPREM_MODEL_CHAT`, `ONPREM_MODEL_HEALTH_QUERY`, `ONPREM_MODEL_TRENDS`,
   `ONPREM_MODEL_SUMMARIZE`, `ONPREM_MODEL_LOOKUP`, `ONPREM_MODEL_FAST`.
   `ModelSpec { alias, thinking: bool, temperature: f32, tools: bool }`.
3. **Model manager** (extend `foundry::FoundryManager`) — `resolve_for(kind) -> ModelSpec`, then
   `ensure_loaded` (already exists) + apply generation params. Add an **LRU resident cap**
   (`ONPREM_MAX_RESIDENT_MODELS`, default 2): before loading a new model, unload the
   least-recently-used one if at the cap. (This is the previously-deferred load/unload lifecycle
   work; it becomes necessary once routing loads >1 model.)
4. **Thinking-mode control** — Qwen3 toggles CoT via its chat template (`enable_thinking`) or a
   `/no_think` sentinel. The generation call passes `thinking` from the `ModelSpec`. Internal
   rewrite/expansion calls (`complete()`) force `thinking = false` regardless of agent.
5. **Tool-calling layer** (structured path) — a small tool schema mapping to the existing Rust
   data layer:
   - `run_aggregation(filter, group_by, metric, top_n)` → DocumentDB aggregation → rows.
   - `fetch_patient(patient_id, fields)` → source/record lookup.
   - `search_records(query, k)` → the semantic pipeline (reuse `/search`).
   The model emits a tool call; the server executes it, feeds results back, and the model
   narrates. The narration + raw rows are returned so the UI can render table + chart.
6. **Endpoints** — `POST /agents/:kind` (or extend `/chat` with an `agent` field). The route
   resolves the model via the router, runs the appropriate path (semantic vs structured), and
   streams tokens (reusing the SSE relay from `plans/04`).

### Bridge + Frontend

7. **Bridge** — the agent commands pass `agent_kind` through; no JWT/router logic in the web
   layer. The active model per tab is surfaced for the badge already shown in the UI
   (`LLM: …` chip).
8. **Frontend** — each AI Agents tab sets its `agent_kind`; the model chip reflects the resolved
   model. ✅ Settings is now a model-management surface via `GET /models/roles`: per-role Model dropdown (assigned variant + family siblings by full id), live download/load via `POST /models/pull` streaming progress to State column (downloaded/current/loaded). Per-role model choices now **persist** via a `settings` collection in DocumentDB (`router_overrides` map: role→variant id); env vars are the baseline and a saved override wins, applied via `AppState::spec_for(kind)` at request time, and set through `PUT /settings/router` (admin-only). The Settings "Load" button downloads, loads, and saves the chosen variant as that role's default.

## Rollout (incremental, cheapest first)

✅ **Phase 0 — single versatile model.** (`onprem-rag-server/src/config.rs` `chat_model` default → `qwen3-8b`)
   Default to `qwen3-8b` (generic-gpu), thinking off. `.env.example` updated. Fixes the 4224-ctx trap immediately.

✅ **Phase 1 — thinking-mode routing.** (`onprem-rag-server/src/foundry/router.rs`)
   `AgentKind` enum (PatientLookup, HealthQuery, Trends, Summarize, Chat, MultiHop, QueryRewrite, Classify, Extract, Verify) + per-task thinking/temperature via `ModelSpec`. `config.rs` nested `RouterConfig` (11 env-backed fields: `chat`, `health_query`, `trends`, `summarize`, `lookup`, `fast`, `extractor`, `verifier`, `max_resident_models`, `npu_enabled`, `npu_ctx_cap`). Env vars `ONPREM_MODEL_CHAT`…`ONPREM_NPU_CTX_CAP` mirrored in `.env.example`.

✅ **Phase 2 — fast lane + lifecycle.** (`foundry/mod.rs` `resolve_variant` + `ensure_loaded_lru`)
   `resolve_variant(alias, pref)` walks device preference, gates NPU on hardware detection + `npu_enabled` + context-cap check, falls back to CPU. Extract/Verify → `[Npu,Cpu]` preferred; context-heavy → `[Gpu]`. **Only GPU-class models count against `max_resident_models`** (NPU/CPU exempt — keeps phi-4-mini hot). NPU load failure retries CPU variant. LRU cap unloads victims via `get_model_variant(victim).unload()`.

✅ **Phase 3 — tool-calling / structured path.** (`onprem-rag-server/src/aggregation/` + `agents/routes.rs`)
   New `aggregation/` module (spec.rs, catalog.rs, validate.rs, execute.rs, intent.rs) with `RunAggregation` spec + hardcoded 6-collection catalog. `foundry/mod.rs` adds `run_aggregation_tool()` + native tool-calling (`tool_choice=Function`). `POST /agents/<kind>` (structured: plan→validate→execute→narrate; semantic: retrieve→generate) + SSE contract (`citations|spec|rows|pipeline|token|error|done`). Bridge `agent(kind,question,callbacks)` relays as Tauri events. Frontend `Agents.tsx` + 4 sub-tabs (Health Query, Trends, Summarize, Patient Lookup) with inline SVG chart (`MiniChart.tsx`).

✅ **Persisted router override.** (`onprem-rag-server/src/settings.rs` + `PUT /settings/router`)
   Per-role model overrides now persist in DocumentDB (`settings` collection, `router_overrides` map); env is baseline, saved override wins, applied via `AppState::spec_for(kind)`. Bridge `set_role_model` + Settings UI "Load" button saves the choice as default.

⏳ **Phase 4 — long summarizer + agentic multi-hop.** On-demand `mistral-nemo-12b`; the multi-hop
   agent loop (`qwen3-8b`/`14b`). Ingest-time catalog population + cohort semantic pre-filter still open.

## Open questions

- **Charting**: does the model return chart-ready structured data (preferred — deterministic), or
  does the UI infer it from the table? Lean structured: the `run_aggregation` tool result is
  already rows the frontend can chart directly.
- **Resident cap on this iGPU**: is 2 models realistic in shared RAM at these sizes, or do we run
  strictly one-at-a-time with swap-on-demand? Measure before committing (`plans/02` decode notes).
- **Tool-calling reliability** of `qwen3-8b` for our aggregation schema — validate against real
  data before trusting it for analytics.

## Workflow note

Design authored here (planning model). When we build it, **implementation → Sonnet**
(`[[use-sonnet-to-code-after-planning]]`); **doc updates as things change → Haiku**
(`[[use-haiku-to-update-plans]]`).
