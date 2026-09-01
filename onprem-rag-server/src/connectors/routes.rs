//! `/sources` routes: list saved sources, test a connection, and save one
//! (credentials encrypted at rest). Managing sources is an admin task; listing is
//! available to any authenticated user so they can pick what to chat over.
//!
//! Also contains schema-introspection routes (`GET /sources/<id>/schema`,
//! `POST /schema/analyze`) used by the Stage 4 Ingest Wizard.

use std::collections::HashMap;

use chrono::{DateTime, Utc};
use mongodb::bson::doc;
use rocket::serde::json::Json;
use rocket::serde::json::{Value, json};
use rocket::{State, delete, get, patch, post};
use serde::{Deserialize, Serialize};

use super::{SourceKind, SourceSpec, TableSchema, connector};
use crate::auth::{audit, guard::AuthUser};
use crate::config::Config;
use crate::crypto::CredentialCipher;
use crate::documentdb::{DocumentDb, INDEXED_TABLES, SOURCES};
use crate::error::{AppError, AppResult};
use crate::state::AppState;

/// Stored source document. `password_enc` is AES-GCM ciphertext (base64).
#[derive(Debug, Serialize, Deserialize)]
struct SourceDoc {
    #[serde(rename = "_id")]
    id: String,
    name: String,
    kind: SourceKind,
    host: String,
    port: u16,
    database: String,
    username: String,
    password_enc: String,
    query: Option<String>,
    table: Option<String>,
    created_by: String,
    created_at: DateTime<Utc>,
    // Last-test outcome. We're a stateless server (no live pool), so "connected"
    // means "the most recent test succeeded", not that a socket is held open.
    // `#[serde(default)]` lets sources saved before this field existed deserialize.
    #[serde(default)]
    status: String, // "" (never tested) | "connected" | "error"
    #[serde(default)]
    last_connected: Option<DateTime<Utc>>,
    #[serde(default)]
    error: Option<String>,
}

/// Public view of a source — never carries the password.
#[derive(Debug, Serialize)]
pub struct SourceInfo {
    pub id: String,
    pub name: String,
    pub kind: SourceKind,
    pub host: String,
    pub port: u16,
    pub database: String,
    pub username: String,
    pub has_password: bool,
    pub query: Option<String>,
    pub table: Option<String>,
    pub created_at: DateTime<Utc>,
    /// "connected" | "error" | "disconnected" (never tested / config changed).
    pub status: String,
    pub last_connected: Option<DateTime<Utc>>,
    /// Human-readable failure reason from the last test, if it failed.
    pub error: Option<String>,
}

impl From<SourceDoc> for SourceInfo {
    fn from(d: SourceDoc) -> Self {
        SourceInfo {
            id: d.id,
            name: d.name,
            kind: d.kind,
            host: d.host,
            port: d.port,
            database: d.database,
            username: d.username,
            has_password: !d.password_enc.is_empty(),
            query: d.query,
            table: d.table,
            created_at: d.created_at,
            // Empty (legacy / untested) surfaces to the UI as "disconnected".
            status: if d.status.is_empty() {
                "disconnected".into()
            } else {
                d.status
            },
            last_connected: d.last_connected,
            error: d.error,
        }
    }
}

/// Incoming source definition (from the add/test form).
#[derive(Debug, Deserialize)]
pub struct SourceInput {
    pub name: String,
    pub kind: SourceKind,
    pub host: String,
    /// Optional — defaults to the engine's conventional port.
    pub port: Option<u16>,
    pub database: String,
    pub username: String,
    pub password: String,
    #[serde(default)]
    pub query: Option<String>,
    #[serde(default)]
    pub table: Option<String>,
}

impl SourceInput {
    fn into_spec(self) -> SourceSpec {
        let port = self.port.unwrap_or_else(|| self.kind.default_port());
        SourceSpec {
            kind: self.kind,
            host: self.host,
            port,
            database: self.database,
            username: self.username,
            password: self.password,
            query: self.query,
            table: self.table,
        }
    }
}

#[derive(Debug, Serialize)]
pub struct TestResult {
    pub ok: bool,
}

/// Load a saved source by id and decrypt its password into a usable [`SourceSpec`].
/// Used by ingestion (WS5) to connect to a previously-registered database.
pub(crate) async fn load_spec(db: &DocumentDb, config: &Config, id: &str) -> AppResult<SourceSpec> {
    let coll = db.collection::<SourceDoc>(SOURCES);
    let doc = coll
        .find_one(doc! { "_id": id })
        .await?
        .ok_or(AppError::NotFound)?;
    let password = CredentialCipher::from_config(config).decrypt(&doc.password_enc)?;
    Ok(SourceSpec {
        kind: doc.kind,
        host: doc.host,
        port: doc.port,
        database: doc.database,
        username: doc.username,
        password,
        query: doc.query,
        table: doc.table,
    })
}

