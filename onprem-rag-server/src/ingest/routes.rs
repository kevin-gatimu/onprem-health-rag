//! Ingestion routes: start a job (`POST /ingest`), follow progress over SSE
//! (`GET /ingest/<job>/stream`), inspect history (`GET /ingest/history`), and
//! delete individual table records (`DELETE /ingest/table/<source_id>/<table>`).
//!
//! The job runs as a detached background task. SSE clients receive process-local
//! notifications and then read the durable job snapshot, so reconnects remain safe
//! without continuously polling DocumentDB.

use std::collections::HashMap;

use futures::TryStreamExt;
use mongodb::bson::{DateTime as BsonDateTime, Document, doc};
use rocket::response::stream::{Event, EventStream};
use rocket::serde::json::Json;
use rocket::{State, delete, get, post};
use serde::{Deserialize, Serialize};
use tokio::time::{Duration, timeout};

use super::{LogEntry, ResumeCheckpoint, run};
use crate::auth::{audit, guard::AuthUser};
use crate::documentdb::{INDEXED_TABLES, RECORDS, SOURCES};
use crate::error::{AppError, AppResult};
use crate::state::AppState;

/// Durable fallback interval for missed notifications or a server restart.
const SNAPSHOT_FALLBACK_INTERVAL: Duration = Duration::from_secs(15);

/// Widened ingest request: caller selects which tables to ingest, optionally excluding
/// PII columns per table, and can cap rows per table for smoke-test runs.
#[derive(Debug, Deserialize)]
pub struct IngestRequest {
    pub source_id: String,
    /// Tables to ingest, in the order they will be processed.
    pub tables: Vec<String>,
    /// Per-table column exclusion list. Excluded columns are dropped before embedding
    /// and are not stored on the record document.
    #[serde(default)]
    pub excluded_columns: Option<HashMap<String, Vec<String>>>,
    /// Optional row cap per table (smoke-test guard).
    #[serde(default)]
    pub limit: Option<i64>,
}

#[derive(Debug, Serialize)]
pub struct IngestResponse {
    pub job_id: String,
}

/// Full 14-field progress snapshot emitted by the SSE stream on every tick.
/// The bridge mirrors this struct exactly; see `src-tauri/src/commands.rs`.
#[derive(Debug, Serialize)]
pub struct IngestProgress {
    pub job_id: String,
    pub status: String,
    /// 1-based index of the table currently being ingested.
    pub table_index: i64,
    pub total_tables: i64,
    pub current_table: String,
    /// Rows processed in the current table (this batch).
    pub table_rows: i64,
    /// Estimated total rows in the current table.
    pub table_total: i64,
    /// Cumulative rows processed across all tables.
    pub processed_rows: i64,
    /// Estimated total rows across all selected tables.
    pub total_rows: i64,
    /// Count of errors encountered (detail in `log`).
    pub errors: i64,
    pub success_tables: i64,
    pub failed_tables: i64,
    /// Cumulative UTF-8 bytes of embedded chunk text — a monotonic proxy for DB growth.
    pub db_size_bytes: i64,
    /// Rows annotated by the clinical extractor (plan 25). Always 0 when
    /// `ONPREM_EXTRACT_ENABLED` is false, which is the default.
    pub extracted_rows: i64,
    /// Full current log; server-capped at 500 entries. Client replaces wholesale each event.
    pub log: Vec<LogEntry>,
}

/// Extract the full `IngestProgress` from a jobs BSON document.
fn progress_from_doc(job_id: &str, doc: &Document) -> IngestProgress {
    IngestProgress {
        job_id: job_id.to_string(),
        status: doc.get_str("status").unwrap_or("running").to_string(),
        table_index: doc.get_i64("table_index").unwrap_or(1),
        total_tables: doc.get_i64("total_tables").unwrap_or(0),
        current_table: doc.get_str("current_table").unwrap_or("").to_string(),
        table_rows: doc.get_i64("table_rows").unwrap_or(0),
        table_total: doc.get_i64("table_total").unwrap_or(0),
        processed_rows: doc.get_i64("processed_rows").unwrap_or(0),
        total_rows: doc.get_i64("total_rows").unwrap_or(0),
        errors: doc.get_i64("errors").unwrap_or(0),
        success_tables: doc.get_i64("success_tables").unwrap_or(0),
        failed_tables: doc.get_i64("failed_tables").unwrap_or(0),
        db_size_bytes: doc.get_i64("db_size_bytes").unwrap_or(0),
        extracted_rows: doc.get_i64("extracted_rows").unwrap_or(0),
        log: extract_log(doc),
    }
}

