//! Deterministic translation of the `QuerySpec` IR into a DocumentDB spec
//! (plan 04 §3).
//!
//! Rung 4 of the executor ladder prefers this over model planning: when the IR
//! already parsed the question, there is no reason to ask a model to re-derive a
//! pipeline. The translation is total-or-nothing — anything it cannot express
//! faithfully returns `None` and the ladder falls back to
//! `simple_patient_count_plan`, then to `plan_aggregation`.
//!
//! **`None` is the safe answer.** Every deliberate `None` below is a case where a
//! partial translation would return a plausible-looking wrong number:
//!
//! - **Joins / reverse-FK scopes** — DocumentDB stores one flat `records`
//!   document per source row. There is no join, so a spec that needs one cannot
//!   be answered here at all (a silent drop of the join predicate would widen
//!   the cohort).
//! - **Derived durations** — "stays over 14 days" is arithmetic over two
//!   columns; `$expr` is blocked by `aggregation::validate`, so there is no way
//!   to express it. Dropping the predicate would over-count.
//! - **Median / Rate** — `RunAggregation::metric` has no percentile or ratio
//!   operator.
//! - **`Like` / `ILike`** — would have to become `$regex` built from user text.
//!   The plan lists only `Eq/In/Gt..` as translatable; a regex assembled from a
//!   question is both an injection surface and a silent semantic change.
//! - **Hour buckets** — `aggregation::spec::BucketUnit` starts at `Day`.
//! - **Cross-table column references** — every column must be bound to the
//!   subject table, because a `records` document only carries its own row.
//!
//! Field paths are emitted **bare** (`admit_date`, not `fields.admit_date`):
//! `aggregation::execute::translate_filter` and `build_group_id` add the
//! `fields.` prefix, and `aggregation::validate` checks bare names against the
//! catalog. Emitting the prefix here would fail validation.

// Plan-04 §3 subsystem: deterministic QuerySpec -> RunAggregation/RunList
// translation, not yet reachable from the live request path. Used only by
// `answer::executor::live::LiveRungs` (rung 4 of the executor ladder), which is
// itself unwired; see TODO(plan03-live) in `nl2sql::prepare`. Wiring is gated on
// the golden suite reaching 55/60 (currently 28/62). Until then every public item
// here is dead from the binary's point of view, and the resulting warning wall
// drowns out real signal — so the gate is recorded here instead of in build output.
#![allow(dead_code)]

use chrono::{DateTime, Datelike, Duration, NaiveDate, Utc};
use serde_json::{Map, Value};

use super::catalog::Catalog;
use super::spec::{
    BucketUnit as AggBucket, Metric, MetricOp, RunAggregation, RunList, Sort, SortDir as AggSortDir,
    TimeBucket,
};
use crate::nl2sql::ir::spec::{
    BucketUnit as IrBucket, ColumnRef, Filter, FilterOp, FilterValue, MeasureOp, OrderTarget,
    QuerySpec, Shape, SortDir as IrSortDir, TimeRange, ValueExpr,
};

/// What a `QuerySpec` translates to. Plan 04 §3 writes this as
/// `Option<RunAggregation | RunList>`; Rust needs a name for the sum type.
#[derive(Debug, Clone)]
pub enum DocDbSpec {
    Aggregation(RunAggregation),
    List(RunList),
}

/// Translate a **bound** `QuerySpec` into a DocumentDB spec, or `None` when the
/// spec cannot be expressed faithfully (see the module docs).
///
/// Relative time ranges are resolved against the current wall clock. Tests use
/// [`spec_from_query_spec_at`] with a frozen `now` so they stay deterministic.
pub fn spec_from_query_spec(spec: &QuerySpec, catalog: &Catalog) -> Option<DocDbSpec> {
    spec_from_query_spec_at(spec, catalog, Utc::now())
}

