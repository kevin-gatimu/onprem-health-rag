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
    /// The predicate phrase is recognised but cannot be expressed in the
    /// current schema (cross-entity join or missing column type).  Callers
    /// must return `None` (NoParse) immediately when they see this variant —
    /// before any concept check and before calling `predicate_to_filter`.
    Unexpressible,
}

impl PredicateKind {
    /// Returns `true` when this predicate kind cannot be compiled into SQL.
    /// Parse rules must check this first and return `None` (refuse) so the
    /// planner can fall back to the semantic path.
    #[inline]
    pub fn is_unexpressible(&self) -> bool {
        matches!(self, PredicateKind::Unexpressible)
    }
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
    // ── Abnormal result ───────────────────────────────────────────────────────
    // Targets the boolean is_abnormal column (Flag role).  Do NOT add "critical"
    // here — criticality is a distinct clinical concept with its own column and
    // role.  The discriminating pair is dx-01 ("critical") vs dx-04 ("abnormal").
    DomainPredicate {
        phrases: &["abnormal result", "abnormal"],
        concept: EntityConcept::LabResult,
        role: ColumnRole::Flag,
        name_hint: Some("abnormal"),
        kind: PredicateKind::IsTrue,
    },
    // ── Critical result ───────────────────────────────────────────────────────
    // Targets the boolean is_critical column (Criticality role).  Kept separate
    // from the abnormality predicate above so "critical" → is_critical and
    // "abnormal" → is_abnormal never cross.  If the binding has no Criticality
    // column, bind() refuses — it must NOT fall back to is_abnormal.
    DomainPredicate {
        phrases: &["critical result", "critical"],
        concept: EntityConcept::LabResult,
        role: ColumnRole::Criticality,
        name_hint: Some("critical"),
        kind: PredicateKind::IsTrue,
    },
    // ── No-show / DNA / missed appointment ───────────────────────────────────
    DomainPredicate {
        phrases: &["no-show", "no show", "dna", "did not attend", "missed"],
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
    // ── Allergy / allergic to ─────────────────────────────────────────────────
    //
    // "What is this patient allergic to?" — grammatical subject is Patient but the
    // question asks about Allergy records.  Unexpressible: cross-entity join
    // required (§0.6 forbids reverse-FK traversal) → NoParse → semantic path.
    DomainPredicate {
        phrases: &["allergic to", "allergic"],
        concept: EntityConcept::Allergy,
        role: ColumnRole::Status,
        name_hint: None,
        kind: PredicateKind::Unexpressible,
    },
    // ── Emergency department attendance ──────────────────────────────────────
    //
    // "How many patients arrived in the emergency last month?" — subject is Patient
    // but the measure is an ED triage/attendance event (Triage).  Unexpressible:
    // requires cross-entity count that the IR cannot express.
    DomainPredicate {
        phrases: &["arrived in the emergency", "in the emergency", "emergency department", "emergency room", "ed attendance", "ed arrival"],
        concept: EntityConcept::Triage,
        role: ColumnRole::EventTime,
        name_hint: None,
        kind: PredicateKind::Unexpressible,
    },
    // ── Cohort: record access ─────────────────────────────────────────────────
    //
    // "Who has accessed this record?" — "record" alone resolves to Document, but
    // the question is about AccessLog.  Unexpressible: concept mismatch forces
    // NoParse on R8 cross-concept check.
    //
    // "Show me all record access logs today" — "access logs" phrase is also
    // Unexpressible: the AccessLog list projection is too sparse for a useful
    // answer (requires cross-entity names from Patient/Provider that §0.6
    // reverse-FK traversal does not permit).
    DomainPredicate {
        phrases: &["accessed", "access logs", "access log"],
        concept: EntityConcept::AccessLog,
        role: ColumnRole::EventTime,
        name_hint: None,
        kind: PredicateKind::Unexpressible,
    },
    // ── Cohort: TB treatment ──────────────────────────────────────────────────
    //
    // "How many patients are on TB treatment?" — subject is Patient but the
    // cohort predicate binds to ProgramEnrollment.  Unexpressible: requires
    // cross-entity join that §0.6 does not permit.
    DomainPredicate {
        phrases: &["tb treatment", "tuberculosis treatment", "on treatment for tb"],
        concept: EntityConcept::ProgramEnrollment,
        role: ColumnRole::Status,
        name_hint: None,
        kind: PredicateKind::Unexpressible,
    },
    // ── Cohort: HIV program ───────────────────────────────────────────────────
    DomainPredicate {
        phrases: &["hiv program", "hiv programme", "enrolled in the hiv"],
        concept: EntityConcept::ProgramEnrollment,
        role: ColumnRole::Status,
        name_hint: None,
        kind: PredicateKind::Unexpressible,
    },
    // ── Cohort: chronic care enrollment ──────────────────────────────────────
    DomainPredicate {
        phrases: &["chronic care", "enrolled in chronic"],
        concept: EntityConcept::ProgramEnrollment,
        role: ColumnRole::Status,
        name_hint: None,
        kind: PredicateKind::Unexpressible,
    },
    // ── Cohort: diabetes ──────────────────────────────────────────────────────
    //
    // "How many patients have type 2 diabetes?" — subject is Patient but the
    // cohort filter binds to a Diagnosis record.  Unexpressible: requires
    // cross-entity join that §0.6 does not permit.
    DomainPredicate {
        phrases: &["type 2 diabetes", "type ii diabetes", "diabetes"],
        concept: EntityConcept::Diagnosis,
        role: ColumnRole::Code,
        name_hint: None,
        kind: PredicateKind::Unexpressible,
    },
    // ── Caesarean delivery ────────────────────────────────────────────────────
    DomainPredicate {
        phrases: &["caesarean", "c-section", "c section", "lscs"],
        concept: EntityConcept::Delivery,
        role: ColumnRole::Type,
        name_hint: Some("mode"),
        kind: PredicateKind::In(&["caesarean", "c_section", "lscs", "cs"]),
    },
    // ── Refused prescription ─────────────────────────────────────────────────
    //
    // "Which prescriptions were refused?" — in the dev schema 'refused' only exists
    // in medication_administration.status (admin_status ENUM), not in
    // prescriptions.status CHECK ('active','dispensed',...).  Concept is set to
    // MedicationAdministration so R8's cross-concept check refuses when the
    // grammatical subject is Prescription → NoParse → semantic fallback.
    DomainPredicate {
        phrases: &["prescriptions were refused", "prescription refused", "refused prescription"],
        concept: EntityConcept::MedicationAdministration,
        role: ColumnRole::Status,
        name_hint: None,
        kind: PredicateKind::In(&["refused"]),
    },
    // ── Night shift / tonight ─────────────────────────────────────────────────
    //
    // "Who is on shift tonight?" — "tonight" scopes the time range to today but
    // also implies the night shift type.  This predicate adds shift_type = 'night'
    // so the query is not an unconstrained list of all shifts today.
    DomainPredicate {
        phrases: &["tonight", "night shift"],
        concept: EntityConcept::Shift,
        role: ColumnRole::Type,
        name_hint: None,
        kind: PredicateKind::In(&["night"]),
    },
    // ── Rejected insurance claim ──────────────────────────────────────────────
    DomainPredicate {
        phrases: &["claims were rejected", "claim rejected", "claims rejected", "rejected claim"],
        concept: EntityConcept::Claim,
        role: ColumnRole::Status,
        name_hint: None,
        kind: PredicateKind::In(&["rejected", "denied"]),
    },
    // ── Unreviewed lab result ─────────────────────────────────────────────────
    //
    // "Which lab results are critical and unreviewed?" — applies alongside the
    // "critical/abnormal" predicate; uses ForeignRef role + hint so that
    // `verified_by` (an unrefined FK column) is matched by name.
    //
    // Alt LabRes has no verified_by or similar reviewer column; its only
    // ForeignRef is req_id (FK to LabReq, the lab-request table).  Because the
    // hint "verified" is a *requirement* (not a preference), the binder returns
    // None for alt LabRes — the parse refuses and falls through to the semantic
    // path rather than silently binding req_id IS NULL, which would answer
    // "this result has no associated lab request" — a completely different
    // clinical proposition.
    DomainPredicate {
        phrases: &["unreviewed", "not reviewed", "without review"],
        concept: EntityConcept::LabResult,
        role: ColumnRole::ForeignRef,
        name_hint: Some("verified"),
        kind: PredicateKind::IsNull,
    },
    // ── Free / available bed ─────────────────────────────────────────────────
    DomainPredicate {
        phrases: &["beds are free", "free beds", "bed is free", "beds available", "available beds"],
        concept: EntityConcept::Bed,
        role: ColumnRole::Status,
        name_hint: None,
        kind: PredicateKind::In(&["available", "free", "vacant"]),
    },
    // ── Surgery: performed (no status column on surgeries) ────────────────────
    //
    // "How many surgeries were performed this month?" — the dev schema's
    // surgeries table tracks only urgency/priority, not a completion status.
    // Unexpressible so the planner falls back to the semantic path.
    DomainPredicate {
        phrases: &["surgeries were performed", "operations were performed", "procedures were performed"],
        concept: EntityConcept::Surgery,
        role: ColumnRole::Status,
        name_hint: None,
        kind: PredicateKind::Unexpressible,
    },
    // ── Surgery: cancelled (no status column on surgeries) ────────────────────
    //
    // "How many operations were cancelled?" — same reasoning as "performed".
    DomainPredicate {
        phrases: &["operations were cancelled", "surgeries were cancelled", "procedures were cancelled"],
        concept: EntityConcept::Surgery,
        role: ColumnRole::Status,
        name_hint: None,
        kind: PredicateKind::Unexpressible,
    },
    // ── Equipment overdue for servicing ───────────────────────────────────────
    //
    // "What equipment is overdue for servicing?" — servicing history lives in
    // equipment_maintenance (separate table), reachable only via a reverse-FK
    // join that §0.6 does not permit.  Unexpressible.
    DomainPredicate {
        phrases: &["overdue for servicing", "overdue for maintenance", "due for servicing", "due for maintenance"],
        concept: EntityConcept::Equipment,
        role: ColumnRole::EventTime,
        name_hint: None,
        kind: PredicateKind::Unexpressible,
    },
    // ── Antenatal visit due ───────────────────────────────────────────────────
    //
    // "Who is due for an antenatal visit this week?" — AntenatalVisit records
    // capture past visits; there is no scheduled/due-date column on the
    // antenatal_visits table that expresses future due dates.  Unexpressible.
    DomainPredicate {
        phrases: &["due for an antenatal", "due for antenatal visit"],
        concept: EntityConcept::AntenatalVisit,
        role: ColumnRole::EventTime,
        name_hint: None,
        kind: PredicateKind::Unexpressible,
    },
];

/// Look up a domain predicate by phrase. Returns the first match.
/// The question must be pre-normalized (lowercase, whitespace-collapsed).
pub fn lookup_predicate(normalized_question: &str) -> Option<&'static DomainPredicate> {
    DOMAIN_PREDICATES
        .iter()
        .find(|p| p.phrases.iter().any(|phrase| normalized_question.contains(phrase)))
}

