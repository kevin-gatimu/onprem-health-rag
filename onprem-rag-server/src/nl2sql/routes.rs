//! NL-to-SQL routes.
//!
//! | Method | Path                                   | Auth    | Description                    |
//! |--------|----------------------------------------|---------|-------------------------------|
//! | POST   | /nl2sql/<source_id>                    | user    | Ask a question; SSE response  |
//! | POST   | /nl2sql/<source_id>/catalog/refresh    | admin   | Rebuild schema_catalog         |
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
use rocket::{State, post};
use serde::{Deserialize, Serialize};

use crate::auth::guard::AuthUser;
use crate::error::{AppError, AppResult};
use crate::foundry::router::AgentKind;
use crate::state::AppState;

use super::catalog::refresh_catalog;
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

#[derive(Debug, Serialize)]
struct SqlEvent<'a> {
    sql: &'a str,
    explanation: &'a str,
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
        return Err(AppError::Unavailable("NL-to-SQL is disabled; set ONPREM_TEXT2SQL_ENABLED=true".into()));
    }

    let foundry = state.foundry()?;
    let spec = state.spec_for(AgentKind::TextToSql);
    let config = &state.config;
    let db = &state.db;

    let req = body.into_inner();
    let question = req.question.clone();

    // Resolve the source's SQL dialect before entering the generator.
    let source_kind = {
        use crate::connectors::routes::load_spec;
        load_spec(db, config, source_id).await?.kind
    };

    // Schema linking: find the most relevant tables for the question.
    let schema_cards = link(
        db,
        config,
        &question,
        source_id,
        config.router.nl2sql_tables_max,
    )
    .await?;

    if schema_cards.is_empty() {
        return Err(AppError::BadRequest(
            "no schema cards found; run catalog refresh first".into(),
        ));
    }

    // Few-shot retrieval: not yet implemented. Slot is wired; the capacity hint
    // comes from config so the field is read and the compiler sees it used.
    let few_shots: Vec<crate::nl2sql::spec::SqlExample> =
        Vec::with_capacity(config.router.nl2sql_fewshots);

    // Generate SQL (first attempt).
    let emit = generate::plan_sql(
        &foundry,
        &spec,
        source_kind,
        &schema_cards,
        &few_shots,
        &question,
    )
    .await?;

    // Validate + LIMIT injection.
    let validated = match validate_sql(&emit.sql, source_kind, config.router.nl2sql_max_rows) {
        Ok(v) => v,
        Err(e) => {
            // One repair pass.
            let repaired = generate::plan_sql_repair(
                &foundry,
                &spec,
                source_kind,
                &schema_cards,
                &few_shots,
                &question,
                &emit.sql,
                &e.to_string(),
            )
            .await?;
            validate_sql(&repaired.sql, source_kind, config.router.nl2sql_max_rows)
                .map_err(|_| e)?
        }
    };

    let sql = validated.sql.clone();
    let explanation = emit.explanation.clone();

    // Execute.
    let (columns, rows) = run_select(db, config, source_id, &sql).await?;

    // Prepare a brief narration prompt (generation happens inside the SSE generator).
    let narration_prompt = build_narration_prompt(&question, &columns, &rows);
    let narration_spec = state.spec_for(AgentKind::Summarize);
    let mut narration_stream = foundry
        .generate_stream_with(&narration_spec, NARRATION_SYSTEM, &narration_prompt)
        .await?;

    let sql_event_json =
        serde_json::to_string(&SqlEvent { sql: &sql, explanation: &explanation })
            .unwrap_or_default();
    let columns_json = serde_json::to_string(&columns).unwrap_or_else(|_| "[]".to_string());
    let rows_json = serde_json::to_string(&rows).unwrap_or_else(|_| "[]".to_string());

    Ok(EventStream! {
        use futures::StreamExt;

        yield Event::data(
            serde_json::to_string("text_to_sql").unwrap_or_default()
        ).event("routed");

        yield Event::data(sql_event_json).event("sql");
        yield Event::data(columns_json).event("columns");
        yield Event::data(rows_json).event("rows");

        let mut think = crate::foundry::think_filter::ThinkFilter::new();
        while let Some(chunk) = narration_stream.next().await {
            match chunk {
                Ok(resp) => {
                    if let Some(token) = resp.choices.first().and_then(|c| c.delta.content.clone()) {
                        let visible = think.push(&token);
                        if !visible.is_empty() {
                            yield token_event(&visible);
                        }
                    }
                }
                Err(e) => {
                    yield Event::data(format!("Foundry Local: {e}")).event("error");
                    break;
                }
            }
        }
        let tail = think.finish();
        if !tail.is_empty() {
            yield token_event(&tail);
        }

        yield Event::data("").event("done");
    })
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
    let count = refresh_catalog(&state.db, &state.config, &spec, source_id).await?;

    Ok(Json(json!({ "source_id": source_id, "tables_indexed": count })))
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

const NARRATION_SYSTEM: &str = "You are a health-records assistant. \
    Summarise the query result in one or two plain sentences. \
    Do not reproduce the raw data; describe what it shows.";

fn build_narration_prompt(
    question: &str,
    columns: &[String],
    rows: &[Vec<serde_json::Value>],
) -> String {
    let row_count = rows.len();
    let cols = columns.join(", ");
    let preview: Vec<String> = rows
        .iter()
        .take(5)
        .map(|r| {
            r.iter()
                .map(|v| match v {
                    serde_json::Value::Null => "null".to_string(),
                    other => other.to_string(),
                })
                .collect::<Vec<_>>()
                .join(", ")
        })
        .collect();
    format!(
        "Question: {question}\n\
         Columns: {cols}\n\
         Rows returned: {row_count}\n\
         First rows:\n{}\n\n\
         Write a one-sentence summary.",
        preview.join("\n")
    )
}

fn token_event(text: &str) -> Event {
    Event::data(serde_json::to_string(text).unwrap_or_else(|_| "\"\"".to_string()))
        .event("token")
}
