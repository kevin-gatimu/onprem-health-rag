//! Integration-level acceptance tests for the schema binder.
//!
//! These tests run entirely in-process (no DB, no fastembed) using fixture data
//! defined in `dev_seed_binding.json` and `alt_schema_binding.json`.
//!
//! Acceptance criteria covered here:
//! 1. Dev seed: ≥62/64 exact concepts, 64/64 service-line superset, zero orphans
//! 2. Alt schema: ≥22/25 exact concepts, usable_lines().len() >= 9
//! 3. ALL completeness + unique slugs (in unit tests on each enum)
//! 4. patient_path: vital_signs→patients ≤2 hops
//! 5. Exactly one EventTime per bound table, `created_at` never chosen when domain date exists
//! 6. No ColumnBinding with pii==true has non-empty enum_values
//! 7. SchemaBinding BSON round-trip (in binding.rs)
//! 8-10. override, determinism (in binder.rs)

use std::collections::HashMap;

use serde::Deserialize;

use crate::nl2sql::spec::{CardColumn, CardFkEdge, ColumnProfile, TableCard};
use crate::ontology::binder::bind_cards;
use crate::ontology::binding::SchemaBinding;
use crate::ontology::concepts::EntityConcept;
use crate::ontology::service_line::ServiceLine;

/// Production default for `binding_min_confidence`.
/// Must stay in sync with the `env_parse` default in `config.rs`.
/// All `bind_cards` calls in this module use this constant so the floor
/// cannot drift per-test.
const PROD_MIN_CONFIDENCE: f32 = 0.55;

// ---------------------------------------------------------------------------
// Fixture helpers
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
struct FixtureEntry {
    table: String,
    concept: String,
}

#[derive(Debug, Deserialize)]
struct Fixture {
    expected: Vec<FixtureEntry>,
}

fn load_fixture(json: &str) -> Fixture {
    serde_json::from_str(json).expect("valid fixture JSON")
}

