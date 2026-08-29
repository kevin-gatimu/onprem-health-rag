// Stream store — high-frequency buffers fed by the bridge's SSE relays.
//
// The reference appended one React setState per streamed token (cloning the whole
// message array each time), and needed ~15 narrow selectors just to survive
// un-batched ingest progress. We do NOT carry that over. Listeners write into
// mutable buffers (below) and flush to this store on requestAnimationFrame, so a
// 60-token burst becomes one paint, not 60 renders. See bridgeEvents.ts.
import { create } from "zustand";
import type { LogLine } from "../lib/bridge";

/** Max server log lines kept in memory (a ring — oldest dropped past the cap). */
const LOG_RING_CAP = 500;

interface StreamState {
  serverLog: LogLine[];
  /** Append a batch of log lines, trimming to the ring cap. */
  appendServerLog: (lines: LogLine[]) => void;
  clearServerLog: () => void;
}

export const useStream = create<StreamState>((set) => ({
  serverLog: [],
  appendServerLog: (lines) =>
    set((s) => {
      const next = s.serverLog.concat(lines);
      return {
        serverLog: next.length > LOG_RING_CAP ? next.slice(next.length - LOG_RING_CAP) : next,
      };
    }),
  clearServerLog: () => set({ serverLog: [] }),
}));

/**
 * A requestAnimationFrame-coalesced buffer. Callers `push()` items from a hot
 * event listener; `flush` is invoked at most once per frame with everything
 * buffered since the last frame. This is the batching primitive bridgeEvents uses
 * to fold token/log bursts into a single store update per paint.
 */
export function createRafBuffer<T>(flush: (batch: T[]) => void) {
  let buffer: T[] = [];
  let handle: number | null = null;

  const run = () => {
    handle = null;
    if (buffer.length === 0) return;
    const batch = buffer;
    buffer = [];
    flush(batch);
  };

  return {
    push(item: T) {
      buffer.push(item);
      if (handle === null) handle = requestAnimationFrame(run);
    },
    /** Force an immediate flush (e.g. on stream end, so the tail isn't lost). */
    flushNow() {
      if (handle !== null) {
        cancelAnimationFrame(handle);
        handle = null;
      }
      run();
    },
    reset() {
      buffer = [];
      if (handle !== null) {
        cancelAnimationFrame(handle);
        handle = null;
      }
    },
  };
}