/// [`spec_from_query_spec`] with an explicit `now` for relative time ranges.
pub fn spec_from_query_spec_at(
    spec: &QuerySpec,
    catalog: &Catalog,
    now: DateTime<Utc>,
) -> Option<DocDbSpec> {
    // --- structural refusals ------------------------------------------------
    if !spec.joins.is_empty() || !spec.related.is_empty() || !spec.duration_filters.is_empty() {
        return None;
    }

    // The subject table must be bound *and* ingested into DocumentDB. An unbound
    // subject means `bind()` never ran; an un-ingested one means there are no
    // documents to aggregate, which is a miss rather than a zero.
    let collection = spec.subject.table.as_deref()?;
    if !catalog.has_collection(collection) {
        return None;
    }

    // --- WHERE --------------------------------------------------------------
    let mut filter = Map::new();
    for f in &spec.filters {
        let (field, value) = translate_filter(collection, catalog, f)?;
        merge_filter(&mut filter, field, value)?;
    }

    // --- time scope: range predicate + optional bucket ----------------------
    let mut time_bucket = None;
    if let Some(ref time) = spec.time {
        let field = field_name(collection, catalog, &time.column)?;
        if let Some(ref range) = time.range {
            let (lo, hi) = resolve_range(range, now)?;
            let mut bounds = Map::new();
            bounds.insert("$gte".to_string(), Value::String(lo));
            if let Some(hi) = hi {
                bounds.insert("$lt".to_string(), Value::String(hi));
            }
            merge_filter(&mut filter, field.clone(), Value::Object(bounds))?;
        }
        if let Some(unit) = time.bucket {
            time_bucket = Some(TimeBucket {
                field,
                unit: map_bucket(unit)?,
            });
        }
    }

    let filter_value = Value::Object(filter);

    match spec.shape {
        // ---------------- list / lookup -> RunList --------------------------
        Shape::List | Shape::Lookup => {
            let mut columns = Vec::new();
            for expr in &spec.projection {
                let column = expr.as_column()?; // a duration cannot be projected here
                columns.push(field_name(collection, catalog, column)?);
            }
            if columns.is_empty() {
                // No projection: return every allow-listed field for the
                // collection, in catalog order. Deterministic, and never a
                // column the catalog does not know.
                columns = catalog
                    .collections
                    .get(collection)?
                    .fields
                    .iter()
                    .map(|f| f.name.clone())
                    .collect();
            }
            if columns.is_empty() {
                return None;
            }
            Some(DocDbSpec::List(RunList {
                collection: collection.to_string(),
                filter: filter_value,
                columns,
                sort: translate_sort(collection, catalog, spec)?,
                limit: spec.limit,
                offset: 0,
            }))
        }

        // ---------------- aggregate shapes -> RunAggregation ----------------
        Shape::Scalar | Shape::Grouped | Shape::TopN | Shape::Trend | Shape::Exists => {
            if spec.measures.len() > 1 {
                // `RunAggregation` carries exactly one metric.
                return None;
            }
            let metric = match spec.measures.first() {
                // No measure on an aggregate shape means "how many" — the same
                // default the SQL compiler uses for `Exists`.
                None => Metric {
                    op: MetricOp::Count,
                    field: None,
                },
                Some(measure) => translate_measure(collection, catalog, measure)?,
            };

            let mut group_by = Vec::with_capacity(spec.dimensions.len());
            for dim in &spec.dimensions {
                group_by.push(field_name(collection, catalog, &dim.column)?);
            }

            Some(DocDbSpec::Aggregation(RunAggregation {
                collection: collection.to_string(),
                filter: filter_value,
                group_by,
                metric,
                time_bucket,
                sort: translate_sort(collection, catalog, spec)?,
                // `Scalar`/`Exists` produce one row; anything else keeps the
                // spec's own limit and lets `build_pipeline` apply its default.
                top_n: match spec.shape {
                    Shape::Scalar | Shape::Exists => Some(1),
                    _ => spec.limit,
                },
            }))
        }

        // `Rate` has no `MetricOp`; a ratio computed from two separate pipelines
        // would be a different query, not a translation of this one.
        Shape::Rate => None,
    }
}

// ---------------------------------------------------------------------------
// Column resolution
// ---------------------------------------------------------------------------

/// Physical column name for `column`, provided it is bound to `collection` and
/// allow-listed in the catalog.
///
/// The same-table requirement is load-bearing: a `records` document holds one
/// source row, so a column from another table is simply not present on it. A
/// `$match` on a missing path matches nothing, which would return a confident
/// zero instead of a miss.
fn field_name(collection: &str, catalog: &Catalog, column: &ColumnRef) -> Option<String> {
    let (table, col) = column.physical.as_ref()?;
    if table != collection {
        return None;
    }
    if catalog.has_field(collection, col) {
        return Some(col.clone());
    }
    let canonical = catalog.resolve_synonym_owned(col);
    if catalog.has_field(collection, &canonical) {
        return Some(canonical);
    }
    None
}

