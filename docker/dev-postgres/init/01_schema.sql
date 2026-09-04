-- ============================================================================
-- Synthetic Hospital EMR/HIS — PostgreSQL 15+
-- Database: health_records (POSTGRES_DB in docker-compose.yml)
--
-- Models a district/referral hospital rather than a flat record dump: an
-- organisational spine (departments → wards → beds), a staffing spine
-- (providers → licences → clinic schedules → leave), a scheduling spine
-- (appointments → encounters), and a clinical spine (triage → notes → orders
-- → results → procedures → medication administration → discharge), plus
-- referrals, bed occupancy, revenue and clinical decision support.
--
-- Compatibility contract: the RAG server's deterministic SQL templates
-- (`nl2sql/routes.rs`) address `patients`, `encounters`, `diagnoses`,
-- `admissions`, `prescriptions`, `prescription_items`, `lab_orders`,
-- `lab_results`, `payments`, `providers`, `medication_catalog` and `counties`
-- by name, along with specific columns on them. Those names and columns are
-- preserved exactly. Everything new is additive.
-- ============================================================================

SET client_encoding = 'UTF8';

-- ── Enumerated types ─────────────────────────────────────────────────────────

CREATE TYPE provider_role   AS ENUM ('doctor','nurse','pharmacist','lab_tech','radiologist','admin','physiotherapist','nutritionist','counsellor');
CREATE TYPE drug_form       AS ENUM ('tablet','capsule','syrup','injection','inhaler','cream','drops','patch','suppository','powder');
CREATE TYPE interaction_severity AS ENUM ('mild','moderate','severe');
CREATE TYPE gender_type     AS ENUM ('male','female','other');
CREATE TYPE encounter_type  AS ENUM ('outpatient','inpatient','emergency','follow_up','telehealth','daycase');
CREATE TYPE priority_type   AS ENUM ('routine','urgent','stat');
CREATE TYPE billing_status  AS ENUM ('draft','billed','partial','paid','submitted','rejected','written_off');
CREATE TYPE appointment_status AS ENUM ('booked','confirmed','checked_in','in_progress','completed','cancelled','no_show','rescheduled');
CREATE TYPE referral_status AS ENUM ('pending','accepted','scheduled','completed','declined','expired');
CREATE TYPE bed_status      AS ENUM ('available','occupied','cleaning','maintenance','blocked');
CREATE TYPE note_type       AS ENUM ('progress','consultation','operative','discharge','nursing','admission','referral_letter');
CREATE TYPE admin_status    AS ENUM ('given','held','refused','missed','self_administered');

-- ── Reference tables ─────────────────────────────────────────────────────────

CREATE TABLE counties (
  id     SMALLINT PRIMARY KEY,
  name   VARCHAR(60)  NOT NULL,
  region VARCHAR(40)  NOT NULL
);
COMMENT ON TABLE counties IS 'Kenyan county lookup used for patient catchment analysis.';

CREATE TABLE icd10_codes (
  code        VARCHAR(10)  PRIMARY KEY,
  description VARCHAR(255) NOT NULL,
  category    VARCHAR(100) NOT NULL,
  chapter     VARCHAR(120),
  is_notifiable BOOLEAN NOT NULL DEFAULT FALSE
);
COMMENT ON TABLE icd10_codes IS 'ICD-10 diagnosis code reference. is_notifiable marks conditions reportable to public health.';

CREATE TABLE procedure_codes (
  code        VARCHAR(12)  PRIMARY KEY,
  description VARCHAR(255) NOT NULL,
  category    VARCHAR(80)  NOT NULL,
  is_surgical BOOLEAN      NOT NULL DEFAULT FALSE,
  typical_minutes SMALLINT
);
COMMENT ON TABLE procedure_codes IS 'Billable/clinical procedure reference used by procedures and surgeries.';

-- ── Organisation: departments, wards, beds ───────────────────────────────────

CREATE TABLE departments (
  id            SMALLSERIAL  PRIMARY KEY,
  code          VARCHAR(12)  UNIQUE NOT NULL,
  name          VARCHAR(80)  NOT NULL,
  dept_type     VARCHAR(20)  NOT NULL CHECK (dept_type IN ('clinical','diagnostic','support','administrative')),
  floor         VARCHAR(20),
  phone_ext     VARCHAR(10),
  head_provider_id INTEGER,          -- FK added after providers exists
  cost_centre   VARCHAR(20),
  is_active     BOOLEAN      NOT NULL DEFAULT TRUE
);
COMMENT ON TABLE departments IS 'Hospital departments. Clinical departments own clinics, wards and encounters.';

CREATE TABLE wards (
  id            SMALLSERIAL  PRIMARY KEY,
  code          VARCHAR(12)  UNIQUE NOT NULL,
  name          VARCHAR(60)  NOT NULL,
  department_id SMALLINT     NOT NULL REFERENCES departments(id),
  ward_type     VARCHAR(30)  NOT NULL CHECK (ward_type IN ('general','icu','hdu','maternity','paediatric','isolation','surgical','psychiatric','renal')),
  floor         VARCHAR(20),
  bed_capacity  SMALLINT     NOT NULL,
  is_active     BOOLEAN      NOT NULL DEFAULT TRUE
);
COMMENT ON TABLE wards IS 'Inpatient wards. bed_capacity is the licensed capacity; occupancy is derived from bed_assignments.';

CREATE TABLE beds (
  id         SERIAL       PRIMARY KEY,
  ward_id    SMALLINT     NOT NULL REFERENCES wards(id),
  bed_no     VARCHAR(10)  NOT NULL,
  bed_type   VARCHAR(20)  NOT NULL DEFAULT 'standard' CHECK (bed_type IN ('standard','cot','incubator','icu','isolation','delivery')),
  status     bed_status   NOT NULL DEFAULT 'available',
  is_active  BOOLEAN      NOT NULL DEFAULT TRUE,
  UNIQUE (ward_id, bed_no)
);
COMMENT ON TABLE beds IS 'Physical beds. status is the live state; historical occupancy lives in bed_assignments.';

-- ── Staffing ─────────────────────────────────────────────────────────────────

CREATE TABLE providers (
  id            SERIAL       PRIMARY KEY,
  employee_no   VARCHAR(20)  UNIQUE NOT NULL,
  first_name    VARCHAR(80)  NOT NULL,
  last_name     VARCHAR(80)  NOT NULL,
  specialty     VARCHAR(80),
  role          provider_role NOT NULL,
  department    VARCHAR(80),                       -- retained: denormalised name
  department_id SMALLINT     REFERENCES departments(id),
  job_title     VARCHAR(80),
  phone         VARCHAR(20),
  email         VARCHAR(150),
  gender        gender_type,
  hire_date     DATE,
  employment_type VARCHAR(20) CHECK (employment_type IN ('permanent','contract','locum','intern','visiting')),
  consultation_fee_kes NUMERIC(8,2),
  is_active     BOOLEAN      NOT NULL DEFAULT TRUE
);
COMMENT ON TABLE providers IS 'All clinical and non-clinical staff. role distinguishes doctors from nurses, pharmacists, lab and imaging staff.';

ALTER TABLE departments
  ADD CONSTRAINT departments_head_provider_fkey
  FOREIGN KEY (head_provider_id) REFERENCES providers(id);

CREATE TABLE provider_licenses (
  id            SERIAL       PRIMARY KEY,
  provider_id   INTEGER      NOT NULL REFERENCES providers(id),
  regulator     VARCHAR(80)  NOT NULL,
  license_no    VARCHAR(40)  NOT NULL,
  license_type  VARCHAR(60)  NOT NULL,
  issued_on     DATE         NOT NULL,
  expires_on    DATE         NOT NULL,
  is_current    BOOLEAN      NOT NULL DEFAULT TRUE,
  UNIQUE (regulator, license_no)
);
COMMENT ON TABLE provider_licenses IS 'Practising licences per clinician. Expiry drives compliance reporting.';

CREATE TABLE provider_schedules (
  id            SERIAL       PRIMARY KEY,
  provider_id   INTEGER      NOT NULL REFERENCES providers(id),
  department_id SMALLINT     NOT NULL REFERENCES departments(id),
  weekday       SMALLINT     NOT NULL CHECK (weekday BETWEEN 1 AND 7),   -- 1 = Monday
  start_time    TIME         NOT NULL,
  end_time      TIME         NOT NULL,
  slot_minutes  SMALLINT     NOT NULL DEFAULT 20,
  room          VARCHAR(20),
  clinic_name   VARCHAR(80),
  max_patients  SMALLINT,
  valid_from    DATE         NOT NULL,
  valid_to      DATE,
  is_active     BOOLEAN      NOT NULL DEFAULT TRUE,
  CHECK (end_time > start_time)
);
COMMENT ON TABLE provider_schedules IS 'Recurring weekly clinic sessions. Appointment slots are booked against these.';

