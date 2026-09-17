//! Persistence layer for `SchemaBinding`.
//!
//! | Collection          | Key             | Value                     |
//! |---------------------|-----------------|---------------------------|
//! | `schema_bindings`   | `source_id`     | latest binding            |
//! | `schema_binding_history` | `source_id:{ts}` | historical snapshot   |

use chrono::Utc;
use mongodb::bson::{self, doc};

use crate::documentdb::DocumentDb;
use crate::error::{AppError, AppResult};
use crate::ontology::binding::SchemaBinding;

/// Upsert the binding for a source (latest only, keyed by `source_id`).
pub async fn save_binding(db: &DocumentDb, binding: &SchemaBinding) -> AppResult<()> {
    let doc = bson::to_document(binding)
        .map_err(|e| AppError::Internal(format!("serialize binding: {e}")))?;

    db.schema_bindings()
        .update_one(
            doc! { "source_id": &binding.source_id },
            doc! { "$set": doc },
        )
        .upsert(true)
        .await
        .map_err(|e| AppError::Internal(format!("upsert binding: {e}")))?;

    Ok(())
}

/// Append a snapshot to the binding history collection.
pub async fn append_history(db: &DocumentDb, binding: &SchemaBinding) -> AppResult<()> {
    let ts = binding.bound_at.to_rfc3339();
    let id = format!("{}:{}", binding.source_id, ts);
    let mut doc = bson::to_document(binding)
        .map_err(|e| AppError::Internal(format!("serialize binding history: {e}")))?;
    doc.insert("_id", id);

    // Ignore duplicate-key errors (idempotent re-runs within the same second)
    match db.schema_binding_history().insert_one(doc).await {
        Ok(_) => {}
        Err(e) => {
            let msg = e.to_string();
            if !msg.contains("11000") {
                // Not a duplicate key error — propagate
                return Err(AppError::Internal(format!("binding history insert: {e}")));
            }
        }
    }
    Ok(())
}

/// Load the latest binding for a source, if any.
pub async fn load_binding(db: &DocumentDb, source_id: &str) -> AppResult<Option<SchemaBinding>> {
    let result = db
        .schema_bindings()
        .find_one(doc! { "source_id": source_id })
        .await
        .map_err(|e| AppError::Internal(format!("load binding: {e}")))?;

    match result {
        None => Ok(None),
        Some(doc) => {
            let binding: SchemaBinding = bson::from_document(doc)
                .map_err(|e| AppError::Internal(format!("deserialize binding: {e}")))?;
            Ok(Some(binding))
        }
    }
}

/// Load the latest bindings for all sources.
pub async fn load_all_bindings(db: &DocumentDb) -> AppResult<Vec<SchemaBinding>> {
    use futures::TryStreamExt;

    let mut cursor = db
        .schema_bindings()
        .find(doc! {})
        .await
        .map_err(|e| AppError::Internal(format!("list bindings: {e}")))?;

    let mut out = Vec::new();
    while let Some(doc) = cursor.try_next().await
        .map_err(|e| AppError::Internal(format!("binding cursor: {e}")))?
    {
        match bson::from_document::<SchemaBinding>(doc) {
            Ok(b) => out.push(b),
            Err(e) => tracing::warn!("skipping malformed binding doc: {e}"),
        }
    }
    Ok(out)
}

/// Load the binding history for a source, newest first.
pub async fn load_binding_history(
    db: &DocumentDb,
    source_id: &str,
) -> AppResult<Vec<SchemaBinding>> {
    use futures::TryStreamExt;

    let opts = mongodb::options::FindOptions::builder()
        .sort(doc! { "bound_at": -1 })
        .limit(20)
        .build();

    let mut cursor = db
        .schema_binding_history()
        .find(doc! { "source_id": source_id })
        .with_options(opts)
        .await
        .map_err(|e| AppError::Internal(format!("list binding history: {e}")))?;

    let mut out = Vec::new();
    while let Some(doc) = cursor.try_next().await
        .map_err(|e| AppError::Internal(format!("binding history cursor: {e}")))?
    {
        match bson::from_document::<SchemaBinding>(doc) {
            Ok(b) => out.push(b),
            Err(e) => tracing::warn!("skipping malformed binding history doc: {e}"),
        }
    }
    Ok(out)
}

/// Ensure indexes on the binding collections.
pub async fn ensure_binding_indexes(db: &DocumentDb) -> AppResult<()> {
    use mongodb::IndexModel;
    use mongodb::options::IndexOptions;

    let unique_opt = IndexOptions::builder().unique(true).build();

    // schema_bindings: unique index on source_id
    db.schema_bindings()
        .create_index(
            IndexModel::builder()
                .keys(doc! { "source_id": 1 })
                .options(unique_opt)
                .build(),
        )
        .await
        .map_err(|e| AppError::Internal(format!("binding index: {e}")))?;

    // schema_binding_history: index for listing by source_id + bound_at
    db.schema_binding_history()
        .create_index(
            IndexModel::builder()
                .keys(doc! { "source_id": 1, "bound_at": -1 })
                .build(),
        )
        .await
        .map_err(|e| AppError::Internal(format!("binding history index: {e}")))?;

    Ok(())
}

/// Persist a binding and append to history (non-fatal wrapper for catalog hook).
pub async fn save_binding_nonfatal(db: &DocumentDb, binding: &SchemaBinding) {
    let ts = Utc::now();
    if let Err(e) = save_binding(db, binding).await {
        tracing::warn!("failed to persist binding for {}: {e}", binding.source_id);
    }
    if let Err(e) = append_history(db, binding).await {
        tracing::warn!("failed to append binding history for {}: {e}", binding.source_id);
    }
    tracing::debug!(
        source_id = %binding.source_id,
        tables = binding.tables.len(),
        degraded = binding.degraded,
        elapsed_ms = (Utc::now() - ts).num_milliseconds(),
        "schema binding saved"
    );
}
