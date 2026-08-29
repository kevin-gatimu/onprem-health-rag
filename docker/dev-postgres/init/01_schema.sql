-- ============================================================================
-- Synthetic Facility A EMR — PostgreSQL 15 (primary instance)
-- Database: synth_emr_a
-- ============================================================================

SET client_encoding = 'UTF8';

-- ── Reference Tables ──────────────────────────────────────────────────────────

CREATE TABLE counties (
  id     SMALLINT PRIMARY KEY,
  name   VARCHAR(60)  NOT NULL,
  region VARCHAR(40)  NOT NULL
);

CREATE TABLE icd10_codes (
  code        VARCHAR(10)  PRIMARY KEY,
  description VARCHAR(255) NOT NULL,
  category    VARCHAR(100) NOT NULL
);

CREATE TYPE provider_role AS ENUM ('doctor','nurse','pharmacist','lab_tech','radiologist','admin','physiotherapist');
CREATE TYPE drug_form     AS ENUM ('tablet','capsule','syrup','injection','inhaler','cream','drops','patch','suppository','powder');
CREATE TYPE interaction_severity AS ENUM ('mild','moderate','severe');
CREATE TYPE gender_type   AS ENUM ('male','female','other');
CREATE TYPE encounter_type AS ENUM ('outpatient','inpatient','emergency','follow_up','telehealth','daycase');
CREATE TYPE priority_type  AS ENUM ('routine','urgent','stat');
CREATE TYPE billing_status AS ENUM ('draft','billed','partial','paid','submitted','rejected','written_off');

CREATE TABLE providers (
  id          SERIAL       PRIMARY KEY,
  employee_no VARCHAR(20)  UNIQUE NOT NULL,
  first_name  VARCHAR(80)  NOT NULL,
  last_name   VARCHAR(80)  NOT NULL,
  specialty   VARCHAR(80),
  role        provider_role NOT NULL,
  department  VARCHAR(80),
  phone       VARCHAR(20),
  is_active   BOOLEAN      NOT NULL DEFAULT TRUE
);

CREATE TABLE medication_catalog (
  id            SERIAL       PRIMARY KEY,
  generic_name  VARCHAR(150) NOT NULL,
  brand_name    VARCHAR(150),
  drug_class    VARCHAR(100) NOT NULL,
  form          drug_form    NOT NULL,
  strength      VARCHAR(50)  NOT NULL,
  unit          VARCHAR(20)  NOT NULL,
  is_controlled BOOLEAN      NOT NULL DEFAULT FALSE,
  is_active     BOOLEAN      NOT NULL DEFAULT TRUE
);

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
  id              SERIAL       PRIMARY KEY,
  name            VARCHAR(150) NOT NULL,
  allergen_class  VARCHAR(80)  NOT NULL,
  cross_reactivity VARCHAR(255)
);

-- ── Patients ──────────────────────────────────────────────────────────────────

CREATE TABLE patients (
  id            UUID         PRIMARY KEY DEFAULT gen_random_uuid(),
  patient_no    VARCHAR(20)  UNIQUE NOT NULL,
  national_id   VARCHAR(20)  UNIQUE,
  first_name    VARCHAR(80)  NOT NULL,
  middle_name   VARCHAR(80),
  last_name     VARCHAR(80)  NOT NULL,
  date_of_birth DATE         NOT NULL,
  gender        gender_type  NOT NULL,
  blood_type    VARCHAR(10)  NOT NULL DEFAULT 'unknown',
  phone_primary VARCHAR(20),
  email         VARCHAR(150),
  address       TEXT,
  county_id     SMALLINT     REFERENCES counties(id),
  marital_status VARCHAR(20),
  occupation    VARCHAR(100),
  next_of_kin   VARCHAR(150),
  nok_phone     VARCHAR(20),
  nok_relation  VARCHAR(50),
  is_active     BOOLEAN      NOT NULL DEFAULT TRUE,
  registered_at TIMESTAMPTZ  NOT NULL DEFAULT NOW()
);

CREATE TABLE patient_allergies (
  id           SERIAL       PRIMARY KEY,
  patient_id   UUID         NOT NULL REFERENCES patients(id),
  allergen_id  INTEGER      NOT NULL REFERENCES allergen_catalog(id),
  reaction     VARCHAR(255) NOT NULL,
  severity     VARCHAR(20)  NOT NULL CHECK (severity IN ('mild','moderate','severe','life_threatening')),
  onset_date   DATE,
  is_active    BOOLEAN      NOT NULL DEFAULT TRUE,
  notes        TEXT
);

