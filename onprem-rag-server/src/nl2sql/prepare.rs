//! Prepare helpers: source linking, SQL planning, and the IR shadow / live path.
//!
//! All publicly exported items are re-exported from `nl2sql::routes` for backward
//! compatibility with existing call sites.

use serde::Serialize;

use crate::error::{AppError, AppResult};
use crate::foundry::router::ModelRole;
use crate::state::AppState;

use super::catalog::refresh_catalog;
use super::execute::run_select;
use super::validate::validate_sql;
use super::text::summarize_result;
use super::{generate, linker};

/// The read allow-list an agent applies to SQL planning (plan 05 §3, hook 1).
///
/// `tables` narrows the linker's candidate cards; `pin_source` restricts which
/// registered source may answer. Both are relevance boundaries — the security
/// boundary is `src/auth/`, and every SQL statement still passes the single
/// `validate::validate_sql` guard with `allowed_tables` derived from the cards
/// that survived this narrowing.
#[derive(Debug, Clone, Default)]
pub(crate) struct SourceScope {
    pub tables: Vec<String>,
    pub pin_source: Option<String>,
}

impl SourceScope {
    fn tables(&self) -> Option<&[String]> {
        if self.tables.is_empty() {
            None
        } else {
            Some(self.tables.as_slice())
        }
    }
}

/// Wire-format event for the `sql` SSE field (used by http.rs).
#[derive(Debug, Serialize)]
pub(crate) struct SqlEvent<'a> {
    pub sql: &'a str,
    pub explanation: &'a str,
}

/// A validated, ready-to-stream SQL result with its answer narration.
pub(crate) struct PreparedNlQuery {
    pub source_id: String,
    pub sql: String,
    pub explanation: String,
    pub columns: Vec<String>,
    pub rows: Vec<Vec<serde_json::Value>>,
    pub answer: String,
    /// Flat IR spec, present when the IR parsed the question (shadow or live mode).
    pub spec: Option<crate::nl2sql::ir::PlannedSpec>,
}

// ---------------------------------------------------------------------------
// Public helpers (called by agents/routes.rs)
// ---------------------------------------------------------------------------

/// Choose the most relevant registered source from its schema cards and prepare
/// a validated, read-only SQL answer for the main chat route.
pub(crate) async fn prepare_auto_query(
    state: &AppState,
    question: &str,
    scope: Option<&SourceScope>,
) -> AppResult<Option<PreparedNlQuery>> {
    let Some((source_id, cards)) = link_auto_source(state, question, scope).await? else {
        return Ok(None);
    };
    prepare_with_cards(state, &source_id, question, cards)
        .await
        .map(Some)
}

/// Like [`prepare_auto_query`] but only tries the deterministic compiler - no
/// model planning. Used for speculative attempts (e.g. anaphoric follow-ups)
/// where a miss must stay cheap and can never trigger a model call.
pub(crate) async fn prepare_auto_query_deterministic(
    state: &AppState,
    question: &str,
    scope: Option<&SourceScope>,
) -> AppResult<Option<PreparedNlQuery>> {
    // Name-only overview wording does not mention a table. Add a linking hint so
    // the patients card is selected, while compiling the untouched question.
    let link_question = if super::routes::is_patient_overview_question(question) {
        format!("{question} patient")
    } else {
        question.to_string()
    };
    let Some((source_id, cards)) = link_auto_source(state, &link_question, scope).await? else {
        return Ok(None);
    };
    if cards.is_empty() {
        return Ok(None);
    }
    let source_kind = crate::connectors::routes::load_spec(&state.db, &state.config, &source_id)
        .await?
        .kind;
    let allowed_tables: Vec<String> = cards.iter().map(|card| card.table_name.clone()).collect();
    Ok(try_deterministic(
        state,
        &source_id,
        question,
        &cards,
        source_kind,
        &allowed_tables,
    )
    .await)
}

// ---------------------------------------------------------------------------
// Package-private helpers
// ---------------------------------------------------------------------------

