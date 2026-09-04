use crate::config::Config;
use std::collections::HashSet;
use std::sync::{Arc, LazyLock};
use std::time::Duration;

use crate::connectors::{SourceSpec, TableSchema, connector};
use crate::documentdb::{DocumentDb, SCHEMA_CATALOG};
use crate::embed::embed_documents;
use crate::error::{AppError, AppResult};
use crate::ontology::binder::BindingOverrides;
use crate::state::BindingCache;
use mongodb::bson::{Bson, DateTime as BsonDateTime, Document, doc};
use sha2::{Digest, Sha256};
use tokio::sync::Mutex;

use super::spec::{CardColumn, CardFkEdge, ColumnProfile, TableCard};

static REFRESHING_SOURCES: LazyLock<Mutex<HashSet<String>>> =
    LazyLock::new(|| Mutex::new(HashSet::new()));

/// Build or refresh schema cards for every table in a source. Replaces all cards
/// for this source so removed or renamed tables cannot remain routing candidates.
///
/// `binding_cache` is the `AppState::bindings` handle; after a successful binding
/// rebuild the new binding is inserted so the running server sees it immediately
/// without a restart.
pub async fn refresh_catalog(
    db: &DocumentDb,
    config: &Config,
    spec: &SourceSpec,
    source_id: &str,
    binding_cache: &BindingCache,
) -> AppResult<usize> {
    refresh_catalog_with_trigger(db, config, spec, source_id, "system", binding_cache).await
}

pub async fn refresh_catalog_with_trigger(
    db: &DocumentDb,
    config: &Config,
    spec: &SourceSpec,
    source_id: &str,
    trigger: &str,
    binding_cache: &BindingCache,
) -> AppResult<usize> {
    {
        let mut refreshing = REFRESHING_SOURCES.lock().await;
        if !refreshing.insert(source_id.to_string()) {
            return Err(AppError::TooManyRequests(
                "schema metadata refresh already in progress for this source".into(),
            ));
        }
    }

    let started_at = BsonDateTime::now();
    let previous_hash = current_hash(db, source_id).await;
    let result = refresh_catalog_inner(db, config, spec, source_id, binding_cache).await;
    let completed_at = BsonDateTime::now();
    match &result {
        Ok(count) => {
            let new_hash = current_hash(db, source_id).await;
            let _ = record_history(
                db,
                source_id,
                trigger,
                "success",
                started_at,
                completed_at,
                previous_hash.as_deref(),
                new_hash.as_deref(),
                *count as i64,
                None,
            )
            .await;
        }
        Err(error) => {
            let message = sanitize_error(&error.to_string());
            let _ = db
                .schema_catalog_state()
                .update_one(
                    doc! { "_id": source_id },
                    doc! {
                        "$set": {
                            "status": "error",
                            "health": "error",
                            "last_check_at": completed_at,
                            "last_error": &message,
                        },
                        "$inc": { "consecutive_failures": 1i64 },
                    },
                )
                .upsert(true)
                .await;
            let _ = record_history(
                db,
                source_id,
                trigger,
                "failed",
                started_at,
                completed_at,
                previous_hash.as_deref(),
                None,
                0,
                Some(&message),
            )
            .await;
        }
    }
    REFRESHING_SOURCES.lock().await.remove(source_id);
    result
}

