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

    if contains_any(&q, AGGREGATION_MARKERS) {
        return Some(QueryIntent::Aggregation);
    }

    if contains_any(&q, LOOKUP_MARKERS) {
        return Some(QueryIntent::Lookup);
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
];

const AGGREGATION_MARKERS: &[&str] = &[
    "how many",
    "count of",
    "number of",
    "total number",
    "total count",
    "average",
    "mean ",
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
}
