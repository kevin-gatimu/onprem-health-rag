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

use std::collections::{HashMap, HashSet};
use std::future::Future;
use std::pin::Pin;
use std::sync::LazyLock;

use regex::Regex;
use serde::{Deserialize, Serialize};

use crate::config::{Config, RetrievalMode};
use crate::documentdb::{DocumentDb, vector::Hit};
use crate::embed;
use crate::error::AppResult;
use crate::telemetry::{RequestTrace, Stage};

/// A retrieved chunk, ready for citation and prompt-building. `fields` is the source
/// row flattened to JSON (converted from BSON here so routes/serialization never touch
/// BSON). `score` is the pipeline score at the point the passage was selected — the
/// rerank score when reranking ran, otherwise the RRF fused score.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Passage {
    pub id: String,
    pub source_id: String,
    pub table: String,
    pub row_pk: String,
    pub chunk_index: i32,
    pub text: String,
    pub fields: serde_json::Value,
    pub score: f64,
    /// True if a cross-encoder assigned `score`; false if it's the RRF fused score.
    pub reranked: bool,
    pub vector_rank: Option<usize>,
    pub text_rank: Option<usize>,
    pub fused_score: f64,
    pub rerank_score: Option<f64>,
}

impl Passage {
    fn from_hit(
        hit: Hit,
        score: f64,
        reranked: bool,
        fused_score: f64,
        vector_rank: Option<usize>,
        text_rank: Option<usize>,
    ) -> Self {
        // Relaxed extended JSON keeps numbers/strings readable for the LLM and UI
        // (canonical would wrap them as {"$numberInt": ...}).
        let fields = hit.fields.into_relaxed_extjson();
        Passage {
            id: hit.id,
            source_id: hit.source_id,
            table: hit.table,
            row_pk: hit.row_pk,
            chunk_index: hit.chunk_index,
            text: hit.text,
            fields,
            score,
            reranked,
            vector_rank,
            text_rank,
            fused_score,
            rerank_score: reranked.then_some(score),
        }
    }
}

// ICD-10 codes (e.g. E11.9) and all-caps drug tokens (e.g. METFORMIN) should be
// quoted in $text queries so the server matches the exact token rather than stemming it.
static RE_ICD: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\b[A-Z]\d{2}(?:\.\d+)?\b").unwrap());
static RE_DRUG: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\b[A-Z]{4,}\b").unwrap());

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

/// Run retrieval while recording PHI-safe per-stage timings in the request summary.
pub async fn retrieve_observed(
    db: &DocumentDb,
    config: &Config,
    queries: &[String],
    mode: RetrievalMode,
    rerank_enabled: bool,
    top_k: usize,
    trace: &RequestTrace,
) -> AppResult<Vec<Passage>> {
    retrieve_inner(
        db,
        config,
        queries,
        mode,
        rerank_enabled,
        top_k,
        Some(trace),
    )
    .await
}

