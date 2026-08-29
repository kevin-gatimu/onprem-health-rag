//! JWT issue/verify (HS256) over the configured secret.

use crate::auth::{Role, User};
use crate::config::Config;
use crate::error::{AppError, AppResult};
use chrono::Utc;
use jsonwebtoken::{DecodingKey, EncodingKey, Header, Validation, decode, encode};
use serde::{Deserialize, Serialize};

/// JWT claims. `sub` is the user id; role is embedded so guards avoid a DB hit.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Claims {
    pub sub: String,
    pub username: String,
    pub role: Role,
    /// Session-revocation counter, copied from the user's `token_version` at issue
    /// time. The guard rejects the token when it no longer matches the DB value.
    /// `#[serde(default)]` accepts tokens issued before this field existed.
    #[serde(default)]
    pub tv: i64,
    pub iat: i64,
    pub exp: i64,
}

/// Issue a signed token for a user, expiring `jwt_ttl_hours` from now.
pub fn issue(config: &Config, user: &User) -> AppResult<String> {
    let now = Utc::now();
    let exp = now + chrono::Duration::hours(config.jwt_ttl_hours);
    let claims = Claims {
        sub: user.id.clone(),
        username: user.username.clone(),
        role: user.role,
        tv: user.token_version,
        iat: now.timestamp(),
        exp: exp.timestamp(),
    };
    encode(
        &Header::default(),
        &claims,
        &EncodingKey::from_secret(config.jwt_secret.as_bytes()),
    )
    .map_err(|e| AppError::Internal(format!("token signing failed: {e}")))
}

/// Verify a token's signature + expiry and return its claims.
pub fn verify(config: &Config, token: &str) -> AppResult<Claims> {
    let data = decode::<Claims>(
        token,
        &DecodingKey::from_secret(config.jwt_secret.as_bytes()),
        &Validation::default(),
    )
    .map_err(|_| AppError::Unauthorized)?;
    Ok(data.claims)
}
