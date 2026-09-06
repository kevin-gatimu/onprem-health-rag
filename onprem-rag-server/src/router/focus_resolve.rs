//! Focus resolution — turn a follow-up into a self-contained question, or into
//! an instruction to mutate the previous `QuerySpec` (plan 06 §3).
//!
//! **Zero model calls.** Every rule here is a regex over the question plus a
//! lookup in [`ConversationFocus`]. That is the point: "why?" and "by ward" are
//! the two most common things a person types, and paying a classifier round-trip
//! to understand them is both slow and unreliable.
//!
//! This module supersedes [`crate::router::focus::resolve`], which is plan 02's
//! narrower resolver over plan 02's narrower focus type. Both are left standing
//! deliberately: the older one is wired into `route_v3` and is the router owner's
//! to retire. Nothing here edits it.
//!
//! # What comes out
//!
//! [`Resolved`] carries three things, and the caller must honour all three:
//!
//! * `question` — the rewritten text, safe to hand to the parser or a model.
//! * `substitutions` — what was borrowed from focus, for `focus_used` and for
//!   `SpecProvenance::focus_subs`.
//! * `action` — what the caller should *do*, which for an ellipsis or a "why?" is
//!   not "route this text" at all.
//!
//! # SQL safety
//!
//! Two functions here produce a `QuerySpec` ([`fill_slot_in_spec`] and
//! [`mark_unbound`]). Both return specs whose `subject.table` is `None`, so
//! `compile()` fails with `UnboundSubject` unless `bind()` runs first — and the
//! bound-and-compiled path already ends in `nl2sql::validate::validate_sql`.
//! A caller therefore *cannot* route one of these specs to a database without
//! passing the existing guard; there is no second copy of it here.

use chrono::{DateTime, Utc};
use regex::Regex;
use std::sync::LazyLock;

use crate::memory::focus::ConversationFocus;
use crate::nl2sql::ir::spec::{
    ColumnRef, Dimension, Filter, FilterOp, FilterValue, MissingSlot, QuerySpec, Subject, TimeScope,
};
use crate::ontology::concepts::{DESCRIPTORS, EntityConcept};
use crate::ontology::roles::ColumnRole;
use crate::router::focus::time_range_label;
use crate::router::time::parse_time;

/// Token ceiling for treating a reply as a slot answer rather than a fresh
/// question (plan 06 §4). "appointments" is an answer; a full sentence is not.
const SLOT_ANSWER_MAX_TOKENS: usize = 6;

// ---------------------------------------------------------------------------
// Substitutions
// ---------------------------------------------------------------------------

/// Which focus slot a substitution drew on. The wire value of `focus_used` is
/// the list of these names — never the values behind them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FocusSlot {
    Patient,
    Provider,
    Place,
    TimeRange,
    Concept,
    LastSpec,
}

impl FocusSlot {
    pub fn name(self) -> &'static str {
        match self {
            FocusSlot::Patient => "patient",
            FocusSlot::Provider => "provider",
            FocusSlot::Place => "place",
            FocusSlot::TimeRange => "time_range",
            FocusSlot::Concept => "concept",
            FocusSlot::LastSpec => "last_spec",
        }
    }
}

/// One reference expanded from focus.
#[derive(Debug, Clone, PartialEq)]
pub struct Substitution {
    pub slot: FocusSlot,
    /// The phrase as the user wrote it ("the patient", "there").
    pub from: String,
    /// What it was replaced with.
    pub to: String,
}

impl Substitution {
    /// Trace/provenance form. Goes into `SpecProvenance::focus_subs` and the
    /// server log — **not** to the client, which sees only slot names, because
    /// `to` can be a ward or a patient key.
    pub fn log(&self) -> String {
        format!("'{}' → '{}'", self.from, self.to)
    }
}

// ---------------------------------------------------------------------------
// Actions
// ---------------------------------------------------------------------------

/// What the router should do with a resolved follow-up.
#[derive(Debug, Clone, PartialEq)]
pub enum FollowUpAction {
    /// Route `Resolved::question` normally. Also the answer when there is no
    /// focus at all.
    Route,
    /// The question was a bare ellipsis over the previous result. The caller
    /// passes `phrase` and `focus.last_spec` to
    /// `nl2sql::ir::mutate::mutate(last, phrase, ents, binding)`; on `Some` it
    /// reports `Structured{SourceSql}` with `deterministic: true`, and on `None`
    /// falls back to routing the text.
    MutateSpec { phrase: String },
    /// "why?" / "explain that" after a structured answer: retrieve narrative
    /// evidence for the rows the last spec selected (`Hybrid`, last spec as
    /// cohort).
    ExplainLast,
    /// A slot answer arrived while a clarification was outstanding. `fill` holds
    /// the merged question and, when one could be built, the merged spec.
    FillClarify { fill: Box<ClarifyFill> },
    /// An explicit "new topic" / "forget that". The caller drops the focus and
    /// routes the question with none.
    Reset,
}

