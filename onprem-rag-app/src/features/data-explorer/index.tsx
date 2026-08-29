// Data Explorer — Stage 5 of the mobile-first UI rebuild.
// Mobile-first: designed at 360px (stat strip 2×2, tree in a drawer, row cards),
// then md:/xl: layer up to a stat row + left tree rail + real data-grid table.
//
// This shell owns component-local view state (selection, paging, search, drawer
// flags) per the plan — none of it lives in a store, since the explorer needs no
// cross-navigation persistence (unlike the ingest wizard). The connection tree +
// stat totals come from the SHARED ['ingest-history'] query cache, so deletes run
// from the ingest wizard or here keep both screens in sync.
import { useEffect, useMemo, useState } from 'react';
import { Table2, Layers, Inbox, PanelLeftOpen } from 'lucide-react';
import { useQuery } from '@tanstack/react-query';
import { getIngestHistory } from '../../lib/bridge';
import type { IngestionHistoryConnection, IngestionHistoryTable, DataRow } from '../../lib/bridge';
import { useUi } from '../../stores/ui';
import { useEffectiveRole } from '../../hooks/useEffectiveRole';
import { Button, EmptyState, Modal } from '../../components/ui';
import { PageContainer } from '../../components/layout/PageContainer';
import OverviewBar from './OverviewBar';
import ConnectionTree from './ConnectionTree';
import RowsGrid from './RowsGrid';
import TableInspector from './TableInspector';
import RowDetail from './RowDetail';

/** The selected table plus its owning connection, resolved from the history cache. */
export interface SelectedTable {
  conn: IngestionHistoryConnection;
  table: IngestionHistoryTable;
}

