//! Tauri commands — the bridge surface the React app invokes. Each maps to a
//! server REST call; later workstreams add auth, models, sources, ingest, chat.

use std::collections::HashMap;

use crate::state::Bridge;
use eventsource_stream::Eventsource;
use futures_util::{
    future::{AbortHandle, Abortable},
    StreamExt,
};
use serde::{Deserialize, Serialize};
use serde_json::json;
use tauri::{Emitter, State};
use tauri_plugin_store::StoreExt;

/// Sentinel error the frontend recognises as "server rejected our token" (401).
/// The React layer maps it to a forced logout. Keep in sync with `SESSION_EXPIRED`
/// in `src/lib/bridge.ts`.
const SESSION_EXPIRED: &str = "__SESSION_EXPIRED__";

/// Public user identity, mirrored from the server's `UserInfo`. Every field the
/// server adds here must be added in BOTH this struct and `src/lib/bridge.ts`, or
/// it is silently dropped before it reaches the React layer.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UserInfo {
    pub id: String,
    pub username: String,
    /// Lowercase role string: "admin" | "doctor" | "nurse" | "analyst".
    pub role: String,
    pub email: String,
    pub name: String,
    /// RFC3339 creation timestamp (e.g. "2026-08-25T10:00:00Z"). Backs the Admin
    /// "Created" column and the Profile "Member since" display.
    pub created_at: String,
}

#[derive(Debug, Deserialize)]
struct LoginResponse {
    token: String,
    user: UserInfo,
}

/// Return the currently configured server base URL.
#[tauri::command]
pub fn get_server_url(bridge: State<'_, Bridge>) -> String {
    bridge.base_url()
}

/// Point the bridge at a different server. Also persists the URL to the store so
/// it survives an app restart (critical on Android where the OS kills backgrounded apps).
#[tauri::command]
pub fn set_server_url(
    url: String,
    app: tauri::AppHandle,
    bridge: State<'_, Bridge>,
) -> Result<(), String> {
    let trimmed = url.trim();
    if trimmed.is_empty() {
        return Err("server URL must not be empty".into());
    }
    bridge.set_base_url(trimmed.to_string());
    let store = app.store("bridge-store.json").map_err(|e| e.to_string())?;
    store.set("base_url", json!(trimmed));
    store.save().map_err(|e| e.to_string())?;
    Ok(())
}

/// Round-trip the server's `/health` endpoint. Proves the bridge → server path
/// end-to-end (Workstream 1 gate).
#[tauri::command]
pub async fn health(bridge: State<'_, Bridge>) -> Result<serde_json::Value, String> {
    let url = bridge.url("/health");
    let resp = bridge
        .client
        .get(&url)
        .send()
        .await
        .map_err(|e| format!("request failed: {e}"))?;
    resp.json::<serde_json::Value>()
        .await
        .map_err(|e| format!("invalid response: {e}"))
}

/// Log in with credentials. On success the bridge stores the JWT (kept out of the
/// web layer), persists it to disk, and returns the user identity.
#[tauri::command]
pub async fn login(
    username: String,
    password: String,
    app: tauri::AppHandle,
    bridge: State<'_, Bridge>,
) -> Result<UserInfo, String> {
    let url = bridge.url("/auth/login");
    let resp = bridge
        .client
        .post(&url)
        .json(&serde_json::json!({ "username": username, "password": password }))
        .send()
        .await
        .map_err(|e| format!("request failed: {e}"))?;

    if resp.status() == reqwest::StatusCode::UNAUTHORIZED {
        return Err("invalid username or password".into());
    }
    if !resp.status().is_success() {
        return Err(format!("login failed: HTTP {}", resp.status()));
    }

    let body: LoginResponse = resp
        .json()
        .await
        .map_err(|e| format!("invalid response: {e}"))?;
    // Clone the token so we can both store it in the Bridge and persist it to disk.
    bridge.set_token(Some(body.token.clone()));
    let store = app.store("bridge-store.json").map_err(|e| e.to_string())?;
    store.set("token", json!(body.token));
    store.save().map_err(|e| e.to_string())?;
    Ok(body.user)
}

/// Return the current user, using the stored token. Errors if not logged in.
#[tauri::command]
pub async fn me(bridge: State<'_, Bridge>) -> Result<UserInfo, String> {
    let token = bridge.token().ok_or("not logged in")?;
    let url = bridge.url("/auth/me");
    let resp = bridge
        .client
        .get(&url)
        .bearer_auth(token)
        .send()
        .await
        .map_err(|e| format!("request failed: {e}"))?;

    check_auth(&bridge, &resp)?;
    resp.json::<UserInfo>()
        .await
        .map_err(|e| format!("invalid response: {e}"))
}

/// Clear the stored token from both the Bridge and the persistent store.
/// Best-effort server-side revocation: bumps our token_version so this token
/// dies immediately, not just at expiry. Any network failure is ignored — local
/// logout must always succeed.
#[tauri::command]
pub async fn logout(app: tauri::AppHandle, bridge: State<'_, Bridge>) -> Result<(), String> {
    bridge.abort_all_runs();
    // Best-effort server-side revocation: bump our token_version so this token
    // dies immediately, not just at expiry. Any failure is ignored — local
    // logout must always succeed.
    if let Some(token) = bridge.token() {
        let url = bridge.url("/auth/logout");
        let _ = bridge.client.post(&url).bearer_auth(token).send().await;
    }
    bridge.set_token(None);
    let store = app.store("bridge-store.json").map_err(|e| e.to_string())?;
    store.delete("token");
    store.save().map_err(|e| e.to_string())?;
    Ok(())
}

/// Whether the bridge currently holds a token (does not validate it server-side).
#[tauri::command]
pub fn is_authenticated(bridge: State<'_, Bridge>) -> bool {
    bridge.token().is_some()
}

// ---------------------------------------------------------------------------
// Stage 9: Profile + User Management — 7 bearer-auth commands that mirror the
// new /auth/* server routes. All follow the same shape as the existing
// bearer-auth commands above (token via bridge.token(), check_auth + error_body).
// ---------------------------------------------------------------------------

/// `GET /auth/users` — list all users sorted by creation date. Admin only.
#[tauri::command]
pub async fn list_users(bridge: State<'_, Bridge>) -> Result<Vec<UserInfo>, String> {
    let token = bridge.token().ok_or("not logged in")?;
    let url = bridge.url("/auth/users");
    let resp = bridge
        .client
        .get(&url)
        .bearer_auth(token)
        .send()
        .await
        .map_err(|e| format!("request failed: {e}"))?;
    check_auth(&bridge, &resp)?;
    if !resp.status().is_success() {
        return Err(error_body(resp).await);
    }
    resp.json::<Vec<UserInfo>>()
        .await
        .map_err(|e| format!("invalid response: {e}"))
}

/// `POST /auth/users` — create a new user. Admin only.
/// Optional fields (`email`, `name`, `role`) serialise as `null` when absent; the
/// server treats absent == null via `#[serde(default)]`.
#[tauri::command]
pub async fn create_user(
    username: String,
    password: String,
    email: Option<String>,
    name: Option<String>,
    role: Option<String>,
    bridge: State<'_, Bridge>,
) -> Result<UserInfo, String> {
    let token = bridge.token().ok_or("not logged in")?;
    let url = bridge.url("/auth/users");
    let resp = bridge
        .client
        .post(&url)
        .bearer_auth(token)
        .json(&serde_json::json!({
            "username": username,
            "password": password,
            "email": email,
            "name": name,
            "role": role,
        }))
        .send()
        .await
        .map_err(|e| format!("request failed: {e}"))?;
    check_auth(&bridge, &resp)?;
    if !resp.status().is_success() {
        return Err(error_body(resp).await);
    }
    resp.json::<UserInfo>()
        .await
        .map_err(|e| format!("invalid response: {e}"))
}

/// `PATCH /auth/users/<id>` — update a user's name, email, or role. Admin only.
#[tauri::command]
pub async fn update_user(
    id: String,
    name: Option<String>,
    email: Option<String>,
    role: Option<String>,
    bridge: State<'_, Bridge>,
) -> Result<UserInfo, String> {
    let token = bridge.token().ok_or("not logged in")?;
    let url = bridge.url(&format!("/auth/users/{id}"));
    let resp = bridge
        .client
        .patch(&url)
        .bearer_auth(token)
        .json(&serde_json::json!({
            "name": name,
            "email": email,
            "role": role,
        }))
        .send()
        .await
        .map_err(|e| format!("request failed: {e}"))?;
    check_auth(&bridge, &resp)?;
    if !resp.status().is_success() {
        return Err(error_body(resp).await);
    }
    resp.json::<UserInfo>()
        .await
        .map_err(|e| format!("invalid response: {e}"))
}

/// `DELETE /auth/users/<id>` — delete a user. Admin only. Ignores the `{ok:true}` body.
#[tauri::command]
pub async fn delete_user(id: String, bridge: State<'_, Bridge>) -> Result<(), String> {
    let token = bridge.token().ok_or("not logged in")?;
    let url = bridge.url(&format!("/auth/users/{id}"));
    let resp = bridge
        .client
        .delete(&url)
        .bearer_auth(token)
        .send()
        .await
        .map_err(|e| format!("request failed: {e}"))?;
    check_auth(&bridge, &resp)?;
    if !resp.status().is_success() {
        return Err(error_body(resp).await);
    }
    Ok(())
}

/// `POST /auth/users/<id>/password` — admin-set a user's password. Admin only.
/// No current-password check on the admin path.
#[tauri::command]
pub async fn set_user_password(
    id: String,
    new_password: String,
    bridge: State<'_, Bridge>,
) -> Result<(), String> {
    let token = bridge.token().ok_or("not logged in")?;
    let url = bridge.url(&format!("/auth/users/{id}/password"));
    let resp = bridge
        .client
        .post(&url)
        .bearer_auth(token)
        .json(&serde_json::json!({ "new_password": new_password }))
        .send()
        .await
        .map_err(|e| format!("request failed: {e}"))?;
    check_auth(&bridge, &resp)?;
    if !resp.status().is_success() {
        return Err(error_body(resp).await);
    }
    Ok(())
}

