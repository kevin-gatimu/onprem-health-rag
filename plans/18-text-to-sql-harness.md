# 18 — Text-to-SQL Harness (structured queries against live sources)

> Status: PLANNED. Adds a dialect-aware NL→SQL pipeline so aggregation / distribution / listing
> questions can be answered with **exact numbers from the operational databases** (Postgres, MySQL,
> SQL Server) instead of (or in addition to) the ingested DocumentDB copy. Generation model:
> **phi-4-mini** via Foundry Local tool-calling. Table metadata lives in DocumentDB and is embedded
> for schema linking. Depends on: `17-intent-router-v2.md` (routing + backend selection).

## Why

- The existing structured path (`aggregation/`) plans a constrained `RunAggregation` spec over the
  **ingested** `records` collection. It cannot express joins, HAVING, window functions, percentiles,
  or histogram buckets — exactly the "distribution" class of questions the user asks for.
- Source DBs are the ground truth; the DocumentDB copy is as fresh as the last ingest and may
  exclude PII columns. "How many encounters last week?" should hit the source.
- Schema linking (retrieve only relevant tables) is the accuracy lever; full multi-DB schema dumps
  blow the phi-4-mini context and cause dialect leakage.

## Architecture

```
question ──► router (plan 17, backend=SourceSql)
   │
   ▼
schema linking ──► DocumentDB `schema_catalog` (vector + $text over table cards)
   │                 top-M tables + FK-neighbor expansion → schema context
   ▼
few-shot lookup ──► `sql_examples` (validated question→SQL pairs, vector match)
   │
   ▼
generate (phi-4-mini, tool-call, per-dialect template) ──► { sql, tables, explanation }
   │
   ▼
validate (sqlparser-rs, dialect AST) ── reject/rewrite ──► SELECT-only, allowlist, LIMIT, timeout
   │
   ▼
execute (connector run_select, read-only, max rows, timeout) ── error ──► 1 repair pass
   │
   ▼
narrate (existing narration prompt) + SSE (sql/columns/rows/token) + audit log
   │
   └─ on success: store (question, sql, dialect, tables) into `sql_examples`  ← Vanna pattern
```

## New DocumentDB collections

### `schema_catalog` — one doc per (source, table)

```jsonc
{
  "_id": "<source_id>:<table>",
  "source_id": "…", "source_name": "Facility A PG", "kind": "postgres",
  "table": "encounters",
  "row_count": 182340,               // estimate from get_schema()
  "columns": [{
    "name": "admit_date", "type": "timestamp", "nullable": false,
    "is_primary_key": false, "is_foreign_key": false, "likely_pii": false,
    "sample_values": ["2026-01-03", "…"],   // low-cardinality, non-PII only (≤10)
    "description": null                      // optional operator note (admin-editable later)
  }],
  "fk_edges": [{ "column": "patient_id", "ref_table": "patients", "ref_column": "id" }],
  "card_text": "table encounters (postgres, Facility A): admit_date timestamp, …", // $text-indexed
  "cardVector": [/* 1024-dim BGE-M3 of card_text */],
  "refreshed_at": "…"
}
```

- **Card text** = compact serialization: table name, engine, column names + types + a few sample
  values + FK edges. This is what gets embedded and retrieved — the "schema card" pattern.
- **Sample values**: top ≤10 distinct values for text/enum-ish columns with distinct-count ≤ 50,
  never from `likely_pii` columns or ingest-excluded columns. Massive win for WHERE-clause literals
  ("status = 'DISCHARGED'" vs the model guessing 'discharged').
- Indexes: cosmosSearch on `cardVector` (reuse `ensure_indexes` machinery, own index name) + `$text`
  on `card_text` + btree on `source_id`.

### `sql_examples` — validated few-shots (the single biggest text-to-SQL accuracy lever)

```jsonc
{ "_id": "…", "question": "…", "sql": "…", "kind": "postgres", "source_id": "…",
  "tables": ["encounters"], "questionVector": [/*1024*/], "created_by": "…", "verified": true }
```

Seeded with ~15 curated pairs per dialect covering the target classes (count, group-by, top-N,
time-bucket trend, histogram, percentile, join+filter). Auto-append pipeline-validated successes
(flag `verified:false` until an admin approves in a later UI pass — don't let a lucky wrong query
poison future few-shots).

## Server module: `onprem-rag-server/src/nl2sql/`

### `catalog.rs` — build + refresh
- `refresh_catalog(db, source_id)`: `connector.get_schema()` → per table: sample values via one
  guarded query per candidate column (`SELECT DISTINCT col FROM t LIMIT 11`, skipped when
  `likely_pii` or excluded) → build card → `embed_documents` (batch 32) → upsert.
- FK edges: extend connector introspection — PG `information_schema.table_constraints` +
  `key_column_usage`; MySQL `key_column_usage.referenced_table_name`; MSSQL `sys.foreign_keys`.
  (`ColumnSchema.is_foreign_key` exists; add the *target* info to `TableSchema` as `fk_edges`.)
