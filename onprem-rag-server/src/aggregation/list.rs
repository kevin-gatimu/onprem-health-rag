//! Paginated record-list executor.
//!
//! Translates a validated `RunList` spec into a DocumentDB aggregation pipeline
//! that deduplicates by `row_pk`, applies the user filter, sorts, skips, and
//! limits. Returns the page rows, the total deduped count, and the executed
//! pipeline for provenance.
//!
//! # Pipeline structure (in order)
//!
//! 1. `$match` — authz scope AND `{table: collection}` AND translated user filter.
//! 2. `$group { _id: "$row_pk", doc: { $first: "$$ROOT" } }` — dedup by row_pk.
//!    Same invariant as the aggregation executor: count per source record, not per chunk.
//! 3. `$replaceRoot { newRoot: "$doc" }` — flatten the dedup wrapper.
//! 4. `$sort` — default ascending `row_pk` for stable pagination.
//! 5. `$skip` — offset for pagination.
//! 6. `$limit` — clamped page size.
//! 7. `$project` — requested `columns` as `fields.<col>` plus identity fields.
//!
//! Total row count is obtained via a parallel `$count` stage on the deduped set
//! (before skip/limit) so the narration can say "showing 1-50 of 60".

use futures::TryStreamExt;
use mongodb::bson::{Bson, Document, doc};
use serde_json::Value as Json;

use super::catalog::Catalog;
use super::execute::translate_filter;
pub use super::spec::DEFAULT_LIST_LIMIT;
use super::spec::{MAX_LIST, RunList, Sort, SortDir};
use crate::auth::guard::AuthUser;
use crate::documentdb::DocumentDb;
use crate::error::{AppError, AppResult};

/// Hard timeout for list pipelines (milliseconds).
const LIST_MAX_TIME_MS: u64 = 30_000;

// ---------------------------------------------------------------------------
// Public entry point
// ---------------------------------------------------------------------------

/// Run the validated list spec and return `(rows, total, pipeline)`.
///
/// - `rows`: the page of deduped records as relaxed extended JSON objects.
/// - `total`: the total deduped row count before skip/limit.
/// - `pipeline`: the executed pipeline for provenance logging.
pub async fn run(
    db: &DocumentDb,
    auth: &AuthUser,
    spec: &RunList,
) -> AppResult<(Vec<Json>, u64, Vec<Document>)> {
    let pipeline = build_pipeline(auth, spec);
    let count_pipeline = build_count_pipeline(auth, spec);

    // Run page and count pipelines concurrently.
    let (rows_result, count_result) = tokio::join!(
        run_page_pipeline(db, &pipeline),
        run_count_pipeline(db, &count_pipeline),
    );

    let rows = rows_result?;
    let total = count_result?;

    tracing::info!(
        collection = %spec.collection,
        rows = rows.len(),
        total,
        user = %auth.username,
        "list query executed"
    );

    Ok((rows, total, pipeline))
}

async fn run_page_pipeline(db: &DocumentDb, pipeline: &[Document]) -> AppResult<Vec<Json>> {
    let mut cursor = db
        .records()
        .aggregate(pipeline.to_vec())
        .max_time(std::time::Duration::from_millis(LIST_MAX_TIME_MS))
        .await?;

    let mut rows = Vec::new();
    while let Some(doc) = cursor.try_next().await? {
        rows.push(Bson::Document(doc).into_relaxed_extjson());
    }
    Ok(rows)
}

async fn run_count_pipeline(db: &DocumentDb, pipeline: &[Document]) -> AppResult<u64> {
    let mut cursor = db
        .records()
        .aggregate(pipeline.to_vec())
        .max_time(std::time::Duration::from_millis(LIST_MAX_TIME_MS))
        .await?;

    if let Some(doc) = cursor.try_next().await? {
        // $count emits { "n": <count> }
        match doc.get("n") {
            Some(Bson::Int32(n)) => return Ok(*n as u64),
            Some(Bson::Int64(n)) => return Ok(*n as u64),
            Some(Bson::Double(n)) => return Ok(*n as u64),
            _ => {}
        }
    }
    Ok(0)
}

// ---------------------------------------------------------------------------
// Pipeline builders (pure — testable without a live DB)
// ---------------------------------------------------------------------------

