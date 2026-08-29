//! Reciprocal Rank Fusion — merge several ranked id lists into one.
//!
//! Vector cosine and `$text` TSVector scores aren't comparable, and multi-query
//! expansion produces several lists per side; RRF fuses them all by *rank*, which
//! sidesteps score scales entirely: `score(d) = Σ_i 1/(k + rank_i(d))` over every
//! list `d` appears in (rank 1-based). `k` (default 60) damps the weight of top
//! ranks so a document must place well across lists to rise.

use std::collections::HashMap;

/// Fuse ranked id lists. Returns `(id, fused_score)` sorted by score descending.
/// Ties break by id for a deterministic order.
pub fn fuse(rankings: &[Vec<String>], k: f64) -> Vec<(String, f64)> {
    let mut scores: HashMap<String, f64> = HashMap::new();
    for ranking in rankings {
        for (rank, id) in ranking.iter().enumerate() {
            *scores.entry(id.clone()).or_insert(0.0) += 1.0 / (k + (rank as f64) + 1.0);
        }
    }
    let mut fused: Vec<(String, f64)> = scores.into_iter().collect();
    fused.sort_by(|a, b| {
        b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal).then_with(|| a.0.cmp(&b.0))
    });
    fused
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rewards_agreement_across_lists() {
        // "b" is rank 1 in one list and rank 2 in the other; "a" is rank 1 only once.
        let a = vec!["a".to_string(), "b".to_string()];
        let b = vec!["b".to_string(), "c".to_string()];
        let fused = fuse(&[a, b], 60.0);
        // b appears in both lists, so it should win despite never being sole top.
        assert_eq!(fused[0].0, "b");
    }

    #[test]
    fn empty_input_is_empty() {
        assert!(fuse(&[], 60.0).is_empty());
    }
}
