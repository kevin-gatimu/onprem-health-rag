// Activity indicator for an agent run.
//
// Renders the pipeline steps the server reports over `agent://stage`; the
// phase-derived labels are the fallback until the first step arrives. Mirror of
// chat/ActivityStrip.tsx with agent-specific fallback wording.
import PipelineTrace from '../../components/PipelineTrace';
import type { AgentPhase } from '../../stores/agents';
import type { StageStep } from '../../stores/stages';

interface AgentActivityStripProps {
  phase: AgentPhase;
  steps: StageStep[];
}

const PHASE_LABELS: Partial<Record<AgentPhase, string>> = {
  routing:    'Choosing the right agent…',
  planning:   'Planning query…',
  running:    'Running aggregation…',
  retrieving: 'Retrieving records…',
  generating: 'Writing answer…',
};

export default function AgentActivityStrip({ phase, steps }: AgentActivityStripProps) {
  const fallback = PHASE_LABELS[phase];
  // 'done' has no label — render nothing once the run is complete, unless the
  // server is still reporting steps we haven't shown.
  if (!fallback && steps.length === 0) return null;

  return <PipelineTrace steps={steps} fallbackLabel={fallback ?? 'Working…'} />;
}
