// TableInspector — drawer (Modal in its mobile bottom-sheet / desktop-centered
// form) for one indexed table: counts + status, admin re-ingest with inline
// progress, admin clear-table, source-db connection info, row-derived schema
// profile, and connection-scoped recent runs.
//
// Re-ingest reuses the global ingestion store: startIngest streams ingest://
// progress events that the boot listener fans into useIngestion.progress. We
// filter that stream to `current_table === this table` for the inline bar, and
// treat the awaited startIngest resolution as the terminal signal that triggers
// cache invalidation.
import { useState } from 'react';
import type { ReactNode } from 'react';
import { Play, Trash2, KeyRound, Loader2 } from 'lucide-react';
import { useQuery, useQueryClient } from '@tanstack/react-query';
import { getTableInfo, startIngest, deleteIngestTable } from '../../lib/bridge';
import { useIngestion } from '../../stores/ingestion';
import { toast } from '../../stores/ui';
import { Modal, Button, Badge } from '../../components/ui';
import type { SelectedTable } from './index';
import { fmtDate, fmtNum, statusTone } from './utils';

interface TableInspectorProps {
  open: boolean;
  onClose: () => void;
  selected: SelectedTable | null;
  isAdmin: boolean;
  /** Called after a clear-table so the shell drops the selection + closes this. */
  onClearedTable: () => void;
}