CREATE TABLE provider_time_off (
  id           SERIAL       PRIMARY KEY,
  provider_id  INTEGER      NOT NULL REFERENCES providers(id),
  start_date   DATE         NOT NULL,
  end_date     DATE         NOT NULL,
  reason       VARCHAR(30)  NOT NULL CHECK (reason IN ('annual_leave','sick_leave','study_leave','conference','maternity','compassionate','off_duty')),
  is_approved  BOOLEAN      NOT NULL DEFAULT TRUE,
  notes        TEXT,
  CHECK (end_date >= start_date)
);
COMMENT ON TABLE provider_time_off IS 'Staff absence, so clinic capacity questions can account for who was actually available.';

-- ── Clinical reference catalogues ────────────────────────────────────────────

CREATE TABLE medication_catalog (
  id            SERIAL       PRIMARY KEY,
  generic_name  VARCHAR(150) NOT NULL,
  brand_name    VARCHAR(150),
  drug_class    VARCHAR(100) NOT NULL,
  atc_code      VARCHAR(10),
  form          drug_form    NOT NULL,
  strength      VARCHAR(50)  NOT NULL,
  unit          VARCHAR(20)  NOT NULL,
  route_default VARCHAR(20)  NOT NULL DEFAULT 'oral',
  unit_price_kes NUMERIC(8,2),
  is_controlled BOOLEAN      NOT NULL DEFAULT FALSE,
  is_formulary  BOOLEAN      NOT NULL DEFAULT TRUE,
  is_active     BOOLEAN      NOT NULL DEFAULT TRUE
);
COMMENT ON TABLE medication_catalog IS 'Hospital formulary.';

CREATE TABLE drug_interactions (
  id              SERIAL       PRIMARY KEY,
  drug1_id        INTEGER      NOT NULL REFERENCES medication_catalog(id),
  drug2_id        INTEGER      NOT NULL REFERENCES medication_catalog(id),
  severity        interaction_severity NOT NULL,
  mechanism       VARCHAR(255),
  clinical_effect TEXT         NOT NULL,
  management      TEXT,
  UNIQUE (drug1_id, drug2_id)
);

CREATE TABLE allergen_catalog (
  id               SERIAL       PRIMARY KEY,
  name             VARCHAR(150) NOT NULL,
  allergen_class   VARCHAR(80)  NOT NULL,
  cross_reactivity VARCHAR(255)
);

CREATE TABLE lab_test_catalog (
  id             SERIAL       PRIMARY KEY,
  panel_name     VARCHAR(120) NOT NULL,
  test_name      VARCHAR(120) NOT NULL,
  specimen_type  VARCHAR(60)  NOT NULL,
  result_unit    VARCHAR(40),
  ref_low        NUMERIC(10,3),
  ref_high       NUMERIC(10,3),
  ref_text       VARCHAR(80),
  turnaround_hours SMALLINT,
  price_kes      NUMERIC(8,2),
  UNIQUE (panel_name, test_name)
);
COMMENT ON TABLE lab_test_catalog IS 'Analyte reference ranges, so abnormal flags in lab_results are reproducible rather than arbitrary.';

CREATE TABLE vaccine_catalog (
  id            SERIAL       PRIMARY KEY,
  name          VARCHAR(100) NOT NULL,
  target_disease VARCHAR(100) NOT NULL,
  doses_required SMALLINT    NOT NULL DEFAULT 1,
  route         VARCHAR(20)  NOT NULL DEFAULT 'intramuscular',
  is_routine    BOOLEAN      NOT NULL DEFAULT TRUE
);

-- ── Patients ─────────────────────────────────────────────────────────────────

CREATE TABLE patients (
  id             UUID         PRIMARY KEY DEFAULT gen_random_uuid(),
  patient_no     VARCHAR(20)  UNIQUE NOT NULL,
  national_id    VARCHAR(20)  UNIQUE,
  first_name     VARCHAR(80)  NOT NULL,
  middle_name    VARCHAR(80),
  last_name      VARCHAR(80)  NOT NULL,
  date_of_birth  DATE         NOT NULL,
  gender         gender_type  NOT NULL,
  blood_type     VARCHAR(10)  NOT NULL DEFAULT 'unknown',
  phone_primary  VARCHAR(20),
  email          VARCHAR(150),
  address        TEXT,
  county_id      SMALLINT     REFERENCES counties(id),
  sub_county     VARCHAR(60),
  marital_status VARCHAR(20)  CHECK (marital_status IN ('single','married','divorced','widowed','separated')),
  occupation     VARCHAR(100),
  education_level VARCHAR(40),
  next_of_kin    VARCHAR(150),
  nok_phone      VARCHAR(20),
  nok_relation   VARCHAR(50),
  preferred_language VARCHAR(30),
  is_deceased    BOOLEAN      NOT NULL DEFAULT FALSE,
  date_of_death  DATE,
  is_active      BOOLEAN      NOT NULL DEFAULT TRUE,
  registered_at  TIMESTAMPTZ  NOT NULL DEFAULT NOW(),
  CHECK (date_of_death IS NULL OR is_deceased)
);
COMMENT ON TABLE patients IS 'Master patient index.';

CREATE TABLE patient_allergies (
  id           SERIAL       PRIMARY KEY,
  patient_id   UUID         NOT NULL REFERENCES patients(id),
  allergen_id  INTEGER      NOT NULL REFERENCES allergen_catalog(id),
  reaction     VARCHAR(255) NOT NULL,
  severity     VARCHAR(20)  NOT NULL CHECK (severity IN ('mild','moderate','severe','life_threatening')),
  onset_date   DATE,
  recorded_by  INTEGER      REFERENCES providers(id),
  is_active    BOOLEAN      NOT NULL DEFAULT TRUE,
  notes        TEXT,
  UNIQUE (patient_id, allergen_id)
);

CREATE TABLE patient_medical_history (
  id             SERIAL       PRIMARY KEY,
  patient_id     UUID         NOT NULL REFERENCES patients(id),
  icd10_code     VARCHAR(10)  REFERENCES icd10_codes(code),
  condition_name VARCHAR(255) NOT NULL,
  diagnosed_date DATE,
  resolved_date  DATE,
  is_chronic     BOOLEAN      NOT NULL DEFAULT FALSE,
  is_active      BOOLEAN      NOT NULL DEFAULT TRUE,
  notes          TEXT
);

CREATE TABLE patient_family_history (
  id             SERIAL       PRIMARY KEY,
  patient_id     UUID         NOT NULL REFERENCES patients(id),
  relation       VARCHAR(30)  NOT NULL,
  condition_name VARCHAR(255) NOT NULL,
  age_at_onset   SMALLINT,
  is_deceased    BOOLEAN      DEFAULT FALSE
);

CREATE TABLE immunizations (
  id            SERIAL       PRIMARY KEY,
  patient_id    UUID         NOT NULL REFERENCES patients(id),
  vaccine_id    INTEGER      NOT NULL REFERENCES vaccine_catalog(id),
  dose_number   SMALLINT     NOT NULL DEFAULT 1,
  administered_on DATE       NOT NULL,
  administered_by INTEGER    REFERENCES providers(id),
  batch_no      VARCHAR(30),
  site          VARCHAR(40),
  adverse_event TEXT
);
COMMENT ON TABLE immunizations IS 'Vaccination record, including catch-up doses for adults.';

-- ── Scheduling: appointments ─────────────────────────────────────────────────

