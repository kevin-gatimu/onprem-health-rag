//! `/auth/*` routes: login, current-user, admin-only user creation, and the new
//! profile-management + admin user-management routes (Stage 9).

use crate::auth::{Role, User, UserInfo, audit, guard::AuthUser, jwt, password};
use crate::documentdb::USERS;
use crate::error::{AppError, AppResult};
use crate::state::AppState;
use chrono::Utc;
use futures::TryStreamExt;
use mongodb::bson::{DateTime as BsonDateTime, Document, doc};
use rocket::serde::json::Json;
use rocket::{State, delete, get, patch, post};
use serde::{Deserialize, Serialize};

#[derive(Debug, Deserialize)]
pub struct LoginRequest {
    /// Accepts either an email address or a username. The field was previously called
    /// `username`; the alias keeps legacy callers (e.g. the Tauri bridge) working.
    #[serde(alias = "username")]
    pub identifier: String,
    pub password: String,
}

#[derive(Debug, Serialize)]
pub struct LoginResponse {
    pub token: String,
    pub user: UserInfo,
}

/// `POST /auth/login` — exchange credentials for a JWT. Public (no guard).
/// Accepts either an email address or a username as `identifier` (or the legacy
/// field name `username` for backward-compat with existing callers).
#[post("/auth/login", data = "<body>")]
pub async fn login(
    body: Json<LoginRequest>,
    state: &State<AppState>,
    remote: Option<std::net::SocketAddr>,
) -> AppResult<Json<LoginResponse>> {
    let ip_key = remote.map(|s| s.ip().to_string()).unwrap_or_else(|| "unknown".to_string());
    // Reject early if this client IP is currently locked out for too many failures.
    if let Some(retry) = state.login_throttle.check(&ip_key) {
        return Err(AppError::TooManyRequests(format!(
            "too many failed login attempts; try again in {}s", retry.as_secs().max(1)
        )));
    }

    let users = state.db.collection::<User>(USERS);
    let id = &body.identifier;
    // Match on either the stored email or the username so users can log in either way.
    let user = users
        .find_one(doc! { "$or": [{ "email": id }, { "username": id }] })
        .await?;

    let user = match user {
        Some(u) => u,
        None => {
            // Constant-time path: always run argon2 so "no such user" takes the same
            // time as "wrong password", defeating timing-based user enumeration.
            password::dummy_verify(&body.password);
            state.login_throttle.record_failure(&ip_key);
            // Record the failed attempt before returning. user_id is unknown here.
            audit::write_audit(&state.db, "", "", "login_failed", id.as_str(), None).await;
            return Err(AppError::Unauthorized);
        }
    };

    if !password::verify_password(&body.password, &user.password_hash) {
        state.login_throttle.record_failure(&ip_key);
        audit::write_audit(
            &state.db, &user.id, &user.username, "login_failed", &user.username, None,
        ).await;
        return Err(AppError::Unauthorized);
    }

    state.login_throttle.record_success(&ip_key);
    let token = jwt::issue(&state.config, &user)?;
    audit::write_audit(&state.db, &user.id, &user.username, "login", &user.username, None).await;
    Ok(Json(LoginResponse { token, user: UserInfo::from(&user) }))
}

/// `GET /auth/me` — the caller's own identity (requires a valid token).
/// Does a DB lookup to return the latest profile data including email and name.
#[get("/auth/me")]
pub async fn me(user: AuthUser, state: &State<AppState>) -> AppResult<Json<UserInfo>> {
    let users = state.db.collection::<User>(USERS);
    let u = users
        .find_one(doc! { "_id": &user.id })
        .await?
        .ok_or(AppError::NotFound)?;
    Ok(Json(UserInfo::from(&u)))
}

#[derive(Debug, Deserialize)]
pub struct CreateUserRequest {
    pub username: String,
    pub password: String,
    #[serde(default)]
    pub email: Option<String>,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub role: Option<Role>,
}

