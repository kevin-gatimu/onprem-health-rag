//! Query intent classification.
//!
//! `classify_lexical` is a fast, model-free path used for the Chat "Auto" mode.
//! AI Agents tab selections (Health Query / Trends / Summarize / Patient Lookup)
//! pass `intent` explicitly in the request body, bypassing this classifier.
//!
//! When neither the tab nor this function produces a result, the caller (the
//! intent router, `router::route`) can escalate to the Tier-2 model classifier
//! (phi-4-mini on the NPU) for ambiguous free-form questions.

use serde::{Deserialize, Serialize};

/// What the user is trying to do. Controls which server path handles the request.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum QueryIntent {
    /// Return specific records for one patient or entity (retrieval path).
    Lookup,
    /// Explain, summarise, or describe something (retrieval + generation path).
    Narrative,
    /// Count, group, aggregate, or rank records (structured path, no time bucket).
    Aggregation,
    /// Trend over time — aggregation with a `time_bucket` (structured path).
    Trend,
    /// List or enumerate all records of a type (paginated list path).
    Enumeration,
    /// Multiple sequential reasoning steps (future multi-hop path; currently
    /// falls back to the narrative path).
    MultiHop,
}

/// Classify a query purely from lexical markers. Returns `None` for questions
/// the lexical pass cannot confidently categorise (use a small-model fallback then).
///
/// Priority: Trend > Enumeration > Aggregation > Lookup > Narrative.
/// Enumeration is checked before Aggregation so a list request is not swallowed
/// by a stray count marker.
pub fn classify_lexical(question: &str) -> Option<QueryIntent> {
    let q = question.to_ascii_lowercase();

    // Trend markers take precedence because a trend question is a specialisation
    // of aggregation.
    if contains_any(&q, TREND_MARKERS) {
        return Some(QueryIntent::Trend);
    }

    // Enumeration before aggregation: "list all patients" must not match "total".
    if contains_any(&q, ENUMERATION_MARKERS) {
        return Some(QueryIntent::Enumeration);
    }

    // Aggregation markers — plus a targeted word-boundary check for "rate" at
    // end of sentence.  "What is our no-show rate?" ends with "rate?" (no space
    // after) so the space-delimited marker `" rate "` does not match; stripping
    // trailing punctuation and checking `ends_with(" rate")` handles this
    // precisely without the false positive of broadening the substring to " rate"
    // (which would also match " rated", " rates", " rateable", etc.).
    let q_stripped = q.trim_end_matches(|c: char| matches!(c, '.' | '?' | '!' | ','));
    if contains_any(&q, AGGREGATION_MARKERS) || q_stripped.ends_with(" rate") {
        return Some(QueryIntent::Aggregation);
    }

    if contains_any(&q, LOOKUP_MARKERS) {
        return Some(QueryIntent::Lookup);
    }

    // "What is/are X by Y?" — a GROUP BY structured query.
    //
    // "What is outstanding by insurer?" asks for a result aggregated or grouped
    // by a dimension (insurer, department, ward, …).  The "by Y" suffix is the
    // same GROUP-BY signal present in the explicit "by clinic" / "by region"
    // aggregation markers above, but covers any dimension.
    //
    // Guard: Trend / Enumeration / Aggregation / Lookup all checked first, so
    // this only fires for "what is/are" questions that carry no other structural
    // signal — preventing it from stealing e.g. "What are all patients by name?"
    // (ENUMERATION "what are" is not a current marker, but "which " / "who " are).
    if (q.starts_with("what is ") || q.starts_with("what are "))
        && q.contains(" by ")
    {
        return Some(QueryIntent::Aggregation);
    }

    if contains_any(&q, NARRATIVE_MARKERS) {
        return Some(QueryIntent::Narrative);
    }

    None
}

fn contains_any(haystack: &str, needles: &[&str]) -> bool {
    needles.iter().any(|n| haystack.contains(n))
}

/// True when the question carries a narrative marker ("summarise", "explain",
/// "describe", …). Exposed for the intent router's hybrid detection: a question
/// that is *both* structural (a cohort filter) and narrative ("summarise the
/// notes of patients who visited more than 3 times") is a hybrid candidate.
pub fn has_narrative_marker(question: &str) -> bool {
    contains_any(&question.to_ascii_lowercase(), NARRATIVE_MARKERS)
}