CREATE TABLE appointments (
  id              UUID         PRIMARY KEY DEFAULT gen_random_uuid(),
  appointment_no  VARCHAR(20)  UNIQUE NOT NULL,
  patient_id      UUID         NOT NULL REFERENCES patients(id),
  provider_id     INTEGER      NOT NULL REFERENCES providers(id),
  department_id   SMALLINT     NOT NULL REFERENCES departments(id),
  schedule_id     INTEGER      REFERENCES provider_schedules(id),
  scheduled_start TIMESTAMPTZ  NOT NULL,
  scheduled_end   TIMESTAMPTZ  NOT NULL,
  appointment_type VARCHAR(24) NOT NULL CHECK (appointment_type IN ('new_visit','follow_up','review','procedure','telehealth','antenatal','vaccination','counselling')),
  status          appointment_status NOT NULL DEFAULT 'booked',
  booking_channel VARCHAR(20)  NOT NULL DEFAULT 'front_desk' CHECK (booking_channel IN ('front_desk','phone','online','walk_in','referral','ward_round')),
  booked_at       TIMESTAMPTZ  NOT NULL,
  booked_by       INTEGER      REFERENCES providers(id),
  reason          TEXT         NOT NULL,
  checked_in_at   TIMESTAMPTZ,
  seen_at         TIMESTAMPTZ,
  wait_minutes    INTEGER,
  encounter_id    UUID,                              -- FK added after encounters exists
  cancelled_at    TIMESTAMPTZ,
  cancellation_reason VARCHAR(120),
  rescheduled_from UUID        REFERENCES appointments(id),
  notes           TEXT,
  CHECK (scheduled_end > scheduled_start)
);
COMMENT ON TABLE appointments IS 'Patient bookings against a named provider and clinic session. The link to encounters records which bookings actually became visits.';

-- ── Encounters ───────────────────────────────────────────────────────────────

CREATE TABLE encounters (
  id              UUID         PRIMARY KEY DEFAULT gen_random_uuid(),
  encounter_no    VARCHAR(20)  UNIQUE NOT NULL,
  patient_id      UUID         NOT NULL REFERENCES patients(id),
  appointment_id  UUID         REFERENCES appointments(id),
  encounter_date  TIMESTAMPTZ  NOT NULL,
  encounter_type  encounter_type NOT NULL,
  department      VARCHAR(80),                        -- retained: denormalised name
  department_id   SMALLINT     REFERENCES departments(id),
  chief_complaint TEXT         NOT NULL,
  provider_id     INTEGER      NOT NULL REFERENCES providers(id),
  attending_provider_id INTEGER REFERENCES providers(id),
  referred_by_id  INTEGER      REFERENCES providers(id),
  ward            VARCHAR(50),
  bed_no          VARCHAR(10),
  arrival_mode    VARCHAR(20)  CHECK (arrival_mode IN ('walk_in','ambulance','referral','police','transfer')),
  status          VARCHAR(20)  NOT NULL DEFAULT 'completed' CHECK (status IN ('in_progress','completed','cancelled')),
  disposition     VARCHAR(30)  CHECK (disposition IN ('discharged_home','admitted','referred_out','died','left_without_being_seen','absconded')),
  discharge_date  TIMESTAMPTZ,
  discharge_notes TEXT,
  follow_up_date  DATE,
  created_at      TIMESTAMPTZ  NOT NULL DEFAULT NOW()
);
COMMENT ON TABLE encounters IS 'One clinical contact. provider_id is the clinician seen; attending_provider_id is the responsible consultant.';

ALTER TABLE appointments
  ADD CONSTRAINT appointments_encounter_fkey
  FOREIGN KEY (encounter_id) REFERENCES encounters(id);

CREATE TABLE triage_assessments (
  id             SERIAL       PRIMARY KEY,
  encounter_id   UUID         NOT NULL REFERENCES encounters(id) UNIQUE,
  patient_id     UUID         NOT NULL REFERENCES patients(id),
  triaged_by     INTEGER      NOT NULL REFERENCES providers(id),
  arrival_time   TIMESTAMPTZ  NOT NULL,
  triage_time    TIMESTAMPTZ  NOT NULL,
  seen_time      TIMESTAMPTZ,
  triage_category SMALLINT    NOT NULL CHECK (triage_category BETWEEN 1 AND 5),
  triage_colour  VARCHAR(10)  NOT NULL CHECK (triage_colour IN ('red','orange','yellow','green','blue')),
  presenting_complaint TEXT   NOT NULL,
  door_to_triage_minutes INTEGER,
  door_to_doctor_minutes INTEGER,
  notes          TEXT
);
COMMENT ON TABLE triage_assessments IS 'Emergency triage with the timing metrics an ED reports on.';

CREATE TABLE vital_signs (
  id              SERIAL       PRIMARY KEY,
  encounter_id    UUID         NOT NULL REFERENCES encounters(id),
  patient_id      UUID         REFERENCES patients(id),
  recorded_by     INTEGER      REFERENCES providers(id),
  temperature_c   NUMERIC(4,1),
  pulse_bpm       SMALLINT,
  resp_rate       SMALLINT,
  bp_systolic     SMALLINT,
  bp_diastolic    SMALLINT,
  spo2_pct        NUMERIC(4,1),
  weight_kg       NUMERIC(5,1),
  height_cm       NUMERIC(5,1),
  bmi             NUMERIC(4,1) GENERATED ALWAYS AS (
                    CASE WHEN height_cm > 0
                    THEN ROUND(weight_kg / POWER(height_cm/100.0, 2), 1)
                    ELSE NULL END) STORED,
  pain_score      SMALLINT CHECK (pain_score BETWEEN 0 AND 10),
  gcs_score       SMALLINT CHECK (gcs_score BETWEEN 3 AND 15),
  blood_glucose   NUMERIC(5,2),
  news2_score     SMALLINT,
  recorded_at     TIMESTAMPTZ  NOT NULL DEFAULT NOW()
);
COMMENT ON TABLE vital_signs IS 'Observations taken at an encounter. news2_score is the aggregate early-warning score.';

CREATE TABLE diagnoses (
  id             SERIAL       PRIMARY KEY,
  encounter_id   UUID         NOT NULL REFERENCES encounters(id),
  patient_id     UUID         NOT NULL REFERENCES patients(id),
  icd10_code     VARCHAR(10)  NOT NULL REFERENCES icd10_codes(code),
  diagnosis_desc TEXT         NOT NULL,
  dx_type        VARCHAR(15)  NOT NULL DEFAULT 'primary' CHECK (dx_type IN ('primary','secondary','tertiary','differential')),
  diagnosed_by   INTEGER      REFERENCES providers(id),
  diagnosed_at   TIMESTAMPTZ  NOT NULL DEFAULT NOW(),
  certainty      VARCHAR(15)  CHECK (certainty IN ('confirmed','probable','suspected','ruled_out')),
  is_active      BOOLEAN      NOT NULL DEFAULT TRUE,
  resolved_at    TIMESTAMPTZ,
  notes          TEXT
);

CREATE TABLE clinical_notes (
  id            SERIAL       PRIMARY KEY,
  encounter_id  UUID         NOT NULL REFERENCES encounters(id),
  patient_id    UUID         NOT NULL REFERENCES patients(id),
  author_id     INTEGER      NOT NULL REFERENCES providers(id),
  note_type     note_type    NOT NULL,
  note_datetime TIMESTAMPTZ  NOT NULL,
  subjective    TEXT,
  objective     TEXT,
  assessment    TEXT,
  plan          TEXT,
  body          TEXT         NOT NULL,
  is_signed     BOOLEAN      NOT NULL DEFAULT TRUE,
  signed_at     TIMESTAMPTZ
);
COMMENT ON TABLE clinical_notes IS 'Narrative documentation in SOAP form. The richest free text in the record and the main target for semantic retrieval.';

-- ── Admissions, beds and transfers ───────────────────────────────────────────

CREATE TABLE admissions (
  id              SERIAL       PRIMARY KEY,
  encounter_id    UUID         NOT NULL REFERENCES encounters(id) UNIQUE,
  patient_id      UUID         NOT NULL REFERENCES patients(id),
  admitting_dr    INTEGER      NOT NULL REFERENCES providers(id),
  attending_dr    INTEGER      REFERENCES providers(id),
  admission_date  TIMESTAMPTZ  NOT NULL,
  ward            VARCHAR(50)  NOT NULL,                 -- retained: denormalised name
  ward_id         SMALLINT     REFERENCES wards(id),
  bed_no          VARCHAR(10),
  admission_type  VARCHAR(20)  NOT NULL CHECK (admission_type IN ('elective','emergency','transfer','maternity','daycase')),
  admission_source VARCHAR(30) CHECK (admission_source IN ('emergency_dept','outpatient_clinic','referral','theatre','other_facility','maternity')),
  admitting_dx    TEXT         NOT NULL,
  is_readmission_30d BOOLEAN   NOT NULL DEFAULT FALSE,
  discharge_date  TIMESTAMPTZ,
  discharge_type  VARCHAR(30)  CHECK (discharge_type IN ('home','transfer','deceased','absconded','against_advice')),
  discharge_summary TEXT,
  length_of_stay_days INTEGER GENERATED ALWAYS AS (
    CASE WHEN discharge_date IS NOT NULL
    THEN EXTRACT(DAY FROM discharge_date - admission_date)::INTEGER
    ELSE NULL END) STORED,
  CHECK (discharge_date IS NULL OR discharge_date >= admission_date)
);
COMMENT ON TABLE admissions IS 'Inpatient stays. length_of_stay_days is derived and can never be negative.';

