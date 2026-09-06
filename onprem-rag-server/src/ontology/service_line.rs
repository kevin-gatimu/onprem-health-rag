//! Fixed hospital service-line ontology.
//!
//! The 13 service lines are a constant; they never change per deployment.
//! Which tables a deployment provides for each service line is determined at
//! catalog time by `SchemaBinding` (binder.rs).
//!
//! **Invariant**: `ServiceLine::ALL` must list all 13 variants in tier order
//! (Tier 1, then Tier 2, then Tier 3). A test asserts the length and that every
//! `slug()` is unique.

use serde::{Deserialize, Serialize};

use crate::ontology::concepts::EntityConcept;

/// A hospital service line — a department or function a real hospital staffs,
/// budgets, and reports on separately.
///
/// 13 fixed lines; no variant may be added or removed without updating `ALL`
/// and the fixture JSON in `tests/`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ServiceLine {
    // Tier 1 — daily operational workhorses
    PatientChart,
    FrontDesk,
    WardBoard,
    Pharmacy,
    Diagnostics,
    Emergency,
    // Tier 2 — distinct audience
    Maternity,
    Theatre,
    Revenue,
    // Tier 3 — periodic / management-facing
    QualitySafety,
    Workforce,
    ChronicCare,
    Facilities,
}

impl ServiceLine {
    /// All 13 service lines in tier order (Tier 1 first, then 2, then 3).
    ///
    /// **Must stay in sync** with the variant list above. A test asserts the
    /// length and unique slugs catch any drift.
    pub const ALL: &'static [ServiceLine] = &[
        ServiceLine::PatientChart,
        ServiceLine::FrontDesk,
        ServiceLine::WardBoard,
        ServiceLine::Pharmacy,
        ServiceLine::Diagnostics,
        ServiceLine::Emergency,
        ServiceLine::Maternity,
        ServiceLine::Theatre,
        ServiceLine::Revenue,
        ServiceLine::QualitySafety,
        ServiceLine::Workforce,
        ServiceLine::ChronicCare,
        ServiceLine::Facilities,
    ];

    /// Human-readable label (shown in the UI tab).
    pub fn label(self) -> &'static str {
        match self {
            ServiceLine::PatientChart => "Patient Chart",
            ServiceLine::FrontDesk => "Front Desk",
            ServiceLine::WardBoard => "Ward Board",
            ServiceLine::Emergency => "Emergency",
            ServiceLine::Maternity => "Maternity",
            ServiceLine::Theatre => "Theatre",
            ServiceLine::Pharmacy => "Pharmacy",
            ServiceLine::Diagnostics => "Diagnostics",
            ServiceLine::Revenue => "Revenue",
            ServiceLine::QualitySafety => "Quality & Safety",
            ServiceLine::Workforce => "Workforce",
            ServiceLine::ChronicCare => "Chronic Care",
            ServiceLine::Facilities => "Facilities",
        }
    }

    /// Snake-case identifier used in API responses and routing.
    pub fn slug(self) -> &'static str {
        match self {
            ServiceLine::PatientChart => "patient_chart",
            ServiceLine::FrontDesk => "front_desk",
            ServiceLine::WardBoard => "ward_board",
            ServiceLine::Emergency => "emergency",
            ServiceLine::Maternity => "maternity",
            ServiceLine::Theatre => "theatre",
            ServiceLine::Pharmacy => "pharmacy",
            ServiceLine::Diagnostics => "diagnostics",
            ServiceLine::Revenue => "revenue",
            ServiceLine::QualitySafety => "quality_safety",
            ServiceLine::Workforce => "workforce",
            ServiceLine::ChronicCare => "chronic_care",
            ServiceLine::Facilities => "facilities",
        }
    }

    /// One-sentence description surfaced in the UI and in model personas.
    pub fn blurb(self) -> &'static str {
        match self {
            ServiceLine::PatientChart =>
                "Everything about one patient: history, notes, vitals, allergies, immunizations and documents.",
            ServiceLine::FrontDesk =>
                "Bookings, clinics, referrals and insurance cover — the front-of-house workflow.",
            ServiceLine::WardBoard =>
                "Who is admitted, where they are, bed state, drug rounds and shift handover.",
            ServiceLine::Emergency =>
                "The front door: triage, acuity, waiting times and emergency admissions.",
            ServiceLine::Maternity =>
                "Antenatal care through delivery to the newborn, including obstetric outcomes.",
            ServiceLine::Theatre =>
                "The operating list, utilisation, consent, and procedural outcomes.",
            ServiceLine::Pharmacy =>
                "Prescribing, dispensing, stock management and medicines safety.",
            ServiceLine::Diagnostics =>
                "Lab orders, results, imaging requests and blood bank inventory.",
            ServiceLine::Revenue =>
                "Bills, claims, collections and insurer reconciliation.",
            ServiceLine::QualitySafety =>
                "Incidents, mortality, notifications, audit trails and safety alerts.",
            ServiceLine::Workforce =>
                "Staffing, licences, rotas, leave and workload by provider.",
            ServiceLine::ChronicCare =>
                "Longitudinal disease-register care: chronic condition enrolments and follow-up.",
            ServiceLine::Facilities =>
                "Equipment availability, maintenance schedules and bed estate.",
        }
    }

    /// Rollout tier: 1 = daily operational, 2 = distinct audience, 3 = periodic.
    pub fn tier(self) -> u8 {
        match self {
            ServiceLine::PatientChart
            | ServiceLine::FrontDesk
            | ServiceLine::WardBoard
            | ServiceLine::Pharmacy
            | ServiceLine::Diagnostics
            | ServiceLine::Emergency => 1,
            ServiceLine::Maternity
            | ServiceLine::Theatre
            | ServiceLine::Revenue => 2,
            ServiceLine::QualitySafety
            | ServiceLine::Workforce
            | ServiceLine::ChronicCare
            | ServiceLine::Facilities => 3,
        }
    }

    /// Entity concepts owned by this service line (§4 of hospital-agents-and-data-map.md).
    ///
    /// Ownership means: "this agent may read tables bound to these concepts and is
    /// prompted about them." It is NOT exclusivity — see `SHARED_CONCEPTS` and the
    /// multi-owner concepts below.
    pub fn concepts(self) -> &'static [EntityConcept] {
        use EntityConcept::*;
        match self {
            ServiceLine::PatientChart => &[
                Diagnosis, ClinicalNote, VitalSign, Allergy, MedicalHistory,
                FamilyHistory, Immunization, Document, Geography,
            ],
            ServiceLine::FrontDesk => &[
                Appointment, ProviderSchedule, ProviderTimeOff, Referral,
                InsurancePolicy, Insurer,
            ],
            ServiceLine::WardBoard => &[
                Admission, BedAssignment, Bed, Ward, Transfer, Shift,
                MedicationAdministration, VitalSign,
            ],
            ServiceLine::Emergency => &[
                Triage, Encounter, VitalSign, Admission, Referral,
            ],
            ServiceLine::Maternity => &[
                AntenatalVisit, Delivery, Newborn, Admission, ProgramEnrollment,
            ],
            ServiceLine::Theatre => &[
                Surgery, Procedure, ProcedureCode, Consent, Equipment,
            ],
            ServiceLine::Pharmacy => &[
                Prescription, PrescriptionItem, Medication, MedicationAdministration,
                StockBatch, StockMovement, DrugInteraction, Allergen, CdsAlert, Allergy,
            ],
            ServiceLine::Diagnostics => &[
                LabOrder, LabResult, LabTest, ImagingOrder, BloodUnit, Transfusion,
            ],
            ServiceLine::Revenue => &[
                Bill, BillItem, Claim, Payment, InsurancePolicy, Insurer,
            ],
            ServiceLine::QualitySafety => &[
                Incident, Mortality, Feedback, NotifiableDisease, AccessLog,
                CdsAlert, DiagnosisCode,
            ],
            ServiceLine::Workforce => &[
                Provider, License, ProviderSchedule, ProviderTimeOff, Shift, Department,
            ],
            ServiceLine::ChronicCare => &[
                CareProgram, ProgramEnrollment, Vaccine, Immunization,
            ],
            ServiceLine::Facilities => &[
                Equipment, EquipmentMaintenance, Ward, Bed,
            ],
        }
    }

    /// Domain vocabulary for router and persona grounding.
    pub fn vocabulary(self) -> &'static [&'static str] {
        match self {
            ServiceLine::PatientChart => &[
                "patient", "history", "note", "allergy", "vital", "immunization", "document",
                "diagnosis", "clinical", "chronic", "summary",
            ],
            ServiceLine::FrontDesk => &[
                "appointment", "booking", "schedule", "referral", "insurance", "clinic",
                "no-show", "wait", "slot", "cover",
            ],
            ServiceLine::WardBoard => &[
                "ward", "bed", "admission", "discharge", "transfer", "occupancy", "round",
                "shift", "handover", "medication", "inpatient", "ipd",
            ],
            ServiceLine::Emergency => &[
                "emergency", "triage", "casualty", "a&e", "er", "acuity", "arrival",
                "category", "waiting", "ambulance",
            ],
            ServiceLine::Maternity => &[
                "delivery", "birth", "maternity", "antenatal", "obstetric", "caesarean",
                "postnatal", "newborn", "labour", "midwife",
            ],
            ServiceLine::Theatre => &[
                "surgery", "theatre", "operating", "procedure", "anaesthesia", "consent",
                "list", "or", "case", "ot",
            ],
            ServiceLine::Pharmacy => &[
                "prescription", "dispensing", "stock", "medication", "drug", "pharmacy",
                "interaction", "formulary", "expiry", "rx",
            ],
            ServiceLine::Diagnostics => &[
                "lab", "laboratory", "result", "imaging", "radiology", "blood", "culture",
                "order", "critical", "turnaround",
            ],
            ServiceLine::Revenue => &[
                "bill", "invoice", "payment", "claim", "insurance", "nhif", "shif",
                "revenue", "collection", "outstanding",
            ],
            ServiceLine::QualitySafety => &[
                "incident", "mortality", "audit", "notification", "feedback", "safety",
                "quality", "break-glass", "alert", "mortality",
            ],
            ServiceLine::Workforce => &[
                "staff", "provider", "leave", "licence", "rota", "on-call", "shifts",
                "workforce", "doctor", "nurse",
            ],
            ServiceLine::ChronicCare => &[
                "chronic", "register", "hiv", "tb", "diabetes", "hypertension", "follow-up",
                "enrolment", "programme", "defaulter",
            ],
            ServiceLine::Facilities => &[
                "equipment", "maintenance", "service", "downtime", "inventory", "bed",
                "ward", "asset", "repair",
            ],
        }
    }

    /// Example questions for the `GET /agents` roster. Each question is tagged with
    /// the entity concepts it requires so `GET /agents` can filter to bound concepts.
    ///
    /// Returns pairs of (question, required_concepts).
    pub fn examples(self) -> &'static [(&'static str, &'static [EntityConcept])] {
        use EntityConcept::*;
        match self {
            ServiceLine::PatientChart => &[
                ("What diagnoses does this patient have?", &[Diagnosis, Patient]),
                ("What is this patient allergic to?", &[Allergy, Patient]),
                ("Show me the latest vital signs for this patient.", &[VitalSign, Patient]),
                ("What medications has this patient been prescribed?", &[Prescription, Patient]),
                ("Who has accessed this patient's record?", &[AccessLog, Patient]),
            ],
            ServiceLine::FrontDesk => &[
                ("Who is booked with Dr Otieno on Thursday?", &[Appointment, Provider]),
                ("Which referrals are still pending?", &[Referral]),
                ("What is our no-show rate this month?", &[Appointment]),
                ("Which clinics run on Tuesday afternoon?", &[ProviderSchedule]),
                ("How long do patients wait before being seen?", &[Appointment, Encounter]),
            ],
            ServiceLine::WardBoard => &[
                ("How many beds are free in ICU?", &[Bed, BedAssignment]),
                ("Who is admitted right now?", &[Admission, Patient]),
                ("Which patients have been in over 14 days?", &[Admission]),
                ("Which doses were missed on the night shift?", &[MedicationAdministration, Shift]),
                ("Who was in charge of Medical Ward B last night?", &[Shift, Ward]),
            ],
            ServiceLine::Emergency => &[
                ("How many category 1 patients arrived last month?", &[Triage]),
                ("What is our door-to-doctor time?", &[Triage, Encounter]),
                ("How many arrived by ambulance?", &[Triage]),
                ("How many emergency admissions were there last week?", &[Admission, Triage]),
            ],
            ServiceLine::Maternity => &[
                ("How many deliveries last month, and how many were caesarean?", &[Delivery]),
                ("What is our stillbirth rate?", &[Delivery, Newborn]),
                ("Which mothers had a postpartum haemorrhage?", &[Delivery]),
                ("How many babies were low birth weight?", &[Newborn]),
                ("Who is due for an antenatal visit?", &[AntenatalVisit, Patient]),
            ],
            ServiceLine::Theatre => &[
                ("What is on tomorrow's operating list?", &[Surgery]),
                ("How many operations were cancelled, and why?", &[Surgery]),
                ("What is our average theatre time for a caesarean?", &[Surgery, Procedure]),
                ("Which cases had no consent recorded?", &[Consent, Surgery]),
            ],
            ServiceLine::Pharmacy => &[
                ("Which medicines are out of stock?", &[StockBatch, StockMovement]),
                ("What expires within 30 days?", &[StockBatch]),
                ("Which patients are on an interacting combination?", &[DrugInteraction, Prescription]),
                ("How much amoxicillin did we dispense last quarter?", &[Prescription, Medication]),
            ],
            ServiceLine::Diagnostics => &[
                ("Which results are critical and unreviewed?", &[LabResult]),
                ("What is our lab turnaround time?", &[LabOrder, LabResult]),
                ("How many units of O-negative do we have?", &[BloodUnit]),
                ("Were there any transfusion reactions?", &[Transfusion]),
            ],
            ServiceLine::Revenue => &[
                ("What is outstanding by insurer?", &[Bill, Claim, Insurer]),
                ("Which claims were rejected, and why?", &[Claim]),
                ("What did we collect last month?", &[Payment]),
                ("What is our average bill by department?", &[Bill, Department]),
            ],
            ServiceLine::QualitySafety => &[
                ("How many falls this quarter?", &[Incident]),
                ("What is our inpatient mortality rate?", &[Mortality]),
                ("Which TB cases have not been notified?", &[NotifiableDisease]),
                ("Show me break-glass record access.", &[AccessLog]),
            ],
            ServiceLine::Workforce => &[
                ("Whose practising licence expires this quarter?", &[License]),
                ("Who is on call tonight?", &[Shift, Provider]),
                ("Which doctor saw the most patients last month?", &[Encounter, Provider]),
                ("How much leave is booked for December?", &[ProviderTimeOff]),
            ],
            ServiceLine::ChronicCare => &[
                ("Who has been lost to follow-up in the HIV clinic?", &[ProgramEnrollment, CareProgram]),
                ("Who is due for review this week?", &[ProgramEnrollment]),
                ("How many patients are on TB treatment?", &[ProgramEnrollment, CareProgram]),
                ("Which vaccines are due this month?", &[Vaccine, Immunization]),
            ],
            ServiceLine::Facilities => &[
                ("Which equipment is out of service?", &[Equipment]),
                ("What is overdue for servicing?", &[EquipmentMaintenance]),
                ("How much downtime did we have on the ventilators?", &[Equipment, EquipmentMaintenance]),
            ],
        }
    }

    /// Parse a slug string back to a ServiceLine variant.
    pub fn from_slug(slug: &str) -> Option<Self> {
        ServiceLine::ALL.iter().copied().find(|l| l.slug() == slug)
    }

    /// Return the service line(s) that own a concept (may be multiple or none).
    pub fn owner_of(concept: EntityConcept) -> &'static [ServiceLine] {
        // Map each concept to its owning lines, constructed from the §4 table.
        // For SHARED_CONCEPTS the answer is "all lines" but the fn caller decides
        // whether to use that or go through SHARED_CONCEPTS directly.
        use EntityConcept::*;
        match concept {
            // Shared concepts — readable by every line (handled separately via SHARED_CONCEPTS)
            Patient | Encounter | Provider | Department | DiagnosisCode => &[],
            // Patient Chart
            Diagnosis      => &[ServiceLine::PatientChart],
            ClinicalNote   => &[ServiceLine::PatientChart],
            Geography      => &[ServiceLine::PatientChart],
            MedicalHistory => &[ServiceLine::PatientChart],
            FamilyHistory  => &[ServiceLine::PatientChart],
            Document       => &[ServiceLine::PatientChart],
            // Multi-owner
            VitalSign      => &[ServiceLine::PatientChart, ServiceLine::WardBoard, ServiceLine::Emergency],
            Allergy        => &[ServiceLine::PatientChart, ServiceLine::Pharmacy],
            Immunization   => &[ServiceLine::PatientChart, ServiceLine::ChronicCare],
            // Front Desk
            Appointment    => &[ServiceLine::FrontDesk],
            Insurer        => &[ServiceLine::FrontDesk, ServiceLine::Revenue],
            InsurancePolicy => &[ServiceLine::FrontDesk, ServiceLine::Revenue],
            Referral       => &[ServiceLine::FrontDesk, ServiceLine::Emergency],
            ProviderSchedule => &[ServiceLine::FrontDesk, ServiceLine::Workforce],
            ProviderTimeOff  => &[ServiceLine::FrontDesk, ServiceLine::Workforce],
            // Ward Board
            Admission        => &[ServiceLine::WardBoard, ServiceLine::Emergency, ServiceLine::Maternity],
            BedAssignment    => &[ServiceLine::WardBoard],
            Bed              => &[ServiceLine::WardBoard, ServiceLine::Facilities],
            Ward             => &[ServiceLine::WardBoard, ServiceLine::Facilities],
            Transfer         => &[ServiceLine::WardBoard],
            Shift            => &[ServiceLine::WardBoard, ServiceLine::Workforce],
            MedicationAdministration => &[ServiceLine::WardBoard, ServiceLine::Pharmacy],
            // Emergency
            Triage           => &[ServiceLine::Emergency],
            // Maternity
            AntenatalVisit   => &[ServiceLine::Maternity],
            Delivery         => &[ServiceLine::Maternity],
            Newborn          => &[ServiceLine::Maternity],
            ProgramEnrollment => &[ServiceLine::Maternity, ServiceLine::ChronicCare],
            // Theatre
            Surgery          => &[ServiceLine::Theatre],
            Procedure        => &[ServiceLine::Theatre],
            ProcedureCode    => &[ServiceLine::Theatre],
            Consent          => &[ServiceLine::Theatre],
            Equipment        => &[ServiceLine::Theatre, ServiceLine::Facilities],
            // Pharmacy
            Prescription     => &[ServiceLine::Pharmacy],
            PrescriptionItem => &[ServiceLine::Pharmacy],
            Medication       => &[ServiceLine::Pharmacy],
            StockBatch       => &[ServiceLine::Pharmacy],
            StockMovement    => &[ServiceLine::Pharmacy],
            DrugInteraction  => &[ServiceLine::Pharmacy],
            Allergen         => &[ServiceLine::Pharmacy],
            CdsAlert         => &[ServiceLine::Pharmacy, ServiceLine::QualitySafety],
            // Diagnostics
            LabOrder         => &[ServiceLine::Diagnostics],
            LabResult        => &[ServiceLine::Diagnostics],
            LabTest          => &[ServiceLine::Diagnostics],
            ImagingOrder     => &[ServiceLine::Diagnostics],
            BloodUnit        => &[ServiceLine::Diagnostics],
            Transfusion      => &[ServiceLine::Diagnostics],
            // Revenue
            Bill             => &[ServiceLine::Revenue],
            BillItem         => &[ServiceLine::Revenue],
            Claim            => &[ServiceLine::Revenue],
            Payment          => &[ServiceLine::Revenue],
            // Quality & Safety
            Incident         => &[ServiceLine::QualitySafety],
            Mortality        => &[ServiceLine::QualitySafety],
            Feedback         => &[ServiceLine::QualitySafety],
            NotifiableDisease => &[ServiceLine::QualitySafety],
            AccessLog        => &[ServiceLine::QualitySafety],
            // Workforce
            License          => &[ServiceLine::Workforce],
            // Chronic Care
            CareProgram      => &[ServiceLine::ChronicCare],
            Vaccine          => &[ServiceLine::ChronicCare],
            // Facilities
            EquipmentMaintenance => &[ServiceLine::Facilities],
            // Fallback
            Unknown          => &[],
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    #[test]
    fn all_count_and_unique_slugs() {
        assert_eq!(
            ServiceLine::ALL.len(),
            13,
            "ServiceLine::ALL must have exactly 13 entries"
        );
        let slugs: HashSet<&str> = ServiceLine::ALL.iter().map(|l| l.slug()).collect();
        assert_eq!(
            slugs.len(),
            13,
            "every ServiceLine slug must be unique; duplicate found"
        );
    }
}
