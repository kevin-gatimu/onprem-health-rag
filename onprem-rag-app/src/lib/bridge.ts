// Typed wrappers over the Tauri bridge commands. The React app never talks to
// the server directly — every call goes through `invoke` into src-tauri, which
// holds the JWT and performs the actual HTTP request.
import { invoke } from "@tauri-apps/api/core";
import type { Role, User } from "./types";

/** Sentinel the bridge returns (as a rejected invoke) when the server rejects
 *  our token with 401. Kept in sync with `SESSION_EXPIRED` in commands.rs. */
export const SESSION_EXPIRED = "__SESSION_EXPIRED__";

type SessionExpiredHandler = () => void;
let onSessionExpired: SessionExpiredHandler | null = null;

/** Register the callback fired when any authed bridge call gets a 401. Wired
 *  once at boot to the session store (kept out of this module to avoid a
 *  bridge ↔ store import cycle). */
export function setSessionExpiredHandler(fn: SessionExpiredHandler): void {
  onSessionExpired = fn;
}

/** invoke() wrapper for authenticated commands: turns the bridge's 401 sentinel
 *  into a forced-logout callback, and never lets the raw sentinel escape to
 *  callers (they would render "__SESSION_EXPIRED__" in an error surface). */
async function authedInvoke<T>(cmd: string, args?: Record<string, unknown>): Promise<T> {
  try {
    return await invoke<T>(cmd, args);
  } catch (e) {
    if (typeof e === "string" && e === SESSION_EXPIRED) {
      onSessionExpired?.();
      throw new Error("Your session expired. Please sign in again.");
    }
    throw e;
  }
}

export interface HealthStatus {
  status: string;
  service: string;
  version: string;
  documentdb: "up" | "down";
}

export function getServerUrl(): Promise<string> {
  return invoke<string>("get_server_url");
}

export function setServerUrl(url: string): Promise<void> {
  return invoke<void>("set_server_url", { url });
}

export function health(): Promise<HealthStatus> {
  return invoke<HealthStatus>("health");
}

/**
 * Log in with an email OR username plus password. The server matches either and
 * returns the full user identity. Mirrors the server `UserInfo` (id, username,
 * email, name, role) — see the `User` type in types.ts.
 */
export function login(identifier: string, password: string): Promise<User> {
  // The bridge command param is still named `username`; the server's login query
  // matches it against both email and username, so an email works here too.
  return invoke<User>("login", { username: identifier, password });
}

export function me(): Promise<User> {
  return authedInvoke<User>("me");
}

/**
 * Dashboard summary. Mirrors the server's `StatsResponse` (via the bridge's
 * `DashboardStats`) — keep the three in sync (server, commands.rs, here).
 */
export interface DashboardStats {
  total_records: number;
  total_tables: number;
  /** RFC3339 timestamp of the last successful ingest, or null if none yet. */
  last_ingest_at: string | null;
  active_connections: number;
  pending_alerts: number;
  /** "running" when Foundry Local is up on the server, else "stopped". */
  llm_status: string;
}

/** `GET /stats` — one call backing the Dashboard stat grid. */
export function getStats(): Promise<DashboardStats> {
  return authedInvoke<DashboardStats>("get_stats");
}

export function logout(): Promise<void> {
  return invoke<void>("logout");
}

export function isAuthenticated(): Promise<boolean> {
  return invoke<boolean>("is_authenticated");
}

// --- Workstream 3: Foundry Local — hardware, models, streaming test ---

export interface ExecutionProvider {
  name: string;
  registered: boolean;
  /** Coarse accelerator class: "CPU" | "GPU" | "NPU" | "Other". */
  device_kind: string;
  /** Friendly label, e.g. "OpenVINO — Intel GPU/NPU". */
  label: string;
}

/** A physical accelerator detected at the OS level, independent of EP registration. */
export interface DetectedDevice {
  /** "CPU" | "GPU" | "NPU". */
  kind: string;
  name: string;
  vendor: string;
}

export interface HardwareInfo {
  execution_providers: ExecutionProvider[];
  detected_hardware: DetectedDevice[];
  current_chat_model: string;
}

export interface ModelSummary {
  alias: string;
  id: string;
  capabilities: string | null;
  input_modalities: string | null;
  output_modalities: string | null;
  context_length: number | null;
  cached: boolean;
  loaded: boolean;
}

