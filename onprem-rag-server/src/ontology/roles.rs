//! Column-role ontology.
//!
//! A `ColumnRole` describes *what a column does* within its table — its semantic
//! purpose regardless of its physical name. The binder assigns one role per
//! column (first-match precedence), downstream plans use roles for time-range
//! filtering (`EventTime`), amount aggregation (`Amount`), and schema-linking.
//!
//! **Invariant**: `ColumnRole::ALL` must list every variant. A test asserts the
//! length and unique slugs.

use serde::{Deserialize, Serialize};

/// The semantic role a column plays within its table.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ColumnRole {
    PrimaryKey,
    // Typed FK references — set when the FK points to a table bound to that concept
    PatientRef,
    EncounterRef,
    ProviderRef,
    DepartmentRef,
    WardRef,
    BedRef,
    /// Generic FK to any other table
    ForeignRef,
    /// Business-level unique identifier (patient_no, encounter_no, rx_number, claim_no)
    BusinessId,
    // Person-name columns (PII — never placed in enum_values)
    PersonGivenName,
    PersonFamilyName,
    PersonFullName,
    /// Primary "when did it happen" timestamp for time-range filtering
    EventTime,
    StartTime,
    EndTime,
    BirthDate,
    DeathDate,
    /// Low-cardinality categorical — status, state, outcome, disposition
    Status,
    Category,
    Type,
    Severity,
    Priority,
    Gender,
    /// Money / financial amount
    Amount,
    /// Non-monetary numeric (weight, count, score, duration in minutes)
    Quantity,
    Measure,
    /// Duration in minutes/days/hours
    Duration,
    /// Boolean flag
    Flag,
    /// Coded value (ICD-10, procedure code, ATC)
    Code,
    Description,
    FreeText,
    /// Location name (room, store, ward) — text
    Location,
    /// PII contact/identifier — phone, email, national ID
    Contact,
    Identifier,
    Unknown,
}

impl ColumnRole {
    /// All column-role variants. **Must stay in sync** with the enum above.
    pub const ALL: &'static [ColumnRole] = &[
        ColumnRole::PrimaryKey,
        ColumnRole::PatientRef,
        ColumnRole::EncounterRef,
        ColumnRole::ProviderRef,
        ColumnRole::DepartmentRef,
        ColumnRole::WardRef,
        ColumnRole::BedRef,
        ColumnRole::ForeignRef,
        ColumnRole::BusinessId,
        ColumnRole::PersonGivenName,
        ColumnRole::PersonFamilyName,
        ColumnRole::PersonFullName,
        ColumnRole::EventTime,
        ColumnRole::StartTime,
        ColumnRole::EndTime,
        ColumnRole::BirthDate,
        ColumnRole::DeathDate,
        ColumnRole::Status,
        ColumnRole::Category,
        ColumnRole::Type,
        ColumnRole::Severity,
        ColumnRole::Priority,
        ColumnRole::Gender,
        ColumnRole::Amount,
        ColumnRole::Quantity,
        ColumnRole::Measure,
        ColumnRole::Duration,
        ColumnRole::Flag,
        ColumnRole::Code,
        ColumnRole::Description,
        ColumnRole::FreeText,
        ColumnRole::Location,
        ColumnRole::Contact,
        ColumnRole::Identifier,
        ColumnRole::Unknown,
    ];

    pub fn slug(self) -> &'static str {
        match self {
            ColumnRole::PrimaryKey => "primary_key",
            ColumnRole::PatientRef => "patient_ref",
            ColumnRole::EncounterRef => "encounter_ref",
            ColumnRole::ProviderRef => "provider_ref",
            ColumnRole::DepartmentRef => "department_ref",
            ColumnRole::WardRef => "ward_ref",
            ColumnRole::BedRef => "bed_ref",
            ColumnRole::ForeignRef => "foreign_ref",
            ColumnRole::BusinessId => "business_id",
            ColumnRole::PersonGivenName => "person_given_name",
            ColumnRole::PersonFamilyName => "person_family_name",
            ColumnRole::PersonFullName => "person_full_name",
            ColumnRole::EventTime => "event_time",
            ColumnRole::StartTime => "start_time",
            ColumnRole::EndTime => "end_time",
            ColumnRole::BirthDate => "birth_date",
            ColumnRole::DeathDate => "death_date",
            ColumnRole::Status => "status",
            ColumnRole::Category => "category",
            ColumnRole::Type => "type",
            ColumnRole::Severity => "severity",
            ColumnRole::Priority => "priority",
            ColumnRole::Gender => "gender",
            ColumnRole::Amount => "amount",
            ColumnRole::Quantity => "quantity",
            ColumnRole::Measure => "measure",
            ColumnRole::Duration => "duration",
            ColumnRole::Flag => "flag",
            ColumnRole::Code => "code",
            ColumnRole::Description => "description",
            ColumnRole::FreeText => "free_text",
            ColumnRole::Location => "location",
            ColumnRole::Contact => "contact",
            ColumnRole::Identifier => "identifier",
            ColumnRole::Unknown => "unknown",
        }
    }

    /// Parse a slug back to a role.
    pub fn from_slug(slug: &str) -> Option<Self> {
        ColumnRole::ALL.iter().copied().find(|r| r.slug() == slug)
    }

    /// Whether this role is PII (columns with this role are never placed in enum_values).
    pub fn is_pii(self) -> bool {
        matches!(
            self,
            ColumnRole::Contact
                | ColumnRole::Identifier
                | ColumnRole::PersonGivenName
                | ColumnRole::PersonFamilyName
                | ColumnRole::PersonFullName
        )
    }

    /// Whether this role is categorical (enum_values should be probed).
    pub fn is_categorical(self) -> bool {
        matches!(
            self,
            ColumnRole::Status
                | ColumnRole::Category
                | ColumnRole::Type
                | ColumnRole::Severity
                | ColumnRole::Priority
                | ColumnRole::Gender
        )
    }
}

