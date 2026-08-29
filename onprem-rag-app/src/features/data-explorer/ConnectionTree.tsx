// ConnectionTree — collapsible connection groups over the shared ['ingest-history']
// cache. Each group lists its indexed tables (name + row/vector counts + status).
// Rendered both in the desktop left rail and inside the mobile drawer (same
// component, same props). Admins get a per-connection clear button in each header.
import { useState } from 'react';
import {
  ChevronRight,
  ChevronDown,
  Database,
  Rows4,
  Boxes,
  Trash2,
  Loader2,
} from 'lucide-react';
import { useQueryClient } from '@tanstack/react-query';
import { deleteIngestConnection } from '../../lib/bridge';
import type { IngestionHistoryConnection } from '../../lib/bridge';
import { toast } from '../../stores/ui';
import { Badge } from '../../components/ui';
import { fmtNum, fmtRelative, statusTone } from './utils';

interface ConnectionTreeProps {
  connections: IngestionHistoryConnection[];
  loading: boolean;
  selectedTableId: string | null;
  onSelectTable: (tableId: string) => void;
  isAdmin: boolean;
  /** Called after a per-connection clear that may have removed the selection. */
  onConnectionCleared: () => void;
}

export default function ConnectionTree({
  connections,
  loading,
  selectedTableId,
  onSelectTable,
  isAdmin,
  onConnectionCleared,
}: ConnectionTreeProps) {
  const queryClient = useQueryClient();
  // Track collapsed groups by source_id. Default expanded (empty set).
  const [collapsed, setCollapsed] = useState<Set<string>>(new Set());
  const [deletingConnId, setDeletingConnId] = useState<string | null>(null);

  function toggle(sourceId: string) {
    setCollapsed((prev) => {
      const next = new Set(prev);
      if (next.has(sourceId)) next.delete(sourceId);
      else next.add(sourceId);
      return next;
    });
  }

  async function handleClearConnection(conn: IngestionHistoryConnection) {
    if (
      !window.confirm(
        `Clear all ${conn.tables.length} ingested table${conn.tables.length !== 1 ? 's' : ''} for "${conn.source_name}"?\n\nThis removes their rows, vectors and lineage from the local store. The source database is not touched.`,
      )
    )
      return;
    setDeletingConnId(conn.source_id);
    try {
      await deleteIngestConnection(conn.source_id);
      onConnectionCleared();
      queryClient.invalidateQueries({ queryKey: ['ingest-history'] });
      queryClient.invalidateQueries({ queryKey: ['records'] });
      queryClient.invalidateQueries({ queryKey: ['table-info'] });
      queryClient.invalidateQueries({ queryKey: ['stats'] });
      toast.success(`Cleared ingested data for "${conn.source_name}".`);
    } catch (err) {
      toast.error(`Failed to clear: ${(err as Error).message}`);
    } finally {
      setDeletingConnId(null);
    }
  }

  if (loading) {
    return (
      <div className="flex items-center gap-2 px-2 py-6 text-sm text-fg-muted">
        <Loader2 size={16} className="animate-spin" />
        Loading…
      </div>
    );
  }

  if (connections.length === 0) {
    return (
      <div className="flex flex-col items-center gap-2 px-2 py-8 text-center text-fg-muted">
        <Database size={22} />
        <span className="text-sm">No connections</span>
      </div>
    );
  }

  return (
    <div className="flex flex-col gap-1">
      {connections.map((conn) => {
        const isCollapsed = collapsed.has(conn.source_id);
        return (
          <div key={conn.source_id} className="flex flex-col">
            {/* Connection header */}
            <div className="flex items-center gap-1.5 rounded-md hover:bg-elevated">
              <button
                onClick={() => toggle(conn.source_id)}
                className="flex flex-1 items-center gap-2 min-w-0 px-2 py-2 text-left min-h-[44px]"
                aria-expanded={!isCollapsed}
              >
                <span className="text-fg-muted flex-shrink-0">
                  {isCollapsed ? <ChevronRight size={14} /> : <ChevronDown size={14} />}
                </span>
                <Database size={14} className="text-fg-muted flex-shrink-0" />
                <span className="flex flex-col min-w-0">
                  <span className="text-sm font-medium text-fg truncate" title={conn.source_name}>
                    {conn.source_name}
                  </span>
                  <span className="text-xs text-fg-muted truncate">
                    {conn.kind}
                    {conn.database ? ` · ${conn.database}` : ''} · {conn.tables.length} table
                    {conn.tables.length !== 1 ? 's' : ''}
                  </span>
                </span>
              </button>
              {isAdmin && conn.tables.length > 0 && (
                <button
                  onClick={() => handleClearConnection(conn)}
                  disabled={deletingConnId === conn.source_id}
                  title={`Clear all ingested data for ${conn.source_name}`}
                  aria-label={`Clear all ingested data for ${conn.source_name}`}
                  className="flex items-center justify-center min-h-[44px] min-w-[44px] rounded text-fg-muted hover:bg-danger-subtle hover:text-danger transition-colors flex-shrink-0 disabled:opacity-40 disabled:cursor-not-allowed"
                >
                  {deletingConnId === conn.source_id ? (
                    <Loader2 size={14} className="animate-spin" />
                  ) : (
                    <Trash2 size={14} />
                  )}
                </button>
              )}
            </div>

            {/* Tables */}
            {!isCollapsed && (
              <div className="flex flex-col gap-0.5 pl-3 pb-1">
                {conn.tables.length === 0 ? (
                  <span className="px-2 py-2 text-xs text-fg-subtle">No indexed tables</span>
                ) : (
                  conn.tables.map((t) => {
                    const active = t.table_id === selectedTableId;
                    return (
                      <button
                        key={t.table_id}
                        onClick={() => onSelectTable(t.table_id)}
                        className={[
                          'flex flex-col gap-1 rounded-md px-2 py-2 text-left w-full min-h-[44px]',
                          'border transition-colors',
                          active
                            ? 'border-accent/50 bg-accent-subtle/25'
                            : 'border-transparent hover:bg-elevated',
                        ].join(' ')}
                      >
                        <span className="flex items-center gap-2 min-w-0">
                          <span
                            className="text-sm text-fg truncate flex-1"
                            title={t.source_table}
                          >
                            {t.source_table}
                          </span>
                          <Badge variant={statusTone(t.status)}>{t.status}</Badge>
                        </span>
                        <span className="flex items-center gap-3 text-xs text-fg-muted">
                          <span className="inline-flex items-center gap-1" title="Indexed rows">
                            <Rows4 size={11} /> {fmtNum(t.row_count)}
                          </span>
                          <span className="inline-flex items-center gap-1" title="Embedded vectors">
                            <Boxes size={11} /> {fmtNum(t.vector_count)}
                          </span>
                          <span className="ml-auto" title="Last ingested">
                            {fmtRelative(t.last_ingested)}
                          </span>
                        </span>
                      </button>
                    );
                  })
                )}
              </div>
            )}
          </div>
        );
      })}
    </div>
  );
}