CREATE TABLE bed_assignments (
  id            SERIAL       PRIMARY KEY,
  admission_id  INTEGER      NOT NULL REFERENCES admissions(id),
  bed_id        INTEGER      NOT NULL REFERENCES beds(id),
  ward_id       SMALLINT     NOT NULL REFERENCES wards(id),
  assigned_at   TIMESTAMPTZ  NOT NULL,
  released_at   TIMESTAMPTZ,
  assigned_by   INTEGER      REFERENCES providers(id),
  CHECK (released_at IS NULL OR released_at >= assigned_at)
);
COMMENT ON TABLE bed_assignments IS 'Which bed a patient occupied and when. Drives occupancy and bed-days.';

CREATE TABLE patient_transfers (
  id             SERIAL       PRIMARY KEY,
  admission_id   INTEGER      NOT NULL REFERENCES admissions(id),
  from_ward_id   SMALLINT     REFERENCES wards(id),
  to_ward_id     SMALLINT     NOT NULL REFERENCES wards(id),
  transferred_at TIMESTAMPTZ  NOT NULL,
  reason         VARCHAR(120) NOT NULL,
  authorised_by  INTEGER      REFERENCES providers(id)
);
COMMENT ON TABLE patient_transfers IS 'Ward-to-ward movement during a stay (e.g. ICU step-down).';

-- ── Referrals ────────────────────────────────────────────────────────────────

CREATE TABLE referrals (
  id              SERIAL       PRIMARY KEY,
  referral_no     VARCHAR(20)  UNIQUE NOT NULL,
  patient_id      UUID         NOT NULL REFERENCES patients(id),
  encounter_id    UUID         REFERENCES encounters(id),
  referring_provider_id INTEGER NOT NULL REFERENCES providers(id),
  from_department_id SMALLINT  REFERENCES departments(id),
  to_department_id   SMALLINT  REFERENCES departments(id),
  to_provider_id  INTEGER      REFERENCES providers(id),
  external_facility VARCHAR(150),
  direction       VARCHAR(10)  NOT NULL CHECK (direction IN ('internal','outbound','inbound')),
  urgency         priority_type NOT NULL DEFAULT 'routine',
  reason          TEXT         NOT NULL,
  clinical_summary TEXT,
  status          referral_status NOT NULL DEFAULT 'pending',
  referred_on     DATE         NOT NULL,
  responded_on    DATE,
  outcome         TEXT,
  CHECK (to_department_id IS NOT NULL OR external_facility IS NOT NULL)
);
COMMENT ON TABLE referrals IS 'Internal specialty referrals and transfers to/from other facilities.';

-- ── Procedures and surgery ───────────────────────────────────────────────────

CREATE TABLE procedures (
  id             SERIAL       PRIMARY KEY,
  encounter_id   UUID         NOT NULL REFERENCES encounters(id),
  patient_id     UUID         NOT NULL REFERENCES patients(id),
  procedure_code VARCHAR(12)  NOT NULL REFERENCES procedure_codes(code),
  procedure_name VARCHAR(255) NOT NULL,
  performed_by   INTEGER      NOT NULL REFERENCES providers(id),
  performed_at   TIMESTAMPTZ  NOT NULL,
  location       VARCHAR(60),
  anaesthesia    VARCHAR(30)  CHECK (anaesthesia IN ('none','local','regional','spinal','general','sedation')),
  outcome        VARCHAR(30)  CHECK (outcome IN ('successful','partial','abandoned','complication')),
  complication_note TEXT,
  notes          TEXT
);
COMMENT ON TABLE procedures IS 'Bedside, clinic and theatre procedures.';

CREATE TABLE surgeries (
  id              SERIAL       PRIMARY KEY,
  procedure_id    INTEGER      REFERENCES procedures(id),
  encounter_id    UUID         NOT NULL REFERENCES encounters(id),
  patient_id      UUID         NOT NULL REFERENCES patients(id),
  theatre_no      VARCHAR(10)  NOT NULL,
  primary_surgeon INTEGER      NOT NULL REFERENCES providers(id),
  assistant_surgeon INTEGER    REFERENCES providers(id),
  anaesthetist_id INTEGER      REFERENCES providers(id),
  scrub_nurse_id  INTEGER      REFERENCES providers(id),
  urgency         VARCHAR(15)  NOT NULL CHECK (urgency IN ('elective','urgent','emergency')),
  asa_grade       SMALLINT     CHECK (asa_grade BETWEEN 1 AND 5),
  scheduled_start TIMESTAMPTZ  NOT NULL,
  actual_start    TIMESTAMPTZ,
  actual_end      TIMESTAMPTZ,
  duration_minutes INTEGER GENERATED ALWAYS AS (
    CASE WHEN actual_end IS NOT NULL AND actual_start IS NOT NULL
    THEN EXTRACT(EPOCH FROM actual_end - actual_start)::INTEGER / 60
    ELSE NULL END) STORED,
  blood_loss_ml   INTEGER,
  status          VARCHAR(20)  NOT NULL DEFAULT 'completed' CHECK (status IN ('scheduled','in_theatre','completed','cancelled','postponed')),
  cancellation_reason VARCHAR(120),
  operative_findings TEXT,
  CHECK (actual_end IS NULL OR actual_start IS NULL OR actual_end >= actual_start)
);
COMMENT ON TABLE surgeries IS 'Theatre list with the full surgical team, timings and utilisation data.';

-- ── Prescriptions and administration ─────────────────────────────────────────

CREATE TABLE prescriptions (
  id             UUID         PRIMARY KEY DEFAULT gen_random_uuid(),
  rx_number      VARCHAR(20)  UNIQUE NOT NULL,
  encounter_id   UUID         NOT NULL REFERENCES encounters(id),
  patient_id     UUID         NOT NULL REFERENCES patients(id),
  prescriber_id  INTEGER      NOT NULL REFERENCES providers(id),
  issue_date     DATE         NOT NULL,
  valid_until    DATE         NOT NULL,
  status         VARCHAR(20)  NOT NULL DEFAULT 'active' CHECK (status IN ('active','dispensed','partially_dispensed','cancelled','expired')),
  dispensed_by   INTEGER      REFERENCES providers(id),
  dispensed_at   TIMESTAMPTZ,
  notes          TEXT,
  created_at     TIMESTAMPTZ  NOT NULL DEFAULT NOW(),
  CHECK (valid_until >= issue_date)
);

CREATE TABLE prescription_items (
  id              SERIAL       PRIMARY KEY,
  prescription_id UUID         NOT NULL REFERENCES prescriptions(id),
  medication_id   INTEGER      NOT NULL REFERENCES medication_catalog(id),
  dosage          VARCHAR(50)  NOT NULL,
  frequency       VARCHAR(50)  NOT NULL,
  duration_days   SMALLINT,
  quantity        SMALLINT     NOT NULL,
  route           VARCHAR(20)  NOT NULL DEFAULT 'oral',
  instructions    TEXT,
  is_dispensed    BOOLEAN      NOT NULL DEFAULT FALSE,
  dispensed_at    TIMESTAMPTZ,
  unit_price_kes  NUMERIC(8,2),
  line_total_kes  NUMERIC(10,2)
);

CREATE TABLE medication_administration (
  id                  SERIAL       PRIMARY KEY,
  prescription_item_id INTEGER     NOT NULL REFERENCES prescription_items(id),
  admission_id        INTEGER      REFERENCES admissions(id),
  patient_id          UUID         NOT NULL REFERENCES patients(id),
  administered_by     INTEGER      REFERENCES providers(id),
  scheduled_at        TIMESTAMPTZ  NOT NULL,
  administered_at     TIMESTAMPTZ,
  dose_given          VARCHAR(50),
  route               VARCHAR(20),
  status              admin_status NOT NULL DEFAULT 'given',
  reason_not_given    VARCHAR(120),
  notes               TEXT
);
COMMENT ON TABLE medication_administration IS 'Medication administration record (MAR) for inpatients: what was actually given, held or missed.';

-- ── Lab ──────────────────────────────────────────────────────────────────────

