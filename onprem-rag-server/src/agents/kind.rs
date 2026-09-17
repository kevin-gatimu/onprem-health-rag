//! Product identity for the agent layer: *which agent the user is talking to*.
//!
//! This is deliberately separate from `foundry::router::ModelRole`, which says
//! only what a single model call is for. Before plan 05 the two were fused into
//! one overloaded enum, so adding a hospital service line implied adding a model
//! role. `AgentKind` is the hospital-facing identity; `ModelRole` is the model
//! plumbing.
//!
//! # Scope is a read allow-list, not a security boundary
//!
//! [`AgentKind::scope`] returns the physical table names an agent may *look at*.
//! It narrows prompts, linker candidates, the aggregation catalog and the
//! retrieval filter so a Maternity question cannot silently join `payments`.
//! It decides nothing about whether a *user* is permitted to do anything —
//! authorisation lives in `src/auth/` and stays there.

use serde::{Deserialize, Serialize};

use crate::ontology::binding::SchemaBinding;
use crate::ontology::concepts::SHARED_CONCEPTS;
use crate::ontology::service_line::ServiceLine;

/// What the user is talking to: the router-driven **Ask** entry point, or one of
/// the 13 hospital service lines.
///
/// Serialises as `"ask"` or the service line's slug (`"maternity"`,
/// `"patient_chart"`, …) — the untagged shape the URL path segment and the
/// persisted `agent_kind` field both use.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum AgentKind {
    /// Router decides which department answers.
    Ask,
    /// A fixed hospital service line.
    Line(ServiceLine),
}

impl AgentKind {
    /// Every reachable agent: Ask plus the 13 lines, in roster order.
    pub fn all() -> Vec<AgentKind> {
        let mut out = vec![AgentKind::Ask];
        out.extend(ServiceLine::ALL.iter().copied().map(AgentKind::Line));
        out
    }

    /// Wire/URL identifier.
    pub fn slug(self) -> &'static str {
        match self {
            AgentKind::Ask => "ask",
            AgentKind::Line(l) => l.slug(),
        }
    }

    /// Human-readable label for the UI tab and persona role line.
    pub fn label(self) -> &'static str {
        match self {
            AgentKind::Ask => "Ask",
            AgentKind::Line(l) => l.label(),
        }
    }

    /// One-sentence description of what this agent covers.
    pub fn blurb(self) -> &'static str {
        match self {
            AgentKind::Ask => {
                "Ask anything about the hospital's records — the router picks the department."
            }
            AgentKind::Line(l) => l.blurb(),
        }
    }

    /// The fixed service line, if any. `None` for Ask (the router is free to
    /// resolve any line).
    pub fn line(self) -> Option<ServiceLine> {
        match self {
            AgentKind::Ask => None,
            AgentKind::Line(l) => Some(l),
        }
    }

    /// Parse a URL path segment or persisted `agent_kind` value.
    ///
    /// Accepts `"ask"`, any service-line slug, and — for one release — the four
    /// legacy mechanism names plus `chat`/`auto`. Resolution is by *name*, never
    /// by position in a list: an unknown value returns `None` so the caller can
    /// answer `400` rather than silently picking a neighbouring agent.
    pub fn parse(s: &str) -> Option<(AgentKind, AgentMode)> {
        let key = s.trim().to_ascii_lowercase();
        if key == "ask" || key == "auto" {
            return Some((AgentKind::Ask, AgentMode::Ask));
        }
        if let Some(line) = ServiceLine::from_slug(&key) {
            return Some((AgentKind::Line(line), AgentMode::Ask));
        }
        legacy_kind(&key)
    }
}

impl Serialize for AgentKind {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(self.slug())
    }
}

impl<'de> Deserialize<'de> for AgentKind {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<AgentKind, D::Error> {
        let raw = String::deserialize(d)?;
        AgentKind::parse(&raw)
            .map(|(kind, _)| kind)
            .ok_or_else(|| serde::de::Error::custom(format!("unknown agent kind '{raw}'")))
    }
}

/// How the agent should answer. Orthogonal to *which* agent answers.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentMode {
    /// Ordinary question answering (default).
    #[default]
    Ask,
    /// Direction-over-time narration; forces a time-bucketed shape.
    Trends,
    /// SBAR shift-handover synthesis over the most recent activity.
    Handover,
}