async fn refresh_catalog_inner(
    db: &DocumentDb,
    config: &Config,
    spec: &SourceSpec,
    source_id: &str,
    binding_cache: &BindingCache,
) -> AppResult<usize> {
    let conn = connector(spec);
    let tables = conn.get_schema().await?;

    let sample_limit = config.router.nl2sql_sample_values;
    let profile_limit = config.schema_profile_sample_rows;
    let fetch_limit = sample_limit.max(profile_limit);
    let mut cards: Vec<TableCard> = Vec::with_capacity(tables.len());

    for table in &tables {
        let rows = if fetch_limit > 0 {
            conn.fetch_table_page(&table.name, &[], None, 0, fetch_limit as i64)
                .await
                .unwrap_or_default()
        } else {
            Vec::new()
        };
        let sample_values = extract_samples(&rows[..rows.len().min(sample_limit)], 3);
        let profiles = build_profiles(table, &rows[..rows.len().min(profile_limit)]);

        let columns: Vec<CardColumn> = table
            .columns
            .iter()
            .map(|c| CardColumn {
                name: c.name.clone(),
                type_: c.type_.clone(),
                nullable: c.nullable,
                is_primary_key: c.is_primary_key,
                is_foreign_key: c.is_foreign_key,
                sample_values: sample_values.get(&c.name).cloned().unwrap_or_default(),
                profile: profiles.get(&c.name).cloned().unwrap_or_default(),
            })
            .collect();

        let fk_edges: Vec<CardFkEdge> = table
            .fk_edges
            .iter()
            .map(|e| CardFkEdge {
                column: e.column.clone(),
                ref_table: e.ref_table.clone(),
                ref_column: e.ref_column.clone(),
            })
            .collect();

        let card_text = build_card_text(&table.name, table.row_count, &columns, &fk_edges);

        cards.push(TableCard {
            source_id: source_id.to_string(),
            table_name: table.name.clone(),
            row_count: table.row_count,
            columns,
            fk_edges,
            card_vector: None,
            card_text,
        });
    }

    // Embed all card texts in one batch before replacing the existing catalog.
    // A failed embed therefore leaves the last known-good cards intact.
    if !cards.is_empty() {
        let texts: Vec<String> = cards.iter().map(|c| c.card_text.clone()).collect();
        let vectors = embed_documents(config, texts).await?;
        for (card, vec) in cards.iter_mut().zip(vectors.into_iter()) {
            card.card_vector = Some(vec);
        }
    }

    let count = cards.len();
    let version = uuid::Uuid::now_v7().to_string();
    let schema_hash = structural_fingerprint(&tables);
    let captured_at = BsonDateTime::now();
    let coll = db.schema_catalog();

    // Write a complete new generation before moving the active pointer. Readers
    // therefore keep using the last known-good catalog throughout a refresh.
    let mut documents = Vec::with_capacity(cards.len());
    for card in &cards {
        let mut document = card_to_bson(card)?;
        document.insert("catalog_version", &version);
        document.insert("schema_hash", &schema_hash);
        document.insert("captured_at", captured_at);
        documents.push(document);
    }
    if !documents.is_empty() {
        if let Err(error) = coll.insert_many(documents).await {
            let _ = coll
                .delete_many(doc! { "source_id": source_id, "catalog_version": &version })
                .await;
            return Err(error.into());
        }
    }

    db.schema_catalog_state()
        .update_one(
            doc! { "_id": source_id },
            doc! { "$set": {
                "active_version": &version,
                "schema_hash": &schema_hash,
                "captured_at": captured_at,
                "last_success_at": captured_at,
                "last_check_at": captured_at,
                "table_count": count as i64,
                "status": "active",
                "health": "healthy",
                "drift_detected": false,
                "consecutive_failures": 0i64,
                "last_error": Bson::Null,
            }},
        )
        .upsert(true)
        .await?;

    // Cleanup happens only after the active generation is durable. A cleanup
    // failure is harmless because linkers filter by the active pointer.
    if let Err(error) = coll
        .delete_many(doc! {
            "source_id": source_id,
            "catalog_version": { "$ne": &version },
        })
        .await
    {
        tracing::warn!(source_id, %error, "failed to remove inactive schema catalog versions");
    }

    super::linker::invalidate_source(source_id);

    // Non-fatal binding rebuild: persists the updated binding to DocumentDB AND
    // updates the in-memory BindingCache so the running server sees it immediately
    // (fixes the "write-only hook" defect — plan 07 agent tabs read from
    // AppState::bindings, not from DocumentDB).
    //
    // Descriptor vectors stay None (degraded mode): re-embedding every concept
    // for each catalog refresh would be expensive and is not needed for correctness.
    // The binding is marked degraded: true to signal this.
    //
    // Overrides ARE applied: they are read from the persisted `schema_metadata_overrides`
    // document so an admin's concept or role override survives every catalog refresh.
    if config.binding_enabled {
        let overrides = load_binding_overrides(db, source_id).await;
        match crate::ontology::binder::build_binding(
            &cards,
            db,
            config,
            source_id,
            None,
            overrides.as_ref(),
        )
        .await
        {
            Ok(binding) => {
                let tables_count = binding.tables.len();
                crate::ontology::store::save_binding_nonfatal(db, &binding).await;
                let arc = Arc::new(binding);
                match binding_cache.write() {
                    Ok(mut w) => {
                        w.insert(source_id.to_string(), arc);
                        tracing::info!(
                            source_id,
                            tables = tables_count,
                            "schema binding rebuilt, persisted, and cached after catalog refresh (degraded)"
                        );
                    }
                    Err(_) => {
                        tracing::warn!(
                            source_id,
                            "binding cache lock poisoned; binding persisted to DB but not cached"
                        );
                    }
                }
            }
            Err(e) => {
                tracing::warn!(
                    source_id,
                    error = %e,
                    "non-fatal: schema binding rebuild failed after catalog refresh"
                );
            }
        }
    }

    tracing::info!(source_id, count, %version, %schema_hash, "schema catalog refreshed");
    Ok(count)
}

