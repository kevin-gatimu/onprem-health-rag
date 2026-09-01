//! Admin-only, PHI-free process metrics summary.

use rocket::get;
use rocket::serde::json::Json;

use crate::auth::guard::AuthUser;
use crate::error::AppResult;
use crate::telemetry::{MetricsSummary, metrics_summary};

#[get("/metrics/summary")]
pub fn summary(admin: AuthUser) -> AppResult<Json<MetricsSummary>> {
    admin.require_admin()?;
    Ok(Json(metrics_summary()))
}