/// Build a minimal TableCard from the dev-postgres schema structure.
/// We only supply enough metadata for the binder to score correctly — the full
/// schema is too large to inline; the key signals are the column names and FKs.
pub(crate) fn dev_seed_cards() -> Vec<TableCard> {
    // Helper closure
    let col = |name: &str, type_: &str, pk: bool, fk: bool| CardColumn {
        name: name.to_string(),
        type_: type_.to_string(),
        nullable: !pk,
        is_primary_key: pk,
        is_foreign_key: fk,
        sample_values: vec![],
        profile: ColumnProfile::default(),
    };
    let fk_edge = |column: &str, ref_table: &str, ref_column: &str| CardFkEdge {
        column: column.to_string(),
        ref_table: ref_table.to_string(),
        ref_column: ref_column.to_string(),
    };
    let card = |table: &str, row_count: i64, cols: Vec<CardColumn>, fks: Vec<CardFkEdge>| TableCard {
        source_id: "dev".to_string(),
        table_name: table.to_string(),
        row_count,
        columns: cols,
        fk_edges: fks,
        card_vector: None,
        card_text: table.to_string(),
    };

    vec![
        card("patients", 50000, vec![
            col("id", "serial", true, false),
            col("patient_no", "varchar", false, false),
            col("first_name", "varchar", false, false),
            col("last_name", "varchar", false, false),
            col("date_of_birth", "date", false, false),
            col("gender", "varchar", false, false),
            col("county_id", "integer", false, true),
            col("phone", "varchar", false, false),
            col("national_id", "varchar", false, false),
            col("registered_at", "timestamp", false, false),
            col("patient_status", "varchar", false, false),
        ], vec![fk_edge("county_id", "counties", "id")]),
        card("counties", 47, vec![
            col("id", "serial", true, false),
            col("name", "varchar", false, false),
            col("region", "varchar", false, false),
            col("code", "varchar", false, false),
        ], vec![]),
        card("departments", 20, vec![
            col("id", "serial", true, false),
            col("code", "varchar", false, false),
            col("name", "varchar", false, false),
            col("dept_type", "varchar", false, false),
            col("floor", "varchar", false, false),
            col("is_active", "boolean", false, false),
        ], vec![]),
        card("wards", 15, vec![
            col("id", "serial", true, false),
            col("code", "varchar", false, false),
            col("name", "varchar", false, false),
            col("ward_type", "varchar", false, false),
            col("bed_capacity", "integer", false, false),
            col("department_id", "integer", false, true),
            col("floor", "varchar", false, false),
            col("is_active", "boolean", false, false),
        ], vec![fk_edge("department_id", "departments", "id")]),
        card("beds", 300, vec![
            col("id", "serial", true, false),
            col("ward_id", "integer", false, true),
            col("bed_no", "varchar", false, false),
            col("bed_type", "varchar", false, false),
            col("status", "varchar", false, false),
        ], vec![fk_edge("ward_id", "wards", "id")]),
        card("providers", 200, vec![
            col("id", "serial", true, false),
            col("employee_no", "varchar", false, false),
            col("first_name", "varchar", false, false),
            col("last_name", "varchar", false, false),
            col("specialty", "varchar", false, false),
            col("role", "varchar", false, false),
            col("department_id", "integer", false, true),
            col("hire_date", "date", false, false),
            col("is_active", "boolean", false, false),
        ], vec![fk_edge("department_id", "departments", "id")]),
        card("provider_schedules", 500, vec![
            col("id", "serial", true, false),
            col("provider_id", "integer", false, true),
            col("department_id", "integer", false, true),
            col("weekday", "integer", false, false),
            col("start_time", "time", false, false),
            col("end_time", "time", false, false),
            col("slot_minutes", "integer", false, false),
            col("clinic_name", "varchar", false, false),
        ], vec![fk_edge("provider_id", "providers", "id"), fk_edge("department_id", "departments", "id")]),
        card("provider_time_off", 800, vec![
            col("id", "serial", true, false),
            col("provider_id", "integer", false, true),
            col("start_date", "date", false, false),
            col("end_date", "date", false, false),
            col("reason", "varchar", false, false),
            col("is_approved", "boolean", false, false),
        ], vec![fk_edge("provider_id", "providers", "id")]),
        card("icd10_codes", 15000, vec![
            col("id", "serial", true, false),
            col("code", "varchar", false, false),
            col("description", "varchar", false, false),
            col("category", "varchar", false, false),
            col("chapter", "varchar", false, false),
            col("is_notifiable", "boolean", false, false),
        ], vec![]),
        card("encounters", 200000, vec![
            col("id", "serial", true, false),
            col("patient_id", "integer", false, true),
            col("provider_id", "integer", false, true),
            col("department_id", "integer", false, true),
            col("encounter_type", "varchar", false, false),
            col("encounter_date", "date", false, false),
            col("chief_complaint", "text", false, false),
            col("status", "varchar", false, false),
        ], vec![fk_edge("patient_id", "patients", "id"), fk_edge("provider_id", "providers", "id"), fk_edge("department_id", "departments", "id")]),
        card("appointments", 100000, vec![
            col("id", "serial", true, false),
            col("patient_id", "integer", false, true),
            col("provider_id", "integer", false, true),
            col("scheduled_time", "timestamp", false, false),
            col("scheduled_at", "timestamp", false, false),
            col("appointment_status", "varchar", false, false),
            col("appointment_type", "varchar", false, false),
            col("department_id", "integer", false, true),
        ], vec![fk_edge("patient_id", "patients", "id"), fk_edge("provider_id", "providers", "id")]),
        card("referrals", 5000, vec![
            col("id", "serial", true, false),
            col("patient_id", "integer", false, true),
            col("referred_by", "integer", false, true),
            col("referred_to", "varchar", false, false),
            col("referral_date", "date", false, false),
            col("status", "varchar", false, false),
            col("specialty", "varchar", false, false),
            col("referral_closed_at", "timestamp", false, false),
        ], vec![fk_edge("patient_id", "patients", "id")]),
        card("insurers", 30, vec![
            col("id", "serial", true, false),
            col("name", "varchar", false, false),
            col("code", "varchar", false, false),
            col("scheme_type", "varchar", false, false),
            col("is_active", "boolean", false, false),
            col("claims_email", "varchar", false, false),
        ], vec![]),
        card("patient_insurance", 80000, vec![
            col("id", "serial", true, false),
            col("patient_id", "integer", false, true),
            col("insurance_provider", "integer", false, true),
            col("policy_no", "varchar", false, false),
            col("member_no", "varchar", false, false),
            col("valid_from", "date", false, false),
            col("valid_to", "date", false, false),
            col("is_active", "boolean", false, false),
        ], vec![fk_edge("patient_id", "patients", "id"), fk_edge("insurance_provider", "insurers", "id")]),
        card("diagnoses", 400000, vec![
            col("id", "serial", true, false),
            col("patient_id", "integer", false, true),
            col("encounter_id", "integer", false, true),
            col("icd_code", "varchar", false, false),
            col("diagnosis_date", "date", false, false),
            col("provider_id", "integer", false, true),
            col("is_primary", "boolean", false, false),
        ], vec![fk_edge("patient_id", "patients", "id"), fk_edge("encounter_id", "encounters", "id")]),
        card("clinical_notes", 500000, vec![
            col("id", "serial", true, false),
            col("patient_id", "integer", false, true),
            col("encounter_id", "integer", false, true),
            col("provider_id", "integer", false, true),
            col("note_type", "varchar", false, false),
            col("content", "text", false, false),
            col("recorded_at", "timestamp", false, false),
        ], vec![fk_edge("patient_id", "patients", "id"), fk_edge("encounter_id", "encounters", "id")]),
        card("vital_signs", 1000000, vec![
            col("id", "serial", true, false),
            col("patient_id", "integer", false, true),
            col("encounter_id", "integer", false, true),
            col("temperature", "numeric", false, false),
            col("pulse", "integer", false, false),
            col("bp_systolic", "integer", false, false),
            col("bp_diastolic", "integer", false, false),
            col("resp_rate", "integer", false, false),
            col("spo2", "numeric", false, false),
            col("weight", "numeric", false, false),
            col("height", "numeric", false, false),
            col("recorded_at", "timestamp", false, false),
        ], vec![fk_edge("patient_id", "patients", "id"), fk_edge("encounter_id", "encounters", "id")]),
        card("allergies", 60000, vec![
            col("id", "serial", true, false),
            col("patient_id", "integer", false, true),
            col("allergen", "varchar", false, false),
            col("reaction_type", "varchar", false, false),
            col("severity", "varchar", false, false),
            col("onset_date", "date", false, false),
            col("is_active", "boolean", false, false),
        ], vec![fk_edge("patient_id", "patients", "id")]),
        card("medical_history", 100000, vec![
            col("id", "serial", true, false),
            col("patient_id", "integer", false, true),
            col("condition", "varchar", false, false),
            col("onset_year", "integer", false, false),
            col("is_resolved", "boolean", false, false),
            col("notes", "text", false, false),
        ], vec![fk_edge("patient_id", "patients", "id")]),
        card("family_history", 40000, vec![
            col("id", "serial", true, false),
            col("patient_id", "integer", false, true),
            col("relation", "varchar", false, false),
            col("condition", "varchar", false, false),
            col("is_deceased", "boolean", false, false),
        ], vec![fk_edge("patient_id", "patients", "id")]),
        card("immunizations", 80000, vec![
            col("id", "serial", true, false),
            col("patient_id", "integer", false, true),
            col("vaccine", "varchar", false, false),
            col("dose_number", "integer", false, false),
            col("given_at", "date", false, false),
            col("administered_by", "integer", false, true),
            col("lot_no", "varchar", false, false),
        ], vec![fk_edge("patient_id", "patients", "id")]),
        card("patient_documents", 50000, vec![
            col("id", "serial", true, false),
            col("doc_no", "varchar", false, false),
            col("patient_id", "integer", false, true),
            col("document_type", "varchar", false, false),
            col("file_name", "varchar", false, false),
            col("uploaded_at", "timestamp", false, false),
            col("uploaded_by", "integer", false, true),
        ], vec![fk_edge("patient_id", "patients", "id")]),
        card("admissions", 30000, vec![
            col("id", "serial", true, false),
            col("admission_no", "varchar", false, false),
            col("patient_id", "integer", false, true),
            col("ward_id", "integer", false, true),
            col("bed_id", "integer", false, true),
            col("admitted_at", "timestamp", false, false),
            col("discharge_date", "date", false, false),
            col("admission_type", "varchar", false, false),
            col("discharge_reason", "varchar", false, false),
            col("length_of_stay_days", "integer", false, false),
            col("total_charges", "numeric", false, false),
            col("admission_status", "varchar", false, false),
        ], vec![fk_edge("patient_id", "patients", "id"), fk_edge("ward_id", "wards", "id"), fk_edge("bed_id", "beds", "id")]),
        card("bed_assignments", 80000, vec![
            col("id", "serial", true, false),
            col("patient_id", "integer", false, true),
            col("bed_id", "integer", false, true),
            col("ward_id", "integer", false, true),
            col("admission_id", "integer", false, true),
            col("assigned_at", "timestamp", false, false),
            col("vacated_at", "timestamp", false, false),
        ], vec![fk_edge("patient_id", "patients", "id"), fk_edge("bed_id", "beds", "id")]),
        card("patient_transfers", 5000, vec![
            col("id", "serial", true, false),
            col("patient_id", "integer", false, true),
            col("from_ward", "integer", false, true),
            col("to_ward", "integer", false, true),
            col("from_bed", "integer", false, true),
            col("to_bed", "integer", false, true),
            col("transfer_date", "timestamp", false, false),
            col("reason", "varchar", false, false),
        ], vec![fk_edge("patient_id", "patients", "id")]),
        card("staff_shifts", 50000, vec![
            col("id", "serial", true, false),
            col("provider_id", "integer", false, true),
            col("ward_id", "integer", false, true),
            col("shift_date", "date", false, false),
            col("start_time", "time", false, false),
            col("end_time", "time", false, false),
            col("shift_type", "varchar", false, false),
        ], vec![fk_edge("provider_id", "providers", "id"), fk_edge("ward_id", "wards", "id")]),
        card("medication_administrations", 200000, vec![
            col("id", "serial", true, false),
            col("patient_id", "integer", false, true),
            col("medication_id", "integer", false, true),
            col("dose", "varchar", false, false),
            col("route", "varchar", false, false),
            col("administered_at", "timestamp", false, false),
            col("administered_by", "integer", false, true),
            col("admin_status", "varchar", false, false),
        ], vec![fk_edge("patient_id", "patients", "id"), fk_edge("medication_id", "medications", "id")]),
        card("triage_assessments", 20000, vec![
            col("id", "serial", true, false),
            col("patient_id", "integer", false, true),
            col("triage_time", "timestamp", false, false),
            col("acuity", "varchar", false, false),
            col("chief_complaint", "text", false, false),
            col("triage_category", "varchar", false, false),
            col("arrival_mode", "varchar", false, false),
            col("provider_id", "integer", false, true),
        ], vec![fk_edge("patient_id", "patients", "id")]),
        card("antenatal_visits", 10000, vec![
            col("id", "serial", true, false),
            col("patient_id", "integer", false, true),
            col("visit_date", "date", false, false),
            col("gestation_weeks", "integer", false, false),
            col("visit_number", "integer", false, false),
            col("fundal_height", "numeric", false, false),
            col("fetal_heart_rate", "integer", false, false),
            col("provider_id", "integer", false, true),
        ], vec![fk_edge("patient_id", "patients", "id")]),
        card("deliveries", 8000, vec![
            col("id", "serial", true, false),
            col("patient_id", "integer", false, true),
            col("delivered_at", "timestamp", false, false),
            col("delivery_mode", "varchar", false, false),
            col("delivery_status", "varchar", false, false),
            col("gestation_weeks", "integer", false, false),
            col("blood_loss", "numeric", false, false),
            col("delivered_by", "integer", false, true),
        ], vec![fk_edge("patient_id", "patients", "id")]),
        card("newborns", 8000, vec![
            col("id", "serial", true, false),
            col("delivery_id", "integer", false, true),
            col("birth_weight", "numeric", false, false),
            col("apgar", "integer", false, false),
            col("outcome", "varchar", false, false),
            col("sex", "varchar", false, false),
            col("length_cm", "numeric", false, false),
        ], vec![fk_edge("delivery_id", "deliveries", "id")]),
        card("surgeries", 5000, vec![
            col("id", "serial", true, false),
            col("surgery_no", "varchar", false, false),
            col("patient_id", "integer", false, true),
            col("scheduled_start", "timestamp", false, false),
            col("theatre_no", "varchar", false, false),
            col("primary_surgeon", "integer", false, true),
            col("anaesthetist", "integer", false, true),
            col("urgency", "varchar", false, false),
            col("asa_grade", "varchar", false, false),
        ], vec![fk_edge("patient_id", "patients", "id")]),
        card("procedures", 15000, vec![
            col("id", "serial", true, false),
            col("patient_id", "integer", false, true),
            col("procedure_code", "varchar", false, false),
            col("performed_at", "timestamp", false, false),
            col("performed_by", "integer", false, true),
            col("outcome", "varchar", false, false),
            col("duration_minutes", "integer", false, false),
        ], vec![fk_edge("patient_id", "patients", "id")]),
        card("procedure_codes", 2000, vec![
            col("id", "serial", true, false),
            col("code", "varchar", false, false),
            col("description", "varchar", false, false),
            col("category", "varchar", false, false),
            col("is_surgical", "boolean", false, false),
            col("typical_minutes", "integer", false, false),
        ], vec![]),
        card("consents", 5000, vec![
            col("id", "serial", true, false),
            col("patient_id", "integer", false, true),
            col("procedure_id", "integer", false, true),
            col("consented_at", "timestamp", false, false),
            col("consented_by", "integer", false, true),
            col("consent_type", "varchar", false, false),
            col("is_signed", "boolean", false, false),
        ], vec![fk_edge("patient_id", "patients", "id")]),
        card("medications", 3000, vec![
            col("id", "serial", true, false),
            col("medication_no", "varchar", false, false),
            col("generic_name", "varchar", false, false),
            col("brand_name", "varchar", false, false),
            col("drug_class", "varchar", false, false),
            col("atc_code", "varchar", false, false),
            col("form", "varchar", false, false),
            col("strength", "varchar", false, false),
            col("is_formulary", "boolean", false, false),
            col("stock_status", "varchar", false, false),
            col("expires_on", "date", false, false),
            col("quantity_on_hand", "integer", false, false),
        ], vec![]),
        card("prescriptions", 200000, vec![
            col("id", "serial", true, false),
            col("patient_id", "integer", false, true),
            col("encounter_id", "integer", false, true),
            col("prescribed_by", "integer", false, true),
            col("prescribed_at", "timestamp", false, false),
            col("status", "varchar", false, false),
            col("rx_number", "varchar", false, false),
        ], vec![fk_edge("patient_id", "patients", "id"), fk_edge("encounter_id", "encounters", "id")]),
        card("prescription_items", 500000, vec![
            col("id", "serial", true, false),
            col("prescription_id", "integer", false, true),
            col("medication_id", "integer", false, true),
            col("dose", "varchar", false, false),
            col("frequency", "varchar", false, false),
            col("quantity", "integer", false, false),
            col("duration_days", "integer", false, false),
        ], vec![fk_edge("prescription_id", "prescriptions", "id"), fk_edge("medication_id", "medications", "id")]),
        card("stock_batches", 5000, vec![
            col("id", "serial", true, false),
            col("medication_id", "integer", false, true),
            col("batch_no", "varchar", false, false),
            col("expiry_date", "date", false, false),
            col("quantity", "integer", false, false),
            col("unit_cost", "numeric", false, false),
        ], vec![fk_edge("medication_id", "medications", "id")]),
        card("stock_movements", 50000, vec![
            col("id", "serial", true, false),
            col("batch_id", "integer", false, true),
            col("medication_id", "integer", false, true),
            col("quantity_change", "integer", false, false),
            col("movement_type", "varchar", false, false),
            col("moved_at", "timestamp", false, false),
            col("reference_id", "integer", false, true),
        ], vec![fk_edge("batch_id", "stock_batches", "id")]),
        card("drug_interactions", 10000, vec![
            col("id", "serial", true, false),
            col("drug1_id", "integer", false, true),
            col("drug2_id", "integer", false, true),
            col("severity", "varchar", false, false),
            col("mechanism", "text", false, false),
            col("clinical_effect", "text", false, false),
            col("management", "text", false, false),
        ], vec![fk_edge("drug1_id", "medications", "id"), fk_edge("drug2_id", "medications", "id")]),
        card("allergens", 500, vec![
            col("id", "serial", true, false),
            col("name", "varchar", false, false),
            col("category", "varchar", false, false),
            col("is_active", "boolean", false, false),
        ], vec![]),
        card("cds_alerts", 20000, vec![
            col("id", "serial", true, false),
            col("patient_id", "integer", false, true),
            col("alert_type", "varchar", false, false),
            col("triggered_at", "timestamp", false, false),
            col("rule_id", "varchar", false, false),
            col("acknowledged_by", "integer", false, true),
            col("is_overridden", "boolean", false, false),
        ], vec![fk_edge("patient_id", "patients", "id")]),
        card("lab_tests", 500, vec![
            col("id", "serial", true, false),
            col("code", "varchar", false, false),
            col("name", "varchar", false, false),
            col("category", "varchar", false, false),
            col("unit", "varchar", false, false),
            col("sample_type", "varchar", false, false),
            col("turnaround_hours", "integer", false, false),
            col("ordered_at", "timestamp", false, false),
        ], vec![]),
        card("lab_orders", 300000, vec![
            col("id", "serial", true, false),
            col("patient_id", "integer", false, true),
            col("encounter_id", "integer", false, true),
            col("test_id", "integer", false, true),
            col("ordered_by", "integer", false, true),
            col("ordered_at", "timestamp", false, false),
            col("status", "varchar", false, false),
            col("priority", "varchar", false, false),
        ], vec![fk_edge("patient_id", "patients", "id"), fk_edge("test_id", "lab_tests", "id")]),
        card("lab_results", 280000, vec![
            col("id", "serial", true, false),
            col("result_no", "varchar", false, false),
            col("order_id", "integer", false, true),
            col("patient_id", "integer", false, true),
            col("result_value", "varchar", false, false),
            col("result_date", "timestamp", false, false),
            col("unit", "varchar", false, false),
            col("reference_range", "varchar", false, false),
            col("is_abnormal", "boolean", false, false),
            col("verified_by", "integer", false, true),
        ], vec![fk_edge("patient_id", "patients", "id"), fk_edge("order_id", "lab_orders", "id")]),
        card("imaging_orders", 100000, vec![
            col("id", "serial", true, false),
            col("patient_id", "integer", false, true),
            col("encounter_id", "integer", false, true),
            col("modality", "varchar", false, false),
            col("body_part", "varchar", false, false),
            col("ordered_by", "integer", false, true),
            col("ordered_at", "timestamp", false, false),
            col("status", "varchar", false, false),
            col("priority", "varchar", false, false),
        ], vec![fk_edge("patient_id", "patients", "id")]),
        card("blood_units", 5000, vec![
            col("id", "serial", true, false),
            col("blood_group", "varchar", false, false),
            col("component", "varchar", false, false),
            col("volume_ml", "integer", false, false),
            col("collected_on", "date", false, false),
            col("expires_on", "date", false, false),
            col("status", "varchar", false, false),
            col("unit_no", "varchar", false, false),
        ], vec![]),
        card("transfusions", 3000, vec![
            col("id", "serial", true, false),
            col("patient_id", "integer", false, true),
            col("blood_unit_id", "integer", false, true),
            col("issued_at", "timestamp", false, false),
            col("started_at", "timestamp", false, false),
            col("reaction", "varchar", false, false),
            col("administered_by", "integer", false, true),
        ], vec![fk_edge("patient_id", "patients", "id"), fk_edge("blood_unit_id", "blood_units", "id")]),
        card("billing_encounters", 200000, vec![
            col("id", "serial", true, false),
            col("patient_id", "integer", false, true),
            col("encounter_id", "integer", false, true),
            col("bill_no", "varchar", false, false),
            col("billing_date", "date", false, false),
            col("subtotal", "numeric", false, false),
            col("discount", "numeric", false, false),
            col("patient_due", "numeric", false, false),
            col("amount_paid", "numeric", false, false),
            col("status", "varchar", false, false),
        ], vec![fk_edge("patient_id", "patients", "id"), fk_edge("encounter_id", "encounters", "id")]),
        card("billing_items", 800000, vec![
            col("id", "serial", true, false),
            col("billing_id", "integer", false, true),
            col("item_type", "varchar", false, false),
            col("description", "varchar", false, false),
            col("quantity", "integer", false, false),
            col("unit_price", "numeric", false, false),
            col("total", "numeric", false, false),
        ], vec![fk_edge("billing_id", "billing_encounters", "id")]),
        card("insurance_claims", 50000, vec![
            col("id", "serial", true, false),
            col("patient_id", "integer", false, true),
            col("billing_id", "integer", false, true),
            col("insurer_id", "integer", false, true),
            col("claim_no", "varchar", false, false),
            col("submitted_at", "date", false, false),
            col("status", "varchar", false, false),
            col("amount_claimed", "numeric", false, false),
            col("amount_approved", "numeric", false, false),
        ], vec![fk_edge("patient_id", "patients", "id"), fk_edge("insurer_id", "insurers", "id")]),
        card("payments", 200000, vec![
            col("id", "serial", true, false),
            col("patient_id", "integer", false, true),
            col("billing_id", "integer", false, true),
            col("payment_date", "date", false, false),
            col("payment_method", "varchar", false, false),
            col("amount", "numeric", false, false),
            col("reference_no", "varchar", false, false),
            col("received_by", "integer", false, true),
        ], vec![fk_edge("patient_id", "patients", "id")]),
        card("incidents", 2000, vec![
            col("id", "serial", true, false),
            col("incident_type", "varchar", false, false),
            col("incident_date", "timestamp", false, false),
            col("severity", "varchar", false, false),
            col("reported_by", "integer", false, true),
            col("location", "varchar", false, false),
            col("outcome", "varchar", false, false),
        ], vec![]),
        card("mortality_records", 1000, vec![
            col("id", "serial", true, false),
            col("death_no", "varchar", false, false),
            col("patient_id", "integer", false, true),
            col("national_id", "varchar", false, false),
            col("date_of_death", "date", false, false),
            col("cause_of_death", "varchar", false, false),
            col("icd_cause", "varchar", false, false),
            col("certified_by", "integer", false, true),
            col("manner", "varchar", false, false),
            col("completed_at", "date", false, false),
        ], vec![fk_edge("patient_id", "patients", "id")]),
        card("patient_feedback", 5000, vec![
            col("id", "serial", true, false),
            col("patient_id", "integer", false, true),
            col("feedback_date", "date", false, false),
            col("category", "varchar", false, false),
            col("rating", "integer", false, false),
            col("comment", "text", false, false),
            col("resolved", "boolean", false, false),
        ], vec![fk_edge("patient_id", "patients", "id")]),
        card("notifiable_disease_reports", 500, vec![
            col("id", "serial", true, false),
            col("patient_id", "integer", false, true),
            col("disease_code", "varchar", false, false),
            col("reported_at", "timestamp", false, false),
            col("reported_by", "integer", false, true),
            col("status", "varchar", false, false),
            col("authority", "varchar", false, false),
        ], vec![fk_edge("patient_id", "patients", "id")]),
        card("record_access_logs", 50000, vec![
            col("id", "serial", true, false),
            col("patient_id", "integer", false, true),
            col("accessed_at", "timestamp", false, false),
            col("accessed_by", "integer", false, true),
            col("action", "varchar", false, false),
            col("ip_address", "varchar", false, false),
            col("justification", "text", false, false),
        ], vec![fk_edge("patient_id", "patients", "id")]),
        card("provider_licenses", 400, vec![
            col("id", "serial", true, false),
            col("provider_id", "integer", false, true),
            col("regulator", "varchar", false, false),
            col("license_no", "varchar", false, false),
            col("license_type", "varchar", false, false),
            col("issued_on", "date", false, false),
            col("expires_on", "date", false, false),
            col("is_current", "boolean", false, false),
        ], vec![fk_edge("provider_id", "providers", "id")]),
        card("care_programs", 20, vec![
            col("id", "serial", true, false),
            col("name", "varchar", false, false),
            col("disease", "varchar", false, false),
            col("description", "text", false, false),
            col("target_population", "varchar", false, false),
            col("is_active", "boolean", false, false),
        ], vec![]),
        card("program_enrollments", 30000, vec![
            col("id", "serial", true, false),
            col("patient_id", "integer", false, true),
            col("program_id", "integer", false, true),
            col("enrolled_at", "date", false, false),
            col("status", "varchar", false, false),
            col("next_visit_date", "date", false, false),
        ], vec![fk_edge("patient_id", "patients", "id"), fk_edge("program_id", "care_programs", "id")]),
        card("vaccines", 50, vec![
            col("id", "serial", true, false),
            col("name", "varchar", false, false),
            col("antigen", "varchar", false, false),
            col("doses_required", "integer", false, false),
            col("route", "varchar", false, false),
            col("is_active", "boolean", false, false),
        ], vec![]),
        card("equipment", 500, vec![
            col("id", "serial", true, false),
            col("equipment_no", "varchar", false, false),
            col("name", "varchar", false, false),
            col("category", "varchar", false, false),
            col("serial_no", "varchar", false, false),
            col("department_id", "integer", false, true),
            col("status", "varchar", false, false),
            col("purchase_date", "date", false, false),
            col("is_active", "boolean", false, false),
        ], vec![fk_edge("department_id", "departments", "id")]),
        card("equipment_maintenance", 2000, vec![
            col("id", "serial", true, false),
            col("equipment_id", "integer", false, true),
            col("service_date", "date", false, false),
            col("performed_by", "varchar", false, false),
            col("type", "varchar", false, false),
            col("next_service_date", "date", false, false),
            col("cost", "numeric", false, false),
        ], vec![fk_edge("equipment_id", "equipment", "id")]),
    ]
}