/// Name-token patterns used by the role assigner (step 3: name-token match).
/// Each entry is a list of lowercase substrings; if any appears in the
/// (lowercased) column name and the type constraint is satisfied, the role
/// is assigned.
pub struct RoleTokens {
    pub role: ColumnRole,
    /// Substrings to match in the column name (any match wins).
    pub tokens: &'static [&'static str],
    /// Required column type class (None = any type).
    pub type_class: Option<TypeClass>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TypeClass {
    Temporal,
    Numeric,
    Text,
    Bool,
}

impl TypeClass {
    /// Returns true if the SQL type string is compatible with this class.
    /// Match is case-insensitive substring-based.
    pub fn matches(self, type_str: &str) -> bool {
        let t = type_str.to_ascii_lowercase();
        match self {
            TypeClass::Temporal => {
                t.contains("date")
                    || t.contains("time")
                    || t.contains("timestamp")
                    || t.contains("datetime")
            }
            TypeClass::Numeric => {
                t.contains("int")
                    || t.contains("numeric")
                    || t.contains("decimal")
                    || t.contains("money")
                    || t.contains("float")
                    || t.contains("double")
                    || t.contains("real")
                    || t.contains("number")
                    || t.contains("serial")
                    || t.contains("smallserial")
                    || t.contains("bigserial")
            }
            TypeClass::Text => {
                t.contains("char")
                    || t.contains("text")
                    || t.contains("varchar")
                    || t.contains("nvarchar")
                    || t.contains("string")
                    || t.contains("clob")
                    || t.contains("enum")
            }
            TypeClass::Bool => {
                t.contains("bool") || t.contains("bit")
            }
        }
    }
}