/// Validate a source definition before we try to connect.
fn validate(input: &SourceInput) -> AppResult<()> {
    if input.name.trim().is_empty() {
        return Err(AppError::BadRequest("source name is required".into()));
    }
    if input.host.trim().is_empty() || input.database.trim().is_empty() {
        return Err(AppError::BadRequest(
            "host and database are required".into(),
        ));
    }
    Ok(())
}

/// Turn a raw driver/connection error into a human-readable message for the UI.
///
/// Ports the intent of the reference app's `translateNetworkCode` /
/// `toDbSchemaErrorMessage`: the Node driver surfaced OS codes (ECONNREFUSED …);
/// our sqlx/tiberius errors carry the equivalent phrases in their `Display`, so
/// we match on substrings. Anything unrecognised falls through to the raw detail
/// (already prefixed with context by `conn_err`) so operators keep the specifics.
pub(crate) fn humanize_conn_error(raw: &str) -> String {
    let l = raw.to_lowercase();
    if l.contains("connection refused") || l.contains("econnrefused") {
        "Connection refused — is the database running on that host and port?".into()
    } else if l.contains("timed out") || l.contains("timeout") || l.contains("etimedout") {
        "Connection timed out — check the host, port, and network.".into()
    } else if l.contains("password authentication failed")
        || l.contains("access denied")
        || l.contains("28p01")
        || l.contains("login failed")
        || l.contains("authentication failed")
    {
        "Authentication failed — check the username and password.".into()
    } else if l.contains("no such host")
        || l.contains("failed to lookup")
        || l.contains("name resolution")
        || l.contains("name or service not known")
        || l.contains("enotfound")
    {
        "Host not found — check the hostname.".into()
    } else if l.contains("does not exist")
        || l.contains("unknown database")
        || l.contains("cannot open database")
    {
        "Database not found — check the database name.".into()
    } else {
        raw.to_string()
    }
}

/// `GET /sources` — list saved sources (no secrets). Any authenticated user.
#[get("/sources")]
pub async fn list_sources(
    state: &State<AppState>,
    _user: AuthUser,
) -> AppResult<Json<Vec<SourceInfo>>> {
    use futures::TryStreamExt;

    let coll = state.db.collection::<SourceDoc>(SOURCES);
    let docs: Vec<SourceDoc> = coll
        .find(doc! {})
        .sort(doc! { "created_at": -1, "_id": -1 })
        .await?
        .try_collect()
        .await?;
    Ok(Json(docs.into_iter().map(SourceInfo::from).collect()))
}

/// `POST /sources/test` — connect and run a trivial query without saving. Admin only.
#[post("/sources/test", data = "<body>")]
pub async fn test_source(user: AuthUser, body: Json<SourceInput>) -> AppResult<Json<TestResult>> {
    user.require_admin()?;
    let input = body.into_inner();
    validate(&input)?;
    if let Err(e) = connector(&input.into_spec()).test().await {
        return Err(AppError::BadRequest(humanize_conn_error(&e.to_string())));
    }
    Ok(Json(TestResult { ok: true }))
}

/// `POST /sources` — test, then save the source with its password encrypted at rest. Admin only.
#[post("/sources", data = "<body>")]
pub async fn create_source(
    state: &State<AppState>,
    user: AuthUser,
    body: Json<SourceInput>,
) -> AppResult<Json<SourceInfo>> {
    user.require_admin()?;
    let input = body.into_inner();
    validate(&input)?;

    let name = input.name.trim().to_string();
    let coll = state.db.collection::<SourceDoc>(SOURCES);
    if coll.find_one(doc! { "name": &name }).await?.is_some() {
        return Err(AppError::BadRequest(format!(
            "a source named '{name}' already exists"
        )));
    }

    let spec = input.into_spec();
    // Reject bad credentials up front so we never persist a source that can't connect.
    if let Err(e) = connector(&spec).test().await {
        return Err(AppError::BadRequest(humanize_conn_error(&e.to_string())));
    }

    let password_enc = CredentialCipher::from_config(&state.config).encrypt(&spec.password)?;
    let now = Utc::now();
    let source = SourceDoc {
        id: uuid::Uuid::now_v7().to_string(),
        name,
        kind: spec.kind,
        host: spec.host,
        port: spec.port,
        database: spec.database,
        username: spec.username,
        password_enc,
        query: spec.query,
        table: spec.table,
        created_by: user.id,
        created_at: now,
        // The pre-save test just passed, so record it as connected.
        status: "connected".into(),
        last_connected: Some(now),
        error: None,
    };
    coll.insert_one(&source).await?;
    // user.id was moved into source.created_by when building the struct; use the
    // stored copy rather than contorting the handler to clone it earlier.
    audit::write_audit(
        &state.db,
        &source.created_by,
        &user.username,
        "source_created",
        &source.id,
        Some(json!({ "name": source.name, "kind": format!("{:?}", source.kind) })),
    )
    .await;
    Ok(Json(SourceInfo::from(source)))
}