/// `POST /auth/users` — create a user (admin only).
#[post("/auth/users", data = "<body>")]
pub async fn create_user(
    admin: AuthUser,
    body: Json<CreateUserRequest>,
    state: &State<AppState>,
) -> AppResult<Json<UserInfo>> {
    admin.require_admin()?;

    let username = body.username.trim();
    if username.is_empty() || body.password.is_empty() {
        return Err(AppError::BadRequest("username and password are required".into()));
    }

    let users = state.db.collection::<User>(USERS);
    if users.find_one(doc! { "username": username }).await?.is_some() {
        return Err(AppError::BadRequest(format!("user '{username}' already exists")));
    }

    // Default email from username when not provided; default name to empty.
    let email = body.email.as_deref().filter(|s| !s.is_empty())
        .map(|s| s.to_string())
        .unwrap_or_else(|| format!("{username}@localhost"));
    let name = body.name.as_deref().unwrap_or("").to_string();

    let now = Utc::now();
    let user = User {
        id: uuid::Uuid::now_v7().to_string(),
        username: username.to_string(),
        password_hash: password::hash_password(&body.password)?,
        // Default to `Doctor` (least-restricted non-admin tier) when no role given.
        role: body.role.unwrap_or(Role::Doctor),
        created_at: now,
        email,
        name,
        updated_at: now,
        token_version: 0,
    };
    users.insert_one(&user).await?;
    audit::write_audit(
        &state.db, &admin.id, &admin.username, "user_created", &user.id,
        Some(serde_json::json!({ "username": user.username, "role": user.role })),
    ).await;
    Ok(Json(UserInfo::from(&user)))
}

// ── Stage 9: profile management + admin user management ─────────────────────

/// `GET /auth/users` — list all users sorted by creation time (admin only).
#[get("/auth/users")]
pub async fn list_users(admin: AuthUser, state: &State<AppState>) -> AppResult<Json<Vec<UserInfo>>> {
    admin.require_admin()?;

    let users = state.db.collection::<User>(USERS);
    let all: Vec<User> = users
        .find(doc! {})
        .sort(doc! { "created_at": 1 })
        .await?
        .try_collect()
        .await?;

    Ok(Json(all.iter().map(UserInfo::from).collect()))
}

/// Patch body for admin editing a user's name, email, and/or role.
#[derive(Debug, Deserialize)]
pub struct UpdateUserRequest {
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub email: Option<String>,
    #[serde(default)]
    pub role: Option<Role>,
}

/// `PATCH /auth/users/<id>` — admin updates a user's name, email, and/or role.
#[patch("/auth/users/<id>", data = "<body>")]
pub async fn update_user(
    admin: AuthUser,
    id: &str,
    body: Json<UpdateUserRequest>,
    state: &State<AppState>,
) -> AppResult<Json<UserInfo>> {
    admin.require_admin()?;

    let users = state.db.collection::<User>(USERS);

    // Load target; 404 if absent.
    let target = users.find_one(doc! { "_id": id }).await?.ok_or(AppError::NotFound)?;

    // Email-conflict check: normalize, then reject if another user has it.
    if let Some(ref email_raw) = body.email {
        let new_email = email_raw.trim().to_lowercase();
        if !new_email.is_empty() && new_email != target.email {
            let conflict = users
                .find_one(doc! { "email": &new_email, "_id": { "$ne": id } })
                .await?;
            if conflict.is_some() {
                return Err(AppError::BadRequest("that email is already in use".into()));
            }
        }
    }

    // Last-admin guard: prevent demoting the only admin to a non-admin role.
    if target.role == Role::Admin {
        if let Some(new_role) = body.role {
            if new_role != Role::Admin {
                let count = users.count_documents(doc! { "role": "admin" }).await?;
                if count <= 1 {
                    return Err(AppError::BadRequest("cannot demote the last admin".into()));
                }
            }
        }
    }

    // Build the $set document from whichever fields were provided.
    let mut set_doc = Document::new();
    if let Some(ref name_raw) = body.name {
        let name = name_raw.trim();
        if !name.is_empty() {
            set_doc.insert("name", name);
        }
    }
    if let Some(ref email_raw) = body.email {
        let new_email = email_raw.trim().to_lowercase();
        if !new_email.is_empty() {
            set_doc.insert("email", new_email);
        }
    }
    if let Some(role) = body.role {
        // Serialize the role enum to its lowercase string value for BSON storage.
        let role_bson = mongodb::bson::to_bson(&role)
            .map_err(|e| AppError::Internal(format!("role serialization: {e}")))?;
        set_doc.insert("role", role_bson);
    }
    set_doc.insert("updated_at", BsonDateTime::now());

    // If the role actually changes, bump token_version so the user's existing
    // tokens (which carry the old role) are rejected — the new role takes effect
    // immediately instead of lingering until the old token expires.
    let role_changed = matches!(body.role, Some(r) if r != target.role);
    let mut update = doc! { "$set": set_doc };
    if role_changed {
        update.insert("$inc", doc! { "token_version": 1i64 });
    }
    users.update_one(doc! { "_id": id }, update).await?;

    // Re-read to return data that reflects what was actually persisted.
    let updated = users.find_one(doc! { "_id": id }).await?.ok_or(AppError::NotFound)?;
    let info = UserInfo::from(&updated);

    audit::write_audit(&state.db, &admin.id, &admin.username, "user_updated", id, None).await;
    Ok(Json(info))
}

