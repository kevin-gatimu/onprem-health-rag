//! Faithfulness verifier — `ModelRole::Verify` (plan 25).
//!
//! A second, cheap pass over an answer the system has already produced: break the
//! answer into its clinical claims and check each one against the passages that were
//! retrieved to support it. This is the safety net for the failure mode that matters
//! most in a health-records assistant — a fluent answer that states a medication, a
//! dose, or an allergy the records never mentioned.
//!
//! Two properties shape the design:
//!
//! * **It runs after the answer has finished streaming.** The user already has their
//!   words; verification only delays the verdict badge, never the first token. That
//!   is why this is a post-stream step in `rag::routes` rather than a gate in front
//!   of generation.
//! * **It fails open, loudly.** A verifier that errors, times out, or returns garbage
//!   reports [`VerifyStatus::Skipped`] and the answer stands. Silently suppressing an
//!   answer because a small model had a bad day would be worse than not checking.
//!
//! Placement mirrors the extractor: `phi-4-mini-reasoning` on `[Npu, Cpu]`, so the
//! check never competes with the chat model on the iGPU.

use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::config::Config;
use crate::foundry::FoundryManager;
use crate::foundry::router::ModelSpec;
use crate::retrieval::Passage;

/// One claim the verifier lifted out of the answer, with its verdict.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ClaimCheck {
    /// The claim as the verifier restated it.
    pub claim: String,
    /// Whether the retrieved passages support it.
    pub supported: bool,
    /// 1-based passage numbers the verifier relied on. Empty for an unsupported claim.
    #[serde(default)]
    pub passages: Vec<usize>,
}

/// Raw verifier tool output, before validation. Kept separate from [`VerifyReport`] so
/// the model's shape and our contract can evolve independently.
#[derive(Debug, Clone, Deserialize)]
pub struct VerifyToolOutput {
    #[serde(default)]
    pub claims: Vec<ClaimCheck>,
}

/// Overall verdict for an answer.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum VerifyStatus {
    /// Every extracted claim is supported by the passages.
    Supported,
    /// At least one claim is supported and at least one is not.
    Partial,
    /// No claim is supported.
    Unsupported,
    /// The check did not run or could not be trusted (disabled, no passages,
    /// model unavailable, timeout, unparseable output).
    Skipped,
}

/// What `/chat` streams in its `verify` event.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VerifyReport {
    pub status: VerifyStatus,
    /// Per-claim detail, in the order the verifier returned them. Empty when skipped.
    pub claims: Vec<ClaimCheck>,
    /// Count of claims whose `supported` is false — the number the UI leads with.
    pub unsupported: usize,
    /// Present only for `Skipped`, explaining why, so a silent no-op is never
    /// indistinguishable from a clean bill of health.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    /// `[N]` markers in the answer that point past the end of the citation list — the
    /// model cited a passage that does not exist.
    ///
    /// This is a deterministic string check, not a model verdict, and it is reported
    /// independently of `status`: an answer can be fully supported and still carry a
    /// bad marker. It rides on this struct so `/chat` emits exactly one `verify` event.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub citation_overflow: Vec<usize>,
}

impl VerifyReport {
    /// A skipped report carrying the reason it was skipped.
    pub fn skipped(reason: impl Into<String>) -> Self {
        VerifyReport {
            status: VerifyStatus::Skipped,
            claims: Vec::new(),
            unsupported: 0,
            reason: Some(reason.into()),
            citation_overflow: Vec::new(),
        }
    }

    /// Fold per-claim verdicts into an overall status.
    ///
    /// An empty claim list is [`VerifyStatus::Skipped`], not `Supported`: a verifier
    /// that found nothing to check has not cleared the answer, and reporting that as
    /// a pass is exactly the false assurance this module exists to avoid.
    pub fn from_claims(mut claims: Vec<ClaimCheck>, passage_count: usize) -> Self {
        // Drop claims with no text, and clamp passage references to what actually
        // existed — a small model will happily cite passage 9 out of 4.
        claims.retain(|c| !c.claim.trim().is_empty());
        for c in claims.iter_mut() {
            c.claim = c.claim.trim().to_string();
            c.passages.retain(|&p| p >= 1 && p <= passage_count);
            c.passages.sort_unstable();
            c.passages.dedup();
            // A claim asserted as supported but citing nothing real is not support.
            if c.supported && c.passages.is_empty() {
                c.supported = false;
            }
        }

        if claims.is_empty() {
            return VerifyReport::skipped("verifier returned no checkable claims");
        }

        let unsupported = claims.iter().filter(|c| !c.supported).count();
        let status = if unsupported == 0 {
            VerifyStatus::Supported
        } else if unsupported == claims.len() {
            VerifyStatus::Unsupported
        } else {
            VerifyStatus::Partial
        };
        VerifyReport {
            status,
            claims,
            unsupported,
            reason: None,
            citation_overflow: Vec::new(),
        }
    }

