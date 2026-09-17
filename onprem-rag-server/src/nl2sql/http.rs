//! HTTP route handlers for NL-to-SQL endpoints.
//!
//! These are the Rocket handlers that were extracted from `routes.rs` to keep
//! it below the 2,906-line size gate.  They are registered in `main.rs` directly
//! as `nl2sql::http::*`.

use rocket::response::stream::{Event, EventStream};
use rocket::serde::json::{Json, json};
use rocket::{State, get, post, put};

use crate::auth::{audit, guard::AuthUser};
use crate::error::{AppError, AppResult};
use crate::ontology::concepts::EntityConcept;
use crate::ontology::roles::ColumnRole;
use crate::ontology::service_line::ServiceLine;
use crate::state::AppState;

use super::catalog::refresh_catalog_with_trigger;
use super::prepare::{PreparedNlQuery, SqlEvent, prepare_query};
use super::routes::{MetadataOverrides, NlQueryRequest};

fn token_event(text: &str) -> Event {
    Event::data(serde_json::to_string(text).unwrap_or_else(|_| "\"\"".to_string())).event("token")
}

fn bson_datetime(document: &mongodb::bson::Document, field: &str) -> Option<String> {
    document
        .get_datetime(field)
        .ok()
        .map(|value| chrono::DateTime::<chrono::Utc>::from(value.to_system_time()).to_rfc3339())
}

/// `POST /nl2sql/<source_id>` -- ask a natural-language question over a registered source.
#[post("/nl2sql/<source_id>", data = "<body>")]
pub async fn nl_query(
    state: &State<AppState>,
    _user: AuthUser,
    source_id: &str,
    body: Json<NlQueryRequest>,
) -> AppResult<EventStream![]> {
    if !state.config.router.text2sql_enabled {
        return Err(AppError::Unavailable(
            "NL-to-SQL is disabled; set ONPREM_TEXT2SQL_ENABLED=true".into(),
        ));
    }

    let req = body.into_inner();
    let question = req.question;
    let prepared = prepare_query(state.inner(), source_id, &question).await?;
    let PreparedNlQuery {
        source_id: _,
        sql,
        explanation,
        columns,
        rows,
        answer,
        spec: _,
    } = prepared;

    let sql_event_json = serde_json::to_string(&SqlEvent {
        sql: &sql,
        explanation: &explanation,
    })
    .unwrap_or_default();
    let columns_json = serde_json::to_string(&columns).unwrap_or_else(|_| "[]".to_string());
    let rows_json = serde_json::to_string(&rows).unwrap_or_else(|_| "[]".to_string());

    Ok(EventStream! {
        yield Event::data(
            serde_json::to_string("text_to_sql").unwrap_or_default()
        ).event("routed");

        yield Event::data(sql_event_json).event("sql");
        yield Event::data(columns_json).event("columns");
        yield Event::data(rows_json).event("rows");

        yield token_event(&answer);
        yield Event::data("").event("done");
    })
}

/// `GET /nl2sql/<source_id>/catalog` -- inspect the active metadata generation.
#[get("/nl2sql/<source_id>/catalog")]
pub async fn catalog_status(
    state: &State<AppState>,
    user: AuthUser,
    source_id: &str,
) -> AppResult<Json<serde_json::Value>> {
    user.require_admin()?;
    let metadata = state
        .db
        .schema_catalog_state()
        .find_one(mongodb::bson::doc! { "_id": source_id })
        .await?
        .ok_or(AppError::NotFound)?;
    Ok(Json(json!({
        "source_id": source_id,
        "active_version": metadata.get_str("active_version").unwrap_or_default(),
        "schema_hash": metadata.get_str("schema_hash").unwrap_or_default(),
        "captured_at": bson_datetime(&metadata, "captured_at"),
        "last_check_at": bson_datetime(&metadata, "last_check_at"),
        "last_success_at": bson_datetime(&metadata, "last_success_at"),
        "table_count": metadata.get_i64("table_count").unwrap_or_default(),
        "status": metadata.get_str("status").unwrap_or("unknown"),
        "health": metadata.get_str("health").unwrap_or("unknown"),
        "drift_detected": metadata.get_bool("drift_detected").unwrap_or(false),
        "consecutive_failures": metadata.get_i64("consecutive_failures").unwrap_or_default(),
        "last_error": metadata.get_str("last_error").ok(),
    })))
}

