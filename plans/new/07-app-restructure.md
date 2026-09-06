<!-- markdownlint-disable MD013 -->

# 07 — App restructure (Tauri bridge + React)

**Goal:** the app renders the hospital roster from the server, shows only usable agents, exposes
Trends/Handover as toggles, displays scope, provenance, focus and suggestions, and keeps drafts
and conversations continuous across tabs. No agent names are hardcoded in the web layer.

**Depends on:** 05 (`GET /agents`, new `AgentKind`), 06 (`suggestions`, `focus_used`, `clarify`).

---

## 1. Bridge (Rust, `src-tauri/src/commands.rs`)

New/changed commands (all keep the JWT in `Bridge` state):

```rust
#[tauri::command] pub async fn list_agents(bridge) -> Result<Vec<AgentInfo>, String>            // GET /agents
#[tauri::command] pub async fn agent(kind: String, question: String, conversation_id: Option<String>, run_id: String,
                                     mode: Option<String>, source_id: Option<String>, suggestion_spec: Option<serde_json::Value>, app, bridge) -> Result<(), String>
#[tauri::command] pub async fn get_source_binding(source_id: String, bridge) -> Result<serde_json::Value, String>   // admin
#[tauri::command] pub async fn rebuild_source_binding(source_id: String, bridge) -> Result<serde_json::Value, String>
#[tauri::command] pub async fn get_catalog_overrides / save_catalog_overrides(source_id, body)  // extended MetadataOverrides
```

Mirror structs (server → bridge → TS, all three must match):

```rust
pub struct AgentInfo { pub kind: String, pub label: String, pub blurb: String, pub tier: u8, pub usable: bool,
                       pub modes: Vec<String>, pub sources: Vec<AgentSourceScope>, pub example_questions: Vec<String> }
pub struct AgentSourceScope { pub source_id: String, pub tables: Vec<String> }
pub struct Suggestion { pub text: String, pub kind: String, pub spec: Option<serde_json::Value>, pub agent: Option<String> }
pub struct Provenance { pub path: Vec<serde_json::Value>, pub backend: String, pub service_line: Option<String>,
                        pub scope: Vec<String>, pub source_id: Option<String>, pub elapsed_ms: serde_json::Value }
pub struct StoredMessage { /* existing */ pub provenance: Option<Provenance>, pub spec: Option<serde_json::Value>,
                           pub suggestions: Option<Vec<Suggestion>>, pub focus_used: Option<Vec<String>>, pub clarify: Option<serde_json::Value>, pub mode: Option<String> }
pub struct SqlResult { /* existing */ pub spec: Option<serde_json::Value>, pub explanation: Option<String> }
```

SSE relay: forward the new events `provenance`, `suggestions`, `clarify` under the existing
envelope (`ChatEvent { run_id, data }`) as `agent://provenance`, `agent://suggestions`,
`agent://clarify` (and `chat://*` equivalents). `routed` payload is passed through unchanged (it
now includes `service_line`, `deterministic`, `focus_used`).

`AgentKind` in the bridge becomes `String` (validated server-side) so new lines never require a
bridge rebuild.

## 2. TypeScript types (`src/lib/bridge.ts`)

```ts
export type AgentKind = string;                     // "ask" | service-line slug; legacy names accepted by server
export type AgentMode = "ask" | "trends" | "handover";
export interface AgentInfo { kind: AgentKind; label: string; blurb: string; tier: 1|2|3; usable: boolean; modes: AgentMode[]; sources: { source_id: string; tables: string[] }[]; example_questions: string[] }
export interface Suggestion { text: string; kind: "drill"|"widen"|"compare"|"switch"|"explain"; spec?: unknown; agent?: AgentKind }
export interface Provenance { path: ProvenanceRung[]; backend: "source_sql"|"document_db"|"semantic"|"hybrid"|"none"; service_line?: string; scope: string[]; source_id?: string; elapsed_ms: Record<string, number> }
export type ProvenanceRung = { rung: string; result: "hit" | "miss" | "skipped"; reason?: string }
export interface RoutedEvent { route: string; intent: string|null; backend: string|null; tier: number; cached: boolean; deterministic: boolean; service_line?: string; focus_used?: string[]; question?: string; slot?: string }
export interface StoredMessage { /* existing */ provenance?: Provenance; spec?: unknown; suggestions?: Suggestion[]; focus_used?: string[]; clarify?: { slot: string; options: string[] }; mode?: AgentMode }
export function listAgents(): Promise<AgentInfo[]>
export function agent(kind: AgentKind, question: string, conversationId: string|null, runId: string, opts?: { mode?: AgentMode; sourceId?: string; suggestionSpec?: unknown }): Promise<void>
```

## 3. Stores

### `src/stores/agentRegistry.ts` (new)

`useAgentRegistry`: `{ agents: AgentInfo[]; loaded: boolean; load(): Promise<void>; byKind(kind): AgentInfo|undefined; usable(): AgentInfo[]; tier(n): AgentInfo[] }`. Loaded once after login (in `bridgeEvents.initBridgeEvents` follow-up or `App.tsx` auth effect); refreshed when the Settings page rebuilds a binding. Falls back to `[{kind:"ask", …}]` if the server is old.

### `src/stores/agents.ts` (changes)