CREATE TABLE lab_orders (
  id            UUID         PRIMARY KEY DEFAULT gen_random_uuid(),
  order_no      VARCHAR(20)  UNIQUE NOT NULL,
  encounter_id  UUID         NOT NULL REFERENCES encounters(id),
  patient_id    UUID         NOT NULL REFERENCES patients(id),
  ordered_by    INTEGER      NOT NULL REFERENCES providers(id),
  collected_by  INTEGER      REFERENCES providers(id),
  order_date    TIMESTAMPTZ  NOT NULL DEFAULT NOW(),
  collected_at  TIMESTAMPTZ,
  panel_name    VARCHAR(150) NOT NULL,
  priority      priority_type NOT NULL DEFAULT 'routine',
  status        VARCHAR(20)  NOT NULL DEFAULT 'resulted' CHECK (status IN ('ordered','collected','processing','resulted','cancelled')),
  specimen_type VARCHAR(80),
  price_kes     NUMERIC(8,2)
);

CREATE TABLE lab_results (
  id              SERIAL       PRIMARY KEY,
  order_id        UUID         NOT NULL REFERENCES lab_orders(id),
  patient_id      UUID         REFERENCES patients(id),
  test_name       VARCHAR(150) NOT NULL,
  result_value    VARCHAR(100) NOT NULL,
  result_numeric  NUMERIC(12,3),
  result_unit     VARCHAR(40),
  reference_range VARCHAR(80),
  is_abnormal     BOOLEAN      NOT NULL DEFAULT FALSE,
  abnormal_flag   VARCHAR(3)   CHECK (abnormal_flag IN ('L','LL','H','HH')),
  is_critical     BOOLEAN      NOT NULL DEFAULT FALSE,
  verified_by     INTEGER      REFERENCES providers(id),
  resulted_at     TIMESTAMPTZ  NOT NULL DEFAULT NOW(),
  notes           VARCHAR(255)
);
COMMENT ON TABLE lab_results IS 'Individual analyte results. patient_id is denormalised so per-patient result questions do not need a two-hop join.';

-- ── Imaging ──────────────────────────────────────────────────────────────────

CREATE TABLE imaging_orders (
  id            UUID         PRIMARY KEY DEFAULT gen_random_uuid(),
  order_no      VARCHAR(20)  UNIQUE NOT NULL,
  encounter_id  UUID         NOT NULL REFERENCES encounters(id),
  patient_id    UUID         NOT NULL REFERENCES patients(id),
  ordered_by    INTEGER      NOT NULL REFERENCES providers(id),
  modality      VARCHAR(20)  NOT NULL CHECK (modality IN ('X-Ray','CT','MRI','Ultrasound','Echocardiogram','Mammography','PET','Nuclear')),
  body_part     VARCHAR(80)  NOT NULL,
  indication    TEXT,
  priority      priority_type NOT NULL DEFAULT 'routine',
  order_date    TIMESTAMPTZ  NOT NULL DEFAULT NOW(),
  performed_at  TIMESTAMPTZ,
  status        VARCHAR(20)  NOT NULL DEFAULT 'resulted' CHECK (status IN ('ordered','scheduled','performed','resulted','cancelled')),
  findings      TEXT,
  impression    TEXT,
  report        TEXT,
  is_abnormal   BOOLEAN,
  radiologist   INTEGER      REFERENCES providers(id),
  reported_at   TIMESTAMPTZ,
  price_kes     NUMERIC(8,2)
);

-- ── Clinical decision support ────────────────────────────────────────────────

CREATE TABLE cds_alerts (
  id              SERIAL       PRIMARY KEY,
  patient_id      UUID         NOT NULL REFERENCES patients(id),
  encounter_id    UUID         REFERENCES encounters(id),
  alert_type      VARCHAR(30)  NOT NULL,
  severity        VARCHAR(10)  NOT NULL CHECK (severity IN ('info','warning','critical')),
  title           VARCHAR(255) NOT NULL,
  message         TEXT         NOT NULL,
  drug1_id        INTEGER      REFERENCES medication_catalog(id),
  drug2_id        INTEGER      REFERENCES medication_catalog(id),
  allergen_id     INTEGER      REFERENCES allergen_catalog(id),
  triggered_at    TIMESTAMPTZ  NOT NULL DEFAULT NOW(),
  triggered_for   INTEGER      REFERENCES providers(id),
  is_acknowledged BOOLEAN      NOT NULL DEFAULT FALSE,
  acknowledged_by INTEGER      REFERENCES providers(id),
  acknowledged_at TIMESTAMPTZ,
  override_reason TEXT
);

-- ── Billing and revenue ──────────────────────────────────────────────────────

CREATE TABLE insurance_providers (
  id            SERIAL       PRIMARY KEY,
  name          VARCHAR(150) NOT NULL,
  short_name    VARCHAR(30),
  type          VARCHAR(20)  NOT NULL CHECK (type IN ('nhif','private','corporate','micro')),
  contact_phone VARCHAR(20),
  claim_email   VARCHAR(150),
  is_active     BOOLEAN      NOT NULL DEFAULT TRUE
);

CREATE TABLE patient_insurance (
  id            SERIAL       PRIMARY KEY,
  patient_id    UUID         NOT NULL REFERENCES patients(id),
  provider_id   INTEGER      NOT NULL REFERENCES insurance_providers(id),
  policy_number VARCHAR(50)  NOT NULL,
  member_no     VARCHAR(50),
  coverage_type VARCHAR(80),
  coverage_limit_kes NUMERIC(12,2),
  valid_from    DATE         NOT NULL,
  valid_to      DATE,
  is_primary    BOOLEAN      NOT NULL DEFAULT TRUE,
  is_active     BOOLEAN      NOT NULL DEFAULT TRUE
);

CREATE TABLE billing_encounters (
  id                  UUID         PRIMARY KEY DEFAULT gen_random_uuid(),
  bill_no             VARCHAR(20)  UNIQUE NOT NULL,
  encounter_id        UUID         NOT NULL REFERENCES encounters(id),
  patient_id          UUID         NOT NULL REFERENCES patients(id),
  insurance_id        INTEGER      REFERENCES patient_insurance(id),
  billing_date        DATE         NOT NULL,
  subtotal_kes        NUMERIC(10,2) NOT NULL DEFAULT 0,
  discount_kes        NUMERIC(10,2) NOT NULL DEFAULT 0,
  insurance_cover_kes NUMERIC(10,2) NOT NULL DEFAULT 0,
  patient_due_kes     NUMERIC(10,2) NOT NULL DEFAULT 0,
  amount_paid_kes     NUMERIC(10,2) NOT NULL DEFAULT 0,
  status              billing_status NOT NULL DEFAULT 'billed',
  notes               TEXT,
  created_at          TIMESTAMPTZ  NOT NULL DEFAULT NOW()
);

CREATE TABLE billing_items (
  id             SERIAL       PRIMARY KEY,
  billing_id     UUID         NOT NULL REFERENCES billing_encounters(id),
  item_type      VARCHAR(20)  NOT NULL CHECK (item_type IN ('consultation','lab','imaging','pharmacy','procedure','theatre','bed','nursing','consumable')),
  item_code      VARCHAR(30),
  description    VARCHAR(255) NOT NULL,
  quantity       SMALLINT     NOT NULL DEFAULT 1,
  unit_price_kes NUMERIC(8,2) NOT NULL,
  total_kes      NUMERIC(10,2) NOT NULL
);

CREATE TABLE insurance_claims (
  id             SERIAL       PRIMARY KEY,
  claim_no       VARCHAR(24)  UNIQUE NOT NULL,
  billing_id     UUID         NOT NULL REFERENCES billing_encounters(id),
  insurance_id   INTEGER      NOT NULL REFERENCES patient_insurance(id),
  submitted_on   DATE         NOT NULL,
  claimed_kes    NUMERIC(10,2) NOT NULL,
  approved_kes   NUMERIC(10,2),
  status         VARCHAR(20)  NOT NULL CHECK (status IN ('submitted','under_review','approved','partially_approved','rejected','paid')),
  decision_on    DATE,
  rejection_reason VARCHAR(255)
);
COMMENT ON TABLE insurance_claims IS 'Claim lifecycle per bill: what was claimed, what the insurer approved, and why anything was cut.';

