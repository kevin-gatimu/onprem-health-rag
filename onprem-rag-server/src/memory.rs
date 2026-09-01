//! Conversation working memory: rolling summary + verbatim tail (plan 22).
//!
//! `/chat` and `/agents` load [`WorkingMemory`] instead of a raw N-message window so
//! long conversations stay bounded: `summary` folds everything before `summary_upto`,
//! and `tail` carries the turns since, itself capped by both message count and an
//! approximate token (word) budget. Compaction runs write-behind after a turn
//! persists — never on the request hot path — and folds the oldest tail turns into
//! `summary` via a compare-and-swap on `summary_upto` (guards concurrent compactions).

use std::sync::Arc;

use futures::TryStreamExt;
use mongodb::bson::{Document, doc, oid::ObjectId};

use crate::config::Config;
use crate::documentdb::DocumentDb;
use crate::error::{AppError, AppResult};
use crate::foundry::FoundryManager;
use crate::foundry::router::{AgentKind, ModelSpec};
use crate::rag::ChatTurn;
use crate::routes::conversations::parse_oid;

/// Batch cap on a single compaction pass's overflow fetch. Keeps the query
/// bounded even if compaction has stalled for a while (a CAS race, a transient
/// model failure) and the un-summarized tail has grown large — the next pass
/// picks up where this one left off rather than re-scanning an ever-growing set.
const MAX_COMPACTION_BATCH: i64 = 200;

/// `summary`/`summary_upto` read off a conversation doc, and the message filter
/// they imply (everything since `summary_upto`, or the whole conversation when
/// there's no summary yet). Shared by `load_working_memory` and
/// `compact_if_needed` — both need the same two reads.
fn summary_state(conv: &Document, conversation_id: &str) -> (Option<String>, Option<ObjectId>, Document) {
    let summary = conv.get_str("summary").ok().map(str::to_string);
    let summary_upto = conv.get_object_id("summary_upto").ok();
    let mut filter = doc! { "conversation_id": conversation_id };
    if let Some(upto) = summary_upto {
        filter.insert("_id", doc! { "$gt": upto });
    }
    (summary, summary_upto, filter)
}

/// Assembled per-turn context: the rolling summary (if any) plus the verbatim
/// tail of turns since the summary was last updated.
#[derive(Debug, Clone, Default)]
pub struct WorkingMemory {
    pub summary: Option<String>,
    pub tail: Vec<ChatTurn>,
}

impl WorkingMemory {
    /// Build a memory with no summary — the stateless (`conversation_id`-less)
    /// path, where the caller supplies its own history directly.
    pub fn from_turns(tail: Vec<ChatTurn>) -> Self {
        WorkingMemory { summary: None, tail }
    }

    pub fn is_empty(&self) -> bool {
        self.summary.is_none() && self.tail.is_empty()
    }

    /// Turns for rewrite/expansion: the summary as a synthetic leading turn (so
    /// pronoun resolution reaches past the verbatim window), then the tail.
    pub fn rewrite_turns(&self) -> Vec<ChatTurn> {
        let mut turns = Vec::with_capacity(self.tail.len() + 1);
        if let Some(summary) = &self.summary {
            turns.push(ChatTurn {
                role: "system".to_string(),
                content: format!("Conversation summary so far: {summary}"),
            });
        }
        turns.extend(self.tail.iter().cloned());
        turns
    }
}

