//! DocumentDB aggregation executor.
//!
//! Translates a validated `RunAggregation` spec into a concrete DocumentDB
//! aggregation pipeline, runs it against the `records` collection, and returns
//! chart-ready rows plus the executed pipeline for provenance.
//!
//! # Pipeline structure (in order)
//!
//! 1. `$match` — authz scope AND `{table: spec.collection}` AND user filter.
//!    The table match scopes the pipeline to the physical source table so the
//!    row_pk dedup in stage 2 is correct (row_pk is unique within a table).
//! 2. `$group { _id: "$row_pk", doc: { $first: "$$ROOT" } }` — dedup by row_pk.
//!    A source row can produce multiple chunks (`chunk_index > 0`). Without this
//!    stage, a row with 3 chunks contributes 3 to a count — silently tripling PHI
//!    statistics. Correctness depends on the table match in stage 1.
//! 3. `$addFields { __bucket: … }` — optional, only for `time_bucket` specs.
//! 4. `$group { _id: <group_key>, value: <metric_expr> }` — user-visible aggregation.
//!    Fields are accessed as `doc.fields.<col>` (post-dedup path).
//! 5. `$addFields { value: { $size: "$value" } }` — only for `Distinct` metric.
//! 6. `$sort` — by metric `value` (default: desc) or a specified dimension.
//! 7. `$limit` — bounded to `top_n` (clamped <= 500 by `validate`).
//! 8. `$project { _id: 0, label: "$_id", value: 1 }` — rename `_id` to `label`.
//!
//! # maxTimeMS
//!
//! Set via `AggregateOptions` to prevent runaway aggregations on large datasets.

use futures::TryStreamExt;
use mongodb::bson::{Bson, Document, doc};
use serde::{Deserialize, Serialize};

use super::spec::{BucketUnit, MetricOp, RunAggregation, SortDir, MAX_TOP_N};
use crate::auth::guard::AuthUser;
use crate::documentdb::DocumentDb;
use crate::error::AppResult;

/// One aggregated result row, ready to plot. `label` is the group-by dimension
/// (stringified), `value` is the computed metric.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AggRow {
    pub label: String,
    pub value: f64,
}

/// Maximum rows when `top_n` is not specified by the caller.
const DEFAULT_TOP_N: u32 = 20;

/// Hard timeout for aggregation pipelines (milliseconds). Prevents a slow scan
/// from blocking the server indefinitely on a large corpus.
const AGGREGATE_MAX_TIME_MS: u64 = 30_000;

// ---------------------------------------------------------------------------
// Public entry point
// ---------------------------------------------------------------------------

/// Run the validated aggregation spec and return chart-ready rows plus the
/// executed pipeline (for the provenance `pipeline` SSE event).
///
/// All collection access goes through `DocumentDb::records()` — all logical
/// entity names ("patients", "prescriptions", etc.) map to the single `records`
/// collection because the executor is the only place that knows this mapping.
pub async fn run(
    db: &DocumentDb,
    auth: &AuthUser,
    spec: &RunAggregation,
) -> AppResult<(Vec<AggRow>, Vec<Document>)> {
    let pipeline = build_pipeline(auth, spec);
    let stored_pipeline = pipeline.clone(); // provenance copy

    let mut cursor = db
        .records()
        .aggregate(pipeline)
        .max_time(std::time::Duration::from_millis(AGGREGATE_MAX_TIME_MS))
        .await?;

    let mut rows = Vec::new();
    while let Some(doc) = cursor.try_next().await? {
        let label = bson_to_label(doc.get("label"));
        let value = bson_to_f64(doc.get("value")).unwrap_or(0.0);
        rows.push(AggRow { label, value });
    }

    tracing::info!(
        collection = %spec.collection,
        rows = rows.len(),
        user = %auth.username,
        "aggregation executed"
    );

    Ok((rows, stored_pipeline))
}

// ---------------------------------------------------------------------------
// Pipeline builder (pure — testable without a live DB)
// ---------------------------------------------------------------------------

