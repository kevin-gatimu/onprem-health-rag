//! MySQL connector (sqlx), mirroring the Postgres connector.

use std::collections::{HashMap, HashSet};
use std::time::Duration;

use async_trait::async_trait;
use base64::Engine;
use base64::engine::general_purpose::STANDARD as B64;
use serde_json::{Map, Value, json};
use sqlx::mysql::{MySqlConnectOptions, MySqlPoolOptions, MySqlRow};
use sqlx::types::BigDecimal;
use sqlx::types::chrono::{NaiveDate, NaiveDateTime, NaiveTime};
use sqlx::{Column, Row, ValueRef};

use super::{ColumnSchema, FetchedRow, FkEdge, SourceConnector, SourceSpec, TableSchema, conn_err,
            make_row_filtered};
use crate::error::AppResult;

pub struct MysqlConnector {
    spec: SourceSpec,
}

impl MysqlConnector {
    pub fn new(spec: SourceSpec) -> Self {
        MysqlConnector { spec }
    }

    fn options(&self) -> MySqlConnectOptions {
        MySqlConnectOptions::new()
            .host(&self.spec.host)
            .port(self.spec.port)
            .username(&self.spec.username)
            .password(&self.spec.password)
            .database(&self.spec.database)
    }

    async fn pool(&self, max: u32) -> AppResult<sqlx::MySqlPool> {
        MySqlPoolOptions::new()
            .max_connections(max)
            .acquire_timeout(Duration::from_secs(15))
            .connect_with(self.options())
            .await
            .map_err(|e| conn_err("MySQL connection failed", e))
    }
}

#[async_trait]
impl SourceConnector for MysqlConnector {
    async fn test(&self) -> AppResult<()> {
        let pool = MySqlPoolOptions::new()
            .max_connections(1)
            .acquire_timeout(Duration::from_secs(8))
            .connect_with(self.options())
            .await
            .map_err(|e| conn_err("MySQL connection failed", e))?;

        sqlx::query("SELECT 1")
            .execute(&pool)
            .await
            .map_err(|e| conn_err("MySQL test query failed", e))?;

        pool.close().await;
        Ok(())
    }

    async fn get_schema(&self) -> AppResult<Vec<TableSchema>> {
        let pool = self.pool(2).await?;

        // Fast row estimates from information_schema (updated on ANALYZE, so approximate).
        let table_rows = sqlx::query(
            "SELECT TABLE_NAME, COALESCE(TABLE_ROWS, 0) AS row_est \
             FROM information_schema.tables \
             WHERE TABLE_SCHEMA = DATABASE() AND TABLE_TYPE = 'BASE TABLE' \
             ORDER BY TABLE_NAME",
        )
        .fetch_all(&pool)
        .await
        .map_err(|e| conn_err("MySQL table list query failed", e))?;

        let mut row_estimates: HashMap<String, i64> = HashMap::new();
        for row in &table_rows {
            let name: String = row.try_get(0).unwrap_or_default();
            // TABLE_ROWS is BIGINT UNSIGNED; try i64 first, fall back to u64.
            let est: i64 = row
                .try_get::<i64, _>(1)
                .unwrap_or_else(|_| row.try_get::<u64, _>(1).unwrap_or(0) as i64);
            row_estimates.insert(name, est);
        }

        // All columns for all user tables in one query.
        let col_rows = sqlx::query(
            "SELECT TABLE_NAME, COLUMN_NAME, DATA_TYPE, IS_NULLABLE, COLUMN_KEY \
             FROM information_schema.columns \
             WHERE TABLE_SCHEMA = DATABASE() \
             ORDER BY TABLE_NAME, ORDINAL_POSITION",
        )
        .fetch_all(&pool)
        .await
        .map_err(|e| conn_err("MySQL column query failed", e))?;

        // FK columns with referenced table + column (for schema cards).
        let fk_rows = sqlx::query(
            "SELECT TABLE_NAME, COLUMN_NAME, REFERENCED_TABLE_NAME, REFERENCED_COLUMN_NAME \
             FROM information_schema.key_column_usage \
             WHERE TABLE_SCHEMA = DATABASE() AND REFERENCED_TABLE_NAME IS NOT NULL",
        )
        .fetch_all(&pool)
        .await
        .map_err(|e| conn_err("MySQL FK query failed", e))?;

        pool.close().await;

        let mut fk_set: HashSet<(String, String)> = HashSet::new();
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

        let mut table_columns: HashMap<String, Vec<ColumnSchema>> = HashMap::new();
        for row in &col_rows {
            let table: String = row.try_get(0).unwrap_or_default();
            let col: String = row.try_get(1).unwrap_or_default();
            let dtype: String = row.try_get(2).unwrap_or_default();
            let nullable: String = row.try_get(3).unwrap_or_default();
            let col_key: String = row.try_get(4).unwrap_or_default();
            table_columns.entry(table.clone()).or_default().push(ColumnSchema {
                is_primary_key: col_key == "PRI",
                is_foreign_key: fk_set.contains(&(table.clone(), col.clone())),
                nullable: nullable.eq_ignore_ascii_case("YES"),
                name: col,
                type_: dtype,
                likely_pii: false,
            });
        }

        let mut schemas: Vec<TableSchema> = row_estimates
            .into_iter()
            .map(|(name, row_count)| {
                let columns = table_columns.remove(&name).unwrap_or_default();
                let fk_edges = fk_edges_map.remove(&name).unwrap_or_default();
                TableSchema { name, row_count, columns, fk_edges }
            })
            .collect();
        schemas.sort_by(|a, b| a.name.cmp(&b.name));
        Ok(schemas)
    }

