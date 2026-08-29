//! Ingestion pipeline: pull rows from a source database, chunk their text
//! projections, embed each chunk locally (fastembed BGE-M3), and upsert
//! `records` documents carrying the vector + structured fields. Progress is tracked
//! on a `jobs` document that the SSE route polls every 500 ms.
//!
//! The pipeline runs per-table: each table is fetched, chunked, and embedded in
//! isolation so a single table failure does not abort the whole job. Excluded
//! (PII) columns are dropped before chunking via `fetch_table`.
//!
//! Runs as a detached background task (`run`) so a long ingest survives the HTTP
//! request that started it.

pub mod routes;

use std::collections::HashMap;

use chrono::Utc;
use mongodb::bson::{Bson, DateTime as BsonDateTime, doc};
use serde::Serialize;
use serde_json::Value;

use crate::config::Config;
use crate::connectors::{self, connector};
use crate::documentdb::{DocumentDb, RECORDS, vector};
use crate::embed;
use crate::error::AppResult;

/// How many chunks to embed + insert per batch. Keeps peak memory bounded and lets
/// progress advance smoothly on large sources.
const BATCH_SIZE: usize = 32;

/// Emit a log line every this many rows so large tables aren't silent.
const LOG_ROW_INTERVAL: i64 = 2500;

/// A stored record: one embedded chunk of one source row. `_id` is deterministic
/// (`{source_id}:{table}:{row_pk}:{chunk_index}`) so re-ingesting the same table
/// is a clean replace — IDs from other tables are unaffected.
#[derive(Debug, Serialize)]
struct RecordDoc {
    #[serde(rename = "_id")]
    id: String,
    source_id: String,
    /// The source table this chunk belongs to. Used by `delete_many` per-table and
    /// read by Data Explorer (Stage 5) to filter records.
    table: String,
    row_pk: String,
    chunk_index: i32,
    /// The row's original columns, kept as structured metadata for filtering/citation.
    fields: Value,
    /// The chunk text that was embedded and full-text indexed.
    text: String,
    #[serde(rename = "contentVector")]
    content_vector: Vec<f32>,
    ingested_at: chrono::DateTime<Utc>,
}

/// One chunk queued for embedding, carrying enough context to build its `RecordDoc`.
struct Chunk {
    row_pk: String,
    chunk_index: i32,
    fields: Value,
    text: String,
    table: String,
}

/// One entry in the job log, emitted as part of every progress snapshot.
/// Level `divider` is a visual separator between table runs; the message is ignored.
#[derive(Debug, Clone, Serialize)]
pub struct LogEntry {
    pub time: String,
    pub level: String,
    pub message: String,
}

/// Snapshot of all counters written to the jobs doc after each batch.
struct Snap<'a> {
    status: &'a str,
    table_index: i64,
    total_tables: i64,
    current_table: &'a str,
    table_rows: i64,
    table_total: i64,
    processed_rows: i64,
    total_rows: i64,
    errors: i64,
    success_tables: i64,
    failed_tables: i64,
    db_size_bytes: i64,
}

/// Append a log entry, capping the vec at 500 to bound the jobs doc size.
fn push_log(log: &mut Vec<LogEntry>, level: &str, message: &str) {
    if log.len() >= 500 {
        // Drop the oldest non-divider entry to make room — keep dividers for context.
        if let Some(pos) = log.iter().position(|e| e.level != "divider") {
            log.remove(pos);
        } else {
            log.remove(0);
        }
    }
    log.push(LogEntry {
        time: Utc::now().to_rfc3339(),
        level: level.to_string(),
        message: message.to_string(),
    });
}

/// Serialize the log vec to a BSON array for storage in the jobs doc.
fn log_to_bson(log: &[LogEntry]) -> Bson {
    Bson::Array(
        log.iter()
            .map(|e| {
                Bson::Document(doc! {
                    "time": &e.time,
                    "level": &e.level,
                    "message": &e.message,
                })
            })
            .collect(),
    )
}