export function getHardware(): Promise<HardwareInfo> {
  return authedInvoke<HardwareInfo>("get_hardware");
}

export function listModels(): Promise<ModelSummary[]> {
  return authedInvoke<ModelSummary[]>("list_models");
}

export interface SelectModelResult {
  /** Resolved variant id now loaded and selected. */
  model: string;
  /** True when the server purged + re-downloaded corrupt cached weights en route. */
  repaired: boolean;
}

/** Download (if needed), load, and select a chat model. Admin only. */
export function selectModel(model: string): Promise<SelectModelResult> {
  return authedInvoke<SelectModelResult>("select_model", { model });
}

export interface EpRegistration {
  success: boolean;
  status: string;
  registered: string[];
  failed: string[];
}

/** Download + register all execution providers available for the host's hardware. Admin only. */
export function registerEps(): Promise<EpRegistration> {
  return authedInvoke<EpRegistration>("register_eps");
}

/**
 * Stream a test completion. Tokens arrive as `chat://token` events, the run ends
 * with `chat://done` (or `chat://error`). The promise resolves when the stream
 * completes. Attach listeners with `@tauri-apps/api/event` before calling.
 */
export function generate(prompt: string): Promise<void> {
  return authedInvoke<void>("generate", { prompt });
}

/** A single downloadable/loadable model variant. Mirrors the server's `VariantInfo`. */
export interface VariantInfo {
  id: string;
  alias: string;
  cached: boolean;
  loaded: boolean;
  current: boolean;
  context_length: number | null;
}

/** One entry in the model-role manifest. Mirrors the server's `ModelRole`. */
export interface ModelRole {
  /** Stable key matching ONPREM_MODEL_* env vars, e.g. `"chat"`. */
  role: string;
  /** Human-readable display label for the Settings table. */
  label: string;
  /** Resolved model alias (follows ONPREM_MODEL_* overrides). */
  model: string;
  /** Inference engine: `"Foundry Local"` or `"fastembed (ONNX)"`. */
  engine: string;
  /** Device placement, e.g. `"GPU"`, `"GPU → CPU"`, `"ORT (CPU/GPU)"`. */
  device: string;
  /** One-sentence description of what this role does. */
  usage: string;
  /** Qwen3 thinking mode for Foundry roles; `null` for fastembed roles. */
  thinking: boolean | null;
  /** `"active"` if wired in the current build; `"planned"` for future workstreams. */
  status: string;
  /** Whether this role is served by Foundry Local (downloadable/loadable variants). */
  managed: boolean;
  /** Downloadable/loadable variants for this role (empty for non-managed roles). */
  variants: VariantInfo[];
  /** The persisted routing override for this role, if an admin has saved one; `null` otherwise. */
  override_variant: string | null;
}

/** `GET /models/roles` — the model-role manifest for this deployment. Pure config read. */
export function getModelRoles(): Promise<ModelRole[]> {
  return authedInvoke<ModelRole[]>("model_roles");
}

/** Persist (or clear, when `variantId` is `null`) a role's model routing override. Admin only. */
export function setRoleModel(role: string, variantId: string | null): Promise<void> {
  return authedInvoke<void>("set_role_model", { role, variantId });
}

/** Result of `deleteModel` — the roles whose saved default named the deleted variant. */
export interface DeleteModelResult {
  cleared_roles: string[];
}

/**
 * Deletes a variant's weights from the server's Foundry model cache and clears any
 * saved role default naming it. Admin only, and destructive — confirm before calling.
 */
export function deleteModel(variantId: string): Promise<DeleteModelResult> {
  return authedInvoke<DeleteModelResult>("delete_model", { variantId });
}

/**
 * Envelope on every `model://*` event (mirror of the bridge's `ModelEvent`) so the
 * boot-time listeners in bridgeEvents.ts can route progress to the right variant.
 */
export interface ModelEvent {
  variant_id: string;
  data: string;
}

/**
 * Download (if needed) and optionally load a model variant. Progress streams back
 * as `model://progress|status` events (handled once, at boot, in bridgeEvents.ts →
 * the models store). Resolves when the stream ends; rejects with the server's
 * error payload. Drive this via `useModels.startDownload`, not directly.
 */
