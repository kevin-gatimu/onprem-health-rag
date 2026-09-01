//! Seed the default admin account on boot if the users collection is empty of it.
//!
//! Also runs a one-time email backfill so the collection is ready for the unique
//! email index that `documentdb::ensure_user_indexes` creates afterwards:
//!   - Any user with a missing or empty `email` gets `<username>@localhost`.
//!
//! Role migration note: old `"user"` role values are left unchanged in the DB.
//! `#[serde(alias = "user")]` on `Role::Doctor` handles them transparently at read
//! time, so no write pass is needed for correctness.

use crate::auth::{Role, User, password::hash_password};
use crate::config::Config;
use crate::documentdb::{DocumentDb, USERS};
use crate::error::AppResult;
use chrono::Utc;
use mongodb::bson::doc;

/// Ensure an admin user exists. Idempotent: only inserts when the configured
/// admin username is absent, so restarts and password rotations elsewhere are safe.
pub async fn seed_admin(db: &DocumentDb, config: &Config) -> AppResult<()> {
    // --- Email backfill ----------------------------------------------------------
    // Must run before `ensure_user_indexes` creates the unique email index.
    // Uses an aggregation-pipeline update so a single round trip handles all rows.
    match db
        .db
        .run_command(doc! {
            "update": USERS,
            "updates": [{
                "q": { "$or": [{ "email": { "$exists": false } }, { "email": "" }] },
                "u": [{ "$set": { "email": { "$concat": ["$username", "@localhost"] } } }],
                "multi": true
            }]
        })
        .await
    {
        Ok(r) => {
            // `nModified` is i32 in the wire protocol; fall back to i64 defensively.
            let n = r
                .get_i32("nModified")
                .map(|v| v as i64)
                .or_else(|_| r.get_i64("nModified"))
                .unwrap_or(0);
            if n > 0 {
                tracing::info!(count = n, "backfilled email for existing users");
            }
        }
        Err(e) => tracing::warn!(error = %e, "email backfill failed (non-fatal)"),
    }

    // --- Admin seed --------------------------------------------------------------
    let users = db.collection::<User>(USERS);
    let existing = users
        .find_one(doc! { "username": &config.admin_username })
        .await?;
    if existing.is_some() {
        tracing::debug!(user = %config.admin_username, "admin already present; skipping seed");
        return Ok(());
    }

    let now = Utc::now();
    let user = User {
        id: uuid::Uuid::now_v7().to_string(),
        username: config.admin_username.clone(),
        password_hash: hash_password(&config.admin_password)?,
        role: Role::Admin,
        created_at: now,
        email: format!("{}@localhost", config.admin_username),
        name: "Administrator".to_string(),
        updated_at: now,
        token_version: 0,
    };
    users.insert_one(&user).await?;
    tracing::info!(user = %config.admin_username, "seeded default admin account");
    Ok(())
}