    /// Attach the deterministic citation-marker check to this report.
    pub fn with_citation_overflow(mut self, invalid: Vec<usize>) -> Self {
        self.citation_overflow = invalid;
        self
    }

    /// Whether this report says anything worth sending to the client. A `Skipped`
    /// verdict with no bad markers is pure noise — the UI would show a badge that
    /// means "we did not check", on every single answer, when the feature is off.
    pub fn is_reportable(&self) -> bool {
        self.status != VerifyStatus::Skipped || !self.citation_overflow.is_empty()
    }
}

/// System prompt for the check. The instruction to judge support *only* from the
/// numbered passages — not from clinical knowledge — is what makes the pass useful:
/// a medically plausible claim that the records do not contain is precisely the
/// hallucination we are hunting.
const VERIFY_SYSTEM: &str = "\
You are a clinical grounding verifier for an on-premises health-records assistant. \
You are given an ANSWER and the numbered PASSAGES that were retrieved from the patient \
records to support it. Call the verify_claims tool. \
RULES: \
(1) Break the answer into its individual factual clinical claims — diagnoses, medications, \
doses, dates, allergies, lab values, counts. \
(2) For each claim, decide whether the PASSAGES state it. Judge ONLY from the passages. \
(3) A claim that is medically plausible but not stated in the passages is NOT supported. \
(4) When a claim is supported, list the 1-based numbers of the passages that state it. \
(5) When a claim is not supported, mark it unsupported and list no passages. \
(6) Ignore hedging, pleasantries, and offers to help further — they are not claims. \
/no_think";

/// Build the user message: the answer, then the numbered passages it must be checked
/// against. Passages are truncated and capped so answer + evidence fit the NPU's
/// context window.
fn build_user(answer: &str, passages: &[Passage], passage_chars: usize) -> String {
    let mut s = String::with_capacity(1024);
    s.push_str("ANSWER:\n");
    s.push_str(answer.trim());
    s.push_str("\n\nPASSAGES:\n");
    for (i, p) in passages.iter().enumerate() {
        let clipped: String = p.text.chars().take(passage_chars).collect();
        s.push_str(&format!("[{}] {}\n", i + 1, clipped.trim()));
    }
    s
}