CREATE TABLE patient_medical_history (
  id             SERIAL       PRIMARY KEY,
  patient_id     UUID         NOT NULL REFERENCES patients(id),
  icd10_code     VARCHAR(10)  REFERENCES icd10_codes(code),
  condition_name VARCHAR(255) NOT NULL,
  diagnosed_date DATE,
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

-- ── Encounters ────────────────────────────────────────────────────────────────

CREATE TABLE encounters (
  id              UUID         PRIMARY KEY DEFAULT gen_random_uuid(),
  encounter_no    VARCHAR(20)  UNIQUE NOT NULL,
  patient_id      UUID         NOT NULL REFERENCES patients(id),
  encounter_date  TIMESTAMPTZ  NOT NULL,
  encounter_type  encounter_type NOT NULL,
  department      VARCHAR(80),
  chief_complaint TEXT         NOT NULL,
  provider_id     INTEGER      NOT NULL REFERENCES providers(id),
  ward            VARCHAR(50),
  bed_no          VARCHAR(10),
  status          VARCHAR(20)  NOT NULL DEFAULT 'completed' CHECK (status IN ('in_progress','completed','cancelled')),
  discharge_date  TIMESTAMPTZ,
  discharge_notes TEXT,
  created_at      TIMESTAMPTZ  NOT NULL DEFAULT NOW()
);

CREATE TABLE vital_signs (
  id              SERIAL       PRIMARY KEY,
  encounter_id    UUID         NOT NULL REFERENCES encounters(id),
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
  pain_score      SMALLINT,
  gcs_score       SMALLINT,
  blood_glucose   NUMERIC(5,2),
  recorded_at     TIMESTAMPTZ  NOT NULL DEFAULT NOW()
);

CREATE TABLE diagnoses (
  id             SERIAL       PRIMARY KEY,
  encounter_id   UUID         NOT NULL REFERENCES encounters(id),
  patient_id     UUID         NOT NULL REFERENCES patients(id),
  icd10_code     VARCHAR(10)  NOT NULL REFERENCES icd10_codes(code),
  diagnosis_desc TEXT         NOT NULL,
  dx_type        VARCHAR(15)  NOT NULL DEFAULT 'primary' CHECK (dx_type IN ('primary','secondary','tertiary','differential')),
  diagnosed_at   TIMESTAMPTZ  NOT NULL DEFAULT NOW(),
  is_active      BOOLEAN      NOT NULL DEFAULT TRUE,
  notes          TEXT
);

-- ── Admissions (Inpatient specific) ──────────────────────────────────────────

CREATE TABLE admissions (
  id              SERIAL       PRIMARY KEY,
  encounter_id    UUID         NOT NULL REFERENCES encounters(id) UNIQUE,
  patient_id      UUID         NOT NULL REFERENCES patients(id),
  admitting_dr    INTEGER      NOT NULL REFERENCES providers(id),
  admission_date  TIMESTAMPTZ  NOT NULL,
  ward            VARCHAR(50)  NOT NULL,
  bed_no          VARCHAR(10),
  admission_type  VARCHAR(20)  NOT NULL CHECK (admission_type IN ('elective','emergency','transfer')),
  admitting_dx    TEXT         NOT NULL,
  discharge_date  TIMESTAMPTZ,
  discharge_type  VARCHAR(30)  CHECK (discharge_type IN ('home','transfer','deceased','absconded','against_advice')),
  discharge_summary TEXT,
  length_of_stay_days INTEGER GENERATED ALWAYS AS (
    CASE WHEN discharge_date IS NOT NULL
    THEN EXTRACT(DAY FROM discharge_date - admission_date)::INTEGER
    ELSE NULL END) STORED
);

-- ── Prescriptions ─────────────────────────────────────────────────────────────

CREATE TABLE prescriptions (
  id             UUID         PRIMARY KEY DEFAULT gen_random_uuid(),
  rx_number      VARCHAR(20)  UNIQUE NOT NULL,
  encounter_id   UUID         NOT NULL REFERENCES encounters(id),
  patient_id     UUID         NOT NULL REFERENCES patients(id),
  prescriber_id  INTEGER      NOT NULL REFERENCES providers(id),
  issue_date     DATE         NOT NULL,
  valid_until    DATE         NOT NULL,
  status         VARCHAR(15)  NOT NULL DEFAULT 'active' CHECK (status IN ('active','dispensed','cancelled','expired')),
  dispensed_by   INTEGER      REFERENCES providers(id),
  dispensed_at   TIMESTAMPTZ,
  notes          TEXT,
  created_at     TIMESTAMPTZ  NOT NULL DEFAULT NOW()
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
  dispensed_at    TIMESTAMPTZ
);

-- ── Lab ───────────────────────────────────────────────────────────────────────

CREATE TABLE lab_orders (
  id            UUID         PRIMARY KEY DEFAULT gen_random_uuid(),
  order_no      VARCHAR(20)  UNIQUE NOT NULL,
  encounter_id  UUID         NOT NULL REFERENCES encounters(id),
  patient_id    UUID         NOT NULL REFERENCES patients(id),
  ordered_by    INTEGER      NOT NULL REFERENCES providers(id),
  order_date    TIMESTAMPTZ  NOT NULL DEFAULT NOW(),
  panel_name    VARCHAR(150) NOT NULL,
  priority      priority_type NOT NULL DEFAULT 'routine',
  status        VARCHAR(20)  NOT NULL DEFAULT 'resulted' CHECK (status IN ('ordered','collected','processing','resulted','cancelled')),
  specimen_type VARCHAR(80)
);

CREATE TABLE lab_results (
  id              SERIAL       PRIMARY KEY,
  order_id        UUID         NOT NULL REFERENCES lab_orders(id),
  test_name       VARCHAR(150) NOT NULL,
  result_value    VARCHAR(100) NOT NULL,
  result_unit     VARCHAR(40),
  reference_range VARCHAR(80),
  is_abnormal     BOOLEAN      NOT NULL DEFAULT FALSE,
  abnormal_flag   VARCHAR(3)   CHECK (abnormal_flag IN ('L','LL','H','HH')),
  resulted_at     TIMESTAMPTZ  NOT NULL DEFAULT NOW(),
  notes           VARCHAR(255)
);

-- ── Imaging ───────────────────────────────────────────────────────────────────

CREATE TABLE imaging_orders (
  id            UUID         PRIMARY KEY DEFAULT gen_random_uuid(),
  order_no      VARCHAR(20)  UNIQUE NOT NULL,
  encounter_id  UUID         NOT NULL REFERENCES encounters(id),
  patient_id    UUID         NOT NULL REFERENCES patients(id),
  ordered_by    INTEGER      NOT NULL REFERENCES providers(id),
  modality      VARCHAR(20)  NOT NULL CHECK (modality IN ('X-Ray','CT','MRI','Ultrasound','Echocardiogram','Mammography','PET','Nuclear')),
  body_part     VARCHAR(80)  NOT NULL,
  indication    TEXT,
  order_date    TIMESTAMPTZ  NOT NULL DEFAULT NOW(),
  status        VARCHAR(20)  NOT NULL DEFAULT 'resulted',
  report        TEXT,
  radiologist   INTEGER      REFERENCES providers(id),
  reported_at   TIMESTAMPTZ
);

-- ── CDS Alerts ────────────────────────────────────────────────────────────────

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
  is_acknowledged BOOLEAN      NOT NULL DEFAULT FALSE,
  acknowledged_by INTEGER      REFERENCES providers(id),
  acknowledged_at TIMESTAMPTZ,
  override_reason TEXT
);

