//! Authentication: user model, password hashing, JWT issue/verify, admin seed,
//! the `AuthUser` request guard, and the `/auth/*` routes.

pub mod audit;
pub mod guard;
pub mod jwt;
pub mod password;
pub mod routes;
pub mod seed;
pub mod throttle;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// Serde bridge for the `User` datetime fields.
///
/// `chrono`'s serde impl reads/writes `DateTime<Utc>` as an RFC 3339 **string**, but
/// the `$set` updates in the auth routes write timestamps via `bson::DateTime::now()`,
/// which lands as a **native BSON datetime**. Reading such a doc back through chrono's
/// string deserializer fails with "invalid type: map, expected an RFC 3339 ... string"
/// and 500s the request (and every later read of that user, e.g. the admin list).
///
/// This module makes reads accept *both* representations and makes every write emit a
/// native BSON datetime, so the collection converges on one type and pre-existing
/// string-typed documents still deserialize cleanly.
mod flexible_datetime {
    use chrono::{DateTime, Utc};
    use mongodb::bson::{Bson, DateTime as BsonDateTime};
    use serde::{Deserialize, Deserializer, Serialize, Serializer};

    pub fn serialize<S: Serializer>(dt: &DateTime<Utc>, s: S) -> Result<S::Ok, S::Error> {
        // Go through SystemTime: the `bson` crate's chrono feature isn't enabled, so
        // `from_chrono` is unavailable, but the SystemTime bridge always is.
        BsonDateTime::from_system_time((*dt).into()).serialize(s)
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<DateTime<Utc>, D::Error> {
        match Bson::deserialize(d)? {
            Bson::DateTime(dt) => Ok(DateTime::<Utc>::from(dt.to_system_time())),
            Bson::String(s) => s.parse::<DateTime<Utc>>().map_err(serde::de::Error::custom),
            other => Err(serde::de::Error::custom(format!(
                "expected a BSON datetime or RFC 3339 string, got {other:?}"
            ))),
        }
    }
}

/// Access role. Four tiers: `admin` manages the system; `doctor` and `nurse` are
/// clinical staff with read/write access; `analyst` is read-only for reporting.
/// Serialises as lowercase strings.
///
/// Backward-compat: documents written before the four-role scheme stored `"user"`.
/// The `#[serde(alias = "user")]` on `Doctor` lets those documents deserialise
/// cleanly as `Doctor` (the least-restricted non-admin tier) without a one-time DB
/// migration. Newly written documents will always contain `"doctor"`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    Admin,
    /// Alias `"user"` accepts legacy documents written before the four-role scheme.
    #[serde(alias = "user")]
    Doctor,
    Nurse,
    Analyst,
}

/// A user document as stored in DocumentDB (`users` collection).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct User {
    #[serde(rename = "_id")]
    pub id: String,
    pub username: String,
    pub password_hash: String,
    pub role: Role,
    #[serde(with = "flexible_datetime")]
    pub created_at: DateTime<Utc>,
    /// Unique email address. `#[serde(default)]` lets pre-migration docs (missing
    /// the field) deserialise to an empty string; the boot backfill in `seed.rs`
    /// derives a value from the username so the unique index won't collide on `""`.
    #[serde(default)]
    pub email: String,
    /// Display name for the UI. Defaults to empty for pre-migration documents.
    #[serde(default)]
    pub name: String,
    /// Last-modified timestamp. Defaults to `Utc::now()` when the field is absent
    /// in old docs; the persisted value is not updated until the doc is next written.
    #[serde(default = "default_now", with = "flexible_datetime")]
    pub updated_at: DateTime<Utc>,
    /// Session-revocation counter. Each issued token embeds the value current at
    /// issue time (the `tv` claim); the `AuthUser` guard rejects any token whose
    /// `tv` differs from the stored value. Bumping it (logout, admin force-logout,
    /// password or role change) invalidates every token issued before the bump.
    /// `#[serde(default)]` lets pre-migration docs deserialise as `0`.
    #[serde(default)]
    pub token_version: i64,
}

fn default_now() -> DateTime<Utc> {
    Utc::now()
}

/// Public view of a user (no password hash), returned to clients.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UserInfo {
    pub id: String,
    pub username: String,
    pub role: Role,
    pub email: String,
    pub name: String,
    /// When the account was created. Backs the Admin "Created" column and the
    /// Profile "Member since" display. Flows through `login` + `me` automatically.
    pub created_at: DateTime<Utc>,
}

impl From<&User> for UserInfo {
    fn from(u: &User) -> Self {
        UserInfo {
            id: u.id.clone(),
            username: u.username.clone(),
            role: u.role,
            email: u.email.clone(),
            name: u.name.clone(),
            created_at: u.created_at,
        }
    }
}
