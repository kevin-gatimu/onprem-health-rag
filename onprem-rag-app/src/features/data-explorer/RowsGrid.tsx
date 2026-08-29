// RowsGrid — the main pane: table header, debounced search, pagination, and the
// row grid. md+ renders a real <table> inside an `overflow-x-auto` box so wide
// grids scroll inside their own container (never the page body). Mobile renders
// each row as a card (first 3 columns) with compact Prev / page / Next controls.
import { useEffect, useMemo, useState } from 'react';
import {
  Search,
  X,
  ChevronLeft,
  ChevronRight,
  ChevronsLeft,
  ChevronsRight,
  PanelRightOpen,
  Loader2,
  Rows4,
} from 'lucide-react';
import { keepPreviousData, useQuery } from '@tanstack/react-query';
import { listRecords } from '../../lib/bridge';
import type { DataRow } from '../../lib/bridge';
import { Button, Input, Badge } from '../../components/ui';
import type { SelectedTable } from './index';
import { fmtNum, truncate } from './utils';

const PAGE_SIZE_OPTIONS = [10, 25, 50, 100] as const;

interface RowsGridProps {
  selected: SelectedTable;
  page: number;
  setPage: (updater: number | ((p: number) => number)) => void;
  pageSize: number;
  setPageSize: (size: number) => void;
  search: string;
  setSearch: (value: string) => void;
  debouncedSearch: string;
  onRowClick: (row: DataRow) => void;
  onInspect: () => void;
}