/// Read persisted `MetadataOverrides` for a source and convert them into the
/// `BindingOverrides` format understood by the binder.
///
/// Returns `None` if no overrides document exists, the DB query fails, or the
/// document cannot be deserialized — all treated as "no overrides" so the hook
/// remains non-fatal.
async fn load_binding_overrides(db: &DocumentDb, source_id: &str) -> Option<BindingOverrides> {
    use crate::ontology::concepts::EntityConcept;
    use crate::ontology::roles::ColumnRole;
    use std::collections::HashMap;

    let doc = db
        .schema_metadata_overrides()
        .find_one(mongodb::bson::doc! { "_id": source_id })
        .await
        .ok()??; // None on error or missing doc — non-fatal

    // Deserialize the stored MetadataOverrides.  We read the raw document here
    // to avoid a circular import between catalog.rs and nl2sql/routes.rs.
    let table_concepts_arr = doc
        .get_array("table_concepts")
        .ok()
        .cloned()
        .unwrap_or_default();
    let column_roles_arr = doc
        .get_array("column_roles")
        .ok()
        .cloned()
        .unwrap_or_default();

    let mut table_concepts: HashMap<String, Option<EntityConcept>> = HashMap::new();
    for item in table_concepts_arr.iter().filter_map(|b| b.as_document()) {
        let table = item.get_str("table").ok()?.to_string();
        let concept = item
            .get_str("concept")
            .ok()
            .and_then(EntityConcept::from_slug);
        table_concepts.insert(table, concept);
    }

    let mut column_roles: HashMap<String, HashMap<String, ColumnRole>> = HashMap::new();
    for item in column_roles_arr.iter().filter_map(|b| b.as_document()) {
        let table = item.get_str("table").ok()?.to_string();
        let column = item.get_str("column").ok()?.to_string();
        let role = item
            .get_str("role")
            .ok()
            .and_then(ColumnRole::from_slug)?;
        column_roles
            .entry(table)
            .or_default()
            .insert(column, role);
    }

    if table_concepts.is_empty() && column_roles.is_empty() {
        return None;
    }

    Some(BindingOverrides {
        table_concepts,
        column_roles,
    })
}

/// Stable structure-only hash used by the drift poller. Data growth and profile
/// changes do not trigger unnecessary catalog generations.
pub fn structural_fingerprint(tables: &[TableSchema]) -> String {
    let mut tables: Vec<&TableSchema> = tables.iter().collect();
    tables.sort_by_key(|table| table.name.to_ascii_lowercase());
    let mut hasher = Sha256::new();
    for table in tables {
        hasher.update(table.name.as_bytes());
        let mut columns: Vec<_> = table.columns.iter().collect();
        columns.sort_by_key(|column| column.name.to_ascii_lowercase());
        for column in columns {
            hasher.update(column.name.as_bytes());
            hasher.update(column.type_.as_bytes());
            hasher.update([
                column.nullable as u8,
                column.is_primary_key as u8,
                column.is_foreign_key as u8,
            ]);
        }
        let mut edges: Vec<_> = table.fk_edges.iter().collect();
        edges.sort_by_key(|edge| (&edge.column, &edge.ref_table, &edge.ref_column));
        for edge in edges {
            hasher.update(edge.column.as_bytes());
            hasher.update(edge.ref_table.as_bytes());
            hasher.update(edge.ref_column.as_bytes());
        }
    }
    format!("{:x}", hasher.finalize())
}

