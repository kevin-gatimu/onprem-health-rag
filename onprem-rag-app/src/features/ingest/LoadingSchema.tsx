// LoadingSchema — shown while getSchema is in flight.
import { Loader2 } from 'lucide-react';

export default function LoadingSchema() {
  return (
    <div className="rounded-lg border border-border bg-surface p-8 flex flex-col items-center gap-3 text-center">
      <Loader2 size={32} className="animate-spin text-accent" />
      <span className="text-sm font-medium text-fg">Reading source schema…</span>
    </div>
  );
}