export default function RowsGrid({
  selected,
  page,
  setPage,
  pageSize,
  setPageSize,
  search,
  setSearch,
  debouncedSearch,
  onRowClick,
  onInspect,
}: RowsGridProps) {
  const { conn, table } = selected;
  const tableId = table.table_id;

  const { data, isLoading, isFetching, isError } = useQuery({
    queryKey: ['records', tableId, page, pageSize, debouncedSearch],
    queryFn: () =>
      listRecords(conn.source_id, table.source_table, page, pageSize, debouncedSearch || null),
    placeholderData: keepPreviousData,
    staleTime: 30_000,
  });

  const rows = data?.rows ?? [];
  const total = data?.total ?? 0;
  const totalPages = Math.max(1, data?.page_count ?? 1);
  const visibleStart = total === 0 ? 0 : (page - 1) * pageSize + 1;
  const visibleEnd = total === 0 ? 0 : Math.min(total, page * pageSize);

  // Grid columns derived from the first row's data keys (headers stay aligned
  // with cells, since excluded columns never land in row.data).
  const columns = useMemo(() => (rows.length > 0 ? Object.keys(rows[0].data) : []), [rows]);

  // Page-jump input mirrors `page`; committing clamps to [1, totalPages].
  const [jumpPage, setJumpPage] = useState(String(page));
  useEffect(() => setJumpPage(String(page)), [page]);
  function submitJump() {
    const next = Number(jumpPage);
    if (!Number.isFinite(next)) return;
    setPage(Math.max(1, Math.min(totalPages, Math.floor(next))));
  }

  return (
    <div className="flex flex-col gap-3 min-w-0">
      {/* ── Table header ── */}
      <div className="flex flex-col gap-2 sm:flex-row sm:items-start sm:justify-between">
        <div className="min-w-0">
          <h2 className="text-base font-semibold text-fg truncate">{table.source_table}</h2>
          <p className="text-xs text-fg-muted truncate">
            {conn.source_name} · {conn.database || conn.kind} · {fmtNum(table.row_count)} rows ·{' '}
            {fmtNum(table.vector_count)} vectors
          </p>
        </div>
        <div className="sm:shrink-0">
          <Button
            variant="secondary"
            size="sm"
            leftIcon={<PanelRightOpen size={14} />}
            onClick={onInspect}
            className="min-h-[44px] w-full sm:w-auto"
          >
            Inspect
          </Button>
        </div>
      </div>

      {/* ── Control bar: search + pagination ── */}
      <div className="flex flex-col gap-2">
        <div className="relative">
          <Input
            icon={<Search size={16} />}
            placeholder="Search rows…"
            value={search}
            onChange={(e) => setSearch(e.target.value)}
            aria-label="Search rows"
          />
          {search && (
            <button
              onClick={() => setSearch('')}
              aria-label="Clear search"
              className="absolute right-3 top-1/2 -translate-y-1/2 flex items-center justify-center w-6 h-6 rounded text-fg-subtle hover:text-fg"
            >
              <X size={14} />
            </button>
          )}
        </div>

        <div className="flex items-center justify-between gap-2 flex-wrap">
          <span className="text-xs text-fg-muted whitespace-nowrap">
            {total === 0 ? 'No rows' : `${fmtNum(visibleStart)}–${fmtNum(visibleEnd)} of ${fmtNum(total)}`}
          </span>

          <div className="flex items-center gap-1">
            {/* First / last + jump: md+ only. */}
            <Button
              variant="ghost"
              size="sm"
              className="hidden md:inline-flex min-h-[40px] min-w-[40px] px-2"
              disabled={page <= 1}
              onClick={() => setPage(1)}
              aria-label="First page"
            >
              <ChevronsLeft size={14} />
            </Button>
            <Button
              variant="ghost"
              size="sm"
              className="min-h-[40px] min-w-[40px] px-2"
              disabled={page <= 1}
              onClick={() => setPage((v: number) => Math.max(1, v - 1))}
              aria-label="Previous page"
            >
              <ChevronLeft size={14} />
            </Button>

            {/* Mobile: compact "Page X of Y". */}
            <span className="md:hidden text-xs text-fg-muted whitespace-nowrap px-1">
              Page {page} of {totalPages}
            </span>

            {/* Desktop: editable jump box. */}
            <span className="hidden md:flex items-center gap-1">
              <input
                value={jumpPage}
                onChange={(e) => setJumpPage(e.target.value.replace(/[^0-9]/g, ''))}
                onBlur={submitJump}
                onKeyDown={(e) => e.key === 'Enter' && submitJump()}
                aria-label="Jump to page"
                className="w-12 text-center py-1 px-1 bg-surface border border-border rounded-md text-sm text-fg focus:border-accent focus:outline-none"
              />
              <span className="text-xs text-fg-muted whitespace-nowrap">/ {totalPages}</span>
            </span>

            <Button
              variant="ghost"
              size="sm"
              className="min-h-[40px] min-w-[40px] px-2"
              disabled={page >= totalPages}
              onClick={() => setPage((v: number) => Math.min(totalPages, v + 1))}
              aria-label="Next page"
            >
              <ChevronRight size={14} />
            </Button>
            <Button
              variant="ghost"
              size="sm"
              className="hidden md:inline-flex min-h-[40px] min-w-[40px] px-2"
              disabled={page >= totalPages}
              onClick={() => setPage(totalPages)}
              aria-label="Last page"
            >
              <ChevronsRight size={14} />
            </Button>

            {/* Page size: md+ only (mobile stays compact). */}
            <select
              value={pageSize}
              onChange={(e) => {
                setPageSize(Number(e.target.value));
                setPage(1);
              }}
              aria-label="Rows per page"
              className="hidden md:block ml-1 py-1 pl-2 pr-6 bg-surface border border-border rounded-md text-sm text-fg cursor-pointer focus:border-accent focus:outline-none"
            >
              {PAGE_SIZE_OPTIONS.map((size) => (
                <option key={size} value={size}>
                  {size} / page
                </option>
              ))}
            </select>
          </div>
        </div>
      </div>

      {/* ── Grid ── */}
      <div className="relative">
        {isFetching && !isLoading && (
          <div className="absolute right-2 top-2 z-10 text-fg-muted">
            <Loader2 size={16} className="animate-spin" />
          </div>
        )}

        {isLoading ? (
          <div className="flex items-center justify-center gap-2 py-12 text-sm text-fg-muted">
            <Loader2 size={18} className="animate-spin" /> Loading rows…
          </div>
        ) : isError ? (
          <p className="py-8 text-center text-sm text-danger">
            Failed to load rows. Try Refresh or reselect the table.
          </p>
        ) : rows.length === 0 ? (
          <div className="flex flex-col items-center gap-2 py-12 text-center text-fg-muted">
            <Rows4 size={28} />
            <span className="text-sm">
              {debouncedSearch ? 'No rows match your search.' : 'This table has no rows.'}
            </span>
          </div>
        ) : (
          <>
            {/* Mobile: row cards (first 3 columns). */}
            <div className="flex flex-col gap-2 md:hidden">
              {rows.map((row, i) => {
                const preview = Object.keys(row.data).slice(0, 3);
                return (
                  <button
                    key={row.id}
                    onClick={() => onRowClick(row)}
                    className="flex flex-col gap-1 rounded-lg border border-border bg-surface p-3 text-left min-h-[44px] active:bg-elevated"
                  >
                    <span className="flex items-center gap-2">
                      <Badge variant="neutral">#{visibleStart + i}</Badge>
                      <span className="text-xs text-fg-subtle truncate flex-1">{row.id}</span>
                    </span>
                    {preview.map((key) => (
                      <span key={key} className="flex gap-2 text-sm min-w-0">
                        <span className="text-fg-muted flex-shrink-0 max-w-[40%] truncate">{key}:</span>
                        <span className="text-fg truncate">{truncate(row.data[key])}</span>
                      </span>
                    ))}
                  </button>
                );
              })}
            </div>

            {/* md+: real table inside its own horizontal-scroll box. */}
            <div className="hidden md:block overflow-x-auto rounded-lg border border-border">
              <table className="min-w-full text-sm border-collapse">
                <thead>
                  <tr className="bg-elevated">
                    <th className="sticky left-0 z-10 bg-elevated px-3 py-2 text-left text-xs font-semibold text-fg-muted border-b border-border">
                      #
                    </th>
                    {columns.map((col) => (
                      <th
                        key={col}
                        className="px-3 py-2 text-left text-xs font-semibold text-fg-muted whitespace-nowrap border-b border-border"
                      >
                        {col}
                      </th>
                    ))}
                  </tr>
                </thead>
                <tbody>
                  {rows.map((row, i) => (
                    <tr
                      key={row.id}
                      onClick={() => onRowClick(row)}
                      className="cursor-pointer hover:bg-elevated/60 border-b border-border last:border-0"
                    >
                      <td className="sticky left-0 z-10 bg-surface px-3 py-2 text-xs text-fg-subtle">
                        {visibleStart + i}
                      </td>
                      {columns.map((col) => (
                        <td
                          key={col}
                          className="px-3 py-2 text-fg whitespace-nowrap max-w-[320px] truncate"
                          title={String(row.data[col] ?? '')}
                        >
                          {truncate(row.data[col])}
                        </td>
                      ))}
                    </tr>
                  ))}
                </tbody>
              </table>
            </div>
          </>
        )}
      </div>
    </div>
  );
}
