//! Clarification-question templates for the v3 intent router.
//!
//! When Tier 1.5 detects a required slot that cannot be filled from the
//! question or focus, the router produces a `Clarify` decision instead of
//! falling to Tier 2.  These templates formulate a short, natural-sounding
//! question — **no model call**.
//!
//! Five templates match the five [`MissingSlot`] variants.

use crate::nl2sql::ir::spec::MissingSlot;
use crate::router::entities::RouteEntities;

/// Return a clarification question for the given missing slot.
///
/// The returned string is addressed directly to the user and references
/// any available entity context from `entities` (e.g. the detected concept
/// or service line) to make it as specific as possible.
pub fn template(slot: &MissingSlot, entities: &RouteEntities) -> String {
    match slot {
        MissingSlot::Subject => {
            if let Some(concept) = entities.concepts.first() {
                use crate::ontology::concepts::DESCRIPTORS;
                let plural = DESCRIPTORS
                    .iter()
                    .find(|d| d.concept == *concept)
                    .map(|d| d.plural)
                    .unwrap_or("records");
                format!("Which {plural} are you asking about?")
            } else {
                "What would you like to know about? \
                 For example, you could ask about encounters, prescriptions, or admissions."
                    .to_string()
            }
        }
        MissingSlot::Patient => {
            "Which patient are you asking about? \
             You can refer to them by name or patient ID (e.g. PT-0042)."
                .to_string()
        }
        MissingSlot::TimeRange => {
            "What time period should I use? \
             For example: \"last 7 days\", \"this month\", or \"Q1\"."
                .to_string()
        }
        MissingSlot::Metric => {
            if let Some(dim) = &entities.dimension {
                format!(
                    "What would you like to measure per {dim}? \
                     For example: count, average, or total."
                )
            } else {
                "What measure are you looking for? \
                 For example: count, average, total, or rate."
                    .to_string()
            }
        }
        MissingSlot::Dimension => {
            if entities.concepts.is_empty() {
                "How would you like to group the results? \
                 For example: by ward, by provider, or by diagnosis."
                    .to_string()
            } else {
                use crate::ontology::concepts::DESCRIPTORS;
                let plural = DESCRIPTORS
                    .iter()
                    .find(|d| Some(&d.concept) == entities.concepts.first())
                    .map(|d| d.plural)
                    .unwrap_or("records");
                format!(
                    "How would you like to group the {plural}? \
                     For example: by ward, by provider, or by month."
                )
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::router::entities::RouteEntities;
    use crate::ontology::concepts::EntityConcept;

    fn empty() -> RouteEntities {
        RouteEntities::default()
    }

    fn with_concept(c: EntityConcept) -> RouteEntities {
        RouteEntities { concepts: vec![c], ..Default::default() }
    }

    fn with_dimension(d: &str) -> RouteEntities {
        RouteEntities { dimension: Some(d.to_string()), ..Default::default() }
    }

    #[test]
    fn subject_with_concept() {
        let q = template(&MissingSlot::Subject, &with_concept(EntityConcept::Prescription));
        assert!(q.to_lowercase().contains("prescription"), "got: {q}");
    }

    #[test]
    fn subject_without_concept() {
        let q = template(&MissingSlot::Subject, &empty());
        assert!(!q.is_empty());
        assert!(q.contains("encounter") || q.contains("ask"));
    }

    #[test]
    fn patient_slot() {
        let q = template(&MissingSlot::Patient, &empty());
        assert!(q.contains("patient") || q.contains("PT-"));
    }

    #[test]
    fn time_range_slot() {
        let q = template(&MissingSlot::TimeRange, &empty());
        assert!(q.contains("time") || q.contains("period") || q.contains("month"));
    }

    #[test]
    fn metric_slot_with_dimension() {
        let q = template(&MissingSlot::Metric, &with_dimension("ward"));
        assert!(q.contains("ward"), "got: {q}");
    }

    #[test]
    fn dimension_slot_with_concept() {
        let q = template(&MissingSlot::Dimension, &with_concept(EntityConcept::Admission));
        assert!(q.to_lowercase().contains("admission"), "got: {q}");
    }

    #[test]
    fn all_five_slots_return_non_empty() {
        for slot in [
            MissingSlot::Subject,
            MissingSlot::Patient,
            MissingSlot::TimeRange,
            MissingSlot::Metric,
            MissingSlot::Dimension,
        ] {
            let q = template(&slot, &empty());
            assert!(!q.is_empty(), "template for {:?} must be non-empty", slot);
        }
    }
}
