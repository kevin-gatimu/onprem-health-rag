//! Admin-only audit log read route: paginated, filterable, newest-first browse
//! of the `audit_log` collection written by `auth::audit::write_audit`.
//!
//! Mirrors the structure and idiom of `routes/explorer.rs` end-to-end: `$facet`
//! for page + count in one round-trip, `AuthUser` guard, `read_count` helper,
//! snake_case throughout.

use chrono::NaiveDate;
use futures::TryStreamExt;
use mongodb::bson::{Bson, DateTime as BsonDateTime, Document, doc};
use rocket::serde::json::Json;
use rocket::{State, get};
use serde::Serialize;
use serde_json::Value;

use crate::auth::guard::AuthUser;
use crate::error::{AppError, AppResult};
use crate::state::AppState;

/// Allowed page sizes for the audit log. Audit rows are compact, so the default
/// is larger than the data-explorer default (50 vs 25).
const ALLOWED_PAGE_SIZES: [i64; 3] = [25, 50, 100];
const DEFAULT_PAGE_SIZE: i64 = 50;

/// One entry returned by `GET /audit`. `_id` is projected as a hex string via a
/// `$toString` stage — it is used only as a stable React key; there are no
/// per-row actions on the read-only log.
#[derive(Debug, Serialize)]
pub struct AuditRow {
    /// ObjectId hex string (from `$toString:"$_id"`).
    pub id: String,
    pub user_id: String,
    pub username: String,
    /// Short action tag, e.g. `"login"`, `"source_created"`, `"ingest_started"`.
    pub action: String,
    /// What the action targeted — a user id, resource id, or endpoint path.
    pub resource: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub details: Option<Value>,
    /// RFC3339 timestamp string.
    pub timestamp: String,
}

/// Paginated response for `GET /audit`.
#[derive(Debug, Serialize)]
pub struct AuditPage {
    pub entries: Vec<AuditRow>,
    pub total: i64,
    pub page: i64,
    pub page_size: i64,
    pub page_count: i64,
    pub has_prev: bool,
    pub has_next: bool,
}