pub async fn poll_source_for_drift(
    db: &DocumentDb,
    config: &Config,
    spec: &SourceSpec,
    source_id: &str,
    binding_cache: &BindingCache,
) -> AppResult<bool> {
    if REFRESHING_SOURCES.lock().await.contains(source_id) {
        return Ok(false);
    }
    let started_at = BsonDateTime::now();
    let tables = connector(spec).get_schema().await?;
    let observed_hash = structural_fingerprint(&tables);
    let previous_hash = current_hash(db, source_id).await;
    let checked_at = BsonDateTime::now();
    let changed = previous_hash.as_deref() != Some(observed_hash.as_str());

    db.schema_catalog_state()
        .update_one(
            doc! { "_id": source_id },
            doc! { "$set": {
                "last_check_at": checked_at,
                "drift_detected": changed,
                "health": if changed { "drift" } else { "healthy" },
            }},
        )
        .upsert(true)
        .await?;

    if changed {
        refresh_catalog_with_trigger(db, config, spec, source_id, "poll", binding_cache).await?;
    } else {
        record_history(
            db,
            source_id,
            "poll",
            "unchanged",
            started_at,
            checked_at,
            previous_hash.as_deref(),
            Some(&observed_hash),
            tables.len() as i64,
            None,
        )
        .await?;
    }
    Ok(changed)
}

pub fn spawn_schema_poller(db: DocumentDb, config: Config, binding_cache: BindingCache) {
    if config.schema_poll_interval_secs == 0 {
        return;
    }
    tokio::spawn(async move {
        let mut interval =
            tokio::time::interval(Duration::from_secs(config.schema_poll_interval_secs));
        interval.tick().await;
        loop {
            interval.tick().await;
            let source_ids = match crate::connectors::routes::connected_source_ids(&db).await {
                Ok(ids) => ids,
                Err(error) => {
                    tracing::warn!(%error, "schema drift poller could not list sources");
                    continue;
                }
            };
            let semaphore = Arc::new(tokio::sync::Semaphore::new(
                config.schema_poll_concurrency.max(1),
            ));
            let mut tasks = Vec::new();
            for source_id in source_ids {
                let db = db.clone();
                let config = config.clone();
                let semaphore = semaphore.clone();
                let binding_cache = binding_cache.clone();
                tasks.push(tokio::spawn(async move {
                    let Ok(_permit) = semaphore.acquire_owned().await else {
                        return;
                    };
                    let result = async {
                        let spec =
                            crate::connectors::routes::load_spec(&db, &config, &source_id).await?;
                        poll_source_for_drift(&db, &config, &spec, &source_id, &binding_cache).await
                    }
                    .await;
                    if let Err(error) = result {
                        if matches!(&error, AppError::TooManyRequests(_)) {
                            return;
                        }
                        let message = sanitize_error(&error.to_string());
                        let now = BsonDateTime::now();
                        let _ = db
                            .schema_catalog_state()
                            .update_one(
                                doc! { "_id": &source_id },
                                doc! { "$set": {
                                    "last_check_at": now,
                                    "health": "degraded",
                                    "last_error": &message,
                                }, "$inc": { "consecutive_failures": 1i64 } },
                            )
                            .upsert(true)
                            .await;
                        let _ = record_history(
                            &db,
                            &source_id,
                            "poll",
                            "failed",
                            now,
                            now,
                            current_hash(&db, &source_id).await.as_deref(),
                            None,
                            0,
                            Some(&message),
                        )
                        .await;
                        tracing::warn!(%source_id, %error, "schema drift poll failed");
                    }
                }));
            }
            for task in tasks {
                let _ = task.await;
            }
        }
    });
}

async fn current_hash(db: &DocumentDb, source_id: &str) -> Option<String> {
    db.schema_catalog_state()
        .find_one(doc! { "_id": source_id })
        .await
        .ok()
        .flatten()
        .and_then(|state| state.get_str("schema_hash").ok().map(str::to_string))
}

#[allow(clippy::too_many_arguments)]
async fn record_history(
    db: &DocumentDb,
    source_id: &str,
    trigger: &str,
    result: &str,
    started_at: BsonDateTime,
    completed_at: BsonDateTime,
    previous_hash: Option<&str>,
    observed_hash: Option<&str>,
    table_count: i64,
    error: Option<&str>,
) -> AppResult<()> {
    db.schema_catalog_history()
        .insert_one(doc! {
            "_id": uuid::Uuid::now_v7().to_string(),
            "source_id": source_id,
            "trigger": trigger,
            "result": result,
            "started_at": started_at,
            "completed_at": completed_at,
            "previous_hash": previous_hash.map(|value| Bson::String(value.to_string())).unwrap_or(Bson::Null),
            "observed_hash": observed_hash.map(|value| Bson::String(value.to_string())).unwrap_or(Bson::Null),
            "table_count": table_count,
            "error": error.map(|value| Bson::String(value.to_string())).unwrap_or(Bson::Null),
        })
        .await?;
    Ok(())
}

