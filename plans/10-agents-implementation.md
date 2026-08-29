# 10 — AI Agents implementation (Stage 7)

Status: **IMPLEMENTED** (2026-08-25) — all three layers landed and verified (`cargo check` + `tsc --noEmit` exit 0). Deliberate deviations below are reflected in the shipped code.

Prereqs landed: Stage 6 chat (`plans/09-chat-implementation.md`), and a **pre-Stage-6** `/agents/<kind>`
endpoint + `agent` bridge command already exist and are modernized here.

---

## Governing principle — the reference is a UX guide, not a blueprint

The Electron reference (`On-premise-Rag-system-for-Health-Records/`) shows the *feature shape* and the
*screen layout*. It does **not** dictate the mechanism. Where our Rust backend is already the better
design, we keep ours and adapt the reference's shape onto it. Explicit divergences for this stage:

| Reference does | We do instead | Why |
|---|---|---|
| Agentic LLM tool-loop; emits `tool-call`/`tool-result` steps | Fixed two-path dispatcher (structured aggregation vs semantic retrieval) already built in `agents/routes.rs` | Ours is deterministic, grounded, and already works. Activity strip is **client-derived from SSE phases** (same as Stage 6), no synthetic tool feed. |
| Charts as ` ```chart-json ` fences parsed out of prose | Charts from typed `rows`+`spec` **SSE events** (primary). Fence interceptor ported too (secondary) | Structured events beat regexing a fence out of an LLM answer. Fence path closes the Stage 6 `Markdown` TODO + future-proofs. |
| `userId: 'default'` (single-tenant) | Conversations scoped by **JWT user id** | Multi-tenant is correct; Stage 6 already does this. |
| `runId` generated server-side | `run_id` stamped by the **bridge** | Consistent with Stage 6 chat. |
| 5 agent tabs incl. "ingestion" | 4 kinds + **Auto**; no ingestion agent | We already have a real Ingest wizard + Data Explorer. |
| `'auto'` lives on the Chat screen | `'auto'` is the **default selector on the Agents screen** | We have no Chat mode dropdown; unify auto here. |
| `ai:agent-context` IPC route | Context panel **derived client-side** from existing `/stats` + `/ingest/history` | No new server route needed. |
| — | Agent-partitioned conversations via an `agent_kind` discriminator on the **same** collections | This is the one thing the reference got right — copy it. |

Agent selector set (snake_case kinds): **`auto`** (default) · `health_query` · `trends` · `patient_lookup` · `summarize`.
`chat` is a valid **routed** result (auto may land there) but is not a selectable tab.

---

## Layer 1 — Server (`onprem-rag-server`)

### 1.1 Conversations become agent-aware — `routes/conversations.rs`

The Stage 6 collections (`chat_conversations`, `chat_messages`) gain an optional `agent_kind`
discriminator. Plain-chat conversations have **no** `agent_kind`; agent conversations store the
user's **selected** kind (`"auto"` | `"health_query"` | …).

**Changes:**

1. `ConversationOut` — add `pub agent_kind: Option<String>`.
2. `conv_doc_to_out` — read it: `agent_kind: d.get_str("agent_kind").ok().map(str::to_string)`.
3. `CreateConversationBody` — add `pub agent_kind: Option<String>`.
4. `create_conversation` — when `body.agent_kind` is `Some(k)` (non-empty), add `"agent_kind": k` to the
   inserted doc **and** to the returned `ConversationOut`.
5. `list_conversations` (**plain chat, `GET /conversations`**) — add `"agent_kind": { "$exists": false }`
   to the filter so agent conversations do **not** leak into the Chat screen. This is the only Stage 6
   behavior change; it is safe (existing plain conversations have no `agent_kind`).
6. **New route** `GET /agent-conversations?<kind>` — list agent conversations for one kind:
   filter `doc! { "user_id": &user.id, "agent_kind": kind }`, sort `updated_at: -1`, map with
   `conv_doc_to_out`. Signature: `#[get("/agent-conversations?<kind>")] pub async fn list_agent_conversations(state, user, kind: &str)`.
