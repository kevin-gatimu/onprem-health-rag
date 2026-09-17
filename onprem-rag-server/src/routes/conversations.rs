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

use crate::agents::kind::AgentMode;
use crate::auth::guard::AuthUser;
use crate::documentdb::DocumentDb;
use crate::error::{AppError, AppResult};
use crate::memory::FOCUS_FIELD;
use crate::memory::focus::ConversationFocus;
use crate::ontology::ServiceLine;
use crate::rag::ChatTurn;
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
    /// Service line the conversation is pinned to, as a slug (plan 07 §4
    /// "Open in {Line}"). Absent on conversations created before plan 06 and on
    /// unpinned ones — the client treats absent as "not pinned", never as a
    /// default line.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub service_line: Option<String>,
    /// Agent mode (`ask` | `trends` | `handover`) the conversation was left in.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mode: Option<String>,
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

#[derive(Debug, Serialize, Deserialize)]
pub struct SqlResult {
    pub source_id: String,
    pub sql: String,
    pub columns: Vec<String>,
    pub rows: Vec<Vec<serde_json::Value>>,
}

/// One follow-up suggestion chip persisted with an assistant message (plan 06 §5).
///
/// Defined here rather than in `answer/suggest.rs` because this is the **wire and
/// storage** shape, and it must match the client mirror
/// (`src-tauri/src/commands.rs::Suggestion`, `src/lib/bridge.ts::Suggestion`)
/// field for field. The generator, when it lands, serialises into this.
///
/// `kind` is a string rather than an enum on purpose: the client already accepts
/// `"drill" | "widen" | "compare" | "switch" | "explain" | (string & {})`, and a
/// Rust enum would serialise a future variant as a hard parse failure on reload
/// of an older message instead of an unknown-but-displayable chip.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SuggestionOut {
    pub text: String,
    pub kind: String,
    /// Pre-bound `QuerySpec` the chip re-runs, passed straight back as
    /// `suggestion_spec` on click. Opaque JSON here — this module does not need
    /// to understand the IR to store it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub spec: Option<serde_json::Value>,
    /// Owning agent slug, when clicking the chip switches tabs.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent: Option<String>,
}

/// A clarification the router posed, persisted so a reload shows the question
/// and its options rather than a blank assistant turn (plan 06 §4).
///
/// Matches `ClarifyPayload` in both client mirrors.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ClarifyOut {
    pub question: String,
    /// `MissingSlot` as a snake_case slug (`subject`, `time_range`, …).
    pub slot: String,
    #[serde(default)]
    pub options: Vec<String>,
}

