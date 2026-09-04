# 07 — Ingest Wizard Implementation (Stage 4)

**Status:** ✅ IMPLEMENTED (2026-08-25). This is the authoritative contract for Stage 4 of the mobile-first
UI rebuild. Server (Rust), bridge (src-tauri + bridge.ts), and app (store + wizard UI) all implement
against the field names and route shapes defined here. Ported from the Electron reference's ingest
wizard, adapted to our **stateless** Rust/Tauri architecture.

> **Naming:** the reference is TypeScript camelCase. **Our contract is snake_case end-to-end** (Rust
> serializes snake_case; bridge.ts mirrors snake_case; UI reads snake_case), matching every prior stage
> (`total_records`, `last_connected`, …). Where this doc lists a reference camelCase name, it is only a
> cross-reference — implement the snake_case name.

---

## Wizard flow (UI state machine)

Six steps, one page (`/ingest`). State lives in a **Zustand `ingestion` store that survives navigation**
(so leaving the page mid-ingest and coming back keeps the live view).

```
pick-connection → loading-schema → analyzing → select-tables → ingesting → complete
```

1. **pick-connection** — source list (reuse `listSources`) + ingestion-history card (`getIngestHistory`).
   Picking a source → `loading-schema`.
2. **loading-schema** — spinner; calls `getSchema(source_id)` → `TableSchema[]`. On success → `analyzing`.
3. **analyzing** — spinner; calls `analyzeSchema({ tables })` → `SchemaAnalysis`. On success → `select-tables`.
   (If analysis fails, still advance to `select-tables` with an empty analysis — analysis is advisory,
   never a hard gate. Surface a warning toast.)
4. **select-tables** — per-table selectable list; expandable columns; PK badge + PII badge; per-table
   column exclusion via an eye toggle; sticky "Start Ingestion" bar. → `ingesting`.
5. **ingesting** — two-column split (RunSummary | ProgressPanel), live SSE. On terminal event → `complete`.
6. **complete** — summary + "New ingestion" (→ `resetWizard`, back to `pick-connection`).

"Start over" at any point → `resetWizard`.

---

## Data types (snake_case — the shared contract)

```ts
// GET /sources/<id>/schema  →  TableSchema[]
interface ColumnSchema {
  name: string;
  type: string;              // db type, e.g. "integer", "varchar", "timestamp"
  nullable: boolean;         // in type, NOT rendered in UI
  is_primary_key: boolean;   // → PK badge
  is_foreign_key: boolean;   // in type, NOT rendered in UI
  likely_pii: boolean;       // get_schema always sets false; analyze fills it via pii_columns
}
interface TableSchema {
  name: string;
  row_count: number;         // fast estimate (reltuples / TABLE_ROWS / sys.partitions), not exact
  columns: ColumnSchema[];
}

// POST /schema/analyze  { tables: TableSchema[] }  →  SchemaAnalysis
interface SchemaAnalysis {
  summary: string;
  suggested_tables: string[];
  pii_columns: Record<string, string[]>;   // table_name → [column names likely PII]
  data_quality_notes: string[];
}

// POST /ingest  (widened)
interface IngestRequest {
  source_id: string;
  tables: string[];
  excluded_columns?: Record<string, string[]>;  // table_name → columns to DROP (not embedded/stored)
  limit?: number;                                // optional, smoke-test cap per table
}

// SSE ingest://progress payload  (the 14-field event)
type IngestStatus = 'running' | 'completed' | 'partial' | 'failed';
interface LogEntry {
  time: string;                                          // ISO
  level: 'info' | 'success' | 'warn' | 'error' | 'divider';
  message: string;
}
interface IngestProgress {
  job_id: string;
  status: IngestStatus;
  table_index: number;        // 1-based index of current table
  total_tables: number;
  current_table: string;
  table_rows: number;         // rows processed in current table
  table_total: number;        // est. rows in current table
  processed_rows: number;     // rows processed across all tables
  total_rows: number;         // est. rows across all selected tables
  errors: number;             // count (detail goes into log at level:error)
  success_tables: number;
  failed_tables: number;
  db_size_bytes: number;      // cumulative UTF-8 bytes of embedded chunk text (our proxy for "DB size")
  log: LogEntry[];            // FULL current log, server-capped 500; client replaces wholesale each event
}

// GET /ingest/history  →  IngestionHistoryConnection[]
interface IngestionHistoryTable {
  table_id: string;           // "{source_id}:{table}"
  source_table: string;
  row_count: number;
  vector_count: number;
  status: string;             // "indexed" | "error" | "indexing"
  last_ingested: string | null;
  last_embedded_at: string | null;
}
interface IngestionHistoryConnection {
  source_id: string;
  source_name: string;
  kind: string;               // postgres | mysql | mssql
  database: string;
  tables: IngestionHistoryTable[];
  total_rows: number;
  total_vectors: number;
  last_ingested: string | null;
}
```