/// Build the full aggregation pipeline for `spec`. Pure function: no I/O,
/// fully testable. Called by `run` above and exercised directly in unit tests.
pub fn build_pipeline(auth: &AuthUser, spec: &RunAggregation) -> Vec<Document> {
    let mut pipeline: Vec<Document> = Vec::new();

    // --- Stage 1: $match (authz AND table scope AND user filter) ---
    // The table match is the correctness invariant for stage 2: row_pk values are
    // only unique within a single table. Without this match, row_pks from different
    // tables collide (e.g. row_pk="1" appears in both patients and encounters) and
    // the dedup group produces wrong counts.
    let mut match_doc = build_authz_filter(auth);
    match_doc.insert("table", &spec.collection); // physical table scope
    for (k, v) in translate_filter(&spec.filter) {
        match_doc.insert(k, v);
    }
    pipeline.push(doc! { "$match": match_doc });

    // --- Stage 2: $group — dedup by row_pk ---
    // Critical invariant: count per source *record*, not per *chunk*.
    // Without this stage a record with N chunks is counted N times.
    // Correctness depends on the table match in stage 1.
    pipeline.push(doc! {
        "$group": {
            "_id": "$row_pk",
            "doc": { "$first": "$$ROOT" }
        }
    });

    // --- Stage 3: $addFields — time bucket (optional) ---
    if let Some(ref tb) = spec.time_bucket {
        let date_expr = build_bucket_expr(tb);
        pipeline.push(doc! { "$addFields": { "__bucket": date_expr } });
    }

    // --- Stage 4: $group — user-visible aggregation ---
    let group_id = build_group_id(spec);
    let metric_expr = build_metric_expr(spec);
    pipeline.push(doc! {
        "$group": {
            "_id": group_id,
            "value": metric_expr,
        }
    });

    // --- Stage 5: $addFields — convert $addToSet array to count (Distinct) ---
    if spec.metric.op == MetricOp::Distinct {
        pipeline.push(doc! { "$addFields": { "value": { "$size": "$value" } } });
    }

    // --- Stage 6: $sort ---
    let sort_dir: i32 = spec
        .sort
        .as_ref()
        .map(|s| if s.dir == SortDir::Asc { 1 } else { -1 })
        .unwrap_or(-1); // default: descending by value

    let sort_field = spec
        .sort
        .as_ref()
        .map(|s| {
            if s.by == "value" {
                "value".to_string()
            } else {
                "_id".to_string()
            }
        })
        .unwrap_or_else(|| "value".to_string());

    pipeline.push(doc! { "$sort": { sort_field: sort_dir } });

    // --- Stage 7: $limit ---
    let limit = spec.top_n.unwrap_or(DEFAULT_TOP_N).min(MAX_TOP_N);
    pipeline.push(doc! { "$limit": limit as i64 });

    // --- Stage 8: $project — rename _id to label ---
    pipeline.push(doc! { "$project": { "_id": 0, "label": "$_id", "value": 1 } });

    pipeline
}

// ---------------------------------------------------------------------------
// Authz filter
// ---------------------------------------------------------------------------

/// Build the mandatory authz `$match` filter from the authenticated user.
///
/// Currently role-level only:
/// - Admin: no additional constraint (sees all records across all sources).
/// - User: same for now (per-clinic / per-source ACLs are a follow-up).
fn build_authz_filter(_auth: &AuthUser) -> Document {
    // TODO(phase-acl): narrow to `auth.source_ids` or `auth.clinic` when those
    // fields are added to `AuthUser`. For now, all authenticated users see all
    // records (admin role check is enforced by the route before reaching here).
    Document::new()
}

// ---------------------------------------------------------------------------
// Filter translation
// ---------------------------------------------------------------------------

/// Translate the spec filter (bare field names to MongoDB `fields.<col>` paths).
/// Handles the custom `$prefix` operator (to `$regex: "^<prefix>"`).
/// `pub(crate)` so the list executor can reuse this translation.
pub(crate) fn translate_filter(filter: &serde_json::Value) -> Vec<(String, Bson)> {
    let mut pairs = Vec::new();
    if let Some(obj) = filter.as_object() {
        for (key, value) in obj {
            let db_key = format!("fields.{key}");
            let bson_value = translate_filter_value(value);
            pairs.push((db_key, bson_value));
        }
    }
    pairs
}

fn translate_filter_value(value: &serde_json::Value) -> Bson {
    if let Some(obj) = value.as_object() {
        // Custom $prefix operator to $regex for ICD code and string prefix matching.
        if let Some(prefix) = obj.get("$prefix").and_then(|v| v.as_str()) {
            return doc! { "$regex": format!("^{prefix}") }.into();
        }
    }
    // Standard conversion: JSON null/bool/number/string/array/object to BSON.
    mongodb::bson::to_bson(value).unwrap_or(Bson::Null)
}