#[derive(Debug, Serialize)]
pub struct MessageOut {
    pub id: String,
    pub role: String,
    pub content: String,
    /// Null for user messages; the reranked passages for semantic assistant messages.
    pub citations: Option<Vec<crate::retrieval::Passage>>,
    /// Post-generation grounding verification for plain chat assistant messages.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub verify: Option<crate::verify::VerifyReport>,
    /// Routed kind for agent assistant messages; `None` for user messages and plain chat.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub agent_kind: Option<String>,
    /// Agent mode (`ask` | `trends` | `handover`) the answer was produced in.
    /// `None` on messages written before plan 05.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mode: Option<String>,
    /// Structured aggregation result for structured-path agent assistant messages.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub structured: Option<StructuredResult>,
    /// Exact operational SQL result used for a text-to-SQL chat answer.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sql_result: Option<SqlResult>,
    /// Follow-up suggestion chips generated with this answer (plan 06 §5).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub suggestions: Option<Vec<SuggestionOut>>,
    /// Focus slot **names** the answer borrowed from (`["patient","time_range"]`).
    ///
    /// Names only, never values: this crosses to the client, and a patient key or
    /// a ward name here would put PHI in a payload whose only job is to render a
    /// "using: …" chip. The values stay server-side in the focus document.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub focus_used: Option<Vec<String>>,
    /// Execution provenance (plan 04 §6): the rung ladder, backend, and timings.
    ///
    /// Passed through as opaque JSON exactly as `answer::provenance::Provenance`
    /// serialised it. Re-typing it here would create a second definition of a
    /// wire format that has already been pinned by a literal-JSON assertion —
    /// and a divergence between the two would fail silently, because the field is
    /// `Option` on both sides.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub provenance: Option<serde_json::Value>,
    /// The `QuerySpec` IR behind a structured answer (plan 03), opaque here.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub spec: Option<serde_json::Value>,
    /// The clarification this assistant turn posed, when it posed one.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub clarify: Option<ClarifyOut>,
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

/// `PATCH /conversations/<id>` body.
///
/// Every field is optional and applied only when present, so the pre-plan-06
/// caller that sends `{"title": "..."}` behaves exactly as before. At least one
/// field must be present — an empty patch is a client bug, and answering `200`
/// to it would hide the bug rather than surface it.
#[derive(Debug, Default, Deserialize)]
pub struct RenameConversationBody {
    #[serde(default)]
    pub title: Option<String>,
    /// Service-line slug to pin the conversation to (plan 07 §4 "Open in {Line}").
    /// Validated against `ServiceLine::ALL`; `Some("")` clears the pin.
    #[serde(default)]
    pub service_line: Option<String>,
    /// Agent mode: `ask` | `trends` | `handover`. `Some("")` clears it.
    #[serde(default)]
    pub mode: Option<String>,
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
        .sort(doc! { "updated_at": -1, "_id": -1 })
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
    // Accept both the new slugs and the legacy mechanism names for one release:
    // a conversation stored as `health_query` must still list under `ask`.
    // Matching is by name through the same table the endpoint uses, never by
    // position in a list of candidates.
    let mut kinds: Vec<String> = vec![kind.to_string()];
    if let Some((parsed, _)) = crate::agents::kind::AgentKind::parse(kind) {
        let slug = parsed.slug().to_string();
        if !kinds.contains(&slug) {
            kinds.push(slug);
        }
        for (legacy, legacy_kind, _) in crate::agents::kind::LEGACY_KINDS {
            if *legacy_kind == parsed && !kinds.iter().any(|k| k == legacy) {
                kinds.push((*legacy).to_string());
            }
        }
    }

    let docs: Vec<Document> = state
        .db
        .chat_conversations()
        .find(doc! { "user_id": &user.id, "agent_kind": { "$in": kinds } })
        .sort(doc! { "updated_at": -1, "_id": -1 })
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
        service_line: None,
        mode: None,
        created_at: millis_to_iso(now.timestamp_millis()),
        updated_at: millis_to_iso(now.timestamp_millis()),
    }))
}

/// `PATCH /conversations/<id>` — update a conversation's title, pinned service
/// line, or agent mode (ownership-checked).
///
/// Additive by construction: each field is applied only when the body carries it,
/// so a pre-plan-06 client sending `{"title": "..."}` produces byte-identical
/// behaviour to before. The service line and mode were added because plan 07 §4's
/// "Open in {Line}" has to survive a reload, and the conversation document is the
/// only per-conversation store that does.
///
/// Both slugs are validated against the ontology rather than stored as free text:
/// an unknown line persisted here would later be read back by
/// `ServiceLine::from_slug` as `None` and silently degrade to "unpinned", which is
/// indistinguishable from a client bug. An empty string is the explicit "clear"
/// signal and unsets the field.
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

    let (set, unset) = build_patch_update(&body)?;
    if set.is_empty() && unset.is_empty() {
        return Err(AppError::BadRequest(
            "patch body must set at least one of title, service_line, mode".into(),
        ));
    }

    let now = BsonDateTime::now();
    let mut set = set;
    set.insert("updated_at", now);

    let mut update = doc! { "$set": set };
    if !unset.is_empty() {
        update.insert("$unset", unset);
    }

    state
        .db
        .chat_conversations()
        .update_one(doc! { "_id": oid }, update)
        .await?;

    // Project the post-update state without a second read: apply exactly the
    // fields the patch touched over the document we already fetched.
    let title = body
        .title
        .clone()
        .unwrap_or_else(|| conv.get_str("title").unwrap_or("").to_string());
    let service_line = patched_field(&body.service_line, &conv, "service_line");
    let mode = patched_field(&body.mode, &conv, "mode");

    Ok(Json(ConversationOut {
        id: id.to_string(),
        title,
        agent_kind: conv.get_str("agent_kind").ok().map(str::to_string),
        service_line,
        mode,
        created_at: read_dt_field(&conv, "created_at"),
        updated_at: millis_to_iso(now.timestamp_millis()),
    }))
}