/// `PATCH /auth/me` — update own display name. Any authenticated user.
#[tauri::command]
pub async fn update_me(name: String, bridge: State<'_, Bridge>) -> Result<UserInfo, String> {
    let token = bridge.token().ok_or("not logged in")?;
    let url = bridge.url("/auth/me");
    let resp = bridge
        .client
        .patch(&url)
        .bearer_auth(token)
        .json(&serde_json::json!({ "name": name }))
        .send()
        .await
        .map_err(|e| format!("request failed: {e}"))?;
    check_auth(&bridge, &resp)?;
    if !resp.status().is_success() {
        return Err(error_body(resp).await);
    }
    resp.json::<UserInfo>()
        .await
        .map_err(|e| format!("invalid response: {e}"))
}

/// `POST /auth/me/password` — change own password. Any authenticated user.
/// HTTP 401 means the current password was wrong — surfaced as a distinct error
/// message rather than "session expired" so the UI can show it inline. Does NOT
/// clear the stored token (the session itself is still valid).
#[tauri::command]
pub async fn change_my_password(
    current_password: String,
    new_password: String,
    bridge: State<'_, Bridge>,
) -> Result<(), String> {
    let token = bridge.token().ok_or("not logged in")?;
    let url = bridge.url("/auth/me/password");
    let resp = bridge
        .client
        .post(&url)
        .bearer_auth(token)
        .json(&serde_json::json!({
            "current_password": current_password,
            "new_password": new_password,
        }))
        .send()
        .await
        .map_err(|e| format!("request failed: {e}"))?;
    // 401 here means "wrong current password", not an expired session — surface it
    // directly rather than routing through check_auth (which would clear the token).
    if resp.status() == reqwest::StatusCode::UNAUTHORIZED {
        return Err("current password is incorrect".into());
    }
    if !resp.status().is_success() {
        return Err(error_body(resp).await);
    }
    Ok(())
}

/// Dashboard summary. Mirrors the server's `StatsResponse` — any field the server
/// adds must be added here AND in `src/lib/bridge.ts` (`DashboardStats`), or it is
/// silently dropped on the way to the UI.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DashboardStats {
    pub total_records: i64,
    pub total_tables: i64,
    pub last_ingest_at: Option<String>,
    pub active_connections: i64,
    pub pending_alerts: i64,
    pub llm_status: String,
}

/// `GET /stats` — the Dashboard's single summary call. Any authenticated user.
#[tauri::command]
pub async fn get_stats(bridge: State<'_, Bridge>) -> Result<DashboardStats, String> {
    let token = bridge.token().ok_or("not logged in")?;
    let url = bridge.url("/stats");
    let resp = bridge
        .client
        .get(&url)
        .bearer_auth(token)
        .send()
        .await
        .map_err(|e| format!("request failed: {e}"))?;

    check_auth(&bridge, &resp)?;
    resp.json::<DashboardStats>()
        .await
        .map_err(|e| format!("invalid response: {e}"))
}

// ---------------------------------------------------------------------------
// Workstream 3: Foundry Local — hardware, models, streaming test generation.
// Types mirror the server's `HardwareInfo` / `ModelSummary` (serde-compatible).
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExecutionProvider {
    pub name: String,
    pub registered: bool,
    /// Coarse accelerator class: "CPU" | "GPU" | "NPU" | "Other".
    pub device_kind: String,
    /// Friendly label, e.g. "OpenVINO — Intel GPU/NPU".
    pub label: String,
}

/// A physical accelerator detected at the OS level, independent of EP registration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DetectedDevice {
    /// "CPU" | "GPU" | "NPU".
    pub kind: String,
    pub name: String,
    pub vendor: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HardwareInfo {
    pub execution_providers: Vec<ExecutionProvider>,
    /// Physical accelerators detected at the OS level (may be empty).
    pub detected_hardware: Vec<DetectedDevice>,
    pub current_chat_model: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelSummary {
    pub alias: String,
    pub id: String,
    pub capabilities: Option<String>,
    pub input_modalities: Option<String>,
    pub output_modalities: Option<String>,
    pub context_length: Option<u64>,
    pub cached: bool,
    pub loaded: bool,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct SelectModelResponse {
    pub model: String,
    /// True when the server purged + re-downloaded corrupt cached weights en route.
    pub repaired: bool,
}

#[derive(serde::Serialize, serde::Deserialize)]
pub struct EpRegistration {
    pub success: bool,
    pub status: String,
    pub registered: Vec<String>,
    pub failed: Vec<String>,
}

/// A single downloadable/loadable model variant. Mirrors the server's `VariantInfo`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VariantInfo {
    pub id: String,
    pub alias: String,
    pub cached: bool,
    pub loaded: bool,
    pub current: bool,
    pub context_length: Option<u64>,
}

/// One row from the model-role manifest. Mirrors the server's `ModelRole` exactly;
/// all fields must match or serde will silently drop them (known bridge gotcha).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelRole {
    pub role: String,
    pub label: String,
    pub model: String,
    pub engine: String,
    pub device: String,
    pub usage: String,
    /// `Some(true/false)` for Foundry chat roles; `None` for fastembed roles.
    pub thinking: Option<bool>,
    pub status: String,
    /// Whether this role is served by Foundry Local (downloadable/loadable variants).
    pub managed: bool,
    /// Downloadable/loadable variants for this role (empty for non-managed roles).
    pub variants: Vec<VariantInfo>,
    /// The persisted routing override for this role, if one has been saved. `None`
    /// for fastembed roles or when no override is set.
    pub override_variant: Option<String>,
}

/// `GET /hardware` — execution providers + current chat model.
#[tauri::command]
pub async fn get_hardware(bridge: State<'_, Bridge>) -> Result<HardwareInfo, String> {
    let token = bridge.token().ok_or("not logged in")?;
    let url = bridge.url("/hardware");
    let resp = bridge
        .client
        .get(&url)
        .bearer_auth(token)
        .send()
        .await
        .map_err(|e| format!("request failed: {e}"))?;
    check_auth(&bridge, &resp)?;
    if !resp.status().is_success() {
        return Err(error_body(resp).await);
    }
    resp.json::<HardwareInfo>()
        .await
        .map_err(|e| format!("invalid response: {e}"))
}

/// `GET /models` — the Foundry Local catalog with cached/loaded state.
#[tauri::command]
pub async fn list_models(bridge: State<'_, Bridge>) -> Result<Vec<ModelSummary>, String> {
    let token = bridge.token().ok_or("not logged in")?;
    let url = bridge.url("/models");
    let resp = bridge
        .client
        .get(&url)
        .bearer_auth(token)
        .send()
        .await
        .map_err(|e| format!("request failed: {e}"))?;
    check_auth(&bridge, &resp)?;
    if !resp.status().is_success() {
        return Err(error_body(resp).await);
    }
    resp.json::<Vec<ModelSummary>>()
        .await
        .map_err(|e| format!("invalid response: {e}"))
}

/// `POST /models/select` — download (if needed), load, and select a chat model. Admin only.
#[tauri::command]
pub async fn select_model(
    model: String,
    bridge: State<'_, Bridge>,
) -> Result<SelectModelResponse, String> {
    let token = bridge.token().ok_or("not logged in")?;
    let url = bridge.url("/models/select");
    let resp = bridge
        .client
        .post(&url)
        .bearer_auth(token)
        .json(&serde_json::json!({ "model": model }))
        .send()
        .await
        .map_err(|e| format!("request failed: {e}"))?;
    check_auth(&bridge, &resp)?;
    if !resp.status().is_success() {
        return Err(error_body(resp).await);
    }
    let body: SelectModelResponse = resp
        .json()
        .await
        .map_err(|e| format!("invalid response: {e}"))?;
    Ok(body)
}

/// `PUT /settings/router` — persist (or clear, when `variant_id` is `None`) a role's
/// model routing override. Admin only.
#[tauri::command]
pub async fn set_role_model(
    role: String,
    variant_id: Option<String>,
    bridge: State<'_, Bridge>,
) -> Result<(), String> {
    let token = bridge.token().ok_or("not logged in")?;
    let url = bridge.url("/settings/router");
    let resp = bridge
        .client
        .put(&url)
        .bearer_auth(token)
        .json(&serde_json::json!({ "role": role, "variant_id": variant_id }))
        .send()
        .await
        .map_err(|e| format!("request failed: {e}"))?;
    check_auth(&bridge, &resp)?;
    if !resp.status().is_success() {
        return Err(error_body(resp).await);
    }
    Ok(())
}

/// Mirrors the server's `DeleteModelResponse` — roles whose saved override named the
/// deleted variant and was therefore cleared.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeleteModelResult {
    pub cleared_roles: Vec<String>,
}

/// `POST /models/delete` — delete a variant's weights from the server's Foundry model
/// cache and clear any persisted role override that named it. Admin only.
#[tauri::command]
pub async fn delete_model(
    variant_id: String,
    bridge: State<'_, Bridge>,
) -> Result<DeleteModelResult, String> {
    let token = bridge.token().ok_or("not logged in")?;
    let url = bridge.url("/models/delete");
    let resp = bridge
        .client
        .post(&url)
        .bearer_auth(token)
        .json(&serde_json::json!({ "variant_id": variant_id }))
        .send()
        .await
        .map_err(|e| format!("request failed: {e}"))?;
    check_auth(&bridge, &resp)?;
    if !resp.status().is_success() {
        return Err(error_body(resp).await);
    }
    resp.json()
        .await
        .map_err(|e| format!("invalid response: {e}"))
}