/// `DELETE /auth/users/<id>` — admin deletes a user (admin only).
///
/// Note: chat conversations owned by the deleted user are left as orphaned
/// documents — no cascade in this stage (harmless for PHI, avoids complexity).
#[delete("/auth/users/<id>")]
pub async fn delete_user(
    admin: AuthUser,
    id: &str,
    state: &State<AppState>,
) -> AppResult<Json<serde_json::Value>> {
    admin.require_admin()?;

    // Self-delete guard: an admin cannot remove their own account.
    if id == admin.id {
        return Err(AppError::BadRequest("you cannot delete your own account".into()));
    }

    let users = state.db.collection::<User>(USERS);

    // Load target; 404 if absent.
    let target = users.find_one(doc! { "_id": id }).await?.ok_or(AppError::NotFound)?;

    // Last-admin guard: cannot delete the only admin.
    if target.role == Role::Admin {
        let count = users.count_documents(doc! { "role": "admin" }).await?;
        if count <= 1 {
            return Err(AppError::BadRequest("cannot delete the last admin".into()));
        }
    }

    users.delete_one(doc! { "_id": id }).await?;

    // Record who was deleted; helpful for reconstructing the audit trail.
    audit::write_audit(
        &state.db, &admin.id, &admin.username, "user_deleted", id,
        Some(serde_json::json!({ "username": target.username })),
    ).await;

    Ok(Json(serde_json::json!({ "ok": true })))
}

/// Request body for admin setting a user's password.
#[derive(Debug, Deserialize)]
pub struct SetPasswordRequest {
    pub new_password: String,
}

/// `POST /auth/users/<id>/password` — admin sets a user's password without
/// requiring the current password (admin path).
#[post("/auth/users/<id>/password", data = "<body>")]
pub async fn set_user_password(
    admin: AuthUser,
    id: &str,
    body: Json<SetPasswordRequest>,
    state: &State<AppState>,
) -> AppResult<Json<serde_json::Value>> {
    admin.require_admin()?;

    let users = state.db.collection::<User>(USERS);

    // 404 if target absent — confirm the user exists before hashing.
    let _ = users.find_one(doc! { "_id": id }).await?.ok_or(AppError::NotFound)?;

    if body.new_password.is_empty() {
        return Err(AppError::BadRequest("new_password cannot be empty".into()));
    }

    let hash = password::hash_password(&body.new_password)?;
    users
        .update_one(
            doc! { "_id": id },
            doc! { "$set": { "password_hash": &hash, "updated_at": BsonDateTime::now() }, "$inc": { "token_version": 1i64 } },
        )
        .await?;

    audit::write_audit(&state.db, &admin.id, &admin.username, "password_set", id, None).await;
    Ok(Json(serde_json::json!({ "ok": true })))
}

/// Request body for self-service name update.
#[derive(Debug, Deserialize)]
pub struct UpdateMeRequest {
    pub name: String,
}