/// Split a `PATCH` body into the `$set` and `$unset` sub-documents it implies.
///
/// Pulled out of the handler so the validation and the additive semantics are
/// unit-testable without a database (the rule "a body with only `title` must
/// behave exactly as before" is an assertion, not a comment).
fn build_patch_update(body: &RenameConversationBody) -> AppResult<(Document, Document)> {
    let mut set = Document::new();
    let mut unset = Document::new();

    if let Some(title) = &body.title {
        set.insert("title", title.clone());
    }

    if let Some(raw) = &body.service_line {
        let slug = raw.trim();
        if slug.is_empty() {
            unset.insert("service_line", "");
        } else {
            let line = ServiceLine::from_slug(slug)
                .ok_or_else(|| AppError::BadRequest(format!("unknown service line: {slug}")))?;
            set.insert("service_line", line.slug());
        }
    }

    if let Some(raw) = &body.mode {
        let slug = raw.trim();
        if slug.is_empty() {
            unset.insert("mode", "");
        } else {
            let mode = AgentMode::from_slug(slug)
                .ok_or_else(|| AppError::BadRequest(format!("unknown mode: {slug}")))?;
            set.insert("mode", mode.slug());
        }
    }

    Ok((set, unset))
}

/// The value a patched string field holds after the update: the patch value when
/// present and non-empty, `None` when the patch cleared it, otherwise whatever
/// the stored document already had.
fn patched_field(patch: &Option<String>, conv: &Document, field: &str) -> Option<String> {
    match patch {
        Some(raw) if raw.trim().is_empty() => None,
        Some(raw) => Some(raw.trim().to_string()),
        None => conv.get_str(field).ok().map(str::to_string),
    }
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
    state
        .db
        .chat_messages()
        .delete_many(doc! { "conversation_id": id })
        .await?;

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
        .sort(doc! { "created_at": 1, "_id": 1 })
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

/// Load recent messages in ascending order for history-aware generation.
pub(crate) async fn load_history(
    db: &DocumentDb,
    conversation_id: &str,
    user_id: &str,
    limit: i64,
) -> Vec<ChatTurn> {
    let oid = match parse_oid(conversation_id) {
        Ok(oid) => oid,
        Err(_) => return Vec::new(),
    };
    let owned = db
        .chat_conversations()
        .find_one(doc! { "_id": oid, "user_id": user_id })
        .await
        .ok()
        .flatten()
        .is_some();
    if !owned {
        return Vec::new();
    }

    let docs: Vec<Document> = match db
        .chat_messages()
        .find(doc! { "conversation_id": conversation_id })
        .sort(doc! { "created_at": -1, "_id": -1 })
        .limit(limit)
        .await
    {
        Ok(cursor) => cursor.try_collect().await.unwrap_or_default(),
        Err(_) => return Vec::new(),
    };

    let mut turns: Vec<ChatTurn> = docs
        .iter()
        .filter_map(|document| {
            Some(ChatTurn {
                role: document.get_str("role").ok()?.to_string(),
                content: document.get_str("content").ok()?.to_string(),
            })
        })
        .collect();
    turns.reverse();
    turns
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

    // A retry re-posts the question the failed run already persisted. Without this
    // the transcript grows a second (and third) copy of the same unanswered turn —
    // visible to the user, and fed to the follow-up rewriter as if they had really
    // asked twice. Only the newest message can match: an assistant reply in between
    // means this is a genuine repeat of an answered question.
    if newest_message_is(db, conversation_id, "user", content).await? {
        db.chat_conversations()
            .update_one(
                doc! { "_id": oid },
                doc! { "$set": { "updated_at": BsonDateTime::now() } },
            )
            .await?;
        return Ok(());
    }

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
    db.chat_conversations()
        .update_one(doc! { "_id": oid }, update)
        .await?;

    Ok(())
}

/// Prefix the conversation title with the department that answered
/// (plan 05 section 7): "Maternity · Deliveries last month".
///
/// Only applied to the **first** assistant message of a conversation, and only
/// when the title does not already carry a prefix -- a later turn routed to a
/// different department must not rename the thread. Failure is non-fatal: a
/// title is cosmetic and must never cost an answer.
pub(crate) async fn prefix_title_with_line(
    db: &DocumentDb,
    conversation_id: &str,
    line_label: &str,
) -> AppResult<()> {
    let oid = parse_oid(conversation_id)?;
    let assistant_messages = db
        .chat_messages()
        .count_documents(doc! { "conversation_id": conversation_id, "role": "assistant" })
        .await?;
    if assistant_messages != 1 {
        return Ok(());
    }
    let Some(conv) = db.chat_conversations().find_one(doc! { "_id": oid }).await? else {
        return Ok(());
    };
    let title = conv.get_str("title").unwrap_or("");
    if title.is_empty() || title.contains(TITLE_SEPARATOR) {
        return Ok(());
    }
    let prefixed = truncate_title(&format!("{line_label}{TITLE_SEPARATOR}{title}"));
    db.chat_conversations()
        .update_one(doc! { "_id": oid }, doc! { "$set": { "title": prefixed } })
        .await?;
    Ok(())
}

/// Whether the conversation's most recent message has this exact role and content.
/// Used to recognise a retry of a turn that never got an answer.
async fn newest_message_is(
    db: &DocumentDb,
    conversation_id: &str,
    role: &str,
    content: &str,
) -> AppResult<bool> {
    let newest = db
        .chat_messages()
        .find_one(doc! { "conversation_id": conversation_id })
        .sort(doc! { "created_at": -1, "_id": -1 })
        .await?;
    Ok(newest.is_some_and(|message| {
        message.get_str("role") == Ok(role) && message.get_str("content") == Ok(content)
    }))
}

/// Insert an assistant message (with citations stored as a JSON string) and bump
/// the conversation's `updated_at`. Used by the Stage 6 `/chat` route.
pub(crate) async fn persist_assistant_message(
    db: &DocumentDb,
    conversation_id: &str,
    user_id: &str,
    content: &str,
    citations: &[crate::retrieval::Passage],
    verify: Option<&crate::verify::VerifyReport>,
) -> AppResult<()> {
    let oid = parse_oid(conversation_id)?;

    // Serialize citations as a compact JSON string — avoids BSON round-trip issues
    // with serde_json::Value and keeps the schema simple.
    let citations_json = serde_json::to_string(citations).unwrap_or_else(|_| "[]".to_string());

    let now = BsonDateTime::now();
    let mut message = doc! {
        "_id": ObjectId::new(),
        "conversation_id": conversation_id,
        "user_id": user_id,
        "role": "assistant",
        "content": content,
        "citations_json": citations_json,
        "created_at": now,
    };
    if let Some(report) = verify {
        let verify_json = serde_json::to_string(report)
            .map_err(|e| crate::error::AppError::Internal(e.to_string()))?;
        message.insert("verify_json", verify_json);
    }
    db.chat_messages().insert_one(message).await?;

    db.chat_conversations()
        .update_one(doc! { "_id": oid }, doc! { "$set": { "updated_at": now } })
        .await?;

    Ok(())
}

pub(crate) async fn persist_sql_assistant_message(
    db: &DocumentDb,
    conversation_id: &str,
    user_id: &str,
    content: &str,
    result: &SqlResult,
) -> AppResult<()> {
    let oid = parse_oid(conversation_id)?;
    let now = BsonDateTime::now();
    let sql_result_json = serde_json::to_string(result)
        .map_err(|error| crate::error::AppError::Internal(error.to_string()))?;
    db.chat_messages()
        .insert_one(doc! {
            "_id": ObjectId::new(),
            "conversation_id": conversation_id,
            "user_id": user_id,
            "role": "assistant",
            "content": content,
            "citations_json": "[]",
            "sql_result_json": sql_result_json,
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
    mode: &str,
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
        "mode": mode,
        "created_at": now,
    };

    if let Some(sj) = structured_json {
        // Structured path: store the aggregation spec/rows/pipeline for chart replay.
        msg_doc.insert("structured_json", sj);
    } else {
        // Semantic path: store citations (may be an empty array for the refuse path).
        let citations_json = serde_json::to_string(citations).unwrap_or_else(|_| "[]".to_string());
        msg_doc.insert("citations_json", citations_json);
    }

    db.chat_messages().insert_one(msg_doc).await?;

    db.chat_conversations()
        .update_one(doc! { "_id": oid }, doc! { "$set": { "updated_at": now } })
        .await?;

    Ok(())
}

/// Plan-06 extras attached to an assistant message after the answer is built.
///
/// A separate struct with a separate write, rather than five more parameters on
/// each of the three `persist_*_assistant_message` functions: those are called
/// from `rag/routes.rs` and `agents/routes.rs`, and widening their signatures
/// would force every existing call site to change for fields most of them never
/// set. Callers persist the message as they do today, then make one extra call.
#[derive(Debug, Default)]
pub(crate) struct AnswerExtras<'a> {
    /// Follow-up chips (plan 06 section 5). `None` and `Some(&[])` both store nothing.
    pub suggestions: Option<&'a [SuggestionOut]>,
    /// Focus slot **names** used, never values (see `MessageOut::focus_used`).
    pub focus_used: &'a [String],
    /// `answer::provenance::Provenance` already serialised. Opaque here.
    pub provenance: Option<serde_json::Value>,
    /// The `QuerySpec` behind the answer, already serialised. Opaque here.
    pub spec: Option<serde_json::Value>,
    /// The clarification this turn posed, if it posed one.
    pub clarify: Option<&'a ClarifyOut>,
}

impl AnswerExtras<'_> {
    /// Whether there is anything at all to write -- lets the caller skip the
    /// round-trip on the common path where the answer carried no extras.
    pub(crate) fn is_empty(&self) -> bool {
        self.suggestions.is_none_or(|s| s.is_empty())
            && self.focus_used.is_empty()
            && self.provenance.is_none()
            && self.spec.is_none()
            && self.clarify.is_none()
    }
}