// Marker lists. Kept as `&[&str]` slices for easy auditing and extension.

const TREND_MARKERS: &[&str] = &[
    "trend",
    "over time",
    "per month",
    "per week",
    "per year",
    "per quarter",
    "monthly",
    "weekly",
    "yearly",
    "quarterly",
    "by month",
    "by week",
    "by year",
    "time series",
    "progression",
    "change over",
    "over the past",
    "last 6 months",
    "last 12 months",
    "year to date",
];

/// Markers that indicate the user wants to enumerate / list all records.
/// Checked before AGGREGATION_MARKERS so list requests are not misclassified.
const ENUMERATION_MARKERS: &[&str] = &[
    "list ",
    "list all",
    "list every",
    "list the ",
    "show all",
    "show every",
    "who are the",
    "names of all",
    "give me a list",
    "enumerate",
    "all patients",
    "all records",
    "all encounters",
    "all diagnoses",
    // Interrogative pronouns introducing a filtered entity set.
    // "Which medicines are out of stock?", "Which patients have been in over 14
    // days?", "Which claims were rejected?" all expect a list of matching rows.
    "which ",
    // "Who is admitted right now?", "Who has been lost to follow-up?" —
    // person-filtered sets.
    "who is ",
    "who has ",
    "who was ",
    // "Whose practising licence expires this quarter?" — possessive filtered set.
    "whose ",
];

const AGGREGATION_MARKERS: &[&str] = &[
    "how many",
    // "How much amoxicillin did we dispense?" — mass-noun quantity, sibling of
    // "how many".  Trailing space avoids matching "how much" in mid-word combos.
    "how much ",
    "count of",
    "number of",
    "total number",
    "total count",
    "average",
    "mean ",
    // Space-delimited so " rated" / " rates" can never match. This catches only
    // mid-sentence rates; end-of-sentence ones ("no-show rate?") are handled by
    // the ends_with(" rate") check in classify_lexical — `q` is only lowercased
    // here, no trailing space is appended.
    " rate ",
    "distribution",
    "most common",
    "least common",
    "top ",
    "bottom ",
    "compare",
    "breakdown",
    "per clinic",
    "per region",
    "per facility",
    "by clinic",
    "by region",
    "by facility",
    "by diagnosis",
    "how often",
    "frequency",
    "prevalence",
    "incidence",
    "percentage of",
    "proportion of",
    "ratio of",
    "sum of",
    "aggregate",
    "group by",
    "ranked by",
    // Distribution / split vocabulary — analytical shapes the earlier markers miss.
    "split",
    "histogram",
    "percentile",
    "median",
    "proportion",
    "ratio",
    "by gender",
    "by sex",
    "by age group",
    "by age",
    "by blood type",
    "by race",
    "by ethnicity",
];

const LOOKUP_MARKERS: &[&str] = &[
    "find patient",
    "patient record",
    "patient with id",
    "lookup",
    "look up",
    "specific patient",
    "patient history for",
    "records for patient",
    "details for patient",
    "patient profile",
    // Entity-info phrasings ("share information about patient Jane Chebet").
    // Without these the question escalates to the Tier-2 model, which can
    // misread it as an aggregation and narrate a useless row count.
    "information about",
    "information on",
    "info about",
    "info on",
    "details about",
    "details on",
    "profile of",
];