// ---------------------------------------------------------------------------
// Measures
// ---------------------------------------------------------------------------

fn translate_measure(
    collection: &str,
    catalog: &Catalog,
    measure: &crate::nl2sql::ir::spec::Measure,
) -> Option<Metric> {
    let target_field = |target: &Option<ValueExpr>| -> Option<String> {
        let expr = target.as_ref()?;
        let column = expr.as_column()?; // durations are refused above
        field_name(collection, catalog, column)
    };

    match &measure.op {
        MeasureOp::Count => Some(Metric {
            op: MetricOp::Count,
            field: None,
        }),
        MeasureOp::CountDistinct => Some(Metric {
            op: MetricOp::Distinct,
            field: Some(target_field(&measure.target)?),
        }),
        MeasureOp::Sum => Some(Metric {
            op: MetricOp::Sum,
            field: Some(target_field(&measure.target)?),
        }),
        MeasureOp::Avg => Some(Metric {
            op: MetricOp::Avg,
            field: Some(target_field(&measure.target)?),
        }),
        MeasureOp::Min => Some(Metric {
            op: MetricOp::Min,
            field: Some(target_field(&measure.target)?),
        }),
        MeasureOp::Max => Some(Metric {
            op: MetricOp::Max,
            field: Some(target_field(&measure.target)?),
        }),
        // No percentile or ratio accumulator in `MetricOp`.
        MeasureOp::Median | MeasureOp::Rate { .. } => None,
    }
}

// ---------------------------------------------------------------------------
// Sort
// ---------------------------------------------------------------------------

/// Translate the first `ORDER BY` clause. `RunAggregation`/`RunList` carry one
/// sort key; a spec with several is translated on its primary key only, which is
/// what `build_pipeline` can express.
fn translate_sort(
    collection: &str,
    catalog: &Catalog,
    spec: &QuerySpec,
) -> Option<Option<Sort>> {
    let Some(order) = spec.order.first() else {
        return Some(None);
    };
    let by = match &order.target {
        // "value" is the computed metric — `build_pipeline` special-cases it.
        OrderTarget::Measure(_) => "value".to_string(),
        OrderTarget::Column(column) => field_name(collection, catalog, column)?,
    };
    let dir = match order.dir {
        IrSortDir::Asc => AggSortDir::Asc,
        IrSortDir::Desc => AggSortDir::Desc,
    };
    Some(Some(Sort { by, dir }))
}

// ---------------------------------------------------------------------------
// Filters
// ---------------------------------------------------------------------------

/// Translate one IR filter into a `(bare field, mongo value)` pair.
fn translate_filter(
    collection: &str,
    catalog: &Catalog,
    filter: &Filter,
) -> Option<(String, Value)> {
    let field = field_name(collection, catalog, &filter.column)?;
    let value = match filter.op {
        FilterOp::Eq => literal(&filter.value)?,
        FilterOp::Ne => op_doc("$ne", literal(&filter.value)?),
        FilterOp::In => {
            let FilterValue::List(items) = &filter.value else {
                return None;
            };
            let mut out = Vec::with_capacity(items.len());
            for item in items {
                out.push(literal(item)?);
            }
            op_doc("$in", Value::Array(out))
        }
        FilterOp::Gt => op_doc("$gt", literal(&filter.value)?),
        FilterOp::Gte => op_doc("$gte", literal(&filter.value)?),
        FilterOp::Lt => op_doc("$lt", literal(&filter.value)?),
        FilterOp::Lte => op_doc("$lte", literal(&filter.value)?),
        FilterOp::Between => {
            let FilterValue::Range(lo, hi) = &filter.value else {
                return None;
            };
            let mut doc = Map::new();
            doc.insert("$gte".to_string(), literal(lo)?);
            doc.insert("$lte".to_string(), literal(hi)?);
            Value::Object(doc)
        }
        // See the module docs: a `$regex` built from question text is both an
        // injection surface and a semantic change.
        FilterOp::Like | FilterOp::ILike => return None,
        FilterOp::IsNull => op_doc("$eq", Value::Null),
        FilterOp::IsNotNull => op_doc("$ne", Value::Null),
        FilterOp::IsTrue => Value::Bool(true),
        FilterOp::IsFalse => Value::Bool(false),
    };
    Some((field, value))
}

