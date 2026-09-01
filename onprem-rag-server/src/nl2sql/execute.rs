use crate::config::Config;
use crate::connectors::{connector, routes::load_spec};
use crate::documentdb::DocumentDb;
use crate::error::AppResult;

/// Load the source spec, build a connector, and run a pre-validated SELECT.
/// The SQL must have already been approved by `validate::validate_sql` before
/// this call. Returns `(column_names, rows)`.
pub async fn run_select(
    db: &DocumentDb,
    config: &Config,
    source_id: &str,
    sql: &str,
) -> AppResult<(Vec<String>, Vec<Vec<serde_json::Value>>)> {
    let spec = load_spec(db, config, source_id).await?;
    let conn = connector(&spec);
    conn.run_select(
        sql,
        config.router.nl2sql_max_rows,
        config.router.nl2sql_timeout_secs,
    )
    .await
}
