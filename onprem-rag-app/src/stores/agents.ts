// Agent store — concurrent runs plus per-conversation prompt queues.
import { create } from 'zustand';
import type { AgentKind, AgentMode, AgentPage, AggRow, ClarifyPayload, Passage, Provenance, RoutedEvent, StageEvent, Suggestion } from '../lib/bridge';
import { applyStageEvent, type StageStep } from './stages';
import { ASK_KIND } from './agentRegistry';

export type AgentPhase =
  | 'routing'
  | 'planning'
  | 'querying'
  | 'running'
  | 'retrieving'
  | 'generating'
  | 'clarifying'
  | 'done'
  | 'stopped';

// Partial SqlResult shape — accumulated field-by-field from SSE events.
export interface AgentSqlResult {
  source_id?: string;
  sql?: string;
  columns?: string[];
  rows?: unknown[][];
  /** QuerySpec IR the SQL came from (plan 06). Spec-derived: no server emits it yet. */
  spec?: unknown;
  /** Human-readable summary of the result (plan 06). Spec-derived: not emitted yet. */
  explanation?: string;
}

export interface AgentPending {
  runId: string;
  conversationId: string;
  selectedKind: AgentKind;
  /** Resolved agent kind from the `routed` SSE event. */
  routedKind: AgentKind | null;
  /** Full routed payload (plan 07) — includes deterministic, focus_used, etc. */
  routed: RoutedEvent | null;
  user: string;
  answer: string;
  citations: Passage[];
  rows: AggRow[] | null;
  /** Enumeration answers only: one page of whole records, not `{label, value}` pairs. */
  listRows: Record<string, unknown>[] | null;
  /** Enumeration answers only: where this page sits in the result set. */
  page: AgentPage | null;
  spec: unknown | null;
  pipeline: unknown[] | null;
  /** Live SQL result accumulated from sql/columns/rows events (agents can return SQL). */
  sqlResult: AgentSqlResult | null;
  /** Execution provenance (plan 06). */
  provenance: Provenance | null;
  /** Follow-up suggestion chips (plan 06). */
  suggestions: Suggestion[];
  /** Active focus entities used to scope this answer (plan 06). */
  focusUsed: string[];
  /** Clarification request — non-null when the router could not resolve a slot (plan 06). */
  clarify: ClarifyPayload | null;
  error: string | null;
  phase: AgentPhase;
  /** Pipeline steps the server reported for this run, oldest first. */
  stages: StageStep[];
  /**
   * The per-turn options this run was sent with. Retained so `retryAgentRun`
   * resends the identical turn (same mode / pinned source / suggestion spec)
   * and so the bubble can badge the mode that actually produced the answer.
   */
  opts?: AgentTurnOpts;
  startedAt: number;
}

/** Per-turn options forwarded to `POST /agents/<kind>` alongside the question. */
export interface AgentTurnOpts {
  mode?: AgentMode;
  sourceId?: string;
  suggestionSpec?: unknown;
}

export interface QueuedAgentPrompt {
  id: string;
  conversationId: string;
  selectedKind: AgentKind;
  user: string;
  /** Carried so a queued or retried turn is sent with the same mode/spec. */
  opts?: AgentTurnOpts;
  queuedAt: number;
}

interface AgentsState {
  activeConversationId: string | null;
  selectedKind: AgentKind;
  /**
   * Active mode, keyed exactly like `drafts` (`"${kind}:${convId ?? '__new__'}"`)
   * so a mode can be chosen before the conversation exists and survives a tab
   * switch, the same way the composer draft does.
   */
  modes: Record<string, AgentMode>;
  /** Last active conversation id per agent kind — restored when re-entering a tab. */
  lastConversationByKind: Record<string, string | null>;
  runs: Record<string, AgentPending>;
  queues: Record<string, QueuedAgentPrompt[]>;
  /**
   * Draft texts keyed by `convId ?? "__new__:" + kind` so switching tabs and
   * back restores the composer contents without losing in-flight text.
   */
  drafts: Record<string, string>;
  setActiveConversation(id: string | null): void;
  setDraft(conversationKey: string, text: string): void;
  /** Select a kind — restores the last open conversation for that kind. */
  setSelectedKind(kind: AgentKind): void;
  /** `conversationKey` uses the same key form as `setDraft`, not a bare id. */
  setMode(conversationKey: string, mode: AgentMode): void;
  startRun(
    runId: string,
    convId: string,
    selectedKind: AgentKind,
    user: string,
    opts?: AgentTurnOpts,
  ): void;
  setRouted(runId: string, kind: AgentKind): void;
  /** Full routed payload handler — derives phase from backend field. */
  setRoutedFull(runId: string, payload: RoutedEvent): void;
  setSpec(runId: string, spec: unknown): void;
  setRows(runId: string, rows: AggRow[]): void;
  setListRows(runId: string, rows: Record<string, unknown>[]): void;
  setPage(runId: string, page: AgentPage): void;
  setPipeline(runId: string, pipeline: unknown[]): void;
  setSqlMetadata(runId: string, sourceId: string, sql: string): void;
  setSqlColumns(runId: string, columns: string[]): void;
  setSqlRows(runId: string, rows: unknown[][]): void;
  setProvenance(runId: string, provenance: Provenance): void;
  setSuggestions(runId: string, suggestions: Suggestion[]): void;
  setFocusUsed(runId: string, focusUsed: string[]): void;
  setClarify(runId: string, clarify: ClarifyPayload): void;
  appendAnswer(runId: string, batch: string[]): void;
  setCitations(runId: string, citations: Passage[]): void;
  pushStage(runId: string, event: StageEvent): void;
  setError(runId: string, message: string): void;
  markStopped(runId: string): void;
  finish(runId: string): void;
  removeRun(runId: string): void;
  enqueue(prompt: QueuedAgentPrompt): void;
  removeQueued(conversationId: string, promptId: string): void;
  reset(): void;
}

