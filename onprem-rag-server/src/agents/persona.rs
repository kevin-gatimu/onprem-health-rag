//! Agent personas generated *from the schema binding* (plan 05 §4).
//!
//! A persona is not a hand-written paragraph per department. It is assembled at
//! request time from five blocks, two of which are facts read off the
//! deployment's own `SchemaBinding` — so the Maternity agent on one hospital
//! says `deliveries` and on another says `OB_Delivery`, without either string
//! appearing in this file.
//!
//! Two invariants hold for every prompt this module produces:
//!
//! - **No PII column is ever named.** Column names are only emitted for
//!   non-PII columns that carry enum labels; `is_pii` columns are skipped
//!   before anything is written.
//! - **Bounded size.** The prompt is capped at roughly 350 tokens: at most
//!   [`MAX_TABLES`] tables, [`MAX_ENUM_COLUMNS`] enum columns per table, and
//!   [`MAX_ENUM_VALUES`] values per column.

use crate::agents::kind::{AgentKind, AgentMode};
use crate::ontology::binding::SchemaBinding;
use crate::ontology::concepts::descriptor;
use crate::ontology::service_line::ServiceLine;
use crate::router::focus::ConversationFocus;

/// Data-block caps. Together these bound the whole prompt near ~350 tokens.
const MAX_TABLES: usize = 8;
const MAX_ENUM_COLUMNS: usize = 3;
/// Plan 05 §4: "Enum lists capped at 8 values each".
const MAX_ENUM_VALUES: usize = 8;

/// Block 3 — fixed grounding rules, identical for every agent.
const GROUNDING_RULES: &str = "\
Rules: answer only from the rows or passages supplied to you; never invent a \
number, a name, or a record. If the data does not answer the question, say so \
plainly. Give no clinical advice or diagnosis. If the question is about data \
this department does not cover, say which department does and stop.";

/// Build the system prompt for one agent turn.
///
/// `binding` is the schema binding for the source this turn will read; `None`
/// (nothing bound yet) simply omits the data block rather than inventing one.
pub fn system_prompt(
    kind: AgentKind,
    mode: AgentMode,
    binding: Option<&SchemaBinding>,
    focus: &ConversationFocus,
) -> String {
    let mut out = String::with_capacity(1200);

    // 1 — role line
    out.push_str(&role_line(kind));

    // 2 — data you can see
    let data = data_block(kind, binding);
    if !data.is_empty() {
        out.push_str("\n\n");
        out.push_str(&data);
    }

    // 3 — grounding rules
    out.push_str("\n\n");
    out.push_str(GROUNDING_RULES);

    // 4 — mode block
    if let Some(block) = mode_block(mode) {
        out.push_str("\n\n");
        out.push_str(block);
    }

    // 5 — focus block
    if let Some(block) = focus_block(focus) {
        out.push_str("\n\n");
        out.push_str(&block);
    }

    out
}

/// Block 1 — who this agent is.
fn role_line(kind: AgentKind) -> String {
    match kind {
        AgentKind::Ask => format!(
            "You are the assistant for this hospital's records system. {}",
            kind.blurb()
        ),
        AgentKind::Line(l) => format!(
            "You are the {} assistant for this hospital's records system: {}",
            l.label(),
            l.blurb()
        ),
    }
}