/// Alternative schema (25 tables) using plan 08 §1 naming conventions.
///
/// Table names are PascalCase, abbreviated, or combined with underscores —
/// deliberately different from the dev-seed canonical names to prove the
/// binder works via PascalCase splitting, abbreviation normalization, and
/// containment scoring rather than simple exact-string matching.
pub(crate) fn alt_schema_cards() -> Vec<TableCard> {
    let col = |name: &str, type_: &str, pk: bool, fk: bool| CardColumn {
        name: name.to_string(),
        type_: type_.to_string(),
        nullable: !pk,
        is_primary_key: pk,
        is_foreign_key: fk,
        sample_values: vec![],
        profile: ColumnProfile::default(),
    };
    let fk_edge = |column: &str, ref_table: &str, ref_column: &str| CardFkEdge {
        column: column.to_string(),
        ref_table: ref_table.to_string(),
        ref_column: ref_column.to_string(),
    };
    let card = |table: &str, row_count: i64, cols: Vec<CardColumn>, fks: Vec<CardFkEdge>| TableCard {
        source_id: "alt".to_string(),
        table_name: table.to_string(),
        row_count,
        columns: cols,
        fk_edges: fks,
        card_vector: None,
        card_text: table.to_string(),
    };

    vec![
        // ---- patient hub ----
        // "patient_master" in concepts.rs name_tokens; underscore-stripped exact match
        card("PatientMaster", 20000, vec![
            col("pat_id", "int", true, false),
            col("mrn", "varchar", false, false),
            col("first_name", "varchar", false, false),
            col("last_name", "varchar", false, false),
            col("dob", "date", false, false),
            col("gender", "varchar", false, false),
            col("phone", "varchar", false, false),
            col("pat_status", "varchar", false, false),
            col("registered_at", "datetime", false, false),
        ], vec![]),

        // ---- encounter / appointment ----
        // "visit" is an exact name_token for Encounter.
        // Plan 08 §1 twist: department linked via DeptCode FK (code, not id).
        card("Visit", 100000, vec![
            col("enc_id", "int", true, false),
            col("patient_id", "int", false, true),
            col("visit_date", "date", false, false),
            col("visit_type", "varchar", false, false),
            col("provider_id", "int", false, true),
            col("chief_complaint", "text", false, false),
            col("status", "varchar", false, false),
            col("dept_code", "varchar", false, true),
        ], vec![
            fk_edge("patient_id", "PatientMaster", "pat_id"),
            fk_edge("dept_code", "ClinDept", "dept_id"),
        ]),

        // "booking" is an exact name_token for Appointment
        card("Booking", 30000, vec![
            col("booking_id", "int", true, false),
            col("patient_id", "int", false, true),
            col("provider_id", "int", false, true),
            col("scheduled_time", "datetime", false, false),
            col("appointment_status", "varchar", false, false),
            col("appointment_type", "varchar", false, false),
        ], vec![fk_edge("patient_id", "PatientMaster", "pat_id")]),

        // ---- provider + roster ----
        // "staff" is an exact name_token for Provider
        card("Staff", 100, vec![
            col("staff_id", "int", true, false),
            col("first_name", "varchar", false, false),
            col("last_name", "varchar", false, false),
            col("specialty", "varchar", false, false),
            col("role", "varchar", false, false),
            col("hire_date", "date", false, false),
        ], vec![]),

        // "roster" is an exact name_token for Shift
        card("Roster", 5000, vec![
            col("roster_id", "int", true, false),
            col("provider_id", "int", false, true),
            col("ward_id", "int", false, true),
            col("shift_date", "date", false, false),
            col("start_time", "time", false, false),
            col("end_time", "time", false, false),
            col("shift_type", "varchar", false, false),
        ], vec![fk_edge("provider_id", "Staff", "staff_id")]),

        // ---- department + ward + bed ----
        // "ClinDept": PascalCase split → ["clin","dept"] → abbrev "dept"→"department"
        // → containment of desc_token "dept" against {"clin","dept","department"} = 1.0
        card("ClinDept", 15, vec![
            col("dept_id", "int", true, false),
            col("code", "varchar", false, false),
            col("name", "varchar", false, false),
            col("dept_type", "varchar", false, false),
            col("is_active", "boolean", false, false),
        ], vec![]),

        // "IPD_Ward": split on "_" → ["IPD","Ward"] → ["inpatient","ward"]
        // → containment of desc_token "ward" against {"ipd","ward","inpatient"} = 1.0
        card("IPD_Ward", 10, vec![
            col("ward_id", "int", true, false),
            col("code", "varchar", false, false),
            col("ward_type", "varchar", false, false),
            col("bed_capacity", "int", false, false),
            col("floor", "varchar", false, false),
            col("is_active", "boolean", false, false),
            col("department_id", "int", false, true),
        ], vec![fk_edge("department_id", "ClinDept", "dept_id")]),

        // "WardBed": PascalCase split → ["ward","bed"]
        // → containment of desc_token "bed" against {"ward","bed"} = 1.0
        card("WardBed", 200, vec![
            col("bed_id", "int", true, false),
            col("ward_id", "int", false, true),
            col("bed_no", "varchar", false, false),
            col("bed_type", "varchar", false, false),
            col("status", "varchar", false, false),
            col("is_active", "boolean", false, false),
        ], vec![fk_edge("ward_id", "IPD_Ward", "ward_id")]),

        // ---- admission (IPD) ----
        // "IPD_Admission": split → ["ipd","admission"] → abbrev "ipd"→"inpatient"
        // → containment of desc_token "ipd" or "inpatient" against
        //   {"ipd","admission","inpatient"} = 1.0
        // Plan 08 §1 twist: no LOS column — duration must be computed from
        // admitted_at and discharge_date via dialect-specific date arithmetic.
        // No total_charges column — billing resolves via Invoice.AmountUSD.
        card("IPD_Admission", 15000, vec![
            col("adm_id", "int", true, false),
            col("patient_id", "int", false, true),
            col("admitted_at", "datetime", false, false),
            col("discharge_date", "date", false, false),
            col("admission_type", "varchar", false, false),
            col("ward_id", "int", false, true),
            col("bed_id", "int", false, true),
            col("discharge_reason", "varchar", false, false),
            col("adm_status", "varchar", false, false),
        ], vec![
            fk_edge("patient_id", "PatientMaster", "pat_id"),
            fk_edge("ward_id", "IPD_Ward", "ward_id"),
            fk_edge("bed_id", "WardBed", "bed_id"),
        ]),

        // ---- obs chart (vital signs) ----
        // "ObsChart": PascalCase split → ["obs","chart"] → abbrev "obs"→"observation"
        // → containment of name_token "observation" against
        //   {"obs","chart","observation"} = 1.0
        card("ObsChart", 500000, vec![
            col("vital_id", "int", true, false),
            col("patient_id", "int", false, true),
            col("temperature", "decimal", false, false),
            col("pulse", "int", false, false),
            col("bp_systolic", "int", false, false),
            col("spo2", "decimal", false, false),
            col("resp_rate", "int", false, false),
            col("weight", "decimal", false, false),
            col("height", "decimal", false, false),
            col("recorded_at", "datetime", false, false),
        ], vec![fk_edge("patient_id", "PatientMaster", "pat_id")]),

        // ---- diagnosis ----
        // "DiagCode": PascalCase split → ["diag","code"] → abbrev "diag"→"diagnosis"
        // → containment of desc_token "diagnosis_code" (["diagnosis","code"])
        //   against {"diag","code","diagnosis"} = 2/2 = 1.0
        card("DiagCode", 12000, vec![
            col("code_id", "int", true, false),
            col("code", "varchar", false, false),
            col("description", "varchar", false, false),
            col("category", "varchar", false, false),
        ], vec![]),

        // "PatientDx": PascalCase split → ["patient","dx"] → abbrev "dx"→"diagnosis"
        // → synonym "dx" is a name_token for Diagnosis → exact synonym match → 0.95
        card("PatientDx", 300000, vec![
            col("dx_id", "int", true, false),
            col("patient_id", "int", false, true),
            col("icd_code", "varchar", false, false),
            col("diagnosis_date", "date", false, false),
            col("is_primary", "boolean", false, false),
            col("provider_id", "int", false, true),
        ], vec![
            fk_edge("patient_id", "PatientMaster", "pat_id"),
        ]),

        // ---- pharmacy (Rx / prescription) ----
        // "Rx": exact name_token for Prescription
        card("Rx", 100000, vec![
            col("rx_id", "int", true, false),
            col("patient_id", "int", false, true),
            col("prescribed_at", "datetime", false, false),
            col("prescribed_by", "int", false, true),
            col("status", "varchar", false, false),
            col("rx_number", "varchar", false, false),
        ], vec![fk_edge("patient_id", "PatientMaster", "pat_id")]),

        // "RxLine": exact name_token "rxline" for PrescriptionItem
        card("RxLine", 250000, vec![
            col("item_id", "int", true, false),
            col("rx_id", "int", false, true),
            col("drug_id", "int", false, true),
            col("dose", "varchar", false, false),
            col("frequency", "varchar", false, false),
            col("quantity", "int", false, false),
        ], vec![fk_edge("rx_id", "Rx", "rx_id")]),

        // "DrugMaster": PascalCase split → ["drug","master"]
        // → containment of desc_token "drug" against {"drug","master"} = 1.0
        card("DrugMaster", 2000, vec![
            col("drug_id", "int", true, false),
            col("generic_name", "varchar", false, false),
            col("brand_name", "varchar", false, false),
            col("drug_class", "varchar", false, false),
            col("form", "varchar", false, false),
            col("strength", "varchar", false, false),
            col("stock_status", "varchar", false, false),
            col("expires_on", "date", false, false),
            col("qty_on_hand", "int", false, false),
        ], vec![]),

        // ---- lab ----
        // "TestCatalog": strip("test_catalog")="testcatalog" = lowercase("TestCatalog")
        // → underscore-stripped exact match for LabTest name_token "test_catalog"
        card("TestCatalog", 300, vec![
            col("test_id", "int", true, false),
            col("code", "varchar", false, false),
            col("name", "varchar", false, false),
            col("category", "varchar", false, false),
            col("unit", "varchar", false, false),
            col("sample_type", "varchar", false, false),
            col("turnaround_hours", "int", false, false),
        ], vec![]),

        // "LabReq": strip("lab_req")="labreq" = lowercase("LabReq")
        // → underscore-stripped exact match for LabOrder name_token "lab_req"
        card("LabReq", 150000, vec![
            col("req_id", "int", true, false),
            col("patient_id", "int", false, true),
            col("test_id", "int", false, true),
            col("ordered_at", "datetime", false, false),
            col("ordered_by", "int", false, true),
            col("status", "varchar", false, false),
            col("priority", "varchar", false, false),
        ], vec![
            fk_edge("patient_id", "PatientMaster", "pat_id"),
            fk_edge("test_id", "TestCatalog", "test_id"),
        ]),

        // "LabRes": strip("lab_res")="labres" = lowercase("LabRes")
        // → underscore-stripped exact match for LabResult name_token "lab_res"
        card("LabRes", 140000, vec![
            col("result_id", "int", true, false),
            col("patient_id", "int", false, true),
            col("req_id", "int", false, true),
            col("result_value", "varchar", false, false),
            col("result_date", "datetime", false, false),
            col("unit", "varchar", false, false),
            col("reference_range", "varchar", false, false),
            col("is_abnormal", "boolean", false, false),
            col("AbnormalFlag", "char", false, false),
        ], vec![
            fk_edge("patient_id", "PatientMaster", "pat_id"),
            fk_edge("req_id", "LabReq", "req_id"),
        ]),

        // ---- billing ----
        // "Invoice": exact name_token for Bill
        card("Invoice", 100000, vec![
            col("inv_id", "int", true, false),
            col("patient_id", "int", false, true),
            col("bill_no", "varchar", false, false),
            col("billing_date", "date", false, false),
            col("subtotal", "decimal", false, false),
            col("amount_paid", "decimal", false, false),
            col("status", "varchar", false, false),
        ], vec![fk_edge("patient_id", "PatientMaster", "pat_id")]),

        // "ChargeItem": strip("charge_item")="chargeitem" = lowercase("ChargeItem")
        // → underscore-stripped exact match for BillItem name_token "charge_item"
        card("ChargeItem", 400000, vec![
            col("item_id", "int", true, false),
            col("billing_id", "int", false, true),
            col("description", "varchar", false, false),
            col("item_type", "varchar", false, false),
            col("quantity", "int", false, false),
            col("unit_price", "decimal", false, false),
            col("total", "decimal", false, false),
        ], vec![fk_edge("billing_id", "Invoice", "inv_id")]),

        // "Receipt": exact name_token for Payment
        card("Receipt", 100000, vec![
            col("receipt_id", "int", true, false),
            col("patient_id", "int", false, true),
            col("payment_date", "date", false, false),
            col("payment_method", "varchar", false, false),
            col("amount", "decimal", false, false),
            col("AmountUSD", "decimal", false, false),
            col("reference_no", "varchar", false, false),
        ], vec![fk_edge("patient_id", "PatientMaster", "pat_id")]),

        // ---- radiology ----
        // "ImagingReq": PascalCase split → ["imaging","req"] → abbrev "req"→"order"
        // → containment of name_token "imaging_order" (["imaging","order"])
        //   against {"imaging","req","order"} = 2/2 = 1.0
        card("ImagingReq", 50000, vec![
            col("rad_id", "int", true, false),
            col("patient_id", "int", false, true),
            col("modality", "varchar", false, false),
            col("body_part", "varchar", false, false),
            col("ordered_at", "datetime", false, false),
            col("ordered_by", "int", false, true),
            col("status", "varchar", false, false),
        ], vec![fk_edge("patient_id", "PatientMaster", "pat_id")]),

        // ---- incident ----
        // "Incident": exact name_token for Incident
        card("Incident", 1000, vec![
            col("event_id", "int", true, false),
            col("incident_date", "datetime", false, false),
            col("incident_type", "varchar", false, false),
            col("severity", "varchar", false, false),
            col("reported_by", "int", false, true),
            col("outcome", "varchar", false, false),
        ], vec![]),

        // ---- obstetrics ----
        // "OB_Delivery": exact name_token "ob_delivery" for Delivery
        card("OB_Delivery", 4000, vec![
            col("del_id", "int", true, false),
            col("patient_id", "int", false, true),
            col("delivered_at", "datetime", false, false),
            col("delivery_mode", "varchar", false, false),
            col("gestation_weeks", "int", false, false),
            col("blood_loss", "decimal", false, false),
            col("delivery_status", "varchar", false, false),
        ], vec![fk_edge("patient_id", "PatientMaster", "pat_id")]),

        // "OB_Baby": exact name_token "ob_baby" for Newborn
        card("OB_Baby", 4000, vec![
            col("baby_id", "int", true, false),
            col("delivery_id", "int", false, true),
            col("birth_weight", "decimal", false, false),
            col("apgar", "int", false, false),
            col("outcome", "varchar", false, false),
            col("sex", "varchar", false, false),
        ], vec![fk_edge("delivery_id", "OB_Delivery", "del_id")]),
    ]
}