/// `GET /audit?<user>&<action>&<from>&<to>&<page>&<page_size>` — paginated,
/// newest-first browse of the audit log. **Admin only.**
///
/// Filters:
/// - `user`   — case-insensitive substring on `username`.
/// - `action` — exact match on the action tag (e.g. `"login_failed"`).
/// - `from` / `to` — `YYYY-MM-DD` or RFC3339; `from` → `$gte`, a bare-date
///   `to` is advanced to the next day for `$lt` (end-of-day inclusive in UTC),
///   a timestamped `to` → `$lte`. Bad date strings return 400.
///
/// Page sizes clamp to `{25, 50, 100}` (default 50).
#[get("/audit?<user>&<action>&<from>&<to>&<page>&<page_size>")]
pub async fn list_audit(
    state: &State<AppState>,
    admin: AuthUser,
    user: Option<String>,
    action: Option<String>,
    from: Option<String>,
    to: Option<String>,
    page: Option<i64>,
    page_size: Option<i64>,
) -> AppResult<Json<AuditPage>> {
    // Admin-only: non-admins get 403 immediately.
    admin.require_admin()?;

    let page = page.unwrap_or(1).max(1);
    let page_size = page_size
        .filter(|ps| ALLOWED_PAGE_SIZES.contains(ps))
        .unwrap_or(DEFAULT_PAGE_SIZE);
    let skip = (page - 1) * page_size;

    // Build the $match document from whichever filters are provided.
    let mut match_doc = doc! {};

    if let Some(ref u) = user {
        let v = u.trim();
        if !v.is_empty() {
            // Case-insensitive substring: admins type partial names, not exact ids.
            match_doc.insert("username", doc! { "$regex": v, "$options": "i" });
        }
    }

    if let Some(ref a) = action {
        let v = a.trim();
        if !v.is_empty() {
            match_doc.insert("action", v.to_string());
        }
    }

    // Date bounds: accumulate into a `timestamp` sub-document then insert once.
    let mut ts_doc = doc! {};
    if let Some(ref from_str) = from {
        // from is always $gte; is_lt is irrelevant for the lower bound.
        let (bdt, _) = parse_bound(from_str, false)?;
        ts_doc.insert("$gte", bdt);
    }
    if let Some(ref to_str) = to {
        let (bdt, is_lt) = parse_bound(to_str, true)?;
        if is_lt {
            // Bare date to → advanced to next-day midnight, paired with $lt so
            // the full calendar day is included.
            ts_doc.insert("$lt", bdt);
        } else {
            // RFC3339 to → exact timestamp, paired with $lte.
            ts_doc.insert("$lte", bdt);
        }
    }
    if !ts_doc.is_empty() {
        match_doc.insert("timestamp", ts_doc);
    }

    // Aggregation: match → sort newest-first → $facet for page slice + total
    // count in one round-trip.  The $addFields stage inside the rows pipeline
    // projects _id as a hex string so the bridge/client never parse an ObjectId.
    let pipeline = vec![
        doc! { "$match": match_doc },
        doc! { "$sort": { "timestamp": -1 } },
        doc! { "$facet": {
            "rows": [
                { "$skip": skip },
                { "$limit": page_size },
                { "$addFields": { "_id": { "$toString": "$_id" } } },
            ],
            "total": [ { "$count": "n" } ],
        }},
    ];

    let docs: Vec<Document> = state
        .db
        .audit_log()
        .aggregate(pipeline)
        .await?
        .try_collect()
        .await?;

    // $facet always yields exactly one document: { rows: [...], total: [{ n }] }.
    let facet = docs.into_iter().next().unwrap_or_default();
    let total = facet
        .get_array("total")
        .ok()
        .and_then(|a| a.first())
        .and_then(Bson::as_document)
        .map(read_count)
        .unwrap_or(0);

    let mut entries = Vec::new();
    if let Ok(arr) = facet.get_array("rows") {
        for entry in arr {
            let Some(row) = entry.as_document() else {
                continue;
            };
            let id = row.get_str("_id").unwrap_or("").to_string();
            let user_id = row.get_str("user_id").unwrap_or("").to_string();
            let username = row.get_str("username").unwrap_or("").to_string();
            let action = row.get_str("action").unwrap_or("").to_string();
            let resource = row.get_str("resource").unwrap_or("").to_string();
            // Convert the details BSON value to a JSON Value when present and non-null.
            let details = row.get("details").and_then(|b| match b {
                Bson::Null => None,
                other => Some(other.clone().into_relaxed_extjson()),
            });
            // Convert the BSON timestamp (milliseconds) to an RFC3339 string via
            // chrono, matching the pattern used elsewhere in the codebase.
            let timestamp = row
                .get_datetime("timestamp")
                .ok()
                .and_then(|dt| {
                    chrono::DateTime::<chrono::Utc>::from_timestamp_millis(dt.timestamp_millis())
                        .map(|ts| ts.to_rfc3339())
                })
                .unwrap_or_default();
            entries.push(AuditRow {
                id,
                user_id,
                username,
                action,
                resource,
                details,
                timestamp,
            });
        }
    }

    let page_count = if total == 0 {
        1
    } else {
        (total + page_size - 1) / page_size
    };

    Ok(Json(AuditPage {
        entries,
        total,
        page,
        page_size,
        page_count,
        has_prev: page > 1,
        has_next: page < page_count,
    }))
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Parse a date bound as either full RFC3339 (`2026-08-25T13:00:00Z`) or a bare
/// `YYYY-MM-DD`. Returns the `BsonDateTime` and a flag indicating whether the
/// caller should pair it with `$lt` (`true`) or `$lte` / `$gte` (`false`).
///
/// When `end_inclusive` is `true` (the `to` bound):
/// - A bare date advances to the start of the next day so the entire calendar
///   day is included when the caller uses `$lt`.
/// - A timestamped value is returned as-is for use with `$lte`.
fn parse_bound(s: &str, end_inclusive: bool) -> Result<(BsonDateTime, bool), AppError> {
    // RFC3339 is tried first; a full timestamp is authoritative regardless of direction.
    // timestamp_millis() is timezone-independent, so no Utc conversion is needed.
    if let Ok(dt) = chrono::DateTime::parse_from_rfc3339(s) {
        let bdt = BsonDateTime::from_millis(dt.timestamp_millis());
        // RFC3339 value → exact bound; is_lt=false so caller uses $lte/$gte.
        return Ok((bdt, false));
    }

    // Fall back to a bare date (e.g. "2026-08-25").
    match NaiveDate::parse_from_str(s, "%Y-%m-%d") {
        Ok(nd) => {
            if end_inclusive {
                // Advance to next-day midnight so the caller can use `$lt` and
                // include the whole calendar day in UTC.
                let next = nd
                    .succ_opt()
                    .ok_or_else(|| AppError::BadRequest(format!("date out of range: {s}")))?;
                let dt = next
                    .and_hms_opt(0, 0, 0)
                    .expect("midnight 00:00:00 is always valid")
                    .and_utc();
                Ok((BsonDateTime::from_millis(dt.timestamp_millis()), true))
            } else {
                // Start of the given day at midnight UTC.
                let dt = nd
                    .and_hms_opt(0, 0, 0)
                    .expect("midnight 00:00:00 is always valid")
                    .and_utc();
                Ok((BsonDateTime::from_millis(dt.timestamp_millis()), false))
            }
        }
        Err(_) => Err(AppError::BadRequest(format!(
            "invalid date: {s} (use YYYY-MM-DD or RFC3339)"
        ))),
    }
}

/// Read a `$count` result document's `n`, tolerating both i32 and i64 encodings.
/// DocumentDB may return either depending on collection size.
fn read_count(d: &Document) -> i64 {
    d.get_i32("n")
        .map(|n| n as i64)
        .or_else(|_| d.get_i64("n"))
        .unwrap_or(0)
}