/// The outcome of focus resolution.
#[derive(Debug, Clone, PartialEq)]
pub struct Resolved {
    pub question: String,
    pub substitutions: Vec<Substitution>,
    pub action: FollowUpAction,
}

impl Resolved {
    /// No focus was used and no follow-up pattern matched.
    pub fn unchanged(question: &str) -> Self {
        Resolved {
            question: question.to_string(),
            substitutions: Vec::new(),
            action: FollowUpAction::Route,
        }
    }

    /// Distinct slot names, in first-use order — the `focus_used` wire value.
    pub fn focus_used(&self) -> Vec<String> {
        let mut out: Vec<String> = Vec::new();
        for s in &self.substitutions {
            let name = s.slot.name().to_string();
            if !out.contains(&name) {
                out.push(name);
            }
        }
        out
    }

    /// Human-readable substitution log for `SpecProvenance::focus_subs`.
    pub fn substitution_log(&self) -> Vec<String> {
        self.substitutions.iter().map(Substitution::log).collect()
    }

    /// True when the caller must not simply route `question`.
    pub fn is_follow_up(&self) -> bool {
        !matches!(self.action, FollowUpAction::Route)
    }
}

// ---------------------------------------------------------------------------
// Patterns
// ---------------------------------------------------------------------------

fn re(src: &str) -> Regex {
    Regex::new(src).expect("focus_resolve pattern compiles")
}

/// Multi-word patient references, longest first so "the same patient" is not
/// half-consumed by "the patient".
static PATIENT_PHRASE: LazyLock<Regex> =
    LazyLock::new(|| re(r"(?i)\b(the same patient|that patient|this patient|the patient)\b"));
/// Possessive pronouns — rewritten with a genitive so the result reads as English.
static PATIENT_POSSESSIVE: LazyLock<Regex> = LazyLock::new(|| re(r"(?i)\b(his|her|their)\b"));
/// Nominative / accusative pronouns.
static PATIENT_PRONOUN: LazyLock<Regex> = LazyLock::new(|| re(r"(?i)\b(he|she|him|they|them)\b"));

static PLACE_PHRASE: LazyLock<Regex> = LazyLock::new(|| {
    re(r"(?i)\b(the same ward|that ward|the same department|that department|the same unit|that unit|in there|there)\b")
});

static TIME_PHRASE: LazyLock<Regex> = LazyLock::new(|| {
    re(r"(?i)\b(the same period|that period|same period|the same month|the same week|those dates|back then|then)\b")
});

static PROVIDER_PHRASE: LazyLock<Regex> = LazyLock::new(|| {
    re(r"(?i)\b(the same provider|that provider|that doctor|the same doctor|that clinician)\b")
});

/// "why?" and its family. Anchored: "why were admissions up in ICU last month"
/// is a real question with its own subject, not a request to explain the last
/// answer, so only a *bare* why counts.
static EXPLAIN_LAST: LazyLock<Regex> = LazyLock::new(|| {
    re(r"(?ix)^\s*(
          why\s*[?.!]*
        | why\s+(is|was|are|were)\s+(that|this|it)\s*[?.!]*
        | explain(\s+(that|this|it|those))?\s*[?.!]*
        | summari[sz]e\s+(that|those|them|it)\s*[?.!]*
        | what\s+does\s+that\s+mean\s*[?.!]*
        | how\s+come\s*[?.!]*
    )$")
});

/// Bare-ellipsis openers (plan 06 §3 row 5). Each is anchored at the start of
/// the question, because these words mid-sentence belong to a complete question.
static ELLIPSIS: LazyLock<Regex> = LazyLock::new(|| {
    re(r"(?ix)^\s*(and\s+)?(
          (by|per)\s+\w+
        | what\s+about\b
        | how\s+about\b
        | and\s+for\b
        | (only|just)\s+\S+
        | as\s+a\s+(percentage|percent|rate|proportion|share)\b
        | as\s+%\s*
        | in\s+percent(age)?\b
        | top\s+\d+
        | bottom\s+\d+
        | the\s+other\s+way\b
        | (ascending|descending)\b
        | flip\s+(the\s+)?order\b
        | without\s+\S+
        | same\s+(for|but\s+for)\b
        | compared?\s+(with|to)\b
    )")
});

/// "per week instead" — a bucket change, which may legitimately trail a longer
/// phrase, so it is matched anywhere rather than anchored.
static BUCKET_INSTEAD: LazyLock<Regex> = LazyLock::new(|| {
    re(r"(?i)\bper\s+(hour|day|week|month|quarter|year)s?\s+instead\b")
});

// ---------------------------------------------------------------------------
// resolve
// ---------------------------------------------------------------------------