export const useAgents = create<AgentsState>((set, get) => ({
  activeConversationId: null,
  selectedKind: ASK_KIND,
  modes: {},
  lastConversationByKind: {},
  runs: {},
  queues: {},
  drafts: {},

  setActiveConversation(activeConversationId) {
    const { selectedKind } = get();
    set((state) => ({
      activeConversationId,
      // Track last conv per kind so re-entering the tab restores it.
      lastConversationByKind: activeConversationId
        ? { ...state.lastConversationByKind, [selectedKind]: activeConversationId }
        : state.lastConversationByKind,
    }));
  },

  setDraft(conversationKey, text) {
    set((state) => ({ drafts: { ...state.drafts, [conversationKey]: text } }));
  },

  setSelectedKind(selectedKind) {
    const lastConv = get().lastConversationByKind[selectedKind] ?? null;
    // Restore the last conversation for this kind instead of resetting to null —
    // ensures draft continuity and that re-entering a tab picks up where you left off.
    set({ selectedKind, activeConversationId: lastConv });
  },

  setMode(conversationKey, mode) {
    set((state) => ({ modes: { ...state.modes, [conversationKey]: mode } }));
  },

  startRun(runId, conversationId, selectedKind, user, opts) {
    set((state) => ({
      runs: {
        ...state.runs,
        [runId]: {
          runId,
          conversationId,
          selectedKind,
          routedKind: null,
          routed: null,
          user,
          answer: '',
          citations: [],
          listRows: null,
          page: null,
          rows: null,
          spec: null,
          pipeline: null,
          sqlResult: null,
          provenance: null,
          suggestions: [],
          focusUsed: [],
          clarify: null,
          error: null,
          phase: 'routing',
          stages: [],
          opts,
          startedAt: Date.now(),
        },
      },
    }));
  },

  setRouted(runId, routedKind) {
    if (!get().runs[runId]) return;
    // Without the full RoutedEvent, derive phase from the kind name only as a
    // coarse heuristic (backend field will refine this via setRoutedFull).
    set((state) => ({
      runs: { ...state.runs, [runId]: { ...state.runs[runId], routedKind, phase: 'retrieving' } },
    }));
  },

  setRoutedFull(runId, payload) {
    const run = get().runs[runId];
    if (!run) return;
    // `payload.backend` is the structured backend; `payload.route` is the route CLASS
    // ("structured" | "semantic" | …) and must never be stored as an agent kind.
    // Non-structured routes carry backend: null, in which case the route class picks
    // the phase.
    let phase: AgentPhase;
    switch (payload.backend) {
      case 'source_sql':  phase = 'querying'; break;
      case 'document_db': phase = 'running'; break;
      default:
        phase = payload.route === 'clarify' ? 'clarifying'
          : payload.route === 'semantic' || payload.route === 'hybrid' ? 'retrieving'
          : 'routing';
    }
    // A service line, when the router resolved one, IS a roster kind — so it is a
    // better routed-kind than the coarse route class.
    const routedKind = payload.service_line ?? run.routedKind;
    // `focus_used` is spec-derived (no server emits it yet); keep the previous value
    // when absent rather than clearing the chip.
    const focusUsed = payload.focus_used ?? run.focusUsed;
    set((state) => ({
      runs: {
        ...state.runs,
        [runId]: { ...state.runs[runId], routed: payload, routedKind, phase, focusUsed },
      },
    }));
  },

  setSpec(runId, spec) {
    if (!get().runs[runId]) return;
    set((state) => ({ runs: { ...state.runs, [runId]: { ...state.runs[runId], spec } } }));
  },

  setRows(runId, rows) {
    if (!get().runs[runId]) return;
    set((state) => ({
      runs: { ...state.runs, [runId]: { ...state.runs[runId], rows, phase: 'running' } },
    }));
  },

  setListRows(runId, listRows) {
    if (!get().runs[runId]) return;
    set((state) => ({
      runs: { ...state.runs, [runId]: { ...state.runs[runId], listRows, phase: 'running' } },
    }));
  },

  setPage(runId, page) {
    if (!get().runs[runId]) return;
    set((state) => ({ runs: { ...state.runs, [runId]: { ...state.runs[runId], page } } }));
  },

  setPipeline(runId, pipeline) {
    if (!get().runs[runId]) return;
    set((state) => ({ runs: { ...state.runs, [runId]: { ...state.runs[runId], pipeline } } }));
  },

  setSqlMetadata(runId, sourceId, sql) {
    if (!get().runs[runId]) return;
    set((state) => ({
      runs: {
        ...state.runs,
        [runId]: {
          ...state.runs[runId],
          sqlResult: { ...state.runs[runId].sqlResult, source_id: sourceId, sql },
        },
      },
    }));
  },

  setSqlColumns(runId, columns) {
    if (!get().runs[runId]) return;
    set((state) => ({
      runs: {
        ...state.runs,
        [runId]: { ...state.runs[runId], sqlResult: { ...state.runs[runId].sqlResult, columns } },
      },
    }));
  },

  setSqlRows(runId, rows) {
    if (!get().runs[runId]) return;
    set((state) => ({
      runs: {
        ...state.runs,
        [runId]: { ...state.runs[runId], sqlResult: { ...state.runs[runId].sqlResult, rows } },
      },
    }));
  },

  setProvenance(runId, provenance) {
    if (!get().runs[runId]) return;
    set((state) => ({ runs: { ...state.runs, [runId]: { ...state.runs[runId], provenance } } }));
  },

  setSuggestions(runId, suggestions) {
    if (!get().runs[runId]) return;
    set((state) => ({ runs: { ...state.runs, [runId]: { ...state.runs[runId], suggestions } } }));
  },

  setFocusUsed(runId, focusUsed) {
    if (!get().runs[runId]) return;
    set((state) => ({ runs: { ...state.runs, [runId]: { ...state.runs[runId], focusUsed } } }));
  },

  setClarify(runId, clarify) {
    if (!get().runs[runId]) return;
    set((state) => ({
      runs: {
        ...state.runs,
        [runId]: { ...state.runs[runId], clarify, phase: 'clarifying' },
      },
    }));
  },

  appendAnswer(runId, batch) {
    if (!get().runs[runId]) return;
    set((state) => ({
      runs: {
        ...state.runs,
        [runId]: {
          ...state.runs[runId],
          answer: state.runs[runId].answer + batch.join(''),
          phase: 'generating',
        },
      },
    }));
  },

  setCitations(runId, citations) {
    if (!get().runs[runId]) return;
    set((state) => ({
      runs: { ...state.runs, [runId]: { ...state.runs[runId], citations } },
    }));
  },

  pushStage(runId, event) {
    const run = get().runs[runId];
    if (!run) return;
    set((state) => ({
      runs: {
        ...state.runs,
        [runId]: { ...state.runs[runId], stages: applyStageEvent(state.runs[runId].stages, event) },
      },
    }));
  },

  setError(runId, error) {
    if (!get().runs[runId]) return;
    set((state) => ({
      runs: { ...state.runs, [runId]: { ...state.runs[runId], error, phase: 'done' } },
    }));
  },

  markStopped(runId) {
    if (!get().runs[runId]) return;
    set((state) => ({
      runs: { ...state.runs, [runId]: { ...state.runs[runId], phase: 'stopped' } },
    }));
  },

  finish(runId) {
    if (!get().runs[runId]) return;
    set((state) => ({
      runs: { ...state.runs, [runId]: { ...state.runs[runId], phase: 'done' } },
    }));
  },

  removeRun(runId) {
    set((state) => {
      const runs = { ...state.runs };
      delete runs[runId];
      return { runs };
    });
  },

  enqueue(prompt) {
    set((state) => ({
      queues: {
        ...state.queues,
        [prompt.conversationId]: [...(state.queues[prompt.conversationId] ?? []), prompt],
      },
    }));
  },

  removeQueued(conversationId, promptId) {
    set((state) => ({
      queues: {
        ...state.queues,
        [conversationId]: (state.queues[conversationId] ?? []).filter((item) => item.id !== promptId),
      },
    }));
  },

  reset() {
    set({
      activeConversationId: null,
      selectedKind: ASK_KIND,
      modes: {},
      lastConversationByKind: {},
      runs: {},
      queues: {},
      drafts: {},
    });
  },
}));
