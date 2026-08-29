//! Constrained aggregation spec — the shape the planning model fills in when it
//! calls the `run_aggregation` tool. Serialises to/from JSON so it can travel as
//! a tool-call argument, through Rust validation, into a DocumentDB pipeline, and
//! back to the client as a provenance `spec` SSE event.

use serde::{Deserialize, Serialize};

/// A fully-described aggregation request.  The planning model emits this via the
/// `run_aggregation` tool; `validate` checks it; `execute::run` translates it to a
/// DocumentDB aggregation pipeline.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RunAggregation {
    /// Logical collection / entity to aggregate over.  Must be an allow-listed name
    /// from the metadata catalog (e.g. `"records"`, `"patients"`, `"prescriptions"`).
    pub collection: String,

    /// Field-level pre-filter.  Keys are **bare** column names (without the `fields.`
    /// path prefix — the executor prefixes them).  Values are plain scalars (equality)
    /// or objects with operators; the only custom operator we define is
    /// `{"$prefix": "E11"}` for ICD-code prefix matching (translated to
    /// `{"$regex": "^E11"}` by the executor).  Other standard MongoDB query operators
    /// (`$gte`, `$lte`, `$in`, …) pass through unchanged.
    #[serde(default = "default_filter")]
    pub filter: serde_json::Value,

    /// Dimensions to group by.  Bare column names; the executor adds the
    /// `doc.fields.` prefix after the chunk-dedup `$group` stage.
    #[serde(default)]
    pub group_by: Vec<String>,

    /// What to compute per group.
    pub metric: Metric,

    /// Optional time-bucketing for trend queries.  Adds a `$dateToString`/expression
    /// stage before the aggregation `$group` so results are bucketed by calendar unit.
    /// Requires the target field to be stored as a BSON date type in DocumentDB.
    pub time_bucket: Option<TimeBucket>,

    /// Ordering for the final result.  Defaults to descending by `value` when absent.
    pub sort: Option<Sort>,

    /// Maximum rows returned.  Clamped to `MAX_TOP_N` (500) by `validate`.
    pub top_n: Option<u32>,
}

/// The metric (what to compute per group).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Metric {
    pub op: MetricOp,
    /// Required for `Sum`/`Avg`/`Min`/`Max`/`Distinct`; ignored for `Count`.
    pub field: Option<String>,
}

/// Supported aggregation operations.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MetricOp {
    /// Count of records per group.
    Count,
    /// Sum of a numeric field per group.
    Sum,
    /// Mean of a numeric field per group.
    Avg,
    /// Minimum value of a field per group.
    Min,
    /// Maximum value of a field per group.
    Max,
    /// Count of distinct values of a field per group (uses `$addToSet` then `$size`).
    Distinct,
}

/// Time-bucketing spec for trend queries.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TimeBucket {
    /// Bare column name of the date/time field to bucket on.
    pub field: String,
    pub unit: BucketUnit,
}

/// Granularity of the time bucket.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BucketUnit {
    Day,
    Week,
    Month,
    Quarter,
    Year,
}

impl BucketUnit {
    /// MongoDB `$dateToString` format for the unit.  Week and Quarter are computed
    /// via separate expression logic in `execute`; Day/Month/Year use plain format strings.
    pub fn date_format(&self) -> Option<&'static str> {
        match self {
            BucketUnit::Day => Some("%Y-%m-%d"),
            BucketUnit::Month => Some("%Y-%m"),
            BucketUnit::Year => Some("%Y"),
            // Week and Quarter need multi-stage expressions, not a single format string.
            BucketUnit::Week | BucketUnit::Quarter => None,
        }
    }
}

/// Sort direction.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Sort {
    /// `"value"` to sort by the metric result, or a bare field name for a dimension.
    pub by: String,
    pub dir: SortDir,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SortDir {
    Asc,
    Desc,
}

/// Hard cap on result rows.  Any `top_n` value above this is clamped by `validate`.
pub const MAX_TOP_N: u32 = 500;

pub fn default_filter() -> serde_json::Value {
    serde_json::json!({})
}

// ---------------------------------------------------------------------------
// List spec
// ---------------------------------------------------------------------------

/// Hard cap on list result rows per page. Protects against large-payload responses.
pub const MAX_LIST: u32 = 200;

/// Default page size for list queries when the planner omits `limit`.
pub const DEFAULT_LIST_LIMIT: u32 = 50;

/// A constrained list/enumeration request. The planning model emits this via the
/// `run_list_records` tool; `validate_list` checks it; `list::run` executes it.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct RunList {
    /// Physical table name (must be in the catalog). The list executor scopes the
    /// `$match` stage to `{ table: collection }` so this is the only required field.
    pub collection: String,

    /// Field-level pre-filter using the same syntax as `RunAggregation.filter`.
    /// Keys are bare column names; the executor adds `fields.` prefix.
    #[serde(default = "default_filter")]
    pub filter: serde_json::Value,

    /// Columns to include in each row. Empty means "all sampled columns for the
    /// table" (filled from the catalog by `validate_list`).
    #[serde(default)]
    pub columns: Vec<String>,

    /// Optional sort. Defaults to ascending `row_pk` for a stable page order.
    pub sort: Option<Sort>,

    /// Page size. Clamped to `MAX_LIST` by `validate_list`.
    pub limit: Option<u32>,

    /// Row offset for pagination (0-based). A follow-up "show the next 50"
    /// question is planned into `offset: 50` from conversation history.
    #[serde(default)]
    pub offset: u32,
}
