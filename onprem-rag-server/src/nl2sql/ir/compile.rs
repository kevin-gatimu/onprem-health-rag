//! Dialect-specific SQL compiler for `QuerySpec`.
//!
//! # Responsibility
//! Takes a fully-bound `QuerySpec` (every `ColumnRef.physical` is `Some`) and
//! returns a dialect-correct SQL string plus a human-readable explanation sentence.
//!
//! # Correctness invariant
//! All literal values are inlined with `'` doubling — no user text is spliced
//! outside a literal. Every compiled statement must then pass through
//! `nl2sql::validate::validate_sql` before execution; the validator re-parses the
//! AST and re-injects the row cap.
//!
//! # Clock injection
//! `compile` takes `now: DateTime<Utc>` — never call `Utc::now()` inside this
//! module, or relative-range golden fixtures break the next day.

use chrono::{DateTime, Datelike, Duration, NaiveDate, Utc};
use thiserror::Error;

use crate::connectors::SourceKind;

use super::spec::{
    BucketUnit, ColumnRef, Dimension, Filter, FilterOp, FilterValue, JoinKind, JoinRef, Measure,
    MeasureOp, Order, OrderTarget, QuerySpec, Shape, SortDir, TimeRange, TimeScope,
};

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

/// A compiled, dialect-specific SQL statement ready for `validate_sql`.
#[derive(Debug, Clone)]
pub struct CompiledSql {
    /// The SQL SELECT statement (no trailing semicolon).
    pub sql: String,
    /// `true` — all values are inlined literals; no `?` / `$1` placeholders.
    pub params_inlined: bool,
    /// Human-readable sentence describing what the query computes.
    pub explanation: String,
}

/// Errors that prevent compilation.
#[derive(Debug, Error, Clone, PartialEq)]
pub enum CompileError {
    #[error("column {concept:?}.{role:?} has no physical binding — bind() must run first")]
    UnboundColumn {
        concept: String,
        role: String,
    },
    #[error("subject table is not bound — bind() must run first")]
    UnboundSubject,
    #[error("Median aggregate is not supported on MySQL; use PostgreSQL or SQL Server")]
    MedianOnMysql,
    #[error("a filter literal contains a blocked token that would be rejected by validate_sql")]
    BlockedLiteral,
    #[error("spec has no measures and shape requires at least one")]
    NoMeasure,
    #[error("a column reference used in a join has no physical binding")]
    UnboundJoin,
}

/// Compile a bound `QuerySpec` to dialect SQL.
///
/// `now` is used to expand relative time ranges (`LastMonth`, `WithinN`, …)
/// into ISO date literals — pass a frozen timestamp in tests, `Utc::now()` in
/// production so the SQL is self-contained provenance.
pub fn compile(
    spec: &QuerySpec,
    dialect: SourceKind,
    max_rows: i64,
    now: DateTime<Utc>,
) -> Result<CompiledSql, CompileError> {
    let ctx = CompileCtx { dialect, max_rows, now };
    ctx.compile(spec)
}

// ---------------------------------------------------------------------------
// Internal context
// ---------------------------------------------------------------------------

struct CompileCtx {
    dialect: SourceKind,
    max_rows: i64,
    now: DateTime<Utc>,
}

impl CompileCtx {
    fn compile(&self, spec: &QuerySpec) -> Result<CompiledSql, CompileError> {
        // The subject table must be bound.
        let subject_table = spec
            .subject
            .table
            .as_deref()
            .ok_or(CompileError::UnboundSubject)?;

        // Build alias map: subject → "t0", each join target → "t1", "t2", …
        // We track join aliases so column references can be qualified.
        let mut alias_map: Vec<(String, String)> = vec![(subject_table.to_string(), "t0".to_string())];
        for (i, join) in spec.joins.iter().enumerate() {
            if let Some(last_hop) = join.hops.last() {
                alias_map.push((last_hop.to_table.clone(), format!("t{}", i + 1)));
            }
        }

        // Resolve a ColumnRef to (alias, column_name) for qualified SQL.
        let resolve = |cr: &ColumnRef| -> Result<(String, String), CompileError> {
            let (tbl, col) = cr.physical.as_ref().ok_or_else(|| CompileError::UnboundColumn {
                concept: format!("{:?}", cr.concept),
                role: format!("{:?}", cr.role),
            })?;
            let alias = alias_map
                .iter()
                .find(|(t, _)| t.eq_ignore_ascii_case(tbl))
                .map(|(_, a)| a.clone())
                .unwrap_or_else(|| "t0".to_string());
            Ok((alias, col.clone()))
        };

        // Check no compiled literal contains a blocked token.
        // We do a pre-check on filter values; validate_sql will catch anything else.
        self.check_literals(spec)?;

        let sql = match spec.shape {
            Shape::Scalar | Shape::Grouped | Shape::TopN | Shape::Rate | Shape::Trend => {
                self.build_aggregate(spec, subject_table, &alias_map, &resolve)?
            }
            Shape::List => self.build_list(spec, subject_table, &alias_map, &resolve)?,
            Shape::Lookup => self.build_lookup(spec, subject_table, &alias_map, &resolve)?,
            Shape::Exists => self.build_exists(spec, subject_table, &alias_map, &resolve)?,
        };

        let explanation = self.build_explanation(spec);

        Ok(CompiledSql { sql, params_inlined: true, explanation })
    }

    // -----------------------------------------------------------------------
    // Aggregate shapes (Scalar, Grouped, TopN, Rate, Trend)
    // -----------------------------------------------------------------------

