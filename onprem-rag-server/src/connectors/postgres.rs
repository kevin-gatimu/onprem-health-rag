//! PostgreSQL connector (sqlx). Uses `PgConnectOptions` rather than a URL so
//! passwords with special characters need no escaping.

use std::collections::{HashMap, HashSet};
use std::time::Duration;

use async_trait::async_trait;
use sqlx::Row;
use sqlx::postgres::{PgConnectOptions, PgPoolOptions};

use super::{
    ColumnSchema, FetchedRow, SourceConnector, SourceSpec, TableSchema, conn_err, make_row_filtered,
};
use crate::error::{AppError, AppResult};

pub struct PostgresConnector {
    spec: SourceSpec,
}

impl PostgresConnector {
    pub fn new(spec: SourceSpec) -> Self {
        PostgresConnector { spec }
    }

    fn options(&self) -> PgConnectOptions {
        PgConnectOptions::new()
            .host(&self.spec.host)
            .port(self.spec.port)
            .username(&self.spec.username)
            .password(&self.spec.password)
            .database(&self.spec.database)
    }

    /// Open a short-lived pool for a single operation and close it afterwards.
    async fn pool(&self, max: u32) -> AppResult<sqlx::PgPool> {
        PgPoolOptions::new()
            .max_connections(max)
            .acquire_timeout(Duration::from_secs(15))
            .connect_with(self.options())
            .await
            .map_err(|e| conn_err("PostgreSQL connection failed", e))
    }
}

#[async_trait]
impl SourceConnector for PostgresConnector {
    async fn test(&self) -> AppResult<()> {
        // A single short-lived connection with a bounded acquire timeout — a
        // test must fail fast on an unreachable host rather than hang.
        let pool = PgPoolOptions::new()
            .max_connections(1)
            .acquire_timeout(Duration::from_secs(8))
            .connect_with(self.options())
            .await
            .map_err(|e| conn_err("PostgreSQL connection failed", e))?;

        sqlx::query("SELECT 1")
            .execute(&pool)
            .await
            .map_err(|e| conn_err("PostgreSQL test query failed", e))?;

        pool.close().await;
        Ok(())
    }

