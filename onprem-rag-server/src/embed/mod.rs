//! Local embeddings via **fastembed** (ONNX Runtime) — never Foundry Local, which
//! has no embedding models. The default model is BGE-M3 (1024-dim, multilingual,
//! 8192-token context). BGE-M3 is **prefix-free / symmetric**: queries and
//! documents embed through the same path, so `embed_query` and `embed_documents`
//! differ only in intent (the seam is kept for a future asymmetric model).
//!
//! fastembed is synchronous and CPU-bound, and `TextEmbedding::embed` takes
//! `&mut self`. We therefore hold one process-global model behind a `Mutex` and do
//! all embedding inside `spawn_blocking` so the Tokio reactor is never blocked.

use std::num::NonZeroUsize;
use std::sync::{Mutex, OnceLock};

use fastembed::{EmbeddingModel, InitOptions, TextEmbedding};
use lru::LruCache;

use crate::config::Config;
use crate::error::{AppError, AppResult};

/// The loaded model, initialised on first use (it downloads on the first run, so we
/// pay that cost only if ingestion/retrieval actually happens).
static EMBEDDER: OnceLock<Mutex<TextEmbedding>> = OnceLock::new();
/// Serialises initialisation so two concurrent first-callers don't both download.
static INIT_LOCK: Mutex<()> = Mutex::new(());

/// Whether the embedding model is resident in this server process.
pub fn is_loaded() -> bool {
    EMBEDDER.get().is_some()
}

/// Per-process LRU cache mapping normalised query text to its vector. 1 KiB of
/// entries covers typical warm-session reuse; the mutex is held only briefly
/// (clone the hit, release). Serialised writes are negligible vs. spawn_blocking.
static QUERY_CACHE: OnceLock<Mutex<LruCache<String, Vec<f32>>>> = OnceLock::new();

fn query_cache() -> &'static Mutex<LruCache<String, Vec<f32>>> {
    QUERY_CACHE.get_or_init(|| Mutex::new(LruCache::new(NonZeroUsize::new(1024).unwrap())))
}

/// Map the configured model name to a fastembed variant. Defaults to BGE-M3.
fn resolve_model(name: &str) -> EmbeddingModel {
    match name.trim().to_ascii_lowercase().as_str() {
        "bge-m3" | "bgem3" | "baai/bge-m3" => EmbeddingModel::BGEM3,
        // Unknown name: fall back to BGE-M3 (the project default) with a warning.
        other => {
            tracing::warn!(
                model = other,
                "unknown embedding model; falling back to bge-m3"
            );
            EmbeddingModel::BGEM3
        }
    }
}

/// Get the process-global embedder, loading it on first call. The load can download
/// the model (first run only) and is thus done under a coarse init lock.
fn get_or_init(model: EmbeddingModel) -> AppResult<&'static Mutex<TextEmbedding>> {
    if let Some(e) = EMBEDDER.get() {
        return Ok(e);
    }
    let _guard = INIT_LOCK
        .lock()
        .map_err(|_| AppError::Internal("embedder init lock poisoned".into()))?;
    // Re-check under the lock: another caller may have initialised while we waited.
    if let Some(e) = EMBEDDER.get() {
        return Ok(e);
    }
    tracing::info!(?model, "loading embedding model (first use)");
    let te = TextEmbedding::try_new(InitOptions::new(model).with_show_download_progress(true))
        .map_err(|e| AppError::Internal(format!("failed to load embedding model: {e}")))?;
    // `set` only fails if another thread won the race; either way EMBEDDER is now set.
    let _ = EMBEDDER.set(Mutex::new(te));
    EMBEDDER
        .get()
        .ok_or_else(|| AppError::Internal("embedder disappeared after init".into()))
}

/// Download the configured weights when needed and load the embedding model.
pub async fn initialize(config: &Config) -> AppResult<()> {
    let model = resolve_model(&config.embedding_model);
    tokio::task::spawn_blocking(move || get_or_init(model).map(|_| ()))
        .await
        .map_err(|e| AppError::Internal(format!("embedding setup task panicked: {e}")))?
}

/// Embed a batch of texts, returning one vector per input (order preserved). Runs on
/// a blocking thread; `expected_dims` is used only to sanity-check the model output.
async fn embed_batch(config: &Config, texts: Vec<String>) -> AppResult<Vec<Vec<f32>>> {
    if texts.is_empty() {
        return Ok(Vec::new());
    }
    let model = resolve_model(&config.embedding_model);
    let expected = config.embedding_dims;

    let vectors = tokio::task::spawn_blocking(move || -> AppResult<Vec<Vec<f32>>> {
        let lock = get_or_init(model)?;
        let mut te = lock
            .lock()
            .map_err(|_| AppError::Internal("embedder lock poisoned".into()))?;
        // `None` lets fastembed pick its default internal batch size.
        te.embed(texts, None)
            .map_err(|e| AppError::Internal(format!("embedding failed: {e}")))
    })
    .await
    .map_err(|e| AppError::Internal(format!("embedding task panicked: {e}")))??;

    if let Some(first) = vectors.first() {
        if first.len() != expected {
            // A mismatch means the vector index (sized to `embedding_dims`) won't
            // accept these vectors — surface it loudly rather than corrupt the store.
            return Err(AppError::Internal(format!(
                "embedding dimension mismatch: model produced {}, config expects {expected}",
                first.len()
            )));
        }
    }
    Ok(vectors)
}

/// Embed documents/passages for storage. (BGE-M3 uses no document prefix.)
pub async fn embed_documents(config: &Config, texts: Vec<String>) -> AppResult<Vec<Vec<f32>>> {
    embed_batch(config, texts).await
}

/// Embed multiple queries for retrieval, checking and filling a per-process LRU
/// cache. All cache misses are embedded in one `spawn_blocking` call so multi-query
/// expansion (typically 3 variants) costs at most one model call.
pub async fn embed_queries(config: &Config, queries: Vec<String>) -> AppResult<Vec<Vec<f32>>> {
    if queries.is_empty() {
        return Ok(Vec::new());
    }
    let keys: Vec<String> = queries
        .iter()
        .map(|q| q.trim().to_ascii_lowercase())
        .collect();
    let mut results: Vec<Option<Vec<f32>>> = vec![None; keys.len()];
    let mut miss_indices: Vec<usize> = Vec::new();

    {
        let mut cache = query_cache()
            .lock()
            .map_err(|_| AppError::Internal("embed cache lock poisoned".into()))?;
        for (i, key) in keys.iter().enumerate() {
            if let Some(v) = cache.get(key) {
                results[i] = Some(v.clone());
            } else {
                miss_indices.push(i);
            }
        }
    }

    if !miss_indices.is_empty() {
        let miss_texts: Vec<String> = miss_indices.iter().map(|&i| keys[i].clone()).collect();
        let vecs = embed_batch(config, miss_texts).await?;
        let mut cache = query_cache()
            .lock()
            .map_err(|_| AppError::Internal("embed cache lock poisoned".into()))?;
        for (&mi, vec) in miss_indices.iter().zip(vecs) {
            cache.put(keys[mi].clone(), vec.clone());
            results[mi] = Some(vec);
        }
    }

    Ok(results.into_iter().map(|v| v.unwrap_or_default()).collect())
}

/// Embed a single query for retrieval (cached via `embed_queries`).
pub async fn embed_query(config: &Config, query: &str) -> AppResult<Vec<f32>> {
    let mut out = embed_queries(config, vec![query.to_string()]).await?;
    out.pop()
        .ok_or_else(|| AppError::Internal("embedding produced no vector".into()))
}