    fn build_aggregate(
        &self,
        spec: &QuerySpec,
        subject_table: &str,
        alias_map: &[(String, String)],
        resolve: &impl Fn(&ColumnRef) -> Result<(String, String), CompileError>,
    ) -> Result<String, CompileError> {
        // SELECT list
        let mut select_parts: Vec<String> = Vec::new();

        // Dimensions go first in GROUP BY shapes.
        for dim in &spec.dimensions {
            let (alias, col) = resolve(&dim.column)?;
            let dim_expr = if spec.shape == Shape::Trend {
                // Time bucket expression.
                let bucket = spec
                    .time
                    .as_ref()
                    .and_then(|t| t.bucket)
                    .unwrap_or(BucketUnit::Month);
                self.time_bucket_expr(&format!("{}.{}", alias, self.qi(&col)), bucket)
            } else {
                format!("{}.{}", alias, self.qi(&col))
            };
            let label = dim.label.replace(' ', "_").to_ascii_lowercase();
            let label = sanitize_alias(&label);
            select_parts.push(format!("{} AS {}", dim_expr, self.qi(&label)));
        }

        // Measures
        for measure in &spec.measures {
            let expr = self.measure_expr(measure, resolve)?;
            let alias = sanitize_alias(&measure.alias);
            select_parts.push(format!("{} AS {}", expr, self.qi(&alias)));
        }

        if select_parts.is_empty() {
            // Default: COUNT(*) when no measures specified and shape is Scalar.
            select_parts.push("COUNT(*) AS count".to_string());
        }

        // FROM + JOINs
        let from_clause = self.build_from(subject_table, alias_map, &spec.joins)?;

        // WHERE
        let where_clause = self.build_where(&spec.filters, spec.time.as_ref(), resolve)?;

        // GROUP BY
        let group_by = if spec.shape == Shape::Grouped || spec.shape == Shape::TopN
            || spec.shape == Shape::Trend || spec.shape == Shape::Rate
        {
            if !spec.dimensions.is_empty() {
                let mut parts = Vec::new();
                for dim in &spec.dimensions {
                    let (alias, col) = resolve(&dim.column)?;
                    let expr = if spec.shape == Shape::Trend {
                        let bucket = spec
                            .time
                            .as_ref()
                            .and_then(|t| t.bucket)
                            .unwrap_or(BucketUnit::Month);
                        self.time_bucket_expr(&format!("{}.{}", alias, self.qi(&col)), bucket)
                    } else {
                        format!("{}.{}", alias, self.qi(&col))
                    };
                    parts.push(expr);
                }
                format!(" GROUP BY {}", parts.join(", "))
            } else {
                String::new()
            }
        } else {
            String::new()
        };

        // ORDER BY
        let order_by = self.build_order_by(&spec.order, &spec.dimensions, resolve)?;

        // LIMIT / TOP
        let limit = spec.limit.map(|n| n as i64).unwrap_or(self.max_rows);
        let limit = limit.max(1).min(self.max_rows);

        let sql = match self.dialect {
            SourceKind::Mssql => {
                let top = if spec.shape == Shape::Scalar { String::new() } else {
                    format!("TOP {} ", limit)
                };
                format!(
                    "SELECT {top}{sel}{from}{whr}{grp}{ord}",
                    top = top,
                    sel = select_parts.join(", "),
                    from = from_clause,
                    whr = where_clause,
                    grp = group_by,
                    ord = order_by,
                )
            }
            SourceKind::Postgres | SourceKind::Mysql => {
                let limit_clause = if spec.shape == Shape::Scalar {
                    String::new()
                } else {
                    format!(" LIMIT {}", limit)
                };
                format!(
                    "SELECT {sel}{from}{whr}{grp}{ord}{lim}",
                    sel = select_parts.join(", "),
                    from = from_clause,
                    whr = where_clause,
                    grp = group_by,
                    ord = order_by,
                    lim = limit_clause,
                )
            }
        };

        Ok(sql)
    }

    fn measure_expr(
        &self,
        measure: &Measure,
        resolve: &impl Fn(&ColumnRef) -> Result<(String, String), CompileError>,
    ) -> Result<String, CompileError> {
        match &measure.op {
            MeasureOp::Count => Ok("COUNT(*)".to_string()),
            MeasureOp::CountDistinct => {
                let cr = measure.target.as_ref().ok_or(CompileError::NoMeasure)?;
                let (alias, col) = resolve(cr)?;
                Ok(format!("COUNT(DISTINCT {}.{})", alias, self.qi(&col)))
            }
            MeasureOp::Sum => {
                let cr = measure.target.as_ref().ok_or(CompileError::NoMeasure)?;
                let (alias, col) = resolve(cr)?;
                Ok(format!("SUM({}.{})", alias, self.qi(&col)))
            }
            MeasureOp::Avg => {
                let cr = measure.target.as_ref().ok_or(CompileError::NoMeasure)?;
                let (alias, col) = resolve(cr)?;
                Ok(format!("AVG({}.{})", alias, self.qi(&col)))
            }
            MeasureOp::Min => {
                let cr = measure.target.as_ref().ok_or(CompileError::NoMeasure)?;
                let (alias, col) = resolve(cr)?;
                Ok(format!("MIN({}.{})", alias, self.qi(&col)))
            }
            MeasureOp::Max => {
                let cr = measure.target.as_ref().ok_or(CompileError::NoMeasure)?;
                let (alias, col) = resolve(cr)?;
                Ok(format!("MAX({}.{})", alias, self.qi(&col)))
            }
            MeasureOp::Median => {
                if self.dialect == SourceKind::Mysql {
                    return Err(CompileError::MedianOnMysql);
                }
                let cr = measure.target.as_ref().ok_or(CompileError::NoMeasure)?;
                let (alias, col) = resolve(cr)?;
                Ok(format!(
                    "PERCENTILE_CONT(0.5) WITHIN GROUP (ORDER BY {}.{})",
                    alias,
                    self.qi(&col)
                ))
            }
            MeasureOp::Rate { numerator } => {
                let pred = self.filter_predicate(numerator, resolve)?;
                Ok(format!(
                    "100.0 * SUM(CASE WHEN {} THEN 1 ELSE 0 END) / NULLIF(COUNT(*), 0)",
                    pred
                ))
            }
        }
    }

