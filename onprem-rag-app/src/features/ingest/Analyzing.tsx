// Analyzing — shown while analyzeSchema is in flight.
import { Loader2 } from 'lucide-react';

export default function Analyzing() {
  return (
    <div className="rounded-lg border border-border bg-surface p-8 flex flex-col items-center gap-3 text-center">
      <Loader2 size={32} className="animate-spin text-accent" />
      <span className="text-sm font-medium text-fg">Analysing schema with AI…</span>
      <span className="text-xs text-fg-muted max-w-xs">
        Identifying clinical tables, PII columns, and data quality issues.
      </span>
    </div>
  );
}
