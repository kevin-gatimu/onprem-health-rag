//! Conversation CRUD routes (Stage 6): `GET /conversations`, `POST /conversations`,
//! `PATCH /conversations/<id>`, `DELETE /conversations/<id>`,
//! `GET /conversations/<id>/messages`.
//! Stage 7 additions: `agent_kind` discriminator on conversations/messages,
//! `GET /agent-conversations?<kind>`, `persist_agent_assistant_message`.
//!
//! All routes are scoped by JWT user id. Ownership mismatch → 404 (don't leak
//! existence). Conversation ids are ObjectId hex strings; a malformed id → 400.

use futures::TryStreamExt;
use mongodb::bson::{DateTime as BsonDateTime, Document, doc, oid::ObjectId};
use rocket::serde::json::Json;
use rocket::{State, delete, get, patch, post};
use serde::{Deserialize, Serialize};

use crate::auth::guard::AuthUser;
use crate::documentdb::DocumentDb;
use crate::error::{AppError, AppResult};
use crate::state::AppState;

// ---------------------------------------------------------------------------
// Wire output structs (snake_case — matches the rest of the API)
// ---------------------------------------------------------------------------

#[derive(Debug, Serialize)]
pub struct ConversationOut {
    pub id: String,
    pub title: String,
    pub created_at: String,
    pub updated_at: String,
    /// Set for agent conversations; absent for plain chat.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub agent_kind: Option<String>,
}

/// Structured aggregation result stored with agent assistant messages (structured path).
/// Parsed back from the `structured_json` string field on reload.
#[derive(Debug, Serialize, Deserialize)]
pub struct StructuredResult {
    pub spec: serde_json::Value,
    /// `[{"label": String, "value": f64}]` chart-ready rows from the executor.
    pub rows: serde_json::Value,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pipeline: Option<serde_json::Value>,
}

#[derive(Debug, Serialize)]
pub struct MessageOut {
    pub id: String,
    pub role: String,
    pub content: String,
    /// Null for user messages; the reranked passages for semantic assistant messages.
    pub citations: Option<Vec<crate::retrieval::Passage>>,
    /// Routed kind for agent assistant messages; `None` for user messages and plain chat.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub agent_kind: Option<String>,
    /// Structured aggregation result for structured-path agent assistant messages.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub structured: Option<StructuredResult>,
    pub created_at: String,
}

// ---------------------------------------------------------------------------
// Request bodies
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
pub struct CreateConversationBody {
    pub title: Option<String>,
    /// When present and non-empty, marks this as an agent conversation and excludes
    /// it from `GET /conversations` (which is the plain-chat screen).
    #[serde(default)]
    pub agent_kind: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct RenameConversationBody {
    pub title: String,
}

// ---------------------------------------------------------------------------
// Routes
// ---------------------------------------------------------------------------

/// `GET /conversations` — list plain-chat conversations for the authenticated user,
/// newest-updated first. The `agent_kind: {$exists: false}` filter excludes agent
/// conversations so they don't appear on the Chat screen. Existing plain conversations
/// have no `agent_kind` field, so this filter is a safe, backwards-compatible addition.
#[get("/conversations")]
pub async fn list_conversations(
    state: &State<AppState>,
    user: AuthUser,
) -> AppResult<Json<Vec<ConversationOut>>> {
    let docs: Vec<Document> = state
        .db
        .chat_conversations()
        .find(doc! { "user_id": &user.id, "agent_kind": { "$exists": false } })
        .sort(doc! { "updated_at": -1 })
        .await?
        .try_collect()
        .await?;

    let out = docs.iter().map(conv_doc_to_out).collect();
    Ok(Json(out))
}

/// `GET /agent-conversations?<kind>` — list agent conversations for one kind,
/// scoped by the authenticated user, newest-updated first.
#[get("/agent-conversations?<kind>")]
pub async fn list_agent_conversations(
    state: &State<AppState>,
    user: AuthUser,
    kind: &str,
) -> AppResult<Json<Vec<ConversationOut>>> {
    let docs: Vec<Document> = state
        .db
        .chat_conversations()
        .find(doc! { "user_id": &user.id, "agent_kind": kind })
        .sort(doc! { "updated_at": -1 })
        .await?
        .try_collect()
        .await?;

    let out = docs.iter().map(conv_doc_to_out).collect();
    Ok(Json(out))
}

/// `POST /conversations` — create a new conversation. `title` defaults to
/// `"New conversation"` and is replaced by the first user message's content on
/// the initial `persist_user_message` call.  When `agent_kind` is supplied and
/// non-empty the conversation is tagged as an agent conversation and excluded from
/// `GET /conversations`.
#[post("/conversations", data = "<body>")]
pub async fn create_conversation(
    state: &State<AppState>,
    user: AuthUser,
    body: Json<CreateConversationBody>,
) -> AppResult<Json<ConversationOut>> {
    let title = body
        .title
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or("New conversation")
        .to_string();
    let now = BsonDateTime::now();
    let id = ObjectId::new();

    // Normalise the agent_kind: trim and treat empty string as absent.
    let agent_kind_out: Option<String> = body
        .agent_kind
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string);

