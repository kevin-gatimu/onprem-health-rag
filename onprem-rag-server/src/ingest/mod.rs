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

pub mod extract;
pub mod routes;

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use tokio::sync::broadcast;

use chrono::Utc;
use mongodb::bson::{Bson, DateTime as BsonDateTime, doc};
use serde::Serialize;
use serde_json::{Map, Value};

use crate::config::Config;
use crate::connectors::{self, connector};
use crate::documentdb::{DocumentDb, RECORDS, vector};
use crate::embed;
use crate::error::{AppError, AppResult};
use crate::foundry::FoundryManager;
use crate::foundry::router::ModelSpec;
use crate::ingest::extract::{ExtractedClinical, Extractor};

/// Emit a log line every this many rows so large tables aren't silent.
const LOG_ROW_INTERVAL: i64 = 2500;
const EXTRACT_BATCH: usize = 16;
const MAX_PATIENT_IDENTITIES: usize = 500_000;
const AUDIT_COLUMNS: [&str; 7] = [
    "created_at",
    "updated_at",
    "deleted_at",
    "inserted_at",
    "modified_at",
    "row_version",
    "last_modified",
];

fn exclude_from_chunk_text(column: &str) -> bool {
    let column = column.to_ascii_lowercase();
    if column.ends_with("_no") || column.ends_with("_number") || column.ends_with("_code") {
        return false;
    }

    AUDIT_COLUMNS.contains(&column.as_str())
        || column == "id"
        || column.ends_with("_id")
        || column == "uuid"
        || column.ends_with("_uuid")
}

fn row_chunk_text(fields: &Map<String, Value>) -> String {
    // Audit fields and opaque IDs add noise, but remain in `fields` for filtering and citations.
    fields
        .iter()
        .filter(|(key, _)| !exclude_from_chunk_text(key))
        .filter_map(|(key, value)| {
            connectors::value_to_plain(value).map(|plain| format!("{key}: {plain}"))
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn patient_identity_line(fields: &Map<String, Value>) -> Option<String> {
    let names = ["first_name", "middle_name", "last_name"]
        .into_iter()
        .filter_map(|key| fields.get(key).and_then(Value::as_str))
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .collect::<Vec<_>>()
        .join(" ");
    let patient_no = fields
        .get("patient_no")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty());

    match (names.is_empty(), patient_no) {
        (false, Some(number)) => Some(format!("patient: {names} ({number})")),
        (false, None) => Some(format!("patient: {names}")),
        (true, Some(number)) => Some(format!("patient: {number}")),
        (true, None) => None,
    }
}

fn enrich_patient_row(
    fields: &mut Map<String, Value>,
    text: &str,
    identities: &HashMap<String, String>,
) -> Option<String> {
    let identity = fields
        .get("patient_id")
        .and_then(Value::as_str)
        .and_then(|patient_id| identities.get(patient_id))?
        .clone();
    fields.insert("patient_ref".to_string(), Value::String(identity.clone()));
    Some(format!("{identity}\n{text}"))
}

async fn load_patient_identities(
    conn: &dyn connectors::SourceConnector,
    schema: &[connectors::TableSchema],
    page_size: i64,
) -> AppResult<(HashMap<String, String>, bool)> {
    let Some(patient_schema) = schema.iter().find(|table| table.name == "patients") else {
        return Ok((HashMap::new(), false));
    };
    let Some(pk_column) = patient_schema
        .columns
        .iter()
        .find(|column| column.is_primary_key)
        .map(|column| column.name.as_str())
    else {
        return Ok((HashMap::new(), false));
    };
    if !patient_schema.columns.iter().any(|column| {
        matches!(
            column.name.as_str(),
            "patient_no" | "first_name" | "middle_name" | "last_name"
        )
    }) {
        return Ok((HashMap::new(), false));
    }

    let mut identities = HashMap::new();
    let mut offset = 0;
    loop {
        let rows = conn
            .fetch_table_page("patients", &[], Some(pk_column), offset, page_size)
            .await?;
        if rows.is_empty() {
            break;
        }
        let fetched = rows.len() as i64;
        for row in rows {
            let Some(patient_id) = row.fields.get(pk_column).and_then(Value::as_str) else {
                continue;
            };
            let Some(identity) = patient_identity_line(&row.fields) else {
                continue;
            };
            if !identities.contains_key(patient_id) && identities.len() >= MAX_PATIENT_IDENTITIES {
                return Ok((HashMap::new(), true));
            }
            identities.insert(patient_id.to_string(), identity);
        }
        offset += fetched;
        if fetched < page_size {
            break;
        }
    }
    Ok((identities, false))
}

#[derive(Clone, Default)]
pub struct IngestProgressHub {
    channels: Arc<Mutex<HashMap<String, broadcast::Sender<()>>>>,
}

impl IngestProgressHub {
    pub fn register(&self, job_id: &str) -> broadcast::Receiver<()> {
        let mut channels = self.channels.lock().expect("ingest progress lock poisoned");
        channels
            .entry(job_id.to_string())
            .or_insert_with(|| broadcast::channel(32).0)
            .subscribe()
    }

    pub fn subscribe(&self, job_id: &str) -> broadcast::Receiver<()> {
        self.register(job_id)
    }

    fn publish(&self, job_id: &str) {
        if let Ok(channels) = self.channels.lock() {
            if let Some(sender) = channels.get(job_id) {
                let _ = sender.send(());
            }
        }
    }

    fn remove(&self, job_id: &str) {
        if let Ok(mut channels) = self.channels.lock() {
            channels.remove(job_id);
        }
    }
}

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
    ingest_generation: String,
    active: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    extracted: Option<ExtractedClinical>,
    ingested_at: chrono::DateTime<Utc>,
}

/// One chunk queued for embedding, carrying enough context to build its `RecordDoc`.
struct Chunk {
    row_pk: String,
    chunk_index: i32,
    fields: Value,
    text: String,
    table: String,
    /// The row's annotation, cloned onto every chunk of that row exactly as `fields`
    /// is — a query that matches any chunk can then read it without a second lookup.
    extracted: Option<ExtractedClinical>,
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
    /// Rows the clinical extractor annotated so far. Always 0 when the extractor is off.
    extracted_rows: i64,
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
async fn write_snap(
    db: &DocumentDb,
    hub: &IngestProgressHub,
    job_id: &str,
    s: &Snap<'_>,
    log: &[LogEntry],
) -> AppResult<()> {
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
                "extracted_rows": s.extracted_rows,
                "log":           log_to_bson(log),
            }},
        )
        .await?;
    hub.publish(job_id);
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