/// Deserialise the `log` array from a BSON jobs document into `Vec<LogEntry>`.
fn extract_log(doc: &Document) -> Vec<LogEntry> {
    doc.get_array("log")
        .map(|a| {
            a.iter()
                .filter_map(|b| {
                    let d = b.as_document()?;
                    Some(LogEntry {
                        time: d.get_str("time").unwrap_or("").to_string(),
                        level: d.get_str("level").unwrap_or("info").to_string(),
                        message: d.get_str("message").unwrap_or("").to_string(),
                    })
                })
                .collect()
        })
        .unwrap_or_default()
}

/// `POST /ingest` — start ingesting selected tables from a saved source. Admin only.
/// Returns immediately with a `job_id`; the client follows progress via the SSE stream.
#[post("/ingest", data = "<body>")]
pub async fn start_ingest(
    state: &State<AppState>,
    user: AuthUser,
    body: Json<IngestRequest>,
) -> AppResult<Json<IngestResponse>> {
    user.require_admin()?;
    let req = body.into_inner();

    if req.tables.is_empty() {
        return Err(AppError::BadRequest("tables must be non-empty".into()));
    }

    // Verify the source exists and load its spec for schema validation.
    let spec =
        crate::connectors::routes::load_spec(&state.db, &state.config, &req.source_id).await?;

    // Validate each requested table against the live schema — this is the injection
    // guard; count_table/fetch_table in the pipeline trust these names are clean.
    let schema = crate::connectors::connector(&spec).get_schema().await?;
    let known: std::collections::HashSet<&str> = schema.iter().map(|t| t.name.as_str()).collect();
    for table in &req.tables {
        if !known.contains(table.as_str()) {
            return Err(AppError::BadRequest(format!(
                "table '{table}' does not exist in the source schema"
            )));
        }
    }

    let ingest_permit = state.admission.ingestion(&req.source_id).await?;
    let excluded = req.excluded_columns.clone().unwrap_or_default();
    let excluded_bson = mongodb::bson::to_bson(&excluded)
        .map_err(|e| AppError::Internal(format!("failed to serialize ingest request: {e}")))?;
    let limit_bson = req
        .limit
        .map_or(mongodb::bson::Bson::Null, mongodb::bson::Bson::Int64);
    let job_id = uuid::Uuid::now_v7().to_string();
    let total_tables = req.tables.len() as i64;
    let first_table = req.tables.first().map(|t| t.as_str()).unwrap_or("");

    state
        .db
        .jobs()
        .insert_one(doc! {
            "_id":           &job_id,
            "source_id":     &req.source_id,
            "requested_tables": &req.tables,
            "excluded_columns": excluded_bson,
            "row_limit": limit_bson,
            "status":        "running",
            "table_index":   1i64,
            "total_tables":  total_tables,
            "current_table": first_table,
            "table_rows":    0i64,
            "table_total":   0i64,
            "processed_rows": 0i64,
            "total_rows":    0i64,
            "errors":        0i64,
            "success_tables": 0i64,
            "failed_tables": 0i64,
            "db_size_bytes": 0i64,
            "extracted_rows": 0i64,
            "log":           [],
            "started_at":    BsonDateTime::now(),
            "finished_at":   mongodb::bson::Bson::Null,
        })
        .await?;

    state.ingest_progress.register(&job_id);

    // Detach the pipeline: it owns cloned handles and outlives this request.
    let db = state.db.clone();
    let config = state.config.clone();
    let progress_hub = state.ingest_progress.clone();
    let foundry = state.foundry_handle();
    let source_id = req.source_id.clone();
    let job = job_id.clone();
    let tables = req.tables.clone();
    let limit = req.limit;
    tokio::spawn(async move {
        let _ingest_permit = ingest_permit;
        run(
            db,
            config,
            progress_hub,
            source_id,
            job,
            tables,
            excluded,
            limit,
            None,
            foundry,
        )
        .await
    });

    // req.source_id was only cloned into the spawn, so it remains valid here.
    audit::write_audit(
        &state.db,
        &user.id,
        &user.username,
        "ingest_started",
        &req.source_id,
        Some(serde_json::json!({ "tables": total_tables })),
    )
    .await;

    Ok(Json(IngestResponse { job_id }))
}

