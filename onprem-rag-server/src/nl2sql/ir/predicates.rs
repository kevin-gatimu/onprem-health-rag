//! Domain predicate table — ontology-level phrase → typed `Filter` or `Measure`.
//!
//! Every predicate is expressed entirely in `(EntityConcept, ColumnRole, name_hint)` —
//! never in physical column names. Binding against the source schema happens in
//! `bind.rs` when the column's `physical` field is filled.
//!
//! # Rule
//! `name_hint` values are generic fragments (`weight`, `expir`, `abnormal`, `los`),
//! never dev-seed column names. The same predicate must work on both the dev binding
//! and the alt binding.

use crate::ontology::concepts::EntityConcept;
use crate::ontology::roles::ColumnRole;

use super::spec::{ColumnRef, Filter, FilterOp, FilterValue};

/// A domain predicate: a phrase that maps to a typed filter expression.
#[derive(Debug, Clone)]
pub struct DomainPredicate {
    /// Display phrase (lowercase, tokenized for matching).
    pub phrases: &'static [&'static str],
    /// The concept this predicate targets.
    pub concept: EntityConcept,
    /// Role + optional hint for the target column.
    pub role: ColumnRole,
    pub name_hint: Option<&'static str>,
    /// How to apply the predicate once the column is resolved.
    pub kind: PredicateKind,
}

#[derive(Debug, Clone)]
pub enum PredicateKind {
    /// `col < value` (numeric)
    Lt(f64),
    /// `col IS TRUE`
    IsTrue,
    /// `col IS NULL`
    IsNull,
    /// `col IS NOT NULL`
    IsNotNull,
    /// `col IN (values)` — static string slice, const-compatible.
    In(&'static [&'static str]),
    /// `col > value` (numeric)
    Gt(f64),
    /// `col = 0` or `col = 0.0`
    IsZero,
}

/// All known domain predicates (expressed in roles only, no physical names).
pub static DOMAIN_PREDICATES: &[DomainPredicate] = &[
    // ── Low birth weight ─────────────────────────────────────────────────────
    DomainPredicate {
        phrases: &["low birth weight", "lbw"],
        concept: EntityConcept::Newborn,
        role: ColumnRole::Measure,
        name_hint: Some("weight"),
        kind: PredicateKind::Lt(2500.0),
    },
    // ── Stillbirth ────────────────────────────────────────────────────────────
    DomainPredicate {
        phrases: &["stillbirth", "stillborn", "still birth"],
        concept: EntityConcept::Delivery,
        role: ColumnRole::Status,
        name_hint: None,
        kind: PredicateKind::In(&["stillbirth", "stillborn"]),
    },
    // ── Triage category 1 / red ───────────────────────────────────────────────
    DomainPredicate {
        phrases: &["category 1", "cat 1", "cat1", "red triage", "triage 1"],
        concept: EntityConcept::Triage,
        role: ColumnRole::Priority,
        name_hint: Some("category"),
        kind: PredicateKind::In(&["1", "red", "category_1"]),
    },
    // ── Abnormal / critical result ────────────────────────────────────────────
    DomainPredicate {
        phrases: &["abnormal result", "critical result", "abnormal", "critical"],
        concept: EntityConcept::LabResult,
        role: ColumnRole::Flag,
        name_hint: Some("abnormal"),
        kind: PredicateKind::IsTrue,
    },
    // ── No-show / DNA ─────────────────────────────────────────────────────────
    DomainPredicate {
        phrases: &["no-show", "no show", "dna", "did not attend"],
        concept: EntityConcept::Appointment,
        role: ColumnRole::Status,
        name_hint: None,
        kind: PredicateKind::In(&["no_show", "dna", "did_not_attend"]),
    },
    // ── Outstanding / unpaid bill ─────────────────────────────────────────────
    DomainPredicate {
        phrases: &["outstanding", "unpaid", "overdue payment", "balance due"],
        concept: EntityConcept::Bill,
        role: ColumnRole::Amount,
        name_hint: Some("due"),
        kind: PredicateKind::Gt(0.0),
    },
    // ── Readmission ───────────────────────────────────────────────────────────
    DomainPredicate {
        phrases: &["readmission", "readmitted"],
        concept: EntityConcept::Admission,
        role: ColumnRole::Flag,
        name_hint: Some("readmission"),
        kind: PredicateKind::IsTrue,
    },
    // ── Out of stock ──────────────────────────────────────────────────────────
    DomainPredicate {
        phrases: &["out of stock", "stockout", "stock out", "zero stock"],
        concept: EntityConcept::StockBatch,
        role: ColumnRole::Quantity,
        name_hint: Some("on_hand"),
        kind: PredicateKind::IsZero,
    },
    // ── Lost to follow-up ─────────────────────────────────────────────────────
    DomainPredicate {
        phrases: &["lost to follow-up", "ltfu", "lost to followup", "defaulted"],
        concept: EntityConcept::ProgramEnrollment,
        role: ColumnRole::Status,
        name_hint: None,
        kind: PredicateKind::In(&["lost_to_followup", "ltfu", "defaulted"]),
    },
    // ── Break-glass ───────────────────────────────────────────────────────────
    DomainPredicate {
        phrases: &["break-glass", "break glass", "emergency access", "override access"],
        concept: EntityConcept::AccessLog,
        role: ColumnRole::Flag,
        name_hint: Some("break_glass"),
        kind: PredicateKind::IsTrue,
    },
    // ── In charge ─────────────────────────────────────────────────────────────
    DomainPredicate {
        phrases: &["in charge", "nurse in charge", "shift leader"],
        concept: EntityConcept::Shift,
        role: ColumnRole::Flag,
        name_hint: Some("in_charge"),
        kind: PredicateKind::IsTrue,
    },
    // ── Pending certification (mortality) ─────────────────────────────────────
    DomainPredicate {
        phrases: &["pending certification", "uncertified death", "no certificate"],
        concept: EntityConcept::Mortality,
        role: ColumnRole::Identifier,
        name_hint: Some("certificate"),
        kind: PredicateKind::IsNull,
    },
    // ── Postpartum haemorrhage ────────────────────────────────────────────────
    DomainPredicate {
        phrases: &["postpartum haemorrhage", "pph", "postpartum hemorrhage"],
        concept: EntityConcept::Delivery,
        role: ColumnRole::Flag,
        name_hint: Some("pph"),
        kind: PredicateKind::IsTrue,
    },
    // ── Equipment out of service ──────────────────────────────────────────────
    DomainPredicate {
        phrases: &["out of service", "faulty", "broken", "under repair"],
        concept: EntityConcept::Equipment,
        role: ColumnRole::Status,
        name_hint: None,
        kind: PredicateKind::In(&["out_of_service", "faulty", "broken"]),
    },
    // ── Cases without consent ─────────────────────────────────────────────────
    DomainPredicate {
        phrases: &["no consent", "without consent", "missing consent", "no consent recorded"],
        concept: EntityConcept::Consent,
        role: ColumnRole::PrimaryKey, // used to detect absence via NOT EXISTS
        name_hint: None,
        kind: PredicateKind::IsNull, // signals anti-join in compiler
    },
];