/// Look up ALL matching domain predicates for a question.
///
/// Parse rules that need to carry conjunctive constraints (e.g. "critical AND
/// unreviewed") must use this instead of `lookup_predicate` so that every
/// matched predicate is either applied or causes a NoParse refusal.
///
/// Callers MUST:
/// 1. Check `pred.kind.is_unexpressible()` — return `None` if any predicate is unexpressible.
/// 2. Check `pred.concept == subject_concept || subject_concept == Unknown` for every
///    predicate — return `None` (cross-concept) if any fails the check.
/// 3. Call `predicate_to_filter` only for predicates that passed both checks.
pub fn lookup_all_predicates(normalized_question: &str) -> Vec<&'static DomainPredicate> {
    DOMAIN_PREDICATES
        .iter()
        .filter(|p| p.phrases.iter().any(|phrase| normalized_question.contains(phrase)))
        .collect()
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
        PredicateKind::Unexpressible => {
            // Callers must check `is_unexpressible()` before calling this function.
            // Reaching here is a programming error — the parse rule did not guard correctly.
            unreachable!(
                "predicate_to_filter called on Unexpressible predicate {:?}; \
                 caller must check is_unexpressible() and return None first",
                pred.phrases
            );
        }
    };
    Filter { column, op, value }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── Regression: tautology guard ──────────────────────────────────────────
    // Each of these statements previously emitted wrong SQL because the
    // predicate's Unexpressible kind was not guarded.  They must all be
    // Unexpressible so parse rules return None immediately.

    #[test]
    fn tb_treatment_predicate_is_unexpressible() {
        let pred = lookup_predicate("how many patients are on tb treatment").unwrap();
        assert!(
            pred.kind.is_unexpressible(),
            "tb treatment predicate must be Unexpressible to prevent tautology filter"
        );
    }

    #[test]
    fn hiv_program_predicate_is_unexpressible() {
        let pred = lookup_predicate("which patients are enrolled in the hiv program").unwrap();
        assert!(
            pred.kind.is_unexpressible(),
            "hiv program predicate must be Unexpressible"
        );
    }

    #[test]
    fn chronic_care_predicate_is_unexpressible() {
        let pred = lookup_predicate("how many patients have been enrolled in chronic care programs").unwrap();
        assert!(
            pred.kind.is_unexpressible(),
            "chronic care predicate must be Unexpressible"
        );
    }

    #[test]
    fn surgery_performed_is_unexpressible() {
        let pred = lookup_predicate("how many surgeries were performed this month").unwrap();
        assert!(
            pred.kind.is_unexpressible(),
            "surgery performed predicate must be Unexpressible (no status column on surgeries)"
        );
    }

    #[test]
    fn surgery_cancelled_is_unexpressible() {
        let pred = lookup_predicate("how many operations were cancelled").unwrap();
        assert!(
            pred.kind.is_unexpressible(),
            "surgery cancelled predicate must be Unexpressible (no status column on surgeries)"
        );
    }

    #[test]
    fn antenatal_due_is_unexpressible() {
        let pred = lookup_predicate("who is due for an antenatal visit this week").unwrap();
        assert!(
            pred.kind.is_unexpressible(),
            "antenatal due predicate must be Unexpressible (no scheduled due-date column)"
        );
    }

    #[test]
    fn access_logs_is_unexpressible() {
        let pred = lookup_predicate("show me all record access logs today").unwrap();
        assert!(
            pred.kind.is_unexpressible(),
            "access logs predicate must be Unexpressible (projection too sparse)"
        );
    }

    #[test]
    fn overdue_servicing_is_unexpressible() {
        let pred = lookup_predicate("what equipment is overdue for servicing").unwrap();
        assert!(
            pred.kind.is_unexpressible(),
            "overdue for servicing predicate must be Unexpressible (requires reverse-FK join)"
        );
    }

    #[test]
    fn lookup_all_returns_both_critical_and_unreviewed() {
        let preds = lookup_all_predicates("which lab results are critical and unreviewed");
        let has_critical = preds.iter().any(|p| p.phrases.contains(&"critical"));
        let has_unreviewed = preds.iter().any(|p| p.phrases.contains(&"unreviewed"));
        assert!(has_critical, "critical predicate must be found");
        assert!(has_unreviewed, "unreviewed predicate must be found");
    }

    #[test]
    fn missed_phrase_matches_appointments() {
        let pred = lookup_predicate("how many appointments were missed last month").unwrap();
        assert_eq!(pred.concept, EntityConcept::Appointment);
        assert!(
            matches!(pred.kind, PredicateKind::In(_)),
            "no-show predicate kind must be In"
        );
    }

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

    // ── ph-05 regression: refused prescription → cross-concept refusal ────────
    //
    // "refused" is valid only in medication_administration.status (admin_status
    // ENUM); prescriptions.status has no such value.  The predicate concept must
    // be MedicationAdministration so R8's cross-concept check refuses when the
    // grammatical subject is Prescription.
    #[test]
    fn refused_prescription_predicate_targets_medication_administration() {
        let pred = lookup_predicate("which prescriptions were refused last month").unwrap();
        assert_eq!(
            pred.concept,
            EntityConcept::MedicationAdministration,
            "refused-prescription predicate must target MedicationAdministration (not Prescription)"
        );
        if let PredicateKind::In(vals) = pred.kind {
            assert!(
                vals.contains(&"refused"),
                "In list must contain 'refused'"
            );
            assert!(
                !vals.contains(&"declined"),
                "'declined' is not a valid admin_status value and must not be in the list"
            );
        } else {
            panic!("refused-prescription predicate kind must be In");
        }
    }

    // ── wf-02 regression: tonight predicate adds night shift filter ───────────
    //
    // "Who is on shift tonight?" — a new domain predicate must match "tonight"
    // and target Shift.Type with In(&["night"]) so the compiled SQL includes
    // a shift_type filter.
    #[test]
    fn tonight_predicate_targets_shift_type() {
        let pred = lookup_predicate("who is on shift tonight").unwrap();
        assert_eq!(
            pred.concept,
            EntityConcept::Shift,
            "tonight predicate must target Shift"
        );
        assert_eq!(
            pred.role,
            ColumnRole::Type,
            "tonight predicate role must be Type (matches shift_type column)"
        );
        if let PredicateKind::In(vals) = pred.kind {
            assert_eq!(vals, &["night"], "tonight predicate must filter to night shift only");
        } else {
            panic!("tonight predicate kind must be In");
        }
    }

    #[test]
    fn night_shift_phrase_also_matches_predicate() {
        let pred = lookup_predicate("who works the night shift").unwrap();
        assert_eq!(pred.concept, EntityConcept::Shift);
    }
}