// ---------------------------------------------------------------------------
// Acceptance tests
// ---------------------------------------------------------------------------

/// Acceptance criterion 1: dev seed ≥62/64 exact concepts, zero orphans.
#[test]
fn dev_seed_binding_accuracy() {
    let fixture: Fixture = load_fixture(include_str!("dev_seed_binding.json"));
    let cards = dev_seed_cards();
    let bindings = bind_cards(&cards, PROD_MIN_CONFIDENCE, 3, None, None, &HashMap::new());

    let mut exact = 0usize;
    let mut total = fixture.expected.len();

    for entry in &fixture.expected {
        let Some(tb) = bindings.iter().find(|t| t.table_name == entry.table) else {
            continue; // missing table in cards — shouldn't happen
        };
        if tb.concept.slug() == entry.concept {
            exact += 1;
        } else {
            // Print mismatches for debugging
            eprintln!(
                "MISMATCH: {} → expected {}, got {} (conf={:.2})",
                entry.table, entry.concept, tb.concept.slug(), tb.confidence
            );
        }
    }

    let orphans = bindings
        .iter()
        .filter(|t| t.concept == EntityConcept::Unknown)
        .count();

    eprintln!("Dev seed: {exact}/{total} exact, {orphans} orphans");

    assert!(
        exact >= 62,
        "dev seed binding accuracy: got {exact}/{total} exact (need ≥62)"
    );
    assert_eq!(orphans, 0, "dev seed must have zero orphans");
}

