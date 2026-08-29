# App feature surface & derived requirements

> Target feature surface for our app, informed by a **separate reference app** (screenshots captured
> 2026-08-23) whose UI Kevin wants ours to be shaped by. The screenshots are **not our build** — they
> are inspiration. This doc records what we want, and the backend work each screen implies. Companion
> to `plans/00-master-plan.md`, `plans/docs/model-selection-and-routing.md`, and
> `plans/docs/aggregation-aware-retrieval.md`.

## Why this doc

The reference app shows a bigger surface than the master plan's workstreams (1–7) build, and it should
**shape our design**. This doc records: (a) each screen and what it does, (b) the data model the
screens assume, (c) the requirements they imply for us, and (d) a **gap table** vs. our current code
so we know what we already have and what is still to build. Screen names/counts below describe the
reference app; our implementation can rename/reshape freely.

## Navigation surface (target)

Sidebar: **Dashboard · Connections · Ingest · Data Explorer · AI Chat · AI Agents · Models ·
Analytics · Outbreak Alerts · Audit Log · Settings · Profile · Admin.**

Only Connections, Ingest, AI Chat, Settings, and (partially) Admin exist in the current build. The
rest are target.

## The data landscape (from Data Explorer)

Four **EMR sources**, unified into DocumentDB — themed as Kenyan clinics:

| Source | Engine | Endpoint / DB | Tables |
| --- | --- | --- | --- |
| **KNH EMR** | MySQL | `localhost:3306/knh_emr` | 24 |
| **CHP EMR** | **MongoDB** | `localhost:27017/chp_emr` | 14 |
| **AKUH EMR** | PostgreSQL | `localhost:5432/akuh_emr` | 25 |
| **NRB EMR** | SQL Server | `localhost:1433/nrb_emr` | 22 |

Totals: **4 connections · 85 tables · 13,818 rows · 13,818 vectors** (1 vector : 1 row here — chunking
is effectively one passage per row for these tables).

Representative AKUH clinical schema (row counts): `patients` 1000, `vital_signs` 3490,
`prescriptions` 598, `lab_orders` 443, `providers` 20, `medication_catalog` 15; and populated-later
tables at 0: `prescription_items`, `payments`, `patient_medical_history`, `patient_insurance`,
`patient_family_history`, `patient_allergies`, `lab_results`. `prescriptions` columns: `id (uuid)`,
`rx_number`, `encounter_id`, `patient_id`, `prescriber_id`, `issue_date`, `valid_until`, `status`.

**Design implications of the data model:**
- **A MongoDB source connector is required** (CHP EMR). The current `SourceKind` is
  `postgres|mysql|mssql` only — MongoDB-as-a-source is a new connector (distinct from the MongoDB-wire
  DocumentDB store we already talk to).
- **Data is cross-source but single-store**: all four EMRs land in one DocumentDB. Analytics and
  "across all clinics" questions are **one aggregation over one store**, not a federated fan-out — a
  major simplification we should lean on (see aggregation-aware-retrieval doc).
- The rich relational schema (patients ↔ encounters ↔ prescriptions ↔ lab_orders ↔ vital_signs) is
  what makes **structured/analytical questions** first-class, not an afterthought.

## Screen-by-screen

### AI Chat
Conversational RAG. Header chips: **LLM** (e.g. `Phi-4-mini-instruct-generic-cpu:5`) and **Embed**
(`qwen3-embedding-0.6b`). A model selector defaulting to **"Auto (pick for me)"**. Conversation list
in a sidebar.
- **Requires:** the **task-aware router** ("Auto") from `model-selection-and-routing.md`; a live
  model/embed badge fed by the server's resolved model; conversation persistence.

### Connections
Per-source cards (KNH/CHP/AKUH/NRB) with status + **Ingest / Disconnect / Edit / Delete**.
- **Requires:** the MongoDB connector; connection-status probe; per-source ingest trigger (exists as
  `/ingest`); edit/delete lifecycle on saved sources.

### Data Explorer
Tree of sources → tables, with **per-table row & vector counts**, column list with types, row
inspector, row search, pagination, and **Refresh / Reindex search / Clear cache / Delete**.
- **Requires:** metadata endpoints — list tables/collections, table schema (columns+types), sample
  rows, and row/vector counts per table. This **metadata catalog is also what grounds the aggregation
  query-planner** (the model needs to know the tables/columns to aggregate over). Build it once,
  reuse it for both Data Explorer and analytics.

### AI Agents
Tabbed task workspace: **Health Query · Summarize · Patient Lookup · Trends · Ingestion**. Health
Query shows a **results table + bar chart** ("Most Commonly Diagnosed Diseases").
- **Requires:** per-tab `agent_kind` routing; the **structured/analytical path** (text → aggregation →
  chart), i.e. aggregation-aware retrieval; chart-ready rows returned by the server.

