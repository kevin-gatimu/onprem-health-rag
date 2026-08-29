//! DocumentDB (MongoDB-wire) connection and typed collection handles.

pub mod vector;

use crate::config::Config;
use crate::error::AppResult;
use mongodb::{Client, Collection, Database, bson::Document, options::ClientOptions};

/// Named collections used by the application.
pub const USERS: &str = "users";
pub const SOURCES: &str = "sources";
pub const RECORDS: &str = "records";
pub const JOBS: &str = "jobs";
pub const SETTINGS: &str = "settings";
/// Append-only security audit log (login, logout, admin actions, etc.).
pub const AUDIT_LOG: &str = "audit_log";
/// Per-table ingestion state: one doc per `{source_id}:{table}`, tracking status,
/// row/vector counts, and timestamps. Read by `GET /ingest/history`.
pub const INDEXED_TABLES: &str = "indexed_tables";
/// Chat: one doc per conversation, scoped by user_id.
pub const CHAT_CONVERSATIONS: &str = "chat_conversations";
/// Chat: one doc per message, linked to a conversation by conversation_id (hex string).
pub const CHAT_MESSAGES: &str = "chat_messages";

/// Thin wrapper over a connected `mongodb::Database`.
#[derive(Clone)]
pub struct DocumentDb {
    pub db: Database,
}

impl DocumentDb {
    /// Connect to DocumentDB using the configured URI. The driver connects lazily,
    /// so callers should `ping()` when they need to confirm reachability.
    pub async fn connect(config: &Config) -> AppResult<Self> {
        let options = ClientOptions::parse(&config.documentdb_uri).await?;
        let client = Client::with_options(options)?;
        let db = client.database(&config.documentdb_db);
        Ok(DocumentDb { db })
    }

    /// Best-effort round-trip to verify the server is reachable.
    pub async fn ping(&self) -> AppResult<()> {
        self.db.run_command(mongodb::bson::doc! { "ping": 1 }).await?;
        Ok(())
    }

    /// A collection handle deserialized to `T` (use `Document` for schemaless access).
    pub fn collection<T: Send + Sync>(&self, name: &str) -> Collection<T> {
        self.db.collection::<T>(name)
    }

    pub fn sources(&self) -> Collection<Document> {
        self.collection(SOURCES)
    }

    pub fn records(&self) -> Collection<Document> {
        self.collection(RECORDS)
    }

    pub fn jobs(&self) -> Collection<Document> {
        self.collection(JOBS)
    }

    pub fn settings(&self) -> Collection<Document> {
        self.collection(SETTINGS)
    }

    pub fn audit_log(&self) -> Collection<Document> {
        self.collection(AUDIT_LOG)
    }

    pub fn indexed_tables(&self) -> Collection<Document> {
        self.collection(INDEXED_TABLES)
    }

    pub fn chat_conversations(&self) -> Collection<Document> {
        self.collection(CHAT_CONVERSATIONS)
    }

    pub fn chat_messages(&self) -> Collection<Document> {
        self.collection(CHAT_MESSAGES)
    }
}

/// Ensure the chat collections have the indexes they need. Call once at boot
/// (best-effort — a failure is logged but non-fatal).
pub async fn ensure_chat_indexes(db: &DocumentDb) -> AppResult<()> {
    db.db
        .run_command(mongodb::bson::doc! {
            "createIndexes": CHAT_CONVERSATIONS,
            "indexes": [{
                "name": "chat_conv_user_updated",
                "key": { "user_id": 1, "updated_at": -1 },
            }]
        })
        .await?;
    db.db
        .run_command(mongodb::bson::doc! {
            "createIndexes": CHAT_MESSAGES,
            "indexes": [{
                "name": "chat_msg_conv_created",
                "key": { "conversation_id": 1, "created_at": 1 },
            }]
        })
        .await?;
    tracing::info!("ensured chat indexes (chat_conv_user_updated, chat_msg_conv_created)");
    Ok(())
}

/// Ensure the `users` collection has the indexes it needs. Call once at boot,
/// after `seed::seed_admin` has run the email backfill, so all users have a
/// non-empty unique email before the unique constraint is applied.
pub async fn ensure_user_indexes(db: &DocumentDb) -> AppResult<()> {
    let command = mongodb::bson::doc! {
        "createIndexes": USERS,
        "indexes": [{
            "name": "users_email_unique",
            "key": { "email": 1 },
            "unique": true,
        }]
    };
    db.db.run_command(command).await?;
    tracing::info!(index = "users_email_unique", "ensured unique email index on users");
    Ok(())
}