/// `POST /ingest/<job>/resume` — safely restart an interrupted generation from its
/// persisted request. Existing active data remains searchable until the retry cuts over.
#[post("/ingest/<job>/resume")]
pub async fn resume_ingest(
    state: &State<AppState>,
    user: AuthUser,
    job: &str,
) -> AppResult<Json<IngestResponse>> {
    user.require_admin()?;
    let saved = state
        .db
        .jobs()
        .find_one(doc! { "_id": job })
        .await?
        .ok_or(AppError::NotFound)?;
    let status = saved.get_str("status").unwrap_or("");
    if !matches!(status, "failed" | "partial") {
        return Err(AppError::BadRequest(
            "only failed or partial ingestion jobs can be resumed".into(),
        ));
    }

    let source_id = saved
        .get_str("source_id")
        .map(str::to_owned)
        .map_err(|_| AppError::BadRequest("job has no resumable source".into()))?;
    let tables: Vec<String> = saved
        .get_array("requested_tables")
        .map_err(|_| AppError::BadRequest("job predates resumable request metadata".into()))?
        .iter()
        .filter_map(|value| value.as_str().map(str::to_owned))
        .collect();
    if tables.is_empty() {
        return Err(AppError::BadRequest("job has no resumable tables".into()));
    }
    let excluded: HashMap<String, Vec<String>> = saved
        .get("excluded_columns")
        .cloned()
        .map(mongodb::bson::from_bson)
        .transpose()
        .map_err(|e| AppError::BadRequest(format!("invalid saved ingest request: {e}")))?
        .unwrap_or_default();
    let limit = saved.get_i64("row_limit").ok();
    let checkpoint = ResumeCheckpoint {
        table: saved
            .get_str("checkpoint_table")
            .ok()
            .filter(|table| tables.iter().any(|saved_table| saved_table == table))
            .map(str::to_owned)
            .ok_or_else(|| AppError::BadRequest("job has no valid resume table".into()))?,
        offset: saved
            .get_i64("checkpoint_offset")
            .ok()
            .filter(|offset| *offset >= 0)
            .ok_or_else(|| AppError::BadRequest("job has no valid resume offset".into()))?,
        generation: saved
            .get_str("checkpoint_generation")
            .ok()
            .filter(|generation| !generation.is_empty())
            .map(str::to_owned)
            .ok_or_else(|| AppError::BadRequest("job predates generation checkpoints".into()))?,
    };
    let permit = state.admission.ingestion(&source_id).await?;

    state
        .db
        .jobs()
        .update_one(
            doc! { "_id": job, "status": { "$in": ["failed", "partial"] } },
            doc! { "$set": {
                "status": "running",
                "finished_at": mongodb::bson::Bson::Null,
                "recovery_error": mongodb::bson::Bson::Null,
                "started_at": BsonDateTime::now(),
            } },
        )
        .await?;
    state.ingest_progress.register(job);

    let db = state.db.clone();
    let config = state.config.clone();
    let progress_hub = state.ingest_progress.clone();
    let foundry = state.foundry_handle();
    let job_id = job.to_string();
    let response_job_id = job_id.clone();
    tokio::spawn(async move {
        let _permit = permit;
        run(
            db,
            config,
            progress_hub,
            source_id,
            job_id,
            tables,
            excluded,
            limit,
            Some(checkpoint),
            foundry,
        )
        .await
    });

    Ok(Json(IngestResponse {
        job_id: response_job_id,
    }))
}

/// `GET /ingest/<job>/stream` — SSE stream of a job's progress. Any authenticated user
/// may watch. Emits immediately, then follows notifications with a durable fallback read.
///
/// Terminal events: `ingest://done` (completed or partial) and `ingest://error` (failed).
/// The bridge maps these Rocket event names to Tauri events.
///
/// When a terminal `completed` or `partial` status is detected, the catalog is rebuilt
/// from the updated store so new tables and fields become available to the planner
/// immediately, without a server restart.
#[get("/ingest/<job>/stream")]
pub async fn ingest_stream(
    state: &State<AppState>,
    _user: AuthUser,
    job: String,
) -> EventStream![] {
    let db = state.db.clone();
    // Clone the catalog handle so the generator can update it after a terminal status.
    // The AppState reference cannot cross the yield boundary, but the Arc handle can.
    let catalog_handle = state.catalog_handle();
    let progress_hub = state.ingest_progress.clone();
    let mut notifications = progress_hub.subscribe(&job);

    EventStream! {
        loop {
            match db.jobs().find_one(doc! { "_id": &job }).await {
                Ok(Some(doc)) => {
                    let status = doc.get_str("status").unwrap_or("running").to_string();
                    let progress = progress_from_doc(&job, &doc);
                    yield Event::json(&progress).event("progress");

                    // Terminal states end the stream. The bridge maps these to Tauri
                    // events: completed/partial -> ingest://done, failed -> ingest://error.
                    match status.as_str() {
                        "completed" | "partial" => {
                            let new_cat = crate::aggregation::catalog::build_from_store(&db).await;
                            if let Ok(mut w) = catalog_handle.write() {
                                *w = std::sync::Arc::new(new_cat);
                            }
                            progress_hub.remove(&job);
                            yield Event::json(&progress).event("done");
                            break;
                        }
                        "failed" => {
                            progress_hub.remove(&job);
                            yield Event::json(&progress).event("error");
                            break;
                        }
                        _ => {}
                    }
                }
                Ok(None) => {
                    yield Event::data(format!("job {job} not found")).event("error");
                    break;
                }
                Err(e) => {
                    yield Event::data(format!("progress read failed: {e}")).event("error");
                    break;
                }
            }

            // Notifications coalesce naturally because each wake-up reloads the latest
            // persisted snapshot. The timeout covers process restarts and missed sends.
            let _ = timeout(SNAPSHOT_FALLBACK_INTERVAL, notifications.recv()).await;
        }
    }
}