-- ── Billing ───────────────────────────────────────────────────────────────────

CREATE TABLE insurance_providers (
  id            SERIAL       PRIMARY KEY,
  name          VARCHAR(150) NOT NULL,
  short_name    VARCHAR(30),
  type          VARCHAR(20)  NOT NULL CHECK (type IN ('nhif','private','corporate','micro')),
  contact_phone VARCHAR(20),
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
  status              billing_status NOT NULL DEFAULT 'billed',
  notes               TEXT,
  created_at          TIMESTAMPTZ  NOT NULL DEFAULT NOW()
);

CREATE TABLE billing_items (
  id             SERIAL       PRIMARY KEY,
  billing_id     UUID         NOT NULL REFERENCES billing_encounters(id),
  item_type      VARCHAR(20)  NOT NULL,
  item_code      VARCHAR(30),
  description    VARCHAR(255) NOT NULL,
  quantity       SMALLINT     NOT NULL DEFAULT 1,
  unit_price_kes NUMERIC(8,2) NOT NULL,
  total_kes      NUMERIC(10,2) NOT NULL
);

CREATE TABLE payments (
  id             SERIAL       PRIMARY KEY,
  billing_id     UUID         NOT NULL REFERENCES billing_encounters(id),
  payment_date   TIMESTAMPTZ  NOT NULL DEFAULT NOW(),
  amount_kes     NUMERIC(10,2) NOT NULL,
  payment_method VARCHAR(20)  NOT NULL CHECK (payment_method IN ('cash','mpesa','bank_transfer','insurance','cheque','waiver')),
  reference_no   VARCHAR(80),
  notes          VARCHAR(255)
);

-- ── Indexes ───────────────────────────────────────────────────────────────────

CREATE INDEX idx_patients_national_id      ON patients(national_id);
CREATE INDEX idx_encounters_patient        ON encounters(patient_id);
CREATE INDEX idx_encounters_date           ON encounters(encounter_date);
CREATE INDEX idx_diagnoses_patient         ON diagnoses(patient_id);
CREATE INDEX idx_diagnoses_icd10           ON diagnoses(icd10_code);
CREATE INDEX idx_prescriptions_patient     ON prescriptions(patient_id);
CREATE INDEX idx_rx_items_medication       ON prescription_items(medication_id);
CREATE INDEX idx_lab_orders_patient        ON lab_orders(patient_id);
CREATE INDEX idx_cds_alerts_patient        ON cds_alerts(patient_id);
CREATE INDEX idx_cds_alerts_unacked        ON cds_alerts(patient_id) WHERE NOT is_acknowledged;
CREATE INDEX idx_billing_patient           ON billing_encounters(patient_id);