/// `POST /hardware/register-eps` — download + register all available execution providers
/// into the server's Foundry core. Admin only.
#[tauri::command]
pub async fn register_eps(bridge: State<'_, Bridge>) -> Result<EpRegistration, String> {
    let token = bridge.token().ok_or("not logged in")?;
    let url = bridge.url("/hardware/register-eps");
    let resp = bridge
        .client
        .post(&url)
        .bearer_auth(token)
        .send()
        .await
        .map_err(|e| format!("request failed: {e}"))?;
    check_auth(&bridge, &resp)?;
    if !resp.status().is_success() {
        return Err(error_body(resp).await);
    }
    resp.json()
        .await
        .map_err(|e| format!("invalid response: {e}"))
}

/// `GET /models/roles` — the roles this deployment uses and which models serve them.
/// Pure config read on the server; works even when Foundry Local is down.
#[tauri::command]
pub async fn model_roles(bridge: State<'_, Bridge>) -> Result<Vec<ModelRole>, String> {
    let token = bridge.token().ok_or("not logged in")?;
    let url = bridge.url("/models/roles");
    let resp = bridge
        .client
        .get(&url)
        .bearer_auth(token)
        .send()
        .await
        .map_err(|e| format!("request failed: {e}"))?;
    check_auth(&bridge, &resp)?;
    if !resp.status().is_success() {
        return Err(error_body(resp).await);
    }
    resp.json::<Vec<ModelRole>>()
        .await
        .map_err(|e| format!("invalid response: {e}"))
}

/// `POST /generate` — stream a test completion. Consumes the server SSE and
/// re-emits each token to the frontend as `chat://token`, ending with
/// `chat://done` (or `chat://error`). Returns once the stream completes.
#[tauri::command]
pub async fn generate(
    prompt: String,
    app: tauri::AppHandle,
    bridge: State<'_, Bridge>,
) -> Result<(), String> {
    let token = bridge.token().ok_or("not logged in")?;
    let url = bridge.url("/generate");
    let resp = bridge
        .client
        .post(&url)
        .bearer_auth(token)
        .json(&serde_json::json!({ "prompt": prompt }))
        .send()
        .await
        .map_err(|e| format!("request failed: {e}"))?;
    check_auth(&bridge, &resp)?;
    if !resp.status().is_success() {
        return Err(error_body(resp).await);
    }

    let mut events = resp.bytes_stream().eventsource();
    while let Some(event) = events.next().await {
        let event = event.map_err(|e| format!("stream error: {e}"))?;
        match event.event.as_str() {
            "token" => {
                let _ = app.emit("chat://token", decode_token(&event.data));
            }
            "error" => {
                let _ = app.emit("chat://error", event.data.clone());
                return Err(event.data);
            }
            "done" => break,
            _ => {}
        }
    }
    let _ = app.emit("chat://done", ());
    Ok(())
}

/// Envelope stamped onto every `model://*` event so the boot-time listeners can
/// route progress to the right variant's store entry (mirror of `ChatEvent`).
#[derive(Serialize, Clone)]
struct ModelEvent {
    variant_id: String,
    data: String,
}

/// `POST /models/pull` — download (with progress) and optionally load a variant.
/// Consumes the server SSE and re-emits each event to the frontend: progress as
/// `model://progress`, status transitions as `model://status`, ending with
/// `model://done` (or `model://error`). Returns once the stream completes.
#[tauri::command]
pub async fn pull_model(
    variant_id: String,
    load: bool,
    app: tauri::AppHandle,
    bridge: State<'_, Bridge>,
) -> Result<(), String> {
    let token = bridge.token().ok_or("not logged in")?;
    let url = bridge.url("/models/pull");
    let resp = bridge
        .client
        .post(&url)
        .bearer_auth(token)
        .json(&serde_json::json!({ "variant_id": variant_id, "load": load }))
        .send()
        .await
        .map_err(|e| format!("request failed: {e}"))?;
    check_auth(&bridge, &resp)?;
    if !resp.status().is_success() {
        return Err(error_body(resp).await);
    }

    let mut events = resp.bytes_stream().eventsource();
    while let Some(event) = events.next().await {
        let event = event.map_err(|e| format!("stream error: {e}"))?;
        match event.event.as_str() {
            "progress" => {
                let _ = app.emit(
                    "model://progress",
                    ModelEvent {
                        variant_id: variant_id.clone(),
                        data: event.data,
                    },
                );
            }
            "status" => {
                let _ = app.emit(
                    "model://status",
                    ModelEvent {
                        variant_id: variant_id.clone(),
                        data: event.data,
                    },
                );
            }
            "error" => {
                let _ = app.emit(
                    "model://error",
                    ModelEvent {
                        variant_id: variant_id.clone(),
                        data: event.data.clone(),
                    },
                );
                return Err(event.data);
            }
            "done" => break,
            _ => {}
        }
    }
    let _ = app.emit(
        "model://done",
        ModelEvent {
            variant_id: variant_id.clone(),
            data: String::new(),
        },
    );
    Ok(())
}

// ---------------------------------------------------------------------------
// Stage 8: Models + Settings — one-call setup status + per-variant unload.
// Types mirror the server's `GpuInfo` / `ServiceStatus` / `SetupStatus` (serde,
// snake_case); `execution_providers` REUSES the shared `ExecutionProvider` mirror
// above so it stays identical to `get_hardware`. Every field must also appear in
// `src/lib/bridge.ts` or serde silently drops it (known bridge gotcha).
// ---------------------------------------------------------------------------

/// GPU presence read at the OS level (independent of Foundry EP registration), so
/// it is populated even when the Foundry core is down.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GpuInfo {
    pub has_gpu: bool,
    pub gpu_name: Option<String>,
}

/// One dependency's health line for the Settings service grid.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ServiceStatus {
    pub name: String,
    /// "ok" | "error" | "unknown".
    pub status: String,
    pub detail: Option<String>,
}

/// One mounted volume on the server host. Mirrors the server's `DiskSpec`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DiskSpec {
    pub mount: String,
    pub total_bytes: u64,
    pub available_bytes: u64,
}

/// Server host machine facts. Mirrors the server's `ServerSpecs`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ServerSpecs {
    pub hostname: String,
    pub os: String,
    pub arch: String,
    pub cpu_model: String,
    pub logical_cores: usize,
    pub physical_cores: Option<usize>,
    pub total_memory_bytes: u64,
    pub disks: Vec<DiskSpec>,
    pub accelerators: Vec<String>,
    pub server_version: String,
}

/// The single payload backing the Settings page. Degraded-safe: when Foundry is
/// down the model/EP lists come back empty and `foundry_endpoint` is `""`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SetupStatus {
    pub gpu: GpuInfo,
    /// `foundry.current_model()` if up, else `""`.
    pub active_chat_model: String,
    /// `"in-process (native SDK)"` when the core is ready, else `""`.
    pub foundry_endpoint: String,
    pub foundry_ready: bool,
    pub services: Vec<ServiceStatus>,
    /// Loaded variant ids (empty when Foundry is down).
    pub loaded_models: Vec<String>,
    /// Cached variant ids (empty when Foundry is down).
    pub cached_models: Vec<String>,
    /// Same shape already mirrored for `get_hardware` (empty when Foundry is down).
    pub execution_providers: Vec<ExecutionProvider>,
    /// Host machine facts (never Foundry-derived, so always populated).
    pub server_specs: ServerSpecs,
}

/// `GET /setup-status` — one call powering the Settings page (GPU, active chat
/// model, Foundry readiness, per-service health, loaded/cached ids, execution
/// providers). Returns 200 even when Foundry is down. Any authenticated user.
#[tauri::command]
pub async fn get_setup_status(bridge: State<'_, Bridge>) -> Result<SetupStatus, String> {
    let token = bridge.token().ok_or("not logged in")?;
    let url = bridge.url("/setup-status");
    let resp = bridge
        .client
        .get(&url)
        .bearer_auth(token)
        .send()
        .await
        .map_err(|e| format!("request failed: {e}"))?;
    check_auth(&bridge, &resp)?;
    if !resp.status().is_success() {
        return Err(error_body(resp).await);
    }
    resp.json::<SetupStatus>()
        .await
        .map_err(|e| format!("invalid response: {e}"))
}