/// Update fields the admin can edit. A blank/omitted password keeps the existing
/// one (matches the reference edit modal). Changing connection params clears the
/// last-test status — the source needs re-testing before it reads "connected".
#[derive(Debug, Deserialize)]
pub struct SourceUpdate {
    pub name: Option<String>,
    pub kind: Option<SourceKind>,
    pub host: Option<String>,
    pub port: Option<u16>,
    pub database: Option<String>,
    pub username: Option<String>,
    /// Blank or omitted → keep the stored password.
    pub password: Option<String>,
    #[serde(default)]
    pub query: Option<String>,
    #[serde(default)]
    pub table: Option<String>,
}

/// `PATCH /sources/<id>` — edit a saved source. Admin only.
#[patch("/sources/<id>", data = "<body>")]
pub async fn update_source(
    state: &State<AppState>,
    user: AuthUser,
    id: &str,
    body: Json<SourceUpdate>,
) -> AppResult<Json<SourceInfo>> {
    user.require_admin()?;
    let coll = state.db.collection::<SourceDoc>(SOURCES);
    let mut doc = coll
        .find_one(doc! { "_id": id })
        .await?
        .ok_or(AppError::NotFound)?;
    let upd = body.into_inner();

    if let Some(n) = upd.name {
        let n = n.trim().to_string();
        if n.is_empty() {
            return Err(AppError::BadRequest("source name is required".into()));
        }
        // Guard against colliding with a different source's name.
        if let Some(other) = coll.find_one(doc! { "name": &n }).await? {
            if other.id != doc.id {
                return Err(AppError::BadRequest(format!(
                    "a source named '{n}' already exists"
                )));
            }
        }
        doc.name = n;
    }
    if let Some(k) = upd.kind {
        doc.kind = k;
    }
    if let Some(h) = upd.host {
        doc.host = h;
    }
    if let Some(p) = upd.port {
        doc.port = p;
    }
    if let Some(db) = upd.database {
        doc.database = db;
    }
    if let Some(u) = upd.username {
        doc.username = u;
    }
    if let Some(q) = upd.query {
        doc.query = Some(q);
    }
    if let Some(t) = upd.table {
        doc.table = Some(t);
    }
    if let Some(pw) = upd.password.filter(|s| !s.is_empty()) {
        doc.password_enc = CredentialCipher::from_config(&state.config).encrypt(&pw)?;
    }

    // Connection params may have changed — invalidate the last-test result.
    doc.status = "disconnected".into();
    doc.last_connected = None;
    doc.error = None;

    coll.replace_one(doc! { "_id": id }, &doc).await?;
    audit::write_audit(
        &state.db,
        &user.id,
        &user.username,
        "source_updated",
        id,
        None,
    )
    .await;
    Ok(Json(SourceInfo::from(doc)))
}

/// `POST /sources/<id>/test` — re-test a saved source and persist the outcome. Admin only.
/// Replaces the reference's Connect/Disconnect pool dance: we have no live pool,
/// so "connect" just means running the test and recording the result.
#[post("/sources/<id>/test")]
pub async fn test_saved_source(
    state: &State<AppState>,
    user: AuthUser,
    id: &str,
) -> AppResult<Json<SourceInfo>> {
    user.require_admin()?;
    let coll = state.db.collection::<SourceDoc>(SOURCES);
    let mut doc = coll
        .find_one(doc! { "_id": id })
        .await?
        .ok_or(AppError::NotFound)?;
    let spec = load_spec(&state.db, &state.config, id).await?;

    match connector(&spec).test().await {
        Ok(()) => {
            doc.status = "connected".into();
            doc.last_connected = Some(Utc::now());
            doc.error = None;
        }
        Err(e) => {
            doc.status = "error".into();
            doc.error = Some(humanize_conn_error(&e.to_string()));
        }
    }

    coll.replace_one(doc! { "_id": id }, &doc).await?;
    Ok(Json(SourceInfo::from(doc)))
}

