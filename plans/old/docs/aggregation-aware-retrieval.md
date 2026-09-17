# Aggregation-aware retrieval

> **Superseded.** This file is retained for link/history stability. The authoritative replacements are [Routing, Agents, and Structured Query](routing-agents-and-structured-query.md) and [Deterministic SQL Matcher](deterministic-sql-matcher.md). Do not treat the statuses, defaults, or diagrams below as current.

> How the app answers **counting / grouping / trend / top-N** questions correctly, instead of guessing
> from a handful of retrieved passages. Design note, 2026-08-23. Companion to
> `plans/01-retrieval-design.md` (semantic pipeline), `plans/docs/model-selection-and-routing.md`
> (which model plans/executes), and `plans/docs/app-feature-surface-and-requirements.md`
> (the Health Query / Trends / Analytics / Outbreak Alerts screens this powers).

## The problem: plain RAG cannot count

The standard pipeline (embed query → top-k nearest chunks → stuff into prompt → generate) is built to
answer *"what/why/tell me about"* questions where the answer lives **inside a few passages**. It is
structurally wrong for **aggregative** questions:

- *"How many patients have diabetes?"* — top-k returns ~6 diabetic patients out of hundreds. The model
  sees 6 and either guesses a number or says "at least 6." **Both are wrong.**
