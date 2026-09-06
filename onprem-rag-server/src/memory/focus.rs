//! `ConversationFocus` — typed slots describing *what* the conversation is about
//! (plan 06 §2), as opposed to [`WorkingMemory`](super::WorkingMemory), which
//! carries *what was said*.
//!
//! Focus is authoritative for **reference resolution**: "the patient", "there",
//! "then", "why?" are answered from these slots, deterministically and with zero
//! model calls. The rolling summary stays what it always was — context for style
//! and long-range recall.
//!
//! # Producer / consumer
//!
//! Produced by [`apply`] after each turn from the routing decision plus a
//! [`ResultDigest`] of what the executor returned. Consumed by
//! `router::focus_resolve` (follow-up rewriting), the persona focus block, and
//! the suggestion generator.
//!
//! # PHI boundary (read this before adding a field)
//!
//! A focus is persisted on the conversation document and its *slot names* reach
//! the client as `focus_used`. Two protections are structural, not conventions:
//!
//! 1. [`FocusEntity`]'s `Debug` is hand-written to redact `display` and `row_pk`,
//!    so `tracing::debug!(?focus)` can never print a patient's name. Only the
//!    business key (`PT-00042` — a pseudonymous identifier the user typed) and
//!    the logical table survive into a log line.
//! 2. `top_labels` are filtered by [`ColumnRole::is_pii`]: a grouped result keyed
//!    on a person-name column contributes **no** labels unless the question was
//!    about that person (`about_person`). Labels are also truncated, so a free-text
//!    column cannot smuggle a narrative into the focus.
//!
//! What focus *can* carry: business identifiers, ward/department/provider display
//! names, a concept, a service line, a time range, a `QuerySpec` (logical roles,
//! not values, except literal filter values the user themselves supplied), row
//! counts, a scalar, and enum-valued group labels.
//!
//! What focus must *never* carry: free-text clinical content, diagnoses, contact
//! details, national ids, or a patient name anywhere a log line can reach.

use std::fmt;

use serde::{Deserialize, Serialize};

use crate::aggregation::execute::AggRow;
use crate::config::Config;
use crate::nl2sql::ir::spec::{ColumnRef, MissingSlot, QuerySpec, Shape, TimeRange};
use crate::ontology::{ColumnRole, EntityConcept, ServiceLine};
use crate::router::{RouteClass, RouteDecision};

/// Hard cap on a single stored label, in characters. A group label is a status
/// or a ward name; anything longer is a free-text column that has no business
/// being in the focus at all.
const LABEL_MAX_CHARS: usize = 40;

// ---------------------------------------------------------------------------
// FocusEntity
// ---------------------------------------------------------------------------

/// One entity the conversation is anchored on — a patient, a provider, a place.
///
/// `key` is the *business* identifier (what the user says out loud: `PT-00042`,
/// `ICU`), `display` the human label used in prompts and the UI chip. `row_pk`
/// and `table` let a follow-up address the exact row without re-resolving.
#[derive(Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct FocusEntity {
    /// Business key. Safe to log.
    pub key: String,
    /// Human-readable label. **Redacted from `Debug`** — may be a person's name.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub display: Option<String>,
    /// Physical primary key of the resolved row. **Redacted from `Debug`.**
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub row_pk: Option<String>,
    /// Logical table the entity was resolved against.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub table: Option<String>,
    /// Turn number this entity was set on — drives the patient TTL.
    #[serde(default)]
    pub set_turn: u32,
}

impl FocusEntity {
    /// An entity known only by its business key.
    pub fn from_key(key: impl Into<String>, set_turn: u32) -> Self {
        FocusEntity {
            key: key.into(),
            display: None,
            row_pk: None,
            table: None,
            set_turn,
        }
    }

    /// The label to show a user: `display` when known, else the key.
    pub fn label(&self) -> &str {
        self.display.as_deref().unwrap_or(&self.key)
    }
}

/// Hand-written so a focus can be `Debug`-logged without leaking a name. The
/// derived impl would print `display` verbatim; this one does not, and the
/// omission is deliberate rather than an oversight to be "fixed" later.
impl fmt::Debug for FocusEntity {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("FocusEntity")
            .field("key", &self.key)
            .field("display", &self.display.as_ref().map(|_| "<redacted>"))
            .field("row_pk", &self.row_pk.as_ref().map(|_| "<redacted>"))
            .field("table", &self.table)
            .field("set_turn", &self.set_turn)
            .finish()
    }
}

// ---------------------------------------------------------------------------
// ResultDigest
// ---------------------------------------------------------------------------

