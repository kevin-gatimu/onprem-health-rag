//! `QuerySpec` — the typed intermediate representation for NL-to-SQL.
//!
//! The IR separates three concerns that change at different rates:
//! * Language → intent  (`parse.rs`): new phrasing
//! * Intent → physical columns (`bind.rs`): new hospital
//! * Physical → SQL text (`compile.rs`): new dialect
//!
//! A `ColumnRef` starts as a logical triple `(concept, role, name_hint)` and is
//! resolved to `physical: Some((table, column))` by `bind()`.  Nothing in this
//! module hard-codes physical names from the dev-seed schema.

use chrono::{DateTime, NaiveDate, Utc};
use serde::{Deserialize, Serialize};

use crate::aggregation::intent::QueryIntent;
use crate::ontology::concepts::EntityConcept;
use crate::ontology::roles::ColumnRole;

// ---------------------------------------------------------------------------
// Top-level spec
// ---------------------------------------------------------------------------

/// The full, typed description of one SQL query.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct QuerySpec {
    /// The primary table concept being queried.
    pub subject: Subject,
    /// High-level query shape (controls SELECT / GROUP BY / LIMIT strategy).
    pub shape: Shape,
    /// Aggregate measures (empty for List / Lookup).
    pub measures: Vec<Measure>,
    /// GROUP BY columns.
    pub dimensions: Vec<Dimension>,
    /// AND-ed WHERE predicates.
    pub filters: Vec<Filter>,
    /// Optional time-range filter + bucketing.
    pub time: Option<TimeScope>,
    /// ORDER BY clauses.
    pub order: Vec<Order>,
    /// LIMIT (or TOP) override. `None` → use `max_rows` from caller.
    pub limit: Option<u32>,
    /// FK joins added by the binder.
    pub joins: Vec<JoinRef>,
    /// Expressions to SELECT for List / Lookup shapes.
    pub projection: Vec<ValueExpr>,
    /// Reverse-FK semi-join / anti-join scopes (plan 03f).
    /// Each scope compiles to an EXISTS / NOT EXISTS correlated subquery.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub related: Vec<RelatedScope>,
    /// Row-level thresholds on a *derived* duration ("stays over 14 days"),
    /// AND-ed into the WHERE clause alongside `filters` (plan 03g §1).
    ///
    /// A separate list from `filters` because the predicate's left-hand side is
    /// an expression over two columns, not a single `ColumnRef`. Both compile
    /// through the same arithmetic helper, so there is exactly one
    /// implementation of the timestamp difference in the codebase.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub duration_filters: Vec<DurationFilter>,
    /// Parsing provenance for logging, evaluation, and UI annotations.
    pub provenance: SpecProvenance,
}

impl QuerySpec {
    /// Map IR shapes to the routing `QueryIntent` used by the rest of the server.
    pub fn intent(&self) -> QueryIntent {
        match self.shape {
            Shape::Scalar | Shape::Grouped | Shape::TopN | Shape::Rate | Shape::Exists => {
                QueryIntent::Aggregation
            }
            Shape::Trend => QueryIntent::Trend,
            Shape::List => QueryIntent::Enumeration,
            Shape::Lookup => QueryIntent::Lookup,
        }
    }
}

// ---------------------------------------------------------------------------
// Subject
// ---------------------------------------------------------------------------

/// The primary concept / table the query draws rows from.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Subject {
    pub concept: EntityConcept,
    /// Filled by `bind()`.  `None` before binding.
    pub table: Option<String>,
}

// ---------------------------------------------------------------------------
// Shape
// ---------------------------------------------------------------------------

/// The structural form of the generated SQL.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Shape {
    /// Single scalar aggregate (e.g. `SELECT COUNT(*)`).
    Scalar,
    /// One row per GROUP BY bucket.
    Grouped,
    /// Time-bucketed trend (`DATE_TRUNC` / `DATE_FORMAT`).
    Trend,
    /// Top-N by a measure.
    TopN,
    /// Paginated list of records.
    List,
    /// Single-record lookup by identifier.
    Lookup,
    /// Rate = numerator / denominator × 100.
    Rate,
    /// Boolean existence check.
    Exists,
}