    // -----------------------------------------------------------------------
    // List shape
    // -----------------------------------------------------------------------

    fn build_list(
        &self,
        spec: &QuerySpec,
        subject_table: &str,
        alias_map: &[(String, String)],
        resolve: &impl Fn(&ColumnRef) -> Result<(String, String), CompileError>,
    ) -> Result<String, CompileError> {
        let select_list = if spec.projection.is_empty() {
            "t0.*".to_string()
        } else {
            spec.projection
                .iter()
                .map(|cr| {
                    resolve(cr).map(|(alias, col)| format!("{}.{}", alias, self.qi(&col)))
                })
                .collect::<Result<Vec<_>, _>>()?
                .join(", ")
        };

        let from_clause = self.build_from(subject_table, alias_map, &spec.joins)?;
        let where_clause = self.build_where(&spec.filters, spec.time.as_ref(), resolve)?;
        let order_by = self.build_order_by(&spec.order, &spec.dimensions, resolve)?;
        let limit = spec.limit.map(|n| n as i64).unwrap_or(self.max_rows);
        let limit = limit.max(1).min(self.max_rows);

        Ok(match self.dialect {
            SourceKind::Mssql => format!(
                "SELECT TOP {limit} {sel}{from}{whr}{ord}",
                sel = select_list,
                from = from_clause,
                whr = where_clause,
                ord = order_by,
            ),
            SourceKind::Postgres | SourceKind::Mysql => format!(
                "SELECT {sel}{from}{whr}{ord} LIMIT {limit}",
                sel = select_list,
                from = from_clause,
                whr = where_clause,
                ord = order_by,
            ),
        })
    }

    // -----------------------------------------------------------------------
    // Lookup shape
    // -----------------------------------------------------------------------

    fn build_lookup(
        &self,
        spec: &QuerySpec,
        subject_table: &str,
        alias_map: &[(String, String)],
        resolve: &impl Fn(&ColumnRef) -> Result<(String, String), CompileError>,
    ) -> Result<String, CompileError> {
        self.build_list(spec, subject_table, alias_map, resolve)
    }

    // -----------------------------------------------------------------------
    // Exists shape
    // -----------------------------------------------------------------------

    fn build_exists(
        &self,
        spec: &QuerySpec,
        subject_table: &str,
        alias_map: &[(String, String)],
        resolve: &impl Fn(&ColumnRef) -> Result<(String, String), CompileError>,
    ) -> Result<String, CompileError> {
        let from_clause = self.build_from(subject_table, alias_map, &spec.joins)?;
        let where_clause = self.build_where(&spec.filters, spec.time.as_ref(), resolve)?;

        Ok(format!(
            "SELECT CASE WHEN EXISTS (SELECT 1{from}{whr}) THEN 1 ELSE 0 END AS exists_flag",
            from = from_clause,
            whr = where_clause,
        ))
    }

    // -----------------------------------------------------------------------
    // FROM + JOIN clauses
    // -----------------------------------------------------------------------

    fn build_from(
        &self,
        subject_table: &str,
        alias_map: &[(String, String)],
        joins: &[JoinRef],
    ) -> Result<String, CompileError> {
        let subject_alias = alias_map
            .iter()
            .find(|(t, _)| t.eq_ignore_ascii_case(subject_table))
            .map(|(_, a)| a.as_str())
            .unwrap_or("t0");

        let mut sql = format!(" FROM {} AS {}", self.qt(subject_table), subject_alias);

        for (i, join) in joins.iter().enumerate() {
            let join_alias = format!("t{}", i + 1);
            let join_kw = match join.kind {
                JoinKind::Inner => "JOIN",
                JoinKind::Left => "LEFT JOIN",
            };
            for hop in &join.hops {
                let from_alias = alias_map
                    .iter()
                    .find(|(t, _)| t.eq_ignore_ascii_case(&hop.from_table))
                    .map(|(_, a)| a.as_str())
                    .unwrap_or("t0");
                let hop_join = format!(
                    " {} {} AS {} ON {}.{} = {}.{}",
                    join_kw,
                    self.qt(&hop.to_table),
                    join_alias,
                    from_alias,
                    self.qi(&hop.join_col),
                    join_alias,
                    self.qi(&hop.via_col),
                );
                sql.push_str(&hop_join);
            }
        }

        Ok(sql)
    }

    // -----------------------------------------------------------------------
    // WHERE clause
    // -----------------------------------------------------------------------

    fn build_where(
        &self,
        filters: &[Filter],
        time: Option<&TimeScope>,
        resolve: &impl Fn(&ColumnRef) -> Result<(String, String), CompileError>,
    ) -> Result<String, CompileError> {
        let mut preds: Vec<String> = Vec::new();

        for f in filters {
            let pred = self.filter_predicate(f, resolve)?;
            preds.push(pred);
        }

        // Time range filter.
        if let Some(ts) = time {
            if let Some(range) = &ts.range {
                let (alias, col) = resolve(&ts.column)?;
                let col_expr = format!("{}.{}", alias, self.qi(&col));
                let range_pred = self.time_range_pred(&col_expr, range);
                preds.push(range_pred);
            }
        }

        if preds.is_empty() {
            Ok(String::new())
        } else {
            Ok(format!(" WHERE {}", preds.join(" AND ")))
        }
    }