/// `POST /models/unload` — unload a variant from memory without deleting its
/// weights. Idempotent server-side (unloading a not-resident model is a no-op
/// success). Admin only (enforced server-side).
#[tauri::command]
pub async fn unload_model(variant_id: String, bridge: State<'_, Bridge>) -> Result<(), String> {
    let token = bridge.token().ok_or("not logged in")?;
    let url = bridge.url("/models/unload");
    let resp = bridge
        .client
        .post(&url)
        .bearer_auth(token)
        .json(&serde_json::json!({ "variant_id": variant_id }))
        .send()
        .await
        .map_err(|e| format!("request failed: {e}"))?;
    check_auth(&bridge, &resp)?;
    if !resp.status().is_success() {
        return Err(error_body(resp).await);
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Workstream 4: source connectors — list, test, save. Types mirror the server's
// `SourceInfo` / `SourceInput`.
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SourceInfo {
    pub id: String,
    pub name: String,
    pub kind: String,
    pub host: String,
    pub port: u16,
    pub database: String,
    pub username: String,
    pub has_password: bool,
    pub query: Option<String>,
    pub table: Option<String>,
    pub created_at: String,
    /// "connected" | "error" | "disconnected". Reflects the last test outcome.
    pub status: String,
    pub last_connected: Option<String>,
    /// Human-readable failure reason from the last test, if it failed.
    pub error: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SchemaCatalogStatus {
    pub source_id: String,
    pub active_version: String,
    pub schema_hash: String,
    pub captured_at: Option<String>,
    pub table_count: i64,
    pub status: String,
    pub health: String,
    pub last_check_at: Option<String>,
    pub last_success_at: Option<String>,
    pub drift_detected: bool,
    pub consecutive_failures: i64,
    pub last_error: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SchemaCatalogRefresh {
    pub source_id: String,
    pub tables_indexed: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SchemaCatalogHistoryItem {
    pub checked_at: Option<String>,
    pub trigger: String,
    pub outcome: String,
    pub previous_hash: Option<String>,
    pub observed_hash: Option<String>,
    pub table_count: i64,
    pub error: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MetadataAlias {
    pub table: String,
    pub column: Option<String>,
    pub alias: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MetadataRelationship {
    pub from_table: String,
    pub from_column: String,
    pub to_table: String,
    pub to_column: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MetadataOverrides {
    pub aliases: Vec<MetadataAlias>,
    pub relationships: Vec<MetadataRelationship>,
}

/// Source definition sent from the add/test form. Mirrors the server's `SourceInput`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SourceInput {
    pub name: String,
    pub kind: String,
    pub host: String,
    pub port: Option<u16>,
    pub database: String,
    pub username: String,
    pub password: String,
    pub query: Option<String>,
    pub table: Option<String>,
}

/// Editable source fields. Mirrors the server's `SourceUpdate` — every field is
/// optional, and a blank/omitted password keeps the stored one.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SourceUpdate {
    pub name: Option<String>,
    pub kind: Option<String>,
    pub host: Option<String>,
    pub port: Option<u16>,
    pub database: Option<String>,
    pub username: Option<String>,
    pub password: Option<String>,
    pub query: Option<String>,
    pub table: Option<String>,
}

/// `GET /sources` — saved sources (no secrets).
#[tauri::command]
pub async fn list_sources(bridge: State<'_, Bridge>) -> Result<Vec<SourceInfo>, String> {
    let token = bridge.token().ok_or("not logged in")?;
    let url = bridge.url("/sources");
    let resp = bridge
        .client
        .get(&url)
        .bearer_auth(token)
        .send()
        .await
        .map_err(|e| format!("request failed: {e}"))?;
    check_auth(&bridge, &resp)?;
    if !resp.status().is_success() {
        return Err(error_body(resp).await);
    }
    resp.json::<Vec<SourceInfo>>()
        .await
        .map_err(|e| format!("invalid response: {e}"))
}

/// `GET /nl2sql/<source_id>/catalog` — inspect active schema metadata. Admin only.
#[tauri::command]
pub async fn get_schema_catalog(
    source_id: String,
    bridge: State<'_, Bridge>,
) -> Result<SchemaCatalogStatus, String> {
    let token = bridge.token().ok_or("not logged in")?;
    let url = bridge.url(&format!("/nl2sql/{source_id}/catalog"));
    let resp = bridge
        .client
        .get(&url)
        .bearer_auth(token)
        .send()
        .await
        .map_err(|e| format!("request failed: {e}"))?;
    check_auth(&bridge, &resp)?;
    if !resp.status().is_success() {
        return Err(error_body(resp).await);
    }
    resp.json::<SchemaCatalogStatus>()
        .await
        .map_err(|e| format!("invalid response: {e}"))
}

/// `POST /nl2sql/<source_id>/catalog/refresh` — rebuild schema metadata. Admin only.
#[tauri::command]
pub async fn refresh_schema_catalog(
    source_id: String,
    bridge: State<'_, Bridge>,
) -> Result<SchemaCatalogRefresh, String> {
    let token = bridge.token().ok_or("not logged in")?;
    let url = bridge.url(&format!("/nl2sql/{source_id}/catalog/refresh"));
    let resp = bridge
        .client
        .post(&url)
        .bearer_auth(token)
        .send()
        .await
        .map_err(|e| format!("request failed: {e}"))?;
    check_auth(&bridge, &resp)?;
    if !resp.status().is_success() {
        return Err(error_body(resp).await);
    }
    resp.json::<SchemaCatalogRefresh>()
        .await
        .map_err(|e| format!("invalid response: {e}"))
}

#[tauri::command]
pub async fn get_schema_catalog_history(
    source_id: String,
    bridge: State<'_, Bridge>,
) -> Result<Vec<SchemaCatalogHistoryItem>, String> {
    let token = bridge.token().ok_or("not logged in")?;
    let resp = bridge
        .client
        .get(bridge.url(&format!("/nl2sql/{source_id}/catalog/history?limit=25")))
        .bearer_auth(token)
        .send()
        .await
        .map_err(|e| format!("request failed: {e}"))?;
    check_auth(&bridge, &resp)?;
    if !resp.status().is_success() {
        return Err(error_body(resp).await);
    }
    resp.json()
        .await
        .map_err(|e| format!("invalid response: {e}"))
}

#[tauri::command]
pub async fn get_schema_metadata_overrides(
    source_id: String,
    bridge: State<'_, Bridge>,
) -> Result<MetadataOverrides, String> {
    let token = bridge.token().ok_or("not logged in")?;
    let resp = bridge
        .client
        .get(bridge.url(&format!("/nl2sql/{source_id}/catalog/overrides")))
        .bearer_auth(token)
        .send()
        .await
        .map_err(|e| format!("request failed: {e}"))?;
    check_auth(&bridge, &resp)?;
    if !resp.status().is_success() {
        return Err(error_body(resp).await);
    }
    resp.json()
        .await
        .map_err(|e| format!("invalid response: {e}"))
}

#[tauri::command]
pub async fn save_schema_metadata_overrides(
    source_id: String,
    overrides: MetadataOverrides,
    bridge: State<'_, Bridge>,
) -> Result<MetadataOverrides, String> {
    let token = bridge.token().ok_or("not logged in")?;
    let resp = bridge
        .client
        .put(bridge.url(&format!("/nl2sql/{source_id}/catalog/overrides")))
        .bearer_auth(token)
        .json(&overrides)
        .send()
        .await
        .map_err(|e| format!("request failed: {e}"))?;
    check_auth(&bridge, &resp)?;
    if !resp.status().is_success() {
        return Err(error_body(resp).await);
    }
    resp.json()
        .await
        .map_err(|e| format!("invalid response: {e}"))
}

/// `POST /sources/test` — connect and verify without saving. Admin only.
#[tauri::command]
pub async fn test_source(source: SourceInput, bridge: State<'_, Bridge>) -> Result<(), String> {
    post_source("/sources/test", &source, &bridge)
        .await
        .map(|_| ())
}

/// `POST /sources` — test then save (password encrypted server-side). Admin only.
#[tauri::command]
pub async fn save_source(
    source: SourceInput,
    bridge: State<'_, Bridge>,
) -> Result<SourceInfo, String> {
    let value = post_source("/sources", &source, &bridge).await?;
    serde_json::from_value(value).map_err(|e| format!("invalid response: {e}"))
}

/// Shared POST for the two source endpoints; returns the raw JSON body on success.
async fn post_source(
    path: &str,
    source: &SourceInput,
    bridge: &Bridge,
) -> Result<serde_json::Value, String> {
    let token = bridge.token().ok_or("not logged in")?;
    let url = bridge.url(path);
    let resp = bridge
        .client
        .post(&url)
        .bearer_auth(token)
        .json(source)
        .send()
        .await
        .map_err(|e| format!("request failed: {e}"))?;
    check_auth(bridge, &resp)?;
    if !resp.status().is_success() {
        return Err(error_body(resp).await);
    }
    resp.json::<serde_json::Value>()
        .await
        .map_err(|e| format!("invalid response: {e}"))
}

/// `PATCH /sources/<id>` — edit a saved source. Admin only.
#[tauri::command]
pub async fn update_source(
    id: String,
    patch: SourceUpdate,
    bridge: State<'_, Bridge>,
) -> Result<SourceInfo, String> {
    let token = bridge.token().ok_or("not logged in")?;
    let url = bridge.url(&format!("/sources/{id}"));
    let resp = bridge
        .client
        .patch(&url)
        .bearer_auth(token)
        .json(&patch)
        .send()
        .await
        .map_err(|e| format!("request failed: {e}"))?;
    check_auth(&bridge, &resp)?;
    if !resp.status().is_success() {
        return Err(error_body(resp).await);
    }
    resp.json::<SourceInfo>()
        .await
        .map_err(|e| format!("invalid response: {e}"))
}

/// `POST /sources/<id>/test` — re-test a saved source, persisting the outcome. Admin only.
#[tauri::command]
pub async fn test_saved_source(
    id: String,
    bridge: State<'_, Bridge>,
) -> Result<SourceInfo, String> {
    let token = bridge.token().ok_or("not logged in")?;
    let url = bridge.url(&format!("/sources/{id}/test"));
    let resp = bridge
        .client
        .post(&url)
        .bearer_auth(token)
        .send()
        .await
        .map_err(|e| format!("request failed: {e}"))?;
    check_auth(&bridge, &resp)?;
    if !resp.status().is_success() {
        return Err(error_body(resp).await);
    }
    resp.json::<SourceInfo>()
        .await
        .map_err(|e| format!("invalid response: {e}"))
}

/// `DELETE /sources/<id>` — remove a source and its ingested records. Admin only.
#[tauri::command]
pub async fn delete_source(id: String, bridge: State<'_, Bridge>) -> Result<(), String> {
    let token = bridge.token().ok_or("not logged in")?;
    let url = bridge.url(&format!("/sources/{id}"));
    let resp = bridge
        .client
        .delete(&url)
        .bearer_auth(token)
        .send()
        .await
        .map_err(|e| format!("request failed: {e}"))?;
    check_auth(&bridge, &resp)?;
    if !resp.status().is_success() {
        return Err(error_body(resp).await);
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Workstream 5: ingestion — start a job and relay its SSE progress to the UI.
// ---------------------------------------------------------------------------

/// One structured log entry in a progress snapshot. Mirrors the server's `LogEntry`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LogEntry {
    pub time: String,
    /// "info" | "success" | "warn" | "error" | "divider"
    pub level: String,
    pub message: String,
}

/// Full 14-field progress snapshot mirrored from the server's `IngestProgress`.
/// Forwarded to the frontend on `ingest://progress`, `ingest://done`, and
/// `ingest://error` events. Every field must be kept in sync with `bridge.ts`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IngestProgress {
    pub job_id: String,
    /// "running" | "completed" | "partial" | "failed"
    pub status: String,
    /// 1-based index of the table currently being ingested.
    pub table_index: i64,
    pub total_tables: i64,
    pub current_table: String,
    /// Rows processed in the current table (this batch).
    pub table_rows: i64,
    /// Estimated total rows in the current table.
    pub table_total: i64,
    /// Cumulative rows processed across all tables.
    pub processed_rows: i64,
    /// Estimated total rows across all selected tables.
    pub total_rows: i64,
    /// Count of errors (detail lives in `log` at level "error").
    pub errors: i64,
    pub success_tables: i64,
    pub failed_tables: i64,
    /// Cumulative UTF-8 bytes of embedded chunk text.
    pub db_size_bytes: i64,
    /// Rows annotated by the clinical extractor (plan 25). 0 unless the server has
    /// ONPREM_EXTRACT_ENABLED=true.
    #[serde(default)]
    pub extracted_rows: i64,
    /// Full current log; server-capped at 500. Client replaces wholesale each event.
    pub log: Vec<LogEntry>,
}

/// Column metadata from a source database table. The `type_` field serializes as
/// `"type"` (Rust keyword avoidance). Mirrors the server's `ColumnSchema`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ColumnSchema {
    pub name: String,
    /// Database type string (e.g. "integer", "varchar"). Serialized as `"type"`.
    #[serde(rename = "type")]
    pub type_: String,
    pub nullable: bool,
    pub is_primary_key: bool,
    pub is_foreign_key: bool,
    pub likely_pii: bool,
}

/// Table metadata (name, row estimate, column list). Mirrors the server's `TableSchema`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TableSchema {
    pub name: String,
    pub row_count: i64,
    pub columns: Vec<ColumnSchema>,
}

/// Schema analysis result from `POST /schema/analyze`. Mirrors `SchemaAnalysis`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SchemaAnalysis {
    pub summary: String,
    pub suggested_tables: Vec<String>,
    /// table_name → column names likely containing PII.
    pub pii_columns: HashMap<String, Vec<String>>,
    pub data_quality_notes: Vec<String>,
}

/// One table row from the ingest history. Mirrors `IngestionHistoryTable`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IngestionHistoryTable {
    /// Composite key `"{source_id}:{table}"`.
    pub table_id: String,
    pub source_table: String,
    pub row_count: i64,
    pub vector_count: i64,
    /// "indexed" | "error" | "indexing"
    pub status: String,
    pub last_ingested: Option<String>,
    pub last_embedded_at: Option<String>,
}

/// Ingest history grouped by source connection. Mirrors `IngestionHistoryConnection`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IngestionHistoryConnection {
    pub source_id: String,
    pub source_name: String,
    /// "postgres" | "mysql" | "mssql"
    pub kind: String,
    pub database: String,
    pub tables: Vec<IngestionHistoryTable>,
    pub total_rows: i64,
    pub total_vectors: i64,
    pub last_ingested: Option<String>,
}

#[derive(Debug, Deserialize)]
struct IngestStarted {
    job_id: String,
}

/// `POST /ingest` then follow `GET /ingest/<job>/stream`. Re-emits each progress
/// snapshot as `ingest://progress`; the terminal `done` Rocket event maps to
/// `ingest://done` and the terminal `error` Rocket event maps to `ingest://error`.
/// Admin only (enforced server-side).
#[tauri::command]
pub async fn start_ingest(
    source_id: String,
    tables: Vec<String>,
    excluded_columns: Option<HashMap<String, Vec<String>>>,
    limit: Option<i64>,
    app: tauri::AppHandle,
    bridge: State<'_, Bridge>,
) -> Result<String, String> {
    let token = bridge.token().ok_or("not logged in")?;

    // Kick off the job; the server returns immediately with an id.
    let start_url = bridge.url("/ingest");
    let resp = bridge
        .client
        .post(&start_url)
        .bearer_auth(&token)
        .json(&serde_json::json!({
            "source_id": source_id,
            "tables": tables,
            "excluded_columns": excluded_columns,
            "limit": limit,
        }))
        .send()
        .await
        .map_err(|e| format!("request failed: {e}"))?;
    check_auth(&bridge, &resp)?;
    if !resp.status().is_success() {
        return Err(error_body(resp).await);
    }
    let started: IngestStarted = resp
        .json()
        .await
        .map_err(|e| format!("invalid response: {e}"))?;
    let job_id = started.job_id;

    // Follow the progress stream to completion.
    let stream_url = bridge.url(&format!("/ingest/{job_id}/stream"));
    let resp = bridge
        .client
        .get(&stream_url)
        .bearer_auth(&token)
        .send()
        .await
        .map_err(|e| format!("request failed: {e}"))?;
    check_auth(&bridge, &resp)?;
    if !resp.status().is_success() {
        return Err(error_body(resp).await);
    }

    let mut events = resp.bytes_stream().eventsource();
    while let Some(event) = events.next().await {
        let event = event.map_err(|e| format!("stream error: {e}"))?;
        match event.event.as_str() {
            "progress" => {
                // Forward the full snapshot to the UI on each tick.
                if let Ok(p) = serde_json::from_str::<IngestProgress>(&event.data) {
                    let _ = app.emit("ingest://progress", p);
                }
            }
            "done" => {
                // Terminal: completed or partial. Forward the final snapshot.
                if let Ok(p) = serde_json::from_str::<IngestProgress>(&event.data) {
                    let _ = app.emit("ingest://done", p);
                }
                return Ok(job_id);
            }
            "error" => {
                // Terminal: failed. Try to forward the structured snapshot; fall back
                // to raw string for unexpected error events (e.g. "job not found").
                if let Ok(p) = serde_json::from_str::<IngestProgress>(&event.data) {
                    let _ = app.emit("ingest://error", p);
                } else {
                    let _ = app.emit("ingest://error", event.data.clone());
                }
                return Err(format!("ingestion failed: {job_id}"));
            }
            _ => {}
        }
    }
    // Stream ended without an explicit terminal event.
    let _ = app.emit("ingest://done", ());
    Ok(job_id)
}

/// `GET /sources/<id>/schema` — enumerate tables + columns from a saved source.
/// Any authenticated user.
#[tauri::command]
pub async fn get_schema(
    source_id: String,
    bridge: State<'_, Bridge>,
) -> Result<Vec<TableSchema>, String> {
    let token = bridge.token().ok_or("not logged in")?;
    let url = bridge.url(&format!("/sources/{source_id}/schema"));
    let resp = bridge
        .client
        .get(&url)
        .bearer_auth(token)
        .send()
        .await
        .map_err(|e| format!("request failed: {e}"))?;
    check_auth(&bridge, &resp)?;
    if !resp.status().is_success() {
        return Err(error_body(resp).await);
    }
    resp.json::<Vec<TableSchema>>()
        .await
        .map_err(|e| format!("invalid response: {e}"))
}

/// `POST /schema/analyze` — AI-assisted PII detection + schema summary.
/// Any authenticated user.
#[tauri::command]
pub async fn analyze_schema(
    tables: Vec<TableSchema>,
    bridge: State<'_, Bridge>,
) -> Result<SchemaAnalysis, String> {
    let token = bridge.token().ok_or("not logged in")?;
    let url = bridge.url("/schema/analyze");
    let resp = bridge
        .client
        .post(&url)
        .bearer_auth(token)
        .json(&serde_json::json!({ "tables": tables }))
        .send()
        .await
        .map_err(|e| format!("request failed: {e}"))?;
    check_auth(&bridge, &resp)?;
    if !resp.status().is_success() {
        return Err(error_body(resp).await);
    }
    resp.json::<SchemaAnalysis>()
        .await
        .map_err(|e| format!("invalid response: {e}"))
}

/// `GET /ingest/history` — ingested-table records grouped by source. Any authenticated user.
#[tauri::command]
pub async fn get_ingest_history(
    bridge: State<'_, Bridge>,
) -> Result<Vec<IngestionHistoryConnection>, String> {
    let token = bridge.token().ok_or("not logged in")?;
    let url = bridge.url("/ingest/history");
    let resp = bridge
        .client
        .get(&url)
        .bearer_auth(token)
        .send()
        .await
        .map_err(|e| format!("request failed: {e}"))?;
    check_auth(&bridge, &resp)?;
    if !resp.status().is_success() {
        return Err(error_body(resp).await);
    }
    resp.json::<Vec<IngestionHistoryConnection>>()
        .await
        .map_err(|e| format!("invalid response: {e}"))
}

/// `DELETE /ingest/table/<source_id>/<table>` — remove one table's indexed records and
/// its history entry. Admin only (enforced server-side).
#[tauri::command]
pub async fn delete_ingest_table(
    source_id: String,
    table: String,
    bridge: State<'_, Bridge>,
) -> Result<(), String> {
    let token = bridge.token().ok_or("not logged in")?;
    let url = bridge.url(&format!("/ingest/table/{source_id}/{table}"));
    let resp = bridge
        .client
        .delete(&url)
        .bearer_auth(token)
        .send()
        .await
        .map_err(|e| format!("request failed: {e}"))?;
    check_auth(&bridge, &resp)?;
    if !resp.status().is_success() {
        return Err(error_body(resp).await);
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Stage 5: Data Explorer — row-grained browse + per-table inspector, plus the
// per-connection / clear-all deletes. Types mirror the server's `explorer.rs`
// (`DataRow`, `RecordsPage`, `TableInfo` and its nested slices). Every field must
// be kept in sync with `src/lib/bridge.ts` or serde silently drops it.
// ---------------------------------------------------------------------------

/// One row in a records page — the row-grained view of chunk-grained storage.
/// Mirrors the server's `DataRow`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DataRow {
    /// The source row primary key (`row_pk`).
    pub id: String,
    pub source_id: String,
    /// The row's original columns as a free-form JSON object (the `fields` map).
    pub data: serde_json::Value,
    /// ISO-8601 timestamp string, or null.
    pub ingested_at: Option<String>,
}

/// A paginated page of row-grained records. Mirrors the server's `RecordsPage`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RecordsPage {
    pub rows: Vec<DataRow>,
    pub total: i64,
    pub page: i64,
    pub page_size: i64,
    pub page_count: i64,
    pub has_prev: bool,
    pub has_next: bool,
}

/// One audit log entry. Mirrors the server's `AuditRow`.
/// Mirror hazard: any field added to the server struct must be added here AND in
/// `src/lib/bridge.ts` (`AuditRow`) or serde silently drops it before it reaches React.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuditRow {
    pub id: String,
    pub user_id: String,
    pub username: String,
    pub action: String,
    pub resource: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub details: Option<serde_json::Value>,
    pub timestamp: String,
}

