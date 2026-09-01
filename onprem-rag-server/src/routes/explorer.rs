//! Data Explorer read routes (Stage 5): row-grained browse over ingested records
//! (`GET /records`) and a per-table inspector (`GET /tables/<table_id>/info`).
//!
//! Both are read-only over collections the ingest pipeline owns (`records`,
//! `indexed_tables`, `jobs`, `sources`) — this module never writes. The two
//! destructive Data Explorer routes (per-connection / clear-all deletes) live in
//! `ingest/routes.rs` alongside the existing per-table delete.
//!
//! Everything is snake_case end-to-end to match the rest of the API.

use futures::TryStreamExt;
use mongodb::bson::{Bson, DateTime as BsonDateTime, Document, doc};
use rocket::serde::json::Json;
use rocket::{State, get};
use serde::Serialize;
use serde_json::Value;

use crate::auth::guard::AuthUser;
use crate::connectors::routes::{is_likely_pii, load_spec};
use crate::error::{AppError, AppResult};
use crate::state::AppState;

// ---------------------------------------------------------------------------
// GET /records — row-grained, paginated, searchable browse
// ---------------------------------------------------------------------------

/// The allowed page sizes; anything else clamps to the default.
const ALLOWED_PAGE_SIZES: [i64; 4] = [10, 25, 50, 100];
const DEFAULT_PAGE_SIZE: i64 = 25;

/// One row in a records page. Records are chunk-grained in storage; this is the
/// row-grained view (group by `row_pk`, take the first chunk's `fields`).
#[derive(Debug, Serialize)]
pub struct DataRow {
    /// The source row primary key (`row_pk`) — stable per source row.
    pub id: String,
    pub source_id: String,
    /// The row's original columns as a JSON object (the `fields` map).
    pub data: Value,
    /// ISO-8601 timestamp string (stored as a string by the ingest pipeline).
    pub ingested_at: Option<String>,
}

/// A paginated page of row-grained records.
#[derive(Debug, Serialize)]
pub struct RecordsPage {
    pub rows: Vec<DataRow>,
    pub total: i64,
    pub page: i64,
    pub page_size: i64,
    pub page_count: i64,
    pub has_prev: bool,
    pub has_next: bool,
}

/// `GET /records?source_id=&table=&page=&page_size=&q=` — row-grained paginated
/// browse of ingested records for one (source, table). Any authenticated user.
///
/// `page` defaults to 1 (min 1); `page_size` clamps to one of {10,25,50,100}
/// (default 25); `q` is an optional case-insensitive substring match on the chunk
/// `text`. The `$regex`-in-`$match` filter is verified to run on DocumentDB.
#[get("/records?<source_id>&<table>&<page>&<page_size>&<q>")]
pub async fn list_records(
    state: &State<AppState>,
    _user: AuthUser,
    source_id: String,
    table: String,
    page: Option<i64>,
    page_size: Option<i64>,
    q: Option<String>,
) -> AppResult<Json<RecordsPage>> {
    let page = page.unwrap_or(1).max(1);
    let page_size = page_size
        .filter(|ps| ALLOWED_PAGE_SIZES.contains(ps))
        .unwrap_or(DEFAULT_PAGE_SIZE);
    let skip = (page - 1) * page_size;

    // Build the $match: always filter (source_id, table); add a case-insensitive
    // text regex when a query is provided. Verified against DocumentDB — the
    // $regex operator is supported inside $match on this build.
    let mut match_doc = doc! { "source_id": &source_id, "table": &table, "active": true };
    if let Some(query) = q.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
        match_doc.insert("text", doc! { "$regex": query, "$options": "i" });
    }

    // Row-grained page: collapse chunks to one row (first chunk's $$ROOT), then a
    // single $facet returns the page slice and the total row count together.
    let pipeline = vec![
        doc! { "$match": match_doc },
        doc! { "$sort": { "row_pk": 1, "chunk_index": 1 } },
        doc! { "$group": { "_id": "$row_pk", "doc": { "$first": "$$ROOT" } } },
        doc! { "$sort": { "_id": 1 } },
        doc! { "$facet": {
            "rows": [ { "$skip": skip }, { "$limit": page_size } ],
            "total": [ { "$count": "n" } ],
        }},
    ];

    let docs: Vec<Document> = state
        .db
        .records()
        .aggregate(pipeline)
        .await?
        .try_collect()
        .await?;

    // $facet yields exactly one document: { rows: [...], total: [{ n }] }.
    let facet = docs.into_iter().next().unwrap_or_default();
    let total = facet
        .get_array("total")
        .ok()
        .and_then(|a| a.first())
        .and_then(Bson::as_document)
        .map(read_count)
        .unwrap_or(0);

    let mut rows = Vec::new();
    if let Ok(arr) = facet.get_array("rows") {
        for entry in arr {
            // Each entry is { _id: <row_pk>, doc: <full record> }.
            let Some(rec) = entry.as_document().and_then(|d| d.get_document("doc").ok()) else {
                continue;
            };
            let id = rec.get_str("row_pk").unwrap_or("").to_string();
            let rec_source_id = rec.get_str("source_id").unwrap_or(&source_id).to_string();
            let data = rec
                .get_document("fields")
                .map(|f| Bson::Document(f.clone()).into_relaxed_extjson())
                .unwrap_or(Value::Null);
            rows.push(DataRow {
                id,
                source_id: rec_source_id,
                data,
                ingested_at: read_ts(rec, "ingested_at"),
            });
        }
    }

    let page_count = if total == 0 {
        1
    } else {
        (total + page_size - 1) / page_size
    };

    Ok(Json(RecordsPage {
        rows,
        total,
        page,
        page_size,
        page_count,
        has_prev: page > 1,
        has_next: page < page_count,
    }))
}