    fn filter_predicate(
        &self,
        f: &Filter,
        resolve: &impl Fn(&ColumnRef) -> Result<(String, String), CompileError>,
    ) -> Result<String, CompileError> {
        let (alias, col) = resolve(&f.column)?;
        let col_expr = format!("{}.{}", alias, self.qi(&col));

        let pred = match &f.op {
            FilterOp::Eq => format!("{} = {}", col_expr, self.literal(&f.value)?),
            FilterOp::Ne => format!("{} <> {}", col_expr, self.literal(&f.value)?),
            FilterOp::Gt => format!("{} > {}", col_expr, self.literal(&f.value)?),
            FilterOp::Gte => format!("{} >= {}", col_expr, self.literal(&f.value)?),
            FilterOp::Lt => format!("{} < {}", col_expr, self.literal(&f.value)?),
            FilterOp::Lte => format!("{} <= {}", col_expr, self.literal(&f.value)?),
            FilterOp::Like => format!("{} LIKE {}", col_expr, self.literal(&f.value)?),
            FilterOp::ILike => match self.dialect {
                SourceKind::Postgres => format!("{} ILIKE {}", col_expr, self.literal(&f.value)?),
                _ => format!("LOWER({}) LIKE LOWER({})", col_expr, self.literal(&f.value)?),
            },
            FilterOp::IsNull => format!("{} IS NULL", col_expr),
            FilterOp::IsNotNull => format!("{} IS NOT NULL", col_expr),
            FilterOp::IsTrue => format!("{} IS TRUE", col_expr),
            FilterOp::IsFalse => format!("{} IS FALSE", col_expr),
            FilterOp::In => {
                let FilterValue::List(items) = &f.value else {
                    return Ok(format!("{} = {}", col_expr, self.literal(&f.value)?));
                };
                let lits: Vec<String> = items
                    .iter()
                    .map(|v| self.literal(v))
                    .collect::<Result<Vec<_>, _>>()?;
                format!("{} IN ({})", col_expr, lits.join(", "))
            }
            FilterOp::Between => {
                let FilterValue::Range(lo, hi) = &f.value else {
                    return Ok(format!("{} = {}", col_expr, self.literal(&f.value)?));
                };
                format!("{} BETWEEN {} AND {}", col_expr, self.literal(lo)?, self.literal(hi)?)
            }
        };

        Ok(pred)
    }

    // -----------------------------------------------------------------------
    // ORDER BY
    // -----------------------------------------------------------------------

    fn build_order_by(
        &self,
        orders: &[Order],
        dimensions: &[Dimension],
        resolve: &impl Fn(&ColumnRef) -> Result<(String, String), CompileError>,
    ) -> Result<String, CompileError> {
        if orders.is_empty() {
            return Ok(String::new());
        }

        let mut parts = Vec::new();
        for ord in orders {
            let dir = match ord.dir {
                SortDir::Asc => "ASC",
                SortDir::Desc => "DESC",
            };
            let expr = match &ord.target {
                OrderTarget::Measure(alias) => self.qi(&sanitize_alias(alias)),
                OrderTarget::Column(cr) => {
                    let (alias, col) = resolve(cr)?;
                    format!("{}.{}", alias, self.qi(&col))
                }
            };
            parts.push(format!("{} {}", expr, dir));
        }

        Ok(format!(" ORDER BY {}", parts.join(", ")))
    }

    // -----------------------------------------------------------------------
    // Time bucket expression (for Trend shape)
    // -----------------------------------------------------------------------

    fn time_bucket_expr(&self, col_expr: &str, unit: BucketUnit) -> String {
        match self.dialect {
            SourceKind::Postgres => {
                format!("DATE_TRUNC('{}', {})", unit.as_str(), col_expr)
            }
            SourceKind::Mysql => match unit {
                BucketUnit::Hour => format!("DATE_FORMAT({}, '%Y-%m-%d %H:00:00')", col_expr),
                BucketUnit::Day => format!("DATE({})", col_expr),
                BucketUnit::Week => format!("DATE_FORMAT({}, '%x-%v')", col_expr),
                BucketUnit::Month => format!("DATE_FORMAT({}, '%Y-%m-01')", col_expr),
                BucketUnit::Quarter => {
                    format!(
                        "CONCAT(YEAR({}), '-Q', QUARTER({}))",
                        col_expr, col_expr
                    )
                }
                BucketUnit::Year => format!("DATE_FORMAT({}, '%Y-01-01')", col_expr),
            },
            SourceKind::Mssql => match unit {
                BucketUnit::Hour => format!("DATEADD(HOUR, DATEDIFF(HOUR, 0, {}), 0)", col_expr),
                BucketUnit::Day => format!("CAST({} AS DATE)", col_expr),
                BucketUnit::Week => format!("DATEADD(WEEK, DATEDIFF(WEEK, 0, {}), 0)", col_expr),
                BucketUnit::Month => {
                    format!("DATEFROMPARTS(YEAR({}), MONTH({}), 1)", col_expr, col_expr)
                }
                BucketUnit::Quarter => {
                    format!(
                        "DATEFROMPARTS(YEAR({0}), (MONTH({0})-1)/3*3+1, 1)",
                        col_expr
                    )
                }
                BucketUnit::Year => format!("DATEFROMPARTS(YEAR({}), 1, 1)", col_expr),
            },
        }
    }