/// Build the page pipeline for `spec`.
pub fn build_pipeline(auth: &AuthUser, spec: &RunList) -> Vec<Document> {
    let limit = spec.limit.unwrap_or(DEFAULT_LIST_LIMIT).min(MAX_LIST);
    let offset = spec.offset;

    let mut pipeline = Vec::new();

    // Stage 1: $match — active generation, authz, table scope, and user filter.
    // The table scope is the correctness invariant: row_pk is unique within a table,
    // so the dedup stage below produces one row per source record only when scoped.
    let mut match_doc = build_authz_filter(auth);
    match_doc.insert("active", true);
    match_doc.insert("table", &spec.collection);
    for (k, v) in translate_filter(&spec.filter) {
        match_doc.insert(k, v);
    }
    pipeline.push(doc! { "$match": match_doc });

    // Stage 2: $group — dedup by row_pk (same invariant as aggregation executor).
    // Correctness depends on the table match above: row_pk is only unique per table.
    pipeline.push(doc! {
        "$group": {
            "_id": "$row_pk",
            "doc": { "$first": "$$ROOT" }
        }
    });

    // Stage 3: $replaceRoot — flatten the dedup wrapper.
    pipeline.push(doc! { "$replaceRoot": { "newRoot": "$doc" } });

    // Stage 4: $sort — default ascending row_pk for stable pagination.
    let (sort_field, sort_dir) = resolve_sort(spec);
    pipeline.push(doc! { "$sort": { sort_field: sort_dir } });

    // Stage 5: $skip — pagination offset.
    if offset > 0 {
        pipeline.push(doc! { "$skip": offset as i64 });
    }

    // Stage 6: $limit — clamped page size.
    pipeline.push(doc! { "$limit": limit as i64 });

    // Stage 7: $project — requested columns plus identity fields.
    if !spec.columns.is_empty() {
        let mut proj = Document::new();
        proj.insert("_id", 1);
        proj.insert("source_id", 1);
        proj.insert("table", 1);
        proj.insert("row_pk", 1);
        for col in &spec.columns {
            proj.insert(format!("fields.{col}"), 1);
        }
        pipeline.push(doc! { "$project": proj });
    }

    pipeline
}

/// Build the count pipeline: same match + dedup, then $count.
pub fn build_count_pipeline(auth: &AuthUser, spec: &RunList) -> Vec<Document> {
    let mut pipeline = Vec::new();

    let mut match_doc = build_authz_filter(auth);
    match_doc.insert("active", true);
    match_doc.insert("table", &spec.collection);
    for (k, v) in translate_filter(&spec.filter) {
        match_doc.insert(k, v);
    }
    pipeline.push(doc! { "$match": match_doc });

    pipeline.push(doc! {
        "$group": { "_id": "$row_pk" }
    });

    pipeline.push(doc! { "$count": "n" });

    pipeline
}

fn resolve_sort(spec: &RunList) -> (String, i32) {
    match &spec.sort {
        Some(Sort { by, dir }) => {
            let db_field = if by == "row_pk" || by == "_id" {
                by.clone()
            } else {
                format!("fields.{by}")
            };
            let dir_int = if *dir == SortDir::Asc { 1 } else { -1 };
            (db_field, dir_int)
        }
        None => ("row_pk".to_string(), 1),
    }
}

fn build_authz_filter(_auth: &AuthUser) -> Document {
    // TODO(phase-acl): narrow to source_ids / clinic when per-user ACLs land.
    Document::new()
}

// ---------------------------------------------------------------------------
// Validator
// ---------------------------------------------------------------------------

/// Operators blocked in list filters (same set as aggregation validator).
const BLOCKED_OPERATORS: &[&str] = &["$where", "$function", "$accumulator", "$expr"];