/// A paginated page of audit log entries. Mirrors the server's `AuditPage`.
/// Mirror hazard: keep in sync with `src/lib/bridge.ts` (`AuditPage`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuditPage {
    pub entries: Vec<AuditRow>,
    pub total: i64,
    pub page: i64,
    pub page_size: i64,
    pub page_count: i64,
    pub has_prev: bool,
    pub has_next: bool,
}

/// The `indexed_tables` slice of the table inspector. Mirrors `TableInfoTable`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TableInfoTable {
    /// Composite id `{source_id}:{table}`.
    pub id: String,
    pub source_id: String,
    pub source_table: String,
    pub row_count: i64,
    pub vector_count: i64,
    pub status: String,
    pub last_ingested: Option<String>,
    pub last_embedded_at: Option<String>,
}

/// The source-connection slice — no secrets; null when the source was deleted.
/// Mirrors the server's `TableInfoConnection`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TableInfoConnection {
    pub id: String,
    pub name: String,
    /// "postgres" | "mysql" | "mssql"
    pub kind: String,
    pub host: String,
    pub port: u16,
    pub database: String,
    pub username: String,
}

/// One column in the row-derived schema profile. The `type_` field serializes as
/// `"type"` (Rust keyword avoidance). Mirrors the server's `TableProfileColumn`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TableProfileColumn {
    pub name: String,
    /// JSON value type: string | number | boolean | object | null. Serialized as `"type"`.
    #[serde(rename = "type")]
    pub type_: String,
    pub nullable: bool,
    pub selected: bool,
    pub pii: bool,
}