async fn activate_generation(
    db: &DocumentDb,
    source_id: &str,
    table: &str,
    generation: &str,
    row_count: i64,
    vector_count: i64,
) -> AppResult<()> {
    let mut session = db.db.client().start_session().await?;
    session.start_transaction().await?;
    let result: AppResult<()> = async {
        db.records()
            .update_many(
                doc! { "source_id": source_id, "table": table, "active": true },
                doc! { "$set": { "active": false } },
            )
            .session(&mut session)
            .await?;
        db.records()
            .update_many(
                doc! { "source_id": source_id, "table": table, "ingest_generation": generation },
                doc! { "$set": { "active": true } },
            )
            .session(&mut session)
            .await?;
        db.indexed_tables()
            .update_one(
                doc! { "_id": format!("{source_id}:{table}") },
                doc! { "$set": {
                    "source_id": source_id,
                    "source_table": table,
                    "status": "indexed",
                    "refresh_status": "idle",
                    "row_count": row_count,
                    "vector_count": vector_count,
                    "active_generation": generation,
                    "last_ingested": BsonDateTime::now(),
                    "last_embedded_at": BsonDateTime::now(),
                } },
            )
            .upsert(true)
            .session(&mut session)
            .await?;
        Ok(())
    }
    .await;
    match result {
        Ok(()) => session.commit_transaction().await.map_err(Into::into),
        Err(error) => {
            let _ = session.abort_transaction().await;
            Err(error)
        }
    }
}

#[derive(Debug, Clone)]
pub struct ResumeCheckpoint {
    pub table: String,
    pub offset: i64,
    pub generation: String,
}