7. `MessageOut` — add:
   - `pub agent_kind: Option<String>` (routed kind for assistant messages; `None` for user + plain chat)
   - `pub structured: Option<StructuredResult>` where
     ```rust
     #[derive(Debug, Serialize, Deserialize)]
     pub struct StructuredResult {
         pub spec: serde_json::Value,
         pub rows: serde_json::Value,           // [{label,value}]
         pub pipeline: Option<serde_json::Value>,
     }
     ```
8. `msg_doc_to_out` — read `agent_kind` (as above) and parse `structured_json`:
   `d.get_str("structured_json").ok().and_then(|s| serde_json::from_str::<StructuredResult>(s).ok())`.
9. **New helper** `persist_agent_assistant_message` (sibling of `persist_assistant_message`, leaves
   Stage 6 `/chat` untouched):
   ```rust
   pub(crate) async fn persist_agent_assistant_message(
       db: &DocumentDb,
       conversation_id: &str,
       user_id: &str,
       content: &str,
       agent_kind: &str,
       citations: &[crate::retrieval::Passage],   // semantic path; empty for structured
       structured_json: Option<&str>,             // Some("{spec,rows,pipeline}") for structured path
   ) -> AppResult<()>
   ```
   Inserts `role: "assistant"`, `content`, `agent_kind`, and **either** `citations_json` (semantic)
   **or** `structured_json` (structured). Bumps `updated_at`. Reuse the existing `citations_json`
   serialization pattern.

`verify_owned`, `load_history`, `persist_user_message` are reused unchanged.

### 1.2 `/agents/<kind>` — auto routing, `routed` event, conversation persistence — `agents/routes.rs`

**A. `AgentRequest`** — add `pub conversation_id: Option<String>` (keep existing `question`, `intent`).

**B. Auto routing.** Before the existing `parse_kind`, resolve the effective kind:

```rust
let resolved_kind: AgentKind = if kind.eq_ignore_ascii_case("auto") {
    classify_auto(foundry, state, &req.question).await   // never HTTP-errors; falls open to Chat
} else {
    parse_kind(kind)?
};
```

New `classify_auto` — the reference's proven ladder (deterministic → model → fail-open), our way:

1. `classify_lexical(&question)` (already in `aggregation/intent.rs`) → `Option<QueryIntent>`.
2. If `None`: one tiny model classify via `ModelSpec::for_kind(AgentKind::Classify, cfg)` returning
   `{intent: lookup|narrative|aggregation|trend|multi_hop}` → `Option<QueryIntent>`. Any failure
   (foundry down, parse error) → `None`. **Must not propagate an error** — auto never 500s on
   classification.
3. Map `QueryIntent` → `AgentKind`:
   | QueryIntent | AgentKind | Path |
   |---|---|---|
   | `Trend` | `Trends` | structured |
   | `Aggregation` | `HealthQuery` | structured |
   | `Lookup` | `PatientLookup` | semantic |
   | `Narrative` | `Summarize` | semantic |
   | `MultiHop` | `Chat` | semantic |
   | (None) | `Chat` | semantic (safe grounded default) |

   Keep this map in one `fn intent_to_kind(QueryIntent) -> AgentKind` for reuse/testing.

**C. `routed` event.** Emit the resolved kind as the **first** SSE event inside the generator (both
paths), so the UI shows the active agent even for explicit selections:
`yield Event::data(serde_json::to_string(kind_str).unwrap_or_default()).event("routed");`
where `kind_str` is the snake_case serialization of `resolved_kind`. Add `"routed"` to the event-contract
doc-comment table at the top of the file.

**D. Conversation persistence (mirror Stage 6 `/chat` exactly — see `rag/routes.rs`).**
When `req.conversation_id` is `Some(cid)`:

- **Before** the generator (so errors are HTTP, not mid-stream): `verify_owned(&state.db, cid, &user.id)?`;
  `let history = load_history(&state.db, cid, &user.id, N).await;`
  `persist_user_message(&state.db, cid, &user.id, &req.question).await?;`
