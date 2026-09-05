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
    /// Columns to SELECT for List / Lookup shapes.
    pub projection: Vec<ColumnRef>,
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
    /// Target column (not needed for `Count`).
    pub target: Option<ColumnRef>,
    /// SQL alias for this measure in the SELECT list.
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
            provenance: SpecProvenance { rule: "R1".into(), focus_subs: vec![] },
        };
        let json = serde_json::to_string(&spec).expect("serialize");
        let back: QuerySpec = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(spec, back);
    }
}