/// `DELETE /sources/<id>` — remove a source and its ingested records. Admin only.
#[delete("/sources/<id>")]
pub async fn delete_source(
    state: &State<AppState>,
    user: AuthUser,
    id: &str,
) -> AppResult<Json<Value>> {
    user.require_admin()?;
    let coll = state.db.collection::<SourceDoc>(SOURCES);
    let res = coll.delete_one(doc! { "_id": id }).await?;
    if res.deleted_count == 0 {
        return Err(AppError::NotFound);
    }
    // Cascade: drop this source's ingested records so we don't leave orphaned
    // chunks + vectors behind pointing at a source that no longer exists.
    state
        .db
        .collection::<mongodb::bson::Document>(crate::documentdb::RECORDS)
        .delete_many(doc! { "source_id": id })
        .await?;
    // Also clear indexed_tables entries for this source.
    state
        .db
        .collection::<mongodb::bson::Document>(INDEXED_TABLES)
        .delete_many(doc! { "source_id": id })
        .await?;
    audit::write_audit(
        &state.db,
        &user.id,
        &user.username,
        "source_deleted",
        id,
        None,
    )
    .await;
    Ok(Json(json!({ "ok": true })))
}

// ---------------------------------------------------------------------------
// Schema introspection routes (Stage 4 Ingest Wizard)
// ---------------------------------------------------------------------------

/// `GET /sources/<id>/schema` — return the table + column schema for a saved source.
/// Any authenticated user may call this (read-only, no credentials exposed).
#[get("/sources/<id>/schema")]
pub async fn get_source_schema(
    state: &State<AppState>,
    _user: AuthUser,
    id: &str,
) -> AppResult<Json<Vec<TableSchema>>> {
    let spec = load_spec(&state.db, &state.config, id).await?;
    let schema = connector(&spec).get_schema().await?;
    Ok(Json(schema))
}

// ---------------------------------------------------------------------------
// Schema analysis types + route
// ---------------------------------------------------------------------------

/// Request body for `POST /schema/analyze`.
#[derive(Debug, Deserialize)]
pub struct AnalyzeRequest {
    pub tables: Vec<TableSchema>,
}

/// Result of the schema analysis: deterministic PII keyword pass always runs;
/// optional LLM pass enriches summary + quality notes when Foundry is available.
#[derive(Debug, Serialize)]
pub struct SchemaAnalysis {
    pub summary: String,
    pub suggested_tables: Vec<String>,
    /// table_name → column names likely containing PII.
    pub pii_columns: HashMap<String, Vec<String>>,
    pub data_quality_notes: Vec<String>,
}

/// Column-name substrings that indicate PII in health records. Matched
/// case-insensitively against each column name.
const PII_KEYWORDS: &[&str] = &[
    "name",
    "first_name",
    "last_name",
    "dob",
    "birth",
    "ssn",
    "social",
    "mrn",
    "patient",
    "email",
    "phone",
    "address",
    "zip",
    "postal",
    "gender",
    "sex",
    "race",
    "ethnicity",
    "insurance",
    "policy",
    "guarantor",
    "next_of_kin",
    "contact",
    "license",
    "passport",
    "national_id",
    "account",
];

/// Deterministic PII check for a single column name: case-insensitive substring match
/// against [`PII_KEYWORDS`]. Single source of truth — used by `POST /schema/analyze`
/// and the Data Explorer row-derived profile (`GET /tables/<id>/info`).
pub(crate) fn is_likely_pii(column_name: &str) -> bool {
    let lower = column_name.to_ascii_lowercase();
    PII_KEYWORDS.iter().any(|kw| lower.contains(kw))
}