/// Resolve a question against the conversation focus (plan 06 §3).
///
/// `now` is injected rather than read from the clock so a test can pin the
/// interpretation of "last quarter" — the same discipline
/// [`crate::router::time::parse_time`] already imposes.
///
/// Rule precedence, and why:
///
/// 1. **Reset** — an explicit instruction outranks anything inferred from it.
/// 2. **Pending clarify** — the user is answering *our* question; reading their
///    one-word reply as a fresh question is the failure this exists to prevent.
/// 3. **Explain-last** — "why?" has no subject of its own to route.
/// 4. **Ellipsis** — a spec mutation is exact where a text rewrite would guess.
/// 5. **Textual rewrite** — pronouns and place/time references.
pub fn resolve(question: &str, focus: &ConversationFocus, now: DateTime<Utc>) -> Resolved {
    // 1 — explicit reset.
    if crate::memory::focus::is_reset_phrase(question) {
        return Resolved {
            question: question.to_string(),
            substitutions: Vec::new(),
            action: FollowUpAction::Reset,
        };
    }

    // 2 — answering an outstanding clarification.
    if focus.pending_clarify.is_some() {
        if let Some(fill) = fill_pending_clarify(focus, question, now) {
            let mut subs = vec![Substitution {
                slot: FocusSlot::Concept,
                from: question.trim().to_string(),
                to: fill.question.clone(),
            }];
            if fill.spec.is_some() {
                subs.push(Substitution {
                    slot: FocusSlot::LastSpec,
                    from: "pending clarify".to_string(),
                    to: "merged spec".to_string(),
                });
            }
            return Resolved {
                question: fill.question.clone(),
                substitutions: subs,
                action: FollowUpAction::FillClarify {
                    fill: Box::new(fill),
                },
            };
        }
        // Fill failed: fall through and treat the text as a fresh question. The
        // caller clears `pending_clarify` — never clarify twice (plan 02 §5).
    }

    // 3 — "why?" over the previous structured answer.
    if focus.last_spec.is_some() && EXPLAIN_LAST.is_match(question) {
        return Resolved {
            question: question.to_string(),
            substitutions: vec![Substitution {
                slot: FocusSlot::LastSpec,
                from: question.trim().to_string(),
                to: "explain last result".to_string(),
            }],
            action: FollowUpAction::ExplainLast,
        };
    }

    // 4 — bare ellipsis over the previous spec.
    if focus.last_spec.is_some() && is_ellipsis(question) {
        return Resolved {
            question: question.to_string(),
            substitutions: vec![Substitution {
                slot: FocusSlot::LastSpec,
                from: question.trim().to_string(),
                to: "mutate previous spec".to_string(),
            }],
            action: FollowUpAction::MutateSpec {
                phrase: question.trim().to_string(),
            },
        };
    }

    // 5 — textual rewrites.
    let mut q = question.to_string();
    let mut subs: Vec<Substitution> = Vec::new();

    if let Some(patient) = &focus.patient {
        let to = format!("patient {}", patient.key);
        if let Some((next, from)) = replace_first(&q, &PATIENT_PHRASE, &to) {
            q = next;
            subs.push(Substitution { slot: FocusSlot::Patient, from, to: to.clone() });
        } else if let Some((next, from)) =
            replace_first(&q, &PATIENT_POSSESSIVE, &format!("{to}'s"))
        {
            q = next;
            subs.push(Substitution { slot: FocusSlot::Patient, from, to: format!("{to}'s") });
        } else if let Some((next, from)) = replace_first(&q, &PATIENT_PRONOUN, &to) {
            q = next;
            subs.push(Substitution { slot: FocusSlot::Patient, from, to });
        }
    }

    if let Some(place) = &focus.place {
        let to = place.label().to_string();
        if let Some((next, from)) = replace_first(&q, &PLACE_PHRASE, &to) {
            q = next;
            subs.push(Substitution { slot: FocusSlot::Place, from, to });
        }
    }

    if let Some(tr) = &focus.time_range {
        let to = time_range_label(tr);
        if let Some((next, from)) = replace_first(&q, &TIME_PHRASE, &to) {
            q = next;
            subs.push(Substitution { slot: FocusSlot::TimeRange, from, to });
        }
    }

    // Provider only when there is no patient in focus: with both present,
    // "him"/"her" is ambiguous, and guessing the wrong person is worse than
    // leaving the pronoun for the parser to fail on visibly (plan 06 §3 row 4).
    if focus.patient.is_none() {
        if let Some(provider) = &focus.provider {
            let to = provider.label().to_string();
            if let Some((next, from)) = replace_first(&q, &PROVIDER_PHRASE, &to) {
                q = next;
                subs.push(Substitution { slot: FocusSlot::Provider, from, to });
            }
        }
    }

    Resolved {
        question: q,
        substitutions: subs,
        action: FollowUpAction::Route,
    }
}

/// Whether the question is a bare ellipsis over the previous result.
pub fn is_ellipsis(question: &str) -> bool {
    ELLIPSIS.is_match(question) || BUCKET_INSTEAD.is_match(question)
}