    // -----------------------------------------------------------------------
    // Time range predicate
    // -----------------------------------------------------------------------

    fn time_range_pred(&self, col_expr: &str, range: &TimeRange) -> String {
        let now_date = self.now.date_naive();

        match range {
            TimeRange::Today => {
                let d = now_date.to_string();
                format!("{} >= '{}' AND {} < '{}'", col_expr, d, col_expr,
                    (now_date + Duration::days(1)).to_string())
            }
            TimeRange::Yesterday => {
                let d = (now_date - Duration::days(1)).to_string();
                format!("{} >= '{}' AND {} < '{}'", col_expr, d, col_expr, now_date)
            }
            TimeRange::ThisWeek => {
                let wd = now_date.weekday().num_days_from_monday() as i64;
                let start = now_date - Duration::days(wd);
                format!("{} >= '{}'", col_expr, start)
            }
            TimeRange::LastWeek => {
                let wd = now_date.weekday().num_days_from_monday() as i64;
                let this_start = now_date - Duration::days(wd);
                let last_start = this_start - Duration::weeks(1);
                format!(
                    "{} >= '{}' AND {} < '{}'",
                    col_expr, last_start, col_expr, this_start
                )
            }
            TimeRange::ThisMonth => {
                let start = NaiveDate::from_ymd_opt(now_date.year(), now_date.month(), 1)
                    .unwrap_or(now_date);
                format!("{} >= '{}'", col_expr, start)
            }
            TimeRange::LastMonth => {
                let (prev_year, prev_month) = if now_date.month() == 1 {
                    (now_date.year() - 1, 12u32)
                } else {
                    (now_date.year(), now_date.month() - 1)
                };
                let start = NaiveDate::from_ymd_opt(prev_year, prev_month, 1).unwrap_or(now_date);
                let end = NaiveDate::from_ymd_opt(now_date.year(), now_date.month(), 1)
                    .unwrap_or(now_date);
                format!(
                    "{} >= '{}' AND {} < '{}'",
                    col_expr, start, col_expr, end
                )
            }
            TimeRange::ThisQuarter => {
                let q = (now_date.month() - 1) / 3;
                let q_start_month = q * 3 + 1;
                let start =
                    NaiveDate::from_ymd_opt(now_date.year(), q_start_month, 1).unwrap_or(now_date);
                format!("{} >= '{}'", col_expr, start)
            }
            TimeRange::LastQuarter => {
                let q = (now_date.month() - 1) / 3;
                let (prev_year, prev_q_month) = if q == 0 {
                    (now_date.year() - 1, 10u32)
                } else {
                    (now_date.year(), q * 3 - 2)
                };
                let start = NaiveDate::from_ymd_opt(prev_year, prev_q_month, 1).unwrap_or(now_date);
                let this_q_month = q * 3 + 1;
                let end =
                    NaiveDate::from_ymd_opt(now_date.year(), this_q_month, 1).unwrap_or(now_date);
                format!(
                    "{} >= '{}' AND {} < '{}'",
                    col_expr, start, col_expr, end
                )
            }
            TimeRange::ThisYear => {
                let start = NaiveDate::from_ymd_opt(now_date.year(), 1, 1).unwrap_or(now_date);
                format!("{} >= '{}'", col_expr, start)
            }
            TimeRange::LastYear => {
                let start =
                    NaiveDate::from_ymd_opt(now_date.year() - 1, 1, 1).unwrap_or(now_date);
                let end = NaiveDate::from_ymd_opt(now_date.year(), 1, 1).unwrap_or(now_date);
                format!(
                    "{} >= '{}' AND {} < '{}'",
                    col_expr, start, col_expr, end
                )
            }
            TimeRange::Last { n, unit } => {
                let d = self.subtract_duration(now_date, *n, *unit);
                format!("{} >= '{}'", col_expr, d)
            }
            TimeRange::Next { n, unit } => {
                let d = self.add_duration(now_date, *n, *unit);
                format!("{} >= '{}' AND {} <= '{}'", col_expr, now_date, col_expr, d)
            }
            TimeRange::Within { n, unit } => {
                let d = self.add_duration(now_date, *n, *unit);
                format!("{} >= '{}' AND {} <= '{}'", col_expr, now_date, col_expr, d)
            }
            TimeRange::Absolute { lo, hi } => {
                let lo_lit = self.literal(lo).unwrap_or_else(|_| "NULL".to_string());
                let hi_lit = self.literal(hi).unwrap_or_else(|_| "NULL".to_string());
                format!("{} BETWEEN {} AND {}", col_expr, lo_lit, hi_lit)
            }
        }
    }

    fn subtract_duration(&self, from: NaiveDate, n: u32, unit: BucketUnit) -> NaiveDate {
        match unit {
            BucketUnit::Hour | BucketUnit::Day => from - Duration::days(n as i64),
            BucketUnit::Week => from - Duration::weeks(n as i64),
            BucketUnit::Month => {
                let total_months = from.year() * 12 + from.month() as i32 - n as i32;
                let year = (total_months - 1) / 12;
                let month = ((total_months - 1) % 12 + 12) % 12 + 1;
                NaiveDate::from_ymd_opt(year, month as u32, 1).unwrap_or(from)
            }
            BucketUnit::Quarter => {
                let total_months = from.year() * 12 + from.month() as i32 - (n as i32 * 3);
                let year = (total_months - 1) / 12;
                let month = ((total_months - 1) % 12 + 12) % 12 + 1;
                NaiveDate::from_ymd_opt(year, month as u32, 1).unwrap_or(from)
            }
            BucketUnit::Year => {
                NaiveDate::from_ymd_opt(from.year() - n as i32, from.month(), 1).unwrap_or(from)
            }
        }
    }

