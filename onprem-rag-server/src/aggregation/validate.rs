//! Aggregation spec validator.
//!
//! Runs after the planning model emits a `RunAggregation` and before the executor
//! ever touches DocumentDB. Every field name and collection is checked against the
//! catalog allow-list; dangerous filter operators are blocked; `top_n` is clamped
//! to `MAX_TOP_N`.
//!
//! # Security contract
//!
//! `validate` + `execute` co-operate to enforce safety:
//! - `validate` checks field/collection names and dangerous operators. It takes an
//!   explicit `&Catalog` so the caller always passes the current runtime catalog
//!   (from `AppState::catalog()`), not a stale static snapshot.
//! - `execute` injects the mandatory authz `$match` prefix server-side, so the
//!   caller never controls the access-scoping part of the pipeline.
//! - No aggregate stage is ever emitted by user-controlled input.

use super::catalog::Catalog;
use super::spec::{MAX_TOP_N, RunAggregation};
use crate::error::{AppError, AppResult};

/// Operators that are never allowed inside a filter value, regardless of context.
/// `$where` and `$function` execute arbitrary JavaScript — unacceptable for PHI.
const BLOCKED_OPERATORS: &[&str] = &["$where", "$function", "$accumulator", "$expr"];

/// Validate `spec` against the catalog allow-list and return a sanitized copy with
/// `top_n` clamped to `MAX_TOP_N`.
///
/// `catalog` must be the current runtime catalog from `AppState::catalog()` so the
/// allow-list reflects the most recent ingested data.
///
/// Returns `AppError::BadRequest` if:
/// - `collection` is not in the catalog
/// - any field in `group_by`, `filter`, `metric.field`, or `time_bucket.field` is
///   not in the catalog for that collection (after synonym resolution)
/// - any filter value contains a blocked operator (`$where`, `$function`, etc.)
///
/// `top_n` above `MAX_TOP_N` is silently clamped so partial results are still
/// returned rather than rejecting the request.
pub fn validate(spec: &RunAggregation, catalog: &Catalog) -> AppResult<RunAggregation> {
    // --- collection allow-list -----------------------------------------------
    if !catalog.has_collection(&spec.collection) {
        let avail: Vec<&str> = {
            let mut k: Vec<&str> = catalog.collections.keys().map(String::as_str).collect();
            k.sort();
            k
        };
        return Err(AppError::BadRequest(format!(
            "unknown collection '{}'; allowed: {}",
            spec.collection,
            if avail.is_empty() { "(none — ingest data first)".to_string() } else { avail.join(", ") }
        )));
    }

    // Helper: resolve synonym then check allow-list for this collection.
    let check_field = |field: &str| -> AppResult<String> {
        let canonical = catalog.resolve_synonym_owned(field);
        if !catalog.has_field(&spec.collection, &canonical) {
            return Err(AppError::BadRequest(format!(
                "field '{}' is not allowed for collection '{}' (resolved canonical: '{}')",
                field, spec.collection, canonical
            )));
        }
        Ok(canonical)
    };

    // --- group_by fields ----------------------------------------------------
    let mut group_by = Vec::with_capacity(spec.group_by.len());
    for f in &spec.group_by {
        group_by.push(check_field(f)?);
    }

    // --- filter field names + dangerous operator check ----------------------
    let filter = &spec.filter;
    if let Some(obj) = filter.as_object() {
        for (key, value) in obj {
            check_field(key)?;
            check_filter_value(value)?;
        }
    }

    // --- metric field -------------------------------------------------------
    let mut metric = spec.metric.clone();
    if let Some(ref f) = spec.metric.field {
        metric.field = Some(check_field(f)?);
    }

    // --- time_bucket field --------------------------------------------------
    let time_bucket = if let Some(ref tb) = spec.time_bucket {
        let canonical = check_field(&tb.field)?;
        Some(super::spec::TimeBucket { field: canonical, unit: tb.unit.clone() })
    } else {
        None
    };

    // --- sort field ---------------------------------------------------------
    // Sort `by` may be "value" (the metric result) or a group dimension. We
    // don't enforce the allow-list on "value" since it is the computed metric.
    let sort = if let Some(ref s) = spec.sort {
        if s.by != "value" {
            check_field(&s.by)?;
        }
        Some(super::spec::Sort { by: s.by.clone(), dir: s.dir.clone() })
    } else {
        None
    };

    // --- clamp top_n --------------------------------------------------------
    let top_n = spec.top_n.map(|n| n.min(MAX_TOP_N));

    Ok(RunAggregation {
        collection: spec.collection.clone(),
        filter: spec.filter.clone(),
        group_by,
        metric,
        time_bucket,
        sort,
        top_n,
    })
}

