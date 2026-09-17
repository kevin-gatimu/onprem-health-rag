# 09 — Stage 6: AI Chat (implementation contract)

Status: **IMPLEMENTED** (2026-08-25) — server + bridge + app all landed and verified (cargo check + tsc clean).

Original plan: Opus plans + verifies each diff; Sonnet implements per layer.

Stage 6 makes `/chat` **stateful**: persistent per-user conversations, message history, and a mobile-first
Chat screen with grounded streaming answers + citations. Builds directly on the existing stateless
`/chat` SSE (`rag/routes.rs`), the bridge `chat` relay (`commands.rs`), and the boot-time listener
registry (`bridgeEvents.ts`).

This doc is the contract for three sequential Sonnet agents (server → bridge → app). Opus reads each
diff (not self-reports), runs `cargo check` / `tsc`, and greps both `commands.rs` + `bridge.ts` to
confirm every new field is mirrored ([[bridge-mirror-structs-drop-fields]]).

---

## Key architectural decisions (read first)

1. **Scope by JWT user id.** Every conversation/message carries `user_id = AuthUser.id` (the JWT `sub`).
   The reference hardcodes `userId: 'default'` — **do not copy that**. All conversation routes filter by
   `user_id`; ownership mismatch → **404** (not 403 — don't leak existence).

2. **`/chat` stays backward-compatible.** `conversation_id` is **optional** on `ChatRequest`. When
   absent, `/chat` behaves exactly as today (stateless, no persistence) — this keeps `/search`, the
   agents path, and the RAGAs eval harness working. When present, the server persists messages and
   loads history from the DB.

3. **No live per-step activity events from the server.** Rocket's `EventStream!` is `'static` and cannot
   hold the borrowed `&State`/`&FoundryManager` across a yield — which is why the current handler does
   *all* retrieval (rewrite→expand→retrieve→rerank) **before** opening the stream, so setup/DB errors
   stay clean HTTP errors. Making retrieval emit live sub-steps would require Arc-ifying `FoundryManager`
   (invasive, touches every foundry call site). **We do not do that in Stage 6.**
   Instead the **live status strip is client-derived** from the SSE phase transitions the client already
   observes, which are honest labels of the phase actually running in each window:
   - request sent, no `citations` yet → **"Searching records…"**
   - `citations` arrived, no `token` yet → **"Reading sources…"**
   - `token`s flowing → **"Generating…"**
   - `done` → hidden
   This needs **zero** server activity events and preserves the valued HTTP-error property. (Deliberate
   deviation from the plan's "pipeline-step activity events", same spirit as dropping clear-cache/reindex
   in Stage 5 — the cost outweighs the value given the `'static` constraint.)

4. **`run_id` correlation is bridge-stamped, server-agnostic.** The client generates a `runId`
   (`crypto.randomUUID()`) per send and passes it to the `chat` command. The **bridge** stamps `run_id`
   into every emitted `chat://*` event; the server never sees it. The boot listener drops any event whose
   `run_id` ≠ the store's active run. Rationale: our chat listeners live once at boot and fan into a
   global store, so a stale run's late tokens (after a remount or rapid error→resend) must be filtered.

5. **`DocumentDb` is `Clone`; `FoundryManager` is not.** Message *persistence* is cheap to do inside the
   generator (clone `db` + owned ids/strings in). We accumulate the assistant's visible text in the
   generator and persist it after the token loop. The user message + auto-title are persisted **before**
   the generator (so they survive a generation failure) as normal awaits (HTTP errors OK).

6. **Persistence ownership.** The **server** is the single writer of chat messages. The client shows an
   optimistic pending exchange during streaming, then invalidates `['messages', convId]` on `done` so the
   authoritative persisted list replaces the optimistic copy (no double-save, no divergence).

7. **Single in-flight run; lock the UI while streaming** (like the reference): composer, new-chat,
   conversation switch, and delete are disabled during a stream. This collapses all cross-conversation
   race complexity — `pending` always belongs to the currently-viewed conversation.

---

## Agent 1 — Server (`onprem-rag-server`)

### 1a. Collections + accessors + indexes — `src/documentdb/mod.rs`

Add two constants + accessors following the existing pattern:

```rust
/// Chat: one doc per conversation, scoped by user_id.
pub const CHAT_CONVERSATIONS: &str = "chat_conversations";
/// Chat: one doc per message, linked to a conversation by conversation_id (hex string).
pub const CHAT_MESSAGES: &str = "chat_messages";
```
```rust
pub fn chat_conversations(&self) -> Collection<Document> { self.collection(CHAT_CONVERSATIONS) }
pub fn chat_messages(&self)      -> Collection<Document> { self.collection(CHAT_MESSAGES) }
```

Add `ensure_chat_indexes` (mirror `ensure_user_indexes`), two `createIndexes` run_commands:
- `chat_conversations`: `{ "user_id": 1, "updated_at": -1 }`, name `chat_conv_user_updated`.
- `chat_messages`: `{ "conversation_id": 1, "created_at": 1 }`, name `chat_msg_conv_created`.

Non-unique. Call best-effort from `main.rs` after `ensure_user_indexes` (warn on error, non-fatal).

### 1b. Conversation routes + persistence helpers — new `src/routes/conversations.rs`

Declare `pub mod conversations;` in `src/routes/mod.rs`.

**Document shapes (BSON, stored):**
- `chat_conversations`: `{ _id: ObjectId, user_id: String, title: String, created_at: BsonDateTime, updated_at: BsonDateTime }`
- `chat_messages`: `{ _id: ObjectId, conversation_id: String (hex of the conversation _id), user_id: String, role: String ("user"|"assistant"), content: String, citations: Option<Array<Passage-as-bson>>, created_at: BsonDateTime }`

**Wire output structs (snake_case, what the API returns):**
```rust
#[derive(Serialize)]
pub struct ConversationOut { pub id: String, pub title: String, pub created_at: String, pub updated_at: String }
#[derive(Serialize)]
pub struct MessageOut {
    pub id: String, pub role: String, pub content: String,
    pub citations: Option<Vec<crate::retrieval::Passage>>, // omitted/null for user messages
    pub created_at: String,
}
```
Timestamps: serialize `BsonDateTime` to ISO-8601 strings (`.try_to_rfc3339_string()`), matching the
existing explorer/ingest convention of string timestamps.

**Routes (all take `user: AuthUser`, filter by `user.id`):**

| Route | Behavior |
|---|---|
| `GET /conversations` | `Vec<ConversationOut>`, `find({user_id})` sort `{updated_at:-1}`. |
| `POST /conversations` | Body `{ title: Option<String> }` (default `"New conversation"`). Insert `{user_id, title, created_at:now, updated_at:now}`. Return the created `ConversationOut`. |
| `PATCH /conversations/<id>` | Body `{ title: String }`. Ownership-checked update of `title` + `updated_at`. Return updated `ConversationOut`. |
| `DELETE /conversations/<id>` | Ownership-checked. Delete the conversation **and cascade** `chat_messages.delete_many({conversation_id: id})`. Return `Json(serde_json::json!({"ok": true}))`. |
| `GET /conversations/<id>/messages` | Ownership-checked. `find({conversation_id:id})` sort `{created_at:1}` → `Vec<MessageOut>`. |

`<id>` is the conversation's ObjectId hex. Parse with `ObjectId::parse_str(id)` → `AppError::BadRequest`
on malformed. "Not found or not owned" (find returns nothing / `user_id` mismatch) → `AppError::NotFound`.
(Check `AppError` variants in `error.rs`; use `NotFound`/`BadRequest` as they exist — match existing usage
in `explorer.rs`.)

**Public helpers (used by `/chat`) — `pub(crate)`:**
```rust
/// Load the last `limit` messages of a conversation as ChatTurns (asc by created_at),
/// for history-aware rewrite. Returns empty if the conversation isn't owned by user_id.
pub(crate) async fn load_history(db: &DocumentDb, conversation_id: &str, user_id: &str, limit: i64)
    -> Vec<crate::rag::ChatTurn>;

/// Verify ownership; return the ObjectId or an AppError (BadRequest/NotFound).
pub(crate) async fn verify_owned(db: &DocumentDb, conversation_id: &str, user_id: &str)
    -> AppResult<ObjectId>;

/// Insert a user message; if this is the conversation's FIRST user message, set the
/// conversation title = truncate(content, 60) (+ "…" if longer). Bumps updated_at.
pub(crate) async fn persist_user_message(db: &DocumentDb, conversation_id: &str, user_id: &str, content: &str)
    -> AppResult<()>;

/// Insert an assistant message (with citations) and bump the conversation's updated_at.
pub(crate) async fn persist_assistant_message(
    db: &DocumentDb, conversation_id: &str, user_id: &str, content: &str, citations: &[crate::retrieval::Passage],
) -> AppResult<()>;
```
Auto-title check: `count_documents({conversation_id, role:"user"})` **before** inserting the new user
message; if `0`, set title. Truncate on char boundary (use `.chars().take(60)`), append `…` if the
original was longer.

### 1c. `/chat` changes — `src/rag/routes.rs`

- Add to `ChatRequest`: `#[serde(default)] pub conversation_id: Option<String>`.
- In `chat()` **before** the generator (keep as awaits → HTTP errors):
  - `let foundry = state.foundry()?;` (unchanged — Foundry-down stays a clean HTTP 503).
  - If `conversation_id` is `Some(cid)`:
    - `routes::conversations::verify_owned(&state.db, cid, &user.id).await?;` (404 for bad/unowned id).
    - Load history from DB: `let history = load_history(&state.db, cid, &user.id, 10).await;` — **use this
      instead of `body.history`** when a conversation is present. (When absent, use `body.history` as today.)
    - `persist_user_message(&state.db, cid, &user.id, &body.question).await?;` (also auto-titles).
  - Change the handler signature `_user: AuthUser` → `user: AuthUser` (we now need `user.id`).
- Keep the existing retrieve-before-generator flow. Keep `passages` alive (don't consume) so it can be
  **moved into the generator** for persistence.
- Move into the generator (owned, all `Send`): `let db = state.db.clone();`, `let persist_target =
  body.conversation_id.clone();`, `let uid = user.id.clone();`, `let passages_for_persist = passages.clone();`
  (Passage derives Clone — verify; it's serialized already so it must be Serialize; add `Clone` if missing).
- Inside the generator: accumulate visible text into `let mut full_answer = String::new();` — append every
  `visible` piece (both the refuse message and streamed tokens) so the persisted assistant content equals
  what was streamed. Track `let mut had_error = false;` set in the `Err(e)` arm.
- After the token loop, **before** `yield done`: if `persist_target` is `Some(cid)` **and** `!had_error`,
  call `persist_assistant_message(&db, &cid, &uid, &full_answer, &passages_for_persist).await` — log a
  `tracing::warn!` on error but still `yield done` (a persistence failure must not crash the stream).
- Event set is **unchanged**: `citations` → `token`* → (`error`?) → `done`. **No `activity` event.**

### 1d. Mount — `src/main.rs`

Add a mount block (or extend an existing one):
```rust
.mount("/", rocket::routes![
    routes::conversations::list_conversations,
    routes::conversations::create_conversation,
    routes::conversations::rename_conversation,
    routes::conversations::delete_conversation,
    routes::conversations::list_messages,
])
```
And after `ensure_user_indexes`: `if let Err(e) = documentdb::ensure_chat_indexes(&db).await { tracing::warn!(...) }`.

### Agent 1 verification (Opus)
- `cd onprem-rag-server && cargo check` clean. (`cargo run`/`build` FAIL on host — Smart App Control,
  OS error 4551 — do **not** run them.)
- Read the full diff. Confirm: ownership 404 on every conversation route; `body.history` ignored when
  `conversation_id` present; user message persisted before stream; assistant persisted only on clean
  completion; `/chat` still compiles with `conversation_id` absent (stateless path intact).
- Optional live check via the mongo MCP / mongosh once server runs: `chat_conversations` +
  `chat_messages` created, indexes present.

---

## Agent 2 — Bridge (`onprem-rag-app/src-tauri` + `src/lib/bridge.ts`)

### 2a. `commands.rs`

**Mirror structs** (add near the Stage 6 block, after `Passage`):
```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Conversation { pub id: String, pub title: String, pub created_at: String, pub updated_at: String }

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StoredMessage {
    pub id: String, pub role: String, pub content: String,
    pub citations: Option<Vec<Passage>>, pub created_at: String,
}
```
**Event envelope** for run_id-stamped emits:
```rust
#[derive(Serialize, Clone)]
struct ChatEvent { run_id: String, data: String }
```

**New commands** (follow the existing reqwest + `bearer_auth` + `check_auth` + `error_body` pattern; the
conversation id is ObjectId hex — URL-safe, no percent-encoding needed):
- `list_conversations() -> Result<Vec<Conversation>, String>` — `GET /conversations`.
- `create_conversation(title: Option<String>) -> Result<Conversation, String>` — `POST /conversations`, body `{title}`.
- `rename_conversation(id: String, title: String) -> Result<Conversation, String>` — `PATCH /conversations/{id}`, body `{title}`.
- `delete_conversation(id: String) -> Result<(), String>` — `DELETE /conversations/{id}` (ignore the `{ok:true}` body on success).
- `get_messages(id: String) -> Result<Vec<StoredMessage>, String>` — `GET /conversations/{id}/messages`.

**Modify `chat`**: add params `run_id: String` and `conversation_id: Option<String>`. Forward
`conversation_id` in the POST body (alongside `question`, `history`, `mode`, `rerank`, `top_k`). Stamp
run_id into every emit:
- `"citations"` → `app.emit("chat://citations", ChatEvent { run_id: run_id.clone(), data: event.data })`
- `"token"` → `app.emit("chat://token", ChatEvent { run_id: run_id.clone(), data: decode_token(&event.data) })`
- `"error"` → `app.emit("chat://error", ChatEvent { run_id: run_id.clone(), data: event.data.clone() })` then `return Err(event.data)`
- terminal → `app.emit("chat://done", ChatEvent { run_id: run_id.clone(), data: String::new() })`
  (uniform envelope; `data` empty for done).

### 2b. `lib.rs`
Register the 5 new commands in the `invoke_handler![...]` list. (`chat` is already registered — signature
change needs no re-registration.)

### 2c. `bridge.ts`
Mirror interfaces + wrappers:
```ts
export interface Conversation { id: string; title: string; created_at: string; updated_at: string }
export interface StoredMessage {
  id: string; role: 'user' | 'assistant'; content: string;
  citations: Passage[] | null; created_at: string;
}
export interface ChatEvent { run_id: string; data: string }  // payload shape of every chat:// event
```
```ts
export const listConversations = () => invoke<Conversation[]>('list_conversations');
export const createConversation = (title?: string) => invoke<Conversation>('create_conversation', { title });
export const renameConversation = (id: string, title: string) => invoke<Conversation>('rename_conversation', { id, title });
export const deleteConversation = (id: string) => invoke<void>('delete_conversation', { id });
export const getMessages = (id: string) => invoke<StoredMessage[]>('get_messages', { id });
```
Modify `chat()`:
```ts
export function chat(
  question: string,
  history: ChatTurn[] = [],
  opts: RetrievalOpts = {},
  conversationId: string | null = null,
  runId: string,
): Promise<void> {
  return invoke('chat', { question, history, opts, conversationId, runId });
}
```
(Tauri maps camelCase JS args → snake_case Rust params, consistent with `deleteIngestTable` in Stage 5.)

### Agent 2 verification (Opus)
- `cd onprem-rag-app/src-tauri && cargo check` clean; `cd onprem-rag-app && npx tsc --noEmit` clean.
- **Mirror grep**: every field of `Conversation`/`StoredMessage` present in BOTH `commands.rs` and
  `bridge.ts`; `chat://*` payloads are the `{run_id,data}` envelope on both sides.

---

## Agent 3 — App (`onprem-rag-app/src`)

react-markdown@10 + remark-gfm@4 are already in `package.json` — no install needed.

### 3a. `stores/chat.ts` — single in-flight run
```ts
import type { Passage } from '../lib/bridge';
export type ChatPhase = 'searching' | 'generating' | 'done';
export interface PendingRun {
  runId: string; conversationId: string; user: string;
  answer: string; citations: Passage[]; error: string | null; phase: ChatPhase;
}
interface ChatState {
  pending: PendingRun | null;
  startRun(runId: string, conversationId: string, user: string): void;
  appendAnswer(runId: string, batch: string[]): void; // ignores stale runId
  setCitations(runId: string, c: Passage[]): void;     // sets phase='generating'
  setError(runId: string, msg: string): void;          // phase='done', error set
  finish(runId: string): void;                          // phase='done'
  clear(): void;
}
```
- Every setter guards `if (get().pending?.runId !== runId) return;` — drops stale-run events.
- `startRun` sets `{ …, answer:'', citations:[], error:null, phase:'searching' }`.
- Derive "is streaming" in components as `pending && pending.phase !== 'done'`.

### 3b. `bridgeEvents.ts` — add chat listeners (once, at boot)
Follow the file's existing pattern. Tokens use a `createRafBuffer<string>` that flushes to
`useChat.getState().appendAnswer(activeRunId, batch)`. Because the buffer flush needs the runId, keep the
current run id in a module-scoped `let activeChatRunId: string | null` updated from each event's
`run_id`, OR read `useChat.getState().pending?.runId` inside the flush. **Simplest:** the flush reads
`const p = useChat.getState().pending; if (p) p… appendAnswer(p.runId, batch)` — but appendAnswer already
guards on runId, and the buffered tokens all belong to whatever run was active when pushed. Use this rule:
- `chat://token` (`{run_id, data}`): if `run_id === useChat.getState().pending?.runId`, `tokenBuffer.push(data)`; else ignore. The rAF flush calls `appendAnswer(run_id_at_flush, batch)` — capture the run id on the buffer alongside, or simpler: **push `{runId, token}` objects** and let the flush group by the current pending runId. Given single-run-at-a-time + input lock, pushing bare strings and flushing to `pending.runId` is safe; still call `appendAnswer(pending.runId, batch)` and let the store guard.
- `chat://citations` (`{run_id, data}`): `JSON.parse(data)` → `Passage[]`, `setCitations(run_id, …)`. (Direct, not buffered — arrives once.)
- `chat://error` (`{run_id, data}`): `setError(run_id, data)`. (Also toasts? No — the Chat view renders the error inline; the awaited `chat()` rejection is what the view catches. Keep the listener to set inline error state.)
- `chat://done` (`{run_id}`): `tokenBuffer.flushNow()` then `finish(run_id)`.
- Register these alongside the ingest listeners; add to the same `unlisteners` array.

### 3c. `features/chat/` — the screen (mobile-first)

Files:
- **`index.tsx`** — shell. Owns view state: `activeConvId: string|null`, drawer flag (mobile conversation
  list), retrieval `opts` (component-local), rename target. Queries:
  `useQuery(['conversations'], listConversations, { staleTime: 30_000 })` and
  `useQuery(['messages', activeConvId], () => getMessages(activeConvId!), { enabled: !!activeConvId, staleTime: 30_000 })`.
  Layout: mobile single column with a "Conversations" button (opens Modal drawer) + MessageList +
  Composer; `md:grid md:grid-cols-[260px_minmax(0,1fr)]` with a left conversation rail (mirror Data
  Explorer's rail/drawer split). Composer sticky at bottom, `pb-[env(safe-area-inset-bottom)]`.
  **Send flow** (see decisions #6/#7):
  ```
  const runId = crypto.randomUUID();
  let convId = activeConvId;
  if (!convId) { const c = await createConversation(); convId = c.id; setActiveConvId(c.id);
                 queryClient.invalidateQueries(['conversations']); }
  useChat.getState().startRun(runId, convId, text);
  try {
    await chat(text, [], opts, convId, runId);          // history [] — server loads from DB
    await queryClient.invalidateQueries(['messages', convId]);
    queryClient.invalidateQueries(['conversations']);   // title/updated_at moved
    useChat.getState().clear();
  } catch (e) { useChat.getState().setError(runId, String(e)); }
  ```
  Lock all controls while `pending && pending.phase !== 'done'`.
- **`ConversationList.tsx`** — used in rail + drawer (like `ConnectionTree`). New-chat button
  (`setActiveConvId(null)` + `useChat.clear()`), list (select → set active + clear pending + close
  drawer), per-item delete (`window.confirm` → `deleteConversation` → invalidate; if active was deleted,
  clear active), rename (inline pencil → small prompt/Modal → `renameConversation` → invalidate). ≥44px
  tap targets. Disabled while streaming.
- **`MessageList.tsx`** — renders `persisted = messagesQuery.data ?? []` then, if
  `pending?.conversationId === activeConvId`, appends the pending user + assistant bubbles. Auto-scroll to
  bottom on `persisted.length`, `pending?.answer`, and mount. Empty state when no messages + no pending.
- **`MessageBubble.tsx`** — user: plain text, right-aligned, accent-subtle bubble. assistant: `<Markdown>`
  body + `<Citations>` + a copy-to-clipboard button (net-new vs reference; drop reference's md/pdf export).
  While the pending assistant is `phase!=='done'` and `answer===''`, show `<ActivityStrip>`.
- **`Markdown.tsx`** — `react-markdown` + `remark-gfm`, Tailwind-styled p/ul/ol/code/pre/table/a/strong.
  Code fences render as styled `<pre>` (```chart handling is Stage 7 — leave a comment). External links
  open safely. Keep it a thin wrapper Stage 7 can extend.
- **`ActivityStrip.tsx`** — client-derived (decision #3). Given `pending`, show spinner + label:
  `phase==='searching'` → "Searching records…"; `phase==='generating' && answer===''` → "Reading
  sources…". Hidden once tokens flow / on done. Small, muted, `Loader2` spinner.
- **`Citations.tsx`** — collapsible "Sources (N)" under an assistant bubble (only if citations present).
  Each source: `source_id` · `row_pk` · score badge; expand to show `text` + `fields`. Reuse `Badge`.
- **`Composer.tsx`** — auto-grow `<textarea>` (max ~5 rows then scroll), Send button (`min-h-[44px]`).
  **Enter = send, Shift+Enter = newline** (deviation from the reference's Ctrl+Enter — better desktop
  ergonomics; the Send button covers mobile). Disabled + spinner while streaming. Sliders icon opens
  `RetrievalSettings`.
- **`RetrievalSettings.tsx`** — `Modal` (bottom-sheet on mobile): mode (vector/hybrid segmented),
  rerank toggle, top_k select (e.g. 4/6/8/10). Writes component-local `opts` passed to `chat()`.
  Unset = server defaults. **Cuttable if it bloats** — core chat works without it.
- **`utils.ts`** — `buildHistory` (not needed — server loads history; omit), `fmtWhen`, truncate helpers.
- **`index.ts`** re-export or default-export `index.tsx` per the existing feature convention (check how
  `features/data-explorer` is imported in `routes.tsx` — it's `import('../features/data-explorer')`, so a
  default export from `index.tsx` suffices).

### 3d. `routes.tsx`
Add: `'/chat': lazy(() => import('../features/chat')) as ComponentType,`. `/chat` is all-roles
(permissions.ts already allows it).

### Agent 3 verification (Opus)
- `cd onprem-rag-app && npx tsc --noEmit` exit 0.
- Read the diffs. Confirm: run_id filtering in the store guards; input/switch/delete locked while
  streaming; `history` passed as `[]` when a conversation exists (server owns history); optimistic pending
  cleared only after `invalidateQueries(['messages'])` resolves (no dupe flash); no per-component
  `listen`/`unlisten` (all chat listeners in `bridgeEvents.ts`); sticky composer clears the bottom bar +
  safe-area at 360px; no horizontal body scroll.

---

## Deliberate deviations from the reference / master plan
- **No server pipeline-step activity events** — client-derived status strip instead (decision #3).
- **Enter-to-send** (reference used Ctrl+Enter).
- **Citations UI added** — the reference has none, but our server already emits `citations`; grounded,
  auditable answers are the product's core value.
- **Per-message copy** added; **md/pdf export dropped**.
- **`run_id` stamped in the bridge**, not the server (server stays correlation-agnostic).
- **Rename route added** (reference had auto-title only) — cheap UX win.

## Out of scope (Stage 7+)
Agent-partitioned conversations + `'auto'` routing (Stage 7); `chart` fence rendering / `AgentChart`
(Stage 7); analytics/alerts.
