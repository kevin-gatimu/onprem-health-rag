//! Liveness / readiness endpoint.

use crate::state::AppState;
use rocket::http::Status;
use rocket::response::status;
use rocket::serde::json::{Json, Value, json};
use rocket::{State, get};

/// `GET /health` — always 200 while the process is up. Reports DocumentDB
/// reachability as a nested field so the app can show a degraded state without
/// the whole request failing.
#[get("/health")]
pub async fn health(state: &State<AppState>) -> Json<Value> {
    let db_ok = state.db.ping().await.is_ok();
    Json(json!({
        "status": "ok",
        "service": "onprem-rag-server",
        "version": env!("CARGO_PKG_VERSION"),
        "documentdb": if db_ok { "up" } else { "down" },
    }))
}

/// `GET /ready` — dependency and capacity readiness without exposing configuration secrets.
#[get("/ready")]
pub async fn ready(state: &State<AppState>) -> status::Custom<Json<Value>> {
    let db_ok = state.db.ping().await.is_ok();
    let indexes_ok = if db_ok {
        crate::documentdb::vector::required_indexes_ready(&state.db)
            .await
            .unwrap_or(false)
    } else {
        false
    };
    let foundry_ok = state.foundry_available();
    let warmup = state.warmup_status();
    let warmup_ok = matches!(warmup, "ready" | "disabled");
    let (generation, retrieval, ingestion) = state.admission.available_capacity();
    let capacity_ok = generation > 0 && retrieval > 0 && ingestion > 0;
    let is_ready = db_ok && indexes_ok && foundry_ok && warmup_ok && capacity_ok;

    status::Custom(
        if is_ready {
            Status::Ok
        } else {
            Status::ServiceUnavailable
        },
        Json(json!({
            "status": if is_ready { "ready" } else { "not_ready" },
            "service": "onprem-rag-server",
            "version": env!("CARGO_PKG_VERSION"),
            "checks": {
                "documentdb": db_ok,
                "required_indexes": indexes_ok,
                "foundry": foundry_ok,
                "warmup": warmup,
                "capacity": {
                    "available": capacity_ok,
                    "generation": generation,
                    "retrieval": retrieval,
                    "ingestion": ingestion,
                }
            }
        })),
    )
}