/// What the last answer *returned*, small enough to keep for the whole
/// conversation. Enough to generate suggestions and to answer "why?" without
/// re-running the query.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ResultDigest {
    /// Shape of the spec that produced the rows (`None` for a semantic answer).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub shape: Option<Shape>,
    #[serde(default)]
    pub row_count: usize,
    #[serde(default)]
    pub columns: Vec<String>,
    /// Up to `ONPREM_FOCUS_TOP_LABELS` group labels, PII-filtered and truncated.
    #[serde(default)]
    pub top_labels: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scalar: Option<f64>,
    /// Set when the result identified exactly one patient (see
    /// [`ResultDigest::detect_single_patient`]) — the "single-patient answer"
    /// half of plan 06 §2.1's `patient` rule.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub patient: Option<FocusEntity>,
}

impl ResultDigest {
    pub fn new(shape: Shape) -> Self {
        ResultDigest {
            shape: Some(shape),
            ..ResultDigest::default()
        }
    }

    pub fn with_row_count(mut self, n: usize) -> Self {
        self.row_count = n;
        self
    }

    pub fn with_columns(mut self, columns: Vec<String>) -> Self {
        self.columns = columns;
        self
    }

    pub fn with_scalar(mut self, value: f64) -> Self {
        self.scalar = Some(value);
        self
    }

    pub fn with_patient(mut self, entity: FocusEntity) -> Self {
        self.patient = Some(entity);
        self
    }

    /// Attach group labels, enforcing plan 06 §2.1's two constraints in one
    /// place: a PII-role label column contributes nothing unless the question
    /// was about that person, and every label is length-bounded.
    ///
    /// `label_role` is the [`ColumnRole`] of the column the labels came from;
    /// `None` means "unknown role", which is treated as non-PII because a
    /// grouped aggregate on an unclassified column is a status in practice —
    /// pass the role whenever the binding knows it.
    pub fn with_labels<S: AsRef<str>>(
        mut self,
        labels: &[S],
        label_role: Option<ColumnRole>,
        about_person: bool,
        max: usize,
    ) -> Self {
        if label_role.is_some_and(ColumnRole::is_pii) && !about_person {
            self.top_labels.clear();
            return self;
        }
        self.top_labels = labels
            .iter()
            .map(|l| sanitize_label(l.as_ref()))
            .filter(|l| !l.is_empty())
            .take(max)
            .collect();
        self
    }

    /// Digest of a DocumentDB aggregation result. `AggRow.label` is already a
    /// group key, never free text, so it needs only truncation.
    pub fn from_agg_rows(shape: Shape, rows: &[AggRow], max: usize) -> Self {
        let scalar = if rows.len() == 1 {
            Some(rows[0].value)
        } else {
            None
        };
        let labels: Vec<&str> = rows.iter().map(|r| r.label.as_str()).collect();
        let mut digest = ResultDigest::new(shape)
            .with_row_count(rows.len())
            .with_labels(&labels, None, false, max);
        digest.scalar = scalar;
        digest
    }

    /// Exactly-one-distinct-value detection for the `BusinessId` column of a
    /// patient-bearing result (plan 06 §2.1). Returns `None` when the column is
    /// absent, when no row carries a value, or when two rows disagree — a
    /// two-patient answer must not silently focus one of them.
    ///
    /// `rows` are positional cells aligned to `columns`, which is what both the
    /// SQL and the `List` paths already hold.
    pub fn detect_single_patient(
        columns: &[String],
        rows: &[Vec<serde_json::Value>],
        business_id_column: &str,
        name_column: Option<&str>,
        turn: u32,
    ) -> Option<FocusEntity> {
        let key_idx = columns.iter().position(|c| c == business_id_column)?;
        let name_idx = name_column.and_then(|n| columns.iter().position(|c| c == n));

        let mut key: Option<String> = None;
        let mut display: Option<String> = None;
        for row in rows {
            let cell = row.get(key_idx)?;
            let value = match cell {
                serde_json::Value::String(s) => s.trim().to_string(),
                serde_json::Value::Number(n) => n.to_string(),
                _ => continue,
            };
            if value.is_empty() {
                continue;
            }
            match &key {
                Some(existing) if existing != &value => return None,
                Some(_) => {}
                None => {
                    key = Some(value);
                    display = name_idx
                        .and_then(|i| row.get(i))
                        .and_then(|v| v.as_str())
                        .map(|s| sanitize_label(s));
                }
            }
        }

        key.map(|k| FocusEntity {
            key: k,
            display: display.filter(|d| !d.is_empty()),
            row_pk: None,
            table: None,
            set_turn: turn,
        })
    }
}

/// Trim, collapse whitespace and cap at [`LABEL_MAX_CHARS`] on a char boundary.
fn sanitize_label(s: &str) -> String {
    let collapsed = s.split_whitespace().collect::<Vec<_>>().join(" ");
    if collapsed.chars().count() <= LABEL_MAX_CHARS {
        collapsed
    } else {
        collapsed.chars().take(LABEL_MAX_CHARS).collect()
    }
}