export function pullModel(variantId: string, load: boolean): Promise<void> {
  return authedInvoke<void>("pull_model", { variantId, load });
}

// --- Stage 8: Models + Settings — setup status + per-variant unload ---

/**
 * GPU presence read at the OS level (independent of Foundry EP registration), so
 * it is populated even when the Foundry core is down. Mirrors the server's `GpuInfo`.
 */
export interface GpuInfo {
  has_gpu: boolean;
  gpu_name: string | null;
}

/** One dependency's health line for the Settings service grid. Mirrors `ServiceStatus`. */
export interface ServiceStatus {
  name: string;
  /** "ok" | "error" | "unknown". */
  status: string;
  detail: string | null;
}

/** One mounted volume on the server host. Mirrors the bridge's `DiskSpec`. */
export interface DiskSpec {
  mount: string;
  total_bytes: number;
  available_bytes: number;
}

/** Server host machine facts for the Settings "Server specs" card. Mirrors `ServerSpecs`. */
export interface ServerSpecs {
  hostname: string;
  os: string;
  arch: string;
  cpu_model: string;
  logical_cores: number;
  physical_cores: number | null;
  total_memory_bytes: number;
  disks: DiskSpec[];
  /** GPU/NPU names, formatted "KIND — name". */
  accelerators: string[];
  server_version: string;
}

/**
 * The single payload backing the Settings page. Mirrors the server's `SetupStatus`
 * (and the bridge's `SetupStatus` struct) — keep all three in sync. Degraded-safe:
 * when Foundry is down the model/EP lists are empty and `foundry_endpoint` is `""`.
 */
export interface SetupStatus {
  gpu: GpuInfo;
  /** `foundry.current_model()` if up, else `""`. */
  active_chat_model: string;
  /** `"in-process (native SDK)"` when the core is ready, else `""`. */
  foundry_endpoint: string;
  foundry_ready: boolean;
  services: ServiceStatus[];
  /** Loaded variant ids (empty when Foundry is down). */
  loaded_models: string[];
  /** Cached variant ids (empty when Foundry is down). */
  cached_models: string[];
  /** Same shape as `HardwareInfo.execution_providers` (empty when Foundry is down). */
  execution_providers: ExecutionProvider[];
  /** Host machine facts (never Foundry-derived, so always populated). */
  server_specs: ServerSpecs;
}

/** `GET /setup-status` — one call backing the Settings page. Any authenticated user. */
export function getSetupStatus(): Promise<SetupStatus> {
  return authedInvoke<SetupStatus>("get_setup_status");
}

/**
 * Unload a variant from memory without deleting its weights. Idempotent server-side
 * (unloading a not-resident model is a no-op success). Admin only.
 */
export function unloadModel(variantId: string): Promise<void> {
  return authedInvoke<void>("unload_model", { variantId });
}

/**
 * Persist (or clear, when `variantId` is `null`) a role's model routing override —
 * the "Set as default" action in the Models UI. Alias for {@link setRoleModel} under
 * the name Layer 3 expects; the underlying Tauri command is `set_role_model`
 * (`PUT /settings/router`) — there is no separate `set_router` command. Admin only.
 */
export function setRouter(role: string, variantId: string | null): Promise<void> {
  return setRoleModel(role, variantId);
}

// --- Workstream 4: source connectors ---

export type SourceKind = "postgres" | "mysql" | "mssql";

export interface SourceInfo {
  id: string;
  name: string;
  kind: SourceKind;
  host: string;
  port: number;
  database: string;
  username: string;
  has_password: boolean;
  query: string | null;
  table: string | null;
  created_at: string;
  /**
   * Last-test outcome — the server is stateless (no live pool), so "connected"
   * means the most recent test succeeded, not that a socket is held open.
   */
  status: "connected" | "error" | "disconnected";
  /** RFC3339 timestamp of the last successful test, or null. */
  last_connected: string | null;
  /** Human-readable failure reason from the last test, if it failed. */
  error: string | null;
}

export interface SourceInput {
  name: string;
  kind: SourceKind;
  host: string;
  port?: number | null;
  database: string;
  username: string;
  password: string;
  query?: string | null;
  table?: string | null;
}

/**
 * Editable source fields. Mirrors the server's `SourceUpdate` — every field is
 * optional and a blank/omitted password keeps the stored one.
 */
