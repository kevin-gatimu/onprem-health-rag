// Live pipeline steps for one chat/agent run.
//
// The server publishes a start and an end for every stage it goes through
// (`progress.rs`), which the bridge relays as `chat://stage` / `agent://stage`.
// Both the chat and agent stores fold those events with `applyStageEvent`, and
// both activity strips render the result — so what the user watches is what the
// server actually did, not a guess based on elapsed time.
import type { StageEvent } from '../lib/bridge';

export interface StageStep {
  /** Stable stage key from the server, e.g. `"rerank"`. */
  stage: string;
  /** Human-readable label to show, e.g. `"Re-ranking the best matches"`. */
  label: string;
  /** PHI-free detail once known, e.g. `"7 rows from the live database"`. */
  detail?: string;
  /** Wall time once the step finished. */
  ms?: number;
  done: boolean;
}

/** Plenty for the longest pipeline; guards against a pathological event storm. */
const MAX_STEPS = 24;

/**
 * Fold one server stage event into the step list. Stages legitimately repeat
 * within a run (a live-SQL attempt and then an aggregation both "execute"), so
 * every `start` appends rather than deduping, and an `end` closes the most
 * recent open step with that key.
 */
export function applyStageEvent(steps: StageStep[], event: StageEvent): StageStep[] {
  if (event.status === 'done') {
    return steps.map((step) => (step.done ? step : { ...step, done: true }));
  }

  if (event.status === 'start') {
    const next = [...steps, { stage: event.stage, label: event.label, done: false }];
    return next.length > MAX_STEPS ? next.slice(next.length - MAX_STEPS) : next;
  }

  // 'end' — close the newest step with this key, or record it if we never saw
  // the start (a late subscription can miss one).
  for (let i = steps.length - 1; i >= 0; i -= 1) {
    if (steps[i].stage === event.stage) {
      const next = [...steps];
      next[i] = {
        ...steps[i],
        done: true,
        ms: event.ms ?? steps[i].ms,
        detail: event.detail ?? steps[i].detail,
      };
      return next;
    }
  }
  return [
    ...steps,
    {
      stage: event.stage,
      label: event.label,
      done: true,
      ms: event.ms,
      detail: event.detail,
    },
  ];
}