fn sanitize_error(error: &str) -> String {
    let single_line = error.replace(['\r', '\n'], " ");
    single_line.chars().take(500).collect()
}

fn build_profiles(
    table: &TableSchema,
    rows: &[crate::connectors::FetchedRow],
) -> std::collections::HashMap<String, ColumnProfile> {
    let mut profiles = std::collections::HashMap::new();
    for column in &table.columns {
        let mut distinct = HashSet::new();
        let mut nulls = 0usize;
        let mut bounds: Vec<String> = Vec::new();
        let type_name = column.type_.to_ascii_lowercase();
        let numeric = [
            "int", "decimal", "numeric", "real", "double", "float", "money",
        ]
        .iter()
        .any(|kind| type_name.contains(kind));
        let temporal = ["date", "time"].iter().any(|kind| type_name.contains(kind));

        for row in rows {
            match row.fields.get(&column.name) {
                None | Some(serde_json::Value::Null) => nulls += 1,
                Some(value) => {
                    distinct.insert(value.to_string());
                    if numeric || temporal {
                        if let Some(value) = crate::connectors::value_to_plain(value) {
                            bounds.push(value);
                        }
                    }
                }
            }
        }
        let sampled = rows.len();
        let non_null = sampled.saturating_sub(nulls);
        let approximate_distinct_count = if sampled == 0 {
            None
        } else if table.row_count <= sampled as i64 {
            Some(distinct.len() as i64)
        } else if non_null == 0 {
            Some(0)
        } else {
            Some(
                ((distinct.len() as f64 / non_null as f64) * table.row_count as f64).round() as i64,
            )
        };
        let (min, max) = if numeric {
            let mut parsed: Vec<(f64, String)> = bounds
                .into_iter()
                .filter_map(|value| value.parse::<f64>().ok().map(|number| (number, value)))
                .collect();
            parsed.sort_by(|a, b| a.0.total_cmp(&b.0));
            (
                parsed.first().map(|(_, value)| value.clone()),
                parsed.last().map(|(_, value)| value.clone()),
            )
        } else if temporal {
            bounds.sort();
            (bounds.first().cloned(), bounds.last().cloned())
        } else {
            (None, None)
        };
        profiles.insert(
            column.name.clone(),
            ColumnProfile {
                approximate_distinct_count,
                null_ratio: (sampled > 0).then_some(nulls as f64 / sampled as f64),
                min,
                max,
                sampled_rows: sampled as i64,
            },
        );
    }
    profiles
}

