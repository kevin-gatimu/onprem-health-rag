//! Retrieval orchestration — the RAG pipeline's "R".
//!
//! Given one or more (already-rewritten/expanded) query strings, this fuses the two
//! search sides and optionally reranks, returning grounded `Passage`s ready to drop
//! into a prompt:
//!
//! ```text
//! queries ─┬─ embed (batch) ──┬─ cosmosSearch (vector) ─┐
//!          │                  └─ $text (lexical, hybrid) ─┤  all concurrent
//!          └──────────────────────────────────────────────┘
//!                                         ↓
//!                              RRF fuse (rank, k=60)
//!                                → take top-N
//!                                → cross-encoder rerank (query[0])
//!                                    → adaptive trim (gate/2)
//!                                        → top-k Passages
//! ```
//!
//! Fusion is by **rank** (RRF), not score, because cosine and TSVector scores aren't
//! comparable. Reranking is the precision lever: a cross-encoder rescores the fused
//! top-N jointly against the primary query, and we keep its top-k.

pub mod rerank;
pub mod rrf;

use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::LazyLock;

use regex::Regex;
use serde::{Deserialize, Serialize};

use crate::config::{Config, RetrievalMode};
use crate::documentdb::{DocumentDb, vector::Hit};
use crate::embed;
use crate::error::AppResult;

/// A retrieved chunk, ready for citation and prompt-building. `fields` is the source
/// row flattened to JSON (converted from BSON here so routes/serialization never touch
/// BSON). `score` is the pipeline score at the point the passage was selected — the
/// rerank score when reranking ran, otherwise the RRF fused score.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Passage {
    pub id: String,
    pub source_id: String,
    pub row_pk: String,
    pub chunk_index: i32,
    pub text: String,
    pub fields: serde_json::Value,
    pub score: f64,
    /// True if a cross-encoder assigned `score`; false if it's the RRF fused score.
    pub reranked: bool,
}

impl Passage {
    fn from_hit(hit: Hit, score: f64, reranked: bool) -> Self {
        // Relaxed extended JSON keeps numbers/strings readable for the LLM and UI
        // (canonical would wrap them as {"$numberInt": ...}).
        let fields = hit.fields.into_relaxed_extjson();
        Passage {
            id: hit.id,
            source_id: hit.source_id,
            row_pk: hit.row_pk,
            chunk_index: hit.chunk_index,
            text: hit.text,
            fields,
            score,
            reranked,
        }
    }
}

// ICD-10 codes (e.g. E11.9) and all-caps drug tokens (e.g. METFORMIN) should be
// quoted in $text queries so the server matches the exact token rather than stemming it.
static RE_ICD: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"\b[A-Z]\d{2}(?:\.\d+)?\b").unwrap()
});
static RE_DRUG: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"\b[A-Z]{4,}\b").unwrap()
});

/// Wrap ICD codes and all-caps drug tokens with double quotes so the MongoDB $text
/// operator matches them exactly. Other query words are left unchanged.
fn enhance_text_query(query: &str) -> String {
    let mut extras: Vec<String> = Vec::new();
    for m in RE_ICD.find_iter(query) {
        extras.push(format!("\"{}\"", m.as_str()));
    }
    for m in RE_DRUG.find_iter(query) {
        let s = m.as_str();
        // Skip tokens already captured by RE_ICD to avoid double-quoting.
        if !RE_ICD.is_match(s) {
            extras.push(format!("\"{}\"", s));
        }
    }
    if extras.is_empty() {
        query.to_string()
    } else {
        format!("{} {}", extras.join(" "), query)
    }
}