// ---------------------------------------------------------------------------
// ConversationFocus
// ---------------------------------------------------------------------------

/// Typed slots for the current subject of the conversation (plan 06 §2).
///
/// `Default` is the stateless focus: a request with no `conversation_id` gets
/// this, and every rule below degrades to "no substitution" against it.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ConversationFocus {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub patient: Option<FocusEntity>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider: Option<FocusEntity>,
    /// Ward / department / theatre / store.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub place: Option<FocusEntity>,
    /// Last subject concept.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub concept: Option<EntityConcept>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub service_line: Option<ServiceLine>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub time_range: Option<TimeRange>,
    /// IR of the last structured answer (plan 03), for follow-up mutation.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_spec: Option<QuerySpec>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_result: Option<ResultDigest>,
    /// Set while a clarification is outstanding; cleared by any answer.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pending_clarify: Option<MissingSlot>,
    /// The question that triggered the outstanding clarification, kept so a
    /// bare slot answer ("appointments") can be merged back into it.
    ///
    /// Not in plan 06 §2's field list, but §4's round-trip is unimplementable
    /// without it when the clarify fired *before* a spec existed — which is
    /// exactly the `Clarify(Subject)` case §9 tests. It is the user's own text
    /// coming straight back, so it discloses nothing new.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pending_question: Option<String>,
    /// Monotonic turn counter; `set_turn` on entities is compared against it.
    #[serde(default)]
    pub turn: u32,
    /// When this focus was last written. `Option` (rather than plan §2's bare
    /// `DateTime`) so `Default` stays derivable for the stateless path.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub updated_at: Option<chrono::DateTime<chrono::Utc>>,
}

impl ConversationFocus {
    /// Nothing has been established yet.
    pub fn is_empty(&self) -> bool {
        self.patient.is_none()
            && self.provider.is_none()
            && self.place.is_none()
            && self.concept.is_none()
            && self.time_range.is_none()
            && self.last_spec.is_none()
            && self.last_result.is_none()
            && self.pending_clarify.is_none()
    }

    /// Slot names that carry a value — the `focus_used` wire value.
    ///
    /// **Names only, never values.** This is what crosses to the client and what
    /// is persisted alongside a message, so a patient key never leaves the
    /// premises through this path even though the focus itself holds one.
    pub fn focus_used(&self) -> Vec<String> {
        let mut used = Vec::new();
        for (present, name) in [
            (self.patient.is_some(), "patient"),
            (self.provider.is_some(), "provider"),
            (self.place.is_some(), "place"),
            (self.concept.is_some(), "concept"),
            (self.service_line.is_some(), "service_line"),
            (self.time_range.is_some(), "time_range"),
            (self.last_spec.is_some(), "last_spec"),
            (self.last_result.is_some(), "last_result"),
        ] {
            if present {
                used.push(name.to_string());
            }
        }
        used
    }

    /// The persona/grounded-prompt focus block (plan 06 §6). Runs against a
    /// **local** model, so it may name the focus patient — that is the whole
    /// point of an on-prem deployment. `None` when there is nothing to say, so
    /// the caller adds no empty section to the prompt.
    pub fn prompt_block(&self) -> Option<String> {
        let mut parts: Vec<String> = Vec::new();
        if let Some(p) = &self.patient {
            parts.push(format!("patient {}", p.label()));
        }
        if let Some(p) = &self.place {
            parts.push(format!("place: {}", p.label()));
        }
        if let Some(p) = &self.provider {
            parts.push(format!("provider: {}", p.label()));
        }
        if let Some(tr) = &self.time_range {
            parts.push(format!(
                "period: {}",
                crate::router::focus::time_range_label(tr)
            ));
        }
        if let Some(c) = &self.concept {
            parts.push(format!("subject: {}", c.slug()));
        }
        if let Some(r) = &self.last_result {
            if let Some(scalar) = r.scalar {
                parts.push(format!("last answer: {scalar}"));
            } else if r.row_count > 0 {
                parts.push(format!("last answer: {} rows", r.row_count));
            }
        }
        if parts.is_empty() {
            None
        } else {
            Some(format!("Current focus: {}.", parts.join("; ")))
        }
    }

    /// Project onto the router's plan-02 focus shape so `route_v3` can consume
    /// this focus without either module owning the other's type.
    ///
    /// Exists because plan 02 defined its own minimal `ConversationFocus` in
    /// `router/focus.rs` before this canonical one landed. Deleting that one is
    /// a reconciliation the router's owner makes, not a rename done behind their
    /// back — until then, this is the seam.
    pub fn to_router_focus(&self) -> crate::router::focus::ConversationFocus {
        crate::router::focus::ConversationFocus {
            concept: self.concept,
            patient_key: self.patient.as_ref().map(|p| p.key.clone()),
            time_range: self.time_range.clone(),
            line: self.service_line,
            last_spec: self.last_spec.clone(),
        }
    }