/// Token-based name → role mapping, checked in order.
/// More-specific patterns should come first.
pub static ROLE_TOKENS: &[RoleTokens] = &[
    // Temporal roles — domain names first (EventTime candidates)
    RoleTokens { role: ColumnRole::BirthDate, tokens: &["birth_date", "dob", "date_of_birth", "born_on", "birthdate"], type_class: Some(TypeClass::Temporal) },
    RoleTokens { role: ColumnRole::DeathDate, tokens: &["death_date", "died_on", "date_of_death", "dod", "deceased_at"], type_class: Some(TypeClass::Temporal) },
    RoleTokens { role: ColumnRole::StartTime, tokens: &["scheduled_start", "actual_start", "starts_at", "start_time", "scheduled_time", "start_date", "admission_date", "admitted_at", "begin_at", "commenced_at"], type_class: Some(TypeClass::Temporal) },
    RoleTokens { role: ColumnRole::EndTime, tokens: &["scheduled_end", "actual_end", "ends_at", "end_time", "end_date", "discharge_date", "discharged_at", "completed_at", "finish_at", "closed_at"], type_class: Some(TypeClass::Temporal) },
    // Domain EventTime candidates (preferred over created_at)
    RoleTokens { role: ColumnRole::EventTime, tokens: &[
        "delivered_at", "visit_date", "encounter_date", "service_date", "occurred_at",
        "recorded_at", "triage_time", "triage_date", "order_date", "ordered_at",
        "collected_on", "collected_at", "issued_at", "billing_date", "bill_date",
        "incident_date", "admission_date", "started_at", "visit_date",
        "procedure_date", "surgery_date", "lab_date", "imaging_date",
        "enrolment_date", "enrolled_at", "approved_on", "approved_at",
        "report_date", "reported_at", "detected_at", "date_of_visit",
        "date_of_admission", "transaction_date", "claim_date",
        "prescription_date", "dispensed_at", "discharge_date",
        "valid_from", "issued_on", "expires_on",
        "date", "visit_date",
    ], type_class: Some(TypeClass::Temporal) },
    // Generic temporal — lower priority than domain names
    RoleTokens { role: ColumnRole::EventTime, tokens: &["_at", "_on", "timestamp", "datetime", "occurred", "recorded"], type_class: Some(TypeClass::Temporal) },
    // Amount (money)
    RoleTokens { role: ColumnRole::Amount, tokens: &[
        "amount", "price", "cost", "fee", "total", "kes", "usd", "due", "paid",
        "subtotal", "discount", "cover", "balance", "charge", "rate", "tariff",
        "premium", "copay", "deductible", "reimburs",
    ], type_class: Some(TypeClass::Numeric) },
    RoleTokens { role: ColumnRole::Duration, tokens: &["minutes", "hours", "days", "duration", "los", "length_of_stay", "wait_time", "turnaround"], type_class: Some(TypeClass::Numeric) },
    // Quantity role — stock / dispensed units (must precede Measure so "quantity_on_hand", "qty_*"
    // aren't swallowed by the generic "quantity" token inside Measure).
    RoleTokens { role: ColumnRole::Quantity, tokens: &[
        "quantity_on_hand", "qty_on_hand", "stock_qty", "available_qty",
        "units_dispensed", "qty_dispensed",
    ], type_class: Some(TypeClass::Numeric) },
    RoleTokens { role: ColumnRole::Measure, tokens: &[
        "weight", "height", "bmi", "temperature", "pulse", "bp_", "spo2", "resp_rate",
        "volume", "count", "score", "grade", "level", "result_value", "quantity",
        "units", "dose", "strength", "concentration", "circumference",
        "apgar", "gestation", "birth_weight", "blood_loss",
    ], type_class: Some(TypeClass::Numeric) },
    // Status / category
    RoleTokens { role: ColumnRole::Status, tokens: &["status", "state", "outcome", "disposition", "result_status", "screening_status", "billing_status", "appointment_status", "referral_status", "bed_status", "admission_status"], type_class: Some(TypeClass::Text) },
    RoleTokens { role: ColumnRole::Gender, tokens: &["gender", "sex"], type_class: Some(TypeClass::Text) },
    RoleTokens { role: ColumnRole::Priority, tokens: &["priority", "urgency", "acuity", "severity_level"], type_class: Some(TypeClass::Text) },
    RoleTokens { role: ColumnRole::Severity, tokens: &["severity", "interaction_severity"], type_class: Some(TypeClass::Text) },
    RoleTokens { role: ColumnRole::Type, tokens: &[
        "_type", "type_", "kind", "category", "class", "mode", "method",
        "encounter_type", "note_type", "drug_form", "form", "component", "source",
        "employment_type", "bed_type", "ward_type", "dept_type", "item_type",
        "delivery_mode", "onset", "urgency", "reason",
    ], type_class: Some(TypeClass::Text) },
    RoleTokens { role: ColumnRole::Category, tokens: &["category", "chapter", "group", "class"], type_class: Some(TypeClass::Text) },
    // Business IDs — must precede Code so specific names like patient_no, rx_number
    // are not swallowed by Code's broad "_no" / "_number" suffix tokens.
    RoleTokens { role: ColumnRole::BusinessId, tokens: &[
        "patient_no", "encounter_no", "rx_number", "claim_no", "bill_no", "unit_no",
        "employee_no", "batch_no", "license_no", "order_no", "ref_no",
        "birth_notification_no", "admission_no", "equipment_no", "result_no",
        "surgery_no", "medication_no", "doc_no", "death_no",
        // MRN (Medical Record Number) is standard hospital patient-identifier
        // vocabulary in clinical information systems; maps to BusinessId role.
        "mrn",
    ], type_class: None },
    // Codes
    RoleTokens { role: ColumnRole::Code, tokens: &[
        "icd", "atc_code", "procedure_code", "drug_code", "code", "_no", "_number",
    ], type_class: None },
    // Name columns (PII)
    RoleTokens { role: ColumnRole::PersonFullName, tokens: &["full_name", "fullname", "name"], type_class: Some(TypeClass::Text) },
    RoleTokens { role: ColumnRole::PersonGivenName, tokens: &["first_name", "given_name", "forename", "firstname"], type_class: Some(TypeClass::Text) },
    RoleTokens { role: ColumnRole::PersonFamilyName, tokens: &["last_name", "surname", "family_name", "lastname"], type_class: Some(TypeClass::Text) },
    // Contact / Identifier (PII)
    RoleTokens { role: ColumnRole::Contact, tokens: &["phone", "email", "mobile", "tel", "contact", "fax"], type_class: None },
    RoleTokens { role: ColumnRole::Identifier, tokens: &["national_id", "passport", "id_number", "id_no", "nin", "ssn"], type_class: None },
    // Flags
    RoleTokens { role: ColumnRole::Flag, tokens: &[
        "is_", "has_", "active", "enabled", "approved", "current", "notifiable",
        "controlled", "formulary", "surgical", "abnormal", "resuscitation",
    ], type_class: Some(TypeClass::Bool) },
    // Description / free text
    RoleTokens { role: ColumnRole::FreeText, tokens: &["notes", "narrative", "remarks", "comment", "text", "reason_text", "details", "clinical_effect", "management"], type_class: Some(TypeClass::Text) },
    RoleTokens { role: ColumnRole::Description, tokens: &["description", "desc", "label", "title", "name"], type_class: Some(TypeClass::Text) },
    // Location
    RoleTokens { role: ColumnRole::Location, tokens: &["room", "location", "floor", "store", "region", "area", "zone", "district", "ward_name", "clinic_name", "theatre_no"], type_class: Some(TypeClass::Text) },
];

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    #[test]
    fn all_count_and_unique_slugs() {
        let expected = 35;
        assert_eq!(
            ColumnRole::ALL.len(),
            expected,
            "ColumnRole::ALL must have exactly {expected} entries"
        );
        let slugs: HashSet<&str> = ColumnRole::ALL.iter().map(|r| r.slug()).collect();
        assert_eq!(
            slugs.len(),
            expected,
            "every ColumnRole slug must be unique; duplicate found"
        );
    }
}