fn op_doc(op: &str, value: Value) -> Value {
    let mut doc = Map::new();
    doc.insert(op.to_string(), value);
    Value::Object(doc)
}

/// Two predicates on one field must be merged into a single operator document —
/// a second `Map` insert would silently discard the first. If either side is a
/// bare equality there is no way to merge, so refuse.
fn merge_filter(filter: &mut Map<String, Value>, field: String, value: Value) -> Option<()> {
    match filter.get_mut(&field) {
        None => {
            filter.insert(field, value);
            Some(())
        }
        Some(existing) => {
            let (Value::Object(existing_ops), Value::Object(new_ops)) = (&mut *existing, &value)
            else {
                return None;
            };
            for (k, v) in new_ops {
                if existing_ops.contains_key(k) {
                    return None; // contradictory bounds on the same operator
                }
                existing_ops.insert(k.clone(), v.clone());
            }
            Some(())
        }
    }
}

/// IR literal to JSON. Dates and timestamps become ISO-8601 strings, matching
/// how ingest stores them in `fields.<col>` (see `catalog::looks_like_date`).
/// ISO-8601 compares correctly lexicographically, which is what the `$gte`/`$lt`
/// bounds rely on.
fn literal(value: &FilterValue) -> Option<Value> {
    match value {
        FilterValue::Str(s) => Some(Value::String(s.clone())),
        FilterValue::Num(n) => serde_json::Number::from_f64(*n).map(Value::Number),
        FilterValue::Bool(b) => Some(Value::Bool(*b)),
        FilterValue::Date(d) => Some(Value::String(d.to_string())),
        FilterValue::Ts(ts) => Some(Value::String(ts.to_rfc3339())),
        // A nested list or range only appears under `In` / `Between`, which
        // unwrap it themselves; a parameter has no value to compare against.
        FilterValue::List(_) | FilterValue::Range(_, _) | FilterValue::Param(_) => None,
    }
}

// ---------------------------------------------------------------------------
// Time
// ---------------------------------------------------------------------------

fn map_bucket(unit: IrBucket) -> Option<AggBucket> {
    match unit {
        IrBucket::Day => Some(AggBucket::Day),
        IrBucket::Week => Some(AggBucket::Week),
        IrBucket::Month => Some(AggBucket::Month),
        IrBucket::Quarter => Some(AggBucket::Quarter),
        IrBucket::Year => Some(AggBucket::Year),
        // `aggregation::spec::BucketUnit` has no Hour.
        IrBucket::Hour => None,
    }
}