CREATE TABLE payments (
  id             SERIAL       PRIMARY KEY,
  billing_id     UUID         NOT NULL REFERENCES billing_encounters(id),
  patient_id     UUID         REFERENCES patients(id),
  payment_date   TIMESTAMPTZ  NOT NULL DEFAULT NOW(),
  amount_kes     NUMERIC(10,2) NOT NULL,
  payment_method VARCHAR(20)  NOT NULL CHECK (payment_method IN ('cash','mpesa','bank_transfer','insurance','cheque','waiver')),
  received_by    INTEGER      REFERENCES providers(id),
  reference_no   VARCHAR(80),
  notes          VARCHAR(255)
);

-- MATERNITY: antenatal care, delivery, the newborn -----------------------------

CREATE TABLE antenatal_visits (
  id              SERIAL       PRIMARY KEY,
  patient_id      UUID         NOT NULL REFERENCES patients(id),
  encounter_id    UUID         REFERENCES encounters(id),
  visit_number    SMALLINT     NOT NULL,
  visit_date      DATE         NOT NULL,
  gestation_weeks SMALLINT     CHECK (gestation_weeks BETWEEN 4 AND 45),
  fundal_height_cm SMALLINT,
  fetal_heart_rate SMALLINT,
  presentation    VARCHAR(30),
  bp_systolic     SMALLINT,
  bp_diastolic    SMALLINT,
  weight_kg       NUMERIC(5,1),
  urine_protein   VARCHAR(20),
  haemoglobin     NUMERIC(4,1),
  risk_factors    TEXT,
  seen_by         INTEGER      REFERENCES providers(id),
  next_visit_date DATE,
  notes           TEXT
);
COMMENT ON TABLE antenatal_visits IS 'Antenatal clinic profile per pregnancy visit - the busiest outpatient service in most district hospitals.';

CREATE TABLE deliveries (
  id                SERIAL      PRIMARY KEY,
  patient_id        UUID        NOT NULL REFERENCES patients(id),
  encounter_id      UUID        NOT NULL REFERENCES encounters(id),
  admission_id      INTEGER     REFERENCES admissions(id),
  delivered_at      TIMESTAMPTZ NOT NULL,
  delivery_mode     VARCHAR(30) NOT NULL CHECK (delivery_mode IN ('spontaneous_vaginal','caesarean','vacuum_assisted','forceps','breech')),
  labour_onset      VARCHAR(15) CHECK (labour_onset IN ('spontaneous','induced','no_labour')),
  gestation_weeks   SMALLINT,
  labour_hours      NUMERIC(4,1),
  delivered_by      INTEGER     NOT NULL REFERENCES providers(id),
  anaesthesia       VARCHAR(30),
  episiotomy        BOOLEAN     NOT NULL DEFAULT FALSE,
  perineal_tear     VARCHAR(20) CHECK (perineal_tear IN ('none','first_degree','second_degree','third_degree','fourth_degree')),
  blood_loss_ml     INTEGER,
  placenta_complete BOOLEAN,
  complications     TEXT,
  outcome           VARCHAR(20) NOT NULL CHECK (outcome IN ('live_birth','stillbirth','multiple_birth','maternal_death'))
);
COMMENT ON TABLE deliveries IS 'The delivery episode for the mother. One row per birth event; each baby is a row in newborns.';

CREATE TABLE newborns (
  id                  SERIAL      PRIMARY KEY,
  delivery_id         INTEGER     NOT NULL REFERENCES deliveries(id),
  mother_patient_id   UUID        NOT NULL REFERENCES patients(id),
  patient_id          UUID        REFERENCES patients(id),
  birth_order         SMALLINT    NOT NULL DEFAULT 1,
  sex                 gender_type NOT NULL,
  birth_weight_g      INTEGER,
  length_cm           NUMERIC(4,1),
  head_circumference_cm NUMERIC(4,1),
  apgar_1min          SMALLINT    CHECK (apgar_1min BETWEEN 0 AND 10),
  apgar_5min          SMALLINT    CHECK (apgar_5min BETWEEN 0 AND 10),
  resuscitation_required BOOLEAN  NOT NULL DEFAULT FALSE,
  admitted_to_nbu     BOOLEAN     NOT NULL DEFAULT FALSE,
  outcome             VARCHAR(20) NOT NULL CHECK (outcome IN ('live','stillborn','neonatal_death')),
  birth_notification_no VARCHAR(24) UNIQUE,
  notes               TEXT
);
COMMENT ON TABLE newborns IS 'Babies delivered here, linked to the mother. Birth weight and Apgar drive neonatal quality reporting.';

-- MORTALITY --------------------------------------------------------------------

CREATE TABLE mortality_records (
  id                    SERIAL      PRIMARY KEY,
  patient_id            UUID        NOT NULL REFERENCES patients(id),
  encounter_id          UUID        REFERENCES encounters(id),
  admission_id          INTEGER     REFERENCES admissions(id),
  died_at               TIMESTAMPTZ NOT NULL,
  place_of_death        VARCHAR(30) NOT NULL CHECK (place_of_death IN ('ward','icu','casualty','theatre','brought_in_dead','maternity','home')),
  immediate_cause_code  VARCHAR(10) REFERENCES icd10_codes(code),
  underlying_cause_code VARCHAR(10) REFERENCES icd10_codes(code),
  contributing_causes   TEXT,
  certified_by          INTEGER     NOT NULL REFERENCES providers(id),
  certificate_no        VARCHAR(24) UNIQUE,
  autopsy_requested     BOOLEAN     NOT NULL DEFAULT FALSE,
  autopsy_performed     BOOLEAN     NOT NULL DEFAULT FALSE,
  mortuary_admitted_at  TIMESTAMPTZ,
  body_released_at      TIMESTAMPTZ,
  notified_to_registrar BOOLEAN     NOT NULL DEFAULT FALSE,
  notes                 TEXT
);
COMMENT ON TABLE mortality_records IS 'Death certification with immediate and underlying cause. Required for civil registration and mortality audit.';

-- PHARMACY STOCK ---------------------------------------------------------------

CREATE TABLE stock_batches (
  id               SERIAL      PRIMARY KEY,
  medication_id    INTEGER     NOT NULL REFERENCES medication_catalog(id),
  batch_no         VARCHAR(30) NOT NULL,
  expiry_date      DATE        NOT NULL,
  quantity_received INTEGER    NOT NULL,
  quantity_on_hand INTEGER     NOT NULL,
  unit_cost_kes    NUMERIC(8,2),
  supplier         VARCHAR(120),
  received_on      DATE        NOT NULL,
  received_by      INTEGER     REFERENCES providers(id),
  store            VARCHAR(20) NOT NULL DEFAULT 'main' CHECK (store IN ('main','ward','theatre','emergency')),
  UNIQUE (medication_id, batch_no)
);
COMMENT ON TABLE stock_batches IS 'Pharmacy stock by batch and expiry. Stock-outs and expiring stock are daily operational questions.';

CREATE TABLE stock_movements (
  id             SERIAL      PRIMARY KEY,
  medication_id  INTEGER     NOT NULL REFERENCES medication_catalog(id),
  batch_id       INTEGER     REFERENCES stock_batches(id),
  movement_type  VARCHAR(20) NOT NULL CHECK (movement_type IN ('receipt','dispense','issue_to_ward','return','adjustment','expiry','disposal')),
  quantity       INTEGER     NOT NULL,
  moved_at       TIMESTAMPTZ NOT NULL,
  moved_by       INTEGER     REFERENCES providers(id),
  prescription_id UUID       REFERENCES prescriptions(id),
  reason         VARCHAR(120),
  balance_after  INTEGER
);
COMMENT ON TABLE stock_movements IS 'Every movement of stock in or out, so consumption and wastage trace back to a prescription.';

-- BLOOD BANK -------------------------------------------------------------------

CREATE TABLE blood_units (
  id              SERIAL      PRIMARY KEY,
  unit_no         VARCHAR(24) UNIQUE NOT NULL,
  blood_group     VARCHAR(10) NOT NULL,
  component       VARCHAR(20) NOT NULL CHECK (component IN ('whole_blood','packed_cells','platelets','fresh_frozen_plasma','cryoprecipitate')),
  volume_ml       INTEGER     NOT NULL,
  source          VARCHAR(30) NOT NULL CHECK (source IN ('voluntary_donation','replacement_donation','regional_blood_centre')),
  collected_on    DATE        NOT NULL,
  expires_on      DATE        NOT NULL,
  screening_status VARCHAR(15) NOT NULL DEFAULT 'passed' CHECK (screening_status IN ('pending','passed','failed')),
  status          VARCHAR(15) NOT NULL DEFAULT 'available' CHECK (status IN ('available','reserved','issued','transfused','discarded','expired')),
  stored_in       VARCHAR(30),
  CHECK (expires_on > collected_on)
);
COMMENT ON TABLE blood_units IS 'Blood bank inventory by component and expiry.';