    fn add_duration(&self, from: NaiveDate, n: u32, unit: BucketUnit) -> NaiveDate {
        match unit {
            BucketUnit::Hour | BucketUnit::Day => from + Duration::days(n as i64),
            BucketUnit::Week => from + Duration::weeks(n as i64),
            BucketUnit::Month => {
                let total_months = from.year() * 12 + from.month() as i32 + n as i32;
                let year = (total_months - 1) / 12;
                let month = ((total_months - 1) % 12) + 1;
                NaiveDate::from_ymd_opt(year, month as u32, 1).unwrap_or(from)
            }
            BucketUnit::Quarter => {
                let total_months = from.year() * 12 + from.month() as i32 + (n as i32 * 3);
                let year = (total_months - 1) / 12;
                let month = ((total_months - 1) % 12) + 1;
                NaiveDate::from_ymd_opt(year, month as u32, 1).unwrap_or(from)
            }
            BucketUnit::Year => {
                NaiveDate::from_ymd_opt(from.year() + n as i32, from.month(), 1).unwrap_or(from)
            }
        }
    }

    // -----------------------------------------------------------------------
    // Literal values
    // -----------------------------------------------------------------------

    fn literal(&self, v: &FilterValue) -> Result<String, CompileError> {
        let s = match v {
            FilterValue::Str(s) => format!("'{}'", s.replace('\'', "''")),
            FilterValue::Num(n) => {
                if n.fract() == 0.0 {
                    format!("{}", *n as i64)
                } else {
                    format!("{}", n)
                }
            }
            FilterValue::Bool(b) => {
                if *b { "TRUE".to_string() } else { "FALSE".to_string() }
            }
            FilterValue::Date(d) => format!("'{}'", d),
            FilterValue::Ts(ts) => format!("'{}'", ts.format("%Y-%m-%dT%H:%M:%S")),
            FilterValue::Param(p) => format!(":{}", p),
            FilterValue::List(items) => {
                let parts: Result<Vec<_>, _> = items.iter().map(|i| self.literal(i)).collect();
                parts?.join(", ")
            }
            FilterValue::Range(lo, hi) => {
                format!("{} AND {}", self.literal(lo)?, self.literal(hi)?)
            }
        };
        Ok(s)
    }

    // -----------------------------------------------------------------------
    // Blocked literal check (brief §0.5)
    // -----------------------------------------------------------------------

    fn check_literals(&self, spec: &QuerySpec) -> Result<(), CompileError> {
        let blocked = [
            "PG_SLEEP", "SLEEP(", "WAITFOR", "BENCHMARK(", "LOAD_FILE(",
            "INTO OUTFILE", "INTO DUMPFILE", "XP_CMDSHELL", "SP_EXECUTESQL",
            "EXEC(", "EXECUTE(", "OPENROWSET(", "OPENDATASOURCE(", "DBCC",
        ];
        for f in &spec.filters {
            let s = format!("{:?}", &f.value).to_uppercase();
            for tok in &blocked {
                if s.contains(tok) {
                    return Err(CompileError::BlockedLiteral);
                }
            }
        }
        Ok(())
    }

    // -----------------------------------------------------------------------
    // Identifier / table quoting
    // -----------------------------------------------------------------------

    /// Quote an identifier per dialect.
    fn qi(&self, name: &str) -> String {
        match self.dialect {
            SourceKind::Postgres => format!("\"{}\"", name.replace('"', "\"\"")),
            SourceKind::Mysql => format!("`{}`", name.replace('`', "``")),
            SourceKind::Mssql => format!("[{}]", name.replace(']', "]]")),
        }
    }

    /// Quote a table (may contain a schema prefix like `dbo.my_table`).
    fn qt(&self, table: &str) -> String {
        if let Some((schema, tbl)) = table.split_once('.') {
            format!("{}.{}", self.qi(schema), self.qi(tbl))
        } else {
            self.qi(table)
        }
    }

    // -----------------------------------------------------------------------
    // Explanation sentence
    // -----------------------------------------------------------------------

    fn build_explanation(&self, spec: &QuerySpec) -> String {
        let shape_word = match spec.shape {
            Shape::Scalar => "Counted",
            Shape::Grouped => "Grouped",
            Shape::Trend => "Trended",
            Shape::TopN => "Ranked",
            Shape::List => "Listed",
            Shape::Lookup => "Retrieved",
            Shape::Rate => "Calculated rate for",
            Shape::Exists => "Checked existence of",
        };

        let subject = spec
            .subject
            .table
            .as_deref()
            .or_else(|| Some(spec.subject.concept.slug()))
            .unwrap_or("records");

        let mut parts = vec![format!("{} {}", shape_word, subject)];

        for f in &spec.filters {
            let col = f
                .column
                .physical
                .as_ref()
                .map(|(_, c)| c.as_str())
                .unwrap_or("field");
            let val = match &f.value {
                FilterValue::Str(s) => format!("= {}", s),
                FilterValue::Num(n) => format!("= {}", n),
                FilterValue::Bool(b) => format!("= {}", b),
                _ => String::new(),
            };
            if !val.is_empty() {
                parts.push(format!("{} {}", col, val));
            }
        }

        if let Some(ts) = &spec.time {
            if let Some(range) = &ts.range {
                let range_str = match range {
                    TimeRange::LastMonth => "last month".to_string(),
                    TimeRange::ThisMonth => "this month".to_string(),
                    TimeRange::LastYear => "last year".to_string(),
                    TimeRange::ThisYear => "this year".to_string(),
                    TimeRange::LastQuarter => "last quarter".to_string(),
                    TimeRange::ThisQuarter => "this quarter".to_string(),
                    TimeRange::Today => "today".to_string(),
                    TimeRange::Yesterday => "yesterday".to_string(),
                    TimeRange::Last { n, unit } => format!("last {} {}s", n, unit.as_str()),
                    TimeRange::Next { n, unit } => format!("next {} {}s", n, unit.as_str()),
                    TimeRange::Within { n, unit } => format!("within {} {}s", n, unit.as_str()),
                    _ => String::new(),
                };
                if !range_str.is_empty() {
                    parts.push(range_str);
                }
            }
        }

        if !spec.dimensions.is_empty() {
            let dim_labels: Vec<&str> =
                spec.dimensions.iter().map(|d| d.label.as_str()).collect();
            parts.push(format!("grouped by {}", dim_labels.join(", ")));
        }

        format!("{}.", parts.join(", "))
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Strip non-identifier characters from an alias.
fn sanitize_alias(s: &str) -> String {
    s.chars()
        .map(|c| if c.is_ascii_alphanumeric() || c == '_' { c } else { '_' })
        .collect()
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use chrono::TimeZone;

    use crate::ontology::binding::JoinHop;
    use crate::ontology::concepts::EntityConcept;
    use crate::ontology::roles::ColumnRole;

    use super::super::spec::*;
    use super::*;

    fn frozen_now() -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 9, 5, 12, 0, 0).unwrap()
    }