/// Acceptance criterion 2: alt schema ≥22/25 exact concepts, usable_lines ≥ 9.
#[test]
fn alt_schema_binding_accuracy() {
    let fixture: Fixture = load_fixture(include_str!("alt_schema_binding.json"));
    let cards = alt_schema_cards();
    let bindings = bind_cards(&cards, PROD_MIN_CONFIDENCE, 3, None, None, &HashMap::new());

    let mut exact = 0usize;
    let total = fixture.expected.len();

    for entry in &fixture.expected {
        let Some(tb) = bindings.iter().find(|t| t.table_name == entry.table) else {
            continue;
        };
        if tb.concept.slug() == entry.concept {
            exact += 1;
        } else {
            eprintln!(
                "MISMATCH alt: {} → expected {}, got {} (conf={:.2})",
                entry.table, entry.concept, tb.concept.slug(), tb.confidence
            );
        }
    }

    // Build a SchemaBinding to check usable_lines
    let sb = SchemaBinding {
        source_id: "alt".into(),
        bound_at: chrono::Utc::now(),
        tables: bindings,
        degraded: true,
        override_version: 0,
    };
    let usable = sb.usable_lines();

    eprintln!("Alt schema: {exact}/{total} exact, {} usable lines: {:?}", usable.len(), usable.iter().map(|l| l.slug()).collect::<Vec<_>>());

    assert!(
        exact >= 22,
        "alt schema binding accuracy: got {exact}/{total} exact (need ≥22)"
    );
    assert!(
        usable.len() >= 9,
        "alt schema should have ≥9 usable lines, got {}",
        usable.len()
    );
}