/// Attach plan-06 extras to the conversation's newest assistant message.
///
/// Targets the newest assistant message rather than an id because the existing
/// `persist_*_assistant_message` helpers do not return one, and the caller writes
/// the message immediately before calling this.
pub(crate) async fn attach_answer_extras(
    db: &DocumentDb,
    conversation_id: &str,
    extras: &AnswerExtras<'_>,
) -> AppResult<()> {
    if extras.is_empty() {
        return Ok(());
    }
    let Some(newest) = db
        .chat_messages()
        .find_one(doc! { "conversation_id": conversation_id, "role": "assistant" })
        .sort(doc! { "created_at": -1, "_id": -1 })
        .await?
    else {
        return Ok(());
    };
    let Ok(oid) = newest.get_object_id("_id") else {
        return Ok(());
    };

    let set = extras_set_doc(extras);
    if set.is_empty() {
        return Ok(());
    }
    db.chat_messages()
        .update_one(doc! { "_id": oid }, doc! { "$set": set })
        .await?;
    Ok(())
}

/// The `$set` document `attach_answer_extras` writes. Split out so the storage
/// encoding is testable without a database.
fn extras_set_doc(extras: &AnswerExtras<'_>) -> Document {
    let mut set = Document::new();

    if let Some(suggestions) = extras.suggestions.filter(|s| !s.is_empty()) {
        if let Ok(json) = serde_json::to_string(suggestions) {
            set.insert("suggestions_json", json);
        }
    }
    if !extras.focus_used.is_empty() {
        set.insert("focus_used", extras.focus_used.to_vec());
    }
    if let Some(provenance) = &extras.provenance {
        set.insert("provenance_json", provenance.to_string());
    }
    if let Some(spec) = &extras.spec {
        set.insert("spec_json", spec.to_string());
    }
    if let Some(clarify) = extras.clarify {
        if let Ok(json) = serde_json::to_string(clarify) {
            set.insert("clarify_json", json);
        }
    }
    set
}

