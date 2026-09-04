//! Ontology API routes.
//!
//! | Method | Path                                          | Auth  | Description                          |
//! |--------|-----------------------------------------------|-------|--------------------------------------|
//! | GET    | /agents                                       | user  | List service lines + usable sources  |
//! | GET    | /sources/<id>/binding                         | user  | Latest binding for a source          |
//! | POST   | /sources/<id>/binding/rebuild                 | admin | Trigger a binding rebuild            |
//! | GET    | /sources/<id>/binding/history                 | admin | Binding history (newest first)       |

use std::collections::HashMap;
use std::sync::Arc;

use rocket::serde::json::{Json, json};
use rocket::{State, get, post};
use serde::{Deserialize, Serialize};

use crate::auth::guard::AuthUser;
use crate::error::{AppError, AppResult};
use crate::nl2sql::catalog::get_catalog_cards;
use crate::ontology::binding::{BindingCoverage, SchemaBinding};
use crate::ontology::binder::{build_binding, BindingOverrides};
use crate::ontology::concepts::EntityConcept;
use crate::ontology::service_line::ServiceLine;
use crate::ontology::store;
use crate::state::AppState;

// ---------------------------------------------------------------------------
// Response types
// ---------------------------------------------------------------------------

/// One row in the `/agents` listing.
#[derive(Debug, Serialize)]
pub struct ServiceLineInfo {
    pub slug: &'static str,
    pub label: &'static str,
    pub blurb: &'static str,
    pub tier: u8,
    pub concepts: Vec<&'static str>,
    pub examples: Vec<ExampleInfo>,
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

#[derive(Debug, Serialize)]
pub struct BindingResponse {
    pub source_id: String,
    pub bound_at: String,
    pub degraded: bool,
    pub coverage: BindingCoverage,
    pub usable_lines: Vec<String>,
    pub tables: Vec<TableSummary>,
}

#[derive(Debug, Serialize)]
pub struct TableSummary {
    pub table_name: String,
    pub concept: String,
    pub confidence: f32,
    pub service_lines: Vec<String>,
    pub event_time_col: Option<String>,
    pub patient_path_hops: Option<usize>,
}

impl BindingResponse {
    fn from_binding(b: &SchemaBinding) -> Self {
        let coverage = b.coverage();
        let usable = b.usable_lines().iter().map(|l| l.slug().to_string()).collect();
        let tables = b
            .tables
            .iter()
            .map(|t| TableSummary {
                table_name: t.table_name.clone(),
                concept: t.concept.slug().to_string(),
                confidence: t.confidence,
                service_lines: t.service_lines.iter().map(|l| l.slug().to_string()).collect(),
                event_time_col: t.event_time_col.clone(),
                patient_path_hops: t.patient_path.as_ref().map(|p| p.len()),
            })
            .collect();
        BindingResponse {
            source_id: b.source_id.clone(),
            bound_at: b.bound_at.to_rfc3339(),
            degraded: b.degraded,
            coverage,
            usable_lines: usable,
            tables,
        }
    }
}

// ---------------------------------------------------------------------------
// GET /agents
// ---------------------------------------------------------------------------

/// List all service lines and, for each connected source, which lines are usable.
#[get("/agents")]
pub async fn list_agents(
    state: &State<AppState>,
    _user: AuthUser,
) -> AppResult<Json<AgentsResponse>> {
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
        })
        .collect();

    // Collect usable lines from all cached bindings
    let bindings = state.bindings();
    let mut source_usable_lines: HashMap<String, Vec<String>> = HashMap::new();
    for (source_id, binding) in &bindings {
        let usable: Vec<String> = binding
            .usable_lines()
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

// ---------------------------------------------------------------------------
// GET /sources/<id>/binding
// ---------------------------------------------------------------------------

#[get("/sources/<source_id>/binding")]
pub async fn get_binding(
    state: &State<AppState>,
    _user: AuthUser,
    source_id: &str,
) -> AppResult<Json<BindingResponse>> {
    // Check in-memory cache first
    if let Some(binding) = state.binding_for(source_id) {
        return Ok(Json(BindingResponse::from_binding(&binding)));
    }
    // Fall back to DB
    let binding = store::load_binding(&state.db, source_id)
        .await?
        .ok_or(AppError::NotFound)?;
    Ok(Json(BindingResponse::from_binding(&binding)))
}

// ---------------------------------------------------------------------------
// POST /sources/<id>/binding/rebuild
// ---------------------------------------------------------------------------

#[post("/sources/<source_id>/binding/rebuild")]
pub async fn rebuild_binding(
    state: &State<AppState>,
    user: AuthUser,
    source_id: &str,
) -> AppResult<Json<serde_json::Value>> {
    user.require_admin()?;
    if !state.config.binding_enabled {
        return Err(AppError::Unavailable("schema binding is disabled".into()));
    }

    let cards = get_catalog_cards(&state.db, &state.config, source_id).await?;
    if cards.is_empty() {
        return Err(AppError::BadRequest(format!(
            "no schema catalog for source {source_id}; run a catalog refresh first"
        )));
    }

    // Get descriptor vectors (compute if needed, non-fatal)
    let dv_arc = state.descriptor_vectors();
    let dv_ref = dv_arc.as_deref();

    let overrides = state.binding_overrides_for(source_id);
    let binding = build_binding(
        &cards,
        &state.db,
        &state.config,
        source_id,
        dv_ref,
        overrides.as_ref(),
    )
    .await?;

    let coverage = binding.coverage();
    store::save_binding_nonfatal(&state.db, &binding).await;
    state.set_binding(source_id.to_string(), Arc::new(binding));

    Ok(Json(json!({
        "status": "ok",
        "source_id": source_id,
        "tables": coverage.total_tables,
        "bound": coverage.bound_tables,
        "orphans": coverage.orphan_tables,
        "usable_lines": coverage.usable_lines.iter().map(|l| l.slug()).collect::<Vec<_>>(),
    })))
}

// ---------------------------------------------------------------------------
// GET /sources/<id>/binding/history
// ---------------------------------------------------------------------------

#[get("/sources/<source_id>/binding/history")]
pub async fn get_binding_history(
    state: &State<AppState>,
    user: AuthUser,
    source_id: &str,
) -> AppResult<Json<Vec<BindingResponse>>> {
    user.require_admin()?;
    let history = store::load_binding_history(&state.db, source_id).await?;
    Ok(Json(history.iter().map(BindingResponse::from_binding).collect()))
}