/// Look up a domain predicate by phrase. Returns the first match.
/// The question must be pre-normalized (lowercase, whitespace-collapsed).
pub fn lookup_predicate(normalized_question: &str) -> Option<&'static DomainPredicate> {
    DOMAIN_PREDICATES
        .iter()
        .find(|p| p.phrases.iter().any(|phrase| normalized_question.contains(phrase)))
}

/// Build a `Filter` from a `DomainPredicate` using a logical (unbound) `ColumnRef`.
/// The `physical` field is `None`; `bind()` will fill it.
pub fn predicate_to_filter(pred: &DomainPredicate) -> Filter {
    let column = ColumnRef {
        concept: pred.concept,
        role: pred.role,
        name_hint: pred.name_hint.map(|h| h.to_string()),
        physical: None,
    };
    let (op, value) = match &pred.kind {
        PredicateKind::Lt(n) => (FilterOp::Lt, FilterValue::Num(*n)),
        PredicateKind::Gt(n) => (FilterOp::Gt, FilterValue::Num(*n)),
        PredicateKind::IsTrue => (FilterOp::IsTrue, FilterValue::Bool(true)),
        PredicateKind::IsNull => (FilterOp::IsNull, FilterValue::Bool(false)),
        PredicateKind::IsNotNull => (FilterOp::IsNotNull, FilterValue::Bool(true)),
        PredicateKind::IsZero => (FilterOp::Eq, FilterValue::Num(0.0)),
        PredicateKind::In(vals) => (
            FilterOp::In,
            FilterValue::List(vals.iter().map(|v| FilterValue::Str(v.to_string())).collect()),
        ),
    };
    Filter { column, op, value }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_physical_names_in_predicates() {
        // Verify that no predicate uses dev-seed physical column names.
        let forbidden = ["patients", "encounters", "deliveries", "prescriptions"];
        for pred in DOMAIN_PREDICATES {
            if let Some(hint) = pred.name_hint {
                for name in &forbidden {
                    assert!(
                        !hint.contains(name),
                        "predicate {:?} name_hint '{}' contains physical name '{}'",
                        pred.phrases,
                        hint,
                        name
                    );
                }
            }
        }
    }

    #[test]
    fn lookup_no_show_predicate() {
        let pred = lookup_predicate("what is our no-show rate").unwrap();
        assert_eq!(pred.concept, EntityConcept::Appointment);
    }

    #[test]
    fn lookup_lbw_predicate() {
        let pred = lookup_predicate("how many babies were low birth weight").unwrap();
        assert_eq!(pred.concept, EntityConcept::Newborn);
        assert!(matches!(pred.kind, PredicateKind::Lt(_)));
    }

    #[test]
    fn lookup_out_of_stock() {
        let pred = lookup_predicate("which medicines are out of stock").unwrap();
        assert_eq!(pred.concept, EntityConcept::StockBatch);
    }

    #[test]
    fn predicate_to_filter_produces_unbound_column_ref() {
        let pred = lookup_predicate("no-show").unwrap();
        let f = predicate_to_filter(pred);
        assert!(f.column.physical.is_none());
    }
}