    /// Serialise for storage. Stored as a JSON **string** on the conversation
    /// document (`focus_json`), matching how `citations_json`, `structured_json`
    /// and `verify_json` are already stored — plan 06 §2 says "BSON", but the
    /// codebase's own comment on that convention says why not: a nested
    /// `QuerySpec` round-trips through serde_json exactly and through BSON only
    /// approximately (tagged enums, `NaiveDate`, `f64` edge cases).
    pub fn to_json_string(&self) -> Option<String> {
        serde_json::to_string(self).ok()
    }

    /// Parse a stored focus. A malformed or older-shape value yields `None`, and
    /// the turn proceeds with `Default` — the same fail-open contract
    /// `load_working_memory` already has.
    pub fn from_json_str(s: &str) -> Option<Self> {
        serde_json::from_str(s).ok()
    }
}

// ---------------------------------------------------------------------------
// Reset gate (plan 06 §2.1 last rule — Tier 0 regex)
// ---------------------------------------------------------------------------

/// "new topic" / "forget that" / "start over" — an explicit instruction to drop
/// the focus. Deliberately narrow: a bare "reset" is a legitimate question about
/// equipment resets, so it is not a trigger.
pub fn is_reset_phrase(question: &str) -> bool {
    static RESET: std::sync::LazyLock<regex::Regex> = std::sync::LazyLock::new(|| {
        regex::Regex::new(
            r"(?ix)
            \b(
                  new\s+topic
                | new\s+question
                | forget\s+(that|it|this|all\s+that|everything)
                | start\s+(over|again|afresh)
                | starting\s+over
                | clear\s+(the\s+)?(context|focus)
                | different\s+topic
                | change\s+of\s+topic
                | let'?s\s+move\s+on
            )\b",
        )
        .expect("reset regex compiles")
    });
    RESET.is_match(question)
}

// ---------------------------------------------------------------------------
// apply (plan 06 §2.1)
// ---------------------------------------------------------------------------

/// Fold one turn's outcome into the focus. Pure and model-free.
///
/// `decision` is the routing decision for the turn just answered; `digest` is
/// what the executor returned (`None` for a conversational or refused turn);
/// `question` is the user's original text, needed only for the reset gate.
///
/// Plan 06 §2.1's signature is `apply(outcome, decision, question)`. `outcome`
/// is plan 04's `ExecOutcome`, which does not exist yet — the executor's owner
/// maps it to a [`ResultDigest`] (constructors above), which is the only part of
/// an outcome the focus is allowed to remember.
pub fn apply(
    prev: &ConversationFocus,
    decision: &RouteDecision,
    digest: Option<&ResultDigest>,
    question: &str,
    config: &Config,
) -> ConversationFocus {
    if !config.focus_enabled {
        return ConversationFocus::default();
    }

    let turn = prev.turn.saturating_add(1);
    let now = chrono::Utc::now();

    // An explicit "new topic" drops everything but the turn counter. Checked
    // before anything else so a reset cannot be partially applied.
    if is_reset_phrase(question) {
        return ConversationFocus {
            turn,
            updated_at: Some(now),
            ..ConversationFocus::default()
        };
    }

    let mut next = prev.clone();
    next.turn = turn;
    next.updated_at = Some(now);

    // ── patient ──────────────────────────────────────────────────────────────
    // A named key wins over an inferred single-patient result; a *different* key
    // replaces the entity outright (and with it any stale display/row_pk).
    // Aggregate questions carry no key and leave the patient standing — asking
    // "how many beds are free" must not forget who we were discussing.
    if let Some(key) = decision.entities.patient_key.as_deref() {
        if next.patient.as_ref().map(|p| p.key.as_str()) != Some(key) {
            next.patient = Some(FocusEntity::from_key(key, turn));
        }
    } else if let Some(found) = digest.and_then(|d| d.patient.clone()) {
        let single_row_lookup = matches!(
            digest.and_then(|d| d.shape),
            Some(Shape::Lookup) | Some(Shape::List)
        );
        if single_row_lookup && next.patient.as_ref().map(|p| p.key.as_str()) != Some(&found.key) {
            next.patient = Some(FocusEntity { set_turn: turn, ..found });
        }
    }

    // Patient TTL. `0` (the default) means "keep until replaced".
    if config.focus_patient_ttl_turns > 0 {
        if let Some(p) = &next.patient {
            if turn.saturating_sub(p.set_turn) > config.focus_patient_ttl_turns {
                next.patient = None;
            }
        }
    }

    // ── provider ─────────────────────────────────────────────────────────────
    if let Some(provider) = decision.entities.provider_ref.as_deref() {
        if next.provider.as_ref().map(|p| p.key.as_str()) != Some(provider) {
            next.provider = Some(FocusEntity::from_key(provider, turn));
        }
    }

    // ── place ────────────────────────────────────────────────────────────────
    // Typed roles first (a bound spec knows a WardRef when it sees one), then
    // the extractor's `(column, value)` enum filters by generic name token.
    if let Some(place) = place_from_spec(decision.query_spec.as_ref(), turn)
        .or_else(|| place_from_enum_filters(&decision.entities.enum_filters, turn))
    {
        if next.place.as_ref().map(|p| p.key.as_str()) != Some(place.key.as_str()) {
            next.place = Some(place);
        }
    }

    // ── time range ───────────────────────────────────────────────────────────
    // Explicit only. "and by ward?" inherits the period rather than widening to
    // all of history, which is what a person asking that question means.
    if let Some(tr) = decision.entities.time_range.clone() {
        next.time_range = Some(tr);
    }

    // ── concept / last_spec / last_result ────────────────────────────────────
    if let Some(spec) = decision.query_spec.as_ref() {
        next.concept = Some(spec.subject.concept);
        next.last_spec = Some(spec.clone());
    } else if let Some(concept) = decision.entities.concepts.first().copied() {
        next.concept = Some(concept);
    }
    if let Some(d) = digest {
        next.last_result = Some(d.clone());
    }

    // ── service line ─────────────────────────────────────────────────────────
    // On a dedicated tab `decision.service_line` is the fixed line, so this is
    // also the "fixed on dedicated tabs" rule.
    if let Some(line) = decision.service_line {
        next.service_line = Some(line);
    }

    // ── pending clarify ──────────────────────────────────────────────────────
    match &decision.class {
        RouteClass::Clarify { slot, .. } => {
            next.pending_clarify = Some(slot.clone());
            next.pending_question = Some(question.to_string());
        }
        _ => {
            next.pending_clarify = None;
            next.pending_question = None;
        }
    }

    next
}