export interface SourceUpdate {
  name?: string | null;
  kind?: SourceKind | null;
  host?: string | null;
  port?: number | null;
  database?: string | null;
  username?: string | null;
  password?: string | null;
  query?: string | null;
  table?: string | null;
}

export function listSources(): Promise<SourceInfo[]> {
  return authedInvoke<SourceInfo[]>("list_sources");
}

/** Connect and verify a source without saving. Admin only. */
export function testSource(source: SourceInput): Promise<void> {
  return authedInvoke<void>("test_source", { source });
}

/** Test then save a source (password encrypted server-side). Admin only. */
export function saveSource(source: SourceInput): Promise<SourceInfo> {
  return authedInvoke<SourceInfo>("save_source", { source });
}

/** Edit a saved source. Blank/omitted password keeps the stored one. Admin only. */
export function updateSource(id: string, patch: SourceUpdate): Promise<SourceInfo> {
  return authedInvoke<SourceInfo>("update_source", { id, patch });
}

/** Re-test a saved source; the server persists the outcome and returns it. Admin only. */
export function testSavedSource(id: string): Promise<SourceInfo> {
  return authedInvoke<SourceInfo>("test_saved_source", { id });
}

/** Delete a source and its ingested records. Admin only. */
export function deleteSource(id: string): Promise<void> {
  return authedInvoke<void>("delete_source", { id });
}

// --- Workstream 5: ingestion ---

/** One structured log entry in a progress snapshot. */
export interface LogEntry {
  time: string;
  level: "info" | "success" | "warn" | "error" | "divider";
  message: string;
}

/**
 * Full 14-field progress snapshot. Arrives as the payload of `ingest://progress`,
 * `ingest://done`, and `ingest://error` events. Mirrors the server's `IngestProgress`
 * and the bridge's `IngestProgress` struct — all three must stay in sync.
 */
export interface IngestProgress {
  job_id: string;
  /** "running" | "completed" | "partial" | "failed" */
  status: "running" | "completed" | "partial" | "failed";
  /** 1-based index of the table currently being ingested. */
  table_index: number;
  total_tables: number;
  current_table: string;
  /** Rows processed in the current table (this batch). */
  table_rows: number;
  /** Estimated total rows in the current table. */
  table_total: number;
  /** Cumulative rows processed across all tables. */
  processed_rows: number;
  /** Estimated total rows across all selected tables. */
  total_rows: number;
  /** Count of errors (detail in `log` at level "error"). */
  errors: number;
  success_tables: number;
  failed_tables: number;
  /** Cumulative UTF-8 bytes of embedded chunk text. */
  db_size_bytes: number;
  /** Rows annotated by the clinical extractor (plan 25); 0 when it is disabled. */
  extracted_rows: number;
  /** Full current log; server-capped at 500. Replace wholesale each event. */
  log: LogEntry[];
}

/** Column metadata from a source database table. */
export interface ColumnSchema {
  name: string;
  /** Database type string, e.g. "integer", "varchar", "timestamp". */
  type: string;
  nullable: boolean;
  is_primary_key: boolean;
  is_foreign_key: boolean;
  /** Always false from get_schema; analyzeSchema fills it via pii_columns. */
  likely_pii: boolean;
}

/** Table metadata (name, fast row-count estimate, column list). */
export interface TableSchema {
  name: string;
  /** Fast catalog estimate — not an exact count. */
  row_count: number;
  columns: ColumnSchema[];
}

/** Result of POST /schema/analyze. */
export interface SchemaAnalysis {
  summary: string;
  suggested_tables: string[];
  /** table_name → column names likely containing PII. */
  pii_columns: Record<string, string[]>;
  data_quality_notes: string[];
}

/** One table row from the ingest history. */
export interface IngestionHistoryTable {
  /** Composite key `"{source_id}:{table}"`. */
  table_id: string;
  source_table: string;
  row_count: number;
  vector_count: number;
  /** "indexed" | "error" | "indexing" */
  status: string;
  last_ingested: string | null;
  last_embedded_at: string | null;
}

/** Ingest history grouped by source connection. */
export interface IngestionHistoryConnection {
  source_id: string;
  source_name: string;
  /** "postgres" | "mysql" | "mssql" */
  kind: string;
  database: string;
  tables: IngestionHistoryTable[];
  total_rows: number;
  total_vectors: number;
  last_ingested: string | null;
}