- Hooks: `POST /sources` (create) and end of every ingest job call refresh; manual
  `POST /sources/<id>/catalog/refresh` (admin).

### `linker.rs` — schema linking
- `link(db, question, entity_hints) -> Vec<TableCard>`: embed question (existing `embed_query`) →
  cosmosSearch top-8 over `schema_catalog` + `$text` over `card_text` → RRF (reuse `retrieval::rrf`)
  → take top-M (`ONPREM_NL2SQL_TABLES_MAX=4`) → expand 1 hop along `fk_edges` (join keys must be in
  context) → group by `source_id`; pick the source with the highest aggregate score (one query hits
  exactly one source DB — cross-source federation is explicitly out of scope).

### `generate.rs` — phi-4-mini, per-dialect prompt
- One template per dialect (NOT one generic template — dialect leakage is the top failure mode):
  - **postgres**: `LIMIT n`, `date_trunc`, `percentile_cont(...) within group`, `width_bucket`,
    `string_agg`, `ilike`.
  - **mysql**: `LIMIT n`, `DATE_FORMAT`, no `percentile_cont` → window `NTILE`/`PERCENT_RANK`
    patterns, `GROUP_CONCAT`, `FLOOR(x/w)*w` histograms.
  - **mssql**: `TOP n` / `OFFSET…FETCH`, `DATETRUNC`/`FORMAT`, `PERCENTILE_CONT` (window),
    `STRING_AGG`, bracket quoting.
- System prompt = dialect rules + schema cards + 2–3 nearest few-shots (from `sql_examples` by
  question-vector similarity, same dialect) + hard rules: single SELECT statement, no comments,
  always fully qualify columns when >1 table, prefer explicit JOIN via listed fk_edges, include
  `LIMIT/TOP` yourself.
- Output via **strict tool-call** `emit_sql { sql: string, tables: string[], explanation: string }`
  (same `plan_aggregation` machinery in `foundry/mod.rs` — add `emit_sql_tool()` + `plan_sql()`).
- `ModelSpec`: new `AgentKind::TextToSql` → alias `ONPREM_MODEL_TEXT2SQL=phi-4-mini-instruct`,
  `thinking:false`, `temperature:0.1`, `tools:true`, device `[Npu, Cpu]` (fits the 4224 NPU cap:
  cards+shots budget ≈ 2.5k tokens; enforce by truncating cards to M tables).

### `validate.rs` — sqlparser-rs policy gate (never execute raw LLM SQL)
- New dep: `sqlparser = "0.5x"` (pin latest; dialects: `PostgreSqlDialect`, `MySqlDialect`,
  `MsSqlDialect`).
- Checks (AST walk):
  1. Parses to exactly **one** `Statement::Query` (no multi-statement, no DDL/DML/`SELECT … INTO`).
  2. Table allowlist: every `TableFactor::Table` (including CTEs resolved) ∈ linked tables for the
     chosen source. Reject unknown columns when resolvable (best-effort; unresolved → allow, the DB
     will error and trigger the repair pass).
  3. No dangerous functions: `pg_sleep`, `pg_read_file`, `load_file`, `sleep`, `benchmark`,
     `openrowset`, `xp_*`; no `INTO OUTFILE`; no locking clauses (`FOR UPDATE/SHARE`).
  4. Row cap: inject `LIMIT ONPREM_NL2SQL_MAX_ROWS` (default 500) / `TOP` when the outer query lacks
     one and has no aggregate-without-group (a lone `COUNT(*)` needs no limit).
  5. Rewrite emitted from the **AST** (`ast.to_string()`), not the raw model text — normalization
     kills comment/homoglyph smuggling.

### `execute.rs` — read-only execution
- Extend `SourceConnector` with:
  ```rust
  /// Run one validated read-only SELECT. Returns column names + rows as JSON values.
  async fn run_select(&self, sql: &str, max_rows: i64, timeout: Duration)
      -> AppResult<(Vec<String>, Vec<Vec<serde_json::Value>>)>;
  ```
- Read-only enforcement (defense in depth, validation is the primary gate):
  - **Postgres**: `BEGIN READ ONLY; … ; COMMIT` + `SET LOCAL statement_timeout`.
  - **MySQL**: `SET SESSION TRANSACTION READ ONLY` + `SET SESSION max_execution_time`.
  - **MSSQL**: no session read-only → rely on AST gate + document least-privilege (db_datareader)
    in README; wrap with `SET LOCK_TIMEOUT` and a tokio timeout.
- Cell decode reuses the existing per-column JSON fallthrough from `fetch_table` (factor the shared
  decode helpers out of `mysql.rs`/`mssql.rs` rather than duplicating).
- Tokio-level `timeout(ONPREM_NL2SQL_TIMEOUT_SECS=30)` wraps every engine.