/// Link the best connected source for a question, lazily building schema cards
/// for sources that predate the NL-to-SQL catalog.
pub(crate) async fn link_auto_source(
    state: &AppState,
    question: &str,
    scope: Option<&SourceScope>,
) -> AppResult<Option<(String, Vec<crate::nl2sql::spec::TableCard>)>> {
    if !state.config.router.text2sql_enabled {
        return Ok(None);
    }
    let mut source_ids = crate::connectors::routes::connected_source_ids(&state.db).await?;
    // An explicit source pin removes the other sources from consideration rather
    // than reordering them: picking "the first that matches" is exactly the
    // positional resolution this codebase has been bitten by before.
    if let Some(pin) = scope.and_then(|s| s.pin_source.as_deref()) {
        source_ids.retain(|id| id == pin);
    }
    if source_ids.is_empty() {
        return Ok(None);
    }
    let scope_tables = scope.and_then(|s| s.tables());
    let mut linked = linker::link_best_source(
        &state.db,
        &state.config,
        question,
        &source_ids,
        state.config.router.nl2sql_tables_max,
        scope_tables,
    )
    .await?;

    // Existing sources may predate the NL-to-SQL catalog. Build missing cards
    // lazily once so chat works without an operator-only setup step.
    if linked.is_none() {
        for source_id in &source_ids {
            match crate::connectors::routes::load_spec(&state.db, &state.config, source_id).await {
                Ok(spec) => {
                    if let Err(error) =
                        refresh_catalog(&state.db, &state.config, &spec, source_id, &state.binding_cache()).await
                    {
                        tracing::warn!(%source_id, %error, "failed to lazily refresh SQL schema catalog");
                    }
                }
                Err(error) => {
                    tracing::warn!(%source_id, %error, "failed to load SQL source for catalog refresh");
                }
            }
        }
        linked = linker::link_best_source(
            &state.db,
            &state.config,
            question,
            &source_ids,
            state.config.router.nl2sql_tables_max,
            scope_tables,
        )
        .await?;
    }

    Ok(linked)
}

pub(crate) async fn prepare_query(
    state: &AppState,
    source_id: &str,
    question: &str,
) -> AppResult<PreparedNlQuery> {
    let cards = linker::link(
        &state.db,
        &state.config,
        question,
        source_id,
        state.config.router.nl2sql_tables_max,
        None,
    )
    .await?;
    prepare_with_cards(state, source_id, question, cards).await
}

pub(crate) async fn prepare_with_cards(
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

    // IR shadow mode (always fall through).
    // parse -> bind -> compile -> validate runs unconditionally; result is only logged.
    // Live execution (ONPREM_SQL_IR_ENABLED=true) also falls through to the template path --
    // returning an empty result set would be indistinguishable from "no rows matched".
    // TODO(plan03-live): wire execute() and remove the fallthrough once golden suite >= 55/60.
    let _ir_outcome = ir_shadow_run(state, source_id, question, &schema_cards, source_kind, &allowed_tables);
    if state.config.sql_ir_enabled {
        tracing::debug!(source_id,
            "IR live-mode: execute wiring not yet present (TODO plan03-live); \
             falling through to template path");
    }

    if let Some(prepared) = try_deterministic(
        state,
        source_id,
        question,
        &schema_cards,
        source_kind,
        &allowed_tables,
    )
    .await
    {
        return Ok(prepared);
    }

    let foundry = state.foundry()?;
    let spec = state.spec_for(ModelRole::TextToSql);
    let few_shots: Vec<crate::nl2sql::spec::SqlExample> =
        Vec::with_capacity(state.config.router.nl2sql_fewshots);
    let prompt_token_budget = state.config.router.nl2sql_prompt_token_budget;
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
            prompt_token_budget,
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
                    prompt_token_budget,
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
                        prompt_token_budget,
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
        spec: None,
    })
}

// ---------------------------------------------------------------------------
// IR shadow / live path
// ---------------------------------------------------------------------------