// ---------------------------------------------------------------------------
// GET /tables/<table_id>/info — per-table inspector
// ---------------------------------------------------------------------------

/// The `indexed_tables` slice of the table inspector.
#[derive(Debug, Serialize)]
pub struct TableInfoTable {
    /// Composite id `{source_id}:{table}`.
    pub id: String,
    pub source_id: String,
    pub source_table: String,
    pub row_count: i64,
    pub vector_count: i64,
    pub status: String,
    pub last_ingested: Option<String>,
    pub last_embedded_at: Option<String>,
}

/// The source-connection slice — no secrets; null when the source was deleted.
#[derive(Debug, Serialize)]
pub struct TableInfoConnection {
    pub id: String,
    pub name: String,
    /// "postgres" | "mysql" | "mssql"
    pub kind: String,
    pub host: String,
    pub port: u16,
    pub database: String,
    pub username: String,
}

/// One column in the row-derived schema profile.
#[derive(Debug, Serialize)]
pub struct TableProfileColumn {
    pub name: String,
    /// JSON value type of a sampled value: string | number | boolean | object | null.
    #[serde(rename = "type")]
    pub type_: String,
    pub nullable: bool,
    pub selected: bool,
    pub pii: bool,
}

/// The row-derived schema profile — inferred from one sampled record's `fields`.
#[derive(Debug, Serialize)]
pub struct TableProfile {
    pub columns: Vec<TableProfileColumn>,
    pub pii_columns: Vec<String>,
    pub selected_columns: Vec<String>,
}

/// One recent ingest run for the connection this table belongs to. Jobs have no
/// per-table breakdown, so runs are connection-scoped (honest).
#[derive(Debug, Serialize)]
pub struct RecentRun {
    pub id: String,
    pub status: String,
    pub rows_processed: i64,
    pub chunks_created: i64,
    pub started_at: Option<String>,
    pub completed_at: Option<String>,
    pub errors: i64,
}

/// The full table inspector payload.
#[derive(Debug, Serialize)]
pub struct TableInfo {
    pub table: TableInfoTable,
    pub connection: Option<TableInfoConnection>,
    pub profile: Option<TableProfile>,
    pub recent_runs: Vec<RecentRun>,
}

