# Fixture Drift Audit: `dev_seed_cards()` vs `01_schema.sql`

**Scope:** Read-only inspection of three files.
**Ground truth:** `docker/dev-postgres/init/01_schema.sql`
**Fixture:** `onprem-rag-server/src/ontology/tests/mod.rs` — `dev_seed_cards()` and `alt_schema_cards()`
**Golden suite:** `onprem-rag-server/src/nl2sql/ir/tests/golden.jsonl`

---

## Bottom line first

**Blast radius: 18 out of 28 blessed dev rows.** Sixty-four percent of the blessed set references names that do not exist in the real schema. The recommendation is to correct the fixture and re-bless those 18 rows in one deliberate pass. Evidence follows.

---

## 1. Table-name drift

The fixture uses 12 table names that differ from the real `CREATE TABLE` name.

| Fixture table | Real table | Basis for match | Real `CREATE TABLE` line |
|---|---|---|---|
| `staff_shifts` | `provider_shifts` | Identical columns (shift_date, shift_type, ward_id, provider_id); comment on real table: "Ward duty rota" | 934 |
| `medication_administrations` | `medication_administration` | Same columns (administered_at, administered_by, dose, route, admin_status equivalent); trailing-s plural only | 598 |
| `medications` | `medication_catalog` | Columns generic_name, brand_name, drug_class, atc_code, form, strength all match | 173 |
| `allergens` | `allergen_catalog` | name column, class/category column, cross-reactivity concept | 201 |
| `allergies` | `patient_allergies` | patient_id FK, allergen FK, severity, is_active | 264 |
| `medical_history` | `patient_medical_history` | patient_id FK, condition/condition_name, onset, is_resolved/is_active | 277 |
| `family_history` | `patient_family_history` | patient_id FK, relation, condition, is_deceased | 289 |
| `insurers` | `insurance_providers` | name, scheme_type/type, is_active, claims email | 697 |
| `lab_tests` | `lab_test_catalog` | name/test_name, category/panel_name, unit/result_unit, turnaround_hours | 208 |
| `incidents` | `incident_reports` | severity, reported_by, incident_type/category, incident_date/occurred_at | 985 |
| `record_access_logs` | `record_access_log` | patient_id FK, accessed_at, accessed_by/provider_id, action/access_type | 1114 |
| `vaccines` | `vaccine_catalog` | name, doses_required, route, is_active (close match) | 223 |

No fixture table could not be matched to a real table.

### Real tables not modeled under any fixture name

All 47 tables in `01_schema.sql` are represented in the fixture under some name. No real table referenced by a golden row is absent from the fixture entirely.

---

## 2. Column-name drift

Only tables that appear in blessed golden rows are fully analysed below. Tables present only in null-SQL rows are summarised at the end of this section.

### 2a. Renamed — same concept, different name