// ---------------------------------------------------------------------------
// Measures
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Measure {
    pub op: MeasureOp,
    /// Target expression (not needed for `Count`) — a stored column, or a
    /// duration derived from two temporal columns (see `ValueExpr`).
    pub target: Option<ValueExpr>,
    /// SQL alias for this measure in the SELECT list.
    ///
    /// For a `ValueExpr::Duration` target the alias **names the unit**
    /// (`avg_los_days`, `avg_turnaround_hours`) so a wrong unit is visible in
    /// the result set rather than silent — plan 03g §1.
    pub alias: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "kind")]
pub enum MeasureOp {
    Count,
    CountDistinct,
    Sum,
    Avg,
    Min,
    Max,
    /// `PERCENTILE_CONT(0.5)` — Postgres / MSSQL only; yields `CompileError` on MySQL.
    Median,
    /// `100.0 * SUM(CASE WHEN numerator THEN 1 ELSE 0 END) / NULLIF(COUNT(*), 0)`.
    Rate { numerator: Box<Filter> },
}

// ---------------------------------------------------------------------------
// Dimensions
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Dimension {
    pub column: ColumnRef,
    /// Display name used in the `explanation` sentence.
    pub label: String,
}

// ---------------------------------------------------------------------------
// Filters
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Filter {
    pub column: ColumnRef,
    pub op: FilterOp,
    pub value: FilterValue,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FilterOp {
    Eq,
    Ne,
    In,
    Gt,
    Gte,
    Lt,
    Lte,
    Between,
    Like,
    ILike,
    IsNull,
    IsNotNull,
    IsTrue,
    IsFalse,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "type", content = "v")]
pub enum FilterValue {
    Str(String),
    Num(f64),
    Bool(bool),
    Date(NaiveDate),
    Ts(DateTime<Utc>),
    /// Used for `IN (...)`.
    List(Vec<FilterValue>),
    /// Used for `BETWEEN ... AND ...`.
    Range(Box<FilterValue>, Box<FilterValue>),
    /// Named query parameter (used by plan_dto fallback).
    Param(String),
}

// ---------------------------------------------------------------------------
// Time scope
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TimeScope {
    /// Which column to filter / bucket.
    pub column: ColumnRef,
    /// Optional date / time range.
    pub range: Option<TimeRange>,
    /// Optional time bucket for Trend shape.
    pub bucket: Option<BucketUnit>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "kind")]
pub enum TimeRange {
    /// Absolute bounds already computed as FilterValue::Date or ::Ts.
    Absolute {
        lo: FilterValue,
        hi: FilterValue,
    },
    Today,
    Yesterday,
    ThisWeek,
    LastWeek,
    ThisMonth,
    LastMonth,
    ThisQuarter,
    LastQuarter,
    ThisYear,
    LastYear,
    /// "last N days / weeks / months / years"
    Last { n: u32, unit: BucketUnit },
    /// "next N days / weeks / months / years"
    Next { n: u32, unit: BucketUnit },
    /// "within N days"
    Within { n: u32, unit: BucketUnit },
    /// Bounded "expires/due this week" — lower AND upper bound (start..end of period).
    /// Distinct from `ThisWeek` which only emits a lower bound ("occurred this week").
    WithinThisWeek,
    /// Bounded "expires/due this month" — lower AND upper bound.
    WithinThisMonth,
    /// Bounded "expires/due this quarter" — lower AND upper bound.
    WithinThisQuarter,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BucketUnit {
    Hour,
    Day,
    Week,
    Month,
    Quarter,
    Year,
}

impl BucketUnit {
    pub fn as_str(self) -> &'static str {
        match self {
            BucketUnit::Hour => "hour",
            BucketUnit::Day => "day",
            BucketUnit::Week => "week",
            BucketUnit::Month => "month",
            BucketUnit::Quarter => "quarter",
            BucketUnit::Year => "year",
        }
    }
}

// ---------------------------------------------------------------------------
// Order
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Order {
    pub target: OrderTarget,
    pub dir: SortDir,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "kind", content = "v")]
