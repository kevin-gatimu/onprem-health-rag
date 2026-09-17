<!-- markdownlint-disable MD013 -->

# 04 — Structured execution and fallback ladder

**Goal:** one `StructuredExecutor` that implements the flowchart's structured branch end-to-end —
*link one source → deterministic SQL → model SQL proposal → AST validation → guarded execution →
exact rows + provenance*, with *bounded failure → validated internal aggregation → semantic RAG* —
and is called identically by `/chat` and every `/agents/<kind>`. Also delivers the two enablers the
ladder needs: **scoped DocumentDB aggregation** and a **retrieval table filter**, and turns
`Hybrid` into a real cohort → retrieval → synthesis path.

**Depends on:** 02 (decision carries line/scope/spec), 03 (`QuerySpec`). **Unblocks:** 05, 06, 07.

---

## 1. Today's duplication

The ladder exists but is spread across `rag/routes.rs::chat` (SourceSql → `run_structured` →
semantic), `agents/routes.rs` (per-kind variants, `should_try_deterministic_sql`),
`resolve_followup_sql`, `resolve_semantic_sql`. Each site chooses slightly different fallbacks and
emits slightly different SSE. Provenance is inferred client-side from *which* events arrived.

## 2. `StructuredExecutor` (`answer/executor.rs`, new; `answer.rs` becomes `answer/mod.rs`)

```rust
pub struct ExecInput<'a> {
    pub decision: &'a RouteDecision,       // plan 02
    pub question: &'a str,                 // resolved_question
    pub user: &'a AuthUser,
    pub memory: &'a WorkingMemory,
    pub focus: &'a ConversationFocus,      // plan 06
    pub mode: AgentMode,                   // Ask | Trends | Handover (plan 05)
    pub trace: &'a RequestTrace,
    pub budget: ExecBudget,                // deadlines per rung
}

pub struct ExecBudget { pub sql_plan: Duration, pub sql_exec: Duration, pub agg: Duration, pub total: Duration }

pub enum ExecOutcome {
    SourceSql   { source_id: String, sql: String, spec: Option<QuerySpec>, columns: Vec<String>, rows: Vec<Vec<Value>>, explanation: String },
    DocDbAgg    { spec: RunAggregation, rows: Vec<AggRow>, pipeline: Vec<Document> },
    DocDbList   { spec: RunList, rows: Vec<Value>, total: u64, citations: Vec<Passage> },
    Semantic    { passages: Vec<Passage>, filter: RetrievalFilter },
    Hybrid      { cohort: Box<ExecOutcome>, passages: Vec<Passage>, filter: RetrievalFilter },
    Clarify     { question: String, slot: MissingSlot, options: Vec<String> },
    Conversational,
}

pub struct Provenance {
    pub path: Vec<Rung>,                   // ordered attempts, e.g. [DeterministicSql(Hit)]
    pub backend: &'static str,             // "source_sql" | "document_db" | "semantic" | "hybrid" | "none"
    pub service_line: Option<ServiceLine>,
    pub scope: Vec<String>,
    pub source_id: Option<String>,
    pub elapsed_ms: HashMap<&'static str, u64>,
}
pub enum Rung { Link(RungResult), DeterministicSql(RungResult), ModelSql(RungResult), Validate(RungResult), Execute(RungResult), Aggregation(RungResult), List(RungResult), Retrieval(RungResult), Clarify }
pub enum RungResult { Hit, Miss(String /* PHI-free reason */), Skipped(&'static str) }

pub async fn run(state: &AppState, input: ExecInput<'_>) -> AppResult<(ExecOutcome, Provenance)>
```

### 2.1 Ladder (exact order)