CREATE TABLE transfusions (
  id              SERIAL      PRIMARY KEY,
  patient_id      UUID        NOT NULL REFERENCES patients(id),
  encounter_id    UUID        NOT NULL REFERENCES encounters(id),
  admission_id    INTEGER     REFERENCES admissions(id),
  blood_unit_id   INTEGER     NOT NULL REFERENCES blood_units(id),
  requested_by    INTEGER     NOT NULL REFERENCES providers(id),
  cross_match_result VARCHAR(20) CHECK (cross_match_result IN ('compatible','incompatible','emergency_release')),
  indication      VARCHAR(150) NOT NULL,
  issued_at       TIMESTAMPTZ,
  started_at      TIMESTAMPTZ,
  completed_at    TIMESTAMPTZ,
  volume_transfused_ml INTEGER,
  administered_by INTEGER     REFERENCES providers(id),
  reaction        VARCHAR(30) NOT NULL DEFAULT 'none' CHECK (reaction IN ('none','febrile','allergic','haemolytic','circulatory_overload','bacterial')),
  reaction_notes  TEXT
);
COMMENT ON TABLE transfusions IS 'Transfusion episodes with cross-match outcome and any reaction - a core patient-safety record.';

-- DUTY ROTA --------------------------------------------------------------------

CREATE TABLE provider_shifts (
  id            SERIAL      PRIMARY KEY,
  provider_id   INTEGER     NOT NULL REFERENCES providers(id),
  ward_id       SMALLINT    REFERENCES wards(id),
  department_id SMALLINT    REFERENCES departments(id),
  shift_date    DATE        NOT NULL,
  shift_type    VARCHAR(15) NOT NULL CHECK (shift_type IN ('day','evening','night','on_call','long_day')),
  starts_at     TIMESTAMPTZ NOT NULL,
  ends_at       TIMESTAMPTZ NOT NULL,
  role_on_shift VARCHAR(40),
  is_in_charge  BOOLEAN     NOT NULL DEFAULT FALSE,
  CHECK (ends_at > starts_at)
);
COMMENT ON TABLE provider_shifts IS 'Ward duty rota. Distinct from provider_schedules, which covers outpatient clinic sessions.';

-- DOCUMENTS AND CONSENT --------------------------------------------------------

CREATE TABLE patient_documents (
  id           SERIAL       PRIMARY KEY,
  patient_id   UUID         NOT NULL REFERENCES patients(id),
  encounter_id UUID         REFERENCES encounters(id),
  doc_type     VARCHAR(30)  NOT NULL CHECK (doc_type IN ('consent','referral_letter','lab_report','imaging_report','discharge_summary','insurance','identification','birth_notification','death_certificate','other')),
  title        VARCHAR(200) NOT NULL,
  file_name    VARCHAR(200) NOT NULL,
  mime_type    VARCHAR(80),
  size_bytes   INTEGER,
  uploaded_by  INTEGER      REFERENCES providers(id),
  uploaded_at  TIMESTAMPTZ  NOT NULL,
  is_signed    BOOLEAN      NOT NULL DEFAULT FALSE,
  notes        TEXT
);
COMMENT ON TABLE patient_documents IS 'Scanned and generated documents attached to the record.';

CREATE TABLE consents (
  id            SERIAL      PRIMARY KEY,
  patient_id    UUID        NOT NULL REFERENCES patients(id),
  encounter_id  UUID        REFERENCES encounters(id),
  procedure_id  INTEGER     REFERENCES procedures(id),
  consent_type  VARCHAR(30) NOT NULL CHECK (consent_type IN ('surgery','anaesthesia','transfusion','hiv_test','photography','data_sharing','postmortem','research')),
  granted       BOOLEAN     NOT NULL,
  granted_by    VARCHAR(20) NOT NULL CHECK (granted_by IN ('patient','guardian','next_of_kin','court_order')),
  witness_id    INTEGER     REFERENCES providers(id),
  signed_at     TIMESTAMPTZ NOT NULL,
  expires_on    DATE,
  withdrawn_at  TIMESTAMPTZ,
  notes         TEXT
);
COMMENT ON TABLE consents IS 'Informed consent, including withdrawal. Surgery and transfusion cannot proceed without it.';

-- PATIENT SAFETY ---------------------------------------------------------------

CREATE TABLE incident_reports (
  id              SERIAL      PRIMARY KEY,
  incident_no     VARCHAR(20) UNIQUE NOT NULL,
  patient_id      UUID        REFERENCES patients(id),
  encounter_id    UUID        REFERENCES encounters(id),
  ward_id         SMALLINT    REFERENCES wards(id),
  department_id   SMALLINT    REFERENCES departments(id),
  reported_by     INTEGER     NOT NULL REFERENCES providers(id),
  occurred_at     TIMESTAMPTZ NOT NULL,
  reported_at     TIMESTAMPTZ NOT NULL,
  category        VARCHAR(30) NOT NULL CHECK (category IN ('medication_error','patient_fall','pressure_ulcer','needlestick','equipment_failure','documentation','aggression','delay_in_care','wrong_site','infection','near_miss','other')),
  severity        VARCHAR(15) NOT NULL CHECK (severity IN ('no_harm','low','moderate','severe','death')),
  description     TEXT        NOT NULL,
  immediate_action TEXT,
  investigation_status VARCHAR(20) NOT NULL DEFAULT 'open' CHECK (investigation_status IN ('open','under_review','closed')),
  root_cause      TEXT,
  actions_taken   TEXT,
  closed_at       TIMESTAMPTZ,
  CHECK (reported_at >= occurred_at)
);
COMMENT ON TABLE incident_reports IS 'Patient-safety incident register, including near misses.';

-- CHRONIC CARE PROGRAMMES ------------------------------------------------------

CREATE TABLE care_programs (
  id                   SMALLSERIAL PRIMARY KEY,
  code                 VARCHAR(16) UNIQUE NOT NULL,
  name                 VARCHAR(100) NOT NULL,
  description          TEXT,
  target_condition     VARCHAR(120),
  review_interval_days SMALLINT,
  is_active            BOOLEAN     NOT NULL DEFAULT TRUE
);
COMMENT ON TABLE care_programs IS 'Longitudinal clinics (HIV comprehensive care, TB, diabetes, hypertension, MCH, palliative).';

CREATE TABLE program_enrollments (
  id               SERIAL      PRIMARY KEY,
  patient_id       UUID        NOT NULL REFERENCES patients(id),
  program_id       SMALLINT    NOT NULL REFERENCES care_programs(id),
  enrollment_no    VARCHAR(24) UNIQUE NOT NULL,
  enrolled_on      DATE        NOT NULL,
  enrolled_by      INTEGER     REFERENCES providers(id),
  status           VARCHAR(20) NOT NULL DEFAULT 'active' CHECK (status IN ('active','transferred_out','lost_to_followup','completed','stopped','died')),
  last_visit_date  DATE,
  next_review_date DATE,
  exit_date        DATE,
  exit_reason      VARCHAR(120),
  notes            TEXT,
  UNIQUE (patient_id, program_id)
);
COMMENT ON TABLE program_enrollments IS 'Who is on which programme, when they are next due, and who has been lost to follow-up.';

-- BIOMEDICAL EQUIPMENT ---------------------------------------------------------

CREATE TABLE equipment (
  id             SERIAL      PRIMARY KEY,
  asset_no       VARCHAR(24) UNIQUE NOT NULL,
  name           VARCHAR(120) NOT NULL,
  category       VARCHAR(50) NOT NULL,
  manufacturer   VARCHAR(80),
  model          VARCHAR(60),
  serial_no      VARCHAR(60),
  department_id  SMALLINT    REFERENCES departments(id),
  ward_id        SMALLINT    REFERENCES wards(id),
  purchase_date  DATE,
  cost_kes       NUMERIC(12,2),
  warranty_expiry DATE,
  status         VARCHAR(20) NOT NULL DEFAULT 'in_service' CHECK (status IN ('in_service','under_repair','standby','decommissioned','awaiting_parts')),
  last_service_date DATE,
  next_service_date DATE
);
COMMENT ON TABLE equipment IS 'Biomedical asset register. Downtime on a ventilator or theatre light is an operational emergency.';

