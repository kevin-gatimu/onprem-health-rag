//! Cross-encoder reranking via **fastembed** (`bge-reranker-v2-m3`, ONNX Runtime) —
//! the biggest precision lever in the pipeline. Unlike the bi-encoder embedder, a
//! cross-encoder scores the query and each passage *jointly*, so it reorders the
//! fused candidates far more accurately than cosine/RRF alone.
//!
//! Mirrors `embed/mod.rs`: one process-global model behind a `Mutex`, all work in
//! `spawn_blocking` (fastembed is sync and `&mut self`). First use downloads the
//! model from HuggingFace (pre-stage for air-gapped hosts).

use std::sync::{Mutex, OnceLock};

use fastembed::{RerankInitOptions, RerankerModel, TextRerank};

use crate::config::Config;
use crate::error::{AppError, AppResult};

static RERANKER: OnceLock<Mutex<TextRerank>> = OnceLock::new();
/// Serialises initialisation so two concurrent first-callers don't both download.
static INIT_LOCK: Mutex<()> = Mutex::new(());

/// Whether the reranker is resident in this server process.
pub fn is_loaded() -> bool {
    RERANKER.get().is_some()
}

/// Map the configured reranker name to a fastembed variant. Defaults to BGE-reranker-v2-m3.
fn resolve_model(name: &str) -> RerankerModel {
    match name.trim().to_ascii_lowercase().as_str() {
        "bge-reranker-v2-m3" | "bgererankerv2m3" | "baai/bge-reranker-v2-m3" => {
            RerankerModel::BGERerankerV2M3
        }
        "bge-reranker-base" | "bgererankerbase" => RerankerModel::BGERerankerBase,
        other => {
            tracing::warn!(
                model = other,
                "unknown reranker model; falling back to bge-reranker-v2-m3"
            );
            RerankerModel::BGERerankerV2M3
        }
    }
}

/// Map a raw cross-encoder logit to a probability in (0, 1).
/// bge-reranker-v2-m3 outputs unbounded logits; sigmoid makes them comparable
/// across queries and lets the score gate (`ONPREM_SCORE_GATE`) use a stable
/// 0–1 scale rather than a logit-specific threshold.
#[inline]
fn sigmoid(x: f32) -> f32 {
    1.0 / (1.0 + (-x).exp())
}

/// Get the process-global reranker, loading it on first call (download on first run).
fn get_or_init(model: RerankerModel) -> AppResult<&'static Mutex<TextRerank>> {
    if let Some(r) = RERANKER.get() {
        return Ok(r);
    }
    let _guard = INIT_LOCK
        .lock()
        .map_err(|_| AppError::Internal("reranker init lock poisoned".into()))?;
    if let Some(r) = RERANKER.get() {
        return Ok(r);
    }
    tracing::info!(?model, "loading reranker model (first use)");
    let tr = TextRerank::try_new(RerankInitOptions::new(model).with_show_download_progress(true))
        .map_err(|e| AppError::Internal(format!("failed to load reranker model: {e}")))?;
    let _ = RERANKER.set(Mutex::new(tr));
    RERANKER
        .get()
        .ok_or_else(|| AppError::Internal("reranker disappeared after init".into()))
}

/// Download the configured weights when needed and load the reranker.
pub async fn initialize(config: &Config) -> AppResult<()> {
    let model = resolve_model(&config.rerank_model);
    tokio::task::spawn_blocking(move || get_or_init(model).map(|_| ()))
        .await
        .map_err(|e| AppError::Internal(format!("reranker setup task panicked: {e}")))?
}

/// Rerank `documents` against `query`. Returns `(original_index, score)` pairs sorted
/// by score descending. Higher score = more relevant; scores are model-specific
/// (not normalised), so use them for ordering, not as absolute confidences.
pub async fn rerank(
    config: &Config,
    query: String,
    documents: Vec<String>,
) -> AppResult<Vec<(usize, f32)>> {
    if documents.is_empty() {
        return Ok(Vec::new());
    }
    let model = resolve_model(&config.rerank_model);

    let mut scored = tokio::task::spawn_blocking(move || -> AppResult<Vec<(usize, f32)>> {
        let lock = get_or_init(model)?;
        let mut tr = lock
            .lock()
            .map_err(|_| AppError::Internal("reranker lock poisoned".into()))?;
        let docs: Vec<&str> = documents.iter().map(String::as_str).collect();
        // return_documents=false: we only need indices + scores (we keep the Hits).
        let results = tr
            .rerank(query.as_str(), docs, false, None)
            .map_err(|e| AppError::Internal(format!("rerank failed: {e}")))?;
        Ok(results
            .into_iter()
            .map(|r| (r.index, sigmoid(r.score)))
            .collect())
    })
    .await
    .map_err(|e| AppError::Internal(format!("rerank task panicked: {e}")))??;

    // fastembed already sorts, but don't rely on it — order explicitly.
    scored.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
    Ok(scored)
}
