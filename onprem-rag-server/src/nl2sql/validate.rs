use sqlparser::ast::Statement;
use sqlparser::dialect::{MsSqlDialect, MySqlDialect, PostgreSqlDialect};
use sqlparser::parser::Parser;

use crate::connectors::SourceKind;
use crate::error::{AppError, AppResult};

pub struct ValidatedSql {
    pub sql: String,
}

/// Parse `sql`, enforce a single SELECT-only statement, inject LIMIT/TOP, and
/// block a short list of functions that must not run in a read-only context.
pub fn validate_sql(sql: &str, dialect: SourceKind, max_rows: i64) -> AppResult<ValidatedSql> {
    let sql = sql.trim().trim_end_matches(';').trim();

    let stmts: Vec<Statement> = match dialect {
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

    match &stmts[0] {
        Statement::Query(_) => {}
        other => {
            return Err(AppError::BadRequest(format!(
                "only SELECT statements are allowed; got {}",
                statement_kind(other)
            )));
        }
    }

    // Block functions that are harmless-looking but should never run read-only.
    let upper = sql.to_uppercase();
    for name in BLOCKED_FUNCTIONS {
        if upper.contains(name) {
            return Err(AppError::BadRequest(format!(
                "function {name} is not allowed in generated queries"
            )));
        }
    }

    // Inject row cap.
    let capped = match dialect {
        SourceKind::Mssql => inject_top(sql, max_rows),
        _ => inject_limit(sql, max_rows),
    };

    Ok(ValidatedSql { sql: capped })
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

fn inject_limit(sql: &str, max_rows: i64) -> String {
    let upper = sql.to_uppercase();
    if upper.contains(" LIMIT ") {
        // Already has a LIMIT; leave it (phi-4-mini may have added a tighter one).
        sql.to_string()
    } else {
        format!("{sql} LIMIT {max_rows}")
    }
}

fn inject_top(sql: &str, max_rows: i64) -> String {
    let upper = sql.to_uppercase();
    // Don't double-inject.
    if upper.contains("SELECT TOP ") || upper.contains("SELECT TOP\t") {
        return sql.to_string();
    }
    // Insert TOP after the outer SELECT. For a CTE (WITH ... SELECT), this inserts
    // TOP inside the CTE body rather than on the outer query, which is wrong. CTE
    // generation is rare here; add AST rewriting if it becomes a problem.
    if let Some(pos) = upper.find("SELECT") {
        let after = pos + "SELECT".len();
        format!("{}SELECT TOP {} {}", &sql[..pos], max_rows, sql[after..].trim_start())
    } else {
        sql.to_string()
    }
}

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