| Table | Fixture column | Fixture type | Real column | Real type | Real line |
|---|---|---|---|---|---|
| `appointments` | `appointment_status` | varchar | `status` | `appointment_status` ENUM | 323 |
| `appointments` | `scheduled_at` | timestamp | `scheduled_start` | TIMESTAMPTZ | 321 |
| `appointments` | `scheduled_time` | timestamp | `scheduled_start` | TIMESTAMPTZ | 321 |
| `admissions` | `admitted_at` | timestamp | `admission_date` | TIMESTAMPTZ | 452 |
| `admissions` | `discharge_reason` | varchar | `discharge_type` | varchar CHECK | 461 |
| `lab_results` | `result_date` | timestamp | `resulted_at` | TIMESTAMPTZ | 645 |
| `lab_results` | `unit` | varchar | `result_unit` | varchar | 639 |
| `referrals` | `referred_by` | integer | `referring_provider_id` | INTEGER FK | 502 |
| `referrals` | `referral_date` | date | `referred_on` | DATE | 511 |
| `referrals` | `referral_closed_at` | timestamp | `responded_on` | DATE | 512 |
| `prescriptions` | `prescribed_by` | integer | `prescriber_id` | INTEGER FK | 571 |
| `prescriptions` | `prescribed_at` | timestamp | `issue_date` | DATE | 573 |
| `payments` | `amount` | numeric | `amount_kes` | NUMERIC(10,2) | 769 |
| `deliveries` | `blood_loss` | numeric | `blood_loss_ml` | INTEGER | 813 |
| `deliveries` | `delivery_status` | varchar | `outcome` | varchar CHECK | 816 |
| `newborns` | `birth_weight` | numeric | `birth_weight_g` | INTEGER | 827 |
| `newborns` | `apgar` | integer | `apgar_1min` / `apgar_5min` | SMALLINT (split) | 831–832 |
| `vital_signs` | `temperature` | numeric | `temperature_c` | NUMERIC(4,1) | 393 |
| `vital_signs` | `pulse` | integer | `pulse_bpm` | SMALLINT | 394 |
| `vital_signs` | `spo2` | numeric | `spo2_pct` | NUMERIC(4,1) | 396 |
| `vital_signs` | `weight` | numeric | `weight_kg` | NUMERIC(5,1) | 398 |
| `vital_signs` | `height` | numeric | `height_cm` | NUMERIC(5,1) | 399 |
| `clinical_notes` | `provider_id` | integer | `author_id` | INTEGER FK | 431 |
| `clinical_notes` | `content` | text | `body` (+ SOAP cols) | TEXT | 439 |
| `clinical_notes` | `recorded_at` | timestamp | `note_datetime` | TIMESTAMPTZ | 434 |
| `diagnoses` | `icd_code` | varchar | `icd10_code` | VARCHAR(10) FK | 417 |
| `diagnoses` | `diagnosis_date` | date | `diagnosed_at` | TIMESTAMPTZ | 421 |
| `diagnoses` | `provider_id` | integer | `diagnosed_by` | INTEGER FK | 422 |
| `diagnoses` | `is_primary` | boolean | `dx_type` | varchar CHECK | 419 |
| `mortality_records` | `death_no` | varchar | `certificate_no` | VARCHAR(24) UNIQUE | 852 |
| `mortality_records` | `date_of_death` | date | `died_at` | TIMESTAMPTZ | 847 |
| `mortality_records` | `icd_cause` | varchar | `immediate_cause_code` / `underlying_cause_code` | VARCHAR FK (split) | 849–850 |
| `incidents` → `incident_reports` | `incident_type` | varchar | `category` | varchar CHECK | 994 |
| `incidents` → `incident_reports` | `incident_date` | timestamp | `occurred_at` | TIMESTAMPTZ | 993 |
| `record_access_logs` → `record_access_log` | `accessed_by` | integer | `provider_id` | INTEGER FK | 1116 |
| `record_access_logs` → `record_access_log` | `action` | varchar | `access_type` | varchar CHECK | 1120 |
| `record_access_logs` → `record_access_log` | `justification` | text | `reason` | VARCHAR(120) | 1121 |
| `equipment` | `equipment_no` | varchar | `asset_no` | VARCHAR(24) | 1041 |
| `staff_shifts` → `provider_shifts` | `start_time` | time | `starts_at` | TIMESTAMPTZ | 940 |
| `staff_shifts` → `provider_shifts` | `end_time` | time | `ends_at` | TIMESTAMPTZ | 941 |
| `lab_tests` → `lab_test_catalog` | `name` | varchar | `test_name` | VARCHAR(120) | 211 |
| `lab_tests` → `lab_test_catalog` | `unit` | varchar | `result_unit` | VARCHAR(40) | 214 |
| `lab_tests` → `lab_test_catalog` | `sample_type` | varchar | `specimen_type` | VARCHAR(60) | 213 |
| `medications` → `medication_catalog` | *(no `medication_no`)* | — | see §2b | — | — |
| `patient_insurance` | `insurance_provider` | integer | `provider_id` | INTEGER FK | 709 |
| `patient_insurance` | `policy_no` | varchar | `policy_number` | VARCHAR(50) | 710 |
| `billing_encounters` | `subtotal` | numeric | `subtotal_kes` | NUMERIC(10,2) | 728 |
| `billing_encounters` | `discount` | numeric | `discount_kes` | NUMERIC(10,2) | 729 |
| `billing_encounters` | `patient_due` | numeric | `patient_due_kes` | NUMERIC(10,2) | 731 |
| `billing_encounters` | `amount_paid` | numeric | `amount_paid_kes` | NUMERIC(10,2) | 732 |
| `insurance_claims` | `insurer_id` | integer | `insurance_id` | INTEGER FK | 753 |
| `insurance_claims` | `submitted_at` | date | `submitted_on` | DATE | 754 |
| `insurance_claims` | `amount_claimed` | numeric | `claimed_kes` | NUMERIC(10,2) | 755 |
| `insurance_claims` | `amount_approved` | numeric | `approved_kes` | NUMERIC(10,2) | 756 |
| `care_programs` | `disease` | varchar | `target_condition` | VARCHAR(120) | 1013 |
| `program_enrollments` | `enrolled_at` | date | `enrolled_on` | DATE | 1025 |
| `program_enrollments` | `next_visit_date` | date | `next_review_date` | DATE | 1029 |
| `notifiable_disease_reports` | `disease_code` | varchar | `icd10_code` | VARCHAR(10) FK | 1078 |
| `notifiable_disease_reports` | `reported_at` | timestamp | `reported_on` | DATE | 1084 |
| `bed_assignments` | `vacated_at` | timestamp | `released_at` | TIMESTAMPTZ | 479 |
| `patient_transfers` | `from_ward` | integer | `from_ward_id` | SMALLINT FK | 487 |
| `patient_transfers` | `to_ward` | integer | `to_ward_id` | SMALLINT FK | 488 |
| `patient_transfers` | `transfer_date` | timestamp | `transferred_at` | TIMESTAMPTZ | 489 |
| `patients` | `phone` | varchar | `phone_primary` | VARCHAR(20) | 243 |
| `allergens` → `allergen_catalog` | `category` | varchar | `allergen_class` | VARCHAR(80) | 203 |
| `stock_batches` | `quantity` | integer | `quantity_received` / `quantity_on_hand` | INTEGER (split) | 871–872 |
| `stock_batches` | `unit_cost` | numeric | `unit_cost_kes` | NUMERIC(8,2) | 873 |
| `equipment_maintenance` | `service_date` | date | `performed_on` | DATE | 1062 |
| `equipment_maintenance` | `type` | varchar | `maintenance_type` | varchar CHECK | 1061 |
| `equipment_maintenance` | `cost` | numeric | `cost_kes` | NUMERIC(10,2) | 1065 |