/// Create the cosmosSearch vector index and the $text index on `schema_catalog`.
pub async fn ensure_nl2sql_indexes(db: &DocumentDb, dims: usize) -> AppResult<()> {
    db.db
        .run_command(doc! {
            "createIndexes": SCHEMA_CATALOG,
            "indexes": [{
                "name": "schema_catalog_source_version",
                "key": { "source_id": 1, "catalog_version": 1, "table_name": 1 }
            }]
        })
        .await?;

    db.db
        .run_command(doc! {
            "createIndexes": SCHEMA_CATALOG,
            "indexes": [{
                "name": "schema_catalog_cardVector_cosmos",
                "key": { "cardVector": "cosmosSearch" },
                "cosmosSearchOptions": {
                    "kind": "vector-ivf",
                    "numLists": 100,
                    "similarity": "COS",
                    "dimensions": dims as i32,
                }
            }]
        })
        .await?;

    db.db
        .run_command(doc! {
            "createIndexes": SCHEMA_CATALOG,
            "indexes": [{
                "name": "schema_catalog_text",
                "key": { "card_text": "text" }
            }]
        })
        .await?;

    db.db
        .run_command(doc! {
            "createIndexes": crate::documentdb::SCHEMA_CATALOG_HISTORY,
            "indexes": [{
                "name": "schema_history_source_completed",
                "key": { "source_id": 1, "completed_at": -1 }
            }]
        })
        .await?;

    tracing::info!(dims, "ensured nl2sql schema catalog indexes");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{build_profiles, structural_fingerprint};
    use crate::connectors::{ColumnSchema, FetchedRow, TableSchema};
    use serde_json::{Map, json};

    fn table(row_count: i64) -> TableSchema {
        TableSchema {
            name: "patients".into(),
            row_count,
            columns: vec![
                ColumnSchema {
                    name: "age".into(),
                    type_: "integer".into(),
                    nullable: true,
                    is_primary_key: false,
                    is_foreign_key: false,
                    likely_pii: false,
                },
                ColumnSchema {
                    name: "created_at".into(),
                    type_: "timestamp".into(),
                    nullable: false,
                    is_primary_key: false,
                    is_foreign_key: false,
                    likely_pii: false,
                },
            ],
            fk_edges: Vec::new(),
        }
    }

    #[test]
    fn structural_hash_ignores_row_count_changes() {
        assert_eq!(
            structural_fingerprint(&[table(10)]),
            structural_fingerprint(&[table(999)])
        );
    }

    #[test]
    fn profiles_report_bounded_nulls_distincts_and_ranges() {
        let rows = vec![
            FetchedRow {
                pk: "1".into(),
                fields: Map::from_iter([
                    ("age".into(), json!(18)),
                    ("created_at".into(), json!("2024-01-01")),
                ]),
                text: String::new(),
            },
            FetchedRow {
                pk: "2".into(),
                fields: Map::from_iter([
                    ("age".into(), serde_json::Value::Null),
                    ("created_at".into(), json!("2024-02-01")),
                ]),
                text: String::new(),
            },
            FetchedRow {
                pk: "3".into(),
                fields: Map::from_iter([
                    ("age".into(), json!(42)),
                    ("created_at".into(), json!("2023-12-01")),
                ]),
                text: String::new(),
            },
        ];
        let profiles = build_profiles(&table(3), &rows);
        let age = &profiles["age"];
        assert_eq!(age.approximate_distinct_count, Some(2));
        assert_eq!(age.null_ratio, Some(1.0 / 3.0));
        assert_eq!(age.min.as_deref(), Some("18"));
        assert_eq!(age.max.as_deref(), Some("42"));
        let created = &profiles["created_at"];
        assert_eq!(created.min.as_deref(), Some("2023-12-01"));
        assert_eq!(created.max.as_deref(), Some("2024-02-01"));
    }

    /// Verify the cache-insert path: inserting a binding for a source replaces any
    /// previous entry and is immediately readable from the same Arc.
    ///
    /// Coverage note: this test covers the in-memory insert logic used by
    /// `refresh_catalog_inner`.  The end-to-end path
    /// (DB refresh → binding build → `save_binding_nonfatal` → cache insert →
    /// `GET /agents` sees updated tabs) depends on a live DocumentDB and is NOT
    /// tested here — it is covered by construction and the integration smoke test
    /// run against a dev deployment.
    #[test]
    fn binding_cache_insert_replaces_previous_entry() {
        use super::BindingCache;
        use crate::ontology::binding::SchemaBinding;
        use std::collections::HashMap;
        use std::sync::{Arc, RwLock};

        let cache: BindingCache = Arc::new(RwLock::new(HashMap::new()));
        let source_id = "test-source";

        let b1 = SchemaBinding {
            source_id: source_id.to_string(),
            bound_at: chrono::Utc::now(),
            tables: vec![],
            degraded: true,
            override_version: 0,
        };
        let b2 = SchemaBinding {
            source_id: source_id.to_string(),
            bound_at: chrono::Utc::now(),
            tables: vec![],
            degraded: true,
            override_version: 1,
        };

        cache.write().unwrap().insert(source_id.to_string(), Arc::new(b1));
        assert_eq!(cache.read().unwrap().get(source_id).unwrap().override_version, 0);

        cache.write().unwrap().insert(source_id.to_string(), Arc::new(b2));
        assert_eq!(
            cache.read().unwrap().get(source_id).unwrap().override_version,
            1,
            "cache must hold the latest binding after replacement"
        );
    }
}

