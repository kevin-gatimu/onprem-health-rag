// Activity indicator for a chat run.
//
// The server streams the pipeline steps it is actually running (`chat://stage`,
// see the server's `progress.rs`), so this renders those. The phase-derived
// wording below is only the fallback for the moment before the first step lands
// — or for a server that doesn't publish them.
import PipelineTrace from '../../components/PipelineTrace';
import type { PendingRun } from '../../stores/chat';

interface ActivityStripProps {
  pending: PendingRun;
}

export default function ActivityStrip({ pending }: ActivityStripProps) {
  // Hidden once tokens are flowing or the run is done.
  if (pending.phase === 'done' || (pending.phase === 'generating' && pending.answer !== '')) {
    return null;
  }

  const fallback =
    pending.phase === 'searching'
      ? 'Searching records…'
      : 'Reading sources…'; // phase==='generating' && answer===''

  return <PipelineTrace steps={pending.stages} fallbackLabel={fallback} />;
}