impl AgentMode {
    pub const ALL: &'static [AgentMode] = &[AgentMode::Ask, AgentMode::Trends, AgentMode::Handover];

    pub fn slug(self) -> &'static str {
        match self {
            AgentMode::Ask => "ask",
            AgentMode::Trends => "trends",
            AgentMode::Handover => "handover",
        }
    }

    pub fn from_slug(s: &str) -> Option<AgentMode> {
        AgentMode::ALL
            .iter()
            .copied()
            .find(|m| m.slug().eq_ignore_ascii_case(s))
    }
}

/// Legacy mechanism-named agents, kept for one release (plan 05 §1).
///
/// The old tabs were named after the *mechanism* that answered them, not the
/// hospital function they served, so none of them maps onto a service line —
/// they all become `Ask` and differ only in mode.
pub const LEGACY_KINDS: &[(&str, AgentKind, AgentMode)] = &[
    ("health_query", AgentKind::Ask, AgentMode::Ask),
    ("trends", AgentKind::Ask, AgentMode::Trends),
    ("patient_lookup", AgentKind::Ask, AgentMode::Ask),
    ("summarize", AgentKind::Ask, AgentMode::Handover),
    ("chat", AgentKind::Ask, AgentMode::Ask),
];

/// Map a legacy agent name onto the new identity + mode, by exact name.
pub fn legacy_kind(name: &str) -> Option<(AgentKind, AgentMode)> {
    LEGACY_KINDS
        .iter()
        .find(|(legacy, _, _)| legacy.eq_ignore_ascii_case(name))
        .map(|(_, kind, mode)| (*kind, *mode))
}

// ---------------------------------------------------------------------------
// Scope — the read allow-list
// ---------------------------------------------------------------------------

impl AgentKind {
    /// Physical table names this agent may read, given a schema binding.
    ///
    /// - `Line(l)` ⇒ tables bound to a concept `l` owns, plus the shared
    ///   concepts (`Patient, Encounter, Provider, Department, DiagnosisCode`)
    ///   which every line may read (data-map §6 rule 2).
    ///   `SchemaBinding::tables_for_line` already unions the shared concepts in.
    /// - **Patient Chart** additionally admits any table within two join hops of
    ///   the patient hub *when the question carries a patient key*, so
    ///   "what is PT-00042 allergic to / owed / booked for" answers from the
    ///   chart without switching tabs.
    /// - `Ask` ⇒ empty (no narrowing). The router decision's own scope is used
    ///   instead; an empty scope means "everything the binding knows".
    ///
    /// The result is a **read allow-list**. It never grants or denies a user
    /// anything — `src/auth/` owns that.
    pub fn scope(self, binding: Option<&SchemaBinding>, patient_key: Option<&str>) -> Vec<String> {
        let (line, binding) = match (self.line(), binding) {
            (Some(l), Some(b)) => (l, b),
            _ => return Vec::new(),
        };

        let mut tables: Vec<String> = binding
            .tables_for_line(line)
            .into_iter()
            .map(|t| t.table_name.clone())
            .collect();

        if line == ServiceLine::PatientChart && patient_key.is_some() {
            for table in &binding.tables {
                let near_patient = table
                    .patient_path
                    .as_ref()
                    .map(|path| path.len() <= 2)
                    .unwrap_or(false);
                if near_patient && !tables.contains(&table.table_name) {
                    tables.push(table.table_name.clone());
                }
            }
        }

        tables.sort();
        tables.dedup();
        tables
    }

    /// Whether the scope is a hard filter. `Ask` reads the whole corpus by
    /// design; a line agent must not silently answer from another department's
    /// tables.
    pub fn scope_is_explicit(self) -> bool {
        !matches!(self, AgentKind::Ask)
    }
}

/// The service line(s) that own a concept, excluding shared concepts (which are
/// owned by nobody in particular and readable by everyone).
///
/// Used by the out-of-scope redirect: "payments belongs to Revenue".
pub fn owners_for_redirect(concept: crate::ontology::concepts::EntityConcept) -> &'static [ServiceLine] {
    if SHARED_CONCEPTS.contains(&concept) {
        return &[];
    }
    ServiceLine::owner_of(concept)
}

