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
    /// Boolean flag (generic is_*/has_*)
    Flag,
    /// Boolean flag for clinical criticality — distinct from Flag/abnormality.
    /// Binds to `is_critical`, `critical_flag`; never falls back to `is_abnormal`.
    Criticality,
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
        ColumnRole::Criticality,
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
            ColumnRole::Criticality => "criticality",
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
    // NOTE: do not add a bare "los" token here. It was removed because it is a
    // substring of "blood_loss_ml" ("...b_l_o_s_s..." contains "los"), which
    // silently misclassified a surgery/delivery blood-loss measurement as a
    // length-of-stay Duration column — a second, spurious Duration candidate
    // that made `AVG(surgery duration)` genuinely ambiguous once "pick the
    // first declared column" was retired (see the defect-closure sweep in
    // nl2sql/ir/bind.rs). "length_of_stay" (the intended, unambiguous spelling)
    // already matches `length_of_stay_days` without the risky abbreviation.
    RoleTokens { role: ColumnRole::Duration, tokens: &["minutes", "hours", "days", "duration", "length_of_stay", "wait_time", "turnaround"], type_class: Some(TypeClass::Numeric) },
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
    // NOTE: PersonFullName intentionally does NOT claim the bare "name" token.
    // "name" as a substring matches dozens of non-person columns (generic_name,
    // brand_name, condition_name, department name, ward name, test_name, etc.).
    // Using the bare token here would silently mark all of them as PII and
    // suppress their enum_values — exactly the defect class that removed "los"
    // from Duration (see the "los"/"blood_loss_ml" note below).  The explicit
    // person-qualifier tokens (full_name, fullname, middle_name) cover every
    // genuine full-name column in the schemas; the bare "name" fallback lives on
    // Description so that non-person name columns reach a sensible role.
    RoleTokens { role: ColumnRole::PersonFullName, tokens: &["full_name", "fullname"], type_class: Some(TypeClass::Text) },
    RoleTokens { role: ColumnRole::PersonGivenName, tokens: &["first_name", "middle_name", "given_name", "forename", "firstname"], type_class: Some(TypeClass::Text) },
    RoleTokens { role: ColumnRole::PersonFamilyName, tokens: &["last_name", "surname", "family_name", "lastname"], type_class: Some(TypeClass::Text) },
    // Contact / Identifier (PII)
    RoleTokens { role: ColumnRole::Contact, tokens: &["phone", "email", "mobile", "tel", "contact", "fax"], type_class: None },
    RoleTokens { role: ColumnRole::Identifier, tokens: &["national_id", "passport", "id_number", "id_no", "nin", "ssn"], type_class: None },
    // Criticality — checked BEFORE generic Flag so is_critical/critical_flag match
    // here, not via the broader "is_" prefix that Flag uses.  Do not add tokens
    // that overlap with the abnormality/Flag column names.
    RoleTokens { role: ColumnRole::Criticality, tokens: &[
        "is_critical", "critical_flag",
    ], type_class: Some(TypeClass::Bool) },
    // Generic boolean flags
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

/// Generic row-bookkeeping timestamp names — audit metadata ("when this row was
/// last touched"), not a domain fact about the entity.  When a table has both a
/// purpose-named `EventTime` column (e.g. `encounter_date`) and one of these,
/// the purpose-named one is preferred; a name on this list is only used when
/// it is the *sole* `EventTime`-role column on the table.
///
/// Shared by `ontology::binder::select_event_time` (informational
/// `TableBinding.event_time_col`) and `nl2sql::ir::bind`'s column-selection
/// (the column actually used to build SQL), so both sites make the same
/// preference decision instead of drifting apart.
pub const GENERIC_TIMESTAMP_NAMES: &[&str] = &[
    "created_at", "updated_at", "modified_at", "created_on",
    "updated_on", "deleted_at", "created_date", "modified_date",
];

/// Whether `name` is a generic bookkeeping timestamp (see
/// [`GENERIC_TIMESTAMP_NAMES`]), case-insensitively.
pub fn is_generic_timestamp_name(name: &str) -> bool {
    GENERIC_TIMESTAMP_NAMES.iter().any(|g| name.eq_ignore_ascii_case(g))
}

// ---------------------------------------------------------------------------
// Column-name ↔ question-word normalisation
// ---------------------------------------------------------------------------
//
// Shared by `nl2sql::ir::bind`'s role disambiguation (plans/new/03b-defect-closure-brief.md
// follow-up: "where the question itself disambiguates, select on that evidence").
// A single normalisation lives here so a column name and a question word are
// reduced to the SAME token space by the SAME rules, whether the comparison is
// "does the question name this column" (bind.rs stage A) or "does this FK
// edge's own column name match a word in the question" (bind.rs's FK-edge
// disambiguation) — one mechanism, reused, not two independently-tuned ones.

/// Trailing suffixes that mark a column as temporal bookkeeping rather than
/// carrying domain meaning on their own — stripped before splitting so
/// `"issue_date"` contributes the token `"issue"`, not `"issue"` plus a
/// meaningless `"date"`.
const TEMPORAL_NAME_SUFFIXES: &[&str] = &["_at", "_on", "_date", "_time"];