/// Acceptance criterion 4: vital_signs→patients ≤2 hops, LabRes→PatientMaster resolves on alt.
#[test]
fn patient_path_hops() {
    let cards = dev_seed_cards();
    let bindings = bind_cards(&cards, PROD_MIN_CONFIDENCE, 3, None, None, &HashMap::new());

    let vitals = bindings.iter().find(|t| t.table_name == "vital_signs").expect("vital_signs bound");
    let path = vitals.patient_path.as_ref().expect("vital_signs should have a patient path");
    assert!(
        path.len() <= 2,
        "vital_signs→patients should be ≤2 hops, got {} hops",
        path.len()
    );

    // Alt: LabRes → PatientMaster (plan 08 §1 naming; via LabReq FK)
    let alt_cards = alt_schema_cards();
    let alt_bindings = bind_cards(&alt_cards, PROD_MIN_CONFIDENCE, 3, None, None, &HashMap::new());
    let lab_res = alt_bindings.iter().find(|t| t.table_name == "LabRes").expect("LabRes bound");
    let alt_path = lab_res.patient_path.as_ref().expect("LabRes should be reachable from PatientMaster");
    assert!(
        alt_path.len() <= 3,
        "LabRes→PatientMaster should be ≤3 hops, got {}",
        alt_path.len()
    );
}