// ---------------------------------------------------------------------------
// Out-of-scope redirect
// ---------------------------------------------------------------------------

/// A question aimed at a department this agent does not cover.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScopeRedirect {
    /// The out-of-scope concept the question pointed at.
    pub concept: crate::ontology::concepts::EntityConcept,
    /// The physical table that carried the evidence.
    pub table: String,
    /// Every line that owns the concept. Never truncated to "the first one" —
    /// a concept with two owners is named with both.
    pub owners: Vec<ServiceLine>,
    /// The clarification to show the user.
    pub message: String,
}

/// Normalise for matching: lowercase, and drop separators so `m-pesa` in a
/// question matches the `mpesa` enum label stored in the binding.
fn normalize(text: &str) -> String {
    text.to_ascii_lowercase()
        .chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .collect()
}

/// Detect a question that plainly names data this agent does not own, and build
/// the redirect that names the agent(s) that do (plan 05 §3 / §4 block 3).
///
/// Evidence is taken **only** from the schema binding: a bound table's concept
/// vocabulary, or one of its non-PII enum labels, appearing in the question.
/// That keeps deployment-specific literals out of the agents layer and means a
/// deployment without the table produces no redirect rather than a wrong one.
///
/// Returns `None` for the Ask agent (nothing is out of scope), when no binding
/// is loaded, and when the evidence resolves to a concept nobody owns — naming
/// an owner we cannot identify would be a guess.
pub fn out_of_scope_redirect(
    kind: AgentKind,
    question: &str,
    binding: Option<&SchemaBinding>,
    patient_key: Option<&str>,
) -> Option<ScopeRedirect> {
    use crate::ontology::concepts::{EntityConcept, descriptor};

    let line = kind.line()?;
    let binding = binding?;
    let scope = kind.scope(Some(binding), patient_key);
    let q_words = question.to_ascii_lowercase();
    let q_squashed = normalize(question);

    // A shared concept's own vocabulary ("patient", "provider", "encounter", ...)
    // shows up both as a table-name segment across many *unrelated* concepts
    // (`patient_documents`, `patient_allergies`, `patient_medical_history`, ...)
    // and, just as easily, as a literal enum value on some unrelated table's own
    // column (`consents.granted_by` includes `'patient'`, meaning the patient —
    // rather than a guardian — gave consent). Neither use is vocabulary that
    // distinguishes one concept from another. Shared concepts already can't be
    // redirect evidence in their own right (`owners_for_redirect` returns no
    // owner for them, since a shared concept belongs to every line); this keeps
    // their vocabulary from leaking in as false evidence for some *other*
    // concept via the raw-table-name-segment and enum-value fallbacks below —
    // previously any question merely containing the word "patient" (nearly all
    // of them) could misfire a redirect to whichever line happened to own an
    // unrelated table carrying that word in either form.
    let shared_vocab: std::collections::HashSet<String> = SHARED_CONCEPTS
        .iter()
        .flat_map(|&c| {
            let d = descriptor(c);
            std::iter::once(d.singular)
                .chain(std::iter::once(d.plural))
                .chain(d.synonyms.iter().copied())
        })
        .map(normalize)
        .collect();

    let mut best: Option<ScopeRedirect> = None;

    for table in &binding.tables {
        if table.concept == EntityConcept::Unknown {
            continue;
        }
        if scope.iter().any(|t| t == &table.table_name) {
            continue;
        }
        let owners = owners_for_redirect(table.concept);
        if owners.is_empty() || owners.contains(&line) {
            continue;
        }

        let d = descriptor(table.concept);
        let named = std::iter::once(d.singular)
            .chain(std::iter::once(d.plural))
            .chain(d.synonyms.iter().copied())
            .any(|term| term.len() >= 4 && q_words.contains(term))
            || table.table_name.split('_').any(|part| {
                part.len() >= 4 && !shared_vocab.contains(part) && q_words.contains(part)
            });

        // Enum labels are literal values from the hospital's own data
        // ("mpesa"); PII columns never contribute one.
        let enum_hit = table.columns.iter().any(|col| {
            !col.is_pii
                && col.enum_values.iter().any(|v| {
                    let v_norm = normalize(v);
                    v_norm.len() >= 4
                        && !shared_vocab.contains(&v_norm)
                        && q_squashed.contains(&v_norm)
                })
        });

        if !named && !enum_hit {
            continue;
        }

        // Prefer evidence from a named concept over an enum-label match: naming
        // the table is a stronger signal than mentioning one of its values.
        let stronger = match &best {
            None => true,
            Some(_) => named,
        };
        if stronger {
            let owner_labels = owners
                .iter()
                .map(|l| l.label())
                .collect::<Vec<_>>()
                .join(" or ");
            best = Some(ScopeRedirect {
                concept: table.concept,
                table: table.table_name.clone(),
                owners: owners.to_vec(),
                message: format!(
                    "That's outside the {} agent — {} is covered by {}. \
                     Try the {} agent, or use Ask.",
                    line.label(),
                    descriptor(table.concept).plural,
                    owner_labels,
                    owner_labels
                ),
            });
            if named {
                break;
            }
        }
    }

    best
}