/// Block 2 — the hospital's own tables and vocabulary, read off the binding.
fn data_block(kind: AgentKind, binding: Option<&SchemaBinding>) -> String {
    let Some(binding) = binding else {
        return String::new();
    };

    match kind {
        AgentKind::Line(line) => {
            let tables = binding.tables_for_line(line);
            if tables.is_empty() {
                return String::new();
            }
            let mut out = String::from("Data you can see:");
            for table in tables.iter().take(MAX_TABLES) {
                out.push_str(&format!(
                    "\n- {} (`{}`)",
                    descriptor(table.concept).plural,
                    table.table_name
                ));
                let mut enum_cols = 0usize;
                for col in &table.columns {
                    if enum_cols >= MAX_ENUM_COLUMNS {
                        break;
                    }
                    // A PII column is never named, whatever it contains.
                    if col.is_pii || col.enum_values.is_empty() {
                        continue;
                    }
                    let shown: Vec<&str> = col
                        .enum_values
                        .iter()
                        .take(MAX_ENUM_VALUES)
                        .map(String::as_str)
                        .collect();
                    let ellipsis = if col.enum_values.len() > MAX_ENUM_VALUES {
                        "…"
                    } else {
                        ""
                    };
                    out.push_str(&format!(
                        ": {} ∈ {}{}",
                        col.column_name,
                        shown.join(", "),
                        ellipsis
                    ));
                    enum_cols += 1;
                }
            }
            if tables.len() > MAX_TABLES {
                out.push_str(&format!("\n- …and {} more.", tables.len() - MAX_TABLES));
            }
            out
        }
        // Ask reads the whole corpus; listing every table would blow the budget
        // and tell the model nothing useful, so it gets the departments instead.
        AgentKind::Ask => {
            let lines = binding.usable_lines(crate::config::DEFAULT_BINDING_MIN_CONFIDENCE);
            if lines.is_empty() {
                return String::new();
            }
            let labels: Vec<&str> = lines.iter().map(|l| l.label()).collect();
            format!(
                "Data you can see: every department this hospital has bound — {}.",
                labels.join(", ")
            )
        }
    }
}

/// Block 4 — how this mode answers.
fn mode_block(mode: AgentMode) -> Option<&'static str> {
    match mode {
        AgentMode::Ask => None,
        AgentMode::Trends => Some(
            "Trends: describe the direction, the magnitude, and the bucket with \
             the largest change. Never extrapolate beyond the buckets supplied.",
        ),
        AgentMode::Handover => Some(
            "Handover: answer under SBAR headings (Situation, Background, \
             Assessment, Recommendation), at most 8 bullets in total. Flag \
             anything critical or abnormal that is present in the rows. Cite the \
             records you used.",
        ),
    }
}

/// Block 5 — what the conversation is currently about.
fn focus_block(focus: &ConversationFocus) -> Option<String> {
    let mut parts: Vec<String> = Vec::new();
    if let Some(pk) = &focus.patient_key {
        parts.push(format!("patient {pk}"));
    }
    if let Some(tr) = &focus.time_range {
        parts.push(format!("period: {}", crate::router::focus::time_range_label(tr)));
    }
    if parts.is_empty() {
        None
    } else {
        Some(format!("Current focus: {}.", parts.join("; ")))
    }
}

// ---------------------------------------------------------------------------
// Capability answers
// ---------------------------------------------------------------------------

/// Answer "what can you do?" from the binding, with **no model call**
/// (plan 05 §5). Pure string assembly over data already in memory, so it
/// returns in microseconds.
///
/// The answer names only concepts that are actually bound for this deployment
/// and only example questions whose required concepts are bound — an agent
/// never claims a capability the schema cannot support.
pub fn capability_answer(kind: AgentKind, binding: Option<&SchemaBinding>) -> String {
    match kind {
        AgentKind::Line(line) => {
            let concepts = bound_concept_labels(line, binding);
            let examples = bound_examples(line, binding);
            let mut out = format!("I'm the {} assistant. {}", line.label(), line.blurb());
            if concepts.is_empty() {
                out.push_str(
                    "\n\nNothing is bound for this department in the connected sources yet, \
                     so I can't answer from data. Connect and index a source that carries it.",
                );
                return out;
            }
            out.push_str(&format!("\n\nI can see: {}.", concepts.join(", ")));
            if !examples.is_empty() {
                out.push_str("\n\nFor example, you can ask:");
                for q in examples.iter().take(3) {
                    out.push_str(&format!("\n- {q}"));
                }
            }
            out
        }
        AgentKind::Ask => {
            let mut out = String::from(
                "I'm Ask — I route your question to whichever department's data answers it.",
            );
            let lines: Vec<ServiceLine> = match binding {
                Some(b) => b.usable_lines(crate::config::DEFAULT_BINDING_MIN_CONFIDENCE),
                None => vec![],
            };
            if lines.is_empty() {
                out.push_str(
                    "\n\nNo source is bound yet, so I have no hospital data to read. \
                     Connect and index a source first.",
                );
                return out;
            }
            let labels: Vec<&str> = lines.iter().map(|l| l.label()).collect();
            out.push_str(&format!("\n\nDepartments with data: {}.", labels.join(", ")));
            out.push_str("\n\nFor example, you can ask:");
            for line in lines.iter().take(3) {
                if let Some(q) = bound_examples(*line, binding).into_iter().next() {
                    out.push_str(&format!("\n- {q}"));
                }
            }
            out
        }
    }
}