/// A place entity taken from a bound spec's filters, by column *role* rather
/// than by physical name. Only `Eq` on a literal string is usable — an `In` or a
/// range does not name one place.
fn place_from_spec(spec: Option<&QuerySpec>, turn: u32) -> Option<FocusEntity> {
    use crate::nl2sql::ir::spec::{FilterOp, FilterValue};
    let spec = spec?;
    spec.filters
        .iter()
        .find(|f| is_place_role(&f.column) && f.op == FilterOp::Eq)
        .and_then(|f| match &f.value {
            FilterValue::Str(s) => Some(FocusEntity {
                key: sanitize_label(s),
                display: Some(sanitize_label(s)),
                row_pk: None,
                table: f.column.physical.as_ref().map(|(t, _)| t.clone()),
                set_turn: turn,
            }),
            _ => None,
        })
}

fn is_place_role(column: &ColumnRef) -> bool {
    matches!(
        column.role,
        ColumnRole::WardRef | ColumnRole::DepartmentRef | ColumnRole::Location
    )
}

/// Fallback for the pre-IR path, where the extractor gives `(column, value)`
/// pairs and no roles. Matched on generic place tokens — never a dev-seed name.
fn place_from_enum_filters(filters: &[(String, String)], turn: u32) -> Option<FocusEntity> {
    const PLACE_TOKENS: [&str; 7] = ["ward", "department", "dept", "theatre", "location", "unit", "store"];
    filters
        .iter()
        .find(|(column, _)| {
            let c = column.to_ascii_lowercase();
            PLACE_TOKENS.iter().any(|t| c.contains(t))
        })
        .map(|(_, value)| FocusEntity {
            key: sanitize_label(value),
            display: Some(sanitize_label(value)),
            row_pk: None,
            table: None,
            set_turn: turn,
        })
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::aggregation::intent::QueryIntent;
    use crate::nl2sql::ir::spec::{
        Filter, FilterOp, FilterValue, SpecProvenance, Subject,
    };
    use crate::router::{RouteEntities, StructuredBackend};

    fn config() -> Config {
        Config::from_env()
    }

    fn spec_for(concept: EntityConcept, shape: Shape) -> QuerySpec {
        QuerySpec {
            subject: Subject { concept, table: None },
            shape,
            measures: vec![],
            dimensions: vec![],
            filters: vec![],
            time: None,
            order: vec![],
            limit: None,
            joins: vec![],
            projection: vec![],
            related: vec![],
            duration_filters: vec![],
            provenance: SpecProvenance { rule: "T".into(), focus_subs: vec![] },
        }
    }

    fn decision(class: RouteClass, entities: RouteEntities, spec: Option<QuerySpec>) -> RouteDecision {
        RouteDecision {
            class,
            tier: 1,
            cached: false,
            tier2_attempted: false,
            entities,
            deterministic: spec.is_some(),
            service_line: None,
            source_id: None,
            scope: vec![],
            query_spec: spec,
            resolved_question: String::new(),
        }
    }

    fn structured() -> RouteClass {
        RouteClass::Structured {
            intent: QueryIntent::Aggregation,
            backend: StructuredBackend::SourceSql,
        }
    }

    #[test]
    fn patient_key_sets_and_replaces_the_entity() {
        let cfg = config();
        let mut ents = RouteEntities::default();
        ents.patient_key = Some("PT-00042".into());
        let f1 = apply(&ConversationFocus::default(), &decision(structured(), ents, None), None, "tell me about PT-00042", &cfg);
        assert_eq!(f1.patient.as_ref().map(|p| p.key.as_str()), Some("PT-00042"));
        assert_eq!(f1.turn, 1);

        let mut ents2 = RouteEntities::default();
        ents2.patient_key = Some("PT-00099".into());
        let f2 = apply(&f1, &decision(structured(), ents2, None), None, "and PT-00099?", &cfg);
        assert_eq!(f2.patient.as_ref().map(|p| p.key.as_str()), Some("PT-00099"));
        assert_eq!(f2.patient.as_ref().map(|p| p.set_turn), Some(2));
    }

    #[test]
    fn aggregate_question_keeps_the_patient() {
        let cfg = config();
        let mut ents = RouteEntities::default();
        ents.patient_key = Some("PT-00042".into());
        let f1 = apply(&ConversationFocus::default(), &decision(structured(), ents, None), None, "about PT-00042", &cfg);
        // A bed-count question names no patient at all.
        let f2 = apply(&f1, &decision(structured(), RouteEntities::default(), None), None, "how many beds are free in ICU", &cfg);
        assert_eq!(f2.patient.as_ref().map(|p| p.key.as_str()), Some("PT-00042"));
    }

    #[test]
    fn explicit_time_range_replaces_and_absence_keeps() {
        let cfg = config();
        let mut ents = RouteEntities::default();
        ents.time_range = Some(TimeRange::LastMonth);
        let f1 = apply(&ConversationFocus::default(), &decision(structured(), ents, None), None, "admissions last month", &cfg);
        assert_eq!(f1.time_range, Some(TimeRange::LastMonth));
        let f2 = apply(&f1, &decision(structured(), RouteEntities::default(), None), None, "by ward", &cfg);
        assert_eq!(f2.time_range, Some(TimeRange::LastMonth), "period must be inherited");
    }

    #[test]
    fn spec_sets_concept_and_last_spec() {
        let cfg = config();
        let spec = spec_for(EntityConcept::Admission, Shape::Scalar);
        let f = apply(&ConversationFocus::default(), &decision(structured(), RouteEntities::default(), Some(spec)), None, "how many admissions", &cfg);
        assert_eq!(f.concept, Some(EntityConcept::Admission));
        assert!(f.last_spec.is_some());
    }

    #[test]
    fn place_from_ward_role_filter() {
        let cfg = config();
        let mut spec = spec_for(EntityConcept::Admission, Shape::Grouped);
        spec.filters.push(Filter {
            column: ColumnRef::logical(EntityConcept::Ward, ColumnRole::WardRef),
            op: FilterOp::Eq,
            value: FilterValue::Str("ICU".into()),
        });
        let f = apply(&ConversationFocus::default(), &decision(structured(), RouteEntities::default(), Some(spec)), None, "only ICU", &cfg);
        assert_eq!(f.place.as_ref().map(|p| p.key.as_str()), Some("ICU"));
    }

    #[test]
    fn place_from_enum_filter_column_token() {
        let cfg = config();
        let mut ents = RouteEntities::default();
        ents.enum_filters = vec![("ward_name".into(), "Medical Ward B".into())];
        let f = apply(&ConversationFocus::default(), &decision(structured(), ents, None), None, "who is in Medical Ward B", &cfg);
        assert_eq!(f.place.as_ref().map(|p| p.key.as_str()), Some("Medical Ward B"));
    }

    #[test]
    fn clarify_sets_pending_and_answer_clears_it() {
        let cfg = config();
        let clarify = RouteClass::Clarify {
            question: "Which records did you mean?".into(),
            slot: MissingSlot::Subject,
        };
        let f1 = apply(&ConversationFocus::default(), &decision(clarify, RouteEntities::default(), None), None, "how many were cancelled?", &cfg);
        assert_eq!(f1.pending_clarify, Some(MissingSlot::Subject));
        assert_eq!(f1.pending_question.as_deref(), Some("how many were cancelled?"));

        let f2 = apply(&f1, &decision(structured(), RouteEntities::default(), None), None, "appointments", &cfg);
        assert!(f2.pending_clarify.is_none());
        assert!(f2.pending_question.is_none());
    }

    #[test]
    fn new_topic_clears_focus() {
        let cfg = config();
        let mut ents = RouteEntities::default();
        ents.patient_key = Some("PT-00042".into());
        ents.time_range = Some(TimeRange::LastMonth);
        let f1 = apply(&ConversationFocus::default(), &decision(structured(), ents, None), None, "about PT-00042 last month", &cfg);
        assert!(!f1.is_empty());

        let f2 = apply(&f1, &decision(structured(), RouteEntities::default(), None), None, "new topic: how many wards are there?", &cfg);
        assert!(f2.is_empty(), "reset must clear every slot, got {f2:?}");
        assert_eq!(f2.turn, 2, "the turn counter survives a reset");
    }

    #[test]
    fn reset_phrases_are_narrow() {
        assert!(is_reset_phrase("new topic"));
        assert!(is_reset_phrase("Forget that, how many beds are free?"));
        assert!(is_reset_phrase("let's start over"));
        assert!(is_reset_phrase("clear the context"));
        // Not resets:
        assert!(!is_reset_phrase("how many equipment resets last month?"));
        assert!(!is_reset_phrase("who is in ICU?"));
        assert!(!is_reset_phrase("what about the month before?"));
    }

    #[test]
    fn focus_used_lists_names_never_values() {
        let focus = ConversationFocus {
            patient: Some(FocusEntity {
                key: "PT-00042".into(),
                display: Some("Jane Chebet".into()),
                ..FocusEntity::default()
            }),
            time_range: Some(TimeRange::LastMonth),
            ..ConversationFocus::default()
        };
        let used = focus.focus_used();
        assert_eq!(used, vec!["patient".to_string(), "time_range".to_string()]);
        let joined = used.join(",");
        assert!(!joined.contains("PT-00042"));
        assert!(!joined.contains("Jane"));
    }

    #[test]
    fn debug_redacts_the_display_name() {
        let focus = ConversationFocus {
            patient: Some(FocusEntity {
                key: "PT-00042".into(),
                display: Some("Jane Chebet".into()),
                row_pk: Some("881".into()),
                ..FocusEntity::default()
            }),
            ..ConversationFocus::default()
        };
        let rendered = format!("{focus:?}");
        assert!(
            !rendered.contains("Jane Chebet"),
            "Debug leaked a patient name: {rendered}"
        );
        assert!(!rendered.contains("881"), "Debug leaked a row pk: {rendered}");
        assert!(rendered.contains("PT-00042"), "the business key is loggable");
    }

    #[test]
    fn top_labels_drop_pii_roles_and_truncate() {
        let long = "a".repeat(120);
        let labels = vec!["Discharged".to_string(), long];
        let digest = ResultDigest::new(Shape::Grouped)
            .with_labels(&labels, Some(ColumnRole::Status), false, 5);
        assert_eq!(digest.top_labels[0], "Discharged");
        assert_eq!(digest.top_labels[1].chars().count(), LABEL_MAX_CHARS);

        let names = vec!["Jane Chebet".to_string()];
        let pii = ResultDigest::new(Shape::Grouped)
            .with_labels(&names, Some(ColumnRole::PersonFullName), false, 5);
        assert!(pii.top_labels.is_empty(), "a person-name column must contribute no labels");

        let about = ResultDigest::new(Shape::Lookup)
            .with_labels(&names, Some(ColumnRole::PersonFullName), true, 5);
        assert_eq!(about.top_labels, vec!["Jane Chebet".to_string()]);
    }

    #[test]
    fn top_labels_respect_the_configured_cap() {
        let labels: Vec<String> = (0..12).map(|i| format!("v{i}")).collect();
        let digest = ResultDigest::new(Shape::Grouped).with_labels(&labels, None, false, 5);
        assert_eq!(digest.top_labels.len(), 5);
    }

    #[test]
    fn single_patient_detection_needs_one_distinct_value() {
        let columns = vec!["patient_no".to_string(), "full_name".to_string()];
        let one = vec![vec![
            serde_json::json!("PT-00042"),
            serde_json::json!("Jane Chebet"),
        ]];
        let found = ResultDigest::detect_single_patient(&columns, &one, "patient_no", Some("full_name"), 3)
            .expect("one distinct patient");
        assert_eq!(found.key, "PT-00042");
        assert_eq!(found.set_turn, 3);

        let two = vec![
            vec![serde_json::json!("PT-00042"), serde_json::json!("A")],
            vec![serde_json::json!("PT-00099"), serde_json::json!("B")],
        ];
        assert!(
            ResultDigest::detect_single_patient(&columns, &two, "patient_no", Some("full_name"), 3).is_none(),
            "two patients must not focus one of them"
        );

        assert!(
            ResultDigest::detect_single_patient(&columns, &one, "missing_col", None, 3).is_none()
        );
    }

    #[test]
    fn agg_rows_digest_carries_scalar_only_for_one_row() {
        let one = [AggRow { label: "total".into(), value: 42.0 }];
        let d = ResultDigest::from_agg_rows(Shape::Scalar, &one, 5);
        assert_eq!(d.scalar, Some(42.0));
        assert_eq!(d.row_count, 1);

        let many = [
            AggRow { label: "ICU".into(), value: 3.0 },
            AggRow { label: "Maternity".into(), value: 7.0 },
        ];
        let d = ResultDigest::from_agg_rows(Shape::Grouped, &many, 5);
        assert_eq!(d.scalar, None);
        assert_eq!(d.top_labels, vec!["ICU".to_string(), "Maternity".to_string()]);
    }

    #[test]
    fn json_round_trip_preserves_every_slot() {
        let focus = ConversationFocus {
            patient: Some(FocusEntity::from_key("PT-00042", 1)),
            concept: Some(EntityConcept::Admission),
            service_line: Some(ServiceLine::WardBoard),
            time_range: Some(TimeRange::LastMonth),
            last_spec: Some(spec_for(EntityConcept::Admission, Shape::Grouped)),
            last_result: Some(ResultDigest::new(Shape::Grouped).with_row_count(4)),
            pending_clarify: Some(MissingSlot::Dimension),
            pending_question: Some("by what?".into()),
            turn: 7,
            ..ConversationFocus::default()
        };
        let json = focus.to_json_string().expect("serialise");
        let back = ConversationFocus::from_json_str(&json).expect("deserialise");
        assert_eq!(back, focus);
    }

    #[test]
    fn malformed_stored_focus_is_none_not_a_panic() {
        assert!(ConversationFocus::from_json_str("{").is_none());
        assert!(ConversationFocus::from_json_str("null").is_none());
        // An unknown-shaped object still parses to an all-default focus, because
        // every field is `#[serde(default)]` — that is the fail-open contract.
        assert!(ConversationFocus::from_json_str(r#"{"unrelated": 1}"#).is_some());
    }

    #[test]
    fn router_projection_carries_the_five_plan_02_fields() {
        let focus = ConversationFocus {
            patient: Some(FocusEntity::from_key("PT-00042", 1)),
            concept: Some(EntityConcept::Encounter),
            service_line: Some(ServiceLine::Emergency),
            time_range: Some(TimeRange::LastWeek),
            last_spec: Some(spec_for(EntityConcept::Encounter, Shape::Scalar)),
            ..ConversationFocus::default()
        };
        let projected = focus.to_router_focus();
        assert_eq!(projected.patient_key.as_deref(), Some("PT-00042"));
        assert_eq!(projected.concept, Some(EntityConcept::Encounter));
        assert_eq!(projected.line, Some(ServiceLine::Emergency));
        assert_eq!(projected.time_range, Some(TimeRange::LastWeek));
        assert!(projected.last_spec.is_some());
    }

    #[test]
    fn prompt_block_is_none_when_focus_is_empty() {
        assert!(ConversationFocus::default().prompt_block().is_none());
        let focus = ConversationFocus {
            patient: Some(FocusEntity {
                key: "PT-00042".into(),
                display: Some("Jane Chebet".into()),
                ..FocusEntity::default()
            }),
            time_range: Some(TimeRange::LastMonth),
            ..ConversationFocus::default()
        };
        let block = focus.prompt_block().expect("block");
        assert!(block.contains("Jane Chebet"), "local prompts may name the patient");
        assert!(block.contains("last month"));
    }

    #[test]
    fn disabled_focus_never_accumulates() {
        let mut cfg = config();
        cfg.focus_enabled = false;
        let mut ents = RouteEntities::default();
        ents.patient_key = Some("PT-00042".into());
        let f = apply(&ConversationFocus::default(), &decision(structured(), ents, None), None, "about PT-00042", &cfg);
        assert!(f.is_empty());
        assert_eq!(f.turn, 0);
    }

    #[test]
    fn patient_ttl_expires_the_entity() {
        let mut cfg = config();
        cfg.focus_patient_ttl_turns = 1;
        let mut ents = RouteEntities::default();
        ents.patient_key = Some("PT-00042".into());
        let mut focus = apply(&ConversationFocus::default(), &decision(structured(), ents, None), None, "about PT-00042", &cfg);
        assert!(focus.patient.is_some());
        // Two turns with no patient mention: set_turn=1, so turn 3 is out of TTL.
        for _ in 0..2 {
            focus = apply(&focus, &decision(structured(), RouteEntities::default(), None), None, "how many wards", &cfg);
        }
        assert!(focus.patient.is_none(), "TTL must expire the patient");
    }
}