/// A record identifier in `question`, for the sole purpose of deciding whether
/// [`AgentKind::scope`]'s Patient Chart widening should apply before the
/// out-of-scope redirect check runs. `scope` only asks whether this is `Some`
/// — the value itself is never read downstream — so this only has to recognise
/// *that* a specific record is named, not agree with any particular format.
///
/// Deliberately separate from `nl2sql::text::extract_record_identifier`: that
/// extractor's narrower business-code format (`PT-00042`) drives hardcoded SQL
/// templates elsewhere in the compiler, and widening it to also accept raw
/// UUID primary keys — which this deployment's data actually uses — would
/// change which of those templates a UUID-bearing question matches. Recognising
/// the same UUIDs here, for this narrower purpose, carries none of that risk.
pub fn scope_widening_key(question: &str) -> Option<String> {
    if let Some(id) = crate::nl2sql::text::extract_record_identifier(question) {
        return Some(id);
    }
    static UUID: std::sync::LazyLock<regex::Regex> = std::sync::LazyLock::new(|| {
        regex::Regex::new(
            r"\b[0-9a-fA-F]{8}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{12}\b",
        )
        .expect("valid uuid regex")
    });
    UUID.find(question).map(|m| m.as_str().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ask_and_thirteen_lines_are_reachable() {
        let all = AgentKind::all();
        assert_eq!(all.len(), 14, "Ask + 13 service lines");
        for kind in &all {
            let (parsed, _) = AgentKind::parse(kind.slug())
                .unwrap_or_else(|| panic!("slug '{}' must round-trip", kind.slug()));
            assert_eq!(parsed, *kind);
        }
    }

    /// Plan 05 §1: the legacy compatibility table, exactly as specified.
    #[test]
    fn legacy_names_map_to_ask_with_the_documented_mode() {
        let expected = [
            ("health_query", AgentKind::Ask, AgentMode::Ask),
            ("trends", AgentKind::Ask, AgentMode::Trends),
            ("patient_lookup", AgentKind::Ask, AgentMode::Ask),
            ("summarize", AgentKind::Ask, AgentMode::Handover),
            ("chat", AgentKind::Ask, AgentMode::Ask),
        ];
        for (name, kind, mode) in expected {
            assert_eq!(
                AgentKind::parse(name),
                Some((kind, mode)),
                "legacy '{name}' must map to {kind:?}/{mode:?}"
            );
        }
        assert_eq!(LEGACY_KINDS.len(), expected.len());
    }

    #[test]
    fn unknown_kinds_are_refused_not_guessed() {
        assert_eq!(AgentKind::parse("maternityy"), None);
        assert_eq!(AgentKind::parse(""), None);
        assert_eq!(AgentKind::parse("obstetrics"), None);
    }

    #[test]
    fn ask_scope_is_unbounded_and_line_scope_is_explicit() {
        assert!(AgentKind::Ask.scope(None, None).is_empty());
        assert!(!AgentKind::Ask.scope_is_explicit());
        assert!(AgentKind::Line(ServiceLine::Maternity).scope_is_explicit());
    }

    // -----------------------------------------------------------------
    // Fixture-backed scope tests (no DB, no model)
    // -----------------------------------------------------------------

    fn dev_binding() -> SchemaBinding {
        use crate::ontology::binder::bind_cards;
        use crate::ontology::tests::{dev_seed_cards, dev_seed_enum_values};
        let cards = dev_seed_cards();
        let tables = bind_cards(&cards, 0.55, 3, None, None, &dev_seed_enum_values());
        SchemaBinding {
            source_id: "dev".into(),
            bound_at: chrono::Utc::now(),
            tables,
            degraded: false,
            override_version: 0,
        }
    }

    /// Data-map §6 / plan 05 §3: every table in the catalog is in the scope of
    /// at least one service line. An orphan is a table no agent can ever read.
    #[test]
    fn no_table_falls_outside_every_service_line_scope() {
        let binding = dev_binding();
        let mut in_scope: Vec<&str> = Vec::new();
        for line in ServiceLine::ALL {
            for name in AgentKind::Line(*line).scope(Some(&binding), None) {
                if let Some(t) = binding.tables.iter().find(|t| t.table_name == name) {
                    in_scope.push(t.table_name.as_str());
                }
            }
        }
        let orphans: Vec<&str> = binding
            .tables
            .iter()
            .map(|t| t.table_name.as_str())
            .filter(|name| !in_scope.contains(name))
            .collect();
        assert!(
            orphans.is_empty(),
            "tables in no service line's scope: {orphans:?}"
        );
    }

    /// Plan 05 §9: a Maternity agent asked about M-Pesa collections is
    /// redirected to Revenue. The evidence is the `payment_method` enum label
    /// carried by the binding — no table name is written into this test's
    /// expectation, and no `payments` query is ever planned.
    #[test]
    fn maternity_redirects_an_mpesa_question_to_revenue() {
        let binding = dev_binding();
        let redirect = out_of_scope_redirect(
            AgentKind::Line(ServiceLine::Maternity),
            "how much did we collect by M-Pesa",
            Some(&binding),
            None,
        )
        .expect("an M-Pesa question must be recognised as outside Maternity");
        assert_eq!(redirect.owners, vec![ServiceLine::Revenue]);
        assert!(
            redirect.message.contains("Revenue"),
            "redirect must name the owning department: {}",
            redirect.message
        );
        // And the table that carried the evidence is genuinely out of scope.
        let scope = AgentKind::Line(ServiceLine::Maternity).scope(Some(&binding), None);
        assert!(
            !scope.contains(&redirect.table),
            "{} must not be in Maternity's scope",
            redirect.table
        );
    }

    /// The Revenue agent itself is not redirected away from its own data.
    #[test]
    fn revenue_is_not_redirected_from_its_own_question() {
        let binding = dev_binding();
        assert!(
            out_of_scope_redirect(
                AgentKind::Line(ServiceLine::Revenue),
                "how much did we collect by M-Pesa",
                Some(&binding),
                None,
            )
            .is_none()
        );
        // Ask is never out of scope.
        assert!(
            out_of_scope_redirect(
                AgentKind::Ask,
                "how much did we collect by M-Pesa",
                Some(&binding),
                None,
            )
            .is_none()
        );
    }

    /// The settled ownership decision (plan 05 brief): Theatre keeps surgeries
    /// and loses admissions; Facilities owns the physical estate.
    #[test]
    fn theatre_drops_admission_and_facilities_owns_the_estate() {
        use crate::ontology::concepts::EntityConcept;
        assert!(!ServiceLine::Theatre.concepts().contains(&EntityConcept::Admission));
        assert!(ServiceLine::Theatre.concepts().contains(&EntityConcept::Surgery));
        assert!(!ServiceLine::owner_of(EntityConcept::Admission).contains(&ServiceLine::Theatre));
        for estate in [EntityConcept::Ward, EntityConcept::Bed] {
            assert!(
                ServiceLine::Facilities.concepts().contains(&estate),
                "Facilities must own {estate:?}"
            );
            assert!(ServiceLine::owner_of(estate).contains(&ServiceLine::Facilities));
        }
    }

    /// Patient Chart reaches one hop further when a patient key is present, and
    /// only then.
    #[test]
    fn patient_chart_widens_only_with_a_patient_key() {
        let binding = dev_binding();
        let chart = AgentKind::Line(ServiceLine::PatientChart);
        let narrow = chart.scope(Some(&binding), None);
        let wide = chart.scope(Some(&binding), Some("PT-00042"));
        assert!(
            wide.len() > narrow.len(),
            "a patient key should widen the chart's scope ({} -> {})",
            narrow.len(),
            wide.len()
        );
        for t in &narrow {
            assert!(wide.contains(t), "widening must not drop {t}");
        }
    }

    /// The regression from production logs: a question that merely contains the
    /// word "patient" (true of nearly every clinical question) must not be
    /// redirected anywhere. "patient" is `Patient`'s own shared-concept
    /// vocabulary and, before this fix, also matched as a raw table-name
    /// segment on every `patient_*`-prefixed table (`patient_documents`,
    /// `patient_allergies`, ...) regardless of which concept actually owned it.
    #[test]
    fn a_bare_mention_of_patient_is_not_evidence_for_an_unrelated_table() {
        let binding = dev_binding();
        for line in ServiceLine::ALL {
            let agent = AgentKind::Line(*line);
            assert_eq!(
                out_of_scope_redirect(agent, "how is the patient", Some(&binding), None),
                None,
                "{:?}: \"patient\" alone must not trigger a redirect",
                line
            );
        }
    }

    /// Bug fixed alongside the above: the redirect check used to hardcode
    /// `patient_key: None`, so Patient Chart's own widening (a couple of tests
    /// up) never got a chance to keep a patient-scoped question from being
    /// redirected away in the first place. Finds whatever table widening adds
    /// (rather than hardcoding one) and confirms passing the same patient key
    /// into `out_of_scope_redirect` suppresses a redirect that fires without it.
    #[test]
    fn a_patient_key_passed_into_the_redirect_check_uses_the_widened_scope() {
        let binding = dev_binding();
        let chart = AgentKind::Line(ServiceLine::PatientChart);
        let narrow = chart.scope(Some(&binding), None);
        let key = "PT-00042";
        let wide = chart.scope(Some(&binding), Some(key));
        let widened_table = binding
            .tables
            .iter()
            .find(|t| wide.contains(&t.table_name) && !narrow.contains(&t.table_name))
            .expect("widening must add at least one table (asserted elsewhere)");
        let vocab = crate::ontology::concepts::descriptor(widened_table.concept);
        let term = [vocab.plural, vocab.singular]
            .into_iter()
            .chain(vocab.synonyms.iter().copied())
            .find(|t| t.len() >= 4)
            .expect("descriptor must have at least one term >= 4 chars long");
        let question = format!("what {term} does this {key} have");

        assert!(
            out_of_scope_redirect(chart, &question, Some(&binding), None).is_some(),
            "without a patient key, {} is still genuinely out of Patient Chart's narrow scope",
            widened_table.table_name
        );
        assert_eq!(
            out_of_scope_redirect(chart, &question, Some(&binding), Some(key)),
            None,
            "with the patient key threaded through, {} is in the widened scope and must not redirect",
            widened_table.table_name
        );
    }

    #[test]
    fn scope_widening_key_recognizes_business_ids_and_uuids() {
        assert_eq!(
            scope_widening_key("what is PT-2024-0042 allergic to").as_deref(),
            Some("PT-2024-0042")
        );
        assert_eq!(
            scope_widening_key(
                "what medication has patient with id 8c580c2d-1ed1-4fc3-a0dc-fbca698da15f been prescribed?"
            )
            .as_deref(),
            Some("8c580c2d-1ed1-4fc3-a0dc-fbca698da15f")
        );
        assert_eq!(scope_widening_key("how many patients do we have"), None);
    }

    #[test]
    fn shared_concepts_have_no_redirect_owner() {
        use crate::ontology::concepts::EntityConcept;
        assert!(owners_for_redirect(EntityConcept::Patient).is_empty());
        assert_eq!(
            owners_for_redirect(EntityConcept::Payment),
            &[ServiceLine::Revenue]
        );
    }
}
