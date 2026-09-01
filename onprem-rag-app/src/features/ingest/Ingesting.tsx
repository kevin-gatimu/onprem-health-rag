// Ingesting — live progress view shown during and after a job.
// Summary rail + live progress panel. On md+ the two sit side by side and the
// panel stretches to the full page height so the log fills a big screen rather
// than sitting in a 256px box under half a viewport of dead space.
import { useEffect, useRef } from 'react';
import {
  Database, CheckCircle2, AlertTriangle, RefreshCw,
} from 'lucide-react';
import { useQuery } from '@tanstack/react-query';
import { listSources } from '../../lib/bridge';
import type { IngestProgress, LogEntry } from '../../lib/bridge';
import { useIngestion } from '../../stores/ingestion';
import { Button } from '../../components/ui';
import { fmtBytes, LOG_ICONS } from './utils';

// ── Helpers ──────────────────────────────────────────────────────────────────

function logLevelClass(level: string): string {
  switch (level) {
    case 'error':   return 'text-danger';
    case 'warn':    return 'text-warning';
    case 'success': return 'text-success';
    default:        return 'text-fg-muted';
  }
}

function progressBarColor(status: IngestProgress['status']): string {
  switch (status) {
    case 'completed': return 'bg-success';
    case 'partial':   return 'bg-warning';
    case 'failed':    return 'bg-danger';
    default:          return 'bg-accent';
  }
}

// ── Sub-components ────────────────────────────────────────────────────────────

function Stat({
  label, value, danger,
}: { label: string; value: string; danger?: boolean }) {
  return (
    <div className="flex flex-col gap-0.5">
      <span className="text-xs text-fg-muted">{label}</span>
      <span className={`text-sm font-semibold ${danger ? 'text-danger' : 'text-fg'}`}>
        {value}
      </span>
    </div>
  );
}

function RunSummary({
  sourceName,
  progress,
  step,
  onNew,
}: {
  sourceName: string;
  progress: IngestProgress | null;
  step: string;
  onNew: () => void;
}) {
  return (
    <div className="rounded-lg border border-border bg-surface p-4 flex flex-col gap-4 md:self-start">
      <div className="flex items-center gap-2">
        <Database size={16} className="text-accent flex-shrink-0" />
        <span className="font-semibold text-fg text-sm truncate">{sourceName}</span>
      </div>
      <div className="grid grid-cols-3 gap-3">
        <Stat label="Tables"    value={String(progress?.total_tables ?? 0)} />
        <Stat label="Processed" value={(progress?.processed_rows ?? 0).toLocaleString()} />
        <Stat
          label="Errors"
          value={(progress?.errors ?? 0).toLocaleString()}
          danger={(progress?.errors ?? 0) > 0}
        />
      </div>
      {step === 'complete' && (
        <Button
          size="sm"
          variant="secondary"
          leftIcon={<RefreshCw size={14} />}
          onClick={onNew}
        >
          New ingestion
        </Button>
      )}
    </div>
  );
}