/// Run the QuerySpec IR (parse -> bind -> compile -> validate) synchronously.
/// Always returns None in shadow mode. See TODO(plan03-live).
pub(crate) fn ir_shadow_run(
    state: &AppState,
    source_id: &str,
    question: &str,
    schema_cards: &[crate::nl2sql::spec::TableCard],
    source_kind: crate::connectors::SourceKind,
    allowed_tables: &[String],
) -> Option<PreparedNlQuery> {
    use crate::nl2sql::ir::{bind, compile, parse, ParseOutcome, PlannedSpec};
    use crate::nl2sql::validate::validate_sql;

    let binding = match state.binding_for(source_id) {
        Some(b) => b,
        None => {
            tracing::debug!(source_id, "ir_shadow: no binding cached, skipping");
            return None;
        }
    };

    let outcome = parse(question, None, &binding, allowed_tables);
    let (spec, missing) = match outcome {
        ParseOutcome::Parsed { spec, missing } => (spec, missing),
        ParseOutcome::NoParse => {
            tracing::debug!(source_id, "ir_shadow: no_parse");
            return None;
        }
    };

    let bound = match bind(spec, &binding, schema_cards, allowed_tables, question) {
        Ok(b) => b,
        Err(e) => {
            tracing::debug!(source_id, error = %e, "ir_shadow: bind_error");
            return None;
        }
    };

    let compiled = match compile(&bound, source_kind, state.config.router.nl2sql_max_rows as i64, chrono::Utc::now()) {
        Ok(c) => c,
        Err(e) => {
            tracing::debug!(source_id, error = ?e, "ir_shadow: compile_error");
            return None;
        }
    };

    let validated = match validate_sql(&compiled.sql, source_kind, state.config.router.nl2sql_max_rows as i64, allowed_tables) {
        Ok(v) => v,
        Err(e) => {
            tracing::debug!(source_id, error = %e, "ir_shadow: validate_error");
            return None;
        }
    };

    let _dto = PlannedSpec::try_from(&bound).ok();

    tracing::info!(
        source_id,
        rule = %bound.provenance.rule,
        shape = ?bound.shape,
        subject_concept = %bound.subject.concept.slug(),
        missing_slots = missing.len(),
        live_mode = state.config.sql_ir_enabled,
        sql_len = validated.sql.len(),
        "ir_shadow"
    );

    // Shadow mode: always fall through -- never execute, never return empty result.
    // TODO(plan03-live): wire execute() and return Some(PreparedNlQuery{..}) here when
    //   ONPREM_SQL_IR_ENABLED=true is promoted.
    None
}

/// Deterministic compile -> validate -> execute; `None` on any miss or failure.
pub(crate) async fn try_deterministic(
    state: &AppState,
    source_id: &str,
    question: &str,
    schema_cards: &[crate::nl2sql::spec::TableCard],
    source_kind: crate::connectors::SourceKind,
    allowed_tables: &[String],
) -> Option<PreparedNlQuery> {
    let sql = super::routes::deterministic_sql(
        question,
        schema_cards,
        source_kind,
        state.config.router.nl2sql_max_rows,
    )?;
    let validated = match validate_sql(
        &sql,
        source_kind,
        state.config.router.nl2sql_max_rows,
        allowed_tables,
    ) {
        Ok(validated) => validated,
        Err(error) => {
            tracing::warn!(%error, "deterministic SQL validation failed; using local planner");
            return None;
        }
    };
    match run_select(&state.db, &state.config, source_id, &validated.sql).await {
        Ok((columns, rows)) => {
            tracing::info!(
                source_id,
                row_count = rows.len(),
                "deterministic SQL query executed"
            );
            let answer = summarize_result(&columns, &rows);
            Some(PreparedNlQuery {
                source_id: source_id.to_string(),
                sql: validated.sql,
                explanation: "Read-only query compiled and validated for the selected source."
                    .to_string(),
                columns,
                rows,
                answer,
                spec: None,
            })
        }
        Err(error) => {
            tracing::warn!(%error, "deterministic SQL execution failed; using local planner");
            None
        }
    }
}