/// Write all progress counters + the current log to the jobs doc. Called after every
/// batch so the SSE poller always sees a fresh snapshot.
async fn write_snap(db: &DocumentDb, job_id: &str, s: &Snap<'_>, log: &[LogEntry]) -> AppResult<()> {
    db.jobs()
        .update_one(
            doc! { "_id": job_id },
            doc! { "$set": {
                "status":        s.status,
                "table_index":   s.table_index,
                "total_tables":  s.total_tables,
                "current_table": s.current_table,
                "table_rows":    s.table_rows,
                "table_total":   s.table_total,
                "processed_rows": s.processed_rows,
                "total_rows":    s.total_rows,
                "errors":        s.errors,
                "success_tables": s.success_tables,
                "failed_tables": s.failed_tables,
                "db_size_bytes": s.db_size_bytes,
                "log":           log_to_bson(log),
            }},
        )
        .await?;
    Ok(())
}

/// Upsert an `indexed_tables` doc for a table at the given status. Row/vector counts
/// are only meaningful after a completed ingest and are set to 0 for "indexing"/"error".
async fn upsert_indexed_table(
    db: &DocumentDb,
    source_id: &str,
    table: &str,
    status: &str,
    row_count: i64,
    vector_count: i64,
    set_timestamps: bool,
) {
    let id = format!("{source_id}:{table}");
    let mut fields = doc! {
        "source_id":    source_id,
        "source_table": table,
        "status":       status,
        "row_count":    row_count,
        "vector_count": vector_count,
    };
    if set_timestamps {
        let now = BsonDateTime::now();
        fields.insert("last_ingested", now);
        fields.insert("last_embedded_at", now);
    }
    let _ = db
        .indexed_tables()
        .update_one(doc! { "_id": &id }, doc! { "$set": fields })
        .upsert(true)
        .await;
}

/// Background entry point: run the pipeline and record the outcome on the job doc.
/// Never panics the task — any error is written back as a failed job.
pub async fn run(
    db: DocumentDb,
    config: Config,
    source_id: String,
    job_id: String,
    tables: Vec<String>,
    excluded_columns: HashMap<String, Vec<String>>,
    limit: Option<i64>,
) {
    if let Err(e) = execute(&db, &config, &source_id, &job_id, &tables, &excluded_columns, limit).await {
        tracing::error!(job = %job_id, error = %e, "ingestion pipeline failed");
        let mut log = vec![];
        push_log(&mut log, "error", &format!("Job failed: {e}"));
        let _ = db
            .jobs()
            .update_one(
                doc! { "_id": &job_id },
                doc! { "$set": {
                    "status": "failed",
                    "log":    log_to_bson(&log),
                    "finished_at": BsonDateTime::now(),
                }},
            )
            .await;
    }
}