function ProgressPanel({
  progress,
  step,
  logEndRef,
}: {
  progress: IngestProgress | null;
  step: string;
  logEndRef: React.RefObject<HTMLDivElement | null>;
}) {
  const isIngesting = step === 'ingesting';
  const isComplete  = step === 'complete';

  // Progress bar percentage
  const pct = progress && progress.total_rows > 0
    ? Math.min(100, Math.round((progress.processed_rows / progress.total_rows) * 100))
    : 0;

  // Current-table bar percentage
  const tablePct = progress && progress.table_total > 0
    ? Math.min(100, Math.round((progress.table_rows / progress.table_total) * 100))
    : 0;

  // Title string
  const title = isComplete
    ? progress?.status === 'completed' ? 'Ingestion complete'
      : progress?.status === 'partial'   ? 'Ingestion partially complete'
      : 'Ingestion failed'
    : 'Ingesting…';

  return (
    <div className="rounded-lg border border-border bg-surface p-4 flex flex-col gap-4 md:min-h-0">

      {/* Title + "Table X of Y" sub-line */}
      <div className="flex flex-col gap-1">
        <span className="font-semibold text-fg text-sm">{title}</span>
        {progress?.current_table && isIngesting && (
          <span className="text-xs text-fg-muted">
            Table{' '}
            <strong className="text-fg">{progress.table_index}</strong>
            {' '}of{' '}
            <strong className="text-fg">{progress.total_tables}</strong>
            :{' '}
            <strong className="text-fg">{progress.current_table}</strong>
          </span>
        )}
      </div>

      {/* Overall progress bar */}
      <div className="flex flex-col gap-1">
        <div className="flex items-center justify-between text-xs text-fg-muted">
          <span>Overall</span>
          <span>
            {(progress?.processed_rows ?? 0).toLocaleString()} /{' '}
            {(progress?.total_rows ?? 0).toLocaleString()} rows
          </span>
        </div>
        <div className="h-2 rounded-full bg-elevated overflow-hidden">
          <div
            className={`h-full rounded-full transition-all duration-300 ${
              progress ? progressBarColor(progress.status) : 'bg-accent'
            }`}
            style={{ width: `${pct}%` }}
          />
        </div>
      </div>

      {/* Current-table bar (only while ingesting) */}
      {isIngesting && progress?.current_table && (
        <div className="flex flex-col gap-1">
          <div className="flex items-center justify-between text-xs text-fg-muted">
            <span className="truncate max-w-[60%]">{progress.current_table}</span>
            <span>
              {progress.table_rows.toLocaleString()} /{' '}
              {progress.table_total.toLocaleString()} rows
            </span>
          </div>
          <div className="h-1.5 rounded-full bg-elevated overflow-hidden">
            <div
              className="h-full rounded-full bg-accent transition-all duration-300"
              style={{ width: `${tablePct}%` }}
            />
          </div>
        </div>
      )}

      {/* Stats row */}
      <div className="grid grid-cols-2 sm:grid-cols-4 gap-3">
        <Stat label="Processed"  value={(progress?.processed_rows ?? 0).toLocaleString()} />
        <Stat label="Total est." value={(progress?.total_rows ?? 0).toLocaleString()} />
        <Stat
          label="Errors"
          value={(progress?.errors ?? 0).toLocaleString()}
          danger={(progress?.errors ?? 0) > 0}
        />
        <Stat
          label="DB size"
          value={progress && progress.db_size_bytes > 0 ? fmtBytes(progress.db_size_bytes) : '—'}
        />
        {/* Clinical extractor (plan 25). Hidden entirely when the server has it off,
            which is the default — a permanent "Annotated 0" would read as a fault. */}
        {(progress?.extracted_rows ?? 0) > 0 && (
          <Stat
            label="Annotated"
            value={(progress?.extracted_rows ?? 0).toLocaleString()}
          />
        )}
      </div>

      {/* Live log panel */}
      <div className="overflow-y-auto max-h-48 sm:max-h-64 md:max-h-none md:min-h-48 md:flex-1 rounded-md bg-elevated p-2 font-mono text-xs">
        {(progress?.log ?? []).map((entry: LogEntry, i: number) =>
          entry.level === 'divider' ? (
            <hr key={i} className="border-border my-1" />
          ) : (
            <div key={i} className={`flex gap-1.5 leading-relaxed ${logLevelClass(entry.level)}`}>
              <span className="text-fg-subtle whitespace-nowrap flex-shrink-0">{entry.time}</span>
              <span className="flex-shrink-0">{LOG_ICONS[entry.level] ?? '·'}</span>
              <span className="break-words min-w-0">{entry.message}</span>
            </div>
          ),
        )}
        {isIngesting && (
          <span className="inline-block animate-pulse text-accent">▋</span>
        )}
        <div ref={logEndRef} />
      </div>

      {/* Completion notes */}
      {isComplete && progress?.status === 'completed' && (
        <div className="flex items-start gap-2 text-sm text-success">
          <CheckCircle2 size={16} className="flex-shrink-0 mt-0.5" />
          <span>Records indexed successfully. You can now use the Chat and Data Explorer pages.</span>
        </div>
      )}
      {isComplete && progress?.status === 'partial' && (
        <div className="flex items-start gap-2 text-sm text-warning">
          <AlertTriangle size={16} className="flex-shrink-0 mt-0.5" />
          <span>
            {progress.success_tables} of {progress.total_tables} tables ingested.{' '}
            {progress.failed_tables} table{progress.failed_tables !== 1 ? 's' : ''} failed —
            check the log.
          </span>
        </div>
      )}
      {isComplete && (progress?.status === 'failed' || !progress) && (
        <div className="flex items-start gap-2 text-sm text-danger">
          <AlertTriangle size={16} className="flex-shrink-0 mt-0.5" />
          <span>All tables failed to ingest. Check the log above for details.</span>
        </div>
      )}

    </div>
  );
}

// ── Main component ────────────────────────────────────────────────────────────

export default function Ingesting() {
  const step        = useIngestion((s) => s.step);
  const sourceId    = useIngestion((s) => s.sourceId);
  const progress    = useIngestion((s) => s.progress);
  const resetWizard = useIngestion((s) => s.resetWizard);

  const { data: sources } = useQuery({
    queryKey: ['sources'],
    queryFn: listSources,
    staleTime: 30_000,
  });

  const sourceName =
    sources?.find((s) => s.id === sourceId)?.name ?? sourceId ?? 'source';

  const logEndRef = useRef<HTMLDivElement | null>(null);

  // Auto-scroll log to bottom whenever the log array changes.
  useEffect(() => {
    logEndRef.current?.scrollIntoView({ behavior: 'auto' });
  }, [progress?.log]);

  return (
    // Stacked and free-flowing on phones; a fixed summary rail plus a
    // full-height progress panel from md up.
    <div className="flex flex-col gap-4 md:grid md:min-h-0 md:flex-1 md:grid-cols-[18rem_minmax(0,1fr)] xl:grid-cols-[22rem_minmax(0,1fr)]">
      <RunSummary
        sourceName={sourceName}
        progress={progress}
        step={step}
        onNew={resetWizard}
      />
      <ProgressPanel
        progress={progress}
        step={step}
        logEndRef={logEndRef}
      />
    </div>
  );
}
