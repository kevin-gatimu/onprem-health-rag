//! Conversational focus — minimal per-turn context for anaphora resolution.
//!
//! `ConversationFocus` is owned by plan 02.  Plan 06 will extend the struct
//! with richer multi-turn fields; plan 02 defines the baseline.

use crate::nl2sql::ir::spec::{QuerySpec, TimeRange};
use crate::ontology::{concepts::EntityConcept, service_line::ServiceLine};

/// Minimal context retained across turns.  Callers that have no prior turn
/// pass `None`; callers with history build this from the previous turn's
/// `RouteDecision`.
#[derive(Debug, Clone, Default)]
pub struct ConversationFocus {
    /// Dominant concept referenced in the previous turn (e.g., `Patient`).
    pub concept: Option<EntityConcept>,
    /// Patient identifier established in the previous turn.
    pub patient_key: Option<String>,
    /// Time range parsed in the previous turn.
    pub time_range: Option<TimeRange>,
    /// Service line active in the previous turn.
    pub line: Option<ServiceLine>,
    /// The last structured spec produced, for anaphoric "same filters" resolution.
    pub last_spec: Option<QuerySpec>,
}

/// A question after anaphoric references have been expanded.
#[derive(Debug, Clone)]
pub struct ResolvedQuestion {
    /// The rewritten question text (substitutions applied in place).
    pub question: String,
    /// Human-readable log of every substitution that was applied, for tracing.
    pub substitutions: Vec<String>,
}

impl ResolvedQuestion {
    /// No substitution was needed.
    pub fn unchanged(q: &str) -> Self {
        ResolvedQuestion { question: q.to_string(), substitutions: vec![] }
    }
}

/// Deterministic anaphora substitution — no model call.
///
/// Rules applied in priority order (earlier rule wins if two patterns both match):
///
/// 1. **Time anaphora**: "same period" / "that period" / "that timeframe" →
///    textual label of `focus.time_range` (e.g. "last month").
/// 2. **Patient anaphora**: "that patient" / "the patient" / "this patient" →
///    `focus.patient_key` (e.g. "PT-0042").
/// 3. **Generic pronoun**: first of "them" / "they" / "those" / "these" that
///    appears as a whole word → plural noun of `focus.concept`
///    (e.g. "encounters").
///
/// All substitutions are pure string operations.  If `focus` is `None` or the
/// relevant focus field is `None`, the corresponding rule is skipped.
pub fn resolve(question: &str, focus: Option<&ConversationFocus>) -> ResolvedQuestion {
    let focus = match focus {
        Some(f) => f,
        None => return ResolvedQuestion::unchanged(question),
    };

    let mut q = question.to_string();
    let mut subs: Vec<String> = Vec::new();

    // Rule 1 — time anaphora
    if let Some(tr) = &focus.time_range {
        let label = time_range_label(tr);
        for pattern in ["same period", "that period", "that timeframe", "same timeframe"] {
            let lower = q.to_lowercase();
            if let Some(pos) = lower.find(pattern) {
                q = format!("{}{}{}", &q[..pos], &label, &q[pos + pattern.len()..]);
                subs.push(format!("'{pattern}' → '{label}'"));
                break; // one time substitution per call
            }
        }
    }

    // Rule 2 — patient anaphora
    if let Some(pk) = &focus.patient_key {
        for pattern in ["that patient", "the patient", "this patient"] {
            let lower = q.to_lowercase();
            if let Some(pos) = lower.find(pattern) {
                q = format!("{}{}{}", &q[..pos], pk, &q[pos + pattern.len()..]);
                subs.push(format!("'{pattern}' → '{pk}'"));
                break;
            }
        }
    }

    // Rule 3 — generic pronoun → concept plural
    if let Some(concept) = &focus.concept {
        use crate::ontology::concepts::DESCRIPTORS;
        let plural = DESCRIPTORS
            .iter()
            .find(|d| d.concept == *concept)
            .map(|d| d.plural)
            .unwrap_or("records");

        for pattern in ["them", "they", "those", "these"] {
            // Match only whole words to avoid replacing "themselves" etc.
            let re_src = format!(r"(?i)\b{pattern}\b");
            if let Ok(re) = regex::Regex::new(&re_src) {
                let replaced = re.replace_all(&q, plural).into_owned();
                if replaced != q {
                    subs.push(format!("'{pattern}' → '{plural}'"));
                    q = replaced;
                    break; // first matching pronoun only
                }
            }
        }
    }

    ResolvedQuestion { question: q, substitutions: subs }
}