### 2b. Absent — fixture declares a column the real table does not have

| Table | Fixture column | Notes |
|---|---|---|
| `patients` | `patient_status` | No equivalent; real uses `is_active` (BOOLEAN) + `is_deceased` (BOOLEAN) separately |
| `admissions` | `admission_no` | Not in real schema; real has no surrogate booking number on this table |
| `admissions` | `admission_status` | Not in real schema; state is implied by `discharge_date IS NULL` |
| `admissions` | `total_charges` | Not in real schema; billing lives in `billing_encounters` |
| `admissions` | `bed_id` | Real schema uses `bed_no` VARCHAR(10), not a FK integer |
| `lab_results` | `result_no` | Not in real schema |
| `lab_tests` → `lab_test_catalog` | `code` | Not in real schema; uniqueness is (panel_name, test_name) |
| `lab_tests` → `lab_test_catalog` | `category` | Not in real schema; closest is `panel_name` |
| `lab_tests` → `lab_test_catalog` | `ordered_at` | Not in catalog; orders belong to `lab_orders` |
| `medications` → `medication_catalog` | `medication_no` | Not in real schema |
| `medications` → `medication_catalog` | `stock_status` | Not in real schema; stock is tracked per batch in `stock_batches` |
| `medications` → `medication_catalog` | `expires_on` | Not in real schema; expiry is per batch in `stock_batches.expiry_date` |
| `medications` → `medication_catalog` | `quantity_on_hand` | Not in real schema at catalog level; in `stock_batches.quantity_on_hand` per batch |
| `mortality_records` | `national_id` | On `patients`, not `mortality_records` |
| `mortality_records` | `cause_of_death` | Not in real schema; real uses `immediate_cause_code` (ICD FK) |
| `mortality_records` | `manner` | Not in real schema |
| `mortality_records` | `completed_at` | Not in real schema |
| `incidents` → `incident_reports` | `location` | Not in real schema; ward/department FK used instead |
| `incidents` → `incident_reports` | `outcome` | Not in real schema |
| `bed_assignments` | `patient_id` | Not in real schema; patient resolved via `admission_id → admissions` |
| `patient_transfers` | `from_bed` | Not in real schema |
| `patient_transfers` | `to_bed` | Not in real schema |
| `patient_transfers` | `patient_id` | Not in real schema; resolved via `admission_id` |
| `allergens` → `allergen_catalog` | `is_active` | Not in real schema |
| `notifiable_disease_reports` | `status` | Not in real schema; closest is `case_classification` (CHECK varchar) |
| `notifiable_disease_reports` | `authority` | Not in real schema; real uses `reported_to` VARCHAR |
| `insurance_claims` | `patient_id` | Not in real schema; patient reached via `billing_id → billing_encounters` |

