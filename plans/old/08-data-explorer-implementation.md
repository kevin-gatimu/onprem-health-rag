# 08 — Data Explorer implementation (Stage 5)

**STATUS: SHIPPED (2026-08-25)**

Authoritative contract for Stage 5 of the UI rebuild (see `UI redo plan.md`). Ported from the Electron
reference `src/renderer/pages/DataExplorer/DataExplorer.tsx`, adapted to the Rust-server + Tauri-bridge
boundary and made mobile-first. **Everything snake_case end-to-end**, matching Stage 4.

Delegation: 3-agent SEQUENTIAL chain (server → bridge → app). Opus reviews each diff before dispatching
the next — do not trust self-reports. See [[use-sonnet-to-code-after-planning]].

## What the reference does (kept vs dropped)

Kept: stat strip (connections/tables/rows/vectors) · connection tree of indexed tables · paginated +
debounced-search row grid · row-detail drawer · table inspector (counts, status, source db, lineage,
schema profile, recent runs) · per-table / per-connection / clear-all deletes · re-ingest a table.

**Dropped from Stage 5 scope**: "Clear cache" (`POST /cache/clear`), "Reindex search" (`POST /reindex`),
and **schema-drift badges** (omitted from tree UI). **No new overview route** — stat strip totals are
derived client-side from `GET /ingest/history` (existing endpoint). **Search uses `$regex`** on the
`text` field (verified on DocumentDB, no fallback needed).

## Storage model (already built — do not change the ingest pipeline)

- **`records`** (chunk-grained): `_id = {source_id}:{table}:{row_pk}:{chunk_index}`, plus `source_id`,
  `table`, `row_pk`, `chunk_index`, `fields` (the row's selected columns as a JSON object — **identical
  across all chunks of a row**), `text`, `contentVector`, `ingested_at`. Row-grained view = group by
  `row_pk`, take `$first`.
- **`indexed_tables`**: `_id = {source_id}:{table}`, `source_id`, `source_table`, `status`
  (indexed|error|indexing), `row_count`, `vector_count`, `last_ingested`, `last_embedded_at`.
- **`jobs`**: `_id`, `source_id`, status, counters, `log`, `started_at`, `finished_at`. No per-table run
  rows → "recent runs" is scoped to the connection.

## Server — `onprem-rag-server` (Agent 1)

New module `src/routes/explorer.rs` (browse + info); two new deletes co-located in `ingest/routes.rs`.
Mount all in `main.rs`. Reuse existing `records()` / `indexed_tables()` / `jobs()` / `sources()` handles.

### 1. `GET /records?source_id=&table=&page=&page_size=&q=` — AuthUser (any role)
Row-grained, paginated, searchable browse. `page` defaults 1 (min 1); `page_size` clamped to one of
{10,25,50,100}, default 25; `q` optional. Aggregation on `records`:
```
[ { $match: { source_id, table, ...(q ? { text: { $regex: q, $options: "i" } } : {}) } },
  { $sort: { row_pk: 1, chunk_index: 1 } },
  { $group: { _id: "$row_pk", doc: { $first: "$$ROOT" } } },
  { $sort: { _id: 1 } },
  { $facet: { rows: [ { $skip: (page-1)*page_size }, { $limit: page_size } ],
              total: [ { $count: "n" } ] } } ]
```
⚠️ **Verify `$regex`-in-`$match` works on DocumentDB via mongosh in-container** (see
[[mongodb-mcp-native-pipeline-limit]]). If unsupported, fall back to `{ $text: { $search: q } }` (the GA
`$text` index already exists). Document whichever ships.

Response (`RecordsPage`): `{ rows: [{ id: <row_pk>, source_id, data: <fields object>, ingested_at }],
total, page, page_size, page_count, has_prev, has_next }`. `page_count = ceil(total/page_size)` (min 1).

### 2. `GET /tables/<table_id>/info` — AuthUser
`table_id = {source_id}:{table}`; split on the **first** `:` (source_id is a colon-free generated id).
Returns `TableInfo`:
- `table`: from the `indexed_tables` doc → `{ id, source_id, source_table, row_count, vector_count,
  status, last_ingested, last_embedded_at }`.
- `connection` (nullable): load + decrypt the source spec → `{ id, name, kind, host, port, database,
  username }`. Null if the source was deleted.
- `profile` (nullable): **row-derived** — fetch one `records` doc for (source_id, table), read `fields`
  keys → `{ columns: [{ name, type: <json value type: string|number|boolean|object|null>, nullable:
  true, selected: true, pii: <deterministic PII keyword check on the column name> }], pii_columns: [..],
  selected_columns: [all] }`. Null when the table has no records. **Reuse the same deterministic PII
  keyword helper `/schema/analyze` uses** (locate it in the schema-analyze code path; do not reinvent).