// ---------------------------------------------------------------------------
// Private helpers
// ---------------------------------------------------------------------------

/// Human-readable label for a time range ("last month"). Also used by the
/// agent persona's focus block.
pub fn time_range_label(tr: &TimeRange) -> String {
    match tr {
        TimeRange::Today => "today".to_string(),
        TimeRange::Yesterday => "yesterday".to_string(),
        TimeRange::ThisWeek => "this week".to_string(),
        TimeRange::LastWeek => "last week".to_string(),
        TimeRange::ThisMonth => "this month".to_string(),
        TimeRange::LastMonth => "last month".to_string(),
        TimeRange::ThisQuarter => "this quarter".to_string(),
        TimeRange::LastQuarter => "last quarter".to_string(),
        TimeRange::ThisYear => "this year".to_string(),
        TimeRange::LastYear => "last year".to_string(),
        TimeRange::Last { n, unit } => format!("last {} {}", n, unit.as_str()),
        TimeRange::Next { n, unit } => format!("next {} {}", n, unit.as_str()),
        TimeRange::Within { n, unit } => format!("within {} {}", n, unit.as_str()),
        TimeRange::Absolute { .. } => "the specified period".to_string(),
        TimeRange::WithinThisWeek => "expiring this week".to_string(),
        TimeRange::WithinThisMonth => "expiring this month".to_string(),
        TimeRange::WithinThisQuarter => "expiring this quarter".to_string(),
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::nl2sql::ir::spec::BucketUnit;

    fn focus_with_time() -> ConversationFocus {
        ConversationFocus {
            time_range: Some(TimeRange::Last { n: 7, unit: BucketUnit::Day }),
            ..ConversationFocus::default()
        }
    }

    fn focus_with_patient() -> ConversationFocus {
        ConversationFocus {
            patient_key: Some("PT-0042".into()),
            ..ConversationFocus::default()
        }
    }

    fn focus_with_concept() -> ConversationFocus {
        ConversationFocus {
            concept: Some(EntityConcept::Encounter),
            ..ConversationFocus::default()
        }
    }

    #[test]
    fn no_focus_is_unchanged() {
        let r = resolve("how many patients?", None);
        assert_eq!(r.question, "how many patients?");
        assert!(r.substitutions.is_empty());
    }

    #[test]
    fn time_anaphora_same_period() {
        let focus = focus_with_time();
        let r = resolve("repeat the analysis for the same period", Some(&focus));
        assert!(r.question.contains("last 7 day"), "got: {}", r.question);
        assert_eq!(r.substitutions.len(), 1);
    }

    #[test]
    fn patient_anaphora_that_patient() {
        let focus = focus_with_patient();
        let r = resolve("show encounters for that patient", Some(&focus));
        assert!(r.question.contains("PT-0042"), "got: {}", r.question);
    }

    #[test]
    fn pronoun_them_to_encounters() {
        let focus = focus_with_concept();
        let r = resolve("how many of them were admitted last month?", Some(&focus));
        // "them" should be replaced by encounters plural
        assert!(
            r.question.to_lowercase().contains("encounter"),
            "got: {}",
            r.question
        );
    }

    #[test]
    fn empty_focus_does_not_panic() {
        let focus = ConversationFocus::default();
        let r = resolve("show all patients", Some(&focus));
        assert_eq!(r.question, "show all patients");
    }
}
