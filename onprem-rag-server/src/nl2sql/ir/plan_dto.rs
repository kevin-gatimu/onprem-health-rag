//! `PlannedSpec` — a flat, human-readable DTO derived from a bound `QuerySpec`.
//!
//! When the IR succeeds, the server returns `PlannedSpec` alongside the SQL in
//! `PreparedNlQuery.spec`.  The frontend can use it to show "Claude thinks you
//! asked for X grouped by Y filtered to Z" without parsing raw SQL.
//!
//! Conversion via `TryFrom<&QuerySpec>` fails only if the spec is unbound
//! (subject `table` is `None`).

use serde::{Deserialize, Serialize};

use crate::aggregation::intent::QueryIntent;

use super::spec::{
    MeasureOp, QuerySpec, Shape, SortDir, TimeRange,
};

// ---------------------------------------------------------------------------
// Flat DTO
// ---------------------------------------------------------------------------

/// Flat, serialisable summary of a `QuerySpec` intended for the API response.
///
/// Every field is `Option<_>` or a `Vec`; nothing panics on partial specs.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PlannedSpec {
    /// Grammar rule that produced this spec (e.g. `"R1"`, `"R7"`).
    pub rule: String,
    /// High-level query intent.
    pub intent: QueryIntent,
    /// Primary concept name (e.g. `"patient"`, `"delivery"`).
    pub subject_concept: String,
    /// Physical table name (filled after binding).
    pub subject_table: Option<String>,
    /// SQL shape (e.g. `"scalar"`, `"grouped"`).
    pub shape: String,
    /// Aggregation descriptions, e.g. `["COUNT(*) AS count"]`.
    pub measures: Vec<String>,
    /// GROUP BY column descriptions, e.g. `["encounters.status"]`.
    pub dimensions: Vec<String>,
    /// WHERE predicate descriptions, e.g. `["status = 'active'"]`.
    pub filters: Vec<String>,
    /// Time range string, e.g. `"last_month"` or `"this_year"`.
    pub time_range: Option<String>,
    /// Time bucket for trend queries, e.g. `"month"`.
    pub time_bucket: Option<String>,
    /// ORDER BY descriptions, e.g. `["count DESC"]`.
    pub order: Vec<String>,
    /// LIMIT override (None → use server default).
    pub limit: Option<u32>,
}

impl TryFrom<&QuerySpec> for PlannedSpec {
    type Error = UnboundSpecError;

    fn try_from(spec: &QuerySpec) -> Result<Self, Self::Error> {
        // Build shape string.
        let shape = match spec.shape {
            Shape::Scalar => "scalar",
            Shape::Grouped => "grouped",
            Shape::Trend => "trend",
            Shape::TopN => "top_n",
            Shape::List => "list",
            Shape::Lookup => "lookup",
            Shape::Rate => "rate",
            Shape::Exists => "exists",
        }
        .to_string();

        // Measures → human-readable strings.
        let measures = spec
            .measures
            .iter()
            .map(|m| {
                let op = match &m.op {
                    MeasureOp::Count => "COUNT(*)".into(),
                    MeasureOp::CountDistinct => {
                        let col = m.target.as_ref().map(describe_target).unwrap_or_default();
                        format!("COUNT(DISTINCT {col})")
                    }
                    MeasureOp::Sum => {
                        let col = m.target.as_ref().map(describe_target).unwrap_or_default();
                        format!("SUM({col})")
                    }
                    MeasureOp::Avg => {
                        let col = m.target.as_ref().map(describe_target).unwrap_or_default();
                        format!("AVG({col})")
                    }
                    MeasureOp::Min => {
                        let col = m.target.as_ref().map(describe_target).unwrap_or_default();
                        format!("MIN({col})")
                    }
                    MeasureOp::Max => {
                        let col = m.target.as_ref().map(describe_target).unwrap_or_default();
                        format!("MAX({col})")
                    }
                    MeasureOp::Median => {
                        let col = m.target.as_ref().map(describe_target).unwrap_or_default();
                        format!("MEDIAN({col})")
                    }
                    MeasureOp::Rate { .. } => "rate(%)".into(),
                };
                format!("{op} AS {}", m.alias)
            })
            .collect();

        // Dimensions → "table.column" or just "concept:role".
        let dimensions = spec
            .dimensions
            .iter()
            .map(|d| {
                if let Some((tbl, col)) = &d.column.physical {
                    format!("{tbl}.{col}")
                } else {
                    format!("{}:{}", d.column.concept.slug(), d.column.role.slug())
                }
            })
            .collect();

        // Filters → brief description.
        let filters = spec
            .filters
            .iter()
            .map(|f| {
                let col = describe_col(&f.column);
                let op = format!("{:?}", f.op).to_ascii_lowercase();
                let val = describe_value(&f.value);
                format!("{col} {op} {val}")
            })
            .collect();

        // Time range.
        let (time_range, time_bucket) = if let Some(ts) = &spec.time {
            let range_str = ts.range.as_ref().map(describe_time_range);
            let bucket_str = ts.bucket.as_ref().map(|b| b.as_str().to_string());
            (range_str, bucket_str)
        } else {
            (None, None)
        };

        // Order.
        let order = spec
            .order
            .iter()
            .map(|o| {
                let dir = match o.dir {
                    SortDir::Asc => "ASC",
                    SortDir::Desc => "DESC",
                };
                match &o.target {
                    super::spec::OrderTarget::Measure(alias) => format!("{alias} {dir}"),
                    super::spec::OrderTarget::Column(col) => {
                        format!("{} {dir}", describe_col(col))
                    }
                }
            })
            .collect();

        Ok(PlannedSpec {
            rule: spec.provenance.rule.clone(),
            intent: spec.intent(),
            subject_concept: spec.subject.concept.slug().to_string(),
            subject_table: spec.subject.table.clone(),
            shape,
            measures,
            dimensions,
            filters,
            time_range,
            time_bucket,
            order,
            limit: spec.limit,
        })
    }
}