const NARRATIVE_MARKERS: &[&str] = &[
    "tell me about",
    "explain",
    "summarise",
    "summarize",
    "describe",
    "what is",
    "what are",
    "why does",
    "why is",
    "how does",
    "give me an overview",
    "provide context",
    "background on",
];

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn trend_markers_recognised() {
        assert_eq!(
            classify_lexical("Show trend of malaria cases per month"),
            Some(QueryIntent::Trend)
        );
        assert_eq!(
            classify_lexical("Malaria over time"),
            Some(QueryIntent::Trend)
        );
    }

    #[test]
    fn aggregation_markers_recognised() {
        assert_eq!(
            classify_lexical("How many patients have diabetes?"),
            Some(QueryIntent::Aggregation)
        );
        assert_eq!(
            classify_lexical("Most common diagnoses"),
            Some(QueryIntent::Aggregation)
        );
        assert_eq!(
            classify_lexical("Top 10 diagnoses by count"),
            Some(QueryIntent::Aggregation)
        );
    }

    #[test]
    fn enumeration_markers_recognised() {
        assert_eq!(
            classify_lexical("list all patients"),
            Some(QueryIntent::Enumeration)
        );
        assert_eq!(
            classify_lexical("who are the patients"),
            Some(QueryIntent::Enumeration)
        );
        assert_eq!(
            classify_lexical("show all records"),
            Some(QueryIntent::Enumeration)
        );
        assert_eq!(
            classify_lexical("list 5 patients"),
            Some(QueryIntent::Enumeration)
        );
    }

    #[test]
    fn enumeration_takes_priority_over_aggregation() {
        // "list all" should win over "how many" if both appear.
        assert_eq!(
            classify_lexical("list all patients and how many there are"),
            Some(QueryIntent::Enumeration)
        );
    }

    #[test]
    fn patient_info_questions_are_lookup() {
        // These must classify at the lexical tier — escalating to the Tier-2
        // model risks a "structured aggregation" misread that narrates a row
        // count instead of the patient's actual fields.
        assert_eq!(
            classify_lexical("share information about patient Jane Chebet"),
            Some(QueryIntent::Lookup)
        );
        assert_eq!(
            classify_lexical("give me details on John Otieno"),
            Some(QueryIntent::Lookup)
        );
        assert_eq!(
            classify_lexical("show the profile of patient 42"),
            Some(QueryIntent::Lookup)
        );
    }

    #[test]
    fn how_many_is_aggregation_not_enumeration() {
        assert_eq!(
            classify_lexical("how many patients have diabetes?"),
            Some(QueryIntent::Aggregation)
        );
    }

    #[test]
    fn lookup_recognised() {
        assert_eq!(
            classify_lexical("Find patient P-001234"),
            Some(QueryIntent::Lookup)
        );
    }

    #[test]
    fn narrative_recognised() {
        assert_eq!(
            classify_lexical("Explain what hypertension means"),
            Some(QueryIntent::Narrative)
        );
    }

    #[test]
    fn ambiguous_returns_none() {
        assert_eq!(classify_lexical("show data"), None);
    }

    // -----------------------------------------------------------------------
    // Regression guards for plan-02 marker additions
    // -----------------------------------------------------------------------

    /// "how much" is the mass-noun sibling of "how many" — both ask for a quantity.
    #[test]
    fn how_much_is_aggregation() {
        assert_eq!(
            classify_lexical("How much amoxicillin did we dispense last quarter?"),
            Some(QueryIntent::Aggregation)
        );
        assert_eq!(
            classify_lexical("How much did the top 5 procedures cost?"),
            Some(QueryIntent::Aggregation)
        );
    }

    /// Rate nouns ("no-show rate", "readmission rate") signal a statistical
    /// metric — they are aggregations, not narrative explanations.
    ///
    /// Rates mid-sentence are caught by the space-delimited `" rate "` marker.
    /// Rates at end of sentence (before "?" or end-of-input) are caught by
    /// `q_stripped.ends_with(" rate")` — stripping trailing punctuation first.
    #[test]
    fn rate_noun_is_aggregation() {
        // End of sentence before "?" — the rate-specific ends_with check fires.
        assert_eq!(
            classify_lexical("What is our no-show rate?"),
            Some(QueryIntent::Aggregation)
        );
        // Mid-sentence "rate for..." — the " rate " marker fires.
        assert_eq!(
            classify_lexical("What is the readmission rate for cardiac patients?"),
            Some(QueryIntent::Aggregation)
        );
        // End of input, no punctuation.
        assert_eq!(
            classify_lexical("What is the complication rate"),
            Some(QueryIntent::Aggregation)
        );
    }

    /// "rated" must NOT trigger the rate aggregation marker.
    /// " rated" contains " rate" as a substring, so without the space-delimited
    /// marker `" rate "` this would be a false positive for Aggregation.
    #[test]
    fn rated_does_not_trigger_aggregation() {
        assert_eq!(
            classify_lexical("Show notes where the pain was rated 8"),
            None,
            "' rated' must not match ' rate ' — different word"
        );
        assert_eq!(
            classify_lexical("Encounters rated as critical"),
            None,
            "' rated' must not match ' rate '"
        );
    }

    /// "which [noun]" introducing a filtered entity set → Enumeration.
    #[test]
    fn which_question_is_enumeration() {
        assert_eq!(
            classify_lexical("Which medicines are out of stock?"),
            Some(QueryIntent::Enumeration)
        );
        assert_eq!(
            classify_lexical("Which claims were rejected?"),
            Some(QueryIntent::Enumeration)
        );
        assert_eq!(
            classify_lexical("Which results are critical and unreviewed?"),
            Some(QueryIntent::Enumeration)
        );
    }

    /// "who is/has/was [predicate]" introduces a person-filtered set → Enumeration.
    #[test]
    fn who_question_is_enumeration() {
        assert_eq!(
            classify_lexical("Who is admitted right now?"),
            Some(QueryIntent::Enumeration)
        );
        assert_eq!(
            classify_lexical("Who has been lost to follow-up in the HIV clinic?"),
            Some(QueryIntent::Enumeration)
        );
    }

    /// "whose [noun]" introduces a possessive-filtered set → Enumeration.
    #[test]
    fn whose_question_is_enumeration() {
        assert_eq!(
            classify_lexical("Whose practising licence expires this quarter?"),
            Some(QueryIntent::Enumeration)
        );
    }

    // `"what is on "` and `" expires"` were removed: they match only one fixture
    // question each and are domain predicates rather than intent-class signals.
    //
    // "What is on tomorrow's list?" falls through to NARRATIVE: "what is"
    // fires in NARRATIVE_MARKERS. The "list " ENUMERATION marker does not
    // match because "list?" (end-of-string with trailing "?") has no space
    // after "list". This costs one fixture row (theatre-list-tomorrow → Semantic).
    //
    // "What expires within 30 days?" returns None — no intent marker applies;
    // it must escalate to Tier 2 or fail-open to Semantic. This costs one more
    // fixture row (pharmacy-expiry → Semantic). Total honest cost: 2 rows → 30/32.
    #[test]
    fn what_is_on_falls_to_narrative_after_marker_removal() {
        assert_eq!(
            classify_lexical("What is on tomorrow's list?"),
            Some(QueryIntent::Narrative),
            "'what is on' removed; 'what is' NARRATIVE fires; route becomes semantic (honest cost)"
        );
    }

    #[test]
    fn expires_predicate_falls_through_to_none() {
        assert_eq!(
            classify_lexical("What expires within 30 days?"),
            None,
            "no intent marker applies; escalate to Tier 2"
        );
    }

    /// "what is/are X by Y" is a GROUP-BY aggregation.
    #[test]
    fn what_is_by_dimension_is_aggregation() {
        assert_eq!(
            classify_lexical("What is outstanding by insurer?"),
            Some(QueryIntent::Aggregation)
        );
        assert_eq!(
            classify_lexical("What are the admission counts by ward?"),
            Some(QueryIntent::Aggregation)
        );
    }

    /// Existing narrative questions must NOT be stolen by the new rules.
    #[test]
    fn narrative_not_stolen_by_new_rules() {
        // "explain" → Narrative (no new marker fires first)
        assert_eq!(
            classify_lexical("Explain what hypertension means"),
            Some(QueryIntent::Narrative)
        );
        // "what is" without " by " stays Narrative
        assert_eq!(
            classify_lexical("What is hypertension?"),
            Some(QueryIntent::Narrative)
        );
        // "tell me about" → Narrative (NARRATIVE_MARKERS fires, no earlier match)
        assert_eq!(
            classify_lexical("Tell me about PT-00042"),
            Some(QueryIntent::Narrative)
        );
    }

    /// "find patient" wins over "which " (LOOKUP checked after ENUMERATION, but
    /// "find patient" is in LOOKUP_MARKERS while "which" appears before LOOKUP).
    /// When both fire, ENUMERATION wins because it is checked first — but for
    /// "find patient X" there is no "which" so LOOKUP fires correctly.
    #[test]
    fn lookup_not_stolen_by_which_marker() {
        assert_eq!(
            classify_lexical("Find patient SYN-P0001"),
            Some(QueryIntent::Lookup)
        );
        assert_eq!(
            classify_lexical("share information about patient Jane Chebet"),
            Some(QueryIntent::Lookup)
        );
    }
}