pub enum OrderTarget {
    /// Reference to a measure alias (e.g. "count").
    Measure(String),
    /// Reference to a column.
    Column(ColumnRef),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SortDir {
    Asc,
    Desc,
}

// ---------------------------------------------------------------------------
// Column reference
// ---------------------------------------------------------------------------

/// A logical column address: concept + role + optional name hint.
/// Resolved to `physical = Some((table, column))` by `bind()`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ColumnRef {
    pub concept: EntityConcept,
    pub role: ColumnRole,
    /// Generic fragment used to disambiguate when multiple columns share one role.
    /// Case-insensitive substring matching against the physical column name.
    /// Never a real dev-seed column name — use generic terms like "weight", "expir".
    pub name_hint: Option<String>,
    /// Filled by `bind()`: `(table_name, column_name)`.
    pub physical: Option<(String, String)>,
}

impl ColumnRef {
    /// Unbound logical column reference.
    pub fn logical(concept: EntityConcept, role: ColumnRole) -> Self {
        ColumnRef { concept, role, name_hint: None, physical: None }
    }

    /// Unbound logical column reference with disambiguation hint.
    pub fn with_hint(concept: EntityConcept, role: ColumnRole, hint: &str) -> Self {
        ColumnRef { concept, role, name_hint: Some(hint.to_string()), physical: None }
    }

    /// Pre-bound column (physical names already known — for hand-built test specs).
    pub fn bound(table: &str, column: &str, concept: EntityConcept, role: ColumnRole) -> Self {
        ColumnRef {
            concept,
            role,
            name_hint: None,
            physical: Some((table.to_string(), column.to_string())),
        }
    }
}

// ---------------------------------------------------------------------------
// Derived temporal measures (plan 03g)
// ---------------------------------------------------------------------------

/// The unit a derived duration is expressed in.
///
/// The unit is **not optional and never inferred at compile time** (plan 03g
/// §1). Length of stay is conventionally days, lab turnaround hours, a triage
/// wait minutes; getting it wrong is a 24× or 60× error that still reads as a
/// plausible number. It therefore travels in the IR next to the endpoints, and
/// the compiled column alias names it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DurationUnit {
    Days,
    Hours,
    Minutes,
}

impl DurationUnit {
    /// Seconds in one unit — the divisor applied to the normalised
    /// elapsed-seconds difference the compiler emits (plan 03g §2).
    pub fn seconds(self) -> i64 {
        match self {
            DurationUnit::Days => 86_400,
            DurationUnit::Hours => 3_600,
            DurationUnit::Minutes => 60,
        }
    }

    /// Lowercase unit word, used as the alias suffix (`..._days`).
    pub fn as_str(self) -> &'static str {
        match self {
            DurationUnit::Days => "days",
            DurationUnit::Hours => "hours",
            DurationUnit::Minutes => "minutes",
        }
    }
}

/// How an interval whose end timestamp is NULL is treated (plan 03g §3).
///
/// A currently-admitted patient has no discharge timestamp. Their stay is not
/// zero and it is not missing — it is *ongoing*. `AVG` skips NULLs, so leaving
/// this implicit silently computes "average stay of the patients who already
/// left", biased short precisely because the long stayers are still there.
/// The choice is therefore carried in the IR, the two variants compile to
/// different SQL, and the explanation sentence states which was used.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OpenInterval {
    /// Exclude open intervals with an explicit `end IS NOT NULL` predicate, so
    /// the excluded population is visible in the SQL rather than an accident of
    /// aggregate NULL semantics.
    CompletedOnly,
    /// Treat an open interval as ending at the frozen `now` passed to
    /// `compile` — "how long have the current inpatients been here".
    AsOfNow,
}

impl OpenInterval {
    /// Clause used in the explanation sentence (plan 03g §3: "the narration
    /// must state it").
    pub fn narration(self) -> &'static str {
        match self {
            OpenInterval::CompletedOnly => "completed intervals only (open intervals excluded)",
            OpenInterval::AsOfNow => "open intervals measured up to now",
        }
    }
}