/// `POST /schema/analyze` — deterministic PII keyword pass (always) + optional LLM
/// pass (when Foundry is available). Never hard-fails: analysis is advisory.
/// Any authenticated user.
#[post("/schema/analyze", data = "<body>")]
pub async fn analyze_schema(
    state: &State<AppState>,
    _user: AuthUser,
    body: Json<AnalyzeRequest>,
) -> AppResult<Json<SchemaAnalysis>> {
    let tables = body.into_inner().tables;

    // --- Deterministic PII keyword pass (always runs, model-free) ---
    let mut pii_columns: HashMap<String, Vec<String>> = HashMap::new();
    let mut suggested_tables: Vec<String> = Vec::new();

    for table in &tables {
        let mut pii_cols: Vec<String> = Vec::new();
        let mut has_text_col = false;

        for col in &table.columns {
            if is_likely_pii(&col.name) {
                pii_cols.push(col.name.clone());
            }
            // Heuristic: a column with a text-like type suggests the table has
            // embeddable content worth ingesting.
            let type_lower = col.type_.to_ascii_lowercase();
            if type_lower.contains("char")
                || type_lower.contains("text")
                || type_lower.contains("clob")
                || type_lower.contains("blob")
            {
                has_text_col = true;
            }
        }

        if !pii_cols.is_empty() {
            pii_columns.insert(table.name.clone(), pii_cols);
        }
        // Suggest a table if it has a text/clinical column or detected PII (clinical data).
        if has_text_col || pii_columns.contains_key(&table.name) {
            suggested_tables.push(table.name.clone());
        }
    }

    // Build a deterministic summary to use as the fallback if the LLM pass fails.
    let n_tables = tables.len();
    let n_pii_tables = pii_columns.len();
    let det_summary = format!(
        "Schema contains {n_tables} table(s); {n_pii_tables} table(s) have columns that \
         may contain protected health information."
    );

    let mut summary = det_summary.clone();
    let mut data_quality_notes: Vec<String> = Vec::new();

    // --- Optional LLM pass (never hard-fails) ---
    if let Ok(foundry) = state.foundry() {
        let table_desc: String = tables
            .iter()
            .map(|t| {
                let cols: Vec<String> = t
                    .columns
                    .iter()
                    .map(|c| format!("  - {} ({})", c.name, c.type_))
                    .collect();
                format!("Table: {}\nColumns:\n{}", t.name, cols.join("\n"))
            })
            .collect::<Vec<_>>()
            .join("\n\n");

        let system = "You are a medical database schema analyst. Analyze the provided table \
                      schemas and respond ONLY with valid JSON matching this exact structure, \
                      no markdown fences:\n\
                      {\"summary\": \"...\", \
                       \"suggested_tables\": [\"table1\", ...], \
                       \"pii_columns\": {\"table_name\": [\"col1\", ...]}, \
                       \"data_quality_notes\": [\"...\", ...]}";
        let user = format!(
            "Analyze this health-records database schema and identify PII, suggested tables \
             for ingestion, and data quality issues:\n\n{table_desc}"
        );

        if let Ok(raw) = foundry.complete(system, &user).await {
            // Parse the LLM JSON response; fall back to deterministic on any failure.
            if let Ok(parsed) = serde_json::from_str::<serde_json::Value>(&raw) {
                if let Some(s) = parsed.get("summary").and_then(|v| v.as_str()) {
                    if !s.is_empty() {
                        summary = s.to_string();
                    }
                }
                // Merge LLM-detected PII columns on top of the deterministic pass.
                if let Some(pii_map) = parsed.get("pii_columns").and_then(|v| v.as_object()) {
                    for (table, cols) in pii_map {
                        if let Some(col_arr) = cols.as_array() {
                            let llm_cols: Vec<String> = col_arr
                                .iter()
                                .filter_map(|c| c.as_str().map(str::to_string))
                                .collect();
                            pii_columns
                                .entry(table.clone())
                                .or_default()
                                .extend(llm_cols);
                        }
                    }
                }
                // LLM-suggested tables (may differ from the keyword pass).
                if let Some(sug_arr) = parsed.get("suggested_tables").and_then(|v| v.as_array()) {
                    let llm_suggested: Vec<String> = sug_arr
                        .iter()
                        .filter_map(|v| v.as_str().map(str::to_string))
                        .collect();
                    // Union: keep deterministic suggestions plus any new LLM ones.
                    for t in llm_suggested {
                        if !suggested_tables.contains(&t) {
                            suggested_tables.push(t);
                        }
                    }
                }
                if let Some(notes) = parsed.get("data_quality_notes").and_then(|v| v.as_array()) {
                    data_quality_notes = notes
                        .iter()
                        .filter_map(|v| v.as_str().map(str::to_string))
                        .collect();
                }
            }
            // If parsing failed, the deterministic values remain — silently degrade.
        }
    }

    // Deduplicate PII column lists (deterministic + LLM may overlap).
    for cols in pii_columns.values_mut() {
        cols.sort();
        cols.dedup();
    }

    Ok(Json(SchemaAnalysis {
        summary,
        suggested_tables,
        pii_columns,
        data_quality_notes,
    }))
}