/// Persist the updated `ConversationFocus` on the conversation document
/// (plan 06 section 2).
///
/// Stored as a compact JSON **string** under `focus_json`, matching this module's
/// existing `citations_json` / `structured_json` / `verify_json` convention. Plan
/// section 2 says "as `focus` (BSON)"; a nested `QuerySpec` round-trips exactly
/// through serde_json and only approximately through BSON (integer widening,
/// `f64`/`i64` coercion, map-key constraints), and a focus that silently changes
/// shape on reload is worse than one extra `to_string`.
///
/// An empty focus unsets the field rather than storing `{}`, so a reset really
/// removes the stored state instead of leaving a husk that reads back as present.
pub(crate) async fn persist_focus(
    db: &DocumentDb,
    conversation_id: &str,
    focus: &ConversationFocus,
) -> AppResult<()> {
    let oid = parse_oid(conversation_id)?;
    if focus.is_empty() {
        db.chat_conversations()
            .update_one(doc! { "_id": oid }, doc! { "$unset": { FOCUS_FIELD: "" } })
            .await?;
        return Ok(());
    }
    // A focus that will not serialise is a bug, not a runtime condition worth an
    // error page: the turn already succeeded. Skip the write and keep the previous
    // focus rather than failing the request over a memory update.
    let Some(json) = focus.to_json_string() else {
        return Ok(());
    };
    db.chat_conversations()
        .update_one(doc! { "_id": oid }, doc! { "$set": { FOCUS_FIELD: json } })
        .await?;
    Ok(())
}