/// The row-derived schema profile. Mirrors the server's `TableProfile`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TableProfile {
    pub columns: Vec<TableProfileColumn>,
    pub pii_columns: Vec<String>,
    pub selected_columns: Vec<String>,
}

/// One recent ingest run for the connection this table belongs to. Connection-scoped
/// (jobs have no per-table breakdown). Mirrors the server's `RecentRun`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RecentRun {
    pub id: String,
    pub status: String,
    pub rows_processed: i64,
    pub chunks_created: i64,
    pub started_at: Option<String>,
    pub completed_at: Option<String>,
    pub errors: i64,
}

/// The full table inspector payload. Mirrors the server's `TableInfo`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TableInfo {
    pub table: TableInfoTable,
    pub connection: Option<TableInfoConnection>,
    pub profile: Option<TableProfile>,
    pub recent_runs: Vec<RecentRun>,
}

/// Percent-encode a single URL path segment (dependency-free). Leaves the RFC 3986
/// unreserved set (`A-Z a-z 0-9 - . _ ~`) as-is and encodes everything else — most
/// importantly the `:` in a `{source_id}:{table}` id, plus any `/` or space a table
/// name might contain — so the whole id arrives as one Rocket path segment.
fn encode_path_segment(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for &b in s.as_bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => {
                out.push(b as char);
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

/// `GET /records?source_id=&table=&page=&page_size=&q=` — row-grained paginated
/// browse of ingested records for one (source, table). Any authenticated user.
/// `q` is only sent when present and non-empty.
#[tauri::command]
pub async fn list_records(
    source_id: String,
    table: String,
    page: i64,
    page_size: i64,
    q: Option<String>,
    bridge: State<'_, Bridge>,
) -> Result<RecordsPage, String> {
    let token = bridge.token().ok_or("not logged in")?;

    // Build the query string manually and percent-encode each value (this reqwest
    // build doesn't expose `.query()`). Append `q` only when it's Some and non-empty
    // (an empty query would filter to nothing server-side).
    let mut qs = format!(
        "/records?source_id={}&table={}&page={}&page_size={}",
        encode_path_segment(&source_id),
        encode_path_segment(&table),
        page,
        page_size,
    );
    if let Some(query_text) = q.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
        qs.push_str(&format!("&q={}", encode_path_segment(query_text)));
    }
    let url = bridge.url(&qs);

    let resp = bridge
        .client
        .get(&url)
        .bearer_auth(token)
        .send()
        .await
        .map_err(|e| format!("request failed: {e}"))?;
    check_auth(&bridge, &resp)?;
    if !resp.status().is_success() {
        return Err(error_body(resp).await);
    }
    resp.json::<RecordsPage>()
        .await
        .map_err(|e| format!("invalid response: {e}"))
}

/// `GET /audit` — filterable, paginated audit log. Admin only (enforced server-side).
/// Filters are appended to the query string only when `Some` and non-empty, matching
/// the `list_records` pattern exactly (manual build + `encode_path_segment`).
#[tauri::command]
pub async fn get_audit(
    user: Option<String>,
    action: Option<String>,
    from: Option<String>,
    to: Option<String>,
    page: i64,
    page_size: i64,
    bridge: State<'_, Bridge>,
) -> Result<AuditPage, String> {
    let token = bridge.token().ok_or("not logged in")?;

    // Build the query string manually; append each filter only when Some and non-empty.
    let mut qs = format!("/audit?page={page}&page_size={page_size}");
    if let Some(v) = user.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
        qs.push_str(&format!("&user={}", encode_path_segment(v)));
    }
    if let Some(v) = action.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
        qs.push_str(&format!("&action={}", encode_path_segment(v)));
    }
    if let Some(v) = from.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
        qs.push_str(&format!("&from={}", encode_path_segment(v)));
    }
    if let Some(v) = to.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
        qs.push_str(&format!("&to={}", encode_path_segment(v)));
    }
    let url = bridge.url(&qs);

    let resp = bridge
        .client
        .get(&url)
        .bearer_auth(token)
        .send()
        .await
        .map_err(|e| format!("request failed: {e}"))?;
    check_auth(&bridge, &resp)?;
    if !resp.status().is_success() {
        return Err(error_body(resp).await);
    }
    resp.json::<AuditPage>()
        .await
        .map_err(|e| format!("invalid response: {e}"))
}

/// `GET /tables/<table_id>/info` — inspector for one indexed table. Any authenticated
/// user. `table_id` is `{source_id}:{table}`; the `:` is percent-encoded so it stays
/// a single Rocket path segment.
#[tauri::command]
pub async fn get_table_info(
    table_id: String,
    bridge: State<'_, Bridge>,
) -> Result<TableInfo, String> {
    let token = bridge.token().ok_or("not logged in")?;
    let url = bridge.url(&format!("/tables/{}/info", encode_path_segment(&table_id)));
    let resp = bridge
        .client
        .get(&url)
        .bearer_auth(token)
        .send()
        .await
        .map_err(|e| format!("request failed: {e}"))?;
    check_auth(&bridge, &resp)?;
    if !resp.status().is_success() {
        return Err(error_body(resp).await);
    }
    resp.json::<TableInfo>()
        .await
        .map_err(|e| format!("invalid response: {e}"))
}

/// `DELETE /ingest/connection/<source_id>` — remove every indexed table and all
/// embedded records for one source connection. Admin only (enforced server-side).
/// Returns the raw server JSON (`{ tables_removed }`).
#[tauri::command]
pub async fn delete_ingest_connection(
    source_id: String,
    bridge: State<'_, Bridge>,
) -> Result<serde_json::Value, String> {
    let token = bridge.token().ok_or("not logged in")?;
    let url = bridge.url(&format!(
        "/ingest/connection/{}",
        encode_path_segment(&source_id)
    ));
    let resp = bridge
        .client
        .delete(&url)
        .bearer_auth(token)
        .send()
        .await
        .map_err(|e| format!("request failed: {e}"))?;
    check_auth(&bridge, &resp)?;
    if !resp.status().is_success() {
        return Err(error_body(resp).await);
    }
    resp.json::<serde_json::Value>()
        .await
        .map_err(|e| format!("invalid response: {e}"))
}

/// `DELETE /ingest/all` — clear every record and indexed-table entry across all
/// sources. Admin only (enforced server-side). Returns the raw server JSON
/// (`{ ok: true }`).
#[tauri::command]
pub async fn clear_all_records(bridge: State<'_, Bridge>) -> Result<serde_json::Value, String> {
    let token = bridge.token().ok_or("not logged in")?;
    let url = bridge.url("/ingest/all");
    let resp = bridge
        .client
        .delete(&url)
        .bearer_auth(token)
        .send()
        .await
        .map_err(|e| format!("request failed: {e}"))?;
    check_auth(&bridge, &resp)?;
    if !resp.status().is_success() {
        return Err(error_body(resp).await);
    }
    resp.json::<serde_json::Value>()
        .await
        .map_err(|e| format!("invalid response: {e}"))
}

// ---------------------------------------------------------------------------
// Live log stream: relay the server's `GET /logs/stream` SSE feed to the frontend
// as `logs://line` events so the per-page log windows can render them. One stream
// per session serves every panel (they filter client-side).
// ---------------------------------------------------------------------------