```text
match decision.class
  Conversational | ConversationMeta → Conversational
  Clarify{..}                       → Clarify
  Semantic                          → rung 6
  Structured{backend: SourceSql}    → rungs 1,2,3,4,5,6
  Structured{backend: DocDb}        → rungs 4,5,6
  Hybrid                            → rungs 1..4 for the cohort (must yield rows) → rung 6 with filter → Hybrid

1 Link      binding = state.binding(decision.source_id); cards = linker.link(scope-filtered) — Miss → rung 4
2 Det. SQL  spec = decision.query_spec or ir::parse(); bind; compile — Miss → rung 3
3 Model SQL plan_spec → bind → compile; on BindError → plan_sql (raw) once — Miss → rung 4
   ↳ both 2 and 3 flow into: validate_sql(allowed = scope ∩ linked) → run_select (cost preflight, timeout)
     validation/exec error on rung 2 → try rung 3 once; on rung 3 → rung 4
4 Aggregation  (only if intent ∈ {Aggregation, Trend} and catalog has collections in scope)
     spec = spec_from_query_spec(query_spec, catalog) else simple_patient_count_plan else plan_aggregation(model, scoped catalog)
     validate → execute (30 s) — Miss → rung 5 if Enumeration/Lookup else rung 6
5 List         (intent ∈ {Enumeration, Lookup}) plan_list(scoped) → validate → run — Miss → rung 6
6 Retrieval    filter = RetrievalFilter::from(decision, focus); retrieve_observed_filtered(); if filter empties results and filter was inferred (not explicit) → retry unfiltered once, record Rung::Retrieval(Miss("filter relaxed"))
```

"Bounded failure" is defined precisely: a rung is *Miss* on parse miss, bind error, validation
error, connector error, timeout, zero-row **scalar** result when the question implied existence
(`Exists` shape), or cost-preflight rejection. Zero rows on List/Grouped is a **Hit** with an
empty result — the narrator says so; it does not trigger a fallback (that would silently swap an
exact "none" for a fuzzy guess).

Per-rung deadlines from `ExecBudget` (defaults: sql_plan = `ONPREM_NL2SQL_PLAN_TIMEOUT_SECS`,
sql_exec = `ONPREM_NL2SQL_TIMEOUT_SECS`, agg = 30 s, total = `ONPREM_EXEC_TOTAL_TIMEOUT_SECS` 120).
When the total is exhausted, skip to rung 6 with `Skipped("budget")`.

### 2.2 Narration

`answer/narrate.rs`: `narrate(outcome, mode, focus) -> NarrationPlan { system, user, spec }`.
Builds on `NARRATION_SYSTEM_PROMPT`; adds the compiler `explanation` and the *agent persona*
(plan 05) and, in `Handover` mode, a structured template (situation / background / assessment /
recommendation headings) — still "narrate only the rows". For `SourceSql` results ≤ 3 rows × 3
columns, skip the model and render `summarize_result` directly (`Structured::Direct` today).

## 3. Scoped DocumentDB aggregation

`aggregation/catalog.rs`:

- `Catalog::scoped(&self, tables: &[String]) -> Catalog` — collections filtered to scope; planner prompt built from the scoped catalog so the model *cannot* name an unowned collection (data-map §6).
- `spec_from_query_spec(spec: &QuerySpec, catalog: &Catalog) -> Option<RunAggregation | RunList>` — deterministic translation of IR into the DocDb spec when the subject table is ingested: `Count → Count`, `Sum/Avg/Min/Max(target) → op on fields.<col>`, `Dimension → group_by`, `TimeScope → time_bucket`, `Filter{Eq/In/Gt..} → $match` operators, `Order/limit → sort/top_n`. Joins are not translatable → `None` (rung 4 uses model planning or misses).
- `execute.rs::build_pipeline` `$match` gains `source_id ∈ scope_sources` when the scope names a source.

## 4. Retrieval filter

`retrieval/mod.rs`:

```rust
#[derive(Debug, Clone, Default, Serialize)]
pub struct RetrievalFilter {
    pub tables: Vec<String>,          // from decision.scope when service_line is Some
    pub source_ids: Vec<String>,
    pub row_pks: Option<Vec<String>>, // hybrid cohort keys (≤ ONPREM_HYBRID_COHORT_MAX 200)
    pub patient_key: Option<String>,  // fields.<BusinessId col> or row_pk of patient table + child rows via fields.<PatientRef col>
    pub explicit: bool,               // true when user named the scope; false when inferred (allows relaxation)
}
pub async fn retrieve_observed_filtered(db, config, queries, mode, rerank, top_k, filter: &RetrievalFilter, trace) -> AppResult<Vec<Passage>>
```