/// Validate a `RunList` spec against the catalog. Returns a sanitized copy with
/// `limit` clamped to `MAX_LIST` and empty `columns` filled from the catalog.
pub fn validate_list(spec: &RunList, catalog: &Catalog) -> AppResult<RunList> {
    // Collection allow-list.
    if !catalog.has_collection(&spec.collection) {
        let avail: Vec<&str> = {
            let mut k: Vec<&str> = catalog.collections.keys().map(String::as_str).collect();
            k.sort();
            k
        };
        return Err(AppError::BadRequest(format!(
            "unknown collection '{}'; allowed: {}",
            spec.collection,
            if avail.is_empty() {
                "(none — ingest data first)".to_string()
            } else {
                avail.join(", ")
            }
        )));
    }

    let check_field = |field: &str| -> AppResult<String> {
        let canonical = catalog.resolve_synonym_owned(field);
        if !catalog.has_field(&spec.collection, &canonical) {
            return Err(AppError::BadRequest(format!(
                "field '{}' is not allowed for collection '{}' (resolved: '{}')",
                field, spec.collection, canonical
            )));
        }
        Ok(canonical)
    };

    // Filter: check field names and blocked operators.
    if let Some(obj) = spec.filter.as_object() {
        for (key, value) in obj {
            check_field(key)?;
            check_list_filter_value(value)?;
        }
    }

    // Columns: resolve synonyms and check allow-list. Empty -> fill from catalog.
    let columns: Vec<String> = if spec.columns.is_empty() {
        // Default projection: first few fields from the catalog (alphabetical).
        catalog
            .collections
            .get(&spec.collection)
            .map(|meta| meta.fields.iter().take(8).map(|f| f.name.clone()).collect())
            .unwrap_or_default()
    } else {
        let mut cols = Vec::with_capacity(spec.columns.len());
        for col in &spec.columns {
            cols.push(check_field(col)?);
        }
        cols
    };

    // Sort field.
    let sort = if let Some(ref s) = spec.sort {
        if s.by != "row_pk" && s.by != "_id" {
            check_field(&s.by)?;
        }
        Some(Sort {
            by: s.by.clone(),
            dir: s.dir.clone(),
        })
    } else {
        None
    };

    // Clamp limit and cap offset.
    let limit = Some(spec.limit.unwrap_or(DEFAULT_LIST_LIMIT).min(MAX_LIST));

    Ok(RunList {
        collection: spec.collection.clone(),
        filter: spec.filter.clone(),
        columns,
        sort,
        limit,
        offset: spec.offset,
    })
}