    fn make_count_spec(table: &str, concept: EntityConcept) -> QuerySpec {
        QuerySpec {
            subject: Subject { concept, table: Some(table.to_string()) },
            shape: Shape::Scalar,
            measures: vec![Measure {
                op: MeasureOp::Count,
                target: None,
                alias: "count".to_string(),
            }],
            dimensions: vec![],
            filters: vec![],
            time: None,
            order: vec![],
            limit: None,
            joins: vec![],
            projection: vec![],
            provenance: SpecProvenance { rule: "R1".into(), focus_subs: vec![] },
        }
    }

    #[test]
    fn compiles_simple_count_postgres() {
        let spec = make_count_spec("patients", EntityConcept::Patient);
        let result = compile(&spec, SourceKind::Postgres, 500, frozen_now()).unwrap();
        assert!(result.sql.starts_with("SELECT COUNT(*) AS"), "got: {}", result.sql);
        assert!(result.sql.contains("FROM \"patients\""), "got: {}", result.sql);
        assert!(result.params_inlined);
    }

    #[test]
    fn compiles_simple_count_mysql() {
        let spec = make_count_spec("patients", EntityConcept::Patient);
        let result = compile(&spec, SourceKind::Mysql, 500, frozen_now()).unwrap();
        assert!(result.sql.contains("FROM `patients`"), "got: {}", result.sql);
    }

    #[test]
    fn compiles_simple_count_mssql() {
        let spec = make_count_spec("patients", EntityConcept::Patient);
        let result = compile(&spec, SourceKind::Mssql, 500, frozen_now()).unwrap();
        assert!(result.sql.contains("FROM [patients]"), "got: {}", result.sql);
    }

    #[test]
    fn compiles_count_with_filter() {
        let mut spec = make_count_spec("encounters", EntityConcept::Encounter);
        spec.filters = vec![Filter {
            column: ColumnRef::bound(
                "encounters", "status", EntityConcept::Encounter, ColumnRole::Status,
            ),
            op: FilterOp::Eq,
            value: FilterValue::Str("active".to_string()),
        }];
        let result = compile(&spec, SourceKind::Postgres, 500, frozen_now()).unwrap();
        assert!(result.sql.contains("WHERE"), "got: {}", result.sql);
        assert!(result.sql.contains("'active'"), "got: {}", result.sql);
    }

    #[test]
    fn compiles_grouped_count() {
        let spec = QuerySpec {
            subject: Subject {
                concept: EntityConcept::Encounter,
                table: Some("encounters".to_string()),
            },
            shape: Shape::Grouped,
            measures: vec![Measure {
                op: MeasureOp::Count,
                target: None,
                alias: "count".to_string(),
            }],
            dimensions: vec![Dimension {
                column: ColumnRef::bound(
                    "encounters", "status", EntityConcept::Encounter, ColumnRole::Status,
                ),
                label: "status".to_string(),
            }],
            filters: vec![],
            time: None,
            order: vec![Order {
                target: OrderTarget::Measure("count".to_string()),
                dir: SortDir::Desc,
            }],
            limit: None,
            joins: vec![],
            projection: vec![],
            provenance: SpecProvenance { rule: "R1".into(), focus_subs: vec![] },
        };
        let result = compile(&spec, SourceKind::Postgres, 500, frozen_now()).unwrap();
        assert!(result.sql.contains("GROUP BY"), "got: {}", result.sql);
        assert!(result.sql.contains("ORDER BY"), "got: {}", result.sql);
    }

    #[test]
    fn median_on_mysql_is_error() {
        let spec = QuerySpec {
            subject: Subject { concept: EntityConcept::VitalSign, table: Some("vital_signs".to_string()) },
            shape: Shape::Scalar,
            measures: vec![Measure {
                op: MeasureOp::Median,
                target: Some(ColumnRef::bound(
                    "vital_signs", "weight", EntityConcept::VitalSign, ColumnRole::Measure,
                )),
                alias: "median_weight".to_string(),
            }],
            dimensions: vec![],
            filters: vec![],
            time: None,
            order: vec![],
            limit: None,
            joins: vec![],
            projection: vec![],
            provenance: SpecProvenance { rule: "R3".into(), focus_subs: vec![] },
        };
        let err = compile(&spec, SourceKind::Mysql, 500, frozen_now()).unwrap_err();
        assert_eq!(err, CompileError::MedianOnMysql);
    }

