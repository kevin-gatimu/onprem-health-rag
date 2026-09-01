//! SQL Server connector (tiberius/TDS). sqlx dropped MSSQL, so this uses tiberius
//! directly over a Tokio TCP stream adapted with `tokio_util::compat`.

use std::collections::{HashMap, HashSet};

use async_trait::async_trait;
use chrono::{NaiveDate, NaiveDateTime};
use serde_json::{Map, Value, json};
use tiberius::numeric::Numeric;
use tiberius::{AuthMethod, Client, Config, Row};
use tokio::net::TcpStream;
use tokio_util::compat::TokioAsyncWriteCompatExt;

use super::{
    ColumnSchema, FetchedRow, FkEdge, SourceConnector, SourceSpec, TableSchema, conn_err,
    make_row_filtered,
};
use crate::error::AppResult;

pub struct MssqlConnector {
    spec: SourceSpec,
}

impl MssqlConnector {
    pub fn new(spec: SourceSpec) -> Self {
        MssqlConnector { spec }
    }

    fn config(&self) -> Config {
        let mut config = Config::new();
        config.host(&self.spec.host);
        config.port(self.spec.port);
        config.database(&self.spec.database);
        config.authentication(AuthMethod::sql_server(
            &self.spec.username,
            &self.spec.password,
        ));
        // Accept self-signed certs — on-prem SQL Server instances commonly use them.
        config.trust_cert();
        config
    }

    async fn new_client(&self) -> AppResult<Client<tokio_util::compat::Compat<TcpStream>>> {
        let config = self.config();
        let tcp = TcpStream::connect(config.get_addr())
            .await
            .map_err(|e| conn_err("SQL Server TCP connect failed", e))?;
        tcp.set_nodelay(true)
            .map_err(|e| conn_err("SQL Server socket setup failed", e))?;
        Client::connect(config, tcp.compat_write())
            .await
            .map_err(|e| conn_err("SQL Server connection failed", e))
    }
}

#[async_trait]
impl SourceConnector for MssqlConnector {
    async fn test(&self) -> AppResult<()> {
        let mut client = self.new_client().await?;

        // Drive a trivial query to end-to-end confirm the session.
        client
            .simple_query("SELECT 1")
            .await
            .map_err(|e| conn_err("SQL Server test query failed", e))?
            .into_first_result()
            .await
            .map_err(|e| conn_err("SQL Server test query failed", e))?;

        Ok(())
    }

