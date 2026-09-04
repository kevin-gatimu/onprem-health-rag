// The steps a run is going through, as the server reports them.
//
// Shared by the chat and agent activity strips. Before this, both showed a single
// spinner line ("Retrieving records…") for the entire wait — a guess, and the same
// guess whether the server was embedding a query, waiting on a cold model, or
// running SQL against a live source. These steps are the real ones (see the
// server's `progress.rs`), so a slow answer at least explains itself.
import { Check, Loader2 } from 'lucide-react';

import type { StageStep } from '../stores/stages';

export interface PipelineTraceProps {
  steps: StageStep[];
  /** Shown while no step has arrived yet — an older server, or a dropped relay. */
  fallbackLabel: string;
}

/** Sub-second steps read better in ms; anything slower is what the user feels. */
function formatDuration(ms: number | undefined): string | null {
  if (ms == null) return null;
  return ms < 1000 ? `${ms} ms` : `${(ms / 1000).toFixed(1)} s`;
}

export default function PipelineTrace({ steps, fallbackLabel }: PipelineTraceProps) {
  if (steps.length === 0) {
    return (
      <div className="flex items-center gap-2 text-xs text-fg-subtle py-1">
        <Loader2 size={12} className="animate-spin flex-shrink-0" aria-hidden="true" />
        <span>{fallbackLabel}</span>
      </div>
    );
  }

  return (
    <ul className="flex flex-col gap-0.5 py-1" aria-live="polite" aria-busy={steps.some((s) => !s.done)}>
      {steps.map((step, index) => {
        const duration = formatDuration(step.ms);
        return (
          <li
            key={`${step.stage}-${index}`}
            className="flex items-center gap-2 text-xs leading-5"
          >
            <span className="flex-shrink-0 w-3 text-fg-subtle" aria-hidden="true">
              {step.done ? (
                <Check size={12} />
              ) : (
                <Loader2 size={12} className="animate-spin" />
              )}
            </span>
            <span className={step.done ? 'text-fg-muted' : 'text-fg-subtle'}>{step.label}</span>
            {step.detail && <span className="text-fg-muted truncate">— {step.detail}</span>}
            {duration && (
              <span className="ml-auto flex-shrink-0 tabular-nums text-fg-muted">{duration}</span>
            )}
          </li>
        );
      })}
    </ul>
  );
}