/**
 * Start ingesting selected tables from a saved source and follow its progress.
 * Snapshots arrive as `ingest://progress` events, ending with `ingest://done`
 * (payload: final `IngestProgress`) or `ingest://error`. The promise resolves with
 * the job id when the stream completes. Attach listeners with
 * `@tauri-apps/api/event` before calling. Admin only (enforced server-side).
 */
export function startIngest(
  sourceId: string,
  tables: string[],
  excludedColumns?: Record<string, string[]> | null,
  limit?: number | null,
): Promise<string> {
  return authedInvoke<string>("start_ingest", {
    sourceId,
    tables,
    excludedColumns: excludedColumns ?? null,
    limit: limit ?? null,
  });
}

/** `GET /sources/<id>/schema` — enumerate tables + columns from a saved source. */
export function getSchema(sourceId: string): Promise<TableSchema[]> {
  return authedInvoke<TableSchema[]>("get_schema", { sourceId });
}

/** `POST /schema/analyze` — AI-assisted PII detection + schema summary. */
export function analyzeSchema(tables: TableSchema[]): Promise<SchemaAnalysis> {
  return authedInvoke<SchemaAnalysis>("analyze_schema", { tables });
}

/** `GET /ingest/history` — ingested-table records grouped by source. */
export function getIngestHistory(): Promise<IngestionHistoryConnection[]> {
  return authedInvoke<IngestionHistoryConnection[]>("get_ingest_history");
}

/**
 * `DELETE /ingest/table/<sourceId>/<table>` — remove one table's indexed records
 * and its history entry. Admin only (enforced server-side).
 */
export function deleteIngestTable(sourceId: string, table: string): Promise<void> {
  return authedInvoke<void>("delete_ingest_table", { sourceId, table });
}

// --- Stage 5: Data Explorer — browse rows + per-table inspector + deletes ---

/**
 * One row in a records page — the row-grained view of chunk-grained storage.
 * Mirrors the server's `DataRow` (and the bridge's `DataRow` struct); snake_case
 * throughout or serde silently drops fields.
 */
export interface DataRow {
  /** The source row primary key (`row_pk`). */
  id: string;
  source_id: string;
  /** The row's original columns as a free-form JSON object (the `fields` map). */
  data: Record<string, unknown>;
  /** ISO-8601 timestamp string, or null. */
  ingested_at: string | null;
}

/** A paginated page of row-grained records. Mirrors the server's `RecordsPage`. */
export interface RecordsPage {
  rows: DataRow[];
  total: number;
  page: number;
  page_size: number;
  page_count: number;
  has_prev: boolean;
  has_next: boolean;
}

/**
 * One audit log entry. Mirrors the server's `AuditRow` (and the bridge's `AuditRow`
 * struct); snake_case throughout or serde silently drops fields.
 */
export interface AuditRow {
  id: string;
  user_id: string;
  username: string;
  action: string;
  resource: string;
  /** Free-form JSON details, when present. */
  details?: unknown;
  /** RFC3339 timestamp string. */
  timestamp: string;
}

/** A paginated page of audit log entries. Mirrors the server's `AuditPage`. */
export interface AuditPage {
  entries: AuditRow[];
  total: number;
  page: number;
  page_size: number;
  page_count: number;
  has_prev: boolean;
  has_next: boolean;
}

/** The `indexed_tables` slice of the table inspector. Mirrors `TableInfoTable`. */
export interface TableInfoTable {
  /** Composite id `{source_id}:{table}`. */
  id: string;
  source_id: string;
  source_table: string;
  row_count: number;
  vector_count: number;
  status: string;
  last_ingested: string | null;
  last_embedded_at: string | null;
}

/** The source-connection slice — no secrets; null when the source was deleted. */
export interface TableInfoConnection {
  id: string;
  name: string;
  /** "postgres" | "mysql" | "mssql" */
  kind: string;
  host: string;
  port: number;
  database: string;
  username: string;
}

/** One column in the row-derived schema profile. Mirrors `TableProfileColumn`. */
export interface TableProfileColumn {
  name: string;
  /** JSON value type: "string" | "number" | "boolean" | "object" | "null". */
  type: string;
  nullable: boolean;
  selected: boolean;
  pii: boolean;
}

