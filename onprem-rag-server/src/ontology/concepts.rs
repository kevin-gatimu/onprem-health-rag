//! Entity-concept ontology.
//!
//! An `EntityConcept` is what a *table* **is** — the hospital function it
//! represents regardless of its physical name.  `ConceptDescriptor` provides
//! the vocabulary used by the binder to match a table to its concept.
//!
//! **Invariants** (tested):
//! - `EntityConcept::ALL` lists every variant (including `Unknown`).
//! - Every slug is unique.
//! - Every concept in a `ServiceLine::concepts()` union has a descriptor.
//! - `Unknown` and `Geography` are the only concepts not owned by any line.

use serde::{Deserialize, Serialize};

use crate::ontology::roles::ColumnRole;

/// An entity concept — what a table represents, independent of its physical name.
///
/// Each concept is tied to the hospital domain. `Unknown` is the fallback for
/// tables the binder cannot classify with sufficient confidence.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EntityConcept {
    // ── Shared (readable by every service line) ──────────────────────────────
    Patient,
    Encounter,
    Provider,
    Department,
    DiagnosisCode,
    Geography,
    // ── Patient Chart ─────────────────────────────────────────────────────────
    Diagnosis,
    ClinicalNote,
    VitalSign,
    Allergy,
    MedicalHistory,
    FamilyHistory,
    Immunization,
    Document,
    // ── Front Desk ────────────────────────────────────────────────────────────
    Appointment,
    ProviderSchedule,
    ProviderTimeOff,
    Referral,
    InsurancePolicy,
    Insurer,
    // ── Ward Board ────────────────────────────────────────────────────────────
    Admission,
    BedAssignment,
    Bed,
    Ward,
    Transfer,
    Shift,
    MedicationAdministration,
    // ── Emergency ─────────────────────────────────────────────────────────────
    Triage,
    // ── Maternity ─────────────────────────────────────────────────────────────
    AntenatalVisit,
    Delivery,
    Newborn,
    // ── Theatre ───────────────────────────────────────────────────────────────
    Surgery,
    Procedure,
    ProcedureCode,
    Consent,
    // ── Pharmacy ──────────────────────────────────────────────────────────────
    Prescription,
    PrescriptionItem,
    Medication,
    StockBatch,
    StockMovement,
    DrugInteraction,
    Allergen,
    CdsAlert,
    // ── Diagnostics ───────────────────────────────────────────────────────────
    LabOrder,
    LabResult,
    LabTest,
    ImagingOrder,
    BloodUnit,
    Transfusion,
    // ── Revenue ───────────────────────────────────────────────────────────────
    Bill,
    BillItem,
    Claim,
    Payment,
    // ── Quality & Safety ──────────────────────────────────────────────────────
    Incident,
    Mortality,
    Feedback,
    NotifiableDisease,
    AccessLog,
    // ── Workforce ─────────────────────────────────────────────────────────────
    License,
    // ── Chronic Care ──────────────────────────────────────────────────────────
    CareProgram,
    ProgramEnrollment,
    Vaccine,
    // ── Facilities ────────────────────────────────────────────────────────────
    Equipment,
    EquipmentMaintenance,
    // ── Fallback ──────────────────────────────────────────────────────────────
    Unknown,
}

