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
    /// When `true` the predicate is satisfied by the ABSENCE of matching child
    /// rows, i.e. the `RelatedScope` it creates must use NOT EXISTS rather than
    /// EXISTS.  Set this on any predicate that means "no <child> where <cond>":
    /// e.g. "out of stock" = no stock_batch where quantity_on_hand > 0.
    pub negated_scope: bool,
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
        negated_scope: false,
    },
    // ── Stillbirth ────────────────────────────────────────────────────────────
    DomainPredicate {
        phrases: &["stillbirth", "stillborn", "still birth"],
        concept: EntityConcept::Delivery,
        role: ColumnRole::Status,
        name_hint: None,
        kind: PredicateKind::In(&["stillbirth", "stillborn"]),
        negated_scope: false,
    },
    // ── Triage category 1 / red ───────────────────────────────────────────────
    DomainPredicate {
        phrases: &["category 1", "cat 1", "cat1", "red triage", "triage 1"],
        concept: EntityConcept::Triage,
        role: ColumnRole::Priority,
        name_hint: Some("category"),
        kind: PredicateKind::In(&["1", "red", "category_1"]),
        negated_scope: false,
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
        negated_scope: false,
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
        negated_scope: false,
    },
    // ── No-show / DNA / missed appointment ───────────────────────────────────
    DomainPredicate {
        phrases: &["no-show", "no show", "dna", "did not attend", "missed"],
        concept: EntityConcept::Appointment,
        role: ColumnRole::Status,
        name_hint: None,
        kind: PredicateKind::In(&["no_show", "dna", "did_not_attend"]),
        negated_scope: false,
    },
    // ── Outstanding / unpaid bill ─────────────────────────────────────────────
    DomainPredicate {
        phrases: &["outstanding", "unpaid", "overdue payment", "balance due"],
        concept: EntityConcept::Bill,
        role: ColumnRole::Amount,
        name_hint: Some("due"),
        kind: PredicateKind::Gt(0.0),
        negated_scope: false,
    },
    // ── Readmission ───────────────────────────────────────────────────────────
    DomainPredicate {
        phrases: &["readmission", "readmitted"],
        concept: EntityConcept::Admission,
        role: ColumnRole::Flag,
        name_hint: Some("readmission"),
        kind: PredicateKind::IsTrue,
        negated_scope: false,
    },
    // ── Out of stock ──────────────────────────────────────────────────────────
    // "Which medicines are out of stock?" (ph-01)
    //
    // A medicine is out of stock when there are NO stock_batch rows with
    // quantity_on_hand > 0.  The correct SQL is:
    //   WHERE NOT EXISTS (SELECT 1 FROM stock_batches WHERE ... AND quantity_on_hand > 0)
    // This requires:
    //   • kind = Gt(0.0)  — the child filter is "quantity_on_hand > 0"
    //   • negated_scope = true — the RelatedScope becomes NOT EXISTS
    DomainPredicate {
        phrases: &["out of stock", "stockout", "stock out", "zero stock"],
        concept: EntityConcept::StockBatch,
        role: ColumnRole::Quantity,
        name_hint: Some("on_hand"),
        kind: PredicateKind::Gt(0.0),
        negated_scope: true,
    },
    // ── Lost to follow-up ─────────────────────────────────────────────────────
    DomainPredicate {
        phrases: &["lost to follow-up", "ltfu", "lost to followup", "defaulted"],
        concept: EntityConcept::ProgramEnrollment,
        role: ColumnRole::Status,
        name_hint: None,
        kind: PredicateKind::In(&["lost_to_followup", "ltfu", "defaulted"]),
        negated_scope: false,
    },
    // ── Break-glass ───────────────────────────────────────────────────────────
    DomainPredicate {
        phrases: &["break-glass", "break glass", "emergency access", "override access"],
        concept: EntityConcept::AccessLog,
        role: ColumnRole::Flag,
        name_hint: Some("break_glass"),
        kind: PredicateKind::IsTrue,
        negated_scope: false,
    },
    // ── In charge ─────────────────────────────────────────────────────────────
    DomainPredicate {
        phrases: &["in charge", "nurse in charge", "shift leader"],
        concept: EntityConcept::Shift,
        role: ColumnRole::Flag,
        name_hint: Some("in_charge"),
        kind: PredicateKind::IsTrue,
        negated_scope: false,
    },
    // ── Pending certification (mortality) ─────────────────────────────────────
    DomainPredicate {
        phrases: &["pending certification", "uncertified death", "no certificate"],
        concept: EntityConcept::Mortality,
        role: ColumnRole::Identifier,
        name_hint: Some("certificate"),
        kind: PredicateKind::IsNull,
        negated_scope: false,
    },
    // ── Postpartum haemorrhage ────────────────────────────────────────────────
    DomainPredicate {
        phrases: &["postpartum haemorrhage", "pph", "postpartum hemorrhage"],
        concept: EntityConcept::Delivery,
        role: ColumnRole::Flag,
        name_hint: Some("pph"),
        kind: PredicateKind::IsTrue,
        negated_scope: false,
    },
    // ── Equipment out of service ──────────────────────────────────────────────
    DomainPredicate {
        phrases: &["out of service", "faulty", "broken", "under repair"],
        concept: EntityConcept::Equipment,
        role: ColumnRole::Status,
        name_hint: None,
        kind: PredicateKind::In(&["out_of_service", "faulty", "broken"]),
        negated_scope: false,
    },
    // ── Cases without consent ─────────────────────────────────────────────────
    DomainPredicate {
        phrases: &["no consent", "without consent", "missing consent", "no consent recorded"],
        concept: EntityConcept::Consent,
        role: ColumnRole::PrimaryKey, // used to detect absence via NOT EXISTS
        name_hint: None,
        kind: PredicateKind::IsNull, // signals anti-join in compiler
        negated_scope: false,
    },
    // ── Allergy / allergic to ─────────────────────────────────────────────────
    //
    // "What is this patient allergic to?" (pc-03)
    //
    // Comment corrected 2026-09-06.  The previous reason — "§0.6 forbids reverse-FK
    // traversal" — has not held since plan 03f amended §0.6 to permit a scoped
    // single reverse hop, and Patient ← patient_allergies is exactly that shape.
    //
    // Two reasons that ARE true today keep this Unexpressible:
    //   1. The allergen's identity is not on the child table.  `patient_allergies`
    //      carries `allergen_id INTEGER NOT NULL REFERENCES allergen_catalog(id)`
    //      (01_schema.sql:267), so answering "allergic to *what*" needs
    //      Patient ← patient_allergies → allergen_catalog: a reverse hop followed
    //      by a forward hop, which 03f §2.1 refuses outright ("no reverse edge
    //      followed by a forward edge; two hops refuse").  The Status role bound
    //      here would reach `severity` (01_schema.sql:269) — how bad the reaction
    //      is, not what caused it.
    //   2. "this patient" supplies no identifier, so the anchor row cannot be
    //      scoped at all; 03b §1 requires the spec to express everything the
    //      question stated.
    DomainPredicate {
        phrases: &["allergic to", "allergic"],
        concept: EntityConcept::Allergy,
        role: ColumnRole::Status,
        name_hint: None,
        kind: PredicateKind::Unexpressible,
        negated_scope: false,
    },
    // ── Emergency department attendance ──────────────────────────────────────
    //
    // "How many patients arrived in the emergency last month?" (em-01)
    //
    // Comment corrected 2026-09-06.  The previous reason — "requires cross-entity
    // count that the IR cannot express" — is no longer true: `triage_assessments`
    // carries `patient_id UUID NOT NULL REFERENCES patients(id)`
    // (01_schema.sql:374), so this is one scoped reverse hop, which 03f permits,
    // and no forward hop is needed.
    //
    // The reason that IS true today is EventTime ambiguity on the child table.
    // `triage_assessments` declares three timestamps in sequence —
    // `arrival_time` (01_schema.sql:376), `triage_time` (:377) and `seen_time`
    // (:378) — and this predicate carries role EventTime with no `name_hint`, so
    // nothing selects between them.  Picking the first-declared column is exactly
    // the false blessing that dx-02 and qs-05 were withdrawn for.  A second,
    // independent ambiguity sits on the measure: "how many patients" and "how many
    // ED attendances" differ for any patient who attended twice in the month, and
    // the question does not say which it wants.  03b §1 says refuse.
    DomainPredicate {
        phrases: &["arrived in the emergency", "in the emergency", "emergency department", "emergency room", "ed attendance", "ed arrival"],
        concept: EntityConcept::Triage,
        role: ColumnRole::EventTime,
        name_hint: None,
        kind: PredicateKind::Unexpressible,
        negated_scope: false,
    },
    // ── Cohort: record access ─────────────────────────────────────────────────
    //
    // Comment corrected 2026-09-06.  The two phrase groups here refuse for two
    // different reasons, and only the first is a real blocker.
    //
    // "Who has accessed this record?" (pc-04) — real blocker: "this record"
    // supplies no identifier.  `record_access_log` has both `patient_id` and
    // `encounter_id` (01_schema.sql:1116, :1118), so there is not even a single
    // candidate anchor to scope to, let alone a literal to scope with.  A spec
    // that drops the demonstrative lists every access in the hospital — 03b §1
    // requires refusal.  (The old reason, "§0.6 reverse-FK traversal does not
    // permit cross-entity names", was wrong on its own terms: the provider's name
    // is reached by a *forward* FK, `provider_id INTEGER NOT NULL REFERENCES
    // providers(id)` at 01_schema.sql:1117, which §0.6 always allowed.)
    //
    // "Show me all record access logs today" (qs-04) — NO schema blocker exists.
    // The question is fully scoped by "today", `accessed_at TIMESTAMPTZ NOT NULL`
    // (01_schema.sql:1119) is unambiguous, and the row shape needs no join.  The
    // refusal rests only on the judgement that a list of raw FK integers is not a
    // useful answer, which is a readability preference, not a correctness
    // constraint.  Recorded here as an entry awaiting a ruling rather than
    // dressed up as a schema limitation.
    DomainPredicate {
        phrases: &["accessed", "access logs", "access log"],
        concept: EntityConcept::AccessLog,
        role: ColumnRole::EventTime,
        name_hint: None,
        kind: PredicateKind::Unexpressible,
        negated_scope: false,
    },
    // ── Cohort: care-programme enrolment (TB / HIV / chronic care) ────────────
    //
    // Comment corrected 2026-09-06 — this reason covers the next three entries,
    // which previously carried a superseded reason (TB) or no reason at all
    // (HIV, chronic care).
    //
    // The old reason, "requires cross-entity join that §0.6 does not permit", has
    // not held since 03f amended §0.6: Patient ← program_enrollments is a single
    // scoped reverse hop and is permitted.
    //
    // The reason that IS true today: the programme's identity is not on the child
    // table.  `program_enrollments` names its programme only by FK —
    // `program_id SMALLINT NOT NULL REFERENCES care_programs(id)`
    // (01_schema.sql:1023) — and the human-readable label lives one hop further
    // out, in `care_programs.name VARCHAR(100) NOT NULL` (01_schema.sql:1012).
    // Distinguishing "TB" from "HIV" from "chronic care" therefore needs
    // Patient ← program_enrollments → care_programs: a reverse hop followed by a
    // forward hop, which 03f §2.1 refuses ("no reverse edge followed by a forward
    // edge; two hops refuse").
    //
    // The Status role these three entries carry cannot substitute:
    // `program_enrollments.status` (01_schema.sql:1027) has the closed domain
    // ('active','transferred_out','lost_to_followup','completed','stopped','died')
    // — enrolment lifecycle, containing no programme name.  Binding it would
    // answer "is enrolled in *something*", silently dropping the condition that
    // makes each of the three questions different from the other two.
    //
    // Unrelated ProgramEnrollment ruling, recorded here so it is not lost: cc-03
    // "Who is due for review this week?" refuses for now, and matches no predicate
    // in this table (it refuses upstream).  One reverse hop and a plain DATE
    // column are both fine, but
    // `CREATE INDEX idx_enrollments_due ON program_enrollments(next_review_date)
    // WHERE status = 'active'` (01_schema.sql:1176) exposes an ambiguity the
    // question never resolves — a patient with status='died' and a future review
    // date.  Both readings differ materially, so 03b §1 says refuse.
    DomainPredicate {
        phrases: &["tb treatment", "tuberculosis treatment", "on treatment for tb"],
        concept: EntityConcept::ProgramEnrollment,
        role: ColumnRole::Status,
        name_hint: None,
        kind: PredicateKind::Unexpressible,
        negated_scope: false,
    },
    // ── Cohort: HIV program ───────────────────────────────────────────────────
    //
    // "Which patients are enrolled in the HIV program?" (cc-02).  Reason: see the
    // care-programme enrolment block above — the programme name lives in
    // `care_programs.name` (01_schema.sql:1012), reachable only by a reverse hop
    // followed by a forward hop, which 03f §2.1 refuses.  This entry previously
    // stated no reason at all.
    DomainPredicate {
        phrases: &["hiv program", "hiv programme", "enrolled in the hiv"],
        concept: EntityConcept::ProgramEnrollment,
        role: ColumnRole::Status,
        name_hint: None,
        kind: PredicateKind::Unexpressible,
        negated_scope: false,
    },
    // ── Cohort: chronic care enrollment ──────────────────────────────────────
    //
    // "How many patients have been enrolled in chronic care programs?" (cc-04).
    // Reason: see the care-programme enrolment block above — same two-hop blocker.
    // This entry previously stated no reason at all.  Note that "chronic" is also
    // a property of a *condition* rather than a programme
    // (`patient_medical_history.is_chronic`, 01_schema.sql:284), so even the
    // anchor concept is unresolved by the question's wording.
    DomainPredicate {
        phrases: &["chronic care", "enrolled in chronic"],
        concept: EntityConcept::ProgramEnrollment,
        role: ColumnRole::Status,
        name_hint: None,
        kind: PredicateKind::Unexpressible,
        negated_scope: false,
    },
    // ── Cohort: diabetes ──────────────────────────────────────────────────────
    //
    // "How many patients have type 2 diabetes?" (gen-03)
    //
    // Comment corrected 2026-09-06; the old reason ("§0.6 does not permit the
    // cross-entity join") was superseded by 03f, which permits the single reverse
    // hop Patient ← diagnoses.  Ruling by the project owner, recorded as given:
    //
    //   gen-03 refuses permanently: `icd10_code` (schema line 417) is
    //   `VARCHAR(10) NOT NULL REFERENCES icd10_codes(code)` — an FK into a
    //   reference table, so an open domain with no derived label set.  Mapping
    //   "type 2 diabetes" → `E11*` asserts a terminology fact that lives in data,
    //   not schema.  The `diagnosis_desc` free-text alternative would wrongly
    //   capture "type 1 diabetes", "family history of diabetes", and "diabetes
    //   insipidus".
    DomainPredicate {
        phrases: &["type 2 diabetes", "type ii diabetes", "diabetes"],
        concept: EntityConcept::Diagnosis,
        role: ColumnRole::Code,
        name_hint: None,
        kind: PredicateKind::Unexpressible,
        negated_scope: false,
    },
    // ── Caesarean delivery ────────────────────────────────────────────────────
    DomainPredicate {
        phrases: &["caesarean", "c-section", "c section", "lscs"],
        concept: EntityConcept::Delivery,
        role: ColumnRole::Type,
        name_hint: Some("mode"),
        kind: PredicateKind::In(&["caesarean", "c_section", "lscs", "cs"]),
        negated_scope: false,
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
        negated_scope: false,
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
        negated_scope: false,
    },
    // ── Rejected insurance claim ──────────────────────────────────────────────
    DomainPredicate {
        phrases: &["claims were rejected", "claim rejected", "claims rejected", "rejected claim"],
        concept: EntityConcept::Claim,
        role: ColumnRole::Status,
        name_hint: None,
        kind: PredicateKind::In(&["rejected", "denied"]),
        negated_scope: false,
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
        negated_scope: false,
    },
    // ── Free / available bed ─────────────────────────────────────────────────
    DomainPredicate {
        phrases: &["beds are free", "free beds", "bed is free", "beds available", "available beds"],
        concept: EntityConcept::Bed,
        role: ColumnRole::Status,
        name_hint: None,
        kind: PredicateKind::In(&["available", "free", "vacant"]),
        negated_scope: false,
    },
    // ── Surgery: performed ────────────────────────────────────────────────────
    //
    // "How many surgeries were performed this month?" (th-01)
    //
    // Comment corrected 2026-09-06.  The old reason — "the surgeries table tracks
    // only urgency/priority, not a completion status" — was simply false:
    // `surgeries.status VARCHAR(20) NOT NULL DEFAULT 'completed' CHECK (status IN
    // ('scheduled','in_theatre','completed','cancelled','postponed'))` exists at
    // 01_schema.sql:557.
    //
    // There is NO schema blocker here.  What blocks it is that "performed" does
    // not name one label in that domain, and the readings differ materially:
    // 'completed' alone excludes an operation still 'in_theatre' at period end,
    // while `actual_start IS NOT NULL` (01_schema.sql:550) is a third reading that
    // counts every operation that actually reached theatre including those later
    // abandoned.  A month's count differs under each.  03b §1 says refuse when the
    // question does not resolve the choice.
    //
    // Contrast the "cancelled" entry below, which Kevin moved off Unexpressible on
    // 2026-09-06: 'cancelled' is a single label in the same CHECK domain, so it has
    // exactly one reading.  This entry stays Unexpressible pending a ruling on what
    // "performed" means; it is recorded as awaiting a ruling, not as a schema limit.
    DomainPredicate {
        phrases: &["surgeries were performed", "operations were performed", "procedures were performed"],
        concept: EntityConcept::Surgery,
        role: ColumnRole::Status,
        name_hint: None,
        kind: PredicateKind::Unexpressible,
        negated_scope: false,
    },
    // ── Surgery: cancelled ────────────────────────────────────────────────────
    //
    // "How many operations were cancelled?" (th-02)
    //
    // Was `Unexpressible` with the comment "same reasoning as performed", which
    // rested on the false claim that `surgeries` has no status column.  Changed to
    // `In(&["cancelled"])` on 2026-09-06 by the project owner's ruling: the column
    // is `surgeries.status VARCHAR(20) NOT NULL DEFAULT 'completed' CHECK (status
    // IN ('scheduled','in_theatre','completed','cancelled','postponed'))` at
    // 01_schema.sql:557, so 'cancelled' is a real label in a closed domain and
    // maps to the question one-for-one — no ambiguity to resolve, unlike
    // "performed" above.
    //
    // The literal comes from the CHECK constraint's own domain, which matters:
    // on a Postgres ENUM a nonexistent label is a runtime error, but on
    // `VARCHAR ... CHECK` it is silently false and returns an empty result
    // indistinguishable from a truthful "none".  `bind::apply_in_domain_narrowing`
    // plus the 03h §3 invariant in `ir/mod.rs` both hold this literal to that
    // domain.
    DomainPredicate {
        phrases: &["operations were cancelled", "surgeries were cancelled", "procedures were cancelled"],
        concept: EntityConcept::Surgery,
        role: ColumnRole::Status,
        name_hint: None,
        kind: PredicateKind::In(&["cancelled"]),
        negated_scope: false,
    },
    // ── Equipment overdue for servicing ───────────────────────────────────────
    //
    // "What equipment is overdue for servicing?" (fac-02)
    //
    // Comment corrected 2026-09-06.  The old reason was wrong twice over: it cited
    // §0.6's reverse-FK ban, which 03f superseded, and it claimed the answer needs
    // `equipment_maintenance` (01_schema.sql:1058) when in fact no join is needed
    // at all — `equipment.next_service_date DATE` sits on the anchor table itself
    // (01_schema.sql:1054), one column away, and `equipment.last_service_date`
    // (:1053) beside it.
    //
    // There is NO schema blocker here.  `next_service_date < now` is a plain
    // forward-column threshold.  What stops it today is mechanical: this entry
    // carries role EventTime with no `name_hint`, and an EventTime slot selects a
    // column recording when something *happened*, not a future due date — the same
    // classification wall that makes fd-04 miss on `scheduled_start`.  Recorded as
    // an entry awaiting a ruling (does "overdue" mean `next_service_date` in the
    // past, and against which clock?), not as a schema limitation.
    DomainPredicate {
        phrases: &["overdue for servicing", "overdue for maintenance", "due for servicing", "due for maintenance"],
        concept: EntityConcept::Equipment,
        role: ColumnRole::EventTime,
        name_hint: None,
        kind: PredicateKind::Unexpressible,
        negated_scope: false,
    },
    // ── Antenatal visit due ───────────────────────────────────────────────────
    //
    // "Who is due for an antenatal visit this week?" (mat-05)
    //
    // Comment corrected 2026-09-06.  The old reason — "there is no scheduled/
    // due-date column on the antenatal_visits table" — was false:
    // `antenatal_visits.next_visit_date DATE` exists at 01_schema.sql:794.
    //
    // The reason that IS true today is a temporal superlative.  `antenatal_visits`
    // has no uniqueness on `patient_id` and carries `visit_number SMALLINT NOT
    // NULL` (01_schema.sql:781) precisely because a pregnancy accumulates many
    // rows, each with its own `next_visit_date`.  Only the *latest* visit's date is
    // the one in force; every earlier row's date is a superseded appointment that
    // may also fall in this week.  Filtering `next_visit_date` across all rows
    // therefore over-reports.  Most-recent-per-patient needs a window function or
    // a correlated MAX, which the IR cannot express — the identical defect that
    // withdrew gen-04's blessing ("missed their *last* appointment").
    DomainPredicate {
        phrases: &["due for an antenatal", "due for antenatal visit"],
        concept: EntityConcept::AntenatalVisit,
        role: ColumnRole::EventTime,
        name_hint: None,
        kind: PredicateKind::Unexpressible,
        negated_scope: false,
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
            "tb treatment predicate must be Unexpressible: the programme name lives in \
             care_programs.name (01_schema.sql:1012), reachable only by a reverse hop then a \
             forward hop, which 03f §2.1 refuses. program_enrollments.status (:1027) holds \
             enrolment lifecycle, not programme identity, so binding it would silently drop \
             the condition that distinguishes TB from HIV from chronic care."
        );
    }

    #[test]
    fn hiv_program_predicate_is_unexpressible() {
        let pred = lookup_predicate("which patients are enrolled in the hiv program").unwrap();
        assert!(
            pred.kind.is_unexpressible(),
            "hiv program predicate must be Unexpressible: same two-hop blocker as TB \
             (care_programs.name, 01_schema.sql:1012)"
        );
    }

    #[test]
    fn chronic_care_predicate_is_unexpressible() {
        let pred = lookup_predicate("how many patients have been enrolled in chronic care programs").unwrap();
        assert!(
            pred.kind.is_unexpressible(),
            "chronic care predicate must be Unexpressible: same two-hop blocker as TB \
             (care_programs.name, 01_schema.sql:1012)"
        );
    }

    #[test]
    fn surgery_performed_is_unexpressible() {
        let pred = lookup_predicate("how many surgeries were performed this month").unwrap();
        assert!(
            pred.kind.is_unexpressible(),
            "surgery performed predicate must be Unexpressible. NOTE the reason: it is NOT \
             a missing column — surgeries.status exists (01_schema.sql:557). 'performed' names \
             no single label in ('scheduled','in_theatre','completed','cancelled','postponed'), \
             and the 'completed' / 'completed+in_theatre' / actual_start-IS-NOT-NULL readings \
             give materially different counts, so 03b §1 requires refusal."
        );
    }

    /// th-02 — "How many operations were cancelled?" must now compile to real SQL.
    ///
    /// Replaces `surgery_cancelled_is_unexpressible`, which pinned a refusal that
    /// rested on a false premise ("no status column on surgeries").  The column is
    /// `surgeries.status VARCHAR(20) NOT NULL DEFAULT 'completed' CHECK (status IN
    /// ('scheduled','in_theatre','completed','cancelled','postponed'))` at
    /// docker/dev-postgres/init/01_schema.sql:557, so 'cancelled' is a real label in
    /// a closed domain with exactly one reading.
    ///
    /// This asserts the emitted statement in all three dialects, not merely that
    /// something compiled: 52 wrong-SQL rows once passed a test that only checked
    /// `sql_pg.is_some()`.  Every statement goes through
    /// `nl2sql::validate::validate_sql` first, as the plans require.
    ///
    /// The expected text is derived from the sibling closed-domain count row `wb-01`
    /// ("How many beds are free?" → `... WHERE t0."status" IN ('available') LIMIT
    /// 500`), not captured from this pipeline's output.
    #[test]
    fn surgery_cancelled_compiles_to_status_in_cancelled() {
        use crate::connectors::SourceKind;
        use crate::nl2sql::ir::{bind, compile, parse, ParseOutcome};
        use crate::nl2sql::validate::validate_sql;
        use crate::ontology::binder::bind_cards;
        use crate::ontology::binding::SchemaBinding;
        use crate::ontology::tests::{dev_seed_cards, dev_seed_enum_values};
        use chrono::{TimeZone, Utc};

        // Same frozen clock, confidence gate and row cap as the golden suite, so a
        // divergence here is a real difference and not a harness artefact.
        let now = Utc.with_ymd_and_hms(2026, 1, 15, 8, 0, 0).unwrap();
        let cards = dev_seed_cards();
        let enum_values = dev_seed_enum_values();
        let binding = SchemaBinding {
            source_id: "dev".to_string(),
            bound_at: now,
            tables: bind_cards(&cards, 0.55, 3, None, None, &enum_values),
            degraded: false,
            override_version: 0,
        };
        let allowed: Vec<String> = cards.iter().map(|c| c.table_name.clone()).collect();
        let question = "How many operations were cancelled?";

        // The predicate itself must no longer be Unexpressible.
        let pred = lookup_predicate("how many operations were cancelled").unwrap();
        assert!(
            !pred.kind.is_unexpressible(),
            "surgery cancelled predicate must be expressible: surgeries.status exists \
             (01_schema.sql:557) and 'cancelled' is one of its CHECK labels"
        );
        assert!(
            matches!(pred.kind, PredicateKind::In(labels) if labels == ["cancelled"]),
            "surgery cancelled must filter status IN ('cancelled') — the literal has to \
             come from the CHECK domain, because on VARCHAR ... CHECK a label that does \
             not exist is silently false and returns an empty result indistinguishable \
             from a truthful 'none'"
        );

        let spec = match parse(question, None, &binding, &allowed) {
            ParseOutcome::Parsed { spec, .. } => spec,
            ParseOutcome::NoParse => panic!("th-02 must parse: {question}"),
        };
        let bound = bind(spec, &binding, &cards, &allowed, question)
            .expect("th-02 must bind against the dev fixture");

        for (dialect, expected) in [
            (
                SourceKind::Postgres,
                "SELECT COUNT(*) AS \"count\" FROM \"surgeries\" AS t0 \
                 WHERE t0.\"status\" IN ('cancelled') LIMIT 500",
            ),
            (
                SourceKind::Mysql,
                "SELECT COUNT(*) AS `count` FROM `surgeries` AS t0 \
                 WHERE t0.`status` IN ('cancelled') LIMIT 500",
            ),
            (
                SourceKind::Mssql,
                "SELECT TOP 500 COUNT(*) AS [count] FROM [surgeries] AS t0 \
                 WHERE t0.[status] IN ('cancelled')",
            ),
        ] {
            let compiled = compile(&bound, dialect, 500, now)
                .unwrap_or_else(|e| panic!("th-02 must compile for {dialect:?}: {e:?}"));
            let validated = validate_sql(&compiled.sql, dialect, 500, &allowed)
                .unwrap_or_else(|e| panic!("th-02 SQL must validate for {dialect:?}: {e:?}"));
            assert_eq!(validated.sql, expected, "th-02 {dialect:?} SQL");
        }
    }

    #[test]
    fn antenatal_due_is_unexpressible() {
        let pred = lookup_predicate("who is due for an antenatal visit this week").unwrap();
        assert!(
            pred.kind.is_unexpressible(),
            "antenatal due predicate must be Unexpressible. NOTE the reason: the due-date \
             column DOES exist (antenatal_visits.next_visit_date, 01_schema.sql:794). The \
             blocker is the temporal superlative — many visits per patient, only the latest \
             next_visit_date is in force, and most-recent-per-group needs a window function \
             the IR cannot express (same defect as gen-04)."
        );
    }

    #[test]
    fn access_logs_is_unexpressible() {
        let pred = lookup_predicate("show me all record access logs today").unwrap();
        assert!(
            pred.kind.is_unexpressible(),
            "access logs predicate must be Unexpressible. NOTE the reason: there is no \
             schema blocker for qs-04 — record_access_log.accessed_at (01_schema.sql:1119) \
             scopes 'today' unambiguously and no join is needed. The refusal is a readability \
             judgement awaiting a ruling. The sibling phrase 'accessed' (pc-04) does have a \
             real blocker: 'this record' supplies no identifier."
        );
    }

    #[test]
    fn overdue_servicing_is_unexpressible() {
        let pred = lookup_predicate("what equipment is overdue for servicing").unwrap();
        assert!(
            pred.kind.is_unexpressible(),
            "overdue for servicing predicate must be Unexpressible. NOTE the reason: it does \
             NOT require a reverse-FK join — equipment.next_service_date is on the anchor table \
             (01_schema.sql:1054). The blocker is that this entry's EventTime role selects a \
             column recording when something happened, not a future due date; the question \
             also does not say what 'overdue' compares against. Awaiting a ruling."
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