/// `GET /logs/stream` — subscribe to server tracing and re-emit each `log` line to
/// the frontend as a `logs://line` event (payload: the raw `LogLine` JSON string).
/// Idempotent: if a stream is already running this returns immediately. Runs until
/// the connection ends (e.g. logout → 401), then clears the guard so it can restart.
#[tauri::command]
pub async fn start_log_stream(
    app: tauri::AppHandle,
    bridge: State<'_, Bridge>,
) -> Result<(), String> {
    use std::sync::atomic::Ordering;

    // Start at most once; `swap` returning true means a stream is already live.
    if bridge.log_streaming.swap(true, Ordering::SeqCst) {
        return Ok(());
    }
    // Any early exit from here must release the guard so a later call can restart.
    let result = run_log_stream(&app, &bridge).await;
    bridge.log_streaming.store(false, Ordering::SeqCst);
    result
}

/// Inner body of `start_log_stream`, split out so the caller can always clear the
/// `log_streaming` guard regardless of how the stream ends.
async fn run_log_stream(app: &tauri::AppHandle, bridge: &Bridge) -> Result<(), String> {
    let token = bridge.token().ok_or("not logged in")?;
    let url = bridge.url("/logs/stream");
    let resp = bridge
        .client
        .get(&url)
        .bearer_auth(&token)
        .send()
        .await
        .map_err(|e| format!("request failed: {e}"))?;
    check_auth(bridge, &resp)?;
    if !resp.status().is_success() {
        return Err(error_body(resp).await);
    }

    let mut events = resp.bytes_stream().eventsource();
    while let Some(event) = events.next().await {
        let event = event.map_err(|e| format!("stream error: {e}"))?;
        match event.event.as_str() {
            "log" => {
                let _ = app.emit("logs://line", event.data);
            }
            "warn" => {
                // Backpressure notice (dropped lines) — surface as an error banner.
                let _ = app.emit("logs://error", event.data);
            }
            _ => {}
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Workstream 6: hybrid retrieval + RAG chat. Types mirror the server's
// `Passage` / `SearchResponse`; `chat` relays the grounded SSE stream.
// ---------------------------------------------------------------------------

/// A retrieved passage backing an answer. Mirrors the server's `Passage`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Passage {
    pub id: String,
    pub source_id: String,
    pub row_pk: String,
    pub chunk_index: i32,
    pub text: String,
    pub fields: serde_json::Value,
    pub score: f64,
    pub reranked: bool,
}

/// One prior conversation turn, sent for history-aware query rewrite.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatTurn {
    pub role: String,
    pub content: String,
}

/// Retrieval overrides from the Chat UI toggles (all optional → server defaults).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RetrievalOpts {
    pub mode: Option<String>,
    pub rerank: Option<bool>,
    pub top_k: Option<usize>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SearchResponse {
    pub query: String,
    pub queries: Vec<String>,
    pub passages: Vec<Passage>,
}

// ---------------------------------------------------------------------------
// Stage 6: persistent conversations — mirror structs + run_id envelope.
// ---------------------------------------------------------------------------

/// One conversation entry. Mirrors the server's `ConversationOut`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Conversation {
    pub id: String,
    pub title: String,
    pub created_at: String,
    pub updated_at: String,
    /// Present only for agent conversations; absent for plain chat.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub agent_kind: Option<String>,
}

/// Structured aggregation result mirrored from the server's `StructuredResult`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StructuredResult {
    pub spec: serde_json::Value,
    pub rows: serde_json::Value,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pipeline: Option<serde_json::Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SqlResult {
    pub source_id: String,
    pub sql: String,
    pub columns: Vec<String>,
    pub rows: Vec<Vec<serde_json::Value>>,
}

/// One stored message in a conversation. Mirrors the server's `MessageOut`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StoredMessage {
    pub id: String,
    pub role: String,
    pub content: String,
    /// `None` for user messages; `Some(passages)` for assistant messages.
    pub citations: Option<Vec<Passage>>,
    /// Persisted grounding verdict for verified assistant messages.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub verify: Option<serde_json::Value>,
    pub created_at: String,
    /// Present only for agent messages.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub agent_kind: Option<String>,
    /// Present only for structured-result agent messages.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub structured: Option<StructuredResult>,
    /// Present for chat answers backed by a live operational SQL query.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sql_result: Option<SqlResult>,
}

/// Envelope stamped onto every `chat://*` event so the frontend can drop events
/// belonging to a stale (superseded) run.
#[derive(Serialize, Clone)]
struct ChatEvent {
    run_id: String,
    data: String,
}

/// `POST /search` — debug view of the ranked/fused/reranked passages, no generation.
#[tauri::command]
pub async fn search(
    query: String,
    opts: RetrievalOpts,
    bridge: State<'_, Bridge>,
) -> Result<SearchResponse, String> {
    let token = bridge.token().ok_or("not logged in")?;
    let url = bridge.url("/search");
    let resp = bridge
        .client
        .post(&url)
        .bearer_auth(token)
        .json(&serde_json::json!({
            "query": query,
            "mode": opts.mode,
            "rerank": opts.rerank,
            "top_k": opts.top_k,
        }))
        .send()
        .await
        .map_err(|e| format!("request failed: {e}"))?;
    check_auth(&bridge, &resp)?;
    if !resp.status().is_success() {
        return Err(error_body(resp).await);
    }
    resp.json::<SearchResponse>()
        .await
        .map_err(|e| format!("invalid response: {e}"))
}

/// `POST /chat` — grounded RAG answer streamed as SSE. Emits `chat://citations`
/// (JSON array of `Passage`) once, then `chat://token` per token, ending with
/// `chat://done` (or `chat://error`). Returns when the stream completes.
///
/// `run_id` is stamped into every emitted event (as `ChatEvent { run_id, data }`) so
/// the frontend can discard events from a superseded run. `conversation_id`, when
/// `Some`, instructs the server to persist the exchange and load history from the DB.
#[tauri::command]
pub async fn chat(
    question: String,
    history: Vec<ChatTurn>,
    opts: RetrievalOpts,
    run_id: String,
    conversation_id: Option<String>,
    app: tauri::AppHandle,
    bridge: State<'_, Bridge>,
) -> Result<(), String> {
    let token = bridge.token().ok_or("not logged in")?;
    let url = bridge.url("/chat");
    let (abort_handle, abort_registration) = AbortHandle::new_pair();
    bridge.register_run(run_id.clone(), abort_handle);
    let result = Abortable::new(
        async {
            let resp = bridge
                .client
                .post(&url)
                .bearer_auth(token)
                .json(&serde_json::json!({
                    "question": question,
                    "history": history,
                    "mode": opts.mode,
                    "rerank": opts.rerank,
                    "top_k": opts.top_k,
                    "conversation_id": conversation_id,
                }))
                .send()
                .await
                .map_err(|e| format!("request failed: {e}"))?;
            check_auth(&bridge, &resp)?;
            if !resp.status().is_success() {
                return Err(error_body(resp).await);
            }

            let mut events = resp.bytes_stream().eventsource();
            while let Some(event) = events.next().await {
                let event = event.map_err(|e| format!("stream error: {e}"))?;
                match event.event.as_str() {
                    "routed" => {
                        // Intent Router v2: the decision payload ({route,intent,tier,cached,backend}).
                        // Relayed for the activity strip; older UIs simply ignore chat://routed.
                        let _ = app.emit(
                            "chat://routed",
                            ChatEvent {
                                run_id: run_id.clone(),
                                data: event.data,
                            },
                        );
                    }
                    "citations" => {
                        let _ = app.emit(
                            "chat://citations",
                            ChatEvent {
                                run_id: run_id.clone(),
                                data: event.data,
                            },
                        );
                    }
                    "sql" | "columns" | "rows" => {
                        let channel = format!("chat://{}", event.event);
                        let _ = app.emit(
                            &channel,
                            ChatEvent {
                                run_id: run_id.clone(),
                                data: event.data,
                            },
                        );
                    }
                    "token" => {
                        let _ = app.emit(
                            "chat://token",
                            ChatEvent {
                                run_id: run_id.clone(),
                                data: decode_token(&event.data),
                            },
                        );
                    }
                    "error" => {
                        let _ = app.emit(
                            "chat://error",
                            ChatEvent {
                                run_id: run_id.clone(),
                                data: event.data.clone(),
                            },
                        );
                        return Err(event.data);
                    }
                    "done" => break,
                    _ => {}
                }
            }
            let _ = app.emit(
                "chat://done",
                ChatEvent {
                    run_id: run_id.clone(),
                    data: String::new(),
                },
            );
            Ok(())
        },
        abort_registration,
    )
    .await;
    bridge.remove_run(&run_id);
    result.unwrap_or_else(|_| Err("__RUN_STOPPED__".to_string()))
}

#[tauri::command]
pub fn cancel_run(run_id: String, bridge: State<'_, Bridge>) -> bool {
    bridge.abort_run(&run_id);
    true
}

/// `GET /conversations` — all conversations for the authenticated user (sorted by most recent).
#[tauri::command]
pub async fn list_conversations(bridge: State<'_, Bridge>) -> Result<Vec<Conversation>, String> {
    let token = bridge.token().ok_or("not logged in")?;
    let url = bridge.url("/conversations");
    let resp = bridge
        .client
        .get(&url)
        .bearer_auth(token)
        .send()
        .await
        .map_err(|e| format!("request failed: {e}"))?;
    check_auth(&bridge, &resp)?;
    if !resp.status().is_success() {
        return Err(error_body(resp).await);
    }
    resp.json::<Vec<Conversation>>()
        .await
        .map_err(|e| format!("invalid response: {e}"))
}

