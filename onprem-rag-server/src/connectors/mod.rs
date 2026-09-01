//! Source-database connectors. A `SourceConnector` knows how to reach one kind of
//! operational SQL database (Postgres, MySQL, SQL Server), verify connectivity, and
//! stream rows for ingestion.
//!
//! WS4 delivers connection config + `test()`. WS5 added row-fetch; Stage 4 (Ingest
//! Wizard) adds schema introspection and per-table fetch with PII column exclusion.

pub mod mssql;
pub mod mysql;
pub mod postgres;
pub mod routes;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::error::{AppError, AppResult};

/// The supported source-database engines.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SourceKind {
    Postgres,
    Mysql,
    Mssql,
}

impl SourceKind {
    /// Conventional default port for this engine, used when the caller omits one.
    pub fn default_port(self) -> u16 {
        match self {
            SourceKind::Postgres => 5432,
            SourceKind::Mysql => 3306,
            SourceKind::Mssql => 1433,
        }
    }
}

/// Everything needed to connect to (and later query) a source database. The
/// password is held in plaintext only in memory; at rest it is encrypted.
#[derive(Debug, Clone)]
pub struct SourceSpec {
    pub kind: SourceKind,
    pub host: String,
    pub port: u16,
    pub database: String,
    pub username: String,
    pub password: String,
    /// Custom SQL to pull records. When absent, `table` is scanned.
    pub query: Option<String>,
    /// Table to scan when no custom `query` is given.
    pub table: Option<String>,
}

/// A foreign-key relationship from a column in this table to another table.
/// Used to expand schema cards in the nl2sql prompt (the join keys must be in context).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FkEdge {
    /// Column in this table that holds the foreign key.
    pub column: String,
    /// Referenced (parent) table.
    pub ref_table: String,
    /// Referenced column in the parent table (usually a PK).
    pub ref_column: String,
}

/// One column in a database table, as returned by `get_schema`.
/// `likely_pii` is always `false` here; `POST /schema/analyze` fills it in from the
/// deterministic keyword pass (and optionally the LLM pass).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ColumnSchema {
    pub name: String,
    /// Database type string (e.g. "integer", "varchar", "timestamp"). The field is
    /// named `type_` in Rust to avoid the keyword; serializes as `"type"`.
    #[serde(rename = "type")]
    pub type_: String,
    pub nullable: bool,
    pub is_primary_key: bool,
    pub is_foreign_key: bool,
    /// Always `false` from `get_schema`; the analyze route merges PII findings in.
    pub likely_pii: bool,
}

/// One table in a source database with column metadata and a fast row-count estimate.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TableSchema {
    pub name: String,
    /// Fast catalog estimate (pg reltuples / MySQL TABLE_ROWS / MSSQL sys.partitions),
    /// not an exact count. Use `count_table` at ingest time for the exact value.
    pub row_count: i64,
    pub columns: Vec<ColumnSchema>,
    /// FK relationships from this table to other tables. Populated only when the
    /// enhanced FK query succeeds (non-empty only after nl2sql catalog refresh).
    #[serde(default)]
    pub fk_edges: Vec<FkEdge>,
}

/// One row pulled from a source, ready for chunking + embedding.
#[derive(Debug, Clone)]
pub struct FetchedRow {
    /// Stable per-row identifier (a natural key column when present, else a row index).
    pub pk: String,
    /// The row's columns as JSON — kept on the record as structured metadata.
    pub fields: Map<String, Value>,
    /// Human-readable `key: value` projection of the row, embedded + full-text indexed.
    pub text: String,
}

/// A live connector for one source database.
#[async_trait]
pub trait SourceConnector: Send + Sync {
    /// Connect and run a trivial query to prove the credentials and reachability.
    async fn test(&self) -> AppResult<()>;

    /// Return all user tables with column metadata and fast row-count estimates.
    /// Partial results are preferred over total failure: if column introspection fails
    /// for a table, return it with `columns: []` rather than erroring the whole call.
    async fn get_schema(&self) -> AppResult<Vec<TableSchema>>;

    /// Count rows in a named table (exact, not an estimate). The caller is responsible
    /// for validating `table` against `get_schema()` before calling this to prevent
    /// SQL injection — `start_ingest` does this validation before spawning the job.
    async fn count_table(&self, table: &str) -> AppResult<i64>;

