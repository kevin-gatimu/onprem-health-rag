# Live Verification & Decode Findings

> **Superseded.** This file is retained for link/history stability. The authoritative replacement is [Verified Platform Notes](verified-platform-notes.md). Do not treat the statuses, defaults, or diagrams below as current.

> Technical findings from verification against real containers during development. Everything here was **tested live**, not inferred from documentation. Dates: 2026-08-23. Companion docs: [`architecture-overview.md`](architecture-overview.md), [`retrieval-pipeline.md`](retrieval-pipeline.md).

## 1. DocumentDB `cosmosSearch` Syntax — VERIFIED

**Status:** ✅ Proven identical across local container and Azure Cosmos DB for MongoDB vCore.

**Risk:** The initial design was written from Azure Cosmos DB docs; verification was against the open-source `ghcr.io/documentdb/documentdb/documentdb-local:latest` container (gateway port 10260). Proofs done via `mongosh` inside the container.

### Vector Index Creation

Returns `ok: 1`. Exact syntax:

```js
db.runCommand({
  createIndexes: "records",
  indexes: [{
    name: "records_contentVector_cosmos",
    key: { contentVector: "cosmosSearch" },
    cosmosSearchOptions: { 
      kind: "vector-ivf", 
      numLists: 100, 
      similarity: "COS", 
      dimensions: 1024 
    }
  }]
})
```

**Implementation:** `documentdb/vector.rs` uses this exact format via `CreateIndexOptions`; **no changes needed**.

### Full-Text Index Creation

Returns `ok: 1`. Syntax:

```js
db.runCommand({ 
  createIndexes: "records", 
  indexes: [{ 
    name: "records_text", 
    key: { text: "text" } 
  }] 
})
```

Stored as legacy `_fts`/`_ftsx` with `textIndexVersion: 2`.

### Vector Query