/// `POST /nl2sql/<source_id>/catalog/refresh` -- rebuild schema cards for a source.
#[post("/nl2sql/<source_id>/catalog/refresh")]
pub async fn catalog_refresh(
    state: &State<AppState>,
    user: AuthUser,
    source_id: &str,
) -> AppResult<Json<serde_json::Value>> {
    user.require_admin()?;
    use crate::connectors::routes::load_spec;

    let spec = load_spec(&state.db, &state.config, source_id).await?;
    let count =
        refresh_catalog_with_trigger(&state.db, &state.config, &spec, source_id, "manual", &state.binding_cache()).await?;
    audit::write_audit(
        &state.db,
        &user.id,
        &user.username,
        "schema_catalog_refreshed",
        source_id,
        Some(json!({ "tables_indexed": count })),
    )
    .await;

    Ok(Json(
        json!({ "source_id": source_id, "tables_indexed": count }),
    ))
}

/// `GET /nl2sql/<source_id>/catalog/history` -- recent metadata checks and refreshes.
#[get("/nl2sql/<source_id>/catalog/history?<limit>")]
pub async fn catalog_history(
    state: &State<AppState>,
    user: AuthUser,
    source_id: &str,
    limit: Option<i64>,
) -> AppResult<Json<Vec<serde_json::Value>>> {
    use futures::TryStreamExt;
    user.require_admin()?;
    let limit = limit.unwrap_or(25).clamp(1, 100);
    let documents: Vec<mongodb::bson::Document> = state
        .db
        .schema_catalog_history()
        .find(mongodb::bson::doc! { "source_id": source_id })
        .sort(mongodb::bson::doc! { "completed_at": -1 })
        .limit(limit)
        .await?
        .try_collect()
        .await?;
    Ok(Json(
        documents
            .iter()
            .map(|item| {
                json!({
                    "checked_at": bson_datetime(item, "completed_at"),
                    "trigger": item.get_str("trigger").unwrap_or("unknown"),
                    "outcome": item.get_str("result").unwrap_or("unknown"),
                    "previous_hash": item.get_str("previous_hash").ok(),
                    "observed_hash": item.get_str("observed_hash").ok(),
                    "table_count": item.get_i64("table_count").unwrap_or_default(),
                    "error": item.get_str("error").ok(),
                })
            })
            .collect(),
    ))
}

#[get("/nl2sql/<source_id>/catalog/overrides")]
pub async fn catalog_overrides(
    state: &State<AppState>,
    user: AuthUser,
    source_id: &str,
) -> AppResult<Json<MetadataOverrides>> {
    user.require_admin()?;
    let value = state
        .db
        .schema_metadata_overrides()
        .find_one(mongodb::bson::doc! { "_id": source_id })
        .await?;
    let overrides = value
        .and_then(|mut document| {
            document.remove("_id");
            mongodb::bson::from_document(document).ok()
        })
        .unwrap_or_default();
    Ok(Json(overrides))
}