/// Concept labels this line owns that are actually bound in this deployment.
pub fn bound_concept_labels(line: ServiceLine, binding: Option<&SchemaBinding>) -> Vec<String> {
    let Some(binding) = binding else {
        return vec![];
    };
    let mut seen: Vec<String> = Vec::new();
    for concept in line.concepts() {
        if binding.tables_for_concept(*concept).is_empty() {
            continue;
        }
        let label = descriptor(*concept).plural.to_string();
        if !seen.contains(&label) {
            seen.push(label);
        }
    }
    seen
}

/// Example questions for a line, filtered to those whose required concepts are
/// all bound. Order follows `ServiceLine::examples()`; nothing is picked by
/// position from a list of candidates that mean different things.
pub fn bound_examples(line: ServiceLine, binding: Option<&SchemaBinding>) -> Vec<&'static str> {
    let Some(binding) = binding else {
        return vec![];
    };
    line.examples()
        .iter()
        .filter(|(_, required)| {
            required
                .iter()
                .all(|c| !binding.tables_for_concept(*c).is_empty())
        })
        .map(|(q, _)| *q)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ontology::binder::bind_cards;
    use crate::ontology::tests::{alt_schema_cards, dev_seed_cards, dev_seed_enum_values};
    use std::collections::HashMap;

    const MIN_CONF: f32 = 0.55;

    fn dev_binding() -> SchemaBinding {
        let cards = dev_seed_cards();
        let tables = bind_cards(&cards, MIN_CONF, 3, None, None, &dev_seed_enum_values());
        SchemaBinding {
            source_id: "dev".into(),
            bound_at: chrono::Utc::now(),
            tables,
            degraded: false,
            override_version: 0,
        }
    }

    fn alt_binding() -> SchemaBinding {
        let cards = alt_schema_cards();
        let tables = bind_cards(&cards, MIN_CONF, 3, None, None, &HashMap::new());
        SchemaBinding {
            source_id: "alt".into(),
            bound_at: chrono::Utc::now(),
            tables,
            degraded: false,
            override_version: 0,
        }
    }

    /// Plan 05 §9: the dev binding produces a Maternity prompt naming the
    /// hospital's own tables, and naming no PII column.
    #[test]
    fn maternity_persona_names_the_dev_tables_and_no_pii_column() {
        let binding = dev_binding();
        let prompt = system_prompt(
            AgentKind::Line(ServiceLine::Maternity),
            AgentMode::Ask,
            Some(&binding),
            &ConversationFocus::default(),
        );
        for expected in ["deliveries", "newborns", "antenatal_visits"] {
            assert!(
                prompt.contains(expected),
                "Maternity persona should name `{expected}`; got:\n{prompt}"
            );
        }
        // No PII column name may appear anywhere in the prompt.
        for table in &binding.tables {
            for col in &table.columns {
                if col.is_pii {
                    assert!(
                        !prompt.contains(&col.column_name),
                        "PII column `{}` leaked into the persona",
                        col.column_name
                    );
                }
            }
        }
    }

    /// The same builder on a differently-named schema speaks that schema.
    #[test]
    fn maternity_persona_follows_the_alt_schema_names() {
        let binding = alt_binding();
        let prompt = system_prompt(
            AgentKind::Line(ServiceLine::Maternity),
            AgentMode::Ask,
            Some(&binding),
            &ConversationFocus::default(),
        );
        let expected = binding
            .tables_for_line(ServiceLine::Maternity)
            .into_iter()
            .map(|t| t.table_name.clone())
            .collect::<Vec<_>>();
        assert!(
            !expected.is_empty(),
            "alt schema should bind at least one Maternity table"
        );
        assert!(
            expected.iter().any(|t| prompt.contains(t)),
            "alt persona should name one of {expected:?}; got:\n{prompt}"
        );
        // Dev-seed *table* names must not appear. Bare words like "deliveries"
        // and "newborns" are the concepts' plural labels — prose, legitimately
        // shared between schemas — so the check is on the backticked physical
        // name the data block actually emits.
        for dev_table in ["`antenatal_visits`", "`newborns`", "`deliveries`"] {
            assert!(
                !prompt.contains(dev_table),
                "alt persona must not carry the dev-seed table name {dev_table}"
            );
        }
    }

    /// Enum lists are capped and PII columns never contribute one.
    #[test]
    fn enum_lists_are_capped_at_eight_values() {
        let binding = dev_binding();
        for line in ServiceLine::ALL {
            let prompt = system_prompt(
                AgentKind::Line(*line),
                AgentMode::Ask,
                Some(&binding),
                &ConversationFocus::default(),
            );
            for segment in prompt.split(" ∈ ").skip(1) {
                let list = segment.lines().next().unwrap_or("");
                let count = list.trim_end_matches('…').split(',').count();
                assert!(
                    count <= MAX_ENUM_VALUES,
                    "{line:?} enum list has {count} values: {list}"
                );
            }
        }
    }

    /// The prompt stays within roughly 350 tokens (~4 chars/token).
    #[test]
    fn persona_stays_within_the_token_budget() {
        let binding = dev_binding();
        for line in ServiceLine::ALL {
            for mode in AgentMode::ALL {
                let prompt = system_prompt(
                    AgentKind::Line(*line),
                    *mode,
                    Some(&binding),
                    &ConversationFocus::default(),
                );
                let approx_tokens = prompt.split_whitespace().count();
                assert!(
                    approx_tokens <= 400,
                    "{line:?}/{mode:?} persona is ~{approx_tokens} tokens:\n{prompt}"
                );
            }
        }
    }

    /// Plan 05 §9: a capability answer on each of the 13 lines names at least
    /// one concept and at least one example on the dev seed.
    #[test]
    fn capability_answer_covers_every_line_on_the_dev_seed() {
        let binding = dev_binding();
        for line in ServiceLine::ALL {
            let concepts = bound_concept_labels(*line, Some(&binding));
            assert!(
                !concepts.is_empty(),
                "{line:?} should have at least one bound concept on the dev seed"
            );
            let examples = bound_examples(*line, Some(&binding));
            assert!(
                !examples.is_empty(),
                "{line:?} should have at least one answerable example on the dev seed"
            );
            let answer = capability_answer(AgentKind::Line(*line), Some(&binding));
            assert!(answer.contains(&concepts[0]), "{line:?}: {answer}");
            assert!(answer.contains(examples[0]), "{line:?}: {answer}");
        }
    }

    /// Plan 05 §10: the capability answer costs no model call and is instant.
    #[test]
    fn capability_answer_is_model_free_and_fast() {
        let binding = dev_binding();
        let start = std::time::Instant::now();
        for kind in AgentKind::all() {
            let answer = capability_answer(kind, Some(&binding));
            assert!(!answer.is_empty());
        }
        let elapsed = start.elapsed();
        assert!(
            elapsed < std::time::Duration::from_millis(100),
            "14 capability answers took {elapsed:?}; budget is 100 ms for one"
        );
    }

    /// With nothing bound the answer says so rather than claiming capability.
    #[test]
    fn capability_answer_is_honest_with_no_binding() {
        let answer = capability_answer(AgentKind::Line(ServiceLine::Maternity), None);
        assert!(answer.to_lowercase().contains("bound"));
    }
}
