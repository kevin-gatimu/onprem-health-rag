# 22 — Chat Memory: Persistence, History Awareness & Compaction

> Status: PLANNED. Defines where conversation memory lives (server-authoritative, client as a thin
> cache), how history reaches the model (rewrite **and** generation), and how long conversations are
> compacted (rolling summary) so quality doesn't degrade and prompts don't grow unbounded.
> Companions: `17` (router uses `has_history`; conversation-meta gate), `19.2` (patient-focus filter),
> `20.1` (single rewrite call absorbs the summary).

## Current state (as built)

- **Persistence exists and is server-side**: `chat_conversations` + messages collections
  (`routes/conversations.rs`), scoped by `user_id`, agent-partitioned via `agent_kind`. Assistant
  messages store citations / structured agg results for replay. CRUD + `GET /conversations/<id>/messages`.
- `/chat` with `conversation_id`: server verifies ownership, loads **last 10 messages**
  (`load_history`), persists the user turn pre-stream and the assistant turn post-stream. Without
  `conversation_id` the call is **stateless** (client-sent `history`) — eval and `/search` rely on this.
- **Gap 1 — history only reaches the rewrite.** `build_semantic_chat_data` calls
  `rewrite_query(history, question)` but `build_prompt(&passages, &standalone)` contains **no prior
  turns**. The generation model has amnesia: "compare that to his previous visit", "shorten your
  last answer", or any reference to prior *answers* fails even though retrieval worked.
- **Gap 2 — no compaction.** `load_history(…, 10)` is a fixed message-count window; long assistant
  answers make the rewrite prompt balloon, and everything before the window is simply gone.
- **Gap 3 — structured path ignores history.** `run_structured` gets the raw `body.question`, so
  follow-up aggregations ("and for females?", "same but per month") plan against an incomplete question.
- **Client**: React Query caches persisted messages per conversation; zustand holds only pending
  stream state. Nothing durable on the client. (Correct for PHI — keep it that way.)

## Decision: who owns memory?

**Server owns memory; client renders it.** Both participate, with a strict split:

| Layer | Owns | Never holds |
|---|---|---|
| DocumentDB | transcripts, citations, agg results, rolling summaries, focus entities | — |
| Server (stateless per request) | history window assembly, compaction, token budgeting | cross-request in-RAM session state (breaks multi-client + restarts) |
| App (React Query + zustand) | render cache of persisted messages, pending stream, draft input | durable transcripts (no localStorage PHI), no history assembly logic |