/// `PATCH /auth/me` — authenticated user updates their own display name.
/// Email and role are admin-assigned; self-service is name only.
#[patch("/auth/me", data = "<body>")]
pub async fn update_me(
    user: AuthUser,
    body: Json<UpdateMeRequest>,
    state: &State<AppState>,
) -> AppResult<Json<UserInfo>> {
    let name = body.name.trim();
    if name.is_empty() {
        return Err(AppError::BadRequest("name cannot be empty".into()));
    }

    let users = state.db.collection::<User>(USERS);
    users
        .update_one(
            doc! { "_id": &user.id },
            doc! { "$set": { "name": name, "updated_at": BsonDateTime::now() } },
        )
        .await?;

    // Re-read so the response reflects the persisted state.
    let updated = users.find_one(doc! { "_id": &user.id }).await?.ok_or(AppError::NotFound)?;
    let info = UserInfo::from(&updated);

    audit::write_audit(&state.db, &user.id, &user.username, "profile_updated", &user.id, None).await;
    Ok(Json(info))
}

/// Request body for self-service password change.
#[derive(Debug, Deserialize)]
pub struct ChangePasswordRequest {
    pub current_password: String,
    pub new_password: String,
}

/// `POST /auth/me/password` — authenticated user changes their own password.
/// Requires the current password (unlike the admin `/auth/users/<id>/password` path).
#[post("/auth/me/password", data = "<body>")]
pub async fn change_my_password(
    user: AuthUser,
    body: Json<ChangePasswordRequest>,
    state: &State<AppState>,
) -> AppResult<Json<serde_json::Value>> {
    let users = state.db.collection::<User>(USERS);

    // Load self to get the current password hash.
    let self_user = users.find_one(doc! { "_id": &user.id }).await?.ok_or(AppError::NotFound)?;

    // Verify current password; 401 so the client can show "Current password is incorrect".
    if !password::verify_password(&body.current_password, &self_user.password_hash) {
        return Err(AppError::Unauthorized);
    }

    if body.new_password.is_empty() {
        return Err(AppError::BadRequest("new_password cannot be empty".into()));
    }

    let hash = password::hash_password(&body.new_password)?;
    users
        .update_one(
            doc! { "_id": &user.id },
            doc! { "$set": { "password_hash": &hash, "updated_at": BsonDateTime::now() }, "$inc": { "token_version": 1i64 } },
        )
        .await?;

    audit::write_audit(&state.db, &user.id, &user.username, "password_changed", &user.id, None).await;
    Ok(Json(serde_json::json!({ "ok": true })))
}

/// `POST /auth/logout` — invalidate the caller's own outstanding tokens by
/// bumping their token_version. The current token fails the guard's version
/// check on its next request (immediate server-side logout, independent of expiry).
#[post("/auth/logout")]
pub async fn logout(user: AuthUser, state: &State<AppState>) -> AppResult<Json<serde_json::Value>> {
    let users = state.db.collection::<User>(USERS);
    users
        .update_one(doc! { "_id": &user.id }, doc! { "$inc": { "token_version": 1i64 } })
        .await?;
    audit::write_audit(&state.db, &user.id, &user.username, "logout", &user.username, None).await;
    Ok(Json(serde_json::json!({ "ok": true })))
}

/// `POST /auth/users/<id>/logout` — admin force-logout: bump a user's
/// token_version so all their outstanding tokens are rejected. Admin only.
#[post("/auth/users/<id>/logout")]
pub async fn force_logout_user(
    admin: AuthUser,
    id: &str,
    state: &State<AppState>,
) -> AppResult<Json<serde_json::Value>> {
    admin.require_admin()?;
    let users = state.db.collection::<User>(USERS);
    let _ = users.find_one(doc! { "_id": id }).await?.ok_or(AppError::NotFound)?;
    users
        .update_one(doc! { "_id": id }, doc! { "$inc": { "token_version": 1i64 } })
        .await?;
    audit::write_audit(&state.db, &admin.id, &admin.username, "force_logout", id, None).await;
    Ok(Json(serde_json::json!({ "ok": true })))
}