// ---------------------------------------------------------------------------
// Ingest history types
// ---------------------------------------------------------------------------

/// One row in the ingest history for a single table.
#[derive(Debug, Serialize)]
pub struct IngestionHistoryTable {
    /// Composite key `"{source_id}:{table}"` — stable identifier for this entry.
    pub table_id: String,
    pub source_table: String,
    pub row_count: i64,
    pub vector_count: i64,
    /// "indexed" | "error" | "indexing"
    pub status: String,
    /// ISO-8601 string or null.
    pub last_ingested: Option<String>,
    /// ISO-8601 string or null.
    pub last_embedded_at: Option<String>,
}

/// Ingest history grouped by source connection.
#[derive(Debug, Serialize)]
pub struct IngestionHistoryConnection {
    pub source_id: String,
    pub source_name: String,
    /// "postgres" | "mysql" | "mssql"
    pub kind: String,
    pub database: String,
    pub tables: Vec<IngestionHistoryTable>,
    pub total_rows: i64,
    pub total_vectors: i64,
    pub last_ingested: Option<String>,
}

/// `GET /ingest/history` — return all indexed-table records grouped by source. Any AuthUser.
#[get("/ingest/history")]
pub async fn ingest_history(
    state: &State<AppState>,
    _user: AuthUser,
) -> AppResult<Json<Vec<IngestionHistoryConnection>>> {
    // Read all indexed_tables documents.
    let indexed: Vec<Document> = state
        .db
        .collection::<Document>(INDEXED_TABLES)
        .find(doc! {})
        .sort(doc! { "last_ingested": -1, "_id": -1 })
        .await?
        .try_collect()
        .await?;

    // Group by source_id in Rust — simpler than a MongoDB $group pipeline here.
    let mut groups: HashMap<String, Vec<Document>> = HashMap::new();
    for d in indexed {
        let sid = d.get_str("source_id").unwrap_or("").to_string();
        groups.entry(sid).or_default().push(d);
    }

    let mut result: Vec<IngestionHistoryConnection> = Vec::new();
    for (source_id, table_docs) in groups {
        // Look up the source for its name, kind, and database.
        let src = state
            .db
            .collection::<Document>(SOURCES)
            .find_one(doc! { "_id": &source_id })
            .await?;
        let (source_name, kind, database) = src
            .as_ref()
            .map(|d| {
                (
                    d.get_str("name").unwrap_or("").to_string(),
                    d.get_str("kind").unwrap_or("postgres").to_string(),
                    d.get_str("database").unwrap_or("").to_string(),
                )
            })
            .unwrap_or_default();

        let mut tables = Vec::new();
        let mut total_rows = 0i64;
        let mut total_vectors = 0i64;
        let mut last_ingested_max: Option<String> = None;

        for td in table_docs {
            let row_count = td.get_i64("row_count").unwrap_or(0);
            let vector_count = td.get_i64("vector_count").unwrap_or(0);
            total_rows += row_count;
            total_vectors += vector_count;

            // Convert BsonDateTime to ISO string; null when the field is absent.
            let li = bson_dt_to_iso(td.get_datetime("last_ingested").ok());
            let lea = bson_dt_to_iso(td.get_datetime("last_embedded_at").ok());

            // Track the max last_ingested across tables (ISO strings sort lexicographically).
            if let Some(ref s) = li {
                match &last_ingested_max {
                    None => last_ingested_max = Some(s.clone()),
                    Some(existing) if s > existing => last_ingested_max = Some(s.clone()),
                    _ => {}
                }
            }

            // The _id is stored as a string by the ingest pipeline.
            let table_id = td
                .get_str("_id")
                .map(str::to_string)
                .unwrap_or_else(|_| format!("{source_id}:?"));
            let source_table = td.get_str("source_table").unwrap_or("").to_string();
            let status = td.get_str("status").unwrap_or("").to_string();

            tables.push(IngestionHistoryTable {
                table_id,
                source_table,
                row_count,
                vector_count,
                status,
                last_ingested: li,
                last_embedded_at: lea,
            });
        }

        result.push(IngestionHistoryConnection {
            source_id,
            source_name,
            kind,
            database,
            tables,
            total_rows,
            total_vectors,
            last_ingested: last_ingested_max,
        });
    }

    // Stable output: sort by source_id.
    result.sort_by(|a, b| a.source_id.cmp(&b.source_id));
    Ok(Json(result))
}