export default function DataExplorer() {
  const navigate = useUi((s) => s.navigate);
  const isAdmin = useEffectiveRole() === 'admin';

  // ── Shared tree/totals source ────────────────────────────────────────────────
  const {
    data: history,
    isLoading: historyLoading,
    isError: historyError,
  } = useQuery({
    queryKey: ['ingest-history'],
    queryFn: getIngestHistory,
    staleTime: 30_000,
  });

  const connections = history ?? [];

  // Stat-strip totals, summed client-side from the history cache.
  const totals = useMemo(() => {
    let tables = 0;
    let rows = 0;
    let vectors = 0;
    for (const c of connections) {
      tables += c.tables.length;
      rows += c.total_rows;
      vectors += c.total_vectors;
    }
    return { connections: connections.length, tables, rows, vectors };
  }, [connections]);

  // ── Component-local view state ─────────────────────────────────────────────────
  const [selectedTableId, setSelectedTableId] = useState<string | null>(null);
  const [page, setPage] = useState(1);
  const [pageSize, setPageSize] = useState(25);
  const [search, setSearch] = useState('');
  const [debouncedSearch, setDebouncedSearch] = useState('');

  // Drawer / overlay flags
  const [tablePickerOpen, setTablePickerOpen] = useState(false); // mobile tree drawer
  const [inspectorOpen, setInspectorOpen] = useState(false);
  const [selectedRow, setSelectedRow] = useState<DataRow | null>(null);

  // Resolve the selected table (+ its connection) from the shared cache. Falls back
  // to null if the table was deleted underneath us (e.g. a clear elsewhere).
  const selected = useMemo<SelectedTable | null>(() => {
    if (!selectedTableId) return null;
    for (const conn of connections) {
      const table = conn.tables.find((t) => t.table_id === selectedTableId);
      if (table) return { conn, table };
    }
    return null;
  }, [selectedTableId, connections]);

  // Debounce the search box (300ms) before it enters the ['records', …] query key,
  // and reset to page 1 whenever the effective query changes.
  useEffect(() => {
    const id = setTimeout(() => {
      setDebouncedSearch(search);
      setPage(1);
    }, 300);
    return () => clearTimeout(id);
  }, [search]);

  // ── Selection helpers ──────────────────────────────────────────────────────────
  function selectTable(tableId: string) {
    if (tableId !== selectedTableId) {
      setSelectedTableId(tableId);
      setSelectedRow(null);
      setInspectorOpen(false);
      setPage(1);
      setSearch('');
      setDebouncedSearch('');
    }
    setTablePickerOpen(false); // always close the mobile drawer after a pick
  }

  /** After a destructive clear that may have removed the current selection. */
  function clearSelection() {
    setSelectedTableId(null);
    setSelectedRow(null);
    setInspectorOpen(false);
    setPage(1);
    setSearch('');
    setDebouncedSearch('');
  }

  const hasAnything = totals.tables > 0;

  // ── Whole-page empty state: nothing ingested yet ───────────────────────────────
  if (!historyLoading && !historyError && !hasAnything) {
    return (
      <PageContainer variant="board">
      <div className="flex flex-col gap-5">
        <Header />
        <OverviewBar totals={totals} isAdmin={isAdmin} onAfterClearAll={clearSelection} />
        <EmptyState
          icon={<Inbox size={40} />}
          title="No ingested data yet"
          description="Connect a source database and run an ingestion to browse tables, columns, and embeddings here."
          action={
            <Button variant="primary" onClick={() => navigate('/ingest')}>
              Go to Ingest
            </Button>
          }
        />
      </div>
      </PageContainer>
    );
  }

  return (
    <PageContainer variant="board">
    <div className="flex flex-col gap-5">
      <Header />

      <OverviewBar totals={totals} isAdmin={isAdmin} onAfterClearAll={clearSelection} />

      {historyError ? (
        <p className="text-sm text-danger">Failed to load ingested tables. Try Refresh above.</p>
      ) : (
        // Mobile: single column. md+: fixed tree rail + fluid main.
        // `min-w-0` on main is what lets the wide data grid scroll inside its own
        // box instead of stretching the page body (no horizontal body scroll).
        <div className="flex flex-col gap-4 md:grid md:grid-cols-[260px_minmax(0,1fr)] md:gap-5 md:items-start">
          {/* Left rail — desktop only. The same ConnectionTree renders in the
              mobile drawer below. */}
          <aside className="hidden md:block rounded-lg border border-border bg-surface overflow-hidden md:sticky md:top-4">
            <div className="flex items-center gap-2 px-3 py-2.5 border-b border-border text-xs font-semibold text-fg-muted uppercase tracking-wide">
              <Layers size={13} />
              Databases
            </div>
            <div className="max-h-[70vh] overflow-y-auto p-2">
              <ConnectionTree
                connections={connections}
                loading={historyLoading}
                selectedTableId={selectedTableId}
                onSelectTable={selectTable}
                isAdmin={isAdmin}
                onConnectionCleared={clearSelection}
              />
            </div>
          </aside>

          <div className="min-w-0 flex flex-col gap-3">
            {/* Mobile: open the tree in a drawer. */}
            <Button
              variant="secondary"
              leftIcon={<PanelLeftOpen size={16} />}
              onClick={() => setTablePickerOpen(true)}
              className="md:hidden w-full min-h-[44px]"
            >
              Tables ({totals.tables})
            </Button>

            {selected ? (
              <RowsGrid
                selected={selected}
                page={page}
                setPage={setPage}
                pageSize={pageSize}
                setPageSize={setPageSize}
                search={search}
                setSearch={setSearch}
                debouncedSearch={debouncedSearch}
                onRowClick={setSelectedRow}
                onInspect={() => setInspectorOpen(true)}
              />
            ) : (
              <EmptyState
                icon={<Table2 size={36} />}
                title="Select a table"
                description="Pick an indexed table from a connection to browse its rows and schema."
              />
            )}
          </div>
        </div>
      )}

      {/* ── Mobile connection-tree drawer ───────────────────────────────────────── */}
      <Modal open={tablePickerOpen} onClose={() => setTablePickerOpen(false)} title="Databases" size="md">
        <ConnectionTree
          connections={connections}
          loading={historyLoading}
          selectedTableId={selectedTableId}
          onSelectTable={selectTable}
          isAdmin={isAdmin}
          onConnectionCleared={clearSelection}
        />
      </Modal>

      {/* ── Table inspector drawer ──────────────────────────────────────────────── */}
      <TableInspector
        open={inspectorOpen && !!selected}
        onClose={() => setInspectorOpen(false)}
        selected={selected}
        isAdmin={isAdmin}
        onClearedTable={clearSelection}
      />

      {/* ── Row detail drawer ───────────────────────────────────────────────────── */}
      <RowDetail open={!!selectedRow} onClose={() => setSelectedRow(null)} row={selectedRow} />
    </div>
    </PageContainer>
  );
}

/** Page header — matches the Connections/Ingest page heading pattern. */
function Header() {
  return (
    <div className="flex flex-col gap-1">
      <h1 className="text-xl font-bold text-fg">Data Explorer</h1>
      <p className="text-sm text-fg-muted">
        Browse ingested records, inspect table schemas, and manage indexed data.
      </p>
    </div>
  );
}