/// If `s` ends in a doubled consonant (e.g. the `"rr"` in `"referr"`, left
/// over after stripping `-ed`/`-ing` from `"referred"`/`"referring"`), also
/// push the singled-consonant form (`"refer"`). English doubles a final
/// consonant before a vowel suffix precisely when the base word is meant to
/// stay the same (`refer` → `referred`, `occur` → `occurred`, `cancel` →
/// `cancelled`) — this recovers that base form generically, for any word
/// with the pattern, rather than listing specific verbs.
fn push_undoubled(s: &str, out: &mut Vec<String>) {
    let bytes = s.as_bytes();
    if bytes.len() < 3 {
        return;
    }
    let last = bytes[bytes.len() - 1];
    let prev = bytes[bytes.len() - 2];
    if last == prev && !matches!(last, b'a' | b'e' | b'i' | b'o' | b'u') {
        out.push(s[..s.len() - 1].to_string());
    }
}

/// Cheap, purely suffix-based stemming for the handful of English
/// inflections that show up in both column names and hospital-question
/// phrasing: `-ing`, `-ed` (with and without a dropped trailing `e` or a
/// doubled final consonant — see [`push_undoubled`]), and plural `-s`. This
/// is NOT a linguistic stemmer — it exists only so that `"booked"`
/// (question) and `"booked_at"` (column), `"issued"` (question) and
/// `"issue_date"` (column), or `"referred"` (question/descriptor) and
/// `"referred_on"` (column) land on a common token without hard-coding any
/// of those words.
///
/// Returns every plausible base form; callers compare token *sets*, so
/// over-generating candidates is safe — it can only create a match where
/// the surface forms are genuinely related by one of these suffixes.
pub fn word_stems(word: &str) -> Vec<String> {
    let w = word.to_ascii_lowercase();
    let mut out = vec![w.clone()];
    if w.len() > 4 {
        if let Some(stem) = w.strip_suffix("ing") {
            out.push(stem.to_string());
            push_undoubled(stem, &mut out);
        }
    }
    if w.len() > 3 {
        if let Some(stem) = w.strip_suffix("ed") {
            out.push(stem.to_string());
            push_undoubled(stem, &mut out);
        }
        if let Some(stem) = w.strip_suffix('d') {
            out.push(stem.to_string());
        }
        if !w.ends_with("ss") {
            if let Some(stem) = w.strip_suffix('s') {
                out.push(stem.to_string());
            }
        }
    }
    out
}

/// Reduce a physical column name to its semantic token set: lowercase, strip
/// one trailing temporal suffix (see [`TEMPORAL_NAME_SUFFIXES`]), split on
/// `_`, and stem each piece (see [`word_stems`]). Tokens shorter than 3
/// characters are dropped — they are almost always leftover suffix debris
/// (`"in"` from `checked_in_at`, `"up"` from `follow_up_date`) rather than
/// evidence of anything, and keeping them invites spurious matches against
/// unrelated short question words.
///
/// `"booked_at"` → `{"booked", "book", "booke"}`.
/// `"issue_date"` → `{"issue"}`.
/// `"follow_up_date"` → `{"follow"}` (`"up"` dropped as too short).
pub fn column_semantic_tokens(col: &str) -> Vec<String> {
    let mut name = col.to_ascii_lowercase();
    for suf in TEMPORAL_NAME_SUFFIXES {
        if name.len() > suf.len() && name.ends_with(suf) {
            name.truncate(name.len() - suf.len());
            break;
        }
    }
    name.split('_')
        .filter(|s| !s.is_empty())
        .flat_map(word_stems)
        .filter(|t| t.len() >= 3)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    #[test]
    fn all_count_and_unique_slugs() {
        let expected = 36;
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

    #[test]
    fn column_semantic_tokens_strip_suffix_and_split() {
        assert_eq!(
            column_semantic_tokens("booked_at"),
            vec!["booked".to_string(), "book".to_string(), "booke".to_string()]
        );
        assert_eq!(column_semantic_tokens("issue_date"), vec!["issue".to_string()]);
        // "up" is dropped: shorter than the 3-char floor.
        assert_eq!(column_semantic_tokens("follow_up_date"), vec!["follow".to_string()]);
    }

    #[test]
    fn word_stems_cover_ed_and_plural_inflection() {
        assert!(word_stems("issued").contains(&"issue".to_string()));
        assert!(word_stems("booked").contains(&"book".to_string()));
        assert!(word_stems("encounters").contains(&"encounter".to_string()));
        assert!(word_stems("reported").contains(&"report".to_string()));
    }

    #[test]
    fn word_stems_undouble_final_consonant() {
        assert!(word_stems("referred").contains(&"refer".to_string()));
        assert!(word_stems("occurred").contains(&"occur".to_string()));
        assert!(word_stems("cancelled").contains(&"cancel".to_string()));
    }
}