/// Run retrieval for a set of queries and return the top-k passages.
///
/// `queries` is the primary query followed by any multi-query expansions; the primary
/// (`queries[0]`) is what the reranker scores against. `mode` and `rerank_enabled` are
/// taken per-request (falling back to config defaults at the call site) so the Chat UI
/// can toggle them.
pub async fn retrieve(
    db: &DocumentDb,
    config: &Config,
    queries: &[String],
    mode: RetrievalMode,
    rerank_enabled: bool,
    top_k: usize,
) -> AppResult<Vec<Passage>> {
    let queries: Vec<String> = queries
        .iter()
        .filter(|q| !q.trim().is_empty())
        .map(|q| q.to_string())
        .collect();
    if queries.is_empty() || top_k == 0 {
        return Ok(Vec::new());
    }

    // Embed all queries in one batch — one spawn_blocking call, one fastembed
    // invocation, LRU cache checked for each before going to the model.
    let query_vecs = embed::embed_queries(config, queries.clone()).await?;

    // Build every search future (vector + optional $text per query) and run
    // them all concurrently. Each future captures an owned DocumentDb clone
    // (cheap: it's a reference-counted MongoDB client handle).
    let per_side = config.retrieve_per_side;
    let mut futs: Vec<Pin<Box<dyn Future<Output = AppResult<Vec<Hit>>> + Send>>> = Vec::new();

    for (q, qv) in queries.iter().zip(query_vecs) {
        let db_v = db.clone();
        futs.push(Box::pin(async move {
            crate::documentdb::vector::vector_search(&db_v, qv, per_side).await
        }));
        if mode == RetrievalMode::Hybrid {
            let db_t = db.clone();
            let enhanced = enhance_text_query(q);
            futs.push(Box::pin(async move {
                crate::documentdb::vector::text_search(&db_t, &enhanced, per_side).await
            }));
        }
    }

    let mut by_id: HashMap<String, Hit> = HashMap::new();
    let mut rankings: Vec<Vec<String>> = Vec::new();

    for result in futures::future::join_all(futs).await {
        rankings.push(collect(&mut by_id, result?));
    }

    let fused = rrf::fuse(&rankings, config.rrf_k);
    if fused.is_empty() {
        return Ok(Vec::new());
    }

    // Rerank the fused top-N (bounded work for the cross-encoder); without reranking
    // we only need the top-k the caller asked for.
    let cutoff = if rerank_enabled { config.rerank_top_n.max(top_k) } else { top_k };
    let candidates: Vec<(String, f64)> = fused.into_iter().take(cutoff).collect();

    if !rerank_enabled {
        return Ok(candidates
            .into_iter()
            .filter_map(|(id, fused_score)| by_id.remove(&id).map(|h| Passage::from_hit(h, fused_score, false)))
            .take(top_k)
            .collect());
    }

    // Cross-encoder rerank against the primary query. Keep ids aligned with the docs
    // we hand the reranker so we can map its (index, score) back to hits.
    let ids: Vec<String> = candidates.iter().map(|(id, _)| id.clone()).collect();
    let docs: Vec<String> = ids
        .iter()
        .map(|id| by_id.get(id).map(|h| h.text.clone()).unwrap_or_default())
        .collect();

    let ranked = rerank::rerank(config, queries[0].clone(), docs).await?;
    let mut passages = Vec::with_capacity(top_k);
    for (idx, score) in ranked.into_iter().take(top_k) {
        if let Some(id) = ids.get(idx) {
            if let Some(hit) = by_id.remove(id) {
                passages.push(Passage::from_hit(hit, score as f64, true));
            }
        }
    }

    // Adaptive trim: drop passages from the tail whose score falls below gate/2.
    // With sigmoid-normalised scores and a gate of 0.30 the trim floor is 0.15 —
    // passages that weak are nearly always noise. Never truncates to zero.
    if let Some(gate) = config.score_gate {
        let half = gate / 2.0;
        while passages.len() > 1 {
            match passages.last() {
                Some(p) if p.score < half => { passages.pop(); }
                _ => break,
            }
        }
    }

    Ok(passages)
}

/// Record each hit by id and return this list's id ordering (for RRF).
fn collect(by_id: &mut HashMap<String, Hit>, hits: Vec<Hit>) -> Vec<String> {
    let mut order = Vec::with_capacity(hits.len());
    for hit in hits {
        order.push(hit.id.clone());
        // First occurrence wins; both sides carry the same immutable chunk, and we
        // don't use the raw per-side score once fused.
        by_id.entry(hit.id.clone()).or_insert(hit);
    }
    order
}