async fn retrieve_inner(
    db: &DocumentDb,
    config: &Config,
    queries: &[String],
    mode: RetrievalMode,
    rerank_enabled: bool,
    top_k: usize,
    trace: Option<&RequestTrace>,
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
    let query_vecs = match trace {
        Some(trace) => {
            trace
                .time(Stage::Embed, embed::embed_queries(config, queries.clone()))
                .await?
        }
        None => embed::embed_queries(config, queries.clone()).await?,
    };

    // Build every search future (vector + optional $text per query) and run
    // them all concurrently. Each future captures an owned DocumentDb clone
    // (cheap: it's a reference-counted MongoDB client handle).
    let per_side = config.retrieve_per_side;
    let mut futs: Vec<Pin<Box<dyn Future<Output = AppResult<Vec<Hit>>> + Send>>> = Vec::new();
    let mut search_sides = Vec::new();

    for (q, qv) in queries.iter().zip(query_vecs) {
        let db_v = db.clone();
        let trace_v = trace.cloned();
        search_sides.push(true);
        futs.push(Box::pin(async move {
            let search = crate::documentdb::vector::vector_search(&db_v, qv, per_side);
            match trace_v {
                Some(trace) => trace.time(Stage::SearchVector, search).await,
                None => search.await,
            }
        }));
        if mode == RetrievalMode::Hybrid {
            search_sides.push(false);
            let db_t = db.clone();
            let enhanced = enhance_text_query(q);
            let trace_t = trace.cloned();
            futs.push(Box::pin(async move {
                let search = crate::documentdb::vector::text_search(&db_t, &enhanced, per_side);
                match trace_t {
                    Some(trace) => trace.time(Stage::SearchText, search).await,
                    None => search.await,
                }
            }));
        }
    }

    let mut by_id: HashMap<String, Hit> = HashMap::new();
    let mut rankings: Vec<Vec<String>> = Vec::new();
    let mut vector_ranks = HashMap::new();
    let mut text_ranks = HashMap::new();

    for (is_vector, result) in search_sides
        .into_iter()
        .zip(futures::future::join_all(futs).await)
    {
        let ranking = collect(&mut by_id, result?);
        let rank_map = if is_vector {
            &mut vector_ranks
        } else {
            &mut text_ranks
        };
        for (index, id) in ranking.iter().enumerate() {
            rank_map
                .entry(id.clone())
                .and_modify(|rank: &mut usize| *rank = (*rank).min(index + 1))
                .or_insert(index + 1);
        }
        rankings.push(ranking);
    }

    let fused = match trace {
        Some(trace) => trace.time_sync(Stage::Rrf, || rrf::fuse(&rankings, config.rrf_k)),
        None => rrf::fuse(&rankings, config.rrf_k),
    };
    if fused.is_empty() {
        if let Some(trace) = trace {
            trace.set_retrieval(0, 0, None);
        }
        return Ok(Vec::new());
    }

    // Keep the strongest chunk for each source row before truncation. Applying this
    // after `top_k` could underfill the result even when lower-ranked unique rows exist.
    let mut unique_rows = HashSet::new();
    let fused: Vec<(String, f64)> = fused
        .into_iter()
        .filter(|(id, _)| {
            by_id.get(id).is_some_and(|hit| {
                unique_rows.insert((hit.source_id.clone(), hit.table.clone(), hit.row_pk.clone()))
            })
        })
        .collect();

    // Rerank the fused top-N (bounded work for the cross-encoder); without reranking
    // we only need the top-k the caller asked for.
    let cutoff = if rerank_enabled {
        adaptive_rerank_cutoff(&fused, top_k, config.rerank_top_n)
    } else {
        top_k
    };
    let candidates: Vec<(String, f64)> = fused.into_iter().take(cutoff).collect();

    if !rerank_enabled {
        let candidates_in = candidates.len();
        let mut seen_rows = HashSet::new();
        let passages: Vec<Passage> = candidates
            .into_iter()
            .filter_map(|(id, fused_score)| {
                let hit = by_id.remove(&id)?;
                seen_rows
                    .insert((hit.source_id.clone(), hit.table.clone(), hit.row_pk.clone()))
                    .then(|| {
                        let vector_rank = vector_ranks.get(&id).copied();
                        let text_rank = text_ranks.get(&id).copied();
                        Passage::from_hit(
                            hit,
                            fused_score,
                            false,
                            fused_score,
                            vector_rank,
                            text_rank,
                        )
                    })
            })
            .take(top_k)
            .collect();
        if let Some(trace) = trace {
            trace.set_retrieval(candidates_in, passages.len(), None);
        }
        return Ok(passages);
    }

    // Cross-encoder rerank against the primary query. Keep ids aligned with the docs
    // we hand the reranker so we can map its (index, score) back to hits.
    let fused_scores: HashMap<String, f64> = candidates.iter().cloned().collect();
    let ids: Vec<String> = candidates.iter().map(|(id, _)| id.clone()).collect();
    let docs: Vec<String> = ids
        .iter()
        .map(|id| by_id.get(id).map(|h| h.text.clone()).unwrap_or_default())
        .collect();

    let candidates_in = candidates.len();
    let rerank_future = rerank::rerank(config, queries[0].clone(), docs);
    let ranked = match trace {
        Some(trace) => trace.time(Stage::Rerank, rerank_future).await?,
        None => rerank_future.await?,
    };
    let mut passages = Vec::with_capacity(top_k);
    let mut seen_rows = HashSet::new();
    for (idx, score) in ranked {
        if let Some(id) = ids.get(idx) {
            if let Some(hit) = by_id.remove(id) {
                let row = (hit.source_id.clone(), hit.table.clone(), hit.row_pk.clone());
                if seen_rows.insert(row) {
                    passages.push(Passage::from_hit(
                        hit,
                        score as f64,
                        true,
                        fused_scores.get(id).copied().unwrap_or_default(),
                        vector_ranks.get(id).copied(),
                        text_ranks.get(id).copied(),
                    ));
                    if passages.len() == top_k {
                        break;
                    }
                }
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
                Some(p) if p.score < half => {
                    passages.pop();
                }
                _ => break,
            }
        }
    }

    if let Some(trace) = trace {
        trace.set_retrieval(
            candidates_in,
            passages.len(),
            passages.first().map(|p| p.score),
        );
    }

    Ok(passages)
}

/// Record each hit by id and return this list's id ordering (for RRF).
fn adaptive_rerank_cutoff(fused: &[(String, f64)], top_k: usize, configured_max: usize) -> usize {
    let maximum = configured_max.max(top_k).min(fused.len());
    let minimum = top_k.max(8).min(maximum);
    if maximum <= minimum {
        return maximum;
    }
    for index in minimum..maximum {
        let previous = fused[index - 1].1;
        let next = fused[index].1;
        if previous > 0.0 && next / previous < 0.65 {
            return index;
        }
    }
    maximum
}

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