CREATE TABLE equipment_maintenance (
  id               SERIAL      PRIMARY KEY,
  equipment_id     INTEGER     NOT NULL REFERENCES equipment(id),
  maintenance_type VARCHAR(20) NOT NULL CHECK (maintenance_type IN ('preventive','corrective','calibration','inspection','installation')),
  performed_on     DATE        NOT NULL,
  vendor           VARCHAR(120),
  technician       VARCHAR(120),
  downtime_hours   NUMERIC(6,1),
  cost_kes         NUMERIC(10,2),
  outcome          VARCHAR(30) CHECK (outcome IN ('resolved','pending_parts','replaced','condemned','no_fault_found')),
  notes            TEXT
);

-- PUBLIC HEALTH NOTIFICATION ---------------------------------------------------

CREATE TABLE notifiable_disease_reports (
  id                  SERIAL      PRIMARY KEY,
  patient_id          UUID        NOT NULL REFERENCES patients(id),
  encounter_id        UUID        REFERENCES encounters(id),
  diagnosis_id        INTEGER     REFERENCES diagnoses(id),
  icd10_code          VARCHAR(10) NOT NULL REFERENCES icd10_codes(code),
  disease_name        VARCHAR(150) NOT NULL,
  case_classification VARCHAR(15) NOT NULL CHECK (case_classification IN ('suspected','probable','confirmed','discarded')),
  detected_on         DATE        NOT NULL,
  reported_on         DATE,
  reported_by         INTEGER     REFERENCES providers(id),
  reported_to         VARCHAR(120),
  lab_confirmed       BOOLEAN     NOT NULL DEFAULT FALSE,
  contact_tracing_done BOOLEAN    NOT NULL DEFAULT FALSE,
  outcome             VARCHAR(20) CHECK (outcome IN ('recovered','under_treatment','died','transferred','unknown')),
  notes               TEXT
);
COMMENT ON TABLE notifiable_disease_reports IS 'Statutory disease notification (TB, malaria, typhoid and other reportable conditions).';

-- PATIENT EXPERIENCE -----------------------------------------------------------

CREATE TABLE patient_feedback (
  id                   SERIAL      PRIMARY KEY,
  patient_id           UUID        REFERENCES patients(id),
  encounter_id         UUID        REFERENCES encounters(id),
  department_id        SMALLINT    REFERENCES departments(id),
  submitted_on         DATE        NOT NULL,
  channel              VARCHAR(20) NOT NULL CHECK (channel IN ('exit_survey','sms','suggestion_box','online','phone_call')),
  overall_rating       SMALLINT    NOT NULL CHECK (overall_rating BETWEEN 1 AND 5),
  waiting_time_rating  SMALLINT    CHECK (waiting_time_rating BETWEEN 1 AND 5),
  staff_courtesy_rating SMALLINT   CHECK (staff_courtesy_rating BETWEEN 1 AND 5),
  cleanliness_rating   SMALLINT    CHECK (cleanliness_rating BETWEEN 1 AND 5),
  would_recommend      BOOLEAN,
  comments             TEXT,
  follow_up_required   BOOLEAN     NOT NULL DEFAULT FALSE,
  resolved_on          DATE
);
COMMENT ON TABLE patient_feedback IS 'Patient experience survey responses per visit and department.';

-- RECORD ACCESS AUDIT ----------------------------------------------------------

CREATE TABLE record_access_log (
  id            BIGSERIAL   PRIMARY KEY,
  patient_id    UUID        NOT NULL REFERENCES patients(id),
  provider_id   INTEGER     NOT NULL REFERENCES providers(id),
  encounter_id  UUID        REFERENCES encounters(id),
  accessed_at   TIMESTAMPTZ NOT NULL,
  access_type   VARCHAR(15) NOT NULL CHECK (access_type IN ('view','edit','print','export','search')),
  module        VARCHAR(40) NOT NULL,
  reason        VARCHAR(120),
  ip_address    VARCHAR(45),
  is_break_glass BOOLEAN    NOT NULL DEFAULT FALSE
);
COMMENT ON TABLE record_access_log IS 'Who opened whose record. Break-glass access is the flag a privacy audit starts from.';

-- ── Indexes ──────────────────────────────────────────────────────────────────

CREATE INDEX idx_patients_national_id      ON patients(national_id);
CREATE INDEX idx_patients_name             ON patients(last_name, first_name);
CREATE INDEX idx_encounters_patient        ON encounters(patient_id);
CREATE INDEX idx_encounters_date           ON encounters(encounter_date);
CREATE INDEX idx_encounters_provider       ON encounters(provider_id);
CREATE INDEX idx_encounters_department     ON encounters(department_id);
CREATE INDEX idx_appointments_patient      ON appointments(patient_id);
CREATE INDEX idx_appointments_provider     ON appointments(provider_id, scheduled_start);
CREATE INDEX idx_appointments_status       ON appointments(status);
CREATE INDEX idx_appointments_start        ON appointments(scheduled_start);
CREATE INDEX idx_diagnoses_patient         ON diagnoses(patient_id);
CREATE INDEX idx_diagnoses_icd10           ON diagnoses(icd10_code);
CREATE INDEX idx_clinical_notes_patient    ON clinical_notes(patient_id);
CREATE INDEX idx_clinical_notes_encounter  ON clinical_notes(encounter_id);
CREATE INDEX idx_prescriptions_patient     ON prescriptions(patient_id);
CREATE INDEX idx_rx_items_medication       ON prescription_items(medication_id);
CREATE INDEX idx_mar_patient               ON medication_administration(patient_id);
CREATE INDEX idx_lab_orders_patient        ON lab_orders(patient_id);
CREATE INDEX idx_lab_results_patient       ON lab_results(patient_id);
CREATE INDEX idx_lab_results_abnormal      ON lab_results(is_abnormal) WHERE is_abnormal;
CREATE INDEX idx_imaging_patient           ON imaging_orders(patient_id);
CREATE INDEX idx_admissions_patient        ON admissions(patient_id);
CREATE INDEX idx_admissions_ward           ON admissions(ward_id);
CREATE INDEX idx_bed_assignments_bed       ON bed_assignments(bed_id);
CREATE INDEX idx_bed_assignments_open      ON bed_assignments(admission_id) WHERE released_at IS NULL;
CREATE INDEX idx_referrals_patient         ON referrals(patient_id);
CREATE INDEX idx_surgeries_surgeon         ON surgeries(primary_surgeon, scheduled_start);
CREATE INDEX idx_cds_alerts_patient        ON cds_alerts(patient_id);
CREATE INDEX idx_cds_alerts_unacked        ON cds_alerts(patient_id) WHERE NOT is_acknowledged;
CREATE INDEX idx_billing_patient           ON billing_encounters(patient_id);
CREATE INDEX idx_claims_billing            ON insurance_claims(billing_id);
CREATE INDEX idx_schedules_provider        ON provider_schedules(provider_id, weekday);
CREATE INDEX idx_anc_patient               ON antenatal_visits(patient_id);
CREATE INDEX idx_deliveries_patient        ON deliveries(patient_id);
CREATE INDEX idx_newborns_mother           ON newborns(mother_patient_id);
CREATE INDEX idx_mortality_patient         ON mortality_records(patient_id);
CREATE INDEX idx_stock_batches_med         ON stock_batches(medication_id);
CREATE INDEX idx_stock_batches_expiry      ON stock_batches(expiry_date);
CREATE INDEX idx_stock_movements_med       ON stock_movements(medication_id, moved_at);
CREATE INDEX idx_blood_units_status        ON blood_units(status, blood_group);
CREATE INDEX idx_transfusions_patient      ON transfusions(patient_id);
CREATE INDEX idx_shifts_date               ON provider_shifts(shift_date, ward_id);
CREATE INDEX idx_documents_patient         ON patient_documents(patient_id);
CREATE INDEX idx_consents_patient          ON consents(patient_id);
CREATE INDEX idx_incidents_occurred        ON incident_reports(occurred_at);
CREATE INDEX idx_enrollments_patient       ON program_enrollments(patient_id);
CREATE INDEX idx_enrollments_due           ON program_enrollments(next_review_date) WHERE status = 'active';
CREATE INDEX idx_equipment_status          ON equipment(status);
CREATE INDEX idx_notifiable_disease        ON notifiable_disease_reports(icd10_code, detected_on);
CREATE INDEX idx_feedback_department       ON patient_feedback(department_id, submitted_on);
CREATE INDEX idx_access_log_patient        ON record_access_log(patient_id, accessed_at);