/// Replace the first match of `pattern`, returning the new string and the text
/// that was matched. `None` when the pattern does not appear.
fn replace_first(q: &str, pattern: &Regex, to: &str) -> Option<(String, String)> {
    let m = pattern.find(q)?;
    let from = m.as_str().to_string();
    let mut next = String::with_capacity(q.len() + to.len());
    next.push_str(&q[..m.start()]);
    next.push_str(to);
    next.push_str(&q[m.end()..]);
    Some((next, from))
}

// ---------------------------------------------------------------------------
// Clarify round-trip (plan 06 §4)
// ---------------------------------------------------------------------------

/// A pending clarification, answered.
#[derive(Debug, Clone, PartialEq)]
pub struct ClarifyFill {
    /// Which slot the reply filled.
    pub slot: MissingSlot,
    /// The normalised value taken from the reply.
    pub value: String,
    /// The original question with the slot filled in — always present, and
    /// always routable through the normal parser.
    pub question: String,
    /// The previous spec with the slot filled, when one could be built. **Always
    /// unbound** (see [`mark_unbound`]).
    pub spec: Option<QuerySpec>,
}

/// Try to read `answer` as the reply to `focus.pending_clarify`.
///
/// Returns `None` when there is no pending clarification, when the reply is too
/// long to be a slot answer *and* does not match the slot's type, or when the
/// value cannot be interpreted for that slot. `None` means "treat it as a fresh
/// question" — the caller then clears `pending_clarify`.
pub fn fill_pending_clarify(
    focus: &ConversationFocus,
    answer: &str,
    now: DateTime<Utc>,
) -> Option<ClarifyFill> {
    let slot = focus.pending_clarify.clone()?;
    let trimmed = answer.trim();
    if trimmed.is_empty() {
        return None;
    }
    let short = trimmed.split_whitespace().count() <= SLOT_ANSWER_MAX_TOKENS;

    // A reply that is itself a question is a fresh question, never a slot value.
    // The `typed` flag below cannot carry this distinction for `Subject`:
    // `concept_from_text` is a *substring* scan, so any sentence that merely
    // mentions a concept ("never mind, what is the bed occupancy right now?")
    // matches, sets `typed`, and would bypass the length guard — splicing the
    // new noun into the OLD pending question and asking something the user never
    // said. Short replies are deliberately exempt: "last week?" is a hedged slot
    // answer, not a new question.
    if !short && is_interrogative(trimmed) {
        return None;
    }

    let (value, typed) = match slot {
        MissingSlot::Subject => {
            let concept = concept_from_text(trimmed)?;
            (concept_label(concept).to_string(), true)
        }
        MissingSlot::TimeRange => {
            let tr = parse_time(trimmed, now)?;
            (time_range_label(&tr), true)
        }
        MissingSlot::Patient => {
            let key = identifier_from_text(trimmed)?;
            (key, true)
        }
        // A dimension or metric name is an open vocabulary — the schema binder
        // decides whether it resolves, so accept any short reply and let the
        // re-route fail visibly if it does not bind.
        MissingSlot::Dimension | MissingSlot::Metric => (trimmed.to_string(), false),
    };
    if !short && !typed {
        return None;
    }

    let pending = focus.pending_question.as_deref().unwrap_or("").trim();
    let question = merge_into_question(pending, &slot, &value, trimmed);
    let spec = focus
        .last_spec
        .as_ref()
        .and_then(|last| fill_slot_in_spec(last, &slot, &value, now));

    Some(ClarifyFill {
        slot,
        value,
        question,
        spec,
    })
}

/// Splice a slot value into the question that triggered the clarification.
///
/// When there is no stored question (a clarify from a path that did not persist
/// one) the reply itself is the question — better a thin question than a
/// fabricated one.
fn merge_into_question(pending: &str, slot: &MissingSlot, value: &str, raw: &str) -> String {
    if pending.is_empty() {
        return raw.to_string();
    }
    match slot {
        MissingSlot::Subject => {
            // "how many were cancelled?" + "appointments"
            //   → "how many appointments were cancelled?"
            static LEAD: LazyLock<Regex> = LazyLock::new(|| {
                re(r"(?i)^\s*(how many|how much|count of|count|list all|list|show me|show)\b")
            });
            match LEAD.find(pending) {
                Some(m) => format!(
                    "{} {}{}",
                    &pending[..m.end()],
                    value,
                    &pending[m.end()..]
                ),
                None => format!("{pending} ({value})"),
            }
        }
        MissingSlot::TimeRange => format!("{pending} {value}"),
        MissingSlot::Patient => format!("{pending} for patient {value}"),
        MissingSlot::Dimension => format!("{pending} by {value}"),
        MissingSlot::Metric => format!("{pending} ({value})"),
    }
    .split_whitespace()
    .collect::<Vec<_>>()
    .join(" ")
}