- *"Most commonly diagnosed diseases this quarter"* (the reference app's bar chart) — the answer is a
  `GROUP BY diagnosis` over the whole population, not a passage.
- *"Trend of malaria cases per month across all clinics"* — a time-bucketed count over 4 EMRs.
- *"Average age of hypertensive patients at KNH vs AKUH"* — a grouped mean with a filter.

The answer to these is a **computation over the data**, and it must be **exact** (health context — a
fabricated case count is a safety/compliance problem). Retrieval alone can't produce it.

**Aggregation-aware retrieval** = the system recognizes analytical intent and routes to a
*compute-over-the-store* path, using retrieval to **scope the population** rather than to supply the
answer. The numbers come from a database aggregation; the model only narrates them.

## The two paths (recap)

From `model-selection-and-routing.md`:

1. **Semantic RAG path** — Chat, Patient Lookup (narrative), general Q&A. Passages → grounded answer.
2. **Structured / analytical path** — Health Query, Trends, Analytics, Outbreak Alerts. Model emits a
   **tool call** → Rust executes a DocumentDB aggregation → model narrates the returned rows → UI
   charts them.

Aggregation-aware retrieval is the design of path 2, plus the **hybrid case** where the two combine.

## Pipeline

```
question
  │
  ├─▶ 1. intent classification ──▶ lookup | narrative | aggregation | trend | multi-hop
  │
  ├─(aggregation/trend)─▶ 2. schema-grounded query planning (tool-call: run_aggregation spec)
  │                        │
  │                        ├─▶ 3. validate spec (allow-list fields, read-only, caps)
  │                        ├─▶ (optional) 3b. semantic pre-filter → cohort of row_pks
  │                        ├─▶ 4. execute DocumentDB aggregation pipeline  → exact rows
  │                        └─▶ 5. grounded narration over rows + 6. chart-ready payload to UI
  │
  └─(lookup/narrative)──▶ existing semantic pipeline (plans/01)
```

### 1. Intent classification
Decide the path before retrieving. Cheap and layered:
- **Lexical markers** (fast, no model): *how many, count, number of, total, average/mean, rate,
  per (month|clinic|region), trend, over time, most/least common, top N, compare, distribution*.
- **Small-model fallback** for ambiguous cases — the **fast lane / `phi-4-mini-instruct`** classifies
  into `{lookup, narrative, aggregation, trend, multi_hop}`. Never the big model; this is a hot path.
- On the AI Agents screen the **tab already declares intent** (Health Query/Trends = analytical), so
  classification is only needed for the free-form **AI Chat "Auto"** box.

### 2. Schema-grounded query planning
The model does **not** write raw SQL/Mongo. It is handed a compact **schema catalog** and emits a
constrained **aggregation spec** via tool-calling:

```jsonc
// run_aggregation(spec)
{
  "collection": "records",          // or a logical entity: patients | encounters | prescriptions …
  "filter": { "diagnosis_code": {"$prefix": "E11"}, "clinic": "KNH" },
  "group_by": ["diagnosis_display"],
  "metric": { "op": "count" },       // count | sum:<field> | avg:<field> | min | max | distinct:<field>
  "time_bucket": { "field": "encounter_date", "unit": "month" },  // optional (trends)
  "sort": { "by": "value", "dir": "desc" },
  "top_n": 10
}
```

Grounding the planner needs a **metadata catalog** (see below) so the model knows which
collections/fields exist and what code vocabularies mean. This is the same catalog Data Explorer needs
— **build it once, reuse it.**

### 3. Validate (safety — non-negotiable for PHI)
The spec is executed **only** after validation, never the model's free text:
- **Field/collection allow-list** from the catalog — reject unknown names (blocks injection & drift).
- **Read-only**: only `$match/$group/$sort/$limit/$bucket/$count/$project`. No `$out`, `$merge`,
  `$function`, `$where`, no writes.
- **Hard caps**: `$limit` on groups (e.g. ≤ 500), pipeline timeout, `maxTimeMS`.
- **Access scope**: apply the user's authz (role/clinic) as a mandatory `$match` prefix.
- **Audit**: log the resolved pipeline + row count + user → feeds the **Audit Log** screen (provenance:
  the answer is reproducible and defensible).

### 3b. Optional semantic pre-filter — the "aggregation-aware retrieval" core
Some questions filter on **meaning**, not a column:
- *"How many patients whose notes mention **sepsis** were readmitted within 30 days?"*
- *"Average length of stay for encounters **described as complicated deliveries**."*

Here retrieval **defines the cohort** and aggregation **computes over it**:
1. Vector + `$text` search over the semantic field → a set of matching `row_pk`s (the cohort).
2. Inject `{ row_pk: { $in: [...] } }` into the aggregation `filter`.
3. Aggregate over just that cohort.

This is the literal meaning of *aggregation-aware retrieval*: retrieval is **shaped by the analytical
goal** (return the population to count), not by "give me 6 passages to read." Note the cohort can be
large — retrieve `row_pk`s in bulk (raise the k, or run a filter-only vector pass), not the top-6.

### 4. Execute against DocumentDB
Translate the validated spec to a DocumentDB aggregation pipeline (`$match → [$bucket] → $group →
$sort → $limit`). **Single-store advantage:** all four EMRs already live in one DocumentDB, so
"across all clinics" is one pipeline — no federation. `clinic`/`source_id` is just another `group_by`
or `filter` dimension.

### 5. Grounded narration
Feed the **returned rows** back to the model with a strict instruction: *narrate only these numbers;
do not invent or extrapolate.* The counts come from the DB; the model supplies prose. This eliminates
numeric hallucination — the failure mode that makes plain-RAG analytics unsafe.

### 6. Chart-ready payload
The aggregation result is **already** `[{label, value}, …]` (+ a time axis for trends) — the exact
shape a bar/line chart wants. Return `{ rows, narration, spec, executed_pipeline }`:
- `rows` → the UI table + chart (deterministic, not inferred from prose).
- `spec`/`executed_pipeline` → provenance for Audit Log and for a "show the query" affordance.

## The metadata catalog (shared dependency)
Build at ingest time and refresh on demand. Per source + globally:
- collections/logical entities, their fields + types, and **row/vector counts** (Data Explorer shows
  these already);
- **code vocabularies** present (diagnosis, medication, lab codes) with display names, so the planner
  can map "diabetes" → ICD-10 `E11*`;
- a curated **field synonym map** (e.g. `dob`/`date_of_birth`/`birthdate`) for cross-EMR consistency,
  since the four source schemas differ.

Without this, the planner hallucinates field names. With it, planning is constrained and cheap.

## How it maps to the app screens
- **Health Query** → path 2, no time bucket. Table + bar chart.
- **Trends** → path 2 with `time_bucket`. Line/area chart; thinking-on model for interpretation.
- **Analytics** → saved/ad-hoc specs; a spec is serializable, so "saved analytics" = stored specs.
- **Outbreak Alerts** → **scheduled** aggregation: run counts per `{diagnosis, clinic/region, week}`
  on a timer; when a bucket crosses a threshold (or z-score vs. baseline), raise an alert. This is
  aggregation-aware retrieval with no user in the loop — the strongest argument that this path is
  core, not a nicety.
- **Dashboard** → cheap top-level `$count`s (sources/tables/rows/vectors, recent ingest).

## How to hook it into the server (`onprem-rag-server/src/`)

✅ **1. Intent** (`aggregation/intent.rs` + router)
   `enum QueryIntent { Lookup, Narrative, Aggregation, Trend, MultiHop }` + lexical `classify_lexical()`. AI Agents tabs pass intent explicitly; Chat "Auto" still lexical-only (small-model fallback deferred to Phase 4).

✅ **2. `run_aggregation` tool** (`aggregation/spec.rs` + `execute.rs`)
   Execution primitive: `spec → validate → DocumentDB pipeline → rows`. `RunAggregation` spec with Metric/TimeBucket/Sort; `MAX_TOP_N=500`. Pipeline: `$match(authz∧filter)` → **`$group{_id:$row_pk, doc:$first}` dedup** → optional time `$addFields` → metric `$group` → `$sort` → `$limit` → `$project`. `maxTimeMS=30000`. Rows returned as `AggRow{label, value:f64}`.

✅ **3. Validation** (`aggregation/validate.rs`)
   Field/collection allow-list (reject unknown); read-only blocks (`$where/$function/$accumulator/$expr`); hard caps (top_n≤500); authz `$match` prefix applied automatically.

✅ **4. Metadata catalog** (`aggregation/catalog.rs`)
   Hardcoded 6-collection seed (records, patients, encounters, prescriptions, lab_orders, vital_signs) with ~30 fields + 12 disease→ICD-10 prefixes + 11 synonyms. `planner_context()` grounds the tool-calling planner. **Ingest-time population still open (Phase 4).**

✅ **5. Endpoint & tool-calling** (`agents/routes.rs`)
   `POST /agents/<kind>` (AuthUser-guarded). Structured kinds: plan→validate→execute→narrate. `foundry/mod.rs` `plan_aggregation(...)` native tool-calling (tool_choice=Function, response_format=JsonSchema, one reprompt). SSE contract: `citations | spec | rows | pipeline | token | error | done`. Body: `{question, intent}`.

⏳ **6. Audit hook** — persist `{user, ts, intent, spec, executed_pipeline, row_count}` for the Audit Log (Phase 4).

⏳ **3b. Semantic pre-filter (cohort)** — retrieval defines cohort, aggregation computes over it; still open.

## Open questions
- **Cohort size cap** for 3b — how large a semantic pre-filter set is safe to `$in`? Consider tagging
  rows at ingest so common cohorts are filterable by column instead of by `row_pk` list.
- **Planner reliability** — validate `phi-4-mini-instruct` / `qwen3-8b` tool-call accuracy against the
  real schema before trusting analytics; keep a rules-based fallback for the top N canned questions.
- **Cross-EMR field harmonization** — the synonym map is manual today; revisit if source count grows.
- **Trend baselines for outbreak alerts** — fixed threshold vs. rolling z-score vs. seasonal — decide
  when that screen is built.

## Workflow note
Design authored in the planning model. Implementation → Sonnet
(`[[use-sonnet-to-code-after-planning]]`); doc updates as things change → Haiku
(`[[use-haiku-to-update-plans]]`).