/// A measure no column stores: the elapsed time between two temporal columns,
/// in an explicit unit.
///
/// Both endpoints are ordinary `ColumnRef`s and are resolved by `bind()`
/// through the same role lookup as every other column — by role and concept,
/// never by position in a candidate list. If either endpoint is absent or
/// ambiguous, binding refuses; a "temporal column of roughly the right kind" is
/// never substituted (plan 03g §4).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DerivedDuration {
    /// Interval start (typically `ColumnRole::StartTime`).
    pub start: ColumnRef,
    /// Interval end (typically `ColumnRole::EndTime`).
    pub end: ColumnRef,
    pub unit: DurationUnit,
    pub open: OpenInterval,
}

impl DerivedDuration {
    /// Alias for a duration that appears in a select list without a measure to
    /// name it (a `List`-shape projection). It still names the unit, because an
    /// unlabelled arithmetic column is the 24×-error hazard plan 03g §1 is about.
    pub fn default_alias(&self) -> String {
        format!("duration_{}", self.unit.as_str())
    }
}

/// A scalar expression usable as a measure target or a projected column.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "kind", content = "v")]
pub enum ValueExpr {
    /// A single stored column.
    Column(ColumnRef),
    /// A duration derived from two temporal columns (plan 03g).
    Duration(DerivedDuration),
}

impl ValueExpr {
    /// The underlying column, when this expression is a plain column.
    pub fn as_column(&self) -> Option<&ColumnRef> {
        match self {
            ValueExpr::Column(cr) => Some(cr),
            ValueExpr::Duration(_) => None,
        }
    }

    /// The derived duration, when this expression is one.
    pub fn as_duration(&self) -> Option<&DerivedDuration> {
        match self {
            ValueExpr::Duration(d) => Some(d),
            ValueExpr::Column(_) => None,
        }
    }
}

impl From<ColumnRef> for ValueExpr {
    fn from(cr: ColumnRef) -> Self {
        ValueExpr::Column(cr)
    }
}

/// A row-level threshold on a derived duration ("admitted over 14 days").
///
/// Shares `DerivedDuration` — and therefore the compiler's single
/// elapsed-seconds helper — with the aggregated form, so a threshold and an
/// average of the same interval can never disagree about the arithmetic.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DurationFilter {
    pub duration: DerivedDuration,
    pub op: FilterOp,
    /// Compared in `duration.unit` — "over 14 days" is `Num(14.0)` with
    /// `unit: Days`, never a minutes-normalised 20160.
    pub value: FilterValue,
}

// ---------------------------------------------------------------------------
// Related scope (reverse-FK semi-join / anti-join)
// ---------------------------------------------------------------------------

/// A single-hop reverse-FK scope added to the WHERE clause as an
/// `EXISTS` (semi-join) or `NOT EXISTS` (anti-join) subquery.
///
/// Created by the parser when a domain predicate targets a concept different
/// from the subject (e.g. "missed" → `Appointment.Status` while the subject
/// is `Patient`).  Resolved by the binder via a reverse-FK index.
///
/// See plan 03f for the hard constraints: single hop only, FK→anchor PK,
/// exactly-one-candidate, no row multiplication.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RelatedScope {
    /// The child concept whose table is referenced in the subquery.
    pub concept: EntityConcept,
    /// Predicates inside the EXISTS/NOT EXISTS correlated subquery.
    pub filters: Vec<Filter>,
    /// Optional time filter on the child table (rare but structurally correct).
    pub time: Option<TimeScope>,
    /// `false` → `EXISTS` (semi-join).  `true` → `NOT EXISTS` (anti-join).
    pub negated: bool,
    /// Resolved by `bind()`.  `None` before binding.
    pub physical: Option<RelatedPhysical>,
}

/// Physical resolution of a `RelatedScope` (filled by `bind()`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RelatedPhysical {
    /// Physical table name for the child (e.g. `"appointments"`).
    pub child_table: String,
    /// Column on the child table that is the FK (e.g. `"patient_id"`).
    pub child_fk_col: String,
    /// Column on the anchor table that is the PK (e.g. `"id"`).
    pub anchor_pk_col: String,
}

