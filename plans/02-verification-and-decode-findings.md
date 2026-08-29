# 02 — Live Verification & Connector Decode Findings

> Design note capturing what was **proven against real containers** during Workstream 5, so WS6
> and beyond can rely on it without re-deriving. Everything here was tested live, not inferred
> from docs. Dates are 2026-08-23.

## 1. DocumentDB `cosmosSearch` syntax — VERIFIED LIVE

The master plan flagged this as the highest-risk assumption: `documentdb/vector.rs` was written
from **Azure Cosmos DB for MongoDB vCore** docs, but our store is the open-source
`ghcr.io/documentdb/documentdb/documentdb-local:latest` container (gateway port 10260). Proven
identical via `mongosh` inside the container (the MongoDB MCP's `aggregate-db` uses a "native
pipeline" that rejects raw run-commands like `createIndexes`, so verification required mongosh).

**Vector index** (`createIndexes`) — returns `ok: 1`:

```js
db.runCommand({
  createIndexes: "records",
  indexes: [{
    name: "records_contentVector_cosmos",
    key: { contentVector: "cosmosSearch" },
    cosmosSearchOptions: { kind: "vector-ivf", numLists: 100, similarity: "COS", dimensions: 1024 }
  }]
})
```

**Full-text index** (`$text`, GA legacy engine) — returns `ok: 1`; stored as legacy `_fts`/`_ftsx`,
`textIndexVersion: 2`:

```js
db.runCommand({ createIndexes: "records", indexes: [{ name: "records_text", key: { text: "text" } }] })
```

**Vector query** — `$search.cosmosSearch` as **stage 1**, score via `$meta:"searchScore"`:

```js
db.records.aggregate([
  { $search: { cosmosSearch: { vector: [/* 1024 f32 */], path: "contentVector", k: 30 } } },
  { $project: { text: 1, fields: 1, source_id: 1, score: { $meta: "searchScore" } } }
])
```

**Full-text query** — `$text` + `$meta:"textScore"`, must sort by the same meta:

```js
db.records.find({ $text: { $search: "chest pain" } }, { score: { $meta: "textScore" } })
  .sort({ score: { $meta: "textScore" } })
```

**Consequences for WS6:**
- `vector.rs` needs **no changes**. The two indexes it creates are correct and present.
- No native `$rankFusion` / `$vectorSearch` on this engine (that's Atlas). Run the two aggregations
  separately and **RRF-fuse in Rust** (k=60), exactly as planned.
- `k` (vector) and `$text` are both **stage-1-only** operators — cannot be combined in one pipeline.

## 2. SQL Server (tiberius) cell decode — VERIFIED LIVE

Reported gap: "MSSQL NUMERIC/DECIMAL decodes to null." Root-caused and fixed against a live
`mcr.microsoft.com/mssql/server` container; covered by an opt-in test
(`connectors::mssql::tests::decodes_tricky_sql_server_types`, gated on `RAG_MSSQL_LIVE=1`).

`cell_to_json` in `connectors/mssql.rs` tries concrete Rust types in order and takes the first
`Ok(Some(_))`. **Order matters** — exact integers before floats, `Numeric` after the primitive
numerics. Findings:

| SQL Server type          | tiberius decodes as        | Our JSON            | Notes |
|--------------------------|----------------------------|---------------------|-------|
| INT/BIGINT/SMALLINT      | i32/i64/i16                | number              | |
| TINYINT                  | **u8** (unsigned)          | number              | added |
| DECIMAL / NUMERIC        | `tiberius::numeric::Numeric` | **string**        | the fix — `Numeric::to_string()` keeps exact precision (health lab values / dosages must not round through f64) |
| MONEY                    | **f64** (not `Numeric`)    | number              | acceptable — decoded, not null. Precision fine for typical ranges; do **not** reorder `Numeric` before `f64` to "fix" this — that would break FLOAT/REAL |
| REAL                     | **f32** (won't decode as f64) | number           | added |
| FLOAT                    | f64                        | number              | |
| BIT                      | bool                       | bool                | |
| NVARCHAR/text            | &str                       | string              | |
| DATETIME2 / DATE         | NaiveDateTime / NaiveDate  | string              | |
| UNIQUEIDENTIFIER (GUID)  | **uuid::Uuid**             | string (36 chars)   | added; parse only, unrelated to our v7 id minting |

**Key rule:** `Numeric` must be tried **after** `f64`/`f32` (so genuine floats stay numbers) but
**before** `bool`/`&str`. MONEY is the one exact-decimal type tiberius routes through f64, so it is
accepted as a number.

Postgres uses whole-row `to_jsonb(_sub)` so it sidesteps per-column decode entirely; MySQL uses the
same per-column try-fallthrough approach as MSSQL.

## 3. UUID version — v7 for all DocumentDB `_id`s

Every UUID we mint is a DocumentDB `_id` (users seed + create, sources, jobs) — a database key that
lands in a Postgres index underneath. Switched `uuid::Uuid::new_v4()` → `now_v7()` (feature `v7`, not
`v4`) at all four sites so keys are **time-ordered**: they sort by creation and keep inserts local in
the B-tree, per the `uuid` crate's own recommendation (v4 for throwaway random IDs, v7 for DB keys /
sortable). Record `_id`s are `source_id:pk:chunk` strings and are unaffected.