/// The pipeline proper. Returns an error only for catastrophic failures that prevent
/// even starting (index creation, source load). Per-table errors are caught, logged,
/// and continue to the next table.
async fn execute(
    db: &DocumentDb,
    config: &Config,
    source_id: &str,
    job_id: &str,
    tables: &[String],
    excluded_columns: &HashMap<String, Vec<String>>,
    limit: Option<i64>,
) -> AppResult<()> {
    // 1. Ensure vector + full-text indexes exist before writing any records.
    vector::ensure_indexes(db, config.embedding_dims).await?;

    // 2. Load + decrypt the source spec.
    let spec = connectors::routes::load_spec(db, config, source_id).await?;
    let conn = connector(&spec);

    let total_tables = tables.len() as i64;
    let mut success_tables = 0i64;
    let mut failed_tables = 0i64;
    let mut errors_count = 0i64;
    let mut processed_rows = 0i64;
    let mut total_rows_est = 0i64;
    let mut db_size_bytes = 0i64;
    let mut log: Vec<LogEntry> = Vec::new();

    let records_coll = db.collection::<RecordDoc>(RECORDS);

    // 3. Per-table loop: each table is an independent unit; one failure continues.
    for (table_idx, table) in tables.iter().enumerate() {
        let table_index = (table_idx + 1) as i64;
        let empty: Vec<String> = Vec::new();
        let excluded = excluded_columns.get(table.as_str()).unwrap_or(&empty);

        // Visual divider + start banner in the log.
        push_log(&mut log, "divider", "");
        push_log(
            &mut log,
            "info",
            &format!("Starting table {table_index} of {total_tables}: {table}"),
        );
        tracing::info!(job = %job_id, table, "starting table {table_index}/{total_tables}");

        // Mark the table as "indexing" immediately so the history view reflects it.
        upsert_indexed_table(db, source_id, table, "indexing", 0, 0, false).await;

        // Write a progress snapshot so the UI shows the current table name.
        let _ = write_snap(
            db,
            job_id,
            &Snap {
                status: "running",
                table_index,
                total_tables,
                current_table: table,
                table_rows: 0,
                table_total: 0,
                processed_rows,
                total_rows: total_rows_est,
                errors: errors_count,
                success_tables,
                failed_tables,
                db_size_bytes,
            },
            &log,
        )
        .await;

        // Exact row count so the progress bar can show a meaningful percentage.
        let table_total = match conn.count_table(table).await {
            Ok(n) => n,
            Err(e) => {
                let msg = format!("count_table({table}) failed: {e}");
                tracing::error!(job = %job_id, "{}", msg);
                push_log(&mut log, "error", &msg);
                failed_tables += 1;
                errors_count += 1;
                upsert_indexed_table(db, source_id, table, "error", 0, 0, false).await;
                continue;
            }
        };
        total_rows_est += table_total;

        // Fetch rows with PII columns already excluded.
        let rows = match conn.fetch_table(table, excluded, limit).await {
            Ok(r) => r,
            Err(e) => {
                let msg = format!("fetch_table({table}) failed: {e}");
                tracing::error!(job = %job_id, "{}", msg);
                push_log(&mut log, "error", &msg);
                failed_tables += 1;
                errors_count += 1;
                upsert_indexed_table(db, source_id, table, "error", 0, 0, false).await;
                continue;
            }
        };
        tracing::info!(job = %job_id, table, rows = rows.len(), "fetched rows");

        // Per-table replace: delete only this table's prior records so other tables
        // remain queryable while this one is re-indexed.
        let _ = db
            .records()
            .delete_many(doc! { "source_id": source_id, "table": table })
            .await;

        // Chunk each row's text into embed-sized passages.
        let mut chunks: Vec<Chunk> = Vec::new();
        for row in rows {
            let texts = if config.chunk_enabled {
                chunk_text(&row.text, config.chunk_size_tokens, config.chunk_overlap_tokens)
            } else {
                vec![row.text.clone()]
            };
            let fields = Value::Object(row.fields);
            for (ci, text) in texts.into_iter().enumerate() {
                if text.trim().is_empty() {
                    continue;
                }
                chunks.push(Chunk {
                    row_pk: row.pk.clone(),
                    chunk_index: ci as i32,
                    fields: fields.clone(),
                    text,
                    table: table.clone(),
                });
            }
        }

        // Embed + insert in BATCH_SIZE batches, advancing counters after each.
        let mut table_rows = 0i64;
        let mut vector_count = 0i64;
        let mut table_failed = false;

        'batches: for batch in chunks.chunks(BATCH_SIZE) {
            let texts: Vec<String> = batch.iter().map(|c| c.text.clone()).collect();
            let vectors = match embed::embed_documents(config, texts).await {
                Ok(v) => v,
                Err(e) => {
                    let msg = format!("embed failed for table {table}: {e}");
                    tracing::error!(job = %job_id, "{}", msg);
                    push_log(&mut log, "error", &msg);
                    errors_count += 1;
                    table_failed = true;
                    break 'batches;
                }
            };

            let docs: Vec<RecordDoc> = batch
                .iter()
                .zip(vectors)
                .map(|(c, vector)| RecordDoc {
                    // Include table in the _id so chunks from different tables never collide.
                    id: format!("{source_id}:{}:{}:{}", c.table, c.row_pk, c.chunk_index),
                    source_id: source_id.to_string(),
                    table: c.table.clone(),
                    row_pk: c.row_pk.clone(),
                    chunk_index: c.chunk_index,
                    fields: c.fields.clone(),
                    text: c.text.clone(),
                    content_vector: vector,
                    ingested_at: Utc::now(),
                })
                .collect();

            if !docs.is_empty() {
                if let Err(e) = records_coll.insert_many(&docs).await {
                    let msg = format!("insert failed for table {table}: {e}");
                    tracing::error!(job = %job_id, "{}", msg);
                    push_log(&mut log, "error", &msg);
                    errors_count += 1;
                    table_failed = true;
                    break 'batches;
                }
                // Accumulate UTF-8 bytes of embedded text as a proxy for "DB growth".
                let batch_bytes: i64 = batch.iter().map(|c| c.text.len() as i64).sum();
                db_size_bytes += batch_bytes;
                vector_count += docs.len() as i64;
            }

            table_rows += batch.len() as i64;
            processed_rows += batch.len() as i64;

            // Periodic log line so large tables aren't silent.
            if table_rows > 0 && table_rows % LOG_ROW_INTERVAL < BATCH_SIZE as i64 {
                push_log(
                    &mut log,
                    "info",
                    &format!("Table {table}: {table_rows} rows processed…"),
                );
            }

            // Write the full progress snapshot after every batch.
            let _ = write_snap(
                db,
                job_id,
                &Snap {
                    status: "running",
                    table_index,
                    total_tables,
                    current_table: table,
                    table_rows,
                    table_total,
                    processed_rows,
                    total_rows: total_rows_est,
                    errors: errors_count,
                    success_tables,
                    failed_tables,
                    db_size_bytes,
                },
                &log,
            )
            .await;
        }

        // Update per-table outcome.
        if table_failed {
            failed_tables += 1;
            upsert_indexed_table(db, source_id, table, "error", table_rows, vector_count, false)
                .await;
            tracing::error!(job = %job_id, table, "table ingestion failed");
        } else {
            success_tables += 1;
            upsert_indexed_table(
                db,
                source_id,
                table,
                "indexed",
                table_rows,
                vector_count,
                true,
            )
            .await;
            push_log(
                &mut log,
                "success",
                &format!(
                    "Table {table}: ingested {table_rows} rows ({vector_count} chunks) successfully"
                ),
            );
            tracing::info!(job = %job_id, table, rows = table_rows, vectors = vector_count, "table ingestion complete");
        }
    }

    // 4. Determine terminal status: completed / partial / failed.
    let final_status = if failed_tables == 0 {
        "completed"
    } else if success_tables > 0 {
        "partial"
    } else {
        "failed"
    };

    let final_level = if final_status == "completed" { "success" } else { "error" };
    push_log(
        &mut log,
        final_level,
        &format!(
            "Ingestion {final_status}: {success_tables} succeeded, {failed_tables} failed"
        ),
    );
    tracing::info!(job = %job_id, status = final_status, success_tables, failed_tables, "ingestion finished");

    // Write final snapshot with terminal status + finished_at.
    db.jobs()
        .update_one(
            doc! { "_id": job_id },
            doc! { "$set": {
                "status":        final_status,
                "table_index":   total_tables,
                "total_tables":  total_tables,
                "current_table": "",
                "table_rows":    0i64,
                "table_total":   0i64,
                "processed_rows": processed_rows,
                "total_rows":    total_rows_est,
                "errors":        errors_count,
                "success_tables": success_tables,
                "failed_tables": failed_tables,
                "db_size_bytes": db_size_bytes,
                "log":           log_to_bson(&log),
                "finished_at":   BsonDateTime::now(),
            }},
        )
        .await?;

    Ok(())
}

/// Split text into overlapping windows. Windows are measured in **words** as a cheap
/// proxy for tokens (no tokenizer here); BGE-M3's 8192-token context makes the
/// approximation safe. Returns the whole text as one chunk when it fits in a window.
fn chunk_text(text: &str, size: usize, overlap: usize) -> Vec<String> {
    let words: Vec<&str> = text.split_whitespace().collect();
    if size == 0 || words.len() <= size {
        return vec![text.to_string()];
    }
    let step = size.saturating_sub(overlap).max(1);
    let mut chunks = Vec::new();
    let mut start = 0;
    while start < words.len() {
        let end = (start + size).min(words.len());
        chunks.push(words[start..end].join(" "));
        if end == words.len() {
            break;
        }
        start += step;
    }
    chunks
}