/** The row-derived schema profile. Mirrors the server's `TableProfile`. */
export interface TableProfile {
  columns: TableProfileColumn[];
  pii_columns: string[];
  selected_columns: string[];
}

/** One recent ingest run (connection-scoped). Mirrors the server's `RecentRun`. */
export interface RecentRun {
  id: string;
  status: string;
  rows_processed: number;
  chunks_created: number;
  started_at: string | null;
  completed_at: string | null;
  errors: number;
}

/** The full table inspector payload. Mirrors the server's `TableInfo`. */
export interface TableInfo {
  table: TableInfoTable;
  connection: TableInfoConnection | null;
  profile: TableProfile | null;
  recent_runs: RecentRun[];
}

/**
 * `GET /records` — row-grained, paginated, searchable browse of ingested records
 * for one (source, table). `q` is an optional case-insensitive substring match; the
 * bridge omits it when empty. Any authenticated user.
 */
export function listRecords(
  sourceId: string,
  table: string,
  page: number,
  pageSize: number,
  q?: string | null,
): Promise<RecordsPage> {
  return authedInvoke<RecordsPage>("list_records", {
    sourceId,
    table,
    page,
    pageSize,
    q: q ?? null,
  });
}

/**
 * `GET /tables/<tableId>/info` — inspector for one indexed table (counts, status,
 * source-db connection, row-derived schema profile, recent runs). `tableId` is
 * `{source_id}:{table}`; the bridge percent-encodes the `:`. Any authenticated user.
 */
export function getTableInfo(tableId: string): Promise<TableInfo> {
  return authedInvoke<TableInfo>("get_table_info", { tableId });
}

/**
 * `DELETE /ingest/connection/<sourceId>` — remove every indexed table and all
 * embedded records for one source connection. Returns the raw server JSON
 * (`{ tables_removed }`). Admin only (enforced server-side).
 */
export function deleteIngestConnection(sourceId: string): Promise<unknown> {
  return authedInvoke<unknown>("delete_ingest_connection", { sourceId });
}

/**
 * `DELETE /ingest/all` — clear every record and indexed-table entry across all
 * sources. Returns the raw server JSON (`{ ok: true }`). Admin only (enforced
 * server-side). Destructive — confirm before calling.
 */
export function clearAllRecords(): Promise<unknown> {
  return authedInvoke<unknown>("clear_all_records");
}

/**
 * `GET /audit` — filterable, paginated audit log. Admin only (enforced server-side).
 * Filters are sent only when truthy; an empty `user` or `action` is treated as
 * "no filter". `from`/`to` accept `YYYY-MM-DD` or RFC3339 strings.
 */
export function getAudit(
  filters: { user?: string; action?: string; from?: string; to?: string },
  page: number,
  pageSize: number,
): Promise<AuditPage> {
  return authedInvoke<AuditPage>("get_audit", {
    user: filters.user?.trim() || null,
    action: filters.action || null,
    from: filters.from || null,
    to: filters.to || null,
    page,
    pageSize,
  });
}

// --- Workstream 6: hybrid retrieval + RAG chat ---

export type RetrievalMode = "vector" | "hybrid";

/** A retrieved passage backing an answer. Mirrors the server's `Passage`. */
export interface Passage {
  id: string;
  source_id: string;
  row_pk: string;
  chunk_index: number;
  text: string;
  fields: Record<string, unknown>;
  score: number;
  reranked: boolean;
}

/** One prior conversation turn, sent for history-aware query rewrite. */
export interface ChatTurn {
  role: "user" | "assistant";
  content: string;
}

/** Per-request retrieval overrides; unset fields fall back to server defaults. */
export interface RetrievalOpts {
  mode?: RetrievalMode | null;
  rerank?: boolean | null;
  top_k?: number | null;
}

export interface SearchResponse {
  /** The standalone query used (after history-aware rewrite). */
  query: string;
  /** All queries issued to retrieval (primary + expansions). */
  queries: string[];
  passages: Passage[];
}

/** Debug view of ranked/fused/reranked passages, no generation. */
export function search(query: string, opts: RetrievalOpts = {}): Promise<SearchResponse> {
  return authedInvoke<SearchResponse>("search", { query, opts });
}