fn check_list_filter_value(value: &serde_json::Value) -> AppResult<()> {
    match value {
        serde_json::Value::Object(obj) => {
            for key in obj.keys() {
                if BLOCKED_OPERATORS.contains(&key.as_str()) {
                    return Err(AppError::BadRequest(format!(
                        "operator '{key}' is not permitted in list filters"
                    )));
                }
            }
            for v in obj.values() {
                check_list_filter_value(v)?;
            }
        }
        serde_json::Value::Array(arr) => {
            for item in arr {
                check_list_filter_value(item)?;
            }
        }
        _ => {}
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Citation synthesis
// ---------------------------------------------------------------------------

/// Synthesise a `citations` JSON string from list rows so the UI can render
/// source citations even for list answers (re-using the existing citations slot).
///
/// Each row becomes a minimal citation with `id`, `source_id`, `row_pk`, and
/// `fields` extracted from the row document.
pub fn rows_to_citations_json(rows: &[Json], collection: &str) -> String {
    let citations: Vec<Json> = rows
        .iter()
        .enumerate()
        .map(|(i, row)| {
            let obj = row.as_object();
            let row_pk = obj
                .and_then(|o| o.get("row_pk"))
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let source_id = obj
                .and_then(|o| o.get("source_id"))
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let fields = obj
                .and_then(|o| o.get("fields"))
                .cloned()
                .unwrap_or(serde_json::json!({}));
            serde_json::json!({
                "id": format!("{collection}-{i}"),
                "source_id": source_id,
                "table": collection,
                "row_pk": row_pk,
                "chunk_index": 0,
                "text": row_pk,
                "fields": fields,
                "score": 1.0,
                "reranked": false,
                "vector_rank": null,
                "text_rank": null,
                "fused_score": 1.0,
                "rerank_score": null,
            })
        })
        .collect();
    serde_json::to_string(&citations).unwrap_or_else(|_| "[]".to_string())
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::auth::{Role, guard::AuthUser};

    fn test_user() -> AuthUser {
        AuthUser {
            id: "u1".into(),
            username: "tester".into(),
            role: Role::Doctor,
        }
    }

    fn patients_spec() -> RunList {
        RunList {
            collection: "patients".into(),
            filter: serde_json::json!({}),
            columns: vec![],
            sort: None,
            limit: Some(50),
            offset: 0,
        }
    }

    #[test]
    fn pipeline_has_table_match_at_stage_0() {
        let spec = patients_spec();
        let pipeline = build_pipeline(&test_user(), &spec);

        assert!(pipeline[0].contains_key("$match"), "stage 0 must be $match");
        let match_doc = pipeline[0].get_document("$match").unwrap();
        assert_eq!(
            match_doc.get_str("table").unwrap_or(""),
            "patients",
            "stage-0 $match must scope to the physical table"
        );
    }

    #[test]
    fn pipeline_has_dedup_group_at_stage_1() {
        let pipeline = build_pipeline(&test_user(), &patients_spec());
        let group = pipeline[1]
            .get_document("$group")
            .expect("stage 1 must be $group");
        assert_eq!(group.get_str("_id").unwrap_or(""), "$row_pk");
    }

    #[test]
    fn pipeline_has_replace_root_at_stage_2() {
        let pipeline = build_pipeline(&test_user(), &patients_spec());
        assert!(
            pipeline[2].contains_key("$replaceRoot"),
            "stage 2 must be $replaceRoot"
        );
    }

    #[test]
    fn pipeline_has_sort_skip_limit_in_order() {
        let spec = RunList {
            offset: 10,
            ..patients_spec()
        };
        let pipeline = build_pipeline(&test_user(), &spec);

        let sort_idx = pipeline
            .iter()
            .position(|s| s.contains_key("$sort"))
            .unwrap();
        let skip_idx = pipeline
            .iter()
            .position(|s| s.contains_key("$skip"))
            .unwrap();
        let limit_idx = pipeline
            .iter()
            .position(|s| s.contains_key("$limit"))
            .unwrap();

        assert!(sort_idx < skip_idx, "$sort must precede $skip");
        assert!(skip_idx < limit_idx, "$skip must precede $limit");
    }

    #[test]
    fn count_pipeline_ends_with_count_stage() {
        let pipeline = build_count_pipeline(&test_user(), &patients_spec());
        let last = pipeline.last().expect("count pipeline must not be empty");
        assert!(
            last.contains_key("$count"),
            "last stage of count pipeline must be $count"
        );
    }

    #[test]
    fn validate_list_rejects_unknown_collection() {
        let cat = crate::aggregation::catalog::build_hardcoded();
        let spec = RunList {
            collection: "no_such_table".into(),
            ..patients_spec()
        };
        let err = validate_list(&spec, &cat).unwrap_err();
        match err {
            AppError::BadRequest(msg) => assert!(msg.contains("unknown collection")),
            other => panic!("expected BadRequest, got {other:?}"),
        }
    }

    #[test]
    fn validate_list_fills_default_columns() {
        let cat = crate::aggregation::catalog::build_hardcoded();
        let spec = RunList {
            columns: vec![],
            ..patients_spec()
        };
        let validated = validate_list(&spec, &cat).expect("should succeed");
        assert!(
            !validated.columns.is_empty(),
            "default columns must be filled"
        );
    }

    #[test]
    fn validate_list_clamps_limit() {
        let cat = crate::aggregation::catalog::build_hardcoded();
        let spec = RunList {
            limit: Some(999),
            ..patients_spec()
        };
        let validated = validate_list(&spec, &cat).expect("should succeed");
        assert_eq!(validated.limit, Some(MAX_LIST));
    }

    #[test]
    fn list_citations_satisfy_persistence_passage_contract() {
        let rows = vec![serde_json::json!({
            "source_id": "source-1",
            "row_pk": "patient-42",
            "fields": { "name": "Example Patient" }
        })];

        let json = rows_to_citations_json(&rows, "patients");
        let passages: Vec<crate::retrieval::Passage> =
            serde_json::from_str(&json).expect("list citations must persist as passages");

        assert_eq!(passages.len(), 1);
        assert_eq!(passages[0].table, "patients");
        assert_eq!(passages[0].row_pk, "patient-42");
    }
}