### Models
Foundry Local management. Family tabs (Qwen 3.5 2B, Qwen2.5 0.5B/1.5B, DeepSeek-R1 Distill 1.5B/7B,
**Phi-4 Mini Instruct 3.8B**, Phi-4 14B). Per-family **device-variant cards**: CPU (Generic) /
GPU (WebGPU) / GPU (OpenVINO) / NPU (OpenVINO), each with size + **Load / Unload / Download** and a
live **activity console**.
- **Requires:** model list grouped by family with all device variants + cached/loaded state
  (`list_models` already returns this flat — needs UI grouping); **load/unload** per variant (select
  exists; explicit unload + the LRU resident cap from the model doc still to build); the log console
  (already have `LogConsole`).

### System Setup / Settings
Hardware detect (Intel Arc iGPU), **service health** (Foundry Chat · Embeddings · DocumentDB
port 10260), active chat model + switch, embedding-model status, loaded models, activity console.
- **Requires:** `/health` extended to per-service health; embedding-service status surfaced.
  **NOTE the embedding discrepancy below.**

### Dashboard / Analytics / Outbreak Alerts / Audit Log / Admin / Profile (target-only)
- **Dashboard:** at-a-glance counts (sources, tables, rows, vectors, ingest freshness) — cheap
  aggregations over the store.
- **Analytics:** saved/ad-hoc structured queries with charts — the aggregation path, surfaced as a
  first-class screen.
- **Outbreak Alerts:** **aggregation-aware retrieval on a schedule** — counts of a diagnosis per
  region/clinic per time window crossing a threshold → alert. Ties directly to the aggregation design.
- **Audit Log:** who accessed which PHI, when, and what query ran (incl. the executed aggregation
  pipeline for provenance) — a compliance requirement for health data.
- **Admin:** user management (roles: admin/user already exist in auth).
- **Profile:** current-user settings.

## Embeddings — the reference app made a different choice than ours

The reference app's Settings shows embeddings served by **Foundry Local `qwen3-embedding-0.6b` on
GPU (WebGPU)**. **Ours deliberately does not**: we embed via **fastembed BGE-M3** (`embed/mod.rs`) —
single ORT stack shared with the reranker, air-gappable, and **prefix-free** (see
`[[embeddings-not-in-foundry-local]]`). This is a settled design decision, not a gap to close.

Worth keeping straight because the two schemes are **not interchangeable**: BGE-M3 is prefix-free;
qwen3-embedding is asymmetric (query gets an `Instruct:` prefix, documents plain). If we ever revisit
this, switching embedding models means **re-embedding the whole store** and updating
`plans/01-retrieval-design.md` + the memory — never mix vectors from the two.

## Gap table — target vs. current build

| Capability | Current build | Target (screens) | Gap |
| --- | --- | --- | --- |
| PG / MySQL / MSSQL source | ✅ | ✅ | — |
| **MongoDB source** | ❌ | ✅ (CHP EMR) | **new connector** |
| Embeddings | fastembed BGE-M3 | (ref app uses Foundry qwen3-embedding) | settled: we keep BGE-M3 |
| Hybrid retrieval + RRF + rerank | ✅ | ✅ | — |
| Grounded single-model chat | ✅ | ✅ | — |
| **Auto model routing** | ✅ (Phases 0–3 shipped) | ✅ | — |
| **AI Agents tabs** | ✅ (Phases 0–3 shipped) | ✅ | — |
| **Aggregation/analytical path** | ✅ (Phases 0–3 shipped) | ✅ (Health Query/Trends/Analytics) | — |
| **Data Explorer metadata** | ❌ | ✅ | schema/sample/counts endpoints |
| **Model load/unload + LRU cap** | ✅ (Phases 0–3 shipped) | full lifecycle | — |
| Log streaming console | ✅ | ✅ | — |
| **Dashboard / Analytics / Outbreak Alerts** | ❌ | ✅ | aggregation-backed screens |
| **Audit Log** | ❌ | ✅ | PHI access + query provenance log |
| Admin (user mgmt) / Profile | partial (roles exist) | ✅ | UI + endpoints |

## Where Phi-4 fits (summary — full analysis in the model doc)

Yes — the Phi-4 series has three real jobs here, none of them "generalist chat":
1. **`phi-4-mini-instruct` (3.8B, tool-calling)** — the **ingestion structured-extractor / code-
   normalizer / PHI de-id worker** and the **aggregation query-planner** (text → `run_aggregation`
   spec). Small, fast, reliable structured output; can run on the **NPU/CPU so it never contends with
   the iGPU that serves chat**.
2. **`phi-4-mini-reasoning`** — the **clinical grounding/faithfulness verifier**: a cheap second pass
   that checks the answer's medication/dose/allergy claims are supported by retrieved passages.
   Safety-critical for PHI.
3. **`phi-4` (14B) / `phi-4-reasoning`** — optional high-accuracy verifier/escalation for the hardest
   clinical reasoning, on demand (accept the latency).

Caveat carried from the model doc: prefer `generic-gpu` variants for anything context-heavy; the
`openvino-npu` variants are **4224-token capped**. For the extractor/verifier roles (single record or
answer+passages) that small context is fine — which is exactly why parking Phi-4-mini on the NPU is
attractive.

## Workflow note

New design authored in the planning model. Implementation → Sonnet
(`[[use-sonnet-to-code-after-planning]]`); doc updates as things change → Haiku
(`[[use-haiku-to-update-plans]]`).
