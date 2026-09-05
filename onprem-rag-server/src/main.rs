//! On-prem health-records RAG server (Rocket).
//!
//! Boots the HTTP API, connects to DocumentDB, and manages Foundry Local,
//! source connectors, ingestion, and RAG chat.

mod admission;
mod agents;
mod aggregation;
mod answer;
mod auth;
mod config;
mod connectors;
mod crypto;
mod documentdb;
mod embed;
mod error;
mod foundry;
mod ingest;
mod logstream;
mod memory;
mod nl2sql;
mod ontology;
mod progress;
mod rag;
mod retrieval;
mod router;
mod routes;
mod settings;
mod state;
mod system;
mod telemetry;
mod verify;

use config::Config;
use documentdb::DocumentDb;
use state::AppState;

#[rocket::launch]
async fn rocket() -> _ {
    // Load `.env` in development; real env vars always win.
    let _ = dotenvy::dotenv();

    // Two sinks for one event stream: the usual formatted terminal output, plus a
    // broadcast layer that feeds the app's live log windows (GET /logs/stream). The
    // same EnvFilter gates both, so `RUST_LOG` controls what the app sees too.
    //
    // Default is `warn` for everything except our own crate: Rocket's own `info`
    // level logs a `Matched: (route_name) METHOD /path` line for every single
    // request plus a full route-listing banner at boot — real signal-to-noise
    // killers in both the terminal and the app's log viewer. `RUST_LOG` still
    // overrides this wholesale if you want Rocket's request log back.
    use tracing_subscriber::layer::SubscriberExt;
    use tracing_subscriber::util::SubscriberInitExt;
    let env_filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| "warn,onprem_server=debug".into());
    tracing_subscriber::registry()
        .with(env_filter)
        .with(tracing_subscriber::fmt::layer())
        .with(logstream::BroadcastLayer)
        .init();

    let config = Config::from_env();
    let issues = config.security_issues();
    if !issues.is_empty() {
        if config.production {
            for i in &issues {
                tracing::error!("insecure configuration: {i}");
            }
            panic!(
                "refusing to start in production with insecure configuration ({} issue(s)); see errors above and .env.example",
                issues.len()
            );
        } else {
            for i in &issues {
                tracing::warn!("insecure configuration (dev): {i}");
            }
        }
    }
    tracing::info!(port = config.port, db = %config.documentdb_db, "starting onprem-rag-server");

    // The driver connects lazily; a failed ping here is a warning, not fatal.
    let db = DocumentDb::connect(&config)
        .await
        .expect("failed to build DocumentDB client");

    // Build the initial catalog after the DB ping so it reflects ingested data.
    // Falls back to an empty catalog on error (server still boots).
    let initial_catalog = match db.ping().await {
        Ok(()) => {
            tracing::info!("connected to DocumentDB");
            // Seed the default admin only when the DB is reachable.
            if let Err(e) = auth::seed::seed_admin(&db, &config).await {
                tracing::warn!(error = %e, "admin seed failed");
            }
            // Create the unique email index on users (idempotent on re-runs).
            if let Err(e) = documentdb::ensure_user_indexes(&db).await {
                tracing::warn!(error = %e, "user index creation failed (non-fatal)");
            }
            // Create chat collection indexes (idempotent, best-effort).
            if let Err(e) = documentdb::ensure_chat_indexes(&db).await {
                tracing::warn!(error = %e, "chat index creation failed (non-fatal)");
            }
            if let Err(e) = documentdb::ensure_records_indexes(&db).await {
                tracing::warn!(error = %e, "records index creation failed (non-fatal)");
            }
            if let Err(e) = nl2sql::catalog::ensure_nl2sql_indexes(&db, config.embedding_dims).await
            {
                tracing::warn!(error = %e, "schema metadata index creation failed (non-fatal)");
            }
            if let Err(e) = documentdb::ensure_ingest_generations(&db).await {
                tracing::warn!(error = %e, "ingestion generation backfill failed (non-fatal)");
            }
            if let Err(e) = documentdb::recover_abandoned_ingestions(&db).await {
                tracing::warn!(error = %e, "abandoned ingestion recovery failed (non-fatal)");
            }
            // Build catalog from ingested data now that the DB is reachable.
            aggregation::catalog::build_from_store(&db).await
        }
        Err(e) => {
            tracing::warn!(error = %e, "DocumentDB not reachable yet; skipping admin seed and catalog build");
            aggregation::catalog::Catalog::empty()
        }
    };

    // Load persisted per-role model routing overrides (best-effort).
    let router_overrides = settings::load_router_overrides(&db)
        .await
        .unwrap_or_default();

    // Initialise Foundry Local (chat). Non-fatal: if the native engine can't start
    // (e.g. Foundry Local not installed), the server still boots and Foundry routes
    // report 503 until it is available.
    let foundry = match foundry::FoundryManager::init(&config) {
        Ok(m) => Some(m),
        Err(e) => {
            tracing::warn!(error = %e, "Foundry Local unavailable; chat/model routes disabled");
            None
        }
    };

    let app_state = AppState::new(
        config.clone(),
        db.clone(),
        foundry,
        router_overrides,
        initial_catalog,
    );

    // Warm models in the background. Foundry EP registration must finish before
    // loading the SQL model or the first request can pay the NPU graph setup cost.
    if config.warmup_enabled {
        let wc = config.clone();
        let warmup_state = app_state.warmup_handle();
        let foundry = app_state.foundry_handle();
        // Warm the shared Core LLM (all generative roles resolve to it) so the first
        // request doesn't pay the load + EP graph-setup cost.
        let core_spec = app_state.spec_for(crate::foundry::router::AgentKind::Chat);
        tokio::spawn(async move {
            let local_models = async {
                let embed_ok = crate::embed::embed_query(&wc, "warmup")
                    .await
                    .inspect(|_| tracing::info!("warmup: embedder ready"))
                    .inspect_err(|e| tracing::warn!(error = %e, "warmup: embedder init failed"))
                    .is_ok();
                let rerank_ok = crate::retrieval::rerank::rerank(
                    &wc,
                    "warmup".to_string(),
                    vec!["warmup".to_string()],
                )
                .await
                .inspect(|_| tracing::info!("warmup: reranker ready"))
                .inspect_err(|e| tracing::warn!(error = %e, "warmup: reranker init failed"))
                .is_ok();
                embed_ok && rerank_ok
            };
            let core_model = async {
                let Some(foundry) = foundry else {
                    return true;
                };
                if let Err(error) = foundry.register_eps().await {
                    tracing::warn!(%error, "startup: execution-provider registration failed");
                    return false;
                }
                foundry
                    .warm_cached_model(&core_spec)
                    .await
                    .inspect_err(|error| tracing::warn!(%error, "warmup: core LLM preload failed"))
                    .is_ok()
            };
            let (local_ok, core_ok) = tokio::join!(local_models, core_model);
            warmup_state.store(
                if local_ok && core_ok { 1 } else { 3 },
                std::sync::atomic::Ordering::Relaxed,
            );
        });
    } else if let Some(foundry) = app_state.foundry_handle() {
        foundry.spawn_startup_registration();
    }

    // Conversation retention sweep (plan 22.7): boot-time + daily, no-op when
    // ONPREM_CONVERSATION_RETENTION_DAYS=0 (default: keep forever).
    memory::spawn_retention_sweep(db.clone(), config.clone());
    nl2sql::catalog::spawn_schema_poller(db.clone(), config.clone(), app_state.binding_cache());

    // Load persisted schema bindings into AppState (non-fatal — schema binding
    // is optional; server boots normally if bindings have never been built).
    if config.binding_enabled {
        match ontology::store::load_all_bindings(&db).await {
            Ok(all) => {
                tracing::info!("loaded {} schema binding(s) from storage", all.len());
                app_state.set_all_bindings(all);
            }
            Err(e) => tracing::warn!("failed to load schema bindings: {e}"),
        }
        // Ensure indexes for the new binding collections (idempotent)
        let db2 = db.clone();
        tokio::spawn(async move {
            if let Err(e) = ontology::store::ensure_binding_indexes(&db2).await {
                tracing::warn!("failed to create binding indexes: {e}");
            }
        });
    }

    // Bind address/port come from our config rather than Rocket.toml.
    let figment = rocket::Config::figment()
        .merge(("address", config.bind_address.clone()))
        .merge(("port", config.port));

    rocket::custom(figment)
        .manage(app_state)
        .mount(
            "/",
            rocket::routes![
                routes::health::health,
                routes::health::ready,
                routes::logs::logs_stream,
                routes::metrics::summary,
                routes::stats::stats
            ],
        )
        .mount(
            "/",
            rocket::routes![
                auth::routes::login,
                auth::routes::me,
                auth::routes::create_user,
                auth::routes::list_users,
                auth::routes::update_user,
                auth::routes::delete_user,
                auth::routes::set_user_password,
                auth::routes::update_me,
                auth::routes::change_my_password,
                auth::routes::logout,
                auth::routes::force_logout_user,
            ],
        )
        .mount(
            "/",
            rocket::routes![
                foundry::routes::hardware,
                foundry::routes::register_eps,
                foundry::routes::list_models,
                foundry::routes::select_model,
                foundry::routes::generate,
                foundry::routes::model_roles,
                foundry::routes::load_specialized_model,
                foundry::routes::pull_model,
                foundry::routes::set_router,
                foundry::routes::set_shared_router,
                foundry::routes::delete_model,
                foundry::routes::unload_model,
                foundry::routes::setup_status,
            ],
        )
        .mount(
            "/",
            rocket::routes![
                connectors::routes::list_sources,
                connectors::routes::test_source,
                connectors::routes::create_source,
                connectors::routes::update_source,
                connectors::routes::test_saved_source,
                connectors::routes::delete_source,
                connectors::routes::get_source_schema,
                connectors::routes::analyze_schema,
            ],
        )
        .mount(
            "/",
            rocket::routes![
                ingest::routes::start_ingest,
                ingest::routes::resume_ingest,
                ingest::routes::ingest_stream,
                ingest::routes::ingest_history,
                ingest::routes::delete_ingest_table,
                ingest::routes::delete_ingest_connection,
                ingest::routes::delete_ingest_all,
            ],
        )
        .mount(
            "/",
            rocket::routes![
                routes::explorer::list_records,
                routes::explorer::get_table_info,
                routes::audit::list_audit,
            ],
        )
        .mount(
            "/",
            rocket::routes![
                rag::routes::route,
                rag::routes::search,
                rag::routes::chat,
                nl2sql::http::nl_query,
                nl2sql::http::catalog_status,
                nl2sql::http::catalog_refresh,
                nl2sql::http::catalog_history,
                nl2sql::http::catalog_overrides,
                nl2sql::http::save_catalog_overrides,
            ],
        )
        .mount(
            "/",
            rocket::routes![
                routes::conversations::list_conversations,
                routes::conversations::list_agent_conversations,
                routes::conversations::create_conversation,
                routes::conversations::rename_conversation,
                routes::conversations::delete_conversation,
                routes::conversations::list_messages,
                routes::progress::run_progress,
            ],
        )
        .mount("/", rocket::routes![agents::routes::agent])
        .mount(
            "/",
            rocket::routes![
                ontology::routes::list_agents,
                ontology::routes::get_binding,
                ontology::routes::rebuild_binding,
                ontology::routes::get_binding_history,
            ],
        )
}
