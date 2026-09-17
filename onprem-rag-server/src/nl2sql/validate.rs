use std::collections::HashSet;
use std::ops::ControlFlow;

use sqlparser::ast::{
    Expr, FunctionArg, FunctionArgExpr, FunctionArguments, Query, SetExpr, Statement, Top,
    TopQuantity, Value, visit_expressions, visit_relations,
};
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

    reject_boundary_day_arithmetic(query)?;

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
        // Strip surrounding dialect-specific quote chars (`"…"`, `` `…` ``, `[…]`)
        // so an IR-compiled quoted identifier still matches the unquoted allowed list.
        let raw = relation.to_string();
        let name = raw
            .trim_matches(|c| c == '"' || c == '`' || c == '[' || c == ']')
            .to_ascii_lowercase();
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

/// Date-part names that are coarser than a second, i.e. the granularities at
/// which the dialects' native difference functions disagree.
///
/// Includes SQL Server's abbreviations, because `DATEDIFF(DD, a, b)` is the same
/// wrong answer as `DATEDIFF(DAY, a, b)`.
const COARSE_DATE_PARTS: &[&str] = &[
    "DAY", "DAYS", "DD", "D", "DAYOFYEAR", "DY", "WEEK", "WEEKS", "WK", "WW", "ISO_WEEK",
    "ISOWK", "ISOWW", "MONTH", "MONTHS", "MM", "M", "QUARTER", "QUARTERS", "QQ", "Q", "YEAR",
    "YEARS", "YY", "YYYY",
];

/// Native difference functions whose semantics differ per dialect.
const DIFF_FUNCTIONS: &[&str] = &["DATEDIFF", "DATEDIFF_BIG", "TIMESTAMPDIFF", "DATE_DIFF"];

/// Reject day-or-coarser native date-difference arithmetic (plan 03g §2).
///
/// This closes a real hole: the substring `BLOCKED_FUNCTIONS` scan above only
/// looks for injection primitives, so before this rule any parseable function
/// name — `DATEDIFF(DAY, admitted, discharged)` included — was waved straight
/// through. The validator has to *understand* a derived duration, not merely fail
/// to recognise it, because the wrong version of this expression produces a
/// plausible number rather than an error:
///
/// * SQL Server `DATEDIFF(DAY, a, b)` counts date-boundary crossings, so a stay
///   from 23:00 Monday to 01:00 Tuesday is reported as 1 day.
/// * MySQL `DATEDIFF(a, b)` also counts date boundaries **and is end-first**, so
///   the naive port of the above returns -1.
/// * MySQL `TIMESTAMPDIFF(DAY, a, b)` truncates, so the same stay is 0.
///
/// Second-granularity differences (`SECOND`, and Postgres `EXTRACT(EPOCH FROM …)`)
/// are untouched: those are the forms that agree, and are what the IR emits.
///
/// One legitimate exception is preserved. Bucket truncation spells its start
/// argument as the epoch anchor `0` — `DATEADD(WEEK, DATEDIFF(WEEK, 0, col), 0)`
/// is SQL Server's `date_trunc`, not an elapsed-time measurement — so a
/// three-argument diff whose *start* argument is a numeric literal is allowed.
/// The two-argument MySQL form has no such use and is rejected outright.
fn reject_boundary_day_arithmetic(query: &Query) -> AppResult<()> {
    let mut offending: Option<String> = None;
    let _ = visit_expressions(query, |expr| {
        let Expr::Function(func) = expr else {
            return ControlFlow::Continue(());
        };
        let name = func.name.to_string().to_ascii_uppercase();
        if !DIFF_FUNCTIONS.contains(&name.as_str()) {
            return ControlFlow::Continue(());
        }
        let args = plain_function_args(&func.args);

        let rejected = match args.len() {
            // MySQL `DATEDIFF(end, start)`: whole-date difference, reversed args.
            2 => true,
            // `DATEDIFF(part, start, end)`: reject a coarse part unless `start` is
            // the epoch anchor of a bucket-truncation idiom.
            3.. => {
                let part = args[0].to_string().to_ascii_uppercase();
                let anchored = matches!(args[1], Expr::Value(Value::Number(_, _)));
                COARSE_DATE_PARTS.contains(&part.as_str()) && !anchored
            }
            _ => false,
        };

        if rejected {
            offending = Some(expr.to_string());
            return ControlFlow::Break(());
        }
        ControlFlow::Continue(())
    });

    if let Some(found) = offending {
        return Err(AppError::BadRequest(format!(
            "day-granularity date arithmetic is not allowed in generated queries \
             (dialects disagree on boundary counting); express the interval in \
             elapsed seconds and divide by the unit: {found}"
        )));
    }
    Ok(())
}

