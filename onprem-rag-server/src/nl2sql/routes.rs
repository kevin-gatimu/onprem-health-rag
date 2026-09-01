//! NL-to-SQL routes.
//!
//! | Method | Path                                   | Auth    | Description                    |
//! |--------|----------------------------------------|---------|-------------------------------|
//! | POST   | /nl2sql/<source_id>                    | user    | Ask a question; SSE response  |
//! | GET    | /nl2sql/<source_id>/catalog            | admin   | Inspect active metadata       |
//! | POST   | /nl2sql/<source_id>/catalog/refresh    | admin   | Rebuild schema_catalog        |
//!
//! SSE event contract:
//! | Event   | Payload                                   |
//! |---------|-------------------------------------------|
//! | routed  | "text_to_sql" (JSON string)               |
//! | sql     | {sql, explanation} (JSON)                 |
//! | columns | ["col1", ...] (JSON array)                |
//! | rows    | [[val, ...], ...] (JSON array of arrays)  |
//! | token   | JSON-encoded string (narration token)     |
//! | error   | bare error string                         |
//! | done    | empty string                              |

use rocket::response::stream::{Event, EventStream};
use rocket::serde::json::{Json, json};
use rocket::{State, get, post, put};
use serde::{Deserialize, Serialize};

use crate::auth::{audit, guard::AuthUser};
use crate::error::{AppError, AppResult};
use crate::foundry::router::AgentKind;
use crate::state::AppState;

use super::catalog::{refresh_catalog, refresh_catalog_with_trigger};
use super::execute::run_select;
use super::generate;
use super::linker::link;
use super::validate::validate_sql;