/// Recursively check a filter value for blocked operators.
fn check_filter_value(value: &serde_json::Value) -> AppResult<()> {
    match value {
        serde_json::Value::Object(obj) => {
            for key in obj.keys() {
                if BLOCKED_OPERATORS.contains(&key.as_str()) {
                    return Err(AppError::BadRequest(format!(
                        "operator '{key}' is not permitted in aggregation filters"
                    )));
                }
                // Recurse into nested values (e.g. `$and: [{...}, {...}]`).
            }
            for v in obj.values() {
                check_filter_value(v)?;
            }
        }
        serde_json::Value::Array(arr) => {
            for item in arr {
                check_filter_value(item)?;
            }
        }
        _ => {}
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::aggregation::catalog::build_hardcoded;
    use crate::aggregation::spec::{Metric, MetricOp};

    fn simple_spec(collection: &str, field: &str) -> RunAggregation {
        RunAggregation {
            collection: collection.to_string(),
            filter: serde_json::json!({}),
            group_by: vec![field.to_string()],
            metric: Metric { op: MetricOp::Count, field: None },
            time_bucket: None,
            sort: None,
            top_n: None,
        }
    }

    #[test]
    fn rejects_unknown_collection() {
        let cat = build_hardcoded();
        let spec = simple_spec("nonexistent_table", "diagnosis_display");
        let err = validate(&spec, &cat).unwrap_err();
        match err {
            AppError::BadRequest(msg) => assert!(msg.contains("unknown collection")),
            other => panic!("expected BadRequest, got {other:?}"),
        }
    }

    #[test]
    fn rejects_unknown_field_in_group_by() {
        let cat = build_hardcoded();
        let spec = simple_spec("encounters", "totally_made_up_column");
        let err = validate(&spec, &cat).unwrap_err();
        match err {
            AppError::BadRequest(msg) => assert!(msg.contains("not allowed")),
            other => panic!("expected BadRequest, got {other:?}"),
        }
    }

    #[test]
    fn rejects_blocked_operator_in_filter() {
        let cat = build_hardcoded();
        let spec = RunAggregation {
            collection: "encounters".to_string(),
            filter: serde_json::json!({ "diagnosis_code": { "$where": "this.x > 0" } }),
            group_by: vec!["diagnosis_display".to_string()],
            metric: Metric { op: MetricOp::Count, field: None },
            time_bucket: None,
            sort: None,
            top_n: None,
        };
        let err = validate(&spec, &cat).unwrap_err();
        match err {
            AppError::BadRequest(msg) => assert!(msg.contains("$where")),
            other => panic!("expected BadRequest, got {other:?}"),
        }
    }

    #[test]
    fn clamps_top_n_above_max() {
        let cat = build_hardcoded();
        let spec = RunAggregation {
            collection: "encounters".to_string(),
            filter: serde_json::json!({}),
            group_by: vec!["diagnosis_display".to_string()],
            metric: Metric { op: MetricOp::Count, field: None },
            time_bucket: None,
            sort: None,
            top_n: Some(1000),
        };
        let sanitized = validate(&spec, &cat).expect("should succeed");
        assert_eq!(sanitized.top_n, Some(MAX_TOP_N));
    }

    #[test]
    fn accepts_valid_spec_unchanged() {
        let cat = build_hardcoded();
        let spec = RunAggregation {
            collection: "encounters".to_string(),
            filter: serde_json::json!({ "diagnosis_code": { "$prefix": "E11" } }),
            group_by: vec!["diagnosis_display".to_string()],
            metric: Metric { op: MetricOp::Count, field: None },
            time_bucket: None,
            sort: Some(super::super::spec::Sort {
                by: "value".to_string(),
                dir: super::super::spec::SortDir::Desc,
            }),
            top_n: Some(10),
        };
        let sanitized = validate(&spec, &cat).expect("should succeed");
        assert_eq!(sanitized.top_n, Some(10));
        assert_eq!(sanitized.collection, "encounters");
    }

    #[test]
    fn resolves_synonym_in_group_by() {
        let cat = build_hardcoded();
        // "date_of_birth" should resolve to "dob" which is allowed in "patients"
        let spec = RunAggregation {
            collection: "patients".to_string(),
            filter: serde_json::json!({}),
            group_by: vec!["date_of_birth".to_string()],
            metric: Metric { op: MetricOp::Count, field: None },
            time_bucket: None,
            sort: None,
            top_n: None,
        };
        let sanitized = validate(&spec, &cat).expect("synonym should resolve");
        assert_eq!(sanitized.group_by, vec!["dob".to_string()]);
    }
}
