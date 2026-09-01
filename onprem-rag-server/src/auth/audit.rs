//! Audit log: records security-relevant events to the `audit_log` collection.
//! All writes are best-effort — a failure emits a warning and never propagates
//! to the caller's request path so the user-visible operation always completes.

use crate::documentdb::{AUDIT_LOG, DocumentDb};
use mongodb::bson::DateTime;
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// One audit-log entry persisted to the `audit_log` collection.
#[derive(Debug, Serialize, Deserialize)]
pub struct AuditEntry {
    pub user_id: String,
    pub username: String,
    /// Short action tag, e.g. `"login"`, `"login_failed"`, `"logout"`.
    pub action: String,
    /// What the action targeted — typically a username, resource id, or endpoint.
    pub resource: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub details: Option<Value>,
    pub timestamp: DateTime,
}

/// Insert one audit entry into `audit_log`. Best-effort: on failure a warning
/// is logged and the error is swallowed so the caller's request is unaffected.
pub async fn write_audit(
    db: &DocumentDb,
    user_id: impl Into<String>,
    username: impl Into<String>,
    action: impl Into<String>,
    resource: impl Into<String>,
    details: Option<Value>,
) {
    let entry = AuditEntry {
        user_id: user_id.into(),
        username: username.into(),
        action: action.into(),
        resource: resource.into(),
        details,
        timestamp: DateTime::now(),
    };
    let col = db.collection::<AuditEntry>(AUDIT_LOG);
    if let Err(e) = col.insert_one(entry).await {
        tracing::warn!(error = %e, "audit log write failed (non-fatal)");
    }
}