// ---------------------------------------------------------------------------
// Error
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnboundSpecError {
    pub detail: String,
}

impl std::fmt::Display for UnboundSpecError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "unbound QuerySpec: {}", self.detail)
    }
}

impl std::error::Error for UnboundSpecError {}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn describe_col(col: &super::spec::ColumnRef) -> String {
    if let Some((tbl, c)) = &col.physical {
        format!("{tbl}.{c}")
    } else if let Some(hint) = &col.name_hint {
        format!("{}:{} (hint: {hint})", col.concept.slug(), col.role.slug())
    } else {
        format!("{}:{}", col.concept.slug(), col.role.slug())
    }
}

/// Describe a measure/projection target for the plan DTO the UI shows.
///
/// A derived duration must render as arithmetic with its unit, not as a column
/// name — the plan panel is where a reader checks that "average length of stay"
/// really means discharge minus admission in days (plan 03g §1).
fn describe_target(v: &super::spec::ValueExpr) -> String {
    use super::spec::ValueExpr;
    match v {
        ValueExpr::Column(cr) => describe_col(cr),
        ValueExpr::Duration(d) => format!(
            "({} - {}) in {} [{}]",
            describe_col(&d.end),
            describe_col(&d.start),
            d.unit.as_str(),
            d.open.narration(),
        ),
    }
}

fn describe_value(v: &super::spec::FilterValue) -> String {
    use super::spec::FilterValue;
    match v {
        FilterValue::Str(s) => format!("'{s}'"),
        FilterValue::Num(n) => n.to_string(),
        FilterValue::Bool(b) => b.to_string(),
        FilterValue::Date(d) => d.to_string(),
        FilterValue::Ts(ts) => ts.format("%Y-%m-%dT%H:%M:%SZ").to_string(),
        FilterValue::List(items) => {
            let parts: Vec<_> = items.iter().map(describe_value).collect();
            format!("({})", parts.join(", "))
        }
        FilterValue::Range(lo, hi) => {
            format!("{} AND {}", describe_value(lo), describe_value(hi))
        }
        FilterValue::Param(name) => format!("${name}"),
    }
}

fn describe_time_range(r: &TimeRange) -> String {
    match r {
        TimeRange::Absolute { lo, hi } => {
            format!("{} to {}", describe_value(lo), describe_value(hi))
        }
        TimeRange::Today => "today".into(),
        TimeRange::Yesterday => "yesterday".into(),
        TimeRange::ThisWeek => "this_week".into(),
        TimeRange::LastWeek => "last_week".into(),
        TimeRange::ThisMonth => "this_month".into(),
        TimeRange::LastMonth => "last_month".into(),
        TimeRange::ThisQuarter => "this_quarter".into(),
        TimeRange::LastQuarter => "last_quarter".into(),
        TimeRange::ThisYear => "this_year".into(),
        TimeRange::LastYear => "last_year".into(),
        TimeRange::Last { n, unit } => format!("last_{n}_{}", unit.as_str()),
        TimeRange::Next { n, unit } => format!("next_{n}_{}", unit.as_str()),
        TimeRange::Within { n, unit } => format!("within_{n}_{}", unit.as_str()),
        TimeRange::WithinThisWeek => "within_this_week".into(),
        TimeRange::WithinThisMonth => "within_this_month".into(),
        TimeRange::WithinThisQuarter => "within_this_quarter".into(),
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::nl2sql::ir::spec::{
        ColumnRef, Filter, FilterOp, FilterValue, Measure, MeasureOp, QuerySpec, Shape,
        SpecProvenance, Subject,
    };
    use crate::ontology::concepts::EntityConcept;
    use crate::ontology::roles::ColumnRole;

    fn minimal_spec(shape: Shape) -> QuerySpec {
        QuerySpec {
            subject: Subject {
                concept: EntityConcept::Patient,
                table: Some("patients".into()),
            },
            shape,
            measures: vec![Measure {
                op: MeasureOp::Count,
                target: None,
                alias: "count".into(),
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
    fn planned_spec_from_scalar() {
        let spec = minimal_spec(Shape::Scalar);
        let dto = PlannedSpec::try_from(&spec).unwrap();
        assert_eq!(dto.shape, "scalar");
        assert_eq!(dto.subject_concept, "patient");
        assert_eq!(dto.measures, vec!["COUNT(*) AS count"]);
    }

    #[test]
    fn planned_spec_from_grouped_with_filter() {
        let mut spec = minimal_spec(Shape::Grouped);
        spec.filters.push(Filter {
            column: ColumnRef::bound("patients", "gender", EntityConcept::Patient, ColumnRole::Gender),
            op: FilterOp::Eq,
            value: FilterValue::Str("female".into()),
        });
        let dto = PlannedSpec::try_from(&spec).unwrap();
        assert_eq!(dto.shape, "grouped");
        assert_eq!(dto.filters.len(), 1);
        assert!(dto.filters[0].contains("patients.gender"));
    }

    #[test]
    fn planned_spec_intent_is_aggregation_for_scalar() {
        let spec = minimal_spec(Shape::Scalar);
        let dto = PlannedSpec::try_from(&spec).unwrap();
        assert_eq!(dto.intent, QueryIntent::Aggregation);
    }

    #[test]
    fn planned_spec_serde_roundtrip() {
        let spec = minimal_spec(Shape::Scalar);
        let dto = PlannedSpec::try_from(&spec).unwrap();
        let json = serde_json::to_string(&dto).expect("serialize");
        let back: PlannedSpec = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(dto, back);
    }
}