    async fn count_table(&self, table: &str) -> AppResult<i64> {
        let pool = self.pool(1).await?;
        // Backtick-quote the identifier (MySQL standard).
        let sql = format!("SELECT count(*) AS n FROM `{table}`");
        let row = sqlx::query(&sql)
            .fetch_one(&pool)
            .await
            .map_err(|e| conn_err(&format!("MySQL count_table({table}) failed"), e))?;
        pool.close().await;
        let n: i64 = row
            .try_get::<i64, _>("n")
            .unwrap_or_else(|_| row.try_get::<u64, _>("n").unwrap_or(0) as i64);
        Ok(n)
    }

    async fn fetch_table(
        &self,
        table: &str,
        excluded: &[String],
        limit: Option<i64>,
    ) -> AppResult<Vec<FetchedRow>> {
        let pool = self.pool(2).await?;
        // Backtick-quote the identifier. The table was validated by start_ingest.
        let mut sql = format!("SELECT * FROM `{table}`");
        if let Some(n) = limit {
            sql.push_str(&format!(" LIMIT {n}"));
        }

        let rows = sqlx::query(&sql)
            .fetch_all(&pool)
            .await
            .map_err(|e| conn_err(&format!("MySQL fetch_table({table}) failed"), e))?;
        pool.close().await;

        let out = rows
            .iter()
            .enumerate()
            .map(|(i, row)| {
                let mut fields = Map::new();
                for (col, column) in row.columns().iter().enumerate() {
                    // Honour excluded columns here — skip building the field entirely
                    // so PII bytes never reach the FetchedRow.
                    if excluded.iter().any(|e| e == column.name()) {
                        continue;
                    }
                    fields.insert(column.name().to_string(), cell_to_json(row, col));
                }
                make_row_filtered(fields, excluded, i)
            })
            .collect();
        Ok(out)
    }

    async fn run_select(
        &self,
        sql: &str,
        max_rows: i64,
        timeout_secs: u64,
    ) -> AppResult<(Vec<String>, Vec<Vec<serde_json::Value>>)> {
        let pool = self.pool(1).await?;
        // max_execution_time is milliseconds; 0 = disabled (reset for the next caller).
        let timeout_ms = timeout_secs * 1_000;

        sqlx::query(&format!("SET SESSION max_execution_time = {timeout_ms}"))
            .execute(&pool)
            .await
            .map_err(|e| conn_err("MySQL set max_execution_time failed", e))?;

        // The sql already carries LIMIT {max_rows} injected by validate_sql; the
        // subquery wrap adds a hard safety cap in case validate_sql is bypassed.
        let capped = format!("SELECT * FROM ({sql}) AS _q LIMIT {max_rows}");

        let rows = sqlx::query(&capped)
            .fetch_all(&pool)
            .await
            .map_err(|e| conn_err("MySQL run_select query failed", e))?;

        // Reset so the pool's lingering connection doesn't timeout unrelated queries.
        sqlx::query("SET SESSION max_execution_time = 0")
            .execute(&pool)
            .await
            .ok();

        pool.close().await;

        if rows.is_empty() {
            return Ok((vec![], vec![]));
        }

        let columns: Vec<String> =
            rows[0].columns().iter().map(|c| c.name().to_string()).collect();
        let mut result_rows = Vec::with_capacity(rows.len());
        for row in &rows {
            let values = (0..columns.len()).map(|i| cell_to_json(row, i)).collect();
            result_rows.push(values);
        }
        Ok((columns, result_rows))
    }
}

/// Decode one MySQL cell to JSON without knowing its type ahead of time. sqlx's
/// `try_get::<T>` fails cleanly on a type mismatch, so we try concrete types in order
/// and take the first that succeeds; anything unrecognised becomes `null`.
fn cell_to_json(row: &MySqlRow, i: usize) -> Value {
    if row.try_get_raw(i).map(|v| v.is_null()).unwrap_or(true) {
        return Value::Null;
    }
    if let Ok(v) = row.try_get::<i64, _>(i) {
        return json!(v);
    }
    if let Ok(v) = row.try_get::<f64, _>(i) {
        return json!(v);
    }
    if let Ok(v) = row.try_get::<bool, _>(i) {
        return json!(v);
    }
    if let Ok(v) = row.try_get::<String, _>(i) {
        return json!(v);
    }
    if let Ok(v) = row.try_get::<BigDecimal, _>(i) {
        return json!(v.to_string());
    }
    if let Ok(v) = row.try_get::<NaiveDateTime, _>(i) {
        return json!(v.to_string());
    }
    if let Ok(v) = row.try_get::<NaiveDate, _>(i) {
        return json!(v.to_string());
    }
    if let Ok(v) = row.try_get::<NaiveTime, _>(i) {
        return json!(v.to_string());
    }
    if let Ok(v) = row.try_get::<Vec<u8>, _>(i) {
        return json!(B64.encode(v));
    }
    Value::Null
}