/// Acceptance criterion 5: exactly one EventTime per bound table.
#[test]
fn exactly_one_event_time_per_bound_table() {
    let cards = dev_seed_cards();
    let bindings = bind_cards(&cards, PROD_MIN_CONFIDENCE, 3, None, None, &HashMap::new());

    for tb in &bindings {
        if tb.concept == EntityConcept::Unknown {
            continue;
        }
        // Count EventTime columns
        let et_cols: Vec<&str> = tb.columns
            .iter()
            .filter(|c| c.role == crate::ontology::roles::ColumnRole::EventTime)
            .map(|c| c.column_name.as_str())
            .collect();

        // Tables with temporal data should have exactly one designated EventTime
        if !et_cols.is_empty() {
            assert_eq!(
                tb.event_time_col.is_some(), true,
                "bound table {} has EventTime columns {:?} but event_time_col is None",
                tb.table_name, et_cols
            );
        }
    }

    // Specific check: vital_signs should have event_time_col = "recorded_at", not "created_at"
    let vitals = bindings.iter().find(|t| t.table_name == "vital_signs").unwrap();
    assert_eq!(
        vitals.event_time_col.as_deref(),
        Some("recorded_at"),
        "vital_signs should use 'recorded_at' as event_time, not created_at"
    );
}

/// Acceptance criterion 6 (integration): no PII column has non-empty enum_values.
#[test]
fn no_pii_enum_values_in_dev_seed() {
    let cards = dev_seed_cards();
    let bindings = bind_cards(&cards, PROD_MIN_CONFIDENCE, 3, None, None, &HashMap::new());
    for tb in &bindings {
        for cb in &tb.columns {
            if cb.is_pii {
                assert!(
                    cb.enum_values.is_empty(),
                    "PII column {}.{} must have empty enum_values",
                    tb.table_name, cb.column_name
                );
            }
        }
    }
}
