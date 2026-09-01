//! Index management for the `records` collection: the `cosmosSearch` vector index
//! used for kNN, plus the legacy `$text` full-text index used for the hybrid search's
//! lexical side. Both are created via `createIndexes` run-commands.
//!
//! ⚠️ VERIFY-EARLY (see plans/00-master-plan.md): the `cosmosSearch` create-index
//! syntax below follows the Azure DocumentDB (Cosmos DB for MongoDB vCore) docs — the
//! same engine — but has **not** yet been confirmed against the local
//! `documentdb-local` container. This is the project's highest-risk unverified call;
//! run it against the live container and adjust `kind`/options if it's rejected.

use futures::TryStreamExt;
use mongodb::bson::{Bson, Document, doc};

use super::{DocumentDb, RECORDS};
use crate::error::AppResult;

/// Index names, kept stable so re-running ingestion is idempotent.
const VECTOR_INDEX: &str = "records_contentVector_cosmos";
const TEXT_INDEX: &str = "records_text";

pub async fn required_indexes_ready(db: &DocumentDb) -> AppResult<bool> {
    let names = db.records().list_index_names().await?;
    Ok(
        names.iter().any(|name| name == VECTOR_INDEX)
            && names.iter().any(|name| name == TEXT_INDEX),
    )
}

/// IVF list count for the vector index. IVF (rather than HNSW) is the broadly
/// available default on this engine; `numLists` ~ sqrt(#rows) is a reasonable start
/// for small/medium corpora.
const IVF_NUM_LISTS: i32 = 100;

/// Ensure both the vector and full-text indexes exist. Idempotent: creating an
/// already-present, identical index is a no-op on the server.
pub async fn ensure_indexes(db: &DocumentDb, dims: usize) -> AppResult<()> {
    ensure_vector_index(db, dims).await?;
    ensure_text_index(db).await?;
    Ok(())
}

/// Create the `cosmosSearch` kNN index on `contentVector`, sized to the embedding
/// dimensionality with cosine similarity (BGE-M3 vectors are normalised).
async fn ensure_vector_index(db: &DocumentDb, dims: usize) -> AppResult<()> {
    let command = doc! {
        "createIndexes": RECORDS,
        "indexes": [ {
            "name": VECTOR_INDEX,
            "key": { "contentVector": "cosmosSearch" },
            "cosmosSearchOptions": {
                "kind": "vector-ivf",
                "numLists": IVF_NUM_LISTS,
                "similarity": "COS",
                "dimensions": dims as i32,
            }
        } ]
    };
    db.db.run_command(command).await?;
    tracing::info!(
        index = VECTOR_INDEX,
        dims,
        "ensured cosmosSearch vector index"
    );
    Ok(())
}

/// Create the legacy `$text` full-text index on the `text` projection.
async fn ensure_text_index(db: &DocumentDb) -> AppResult<()> {
    let command = doc! {
        "createIndexes": RECORDS,
        "indexes": [ {
            "name": TEXT_INDEX,
            "key": { "text": "text" }
        } ]
    };
    db.db.run_command(command).await?;
    tracing::info!(index = TEXT_INDEX, "ensured $text full-text index");
    Ok(())
}

// --- Search (WS6): the two retrieval sides that RRF fuses -------------------

/// One retrieved `records` chunk plus the score that surfaced it. `fields` stays as
/// BSON (arbitrary source columns); the retrieval layer converts it to JSON only when
/// building a citation. `score` is the engine score — cosine for vector, TSVector
/// rank for `$text` — and is *not* comparable across the two (that's why we fuse by
/// rank, not score).
#[derive(Debug, Clone)]
pub struct Hit {
    pub id: String,
    pub source_id: String,
    pub table: String,
    pub row_pk: String,
    pub chunk_index: i32,
    pub text: String,
    pub fields: Bson,
}

/// Fields both search sides project, plus the meta score under `score`.
fn projection(score_meta: &str) -> Document {
    doc! {
        "text": 1, "fields": 1, "source_id": 1, "table": 1, "row_pk": 1, "chunk_index": 1,
        "score": { "$meta": score_meta },
    }
}

/// Pull the fields we need out of a projected result document.
fn hit_from_doc(d: &Document) -> Option<Hit> {
    Some(Hit {
        id: d.get_str("_id").ok()?.to_string(),
        source_id: d.get_str("source_id").unwrap_or_default().to_string(),
        table: d.get_str("table").unwrap_or_default().to_string(),
        row_pk: d.get_str("row_pk").unwrap_or_default().to_string(),
        chunk_index: d.get_i32("chunk_index").unwrap_or(0),
        text: d.get_str("text").unwrap_or_default().to_string(),
        fields: d.get("fields").cloned().unwrap_or(Bson::Null),
    })
}

/// kNN over the `cosmosSearch` vector index. `cosmosSearch` must be pipeline stage 1;
/// the query vector is embedded by the caller (BGE-M3, same encoder as documents).
pub async fn vector_search(db: &DocumentDb, query_vector: Vec<f32>, k: i64) -> AppResult<Vec<Hit>> {
    if query_vector.is_empty() {
        return Ok(Vec::new());
    }
    let vector: Vec<Bson> = query_vector
        .into_iter()
        .map(|v| Bson::Double(v as f64))
        .collect();
    let pipeline = vec![
        doc! { "$search": { "cosmosSearch": { "vector": vector, "path": "contentVector", "k": k.saturating_mul(2) } } },
        doc! { "$match": { "active": true } },
        doc! { "$limit": k },
        doc! { "$project": projection("searchScore") },
    ];
    let mut cursor = db.records().aggregate(pipeline).await?;
    let mut hits = Vec::new();
    while let Some(d) = cursor.try_next().await? {
        if let Some(h) = hit_from_doc(&d) {
            hits.push(h);
        }
    }
    Ok(hits)
}

/// Lexical search over the legacy `$text` index, ranked by `textScore`. Exact-term
/// recall (drug names, ICD codes, lab values) that embeddings miss.
pub async fn text_search(db: &DocumentDb, query: &str, limit: i64) -> AppResult<Vec<Hit>> {
    if query.trim().is_empty() {
        return Ok(Vec::new());
    }
    let mut cursor = db
        .records()
        .find(doc! { "$text": { "$search": query }, "active": true })
        .projection(projection("textScore"))
        .sort(doc! { "score": { "$meta": "textScore" } })
        .limit(limit)
        .await?;
    let mut hits = Vec::new();
    while let Some(d) = cursor.try_next().await? {
        if let Some(h) = hit_from_doc(&d) {
            hits.push(h);
        }
    }
    Ok(hits)
}