// ---------------------------------------------------------------------------
// Request bodies
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
pub struct NlQueryRequest {
    pub question: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MetadataAlias {
    pub table: String,
    pub column: Option<String>,
    pub alias: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MetadataRelationship {
    pub from_table: String,
    pub from_column: String,
    pub to_table: String,
    pub to_column: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct MetadataOverrides {
    #[serde(default)]
    pub aliases: Vec<MetadataAlias>,
    #[serde(default)]
    pub relationships: Vec<MetadataRelationship>,
}

#[derive(Debug, Serialize)]
struct SqlEvent<'a> {
    sql: &'a str,
    explanation: &'a str,
}

pub(crate) struct PreparedNlQuery {
    pub source_id: String,
    pub sql: String,
    pub explanation: String,
    pub columns: Vec<String>,
    pub rows: Vec<Vec<serde_json::Value>>,
    pub answer: String,
}

// ---------------------------------------------------------------------------
// Routes
// ---------------------------------------------------------------------------

/// `POST /nl2sql/<source_id>` — ask a natural-language question over a registered source.
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

/// `GET /nl2sql/<source_id>/catalog` — inspect the active metadata generation.
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

/// `POST /nl2sql/<source_id>/catalog/refresh` — rebuild schema cards for a source.
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
        refresh_catalog_with_trigger(&state.db, &state.config, &spec, source_id, "manual").await?;
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

/// `GET /nl2sql/<source_id>/catalog/history` — recent metadata checks and refreshes.
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
    Ok(())
}

fn bson_datetime(document: &mongodb::bson::Document, field: &str) -> Option<String> {
    document
        .get_datetime(field)
        .ok()
        .map(|value| chrono::DateTime::<chrono::Utc>::from(value.to_system_time()).to_rfc3339())
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Choose the most relevant registered source from its schema cards and prepare
/// a validated, read-only SQL answer for the main chat route.
pub(crate) async fn prepare_auto_query(
    state: &AppState,
    question: &str,
) -> AppResult<Option<PreparedNlQuery>> {
    if !state.config.router.text2sql_enabled {
        return Ok(None);
    }
    let source_ids = crate::connectors::routes::connected_source_ids(&state.db).await?;
    if source_ids.is_empty() {
        return Ok(None);
    }
    let mut linked = super::linker::link_best_source(
        &state.db,
        &state.config,
        question,
        &source_ids,
        state.config.router.nl2sql_tables_max,
    )
    .await?;

    // Existing sources may predate the NL-to-SQL catalog. Build missing cards
    // lazily once so chat works without an operator-only setup step.
    if linked.is_none() {
        for source_id in &source_ids {
            match crate::connectors::routes::load_spec(&state.db, &state.config, &source_id).await {
                Ok(spec) => {
                    if let Err(error) =
                        refresh_catalog(&state.db, &state.config, &spec, &source_id).await
                    {
                        tracing::warn!(%source_id, %error, "failed to lazily refresh SQL schema catalog");
                    }
                }
                Err(error) => {
                    tracing::warn!(%source_id, %error, "failed to load SQL source for catalog refresh");
                }
            }
        }
        linked = super::linker::link_best_source(
            &state.db,
            &state.config,
            question,
            &source_ids,
            state.config.router.nl2sql_tables_max,
        )
        .await?;
    }

    let Some((source_id, cards)) = linked else {
        return Ok(None);
    };
    prepare_with_cards(state, &source_id, question, cards)
        .await
        .map(Some)
}

async fn prepare_query(
    state: &AppState,
    source_id: &str,
    question: &str,
) -> AppResult<PreparedNlQuery> {
    let cards = link(
        &state.db,
        &state.config,
        question,
        source_id,
        state.config.router.nl2sql_tables_max,
    )
    .await?;
    prepare_with_cards(state, source_id, question, cards).await
}

async fn prepare_with_cards(
    state: &AppState,
    source_id: &str,
    question: &str,
    schema_cards: Vec<crate::nl2sql::spec::TableCard>,
) -> AppResult<PreparedNlQuery> {
    if schema_cards.is_empty() {
        return Err(AppError::BadRequest(
            "no schema cards found; refresh the source catalog first".into(),
        ));
    }

    use crate::connectors::routes::load_spec;
    let source_kind = load_spec(&state.db, &state.config, source_id).await?.kind;
    let allowed_tables: Vec<String> = schema_cards
        .iter()
        .map(|card| card.table_name.clone())
        .collect();

    if let Some(sql) = deterministic_sql(
        question,
        &schema_cards,
        source_kind,
        state.config.router.nl2sql_max_rows,
    ) {
        match validate_sql(
            &sql,
            source_kind,
            state.config.router.nl2sql_max_rows,
            &allowed_tables,
        ) {
            Ok(validated) => {
                match run_select(&state.db, &state.config, source_id, &validated.sql).await {
                    Ok((columns, rows)) => {
                        tracing::info!(
                            source_id,
                            row_count = rows.len(),
                            "deterministic SQL query executed"
                        );
                        let answer = summarize_result(&columns, &rows);
                        return Ok(PreparedNlQuery {
                            source_id: source_id.to_string(),
                            sql: validated.sql,
                            explanation:
                                "Read-only query compiled and validated for the selected source."
                                    .to_string(),
                            columns,
                            rows,
                            answer,
                        });
                    }
                    Err(error) => {
                        tracing::warn!(%error, "deterministic SQL execution failed; using local planner")
                    }
                }
            }
            Err(error) => {
                tracing::warn!(%error, "deterministic SQL validation failed; using local planner")
            }
        }
    }

    let foundry = state.foundry()?;
    let spec = state.spec_for(AgentKind::TextToSql);
    let few_shots: Vec<crate::nl2sql::spec::SqlExample> =
        Vec::with_capacity(state.config.router.nl2sql_fewshots);
    let plan_timeout =
        std::time::Duration::from_secs(state.config.router.nl2sql_plan_timeout_secs.max(1));
    let planning_deadline = tokio::time::Instant::now() + plan_timeout;
    let mut repaired = false;
    let mut emit = tokio::time::timeout_at(
        planning_deadline,
        generate::plan_sql(
            &foundry,
            &spec,
            source_kind,
            &schema_cards,
            &few_shots,
            question,
        ),
    )
    .await
    .map_err(|_| AppError::Unavailable("text-to-SQL planning timed out".into()))??;

    let mut validated = match validate_sql(
        &emit.sql,
        source_kind,
        state.config.router.nl2sql_max_rows,
        &allowed_tables,
    ) {
        Ok(validated) => validated,
        Err(error) => {
            repaired = true;
            emit = tokio::time::timeout_at(
                planning_deadline,
                generate::plan_sql_repair(
                    &foundry,
                    &spec,
                    source_kind,
                    &schema_cards,
                    &few_shots,
                    question,
                    &emit.sql,
                    &error.to_string(),
                ),
            )
            .await
            .map_err(|_| {
                AppError::Unavailable("text-to-SQL planning deadline exceeded".into())
            })??;
            validate_sql(
                &emit.sql,
                source_kind,
                state.config.router.nl2sql_max_rows,
                &allowed_tables,
            )?
        }
    };

    let (columns, rows) =
        match run_select(&state.db, &state.config, source_id, &validated.sql).await {
            Ok(result) => result,
            Err(error) if !repaired => {
                emit = tokio::time::timeout_at(
                    planning_deadline,
                    generate::plan_sql_repair(
                        &foundry,
                        &spec,
                        source_kind,
                        &schema_cards,
                        &few_shots,
                        question,
                        &validated.sql,
                        &error.to_string(),
                    ),
                )
                .await
                .map_err(|_| {
                    AppError::Unavailable("text-to-SQL planning deadline exceeded".into())
                })??;
                validated = validate_sql(
                    &emit.sql,
                    source_kind,
                    state.config.router.nl2sql_max_rows,
                    &allowed_tables,
                )?;
                run_select(&state.db, &state.config, source_id, &validated.sql).await?
            }
            Err(error) => return Err(error),
        };

    tracing::info!(
        source_id,
        row_count = rows.len(),
        "text-to-SQL query executed"
    );

    let answer = summarize_result(&columns, &rows);

    Ok(PreparedNlQuery {
        source_id: source_id.to_string(),
        sql: validated.sql,
        explanation: "Read-only query generated and validated for the selected source.".to_string(),
        columns,
        rows,
        answer,
    })
}

fn deterministic_sql(
    question: &str,
    schema_cards: &[crate::nl2sql::spec::TableCard],
    source_kind: crate::connectors::SourceKind,
    max_rows: i64,
) -> Option<String> {
    let question = normalize_question(question);
    let max_rows = max_rows.max(1);

    for card in schema_cards {
        if !is_safe_table_name(&card.table_name) {
            continue;
        }
        let entity = card
            .table_name
            .rsplit('.')
            .next()
            .unwrap_or(&card.table_name)
            .replace('_', " ")
            .to_ascii_lowercase();
        let mut entities = vec![entity.clone()];
        if let Some(singular) = entity.strip_suffix('s') {
            if !singular.is_empty() {
                entities.push(singular.to_string());
            }
        }

        for entity_name in entities {
            let count_templates = [
                format!("how many {entity_name}"),
                format!("how many {entity_name} do we have"),
                format!("how many {entity_name} are there"),
                format!("count {entity_name}"),
                format!("number of {entity_name}"),
                format!("what is the number of {entity_name}"),
            ];
            if count_templates.contains(&question) {
                let alias = entity.replace(' ', "_").trim_end_matches('s').to_string();
                return Some(format!(
                    "SELECT COUNT(*) AS {alias}_count FROM {}",
                    card.table_name
                ));
            }

            if let Some(sql) =
                relationship_filtered_count_sql(question.as_str(), &entity_name, card, schema_cards)
            {
                return Some(sql);
            }

            if let Some(sql) = grouped_count_sql(question.as_str(), &entity_name, card, source_kind)
            {
                return Some(sql);
            }

            if let Some(limit) = listing_limit(&question, &entity_name, max_rows) {
                return Some(match source_kind {
                    crate::connectors::SourceKind::Mssql => {
                        format!("SELECT TOP {limit} * FROM {}", card.table_name)
                    }
                    crate::connectors::SourceKind::Postgres
                    | crate::connectors::SourceKind::Mysql => {
                        format!("SELECT * FROM {} LIMIT {limit}", card.table_name)
                    }
                });
            }
        }
    }
    None
}

fn relationship_filtered_count_sql(
    question: &str,
    entity: &str,
    base: &crate::nl2sql::spec::TableCard,
    cards: &[crate::nl2sql::spec::TableCard],
) -> Option<String> {
    let (preposition, value) =
        ["are from", "come from", "are in"]
            .into_iter()
            .find_map(|preposition| {
                question
                    .strip_prefix(&format!("how many {entity} {preposition} "))
                    .map(|value| (preposition, value.trim()))
            })?;
    if value.is_empty() || value.len() > 128 {
        return None;
    }

    let geographic = [
        "county", "city", "location", "region", "state", "country", "district", "ward",
    ];
    let mut candidates = Vec::new();
    for edge in &base.fk_edges {
        if !is_safe_identifier(&edge.column) || !is_safe_identifier(&edge.ref_column) {
            continue;
        }
        let Some(target) = cards.iter().find(|card| {
            card.table_name.eq_ignore_ascii_case(&edge.ref_table)
                || card
                    .table_name
                    .rsplit('.')
                    .next()
                    .is_some_and(|name| name.eq_ignore_ascii_case(&edge.ref_table))
        }) else {
            continue;
        };
        if !is_safe_table_name(&target.table_name) {
            continue;
        }
        let lower_column = edge.column.to_ascii_lowercase();
        let relation = lower_column
            .strip_suffix("_id")
            .unwrap_or(&lower_column)
            .to_string();
        let relation_name = format!("{relation}_name");
        let label = target.columns.iter().find(|column| {
            is_safe_identifier(&column.name)
                && (["name", "title", "label", "code"]
                    .iter()
                    .any(|candidate| column.name.eq_ignore_ascii_case(candidate))
                    || column.name.eq_ignore_ascii_case(&relation_name))
        })?;
        let sample_match = label
            .sample_values
            .iter()
            .any(|sample| normalize_question(sample) == value);
        let score = i32::from(sample_match) * 10
            + i32::from(question.split_whitespace().any(|word| word == relation)) * 4
            + i32::from(preposition.contains("from") && geographic.contains(&relation.as_str()))
                * 5;
        if score > 0 {
            candidates.push((score, edge, target, label));
        }
    }
    candidates.sort_by(|a, b| b.0.cmp(&a.0));
    let (_, edge, target, label) = candidates.into_iter().next()?;
    let literal = value.replace('\'', "''");
    let alias = entity.replace(' ', "_").trim_end_matches('s').to_string();
    Some(format!(
        "SELECT COUNT(*) AS {alias}_count FROM {base_table} AS base JOIN {target_table} AS lookup ON base.{base_column} = lookup.{target_column} WHERE LOWER(lookup.{label_column}) = LOWER('{literal}')",
        base_table = base.table_name,
        target_table = target.table_name,
        base_column = edge.column,
        target_column = edge.ref_column,
        label_column = label.name,
    ))
}

fn grouped_count_sql(
    question: &str,
    entity: &str,
    card: &crate::nl2sql::spec::TableCard,
    source_kind: crate::connectors::SourceKind,
) -> Option<String> {
    let count_alias = format!("{}_count", entity.replace(' ', "_").trim_end_matches('s'));
    for column in &card.columns {
        if !is_safe_identifier(&column.name) {
            continue;
        }
        let label = column.name.replace('_', " ").to_ascii_lowercase();
        let templates = [
            format!("count {entity} by {label}"),
            format!("number of {entity} by {label}"),
            format!("show number of {entity} by {label}"),
            format!("show the number of {entity} by {label}"),
        ];
        if templates.iter().any(|template| template == question) {
            return Some(format!(
                "SELECT {column}, COUNT(*) AS {count_alias} FROM {table} GROUP BY {column} ORDER BY {count_alias} DESC",
                column = column.name,
                table = card.table_name,
            ));
        }
    }

    for period in ["day", "month", "year"] {
        let templates = [
            format!("count {entity} per {period}"),
            format!("count {entity} by {period}"),
            format!("number of {entity} per {period}"),
            format!("number of {entity} by {period}"),
            format!("show number of {entity} per {period}"),
            format!("show the number of {entity} per {period}"),
        ];
        if !templates.iter().any(|template| template == question) {
            continue;
        }
        let preferred = format!("{}_date", entity.trim_end_matches('s').replace(' ', "_"));
        let date_column = card
            .columns
            .iter()
            .filter(|column| is_safe_identifier(&column.name))
            .find(|column| column.name.eq_ignore_ascii_case(&preferred))
            .or_else(|| {
                card.columns.iter().find(|column| {
                    is_safe_identifier(&column.name)
                        && (column.type_.to_ascii_lowercase().contains("date")
                            || column.type_.to_ascii_lowercase().contains("time"))
                })
            })?;
        let bucket = match source_kind {
            crate::connectors::SourceKind::Postgres => {
                format!("DATE_TRUNC('{period}', {})", date_column.name)
            }
            crate::connectors::SourceKind::Mysql => match period {
                "day" => format!("DATE({})", date_column.name),
                "month" => format!("DATE_FORMAT({}, '%Y-%m-01')", date_column.name),
                _ => format!("DATE_FORMAT({}, '%Y-01-01')", date_column.name),
            },
            crate::connectors::SourceKind::Mssql => match period {
                "day" => format!("CAST({} AS date)", date_column.name),
                "month" => format!("DATEFROMPARTS(YEAR({0}), MONTH({0}), 1)", date_column.name),
                _ => format!("DATEFROMPARTS(YEAR({}), 1, 1)", date_column.name),
            },
        };
        return Some(format!(
            "SELECT {bucket} AS {period}, COUNT(*) AS {count_alias} FROM {table} GROUP BY {bucket} ORDER BY {period}",
            table = card.table_name,
        ));
    }
    None
}

fn normalize_question(question: &str) -> String {
    question
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() || character == '_' {
                character.to_ascii_lowercase()
            } else {
                ' '
            }
        })
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

fn is_safe_table_name(table: &str) -> bool {
    !table.is_empty() && table.split('.').all(is_safe_identifier)
}

fn is_safe_identifier(identifier: &str) -> bool {
    !identifier.is_empty()
        && identifier
            .chars()
            .next()
            .is_some_and(|character| character.is_ascii_alphabetic() || character == '_')
        && identifier
            .chars()
            .all(|character| character.is_ascii_alphanumeric() || character == '_')
}

fn listing_limit(question: &str, entity: &str, max_rows: i64) -> Option<i64> {
    for prefix in ["list", "show me", "show", "display", "get"] {
        let Some(mut rest) = question.strip_prefix(prefix) else {
            continue;
        };
        rest = rest.trim();
        rest = rest.strip_prefix("the ").unwrap_or(rest);
        if rest == entity || rest == format!("all {entity}") {
            return Some(max_rows);
        }
        rest = rest.strip_prefix("first ").unwrap_or(rest);
        let (amount, remainder) = rest.split_once(' ')?;
        if remainder == entity {
            let requested = amount.parse::<i64>().ok()?;
            return Some(requested.clamp(1, max_rows));
        }
    }
    None
}

fn summarize_result(columns: &[String], rows: &[Vec<serde_json::Value>]) -> String {
    if rows.is_empty() {
        return "No matching records were found.".to_string();
    }

    if columns.len() == 1 && rows.len() == 1 && rows[0].len() == 1 {
        let label = columns[0].replace('_', " ");
        let value = match &rows[0][0] {
            serde_json::Value::String(value) => value.clone(),
            value => value.to_string(),
        };
        return format!("{label}: {value}.");
    }

    let noun = if rows.len() == 1 { "row" } else { "rows" };
    format!("Returned {} {noun} from the live database.", rows.len())
}

fn token_event(text: &str) -> Event {
    Event::data(serde_json::to_string(text).unwrap_or_else(|_| "\"\"".to_string())).event("token")
}

#[cfg(test)]
mod tests {
    use super::{deterministic_sql, summarize_result};
    use crate::connectors::SourceKind;
    use crate::nl2sql::spec::{CardColumn, CardFkEdge, TableCard};