/// Resolve a `TimeRange` to `(inclusive lower bound, exclusive upper bound)` as
/// ISO-8601 dates.
///
/// The bounds mirror `nl2sql::ir::compile::time_range_pred` exactly, including
/// the open-ended `ThisMonth`/`ThisQuarter`/`ThisYear` cases — matching bounds
/// is what makes cross-rung agreement (plan 04a §3) checkable at all.
fn resolve_range(range: &TimeRange, now: DateTime<Utc>) -> Option<(String, Option<String>)> {
    let today = now.date_naive();
    let iso = |d: NaiveDate| d.to_string();

    let bounds = match range {
        TimeRange::Today => (today, Some(today + Duration::days(1))),
        TimeRange::Yesterday => (today - Duration::days(1), Some(today)),
        TimeRange::ThisWeek => {
            let start = week_start(today);
            (start, Some(start + Duration::weeks(1)))
        }
        TimeRange::LastWeek => {
            let this_start = week_start(today);
            (this_start - Duration::weeks(1), Some(this_start))
        }
        // Lower bound only, as the SQL compiler does.
        TimeRange::ThisMonth => (month_start(today.year(), today.month())?, None),
        TimeRange::LastMonth => {
            let (py, pm) = prev_month(today.year(), today.month());
            (
                month_start(py, pm)?,
                Some(month_start(today.year(), today.month())?),
            )
        }
        TimeRange::ThisQuarter => (quarter_start(today)?, None),
        TimeRange::LastQuarter => {
            let q = (today.month() - 1) / 3;
            let (py, pm) = if q == 0 {
                (today.year() - 1, 10u32)
            } else {
                (today.year(), q * 3 - 2)
            };
            (month_start(py, pm)?, Some(quarter_start(today)?))
        }
        TimeRange::ThisYear => (month_start(today.year(), 1)?, None),
        TimeRange::LastYear => (
            month_start(today.year() - 1, 1)?,
            Some(month_start(today.year(), 1)?),
        ),
        TimeRange::Last { n, unit } => (shift(today, -(*n as i64), *unit), None),
        TimeRange::Next { n, unit } | TimeRange::Within { n, unit } => (
            today,
            // `<=` in SQL; `$lt` here needs the following day to keep the same
            // inclusive upper bound.
            Some(shift(today, *n as i64, *unit) + Duration::days(1)),
        ),
        TimeRange::WithinThisWeek => {
            let start = week_start(today);
            (start, Some(start + Duration::weeks(1)))
        }
        TimeRange::WithinThisMonth => {
            let (ny, nm) = next_month(today.year(), today.month());
            (
                month_start(today.year(), today.month())?,
                Some(month_start(ny, nm)?),
            )
        }
        TimeRange::WithinThisQuarter => {
            let start = quarter_start(today)?;
            let q_month = start.month();
            let (ey, em) = if q_month + 3 > 12 {
                (start.year() + 1, 1u32)
            } else {
                (start.year(), q_month + 3)
            };
            (start, Some(month_start(ey, em)?))
        }
        TimeRange::Absolute { lo, hi } => {
            // SQL uses BETWEEN (inclusive on both ends). `$lt` needs the day
            // after `hi` to mean the same thing.
            let lo = date_of(lo)?;
            let hi = date_of(hi)?;
            (lo, Some(hi + Duration::days(1)))
        }
    };

    Some((iso(bounds.0), bounds.1.map(iso)))
}

fn week_start(day: NaiveDate) -> NaiveDate {
    day - Duration::days(day.weekday().num_days_from_monday() as i64)
}

fn month_start(year: i32, month: u32) -> Option<NaiveDate> {
    NaiveDate::from_ymd_opt(year, month, 1)
}

fn quarter_start(day: NaiveDate) -> Option<NaiveDate> {
    let q = (day.month() - 1) / 3;
    month_start(day.year(), q * 3 + 1)
}

fn prev_month(year: i32, month: u32) -> (i32, u32) {
    if month == 1 { (year - 1, 12) } else { (year, month - 1) }
}

fn next_month(year: i32, month: u32) -> (i32, u32) {
    if month == 12 { (year + 1, 1) } else { (year, month + 1) }
}

/// Mirrors `compile::subtract_duration` / `compile::add_duration` exactly,
/// including their treatment of `Hour` as a whole day and their snapping of
/// month/quarter/year shifts to the first of the month. Matching the SQL
/// compiler's bounds is what makes cross-rung agreement (plan 04a §3)
/// checkable — a 30-day approximation here would disagree with the SQL rung on
/// every February.
fn shift(from: NaiveDate, n: i64, unit: IrBucket) -> NaiveDate {
    match unit {
        IrBucket::Hour | IrBucket::Day => from + Duration::days(n),
        IrBucket::Week => from + Duration::weeks(n),
        IrBucket::Month => shift_months(from, n),
        IrBucket::Quarter => shift_months(from, n * 3),
        IrBucket::Year => {
            NaiveDate::from_ymd_opt((from.year() as i64 + n) as i32, from.month(), 1)
                .unwrap_or(from)
        }
    }
}

/// Calendar month arithmetic, snapped to the first of the month — the same
/// formula `compile.rs` uses.
fn shift_months(from: NaiveDate, delta: i64) -> NaiveDate {
    let total = from.year() as i64 * 12 + from.month() as i64 + delta;
    let year = (total - 1).div_euclid(12);
    let month = (total - 1).rem_euclid(12) + 1;
    NaiveDate::from_ymd_opt(year as i32, month as u32, 1).unwrap_or(from)
}

fn date_of(value: &FilterValue) -> Option<NaiveDate> {
    match value {
        FilterValue::Date(d) => Some(*d),
        FilterValue::Ts(ts) => Some(ts.date_naive()),
        FilterValue::Str(s) => NaiveDate::parse_from_str(s, "%Y-%m-%d").ok(),
        _ => None,
    }
}