    let mut insert_doc = doc! {
        "_id": id,
        "user_id": &user.id,
        "title": &title,
        "created_at": now,
        "updated_at": now,
    };
    if let Some(k) = &agent_kind_out {
        insert_doc.insert("agent_kind", k.as_str());
    }

    state.db.chat_conversations().insert_one(insert_doc).await?;

    Ok(Json(ConversationOut {
        id: id.to_hex(),
        title,
        agent_kind: agent_kind_out,
        created_at: millis_to_iso(now.timestamp_millis()),
        updated_at: millis_to_iso(now.timestamp_millis()),
    }))
}

/// `PATCH /conversations/<id>` — rename a conversation (ownership-checked).
#[patch("/conversations/<id>", data = "<body>")]
pub async fn rename_conversation(
    state: &State<AppState>,
    user: AuthUser,
    id: &str,
    body: Json<RenameConversationBody>,
) -> AppResult<Json<ConversationOut>> {
    let oid = parse_oid(id)?;

    let conv = state
        .db
        .chat_conversations()
        .find_one(doc! { "_id": oid, "user_id": &user.id })
        .await?
        .ok_or(AppError::NotFound)?;

    let now = BsonDateTime::now();
    state
        .db
        .chat_conversations()
        .update_one(
            doc! { "_id": oid },
            doc! { "$set": { "title": &body.title, "updated_at": now } },
        )
        .await?;

    Ok(Json(ConversationOut {
        id: id.to_string(),
        title: body.title.clone(),
        agent_kind: conv.get_str("agent_kind").ok().map(str::to_string),
        created_at: read_dt_field(&conv, "created_at"),
        updated_at: millis_to_iso(now.timestamp_millis()),
    }))
}

/// `DELETE /conversations/<id>` — delete a conversation and cascade to its
/// messages. Ownership-checked; returns `{"ok": true}`.
#[delete("/conversations/<id>")]
pub async fn delete_conversation(
    state: &State<AppState>,
    user: AuthUser,
    id: &str,
) -> AppResult<Json<serde_json::Value>> {
    let oid = parse_oid(id)?;

    let result = state
        .db
        .chat_conversations()
        .delete_one(doc! { "_id": oid, "user_id": &user.id })
        .await?;

    if result.deleted_count == 0 {
        return Err(AppError::NotFound);
    }

    // Cascade: delete all messages belonging to this conversation.
    state.db.chat_messages().delete_many(doc! { "conversation_id": id }).await?;

    Ok(Json(serde_json::json!({ "ok": true })))
}

/// `GET /conversations/<id>/messages` — list all messages for a conversation
/// (ownership-checked), oldest first.
#[get("/conversations/<id>/messages")]
pub async fn list_messages(
    state: &State<AppState>,
    user: AuthUser,
    id: &str,
) -> AppResult<Json<Vec<MessageOut>>> {
    let oid = parse_oid(id)?;

    // Verify ownership — 404 on mismatch to avoid leaking existence.
    state
        .db
        .chat_conversations()
        .find_one(doc! { "_id": oid, "user_id": &user.id })
        .await?
        .ok_or(AppError::NotFound)?;

    let docs: Vec<Document> = state
        .db
        .chat_messages()
        .find(doc! { "conversation_id": id })
        .sort(doc! { "created_at": 1 })
        .await?
        .try_collect()
        .await?;

    let out = docs.iter().map(msg_doc_to_out).collect();
    Ok(Json(out))
}