### 2c. Type-mismatched — same column name, materially different type

| Table | Column | Fixture type | Real type | Real line | Risk |
|---|---|---|---|---|---|
| `appointments` | `status` (as `appointment_status`) | varchar | `appointment_status` ENUM | 323 | A literal filter like `status = 'no_show'` works on ENUM; but see §2a — column is renamed |
| `encounters` | `status` | varchar | varchar CHECK ('in_progress','completed','cancelled') | 358 | Low runtime risk; CHECK does not prevent literal match |
| `admissions` | `admission_type` | varchar | varchar CHECK ('elective','emergency','transfer','maternity','daycase') | 457 | Low risk; no golden rows filter on this column |
| `lab_results` | `is_abnormal` | boolean | BOOLEAN | 640 | No mismatch — identical |
| `beds` | `status` | varchar | `bed_status` ENUM | 96 | A literal filter would need the enum value to match; no golden rows touch this |

No same-name columns with materially incompatible types appear in blessed golden rows.

---

## 3. Blast radius

This section identifies which blessed rows in `golden.jsonl` (binding `"dev"`, at least one non-null `expect_sql_*`) reference a drifted name and would produce wrong or erroring SQL against the real database.

**Total blessed dev rows: 28**

| Row id | Drifted names used | Dialect cells affected |
|---|---|---|
| `pc-02` | `patients.patient_status` (absent) | 3 |
| `fd-01` | `appointments.appointment_status` (renamed → `status`) | 3 |
| `fd-02` | `referrals.referral_closed_at` (absent in real), `referrals.referral_date` (renamed → `referred_on`) | 3 |
| `fd-03` | `appointments.scheduled_at` (renamed → `scheduled_start`) | 3 |
| `wb-05` | `admissions.admitted_at` (renamed → `admission_date`) | 3 |
| `mat-02` | `deliveries.delivery_status` (renamed → `outcome`) | 3 |
| `mat-03` | `newborns.birth_weight` (renamed → `birth_weight_g`) | 3 |
| `dx-02` | table `lab_tests` (renamed → `lab_test_catalog`); column `ordered_at` (absent in catalog) | 3 |
| `dx-04` | `lab_results.result_date` (renamed → `resulted_at`) | 3 |
| `rev-03` | `payments.amount` (renamed → `amount_kes`) | 3 |
| `qs-01` | table `incidents` (renamed → `incident_reports`); `incident_date` (renamed → `occurred_at`) | 3 |
| `qs-03` | `mortality_records.death_no` (renamed → `certificate_no`); `national_id` (absent); `completed_at` (absent) | 3 |
| `qs-05` | table `incidents` (renamed → `incident_reports`); `incident_date` (renamed → `occurred_at`) | 3 |
| `wf-02` | table `staff_shifts` (renamed → `provider_shifts`) | 3 |
| `ph-02` | table `medications` (renamed → `medication_catalog`); `medication_no` (absent); `stock_status` (absent); `expires_on` (absent) | 3 |
| `ph-03` | `prescriptions.prescribed_at` (renamed → `issue_date`) | 3 |
| `gen-02` | `admissions.admission_no` (absent); `admissions.admission_status` (absent) | 3 |
| `fac-01` | `equipment.equipment_no` (renamed → `asset_no`) | 3 |

**Drifted rows: 18 of 28 (64%).** Each row has 3 dialect cells, so 54 SQL cells in total would need to be rewritten.

**Rows unaffected (10):**