/// Load working memory for a conversation. Ownership must already be verified by
/// the caller (`routes::conversations::verify_owned`). Best-effort: any DB error
/// yields an empty `WorkingMemory` — the turn still proceeds, just without
/// continuity, matching the old `load_history`'s fail-open behaviour.
pub async fn load_working_memory(db: &DocumentDb, conversation_id: &str, config: &Config) -> WorkingMemory {
    let Ok(oid) = parse_oid(conversation_id) else {
        return WorkingMemory::default();
    };
    let Ok(Some(conv)) = db.chat_conversations().find_one(doc! { "_id": oid }).await else {
        return WorkingMemory::default();
    };
    let (summary, _, filter) = summary_state(&conv, conversation_id);

    let docs: Vec<Document> = match db
        .chat_messages()
        .find(filter)
        .projection(doc! { "role": 1, "content": 1 })
        .sort(doc! { "created_at": -1 })
        .limit(config.history_tail_max_turns)
        .await
    {
        Ok(cursor) => cursor.try_collect().await.unwrap_or_default(),
        Err(_) => Vec::new(),
    };

    // `docs` is newest-first. Clip assistant content, accumulate a word budget,
    // and stop once it's spent (always keeping at least the newest message even
    // if it alone exceeds the budget) — then reverse to chronological order.
    let mut tail: Vec<ChatTurn> = Vec::new();
    let mut words_used = 0usize;
    for d in &docs {
        let (Some(role), Some(content)) = (d.get_str("role").ok(), d.get_str("content").ok()) else {
            continue;
        };
        let (content, words) = if role == "assistant" {
            clip_words(content, config.history_msg_clip)
        } else {
            (content.to_string(), content.split_whitespace().count())
        };
        if !tail.is_empty() && words_used + words > config.history_tail_max_tokens {
            break;
        }
        words_used += words;
        tail.push(ChatTurn { role: role.to_string(), content });
    }
    tail.reverse();

    WorkingMemory { summary, tail }
}

/// First `max_words` words of `text` plus that returned string's own word
/// count, so callers needing both don't re-scan the (possibly clipped) result.
/// Appends an ellipsis marker when truncated.
fn clip_words(text: &str, max_words: usize) -> (String, usize) {
    let words: Vec<&str> = text.split_whitespace().collect();
    if words.len() <= max_words {
        (text.to_string(), words.len())
    } else {
        (format!("{}\u{2026}", words[..max_words].join(" ")), max_words)
    }
}

// ---------------------------------------------------------------------------
// Compaction (write-behind, never on the hot path)
// ---------------------------------------------------------------------------

/// Check whether `conversation_id`'s un-summarized tail has grown past the
/// compaction threshold and, if so, spawn a detached task that folds the oldest
/// overflow turns into `summary`. Never blocks the caller. `foundry` is `None`
/// when Foundry Local is unavailable — compaction is then simply skipped (the
/// next trigger retries); a failure inside the task is logged, never propagated.
pub fn maybe_spawn_compaction(
    db: DocumentDb,
    foundry: Option<Arc<FoundryManager>>,
    config: Config,
    conversation_id: String,
) {
    tokio::spawn(async move {
        let Some(foundry) = foundry else { return };
        if let Err(e) = compact_if_needed(&db, &foundry, &config, &conversation_id).await {
            tracing::warn!(error = %e, conversation_id, "conversation compaction failed (non-fatal)");
        }
    });
}