### Divergence from the reference (deliberate)

- **`log` is the full array per event, not an incremental `logEntry?`.** Our SSE is a 500 ms poller over
  the `jobs` doc (we do not have the reference's in-process event emitter). Sending the whole capped log
  each poll is robust against missed/duplicated lines; the client just sets `log = progress.log`. No
  rAF batching needed at 2 events/sec (the rAF buffer in `stream.ts` is for per-token chat bursts).
- **`db_size_bytes`** = cumulative UTF-8 byte length of embedded chunk text (the reference measured its
  SQLite file; we have none). Honest, monotonic, and gives the stat a meaningful growing number.
- **`status` vocabulary** becomes `running | completed | partial | failed` (was `running | done | failed`).
  `partial` = ≥1 table failed but ≥1 succeeded. **This means `routes/stats.rs` `last_ingest_at` must now
  match `completed`/`partial`, not `"done"`** — update it.
- **Drift lives in Data Explorer (Stage 5), not here.** Only history + the wizard ship on `/ingest` now.
  Do NOT build `GET /ingest/drift` in this stage.

---

## Server work (`onprem-rag-server`)

All new mutation routes are **admin-guarded** (match the existing `AdminUser` guard used by
`/sources` mutations). Reads (`/schema`, `/ingest/history`) require any `AuthUser`.

### 1. `connectors/mod.rs` — trait + shared helpers

Add to the `SourceConnector` trait:

```rust
async fn get_schema(&self) -> AppResult<Vec<TableSchema>>;
async fn count_table(&self, table: &str) -> AppResult<i64>;
async fn fetch_table(&self, table: &str, excluded: &[String], limit: Option<i64>)
    -> AppResult<Vec<FetchedRow>>;
```

New public types in `connectors/mod.rs` (serde snake_case; `type` field uses `#[serde(rename = "type")]`
on a Rust field named `type_` or use `r#type`):

```rust
#[derive(Serialize)]
pub struct ColumnSchema {
    pub name: String,
    #[serde(rename = "type")]
    pub type_: String,
    pub nullable: bool,
    pub is_primary_key: bool,
    pub is_foreign_key: bool,
    pub likely_pii: bool,      // always false here; analyze fills it
}
#[derive(Serialize)]
pub struct TableSchema {
    pub name: String,
    pub row_count: i64,
    pub columns: Vec<ColumnSchema>,
}
```

Keep the existing `fetch(limit)` for backward-compat OR migrate its one caller; simpler: leave `fetch`
in place (Data Explorer / tests may still use it) and add the three new methods.

`fetch_table` reuses `make_row` but **drops excluded columns from the fields map before `project_text`**,
so excluded (e.g. PII) columns are neither embedded nor stored.

### 2. Per-driver schema SQL

**Postgres** (`postgres.rs`) — one pooled connection, fast estimates:
- tables + row estimate:
  ```sql
  SELECT c.relname AS table_name, c.reltuples::bigint AS row_estimate
  FROM pg_class c JOIN pg_namespace n ON n.oid = c.relnamespace
  WHERE c.relkind = 'r' AND n.nspname NOT IN ('pg_catalog','information_schema')
  ORDER BY c.relname
  ```
- columns:
  ```sql
  SELECT table_name, column_name, data_type, is_nullable
  FROM information_schema.columns
  WHERE table_schema NOT IN ('pg_catalog','information_schema')
  ORDER BY table_name, ordinal_position
  ```
- PK / FK: `information_schema.table_constraints tc JOIN key_column_usage kcu` on
  `constraint_name`+`table_schema`, filter `tc.constraint_type = 'PRIMARY KEY'` (and `'FOREIGN KEY'`).
- `count_table(t)`: `SELECT count(*) FROM "{t}"` (exact, at ingest time). Quote the identifier; reject
  table names not present in `get_schema()` to avoid injection.

**MySQL** (`mysql.rs`) — filter by `TABLE_SCHEMA = DATABASE()`:
- tables: `SELECT TABLE_NAME, TABLE_ROWS FROM information_schema.tables WHERE TABLE_SCHEMA = DATABASE() AND TABLE_TYPE='BASE TABLE'`
- columns: `information_schema.columns` (COLUMN_NAME, DATA_TYPE, IS_NULLABLE, COLUMN_KEY — `COLUMN_KEY='PRI'` → PK, `'MUL'` often FK).
- FK precise: `information_schema.key_column_usage WHERE REFERENCED_TABLE_NAME IS NOT NULL`.
- `count_table`: `SELECT count(*) FROM \`{t}\``.

**MSSQL** (`mssql.rs`) — one TDS connection:
- tables: `SELECT TABLE_NAME FROM INFORMATION_SCHEMA.TABLES WHERE TABLE_TYPE='BASE TABLE'`
- row estimate: `SELECT OBJECT_NAME(object_id) AS t, SUM(row_count) FROM sys.dm_db_partition_stats WHERE index_id IN (0,1) GROUP BY object_id` (map by name).
- columns: `INFORMATION_SCHEMA.COLUMNS` (COLUMN_NAME, DATA_TYPE, IS_NULLABLE).
- PK/FK: `INFORMATION_SCHEMA.TABLE_CONSTRAINTS` + `KEY_COLUMN_USAGE`.
- `count_table`: `SELECT COUNT_BIG(*) FROM [{t}]`.

If any driver's schema query is nontrivial to get right, return partial data (empty columns) rather than
erroring the whole call — a table with `columns: []` still lets the user select it.

### 3. `documentdb/mod.rs` — new collection

```rust
pub const INDEXED_TABLES: &str = "indexed_tables";
pub fn indexed_tables(&self) -> Collection<Document> { self.db.collection(INDEXED_TABLES) }
```

`indexed_tables` doc shape (`_id = "{source_id}:{table}"`):
```
{ _id, source_id, source_table, row_count, vector_count,
  status: "indexed"|"error"|"indexing", last_ingested, last_embedded_at }
```

### 4. `ingest/mod.rs` — rewrite the pipeline (per-table loop)

`execute()` becomes:
1. Ensure vector indexes (existing step).
2. `load_spec(db, config, source_id)`.
3. For each `table` in request order (`table_index` 1-based):
   - Write a `divider` log line + `info` "Starting table X of Y: {table}".
   - Mark `indexed_tables` doc `status:"indexing"`.
   - `count_table` → `table_total`; add to running `total_rows` progress.
   - `fetch_table(table, excluded_columns[table], limit)`.
   - `records().delete_many({ source_id, table })` — replaces ONLY this table's records (not all).
   - Chunk each row's text (`chunk_text`), embed in `BATCH_SIZE=32` batches (`embed::embed_documents`),
     insert `RecordDoc`. After each batch, advance `table_rows` / `processed_rows`, add embedded bytes to
     `db_size_bytes`, write the progress snapshot to the `jobs` doc. Emit a log line every ~2500 rows.
   - On table success: `success_tables += 1`; upsert `indexed_tables` doc `status:"indexed"`,
     `row_count`, `vector_count` (chunks inserted), `last_ingested`/`last_embedded_at` = now; log `success`.
   - On table error: `failed_tables += 1`, `errors += 1`; upsert `indexed_tables` `status:"error"`;
     log `error` with the message; **continue to next table** (don't abort the whole job).
4. Terminal status: all ok → `completed`; some failed + some ok → `partial`; all failed → `failed`.
   Write final snapshot + `finished_at`.

`RecordDoc` gains `table: String`; `_id` becomes `"{source_id}:{table}:{row_pk}:{chunk_index}"`
(row_pk alone can collide across tables). Data Explorer (Stage 5) will read `table`.

`run()` keeps the detached-task + mark-failed-on-panic wrapper.

`chunk_text` timestamps: **`Date::now`/`Utc::now` is fine in Rust** (the no-`Date.now` rule is a
workflow-script constraint, not a Rust one) — use `chrono::Utc::now()` for `time` on log entries.

### 5. `ingest/routes.rs` — widen request + grow Progress

- `IngestRequest` → `{ source_id, tables: Vec<String>, excluded_columns: Option<HashMap<String, Vec<String>>>, limit: Option<i64> }`.
- `POST /ingest`: validate `tables` non-empty; validate each table exists in `get_schema()` (guard against
  injection in `count_table`/`fetch_table`); insert the extended `jobs` doc; spawn `run`.
- `Progress` struct grows from 4 → the 14 fields above (+ `log: Vec<LogEntry>`). The `GET /ingest/<job>/stream`
  poller reads the jobs doc and serializes the full `IngestProgress` each tick. Cap `log` at 500 server-side.
- Terminal detection: emit `ingest://done` on `completed`/`partial`, `ingest://error` on `failed`
  (bridge decides; see below).

### 6. New routes

- `GET /sources/<id>/schema` → `Json<Vec<TableSchema>>` (any AuthUser). `load_spec` → `connector(&spec).get_schema()`.
- `POST /schema/analyze` (`{ tables: Vec<TableSchema> }`) → `Json<SchemaAnalysis>` (any AuthUser):
  1. **Deterministic PII keyword pass** (always runs, model-free — model of `aggregation/intent.rs`
     `classify_lexical`): match column names against a keyword list — `name, first_name, last_name, dob,
     birth, ssn, social, mrn, patient, email, phone, address, zip, postal, gender, sex, race, ethnicity,
     insurance, policy, guarantor, next_of_kin, contact, license, passport, national_id, account`.
     Populate `pii_columns` and set `suggested_tables` = tables that have a plausible text/clinical column.
  2. **Optional LLM pass** — if `state.foundry().is_ok()`, call `complete_with(spec, system, user)` with
     `response_format: ChatResponseFormat::JsonSchema(...)` (template: `foundry/mod.rs:744` `plan_aggregation`)
     to produce `summary` + `data_quality_notes` + refine `pii_columns`/`suggested_tables`. On any parse/
     model failure, fall back to the deterministic result with a generic `summary`. **Never hard-fail** —
     analysis is advisory.
- `GET /ingest/history` → `Json<Vec<IngestionHistoryConnection>>` (any AuthUser): read `indexed_tables`,
  group by `source_id`, join `sources` for `source_name`/`kind`/`database`. `total_rows`/`total_vectors`
  = sums; `last_ingested` = max.
- `DELETE /ingest/table/<source_id>/<table>` (admin): delete `indexed_tables` doc + `records().delete_many({source_id, table})`.
- Extend existing `DELETE /sources/<id>` to also clear that source's `indexed_tables` docs (it already
  cascades `records`).

Mount all new routes in `main.rs`. Update `routes/stats.rs` `last_ingest_at` to match `completed`/`partial`.

**Verify:** `cargo check` in `onprem-rag-server` (NOT `cargo run`/`build` — Smart App Control blocks those
on this host until reboot, OS error 4551).

---

## Bridge work (`onprem-rag-app/src-tauri` + `src/lib/bridge.ts`)

> **Mirror hazard:** every field above must appear in BOTH `commands.rs` structs AND `bridge.ts` types,
> or it is silently dropped.

### `src-tauri/src/commands.rs`
- Grow the `IngestProgress` mirror struct 4 → 16 fields (+ `log: Vec<LogEntry>`, + `LogEntry` struct).
- Widen `start_ingest` payload to `{ source_id, tables, excluded_columns, limit }`; keep the SSE relay
  (`ingest://progress` per event; terminal → `ingest://done` on `completed`/`partial`, `ingest://error`
  on `failed`).
- New commands: `get_schema(source_id) -> Vec<TableSchema>`, `analyze_schema(tables) -> SchemaAnalysis`,
  `get_ingest_history() -> Vec<IngestionHistoryConnection>`, `delete_ingest_table(source_id, table)`.
  Add `TableSchema`/`ColumnSchema`/`SchemaAnalysis`/`IngestionHistoryConnection`/`IngestionHistoryTable`
  mirror structs. Register every new command in `lib.rs`.

### `src/lib/bridge.ts`
- Replace the 4-field `IngestProgress` with the 16-field interface above (+ `LogEntry`).
- Widen `startIngest(sourceId, tables, excludedColumns?, limit?)`.
- Add: `getSchema(sourceId)`, `analyzeSchema(tables)`, `getIngestHistory()`, `deleteIngestTable(sourceId, table)`.
- Add all new TS types (`TableSchema`, `ColumnSchema`, `SchemaAnalysis`, `IngestionHistory*`).

**Verify:** `cargo check` in `src-tauri`; `npx tsc --noEmit` in `onprem-rag-app`.

---

## App work (`onprem-rag-app/src`)

### `stores/ingestion.ts` (new)
Wizard state machine + progress state that **survives navigation**:
```ts
type IngestStep = 'pick-connection'|'loading-schema'|'analyzing'|'select-tables'|'ingesting'|'complete';
```
Holds: `step`, selected `sourceId`, `schema: TableSchema[]`, `analysis: SchemaAnalysis|null`,
`selections: Record<tableName, { selected: boolean; excluded_columns: string[]; expanded: boolean }>`,
current `progress: IngestProgress|null`, and a log ring (the store just mirrors `progress.log`, cap 500
already enforced server-side). Actions: `pickSource`, `setSchema`, `setAnalysis`, `toggleTable`,
`toggleColumn`, `toggleExpand`, `applyProgress(event)`, `resetWizard`. `applyProgress` flips `step` →
`complete` on terminal status.

### `lib/bridgeEvents.ts`
Add a one-time `ingest://progress` / `ingest://done` / `ingest://error` listener that fans into the
ingestion store's `applyProgress`. Register at boot alongside the existing `logs://` listeners. **No
per-component `listen`** (leak-prone). No rAF buffer needed (2 events/sec).

### `features/ingest/` (new) — mobile-first (360px first, ≥44px tap targets, no horizontal body scroll)
- `Ingest.tsx` — step router reading the ingestion store.
- `PickConnection.tsx` — source cards (reuse `listSources`) + `IngestionHistory` card (`getIngestHistory`,
  per-table + per-connection delete).
- `LoadingSchema.tsx` / `Analyzing.tsx` — spinners driving the two async bridge calls.
- `SelectTables.tsx` — per-table selectable rows; expand → column list with PK badge + PII badge
  (`col.likely_pii || analysis.pii_columns[table]?.includes(col.name)`); eye toggle per column to add to
  `excluded_columns`; sticky bottom "Start Ingestion" bar (count of selected tables). PII is **flagged,
  never auto-excluded**.
- `Ingesting.tsx` — two-column split on `md+`, stacked on phones. Left = RunSummary (connection name;
  Tables/Processed/Errors stats; "New ingestion" on complete). Right = ProgressPanel (title; "Table X of
  Y: {current_table}"; overall bar red/amber/green by error ratio; current-table bar; stats row
  Processed / Total est. / Errors / DB size via a `fmtBytes` helper; live log with icons
  `{info:'·', success:'✓', warn:'⚠', error:'✕'}`, `divider` → `<hr>`, blinking cursor, auto-scroll;
  completion notes).
- Wire `'/ingest'` into `app/routes.tsx` (lazy import `../features/ingest`).

**Verify:** `npx tsc --noEmit`; resize to 360×640, confirm no horizontal body scroll and ≥44px targets.

---

## Delegation plan (Sonnet)

Server files heavily overlap (`connectors/mod.rs`, `ingest/`, `main.rs`), so server work is **one
sequential Sonnet agent**, not parallel (avoids merge conflicts). Then bridge, then app.

1. **Agent 1 — server** (§Server work, all of it). Verify `cargo check`.
2. **Agent 2 — bridge** (§Bridge work). Depends on 1. Verify `cargo check` + `tsc`.
3. **Agent 3 — app store + events + wizard UI** (§App work). Depends on 2. Verify `tsc` + 360px.

Opus reviews each agent's diff before dispatching the next (do not trust self-reports).