/// Background entry point: run the pipeline and record the outcome on the job doc.
/// Never panics the task — any error is written back as a failed job.
#[allow(clippy::too_many_arguments)]
/// Run one ingestion job to completion.
///
/// `catalog_handle` / `router_cache` are the live `AppState` handles: on success this
/// rebuilds the aggregation catalog itself. It used to be rebuilt only inside the
/// `/ingest/stream` SSE generator, so an ingest nobody watched to the end left the
/// running server with a stale (often empty) allow-list — every structured question
/// then failed validation with "ingest data first" against a fully ingested store,
/// until the next restart.
#[allow(clippy::too_many_arguments)]
pub async fn run(
    db: DocumentDb,
    config: Config,
    progress_hub: IngestProgressHub,
    catalog_handle: Arc<std::sync::RwLock<Arc<crate::aggregation::catalog::Catalog>>>,
    router_cache: Arc<crate::router::RouterCache>,
    binding_cache: crate::state::BindingCache,
    source_id: String,
    job_id: String,
    tables: Vec<String>,
    excluded_columns: HashMap<String, Vec<String>>,
    limit: Option<i64>,
    resume: Option<ResumeCheckpoint>,
    foundry: Option<Arc<FoundryManager>>,
    extract_spec: ModelSpec,
) {
    let extractor = Extractor::new(&config, foundry, extract_spec);
    let result = execute(
        &db,
        &config,
        &progress_hub,
        &source_id,
        &job_id,
        &tables,
        &excluded_columns,
        limit,
        resume.as_ref(),
        extractor.as_ref(),
    )
    .await;
    if let Err(e) = &result {
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
        progress_hub.publish(&job_id);
    } else {
        // Records are active now, so the allow-list can see them. Do this before
        // the terminal publish so a client that reacts to "completed" by asking a
        // question finds the new tables already accepted.
        let new_catalog = crate::aggregation::catalog::build_from_store(&db).await;
        if let Ok(mut writer) = catalog_handle.write() {
            *writer = Arc::new(new_catalog);
        }
        // A new schema can flip a cached semantic decision to structured.
        router_cache.clear();
        tracing::info!(job = %job_id, "catalog rebuilt after ingestion");
    }

    if result.is_ok() && config.router.text2sql_enabled {
        match connectors::routes::load_spec(&db, &config, &source_id).await {
            Ok(spec) => {
                if let Err(error) = crate::nl2sql::catalog::refresh_catalog_with_trigger(
                    &db,
                    &config,
                    &spec,
                    &source_id,
                    "ingestion",
                    &binding_cache,
                )
                .await
                {
                    tracing::warn!(job = %job_id, %source_id, %error, "post-ingestion schema metadata refresh failed");
                }
            }
            Err(error) => {
                tracing::warn!(job = %job_id, %source_id, %error, "could not load source for post-ingestion metadata refresh");
            }
        }
    }
    progress_hub.publish(&job_id);
    progress_hub.remove(&job_id);
}