/// Concept whose singular / plural / synonym appears in `text`.
///
/// First match in `DESCRIPTORS` order, which is stable and documented — the same
/// scan `router::entities` uses, so a term that resolves there resolves here.
pub fn concept_from_text(text: &str) -> Option<EntityConcept> {
    let t = text.to_ascii_lowercase();
    DESCRIPTORS
        .iter()
        .find(|d| {
            t.contains(d.singular) || t.contains(d.plural) || d.synonyms.iter().any(|s| t.contains(s))
        })
        .map(|d| d.concept)
}

fn concept_label(concept: EntityConcept) -> &'static str {
    DESCRIPTORS
        .iter()
        .find(|d| d.concept == concept)
        .map(|d| d.plural)
        .unwrap_or("records")
}

/// A business identifier in a short reply ("PT-00042", "ENC-2026-0007").
///
/// Broader than `nl2sql::text::extract_record_identifier`, which requires two
/// dashes: a clarify reply is often the bare key a user reads off a screen.
/// Business key mentioned in `text`, upper-cased.
///
/// The digit run is `{2,8}`, not `{2,4}`: real keys are five digits wide
/// (`patients.patient_no` is seeded `PT-00001`, `02_seed.sql:941`, on a
/// `VARCHAR(20)` column, `01_schema.sql:236`), so a 4-digit cap failed to match
/// every key the dev source actually contains — `\d{2,4}` consumed `0004` and
/// then found no word boundary before the trailing `2`. Bounded rather than `+`
/// so a long digit string is not mistaken for a key.
fn identifier_from_text(text: &str) -> Option<String> {
    static ID: LazyLock<Regex> =
        LazyLock::new(|| re(r"(?i)\b[a-z]{2,6}-?\d{2,8}(-\d{2,6})?\b"));
    ID.find(text).map(|m| m.as_str().to_ascii_uppercase())
}

/// Does `text` read as a question in its own right?
///
/// A trailing `?` or any interrogative word. Used only to reject *long* replies
/// during clarification: a user who types a whole question has moved on from the
/// slot we asked about, so filling it would fabricate a question they never
/// asked. Short replies never reach this check.
fn is_interrogative(text: &str) -> bool {
    static WH: LazyLock<Regex> =
        LazyLock::new(|| re(r"(?i)\b(what|which|who|whom|whose|when|where|why|how)\b"));
    text.ends_with('?') || WH.is_match(text)
}

// ---------------------------------------------------------------------------
// Spec slot filling
// ---------------------------------------------------------------------------

/// Clear every binding decision on a spec, so `bind()` must run again.
///
/// `subject.table = None` alone makes `compile()` return
/// `CompileError::UnboundSubject`; the column and join clearing keeps a stale
/// physical name from a *previous* schema out of the new spec. This is the
/// mechanism by which nothing in this module can reach a database without going
/// through bind → compile → `validate_sql`.
pub fn mark_unbound(spec: &mut QuerySpec) {
    spec.subject.table = None;
    spec.joins.clear();
    for f in &mut spec.filters {
        f.column.physical = None;
    }
    for d in &mut spec.dimensions {
        d.column.physical = None;
    }
    for m in &mut spec.measures {
        if let Some(crate::nl2sql::ir::spec::ValueExpr::Column(c)) = m.target.as_mut() {
            c.physical = None;
        }
    }
    if let Some(t) = spec.time.as_mut() {
        t.column.physical = None;
    }
}

/// Fill one missing slot in the previous spec (plan 06 §4).
///
/// The returned spec is unbound, so the caller must `bind()` it — which is also
/// how an unfillable combination surfaces: a bind failure is visible, whereas a
/// silently half-filled spec is not.
///
/// `Metric` returns `None`: choosing an aggregate column needs the schema
/// binding, which this module does not have. The caller re-routes the merged
/// question instead, and the parser picks the measure with the binding in hand.
pub fn fill_slot_in_spec(
    last: &QuerySpec,
    slot: &MissingSlot,
    value: &str,
    now: DateTime<Utc>,
) -> Option<QuerySpec> {
    let mut spec = last.clone();
    match slot {
        MissingSlot::Subject => {
            let concept = concept_from_text(value)?;
            spec.subject = Subject { concept, table: None };
            // A subject change invalidates every column addressed against the
            // old concept — drop them rather than carry a mismatch into bind.
            spec.dimensions.clear();
            spec.filters.clear();
            spec.time = None;
        }
        MissingSlot::TimeRange => {
            let range = parse_time(value, now)?;
            let column = spec
                .time
                .as_ref()
                .map(|t| t.column.clone())
                .unwrap_or_else(|| {
                    ColumnRef::logical(spec.subject.concept, ColumnRole::EventTime)
                });
            let bucket = spec.time.as_ref().and_then(|t| t.bucket);
            spec.time = Some(TimeScope {
                column,
                range: Some(range),
                bucket,
            });
        }
        MissingSlot::Patient => {
            let key = identifier_from_text(value)?;
            spec.filters.retain(|f| f.column.role != ColumnRole::BusinessId);
            spec.filters.push(Filter {
                column: ColumnRef::logical(EntityConcept::Patient, ColumnRole::BusinessId),
                op: FilterOp::Eq,
                value: FilterValue::Str(key),
            });
        }
        MissingSlot::Dimension => {
            // A name hint, not a physical column: `bind()` matches it against
            // the real schema, so no physical name is invented here.
            spec.dimensions = vec![Dimension {
                column: ColumnRef::with_hint(
                    spec.subject.concept,
                    ColumnRole::Category,
                    value,
                ),
                label: value.to_string(),
            }];
        }
        MissingSlot::Metric => return None,
    }
    mark_unbound(&mut spec);
    spec.provenance.focus_subs.push(format!(
        "clarify {} → '{}'",
        slot_name(slot),
        value
    ));
    Some(spec)
}