/// Insert the assistant turn for a clarification and store the partial spec with
/// it (plan 06 section 4).
///
/// The partial spec is persisted on the *message*, not only in the focus, because
/// section 4 allows the fill to come from "the stored partial spec on the clarify
/// message": the focus can be superseded by a concurrent turn, whereas the
/// message that asked the question cannot.
pub(crate) async fn persist_clarify_message(
    db: &DocumentDb,
    conversation_id: &str,
    user_id: &str,
    clarify: &ClarifyOut,
    partial_spec: Option<&serde_json::Value>,
) -> AppResult<()> {
    let oid = parse_oid(conversation_id)?;
    let now = BsonDateTime::now();
    let clarify_json =
        serde_json::to_string(clarify).map_err(|e| AppError::Internal(e.to_string()))?;

    let mut message = doc! {
        "_id": ObjectId::new(),
        "conversation_id": conversation_id,
        "user_id": user_id,
        "role": "assistant",
        "content": clarify.question.clone(),
        "citations_json": "[]",
        "clarify_json": clarify_json,
        "created_at": now,
    };
    if let Some(spec) = partial_spec {
        message.insert("spec_json", spec.to_string());
    }
    db.chat_messages().insert_one(message).await?;
    db.chat_conversations()
        .update_one(doc! { "_id": oid }, doc! { "$set": { "updated_at": now } })
        .await?;
    Ok(())
}