Implementation: `$match` stage placed **before** `cosmosSearch` is not permitted (cosmosSearch
must be first) — instead pass the filter through the `cosmosSearch` `filter` option (supported for
`vector-ivf`/`hnsw` in DocumentDB; **VERIFY-EARLY** live: `{"$search": {"cosmosSearch": {"vector":…,
"path": "vector", "k": 50, "filter": {"table": {"$in": [...]}}}}}`). If the container rejects
`filter`, fall back to over-fetching `k × 4` and `$match`-ing after, capped at 400. `$text` side:
plain `$match` with `$text` + filter fields. Required indexes: compound `{ table: 1, active: 1 }`,
`{ source_id: 1, table: 1, active: 1 }`, `{ "fields.<patient business id>": 1 }` created by
`ensure_indexes` for the columns bound `BusinessId` on Patient tables (plan 01 knows them).

`patient_key` resolution: the ingested `records` doc for a child row carries
`fields.<PatientRef col>` (UUID) — not the human `patient_no`. `RetrievalFilter::for_patient`
resolves `patient_no → patients.row_pk` with one `find_one`, then filters children on
`fields.<PatientRef col> == that pk` for every bound table with a 1-hop `patient_path`, plus the
patient row itself. Longer paths are skipped (documented limitation).

## 5. Hybrid cohort executor

Rungs 1–4 run with the cohort `QuerySpec` forced to `Shape::List` and projection = `[PrimaryKey,
BusinessId, PatientRef]`. From the rows build `RetrievalFilter { row_pks | patient_keys, explicit:
true }` (cap 200; if more, keep the first 200 by the spec's order and tell the narrator "first 200
of N"). Run rung 6 with the narrative part of the question. Narrate with both the cohort summary
line ("Cohort: 37 admissions over 14 days") and citations. SSE emits `spec`/`rows`/`sql` for the
cohort **and** `citations` for the passages.

## 6. SSE contract (shared by `/chat` and `/agents/*`)

Order: `routed` → [`clarify`] → [`sql` `columns` `rows`] | [`spec` `rows` `pipeline`] | [`citations`] →
**`provenance`** → `token`* → [`verify`] → [`suggestions`] (plan 06) → `done` | `error`.

`provenance` payload = `Provenance` JSON (PHI-free: rung names, reasons like "bind error: no
EventTime on Bill", timings). `sql` payload gains `spec` (the `QuerySpec`) and `explanation`.
`routed` unchanged from plan 02.

Persisted `messages` gain `provenance` and `spec` fields (`StoredMessage` mirror in plan 07).

## 7. Files

- `answer/mod.rs` (moved), `answer/executor.rs`, `answer/narrate.rs`, `answer/provenance.rs` — new.
- `rag/routes.rs::chat`, `agents/routes.rs::agent` — replace inline ladders with `executor::run`; delete `resolve_followup_sql`, `resolve_semantic_sql` (focus handles them in 02/06), `should_try_deterministic_sql`, `class_to_agent_kind` (replaced in 05).
- `aggregation/catalog.rs` (`scoped`, `spec_from_query_spec`), `aggregation/execute.rs` (`source_id` match).
- `retrieval/mod.rs` (`RetrievalFilter`, `retrieve_observed_filtered`), `documentdb/mod.rs` (indexes).
- `config.rs`: `ONPREM_EXEC_TOTAL_TIMEOUT_SECS` (120), `ONPREM_HYBRID_COHORT_MAX` (200), `ONPREM_RETRIEVAL_FILTER_RELAX` (true).

## 8. Tests

- Ladder state machine with a mocked `Rungs` trait: every `(class, backend, intent)` × failure point yields the documented next rung; total-budget skip; no fallback on empty List.
- `spec_from_query_spec` round-trips the A-series specs to `RunAggregation` where single-table.
- Retrieval filter: live test that a Maternity-scoped query never returns `billing_*` passages; relaxation path when the scope is inferred and empty.
- Hybrid: "summarise the notes of patients admitted more than 14 days" on the dev seed yields cohort rows + only clinical_notes/admissions passages.
- SSE ordering test on `/chat` and `/agents/maternity`: `provenance` precedes the first `token`.

## 9. Acceptance

- `/chat` and all agents share one executor; grep shows a single call site for `prepare_auto_query*`.
- `provenance.path` present on 100 % of persisted assistant messages created after the change.
- Semantic fallbacks that happen after a structured miss carry `Rung::*(Miss(reason))` — none carry an empty path.
