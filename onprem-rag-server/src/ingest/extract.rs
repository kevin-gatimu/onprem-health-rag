//! Ingestion clinical extractor — `AgentKind::Extract` (plan 25).
//!
//! Reads a row's free-text projection and returns the clinical entities it names,
//! each mapped to a standard vocabulary: conditions to ICD-10, medications to RxNorm,
//! labs and observations to LOINC. The result is stored on the record as an
//! `extracted` sub-document; the embedded `text` is **never** modified, so retrieval
//! behaves exactly as it did before the extractor existed and turning the feature off
//! is a clean revert rather than a re-ingest.
//!
//! Placement is the point of the role: phi-4-mini sits on the NPU (`[Npu, Cpu]` in
//! `ModelSpec::for_kind`), so a long ingest annotating thousands of rows never
//! contends with the iGPU that is serving chat at the same time.
//!
//! Everything here degrades to `None`. A model that is missing, slow, or returns
//! unparseable JSON costs the row its annotation and nothing else — extraction must
//! never fail a table that would otherwise have ingested cleanly.

use std::sync::Arc;
use std::time::Duration;

use futures::StreamExt;
use serde::{Deserialize, Serialize};

use crate::config::Config;
use crate::foundry::FoundryManager;
use crate::foundry::router::ModelSpec;

/// One extracted clinical entity plus the standard code the model mapped it to.
///
/// `code` is whatever the model produced and is deliberately **not** validated against
/// a real terminology server — there is none on-prem. Treat it as a search aid, not as
/// billing-grade coding. `system` is stamped by us from the field the term arrived in,
/// so the model only has to produce the code itself.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CodedTerm {
    /// The span as it appears in the note, e.g. `"type 2 diabetes"`.
    pub text: String,
    /// Standard code, e.g. `"E11"`. Empty when the model declined to map the term.
    #[serde(default)]
    pub code: String,
    /// Vocabulary the code belongs to: `icd10`, `rxnorm`, or `loinc`. Filled in by
    /// `stamp_systems`, never by the model.
    #[serde(default)]
    pub system: String,
}

/// The extractor's per-row output, stored as `records.extracted`.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ExtractedClinical {
    /// Diagnoses and problems, mapped to ICD-10.
    #[serde(default)]
    pub conditions: Vec<CodedTerm>,
    /// Drugs, mapped to RxNorm.
    #[serde(default)]
    pub medications: Vec<CodedTerm>,
    /// Labs, vitals, and observations, mapped to LOINC.
    #[serde(default)]
    pub labs: Vec<CodedTerm>,
    /// Model alias that produced this annotation, so a later re-index can tell which
    /// rows were done by which model.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub model: String,
}