### `repair` — one-shot error feedback
- On execution error: re-prompt phi-4-mini once with the original context + failed SQL + the DB
  error string ("fix the query; same rules"). Second failure → structured DocDb fallback →
  semantic fallback (plan 17 chain). Never loop more than once.

## Distribution / aggregation-aware coverage (acceptance queries)

| Class | Example | Postgres shape |
|---|---|---|
| Count/filter | how many encounters in July? | `SELECT count(*) … WHERE date_trunc('month',…)` |
| Group + top-N | top 5 diagnoses by patient count | `GROUP BY … ORDER BY count DESC LIMIT 5` |
| Trend | admissions per month this year | `date_trunc('month') GROUP BY 1 ORDER BY 1` |
| Distribution | age distribution of diabetic patients | `width_bucket(age,0,100,10) GROUP BY bucket` |
| Percentile | median length of stay | `percentile_cont(0.5) WITHIN GROUP (ORDER BY los)` |
| Ratio/split | male/female split | `GROUP BY gender` + narration computes % |
| Join+filter | patients with >3 visits | `JOIN … GROUP BY … HAVING count(*) > 3` |

The MySQL/MSSQL few-shot seeds mirror each class in their dialect. These 7 classes are the test
fixture set (below) and drive the router's structured markers (plan 17 Tier 1 additions).

## Routes, SSE, app

- `POST /agents/sql` (AuthUser): body `{ question, source_id? }`. SSE events:
  `routed` → `sql { sql, kind, source, tables }` → `columns` → `rows` (≤ max_rows) → `token`
  (narration) → `done` | `error`. `/chat` reaches the same executor through the router
  (backend=SourceSql) and emits identical events.
- **Audit**: log `{ user, ts, question, source_id, sql, row_count, duration_ms, outcome }` — the SQL
  actually executed (post-AST-rewrite), reusing the audit write-path from stage 10.
- Bridge: relay `agent://sql|columns` (new) alongside existing events. App: Agents tab gains a
  "Data Query" sub-tab: SQL shown in a copyable block (syntax-highlight optional), result table
  reusing the existing rows table, MiniChart when result is (label, numeric) shaped.

## Config surface (`.env.example`)

```
ONPREM_TEXT2SQL_ENABLED=false          # master switch (router won't pick SourceSql when off)
ONPREM_MODEL_TEXT2SQL=phi-4-mini-instruct
ONPREM_NL2SQL_TABLES_MAX=4             # schema cards per prompt
ONPREM_NL2SQL_FEWSHOTS=3
ONPREM_NL2SQL_MAX_ROWS=500
ONPREM_NL2SQL_TIMEOUT_SECS=30
ONPREM_NL2SQL_SAMPLE_VALUES=10         # per column, 0 disables sampling
```

## Phases + gates

1. **Catalog** — `nl2sql/catalog.rs`, FK introspection in 3 connectors, refresh hooks + route,
   indexes. Gate: refresh against the dev-sources Postgres yields cards with samples + fk_edges;
   cosmosSearch over cards returns `encounters` for "admissions per month".
2. **Linker + few-shot store** — `linker.rs`, `sql_examples` seeding CLI-ish route
   (`POST /nl2sql/examples` admin). Gate: unit tests on RRF-fused linking with hint injection.
3. **Generate + validate** — `generate.rs`, `validate.rs`, `plan_sql` tool-calling, dialect
   templates. Gate: `cargo test` fixture suite — 7 classes × 3 dialects of *expected-shape* asserts
   (parse + policy pass), plus adversarial fixtures (multi-statement, UPDATE, pg_sleep, unknown
   table → all rejected).
4. **Execute + repair + SSE** — connector `run_select`, `/agents/sql`, audit, bridge + UI sub-tab.
   Gate: end-to-end against seeded Postgres: all 7 acceptance queries return correct numbers
   (verified against hand-written SQL); MySQL/MSSQL smoke via docker profiles.
5. **Router integration** — flip `ONPREM_TEXT2SQL_ENABLED=true` path in plan 17 Phase D; fallback
   chain verified (kill the source container mid-demo → answer still arrives via DocDb/semantic).

## Risks

- **phi-4-mini SQL quality**: mitigated by schema cards + samples + dialect few-shots + repair pass;
  if accuracy stalls, `ONPREM_MODEL_TEXT2SQL` can point at qwen3-8b (GPU) without code changes.
- **PII leakage via samples**: sampling skips `likely_pii` and ingest-excluded columns; catalog
  refresh runs the same deterministic PII pass used by the ingest wizard before sampling.
- **sqlparser dialect gaps** (vendor quirks may not parse): treat parse failure as validation
  failure → repair pass emits more conservative SQL; log the construct for template tuning.
- **Stale catalog**: refreshed on ingest + manual; `refreshed_at` surfaced in the UI so operators
  can see drift.