Rationale: many desktop clients per server + stateless JWT design means the DB is the only safe
source of truth; the Tauri webview must not accumulate PHI on disk; and the eval harness keeps the
stateless `history:[…]` escape hatch. The client sending `history` alongside `conversation_id` is
already ignored — keep that (server-loaded history can't be spoofed cross-conversation).

## Design

### 22.1 Conversation working memory = rolling summary + verbatim tail

Add to the conversation doc:

```jsonc
{
  // …existing fields…
  "summary": "Patient in focus: John Carter (id 4711). User asked about his HbA1c trend; …",
  "summary_upto": ObjectId, // last message folded into the summary
  "focus": { "patient_pk": "4711", "table": "patients" } // optional sticky entities (22.4)
}
```

Per turn the server assembles **working memory** = `summary` (if any) + last `K` messages after
`summary_upto`, where the tail is bounded by *tokens*, not count:

- `ONPREM_HISTORY_TAIL_MAX_TURNS=8`, `ONPREM_HISTORY_TAIL_MAX_TOKENS=1200` (word-approx, same
  convention as chunking). Assistant messages in the tail are clipped to their first
  `ONPREM_HISTORY_MSG_CLIP=200` words — old full answers add tokens, not signal (citations live on
  the message doc for the UI; the model doesn't need them back).
- New `memory.rs` (or extend `routes/conversations.rs`): `load_working_memory(db, cid, uid) ->
  WorkingMemory { summary: Option<String>, tail: Vec<ChatTurn> }` replaces raw `load_history` in `/chat`.

### 22.2 Compaction (write-behind, never on the hot path)

- **Trigger**: after the assistant turn persists, if messages-after-`summary_upto` exceed
  `ONPREM_COMPACT_AFTER_TURNS=12` (or their word-count exceeds ~2400), spawn a detached tokio task
  (same pattern as ingest jobs) that:
  1. loads `summary` + the oldest overflow turns (all but the newest `TAIL_MAX_TURNS`),
  2. calls the **fast model** (`AgentKind::QueryRewrite`-class spec, `ONPREM_MODEL_FAST`,
     temperature 0.1, `max_tokens 256`) with "update this running summary; preserve patient
     names/ids, dates, numeric findings, open questions",
  3. compare-and-swaps `summary` + `summary_upto` on the conversation doc (guard with
     `summary_upto` in the filter so concurrent compactions can't interleave).
- Compaction failure is harmless: next turn just carries a longer tail; retry on the following
  trigger. Never blocks or delays a user-visible stream.
- Summaries are instructed to be **entity-dense and ≤150 words** — they exist for pronoun
  resolution and continuity, not prose.

### 22.3 History reaches generation, not just rewrite

- `build_prompt` gains an optional conversation preamble, budgeted separately from passages:

  ```
  [system: existing grounded SYSTEM_PROMPT + "Conversation summary and recent turns are context
   for continuity; RECORDS in the numbered context remain the only citable source."]
  Conversation summary: {summary}
  Recent turns: {clipped tail}
  Context: [1]… [2]…
  Question: {question}
  ```

  Keeping history in the *user* prompt (not as chat-role messages) preserves the existing
  single-system/single-user streaming path and the `<think>` filter untouched.
- Rewrite (and 20.1's fused rewrite+expand) consumes the same `WorkingMemory` — summary first, tail
  after — so pronouns resolve even past the verbatim window.
- **Structured path**: pass the rewritten standalone question into `run_structured` instead of
  `body.question` (one-line change with outsized effect on follow-up aggregations). The agg planner
  prompt itself stays history-free — the standalone question is the contract.

### 22.4 Sticky focus entities (optional, after 19.2)

- When a turn resolves a concrete patient/entity (router Tier-2 hints or a lookup answer), write
  `focus` on the conversation. Subsequent ambiguous turns ("what meds is *he* on?") get `focus`
  appended to the rewrite context, and — once 19.2 lands — a candidate metadata pre-filter
  (suggested, never forced: the rewrite may switch patients, so focus updates every turn it
  resolves and clears on explicit topic change detected by the rewrite output).

### 22.5 Conversation-meta queries

- "What have I asked so far?" / "summarize our conversation" must NOT hit retrieval. Router (17)
  Tier 0/1 gains a `ConversationMeta` detection (regex set from the Electron prior art) → `/chat`
  answers from `WorkingMemory` alone (summary + tail → one generation call, empty citations).

### 22.6 Client-side rules (small, mostly already true)

- Keep: React Query `['messages', cid]` as the render cache, zustand for the pending stream,
  optimistic append of the user turn, invalidate on `done`.
- Add: persist **draft input** per conversation in zustand `persist` (UI nicety, not PHI-transcript);
  ensure the messages query cache is `gcTime`-bounded (default is fine) and cleared on logout
  (`queryClient.clear()` in the session store logout path — verify, add if missing).
- Never mirror transcripts to localStorage/disk. Android build inherits the same rule.

### 22.7 Retention & hygiene

- Message docs: index `{conversation_id: 1, created_at: -1}` (verify exists; add in
  `ensure_indexes`). Cap stored message size (`ONPREM_MESSAGE_MAX_BYTES=32768`, clip with marker).
- Admin retention knob `ONPREM_CONVERSATION_RETENTION_DAYS=0` (0 = keep forever): a boot-time +
  daily sweep deleting conversations (and their messages) older than the cutoff, audit-logged.
  Deleting a user already orphans conversations — the sweep also removes orphans older than 30 days.

## Config surface

```
ONPREM_HISTORY_TAIL_MAX_TURNS=8
ONPREM_HISTORY_TAIL_MAX_TOKENS=1200
ONPREM_HISTORY_MSG_CLIP=200
ONPREM_COMPACT_AFTER_TURNS=12
ONPREM_CONVERSATION_RETENTION_DAYS=0
ONPREM_MESSAGE_MAX_BYTES=32768
```

## Phases + gates

1. **Working memory + generation preamble** (22.1 + 22.3): `memory.rs`, `build_prompt` preamble,
   standalone→`run_structured`. Gate: `cargo test` fixtures for tail budgeting/clipping; manual —
   "shorten your last answer" works; follow-up aggregation "and for females?" plans correctly.
2. **Compaction** (22.2): write-behind task + CAS update. Gate: unit test the fold/CAS logic with a
   stubbed model; 30-turn manual conversation keeps prompts bounded (log the assembled token count
   via plan-21 spans) and still resolves turn-1 entities.
3. **Meta queries + focus** (22.5, 22.4): after router 17 Phase A/B. Gate: "what did I ask first?"
   answers from the transcript with no retrieval span in the logs.
4. **Hygiene** (22.6, 22.7): indexes, clip, retention sweep, logout cache clear. Gate: sweep dry-run
   logs candidates; message index present on fresh boot.

## Risks

- Summary drift/hallucinated memory: summaries are model output — keep them short, entity-dense,
  and *never citable* (the system prompt explicitly ranks numbered records above conversation
  context; the score-gated retrieval still guards facts).
- Concurrent turns in one conversation (two windows open): CAS on `summary_upto` + last-write-wins
  on messages is acceptable; do not build locking for this.
- The 10-message `load_history` remains for `/agents` until it migrates to `WorkingMemory` in
  phase 1 (same call-site swap).