    async fn get_schema(&self) -> AppResult<Vec<TableSchema>> {
        let pool = self.pool(2).await?;

        // Fast row estimates from the planner statistics — no full-table scan.
        let table_rows = sqlx::query(
            "SELECT c.relname AS table_name, GREATEST(c.reltuples::bigint, 0) AS row_estimate \
             FROM pg_class c \
             JOIN pg_namespace n ON n.oid = c.relnamespace \
             WHERE c.relkind = 'r' \
               AND n.nspname NOT IN ('pg_catalog','information_schema') \
             ORDER BY c.relname",
        )
        .fetch_all(&pool)
        .await
        .map_err(|e| conn_err("PostgreSQL table list query failed", e))?;

        let mut row_estimates: HashMap<String, i64> = HashMap::new();
        for row in &table_rows {
            let name: String = row.try_get("table_name").unwrap_or_default();
            let est: i64 = row.try_get("row_estimate").unwrap_or(0);
            row_estimates.insert(name, est);
        }

        // All columns across all user tables in one query.
        let col_rows = sqlx::query(
            "SELECT table_name, column_name, data_type, is_nullable \
             FROM information_schema.columns \
             WHERE table_schema NOT IN ('pg_catalog','information_schema') \
             ORDER BY table_name, ordinal_position",
        )
        .fetch_all(&pool)
        .await
        .map_err(|e| conn_err("PostgreSQL column query failed", e))?;

        // Primary-key columns.
        let pk_rows = sqlx::query(
            "SELECT tc.table_name, kcu.column_name \
             FROM information_schema.table_constraints tc \
             JOIN information_schema.key_column_usage kcu \
               ON tc.constraint_name = kcu.constraint_name \
              AND tc.table_schema    = kcu.table_schema \
             WHERE tc.constraint_type = 'PRIMARY KEY' \
               AND tc.table_schema NOT IN ('pg_catalog','information_schema')",
        )
        .fetch_all(&pool)
        .await
        .map_err(|e| conn_err("PostgreSQL PK query failed", e))?;

        let mut pk_set: HashSet<(String, String)> = HashSet::new();
        for row in &pk_rows {
            let t: String = row.try_get(0).unwrap_or_default();
            let c: String = row.try_get(1).unwrap_or_default();
            pk_set.insert((t, c));
        }

        // Foreign-key columns with referenced table + column (for schema cards).
        let fk_rows = sqlx::query(
            "SELECT tc.table_name, kcu.column_name, ccu.table_name AS ref_table, \
                    ccu.column_name AS ref_column \
             FROM information_schema.table_constraints tc \
             JOIN information_schema.key_column_usage kcu \
               ON tc.constraint_name = kcu.constraint_name \
              AND tc.table_schema    = kcu.table_schema \
             JOIN information_schema.constraint_column_usage ccu \
               ON ccu.constraint_name = tc.constraint_name \
              AND ccu.table_schema    = tc.table_schema \
             WHERE tc.constraint_type = 'FOREIGN KEY' \
               AND tc.table_schema NOT IN ('pg_catalog','information_schema')",
        )
        .fetch_all(&pool)
        .await
        .map_err(|e| conn_err("PostgreSQL FK query failed", e))?;

        let mut fk_set: HashSet<(String, String)> = HashSet::new();
        // Map: table_name → Vec<FkEdge>
        let mut fk_edges_map: HashMap<String, Vec<FkEdge>> = HashMap::new();
        for row in &fk_rows {
            let t: String = row.try_get(0).unwrap_or_default();
            let c: String = row.try_get(1).unwrap_or_default();
            let ref_t: String = row.try_get(2).unwrap_or_default();
            let ref_c: String = row.try_get(3).unwrap_or_default();
            fk_set.insert((t.clone(), c.clone()));
            if !ref_t.is_empty() {
                fk_edges_map.entry(t).or_default().push(FkEdge {
                    column: c,
                    ref_table: ref_t,
                    ref_column: ref_c,
                });
            }
        }

        pool.close().await;

        // Group columns by table, preserving the order from the sorted query.
        let mut table_columns: HashMap<String, Vec<ColumnSchema>> = HashMap::new();
        for row in &col_rows {
            let table: String = row.try_get("table_name").unwrap_or_default();
            let col: String = row.try_get("column_name").unwrap_or_default();
            let dtype: String = row.try_get("data_type").unwrap_or_default();
            let nullable: String = row.try_get("is_nullable").unwrap_or_default();
            table_columns
                .entry(table.clone())
                .or_default()
                .push(ColumnSchema {
                    is_primary_key: pk_set.contains(&(table.clone(), col.clone())),
                    is_foreign_key: fk_set.contains(&(table.clone(), col.clone())),
                    nullable: nullable.eq_ignore_ascii_case("YES"),
                    name: col,
                    type_: dtype,
                    likely_pii: false,
                });
        }

        // Build the final list in alphabetical order (matching the table query order).
        let mut schemas: Vec<TableSchema> = row_estimates
            .into_iter()
            .map(|(name, row_count)| {
                let columns = table_columns.remove(&name).unwrap_or_default();
                TableSchema {
                    name,
                    row_count,
                    columns,
                }
            })
            .collect();
        schemas.sort_by(|a, b| a.name.cmp(&b.name));
        Ok(schemas)
    }

    async fn count_table(&self, table: &str) -> AppResult<i64> {
        let pool = self.pool(1).await?;
        // Double-quote the identifier (Postgres standard). The table name was validated
        // against get_schema() by start_ingest before the job was spawned.
        let sql = format!("SELECT count(*) AS n FROM \"{table}\"");
        let row = sqlx::query(&sql)
            .fetch_one(&pool)
            .await
            .map_err(|e| conn_err(&format!("PostgreSQL count_table({table}) failed"), e))?;
        pool.close().await;
        let n: i64 = row.try_get("n").unwrap_or(0);
        Ok(n)
    }