/// `POST /conversations` — create a new conversation with an optional title and agent kind.
/// `agent_kind`, when provided, marks this as an agent conversation (the server stores it
/// and `GET /conversations` excludes it; `GET /agent-conversations` returns it instead).
#[tauri::command]
pub async fn create_conversation(
    title: Option<String>,
    agent_kind: Option<String>,
    bridge: State<'_, Bridge>,
) -> Result<Conversation, String> {
    let token = bridge.token().ok_or("not logged in")?;
    let url = bridge.url("/conversations");
    let resp = bridge
        .client
        .post(&url)
        .bearer_auth(token)
        .json(&serde_json::json!({ "title": title, "agent_kind": agent_kind }))
        .send()
        .await
        .map_err(|e| format!("request failed: {e}"))?;
    check_auth(&bridge, &resp)?;
    if !resp.status().is_success() {
        return Err(error_body(resp).await);
    }
    resp.json::<Conversation>()
        .await
        .map_err(|e| format!("invalid response: {e}"))
}

/// `PATCH /conversations/<id>` — rename a conversation. Returns the updated entry.
#[tauri::command]
pub async fn rename_conversation(
    id: String,
    title: String,
    bridge: State<'_, Bridge>,
) -> Result<Conversation, String> {
    let token = bridge.token().ok_or("not logged in")?;
    let url = bridge.url(&format!("/conversations/{id}"));
    let resp = bridge
        .client
        .patch(&url)
        .bearer_auth(token)
        .json(&serde_json::json!({ "title": title }))
        .send()
        .await
        .map_err(|e| format!("request failed: {e}"))?;
    check_auth(&bridge, &resp)?;
    if !resp.status().is_success() {
        return Err(error_body(resp).await);
    }
    resp.json::<Conversation>()
        .await
        .map_err(|e| format!("invalid response: {e}"))
}

/// `DELETE /conversations/<id>` — delete a conversation and cascade its messages.
/// Ignores the `{ "ok": true }` body on success.
#[tauri::command]
pub async fn delete_conversation(id: String, bridge: State<'_, Bridge>) -> Result<(), String> {
    let token = bridge.token().ok_or("not logged in")?;
    let url = bridge.url(&format!("/conversations/{id}"));
    let resp = bridge
        .client
        .delete(&url)
        .bearer_auth(token)
        .send()
        .await
        .map_err(|e| format!("request failed: {e}"))?;
    check_auth(&bridge, &resp)?;
    if !resp.status().is_success() {
        return Err(error_body(resp).await);
    }
    Ok(())
}

/// `GET /conversations/<id>/messages` — all messages in a conversation (ascending by time).
#[tauri::command]
pub async fn get_messages(
    id: String,
    bridge: State<'_, Bridge>,
) -> Result<Vec<StoredMessage>, String> {
    let token = bridge.token().ok_or("not logged in")?;
    let url = bridge.url(&format!("/conversations/{id}/messages"));
    let resp = bridge
        .client
        .get(&url)
        .bearer_auth(token)
        .send()
        .await
        .map_err(|e| format!("request failed: {e}"))?;
    check_auth(&bridge, &resp)?;
    if !resp.status().is_success() {
        return Err(error_body(resp).await);
    }
    resp.json::<Vec<StoredMessage>>()
        .await
        .map_err(|e| format!("invalid response: {e}"))
}

/// `GET /agent-conversations?kind=<kind>` — agent conversations for one kind,
/// most-recent first. Mirrors `list_conversations` but hits the agent-specific route.
#[tauri::command]
pub async fn list_agent_conversations(
    kind: String,
    bridge: State<'_, Bridge>,
) -> Result<Vec<Conversation>, String> {
    let token = bridge.token().ok_or("not logged in")?;
    let url = bridge.url(&format!(
        "/agent-conversations?kind={}",
        encode_path_segment(&kind)
    ));
    let resp = bridge
        .client
        .get(&url)
        .bearer_auth(token)
        .send()
        .await
        .map_err(|e| format!("request failed: {e}"))?;
    check_auth(&bridge, &resp)?;
    if !resp.status().is_success() {
        return Err(error_body(resp).await);
    }
    resp.json::<Vec<Conversation>>()
        .await
        .map_err(|e| format!("invalid response: {e}"))
}

/// `POST /agents/<kind>` — AI Agents SSE endpoint. The server emits a `routed` event
/// first (payload: the resolved agent kind as a JSON string), then path-specific events:
///   - Structured kinds (health_query, trends): `spec`, `rows`, `pipeline`, then `token`*.
///   - Semantic kinds (summarize, patient_lookup, chat): `citations`, then `token`*.
/// `kind` may be `"auto"` — the server resolves the real kind and announces it via
/// the first `routed` event.
///
/// All events are wrapped in `ChatEvent { run_id, data }` so the frontend can drop events
/// from a superseded run, mirroring the `/chat` stream contract exactly.
/// `conversation_id`, when `Some`, instructs the server to persist the exchange and load
/// history from the DB.
#[tauri::command]
pub async fn agent(
    kind: String,
    question: String,
    conversation_id: Option<String>,
    run_id: String,
    app: tauri::AppHandle,
    bridge: State<'_, Bridge>,
) -> Result<(), String> {
    let token = bridge.token().ok_or("not logged in")?;
    // Build the path dynamically; `kind` is validated server-side (unknown kinds
    // return a 400 from the Rocket route guard, caught by `check_auth` / `error_body`).
    let url = bridge.url(&format!("/agents/{}", encode_path_segment(&kind)));
    let (abort_handle, abort_registration) = AbortHandle::new_pair();
    bridge.register_run(run_id.clone(), abort_handle);
    let result = Abortable::new(
        async {
            let resp = bridge
                .client
                .post(&url)
                .bearer_auth(token)
                .json(&serde_json::json!({
                    "question": question,
                    "conversation_id": conversation_id,
                }))
                .send()
                .await
                .map_err(|e| format!("request failed: {e}"))?;
            check_auth(&bridge, &resp)?;
            if !resp.status().is_success() {
                return Err(error_body(resp).await);
            }

            let mut events = resp.bytes_stream().eventsource();
            while let Some(event) = events.next().await {
                let event = event.map_err(|e| format!("stream error: {e}"))?;
                match event.event.as_str() {
                    // First event: the server's resolved agent kind (JSON-encoded string).
                    "routed" => {
                        let _ = app.emit(
                            "agent://routed",
                            ChatEvent {
                                run_id: run_id.clone(),
                                data: event.data,
                            },
                        );
                    }
                    // Structured path: the parsed aggregation spec (provenance).
                    "spec" => {
                        let _ = app.emit(
                            "agent://spec",
                            ChatEvent {
                                run_id: run_id.clone(),
                                data: event.data,
                            },
                        );
                    }
                    // Structured path: chart-ready rows [{label, value}].
                    "rows" => {
                        let _ = app.emit(
                            "agent://rows",
                            ChatEvent {
                                run_id: run_id.clone(),
                                data: event.data,
                            },
                        );
                    }
                    // Structured path: the MongoDB pipeline used (explain / audit trail).
                    "pipeline" => {
                        let _ = app.emit(
                            "agent://pipeline",
                            ChatEvent {
                                run_id: run_id.clone(),
                                data: event.data,
                            },
                        );
                    }
                    // Semantic path: citation passages backing the answer.
                    "citations" => {
                        let _ = app.emit(
                            "agent://citations",
                            ChatEvent {
                                run_id: run_id.clone(),
                                data: event.data,
                            },
                        );
                    }
                    // Both paths: streamed narration / answer tokens (JSON-encoded to preserve spaces).
                    "token" => {
                        let _ = app.emit(
                            "agent://token",
                            ChatEvent {
                                run_id: run_id.clone(),
                                data: decode_token(&event.data),
                            },
                        );
                    }
                    "error" => {
                        let _ = app.emit(
                            "agent://error",
                            ChatEvent {
                                run_id: run_id.clone(),
                                data: event.data.clone(),
                            },
                        );
                        return Err(event.data);
                    }
                    "done" => break,
                    _ => {}
                }
            }
            let _ = app.emit(
                "agent://done",
                ChatEvent {
                    run_id: run_id.clone(),
                    data: String::new(),
                },
            );
            Ok(())
        },
        abort_registration,
    )
    .await;
    bridge.remove_run(&run_id);
    result.unwrap_or_else(|_| Err("__RUN_STOPPED__".to_string()))
}

/// The server JSON-encodes `token` payloads so SSE doesn't strip their leading
/// spaces. Decode back to the raw token; fall back to the raw field if it somehow
/// isn't valid JSON (defensive — keeps a malformed frame from vanishing).
fn decode_token(data: &str) -> String {
    serde_json::from_str::<String>(data).unwrap_or_else(|_| data.to_string())
}

/// If the server rejected the token, drop it so the UI can send the user back to login.
/// Returns `SESSION_EXPIRED` so the React layer can map it to a forced-logout banner.
fn check_auth(bridge: &Bridge, resp: &reqwest::Response) -> Result<(), String> {
    if resp.status() == reqwest::StatusCode::UNAUTHORIZED {
        bridge.set_token(None);
        return Err(SESSION_EXPIRED.into());
    }
    Ok(())
}

/// `POST /auth/users/<id>/logout` — admin force-logout: bump a user's
/// token_version server-side so all their outstanding tokens are rejected. Admin only.
#[tauri::command]
pub async fn force_logout_user(id: String, bridge: State<'_, Bridge>) -> Result<(), String> {
    let token = bridge.token().ok_or("not logged in")?;
    let url = bridge.url(&format!("/auth/users/{id}/logout"));
    let resp = bridge
        .client
        .post(&url)
        .bearer_auth(token)
        .send()
        .await
        .map_err(|e| format!("request failed: {e}"))?;
    check_auth(&bridge, &resp)?;
    if !resp.status().is_success() {
        return Err(error_body(resp).await);
    }
    Ok(())
}

/// Extract a human-readable error from a non-2xx response (server `AppError` JSON
/// has an `error` field; fall back to the status code).
async fn error_body(resp: reqwest::Response) -> String {
    let status = resp.status();
    match resp.json::<serde_json::Value>().await {
        Ok(v) => v
            .get("error")
            .and_then(|e| e.as_str())
            .map(str::to_string)
            .unwrap_or_else(|| format!("HTTP {status}")),
        Err(_) => format!("HTTP {status}"),
    }
}