fn slot_name(slot: &MissingSlot) -> &'static str {
    match slot {
        MissingSlot::Subject => "subject",
        MissingSlot::Patient => "patient",
        MissingSlot::TimeRange => "time_range",
        MissingSlot::Dimension => "dimension",
        MissingSlot::Metric => "metric",
    }
}

/// The last spec re-cast as the cohort of an `ExplainLast` follow-up.
///
/// A "why?" needs the *rows*, not the aggregate: a `Scalar` count over admissions
/// becomes a `List` of those admissions, which is what the retrieval filter can
/// use. Unbound, like everything else leaving this module.
pub fn cohort_from_last(last: &QuerySpec) -> QuerySpec {
    use crate::nl2sql::ir::spec::Shape;
    let mut spec = last.clone();
    spec.shape = Shape::List;
    spec.measures.clear();
    spec.dimensions.clear();
    spec.order.clear();
    mark_unbound(&mut spec);
    spec.provenance
        .focus_subs
        .push("explain last result".to_string());
    spec
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::memory::focus::{FocusEntity, ResultDigest};
    use crate::nl2sql::ir::spec::{Measure, MeasureOp, Shape, SpecProvenance, TimeRange};

    fn now() -> DateTime<Utc> {
        "2026-08-15T12:00:00Z".parse().expect("fixed clock")
    }

    fn spec(concept: EntityConcept, shape: Shape) -> QuerySpec {
        QuerySpec {
            subject: Subject { concept, table: Some("phys_table".into()) },
            shape,
            measures: vec![Measure {
                op: MeasureOp::Count,
                target: None,
                alias: "count".into(),
            }],
            dimensions: vec![],
            filters: vec![],
            time: None,
            order: vec![],
            limit: None,
            joins: vec![],
            projection: vec![],
            related: vec![],
            duration_filters: vec![],
            provenance: SpecProvenance { rule: "R1".into(), focus_subs: vec![] },
        }
    }

    fn focus_with_patient() -> ConversationFocus {
        ConversationFocus {
            patient: Some(FocusEntity {
                key: "PT-00042".into(),
                display: Some("Jane Chebet".into()),
                ..FocusEntity::default()
            }),
            ..ConversationFocus::default()
        }
    }

    // ── A10a → A10b (plan 06 §9 first test) ─────────────────────────────────

    #[test]
    fn a10b_patient_name_resolves_from_focus_without_a_model() {
        let focus = focus_with_patient();
        let r = resolve("what is the patient's name?", &focus, now());
        assert!(
            r.question.contains("patient PT-00042"),
            "expected the focus key spliced in, got: {}",
            r.question
        );
        assert_eq!(r.focus_used(), vec!["patient".to_string()]);
        assert_eq!(r.action, FollowUpAction::Route);
    }

    #[test]
    fn pronouns_resolve_to_the_focus_patient() {
        let focus = focus_with_patient();
        for q in ["what are his allergies?", "was she admitted last week?", "list them"] {
            let r = resolve(q, &focus, now());
            assert!(
                r.question.contains("PT-00042"),
                "{q} did not resolve: {}",
                r.question
            );
            assert_eq!(r.focus_used(), vec!["patient".to_string()]);
        }
    }

    #[test]
    fn possessive_pronoun_keeps_the_sentence_grammatical() {
        let focus = focus_with_patient();
        let r = resolve("what are their vitals?", &focus, now());
        assert!(r.question.contains("patient PT-00042's vitals"), "got: {}", r.question);
    }

    #[test]
    fn no_focus_means_no_substitution() {
        let r = resolve("what are his allergies?", &ConversationFocus::default(), now());
        assert_eq!(r.question, "what are his allergies?");
        assert!(r.substitutions.is_empty());
        assert_eq!(r.action, FollowUpAction::Route);
    }

    #[test]
    fn place_and_time_references_resolve() {
        let focus = ConversationFocus {
            place: Some(FocusEntity {
                key: "ICU".into(),
                display: Some("ICU".into()),
                ..FocusEntity::default()
            }),
            time_range: Some(TimeRange::LastMonth),
            ..ConversationFocus::default()
        };
        let r = resolve("how many beds are free there in that period?", &focus, now());
        assert!(r.question.contains("ICU"), "got: {}", r.question);
        assert!(r.question.contains("last month"), "got: {}", r.question);
        assert_eq!(r.focus_used(), vec!["place".to_string(), "time_range".to_string()]);
    }

    #[test]
    fn provider_resolves_only_without_a_patient_in_focus() {
        let provider = FocusEntity {
            key: "DR-7".into(),
            display: Some("Dr Otieno".into()),
            ..FocusEntity::default()
        };
        let focus = ConversationFocus {
            provider: Some(provider.clone()),
            ..ConversationFocus::default()
        };
        let r = resolve("how many surgeries did that doctor perform?", &focus, now());
        assert!(r.question.contains("Dr Otieno"), "got: {}", r.question);
        assert_eq!(r.focus_used(), vec!["provider".to_string()]);

        // With a patient also in focus the provider phrase is left alone.
        let ambiguous = ConversationFocus {
            provider: Some(provider),
            ..focus_with_patient()
        };
        let r = resolve("how many surgeries did that doctor perform?", &ambiguous, now());
        assert!(!r.question.contains("Dr Otieno"), "got: {}", r.question);
    }

    // ── §9 six-step sequence: the *routing decisions* it must produce ───────

    #[test]
    fn dev_seed_sequence_produces_the_expected_actions() {
        let mut focus = ConversationFocus::default();
        // Turn 1: a full question — nothing to resolve.
        let r = resolve("how many admissions last month", &focus, now());
        assert_eq!(r.action, FollowUpAction::Route);

        // From turn 2 on there is a previous spec to mutate.
        focus.last_spec = Some(spec(EntityConcept::Admission, Shape::Scalar));
        focus.last_result = Some(ResultDigest::new(Shape::Scalar).with_scalar(128.0));
        focus.time_range = Some(TimeRange::LastMonth);

        for q in [
            "by ward",
            "only ICU",
            "as a percentage",
            "what about the month before",
        ] {
            let r = resolve(q, &focus, now());
            assert_eq!(
                r.action,
                FollowUpAction::MutateSpec { phrase: q.to_string() },
                "{q} should mutate the previous spec, got {:?}",
                r.action
            );
            assert_eq!(r.focus_used(), vec!["last_spec".to_string()]);
        }

        // Step 6: "why?" → explain the last result.
        let r = resolve("why?", &focus, now());
        assert_eq!(r.action, FollowUpAction::ExplainLast);
    }

    #[test]
    fn ellipsis_needs_a_previous_spec() {
        let focus = ConversationFocus::default();
        let r = resolve("by ward", &focus, now());
        assert_eq!(
            r.action,
            FollowUpAction::Route,
            "with no last_spec there is nothing to mutate"
        );
    }

    #[test]
    fn ellipsis_patterns_are_anchored() {
        assert!(is_ellipsis("by ward"));
        assert!(is_ellipsis("and by gender?"));
        assert!(is_ellipsis("what about last month?"));
        assert!(is_ellipsis("only caesareans"));
        assert!(is_ellipsis("just ICU"));
        assert!(is_ellipsis("as a percentage"));
        assert!(is_ellipsis("per week instead"));
        assert!(is_ellipsis("top 5"));
        assert!(is_ellipsis("without the cancelled ones"));
        // Complete questions must not be mistaken for ellipses.
        assert!(!is_ellipsis("how many admissions by ward last month?"));
        assert!(!is_ellipsis("which patients are in ICU?"));
        assert!(!is_ellipsis("show admissions per week for the last year"));
    }

    #[test]
    fn explain_last_is_only_a_bare_why() {
        let mut focus = ConversationFocus::default();
        focus.last_spec = Some(spec(EntityConcept::Admission, Shape::Scalar));
        for q in ["why?", "why is that", "explain that", "summarise those", "how come?"] {
            assert_eq!(
                resolve(q, &focus, now()).action,
                FollowUpAction::ExplainLast,
                "{q}"
            );
        }
        // A why-question with its own subject routes normally.
        let r = resolve("why were admissions higher in ICU last month?", &focus, now());
        assert_eq!(r.action, FollowUpAction::Route);
    }

    #[test]
    fn reset_outranks_everything_else() {
        let mut focus = focus_with_patient();
        focus.last_spec = Some(spec(EntityConcept::Admission, Shape::Scalar));
        focus.pending_clarify = Some(MissingSlot::Subject);
        let r = resolve("forget that, new topic", &focus, now());
        assert_eq!(r.action, FollowUpAction::Reset);
        assert!(r.substitutions.is_empty());
    }

    // ── §9 clarify round-trip ───────────────────────────────────────────────

    #[test]
    fn clarify_round_trip_subject_merges_into_the_question() {
        let focus = ConversationFocus {
            pending_clarify: Some(MissingSlot::Subject),
            pending_question: Some("how many were cancelled?".into()),
            ..ConversationFocus::default()
        };
        let r = resolve("appointments", &focus, now());
        let FollowUpAction::FillClarify { fill } = &r.action else {
            panic!("expected FillClarify, got {:?}", r.action);
        };
        assert_eq!(fill.slot, MissingSlot::Subject);
        assert_eq!(r.question, "how many appointments were cancelled?");
        // No previous spec existed, so the merged question is the whole payload.
        assert!(fill.spec.is_none());
    }

    #[test]
    fn clarify_round_trip_fills_the_time_slot_in_the_spec() {
        let focus = ConversationFocus {
            pending_clarify: Some(MissingSlot::TimeRange),
            pending_question: Some("how many admissions".into()),
            last_spec: Some(spec(EntityConcept::Admission, Shape::Scalar)),
            ..ConversationFocus::default()
        };
        let r = resolve("last quarter", &focus, now());
        let FollowUpAction::FillClarify { fill } = &r.action else {
            panic!("expected FillClarify, got {:?}", r.action);
        };
        assert_eq!(r.question, "how many admissions last quarter");
        let merged = fill.spec.as_ref().expect("spec filled");
        assert_eq!(
            merged.time.as_ref().and_then(|t| t.range.clone()),
            Some(TimeRange::LastQuarter)
        );
    }

    #[test]
    fn clarify_reply_that_does_not_answer_is_a_fresh_question() {
        let focus = ConversationFocus {
            pending_clarify: Some(MissingSlot::Subject),
            pending_question: Some("how many were cancelled?".into()),
            ..ConversationFocus::default()
        };
        // No concept term anywhere in this reply.
        let r = resolve("actually never mind, what is the bed occupancy right now?", &focus, now());
        assert_eq!(r.action, FollowUpAction::Route);
        assert_eq!(r.question, "actually never mind, what is the bed occupancy right now?");
    }

    #[test]
    fn clarify_patient_slot_accepts_a_bare_key() {
        let focus = ConversationFocus {
            pending_clarify: Some(MissingSlot::Patient),
            pending_question: Some("show the last vitals".into()),
            last_spec: Some(spec(EntityConcept::VitalSign, Shape::List)),
            ..ConversationFocus::default()
        };
        let r = resolve("PT-00042", &focus, now());
        let FollowUpAction::FillClarify { fill } = &r.action else {
            panic!("expected FillClarify, got {:?}", r.action);
        };
        assert_eq!(fill.value, "PT-00042");
        let merged = fill.spec.as_ref().expect("spec filled");
        assert!(
            merged.filters.iter().any(|f| f.column.role == ColumnRole::BusinessId
                && f.value == FilterValue::Str("PT-00042".into())),
            "expected a BusinessId filter, got {:?}",
            merged.filters
        );
    }

    #[test]
    fn metric_slot_cannot_be_filled_without_the_binding() {
        let last = spec(EntityConcept::Admission, Shape::Scalar);
        assert!(fill_slot_in_spec(&last, &MissingSlot::Metric, "average", now()).is_none());
    }

    // ── SQL-safety invariants ───────────────────────────────────────────────

    #[test]
    fn every_produced_spec_is_unbound() {
        let last = spec(EntityConcept::Admission, Shape::Scalar);
        assert!(last.subject.table.is_some(), "fixture starts bound");

        let filled = fill_slot_in_spec(&last, &MissingSlot::TimeRange, "last quarter", now())
            .expect("filled");
        assert!(
            filled.subject.table.is_none(),
            "a filled spec must be unbound so compile() cannot skip bind()"
        );
        assert!(filled.time.as_ref().is_some_and(|t| t.column.physical.is_none()));

        let cohort = cohort_from_last(&last);
        assert!(cohort.subject.table.is_none());
        assert_eq!(cohort.shape, Shape::List);
        assert!(cohort.measures.is_empty());
    }

    #[test]
    fn mark_unbound_clears_every_physical_reference() {
        let mut s = spec(EntityConcept::Admission, Shape::Grouped);
        s.joins = vec![];
        s.filters.push(Filter {
            column: ColumnRef::bound("t", "c", EntityConcept::Admission, ColumnRole::Status),
            op: FilterOp::Eq,
            value: FilterValue::Str("x".into()),
        });
        s.dimensions.push(Dimension {
            column: ColumnRef::bound("t", "d", EntityConcept::Admission, ColumnRole::Category),
            label: "d".into(),
        });
        mark_unbound(&mut s);
        assert!(s.subject.table.is_none());
        assert!(s.filters.iter().all(|f| f.column.physical.is_none()));
        assert!(s.dimensions.iter().all(|d| d.column.physical.is_none()));
    }

    #[test]
    fn focus_used_never_contains_a_value() {
        let focus = focus_with_patient();
        let r = resolve("what are his allergies?", &focus, now());
        let joined = r.focus_used().join(",");
        assert!(!joined.contains("PT-00042"));
        assert!(!joined.contains("Jane"));
        // The trace form may contain the key — it is server-side only.
        assert!(r.substitution_log().join(";").contains("PT-00042"));
    }
}