    async fn fetch_table_page(
        &self,
        table: &str,
        excluded: &[String],
        order_by: Option<&str>,
        offset: i64,
        page_size: i64,
    ) -> AppResult<Vec<FetchedRow>> {
        let pool = self.pool(2).await?;
        let order = order_by
            .map(|column| format!("\"{column}\""))
            .unwrap_or_else(|| "ctid".to_string());
        let sql = format!(
            "SELECT to_jsonb(_sub) AS row_json FROM (SELECT * FROM \"{table}\" ORDER BY {order} OFFSET {offset} LIMIT {page_size}) AS _sub"
        );

        let rows = sqlx::query(&sql)
            .fetch_all(&pool)
            .await
            .map_err(|e| conn_err(&format!("PostgreSQL fetch_table_page({table}) failed"), e))?;
        pool.close().await;

        let mut out = Vec::with_capacity(rows.len());
        for (i, row) in rows.iter().enumerate() {
            let value: serde_json::Value = row
                .try_get("row_json")
                .map_err(|e| AppError::Internal(format!("decoding row {i} failed: {e}")))?;
            match value {
                serde_json::Value::Object(fields) => {
                    out.push(make_row_filtered(fields, excluded, offset as usize + i));
                }
                other => {
                    return Err(AppError::Internal(format!(
                        "expected a JSON object per row, got {other}"
                    )));
                }
            }
        }
        Ok(out)
    }

    async fn run_select(
        &self,
        sql: &str,
        max_rows: i64,
        timeout_secs: u64,
    ) -> AppResult<(Vec<String>, Vec<Vec<serde_json::Value>>)> {
        use serde_json::Value;

        let pool = self.pool(1).await?;

        // Wrap the user SQL so we get one JSONB object per row with column names as keys.
        // statement_timeout is set per-transaction so it never leaks to subsequent queries.
        let wrapped = format!(
            "SELECT to_jsonb(_w) AS _row FROM ({sql}) AS _w LIMIT {max_rows}"
        );
        let timeout_ms = timeout_secs * 1_000;

        // Read-only transaction with a per-statement timeout so no DML can slip through
        // at the engine level even if validate_sql's AST pass is somehow bypassed.
        let mut tx = pool
            .begin()
            .await
            .map_err(|e| conn_err("PostgreSQL begin transaction failed", e))?;

        sqlx::query(&format!("SET LOCAL transaction_read_only = on"))
            .execute(&mut *tx)
            .await
            .map_err(|e| conn_err("PostgreSQL set read-only failed", e))?;

        sqlx::query(&format!("SET LOCAL statement_timeout = '{timeout_ms}ms'"))
            .execute(&mut *tx)
            .await
            .map_err(|e| conn_err("PostgreSQL set statement_timeout failed", e))?;

        let raw_rows = sqlx::query(&wrapped)
            .fetch_all(&mut *tx)
            .await
            .map_err(|e| conn_err("PostgreSQL run_select query failed", e))?;

        tx.rollback()
            .await
            .map_err(|e| conn_err("PostgreSQL rollback failed", e))?;

        pool.close().await;

        if raw_rows.is_empty() {
            return Ok((vec![], vec![]));
        }

        // Extract column names from the first row's JSONB object, then build the result.
        let first: Value = raw_rows[0]
            .try_get("_row")
            .map_err(|e| AppError::Internal(format!("decoding result row 0 failed: {e}")))?;

        let columns: Vec<String> = match &first {
            Value::Object(map) => map.keys().cloned().collect(),
            other => {
                return Err(AppError::Internal(format!(
                    "expected JSON object per row, got {other}"
                )));
            }
        };

        let mut result_rows: Vec<Vec<Value>> = Vec::with_capacity(raw_rows.len());
        for (i, raw) in raw_rows.iter().enumerate() {
            let obj: Value = raw
                .try_get("_row")
                .map_err(|e| AppError::Internal(format!("decoding result row {i} failed: {e}")))?;
            match obj {
                Value::Object(map) => {
                    let row: Vec<Value> =
                        columns.iter().map(|k| map.get(k).cloned().unwrap_or(Value::Null)).collect();
                    result_rows.push(row);
                }
                other => {
                    return Err(AppError::Internal(format!(
                        "expected JSON object at row {i}, got {other}"
                    )));
                }
            }
        }

        Ok((columns, result_rows))
    }
}
