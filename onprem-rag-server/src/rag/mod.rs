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
pub async fn rewrite_query(
    foundry: &FoundryManager,
    history: &[ChatTurn],
    question: &str,
) -> String {
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
    let user =
        format!("Conversation:\n{convo}\n\nFollow-up question: {question}\n\nStandalone query:");
    match foundry.complete(system, &user).await {
        Ok(text) => {
            let line = first_line(&text);
            if line.is_empty() {
                question.to_string()
            } else {
                line
            }
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
    if !config.multi_query_enabled || config.multi_query_count <= 1 || skip_expansion(query) {
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

/// Cheap, deterministic queries already carry enough lexical signal and do not justify
/// a model round-trip for expansion.
fn skip_expansion(query: &str) -> bool {
    let trimmed = query.trim();
    if trimmed.split_whitespace().count() <= 3 || trimmed.contains('"') {
        return true;
    }
    trimmed.split_whitespace().any(|token| {
        let token =
            token.trim_matches(|c: char| !c.is_ascii_alphanumeric() && c != '-' && c != '.');
        token.len() >= 3
            && token.chars().any(|c| c.is_ascii_digit())
            && token
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '.')
    })
}

/// Rewrite and expand queries in one step. When there is no history, the original
/// question is returned as-is (no model call). When there is history AND multi-query
/// expansion is enabled, a single model call produces both the standalone rewrite and
/// the alternative phrasings — saving one round-trip compared to two sequential calls.
///
/// Never fails the caller: on any model error it falls back to the two-step path.
pub async fn prepare_queries(
    foundry: &FoundryManager,
    config: &Config,
    history: &[ChatTurn],
    question: &str,
) -> (String, Vec<String>) {
    let want_expansion =
        config.multi_query_enabled && config.multi_query_count > 1 && !skip_expansion(question);

    // No history: standalone is the original question. Optionally expand.
    if history.is_empty() {
        if !want_expansion {
            return (question.to_string(), vec![question.to_string()]);
        }
        let queries = expand_queries(foundry, config, question).await;
        return (question.to_string(), queries);
    }

    // History present, no expansion: rewrite only.
    if !want_expansion {
        let standalone = rewrite_query(foundry, history, question).await;
        return (standalone.clone(), vec![standalone]);
    }

    // History present + expansion: one combined model call.
    let extra = config.multi_query_count - 1;
    let convo = history
        .iter()
        .map(|t| format!("{}: {}", t.role, t.content))
        .collect::<Vec<_>>()
        .join("\n");
    let system = format!(
        "You are a health-records search assistant. Resolve the follow-up question into a \
         standalone query, then produce {extra} alternative phrasings.\n\
         Format (output only these lines):\n\
         STANDALONE: <self-contained question with pronouns resolved>\n\
         VARIANT: <alternative phrasing>\n\
         (one VARIANT line per alternative)"
    );
    let user = format!("Conversation:\n{convo}\n\nFollow-up question: {question}");

    match foundry.complete(&system, &user).await {
        Ok(text) => {
            let mut standalone = question.to_string();
            let mut variants: Vec<String> = Vec::new();
            for line in text.lines().map(str::trim).filter(|l| !l.is_empty()) {
                if let Some(s) = line.strip_prefix("STANDALONE:") {
                    let s = s.trim().trim_matches(|c| c == '"' || c == '\'').trim();
                    if !s.is_empty() {
                        standalone = s.to_string();
                    }
                } else if let Some(v) = line.strip_prefix("VARIANT:") {
                    let v = strip_list_marker(v.trim());
                    if !v.is_empty() {
                        variants.push(v);
                    }
                }
            }
            variants.truncate(extra);
            let mut all = vec![standalone.clone()];
            for v in variants {
                if !all.iter().any(|q: &String| q.eq_ignore_ascii_case(&v)) {
                    all.push(v);
                }
            }
            (standalone, all)
        }
        Err(e) => {
            tracing::warn!(error = %e, "prepare_queries combined call failed; falling back to two-step");
            let standalone = rewrite_query(foundry, history, question).await;
            let queries = expand_queries(foundry, config, &standalone).await;
            (standalone, queries)
        }
    }
}

/// The grounded system prompt: answer only from context, cite by number, refuse when
/// the context doesn't support an answer (anti-hallucination — this is PHI).
pub const SYSTEM_PROMPT: &str = "You are a clinical records assistant. Answer the user's question \
    using ONLY the numbered context passages provided. Cite the passages you use inline as [1], \
    [2], etc. If the context does not contain enough information to answer, say you don't have \
    relevant records and do not speculate. Be concise and precise; never invent patient details, \
    dosages, dates, or values that are not in the context. Conversation summary and recent turns \
    are context for continuity; RECORDS in the numbered context remain the only citable source.";

/// Enforce deterministic approximate-token budgets before citations are serialized.
/// Source projections are whitespace-delimited, so word count is a conservative and
/// model-independent proxy that avoids loading a tokenizer on the request path.
pub fn apply_context_budget(
    passages: Vec<Passage>,
    total_tokens: usize,
    per_row_tokens: usize,
) -> Vec<Passage> {
    let mut remaining = total_tokens;
    let per_row_tokens = per_row_tokens.min(total_tokens);
    let mut bounded = Vec::new();

    for mut passage in passages {
        let allowance = remaining.min(per_row_tokens);
        if allowance == 0 {
            break;
        }
        let words: Vec<&str> = passage.text.split_whitespace().take(allowance).collect();
        if words.is_empty() {
            continue;
        }
        remaining = remaining.saturating_sub(words.len());
        passage.text = words.join(" ");
        bounded.push(passage);
    }
    bounded
}

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

    let mut preamble = String::new();
    if let Some(summary) = &memory.summary {
        preamble.push_str(&format!("Conversation summary: {summary}\n"));
    }
    if !memory.tail.is_empty() {
        preamble.push_str("Recent turns:\n");
        for t in &memory.tail {
            preamble.push_str(&format!("{}: {}\n", t.role, t.content));
        }
    }
    if !preamble.is_empty() {
        preamble.push('\n');
    }

    format!("{preamble}Context:\n{ctx}Question: {question}")
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

#[cfg(test)]
mod tests {
    use super::*;

    fn passage(id: &str, text: &str) -> Passage {
        Passage {
            id: id.to_string(),
            source_id: "source".to_string(),
            table: "encounters".to_string(),
            row_pk: id.to_string(),
            chunk_index: 0,
            text: text.to_string(),
            fields: serde_json::json!({}),
            score: 0.9,
            reranked: true,
            vector_rank: Some(1),
            text_rank: Some(2),
            fused_score: 0.03,
            rerank_score: Some(0.9),
        }
    }

    #[test]
    fn context_budget_caps_each_row_and_total() {
        let bounded = apply_context_budget(
            vec![
                passage("one", "one two three four five"),
                passage("two", "six seven eight nine ten"),
            ],
            5,
            3,
        );

        assert_eq!(bounded.len(), 2);
        assert_eq!(bounded[0].text, "one two three");
        assert_eq!(bounded[1].text, "six seven");
    }

    #[test]
    fn zero_context_budget_removes_all_passages() {
        assert!(apply_context_budget(vec![passage("one", "one")], 0, 10).is_empty());
    }
}