    fn column(name: &str, type_: &str) -> CardColumn {
        CardColumn {
            name: name.into(),
            type_: type_.into(),
            nullable: false,
            is_primary_key: false,
            is_foreign_key: false,
            sample_values: Vec::new(),
            profile: Default::default(),
        }
    }

    fn patients_card() -> TableCard {
        TableCard {
            source_id: "source-1".into(),
            table_name: "patients".into(),
            row_count: 60,
            columns: vec![column("gender", "character varying")],
            fk_edges: Vec::new(),
            card_vector: None,
            card_text: "Table: patients".into(),
        }
    }

    #[test]
    fn compiles_simple_counts_without_the_model() {
        let cards = vec![patients_card()];
        let sql = deterministic_sql(
            "How many patients do we have?",
            &cards,
            SourceKind::Postgres,
            100,
        );
        assert_eq!(
            sql.as_deref(),
            Some("SELECT COUNT(*) AS patient_count FROM patients")
        );
    }

    #[test]
    fn compiles_bounded_listings_without_the_model() {
        let cards = vec![patients_card()];
        assert_eq!(
            deterministic_sql("List 5 patients", &cards, SourceKind::Postgres, 100).as_deref(),
            Some("SELECT * FROM patients LIMIT 5")
        );
        assert_eq!(
            deterministic_sql(
                "Show me the first 5 patients",
                &cards,
                SourceKind::Mssql,
                100
            )
            .as_deref(),
            Some("SELECT TOP 5 * FROM patients")
        );
    }