// ---------------------------------------------------------------------------
// Group-by id expression builder
// ---------------------------------------------------------------------------

/// Build the `_id` expression for the aggregation `$group` stage.
///
/// After the dedup stage, structured fields live under `doc.fields.<col>`.
/// - No group_by + no time_bucket: `null` (global total).
/// - Time bucket only: `"$__bucket"`.
/// - Single group_by only: `"$doc.fields.<col>"` (scalar string/number _id).
/// - Multiple group_by: `{ col1: "$doc.fields.col1", … }` (object _id).
/// - Group_by + time_bucket: `{ bucket: "$__bucket", col1: "…", … }`.
fn build_group_id(spec: &RunAggregation) -> Bson {
    let has_bucket = spec.time_bucket.is_some();
    let has_group = !spec.group_by.is_empty();

    match (has_bucket, has_group) {
        (false, false) => Bson::Null,
        (true, false) => Bson::String("$__bucket".to_string()),
        (false, true) if spec.group_by.len() == 1 => {
            let col = &spec.group_by[0];
            Bson::String(format!("$doc.fields.{col}"))
        }
        _ => {
            let mut id_doc = Document::new();
            if has_bucket {
                id_doc.insert("bucket", "$__bucket");
            }
            for col in &spec.group_by {
                id_doc.insert(col.as_str(), format!("$doc.fields.{col}"));
            }
            id_doc.into()
        }
    }
}

// ---------------------------------------------------------------------------
// Metric expression builder
// ---------------------------------------------------------------------------

fn build_metric_expr(spec: &RunAggregation) -> Bson {
    let field_path = spec.metric.field.as_ref().map(|f| format!("$doc.fields.{f}"));

    match &spec.metric.op {
        MetricOp::Count => doc! { "$sum": 1 }.into(),
        MetricOp::Sum => {
            let path = field_path.unwrap_or_default();
            doc! { "$sum": path }.into()
        }
        MetricOp::Avg => {
            let path = field_path.unwrap_or_default();
            doc! { "$avg": path }.into()
        }
        MetricOp::Min => {
            let path = field_path.unwrap_or_default();
            doc! { "$min": path }.into()
        }
        MetricOp::Max => {
            let path = field_path.unwrap_or_default();
            doc! { "$max": path }.into()
        }
        MetricOp::Distinct => {
            // Accumulate distinct values with $addToSet; the size is computed in
            // a subsequent $addFields stage (see Stage 5 in build_pipeline).
            let path = field_path.unwrap_or_default();
            doc! { "$addToSet": path }.into()
        }
    }
}

// ---------------------------------------------------------------------------
// Time-bucket expression builder (Stage E)
// ---------------------------------------------------------------------------

/// Build the value expression for `$addFields { __bucket: <expr> }`.
///
/// Ingested dates are stored as strings (Postgres `to_jsonb` serialises DATE/TIMESTAMP
/// as ISO-8601 strings). `$dateToString` and the ISO date operators require a BSON
/// Date type; feeding a string directly produces null or aborts. The `$convert`
/// wrapper coerces the string to Date first and yields null on bad input rather than
/// aborting the whole pipeline, so rows with invalid or missing dates are simply
/// excluded from the bucket.
///
/// Uses `$convert { input: <field>, to: "date", onError: null, onNull: null }` before
/// passing the result to `$dateToString` / `$isoWeek` / `$year`. This also passes
/// through values that are already a BSON Date, so the pipeline stays correct when
/// the source stores native date types.
fn build_bucket_expr(tb: &super::spec::TimeBucket) -> Bson {
    // Coerce the field (string or date) to a BSON Date, yielding null on any error.
    let date_path = doc! {
        "$convert": {
            "input": format!("$doc.fields.{}", tb.field),
            "to": "date",
            "onError": Bson::Null,
            "onNull": Bson::Null,
        }
    };

    match tb.unit {
        BucketUnit::Day | BucketUnit::Month | BucketUnit::Year => {
            let fmt = tb.unit.date_format().unwrap(); // Day/Month/Year all have formats
            doc! {
                "$dateToString": { "format": fmt, "date": &date_path }
            }
            .into()
        }
        BucketUnit::Week => {
            // ISO year + "-W" + zero-padded ISO week number.
            doc! {
                "$concat": [
                    { "$toString": { "$isoWeekYear": &date_path } },
                    "-W",
                    { "$toString": { "$isoWeek": &date_path } }
                ]
            }
            .into()
        }
        BucketUnit::Quarter => {
            // "YYYY-Qn" — quarter derived from month: Q = ceil(month / 3).
            doc! {
                "$concat": [
                    { "$toString": { "$year": &date_path } },
                    "-Q",
                    { "$toString": {
                        "$ceil": { "$divide": [ { "$month": &date_path }, 3.0 ] }
                    }}
                ]
            }
            .into()
        }
    }
}