/// Run the faithfulness check over a finished answer.
///
/// Never returns an error: every failure path produces a [`VerifyStatus::Skipped`]
/// report with a reason. Callers stream the result as-is.
pub async fn check(
    foundry: Option<&FoundryManager>,
    spec: &ModelSpec,
    config: &Config,
    answer: &str,
    passages: &[Passage],
) -> VerifyReport {
    if !config.router.verify_enabled {
        return VerifyReport::skipped("verification disabled");
    }
    let Some(foundry) = foundry else {
        return VerifyReport::skipped("Foundry Local unavailable");
    };
    if answer.trim().is_empty() {
        return VerifyReport::skipped("empty answer");
    }
    if passages.is_empty() {
        // Nothing to check against. An answer with no sources is already handled by
        // the score gate upstream; claiming "supported" here would be meaningless.
        return VerifyReport::skipped("no passages to verify against");
    }

    let evidence = &passages[..passages.len().min(config.router.verify_max_passages)];
    let user = build_user(answer, evidence, config.router.verify_passage_chars);

    let call = foundry.plan_verification(spec, VERIFY_SYSTEM, &user);
    match tokio::time::timeout(Duration::from_secs(config.router.verify_timeout_secs), call).await {
        Ok(Ok(out)) => VerifyReport::from_claims(out.claims, evidence.len()),
        Ok(Err(e)) => {
            tracing::warn!(error = %e, "faithfulness verifier failed; answer left unverified");
            VerifyReport::skipped(format!("verifier failed: {e}"))
        }
        Err(_) => {
            tracing::warn!(
                secs = config.router.verify_timeout_secs,
                "faithfulness verifier timed out; answer left unverified"
            );
            VerifyReport::skipped("verifier timed out")
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn claim(text: &str, supported: bool, passages: Vec<usize>) -> ClaimCheck {
        ClaimCheck {
            claim: text.into(),
            supported,
            passages,
        }
    }

    #[test]
    fn all_supported_is_supported() {
        let r = VerifyReport::from_claims(
            vec![
                claim("on metformin", true, vec![1]),
                claim("HbA1c 7.2", true, vec![2]),
            ],
            3,
        );
        assert_eq!(r.status, VerifyStatus::Supported);
        assert_eq!(r.unsupported, 0);
        assert!(r.reason.is_none());
    }

    #[test]
    fn mixed_verdicts_are_partial() {
        let r = VerifyReport::from_claims(
            vec![
                claim("on metformin", true, vec![1]),
                claim("penicillin allergy", false, vec![]),
            ],
            3,
        );
        assert_eq!(r.status, VerifyStatus::Partial);
        assert_eq!(r.unsupported, 1);
    }

    #[test]
    fn none_supported_is_unsupported() {
        let r = VerifyReport::from_claims(vec![claim("invented dose", false, vec![])], 2);
        assert_eq!(r.status, VerifyStatus::Unsupported);
        assert_eq!(r.unsupported, 1);
    }

    #[test]
    fn no_claims_is_skipped_not_a_pass() {
        // The important one: "nothing to check" must never render as a clean bill.
        let r = VerifyReport::from_claims(vec![], 3);
        assert_eq!(r.status, VerifyStatus::Skipped);
        assert!(r.reason.is_some());
    }

    #[test]
    fn blank_claims_are_dropped() {
        let r = VerifyReport::from_claims(
            vec![
                claim("   ", true, vec![1]),
                claim("on metformin", true, vec![1]),
            ],
            2,
        );
        assert_eq!(r.claims.len(), 1);
        assert_eq!(r.claims[0].claim, "on metformin");
    }

    #[test]
    fn out_of_range_passage_refs_are_dropped() {
        // A supported claim whose only citation was a hallucinated passage number is
        // demoted to unsupported rather than trusted.
        let r = VerifyReport::from_claims(vec![claim("on metformin", true, vec![9])], 3);
        assert!(r.claims[0].passages.is_empty());
        assert!(!r.claims[0].supported);
        assert_eq!(r.status, VerifyStatus::Unsupported);
    }

    #[test]
    fn valid_refs_survive_and_are_normalised() {
        let r = VerifyReport::from_claims(vec![claim("on metformin", true, vec![2, 1, 2, 7])], 3);
        assert_eq!(r.claims[0].passages, vec![1, 2]);
        assert!(r.claims[0].supported);
    }

    #[test]
    fn skipped_report_serialises_with_reason() {
        let json = serde_json::to_string(&VerifyReport::skipped("verifier timed out")).unwrap();
        assert!(json.contains("\"status\":\"skipped\""));
        assert!(json.contains("verifier timed out"));
    }

    #[test]
    fn clean_report_omits_reason() {
        let r = VerifyReport::from_claims(vec![claim("on metformin", true, vec![1])], 1);
        let json = serde_json::to_string(&r).unwrap();
        assert!(
            !json.contains("reason"),
            "reason must be absent on a real verdict"
        );
    }

    #[test]
    fn user_message_numbers_passages_from_one() {
        let p = |t: &str| Passage {
            id: "i".into(),
            source_id: "s".into(),
            table: "t".into(),
            row_pk: "r".into(),
            chunk_index: 0,
            text: t.into(),
            fields: serde_json::Value::Null,
            score: 1.0,
            reranked: false,
            vector_rank: None,
            text_rank: None,
            fused_score: 1.0,
            rerank_score: None,
        };
        let msg = build_user("answer text", &[p("first"), p("second")], 100);
        assert!(msg.contains("[1] first"));
        assert!(msg.contains("[2] second"));
        assert!(msg.contains("ANSWER:\nanswer text"));
    }

    #[test]
    fn passage_text_is_truncated_to_the_budget() {
        let p = Passage {
            id: "i".into(),
            source_id: "s".into(),
            table: "t".into(),
            row_pk: "r".into(),
            chunk_index: 0,
            text: "abcdefghij".into(),
            fields: serde_json::Value::Null,
            score: 1.0,
            reranked: false,
            vector_rank: None,
            text_rank: None,
            fused_score: 1.0,
            rerank_score: None,
        };
        let msg = build_user("a", std::slice::from_ref(&p), 4);
        assert!(msg.contains("[1] abcd"));
        assert!(!msg.contains("abcde"));
    }
}