- **Semantic path only:** feed `history` into `rewrite_query(foundry, &history, &req.question)` (currently
  passes `&[]`). Structured planning stays per-question — history-aware aggregation refinement is a
  **documented Stage 7 limitation** (follow-ups like "now by month" re-plan from scratch).
- Clone `db`, `user.id`, `cid`, `resolved_kind` string, and the already-serialized `citations_json` /
  `spec_json`+`rows_json`+`pipeline_json` into the `'static EventStream!` generator (all are `String` /
  `Clone` — `DocumentDb` is `Clone`; `FoundryManager` is **not** and must not cross the yield, same
  constraint as Stage 6).
- Accumulate `full_answer` from visible tokens (+ think-tail) exactly as Stage 6 does.
- **After** the token loop, guarded by a `had_error` flag: call `persist_agent_assistant_message`.
  Structured path passes `structured_json = Some("{\"spec\":…,\"rows\":…,\"pipeline\":…}")` assembled
  from the three already-serialized strings + empty citations; semantic path passes the `passages` +
  `None`. Warn-not-crash on persist failure.

When `conversation_id` is `None`: **stateless path 100% unchanged** — the eval harness and any other
caller keep working.

**E.** No change to `token_event` / `parse_kind` beyond the doc-comment. `parse_kind` still rejects
truly unknown kinds; `"auto"` is intercepted before it.

### 1.3 Mount — `main.rs`

Mount the new `list_agent_conversations` route alongside the existing conversation routes.

### 1.4 Server verification

`cargo check` clean. Exercise from `server.http` with a real JWT: `POST /agents/auto` (confirm a `routed`
event names a sensible kind), `POST /agents/health_query` with a `conversation_id`, then
`GET /agent-conversations?kind=health_query` and `GET /conversations/<id>/messages` (confirm the assistant
message carries `agent_kind` + `structured`).

---

## Layer 2 — Bridge (`onprem-rag-app/src-tauri` + `src/lib/bridge.ts`)

> **Mirror hazard** (memory `bridge-mirror-structs-drop-fields`): every new server field must appear in
> **both** `commands.rs` and `bridge.ts` or it is silently dropped.

### 2.1 `src-tauri/src/commands.rs`

1. **Mirror structs** — extend the Stage 6 conversation/message mirrors:
   - `Conversation` (or `ConversationOut` mirror): add `agent_kind: Option<String>`.
   - `StoredMessage` (or `MessageOut` mirror): add `agent_kind: Option<String>` and
     `structured: Option<StructuredResult>` with
     `struct StructuredResult { spec: serde_json::Value, rows: serde_json::Value, pipeline: Option<serde_json::Value> }`.
2. **`create_conversation` cmd** — add optional `agent_kind: Option<String>` param; forward it in the
   POST body when present.
3. **New cmd** `list_agent_conversations(kind: String, …) -> Result<Vec<Conversation>, String>` →
   `GET /agent-conversations?kind={kind}` (percent-encode `kind` via the existing helper if present;
   these values are safe snake_case but stay consistent).
4. **Rewrite the `agent` cmd** (drop the pre-Stage-6 raw relay). New signature:
   ```rust
   pub async fn agent(
       kind: String,
       question: String,
       conversation_id: Option<String>,
       run_id: String,
       app: AppHandle,
       bridge: State<'_, Bridge>,
   ) -> Result<(), String>
   ```
   - POST `/agents/{kind}` with body `{ question, conversation_id }`.
   - Relay SSE, **stamping `run_id` into every emit** via the Stage 6 `ChatEvent { run_id, data }`
     envelope (reuse `ChatEvent`; no new struct). Event name mapping:
     `routed → agent://routed`, `spec → agent://spec`, `rows → agent://rows`,
     `pipeline → agent://pipeline`, `citations → agent://citations`, `token → agent://token`,
     `error → agent://error`, `done → agent://done`.
   - **`token`** payloads are JSON-encoded server-side — decode them with the existing `decode_token`
     helper before stamping into `ChatEvent.data` (same as the Stage 6 `chat` cmd). All other payloads
     (`routed`, `spec`, `rows`, `pipeline`, `citations`) are passed through **as-is** (the app JSON-parses
     them).
