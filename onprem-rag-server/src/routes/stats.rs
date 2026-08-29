//! `GET /stats` — the dashboard summary. One call backs the Dashboard stat grid so
//! the app doesn't fan out across several endpoints on load.
//!
//! Everything here is derived from data the server already holds:
//!   - `total_records`  — distinct source rows across the `records` collection
//!     (records are chunk-grained, so we group by `source_id + row_pk`).
//!   - `total_tables`   — distinct ingested sources (one source ≈ one table today;
//!     multi-table ingest lands in WS5, at which point this reads a real table field).
//!   - `last_ingest_at` — when the most recent ingest job finished.
//!   - `active_connections` — saved sources.
//!   - `pending_alerts` — outbreak alerts are a stub (reference has no backend), so 0.
//!   - `llm_status`     — whether Foundry Local came up on this server.

use chrono::{DateTime, Utc};
use futures::TryStreamExt;
use mongodb::bson::{doc, Document};
use rocket::serde::json::Json;
use rocket::{get, State};
use serde::Serialize;

use crate::auth::guard::AuthUser;
use crate::documentdb::{JOBS, RECORDS, SOURCES};
use crate::error::AppResult;
use crate::state::AppState;

/// Dashboard summary. Mirror any field change in BOTH `src-tauri/src/commands.rs`
/// (`DashboardStats`) and `src/lib/bridge.ts` (`DashboardStats`), or it is dropped.
#[derive(Debug, Serialize)]
pub struct StatsResponse {
    pub total_records: i64,
    pub total_tables: i64,
    pub last_ingest_at: Option<DateTime<Utc>>,
    pub active_connections: i64,
    pub pending_alerts: i64,
    /// "running" when Foundry Local is available on the server, else "stopped".
    pub llm_status: String,
}

/// `GET /stats` — dashboard summary. Any authenticated user.
#[get("/stats")]
pub async fn stats(state: &State<AppState>, _user: AuthUser) -> AppResult<Json<StatsResponse>> {
    let db = &state.db;

    // Saved connections — a plain count.
    let active_connections = db.collection::<Document>(SOURCES).count_documents(doc! {}).await? as i64;

    // Distinct rows: records are one-chunk-per-doc, so collapse by (source, row) first.
    let total_records = count_aggregate(
        db.collection::<Document>(RECORDS),
        vec![
            doc! { "$group": { "_id": { "s": "$source_id", "r": "$row_pk" } } },
            doc! { "$count": "n" },
        ],
    )
    .await?;

    // Distinct ingested sources ≈ tables indexed.
    let total_tables = count_aggregate(
        db.collection::<Document>(RECORDS),
        vec![
            doc! { "$group": { "_id": "$source_id" } },
            doc! { "$count": "n" },
        ],
    )
    .await?;

    // Most recent successful ingest. Status vocabulary is now "completed" or "partial"
    // (was "done" before Stage 4). Match both so old jobs still appear in the stat.
    let last_ingest_at = db
        .collection::<Document>(JOBS)
        .find_one(doc! { "status": { "$in": ["completed", "partial", "done"] } })
        .sort(doc! { "finished_at": -1 })
        .await?
        .and_then(|j| {
            j.get_datetime("finished_at")
                .ok()
                .and_then(|d| DateTime::<Utc>::from_timestamp_millis(d.timestamp_millis()))
        });

    // Outbreak alerts have no server-side source yet (stub in the reference too).
    let pending_alerts = 0;

    let llm_status = if state.foundry().is_ok() { "running" } else { "stopped" };

    Ok(Json(StatsResponse {
        total_records,
        total_tables,
        last_ingest_at,
        active_connections,
        pending_alerts,
        llm_status: llm_status.to_string(),
    }))
}

/// Run a `$count`-terminated aggregation and read back the count, defaulting to 0
/// when the pipeline yields nothing (empty collection → no `$count` document).
async fn count_aggregate(
    coll: mongodb::Collection<Document>,
    pipeline: Vec<Document>,
) -> AppResult<i64> {
    let docs: Vec<Document> = coll.aggregate(pipeline).await?.try_collect().await?;
    Ok(docs
        .first()
        .and_then(|d| d.get_i32("n").ok().map(|n| n as i64).or_else(|| d.get_i64("n").ok()))
        .unwrap_or(0))
}