// ---------------------------------------------------------------------------
// pub(crate) helpers — used by `rag/routes.rs` `/chat` and `agents/routes.rs`
// ---------------------------------------------------------------------------

/// Verify that `conversation_id` exists and is owned by `user_id`. Returns the
/// `ObjectId` on success; `BadRequest` for a malformed id, `NotFound` if the
/// conversation doesn't exist or belongs to another user.
pub(crate) async fn verify_owned(
    db: &DocumentDb,
    conversation_id: &str,
    user_id: &str,
) -> AppResult<ObjectId> {
    let oid = parse_oid(conversation_id)?;
    db.chat_conversations()
        .find_one(doc! { "_id": oid, "user_id": user_id })
        .await?
        .ok_or(AppError::NotFound)?;
    Ok(oid)
}

/// Clip `s` to at most `max_bytes` (on a char boundary), appending a marker when
/// truncated. Applied to message content before persisting (plan 22.7 hygiene) —
/// keeps one runaway generation from bloating a conversation doc indefinitely.
pub(crate) fn clip_message_bytes(s: &str, max_bytes: usize) -> String {
    if s.len() <= max_bytes {
        return s.to_string();
    }
    let mut end = max_bytes.min(s.len());
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}\n[truncated]", &s[..end])
}

/// Insert a user message. If this is the conversation's **first** user message,
/// also auto-titles the conversation to `truncate(content, 60)`. Bumps `updated_at`.
pub(crate) async fn persist_user_message(
    db: &DocumentDb,
    conversation_id: &str,
    user_id: &str,
    content: &str,
) -> AppResult<()> {
    let oid = parse_oid(conversation_id)?;

    // Count user messages before inserting to decide whether to auto-title.
    let existing = db
        .chat_messages()
        .count_documents(doc! { "conversation_id": conversation_id, "role": "user" })
        .await?;

    let now = BsonDateTime::now();
    db.chat_messages()
        .insert_one(doc! {
            "_id": ObjectId::new(),
            "conversation_id": conversation_id,
            "user_id": user_id,
            "role": "user",
            "content": content,
            "created_at": now,
        })
        .await?;

    // Build the update: auto-title on first message, always bump updated_at.
    let update = if existing == 0 {
        let title = truncate_title(content);
        doc! { "$set": { "title": title, "updated_at": now } }
    } else {
        doc! { "$set": { "updated_at": now } }
    };
    db.chat_conversations().update_one(doc! { "_id": oid }, update).await?;

    Ok(())
}

/// Insert an assistant message (with citations stored as a JSON string) and bump
/// the conversation's `updated_at`. Used by the Stage 6 `/chat` route.
pub(crate) async fn persist_assistant_message(
    db: &DocumentDb,
    conversation_id: &str,
    user_id: &str,
    content: &str,
    citations: &[crate::retrieval::Passage],
) -> AppResult<()> {
    let oid = parse_oid(conversation_id)?;

    // Serialize citations as a compact JSON string — avoids BSON round-trip issues
    // with serde_json::Value and keeps the schema simple.
    let citations_json =
        serde_json::to_string(citations).unwrap_or_else(|_| "[]".to_string());

    let now = BsonDateTime::now();
    db.chat_messages()
        .insert_one(doc! {
            "_id": ObjectId::new(),
            "conversation_id": conversation_id,
            "user_id": user_id,
            "role": "assistant",
            "content": content,
            "citations_json": citations_json,
            "created_at": now,
        })
        .await?;

    db.chat_conversations()
        .update_one(doc! { "_id": oid }, doc! { "$set": { "updated_at": now } })
        .await?;

    Ok(())
}

