// Client-derived activity indicator. The server emits no pipeline-step events
// (see 09-chat-implementation.md decision #3), so status is inferred from the
// phase transitions the client already observes:
//   phase='searching', answer=''   → "Searching records…"
//   phase='generating', answer=''  → "Reading sources…"
//   tokens flowing / done          → hidden
import { Loader2 } from 'lucide-react';
import type { PendingRun } from '../../stores/chat';

interface ActivityStripProps {
  pending: PendingRun;
}

export default function ActivityStrip({ pending }: ActivityStripProps) {
  // Hidden once tokens are flowing or the run is done.
  if (pending.phase === 'done' || (pending.phase === 'generating' && pending.answer !== '')) {
    return null;
  }

  const label =
    pending.phase === 'searching'
      ? 'Searching records…'
      : 'Reading sources…'; // phase==='generating' && answer===''

  return (
    <div className="flex items-center gap-2 text-xs text-fg-subtle py-1">
      <Loader2 size={12} className="animate-spin flex-shrink-0" aria-hidden="true" />
      <span>{label}</span>
    </div>
  );
}