// --- Stage 6: persistent conversations ---

/** One conversation entry. Mirrors the server's `ConversationOut`. */
export interface Conversation {
  id: string;
  title: string;
  created_at: string;
  updated_at: string;
  /** Present only for agent conversations; absent for plain chat. */
  agent_kind?: string;
}

/** One stored message in a conversation. Mirrors the server's `MessageOut`. */
export interface StoredMessage {
  id: string;
  role: "user" | "assistant";
  content: string;
  /** `null` for user messages; passage array for assistant messages. */
  citations: Passage[] | null;
  created_at: string;
  /** Present only for agent messages. */
  agent_kind?: string;
  /** Present only for structured-result agent messages. */
  structured?: StructuredResult;
}

/**
 * Payload shape of every `chat://*` event. The bridge stamps `run_id` into each
 * event so listeners can discard events from a superseded run.
 */
export interface ChatEvent {
  run_id: string;
  data: string;
}

/** `GET /conversations` — all conversations for the current user, most-recent first. */
export const listConversations = (): Promise<Conversation[]> =>
  authedInvoke<Conversation[]>("list_conversations");

/** `POST /conversations` — create a new conversation. `title` defaults to "New conversation" server-side.
 * `agentKind`, when provided, marks this as an agent conversation (excluded from `listConversations`;
 * returned by `listAgentConversations` instead). */
export const createConversation = (title?: string, agentKind?: string): Promise<Conversation> =>
  authedInvoke<Conversation>("create_conversation", { title, agentKind });

/** `PATCH /conversations/<id>` — rename a conversation. Returns the updated entry. */
export const renameConversation = (id: string, title: string): Promise<Conversation> =>
  authedInvoke<Conversation>("rename_conversation", { id, title });

/** `DELETE /conversations/<id>` — delete a conversation and cascade its messages. */
export const deleteConversation = (id: string): Promise<void> =>
  authedInvoke<void>("delete_conversation", { id });

/** `GET /conversations/<id>/messages` — all messages in a conversation, oldest first. */
export const getMessages = (id: string): Promise<StoredMessage[]> =>
  authedInvoke<StoredMessage[]>("get_messages", { id });

/** One claim the faithfulness verifier lifted out of an answer, with its verdict. */
export interface ClaimCheck {
  claim: string;
  supported: boolean;
  /** 1-based indices into the citation list. Empty when unsupported. */
  passages: number[];
}

/**
 * Post-stream grounding report (plan 25), delivered as a `chat://verify` event
 * after the answer has finished streaming. `skipped` means the check did not run
 * or could not be trusted — never treat it as a pass; `reason` says why.
 */
export interface VerifyReport {
  status: "supported" | "partial" | "unsupported" | "skipped";
  claims: ClaimCheck[];
  unsupported: number;
  reason?: string;
  /** `[N]` markers in the answer pointing past the end of the citation list. */
  citation_overflow?: number[];
}

/**
 * Ask a grounded question. The passages backing the answer arrive first as a
 * `chat://citations` event (payload: `ChatEvent` with `data` = JSON `Passage[]`),
 * then tokens as `chat://token` (`ChatEvent` with `data` = token string), ending
 * with `chat://done` (or `chat://error`). Every event is enveloped as `ChatEvent`
 * so listeners can filter stale runs by `run_id`.
 *
 * `conversationId`, when non-null, instructs the server to persist the exchange
 * and load history from the DB. `runId` is a client-generated UUID (e.g.
 * `crypto.randomUUID()`) that the bridge stamps into every emitted event.
 */
export function chat(
  question: string,
  history: ChatTurn[] = [],
  opts: RetrievalOpts = {},
  conversationId: string | null = null,
  runId: string,
): Promise<void> {
  return authedInvoke<void>("chat", { question, history, opts, conversationId, runId });
}

// --- Live log stream: server `tracing` events surfaced in the app ---

/** One server log event. `target` is the Rust module path (e.g. `onprem_server::embed`), which the UI filters on. */
export interface LogLine {
  seq: number;
  ts: string;
  level: "TRACE" | "DEBUG" | "INFO" | "WARN" | "ERROR";
  target: string;
  message: string;
}