- `recent_runs`: `jobs` where `source_id` = this source, sort `started_at` desc, limit 5 → `[{ id,
  status, rows_processed: <processed_rows>, chunks_created: <processed_rows or 0 if absent>, started_at,
  completed_at: <finished_at|null>, errors: <errors count> }]`. Connection-scoped (honest — jobs have no
  per-table breakdown).

### 3. `DELETE /ingest/connection/<source_id>` — Admin
Delete all `indexed_tables` docs for `source_id` **and** all `records` for `source_id`. Return
`{ tables_removed: <n indexed_tables removed> }`.

### 4. `DELETE /ingest/all` — Admin
`delete_many({})` on both `records` and `indexed_tables`. Return `{ ok: true }`.

### Cosmetic while in the file
Fix the stale `ingest/routes.rs` comment (line ~50) "Full 16-field progress snapshot" → **14-field**.

### Reuse (no new work)
`GET /ingest/history` (tree + client-computed totals), `DELETE /ingest/table/<source_id>/<table>`
(per-table clear), `POST /ingest` (re-ingest one table).

## Bridge — `src-tauri/src/commands.rs` + `src/lib/bridge.ts` (Agent 2)

Mirror structs in commands.rs (snake_case, `#[derive(Serialize/Deserialize)]` as needed), register in
`lib.rs`, and matching interfaces + wrappers in bridge.ts. ⚠️ [[bridge-mirror-structs-drop-fields]] — a
field missing from either side is silently dropped.
- `DataRow { id: String, source_id: String, data: serde_json::Value, ingested_at: String }`
- `RecordsPage { rows, total, page, page_size, page_count, has_prev, has_next }`
- `TableInfo` + nested `TableInfoTable`, `TableInfoConnection`, `TableProfile`,
  `TableProfileColumn`, `RecentRun` (all snake_case matching the server JSON).
- Commands: `list_records(source_id, table, page, page_size, q: Option<String>) -> RecordsPage`,
  `get_table_info(table_id) -> TableInfo`, `delete_ingest_connection(source_id) -> Value`,
  `clear_all_records() -> Value`.
- bridge.ts wrappers: `listRecords`, `getTableInfo`, `deleteIngestConnection`, `clearAllRecords`.

## App — `src/features/data-explorer/` (Agent 3)

Wire `'/data'` into `app/routes.tsx`. TanStack Query keys: `['ingest-history']` (reused — tree +
totals summed client-side), `['records', tableId, page, pageSize, q]` (`placeholderData:
keepPreviousData`, staleTime 30s), `['table-info', tableId]`. Selection/paging/search/drawers are
component-local `useState` (no cross-nav persistence needed here, unlike the ingest wizard).

Responsive (per UI plan):
- **< md**: stat strip 2×2 → "Tables" button opens the connection tree in a **drawer** → **row cards**
  (first 3 columns, tap → Row Detail drawer). Inspector opens as a drawer. Compact pagination.
- **md/xl**: stat strip + connection tree left rail + row `<table>` (in `overflow-x-auto`) right +
  inspector drawer + row drawer.

Files: `index.tsx` (shell + responsive body + drawer orchestration) · `OverviewBar.tsx` (stat tiles +
Refresh + Clear-all-with-confirm) · `ConnectionTree.tsx` (collapsible groups + table rows; rendered in
the rail on md+ and inside a drawer on mobile) · `RowsGrid.tsx` (search + pagination; `<table>` on md+,
row cards on mobile) · `TableInspector.tsx` (counts/status/re-ingest+clear/source-db/lineage/schema/
recent-runs drawer) · `RowDetail.tsx` (row drawer) · `utils.ts` (truncate, fmtDate, fmtRelative,
formatValue, fmtNum, statusTone).

Re-ingest: `startIngest(source_id, [table])`, watch the global `useIngestion` progress filtered to
`current_table === table`; on terminal status invalidate `['records',…]`, `['table-info',…]`,
`['ingest-history']`, `['stats']`. Mobile-first: ≥44px tap targets, safe-area insets on sticky/bottom
elements, no horizontal body scroll (grid scrolls inside its own `overflow-x-auto`).

## Verify per layer
`cargo check` (Rust — `cargo run`/`build` still fail on host: Smart App Control, OS 4551, until reboot);
`npx tsc --noEmit` (app); grep both commands.rs + bridge.ts to confirm every field is mirrored; resize
to 360px and confirm no horizontal body scroll + ≥44px targets.
