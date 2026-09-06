//! `GET /agents` — the agent roster the UI renders.
//!
//! Moved here from `ontology/routes.rs` (plan 05 §8): the roster is an
//! *agent-layer* concern. The ontology module still owns what a service line
//! *is*; this module owns how the product presents one.
//!
//! Two additions over the old listing:
//!
//! - `example_questions` — the data-map §5 questions for the line, filtered to
//!   those whose required concepts are actually bound for some connected source.
//!   An agent never advertises a question the schema cannot answer.
//! - `modes` — `["ask", "trends", "handover"]`, generated from `AgentMode::ALL`
//!   so the wire list cannot drift from the enum.

use std::collections::HashMap;

use rocket::serde::json::Json;
use rocket::{State, get};
use serde::Serialize;

use crate::agents::kind::AgentMode;
use crate::agents::persona;
use crate::auth::guard::AuthUser;
use crate::error::AppResult;
use crate::ontology::binding::SchemaBinding;
use crate::ontology::service_line::ServiceLine;
use crate::state::AppState;

/// One row in the `/agents` listing.
#[derive(Debug, Serialize)]
pub struct ServiceLineInfo {
    pub slug: &'static str,
    pub label: &'static str,
    pub blurb: &'static str,
    pub tier: u8,
    pub concepts: Vec<&'static str>,
    /// Every example with its concept requirements (unfiltered) — kept for
    /// clients that want to show why a question is unavailable.
    pub examples: Vec<ExampleInfo>,
    /// Answerable examples only: required concepts bound for some source.
    pub example_questions: Vec<&'static str>,
    /// Modes this agent supports. Always the full set — a mode changes how an
    /// answer is shaped, not whether the line has data.
    pub modes: Vec<&'static str>,
}

#[derive(Debug, Serialize)]
pub struct ExampleInfo {
    pub question: &'static str,
    pub required_concepts: Vec<&'static str>,
}

#[derive(Debug, Serialize)]
pub struct AgentsResponse {
    pub service_lines: Vec<ServiceLineInfo>,
    /// Map of source_id → list of usable service line slugs for that source.
    pub source_usable_lines: HashMap<String, Vec<String>>,
}

/// Pick the binding used to decide which examples are answerable: the one with
/// the most usable lines. A deployment with several sources advertises what any
/// of them can answer, which is what Ask will actually try.
fn best_binding(
    bindings: &HashMap<String, std::sync::Arc<SchemaBinding>>,
    min_confidence: f32,
) -> Option<std::sync::Arc<SchemaBinding>> {
    bindings
        .values()
        .max_by_key(|b| b.usable_lines(min_confidence).len())
        .cloned()
}

/// List all service lines, their answerable examples, their modes, and — per
/// connected source — which lines are usable.
#[get("/agents")]
pub async fn list_agents(
    state: &State<AppState>,
    _user: AuthUser,
) -> AppResult<Json<AgentsResponse>> {
    let bindings = state.bindings();
    let min_confidence = state.config.binding_min_confidence;
    let binding = best_binding(&bindings, min_confidence);
    let modes: Vec<&'static str> = AgentMode::ALL.iter().map(|m| m.slug()).collect();

    let service_lines: Vec<ServiceLineInfo> = ServiceLine::ALL
        .iter()
        .map(|&line| ServiceLineInfo {
            slug: line.slug(),
            label: line.label(),
            blurb: line.blurb(),
            tier: line.tier(),
            concepts: line.concepts().iter().map(|c| c.slug()).collect(),
            examples: line
                .examples()
                .iter()
                .map(|(q, concepts)| ExampleInfo {
                    question: q,
                    required_concepts: concepts.iter().map(|c| c.slug()).collect(),
                })
                .collect(),
            example_questions: persona::bound_examples(line, binding.as_deref()),
            modes: modes.clone(),
        })
        .collect();

    let mut source_usable_lines: HashMap<String, Vec<String>> = HashMap::new();
    for (source_id, b) in &bindings {
        let usable: Vec<String> = b
            .usable_lines(min_confidence)
            .iter()
            .map(|l| l.slug().to_string())
            .collect();
        source_usable_lines.insert(source_id.clone(), usable);
    }

    Ok(Json(AgentsResponse {
        service_lines,
        source_usable_lines,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The wire mode list is generated from the enum, so it cannot drift.
    #[test]
    fn modes_match_the_enum() {
        let modes: Vec<&str> = AgentMode::ALL.iter().map(|m| m.slug()).collect();
        assert_eq!(modes, vec!["ask", "trends", "handover"]);
    }
}
