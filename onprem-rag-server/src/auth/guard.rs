//! `AuthUser` request guard — extracts and verifies the Bearer token, making the
//! authenticated identity available to any route that names it as a parameter.

use crate::auth::{Role, User, jwt};
use crate::documentdb::USERS;
use crate::error::AppError;
use crate::state::AppState;
use mongodb::bson::doc;
use rocket::request::{FromRequest, Outcome, Request};

/// An authenticated caller, derived from a verified JWT. A route parameter of this
/// type makes the route require a valid token.
#[derive(Debug, Clone)]
pub struct AuthUser {
    pub id: String,
    pub username: String,
    pub role: Role,
}

impl AuthUser {
    /// Require admin role, or return `Forbidden`.
    pub fn require_admin(&self) -> Result<(), AppError> {
        if self.role == Role::Admin {
            Ok(())
        } else {
            Err(AppError::Forbidden)
        }
    }
}

#[rocket::async_trait]
impl<'r> FromRequest<'r> for AuthUser {
    type Error = AppError;

    async fn from_request(req: &'r Request<'_>) -> Outcome<Self, Self::Error> {
        let header = match req.headers().get_one("Authorization") {
            Some(h) => h,
            None => return Outcome::Error((rocket::http::Status::Unauthorized, AppError::Unauthorized)),
        };
        let token = match header.strip_prefix("Bearer ").or_else(|| header.strip_prefix("bearer ")) {
            Some(t) => t.trim(),
            None => return Outcome::Error((rocket::http::Status::Unauthorized, AppError::Unauthorized)),
        };

        let state = match req.rocket().state::<AppState>() {
            Some(s) => s,
            None => {
                return Outcome::Error((
                    rocket::http::Status::InternalServerError,
                    AppError::Internal("missing app state".into()),
                ));
            }
        };

        let claims = match jwt::verify(&state.config, token) {
            Ok(c) => c,
            Err(_) => {
                return Outcome::Error((rocket::http::Status::Unauthorized, AppError::Unauthorized));
            }
        };

        // Session revocation: the token's `tv` must equal the user's current
        // token_version. A mismatch (or a missing user) means the token was
        // invalidated — logout, admin force-logout, or a password/role change —
        // so reject it even though the signature and expiry are still valid.
        // One indexed `_id` lookup per authenticated request (near-stateless).
        let users = state.db.collection::<User>(USERS);
        match users.find_one(doc! { "_id": &claims.sub }).await {
            Ok(Some(u)) if u.token_version == claims.tv => Outcome::Success(AuthUser {
                id: claims.sub,
                username: claims.username,
                role: claims.role,
            }),
            Ok(_) => Outcome::Error((rocket::http::Status::Unauthorized, AppError::Unauthorized)),
            Err(e) => Outcome::Error((
                rocket::http::Status::InternalServerError,
                AppError::Internal(format!("auth lookup failed: {e}")),
            )),
        }
    }
}
