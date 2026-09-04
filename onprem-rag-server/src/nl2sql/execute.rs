use crate::config::Config;
use crate::connectors::{connector, routes::load_spec};
use crate::documentdb::DocumentDb;
use crate::error::{AppError, AppResult};

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
    let max_cost = config.router.nl2sql_max_plan_cost;
    if max_cost > 0.0 {
        match conn
            .estimate_cost(sql, config.router.nl2sql_timeout_secs)
            .await
        {
            Ok(Some(cost)) => {
                tracing::debug!(cost, max_cost, "nl2sql query plan cost estimated");
                if cost > max_cost {
                    return Err(AppError::BadRequest(format!(
                        "query rejected by cost pre-flight: estimated plan cost {cost:.0} exceeds limit {max_cost:.0}"
                    )));
                }
            }
            Ok(None) => {}
            Err(error) => {
                tracing::warn!(%error, "nl2sql cost pre-flight failed; continuing execution");
            }
        }
    }
    conn.run_select(
        sql,
        config.router.nl2sql_max_rows,
        config.router.nl2sql_timeout_secs,
    )
    .await
}