/// The pipeline proper. Returns an error only for catastrophic failures that prevent
/// even starting (index creation, source load). Per-table errors are caught, logged,
/// and continue to the next table.
#[allow(clippy::too_many_arguments)]
async fn execute(
    db: &DocumentDb,
    config: &Config,
    progress_hub: &IngestProgressHub,
    source_id: &str,
    job_id: &str,
    tables: &[String],
    excluded_columns: &HashMap<String, Vec<String>>,
    limit: Option<i64>,
    resume: Option<&ResumeCheckpoint>,
    extractor: Option<&Extractor>,
) -> AppResult<()> {
    // 1. Ensure vector + full-text indexes exist before writing any records.
    vector::ensure_indexes(db, config.embedding_dims).await?;

    // 2. Load + decrypt the source spec.
    let spec = connectors::routes::load_spec(db, config, source_id).await?;
    let conn = connector(&spec);
    let schema = conn.get_schema().await?;

    let resume_table_index = resume
        .map(|checkpoint| {
            tables
                .iter()
                .position(|table| table == &checkpoint.table)
                .ok_or_else(|| {
                    AppError::BadRequest("checkpoint table is not in the saved request".into())
                })
        })
        .transpose()?;

    let total_tables = tables.len() as i64;
    let mut success_tables = resume_table_index.unwrap_or(0) as i64;
    let mut failed_tables = 0i64;
    let mut errors_count = 0i64;
    let mut processed_rows = 0i64;
    let mut total_rows_est = 0i64;
    let mut db_size_bytes = 0i64;
    let mut extracted_rows = 0i64;
    let mut log: Vec<LogEntry> = Vec::new();

    let records_coll = db.collection::<RecordDoc>(RECORDS);

    let patient_identities = match load_patient_identities(
        conn.as_ref(),
        &schema,
        config.ingest_page_size as i64,
    )
    .await
    {
        Ok((identities, false)) => identities,
        Ok((_, true)) => {
            let message = format!(
                "Patient identity enrichment disabled: source exceeds {MAX_PATIENT_IDENTITIES} patients"
            );
            tracing::warn!(job = %job_id, "{message}");
            push_log(&mut log, "warn", &message);
            HashMap::new()
        }
        Err(error) => {
            let message = format!("Patient identity enrichment unavailable: {error}");
            tracing::warn!(job = %job_id, "{message}");
            push_log(&mut log, "warn", &message);
            HashMap::new()
        }
    };
    if !patient_identities.is_empty() {
        tracing::info!(
            job = %job_id,
            patients = patient_identities.len(),
            "loaded patient identities for ingest enrichment"
        );
    }

    if let Some(ex) = extractor {
        push_log(
            &mut log,
            "info",
            &format!(
                "Clinical extractor enabled ({}): notes of {}+ words will be annotated with ICD-10 / RxNorm / LOINC codes",
                ex.alias(),
                config.router.extract_min_words
            ),
        );
    }

    // 3. Per-table loop: each table is an independent unit; one failure continues.
    for (table_idx, table) in tables.iter().enumerate() {
        if resume_table_index.is_some_and(|resume_index| table_idx < resume_index) {
            continue;
        }
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

        let table_id = format!("{source_id}:{table}");
        let previous_generation = db
            .indexed_tables()
            .find_one(doc! { "_id": &table_id })
            .await?
            .and_then(|document| {
                document
                    .get_str("active_generation")
                    .ok()
                    .map(str::to_owned)
            });
        if previous_generation.is_some() {
            let _ = db
                .indexed_tables()
                .update_one(
                    doc! { "_id": &table_id },
                    doc! { "$set": { "refresh_status": "indexing" } },
                )
                .await;
        } else {
            upsert_indexed_table(db, source_id, table, "indexing", 0, 0, false).await;
        }

        // Write a progress snapshot so the UI shows the current table name.
        let _ = write_snap(
            db,
            progress_hub,
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
                extracted_rows,
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

        let order_by = schema
            .iter()
            .find(|entry| entry.name == *table)
            .and_then(|entry| entry.columns.iter().find(|column| column.is_primary_key))
            .map(|column| column.name.as_str());
        let resumed_table = resume.filter(|checkpoint| checkpoint.table == *table);
        let generation = resumed_table
            .map(|checkpoint| checkpoint.generation.clone())
            .unwrap_or_else(|| uuid::Uuid::now_v7().to_string());
        let page_size = config.ingest_page_size as i64;
        let row_limit = limit.unwrap_or(i64::MAX).max(0).min(table_total);
        let mut offset = resumed_table
            .map(|checkpoint| checkpoint.offset.clamp(0, row_limit))
            .unwrap_or(0);
        let mut table_rows = offset;
        let mut vector_count = if resumed_table.is_some() {
            records_coll
                .count_documents(doc! {
                    "source_id": source_id,
                    "table": table,
                    "ingest_generation": &generation,
                    "active": false,
                })
                .await? as i64
        } else {
            0
        };
        let mut table_failed = false;

        db.jobs()
            .update_one(
                doc! { "_id": job_id },
                doc! { "$set": {
                    "checkpoint_table": table,
                    "checkpoint_offset": offset,
                    "checkpoint_generation": &generation,
                } },
            )
            .await?;

        'pages: while offset < row_limit {
            let requested = page_size.min(row_limit - offset);
            let rows = match conn
                .fetch_table_page(table, excluded, order_by, offset, requested)
                .await
            {
                Ok(rows) => rows,
                Err(e) => {
                    let msg = format!("fetch_table_page({table}, offset={offset}) failed: {e}");
                    tracing::error!(job = %job_id, "{}", msg);
                    push_log(&mut log, "error", &msg);
                    errors_count += 1;
                    table_failed = true;
                    break 'pages;
                }
            };
            if rows.is_empty() {
                break;
            }
            let fetched_rows = rows.len() as i64;
            // A crash can leave a partially inserted page. Replaying the checkpointed
            // page first removes only that generation's affected rows, making retry safe.
            let row_pks: Vec<String> = rows.iter().map(|row| row.pk.clone()).collect();
            let deleted = records_coll
                .delete_many(doc! {
                    "source_id": source_id,
                    "table": table,
                    "ingest_generation": &generation,
                    "active": false,
                    "row_pk": { "$in": &row_pks },
                })
                .await?;
            vector_count = vector_count.saturating_sub(deleted.deleted_count as i64);
            let mut annotations: HashMap<String, ExtractedClinical> = HashMap::new();
            if let Some(extractor) = extractor {
                let candidates: Vec<(String, String)> = rows
                    .iter()
                    .map(|row| (row.pk.clone(), row.text.clone()))
                    .collect();
                for batch in candidates.chunks(EXTRACT_BATCH) {
                    let result = extractor.extract_batch(batch.to_vec()).await;
                    extracted_rows += result.annotated.len() as i64;
                    annotations.extend(result.annotated);
                }
            }

            let mut chunks = Vec::new();
            for row in rows {
                let extracted = annotations.remove(&row.pk);
                let mut fields = row.fields;
                let projected_text = row_chunk_text(&fields);
                let enriched_text = (table != "patients")
                    .then(|| enrich_patient_row(&mut fields, &projected_text, &patient_identities))
                    .flatten();
                let text = enriched_text.as_deref().unwrap_or(&projected_text);
                let patient_ref = fields.get("patient_ref").and_then(Value::as_str);
                let mut texts = if config.chunk_enabled {
                    chunk_text(text, config.chunk_size_tokens, config.chunk_overlap_tokens)
                } else {
                    vec![text.to_string()]
                };
                if let Some(identity) = patient_ref {
                    for chunk in &mut texts {
                        if !chunk.starts_with(identity) {
                            *chunk = format!("{identity}\n{chunk}");
                        }
                    }
                }
                let fields = Value::Object(fields);
                for (ci, text) in texts.into_iter().enumerate() {
                    if !text.trim().is_empty() {
                        chunks.push(Chunk {
                            row_pk: row.pk.clone(),
                            chunk_index: ci as i32,
                            fields: fields.clone(),
                            text,
                            table: table.clone(),
                            extracted: extracted.clone(),
                        });
                    }
                }
            }

            for batch in chunks.chunks(config.ingest_embed_batch_size) {
                let texts: Vec<String> = batch.iter().map(|chunk| chunk.text.clone()).collect();
                let vectors = match embed::embed_documents(config, texts).await {
                    Ok(vectors) => vectors,
                    Err(e) => {
                        let msg = format!("embed failed for table {table}: {e}");
                        tracing::error!(job = %job_id, "{}", msg);
                        push_log(&mut log, "error", &msg);
                        errors_count += 1;
                        table_failed = true;
                        break 'pages;
                    }
                };
                let docs: Vec<RecordDoc> = batch
                    .iter()
                    .zip(vectors)
                    .map(|(chunk, vector)| RecordDoc {
                        id: format!(
                            "{source_id}:{}:{}:{}:{generation}",
                            chunk.table, chunk.row_pk, chunk.chunk_index
                        ),
                        source_id: source_id.to_string(),
                        table: chunk.table.clone(),
                        row_pk: chunk.row_pk.clone(),
                        chunk_index: chunk.chunk_index,
                        fields: chunk.fields.clone(),
                        text: chunk.text.clone(),
                        content_vector: vector,
                        ingest_generation: generation.clone(),
                        active: false,
                        extracted: chunk.extracted.clone(),
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
                        break 'pages;
                    }
                    db_size_bytes += batch
                        .iter()
                        .map(|chunk| chunk.text.len() as i64)
                        .sum::<i64>();
                    vector_count += docs.len() as i64;
                }
            }
            if table_failed {
                break;
            }

            offset += fetched_rows;
            table_rows += fetched_rows;
            processed_rows += fetched_rows;
            let _ = db
                .jobs()
                .update_one(
                    doc! { "_id": job_id },
                    doc! { "$set": { "checkpoint_table": table, "checkpoint_offset": offset } },
                )
                .await;

            if table_rows > 0 && table_rows % LOG_ROW_INTERVAL < page_size {
                push_log(
                    &mut log,
                    "info",
                    &format!("Table {table}: {table_rows} rows processed…"),
                );
            }
            let _ = write_snap(
                db,
                progress_hub,
                job_id,
                &Snap {
                    status: "running",
                    table_index,
                    total_tables,
                    current_table: table,
                    table_rows,
                    table_total: row_limit,
                    processed_rows,
                    total_rows: total_rows_est,
                    errors: errors_count,
                    success_tables,
                    failed_tables,
                    db_size_bytes,
                    extracted_rows,
                },
                &log,
            )
            .await;

            if fetched_rows < requested {
                break;
            }
        }

        // Update per-table outcome. A failed refresh never replaces the prior active generation.
        // Stop at the first failure so the durable checkpoint unambiguously identifies
        // the generation and page from which a resume must continue.
        let mut stop_after_table = false;
        if table_failed {
            failed_tables += 1;
            stop_after_table = true;
            if previous_generation.is_some() {
                let _ = db
                    .indexed_tables()
                    .update_one(
                        doc! { "_id": &table_id },
                        doc! { "$set": { "refresh_status": "error" } },
                    )
                    .await;
            } else {
                upsert_indexed_table(
                    db,
                    source_id,
                    table,
                    "error",
                    table_rows,
                    vector_count,
                    false,
                )
                .await;
            }
            tracing::error!(job = %job_id, table, "table ingestion failed");
        } else if let Err(error) =
            activate_generation(db, source_id, table, &generation, table_rows, vector_count).await
        {
            failed_tables += 1;
            errors_count += 1;
            stop_after_table = true;
            push_log(
                &mut log,
                "error",
                &format!("cutover failed for table {table}: {error}"),
            );
        } else {
            success_tables += 1;
            let _ = db
                .records()
                .delete_many(doc! {
                    "source_id": source_id,
                    "table": table,
                    "active": false,
                    "ingest_generation": { "$ne": &generation },
                })
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

        if stop_after_table {
            break;
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

    let final_level = if final_status == "completed" {
        "success"
    } else {
        "error"
    };
    push_log(
        &mut log,
        final_level,
        &format!("Ingestion {final_status}: {success_tables} succeeded, {failed_tables} failed"),
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
                "extracted_rows": extracted_rows,
                "log":           log_to_bson(&log),
                "finished_at":   BsonDateTime::now(),
            }},
        )
        .await?;
    progress_hub.publish(job_id);

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

#[cfg(test)]
mod tests {
    use super::{enrich_patient_row, patient_identity_line, row_chunk_text};
    use serde_json::{Map, Value, json};
    use std::collections::HashMap;

    fn object(value: Value) -> Map<String, Value> {
        value.as_object().expect("test value is an object").clone()
    }

    #[test]
    fn builds_patient_identity_without_middle_name() {
        let fields = object(json!({
            "patient_no": "SYN-2024-0001",
            "first_name": "Jane",
            "last_name": "Chebet"
        }));

        assert_eq!(
            patient_identity_line(&fields).as_deref(),
            Some("patient: Jane Chebet (SYN-2024-0001)")
        );
    }

    #[test]
    fn builds_patient_identity_with_middle_name() {
        let fields = object(json!({
            "patient_no": "SYN-2024-0001",
            "first_name": "Jane",
            "middle_name": "Wanjiku",
            "last_name": "Chebet"
        }));

        assert_eq!(
            patient_identity_line(&fields).as_deref(),
            Some("patient: Jane Wanjiku Chebet (SYN-2024-0001)")
        );
    }

    #[test]
    fn falls_back_to_patient_number_when_names_are_missing() {
        let fields = object(json!({"patient_no": "SYN-2024-0001"}));

        assert_eq!(
            patient_identity_line(&fields).as_deref(),
            Some("patient: SYN-2024-0001")
        );
    }

    #[test]
    fn prepends_patient_identity_and_adds_patient_ref_field() {
        let mut fields = object(json!({"patient_id": "patient-1", "icd10_code": "I25.1"}));
        let identities = HashMap::from([(
            "patient-1".to_string(),
            "patient: Jane Chebet (SYN-2024-0001)".to_string(),
        )]);

        let text = enrich_patient_row(&mut fields, "icd10_code: I25.1", &identities);

        assert_eq!(
            text.as_deref(),
            Some("patient: Jane Chebet (SYN-2024-0001)\nicd10_code: I25.1")
        );
        assert_eq!(
            fields.get("patient_ref").and_then(Value::as_str),
            Some("patient: Jane Chebet (SYN-2024-0001)")
        );
    }

    #[test]
    fn ignores_non_string_patient_id() {
        let mut fields = object(json!({"patient_id": 42, "icd10_code": "I25.1"}));
        let identities = HashMap::from([("42".to_string(), "patient: Jane Chebet".to_string())]);

        assert_eq!(
            enrich_patient_row(&mut fields, "icd10_code: I25.1", &identities),
            None
        );
        assert!(!fields.contains_key("patient_ref"));
    }

    #[test]
    fn excludes_audit_columns_from_chunk_text_but_keeps_clinical_dates_and_metadata() {
        let fields = object(json!({
            "encounter_date": "2023-01-23",
            "created_at": "2026-08-25T10:30:00Z",
            "UPDATED_AT": "2026-08-26T11:00:00Z",
            "diagnosis": "Appendicitis"
        }));

        let text = row_chunk_text(&fields);

        assert!(text.contains("encounter_date: 2023-01-23"));
        assert!(text.contains("diagnosis: Appendicitis"));
        assert!(!text.to_ascii_lowercase().contains("created_at"));
        assert!(!text.to_ascii_lowercase().contains("updated_at"));
        assert!(fields.contains_key("created_at"));
        assert!(fields.contains_key("UPDATED_AT"));
    }

    #[test]
    fn excludes_opaque_identifiers_from_chunk_text_but_keeps_business_ids_and_metadata() {
        let row_id = "72d9e10e-d76f-465b-9093-ee9a2f6661d5";
        let patient_id = "c21a8c53-5981-4cab-88ee-ee79ea28d721";
        let fields = object(json!({
            "id": row_id,
            "PATIENT_ID": patient_id,
            "allergen_id": 1,
            "uuid": "24b219a0-6a56-4732-9f4a-94971cc68024",
            "visit_uuid": "a2c6af54-09d7-4f75-bcf7-4c27c8c73aec",
            "patient_no": "SYN-2024-0001",
            "icd10_code": "T78.40XA",
            "encounter_date": "2023-01-23"
        }));

        let text = row_chunk_text(&fields);

        assert!(!text.contains(row_id));
        assert!(!text.contains(patient_id));
        assert!(!text.to_ascii_lowercase().contains("allergen_id"));
        assert!(!text.to_ascii_lowercase().contains("allergen id"));
        assert!(!text.contains("24b219a0-6a56-4732-9f4a-94971cc68024"));
        assert!(!text.contains("a2c6af54-09d7-4f75-bcf7-4c27c8c73aec"));
        assert!(text.contains("patient_no: SYN-2024-0001"));
        assert!(text.contains("icd10_code: T78.40XA"));
        assert!(text.contains("encounter_date: 2023-01-23"));
        assert_eq!(fields.get("id").and_then(Value::as_str), Some(row_id));
        assert_eq!(
            fields.get("PATIENT_ID").and_then(Value::as_str),
            Some(patient_id)
        );
        assert_eq!(fields.get("allergen_id").and_then(Value::as_i64), Some(1));
    }

    #[test]
    fn patient_identity_enrichment_survives_audit_column_filtering() {
        let mut fields = object(json!({
            "patient_id": "patient-1",
            "encounter_date": "2023-01-23",
            "created_at": "2026-08-25T10:30:00Z"
        }));
        let identities = HashMap::from([(
            "patient-1".to_string(),
            "patient: Jane Chebet (SYN-2024-0001)".to_string(),
        )]);

        let projected = row_chunk_text(&fields);
        let text = enrich_patient_row(&mut fields, &projected, &identities)
            .expect("patient identity should be available");

        assert!(text.starts_with("patient: Jane Chebet (SYN-2024-0001)\n"));
        assert!(text.contains("encounter_date: 2023-01-23"));
        assert!(!text.contains("created_at"));
        assert!(!text.contains("patient_id"));
        assert_eq!(
            fields.get("created_at").and_then(Value::as_str),
            Some("2026-08-25T10:30:00Z")
        );
        assert_eq!(
            fields.get("patient_ref").and_then(Value::as_str),
            Some("patient: Jane Chebet (SYN-2024-0001)")
        );
    }
}