5. `lib.rs` — register `list_agent_conversations`; `agent` and `create_conversation` are already
   registered (keep). Remove the old `agent` param list from any stale registration comment.

### 2.2 `src/lib/bridge.ts`

1. `AgentKind` — replace the 4-value union with:
   `"auto" | "health_query" | "trends" | "patient_lookup" | "summarize" | "chat"`.
2. Types:
   - `AggRow { label: string; value: number }` (keep).
   - `StructuredResult { spec: AggSpec | Record<string, unknown>; rows: AggRow[]; pipeline?: unknown[] }`.
   - `Conversation` — add `agent_kind?: string`.
   - `StoredMessage` — add `agent_kind?: string` and `structured?: StructuredResult`.
   - Reuse `ChatEvent { run_id: string; data: string }`.
3. **Rewrite `agent()` wrapper** — drop the old per-call `listen`/`unlisten` cleanup pattern. New shape
   mirrors Stage 6 `chat()`:
   ```ts
   export async function agent(
     kind: AgentKind,
     question: string,
     conversationId: string | null,
     runId: string,
   ): Promise<void>   // invoke('agent', { kind, question, conversationId, runId }) — camelCase args
   ```
   No listeners here; events are handled centrally (2.3 in Layer 3).
4. Conversation wrappers: add `listAgentConversations(kind: AgentKind)`; extend `createConversation`
   to accept an optional `agentKind`. `renameConversation` / `deleteConversation` / `getMessages` are
   reused unchanged (they operate by id).

### 2.3 Bridge verification

`cargo check` clean; `tsc --noEmit` clean. Grep both files to confirm `agent_kind` + `structured` are
mirrored.

---

## Layer 3 — App (`onprem-rag-app/src`)

### 3.1 Dependencies — `package.json`

Add: `zod`, `d3-selection`, `d3-array`, `d3-scale`, `d3-shape`, `d3-format` (+ `@types/d3-*`).
(These are the AgentChart runtime deps; `react-markdown`/`remark-gfm` already present from Stage 6.)

### 3.2 Shared chart components — `src/components/chart/`

Port from the reference `src/renderer/components/AgentChart/`, adapted to Tailwind v4 tokens.

- **`chart-schema.ts`** — port verbatim (it is good, self-contained Zod):
  `CHART_TYPES = ['bar','hbar','line','area','donut','grouped-bar','stacked-bar','kpi']`;
  `POINT_SCHEMA {x: string|number, y: number}`; `SERIES_SCHEMA {name, data}`;
  `CHART_SCHEMA {chartType, title, unit?, xLabel?, yLabel?, series[≥1]}`; `ParsedChart`, `ChartPoint`.
  `parseChart(raw)`: JSON path (starts with `{`) → `JSON.parse` → `CHART_SCHEMA.safeParse`, drop
  non-finite `y`; legacy `type:/title:/- label: X | value: Y` grammar otherwise. `hasPlottableData`
  guard: return `null` (render nothing) unless ≥1 point has finite `y > 0`.