// ---------------------------------------------------------------------------
// Join reference
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct JoinRef {
    /// The FK hops that implement this join.
    pub hops: Vec<crate::ontology::binding::JoinHop>,
    /// SQL alias for the final joined table (e.g. "t1", "t2").
    pub alias: String,
    pub kind: JoinKind,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum JoinKind {
    Inner,
    Left,
}

// ---------------------------------------------------------------------------
// Provenance
// ---------------------------------------------------------------------------

/// Which grammar rule produced this spec and any substitutions made.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SpecProvenance {
    /// Short grammar rule id (e.g. "R1", "R7").
    pub rule: String,
    /// Focus substitutions applied (e.g. previous turn's subject).
    pub focus_subs: Vec<String>,
}

// ---------------------------------------------------------------------------
// Missing slots (for clarification)
// ---------------------------------------------------------------------------

/// A slot that was required by the matched rule but could not be resolved.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MissingSlot {
    /// Subject concept could not be resolved from the question.
    Subject,
    /// A specific patient identifier was needed but not present.
    Patient,
    /// A time range was needed but could not be parsed from the question.
    TimeRange,
    /// A dimension column was ambiguous (candidates provided separately).
    Dimension,
    /// A required measure column was unbound in this schema.
    Metric,
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::NaiveDate;

    #[test]
    fn shape_serde_roundtrip() {
        for shape in [
            Shape::Scalar, Shape::Grouped, Shape::Trend, Shape::TopN,
            Shape::List, Shape::Lookup, Shape::Rate, Shape::Exists,
        ] {
            let json = serde_json::to_string(&shape).expect("serialise Shape");
            let back: Shape = serde_json::from_str(&json).expect("deserialise Shape");
            assert_eq!(shape, back);
        }
    }

    #[test]
    fn filter_value_list_serde_roundtrip() {
        let v = FilterValue::List(vec![
            FilterValue::Str("active".to_string()),
            FilterValue::Str("pending".to_string()),
        ]);
        let json = serde_json::to_string(&v).expect("serialize");
        let back: FilterValue = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(v, back);
    }

    #[test]
    fn filter_value_range_roundtrip() {
        let lo = FilterValue::Date(NaiveDate::from_ymd_opt(2026, 1, 1).unwrap());
        let hi = FilterValue::Date(NaiveDate::from_ymd_opt(2026, 12, 31).unwrap());
        let v = FilterValue::Range(Box::new(lo), Box::new(hi));
        let json = serde_json::to_string(&v).expect("serialize");
        let back: FilterValue = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(v, back);
    }

    #[test]
    fn query_spec_intent_mapping() {
        let make_spec = |shape: Shape| QuerySpec {
            subject: Subject { concept: EntityConcept::Patient, table: None },
            shape,
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
            provenance: SpecProvenance { rule: "R1".into(), focus_subs: vec![] },
        };
        assert_eq!(make_spec(Shape::Scalar).intent(), QueryIntent::Aggregation);
        assert_eq!(make_spec(Shape::Grouped).intent(), QueryIntent::Aggregation);
        assert_eq!(make_spec(Shape::Trend).intent(), QueryIntent::Trend);
        assert_eq!(make_spec(Shape::TopN).intent(), QueryIntent::Aggregation);
        assert_eq!(make_spec(Shape::List).intent(), QueryIntent::Enumeration);
        assert_eq!(make_spec(Shape::Lookup).intent(), QueryIntent::Lookup);
        assert_eq!(make_spec(Shape::Rate).intent(), QueryIntent::Aggregation);
        assert_eq!(make_spec(Shape::Exists).intent(), QueryIntent::Aggregation);
    }

    #[test]
    fn query_spec_full_serde_roundtrip() {
        let spec = QuerySpec {
            subject: Subject {
                concept: EntityConcept::Encounter,
                table: Some("encounters".to_string()),
            },
            shape: Shape::Scalar,
            measures: vec![Measure {
                op: MeasureOp::Count,
                target: None,
                alias: "count".to_string(),
            }],
            dimensions: vec![],
            filters: vec![Filter {
                column: ColumnRef::bound(
                    "encounters", "status",
                    EntityConcept::Encounter, ColumnRole::Status,
                ),
                op: FilterOp::Eq,
                value: FilterValue::Str("active".to_string()),
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
        let json = serde_json::to_string(&spec).expect("serialize");
        let back: QuerySpec = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(spec, back);
    }
}