    #[test]
    fn compiles_rate() {
        let spec = QuerySpec {
            subject: Subject { concept: EntityConcept::Appointment, table: Some("appointments".to_string()) },
            shape: Shape::Rate,
            measures: vec![Measure {
                op: MeasureOp::Rate {
                    numerator: Box::new(Filter {
                        column: ColumnRef::bound(
                            "appointments", "status", EntityConcept::Appointment, ColumnRole::Status,
                        ),
                        op: FilterOp::Eq,
                        value: FilterValue::Str("no_show".to_string()),
                    }),
                },
                target: None,
                alias: "no_show_rate".to_string(),
            }],
            dimensions: vec![],
            filters: vec![],
            time: None,
            order: vec![],
            limit: None,
            joins: vec![],
            projection: vec![],
            provenance: SpecProvenance { rule: "R7".into(), focus_subs: vec![] },
        };
        let result = compile(&spec, SourceKind::Postgres, 500, frozen_now()).unwrap();
        assert!(result.sql.contains("CASE WHEN"), "got: {}", result.sql);
        assert!(result.sql.contains("NULLIF(COUNT(*), 0)"), "got: {}", result.sql);
    }

    #[test]
    fn last_month_range_is_deterministic() {
        // frozen 2026-09-05 → last month = August 2026
        let spec = QuerySpec {
            subject: Subject { concept: EntityConcept::Delivery, table: Some("deliveries".to_string()) },
            shape: Shape::Scalar,
            measures: vec![Measure { op: MeasureOp::Count, target: None, alias: "count".to_string() }],
            dimensions: vec![],
            filters: vec![],
            time: Some(TimeScope {
                column: ColumnRef::bound(
                    "deliveries", "delivered_at", EntityConcept::Delivery, ColumnRole::EventTime,
                ),
                range: Some(TimeRange::LastMonth),
                bucket: None,
            }),
            order: vec![],
            limit: None,
            joins: vec![],
            projection: vec![],
            provenance: SpecProvenance { rule: "R1".into(), focus_subs: vec![] },
        };
        let result = compile(&spec, SourceKind::Postgres, 500, frozen_now()).unwrap();
        assert!(result.sql.contains("'2026-08-01'"), "got: {}", result.sql);
        assert!(result.sql.contains("'2026-09-01'"), "got: {}", result.sql);
    }

    #[test]
    fn unbound_column_is_error() {
        let spec = QuerySpec {
            subject: Subject { concept: EntityConcept::Patient, table: Some("patients".to_string()) },
            shape: Shape::Scalar,
            measures: vec![Measure {
                op: MeasureOp::Sum,
                target: Some(ColumnRef::logical(EntityConcept::Patient, ColumnRole::Amount)),
                alias: "total".to_string(),
            }],
            dimensions: vec![],
            filters: vec![],
            time: None,
            order: vec![],
            limit: None,
            joins: vec![],
            projection: vec![],
            provenance: SpecProvenance { rule: "R2".into(), focus_subs: vec![] },
        };
        let err = compile(&spec, SourceKind::Postgres, 500, frozen_now()).unwrap_err();
        assert!(matches!(err, CompileError::UnboundColumn { .. }));
    }

    #[test]
    fn blocked_literal_returns_error() {
        let spec = QuerySpec {
            subject: Subject { concept: EntityConcept::Patient, table: Some("patients".to_string()) },
            shape: Shape::Scalar,
            measures: vec![Measure { op: MeasureOp::Count, target: None, alias: "count".to_string() }],
            dimensions: vec![],
            filters: vec![Filter {
                column: ColumnRef::bound(
                    "patients", "status", EntityConcept::Patient, ColumnRole::Status,
                ),
                op: FilterOp::Eq,
                value: FilterValue::Str("pg_sleep(10)".to_string()),
            }],
            time: None,
            order: vec![],
            limit: None,
            joins: vec![],
            projection: vec![],
            provenance: SpecProvenance { rule: "R1".into(), focus_subs: vec![] },
        };
        let err = compile(&spec, SourceKind::Postgres, 500, frozen_now()).unwrap_err();
        assert_eq!(err, CompileError::BlockedLiteral);
    }

    #[test]
    fn list_shape_applies_limit() {
        let spec = QuerySpec {
            subject: Subject { concept: EntityConcept::Patient, table: Some("patients".to_string()) },
            shape: Shape::List,
            measures: vec![],
            dimensions: vec![],
            filters: vec![],
            time: None,
            order: vec![],
            limit: None,
            joins: vec![],
            projection: vec![],
            provenance: SpecProvenance { rule: "R8".into(), focus_subs: vec![] },
        };
        let result = compile(&spec, SourceKind::Postgres, 100, frozen_now()).unwrap();
        assert!(result.sql.ends_with("LIMIT 100"), "got: {}", result.sql);
    }

    #[test]
    fn compile_validates_through_sqlparser() {
        use crate::nl2sql::validate::validate_sql;

        let spec = make_count_spec("encounters", EntityConcept::Encounter);
        let compiled = compile(&spec, SourceKind::Postgres, 500, frozen_now()).unwrap();
        let validated = validate_sql(
            &compiled.sql,
            SourceKind::Postgres,
            500,
            &["encounters".to_string()],
        )
        .expect("compiled SQL must validate");
        // Post-validate string is what golden fixtures assert.
        assert!(!validated.sql.is_empty());
    }
}