    #[test]
    fn compiles_grouped_counts_without_the_model() {
        let cards = vec![patients_card()];
        assert_eq!(
            deterministic_sql(
                "Count patients by gender",
                &cards,
                SourceKind::Postgres,
                100
            )
            .as_deref(),
            Some(
                "SELECT gender, COUNT(*) AS patient_count FROM patients GROUP BY gender ORDER BY patient_count DESC"
            )
        );
    }

    #[test]
    fn compiles_time_buckets_without_the_model() {
        let mut encounters = patients_card();
        encounters.table_name = "encounters".into();
        encounters.columns = vec![column("encounter_date", "timestamp with time zone")];
        let cards = vec![encounters];
        assert_eq!(
            deterministic_sql(
                "Show the number of encounters per month",
                &cards,
                SourceKind::Postgres,
                100
            )
            .as_deref(),
            Some(
                "SELECT DATE_TRUNC('month', encounter_date) AS month, COUNT(*) AS encounter_count FROM encounters GROUP BY DATE_TRUNC('month', encounter_date) ORDER BY month"
            )
        );
    }

    #[test]
    fn compiles_foreign_key_lookup_counts_without_the_model() {
        let mut patients = patients_card();
        patients.columns.push(column("county_id", "integer"));
        patients.fk_edges.push(CardFkEdge {
            column: "county_id".into(),
            ref_table: "counties".into(),
            ref_column: "id".into(),
        });
        let counties = TableCard {
            source_id: "source-1".into(),
            table_name: "counties".into(),
            row_count: 4,
            columns: vec![column("id", "integer"), column("name", "character varying")],
            fk_edges: Vec::new(),
            card_vector: None,
            card_text: "Table: counties".into(),
        };
        assert_eq!(
            deterministic_sql(
                "How many patients are from Nyeri?",
                &[patients, counties],
                SourceKind::Postgres,
                100,
            )
            .as_deref(),
            Some(
                "SELECT COUNT(*) AS patient_count FROM patients AS base JOIN counties AS lookup ON base.county_id = lookup.id WHERE LOWER(lookup.name) = LOWER('nyeri')"
            )
        );
    }

    #[test]
    fn leaves_filtered_questions_for_the_local_model() {
        let cards = vec![patients_card()];
        assert!(
            deterministic_sql("List 5 female patients", &cards, SourceKind::Postgres, 100)
                .is_none()
        );
        assert!(
            deterministic_sql(
                "How many active patients?",
                &cards,
                SourceKind::Postgres,
                100
            )
            .is_none()
        );
    }

    #[test]
    fn summarizes_a_scalar_without_another_model_call() {
        let answer = summarize_result(&["patient_count".into()], &[vec![60.into()]]);
        assert_eq!(answer, "patient count: 60.");
    }

    #[test]
    fn summarizes_empty_and_tabular_results() {
        assert_eq!(
            summarize_result(&[], &[]),
            "No matching records were found."
        );
        assert_eq!(
            summarize_result(&["id".into()], &[vec![1.into()], vec![2.into()]]),
            "Returned 2 rows from the live database."
        );
    }
}