    /// Fetch one bounded page from a named table. `order_by` must come from the
    /// connector's introspected schema; callers must never pass client-provided SQL.
    /// Excluded columns are removed before projection so PII is neither embedded nor
    /// stored. The returned vector has at most `page_size` rows.
    async fn fetch_table_page(
        &self,
        table: &str,
        excluded: &[String],
        order_by: Option<&str>,
        offset: i64,
        page_size: i64,
    ) -> AppResult<Vec<FetchedRow>>;

    /// Execute a **pre-validated, read-only SELECT** and return `(column_names, rows)`.
    ///
    /// The caller MUST validate `sql` with `nl2sql::validate::validate_sql` before
    /// calling this — the connector enforces `max_rows` and `timeout_secs` but relies
    /// on the validation gate to reject DDL/DML. Each row is a `Vec<Value>` aligned to
    /// `column_names`. Returns `([], [])` when the query produces no rows.
    async fn run_select(
        &self,
        sql: &str,
        max_rows: i64,
        timeout_secs: u64,
    ) -> AppResult<(Vec<String>, Vec<Vec<serde_json::Value>>)>;
}

/// Build the connector for a spec's engine.
pub fn connector(spec: &SourceSpec) -> Box<dyn SourceConnector> {
    match spec.kind {
        SourceKind::Postgres => Box::new(postgres::PostgresConnector::new(spec.clone())),
        SourceKind::Mysql => Box::new(mysql::MysqlConnector::new(spec.clone())),
        SourceKind::Mssql => Box::new(mssql::MssqlConnector::new(spec.clone())),
    }
}

/// Map a source-connection failure to a clean 400 (bad credentials/host/etc.),
/// keeping the underlying detail for the operator via the message.
pub(crate) fn conn_err(context: &str, e: impl std::fmt::Display) -> AppError {
    AppError::BadRequest(format!("{context}: {e}"))
}

/// Candidate column names, in priority order, treated as a row's natural key.
const PK_CANDIDATES: [&str; 5] = ["id", "_id", "uuid", "pk", "row_id"];

/// Assemble a [`FetchedRow`] from a decoded row: pick a primary key, build the text
/// projection, and keep the fields. `index` is the 0-based row position (the pk fallback).
pub(crate) fn make_row(fields: Map<String, Value>, index: usize) -> FetchedRow {
    let pk = pick_pk(&fields, index);
    let text = project_text(&fields);
    FetchedRow { pk, fields, text }
}

/// Like `make_row` but drops `excluded` columns from the fields map *before*
/// `project_text`. Used by `fetch_table` so PII columns are neither embedded nor
/// stored on the resulting `RecordDoc`.
pub(crate) fn make_row_filtered(
    mut fields: Map<String, Value>,
    excluded: &[String],
    index: usize,
) -> FetchedRow {
    // Drop before project_text — excluded columns must not appear in the embedded text.
    for col in excluded {
        fields.remove(col.as_str());
    }
    make_row(fields, index)
}

/// Choose a stable primary key: the first present natural-key column (case-insensitive),
/// else a synthetic `row-{index}`.
fn pick_pk(fields: &Map<String, Value>, index: usize) -> String {
    for cand in PK_CANDIDATES {
        if let Some(v) = fields
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case(cand))
            .map(|(_, v)| v)
        {
            if let Some(s) = value_to_plain(v) {
                if !s.is_empty() {
                    return s;
                }
            }
        }
    }
    format!("row-{index}")
}

/// Render a row as a `key: value` block, one field per line, skipping nulls/empties.
/// This is what gets embedded and full-text indexed.
fn project_text(fields: &Map<String, Value>) -> String {
    fields
        .iter()
        .filter_map(|(k, v)| value_to_plain(v).map(|s| format!("{k}: {s}")))
        .collect::<Vec<_>>()
        .join("\n")
}

/// Flatten a JSON value to a plain string for projection. Nulls and empty strings
/// return `None` (dropped); arrays/objects are compact-serialised.
pub(crate) fn value_to_plain(v: &Value) -> Option<String> {
    match v {
        Value::Null => None,
        Value::String(s) if s.is_empty() => None,
        Value::String(s) => Some(s.clone()),
        Value::Bool(b) => Some(b.to_string()),
        Value::Number(n) => Some(n.to_string()),
        Value::Array(_) | Value::Object(_) => serde_json::to_string(v).ok(),
    }
}