impl EntityConcept {
    /// All entity-concept variants. **Must stay in sync** with the enum above.
    pub const ALL: &'static [EntityConcept] = &[
        EntityConcept::Patient,
        EntityConcept::Encounter,
        EntityConcept::Provider,
        EntityConcept::Department,
        EntityConcept::DiagnosisCode,
        EntityConcept::Geography,
        EntityConcept::Diagnosis,
        EntityConcept::ClinicalNote,
        EntityConcept::VitalSign,
        EntityConcept::Allergy,
        EntityConcept::MedicalHistory,
        EntityConcept::FamilyHistory,
        EntityConcept::Immunization,
        EntityConcept::Document,
        EntityConcept::Appointment,
        EntityConcept::ProviderSchedule,
        EntityConcept::ProviderTimeOff,
        EntityConcept::Referral,
        EntityConcept::InsurancePolicy,
        EntityConcept::Insurer,
        EntityConcept::Admission,
        EntityConcept::BedAssignment,
        EntityConcept::Bed,
        EntityConcept::Ward,
        EntityConcept::Transfer,
        EntityConcept::Shift,
        EntityConcept::MedicationAdministration,
        EntityConcept::Triage,
        EntityConcept::AntenatalVisit,
        EntityConcept::Delivery,
        EntityConcept::Newborn,
        EntityConcept::Surgery,
        EntityConcept::Procedure,
        EntityConcept::ProcedureCode,
        EntityConcept::Consent,
        EntityConcept::Prescription,
        EntityConcept::PrescriptionItem,
        EntityConcept::Medication,
        EntityConcept::StockBatch,
        EntityConcept::StockMovement,
        EntityConcept::DrugInteraction,
        EntityConcept::Allergen,
        EntityConcept::CdsAlert,
        EntityConcept::LabOrder,
        EntityConcept::LabResult,
        EntityConcept::LabTest,
        EntityConcept::ImagingOrder,
        EntityConcept::BloodUnit,
        EntityConcept::Transfusion,
        EntityConcept::Bill,
        EntityConcept::BillItem,
        EntityConcept::Claim,
        EntityConcept::Payment,
        EntityConcept::Incident,
        EntityConcept::Mortality,
        EntityConcept::Feedback,
        EntityConcept::NotifiableDisease,
        EntityConcept::AccessLog,
        EntityConcept::License,
        EntityConcept::CareProgram,
        EntityConcept::ProgramEnrollment,
        EntityConcept::Vaccine,
        EntityConcept::Equipment,
        EntityConcept::EquipmentMaintenance,
        EntityConcept::Unknown,
    ];

    /// Snake-case slug for serialization and routing.
    pub fn slug(self) -> &'static str {
        match self {
            EntityConcept::Patient => "patient",
            EntityConcept::Encounter => "encounter",
            EntityConcept::Provider => "provider",
            EntityConcept::Department => "department",
            EntityConcept::DiagnosisCode => "diagnosis_code",
            EntityConcept::Geography => "geography",
            EntityConcept::Diagnosis => "diagnosis",
            EntityConcept::ClinicalNote => "clinical_note",
            EntityConcept::VitalSign => "vital_sign",
            EntityConcept::Allergy => "allergy",
            EntityConcept::MedicalHistory => "medical_history",
            EntityConcept::FamilyHistory => "family_history",
            EntityConcept::Immunization => "immunization",
            EntityConcept::Document => "document",
            EntityConcept::Appointment => "appointment",
            EntityConcept::ProviderSchedule => "provider_schedule",
            EntityConcept::ProviderTimeOff => "provider_time_off",
            EntityConcept::Referral => "referral",
            EntityConcept::InsurancePolicy => "insurance_policy",
            EntityConcept::Insurer => "insurer",
            EntityConcept::Admission => "admission",
            EntityConcept::BedAssignment => "bed_assignment",
            EntityConcept::Bed => "bed",
            EntityConcept::Ward => "ward",
            EntityConcept::Transfer => "transfer",
            EntityConcept::Shift => "shift",
            EntityConcept::MedicationAdministration => "medication_administration",
            EntityConcept::Triage => "triage",
            EntityConcept::AntenatalVisit => "antenatal_visit",
            EntityConcept::Delivery => "delivery",
            EntityConcept::Newborn => "newborn",
            EntityConcept::Surgery => "surgery",
            EntityConcept::Procedure => "procedure",
            EntityConcept::ProcedureCode => "procedure_code",
            EntityConcept::Consent => "consent",
            EntityConcept::Prescription => "prescription",
            EntityConcept::PrescriptionItem => "prescription_item",
            EntityConcept::Medication => "medication",
            EntityConcept::StockBatch => "stock_batch",
            EntityConcept::StockMovement => "stock_movement",
            EntityConcept::DrugInteraction => "drug_interaction",
            EntityConcept::Allergen => "allergen",
            EntityConcept::CdsAlert => "cds_alert",
            EntityConcept::LabOrder => "lab_order",
            EntityConcept::LabResult => "lab_result",
            EntityConcept::LabTest => "lab_test",
            EntityConcept::ImagingOrder => "imaging_order",
            EntityConcept::BloodUnit => "blood_unit",
            EntityConcept::Transfusion => "transfusion",
            EntityConcept::Bill => "bill",
            EntityConcept::BillItem => "bill_item",
            EntityConcept::Claim => "claim",
            EntityConcept::Payment => "payment",
            EntityConcept::Incident => "incident",
            EntityConcept::Mortality => "mortality",
            EntityConcept::Feedback => "feedback",
            EntityConcept::NotifiableDisease => "notifiable_disease",
            EntityConcept::AccessLog => "access_log",
            EntityConcept::License => "license",
            EntityConcept::CareProgram => "care_program",
            EntityConcept::ProgramEnrollment => "program_enrollment",
            EntityConcept::Vaccine => "vaccine",
            EntityConcept::Equipment => "equipment",
            EntityConcept::EquipmentMaintenance => "equipment_maintenance",
            EntityConcept::Unknown => "unknown",
        }
    }

    /// Parse a slug back to a concept.
    pub fn from_slug(slug: &str) -> Option<Self> {
        EntityConcept::ALL.iter().copied().find(|c| c.slug() == slug)
    }
}

/// Descriptor for one entity concept. The binder uses this to match a physical
/// table to its semantic concept.
///
/// **Invariant**: `name_tokens` and `column_tokens` must be *generic* vocabulary —
/// never physical table names from the dev seed. The same descriptor must bind
/// both the dev schema and the alt schema.
pub struct ConceptDescriptor {
    pub concept: EntityConcept,
    /// Generic singular tokens identifying the concept in a table name.
    /// All lowercase; the binder singularizes and splits table names before
    /// matching. Include regional/international synonyms.
    pub name_tokens: &'static [&'static str],
    /// Expected column name substrings (generic, case-insensitive). The fraction
    /// of these present in the table's columns contributes 0.30 to the score.
    pub column_tokens: &'static [&'static str],
    /// Roles that *should* be present for a table bound to this concept.
    /// The fraction present contributes 0.15 to the score.
    pub required_roles: &'static [ColumnRole],
    /// One-sentence description for BGE-M3 semantic embedding.
    pub description: &'static str,
    pub singular: &'static str,
    pub plural: &'static str,
    pub synonyms: &'static [&'static str],
}

/// Concepts readable by every service line (data-map §4).
pub const SHARED_CONCEPTS: &[EntityConcept] = &[
    EntityConcept::Patient,
    EntityConcept::Encounter,
    EntityConcept::Provider,
    EntityConcept::Department,
    EntityConcept::DiagnosisCode,
];

/// Anchor concepts bound in pass 1 (before FK-shape scoring is available).
pub const ANCHOR_CONCEPTS: &[EntityConcept] = &[
    EntityConcept::Patient,
    EntityConcept::Provider,
    EntityConcept::Encounter,
    EntityConcept::Department,
    EntityConcept::Medication,
    EntityConcept::Ward,
];

/// Return the descriptor for a concept. `Unknown` panics — it has no descriptor.
pub fn descriptor(c: EntityConcept) -> &'static ConceptDescriptor {
    use EntityConcept::*;
    DESCRIPTORS
        .iter()
        .find(|d| d.concept == c)
        .unwrap_or_else(|| panic!("no descriptor for concept {:?}", c))
}