/// Render a card to a BSON Document. The card_vector (Vec<f32>) serializes as
/// Array([Double, ...]) which round-trips correctly through BSON.
fn card_to_bson(card: &TableCard) -> AppResult<Document> {
    let vector_bson: Vec<Bson> = card
        .card_vector
        .as_deref()
        .unwrap_or_default()
        .iter()
        .map(|&f| Bson::Double(f as f64))
        .collect();

    let cols_bson: Vec<Bson> = card
        .columns
        .iter()
        .map(|c| {
            Bson::Document(doc! {
                "name": &c.name,
                "type": &c.type_,
                "nullable": c.nullable,
                "is_primary_key": c.is_primary_key,
                "is_foreign_key": c.is_foreign_key,
                "sample_values": c.sample_values.iter().map(|s| Bson::String(s.clone())).collect::<Vec<_>>(),
                "profile": {
                    "approximate_distinct_count": c.profile.approximate_distinct_count,
                    "null_ratio": c.profile.null_ratio,
                    "min": c.profile.min.clone(),
                    "max": c.profile.max.clone(),
                    "sampled_rows": c.profile.sampled_rows,
                },
            })
        })
        .collect();

    let fk_bson: Vec<Bson> = card
        .fk_edges
        .iter()
        .map(|e| {
            Bson::Document(doc! {
                "column": &e.column,
                "ref_table": &e.ref_table,
                "ref_column": &e.ref_column,
            })
        })
        .collect();

    Ok(doc! {
        "source_id": &card.source_id,
        "table_name": &card.table_name,
        "row_count": card.row_count,
        "columns": cols_bson,
        "fk_edges": fk_bson,
        "cardVector": vector_bson,
        "card_text": &card.card_text,
    })
}

/// Build the human-readable card text embedded into the vector store.
fn build_card_text(
    table: &str,
    row_count: i64,
    columns: &[CardColumn],
    fk_edges: &[CardFkEdge],
) -> String {
    let mut lines = vec![format!("Table: {table} ({row_count} rows)")];

    let col_parts: Vec<String> = columns
        .iter()
        .map(|c| {
            let mut parts = vec![format!("{} {}", c.name, c.type_)];
            if c.is_primary_key {
                parts.push("PK".into());
            }
            if c.is_foreign_key {
                parts.push("FK".into());
            }
            if !c.nullable {
                parts.push("NOT NULL".into());
            }
            if !c.sample_values.is_empty() {
                parts.push(format!(
                    "e.g. {}",
                    c.sample_values[..c.sample_values.len().min(3)].join(", ")
                ));
            }
            parts.join(" ")
        })
        .collect();
    lines.push(format!("Columns: {}", col_parts.join(", ")));

    if !fk_edges.is_empty() {
        let fk_parts: Vec<String> = fk_edges
            .iter()
            .map(|e| format!("{} -> {}.{}", e.column, e.ref_table, e.ref_column))
            .collect();
        lines.push(format!("FK: {}", fk_parts.join(", ")));
    }

    lines.join("\n")
}

/// Pull up to `n` distinct sample values per column from the fetched rows.
fn extract_samples(
    rows: &[crate::connectors::FetchedRow],
    n: usize,
) -> std::collections::HashMap<String, Vec<String>> {
    let mut out: std::collections::HashMap<String, Vec<String>> = std::collections::HashMap::new();
    for row in rows {
        for (col, val) in &row.fields {
            let entry = out.entry(col.clone()).or_default();
            if entry.len() >= n {
                continue;
            }
            if let Some(s) = crate::connectors::value_to_plain(val) {
                if !entry.contains(&s) {
                    entry.push(s);
                }
            }
        }
    }
    out
}

/// Load all `TableCard`s for the active catalog version of a source.
///
/// Returns an empty `Vec` if no catalog has been built yet.
pub async fn get_catalog_cards(
    db: &crate::documentdb::DocumentDb,
    config: &crate::config::Config,
    source_id: &str,
) -> crate::error::AppResult<Vec<super::spec::TableCard>> {
    use futures::TryStreamExt;
    // Resolve the active catalog version for this source
    let state_doc = db
        .schema_catalog_state()
        .find_one(doc! { "_id": source_id })
        .await
        .map_err(|e| crate::error::AppError::Internal(format!("catalog state lookup: {e}")))?;
    let Some(state) = state_doc else {
        return Ok(vec![]);
    };
    let version = match state.get_str("active_version") {
        Ok(v) => v.to_string(),
        Err(_) => return Ok(vec![]),
    };

    let mut cursor = db
        .schema_catalog()
        .find(doc! { "source_id": source_id, "catalog_version": &version })
        .await
        .map_err(|e| crate::error::AppError::Internal(format!("catalog find: {e}")))?;

    let mut cards = Vec::new();
    while let Some(raw) = cursor
        .try_next()
        .await
        .map_err(|e| crate::error::AppError::Internal(format!("catalog cursor: {e}")))?
    {
        if let Ok(card) = super::linker::doc_to_card(raw) {
            cards.push(card);
        }
    }
    Ok(cards)
}

