//! Persisted server-wide settings, currently just per-role model routing overrides.
//!
//! Stored as a single document (`_id: "app_settings"`) in the `settings` collection with a
//! `router_overrides` sub-document mapping role key -> variant id. This lets an admin's
//! "Load" choice in Settings survive a server restart and win over the `ONPREM_MODEL_*`
//! env defaults, without needing a collection-per-role.

use std::collections::HashMap;

use mongodb::bson::{DateTime, doc};

use crate::documentdb::DocumentDb;
use crate::error::AppResult;

pub const SETTINGS_DOC_ID: &str = "app_settings";

/// Load persisted router overrides (role key -> variant id). Empty if the doc is absent.
pub async fn load_router_overrides(db: &DocumentDb) -> AppResult<HashMap<String, String>> {
    let mut map = HashMap::new();
    if let Some(d) = db
        .settings()
        .find_one(doc! { "_id": SETTINGS_DOC_ID })
        .await?
    {
        if let Ok(ov) = d.get_document("router_overrides") {
            for (k, v) in ov.iter() {
                if let Some(s) = v.as_str() {
                    map.insert(k.clone(), s.to_string());
                }
            }
        }
    }
    Ok(map)
}

/// Upsert (Some) or clear (None) one role's override. Uses a dotted field path so other
/// roles' overrides in the same document are left untouched.
pub async fn set_router_override(
    db: &DocumentDb,
    role: &str,
    variant_id: Option<&str>,
    updated_by: &str,
) -> AppResult<()> {
    let field = format!("router_overrides.{role}");
    let update = match variant_id {
        Some(v) => {
            doc! { "$set": { field: v, "updated_at": DateTime::now(), "updated_by": updated_by } }
        }
        None => {
            doc! { "$unset": { field: "" }, "$set": { "updated_at": DateTime::now(), "updated_by": updated_by } }
        }
    };
    db.settings()
        .update_one(doc! { "_id": SETTINGS_DOC_ID }, update)
        .upsert(true)
        .await?;
    Ok(())
}

/// Clear every role override that points at `variant_id`, returning the roles cleared.
///
/// Called when a variant's weights are deleted from the Foundry cache: an override left
/// naming a missing variant would silently re-download several GB on that role's next
/// call, so cache and settings are cleaned together.
pub async fn clear_router_overrides_for_variant(
    db: &DocumentDb,
    variant_id: &str,
    updated_by: &str,
) -> AppResult<Vec<String>> {
    let roles: Vec<String> = load_router_overrides(db)
        .await?
        .into_iter()
        .filter(|(_, v)| v == variant_id)
        .map(|(role, _)| role)
        .collect();
    for role in &roles {
        set_router_override(db, role, None, updated_by).await?;
    }
    Ok(roles)
}
