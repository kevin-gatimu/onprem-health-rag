// Chat store — concurrent runs plus per-conversation prompt queues.
import { create } from 'zustand';
import type { Passage, RetrievalOpts, SqlResult, VerifyReport } from '../lib/bridge';

export type ChatPhase = 'searching' | 'generating' | 'done' | 'stopped';

export interface PendingRun {
  runId: string;
  conversationId: string;
  user: string;
  opts: RetrievalOpts;
  answer: string;
  citations: Passage[];
  sqlResult: Partial<SqlResult> | null;
  /** Faithfulness verdict, or null until the post-stream `verify` event arrives
   *  (which it may never do — the check is opt-in server-side). */
  verify: VerifyReport | null;
  error: string | null;
  phase: ChatPhase;
  startedAt: number;
}

export interface QueuedChatPrompt {
  id: string;
  conversationId: string;
  user: string;
  opts: RetrievalOpts;
  queuedAt: number;
}

interface ChatState {
  activeConversationId: string | null;
  runs: Record<string, PendingRun>;
  queues: Record<string, QueuedChatPrompt[]>;
  drafts: Record<string, string>;
  setActiveConversation(id: string | null): void;
  setDraft(conversationKey: string, text: string): void;
  startRun(runId: string, conversationId: string, user: string, opts: RetrievalOpts): void;
  appendAnswer(runId: string, batch: string[]): void;
  setCitations(runId: string, citations: Passage[]): void;
  setSqlMetadata(runId: string, sourceId: string, sql: string): void;
  setSqlColumns(runId: string, columns: string[]): void;
  setSqlRows(runId: string, rows: unknown[][]): void;
  setVerify(runId: string, report: VerifyReport): void;
  setError(runId: string, message: string): void;
  markStopped(runId: string): void;
  finish(runId: string): void;
  removeRun(runId: string): void;
  enqueue(prompt: QueuedChatPrompt): void;
  removeQueued(conversationId: string, promptId: string): void;
  reset(): void;
}

export const useChat = create<ChatState>((set, get) => ({
  activeConversationId: null,
  runs: {},
  queues: {},
  drafts: {},

  setActiveConversation(activeConversationId) {
    set({ activeConversationId });
  },

  setDraft(conversationKey, text) {
    set((state) => ({ drafts: { ...state.drafts, [conversationKey]: text } }));
  },

  startRun(runId, conversationId, user, opts) {
    set((state) => ({
      runs: {
        ...state.runs,
        [runId]: {
          runId,
          conversationId,
          user,
          opts,
          answer: '',
          citations: [],
          sqlResult: null,
          verify: null,
          error: null,
          phase: 'searching',
          startedAt: Date.now(),
        },
      },
    }));
  },

  appendAnswer(runId, batch) {
    const run = get().runs[runId];
    if (!run) return;
    set((state) => ({
      runs: {
        ...state.runs,
        [runId]: { ...state.runs[runId], answer: state.runs[runId].answer + batch.join('') },
      },
    }));
  },

  setCitations(runId, citations) {
    const run = get().runs[runId];
    if (!run) return;
    set((state) => ({
      runs: { ...state.runs, [runId]: { ...state.runs[runId], citations, phase: 'generating' } },
    }));
  },

  setSqlMetadata(runId, source_id, sql) {
    if (!get().runs[runId]) return;
    set((state) => ({
      runs: {
        ...state.runs,
        [runId]: { ...state.runs[runId], sqlResult: { ...state.runs[runId].sqlResult, source_id, sql } },
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

  setVerify(runId, verify) {
    if (!get().runs[runId]) return;
    set((state) => ({
      runs: { ...state.runs, [runId]: { ...state.runs[runId], verify } },
    }));
  },

  setError(runId, error) {
    const run = get().runs[runId];
    if (!run) return;
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
    const run = get().runs[runId];
    if (!run) return;
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
    set({ activeConversationId: null, runs: {}, queues: {}, drafts: {} });
  },
}));