    async fn get_schema(&self) -> AppResult<Vec<TableSchema>> {
        // One connection for all schema queries — avoids multiple TDS handshakes.
        let mut client = self.new_client().await?;

        // 1. User table names.
        let table_rows: Vec<Row> = client
            .simple_query(
                "SELECT TABLE_NAME FROM INFORMATION_SCHEMA.TABLES \
                 WHERE TABLE_TYPE = 'BASE TABLE' ORDER BY TABLE_NAME",
            )
            .await
            .map_err(|e| conn_err("SQL Server table list query failed", e))?
            .into_first_result()
            .await
            .map_err(|e| conn_err("SQL Server table list query failed", e))?;

        let tables: Vec<String> = table_rows
            .iter()
            .filter_map(|r| r.try_get::<&str, _>(0).ok().flatten().map(str::to_string))
            .collect();

        // 2. Fast row estimates from partition stats (avoids COUNT(*) on every table).
        let est_rows: Vec<Row> = client
            .simple_query(
                "SELECT OBJECT_NAME(object_id) AS t, SUM(row_count) AS cnt \
                 FROM sys.dm_db_partition_stats \
                 WHERE index_id IN (0, 1) \
                 GROUP BY object_id",
            )
            .await
            .map_err(|e| conn_err("SQL Server row estimate query failed", e))?
            .into_first_result()
            .await
            .map_err(|e| conn_err("SQL Server row estimate query failed", e))?;

        let mut row_estimates: HashMap<String, i64> = HashMap::new();
        for row in &est_rows {
            if let (Ok(Some(name)), Ok(Some(cnt))) =
                (row.try_get::<&str, _>(0), row.try_get::<i64, _>(1))
            {
                row_estimates.insert(name.to_string(), cnt);
            }
        }

        // 3. All columns across all user tables.
        let col_rows: Vec<Row> = client
            .simple_query(
                "SELECT TABLE_NAME, COLUMN_NAME, DATA_TYPE, IS_NULLABLE \
                 FROM INFORMATION_SCHEMA.COLUMNS \
                 ORDER BY TABLE_NAME, ORDINAL_POSITION",
            )
            .await
            .map_err(|e| conn_err("SQL Server column query failed", e))?
            .into_first_result()
            .await
            .map_err(|e| conn_err("SQL Server column query failed", e))?;

        // 4. Primary-key columns.
        let pk_rows: Vec<Row> = client
            .simple_query(
                "SELECT kcu.TABLE_NAME, kcu.COLUMN_NAME \
                 FROM INFORMATION_SCHEMA.TABLE_CONSTRAINTS tc \
                 JOIN INFORMATION_SCHEMA.KEY_COLUMN_USAGE kcu \
                   ON tc.CONSTRAINT_NAME = kcu.CONSTRAINT_NAME \
                  AND tc.TABLE_SCHEMA    = kcu.TABLE_SCHEMA \
                 WHERE tc.CONSTRAINT_TYPE = 'PRIMARY KEY'",
            )
            .await
            .map_err(|e| conn_err("SQL Server PK query failed", e))?
            .into_first_result()
            .await
            .map_err(|e| conn_err("SQL Server PK query failed", e))?;

        let mut pk_set: HashSet<(String, String)> = HashSet::new();
        for row in &pk_rows {
            if let (Ok(Some(t)), Ok(Some(c))) =
                (row.try_get::<&str, _>(0), row.try_get::<&str, _>(1))
            {
                pk_set.insert((t.to_string(), c.to_string()));
            }
        }

        // 5. FK columns with referenced table + column (for schema cards).
        let fk_rows: Vec<Row> = client
            .simple_query(
                "SELECT \
                   OBJECT_NAME(fk.parent_object_id) AS table_name, \
                   COL_NAME(fk.parent_object_id, fk.parent_column_id) AS column_name, \
                   OBJECT_NAME(fk.referenced_object_id) AS ref_table, \
                   COL_NAME(fk.referenced_object_id, fk.referenced_column_id) AS ref_column \
                 FROM sys.foreign_key_columns fk",
            )
            .await
            .map_err(|e| conn_err("SQL Server FK query failed", e))?
            .into_first_result()
            .await
            .map_err(|e| conn_err("SQL Server FK query failed", e))?;

        let mut fk_set: HashSet<(String, String)> = HashSet::new();
        let mut fk_edges_map: HashMap<String, Vec<FkEdge>> = HashMap::new();
        for row in &fk_rows {
            if let (Ok(Some(t)), Ok(Some(c))) =
                (row.try_get::<&str, _>(0), row.try_get::<&str, _>(1))
            {
                let ref_t = row
                    .try_get::<&str, _>(2)
                    .ok()
                    .flatten()
                    .unwrap_or("")
                    .to_string();
                let ref_c = row
                    .try_get::<&str, _>(3)
                    .ok()
                    .flatten()
                    .unwrap_or("")
                    .to_string();
                fk_set.insert((t.to_string(), c.to_string()));
                if !ref_t.is_empty() {
                    fk_edges_map.entry(t.to_string()).or_default().push(FkEdge {
                        column: c.to_string(),
                        ref_table: ref_t,
                        ref_column: ref_c,
                    });
                }
            }
        }

        // Group columns by table.
        let mut table_columns: HashMap<String, Vec<ColumnSchema>> = HashMap::new();
        for row in &col_rows {
            let table = match row.try_get::<&str, _>(0).ok().flatten() {
                Some(t) => t.to_string(),
                None => continue,
            };
            let col = match row.try_get::<&str, _>(1).ok().flatten() {
                Some(c) => c.to_string(),
                None => continue,
            };
            let dtype = row
                .try_get::<&str, _>(2)
                .ok()
                .flatten()
                .unwrap_or("")
                .to_string();
            let nullable = row.try_get::<&str, _>(3).ok().flatten().unwrap_or("NO");
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

        // Assemble in the order from the table-list query so the response is stable.
        let schemas = tables
            .into_iter()
            .map(|name| {
                let row_count = row_estimates.get(&name).copied().unwrap_or(0);
                let columns = table_columns.remove(&name).unwrap_or_default();
                let fk_edges = fk_edges_map.remove(&name).unwrap_or_default();
                TableSchema {
                    name,
                    row_count,
                    columns,
                    fk_edges,
                }
            })
            .collect();
        Ok(schemas)
    }

    async fn count_table(&self, table: &str) -> AppResult<i64> {
        let mut client = self.new_client().await?;
        // Bracket-quote the identifier (SQL Server standard).
        let sql = format!("SELECT COUNT_BIG(*) AS n FROM [{table}]");
        let rows: Vec<Row> = client
            .simple_query(sql)
            .await
            .map_err(|e| conn_err(&format!("SQL Server count_table({table}) failed"), e))?
            .into_first_result()
            .await
            .map_err(|e| conn_err(&format!("SQL Server count_table({table}) failed"), e))?;
        let n: i64 = rows
            .first()
            .and_then(|r| r.try_get::<i64, _>(0).ok().flatten())
            .unwrap_or(0);
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
        let mut client = self.new_client().await?;
        let order = order_by
            .map(|column| format!("[{column}]"))
            .unwrap_or_else(|| "(SELECT NULL)".to_string());
        let sql = format!(
            "SELECT * FROM [{table}] ORDER BY {order} OFFSET {offset} ROWS FETCH NEXT {page_size} ROWS ONLY"
        );

        let rows: Vec<Row> = client
            .simple_query(sql)
            .await
            .map_err(|e| conn_err(&format!("SQL Server fetch_table_page({table}) failed"), e))?
            .into_first_result()
            .await
            .map_err(|e| conn_err(&format!("SQL Server fetch_table_page({table}) failed"), e))?;

        let out = rows
            .iter()
            .enumerate()
            .map(|(i, row)| {
                let mut fields = Map::new();
                let names: Vec<String> =
                    row.columns().iter().map(|c| c.name().to_string()).collect();
                for (col, name) in names.into_iter().enumerate() {
                    if excluded.iter().any(|e| e == &name) {
                        continue;
                    }
                    fields.insert(name, cell_to_json(row, col));
                }
                make_row_filtered(fields, excluded, offset as usize + i)
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
        use crate::error::AppError;
        use tokio::time::{Duration, timeout};

        // tiberius has no per-query timeout; wrap query+fetch in a Tokio timeout.
        // The sql already carries TOP {max_rows} injected by validate_sql, but that
        // only lands on the outer SELECT for a plain query — inject_top() puts it
        // inside the CTE body for a `WITH ... SELECT` shape, leaving the outer
        // SELECT unbounded. Mirror postgres/mysql's defense-in-depth: wrap in an
        // outer TOP regardless of what validate_sql already injected.
        let owned_sql = format!("SELECT TOP ({max_rows}) * FROM ({sql}) AS _w");
        let mut client = self.new_client().await?;

        let rows: Vec<Row> = timeout(Duration::from_secs(timeout_secs), async move {
            client
                .simple_query(owned_sql.as_str())
                .await
                .map_err(|e| conn_err("SQL Server run_select query failed", e))?
                .into_first_result()
                .await
                .map_err(|e| conn_err("SQL Server run_select result failed", e))
        })
        .await
        .map_err(|_| {
            AppError::BadRequest(format!("SQL Server query timed out after {timeout_secs}s"))
        })??;

        if rows.is_empty() {
            return Ok((vec![], vec![]));
        }

        let columns: Vec<String> = rows[0]
            .columns()
            .iter()
            .map(|c| c.name().to_string())
            .collect();
        let mut result_rows = Vec::with_capacity(rows.len());
        for row in &rows {
            let values: Vec<serde_json::Value> =
                (0..columns.len()).map(|i| cell_to_json(row, i)).collect();
            result_rows.push(values);
        }
        Ok((columns, result_rows))
    }
}

/// Decode one SQL Server cell to JSON. tiberius `try_get::<T>` returns `Ok(None)` for
/// SQL NULL and `Err` on a type mismatch, so we try concrete types in order and take
/// the first that yields a value; unrecognised types become `null`.
///
/// The type order matters: exact integers before floats; `Numeric` (DECIMAL/NUMERIC/
/// MONEY) after the primitive numerics, rendered as a string to preserve full precision
/// (health data — lab values, dosages, costs — must not be rounded through `f64`).
fn cell_to_json(row: &Row, i: usize) -> Value {
    if let Ok(Some(v)) = row.try_get::<i64, _>(i) {
        return json!(v);
    }
    if let Ok(Some(v)) = row.try_get::<i32, _>(i) {
        return json!(v);
    }
    if let Ok(Some(v)) = row.try_get::<i16, _>(i) {
        return json!(v);
    }
    if let Ok(Some(v)) = row.try_get::<u8, _>(i) {
        // SQL Server TINYINT is unsigned; tiberius surfaces it as u8.
        return json!(v);
    }
    if let Ok(Some(v)) = row.try_get::<f64, _>(i) {
        return json!(v);
    }
    if let Ok(Some(v)) = row.try_get::<f32, _>(i) {
        // REAL is 32-bit; it won't decode as f64.
        return json!(v);
    }
    // DECIMAL / NUMERIC / MONEY arrive as `Numeric`; keep exact precision as a string.
    if let Ok(Some(v)) = row.try_get::<Numeric, _>(i) {
        return json!(v.to_string());
    }
    if let Ok(Some(v)) = row.try_get::<bool, _>(i) {
        return json!(v);
    }
    if let Ok(Some(v)) = row.try_get::<&str, _>(i) {
        return json!(v);
    }
    if let Ok(Some(v)) = row.try_get::<NaiveDateTime, _>(i) {
        return json!(v.to_string());
    }
    if let Ok(Some(v)) = row.try_get::<NaiveDate, _>(i) {
        return json!(v.to_string());
    }
    // UNIQUEIDENTIFIER (GUID) and any type we don't model fall through to null.
    if let Ok(Some(v)) = row.try_get::<uuid::Uuid, _>(i) {
        return json!(v.to_string());
    }
    Value::Null
}
