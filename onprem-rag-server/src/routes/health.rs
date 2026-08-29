//! Liveness / readiness endpoint.

use crate::state::AppState;
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