/**
 * Start relaying the server's `/logs/stream` SSE feed. Idempotent — the bridge
 * runs at most one stream per session. Each line arrives as a `logs://line`
 * event (payload: JSON string of a `LogLine`); backpressure notices arrive as
 * `logs://error`. Panels subscribe via the shared log store, not directly.
 */
export function startLogStream(): Promise<void> {
  return authedInvoke<void>("start_log_stream");
}

// --- Workstream 7: AI Agents ---

/** The agent kinds. `"auto"` lets the server resolve the real kind (announced via the `routed` event). */
export type AgentKind = "auto" | "health_query" | "trends" | "patient_lookup" | "summarize" | "chat";

/** One row from a structured aggregation result (chart-ready). */
export interface AggRow {
  label: string;
  value: number;
}

/** The aggregation spec the planner produced — shown in the "Show query" panel. */
export interface AggSpec {
  collection: string;
  filter: Record<string, unknown>;
  group_by: string[];
  metric: { op: string; field: string | null };
  time_bucket: { field: string; unit: string } | null;
  sort: { by: string; dir: string } | null;
  top_n: number | null;
}

/** Structured aggregation result persisted on assistant messages. Mirrors the server's `StructuredResult`. */
export interface StructuredResult {
  spec: AggSpec | Record<string, unknown>;
  rows: AggRow[];
  pipeline?: unknown[];
}

/**
 * Invoke the AI Agents endpoint. Events are relayed centrally through
 * bridgeEvents.ts as `agent://routed|spec|rows|pipeline|citations|token|error|done`,
 * each enveloped as `ChatEvent` and stamped with `runId` so stale runs are dropped.
 * `conversationId`, when non-null, tells the server to persist the exchange and load
 * history from the DB. `kind` may be "auto" (server resolves the real kind and emits
 * it via the `routed` event first).
 */
export function agent(
  kind: AgentKind,
  question: string,
  conversationId: string | null,
  runId: string,
): Promise<void> {
  return authedInvoke<void>("agent", { kind, question, conversationId, runId });
}

/** `GET /agent-conversations?kind=` — agent conversations for one kind, most-recent first. */
export const listAgentConversations = (kind: AgentKind): Promise<Conversation[]> =>
  authedInvoke<Conversation[]>("list_agent_conversations", { kind });

// --- Stage 9: Profile + User Management ---

/** `GET /auth/users` — list all users sorted by creation date. Admin only. */
export function listUsers(): Promise<User[]> {
  return authedInvoke<User[]>("list_users");
}

/** `POST /auth/users` — create a new user. Admin only. */
export function createUser(input: {
  username: string;
  password: string;
  email?: string;
  name?: string;
  role?: Role;
}): Promise<User> {
  return authedInvoke<User>("create_user", {
    username: input.username,
    password: input.password,
    email: input.email,
    name: input.name,
    role: input.role,
  });
}

/** `PATCH /auth/users/<id>` — update a user's name, email, or role. Admin only. */
export function updateUser(
  id: string,
  patch: { name?: string; email?: string; role?: Role },
): Promise<User> {
  return authedInvoke<User>("update_user", {
    id,
    name: patch.name,
    email: patch.email,
    role: patch.role,
  });
}

/** `DELETE /auth/users/<id>` — delete a user. Admin only. */
export function deleteUser(id: string): Promise<void> {
  return authedInvoke<void>("delete_user", { id });
}

/**
 * `POST /auth/users/<id>/password` — admin-set a user's password. Admin only.
 * No current-password check on the admin path.
 */
export function setUserPassword(id: string, newPassword: string): Promise<void> {
  return authedInvoke<void>("set_user_password", { id, newPassword });
}

/** `PATCH /auth/me` — update own display name. Any authenticated user. */
export function updateMe(name: string): Promise<User> {
  return authedInvoke<User>("update_me", { name });
}

/**
 * `POST /auth/me/password` — change own password. Any authenticated user.
 * Rejects with `"current password is incorrect"` on HTTP 401 from the server.
 */
export function changeMyPassword(currentPassword: string, newPassword: string): Promise<void> {
  return authedInvoke<void>("change_my_password", { currentPassword, newPassword });
}

/** Admin force-logout: invalidate a user's sessions server-side. */
export function forceLogoutUser(id: string): Promise<void> {
  return authedInvoke<void>("force_logout_user", { id });
}