impl ExtractedClinical {
    /// Total entities found — used to decide whether the annotation is worth storing.
    pub fn len(&self) -> usize {
        self.conditions.len() + self.medications.len() + self.labs.len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Stamp the vocabulary onto each term and drop entries the model left blank.
    ///
    /// The model is only asked for `text` + `code`; which vocabulary applies is a
    /// property of the field it arrived in, so deriving it here removes a whole class
    /// of mistake. A model-supplied `system` is overwritten, never trusted.
    fn stamp_systems(&mut self, model: &str) {
        for (terms, system) in [
            (&mut self.conditions, "icd10"),
            (&mut self.medications, "rxnorm"),
            (&mut self.labs, "loinc"),
        ] {
            terms.retain(|t| !t.text.trim().is_empty());
            for t in terms.iter_mut() {
                t.text = t.text.trim().to_string();
                t.code = t.code.trim().to_string();
                t.system = system.to_string();
            }
        }
        self.model = model.to_string();
    }
}

/// Whether a row's text is worth a model call.
///
/// Short projections are structured column dumps rather than notes — the extractor
/// would spend an NPU round-trip to find nothing. Counting is capped at `min_words`
/// so a very long note costs the same check as a short one.
pub fn eligible(text: &str, min_words: usize) -> bool {
    text.split_whitespace().take(min_words).count() >= min_words
}

/// System prompt. Deliberately blunt and closed-world: small models invent codes
/// enthusiastically, so rule (6) — leave the code empty rather than guess — is the
/// single most important line here.
const EXTRACT_SYSTEM: &str = "\
You are a clinical information extractor working inside an on-premises health-records system. \
Read the record text and call the extract_clinical tool with the clinical entities it contains. \
RULES: \
(1) Extract ONLY entities that literally appear in the text — never infer, complete, or add \
anything the text does not state. \
(2) conditions = diagnoses and problems, coded to ICD-10. \
(3) medications = drugs, coded to RxNorm. \
(4) labs = laboratory results, vitals, and observations, coded to LOINC. \
(5) Put the exact wording from the text in the text field. \
(6) If you do not know the correct code with confidence, leave code as an empty string. \
An empty code is CORRECT; a guessed code is a defect. \
(7) If the text contains no clinical entities of a kind, return an empty array for it. \
/no_think";

/// A configured extractor. Built once per ingest job; `None` when the feature is off
/// or Foundry is unavailable, which keeps the call site in `ingest::execute` down to a
/// single `if let Some(..)`.
pub struct Extractor {
    foundry: Arc<FoundryManager>,
    spec: ModelSpec,
    min_words: usize,
    concurrency: usize,
    timeout: Duration,
    max_chars: usize,
}

impl Extractor {
    /// Build an extractor from config, or `None` when disabled / Foundry-less.
    pub fn new(
        config: &Config,
        foundry: Option<Arc<FoundryManager>>,
        spec: ModelSpec,
    ) -> Option<Self> {
        if !config.router.extract_enabled {
            return None;
        }
        let foundry = foundry?;
        Some(Extractor {
            foundry,
            spec,
            min_words: config.router.extract_min_words,
            // Concurrency 0 would make `buffer_unordered` yield nothing forever.
            concurrency: config.router.extract_concurrency.max(1),
            timeout: Duration::from_secs(config.router.extract_timeout_secs),
            max_chars: config.router.extract_max_chars,
        })
    }

    /// The model alias this extractor routes to (for the job log).
    pub fn alias(&self) -> &str {
        &self.spec.alias
    }

    /// Extract one row. Returns `None` for any failure — timeout, model unavailable,
    /// unparseable output, or simply nothing found.
    async fn extract_one(&self, text: &str) -> Option<ExtractedClinical> {
        // Truncate on a char boundary: the NPU variants are context-capped, and a long
        // note would otherwise be rejected outright rather than partially read.
        let clipped: String = text.chars().take(self.max_chars).collect();
        let user = format!("Record text:\n{clipped}");

        let call = self
            .foundry
            .plan_extraction(&self.spec, EXTRACT_SYSTEM, &user);
        let mut out = match tokio::time::timeout(self.timeout, call).await {
            Ok(Ok(v)) => v,
            Ok(Err(e)) => {
                tracing::debug!(error = %e, "extractor call failed; row left unannotated");
                return None;
            }
            Err(_) => {
                tracing::debug!(
                    secs = self.timeout.as_secs(),
                    "extractor timed out; row left unannotated"
                );
                return None;
            }
        };
        out.stamp_systems(&self.spec.alias);
        if out.is_empty() { None } else { Some(out) }
    }

