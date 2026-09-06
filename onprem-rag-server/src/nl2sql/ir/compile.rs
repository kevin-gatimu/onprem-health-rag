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
    BucketUnit, ColumnRef, DerivedDuration, Dimension, DurationFilter, Filter, FilterOp,
    FilterValue, JoinKind, JoinRef, Measure, MeasureOp, OpenInterval, Order, OrderTarget, QuerySpec,
    RelatedScope, Shape, SortDir, TimeRange, ValueExpr,
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

        // WHERE (main filters + reverse-FK EXISTS/NOT EXISTS subqueries)
        let where_clause = {
            let base = self.build_where(spec, resolve)?;
            let related = self.build_related_clauses(&spec.related, "t0")?;
            match (base.is_empty(), related.is_empty()) {
                (_, true) => base,
                (true, false) => format!(" WHERE {}", related),
                (false, false) => format!("{} AND {}", base, related),
            }
        };

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

    // -----------------------------------------------------------------------
    // Derived temporal measures (plan 03g §2)
    // -----------------------------------------------------------------------

    /// Elapsed **seconds** between two timestamp expressions, per dialect.
    ///
    /// This is the single arithmetic primitive for every derived duration. It
    /// exists because the dialects' native day-difference functions do not agree
    /// and none of them means "elapsed time" (plan 03g §2):
    ///
    /// * SQL Server `DATEDIFF(DAY, a, b)` counts *date-boundary crossings*, so
    ///   23:00 Monday → 01:00 Tuesday is `1` — a two-hour stay reported as a day.
    /// * MySQL `DATEDIFF(a, b)` also counts date boundaries **and takes its
    ///   arguments end-first**, so the naive port silently returns a negative
    ///   length of stay. `TIMESTAMPDIFF(DAY, …)` truncates to whole days, so the
    ///   same two-hour stay becomes `0`.
    /// * Postgres `b - a` yields an `interval`, which is neither of the above.
    ///
    /// Normalising to seconds first and dividing by the unit afterwards makes all
    /// three dialects return the *same real number* for the same two instants.
    /// Every call site passes `(start, end)` in that order, and the division is
    /// by a floating literal so SQL Server's integer division cannot truncate.
    fn elapsed_seconds_expr(&self, start: &str, end: &str) -> String {
        match self.dialect {
            SourceKind::Postgres => format!("EXTRACT(EPOCH FROM ({} - {}))", end, start),
            SourceKind::Mysql => format!("TIMESTAMPDIFF(SECOND, {}, {})", start, end),
            // DATEDIFF_BIG, not DATEDIFF: a seconds difference overflows a 32-bit
            // int at ~68 years, which a birth-date-to-now interval reaches.
            SourceKind::Mssql => format!("DATEDIFF_BIG(SECOND, {}, {})", start, end),
        }
    }

    /// The timestamp literal standing for "now" in `OpenInterval::AsOfNow`.
    ///
    /// Frozen from the compile-time clock rather than emitted as `NOW()` /
    /// `GETDATE()` so the SQL is self-contained provenance and identical across
    /// dialects — the same reason `time_range_pred` inlines date literals.
    fn now_ts_literal(&self) -> String {
        format!("'{}'", self.now.format("%Y-%m-%d %H:%M:%S"))
    }

    /// A derived duration as a scalar SQL expression, in `d.unit`.
    ///
    /// Note what is *not* here: no `ABS`, no `GREATEST(…, 0)`, no filter on the
    /// sign. A negative duration means `end < start` in the data — a real
    /// data-quality problem the caller must be able to see (plan 03g §3). The
    /// expression must not crash on one, and must not hide one either.
    fn duration_expr(
        &self,
        d: &DerivedDuration,
        resolve: &impl Fn(&ColumnRef) -> Result<(String, String), CompileError>,
    ) -> Result<String, CompileError> {
        let (sa, sc) = resolve(&d.start)?;
        let start = format!("{}.{}", sa, self.qi(&sc));
        let (ea, ec) = resolve(&d.end)?;
        let end_col = format!("{}.{}", ea, self.qi(&ec));
        let end = match d.open {
            OpenInterval::CompletedOnly => end_col,
            // An open interval is measured up to the frozen clock. COALESCE, not
            // a NULL-dropping aggregate: the row stays in the population.
            OpenInterval::AsOfNow => {
                format!("COALESCE({}, {})", end_col, self.now_ts_literal())
            }
        };
        Ok(format!(
            "({} / {}.0)",
            self.elapsed_seconds_expr(&start, &end),
            d.unit.seconds()
        ))
    }

    /// Row-level guards that make the open-interval choice explicit in the SQL.
    ///
    /// `CompletedOnly` must *say* it excludes open intervals rather than relying
    /// on `AVG` skipping NULLs — the silent version is exactly the plan 03g §3
    /// trap ("average stay of patients who already left", biased short).
    /// `AsOfNow` still requires a start; only the end may be open.
    fn duration_guards(
        &self,
        d: &DerivedDuration,
        resolve: &impl Fn(&ColumnRef) -> Result<(String, String), CompileError>,
    ) -> Result<Vec<String>, CompileError> {
        let (sa, sc) = resolve(&d.start)?;
        let mut preds = vec![format!("{}.{} IS NOT NULL", sa, self.qi(&sc))];
        if d.open == OpenInterval::CompletedOnly {
            let (ea, ec) = resolve(&d.end)?;
            preds.push(format!("{}.{} IS NOT NULL", ea, self.qi(&ec)));
        }
        Ok(preds)
    }

    /// A `ValueExpr` as a scalar SQL expression — a stored column or a derived
    /// duration. Aggregates, projections, thresholds and ORDER BY all go through
    /// this one function so the arithmetic cannot diverge between them.
    fn value_expr(
        &self,
        v: &ValueExpr,
        resolve: &impl Fn(&ColumnRef) -> Result<(String, String), CompileError>,
    ) -> Result<String, CompileError> {
        match v {
            ValueExpr::Column(cr) => {
                let (alias, col) = resolve(cr)?;
                Ok(format!("{}.{}", alias, self.qi(&col)))
            }
            ValueExpr::Duration(d) => self.duration_expr(d, resolve),
        }
    }

    /// Every derived duration mentioned anywhere in the spec, so `build_where`
    /// can emit its open-interval guards exactly once per distinct interval.
    fn spec_durations(spec: &QuerySpec) -> Vec<&DerivedDuration> {
        let mut out: Vec<&DerivedDuration> = Vec::new();
        for m in &spec.measures {
            if let Some(d) = m.target.as_ref().and_then(ValueExpr::as_duration) {
                out.push(d);
            }
        }
        for p in &spec.projection {
            if let Some(d) = p.as_duration() {
                out.push(d);
            }
        }
        for df in &spec.duration_filters {
            out.push(&df.duration);
        }
        out
    }

    fn measure_expr(
        &self,
        measure: &Measure,
        resolve: &impl Fn(&ColumnRef) -> Result<(String, String), CompileError>,
    ) -> Result<String, CompileError> {
        // Every non-Count aggregate takes the same scalar operand expression, so
        // a derived duration is aggregated by exactly the same code path as a
        // stored column — there is no second arithmetic implementation to drift.
        let operand = || -> Result<String, CompileError> {
            let target = measure.target.as_ref().ok_or(CompileError::NoMeasure)?;
            self.value_expr(target, resolve)
        };

        match &measure.op {
            MeasureOp::Count => Ok("COUNT(*)".to_string()),
            MeasureOp::CountDistinct => Ok(format!("COUNT(DISTINCT {})", operand()?)),
            MeasureOp::Sum => Ok(format!("SUM({})", operand()?)),
            MeasureOp::Avg => Ok(format!("AVG({})", operand()?)),
            MeasureOp::Min => Ok(format!("MIN({})", operand()?)),
            MeasureOp::Max => Ok(format!("MAX({})", operand()?)),
            MeasureOp::Median => {
                if self.dialect == SourceKind::Mysql {
                    return Err(CompileError::MedianOnMysql);
                }
                Ok(format!(
                    "PERCENTILE_CONT(0.5) WITHIN GROUP (ORDER BY {})",
                    operand()?
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
            // A projected duration needs an alias, because the expression is
            // arithmetic and the alias is what names the unit to the reader
            // (plan 03g §1). A projected column keeps its own name.
            spec.projection
                .iter()
                .map(|v| match v {
                    ValueExpr::Column(cr) => {
                        resolve(cr).map(|(alias, col)| format!("{}.{}", alias, self.qi(&col)))
                    }
                    ValueExpr::Duration(d) => self.duration_expr(d, resolve).map(|e| {
                        format!("{} AS {}", e, self.qi(&sanitize_alias(&d.default_alias())))
                    }),
                })
                .collect::<Result<Vec<_>, _>>()?
                .join(", ")
        };

        let from_clause = self.build_from(subject_table, alias_map, &spec.joins)?;
        let where_clause = {
            let base = self.build_where(spec, resolve)?;
            let related = self.build_related_clauses(&spec.related, "t0")?;
            match (base.is_empty(), related.is_empty()) {
                (_, true) => base,
                (true, false) => format!(" WHERE {}", related),
                (false, false) => format!("{} AND {}", base, related),
            }
        };
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
        let where_clause = {
            let base = self.build_where(spec, resolve)?;
            let related = self.build_related_clauses(&spec.related, "t0")?;
            match (base.is_empty(), related.is_empty()) {
                (_, true) => base,
                (true, false) => format!(" WHERE {}", related),
                (false, false) => format!("{} AND {}", base, related),
            }
        };

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
        spec: &QuerySpec,
        resolve: &impl Fn(&ColumnRef) -> Result<(String, String), CompileError>,
    ) -> Result<String, CompileError> {
        let mut preds: Vec<String> = Vec::new();

        for f in &spec.filters {
            let pred = self.filter_predicate(f, resolve)?;
            preds.push(pred);
        }

        // Open-interval guards for every derived duration in the spec, emitted
        // before the thresholds so the SQL reads as "these rows have a measurable
        // interval, and of those …" (plan 03g §3).
        for d in Self::spec_durations(spec) {
            for g in self.duration_guards(d, resolve)? {
                if !preds.contains(&g) {
                    preds.push(g);
                }
            }
        }

        // Row-level thresholds on a derived duration ("stays over 14 days").
        for df in &spec.duration_filters {
            preds.push(self.duration_filter_predicate(df, resolve)?);
        }

        // Time range filter.
        if let Some(ts) = &spec.time {
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

    // -----------------------------------------------------------------------
    // Related-scope predicates (EXISTS / NOT EXISTS subqueries, plan 03f)
    // -----------------------------------------------------------------------

    /// Build the EXISTS / NOT EXISTS predicates for all `RelatedScope`s and
    /// return them as a string ready to be appended to the WHERE clause.
    ///
    /// Each scope emits:
    /// ```sql
    /// EXISTS (SELECT 1 FROM child AS s0
    ///         WHERE s0.fk_col = t0.pk_col AND s0.filter_col = 'val')
    /// ```
    /// or `NOT EXISTS` when `rs.negated` is `true`.
    ///
    /// Returns an empty string when `related` is empty so callers can
    /// concatenate unconditionally.
    fn build_related_clauses(
        &self,
        related: &[RelatedScope],
        anchor_alias: &str,
    ) -> Result<String, CompileError> {
        if related.is_empty() {
            return Ok(String::new());
        }
        let mut parts: Vec<String> = Vec::new();
        for (i, rs) in related.iter().enumerate() {
            let physical = rs.physical.as_ref().ok_or(CompileError::UnboundSubject)?;
            let sq_alias = format!("s{}", i);

            // Closure resolving any ColumnRef in the child table to sq_alias.col.
            let child_resolve = |cr: &ColumnRef| -> Result<(String, String), CompileError> {
                let (_, col) = cr.physical.as_ref().ok_or_else(|| CompileError::UnboundColumn {
                    concept: format!("{:?}", cr.concept),
                    role: format!("{:?}", cr.role),
                })?;
                Ok((sq_alias.clone(), col.clone()))
            };

            // FK join condition: child.fk_col = anchor.pk_col
            let fk_pred = format!(
                "{}.{} = {}.{}",
                sq_alias,
                self.qi(&physical.child_fk_col),
                anchor_alias,
                self.qi(&physical.anchor_pk_col),
            );
            let mut inner_preds = vec![fk_pred];

            // Filter predicates inside the subquery.
            for f in &rs.filters {
                inner_preds.push(self.filter_predicate(f, &child_resolve)?);
            }

            // Optional time filter on the child.
            if let Some(ts) = &rs.time {
                if let Some(range) = &ts.range {
                    let (_, col) = ts.column.physical.as_ref().ok_or_else(|| {
                        CompileError::UnboundColumn {
                            concept: format!("{:?}", ts.column.concept),
                            role: format!("{:?}", ts.column.role),
                        }
                    })?;
                    let col_expr = format!("{}.{}", sq_alias, self.qi(col));
                    inner_preds.push(self.time_range_pred(&col_expr, range));
                }
            }

            let inner_where = format!(" WHERE {}", inner_preds.join(" AND "));
            let kw = if rs.negated { "NOT EXISTS" } else { "EXISTS" };
            parts.push(format!(
                "{}(SELECT 1 FROM {} AS {}{})",
                kw,
                self.qt(&physical.child_table),
                sq_alias,
                inner_where,
            ));
        }
        Ok(parts.join(" AND "))
    }

    /// A threshold on a derived duration ("stays over 14 days").
    ///
    /// The comparison value is in `df.duration.unit`, and the left-hand side is
    /// the duration expression in that same unit, so the SQL contains the number
    /// the question said (`14`) rather than a normalised `20160` minutes — the
    /// literal stays auditable against the question (plan 03g §1).
    fn duration_filter_predicate(
        &self,
        df: &DurationFilter,
        resolve: &impl Fn(&ColumnRef) -> Result<(String, String), CompileError>,
    ) -> Result<String, CompileError> {
        let lhs = self.duration_expr(&df.duration, resolve)?;
        let pred = match &df.op {
            FilterOp::Gt => format!("{} > {}", lhs, self.literal(&df.value)?),
            FilterOp::Gte => format!("{} >= {}", lhs, self.literal(&df.value)?),
            FilterOp::Lt => format!("{} < {}", lhs, self.literal(&df.value)?),
            FilterOp::Lte => format!("{} <= {}", lhs, self.literal(&df.value)?),
            FilterOp::Eq => format!("{} = {}", lhs, self.literal(&df.value)?),
            FilterOp::Ne => format!("{} <> {}", lhs, self.literal(&df.value)?),
            FilterOp::Between => {
                let FilterValue::Range(lo, hi) = &df.value else {
                    return Err(CompileError::NoMeasure);
                };
                format!("{} BETWEEN {} AND {}", lhs, self.literal(lo)?, self.literal(hi)?)
            }
            // A duration is a number; string/null/boolean operators are not
            // expressible on one. Refuse rather than emit something else
            // (defect-closure brief §1).
            _ => return Err(CompileError::NoMeasure),
        };
        Ok(pred)
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
                let end = start + Duration::weeks(1);
                format!("{} >= '{}' AND {} < '{}'", col_expr, start, col_expr, end)
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
            // Bounded "expires/due this <period>" — full range [start, end).
            // Distinct from ThisWeek/ThisMonth/ThisQuarter (lower bound only).
            TimeRange::WithinThisWeek => {
                let wd = now_date.weekday().num_days_from_monday() as i64;
                let start = now_date - Duration::days(wd);
                let end = start + Duration::weeks(1);
                format!("{} >= '{}' AND {} < '{}'", col_expr, start, col_expr, end)
            }
            TimeRange::WithinThisMonth => {
                let start = NaiveDate::from_ymd_opt(now_date.year(), now_date.month(), 1)
                    .unwrap_or(now_date);
                let (end_year, end_month) = if now_date.month() == 12 {
                    (now_date.year() + 1, 1u32)
                } else {
                    (now_date.year(), now_date.month() + 1)
                };
                let end = NaiveDate::from_ymd_opt(end_year, end_month, 1).unwrap_or(now_date);
                format!("{} >= '{}' AND {} < '{}'", col_expr, start, col_expr, end)
            }
            TimeRange::WithinThisQuarter => {
                let q = (now_date.month() - 1) / 3;
                let q_start_month = q * 3 + 1;
                let start = NaiveDate::from_ymd_opt(now_date.year(), q_start_month, 1)
                    .unwrap_or(now_date);
                let (end_year, end_month) = if q_start_month + 3 > 12 {
                    (now_date.year() + 1, 1u32)
                } else {
                    (now_date.year(), q_start_month + 3)
                };
                let end = NaiveDate::from_ymd_opt(end_year, end_month, 1).unwrap_or(now_date);
                format!("{} >= '{}' AND {} < '{}'", col_expr, start, col_expr, end)
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
        let values = spec
            .filters
            .iter()
            .map(|f| &f.value)
            .chain(spec.duration_filters.iter().map(|df| &df.value));
        for v in values {
            let s = format!("{:?}", v).to_uppercase();
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
                    TimeRange::WithinThisWeek => "expiring this week".to_string(),
                    TimeRange::WithinThisMonth => "expiring this month".to_string(),
                    TimeRange::WithinThisQuarter => "expiring this quarter".to_string(),
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

        // A derived duration must narrate its unit and its open-interval choice,
        // otherwise "average length of stay = 3.4" is unreadable and the excluded
        // population is invisible (plan 03g §1, §3).
        let mut said: Vec<String> = Vec::new();
        for d in Self::spec_durations(spec) {
            let sentence = format!(
                "elapsed time in {} between {} and {}, {}",
                d.unit.as_str(),
                d.start
                    .physical
                    .as_ref()
                    .map(|(_, c)| c.as_str())
                    .unwrap_or("interval start"),
                d.end
                    .physical
                    .as_ref()
                    .map(|(_, c)| c.as_str())
                    .unwrap_or("interval end"),
                d.open.narration(),
            );
            if !said.contains(&sentence) {
                said.push(sentence);
            }
        }
        parts.extend(said);

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
            related: vec![],
            duration_filters: vec![],
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
            related: vec![],
            duration_filters: vec![],
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
                target: Some(ValueExpr::Column(ColumnRef::bound(
                    "vital_signs", "weight", EntityConcept::VitalSign, ColumnRole::Measure,
                ))),
                alias: "median_weight".to_string(),
            }],
            dimensions: vec![],
            filters: vec![],
            time: None,
            order: vec![],
            limit: None,
            joins: vec![],
            projection: vec![],
            related: vec![],
            duration_filters: vec![],
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
            related: vec![],
            duration_filters: vec![],
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
            related: vec![],
            duration_filters: vec![],
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
                target: Some(ValueExpr::Column(ColumnRef::logical(EntityConcept::Patient, ColumnRole::Amount))),
                alias: "total".to_string(),
            }],
            dimensions: vec![],
            filters: vec![],
            time: None,
            order: vec![],
            limit: None,
            joins: vec![],
            projection: vec![],
            related: vec![],
            duration_filters: vec![],
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
            related: vec![],
            duration_filters: vec![],
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
            related: vec![],
            duration_filters: vec![],
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

    // -----------------------------------------------------------------------
    // Row-multiplication guard (plan 03f §2.4)
    // -----------------------------------------------------------------------

    /// Build a fully-bound COUNT spec for Patient with one RelatedScope on
    /// Appointment (simulating "How many patients missed an appointment?").
    ///
    /// Physical setup: anchor=patients(id), child=appointments(patient_id→id),
    /// filter: status IN ('no_show').
    ///
    /// If the related scope were emitted as an INNER JOIN instead of EXISTS,
    /// a patient with TWO no-show appointments would be counted twice — the
    /// definition of row multiplication.  This test asserts the compiled SQL
    /// uses EXISTS, which prevents that.  See the companion test below for the
    /// "teeth check".
    fn make_count_with_related_scope() -> QuerySpec {
        QuerySpec {
            subject: Subject {
                concept: EntityConcept::Patient,
                table: Some("patients".to_string()),
            },
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
            duration_filters: vec![],
            related: vec![RelatedScope {
                concept: EntityConcept::Appointment,
                filters: vec![Filter {
                    column: ColumnRef {
                        concept: EntityConcept::Appointment,
                        role: ColumnRole::Status,
                        name_hint: None,
                        physical: Some(("appointments".to_string(), "status".to_string())),
                    },
                    op: FilterOp::In,
                    value: FilterValue::List(vec![FilterValue::Str("no_show".to_string())]),
                }],
                time: None,
                negated: false,
                physical: Some(super::super::spec::RelatedPhysical {
                    child_table: "appointments".to_string(),
                    child_fk_col: "patient_id".to_string(),
                    anchor_pk_col: "id".to_string(),
                }),
            }],
            provenance: SpecProvenance { rule: "R1".into(), focus_subs: vec![] },
        }
    }

    /// Guard: the related scope compiles to EXISTS (not a multiplying INNER JOIN).
    ///
    /// A patient with TWO no-show appointments must count as ONE row.
    /// EXISTS is the correct form; INNER JOIN would multiply.
    ///
    /// To verify this test has teeth (plan 03f requirement):
    ///   1. This test currently passes because the compiler emits EXISTS.
    ///   2. We temporarily changed the compiler to emit INNER JOIN (see commit
    ///      history or `build_related_clauses` in compile.rs — the kw line),
    ///      confirmed this test FAILED, then reverted.
    #[test]
    fn related_scope_uses_exists_not_join_pg() {
        let spec = make_count_with_related_scope();
        let result = compile(&spec, SourceKind::Postgres, 500, frozen_now()).unwrap();
        // Must contain EXISTS subquery (allow "EXISTS(" or "EXISTS (").
        assert!(
            result.sql.contains("EXISTS(SELECT 1 FROM") || result.sql.contains("EXISTS (SELECT 1 FROM"),
            "expected EXISTS subquery, got: {}",
            result.sql
        );
        // Must NOT join to appointments as a multiplying INNER JOIN.
        // A bare JOIN or INNER JOIN to the child table would multiply rows.
        assert!(
            !result.sql.to_ascii_uppercase().contains("JOIN \"APPOINTMENTS\"")
            && !result.sql.to_ascii_uppercase().contains("JOIN `APPOINTMENTS`")
            && !result.sql.to_ascii_uppercase().contains("JOIN [APPOINTMENTS]"),
            "expected no outer JOIN to appointments, got: {}",
            result.sql
        );
    }

    #[test]
    fn related_scope_uses_exists_not_join_mysql() {
        let spec = make_count_with_related_scope();
        let result = compile(&spec, SourceKind::Mysql, 500, frozen_now()).unwrap();
        assert!(
            result.sql.contains("EXISTS(SELECT 1 FROM") || result.sql.contains("EXISTS (SELECT 1 FROM"),
            "expected EXISTS subquery, got: {}",
            result.sql
        );
        assert!(
            !result.sql.to_ascii_uppercase().contains("JOIN `APPOINTMENTS`"),
            "got: {}",
            result.sql
        );
    }

    #[test]
    fn related_scope_uses_exists_not_join_mssql() {
        let spec = make_count_with_related_scope();
        let result = compile(&spec, SourceKind::Mssql, 500, frozen_now()).unwrap();
        assert!(
            result.sql.contains("EXISTS(SELECT 1 FROM") || result.sql.contains("EXISTS (SELECT 1 FROM"),
            "expected EXISTS subquery, got: {}",
            result.sql
        );
        assert!(
            !result.sql.to_ascii_uppercase().contains("JOIN [APPOINTMENTS]"),
            "got: {}",
            result.sql
        );
    }

    /// Confirm the EXISTS sql passes validate_sql (child table must be in allowed_tables).
    #[test]
    fn related_scope_exists_validates() {
        use crate::nl2sql::validate::validate_sql;
        let spec = make_count_with_related_scope();
        let compiled = compile(&spec, SourceKind::Postgres, 500, frozen_now()).unwrap();
        let validated = validate_sql(
            &compiled.sql,
            SourceKind::Postgres,
            500,
            &["patients".to_string(), "appointments".to_string()],
        )
        .expect("EXISTS subquery SQL must pass validate_sql");
        assert!(!validated.sql.is_empty());
    }
    // -----------------------------------------------------------------------
    // Derived temporal measures (plan 03g)
    // -----------------------------------------------------------------------

    /// Start of the interval every cross-dialect test below measures: 23:00.
    ///
    /// The interval is 2026-01-12 23:00 → 2026-01-13 01:00 — it crosses midnight
    /// but spans only two hours. That choice is the whole point of the test. A
    /// clean multi-day span (say 09:00 Monday → 09:00 Thursday) is 3 days under
    /// elapsed-seconds arithmetic, 3 under SQL Server's boundary counting and 3
    /// under MySQL's truncation, so it agrees under every wrong implementation
    /// and proves nothing. Two hours across midnight separates all three: the
    /// true answer is 2/24 of a day (0.08333…), boundary counting says 1 (a
    /// twelvefold overstatement) and whole-unit truncation says 0.
    fn iv_start() -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 1, 12, 23, 0, 0).unwrap()
    }

    /// End of that interval: 01:00 the next calendar day.
    fn iv_end() -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 1, 13, 1, 0, 0).unwrap()
    }

    fn los_duration(unit: DurationUnit, open: OpenInterval) -> DerivedDuration {
        DerivedDuration {
            start: ColumnRef::bound(
                "admissions", "admission_date", EntityConcept::Admission, ColumnRole::StartTime,
            ),
            end: ColumnRef::bound(
                "admissions", "discharge_date", EntityConcept::Admission, ColumnRole::EndTime,
            ),
            unit,
            open,
        }
    }

    fn make_avg_duration_spec(unit: DurationUnit, open: OpenInterval) -> QuerySpec {
        QuerySpec {
            subject: Subject {
                concept: EntityConcept::Admission,
                table: Some("admissions".to_string()),
            },
            shape: Shape::Scalar,
            measures: vec![Measure {
                op: MeasureOp::Avg,
                target: Some(ValueExpr::Duration(los_duration(unit, open))),
                alias: format!("avg_los_{}", unit.as_str()),
            }],
            dimensions: vec![],
            filters: vec![],
            time: None,
            order: vec![],
            limit: None,
            joins: vec![],
            projection: vec![],
            related: vec![],
            duration_filters: vec![],
            provenance: SpecProvenance { rule: "R3".into(), focus_subs: vec![] },
        }
    }

    /// The argument of the outermost `AVG(...)` in `sql`, with parens balanced.
    fn avg_arg(sql: &str) -> String {
        let at = sql.find("AVG(").unwrap_or_else(|| panic!("no AVG( in {sql}")) + "AVG(".len();
        let mut depth = 1usize;
        let mut out = String::new();
        for ch in sql[at..].chars() {
            match ch {
                '(' => depth += 1,
                ')' => {
                    depth -= 1;
                    if depth == 0 {
                        return out;
                    }
                }
                _ => {}
            }
            out.push(ch);
        }
        panic!("unbalanced AVG( in {sql}");
    }

    /// Seconds in one unit of a SQL date part, including SQL Server's spellings.
    fn part_seconds(part: &str) -> f64 {
        match part {
            "SECOND" | "SS" | "S" => 1.0,
            "MINUTE" | "MI" | "N" => 60.0,
            "HOUR" | "HH" => 3_600.0,
            "DAY" | "DD" | "D" => 86_400.0,
            "WEEK" | "WK" | "WW" => 604_800.0,
            other => panic!("unmodelled date part: {other}"),
        }
    }

    /// How many boundaries of `part` lie between the two instants — the thing
    /// SQL Server's `DATEDIFF` actually counts.
    fn boundary_count(part: &str, start: DateTime<Utc>, end: DateTime<Utc>) -> f64 {
        let per = part_seconds(part) as i64;
        (end.timestamp().div_euclid(per) - start.timestamp().div_euclid(per)) as f64
    }

    /// The leading date-part argument of a diff call, if it has one.
    ///
    /// `None` distinguishes MySQL's two-argument `DATEDIFF(end, start)`, whose
    /// first argument is a column rather than a part name.
    fn date_part_of(upper_func: &str) -> Option<String> {
        let open = upper_func.find('(')? + 1;
        let first = upper_func[open..].split(',').next()?.trim().to_string();
        if !first.is_empty() && first.chars().all(|c| c.is_ascii_alphabetic() || c == '_') {
            Some(first)
        } else {
            None
        }
    }

    /// A literal model of the number each engine would return for one emitted
    /// duration expression, given the two timestamps a row holds.
    ///
    /// This is how the cross-dialect test gets teeth without a database — PHI
    /// stays on-prem, so no test opens a connection. Each arm encodes the
    /// documented semantics of one function *including the wrong ones*: if
    /// `elapsed_seconds_expr` is ever changed to emit a native day difference,
    /// this evaluator faithfully reports the wrong number that dialect would
    /// produce and the assertions below fail. An unrecognised form panics rather
    /// than being waved through, so a novel emission cannot pass by default.
    fn model_eval(
        expr: &str,
        start_col: &str,
        end_col: &str,
        start: DateTime<Utc>,
        end: DateTime<Utc>,
    ) -> f64 {
        let trimmed = expr.trim();
        let inner = trimmed
            .strip_prefix('(')
            .and_then(|s| s.strip_suffix(')'))
            .unwrap_or(trimmed);
        let slash = inner
            .rfind(" / ")
            .unwrap_or_else(|| panic!("duration expression must divide by its unit: {inner}"));
        let func = inner[..slash].trim();
        let divisor: f64 = inner[slash + 3..]
            .trim()
            .parse()
            .unwrap_or_else(|_| panic!("unit divisor must be a numeric literal: {inner}"));

        let si = func
            .find(start_col)
            .unwrap_or_else(|| panic!("start column {start_col} missing from {func}"));
        let ei = func
            .find(end_col)
            .unwrap_or_else(|| panic!("end column {end_col} missing from {func}"));
        let start_first = si < ei;
        let secs = (end - start).num_seconds() as f64;
        let signed = if start_first { secs } else { -secs };

        let upper = func.to_ascii_uppercase();
        let name = upper[..upper.find('(').unwrap()].to_string();
        let raw = match name.as_str() {
            // Postgres subtracts intervals and EXTRACTs seconds. The minuend
            // comes first, so seeing the *start* first means the operands are
            // the wrong way round and the result is negative.
            "EXTRACT" => -signed,
            "TIMESTAMPDIFF" | "DATEDIFF" | "DATEDIFF_BIG" => match date_part_of(&upper) {
                // The one granularity every dialect agrees on.
                Some(p) if p == "SECOND" => signed,
                // MySQL truncates towards zero at the named part …
                Some(p) if name == "TIMESTAMPDIFF" => (signed / part_seconds(&p)).trunc(),
                // … whereas SQL Server counts the part's boundary crossings.
                Some(p) => boundary_count(&p, start, end) * if start_first { 1.0 } else { -1.0 },
                // Two-argument MySQL `DATEDIFF(end, start)`: whole dates, and
                // end-first, so start-first here means a negated answer.
                None => boundary_count("DAY", start, end) * if start_first { -1.0 } else { 1.0 },
            },
            other => panic!("unmodelled duration form {other}: {func}"),
        };
        raw / divisor
    }

    /// Plan 03g §2: the three dialects must return the *same* number for the
    /// same interval, and it must be the true elapsed fraction.
    #[test]
    fn duration_agrees_across_dialects_on_a_midnight_crossing_interval() {
        let spec = make_avg_duration_spec(DurationUnit::Days, OpenInterval::CompletedOnly);
        // Two hours of a 24-hour day.
        let expected = 2.0 / 24.0;
        let mut seen: Vec<(SourceKind, f64, String)> = Vec::new();

        for dialect in [SourceKind::Postgres, SourceKind::Mysql, SourceKind::Mssql] {
            let sql = compile(&spec, dialect, 500, frozen_now()).unwrap().sql;
            let expr = avg_arg(&sql);
            let v = model_eval(&expr, "admission_date", "discharge_date", iv_start(), iv_end());
            assert!(
                (v - expected).abs() < 1e-9,
                "{dialect:?} computes {v} days for a 23:00→01:00 stay, expected {expected}. \
                 1 means boundary counting, 0 means whole-unit truncation. expr: {expr}",
            );
            seen.push((dialect, v, expr));
        }

        let (_, first, ref first_expr) = seen[0];
        for (dialect, v, expr) in &seen {
            assert!(
                (v - first).abs() < 1e-12,
                "{dialect:?} disagrees with the first dialect: {v} vs {first} \
                 ({expr} vs {first_expr})",
            );
        }
    }

    /// Plan 03g §2: the endpoint order differs per dialect (MySQL and SQL Server
    /// take `(unit, start, end)`; Postgres subtracts `end - start`), and getting
    /// it backwards yields a negative length of stay rather than an error. Assert
    /// positivity for each dialect, and pin the literal order that produces it.
    #[test]
    fn duration_argument_order_is_start_then_end_per_dialect() {
        let spec = make_avg_duration_spec(DurationUnit::Hours, OpenInterval::CompletedOnly);
        let mut exprs: Vec<(SourceKind, String)> = Vec::new();

        for dialect in [SourceKind::Postgres, SourceKind::Mysql, SourceKind::Mssql] {
            let sql = compile(&spec, dialect, 500, frozen_now()).unwrap().sql;
            let expr = avg_arg(&sql);
            let v = model_eval(&expr, "admission_date", "discharge_date", iv_start(), iv_end());
            assert!(
                v > 0.0,
                "{dialect:?} produced a negative duration ({v}) — the endpoints are \
                 reversed: {expr}",
            );
            assert!((v - 2.0).abs() < 1e-9, "{dialect:?} expected 2 hours, got {v}: {expr}");
            exprs.push((dialect, expr));
        }

        let pick = |k: SourceKind| -> String {
            exprs.iter().find(|(d, _)| *d == k).unwrap().1.clone()
        };
        let pg = pick(SourceKind::Postgres);
        assert!(
            pg.contains(r#"EXTRACT(EPOCH FROM (t0."discharge_date" - t0."admission_date"))"#),
            "postgres must subtract start from end: {pg}",
        );
        let my = pick(SourceKind::Mysql);
        assert!(
            my.contains("TIMESTAMPDIFF(SECOND, t0.`admission_date`, t0.`discharge_date`)"),
            "mysql must pass (unit, start, end): {my}",
        );
        let ms = pick(SourceKind::Mssql);
        assert!(
            ms.contains("DATEDIFF_BIG(SECOND, t0.[admission_date], t0.[discharge_date])"),
            "sql server must pass (unit, start, end): {ms}",
        );
    }

    /// Plan 03g §1: the unit is carried in the IR, divides the elapsed seconds,
    /// and is named in the emitted alias. An unlabelled duration column is the
    /// 24×-error hazard.
    #[test]
    fn duration_unit_divides_seconds_and_names_the_alias() {
        for (unit, divisor, alias) in [
            (DurationUnit::Days, "86400.0", "avg_los_days"),
            (DurationUnit::Hours, "3600.0", "avg_los_hours"),
            (DurationUnit::Minutes, "60.0", "avg_los_minutes"),
        ] {
            let spec = make_avg_duration_spec(unit, OpenInterval::CompletedOnly);
            let sql = compile(&spec, SourceKind::Postgres, 500, frozen_now()).unwrap().sql;
            assert!(sql.contains(&format!("/ {divisor}")), "expected /{divisor} in {sql}");
            assert!(sql.contains(&format!("AS \"{alias}\"")), "expected alias {alias} in {sql}");
        }
    }

    /// Plan 03g §3: `CompletedOnly` must *say* it excludes open intervals. If the
    /// exclusion were left to `AVG` skipping NULLs, the answer would silently be
    /// "average stay of patients who already left", biased short.
    #[test]
    fn completed_only_states_its_exclusion_in_the_where_clause() {
        let spec = make_avg_duration_spec(DurationUnit::Days, OpenInterval::CompletedOnly);
        let sql = compile(&spec, SourceKind::Postgres, 500, frozen_now()).unwrap().sql;
        assert!(sql.contains(r#"t0."admission_date" IS NOT NULL"#), "got: {sql}");
        assert!(sql.contains(r#"t0."discharge_date" IS NOT NULL"#), "got: {sql}");
        assert!(!sql.contains("COALESCE"), "completed-only must not fabricate an end: {sql}");
    }

    /// Plan 03g §3: the other reading — open intervals measured up to the frozen
    /// clock — keeps those rows in the population instead of dropping them.
    #[test]
    fn as_of_now_measures_open_intervals_to_the_frozen_clock() {
        let spec = make_avg_duration_spec(DurationUnit::Days, OpenInterval::AsOfNow);
        let sql = compile(&spec, SourceKind::Postgres, 500, frozen_now()).unwrap().sql;
        assert!(
            sql.contains(r#"COALESCE(t0."discharge_date", '2026-09-05 12:00:00')"#),
            "open end must fall back to the frozen now: {sql}",
        );
        assert!(sql.contains(r#"t0."admission_date" IS NOT NULL"#), "got: {sql}");
        assert!(
            !sql.contains(r#"t0."discharge_date" IS NOT NULL"#),
            "as-of-now must not exclude the still-admitted rows it exists to include: {sql}",
        );
    }

    /// Plan 03g §3: a negative duration is a data-quality fact about the row, not
    /// something the measure may hide. No ABS, no GREATEST, no sign predicate —
    /// and no crash either, since the expression is plain arithmetic.
    #[test]
    fn negative_durations_are_neither_clamped_nor_filtered() {
        for dialect in [SourceKind::Postgres, SourceKind::Mysql, SourceKind::Mssql] {
            let spec = make_avg_duration_spec(DurationUnit::Days, OpenInterval::CompletedOnly);
            let sql = compile(&spec, dialect, 500, frozen_now()).unwrap().sql.to_ascii_uppercase();
            for banned in ["ABS(", "GREATEST(", "IIF(", "> 0", ">= 0"] {
                assert!(!sql.contains(banned), "{dialect:?} hides negative durations ({banned}): {sql}");
            }
        }
        // And the model confirms the arithmetic simply reports the negative
        // number when the row's timestamps are out of order.
        let spec = make_avg_duration_spec(DurationUnit::Hours, OpenInterval::CompletedOnly);
        let expr = avg_arg(&compile(&spec, SourceKind::Postgres, 500, frozen_now()).unwrap().sql);
        let backwards = model_eval(&expr, "admission_date", "discharge_date", iv_end(), iv_start());
        assert!((backwards + 2.0).abs() < 1e-9, "expected -2 hours, got {backwards}");
    }

    /// Plan 03g §1: a threshold on a derived duration compares against the unit
    /// the question used, so the literal in the SQL is the number in the question.
    #[test]
    fn duration_threshold_compares_in_the_questions_unit() {
        let mut spec = QuerySpec {
            subject: Subject {
                concept: EntityConcept::Admission,
                table: Some("admissions".to_string()),
            },
            shape: Shape::List,
            measures: vec![],
            dimensions: vec![],
            filters: vec![],
            time: None,
            order: vec![],
            limit: None,
            joins: vec![],
            projection: vec![ValueExpr::Column(ColumnRef::bound(
                "admissions", "admission_date", EntityConcept::Admission, ColumnRole::StartTime,
            ))],
            related: vec![],
            duration_filters: vec![DurationFilter {
                duration: los_duration(DurationUnit::Days, OpenInterval::AsOfNow),
                op: FilterOp::Gt,
                value: FilterValue::Num(14.0),
            }],
            provenance: SpecProvenance { rule: "R8".into(), focus_subs: vec![] },
        };
        let sql = compile(&spec, SourceKind::Postgres, 500, frozen_now()).unwrap().sql;
        assert!(sql.contains("/ 86400.0) > 14"), "threshold must be in days: {sql}");
        assert!(sql.contains("COALESCE"), "\"in over 14 days\" includes current inpatients: {sql}");

        // The same threshold in hours divides by 3600 and keeps the literal.
        spec.duration_filters[0].duration.unit = DurationUnit::Hours;
        let sql = compile(&spec, SourceKind::Postgres, 500, frozen_now()).unwrap().sql;
        assert!(sql.contains("/ 3600.0) > 14"), "got: {sql}");
    }

    /// A duration in a select list still names its unit, since nothing else in a
    /// `List` projection labels the column.
    #[test]
    fn projected_duration_alias_names_its_unit() {
        let spec = QuerySpec {
            subject: Subject {
                concept: EntityConcept::Admission,
                table: Some("admissions".to_string()),
            },
            shape: Shape::List,
            measures: vec![],
            dimensions: vec![],
            filters: vec![],
            time: None,
            order: vec![],
            limit: None,
            joins: vec![],
            projection: vec![ValueExpr::Duration(los_duration(
                DurationUnit::Days,
                OpenInterval::CompletedOnly,
            ))],
            related: vec![],
            duration_filters: vec![],
            provenance: SpecProvenance { rule: "R8".into(), focus_subs: vec![] },
        };
        let sql = compile(&spec, SourceKind::Postgres, 500, frozen_now()).unwrap().sql;
        assert!(sql.contains(r#"AS "duration_days""#), "got: {sql}");
    }

    /// The validator must accept the elapsed-seconds form in every dialect —
    /// the companion rejection tests live in `nl2sql::validate`.
    #[test]
    fn derived_duration_passes_validate_sql_in_every_dialect() {
        use crate::nl2sql::validate::validate_sql;
        let spec = make_avg_duration_spec(DurationUnit::Days, OpenInterval::CompletedOnly);
        for dialect in [SourceKind::Postgres, SourceKind::Mysql, SourceKind::Mssql] {
            let compiled = compile(&spec, dialect, 500, frozen_now()).unwrap();
            validate_sql(&compiled.sql, dialect, 500, &["admissions".to_string()])
                .unwrap_or_else(|e| panic!("{dialect:?} duration SQL rejected: {e:?}\n{}", compiled.sql));
        }
    }

    /// The narration has to name the unit and the open-interval choice, or
    /// "average length of stay = 3.4" is unreadable and the excluded population
    /// is invisible.
    #[test]
    fn duration_explanation_names_unit_and_open_interval_choice() {
        let completed = make_avg_duration_spec(DurationUnit::Days, OpenInterval::CompletedOnly);
        let ex = compile(&completed, SourceKind::Postgres, 500, frozen_now()).unwrap().explanation;
        assert!(ex.contains("days"), "got: {ex}");
        assert!(ex.contains("open intervals excluded"), "got: {ex}");

        let open = make_avg_duration_spec(DurationUnit::Hours, OpenInterval::AsOfNow);
        let ex = compile(&open, SourceKind::Postgres, 500, frozen_now()).unwrap().explanation;
        assert!(ex.contains("hours"), "got: {ex}");
        assert!(ex.contains("measured up to now"), "got: {ex}");
    }
}
