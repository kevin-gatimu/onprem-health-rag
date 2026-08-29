// Client-derived activity indicator for agent runs. The server emits a `routed`
// event that advances the phase; the UI maps phase → a human-readable label.
// Mirror of chat/ActivityStrip.tsx with agent-specific phase labels.
import { Loader2 } from 'lucide-react';
import type { AgentPhase } from '../../stores/agents';

interface AgentActivityStripProps {
  phase: AgentPhase;
}

const PHASE_LABELS: Partial<Record<AgentPhase, string>> = {
  routing:    'Choosing the right agent…',
  planning:   'Planning query…',
  running:    'Running aggregation…',
  retrieving: 'Retrieving records…',
  generating: 'Writing answer…',
};

export default function AgentActivityStrip({ phase }: AgentActivityStripProps) {
  const label = PHASE_LABELS[phase];
  // 'done' has no label — render nothing once the run is complete.
  if (!label) return null;

  return (
    <div className="flex items-center gap-2 text-xs text-fg-subtle py-1">
      <Loader2 size={12} className="animate-spin flex-shrink-0" aria-hidden="true" />
      <span>{label}</span>
    </div>
  );
}