- **`AgentChart.tsx`** — port the d3 SVG renderer (8 types; `kpi` = HTML tiles). Uses
  `d3-{selection,array,scale,shape,format}`. `ResizeObserver` observes width, clamps
  `Math.max(280, Math.min(width, 720))`. Reads CSS vars via `getComputedStyle(svgEl)`:
  `--chart-series-1..8`, `--chart-grid`, `--chart-axis`, `--chart-label`, `--chart-value` (already
  defined in `theme.css` per the plan's token block; add any missing `--chart-grid/axis/label/value`).
  Props: `{ data: string }` (raw fence body) **or** add a `{ spec: ParsedChart }` overload so the
  structured path can pass a pre-built chart without round-tripping through a string. Simplest: export
  a `ChartView({ chart: ParsedChart })` inner and have `AgentChart({ data })` call `parseChart` then
  `ChartView`. On `null` → render nothing (no error UI), matching the reference.
- **`specToChart(spec, rows)`** (new, our structured→chart adapter) in `components/chart/`:
  - `chartType`: `spec.time_bucket` present → `'line'`; else `rows.length <= 8` → `'bar'` : `'hbar'`.
  - `title`: from the question (passed in) or a spec-derived label.
  - `unit`: from `spec.metric.op` (`'count'` → "records", `'avg'`/`'sum'` → the field, etc.) — best-effort.
  - single series: `rows.map(r => ({ x: r.label, y: r.value }))`.
  - Returns a `ParsedChart` (or `null` if no plottable rows).

### 3.3 Shared Markdown + fence interception — move `features/chat/Markdown.tsx` → `components/Markdown.tsx`

- Move the file; update the Stage 6 chat import (`features/chat/MessageBubble` → `../../components/Markdown`).
- Add the reference's fence interception (`markdown-components.tsx` logic): a
  `CHART_LANGS = new Set(['language-chart','language-chart-json'])`; override `pre` (peek child
  `className`) and `code` (own `className`) — either match short-circuits to
  `<AgentChart data={String(children).trim()} />`. Failure renders nothing (parseChart → null).
  This closes the Stage 6 `Markdown` TODO. Both Chat and Agents use this one component.

### 3.4 Store — `src/stores/agents.ts` (mirror `stores/chat.ts`)

Single in-flight `pending` run; every setter `run_id`-guarded:

```ts
type AgentPhase = 'routing' | 'planning' | 'running' | 'retrieving' | 'generating' | 'done';
interface AgentPending {
  runId: string;
  conversationId: string;
  selectedKind: AgentKind;      // what the user picked (may be 'auto')
  routedKind: AgentKind | null; // resolved kind from the `routed` event
  user: string;                 // the question (optimistic user bubble)
  answer: string;               // accumulated narration/generation tokens
  citations: Passage[];         // semantic path
  rows: AggRow[] | null;        // structured path
  spec: unknown | null;         // structured path
  pipeline: unknown[] | null;   // structured path
  error: string | null;
  phase: AgentPhase;
}
```

Setters: `startRun(runId, convId, selectedKind, user)` (phase `'routing'`), `setRouted(runId, kind)`
(phase → `'planning'` for structured kinds / `'retrieving'` for semantic), `setSpec`, `setRows`
(phase → `'running'` then generation begins), `setPipeline`, `appendAnswer(runId, batch)`
(phase → `'generating'`), `setCitations`, `setError`, `finish(runId)` (phase → `'done'`), `clear()`.

### 3.5 Boot listeners — `src/lib/bridgeEvents.ts` (+ agent listeners)

Add one block mirroring the Stage 6 chat listeners (registered **once** at boot; `run_id`-filtered):

- `agent://token` — rAF-buffered via a second `createRafBuffer<string>` flushing `appendAnswer`;
  push only when `run_id === useAgents.getState().pending?.runId`.
- `agent://routed` — `JSON.parse(data)` (string) → `setRouted`.
- `agent://spec` — `JSON.parse(data)` → `setSpec`.
- `agent://rows` — `JSON.parse(data)` as `AggRow[]` → `setRows`.
- `agent://pipeline` — `JSON.parse(data)` → `setPipeline`.
- `agent://citations` — `JSON.parse(data)` as `Passage[]` → `setCitations`.
- `agent://error` — `setError`.
- `agent://done` — `tokenBuffer.flushNow()` + `finish`.

All parses wrapped in try/catch (ignore malformed), same as Stage 6.

### 3.6 Feature — `src/features/agents/`

Mirror `features/chat/` composition; mobile-first (design at 360px, layer `md:`/`xl:`).

- **`index.tsx`** — shell:
  - **Agent selector** (segmented control / tab strip, horizontal-scroll on mobile): Auto · Health Query
    · Trends · Patient Lookup · Summarize. Selecting a kind resets `activeConvId` to null and `clear()`s
    the store (switching agent = new partition). Disabled while streaming.
  - **Conversation rail** (md+ `grid-cols-[260px_1fr]`) / **drawer** (mobile Modal, same as chat), backed
    by `useQuery(['agent-conversations', selectedKind], () => listAgentConversations(selectedKind))`.
  - **Context panel** (md+ optional third column or a drawer toggle): "Data Overview" from
    `useQuery(['stats'])` (`getStats`) — connections/tables/records; "Indexed Tables" from
    `useQuery(['ingest-history'])` (`getIngestHistory`); static per-agent "What this agent does" +
    tool-description text (UI-only constants). **No new server route.**
  - **Send flow** (mirror chat): create-if-none via `createConversation(undefined, selectedKind)` →
    `startRun(runId, convId, selectedKind, text)` → `await agent(selectedKind, text, convId, runId)` →
    `await invalidateQueries(['messages', convId])` → `invalidateQueries(['agent-conversations', selectedKind])`
    → `clear()`. On throw → `setError`. **All controls locked while `pending.phase !== 'done'`.**
- **`ConversationList.tsx`** — reuse the chat pattern (new/select/inline-rename/delete-confirm+cascade),
  queryKey `['agent-conversations', selectedKind]`.
- **`MessageList.tsx`** — persisted (`getMessages`) + optimistic pending pair; autoscroll.
- **`MessageBubble.tsx`** — user plain; assistant:
  - **routed-kind badge** (e.g. "Trends") when `agent_kind`/`routedKind` present.
  - **structured** message → render `<AgentChart>` (via `specToChart(structured.spec, structured.rows)`)
    **above** the narration; a collapsible "Query" disclosure showing `pipeline` (like Citations); the
    narration via shared `Markdown`.
  - **semantic** message → `Markdown` narration + `Citations` (reuse chat's Citations component).
  - while `answer` empty and streaming → `AgentActivityStrip`.
- **`AgentActivityStrip.tsx`** — **client-derived** from `pending.phase`:
  `routing → "Choosing the right agent…"`, `planning → "Planning query…"`,
  `running → "Running aggregation…"`, `retrieving → "Retrieving records…"`,
  `generating → "Writing answer…"`. (No server tool events — deliberate, per governing principle.)
- **`Composer.tsx`** — reuse the chat Composer (auto-grow, Enter=send / Shift+Enter=newline, safe-area pb).
- Reuse chat's `Citations.tsx` (import from `features/chat` or lift to `components/` if cleaner).

### 3.7 Route — `src/app/routes.tsx`

Wire `'/agents'` → lazy `features/agents`. Permission is already `admin`/`doctor`/`analyst` in the
Stage 0 matrix — confirm the guard uses it; no matrix change.

### 3.8 App verification

`tsc --noEmit` exit 0. At 360px: agent tab strip scrolls, conversation drawer works, composer clears the
bottom bar, a structured answer shows a chart that fits (no body horizontal scroll), tap targets ≥44px.

---

## Deliberate deviations (record in the progress memory when done)

1. No agentic tool loop / no `tool-call`/`tool-result` events — activity strip is client-derived from
   SSE phase transitions.
2. Charts primarily from structured `rows`/`spec` SSE; fence interceptor ported as the secondary path
   (closes the Stage 6 `Markdown` TODO) but rarely fires given the strict narration prompt.
3. No "ingestion" agent (covered by Ingest wizard + Data Explorer).
4. `'auto'` selector on the Agents screen (reference puts it on Chat); `chat` is a routed result, not a tab.
5. Agent context panel derived client-side from `/stats` + `/ingest/history` — no `/agents/context` route.
6. `run_id` stamped by the bridge, not the server (Stage 6 consistency).
7. Structured planning is **not** history-aware — follow-up refinement re-plans from scratch (documented
   limitation; semantic path *is* history-aware via `rewrite_query`).
8. Auto classification: lexical → optional model fallback → semantic `Chat` default; never HTTP-500s on
   classification failure.

## Verification checklist (per layer, Opus-verified — read diffs, re-run checks)

- **Server:** `cargo check` clean; `server.http` walk (auto routed event, agent conversation persist +
  re-fetch shows `agent_kind` + `structured`).
- **Bridge:** `cargo check` + `tsc` clean; grep confirms `agent_kind`/`structured` mirrored in both files.
- **App:** `tsc` clean; 360px walkthrough; a structured query renders a chart, a semantic query renders
  citations, an auto query shows the routed badge; reload a conversation and confirm the chart/citations
  re-render from stored history.
