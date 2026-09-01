use std::collections::HashSet;
use std::ops::ControlFlow;

use sqlparser::ast::{Expr, SetExpr, Statement, Top, TopQuantity, Value, visit_relations};
use sqlparser::dialect::{MsSqlDialect, MySqlDialect, PostgreSqlDialect};
use sqlparser::parser::Parser;

use crate::connectors::SourceKind;
use crate::error::{AppError, AppResult};

#[derive(Debug)]
pub struct ValidatedSql {
    pub sql: String,
}

/// Parse `sql`, enforce a single SELECT-only statement and linked-table
/// allowlist, apply an outer row cap, and return normalized SQL from the AST.
pub fn validate_sql(
    sql: &str,
    dialect: SourceKind,
    max_rows: i64,
    allowed_tables: &[String],
) -> AppResult<ValidatedSql> {
    let sql = sql.trim().trim_end_matches(';').trim();

    let mut stmts: Vec<Statement> = match dialect {
        SourceKind::Postgres => Parser::parse_sql(&PostgreSqlDialect {}, sql),
        SourceKind::Mysql => Parser::parse_sql(&MySqlDialect {}, sql),
        SourceKind::Mssql => Parser::parse_sql(&MsSqlDialect {}, sql),
    }
    .map_err(|e| AppError::BadRequest(format!("SQL parse error: {e}")))?;

    if stmts.len() != 1 {
        return Err(AppError::BadRequest(format!(
            "expected exactly one statement, got {}",
            stmts.len()
        )));
    }

    let Statement::Query(query) = &mut stmts[0] else {
        return Err(AppError::BadRequest(format!(
            "only SELECT statements are allowed; got {}",
            statement_kind(&stmts[0])
        )));
    };

    let upper = sql.to_uppercase();
    for name in BLOCKED_FUNCTIONS {
        if upper.contains(name) {
            return Err(AppError::BadRequest(format!(
                "function {name} is not allowed in generated queries"
            )));
        }
    }

    if !query.locks.is_empty() {
        return Err(AppError::BadRequest(
            "locking clauses are not allowed in generated queries".into(),
        ));
    }

    let allowed: HashSet<String> = allowed_tables
        .iter()
        .map(|table| table.to_ascii_lowercase())
        .collect();
    let ctes: HashSet<String> = query
        .with
        .iter()
        .flat_map(|with| with.cte_tables.iter())
        .map(|cte| cte.alias.name.value.to_ascii_lowercase())
        .collect();
    let mut denied = None;
    let _ = visit_relations(&*query, |relation| {
        let name = relation.to_string().to_ascii_lowercase();
        if !allowed.contains(&name) && !ctes.contains(&name) {
            denied = Some(relation.to_string());
            return ControlFlow::Break(());
        }
        ControlFlow::Continue(())
    });
    if let Some(table) = denied {
        return Err(AppError::BadRequest(format!(
            "table {table} was not selected by schema linking"
        )));
    }

    let max_rows = u64::try_from(max_rows.max(1)).unwrap_or(1);
    match dialect {
        SourceKind::Mssql => {
            let SetExpr::Select(select) = query.body.as_mut() else {
                return Err(AppError::BadRequest(
                    "SQL Server set operations are not supported for generated queries".into(),
                ));
            };
            let row_limit = select
                .top
                .as_ref()
                .and_then(|top| match top.quantity {
                    Some(TopQuantity::Constant(value)) if !top.percent && !top.with_ties => {
                        Some(value.clamp(1, max_rows))
                    }
                    _ => None,
                })
                .unwrap_or(max_rows);
            select.top = Some(Top {
                with_ties: false,
                percent: false,
                quantity: Some(TopQuantity::Constant(row_limit)),
            });
        }
        SourceKind::Postgres | SourceKind::Mysql => {
            let row_limit = query
                .limit
                .as_ref()
                .and_then(constant_limit)
                .map(|value| value.clamp(1, max_rows))
                .unwrap_or(max_rows);
            query.limit = Some(Expr::Value(Value::Number(row_limit.to_string(), false)));
        }
    }

    Ok(ValidatedSql {
        sql: stmts.remove(0).to_string(),
    })
}

