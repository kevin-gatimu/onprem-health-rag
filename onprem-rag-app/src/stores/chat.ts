// Chat store — single in-flight RAG run.
//
// There is only ever one pending run at a time (the UI locks during streaming).
// Every setter guards on runId so a stale run's late events are silently dropped.
// The boot-time listeners in bridgeEvents.ts write here; the Chat screen reads it.
import { create } from 'zustand';
import type { Passage, VerifyReport } from '../lib/bridge';

export type ChatPhase = 'searching' | 'generating' | 'done';

export interface PendingRun {
  runId: string;
  conversationId: string;
  /** The user's question text (used for the optimistic user bubble). */
  user: string;
  /** Accumulated streamed answer text. */
  answer: string;
  citations: Passage[];
  /** Faithfulness verdict, or null until the post-stream `verify` event arrives
   *  (which it may never do — the check is opt-in server-side). */
  verify: VerifyReport | null;
  error: string | null;
  phase: ChatPhase;
}

interface ChatState {
  pending: PendingRun | null;
  startRun(runId: string, conversationId: string, user: string): void;
  /** Append a batch of token strings. Ignores stale runId. */
  appendAnswer(runId: string, batch: string[]): void;
  /** Set the retrieved passages and advance phase to 'generating'. Ignores stale runId. */
  setCitations(runId: string, c: Passage[]): void;
  /** Attach the post-stream faithfulness report. Ignores stale runId. */
  setVerify(runId: string, report: VerifyReport): void;
  /** Record an error and set phase to 'done'. Ignores stale runId. */
  setError(runId: string, msg: string): void;
  /** Mark phase 'done' (stream completed normally). Ignores stale runId. */
  finish(runId: string): void;
  /** Clear the pending run (call only after invalidateQueries resolves). */
  clear(): void;
}

export const useChat = create<ChatState>((set, get) => ({
  pending: null,

  startRun(runId, conversationId, user) {
    set({
      pending: {
        runId,
        conversationId,
        user,
        answer: '',
        citations: [],
        verify: null,
        error: null,
        phase: 'searching',
      },
    });
  },

  appendAnswer(runId, batch) {
    if (get().pending?.runId !== runId) return;
    set((s) => ({
      pending: s.pending
        ? { ...s.pending, answer: s.pending.answer + batch.join('') }
        : null,
    }));
  },

  setCitations(runId, c) {
    if (get().pending?.runId !== runId) return;
    set((s) => ({
      pending: s.pending
        ? { ...s.pending, citations: c, phase: 'generating' }
        : null,
    }));
  },

  setVerify(runId, report) {
    if (get().pending?.runId !== runId) return;
    set((s) => ({
      pending: s.pending ? { ...s.pending, verify: report } : null,
    }));
  },

  setError(runId, msg) {
    if (get().pending?.runId !== runId) return;
    set((s) => ({
      pending: s.pending
        ? { ...s.pending, error: msg, phase: 'done' }
        : null,
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