export default function TableInspector({
  open,
  onClose,
  selected,
  isAdmin,
  onClearedTable,
}: TableInspectorProps) {
  const queryClient = useQueryClient();
  const tableId = selected?.table.table_id ?? null;

  const { data: info, isLoading, isError } = useQuery({
    queryKey: ['table-info', tableId],
    queryFn: () => getTableInfo(tableId as string),
    enabled: open && !!tableId,
    staleTime: 30_000,
  });

  // Live re-ingest progress from the shared ingestion store, filtered to this table.
  const progress = useIngestion((s) => s.progress);
  const [reingesting, setReingesting] = useState(false);
  const [clearing, setClearing] = useState(false);

  const reProg =
    reingesting && progress && progress.current_table === selected?.table.source_table
      ? progress
      : null;

  async function handleReingest() {
    if (!selected) return;
    const { conn, table } = selected;
    // The re-ingest streams through the SHARED ingestion store: applyProgress flips
    // the wizard's `step` to 'complete' on the terminal event. Snapshot it here and
    // restore in `finally` so the Ingest wizard isn't left stranded on a completion
    // screen for a job it didn't start (while preserving any real in-progress step).
    const prevStep = useIngestion.getState().step;
    setReingesting(true);
    toast.info(`Re-ingesting "${table.source_table}"…`);
    try {
      // Resolves with the job id when the stream completes (terminal signal).
      await startIngest(conn.source_id, [table.source_table]);
      const status = useIngestion.getState().progress?.status;
      queryClient.invalidateQueries({ queryKey: ['records', table.table_id] });
      queryClient.invalidateQueries({ queryKey: ['table-info', table.table_id] });
      queryClient.invalidateQueries({ queryKey: ['ingest-history'] });
      queryClient.invalidateQueries({ queryKey: ['stats'] });
      if (status === 'failed') toast.error('Re-ingest failed. Check the server logs.');
      else if (status === 'partial') toast.warning('Re-ingest finished with some errors.');
      else toast.success('Re-ingest complete.');
    } catch (err) {
      toast.error(`Could not re-ingest: ${(err as Error).message}`);
    } finally {
      setReingesting(false);
      useIngestion.setState({ step: prevStep });
    }
  }

  async function handleClearTable() {
    if (!selected) return;
    const { conn, table } = selected;
    if (
      !window.confirm(
        `Clear "${table.source_table}"?\n\nThis removes its rows, vectors and lineage from the local store. The source database is not touched.`,
      )
    )
      return;
    setClearing(true);
    try {
      await deleteIngestTable(conn.source_id, table.source_table);
      onClearedTable();
      queryClient.invalidateQueries({ queryKey: ['ingest-history'] });
      queryClient.invalidateQueries({ queryKey: ['records'] });
      queryClient.invalidateQueries({ queryKey: ['stats'] });
      toast.success('Table removed.');
    } catch (err) {
      toast.error(`Failed to clear table: ${(err as Error).message}`);
    } finally {
      setClearing(false);
    }
  }

  // Inline re-ingest progress bar width.
  const progressPct = reProg
    ? reProg.table_total > 0
      ? Math.min(100, Math.round((reProg.table_rows / reProg.table_total) * 100))
      : 30
    : 0;

  return (
    <Modal open={open} onClose={onClose} title="Table inspector" size="lg">
      {isLoading ? (
        <div className="flex items-center gap-2 py-8 text-sm text-fg-muted">
          <Loader2 size={16} className="animate-spin" /> Loading inspector…
        </div>
      ) : isError || !info ? (
        <p className="py-8 text-sm text-danger">Failed to load table info.</p>
      ) : (
        <div className="flex flex-col gap-5">
          {/* Counts + status */}
          <div className="flex flex-col gap-3">
            <div className="grid grid-cols-3 gap-2">
              <Stat label="Rows" value={fmtNum(info.table.row_count)} />
              <Stat label="Vectors" value={fmtNum(info.table.vector_count)} />
              <Stat label="Columns" value={String(info.profile?.columns.length ?? 0)} />
            </div>
            <div>
              <Badge variant={statusTone(info.table.status)}>{info.table.status}</Badge>
            </div>
          </div>

          {/* Re-ingest inline progress */}
          {reProg && (
            <div className="flex flex-col gap-1.5 rounded-md border border-border bg-elevated p-3">
              <span className="flex items-center gap-2 text-xs text-fg-muted">
                <Loader2 size={13} className="animate-spin" />
                Re-ingesting {fmtNum(reProg.table_rows)}
                {reProg.table_total ? ` / ${fmtNum(reProg.table_total)}` : ''}
              </span>
              <div className="h-1.5 rounded-full bg-border overflow-hidden">
                <div
                  className="h-full bg-accent transition-[width] duration-300"
                  style={{ width: `${progressPct}%` }}
                />
              </div>
            </div>
          )}

          {/* Admin actions */}
          {isAdmin && (
            <div className="flex flex-col gap-2 sm:flex-row">
              <Button
                variant="secondary"
                size="sm"
                full
                loading={reingesting}
                disabled={reingesting}
                leftIcon={<Play size={14} />}
                onClick={handleReingest}
                className="min-h-[44px]"
              >
                Re-ingest
              </Button>
              <Button
                variant="danger"
                size="sm"
                full
                loading={clearing}
                disabled={clearing}
                leftIcon={<Trash2 size={14} />}
                onClick={handleClearTable}
                className="min-h-[44px]"
              >
                Clear table
              </Button>
            </div>
          )}

          {/* Source connection */}
          <Section title="Source database">
            {info.connection ? (
              <dl className="grid grid-cols-[auto_1fr] gap-x-4 gap-y-1 text-sm">
                <Field label="Name" value={info.connection.name} />
                <Field label="Type" value={info.connection.kind} />
                <Field
                  label="Host"
                  value={`${info.connection.host}:${info.connection.port}`}
                />
                <Field label="Database" value={info.connection.database} />
                <Field label="User" value={info.connection.username} />
              </dl>
            ) : (
              <p className="text-sm text-fg-subtle">Source connection deleted.</p>
            )}
          </Section>

          {/* Lineage */}
          <Section title="Lineage">
            <dl className="grid grid-cols-[auto_1fr] gap-x-4 gap-y-1 text-sm">
              <Field label="Last ingested" value={fmtDate(info.table.last_ingested)} />
              <Field label="Last embedded" value={fmtDate(info.table.last_embedded_at)} />
            </dl>
          </Section>

          {/* Schema profile */}
          <Section title={`Schema${info.profile ? ` (${info.profile.columns.length})` : ''}`}>
            {info.profile && info.profile.columns.length > 0 ? (
              <div className="flex flex-col divide-y divide-border rounded-md border border-border">
                {info.profile.columns.map((col) => (
                  <div key={col.name} className="flex items-center gap-2 px-3 py-2 text-sm">
                    <span className="text-fg truncate flex-1" title={col.name}>
                      {col.name}
                    </span>
                    <span className="text-xs text-fg-muted">{col.type}</span>
                    {col.pii && (
                      <Badge variant="warning">
                        <KeyRound size={10} /> PII
                      </Badge>
                    )}
                    {!col.selected && <Badge variant="neutral">excluded</Badge>}
                  </div>
                ))}
              </div>
            ) : (
              <p className="text-sm text-fg-subtle">No schema profile for this table.</p>
            )}
          </Section>

          {/* Recent runs (connection-scoped) */}
          <Section title="Recent ingestion runs">
            {info.recent_runs.length === 0 ? (
              <p className="text-sm text-fg-subtle">No ingestion runs recorded yet.</p>
            ) : (
              <div className="flex flex-col gap-2">
                {info.recent_runs.map((run) => (
                  <div
                    key={run.id}
                    className="flex items-center justify-between gap-2 rounded-md border border-border px-3 py-2 text-sm"
                  >
                    <span className="flex items-center gap-2 min-w-0">
                      <Badge variant={statusTone(run.status)}>{run.status}</Badge>
                      <span className="text-fg-muted truncate">
                        {fmtNum(run.rows_processed)} rows · {fmtNum(run.chunks_created)} chunks
                      </span>
                    </span>
                    <span className="text-xs text-fg-subtle whitespace-nowrap">
                      {fmtDate(run.completed_at ?? run.started_at)}
                    </span>
                  </div>
                ))}
              </div>
            )}
          </Section>
        </div>
      )}
    </Modal>
  );
}

function Stat({ label, value }: { label: string; value: string }) {
  return (
    <div className="flex flex-col items-center gap-0.5 rounded-md border border-border bg-elevated py-3">
      <span className="text-lg font-semibold text-fg leading-none">{value}</span>
      <span className="text-xs text-fg-muted">{label}</span>
    </div>
  );
}

function Section({ title, children }: { title: string; children: ReactNode }) {
  return (
    <div className="flex flex-col gap-2">
      <h4 className="text-xs font-semibold uppercase tracking-wide text-fg-muted">{title}</h4>
      {children}
    </div>
  );
}

function Field({ label, value }: { label: string; value: string }) {
  return (
    <>
      <dt className="text-fg-muted">{label}</dt>
      <dd className="text-fg break-words min-w-0">{value}</dd>
    </>
  );
}