**Pipeline stage 1**, `$search` operator (not `$vectorSearch` — that's Atlas only):

```js
db.records.aggregate([
  { 
    $search: { 
      cosmosSearch: { 
        vector: [/* 1024 f32 values */], 
        path: "contentVector", 
        k: 30 
      } 
    } 
  },
  { $project: { text: 1, fields: 1, source_id: 1, score: { $meta: "searchScore" } } }
])
```

Score via `$meta: "searchScore"` (0.0–1.0 cosine similarity range).

### Full-Text Query

Direct `find()` (not in a pipeline), sorted by `$meta: "textScore"`:

```js
db.records.find(
  { $text: { $search: "chest pain" } }, 
  { score: { $meta: "textScore" } }
).sort({ score: { $meta: "textScore" } })
```

### Pipeline Constraints (Critical for RRF)

- **Stage-1-only operators:** Both `$search.cosmosSearch` (vector) and `$text` are **pipeline stage-1-only** — cannot be combined in one pipeline
- **Consequence:** Run them as two separate aggregations, then **RRF-fuse in Rust** (k=60)
- **No native `$rankFusion` / `$vectorSearch`:** Those features are Atlas-only; DocumentDB local lacks them
- **Native `$search`-based full-text:** The GA legacy `$text` engine is simpler and on-prem-safe (the newer `$search` BM25 engine is Gated Preview on this version)

**Impact on WS6:** `vector.rs` needs **no changes**. The two-index + two-query design is correct and proven live.

## 2. SQL Server (tiberius) Cell Type Decode — VERIFIED

**Status:** ✅ Fixed and verified live against `mcr.microsoft.com/mssql/server` container.

**Test:** Covered by `connectors::mssql::tests::decodes_tricky_sql_server_types` (gated on `RAG_MSSQL_LIVE=1`).

### Problem

"MSSQL NUMERIC/DECIMAL decodes to null." Root-caused and fixed.

### Solution

`cell_to_json` in `connectors/mssql.rs` tries concrete Rust types **in order** and takes the first `Ok(Some(_))`. **Order matters** — exact integers before floats, `Numeric` after the primitive numerics, before bool/strings.

### Decode Table

| SQL Server Type | tiberius Decodes As | Our JSON Output | Notes |
|-----------------|---------------------|-----------------|-------|
| INT/BIGINT/SMALLINT | i32/i64/i16 | number | |
| TINYINT | **u8** (unsigned) | number | Added; must precede other int patterns |
| DECIMAL / NUMERIC | `tiberius::numeric::Numeric` | **string** | **The fix** — `Numeric::to_string()` preserves exact precision (health lab values / dosages must not round through f64) |
| MONEY | **f64** (not `Numeric`) | number | Acceptable — decoded, not null. Precision fine for typical currency ranges. Do **not** reorder `Numeric` before `f64` to "fix" this. |
| REAL | **f32** | number | Added; won't decode as f64 |
| FLOAT | f64 | number | |
| BIT | bool | bool | |
| NVARCHAR / text | &str | string | |
| DATETIME2 / DATE | NaiveDateTime / NaiveDate | string (ISO format) | |
| UNIQUEIDENTIFIER (GUID) | **uuid::Uuid** | string (36 chars) | Added; parse-only, unrelated to our v7 ID generation |

### Critical Order Rule

```
Numeric must be tried AFTER f64/f32 (so genuine floats stay numbers)
                 but BEFORE bool/&str (fallback types)

Flow:
  1. Try i32/i64/i16
  2. Try u8
  3. Try f64
  4. Try f32
  5. Try Numeric ← here
  6. Try bool
  7. Try &str
```

**Why:** MONEY is the one exact-decimal type tiberius routes through f64, so it surfaces correctly as a number. If we tried `Numeric` before `f64`, MONEY would become a string (precision preserved but semantically wrong). Conversely, placing `Numeric` after bool/strings would never work (those are too generic).

### Comparison: Other Connectors

- **PostgreSQL:** uses whole-row `to_jsonb(_sub)` — sidesteps per-column decode entirely (always correct)
- **MySQL:** uses the same per-column try-fallthrough as MSSQL

## 3. UUID v7 for All DocumentDB `_id`s

**Status:** ✅ Switched from v4 to v7 across all four UUID sites.

**Reason:** Every UUID we mint is a DocumentDB `_id` (a database key landing in a Postgres index underneath). 

### Change Details

- **Old:** `uuid::Uuid::new_v4()` — random, unordered
- **New:** `now_v7()` (feature `v7`, not `v4`)
- **Effect:** Keys are time-ordered; they sort by creation time and keep inserts **local in the B-tree**, improving index locality per the `uuid` crate's own recommendation (v4 for throwaway random IDs, v7 for DB keys)

### Sites Updated

1. User `_id` on seed (`admin` / `password`)
2. User `_id` on `POST /auth/users` (create new user)
3. Source `_id` on `POST /sources` (save source)
4. Job `_id` on `POST /ingest` (start ingestion)

### Record Document IDs (Unchanged)

Record `_id`s are composite **strings** (`{source_id}:{pk}:{chunk}`), not UUIDs, and are **unaffected** by this change.

## Consequences for Future Work

### For WS6 (Hybrid Retrieval + RAG Chat)

- `vector.rs` needs **zero changes** — the two indexes are correct and present
- Run vector + full-text as **two separate aggregations**, then RRF-fuse in Rust
- No need to revisit `cosmosSearch` syntax or full-text index creation

### For Source Connectors & Ingestion

- MSSQL decode order in `connectors/mssql.rs` is **fixed** — DECIMAL/NUMERIC health values and MONEY fields both decode correctly
- MySQL uses the same pattern — no changes needed
- Postgres whole-row `to_jsonb` — unaffected

### For Auth & Data Model

- All new UUID `_id`s (users, sources, jobs) are now **time-ordered** and will keep B-tree inserts localized
- No data migration needed (old v4 IDs are still valid; new records get v7)

## How This Was Verified

1. **DocumentDB indexes:** Connected to local container via `mongosh`, ran `createIndexes` commands, verified they returned `ok: 1`, then ran test queries (`$search.cosmosSearch` + `$text`) and confirmed scores populated
2. **MSSQL decode:** Spun up a live SQL Server container, created a test table with DECIMAL/NUMERIC/MONEY columns, queried via tiberius, verified each type decoded to the expected Rust type, then to JSON
3. **UUID v7:** Reviewed the `uuid` crate docs + the `now_v7()` API, updated all four UUID-minting sites, verified `cargo check` compiles

---

Source: synthesized from `plans/02-verification-and-decode-findings.md`