async fn compact_if_needed(
    db: &DocumentDb,
    foundry: &FoundryManager,
    config: &Config,
    conversation_id: &str,
) -> AppResult<()> {
    let oid = parse_oid(conversation_id)?;
    let conv = db.chat_conversations().find_one(doc! { "_id": oid }).await?.ok_or(AppError::NotFound)?;
    let (prior_summary, summary_upto, filter) = summary_state(&conv, conversation_id);

    let overflow: Vec<Document> = db
        .chat_messages()
        .find(filter)
        .projection(doc! { "role": 1, "content": 1 })
        .sort(doc! { "created_at": 1 })
        .limit(MAX_COMPACTION_BATCH)
        .await?
        .try_collect()
        .await?;

    let total_words: usize =
        overflow.iter().filter_map(|d| d.get_str("content").ok()).map(|c| c.split_whitespace().count()).sum();
    let over_turns = overflow.len() as i64 > config.compact_after_turns;
    let over_words = total_words > 2400;
    let keep_tail = config.history_tail_max_turns.max(0) as usize;
    if (!over_turns && !over_words) || overflow.len() <= keep_tail {
        return Ok(());
    }

    // Fold everything but the newest `keep_tail` turns; those stay verbatim in
    // the next `load_working_memory` call.
    let fold = &overflow[..overflow.len() - keep_tail];
    let Some(new_summary_upto) = fold.last().and_then(|d| d.get_object_id("_id").ok()) else {
        return Ok(());
    };
    let transcript: String = fold
        .iter()
        .filter_map(|d| {
            let role = d.get_str("role").ok()?;
            let content = d.get_str("content").ok()?;
            Some(format!("{role}: {content}"))
        })
        .collect::<Vec<_>>()
        .join("\n");

    let system = "You maintain a running summary of a clinical-records chat for continuity \
        across a long conversation. Update the summary with the new turns below. Preserve \
        patient names/ids, dates, numeric findings, and open questions. Be entity-dense and \
        concise (150 words max). Output ONLY the updated summary, no preamble.";
    let user = match &prior_summary {
        Some(s) => format!("Current summary: {s}\n\nNew turns:\n{transcript}\n\nUpdated summary:"),
        None => format!("New turns:\n{transcript}\n\nSummary:"),
    };

    let mut spec = ModelSpec::for_kind(AgentKind::QueryRewrite, config);
    spec.temperature = 0.1;
    spec.max_tokens = Some(256);
    let new_summary = foundry.complete_with(&spec, system, &user).await?;
    let new_summary = new_summary.trim();
    if new_summary.is_empty() {
        return Ok(());
    }

    // CAS on `summary_upto`: only apply if it hasn't moved since we read it,
    // so two concurrent compactions (two windows open) can't interleave.
    let cas_filter = match summary_upto {
        Some(u) => doc! { "_id": oid, "summary_upto": u },
        None => doc! { "_id": oid, "summary_upto": { "$exists": false } },
    };
    db.chat_conversations()
        .update_one(cas_filter, doc! { "$set": { "summary": new_summary, "summary_upto": new_summary_upto } })
        .await?;
    Ok(())
}

// ---------------------------------------------------------------------------
// Retention sweep (hygiene)
// ---------------------------------------------------------------------------

/// Spawn the boot-time + daily retention sweep loop. A no-op when retention is
/// disabled (`ONPREM_CONVERSATION_RETENTION_DAYS=0`, the default).
pub fn spawn_retention_sweep(db: DocumentDb, config: Config) {
    if config.conversation_retention_days <= 0 {
        return;
    }
    tokio::spawn(async move {
        loop {
            run_retention_sweep(&db, &config).await;
            tokio::time::sleep(std::time::Duration::from_secs(24 * 3600)).await;
        }
    });
}

/// Delete conversations (and their messages) whose `updated_at` is older than
/// `conversation_retention_days`. Best-effort: errors are logged, never fatal.
async fn run_retention_sweep(db: &DocumentDb, config: &Config) {
    let cutoff_millis =
        chrono::Utc::now().timestamp_millis() - config.conversation_retention_days * 86_400_000;
    let cutoff = mongodb::bson::DateTime::from_millis(cutoff_millis);
    let filter = doc! { "updated_at": { "$lt": cutoff } };

    let stale: Vec<Document> = match db.chat_conversations().find(filter.clone()).await {
        Ok(c) => c.try_collect().await.unwrap_or_default(),
        Err(e) => {
            tracing::warn!(error = %e, "retention sweep: listing stale conversations failed");
            return;
        }
    };
    if stale.is_empty() {
        return;
    }
    let ids: Vec<String> = stale.iter().filter_map(|d| d.get_object_id("_id").ok().map(|o| o.to_hex())).collect();

    if let Err(e) = db.chat_messages().delete_many(doc! { "conversation_id": { "$in": &ids } }).await {
        tracing::warn!(error = %e, "retention sweep: message delete failed");
        return;
    }
    match db.chat_conversations().delete_many(filter).await {
        Ok(r) => tracing::info!(deleted = r.deleted_count, "retention sweep: removed stale conversations"),
        Err(e) => tracing::warn!(error = %e, "retention sweep: conversation delete failed"),
    }
}