fn constant_limit(expr: &Expr) -> Option<u64> {
    let Expr::Value(Value::Number(value, _)) = expr else {
        return None;
    };
    value.parse().ok()
}

const BLOCKED_FUNCTIONS: &[&str] = &[
    "PG_SLEEP",
    "SLEEP(",
    "WAITFOR",
    "BENCHMARK(",
    "LOAD_FILE(",
    "INTO OUTFILE",
    "INTO DUMPFILE",
    "XP_CMDSHELL",
    "SP_EXECUTESQL",
    "EXEC(",
    "EXECUTE(",
    "OPENROWSET(",
    "OPENDATASOURCE(",
    "DBCC",
];

fn statement_kind(s: &Statement) -> &'static str {
    match s {
        Statement::Insert { .. } => "INSERT",
        Statement::Update { .. } => "UPDATE",
        Statement::Delete(_) => "DELETE",
        Statement::Drop { .. } => "DROP",
        Statement::CreateTable(_) => "CREATE TABLE",
        Statement::AlterTable { .. } => "ALTER TABLE",
        Statement::Truncate { .. } => "TRUNCATE",
        _ => "non-SELECT",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tables(names: &[&str]) -> Vec<String> {
        names.iter().map(|name| (*name).to_string()).collect()
    }

    #[test]
    fn rejects_tables_outside_linked_schema() {
        let error = validate_sql(
            "SELECT * FROM secrets",
            SourceKind::Postgres,
            100,
            &tables(&["patients"]),
        )
        .expect_err("unlinked table must be rejected");
        assert!(error.to_string().contains("secrets"));
    }

    #[test]
    fn allows_ctes_built_from_linked_tables() {
        let sql = validate_sql(
            "WITH recent AS (SELECT id FROM patients) SELECT * FROM recent",
            SourceKind::Postgres,
            100,
            &tables(&["patients"]),
        )
        .expect("CTE should be allowed")
        .sql;
        assert!(sql.ends_with("LIMIT 100"));
    }

    #[test]
    fn replaces_oversized_postgres_limit() {
        let sql = validate_sql(
            "SELECT * FROM patients LIMIT 99999",
            SourceKind::Postgres,
            100,
            &tables(&["patients"]),
        )
        .expect("query should validate")
        .sql;
        assert!(sql.ends_with("LIMIT 100"));
        assert!(!sql.contains("99999"));
    }

    #[test]
    fn preserves_smaller_requested_limit() {
        let sql = validate_sql(
            "SELECT * FROM patients LIMIT 5",
            SourceKind::Postgres,
            100,
            &tables(&["patients"]),
        )
        .expect("query should validate")
        .sql;
        assert!(sql.ends_with("LIMIT 5"));
    }

    #[test]
    fn replaces_sql_server_top_on_outer_select() {
        let sql = validate_sql(
            "WITH recent AS (SELECT id FROM patients) SELECT TOP 5000 * FROM recent",
            SourceKind::Mssql,
            100,
            &tables(&["patients"]),
        )
        .expect("query should validate")
        .sql;
        assert!(sql.contains("SELECT TOP 100 * FROM recent"));
        assert!(!sql.contains("TOP 5000"));
    }

    #[test]
    fn preserves_smaller_sql_server_top() {
        let sql = validate_sql(
            "SELECT TOP 5 * FROM patients",
            SourceKind::Mssql,
            100,
            &tables(&["patients"]),
        )
        .expect("query should validate")
        .sql;
        assert!(sql.contains("SELECT TOP 5 * FROM patients"));
    }

    #[test]
    fn rejects_locking_queries() {
        let error = validate_sql(
            "SELECT * FROM patients FOR UPDATE",
            SourceKind::Postgres,
            100,
            &tables(&["patients"]),
        )
        .expect_err("locking query must be rejected");
        assert!(error.to_string().contains("locking"));
    }
}
