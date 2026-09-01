// Agent store — concurrent runs plus per-conversation prompt queues.
import { create } from 'zustand';
import type { AgentKind, AggRow, Passage } from '../lib/bridge';

export type AgentPhase =
  | 'routing'
  | 'planning'
  | 'running'
  | 'retrieving'
  | 'generating'
  | 'done'
  | 'stopped';

export interface AgentPending {
  runId: string;
  conversationId: string;
  selectedKind: AgentKind;
  routedKind: AgentKind | null;
  user: string;
  answer: string;
  citations: Passage[];
  rows: AggRow[] | null;
  spec: unknown | null;
  pipeline: unknown[] | null;
  error: string | null;
  phase: AgentPhase;
  startedAt: number;
}

export interface QueuedAgentPrompt {
  id: string;
  conversationId: string;
  selectedKind: AgentKind;
  user: string;
  queuedAt: number;
}

const STRUCTURED_KINDS: AgentKind[] = ['health_query', 'trends'];

interface AgentsState {
  activeConversationId: string | null;
  selectedKind: AgentKind;
  runs: Record<string, AgentPending>;
  queues: Record<string, QueuedAgentPrompt[]>;
  drafts: Record<string, string>;
  setActiveConversation(id: string | null): void;
  setDraft(conversationKey: string, text: string): void;
  setSelectedKind(kind: AgentKind): void;
  startRun(runId: string, convId: string, selectedKind: AgentKind, user: string): void;
  setRouted(runId: string, kind: AgentKind): void;
  setSpec(runId: string, spec: unknown): void;
  setRows(runId: string, rows: AggRow[]): void;
  setPipeline(runId: string, pipeline: unknown[]): void;
  appendAnswer(runId: string, batch: string[]): void;
  setCitations(runId: string, citations: Passage[]): void;
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
  selectedKind: 'auto',
  runs: {},
  queues: {},
  drafts: {},

  setActiveConversation(activeConversationId) {
    set({ activeConversationId });
  },

  setDraft(conversationKey, text) {
    set((state) => ({ drafts: { ...state.drafts, [conversationKey]: text } }));
  },

  setSelectedKind(selectedKind) {
    set({ selectedKind, activeConversationId: null });
  },

  startRun(runId, conversationId, selectedKind, user) {
    set((state) => ({
      runs: {
        ...state.runs,
        [runId]: {
          runId,
          conversationId,
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
          startedAt: Date.now(),
        },
      },
    }));
  },

  setRouted(runId, routedKind) {
    const run = get().runs[runId];
    if (!run) return;
    const phase: AgentPhase = STRUCTURED_KINDS.includes(routedKind) ? 'planning' : 'retrieving';
    set((state) => ({
      runs: { ...state.runs, [runId]: { ...state.runs[runId], routedKind, phase } },
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

  setPipeline(runId, pipeline) {
    if (!get().runs[runId]) return;
    set((state) => ({ runs: { ...state.runs, [runId]: { ...state.runs[runId], pipeline } } }));
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
    set({ activeConversationId: null, selectedKind: 'auto', runs: {}, queues: {}, drafts: {} });
  },
}));