/// The positional argument expressions of a function call, in order.
///
/// Named and wildcard arguments are skipped — neither form appears in a date
/// difference, and skipping them keeps the positional indices meaningful.
fn plain_function_args(args: &FunctionArguments) -> Vec<&Expr> {
    let FunctionArguments::List(list) = args else {
        return Vec::new();
    };
    list.args
        .iter()
        .filter_map(|a| match a {
            FunctionArg::Unnamed(FunctionArgExpr::Expr(e)) => Some(e),
            _ => None,
        })
        .collect()
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

    // ── Dialect-quoted identifier tests (testing the trim_matches normalization) ──

    #[test]
    fn pg_double_quoted_allowed_table_passes() {
        // The IR compiler emits double-quoted identifiers for PG dialect.
        // validate_sql must accept them when the unquoted name is in allowed_tables.
        validate_sql(
            r#"SELECT COUNT(*) AS count FROM "encounters" LIMIT 500"#,
            SourceKind::Postgres,
            500,
            &tables(&["encounters"]),
        )
        .expect("double-quoted allowed table must pass");
    }

    #[test]
    fn mysql_backtick_quoted_allowed_table_passes() {
        validate_sql(
            "SELECT COUNT(*) AS count FROM `encounters` LIMIT 500",
            SourceKind::Mysql,
            500,
            &tables(&["encounters"]),
        )
        .expect("backtick-quoted allowed table must pass");
    }

    #[test]
    fn mssql_bracket_quoted_allowed_table_passes() {
        validate_sql(
            "SELECT COUNT(*) AS count FROM [encounters]",
            SourceKind::Mssql,
            500,
            &tables(&["encounters"]),
        )
        .expect("bracket-quoted allowed table must pass");
    }

    #[test]
    fn pg_double_quoted_disallowed_table_is_denied() {
        // Even with dialect quoting, a table not in allowed_tables must be rejected.
        let err = validate_sql(
            r#"SELECT COUNT(*) AS count FROM "billing_items" LIMIT 500"#,
            SourceKind::Postgres,
            500,
            &tables(&["encounters"]),  // billing_items not in allowed list
        )
        .expect_err("disallowed double-quoted table must be denied");
        assert!(matches!(err, AppError::BadRequest(_)));
    }

    #[test]
    fn mysql_backtick_disallowed_table_is_denied() {
        let err = validate_sql(
            "SELECT COUNT(*) AS count FROM `billing_items` LIMIT 500",
            SourceKind::Mysql,
            500,
            &tables(&["encounters"]),
        )
        .expect_err("disallowed backtick-quoted table must be denied");
        assert!(matches!(err, AppError::BadRequest(_)));
    }

    #[test]
    fn mssql_bracket_disallowed_table_is_denied() {
        let err = validate_sql(
            "SELECT COUNT(*) AS count FROM [billing_items]",
            SourceKind::Mssql,
            500,
            &tables(&["encounters"]),
        )
        .expect_err("disallowed bracket-quoted table must be denied");
        assert!(matches!(err, AppError::BadRequest(_)));
    }

    #[test]
    fn schema_qualified_table_is_denied_when_only_bare_table_allowed() {
        // "schema"."table" — the trim_matches only strips outermost quote chars, so
        // the inner `"."` remains and the result is not in the bare table allowed list.
        // This is the safe-fail-closed behavior: a qualified name requires an exact
        // match in allowed_tables (e.g. "public.encounters"), never falls back to the
        // bare table name.
        let result = validate_sql(
            r#"SELECT 1 FROM "public"."encounters" LIMIT 1"#,
            SourceKind::Postgres,
            1,
            &tables(&["encounters"]),  // "public.encounters" not listed — deny
        );
        // Expected: denied (schema qualification is not stripped).
        assert!(
            result.is_err(),
            "schema-qualified table must be denied when only bare name is allowed"
        );
    }
    // -----------------------------------------------------------------------
    // Day-granularity date arithmetic (plan 03g §2)
    // -----------------------------------------------------------------------

    /// SQL Server's `DATEDIFF(DAY, a, b)` counts date-boundary crossings, so a
    /// stay from 23:00 Monday to 01:00 Tuesday is reported as one day. The
    /// validator must recognise the expression and refuse it, not merely fail to
    /// recognise the function name.
    #[test]
    fn rejects_sqlserver_day_datediff() {
        let error = validate_sql(
            "SELECT AVG(DATEDIFF(DAY, admitted_at, discharged_at)) FROM admissions",
            SourceKind::Mssql,
            100,
            &tables(&["admissions"]),
        )
        .expect_err("native day difference must be rejected");
        assert!(error.to_string().contains("day-granularity"), "got: {error}");
    }

    /// The abbreviated part name is the same wrong answer.
    #[test]
    fn rejects_sqlserver_abbreviated_day_datediff() {
        let error = validate_sql(
            "SELECT AVG(DATEDIFF(DD, admitted_at, discharged_at)) FROM admissions",
            SourceKind::Mssql,
            100,
            &tables(&["admissions"]),
        )
        .expect_err("DD is DAY");
        assert!(error.to_string().contains("day-granularity"), "got: {error}");
    }

    /// MySQL's two-argument `DATEDIFF(end, start)` counts whole dates *and* takes
    /// its arguments end-first, so porting the SQL Server form by dropping the
    /// unit silently negates the answer as well as coarsening it.
    #[test]
    fn rejects_mysql_two_argument_datediff() {
        let error = validate_sql(
            "SELECT AVG(DATEDIFF(discharged_at, admitted_at)) FROM admissions",
            SourceKind::Mysql,
            100,
            &tables(&["admissions"]),
        )
        .expect_err("two-argument DATEDIFF must be rejected");
        assert!(error.to_string().contains("day-granularity"), "got: {error}");
    }

    /// MySQL `TIMESTAMPDIFF(DAY, …)` truncates whole units — the same interval
    /// comes back as 0 — so it is refused for the same reason.
    #[test]
    fn rejects_mysql_day_timestampdiff() {
        let error = validate_sql(
            "SELECT AVG(TIMESTAMPDIFF(DAY, admitted_at, discharged_at)) FROM admissions",
            SourceKind::Mysql,
            100,
            &tables(&["admissions"]),
        )
        .expect_err("truncating day difference must be rejected");
        assert!(error.to_string().contains("day-granularity"), "got: {error}");
    }

    /// The forms the IR actually emits are second-granularity, which every
    /// dialect agrees on, and must pass.
    #[test]
    fn allows_second_granularity_elapsed_time() {
        for (sql, dialect) in [
            (
                "SELECT AVG((EXTRACT(EPOCH FROM (discharged_at - admitted_at)) / 86400.0)) FROM admissions",
                SourceKind::Postgres,
            ),
            (
                "SELECT AVG((TIMESTAMPDIFF(SECOND, admitted_at, discharged_at) / 86400.0)) FROM admissions",
                SourceKind::Mysql,
            ),
            (
                "SELECT AVG((DATEDIFF_BIG(SECOND, admitted_at, discharged_at) / 86400.0)) FROM admissions",
                SourceKind::Mssql,
            ),
        ] {
            validate_sql(sql, dialect, 100, &tables(&["admissions"]))
                .unwrap_or_else(|e| panic!("{dialect:?} elapsed-seconds form rejected: {e}"));
        }
    }

    /// A coarse part is still legitimate for *bucket truncation*: SQL Server
    /// spells `date_trunc` as `DATEADD(WEEK, DATEDIFF(WEEK, 0, col), 0)`, which is
    /// not an elapsed-time measurement. The epoch anchor `0` in the start position
    /// is what distinguishes it, and the trend queries depend on it.
    #[test]
    fn allows_sqlserver_bucket_truncation_idiom() {
        for part in ["HOUR", "WEEK"] {
            let sql = format!(
                "SELECT DATEADD({part}, DATEDIFF({part}, 0, occurred_at), 0) AS bucket, \
                 COUNT(*) FROM encounters GROUP BY DATEADD({part}, DATEDIFF({part}, 0, occurred_at), 0)"
            );
            validate_sql(&sql, SourceKind::Mssql, 100, &tables(&["encounters"]))
                .unwrap_or_else(|e| panic!("{part} bucket truncation rejected: {e}"));
        }
    }

    /// The rule looks inside nested expressions, not just at the top level.
    #[test]
    fn rejects_day_datediff_nested_in_a_predicate() {
        let error = validate_sql(
            "SELECT id FROM admissions WHERE DATEDIFF(DAY, admitted_at, discharged_at) > 14",
            SourceKind::Mssql,
            100,
            &tables(&["admissions"]),
        )
        .expect_err("a nested day difference must be rejected too");
        assert!(error.to_string().contains("day-granularity"), "got: {error}");
    }
}