// ---------------------------------------------------------------------------
// Result extraction helpers
// ---------------------------------------------------------------------------

/// Stringify any BSON value for use as a chart label.
fn bson_to_label(v: Option<&Bson>) -> String {
    match v {
        None | Some(Bson::Null) => "(all)".to_string(),
        Some(Bson::String(s)) => s.clone(),
        Some(Bson::Boolean(b)) => b.to_string(),
        Some(Bson::Int32(n)) => n.to_string(),
        Some(Bson::Int64(n)) => n.to_string(),
        Some(Bson::Double(n)) => format!("{n}"),
        Some(other) => {
            serde_json::to_string(&other.clone().into_relaxed_extjson())
                .unwrap_or_else(|_| "(complex)".to_string())
        }
    }
}

/// Extract a BSON value as `f64`.
fn bson_to_f64(v: Option<&Bson>) -> Option<f64> {
    match v? {
        Bson::Double(d) => Some(*d),
        Bson::Int32(n) => Some(*n as f64),
        Bson::Int64(n) => Some(*n as f64),
        Bson::Array(a) => Some(a.len() as f64),
        _ => None,
    }
}

// ---------------------------------------------------------------------------
// Tests — pipeline structure (no live DB required)
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::aggregation::spec::{Metric, MetricOp, Sort, SortDir};
    use crate::auth::{Role, guard::AuthUser};

    fn test_user() -> AuthUser {
        AuthUser { id: "u1".into(), username: "tester".into(), role: Role::Doctor }
    }

    fn admin_user() -> AuthUser {
        AuthUser { id: "a1".into(), username: "admin".into(), role: Role::Admin }
    }

    /// "Most common diagnoses" spec — the canonical example from the plan.
    fn most_common_diagnoses_spec() -> RunAggregation {
        RunAggregation {
            collection: "encounters".into(),
            filter: serde_json::json!({}),
            group_by: vec!["diagnosis_display".into()],
            metric: Metric { op: MetricOp::Count, field: None },
            time_bucket: None,
            sort: Some(Sort { by: "value".into(), dir: SortDir::Desc }),
            top_n: Some(10),
        }
    }

    #[test]
    fn stage_0_match_contains_table_scope() {
        // Stage B correctness: the first $match must scope to the physical table.
        let spec = most_common_diagnoses_spec();
        let pipeline = build_pipeline(&test_user(), &spec);

        assert!(pipeline[0].contains_key("$match"), "stage 0 must be $match");
        let match_doc = pipeline[0].get_document("$match").unwrap();
        assert_eq!(
            match_doc.get_str("table").unwrap_or(""),
            "encounters",
            "stage-0 $match must scope to spec.collection"
        );
    }

    #[test]
    fn most_common_diagnoses_pipeline_structure() {
        let spec = most_common_diagnoses_spec();
        let pipeline = build_pipeline(&test_user(), &spec);

        // Expect: $match, $group(dedup), $group(agg), $sort, $limit, $project
        assert!(pipeline.len() >= 5, "pipeline too short: {}", pipeline.len());

        // Stage 0: $match
        assert!(pipeline[0].contains_key("$match"), "stage 0 must be $match");

        // Stage 1: $group with _id: "$row_pk" and doc: {$first: "$$ROOT"}
        let dedup = pipeline[1].get_document("$group").expect("stage 1 must be $group");
        assert_eq!(
            dedup.get_str("_id").unwrap_or(""),
            "$row_pk",
            "dedup $group must group on row_pk"
        );
        let doc_field = dedup.get_document("doc").expect("dedup stage must carry 'doc' field");
        assert!(doc_field.contains_key("$first"), "dedup doc must use $first");

        // Stage 2: $group for the aggregation dimension
        let agg = pipeline[2].get_document("$group").expect("stage 2 must be $group");
        let id = agg.get("_id").expect("agg $group must have _id");
        let id_str = match id {
            Bson::String(s) => s.as_str(),
            _ => panic!("expected string _id for single group_by, got {:?}", id),
        };
        assert!(
            id_str.contains("doc.fields.diagnosis_display"),
            "group _id must reference doc.fields.diagnosis_display, got {id_str}"
        );
        // The metric must use $sum: 1 for Count
        let value_expr = agg.get_document("value").expect("agg $group must have 'value'");
        assert!(value_expr.contains_key("$sum"), "Count metric must use $sum");

        let last = &pipeline[pipeline.len() - 1];
        assert!(last.contains_key("$project"), "last stage must be $project");

        let has_sort = pipeline.iter().any(|s| s.contains_key("$sort"));
        let has_limit = pipeline.iter().any(|s| s.contains_key("$limit"));
        assert!(has_sort, "pipeline must contain a $sort stage");
        assert!(has_limit, "pipeline must contain a $limit stage");

        let sort_idx = pipeline.iter().position(|s| s.contains_key("$sort")).unwrap();
        let limit_idx = pipeline.iter().position(|s| s.contains_key("$limit")).unwrap();
        assert!(sort_idx < limit_idx, "$sort must precede $limit");
    }

    #[test]
    fn filter_prefix_translates_to_regex() {
        let spec = RunAggregation {
            collection: "encounters".into(),
            filter: serde_json::json!({ "diagnosis_code": { "$prefix": "E11" } }),
            group_by: vec!["diagnosis_display".into()],
            metric: Metric { op: MetricOp::Count, field: None },
            time_bucket: None,
            sort: None,
            top_n: Some(5),
        };
        let pipeline = build_pipeline(&test_user(), &spec);
        let match_doc = pipeline[0].get_document("$match").unwrap();
        let diag_filter = match_doc.get("fields.diagnosis_code")
            .expect("filter must use fields. prefix");
        let filter_doc = match diag_filter {
            Bson::Document(d) => d,
            _ => panic!("expected document for diagnosis_code filter"),
        };
        let regex = filter_doc.get_str("$regex").expect("$prefix must translate to $regex");
        assert!(regex.starts_with("^E11"), "regex must start with ^E11, got {regex}");
    }

    #[test]
    fn dedup_stage_is_second() {
        let pipeline = build_pipeline(&admin_user(), &most_common_diagnoses_spec());
        // Stage 0: $match, Stage 1: $group(dedup)
        let dedup_group = pipeline[1].get_document("$group")
            .expect("stage index 1 must be $group");
        assert_eq!(
            dedup_group.get_str("_id").unwrap_or(""),
            "$row_pk",
            "the dedup group must always be the second stage"
        );
    }

    #[test]
    fn distinct_metric_adds_size_stage() {
        let spec = RunAggregation {
            collection: "encounters".into(),
            filter: serde_json::json!({}),
            group_by: vec!["clinic".into()],
            metric: Metric { op: MetricOp::Distinct, field: Some("patient_id".into()) },
            time_bucket: None,
            sort: None,
            top_n: Some(20),
        };
        let pipeline = build_pipeline(&test_user(), &spec);
        let has_size = pipeline.iter().any(|stage| {
            stage
                .get_document("$addFields")
                .ok()
                .and_then(|af| af.get_document("value").ok())
                .map(|v| v.contains_key("$size"))
                .unwrap_or(false)
        });
        assert!(has_size, "Distinct metric must add a $size stage");
    }

    #[test]
    fn bucket_expr_uses_convert_wrapper() {
        // Stage E: the time-bucket expression must wrap the field in $convert so
        // string-stored dates (Postgres to_jsonb output) are coerced to BSON Date.
        use crate::aggregation::spec::{BucketUnit, TimeBucket};
        let tb = TimeBucket { field: "encounter_date".into(), unit: BucketUnit::Month };
        let expr_bson = build_bucket_expr(&tb);
        let expr_json =
            serde_json::to_string(&expr_bson.into_relaxed_extjson()).unwrap_or_default();
        assert!(
            expr_json.contains("$convert"),
            "bucket expr must include $convert for string-to-date coercion; got: {expr_json}"
        );
    }
}