/// `GET /tables/<table_id>/info` — inspector for one indexed table. Any AuthUser.
///
/// `table_id = {source_id}:{table}`; split on the **first** `:` (source ids are
/// colon-free generated ids, table names may not be). Returns 404 when there is no
/// `indexed_tables` doc for the id.
#[get("/tables/<table_id>/info")]
pub async fn get_table_info(
    state: &State<AppState>,
    _user: AuthUser,
    table_id: &str,
) -> AppResult<Json<TableInfo>> {
    let (source_id, table) = table_id
        .split_once(':')
        .ok_or_else(|| AppError::BadRequest("table_id must be '{source_id}:{table}'".into()))?;

    // --- table: from indexed_tables (required; 404 if the entry is gone) ---
    let it = state
        .db
        .indexed_tables()
        .find_one(doc! { "_id": table_id })
        .await?
        .ok_or(AppError::NotFound)?;

    let table_slice = TableInfoTable {
        id: table_id.to_string(),
        source_id: source_id.to_string(),
        source_table: it.get_str("source_table").unwrap_or(table).to_string(),
        row_count: it.get_i64("row_count").unwrap_or(0),
        vector_count: it.get_i64("vector_count").unwrap_or(0),
        status: it.get_str("status").unwrap_or("").to_string(),
        last_ingested: bson_dt_to_iso(it.get_datetime("last_ingested").ok()),
        last_embedded_at: bson_dt_to_iso(it.get_datetime("last_embedded_at").ok()),
    };

    // --- connection: decrypt the source spec (reuse load_spec); null if deleted ---
    let connection = match load_spec(&state.db, &state.config, source_id).await.ok() {
        Some(spec) => {
            // load_spec drops name/id; read the source doc for its display name.
            let name = state
                .db
                .sources()
                .find_one(doc! { "_id": source_id })
                .await?
                .and_then(|d| d.get_str("name").ok().map(str::to_string))
                .unwrap_or_default();
            Some(TableInfoConnection {
                id: source_id.to_string(),
                name,
                // SourceKind serializes lowercase → "postgres"/"mysql"/"mssql".
                kind: serde_json::to_value(spec.kind)
                    .ok()
                    .and_then(|v| v.as_str().map(str::to_string))
                    .unwrap_or_default(),
                host: spec.host,
                port: spec.port,
                database: spec.database,
                username: spec.username,
            })
        }
        None => None,
    };

    // --- profile: row-derived from one sampled record's fields; null if none ---
    let profile = state
        .db
        .records()
        .find_one(doc! { "source_id": source_id, "table": table, "active": true })
        .await?
        .and_then(|rec| rec.get_document("fields").cloned().ok())
        .map(|fields| {
            let mut columns = Vec::new();
            let mut pii_columns = Vec::new();
            let mut selected_columns = Vec::new();
            for (name, value) in &fields {
                let pii = is_likely_pii(name);
                if pii {
                    pii_columns.push(name.clone());
                }
                selected_columns.push(name.clone());
                columns.push(TableProfileColumn {
                    name: name.clone(),
                    type_: json_type_of(value),
                    nullable: true,
                    selected: true,
                    pii,
                });
            }
            TableProfile {
                columns,
                pii_columns,
                selected_columns,
            }
        });

    // --- recent_runs: jobs for this source, newest first, up to 5 ---
    let job_docs: Vec<Document> = state
        .db
        .jobs()
        .find(doc! { "source_id": source_id })
        .sort(doc! { "started_at": -1 })
        .limit(5)
        .await?
        .try_collect()
        .await?;

    let recent_runs = job_docs.iter().map(recent_run_from_doc).collect();

    Ok(Json(TableInfo {
        table: table_slice,
        connection,
        profile,
        recent_runs,
    }))
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Map a `jobs` document to a `RecentRun`, tolerating both the current job schema
/// (`processed_rows`, `errors` as an i64 count) and older jobs (`processed`,
/// `errors` as a string array).
fn recent_run_from_doc(d: &Document) -> RecentRun {
    // Current schema stores processed_rows; older jobs used `processed`.
    let rows_processed = d
        .get_i64("processed_rows")
        .or_else(|_| d.get_i64("processed"))
        .unwrap_or(0);

    // errors is an i64 count in the current schema; older jobs stored an array of
    // error strings — fall back to its length.
    let errors = d
        .get_i64("errors")
        .unwrap_or_else(|_| d.get_array("errors").map(|a| a.len() as i64).unwrap_or(0));

    RecentRun {
        id: d.get_str("_id").unwrap_or("").to_string(),
        status: d.get_str("status").unwrap_or("").to_string(),
        rows_processed,
        chunks_created: rows_processed,
        started_at: bson_dt_to_iso(d.get_datetime("started_at").ok()),
        completed_at: bson_dt_to_iso(d.get_datetime("finished_at").ok()),
        errors,
    }
}

/// The JSON value-type label for a BSON value, clamped to the profile's vocabulary
/// (string | number | boolean | object | null). Arrays report as "object".
fn json_type_of(value: &Bson) -> String {
    match value {
        Bson::String(_) => "string",
        Bson::Double(_) | Bson::Int32(_) | Bson::Int64(_) | Bson::Decimal128(_) => "number",
        Bson::Boolean(_) => "boolean",
        Bson::Null | Bson::Undefined => "null",
        _ => "object",
    }
    .to_string()
}

/// Read a `$count` result document's `n`, tolerating i32 or i64 encodings.
fn read_count(d: &Document) -> i64 {
    d.get_i32("n")
        .map(|n| n as i64)
        .or_else(|_| d.get_i64("n"))
        .unwrap_or(0)
}

/// Read a timestamp field that may be stored as an ISO string (records) or a BSON
/// date, returning an ISO-8601 string either way.
fn read_ts(d: &Document, key: &str) -> Option<String> {
    if let Ok(s) = d.get_str(key) {
        return Some(s.to_string());
    }
    bson_dt_to_iso(d.get_datetime(key).ok())
}

/// Convert a BSON DateTime reference to an ISO-8601 string; `None` on null/failure.
fn bson_dt_to_iso(dt: Option<&BsonDateTime>) -> Option<String> {
    dt.and_then(|d| {
        chrono::DateTime::<chrono::Utc>::from_timestamp_millis(d.timestamp_millis())
            .map(|dt| dt.to_rfc3339())
    })
}