    /// Extract a batch of `(row_pk, text)` pairs with bounded concurrency.
    ///
    /// Only annotated rows come back; ineligible rows and empty model results are omitted.
    /// Takes the rows **by value** rather than by slice on purpose: a future that
    /// borrows from the caller's buffer is generic over that borrow's lifetime, which
    /// makes the whole stream higher-ranked and costs `ingest::run` the `Send` bound
    /// `tokio::spawn` needs. Owning the rows keeps every lifetime concrete.
    pub async fn extract_batch(&self, rows: Vec<(String, String)>) -> ExtractBatch {
        let eligible_rows: Vec<(String, String)> = rows
            .into_iter()
            .filter(|(_, text)| eligible(text, self.min_words))
            .collect();

        let jobs = eligible_rows.into_iter().map(|(pk, text)| async move {
            let out = self.extract_one(&text).await;
            (pk, out)
        });

        let results: Vec<(String, Option<ExtractedClinical>)> = futures::stream::iter(jobs)
            .buffer_unordered(self.concurrency)
            .collect()
            .await;

        let annotated = results
            .into_iter()
            .filter_map(|(pk, out)| out.map(|extracted| (pk, extracted)))
            .collect();
        ExtractBatch { annotated }
    }
}

/// Outcome of one `extract_batch` call.
pub struct ExtractBatch {
    /// `(row_pk, annotation)` for every row that produced at least one entity.
    pub annotated: Vec<(String, ExtractedClinical)>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stamp_systems_sets_vocabulary_per_field() {
        let mut e = ExtractedClinical {
            conditions: vec![CodedTerm {
                text: " diabetes ".into(),
                code: " E11 ".into(),
                system: String::new(),
            }],
            medications: vec![CodedTerm {
                text: "metformin".into(),
                code: String::new(),
                system: "wrong".into(),
            }],
            labs: vec![CodedTerm {
                text: "HbA1c".into(),
                code: "4548-4".into(),
                system: String::new(),
            }],
            model: String::new(),
        };
        e.stamp_systems("phi-4-mini");

        assert_eq!(e.conditions[0].system, "icd10");
        assert_eq!(e.conditions[0].text, "diabetes", "text should be trimmed");
        assert_eq!(e.conditions[0].code, "E11", "code should be trimmed");
        // A model-supplied system is always overwritten, never trusted.
        assert_eq!(e.medications[0].system, "rxnorm");
        assert_eq!(e.labs[0].system, "loinc");
        assert_eq!(e.model, "phi-4-mini");
    }

    #[test]
    fn stamp_systems_drops_blank_terms() {
        let mut e = ExtractedClinical {
            conditions: vec![
                CodedTerm {
                    text: "  ".into(),
                    code: "E11".into(),
                    system: String::new(),
                },
                CodedTerm {
                    text: "asthma".into(),
                    code: String::new(),
                    system: String::new(),
                },
            ],
            ..Default::default()
        };
        e.stamp_systems("m");
        assert_eq!(e.conditions.len(), 1);
        assert_eq!(e.conditions[0].text, "asthma");
    }

    #[test]
    fn a_term_with_no_code_still_survives() {
        // Rule (6) tells the model to leave the code blank rather than guess, so a
        // blank code must not be treated as a blank term.
        let mut e = ExtractedClinical {
            conditions: vec![CodedTerm {
                text: "acute kidney injury".into(),
                code: String::new(),
                system: String::new(),
            }],
            ..Default::default()
        };
        e.stamp_systems("m");
        assert_eq!(e.conditions.len(), 1);
        assert!(!e.is_empty());
    }

    #[test]
    fn empty_annotation_is_reported_empty() {
        let e = ExtractedClinical::default();
        assert!(e.is_empty());
        assert_eq!(e.len(), 0);
    }

    #[test]
    fn eligibility_is_a_word_count_floor() {
        assert!(!eligible("one two three four", 5));
        assert!(eligible("one two three four five", 5));
        assert!(eligible("one two three four five six", 5));
        assert!(!eligible("", 1));
    }

    #[test]
    fn extracted_round_trips_through_json() {
        // The annotation is written into BSON via serde; a rename or a missing
        // `default` would silently drop a field on read-back.
        let mut e = ExtractedClinical {
            labs: vec![CodedTerm {
                text: "HbA1c 7.2%".into(),
                code: "4548-4".into(),
                system: String::new(),
            }],
            ..Default::default()
        };
        e.stamp_systems("phi-4-mini");
        let json = serde_json::to_string(&e).unwrap();
        let back: ExtractedClinical = serde_json::from_str(&json).unwrap();
        assert_eq!(back.labs[0].code, "4548-4");
        assert_eq!(back.labs[0].system, "loinc");
        assert_eq!(back.model, "phi-4-mini");
    }

    #[test]
    fn missing_arrays_deserialize_as_empty() {
        // Small models routinely omit a key entirely instead of sending [].
        let back: ExtractedClinical =
            serde_json::from_str(r#"{"conditions":[{"text":"asthma"}]}"#).unwrap();
        assert_eq!(back.conditions.len(), 1);
        assert_eq!(back.conditions[0].code, "");
        assert!(back.medications.is_empty());
        assert!(back.labs.is_empty());
    }
}