/// Read back the partial spec stored on the conversation's most recent clarify
/// message (plan 06 section 4's fallback source for the slot fill).
///
/// Returns raw JSON: this module deliberately does not depend on the IR types, so
/// the caller in `router/focus_resolve.rs` deserialises into `QuerySpec`.
pub(crate) async fn load_pending_clarify_spec(
    db: &DocumentDb,
    conversation_id: &str,
) -> Option<serde_json::Value> {
    let newest = db
        .chat_messages()
        .find_one(doc! { "conversation_id": conversation_id, "role": "assistant" })
        .sort(doc! { "created_at": -1, "_id": -1 })
        .await
        .ok()
        .flatten()?;
    // Only the *newest* assistant message counts: an older clarify has already
    // been answered or abandoned, and reviving its spec would answer a question
    // the user has moved on from.
    newest.get_str("clarify_json").ok()?;
    let spec = newest.get_str("spec_json").ok()?;
    serde_json::from_str(spec).ok()
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

/// Separator between the answering department and the question in a title.
const TITLE_SEPARATOR: &str = " · ";

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
        id: d
            .get_object_id("_id")
            .map(|o| o.to_hex())
            .unwrap_or_default(),
        title: d.get_str("title").unwrap_or("").to_string(),
        agent_kind: d.get_str("agent_kind").ok().map(str::to_string),
        service_line: d.get_str("service_line").ok().map(str::to_string),
        mode: d.get_str("mode").ok().map(str::to_string),
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

    let sql_result = d
        .get_str("sql_result_json")
        .ok()
        .and_then(|s| serde_json::from_str::<SqlResult>(s).ok());

    let verify = d
        .get_str("verify_json")
        .ok()
        .and_then(|s| serde_json::from_str::<crate::verify::VerifyReport>(s).ok());

    let agent_kind = d.get_str("agent_kind").ok().map(str::to_string);
    let mode = d.get_str("mode").ok().map(str::to_string);

    let suggestions = d
        .get_str("suggestions_json")
        .ok()
        .and_then(|s| serde_json::from_str::<Vec<SuggestionOut>>(s).ok())
        .filter(|s| !s.is_empty());

    // Focus slot names are stored as a plain BSON array -- no JSON string, because
    // the value is a flat list of short ASCII identifiers with no nesting to lose.
    let focus_used = d
        .get_array("focus_used")
        .ok()
        .map(|values| {
            values
                .iter()
                .filter_map(|v| v.as_str().map(str::to_string))
                .collect::<Vec<String>>()
        })
        .filter(|names| !names.is_empty());

    // Provenance and spec are re-emitted exactly as the producer serialised them.
    // Parsing into a local type and re-serialising would risk changing the
    // encoding of a wire shape that is pinned elsewhere by a literal-JSON
    // assertion -- and `Provenance` is Serialize-only, so it could not round-trip
    // even if that were wanted.
    let provenance = d
        .get_str("provenance_json")
        .ok()
        .and_then(|s| serde_json::from_str::<serde_json::Value>(s).ok());

    let spec = d
        .get_str("spec_json")
        .ok()
        .and_then(|s| serde_json::from_str::<serde_json::Value>(s).ok());

    let clarify = d
        .get_str("clarify_json")
        .ok()
        .and_then(|s| serde_json::from_str::<ClarifyOut>(s).ok());

    MessageOut {
        id: d
            .get_object_id("_id")
            .map(|o| o.to_hex())
            .unwrap_or_default(),
        role: d.get_str("role").unwrap_or("").to_string(),
        content: d.get_str("content").unwrap_or("").to_string(),
        citations,
        verify,
        agent_kind,
        mode,
        structured,
        sql_result,
        suggestions,
        focus_used,
        provenance,
        spec,
        clarify,
        created_at: read_dt_field(d, "created_at"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::verify::VerifyStatus;

    #[test]
    fn stored_sql_result_is_returned_with_message() {
        let result = serde_json::json!({
            "source_id": "source-1",
            "sql": "SELECT COUNT(*) FROM patients",
            "columns": ["count"],
            "rows": [[42]]
        });
        let document = doc! {
            "role": "assistant",
            "content": "There are 42 patients.",
            "sql_result_json": result.to_string(),
        };

        let message = msg_doc_to_out(&document);
        let sql_result = message.sql_result.expect("SQL result");
        assert_eq!(sql_result.source_id, "source-1");
        assert_eq!(sql_result.columns, vec!["count"]);
        assert_eq!(sql_result.rows[0][0], serde_json::json!(42));
    }

    #[test]
    fn stored_verification_is_returned_with_message() {
        let report = serde_json::json!({
            "status": "partial",
            "claims": [],
            "unsupported": 1,
            "citation_overflow": [3]
        });
        let document = doc! {
            "role": "assistant",
            "content": "answer",
            "verify_json": report.to_string(),
        };

        let message = msg_doc_to_out(&document);
        let verification = message.verify.expect("verification report");
        assert_eq!(verification.status, VerifyStatus::Partial);
        assert_eq!(verification.unsupported, 1);
        assert_eq!(verification.citation_overflow, vec![3]);
    }
}