/// Insert an agent assistant message and bump `updated_at`. Used by `/agents/<kind>`.
///
/// **Semantic path** (PatientLookup, Summarize, Chat): pass the reranked `citations`
/// and `structured_json = None` — stores `citations_json`.
/// **Structured path** (HealthQuery, Trends): pass `citations = &[]` and
/// `structured_json = Some(r#"{"spec":…,"rows":…,"pipeline":…}"#)` assembled from the
/// three already-serialized JSON strings. The `structured_json` key replaces
/// `citations_json` so the two branches stay cleanly separated on read.
pub(crate) async fn persist_agent_assistant_message(
    db: &DocumentDb,
    conversation_id: &str,
    user_id: &str,
    content: &str,
    agent_kind: &str,
    citations: &[crate::retrieval::Passage],
    structured_json: Option<&str>,
) -> AppResult<()> {
    let oid = parse_oid(conversation_id)?;
    let now = BsonDateTime::now();

    let mut msg_doc = doc! {
        "_id": ObjectId::new(),
        "conversation_id": conversation_id,
        "user_id": user_id,
        "role": "assistant",
        "content": content,
        "agent_kind": agent_kind,
        "created_at": now,
    };

    if let Some(sj) = structured_json {
        // Structured path: store the aggregation spec/rows/pipeline for chart replay.
        msg_doc.insert("structured_json", sj);
    } else {
        // Semantic path: store citations (may be an empty array for the refuse path).
        let citations_json =
            serde_json::to_string(citations).unwrap_or_else(|_| "[]".to_string());
        msg_doc.insert("citations_json", citations_json);
    }

    db.chat_messages().insert_one(msg_doc).await?;

    db.chat_conversations()
        .update_one(doc! { "_id": oid }, doc! { "$set": { "updated_at": now } })
        .await?;

    Ok(())
}

// ---------------------------------------------------------------------------
// Private helpers
// ---------------------------------------------------------------------------

/// Parse an ObjectId hex string, returning `AppError::BadRequest` on failure.
/// `pub(crate)` so `memory.rs` can reuse it instead of calling `ObjectId::parse_str`
/// directly.
pub(crate) fn parse_oid(id: &str) -> AppResult<ObjectId> {
    ObjectId::parse_str(id)
        .map_err(|_| AppError::BadRequest(format!("invalid conversation id: {id}")))
}

/// Truncate `s` at 60 chars (on char boundary), appending `…` if longer.
fn truncate_title(s: &str) -> String {
    let count = s.chars().count();
    if count > 60 {
        let truncated: String = s.chars().take(60).collect();
        format!("{truncated}\u{2026}")
    } else {
        s.to_string()
    }
}

/// Convert a Unix milliseconds timestamp to an ISO-8601 string.
fn millis_to_iso(millis: i64) -> String {
    chrono::DateTime::<chrono::Utc>::from_timestamp_millis(millis)
        .map(|dt| dt.to_rfc3339())
        .unwrap_or_default()
}

/// Read a BSON DateTime field from a document and return an ISO-8601 string.
fn read_dt_field(d: &Document, key: &str) -> String {
    d.get_datetime(key)
        .ok()
        .and_then(|dt| {
            chrono::DateTime::<chrono::Utc>::from_timestamp_millis(dt.timestamp_millis())
                .map(|t| t.to_rfc3339())
        })
        .unwrap_or_default()
}

/// Build a `ConversationOut` from a raw BSON document.
fn conv_doc_to_out(d: &Document) -> ConversationOut {
    ConversationOut {
        id: d.get_object_id("_id").map(|o| o.to_hex()).unwrap_or_default(),
        title: d.get_str("title").unwrap_or("").to_string(),
        agent_kind: d.get_str("agent_kind").ok().map(str::to_string),
        created_at: read_dt_field(d, "created_at"),
        updated_at: read_dt_field(d, "updated_at"),
    }
}

/// Build a `MessageOut` from a raw BSON document.
/// `citations_json` (semantic path) and `structured_json` (structured path) are stored
/// as compact JSON strings; absent or malformed → `None` in the output.
fn msg_doc_to_out(d: &Document) -> MessageOut {
    let citations = d
        .get_str("citations_json")
        .ok()
        .and_then(|s| serde_json::from_str::<Vec<crate::retrieval::Passage>>(s).ok());

    let structured = d
        .get_str("structured_json")
        .ok()
        .and_then(|s| serde_json::from_str::<StructuredResult>(s).ok());

    let agent_kind = d.get_str("agent_kind").ok().map(str::to_string);

    MessageOut {
        id: d.get_object_id("_id").map(|o| o.to_hex()).unwrap_or_default(),
        role: d.get_str("role").unwrap_or("").to_string(),
        content: d.get_str("content").unwrap_or("").to_string(),
        citations,
        agent_kind,
        structured,
        created_at: read_dt_field(d, "created_at"),
    }
}