/// `DELETE /ingest/table/<source_id>/<table>` — remove one table's indexed records and
/// its history entry. Admin only.
#[delete("/ingest/table/<source_id>/<table>")]
pub async fn delete_ingest_table(
    state: &State<AppState>,
    user: AuthUser,
    source_id: &str,
    table: &str,
) -> AppResult<Json<serde_json::Value>> {
    user.require_admin()?;
    let id = format!("{source_id}:{table}");

    // Remove the history entry.
    state
        .db
        .collection::<Document>(INDEXED_TABLES)
        .delete_one(doc! { "_id": &id })
        .await?;

    // Remove all embedded records for this table.
    state
        .db
        .collection::<Document>(RECORDS)
        .delete_many(doc! { "source_id": source_id, "table": table })
        .await?;

    // Rebuild catalog so the deleted table stops appearing in the planner context.
    let new_cat = crate::aggregation::catalog::build_from_store(&state.db).await;
    state.set_catalog(new_cat);

    Ok(Json(serde_json::json!({ "ok": true })))
}

/// `DELETE /ingest/connection/<source_id>` — remove every indexed table and all
/// embedded records for one source connection. Admin only. Used by the Data
/// Explorer "clear connection" action. Returns the number of `indexed_tables`
/// entries removed.
#[delete("/ingest/connection/<source_id>")]
pub async fn delete_ingest_connection(
    state: &State<AppState>,
    user: AuthUser,
    source_id: &str,
) -> AppResult<Json<serde_json::Value>> {
    user.require_admin()?;

    // Remove all history entries for this source, counting how many.
    let removed = state
        .db
        .collection::<Document>(INDEXED_TABLES)
        .delete_many(doc! { "source_id": source_id })
        .await?
        .deleted_count;

    // Remove all embedded records for this source.
    state
        .db
        .collection::<Document>(RECORDS)
        .delete_many(doc! { "source_id": source_id })
        .await?;

    // Rebuild catalog so deleted tables stop appearing in the planner context.
    let new_cat = crate::aggregation::catalog::build_from_store(&state.db).await;
    state.set_catalog(new_cat);

    Ok(Json(
        serde_json::json!({ "tables_removed": removed as i64 }),
    ))
}

/// `DELETE /ingest/all` — clear every record and indexed-table entry across all
/// sources. Admin only. Used by the Data Explorer "clear all" action.
#[delete("/ingest/all")]
pub async fn delete_ingest_all(
    state: &State<AppState>,
    user: AuthUser,
) -> AppResult<Json<serde_json::Value>> {
    user.require_admin()?;

    state
        .db
        .collection::<Document>(RECORDS)
        .delete_many(doc! {})
        .await?;
    state
        .db
        .collection::<Document>(INDEXED_TABLES)
        .delete_many(doc! {})
        .await?;

    // Rebuild catalog — all tables are gone so the catalog becomes empty.
    let new_cat = crate::aggregation::catalog::build_from_store(&state.db).await;
    state.set_catalog(new_cat);

    Ok(Json(serde_json::json!({ "ok": true })))
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Convert a BSON DateTime reference to an ISO-8601 string, returning `None` when
/// the value is null or the conversion fails.
fn bson_dt_to_iso(dt: Option<&BsonDateTime>) -> Option<String> {
    dt.and_then(|d| {
        chrono::DateTime::<chrono::Utc>::from_timestamp_millis(d.timestamp_millis())
            .map(|dt| dt.to_rfc3339())
    })
}
