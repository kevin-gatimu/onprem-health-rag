// PickConnection — Step 1: choose a source, load its schema, then analyse.
// Also renders the ingestion-history card with per-table and per-connection deletes.
// Mobile-first: all interactive targets are min 44px, nothing overflows at 360 px.
import { useState } from 'react';
import { Database, History, Layers, RefreshCw, Trash2, Download } from 'lucide-react';
import { useQuery, useQueryClient } from '@tanstack/react-query';
import {
  listSources,
  getSchema,
  analyzeSchema,
  getIngestHistory,
  deleteIngestTable,
} from '../../lib/bridge';
import type { IngestionHistoryConnection } from '../../lib/bridge';
import { useIngestion } from '../../stores/ingestion';
import { toast, useUi } from '../../stores/ui';
import { Badge, Button, EmptyState } from '../../components/ui';
import { fmtWhen } from './utils';

export default function PickConnection() {
  const queryClient = useQueryClient();
  const navigate = useUi((s) => s.navigate);

  const { data: sources, isLoading: sourcesLoading } = useQuery({
    queryKey: ['sources'],
    queryFn: listSources,
    staleTime: 30_000,
  });

  const { data: history, isLoading: historyLoading, refetch: refetchHistory } = useQuery({
    queryKey: ['ingest-history'],
    queryFn: getIngestHistory,
    staleTime: 30_000,
  });

  // Track which item is mid-delete so its button can show a disabled state.
  const [deletingTableId, setDeletingTableId] = useState<string | null>(null);
  const [deletingConnId, setDeletingConnId]   = useState<string | null>(null);

  // ── Schema load + analyse flow ──────────────────────────────────────────────

  async function handlePickSource(id: string) {
    const store = useIngestion.getState();
    store.pickSource(id);
    store.setStep('loading-schema');
    store.setAnalysisError(null);
    try {
      const tables = await getSchema(id);
      store.setSchema(tables);
      store.setStep('analyzing');
      try {
        const analysis = await analyzeSchema(tables);
        store.setAnalysis(analysis);
      } catch (err) {
        store.setAnalysisError((err as Error).message ?? 'AI analysis failed.');
        toast.warning('AI analysis skipped — all tables pre-selected; review manually.');
      }
      store.setStep('select-tables');
    } catch (err) {
      toast.error(`Failed to load schema: ${(err as Error).message}`);
      store.setStep('pick-connection');
    }
  }

  // ── History deletion ─────────────────────────────────────────────────────────

  async function handleDeleteHistoryTable(
    conn: IngestionHistoryConnection,
    sourceTable: string,
    tableId: string,
  ) {
    if (
      !window.confirm(
        `Delete all ingested data for "${sourceTable}"?\n\nThis removes its rows, vectors and lineage from the local store. The source database is not touched.`,
      )
    )
      return;
    setDeletingTableId(tableId);
    try {
      await deleteIngestTable(conn.source_id, sourceTable);
      toast.success(`Deleted "${sourceTable}" from the store.`);
      queryClient.invalidateQueries({ queryKey: ['ingest-history'] });
      queryClient.invalidateQueries({ queryKey: ['stats'] });
    } catch (err) {
      toast.error(`Failed to delete: ${(err as Error).message}`);
    } finally {
      setDeletingTableId(null);
    }
  }

  async function handleDeleteHistoryConnection(conn: IngestionHistoryConnection) {
    if (
      !window.confirm(
        `Delete ALL ${conn.tables.length} ingested table${conn.tables.length !== 1 ? 's' : ''} for "${conn.source_name}"?\n\nThis removes their rows, vectors and lineage from the local store. The source database is not touched.`,
      )
    )
      return;
    setDeletingConnId(conn.source_id);
    try {
      for (const t of conn.tables) {
        await deleteIngestTable(conn.source_id, t.source_table);
      }
      toast.success(`Cleared ingested data for "${conn.source_name}".`);
      queryClient.invalidateQueries({ queryKey: ['ingest-history'] });
      queryClient.invalidateQueries({ queryKey: ['stats'] });
    } catch (err) {
      toast.error(`Failed to delete: ${(err as Error).message}`);
    } finally {
      setDeletingConnId(null);
    }
  }

  // ── Render ───────────────────────────────────────────────────────────────────

  return (
    <div className="flex flex-col gap-4">

      {/* ── Step 1: Choose a source ── */}
      <div className="rounded-lg border border-border bg-surface p-4 flex flex-col gap-4">
        <div className="flex items-center gap-2">
          <span className="flex items-center justify-center w-6 h-6 rounded-full bg-accent text-accent-fg text-xs font-bold flex-shrink-0">
            1
          </span>
          <span className="font-semibold text-fg text-sm">Choose a source database</span>
        </div>

        {sourcesLoading ? (
          <p className="text-sm text-fg-muted">Loading connections…</p>
        ) : !sources || sources.length === 0 ? (
          <EmptyState
            icon={<Database size={28} />}
            title="No connections yet"
            description="Add a database connection first."
            action={
              <Button size="sm" variant="primary" onClick={() => navigate('/connections')}>
                Go to Connections
              </Button>
            }
          />
        ) : (
          <div className="flex flex-col gap-2">
            {sources.map((source) => (
              <button
                key={source.id}
                onClick={() => handlePickSource(source.id)}
                className={[
                  'flex items-center gap-3 p-3 rounded-md border text-left w-full',
                  'transition-colors duration-150 min-h-[52px]',
                  'border-border bg-elevated hover:border-accent/50 hover:bg-accent-subtle/20',
                ].join(' ')}
              >
                <Database size={16} className="text-accent flex-shrink-0" />
                <div className="flex-1 min-w-0">
                  <p className="text-sm font-medium text-fg truncate">{source.name}</p>
                  <p className="text-xs text-fg-muted truncate">
                    {source.kind} · {source.host}:{source.port}/{source.database}
                  </p>
                </div>
                <Badge
                  variant={
                    source.status === 'connected'
                      ? 'success'
                      : source.status === 'error'
                      ? 'error'
                      : 'neutral'
                  }
                  dot
                >
                  {source.status}
                </Badge>
                <Download size={14} className="text-fg-muted flex-shrink-0" />
              </button>
            ))}
          </div>
        )}
      </div>

      {/* ── Ingestion history ── */}
      <div className="rounded-lg border border-border bg-surface p-4 flex flex-col gap-4">
        <div className="flex items-center gap-2">
          <History size={16} className="text-fg-muted flex-shrink-0" />
          <span className="font-semibold text-fg text-sm flex-1">Ingestion history</span>
          <button
            onClick={() => refetchHistory()}
            title="Refresh history"
            className="flex items-center justify-center min-h-[44px] min-w-[44px] p-1.5 rounded hover:bg-elevated text-fg-muted hover:text-fg transition-colors"
          >
            <RefreshCw size={14} />
          </button>
        </div>

        {historyLoading ? (
          <p className="text-sm text-fg-muted">Loading history…</p>
        ) : !history || history.length === 0 ? (
          <EmptyState
            icon={<Layers size={24} />}
            title="Nothing ingested yet"
            description="Choose a database above and load its schema to begin."
          />
        ) : (
          <div className="flex flex-col gap-3">
            {history.map((conn) => (
              <div key={conn.source_id} className="rounded-md border border-border overflow-hidden">

                {/* Connection-level header */}
                <div className="flex items-start gap-3 p-3 bg-elevated">
                  <Database size={14} className="text-fg-muted flex-shrink-0 mt-0.5" />
                  <div className="flex-1 min-w-0">
                    <p className="text-sm font-semibold text-fg truncate">{conn.source_name}</p>
                    <p className="text-xs text-fg-muted truncate">
                      {conn.kind} · {conn.database}
                    </p>
                    <p className="text-xs text-fg-muted mt-0.5">
                      {conn.tables.length} table{conn.tables.length !== 1 ? 's' : ''} ·{' '}
                      {conn.total_rows.toLocaleString()} rows ·{' '}
                      {conn.total_vectors.toLocaleString()} vectors · Last:{' '}
                      {fmtWhen(conn.last_ingested)}
                    </p>
                  </div>
                  <button
                    onClick={() => handleDeleteHistoryConnection(conn)}
                    title={`Delete all ingested data for ${conn.source_name}`}
                    aria-label={`Delete all ingested data for ${conn.source_name}`}
                    disabled={deletingConnId === conn.source_id}
                    className="flex items-center justify-center min-h-[44px] min-w-[44px] p-2 rounded hover:bg-danger-subtle text-fg-muted hover:text-danger transition-colors flex-shrink-0 disabled:opacity-40 disabled:cursor-not-allowed"
                  >
                    <Trash2 size={14} />
                  </button>
                </div>

                {/* Per-table rows */}
                <div className="divide-y divide-border">
                  {conn.tables.map((t) => (
                    <div key={t.table_id} className="flex items-center gap-2 px-3 py-2 text-xs">
                      <span className="flex-1 text-fg truncate min-w-0">{t.source_table}</span>
                      <Badge
                        variant={
                          t.status === 'indexed' ? 'success'
                          : t.status === 'error' ? 'error'
                          : 'neutral'
                        }
                      >
                        {t.status}
                      </Badge>
                      <span className="text-fg-muted whitespace-nowrap">
                        {t.row_count.toLocaleString()} rows
                      </span>
                      <span className="text-fg-muted whitespace-nowrap">
                        {t.vector_count.toLocaleString()} vec
                      </span>
                      <span className="text-fg-muted whitespace-nowrap hidden sm:inline">
                        {fmtWhen(t.last_ingested)}
                      </span>
                      <button
                        onClick={() => handleDeleteHistoryTable(conn, t.source_table, t.table_id)}
                        title={`Delete "${t.source_table}" from the store`}
                        aria-label={`Delete ${t.source_table} from the store`}
                        disabled={deletingTableId === t.table_id}
                        className="flex items-center justify-center min-h-[44px] min-w-[44px] p-2 rounded hover:bg-danger-subtle text-fg-muted hover:text-danger transition-colors flex-shrink-0 disabled:opacity-40 disabled:cursor-not-allowed"
                      >
                        <Trash2 size={13} />
                      </button>
                    </div>
                  ))}
                </div>

              </div>
            ))}
          </div>
        )}
      </div>

    </div>
  );
}
