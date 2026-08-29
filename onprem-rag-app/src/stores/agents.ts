// Agent store — single in-flight agent run.
//
// There is only ever one pending run at a time (the UI locks during streaming).
// Every setter guards on runId so a stale run's late events are silently dropped.
// The boot-time listeners in bridgeEvents.ts write here; the Agents screen reads it.
import { create } from 'zustand';
import type { AgentKind, AggRow, Passage } from '../lib/bridge';

export type AgentPhase =
  | 'routing'
  | 'planning'
  | 'running'
  | 'retrieving'
  | 'generating'
  | 'done';

export interface AgentPending {
  runId: string;
  conversationId: string;
  /** The kind the user selected (may be 'auto' before routing). */
  selectedKind: AgentKind;
  /** Resolved kind emitted by the server's routed event; null until received. */
  routedKind: AgentKind | null;
  /** The user's question text (used for the optimistic user bubble). */
  user: string;
  /** Accumulated streamed answer text. */
  answer: string;
  citations: Passage[];
  /** Structured aggregation rows; null for non-structured kinds. */
  rows: AggRow[] | null;
  /** The aggregation spec the planner produced; null until received. */
  spec: unknown | null;
  /** The raw aggregation pipeline the planner produced; null until received. */
  pipeline: unknown[] | null;
  error: string | null;
  phase: AgentPhase;
}

// Kinds that emit a planning phase (spec + rows) before generating text.
const STRUCTURED_KINDS: AgentKind[] = ['health_query', 'trends'];

interface AgentsState {
  pending: AgentPending | null;
  startRun(runId: string, convId: string, selectedKind: AgentKind, user: string): void;
  /** Record the routed kind and advance the phase accordingly. Ignores stale runId. */
  setRouted(runId: string, kind: AgentKind): void;
  /** Store the aggregation spec. Ignores stale runId. */
  setSpec(runId: string, spec: unknown): void;
  /** Store the aggregation rows and advance phase to 'running'. Ignores stale runId. */
  setRows(runId: string, rows: AggRow[]): void;
  /** Store the raw aggregation pipeline. Ignores stale runId. */
  setPipeline(runId: string, pipeline: unknown[]): void;
  /** Append a batch of token strings and advance phase to 'generating'. Ignores stale runId. */
  appendAnswer(runId: string, batch: string[]): void;
  /** Set citations. Ignores stale runId. */
  setCitations(runId: string, c: Passage[]): void;
  /** Record an error and set phase to 'done'. Ignores stale runId. */
  setError(runId: string, msg: string): void;
  /** Mark phase 'done' (stream completed normally). Ignores stale runId. */
  finish(runId: string): void;
  /** Clear the pending run (call only after invalidateQueries resolves). */
  clear(): void;
}

export const useAgents = create<AgentsState>((set, get) => ({
  pending: null,

  startRun(runId, convId, selectedKind, user) {
    set({
      pending: {
        runId,
        conversationId: convId,
        selectedKind,
        routedKind: null,
        user,
        answer: '',
        citations: [],
        rows: null,
        spec: null,
        pipeline: null,
        error: null,
        phase: 'routing',
      },
    });
  },

  setRouted(runId, kind) {
    if (get().pending?.runId !== runId) return;
    // Structured kinds emit spec + rows before generating text → 'planning';
    // all others go straight to retrieval.
    const phase: AgentPhase = STRUCTURED_KINDS.includes(kind) ? 'planning' : 'retrieving';
    set((s) => ({
      pending: s.pending ? { ...s.pending, routedKind: kind, phase } : null,
    }));
  },

  setSpec(runId, spec) {
    if (get().pending?.runId !== runId) return;
    set((s) => ({
      pending: s.pending ? { ...s.pending, spec } : null,
    }));
  },

  setRows(runId, rows) {
    if (get().pending?.runId !== runId) return;
    set((s) => ({
      pending: s.pending ? { ...s.pending, rows, phase: 'running' } : null,
    }));
  },

  setPipeline(runId, pipeline) {
    if (get().pending?.runId !== runId) return;
    set((s) => ({
      pending: s.pending ? { ...s.pending, pipeline } : null,
    }));
  },

  appendAnswer(runId, batch) {
    if (get().pending?.runId !== runId) return;
    set((s) => ({
      pending: s.pending
        ? { ...s.pending, answer: s.pending.answer + batch.join(''), phase: 'generating' }
        : null,
    }));
  },

  setCitations(runId, c) {
    if (get().pending?.runId !== runId) return;
    set((s) => ({
      pending: s.pending ? { ...s.pending, citations: c } : null,
    }));
  },

  setError(runId, msg) {
    if (get().pending?.runId !== runId) return;
    set((s) => ({
      pending: s.pending ? { ...s.pending, error: msg, phase: 'done' } : null,
    }));
  },

  finish(runId) {
    if (get().pending?.runId !== runId) return;
    set((s) => ({
      pending: s.pending ? { ...s.pending, phase: 'done' } : null,
    }));
  },

  clear() {
    set({ pending: null });
  },
}));