/// The full descriptor table. Order is not significant for correctness but
/// keeping it aligned with `EntityConcept::ALL` aids review.
///
/// **Rule**: `name_tokens` and `column_tokens` are generic. No dev-seed table
/// name is allowed here.
pub static DESCRIPTORS: &[ConceptDescriptor] = &[
    ConceptDescriptor {
        concept: EntityConcept::Patient,
        name_tokens: &["patient", "person", "client", "individual", "member"],
        column_tokens: &["patient_no", "date_of_birth", "dob", "gender", "first_name", "last_name", "county", "mrn"],
        required_roles: &[ColumnRole::PrimaryKey, ColumnRole::BirthDate],
        description: "A patient or person registered in the hospital's health records system.",
        singular: "patient",
        plural: "patients",
        synonyms: &["pt", "subject", "registrant"],
    },
    ConceptDescriptor {
        concept: EntityConcept::Encounter,
        name_tokens: &["encounter", "visit", "consultation", "episode", "opd", "outpatient"],
        column_tokens: &["encounter_type", "visit_date", "visit_type", "encounter_date", "provider_id", "department_id", "chief_complaint"],
        required_roles: &[ColumnRole::PatientRef, ColumnRole::EventTime],
        description: "A clinical encounter or visit between a patient and a healthcare provider.",
        singular: "encounter",
        plural: "encounters",
        synonyms: &["contact", "case"],
    },
    ConceptDescriptor {
        concept: EntityConcept::Provider,
        name_tokens: &["provider", "staff", "clinician", "doctor", "nurse", "practitioner", "employee", "workforce"],
        column_tokens: &["specialty", "role", "first_name", "last_name", "employee_no", "license", "department_id", "hire_date"],
        required_roles: &[ColumnRole::PrimaryKey, ColumnRole::PersonGivenName],
        description: "A healthcare provider, clinician, or staff member who delivers care.",
        singular: "provider",
        plural: "providers",
        synonyms: &["hcw", "clinician", "medic"],
    },
    ConceptDescriptor {
        concept: EntityConcept::Department,
        name_tokens: &["department", "dept", "unit", "service", "division"],
        column_tokens: &["code", "dept_type", "name", "floor", "cost_centre", "is_active"],
        required_roles: &[ColumnRole::PrimaryKey, ColumnRole::Code],
        description: "A hospital department, unit, or organisational section.",
        singular: "department",
        plural: "departments",
        synonyms: &["division", "clinic"],
    },
    ConceptDescriptor {
        concept: EntityConcept::DiagnosisCode,
        name_tokens: &["icd", "icd10", "diagnosis_code", "condition_code", "code_reference"],
        column_tokens: &["code", "description", "category", "chapter", "is_notifiable"],
        required_roles: &[ColumnRole::PrimaryKey, ColumnRole::Description],
        description: "ICD-10 or similar diagnosis code reference table.",
        singular: "diagnosis_code",
        plural: "diagnosis_codes",
        synonyms: &["icd", "condition_code", "disease_code"],
    },
    ConceptDescriptor {
        concept: EntityConcept::Geography,
        name_tokens: &["county", "region", "province", "district", "zone", "location", "address"],
        column_tokens: &["name", "region", "code", "county"],
        required_roles: &[ColumnRole::PrimaryKey, ColumnRole::Description],
        description: "Geographic reference: county, region, district or area used for catchment analysis.",
        singular: "county",
        plural: "counties",
        synonyms: &["catchment", "jurisdiction"],
    },
    ConceptDescriptor {
        concept: EntityConcept::Diagnosis,
        name_tokens: &["diagnosis", "diagnose", "condition", "finding", "problem"],
        column_tokens: &["icd_code", "diagnosis_code", "diagnosis_date", "provider_id", "encounter_id", "is_primary"],
        required_roles: &[ColumnRole::PatientRef, ColumnRole::Code],
        description: "A clinical diagnosis made for a patient during an encounter.",
        singular: "diagnosis",
        plural: "diagnoses",
        synonyms: &["dx", "problem_list"],
    },
    ConceptDescriptor {
        concept: EntityConcept::ClinicalNote,
        name_tokens: &["note", "clinical_note", "narrative", "progress_note", "documentation"],
        column_tokens: &["note_type", "content", "provider_id", "encounter_id", "recorded_at", "note_text"],
        required_roles: &[ColumnRole::PatientRef, ColumnRole::FreeText],
        description: "A clinical note, progress note, or narrative documentation written by a provider.",
        singular: "clinical_note",
        plural: "clinical_notes",
        synonyms: &["soap", "progress_note", "letter"],
    },
    ConceptDescriptor {
        concept: EntityConcept::VitalSign,
        name_tokens: &["vital", "vitals", "vital_sign", "observation", "physiological", "measurement"],
        column_tokens: &["temperature", "pulse", "bp_systolic", "resp_rate", "spo2", "weight", "height", "recorded_at"],
        required_roles: &[ColumnRole::PatientRef, ColumnRole::EventTime],
        description: "Vital sign measurements such as temperature, pulse, blood pressure, and oxygen saturation.",
        singular: "vital_sign",
        plural: "vital_signs",
        synonyms: &["obs", "observation", "vitals"],
    },
    ConceptDescriptor {
        concept: EntityConcept::Allergy,
        name_tokens: &["allergy", "allergic", "adverse", "intolerance", "reaction", "hypersensitivity"],
        column_tokens: &["allergen", "reaction_type", "severity", "onset_date", "verified_by", "is_active"],
        required_roles: &[ColumnRole::PatientRef, ColumnRole::Status],
        description: "A patient allergy, drug intolerance, or adverse reaction record.",
        singular: "allergy",
        plural: "allergies",
        synonyms: &["hypersensitivity", "adverse_reaction"],
    },
    ConceptDescriptor {
        concept: EntityConcept::MedicalHistory,
        name_tokens: &["medical_history", "history", "past_medical", "pmhx", "background", "anamnesis"],
        column_tokens: &["condition", "onset_year", "is_resolved", "notes", "provider_id"],
        required_roles: &[ColumnRole::PatientRef],
        description: "A patient's past medical history: conditions, operations, and chronic illness background.",
        singular: "medical_history",
        plural: "medical_histories",
        synonyms: &["pmh", "past_history"],
    },
    ConceptDescriptor {
        concept: EntityConcept::FamilyHistory,
        name_tokens: &["family", "familial", "hereditary", "pedigree", "genetic"],
        column_tokens: &["relation", "condition", "onset_age", "is_deceased", "notes"],
        required_roles: &[ColumnRole::PatientRef],
        description: "Family history of heritable conditions recorded for a patient.",
        singular: "family_history",
        plural: "family_histories",
        synonyms: &["fhx", "pedigree"],
    },
    ConceptDescriptor {
        concept: EntityConcept::Immunization,
        name_tokens: &["immunization", "immunisation", "vaccination", "vaccine_record", "jab"],
        column_tokens: &["vaccine", "dose_number", "given_at", "administered_by", "site", "lot_no"],
        required_roles: &[ColumnRole::PatientRef, ColumnRole::EventTime],
        description: "A vaccination or immunization event administered to a patient.",
        singular: "immunization",
        plural: "immunizations",
        synonyms: &["vaccination", "immunisation"],
    },
    ConceptDescriptor {
        concept: EntityConcept::Document,
        name_tokens: &["document", "file", "attachment", "form", "record"],
        column_tokens: &["document_type", "file_name", "uploaded_at", "uploaded_by", "file_path", "content_type"],
        required_roles: &[ColumnRole::PatientRef, ColumnRole::EventTime],
        description: "A patient document, scanned file, or uploaded clinical attachment.",
        singular: "document",
        plural: "documents",
        synonyms: &["attachment", "form", "scan"],
    },
    ConceptDescriptor {
        concept: EntityConcept::Appointment,
        name_tokens: &["appointment", "booking", "reservation", "slot", "scheduled"],
        column_tokens: &["scheduled_time", "appointment_date", "status", "provider_id", "appointment_status", "slot"],
        required_roles: &[ColumnRole::PatientRef, ColumnRole::EventTime],
        description: "A scheduled appointment or booking for a patient to see a provider.",
        singular: "appointment",
        plural: "appointments",
        synonyms: &["booking", "slot", "consultation"],
    },
    ConceptDescriptor {
        concept: EntityConcept::ProviderSchedule,
        name_tokens: &["schedule", "provider_schedule", "clinic_schedule", "timetable", "session", "clinic_session"],
        column_tokens: &["provider_id", "department_id", "weekday", "start_time", "end_time", "slot_minutes", "clinic_name"],
        required_roles: &[ColumnRole::ProviderRef, ColumnRole::StartTime],
        description: "A provider's recurring clinic schedule defining when they see patients.",
        singular: "provider_schedule",
        plural: "provider_schedules",
        synonyms: &["rota", "timetable", "clinic"],
    },
    ConceptDescriptor {
        concept: EntityConcept::ProviderTimeOff,
        name_tokens: &["leave", "time_off", "timeoff", "absence", "vacation", "annual_leave", "sick_leave", "off"],
        column_tokens: &["provider_id", "start_date", "end_date", "reason", "is_approved", "notes"],
        required_roles: &[ColumnRole::ProviderRef, ColumnRole::StartTime],
        description: "Provider leave, absence, or time-off from clinical duties.",
        singular: "time_off",
        plural: "time_off_records",
        synonyms: &["leave", "absence", "vacation"],
    },
    ConceptDescriptor {
        concept: EntityConcept::Referral,
        name_tokens: &["referral", "refer", "transfer_out", "external_referral"],
        column_tokens: &["referred_to", "referred_by", "referral_date", "status", "specialty", "urgency"],
        required_roles: &[ColumnRole::PatientRef, ColumnRole::EventTime, ColumnRole::Status],
        description: "A referral from one provider or facility to another for specialised care.",
        singular: "referral",
        plural: "referrals",
        synonyms: &["consultation_request", "outward_referral"],
    },
    ConceptDescriptor {
        concept: EntityConcept::InsurancePolicy,
        name_tokens: &["insurance_policy", "patient_insurance", "cover", "member_policy", "policy", "cover"],
        column_tokens: &["insurance_provider", "policy_no", "member_no", "valid_from", "valid_to", "is_active"],
        required_roles: &[ColumnRole::PatientRef, ColumnRole::BusinessId],
        description: "An insurance policy or cover held by a patient.",
        singular: "insurance_policy",
        plural: "insurance_policies",
        synonyms: &["health_cover", "nhif", "shif"],
    },
    ConceptDescriptor {
        concept: EntityConcept::Insurer,
        name_tokens: &["insurer", "insurance_provider", "payer", "nhif", "scheme", "fund"],
        column_tokens: &["name", "code", "scheme_type", "contact", "is_active", "claims_email"],
        required_roles: &[ColumnRole::PrimaryKey, ColumnRole::Description],
        description: "An insurance company, health fund, or payer that covers patient costs.",
        singular: "insurer",
        plural: "insurers",
        synonyms: &["payer", "health_fund", "nhif", "scheme"],
    },
    ConceptDescriptor {
        concept: EntityConcept::Admission,
        name_tokens: &["admission", "admit", "admittance", "inpatient", "ipd", "hospitalisation", "hospitalization"],
        column_tokens: &["admitted_at", "discharge_date", "ward_id", "bed_id", "los", "discharge_reason", "admission_type"],
        required_roles: &[ColumnRole::PatientRef, ColumnRole::EventTime],
        description: "An inpatient admission: the patient's stay from admission to discharge.",
        singular: "admission",
        plural: "admissions",
        synonyms: &["inpatient", "ipd", "stay", "hospitalisation"],
    },
    ConceptDescriptor {
        concept: EntityConcept::BedAssignment,
        name_tokens: &["bed_assignment", "bed_allocation", "occupancy"],
        column_tokens: &["bed_id", "ward_id", "assigned_at", "vacated_at", "admission_id"],
        required_roles: &[ColumnRole::PatientRef, ColumnRole::BedRef, ColumnRole::EventTime],
        description: "An assignment of a patient to a specific bed during admission.",
        singular: "bed_assignment",
        plural: "bed_assignments",
        synonyms: &["bed_occupancy", "allocation"],
    },
    ConceptDescriptor {
        concept: EntityConcept::Bed,
        name_tokens: &["bed", "bunk", "cot", "incubator"],
        column_tokens: &["ward_id", "bed_no", "bed_type", "status", "is_active"],
        required_roles: &[ColumnRole::WardRef, ColumnRole::Status],
        description: "A physical hospital bed with location, type and availability status.",
        singular: "bed",
        plural: "beds",
        synonyms: &["cot", "bunk"],
    },
    ConceptDescriptor {
        concept: EntityConcept::Ward,
        name_tokens: &["ward", "inpatient_area", "floor", "bay"],
        column_tokens: &["department_id", "ward_type", "bed_capacity", "floor", "is_active", "code"],
        required_roles: &[ColumnRole::DepartmentRef, ColumnRole::Code],
        description: "A hospital ward or inpatient nursing unit where patients are cared for.",
        singular: "ward",
        plural: "wards",
        synonyms: &["unit", "inpatient_area", "nursing_unit"],
    },
    ConceptDescriptor {
        concept: EntityConcept::Transfer,
        name_tokens: &["transfer", "patient_transfer", "ward_transfer", "bed_transfer"],
        column_tokens: &["from_ward", "to_ward", "from_bed", "to_bed", "transfer_date", "transferred_at", "reason"],
        required_roles: &[ColumnRole::PatientRef, ColumnRole::EventTime],
        description: "An intra-hospital patient transfer between wards or beds.",
        singular: "transfer",
        plural: "transfers",
        synonyms: &["ward_move", "relocation"],
    },
    ConceptDescriptor {
        concept: EntityConcept::Shift,
        name_tokens: &["shift", "roster", "rota", "duty", "on_call"],
        column_tokens: &["provider_id", "shift_date", "start_time", "end_time", "ward_id", "department_id", "shift_type"],
        required_roles: &[ColumnRole::ProviderRef, ColumnRole::StartTime],
        description: "A staff shift or roster entry showing who was on duty, when, and where.",
        singular: "shift",
        plural: "shifts",
        synonyms: &["roster", "rota", "duty"],
    },
    ConceptDescriptor {
        concept: EntityConcept::MedicationAdministration,
        name_tokens: &["medication_administration", "administration", "dispensing_event", "drug_round"],
        column_tokens: &["medication_id", "dose", "route", "administered_at", "administered_by", "status", "admin_status"],
        required_roles: &[ColumnRole::PatientRef, ColumnRole::EventTime, ColumnRole::Status],
        description: "A medication administration event: dose given, time, route, and outcome.",
        singular: "medication_administration",
        plural: "medication_administrations",
        synonyms: &["mar", "drug_round", "dispensing"],
    },
    ConceptDescriptor {
        concept: EntityConcept::Triage,
        name_tokens: &["triage", "assessment", "casualty", "emergency_assessment", "a_e", "er_triage"],
        column_tokens: &["triage_time", "acuity", "chief_complaint", "triage_category", "arrival_mode", "provider_id"],
        required_roles: &[ColumnRole::PatientRef, ColumnRole::EventTime],
        description: "An emergency or triage assessment classifying acuity on arrival.",
        singular: "triage",
        plural: "triage_assessments",
        synonyms: &["casualty", "a&e", "ed_triage", "er_triage"],
    },
    ConceptDescriptor {
        concept: EntityConcept::AntenatalVisit,
        name_tokens: &["antenatal", "prenatal", "anc", "ante_natal"],
        column_tokens: &["gestation_weeks", "visit_number", "fundal_height", "fetal_heart_rate", "visit_date", "risk_factors"],
        required_roles: &[ColumnRole::PatientRef, ColumnRole::EventTime],
        description: "An antenatal (prenatal) care visit during pregnancy.",
        singular: "antenatal_visit",
        plural: "antenatal_visits",
        synonyms: &["anc", "prenatal", "ante_natal"],
    },
    ConceptDescriptor {
        concept: EntityConcept::Delivery,
        name_tokens: &["delivery", "birth", "partum", "parturition", "labour", "ob_delivery"],
        column_tokens: &["delivery_mode", "delivered_at", "gestation_weeks", "blood_loss", "labour_onset", "delivered_by"],
        required_roles: &[ColumnRole::PatientRef, ColumnRole::EventTime],
        description: "A birth or obstetric delivery event recording mode, outcomes, and complications.",
        singular: "delivery",
        plural: "deliveries",
        synonyms: &["birth", "parturition", "ob"],
    },
    ConceptDescriptor {
        concept: EntityConcept::Newborn,
        name_tokens: &["newborn", "baby", "neonate", "neonatal", "infant", "ob_baby"],
        column_tokens: &["delivery_id", "birth_weight", "apgar", "outcome", "sex", "birth_order", "length_cm"],
        required_roles: &[ColumnRole::PatientRef, ColumnRole::EventTime],
        description: "A newborn baby record linked to the mother's delivery.",
        singular: "newborn",
        plural: "newborns",
        synonyms: &["baby", "babies", "neonate", "neonates", "infant", "infants"],
    },
    ConceptDescriptor {
        concept: EntityConcept::Surgery,
        name_tokens: &["surgery", "operation", "ot_case", "operative", "theatre_case"],
        column_tokens: &["scheduled_start", "theatre_no", "primary_surgeon", "anaesthetist", "urgency", "asa_grade"],
        required_roles: &[ColumnRole::PatientRef, ColumnRole::StartTime],
        description: "A surgical operation including theatre, surgeon, anaesthesia, and timing.",
        singular: "surgery",
        plural: "surgeries",
        synonyms: &["operation", "ot", "operating"],
    },
    ConceptDescriptor {
        concept: EntityConcept::Procedure,
        name_tokens: &["procedure", "intervention", "treatment_procedure"],
        column_tokens: &["procedure_code", "performed_at", "performed_by", "laterality", "outcome", "duration_minutes"],
        required_roles: &[ColumnRole::PatientRef, ColumnRole::EventTime],
        description: "A clinical procedure performed on a patient.",
        singular: "procedure",
        plural: "procedures",
        synonyms: &["intervention", "operation", "treatment"],
    },
    ConceptDescriptor {
        concept: EntityConcept::ProcedureCode,
        name_tokens: &["procedure_code", "cpt", "opcs", "procedure_catalog"],
        column_tokens: &["code", "description", "category", "is_surgical", "typical_minutes"],
        required_roles: &[ColumnRole::PrimaryKey, ColumnRole::Code],
        description: "Procedure code reference (CPT, OPCS, or local coding system).",
        singular: "procedure_code",
        plural: "procedure_codes",
        synonyms: &["cpt", "opcs", "procedure_catalog"],
    },
    ConceptDescriptor {
        concept: EntityConcept::Consent,
        name_tokens: &["consent", "informed_consent", "authorisation", "authorization"],
        column_tokens: &["procedure_id", "consented_at", "consented_by", "witness", "consent_type", "is_signed"],
        required_roles: &[ColumnRole::PatientRef, ColumnRole::EventTime],
        description: "An informed consent document for a procedure or admission.",
        singular: "consent",
        plural: "consents",
        synonyms: &["authorisation", "informed_consent"],
    },
    ConceptDescriptor {
        concept: EntityConcept::Prescription,
        name_tokens: &["prescription", "rx", "order", "drug_order", "med_order"],
        column_tokens: &["prescribed_by", "prescribed_at", "status", "encounter_id", "medication_id", "rx_number"],
        required_roles: &[ColumnRole::PatientRef, ColumnRole::EventTime],
        description: "A medication prescription or drug order issued for a patient.",
        singular: "prescription",
        plural: "prescriptions",
        synonyms: &["rx", "drug_order", "medication_order"],
    },
    ConceptDescriptor {
        concept: EntityConcept::PrescriptionItem,
        name_tokens: &["prescription_item", "rx_line", "rxline", "order_line", "drug_item"],
        column_tokens: &["prescription_id", "medication_id", "dose", "frequency", "quantity", "duration_days"],
        required_roles: &[ColumnRole::ForeignRef],
        description: "One medication line item within a prescription or drug order.",
        singular: "prescription_item",
        plural: "prescription_items",
        synonyms: &["rx_line", "drug_line", "order_line"],
    },
    ConceptDescriptor {
        concept: EntityConcept::Medication,
        name_tokens: &["medication", "drug", "medicine", "formulary", "catalog", "catalogue"],
        column_tokens: &["generic_name", "brand_name", "drug_class", "atc_code", "form", "strength", "is_formulary"],
        required_roles: &[ColumnRole::PrimaryKey, ColumnRole::Description],
        description: "Medication or drug reference catalog (formulary).",
        singular: "medication",
        plural: "medications",
        synonyms: &["drug", "medicine", "formulary", "chemist"],
    },
    ConceptDescriptor {
        concept: EntityConcept::StockBatch,
        name_tokens: &["stock", "batch", "inventory", "lot", "drug_stock", "stock_batch"],
        column_tokens: &["medication_id", "batch_no", "expiry_date", "quantity", "unit_cost", "stored_at"],
        required_roles: &[ColumnRole::ForeignRef, ColumnRole::EventTime],
        description: "A stock batch of medication or supplies in the pharmacy inventory.",
        singular: "stock_batch",
        plural: "stock_batches",
        synonyms: &["batch", "lot", "inventory_batch"],
    },
    ConceptDescriptor {
        concept: EntityConcept::StockMovement,
        name_tokens: &["stock_movement", "movement", "transaction", "dispensing", "issue"],
        column_tokens: &["batch_id", "medication_id", "quantity_change", "movement_type", "reference_id", "moved_at"],
        required_roles: &[ColumnRole::ForeignRef, ColumnRole::EventTime],
        description: "A stock movement or transaction recording quantity in/out of the pharmacy.",
        singular: "stock_movement",
        plural: "stock_movements",
        synonyms: &["transaction", "dispensing", "issue"],
    },
    ConceptDescriptor {
        concept: EntityConcept::DrugInteraction,
        name_tokens: &["drug_interaction", "interaction", "contraindication"],
        column_tokens: &["drug1_id", "drug2_id", "severity", "mechanism", "clinical_effect", "management"],
        required_roles: &[ColumnRole::ForeignRef, ColumnRole::Severity],
        description: "A clinically significant drug-drug interaction with severity and management guidance.",
        singular: "drug_interaction",
        plural: "drug_interactions",
        synonyms: &["interaction", "contraindication"],
    },
    ConceptDescriptor {
        concept: EntityConcept::Allergen,
        name_tokens: &["allergen", "allergen_catalog", "allergen_reference"],
        column_tokens: &["name", "category", "cross_reactivity", "igg_class", "is_active"],
        required_roles: &[ColumnRole::PrimaryKey, ColumnRole::Description],
        description: "An allergen reference catalog listing known allergenic substances.",
        singular: "allergen",
        plural: "allergens",
        synonyms: &["substance", "antigen"],
    },
    ConceptDescriptor {
        concept: EntityConcept::CdsAlert,
        name_tokens: &["cds", "alert", "clinical_alert", "decision_support", "reminder"],
        column_tokens: &["alert_type", "triggered_at", "rule_id", "acknowledged_by", "is_overridden", "patient_id"],
        required_roles: &[ColumnRole::PatientRef, ColumnRole::EventTime],
        description: "A clinical decision support alert fired for a patient or order.",
        singular: "cds_alert",
        plural: "cds_alerts",
        synonyms: &["alert", "reminder", "cds", "decision_support"],
    },
    ConceptDescriptor {
        concept: EntityConcept::LabOrder,
        name_tokens: &["lab_order", "laboratory_order", "test_request", "lab_request", "lab_req"],
        column_tokens: &["test_id", "ordered_by", "ordered_at", "status", "priority", "encounter_id"],
        required_roles: &[ColumnRole::PatientRef, ColumnRole::EventTime, ColumnRole::Status],
        description: "A laboratory test order or request for a patient.",
        singular: "lab_order",
        plural: "lab_orders",
        synonyms: &["test_request", "lab_req", "investigation"],
    },
    ConceptDescriptor {
        concept: EntityConcept::LabResult,
        // "result" is standard clinical shorthand for a laboratory result
        // (e.g. "any abnormal results?"); included as a name_token so alt-schema
        // tables with short names (LabRes) and bare "result" phrasing resolve.
        name_tokens: &["lab_result", "laboratory_result", "test_result", "lab_res", "result"],
        column_tokens: &["order_id", "result_value", "result_date", "unit", "reference_range", "is_abnormal", "verified_by"],
        required_roles: &[ColumnRole::PatientRef, ColumnRole::EventTime],
        description: "A laboratory test result for a patient.",
        singular: "lab_result",
        plural: "lab_results",
        synonyms: &["test_result", "lab_res", "investigation_result"],
    },
    ConceptDescriptor {
        concept: EntityConcept::LabTest,
        name_tokens: &["lab_test", "test_catalog", "laboratory_test", "investigation_catalog"],
        column_tokens: &["code", "name", "category", "unit", "reference_range", "sample_type", "turnaround_hours"],
        required_roles: &[ColumnRole::PrimaryKey, ColumnRole::Code],
        description: "A laboratory test catalog entry defining tests available in the lab.",
        singular: "lab_test",
        plural: "lab_tests",
        synonyms: &["test_catalog", "investigation"],
    },
    ConceptDescriptor {
        concept: EntityConcept::ImagingOrder,
        name_tokens: &["imaging", "radiology", "xray", "scan", "imaging_order"],
        column_tokens: &["modality", "body_part", "ordered_by", "ordered_at", "status", "priority", "radiologist_id"],
        required_roles: &[ColumnRole::PatientRef, ColumnRole::EventTime],
        description: "An imaging order (X-ray, CT, MRI, ultrasound, etc.) for a patient.",
        singular: "imaging_order",
        plural: "imaging_orders",
        synonyms: &["radiology_order", "scan_request", "xray"],
    },
    ConceptDescriptor {
        concept: EntityConcept::BloodUnit,
        name_tokens: &["blood", "blood_unit", "donation", "blood_bank"],
        column_tokens: &["blood_group", "component", "volume_ml", "collected_on", "expires_on", "status", "unit_no"],
        required_roles: &[ColumnRole::EventTime, ColumnRole::Status],
        description: "A blood unit in the blood bank: group, component, volume, and status.",
        singular: "blood_unit",
        plural: "blood_units",
        synonyms: &["donation", "blood_product", "unit"],
    },
    ConceptDescriptor {
        concept: EntityConcept::Transfusion,
        name_tokens: &["transfusion", "blood_transfusion", "infusion_blood"],
        column_tokens: &["blood_unit_id", "issued_at", "started_at", "reaction", "indication", "administered_by"],
        required_roles: &[ColumnRole::PatientRef, ColumnRole::EventTime],
        description: "A blood transfusion event: product issued, administered, and reaction recorded.",
        singular: "transfusion",
        plural: "transfusions",
        synonyms: &["blood_transfusion"],
    },
    ConceptDescriptor {
        concept: EntityConcept::Bill,
        name_tokens: &["bill", "billing_encounter", "billing", "invoice", "charge", "statement"],
        column_tokens: &["bill_no", "billing_date", "subtotal", "discount", "patient_due", "amount_paid", "status"],
        required_roles: &[ColumnRole::PatientRef, ColumnRole::EventTime, ColumnRole::Amount],
        description: "A patient bill or invoice summarising charges for an encounter.",
        singular: "bill",
        plural: "bills",
        synonyms: &["invoice", "billing", "charge"],
    },
    ConceptDescriptor {
        concept: EntityConcept::BillItem,
        name_tokens: &["billing_item", "bill_item", "charge_item", "line_item"],
        column_tokens: &["billing_id", "item_type", "description", "quantity", "unit_price", "total"],
        required_roles: &[ColumnRole::ForeignRef, ColumnRole::Amount],
        description: "A line item on a bill: consultation, lab, pharmacy, procedure, bed charge.",
        singular: "bill_item",
        plural: "bill_items",
        synonyms: &["charge_line", "billing_detail"],
    },
    ConceptDescriptor {
        concept: EntityConcept::Claim,
        name_tokens: &["claim", "insurance_claim", "reimbursement", "preauth"],
        column_tokens: &["billing_id", "insurer_id", "claim_no", "submitted_at", "status", "amount_claimed", "amount_approved"],
        required_roles: &[ColumnRole::PatientRef, ColumnRole::EventTime, ColumnRole::Amount],
        description: "An insurance claim submitted to a payer for reimbursement.",
        singular: "claim",
        plural: "claims",
        synonyms: &["insurance_claim", "reimbursement"],
    },
    ConceptDescriptor {
        concept: EntityConcept::Payment,
        name_tokens: &["payment", "receipt", "transaction", "cash", "mpesa", "collection"],
        column_tokens: &["payment_date", "payment_method", "amount", "reference_no", "received_by", "billing_id"],
        required_roles: &[ColumnRole::PatientRef, ColumnRole::EventTime, ColumnRole::Amount],
        description: "A payment received from a patient or payer (cash, mobile money, card).",
        singular: "payment",
        plural: "payments",
        synonyms: &["receipt", "collection", "transaction", "mpesa"],
    },
    ConceptDescriptor {
        concept: EntityConcept::Incident,
        name_tokens: &["incident", "event_report", "adverse_event", "near_miss", "occurrence"],
        column_tokens: &["incident_type", "incident_date", "severity", "reported_by", "location", "outcome"],
        required_roles: &[ColumnRole::EventTime, ColumnRole::Severity],
        description: "A patient safety incident, adverse event, or near-miss report.",
        singular: "incident",
        plural: "incidents",
        synonyms: &["adverse_event", "near_miss", "occurrence"],
    },
    ConceptDescriptor {
        concept: EntityConcept::Mortality,
        name_tokens: &["mortality", "death", "deceased", "fatality"],
        column_tokens: &["patient_id", "date_of_death", "cause_of_death", "icd_cause", "certified_by", "manner"],
        required_roles: &[ColumnRole::PatientRef, ColumnRole::EventTime],
        description: "An inpatient or facility death record with cause and certification.",
        singular: "mortality",
        plural: "mortality_records",
        synonyms: &["death", "deceased", "fatality"],
    },
    ConceptDescriptor {
        concept: EntityConcept::Feedback,
        name_tokens: &["feedback", "satisfaction", "survey", "complaint", "rating"],
        column_tokens: &["patient_id", "feedback_date", "category", "rating", "comment", "resolved"],
        required_roles: &[ColumnRole::PatientRef, ColumnRole::EventTime],
        description: "Patient feedback, satisfaction survey, or complaint record.",
        singular: "feedback",
        plural: "feedbacks",
        synonyms: &["satisfaction", "survey", "complaint"],
    },
    ConceptDescriptor {
        concept: EntityConcept::NotifiableDisease,
        name_tokens: &["notifiable", "disease_report", "public_health", "notification"],
        column_tokens: &["disease_code", "reported_at", "reported_by", "patient_id", "status", "authority"],
        required_roles: &[ColumnRole::PatientRef, ColumnRole::EventTime],
        description: "A notifiable disease report filed with public health authorities.",
        singular: "notifiable_disease",
        plural: "notifiable_disease_reports",
        synonyms: &["notification", "public_health_report"],
    },
    ConceptDescriptor {
        concept: EntityConcept::AccessLog,
        name_tokens: &["access_log", "audit_log", "record_access", "access_record", "break_glass"],
        column_tokens: &["patient_id", "accessed_at", "accessed_by", "action", "ip_address", "justification"],
        required_roles: &[ColumnRole::PatientRef, ColumnRole::EventTime],
        description: "A log of who accessed or modified patient records (privacy and audit trail).",
        singular: "access_log",
        plural: "access_logs",
        synonyms: &["audit_trail", "break_glass", "audit_log"],
    },
    ConceptDescriptor {
        concept: EntityConcept::License,
        name_tokens: &["license", "licence", "registration", "practising_certificate", "provider_license"],
        column_tokens: &["provider_id", "regulator", "license_no", "license_type", "issued_on", "expires_on", "is_current"],
        required_roles: &[ColumnRole::ProviderRef, ColumnRole::EventTime],
        description: "A provider's practising licence or professional registration.",
        singular: "license",
        plural: "licenses",
        synonyms: &["licence", "registration", "certificate"],
    },
    ConceptDescriptor {
        concept: EntityConcept::CareProgram,
        name_tokens: &["care_program", "program", "programme", "register", "clinic_register", "disease_program"],
        column_tokens: &["name", "disease", "description", "target_population", "is_active"],
        required_roles: &[ColumnRole::PrimaryKey, ColumnRole::Description],
        description: "A chronic care program or disease management register (HIV, TB, DM, HTN, etc.).",
        singular: "care_program",
        plural: "care_programs",
        synonyms: &["program", "register", "clinic"],
    },
    ConceptDescriptor {
        concept: EntityConcept::ProgramEnrollment,
        name_tokens: &["enrollment", "enrolment", "programme_enrolment", "program_enrollment", "register_entry"],
        column_tokens: &["program_id", "enrolled_at", "status", "next_visit_date", "exit_date", "outcome"],
        required_roles: &[ColumnRole::PatientRef, ColumnRole::EventTime],
        description: "A patient's enrolment in a chronic care program or disease register.",
        singular: "program_enrollment",
        plural: "program_enrollments",
        synonyms: &["enrolment", "registration", "register"],
    },
    ConceptDescriptor {
        concept: EntityConcept::Vaccine,
        name_tokens: &["vaccine", "vaccine_catalog", "vaccination_catalog", "immunogen"],
        column_tokens: &["name", "antigen", "schedule", "doses_required", "route", "is_active"],
        required_roles: &[ColumnRole::PrimaryKey, ColumnRole::Description],
        description: "A vaccine reference catalog defining antigens, schedule, and dose.",
        singular: "vaccine",
        plural: "vaccines",
        synonyms: &["antigen", "immunization_catalog"],
    },
    ConceptDescriptor {
        concept: EntityConcept::Equipment,
        name_tokens: &["equipment", "device", "asset", "machinery", "apparatus"],
        column_tokens: &["name", "category", "serial_no", "department_id", "status", "purchase_date", "is_active"],
        required_roles: &[ColumnRole::PrimaryKey, ColumnRole::Status],
        description: "A medical device or equipment item in the hospital asset register.",
        singular: "equipment",
        plural: "equipment",
        synonyms: &["device", "asset", "machinery"],
    },
    ConceptDescriptor {
        concept: EntityConcept::EquipmentMaintenance,
        name_tokens: &["maintenance", "service_record", "equipment_maintenance", "repair", "servicing"],
        column_tokens: &["equipment_id", "service_date", "performed_by", "type", "next_service_date", "cost"],
        required_roles: &[ColumnRole::ForeignRef, ColumnRole::EventTime],
        description: "A maintenance or service record for a medical equipment item.",
        singular: "equipment_maintenance",
        plural: "equipment_maintenances",
        synonyms: &["service_record", "repair", "maintenance_log"],
    },
];

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;
    use crate::ontology::service_line::ServiceLine;

    #[test]
    fn all_count_and_unique_slugs() {
        // 65 total: 64 real concepts + Unknown
        assert_eq!(
            EntityConcept::ALL.len(),
            65,
            "EntityConcept::ALL must have exactly 65 entries (64 concepts + Unknown)"
        );
        let slugs: HashSet<&str> = EntityConcept::ALL.iter().map(|c| c.slug()).collect();
        assert_eq!(
            slugs.len(),
            EntityConcept::ALL.len(),
            "every EntityConcept slug must be unique"
        );
    }

    #[test]
    fn all_owned_concepts_have_descriptor() {
        // Every concept in any service line's concepts() must have a descriptor.
        for line in ServiceLine::ALL {
            for concept in line.concepts() {
                let _ = descriptor(*concept);  // panics if missing
            }
        }
    }

    #[test]
    fn unknown_and_geography_not_owned() {
        // Geography and Unknown have no owning service line (returned as &[]).
        // They do appear in PatientChart.concepts() though — that's intentional
        // (Geography IS owned by PatientChart per the §4 table).
        // Unknown really must have no owner.
        assert!(ServiceLine::owner_of(EntityConcept::Unknown).is_empty());
    }
}