- `selectedKind: AgentKind` (string), `mode: AgentMode` per conversation (`modes: Record<convId, AgentMode>`).
- `AgentPending` gains `provenance: Provenance|null`, `suggestions: Suggestion[]`, `focusUsed: string[]`, `clarify: {question, slot, options}|null`, `routed: RoutedEvent|null`, `sqlResult: Partial<SqlResult>|null` (agents can now return live SQL — reuse the chat store's accumulator logic).
- `STRUCTURED_KINDS` removed; structured-ness is derived from `routed.backend`/`provenance`.
- **Draft continuity**: `setSelectedKind` no longer resets `activeConversationId`; drafts are keyed by conversation (`drafts[convId ?? "__new__:"+kind]`) so switching tabs and back restores the text. Add `lastConversationByKind: Record<AgentKind, string|null>` so re-entering a tab restores its last conversation.
- Event subscribers: handle `agent://provenance`, `agent://suggestions`, `agent://clarify`, and set `phase` from `routed.backend` (`source_sql → 'querying'`, `document_db → 'running'`, `semantic → 'retrieving'`, `hybrid → 'running'`), plus new `AgentPhase` values `'clarifying'`.

### `src/stores/chat.ts`

Same additions for `/chat` (`provenance`, `suggestions`, `focusUsed`, `clarify`) since Ask on the `/chat` route uses the same executor.

## 4. Features

### `src/features/agents/index.tsx`

- Delete `KIND_TABS`, `KIND_BLURBS`. Render tabs from `useAgentRegistry().usable()`: **Ask** first, then Tier 1 lines; Tier 2/3 collapse into a "More ▾" menu (data-map §3). Unusable lines appear greyed in "More" with the tooltip "No bound tables in connected sources".
- Tab bar right side: **Mode toggle** segmented control `Ask | Trends | Handover` (hidden for lines whose `modes` lacks one). Mode is sent with each prompt and shown as a badge on the assistant bubble.
- Right context panel (xl+) becomes **Scope panel** (`ScopePanel.tsx`): agent blurb, "Data this agent can see" — table chips grouped by source (from `AgentInfo.sources`), `example_questions` as clickable chips (empty-conversation state shows them centered in the message pane as well).
- Ask tab still renders the chat layout but now also shows the department badge from `routed.service_line` on each answer and a "Open in {Line}" link that switches tab **and carries the conversation** (server allows an `agent_kind` change via `PATCH /conversations/<id>` — add `{ agent_kind }` to the PATCH body server-side; the conversation's focus travels with it).

### `src/features/agents/MessageBubble.tsx`

- Badges: routed line label (from registry, not `KIND_LABELS`), mode, `deterministic` lightning icon, `focus_used` chip ("using: patient · period").
- Result blocks: `SqlResultTable` (reuse from chat) when `sql_result`; `AgentChart` when `structured`; `Citations` when citations; clarify block (question + option buttons that submit the option as the next prompt).
- **Provenance disclosure** (`ProvenanceStrip.tsx`, new, shared with chat): compact horizontal rung list `Link ✓ → Deterministic SQL ✓ → Validate ✓ → Execute ✓ · 212 ms`, misses shown with their PHI-free reason on hover; expand to see `sql` + `explanation` (SourceSql), `spec` + `pipeline` (DocDb), or the retrieval filter (semantic).
- **Suggestions** (`SuggestionChips.tsx`, new): ≤ 4 chips under the last assistant message only; click → `submitAgentPrompt(text, { suggestionSpec: spec, kind: suggestion.agent ?? current })`; `switch` kind renders with an arrow icon and changes tab (carrying the conversation as above).

### `src/features/chat/*`

Ask on `/chat` gets the same `ProvenanceStrip`, `SuggestionChips`, clarify block and department badge (components are shared from `src/components/answer/`).

### `src/features/settings` — new **Data Binding** section (admin)

- Per source: coverage table (13 lines × usable/tables/missing concepts), "Rebuild binding", history diff.
- Table list with detected concept + confidence; inline override select (concept or *ignore*); per-column role override in an expandable row; service-line add/remove. Saves through `save_catalog_overrides` (extended body) and refreshes the registry.
- Orphans warning banner (tables with no service line).

### Navigation

`navigation.ts`: label "AI Agents" → "Hospital Agents"; keep `/chat` → agents feature mapping.

## 5. Conversation continuity rules (UX contract)

1. Switching tabs never discards the composer draft.
2. Re-entering a tab restores its last open conversation.
3. A conversation can move between agents ("Open in Maternity"); its focus and history come with it; the bubble badge shows which agent answered each turn.
4. Clicking a suggestion is a normal turn (persisted as the user message text) — never a hidden action.
5. Clarify options are buttons; free-text answers work too.

## 6. Legacy

Old persisted conversations with `agent_kind ∈ {health_query, trends, patient_lookup, summarize, chat}` are listed under **Ask** with a small legacy badge; the server maps them (plan 05 §1).

## 7. Tests / checks

- `npx tsc --noEmit`; `cargo check` in `src-tauri`.
- Bridge round-trip test (Rust unit): server `StoredMessage` JSON with all new fields deserialises without loss.
- Browser harness (repo memory: mockIPC + relay on 8010): registry renders 13 + Ask tabs on the dev seed; suggestion click produces a `routed` with `deterministic: true`; draft survives tab switch; clarify option click submits.
- Visual check of `ProvenanceStrip` for the four backends.

## 8. Acceptance

- No string literal of a service-line slug or label in `src/` outside tests (registry-driven).
- All new server fields visible in the UI (provenance, suggestions, focus chip, mode badge, scope panel).
- Draft continuity and conversation hand-off verified in the harness.