#[put("/nl2sql/<source_id>/catalog/overrides", data = "<body>")]
pub async fn save_catalog_overrides(
    state: &State<AppState>,
    user: AuthUser,
    source_id: &str,
    body: Json<MetadataOverrides>,
) -> AppResult<Json<MetadataOverrides>> {
    user.require_admin()?;
    let overrides = body.into_inner();
    validate_overrides(&state.db, source_id, &overrides).await?;
    let mut document = mongodb::bson::to_document(&overrides)
        .map_err(|error| AppError::Internal(error.to_string()))?;
    document.insert("_id", source_id);
    document.insert("updated_at", mongodb::bson::DateTime::now());
    document.insert("updated_by", user.id.clone());
    state
        .db
        .schema_metadata_overrides()
        .replace_one(mongodb::bson::doc! { "_id": source_id }, document)
        .upsert(true)
        .await?;
    super::linker::invalidate_source(source_id);
    audit::write_audit(
        &state.db,
        &user.id,
        &user.username,
        "schema_metadata_overrides_updated",
        source_id,
        Some(json!({
            "aliases": overrides.aliases.len(),
            "relationships": overrides.relationships.len(),
            "table_concepts": overrides.table_concepts.len(),
            "column_roles": overrides.column_roles.len(),
            "service_lines": overrides.service_lines.len(),
        })),
    )
    .await;
    Ok(Json(overrides))
}

async fn validate_overrides(
    db: &crate::documentdb::DocumentDb,
    source_id: &str,
    overrides: &MetadataOverrides,
) -> AppResult<()> {
    use futures::TryStreamExt;
    use std::collections::{HashMap, HashSet};

    let state = db
        .schema_catalog_state()
        .find_one(mongodb::bson::doc! { "_id": source_id })
        .await?
        .ok_or(AppError::NotFound)?;
    let version = state
        .get_str("active_version")
        .map_err(|_| AppError::BadRequest("refresh metadata before editing overrides".into()))?;
    let mut cursor = db
        .schema_catalog()
        .find(mongodb::bson::doc! {
            "source_id": source_id, "catalog_version": version
        })
        .await?;
    let mut schema: HashMap<String, HashSet<String>> = HashMap::new();
    while let Some(card) = cursor.try_next().await? {
        let table = card.get_str("table_name").unwrap_or_default().to_string();
        let columns = card
            .get_array("columns")
            .ok()
            .into_iter()
            .flatten()
            .filter_map(|value| value.as_document())
            .filter_map(|value| value.get_str("name").ok())
            .map(str::to_string)
            .collect();
        schema.insert(table, columns);
    }
    let valid_column = |table: &str, column: &str| {
        schema
            .get(table)
            .is_some_and(|columns| columns.contains(column))
    };
    for alias in &overrides.aliases {
        if alias.alias.trim().is_empty()
            || !schema.contains_key(&alias.table)
            || alias
                .column
                .as_deref()
                .is_some_and(|column| !valid_column(&alias.table, column))
        {
            return Err(AppError::BadRequest(
                "alias references an unknown table or column".into(),
            ));
        }
    }
    for edge in &overrides.relationships {
        if !valid_column(&edge.from_table, &edge.from_column)
            || !valid_column(&edge.to_table, &edge.to_column)
        {
            return Err(AppError::BadRequest(
                "relationship references an unknown table or column".into(),
            ));
        }
    }
    for tc in &overrides.table_concepts {
        if !schema.contains_key(&tc.table) {
            return Err(AppError::BadRequest(format!(
                "table_concepts: unknown table '{}'",
                tc.table
            )));
        }
        if let Some(slug) = &tc.concept {
            if EntityConcept::from_slug(slug).is_none() {
                return Err(AppError::BadRequest(format!(
                    "table_concepts: unknown concept slug '{}'",
                    slug
                )));
            }
        }
    }
    for cr in &overrides.column_roles {
        if !valid_column(&cr.table, &cr.column) {
            return Err(AppError::BadRequest(format!(
                "column_roles: unknown table '{}' or column '{}'",
                cr.table, cr.column
            )));
        }
        if ColumnRole::from_slug(&cr.role).is_none() {
            return Err(AppError::BadRequest(format!(
                "column_roles: unknown role slug '{}'",
                cr.role
            )));
        }
    }
    for slug in &overrides.service_lines {
        if ServiceLine::from_slug(slug).is_none() {
            return Err(AppError::BadRequest(format!(
                "service_lines: unknown service-line slug '{}'",
                slug
            )));
        }
    }
    Ok(())
}
