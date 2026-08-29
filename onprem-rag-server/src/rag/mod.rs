//! RAG orchestration: turn a chat turn into grounded context, then a grounded prompt.
//!
//! Steps, in order:
//! 1. **History-aware rewrite** — fold the recent conversation into a single
//!    standalone query (so "and his dosage?" becomes a self-contained question).
//! 2. **Multi-query expansion** — ask the LLM for a few alternative phrasings to widen
//!    recall (embeddings and lexical search both miss on wording alone).
//! 3. **Retrieve** (see `crate::retrieval`) — fuse + rerank into top-k passages.
//! 4. **Build prompt** — a strict, citation-required system prompt + the passages as
//!    numbered context; generation is grounded and refuses when nothing is relevant.
//!
//! Steps 1–2 use the *chat* model (`FoundryManager::complete`); they're best-effort —
//! if the model is slow/unavailable the pipeline degrades to the raw query rather than
//! failing, because retrieval quality shouldn't hard-depend on rewrite success.

pub mod routes;

use crate::config::Config;
use crate::foundry::FoundryManager;
use crate::retrieval::Passage;

/// One prior message in the conversation (for history-aware rewrite).
#[derive(Debug, Clone, serde::Deserialize)]
pub struct ChatTurn {
    /// "user" or "assistant".
    pub role: String,
    pub content: String,
}

/// Fold conversation history into a single standalone query. Returns the original
/// question unchanged when there's no history or the rewrite is unusable.
pub async fn rewrite_query(foundry: &FoundryManager, history: &[ChatTurn], question: &str) -> String {
    if history.is_empty() {
        return question.to_string();
    }
    let convo = history
        .iter()
        .map(|t| format!("{}: {}", t.role, t.content))
        .collect::<Vec<_>>()
        .join("\n");
    let system = "You rewrite a follow-up question into a standalone search query using the \
                  conversation for context. Resolve pronouns and references. Output ONLY the \
                  rewritten query on a single line, with no preamble or quotes.";
    let user = format!("Conversation:\n{convo}\n\nFollow-up question: {question}\n\nStandalone query:");
    match foundry.complete(system, &user).await {
        Ok(text) => {
            let line = first_line(&text);
            if line.is_empty() { question.to_string() } else { line }
        }
        Err(e) => {
            tracing::warn!(error = %e, "query rewrite failed; using original question");
            question.to_string()
        }
    }
}

/// Produce alternative phrasings of `query` to widen retrieval recall. The result
/// always includes `query` itself as the first (primary) entry; `count` is the total
/// target including the primary. Best-effort: on failure it's just `[query]`.
pub async fn expand_queries(foundry: &FoundryManager, config: &Config, query: &str) -> Vec<String> {
    let mut out = vec![query.to_string()];
    if !config.multi_query_enabled || config.multi_query_count <= 1 {
        return out;
    }
    let extra = config.multi_query_count - 1;
    let system = format!(
        "You generate alternative search queries for a health-records retrieval system. \
         Given a query, produce {extra} diverse rephrasings that capture different wording, \
         synonyms, and clinical terminology for the same information need. Output each on its \
         own line, no numbering, no preamble."
    );
    match foundry.complete(&system, query).await {
        Ok(text) => {
            for line in text.lines().map(str::trim).filter(|l| !l.is_empty()) {
                let cleaned = strip_list_marker(line);
                if !cleaned.is_empty() && !out.iter().any(|q| q.eq_ignore_ascii_case(&cleaned)) {
                    out.push(cleaned);
                }
                if out.len() >= config.multi_query_count {
                    break;
                }
            }
        }
        Err(e) => tracing::warn!(error = %e, "multi-query expansion failed; using single query"),
    }
    out
}

/// The grounded system prompt: answer only from context, cite by number, refuse when
/// the context doesn't support an answer (anti-hallucination — this is PHI).
pub const SYSTEM_PROMPT: &str = "You are a clinical records assistant. Answer the user's question \
    using ONLY the numbered context passages provided. Cite the passages you use inline as [1], \
    [2], etc. If the context does not contain enough information to answer, say you don't have \
    relevant records and do not speculate. Be concise and precise; never invent patient details, \
    dosages, dates, or values that are not in the context.";

/// Build the user-message body: numbered context passages followed by the question.
/// Passage numbering here is 1-based and matches the citation indices the model emits
/// and the `citations` the route sends to the client.
pub fn build_prompt(passages: &[Passage], question: &str) -> String {
    let mut ctx = String::new();
    for (i, p) in passages.iter().enumerate() {
        ctx.push_str(&format!("[{}] {}\n\n", i + 1, p.text.trim()));
    }
    if ctx.is_empty() {
        ctx.push_str("(no relevant records found)\n\n");
    }
    format!("Context:\n{ctx}Question: {question}")
}

/// First non-empty line, trimmed of surrounding quotes/whitespace.
fn first_line(text: &str) -> String {
    text.lines()
        .map(str::trim)
        .find(|l| !l.is_empty())
        .map(|l| l.trim_matches(|c| c == '"' || c == '\'').trim().to_string())
        .unwrap_or_default()
}

/// Strip a leading list marker ("1. ", "- ", "* ") and surrounding quotes.
fn strip_list_marker(line: &str) -> String {
    let l = line.trim();
    let l = l
        .trim_start_matches(|c: char| c.is_ascii_digit())
        .trim_start_matches(['.', ')', '-', '*', ' ']);
    l.trim_matches(|c| c == '"' || c == '\'').trim().to_string()
}
