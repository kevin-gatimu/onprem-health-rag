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
mod rag;
mod retrieval;
mod router;
mod routes;
mod settings;
mod state;
mod telemetry;

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

    // Register execution providers into the in-process core in the background.
    if let Some(f) = &foundry {
        f.spawn_startup_registration();
    }

    let app_state = AppState::new(
        config.clone(),
        db,
        foundry,
        router_overrides,
        initial_catalog,
    );

    // Warm start: prime the fastembed embedder and reranker in the background so
    // the first real request does not pay the ONNX model-load latency (~1–3 s).
    if config.warmup_enabled {
        let wc = config.clone();
        let warmup_state = app_state.warmup_handle();
        tokio::spawn(async move {
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
            warmup_state.store(
                if embed_ok && rerank_ok { 1 } else { 3 },
                std::sync::atomic::Ordering::Relaxed,
            );
        });
    }

    // Conversation retention sweep (plan 22.7): boot-time + daily, no-op when
    // ONPREM_CONVERSATION_RETENTION_DAYS=0 (default: keep forever).
    memory::spawn_retention_sweep(db.clone(), config.clone());

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
                foundry::routes::pull_model,
                foundry::routes::set_router,
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
            rocket::routes![rag::routes::route, rag::routes::search, rag::routes::chat,],
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
            ],
        )
        .mount("/", rocket::routes![agents::routes::agent])
}
