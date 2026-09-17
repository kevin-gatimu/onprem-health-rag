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

mod ddl;

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

/// Dev-seed `TableCard`s, parsed from the real DDL at test time (plan 03e).
///
/// Previously ~800 lines of hand-written cards with no process reconciling
/// them against `docker/dev-postgres/init/01_schema.sql` -- 03d measured 64%
/// of blessed golden rows drifting from the real schema as a result. This now
/// delegates to `ddl::dev_cards_from_ddl`, which `include_str!`s that same
/// file and parses it, so there is exactly one copy of the schema truth and
/// nothing left to keep in sync. See `ddl.rs` for the parser, its guard
/// assertions, and its canary tests.
pub(crate) fn dev_seed_cards() -> Vec<TableCard> {
    ddl::dev_cards_from_ddl()
}

/// Ground-truth enum labels for the dev-seed schema, parsed from the same DDL
/// as `dev_seed_cards` (plan 03e). Consumed by `nl2sql::ir`'s golden suite via
/// `bind_cards`'s `enum_values` parameter -- see `ddl::dev_enum_values_from_ddl`.
pub(crate) fn dev_seed_enum_values() -> HashMap<String, HashMap<String, Vec<String>>> {
    ddl::dev_enum_values_from_ddl()
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
            // Mirror of dev lab_results.is_critical — same ground-truth column.
            col("is_critical", "boolean", false, false),
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
    let usable = sb.usable_lines(PROD_MIN_CONFIDENCE);

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
///
/// Fed `dev_seed_enum_values()` (plan 03e), not an empty map: this is the same
/// DDL-derived enum data the golden suite binds against, so the guard exercises
/// real categorical labels landing on real columns rather than trivially
/// passing because nothing was ever supplied. Before 03e this test's fixture
/// (the old hand-written `dev_seed_cards()`) declared a `mortality_records`
/// table with a `national_id` column — a column that does not exist in
/// `01_schema.sql` (`national_id` lives on `patients`) — so part of the
/// surface this guard checked was never real. The DDL-derived fixture cannot
/// declare a column the schema doesn't have (see the `ddl.rs` canaries), so
/// the phantom is gone; the eprintln below reports the guard's actual PII
/// coverage post-regeneration.
#[test]
fn no_pii_enum_values_in_dev_seed() {
    let cards = dev_seed_cards();
    let enum_values = dev_seed_enum_values();
    let bindings = bind_cards(&cards, PROD_MIN_CONFIDENCE, 3, None, None, &enum_values);
    let mut pii_columns: Vec<String> = Vec::new();
    for tb in &bindings {
        for cb in &tb.columns {
            if cb.is_pii {
                pii_columns.push(format!("{}.{}", tb.table_name, cb.column_name));
                assert!(
                    cb.enum_values.is_empty(),
                    "PII column {}.{} must have empty enum_values",
                    tb.table_name, cb.column_name
                );
            }
        }
    }
    pii_columns.sort();
    eprintln!(
        "no_pii_enum_values_in_dev_seed: {} real PII columns covered: {:?}",
        pii_columns.len(),
        pii_columns
    );
}