| Row id | Why clean |
|---|---|
| `pc-01` | `COUNT(*)` only; no columns referenced |
| `pc-05` | `patients.registered_at` exists in real schema (line 259) |
| `wb-04` | `admissions.length_of_stay_days` is a generated column in real schema (lines 464–467) |
| `em-02` | `triage_assessments.triage_time` exists in real schema (line 377) |
| `mat-01` | `deliveries.delivered_at` exists in real schema (line 804) |
| `rev-05` | `billing_encounters.billing_date` exists in real schema (line 727) |
| `wf-04` | `COUNT(*)` only; no columns referenced |
| `gen-01` | `encounters.encounter_date` exists in real schema (line 347) |
| `gen-05` | `COUNT(*)` only; no columns referenced |
| `fac-03` | `COUNT(*)` only; no columns referenced |

### Counting method

Every row in `golden.jsonl` was checked for: (a) `"binding":"dev"`, (b) non-null `expect_sql_pg`. The pg SQL was then scanned for each table name and column name. A row was marked drifted if any referenced name is absent or renamed in `01_schema.sql`. The count of distinct `id` values is the blast radius. Dialect columns (mysql, mssql) were not double-counted — they move in lockstep with the pg cell on a single re-bless.

---

## 4. The alt fixture

`alt_schema_cards()` (lines 697–1053 of `mod.rs`) is **not** a representation of `01_schema.sql` and has no drift to report.

Evidence:

1. The docstring says explicitly: *"Alternative schema (25 tables) using plan 08 §1 naming conventions. Table names are PascalCase, abbreviated, or combined with underscores — deliberately different from the dev-seed canonical names to prove the binder works via PascalCase splitting, abbreviation normalization, and containment scoring rather than simple exact-string matching."*

2. Every table entry carries an inline comment explaining the scoring path by which the binder should resolve it (e.g. `"IPD_Ward"` → PascalCase split → ["IPD","Ward"] → abbrev "IPD"→"inpatient" → containment of "ward" = 1.0). The names are chosen to stress-test the normalizer, not to match any real database.

3. The `source_id` is `"alt"`, not `"dev"`. Golden rows using `"binding":"alt"` are explicitly exercising the alt schema.

4. Golden test `alt_schema_binding_accuracy` (line 1101) asserts only that ≥22/25 concepts are correctly bound — it makes no assertion that the table or column names match any ground-truth schema.

**Conclusion:** The alt fixture's names are correct by construction. Correcting the dev fixture has zero implication for the alt fixture and its blessed rows.

---

## 5. Recommendation and cost

### Recommend: correct the fixture and re-bless in one deliberate pass.

**Concrete cost:** 18 rows to re-bless (54 SQL cells, 3 dialects each). The work is mechanical: for each drifted row, update `dev_seed_cards()` to use the real table and column names, then run the IR against the corrected binding and record the new SQL. A single focused session is sufficient.

**What changes in the fixture:**

The largest-impact corrections are the table renames (which cascade to many column names):
- `staff_shifts` → `provider_shifts` (fixes wf-02)
- `incidents` → `incident_reports` (fixes qs-01, qs-05)
- `medications` → `medication_catalog` (fixes ph-02; requires removing absent columns and correcting the medication concept to point at catalog, not a stock summary)
- `lab_tests` → `lab_test_catalog` (fixes dx-02; `ordered_at` must move to `lab_orders`)
- Column-only renames for the remaining 14 rows

**What the project loses by not correcting:**

If the fixture is left as a self-consistent synthetic schema, the golden suite passes internally but the 18 blessed rows test SQL that cannot run against the real database. Any engineer who generates SQL via the NL2SQL path and tries to execute it against `health_records` will get "column does not exist" or "relation does not exist" errors for those 18 question types. The suite gives false assurance — it counts as verified what is actually broken in production. That is the material cost of the do-nothing option: 18 questions (no-show rate, shift queries, lab abnormalities, prescription counts, payment totals, incident counts, admission lookup, equipment lookup, mortality certification) produce SQL that silently fails on the real schema.

**Trade-off stated plainly:** Re-blessing 18 rows is a small, bounded effort. Leaving the drift in place makes the golden suite a test of an imaginary schema rather than the deployed one. The correction should be done.
