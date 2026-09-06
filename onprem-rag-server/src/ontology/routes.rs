//! Ontology API routes.
//!
//! | Method | Path                                          | Auth  | Description                          |
//! |--------|-----------------------------------------------|-------|--------------------------------------|
//! | GET    | /sources/<id>/binding                         | user  | Latest binding for a source          |
//! | POST   | /sources/<id>/binding/rebuild                 | admin | Trigger a binding rebuild            |
//! | GET    | /sources/<id>/binding/history                 | admin | Binding history (newest first)       |
//!
//! `GET /agents` used to live here; it moved to `agents::registry` in plan 05.

use std::sync::Arc;

use rocket::serde::json::{Json, json};
use rocket::{State, get, post};
use serde::Serialize;

use crate::auth::guard::AuthUser;
use crate::error::{AppError, AppResult};
use crate::nl2sql::catalog::get_catalog_cards;
use crate::ontology::binding::{BindingCoverage, SchemaBinding};
use crate::ontology::binder::build_binding;
use crate::ontology::service_line::ServiceLine;
use crate::ontology::store;
use crate::state::AppState;

// ---------------------------------------------------------------------------
// Response types
// ---------------------------------------------------------------------------

#[derive(Debug, Serialize)]
pub struct BindingResponse {
    pub source_id: String,
    pub bound_at: String,
    pub degraded: bool,
    pub coverage: BindingCoverage,
    pub usable_lines: Vec<String>,
    /// Tables in the schema catalog that fall in **no** service line's scope
    /// (data-map §6, plan 05 §3). The invariant is `orphans: []`; a non-empty
    /// list is an admin-visible gap in the ontology, not an error.
    pub orphans: Vec<String>,
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
    fn from_binding(b: &SchemaBinding, min_confidence: f32) -> Self {
        let coverage = b.coverage(min_confidence);
        let usable = b.usable_lines(min_confidence).iter().map(|l| l.slug().to_string()).collect();
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
        // A table is an orphan when no service line's scope contains it. Scope is
        // the union over all 13 lines, so this is the live form of the "no
        // orphans" test that runs against the fixture.
        let mut orphans: Vec<String> = b
            .tables
            .iter()
            .filter(|t| {
                !ServiceLine::ALL
                    .iter()
                    .any(|line| b.tables_for_line(*line).iter().any(|s| s.table_name == t.table_name))
            })
            .map(|t| t.table_name.clone())
            .collect();
        orphans.sort();

        BindingResponse {
            source_id: b.source_id.clone(),
            bound_at: b.bound_at.to_rfc3339(),
            degraded: b.degraded,
            coverage,
            usable_lines: usable,
            orphans,
            tables,
        }
    }
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
    let min_conf = state.config.binding_min_confidence;
    if let Some(binding) = state.binding_for(source_id) {
        return Ok(Json(BindingResponse::from_binding(&binding, min_conf)));
    }
    // Fall back to DB
    let binding = store::load_binding(&state.db, source_id)
        .await?
        .ok_or(AppError::NotFound)?;
    Ok(Json(BindingResponse::from_binding(&binding, min_conf)))
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

    let coverage = binding.coverage(state.config.binding_min_confidence);
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
    let min_conf = state.config.binding_min_confidence;
    let history = store::load_binding_history(&state.db, source_id).await?;
    Ok(Json(history.iter().map(|b| BindingResponse::from_binding(b, min_conf)).collect()))
}
