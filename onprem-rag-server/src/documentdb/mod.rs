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
/// nl2sql: versioned table cards with embedded schema text + cardVector.
pub const SCHEMA_CATALOG: &str = "schema_catalog";
/// One active schema-catalog version and refresh status per source.
pub const SCHEMA_CATALOG_STATE: &str = "schema_catalog_state";
/// Bounded operational history of metadata checks and refreshes.
pub const SCHEMA_CATALOG_HISTORY: &str = "schema_catalog_history";
/// Admin-curated aliases and undeclared relationships, independent of generations.
pub const SCHEMA_METADATA_OVERRIDES: &str = "schema_metadata_overrides";
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
        self.db
            .run_command(mongodb::bson::doc! { "ping": 1 })
            .await?;
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

    pub fn schema_catalog(&self) -> Collection<Document> {
        self.collection(SCHEMA_CATALOG)
    }

    pub fn schema_catalog_state(&self) -> Collection<Document> {
        self.collection(SCHEMA_CATALOG_STATE)
    }

    pub fn schema_catalog_history(&self) -> Collection<Document> {
        self.collection(SCHEMA_CATALOG_HISTORY)
    }

    pub fn schema_metadata_overrides(&self) -> Collection<Document> {
        self.collection(SCHEMA_METADATA_OVERRIDES)
    }
}

/// Backfill generation markers for records created before versioned ingestion.
/// Idempotent and safe to run at every startup.
pub async fn ensure_records_indexes(db: &DocumentDb) -> AppResult<()> {
    db.db
        .run_command(mongodb::bson::doc! {
            "createIndexes": RECORDS,
            "indexes": [
                {
                    "name": "records_source_table_active_row",
                    "key": {
                        "source_id": 1,
                        "table": 1,
                        "active": 1,
                        "row_pk": 1,
                        "chunk_index": 1
                    }
                },
                {
                    "name": "records_source_table_active_generation",
                    "key": {
                        "source_id": 1,
                        "table": 1,
                        "active": 1,
                        "ingest_generation": 1
                    }
                }
            ]
        })
        .await?;
    Ok(())
}

pub async fn ensure_ingest_generations(db: &DocumentDb) -> AppResult<()> {
    db.records()
        .update_many(
            mongodb::bson::doc! { "active": { "$exists": false } },
            mongodb::bson::doc! { "$set": { "active": true, "ingest_generation": "legacy" } },
        )
        .await?;
    db.indexed_tables()
        .update_many(
            mongodb::bson::doc! { "active_generation": { "$exists": false }, "status": "indexed" },
            mongodb::bson::doc! { "$set": { "active_generation": "legacy", "refresh_status": "idle" } },
        )
        .await?;
    Ok(())
}

/// Mark work interrupted by a previous process exit as resumable failure.
/// Active and staged generations are retained; retry cleanup remains generation-safe.
pub async fn recover_abandoned_ingestions(db: &DocumentDb) -> AppResult<()> {
    let now = mongodb::bson::DateTime::now();

    db.indexed_tables()
        .update_many(
            mongodb::bson::doc! { "refresh_status": "indexing" },
            mongodb::bson::doc! { "$set": {
                "refresh_status": "idle",
                "recovered_at": now,
            } },
        )
        .await?;
    db.jobs()
        .update_many(
            mongodb::bson::doc! { "status": "running" },
            mongodb::bson::doc! { "$set": {
                "status": "failed",
                "finished_at": now,
                "recovery_error": "server restarted before ingestion completed",
            }, "$inc": { "errors": 1i64 } },
        )
        .await?;

    Ok(())
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
    tracing::info!(
        index = "users_email_unique",
        "ensured unique email index on users"
    );
    Ok(())
}
