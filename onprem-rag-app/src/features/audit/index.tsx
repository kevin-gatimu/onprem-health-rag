// Audit Log screen — admin-only, filterable, paginated view of the `audit_log`
// collection (Stage 10, Layer 3a).
//
// Mobile-first: entries render as stacked cards below `md`; a full <table> at
// `md+`. The table sits inside its own overflow-x-auto box so there is zero
// horizontal scroll of the page body at 360 px. Tap targets ≥ 44 px throughout.
//
// Data: TanStack useQuery with keepPreviousData so the prior page stays visible
// while a new page or filter result loads. staleTime 30 s matches the rest of the
// app's read-only admin queries.
//
// Admin gate: the route is already guarded by permissions.ts, but we add a
// defensive fallback using the real `role` from useSession (never previewRole).
import { useEffect, useMemo, useState } from 'react';
import { keepPreviousData, useQuery } from '@tanstack/react-query';
import {
  ChevronLeft,
  ChevronRight,
  ChevronsLeft,
  ChevronsRight,
  ClipboardList,
  Loader2,
} from 'lucide-react';
import { getAudit } from '../../lib/bridge';
import { Button, Input, Select, Badge, EmptyState, cn } from '../../components/ui';
import type { BadgeVariant } from '../../components/ui';
import { PageContainer } from '../../components/layout/PageContainer';
import { useSession } from '../../stores/session';

// ── Known-action constants ───────────────────────────────────────────────────────
// Hard-coded so we never need a /audit/actions endpoint. These are every action
// string that write_audit is called with across the current build.
const KNOWN_ACTIONS = [
  'login',
  'login_failed',
  'user_created',
  'user_updated',
  'user_deleted',
  'password_set',
  'password_changed',
  'profile_updated',
  'source_created',
  'source_updated',
  'source_deleted',
  'ingest_started',
] as const;

const ACTION_LABEL: Record<string, string> = {
  login:            'Login',
  login_failed:     'Login failed',
  user_created:     'User created',
  user_updated:     'User updated',
  user_deleted:     'User deleted',
  password_set:     'Password set',
  password_changed: 'Password changed',
  profile_updated:  'Profile updated',
  source_created:   'Source created',
  source_updated:   'Source updated',
  source_deleted:   'Source deleted',
  ingest_started:   'Ingest started',
};

// Maps each action to the Badge variant that best communicates its severity.
// Failures → error; destructive deletes → error; auth events → info;
// creates → success; updates / password ops → neutral.
const ACTION_BADGE: Record<string, BadgeVariant> = {
  login:            'info',
  login_failed:     'error',
  user_created:     'success',
  user_updated:     'neutral',
  user_deleted:     'error',
  password_set:     'neutral',
  password_changed: 'neutral',
  profile_updated:  'neutral',
  source_created:   'success',
  source_updated:   'neutral',
  source_deleted:   'error',
  ingest_started:   'info',
};

const PAGE_SIZE_OPTIONS = [25, 50, 100] as const;
const DEFAULT_PAGE_SIZE = 50;

// ── Detail formatter ─────────────────────────────────────────────────────────────
// Renders the optional `details` field compactly. JSON-stringifies objects and
// truncates long blobs so a single entry never dominates the view.
function fmtDetails(val: unknown, maxLen = 120): string {
  if (val === null || val === undefined) return '';
  const s = typeof val === 'string' ? val : JSON.stringify(val);
  return s.length > maxLen ? `${s.slice(0, maxLen)}…` : s;
}

// ── AuditLog ─────────────────────────────────────────────────────────────────────
export default function AuditLog() {
  // Real role — never previewRole. Defensive fallback in case a navigation error
  // ever lets a non-admin reach this route.
  const role = useSession((s) => s.user?.role);

  // ── Filter state ──────────────────────────────────────────────────────────────
  const [userFilter, setUserFilter]     = useState('');
  const [debouncedUser, setDebouncedUser] = useState('');
  const [actionFilter, setActionFilter] = useState('');
  const [fromFilter, setFromFilter]     = useState('');
  const [toFilter, setToFilter]         = useState('');
  const [page, setPage]         = useState(1);
  const [pageSize, setPageSize] = useState(DEFAULT_PAGE_SIZE);

  // Debounce the username substring filter (~300 ms). Page resets when the
  // debounced value settles, not on every keystroke, so intermediate keystrokes
  // don't fire server requests.
  useEffect(() => {
    const tid = setTimeout(() => {
      setDebouncedUser(userFilter);
      setPage(1);
    }, 300);
    return () => clearTimeout(tid);
  }, [userFilter]);

  // Freeze the filter object so useQuery only re-runs on real value changes.
  const filters = useMemo(
    () => ({
      user:   debouncedUser || undefined,
      action: actionFilter  || undefined,
      from:   fromFilter    || undefined,
      to:     toFilter      || undefined,
    }),
    [debouncedUser, actionFilter, fromFilter, toFilter],
  );

  // "Clear filters" button visible as soon as any raw input has a value (before
  // the debounce fires for the user field).
  const hasFilters = !!(userFilter || actionFilter || fromFilter || toFilter);

  // ── Data ──────────────────────────────────────────────────────────────────────
  const { data, isLoading, isFetching, isError } = useQuery({
    queryKey: ['audit', filters, page, pageSize],
    queryFn:  () => getAudit(filters, page, pageSize),
    placeholderData: keepPreviousData,
    staleTime: 30_000,
  });

  const entries    = data?.entries ?? [];
  const total      = data?.total   ?? 0;
  const totalPages = Math.max(1, data?.page_count ?? 1);
  const visibleStart = total === 0 ? 0 : (page - 1) * pageSize + 1;
  const visibleEnd   = total === 0 ? 0 : Math.min(total, page * pageSize);

  // Action select options — computed once (KNOWN_ACTIONS is a module const).
  const actionOptions = useMemo(
    () => [
      { value: '', label: 'All actions' },
      ...KNOWN_ACTIONS.map((a) => ({ value: a, label: ACTION_LABEL[a] ?? a })),
    ],
    [],
  );

  // ── Helpers ───────────────────────────────────────────────────────────────────
  function clearFilters() {
    setUserFilter('');
    setDebouncedUser('');
    setActionFilter('');
    setFromFilter('');
    setToFilter('');
    setPage(1);
  }

  // ── Admin gate (defensive) ────────────────────────────────────────────────────
  if (role !== 'admin') {
    return (
      <div className="flex flex-col items-center justify-center py-20 text-fg-muted text-sm">
        Administrator access required.
      </div>
    );
  }

  // ── Render ────────────────────────────────────────────────────────────────────
  return (
    <PageContainer variant="board">
    <div className="flex flex-col gap-4">

      {/* ── Page header ───────────────────────────────────────────────────────── */}
      <div>
        <h1 className="text-xl font-bold text-fg">Audit Log</h1>
        <p className="text-sm text-fg-muted mt-0.5">
          Review all security-relevant actions on the system.
        </p>
      </div>

      {/* ── Filter bar ────────────────────────────────────────────────────────── */}
      {/* Stacks vertically on mobile; becomes a row of controls at md. */}
      <div className="flex flex-col gap-2 md:flex-row md:items-end md:flex-wrap">

        {/* Username substring filter — value is raw; debounce fires the query */}
        <div className="flex-1 min-w-0 md:min-w-[160px] md:max-w-xs">
          <Input
            placeholder="Filter by user…"
            value={userFilter}
            onChange={(e) => setUserFilter(e.target.value)}
            aria-label="Filter by username"
          />
        </div>

        {/* Action filter — hard-coded options, no server round-trip needed */}
        <div className="md:w-52">
          <Select
            value={actionFilter}
            onChange={(e) => { setActionFilter(e.target.value); setPage(1); }}
            options={actionOptions}
            aria-label="Filter by action"
            className="min-h-[44px]"
          />
        </div>

        {/* From date */}
        <div className="flex flex-col gap-1">
          <label className="text-xs font-medium text-fg-muted" htmlFor="audit-from">
            From
          </label>
          <input
            id="audit-from"
            type="date"
            value={fromFilter}
            onChange={(e) => { setFromFilter(e.target.value); setPage(1); }}
            className={cn(
              'py-2 pl-3 pr-3 bg-surface border border-border rounded-md',
              'text-sm text-fg min-h-[44px]',
              'focus:border-accent focus:ring-2 focus:ring-accent/20 focus:outline-none',
            )}
            aria-label="From date"
          />
        </div>

        {/* To date */}
        <div className="flex flex-col gap-1">
          <label className="text-xs font-medium text-fg-muted" htmlFor="audit-to">
            To
          </label>
          <input
            id="audit-to"
            type="date"
            value={toFilter}
            onChange={(e) => { setToFilter(e.target.value); setPage(1); }}
            className={cn(
              'py-2 pl-3 pr-3 bg-surface border border-border rounded-md',
              'text-sm text-fg min-h-[44px]',
              'focus:border-accent focus:ring-2 focus:ring-accent/20 focus:outline-none',
            )}
            aria-label="To date"
          />
        </div>

        {/* Clear filters — only rendered when at least one filter is active */}
        {hasFilters && (
          <Button
            variant="ghost"
            size="sm"
            onClick={clearFilters}
            className="min-h-[44px] self-end"
          >
            Clear filters
          </Button>
        )}
      </div>

      {/* ── Summary + pagination controls ─────────────────────────────────────── */}
      {/* Mirrors RowsGrid: summary left, Prev/Next right, page-size select md+. */}
      <div className="flex items-center justify-between gap-2 flex-wrap">
        <span className="text-xs text-fg-muted whitespace-nowrap">
          {total === 0
            ? 'No entries'
            : `${visibleStart}–${visibleEnd} of ${total}`}
        </span>

        <div className="flex items-center gap-1">
          {/* First page: md+ only */}
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
            onClick={() => setPage((v) => Math.max(1, v - 1))}
            aria-label="Previous page"
          >
            <ChevronLeft size={14} />
          </Button>

          {/* Mobile: compact "Page X of Y" */}
          <span className="md:hidden text-xs text-fg-muted whitespace-nowrap px-1">
            Page {page} of {totalPages}
          </span>

          {/* Desktop: plain page indicator */}
          <span className="hidden md:inline text-xs text-fg-muted whitespace-nowrap px-1">
            Page {page} / {totalPages}
          </span>

          <Button
            variant="ghost"
            size="sm"
            className="min-h-[40px] min-w-[40px] px-2"
            disabled={page >= totalPages}
            onClick={() => setPage((v) => Math.min(totalPages, v + 1))}
            aria-label="Next page"
          >
            <ChevronRight size={14} />
          </Button>

          {/* Last page: md+ only */}
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

          {/* Page-size selector: md+ only (mobile stays compact) */}
          <select
            value={pageSize}
            onChange={(e) => { setPageSize(Number(e.target.value)); setPage(1); }}
            aria-label="Entries per page"
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

      {/* ── Results ───────────────────────────────────────────────────────────── */}
      <div className="relative">
        {/* Subtle spinner while keepPreviousData is shown during a background refetch */}
        {isFetching && !isLoading && (
          <div className="absolute right-2 top-2 z-10 text-fg-muted">
            <Loader2 size={16} className="animate-spin" />
          </div>
        )}

        {isLoading ? (
          <div className="flex items-center justify-center gap-2 py-12 text-sm text-fg-muted">
            <Loader2 size={18} className="animate-spin" /> Loading audit log…
          </div>
        ) : isError ? (
          <p className="py-8 text-center text-sm text-danger">
            Failed to load audit entries. Check your connection and try again.
          </p>
        ) : entries.length === 0 ? (
          <EmptyState
            icon={<ClipboardList size={32} />}
            title={hasFilters ? 'No matching entries' : 'No audit entries yet'}
            description={
              hasFilters
                ? 'No audit entries match these filters.'
                : 'Audit entries will appear here once users take actions on the system.'
            }
          />
        ) : (
          <>
            {/* ── Mobile cards (hidden at md+) ──────────────────────────────── */}
            <div className="flex flex-col gap-2 md:hidden">
              {entries.map((row) => (
                <div
                  key={row.id}
                  className="flex flex-col gap-1.5 rounded-lg border border-border bg-surface p-3 min-h-[44px]"
                >
                  {/* Top row: action badge + timestamp */}
                  <div className="flex items-center justify-between gap-2 flex-wrap">
                    <Badge variant={ACTION_BADGE[row.action] ?? 'neutral'}>
                      {ACTION_LABEL[row.action] ?? row.action}
                    </Badge>
                    <span className="text-xs text-fg-muted whitespace-nowrap">
                      {new Date(row.timestamp).toLocaleString()}
                    </span>
                  </div>

                  {/* User + resource */}
                  <div className="flex flex-col gap-0.5 text-sm">
                    <span className="text-fg font-medium truncate">
                      {row.username || row.user_id}
                    </span>
                    <span className="text-fg-muted truncate">{row.resource}</span>
                  </div>

                  {/* Details — compact key/value rendering, truncated */}
                  {row.details !== undefined && row.details !== null && (
                    <span className="text-xs text-fg-subtle break-all line-clamp-2">
                      {fmtDetails(row.details)}
                    </span>
                  )}
                </div>
              ))}
            </div>

            {/* ── Desktop table (hidden below md) ──────────────────────────── */}
            {/* In its own overflow-x-auto box — the page body never scrolls
                horizontally regardless of content width. */}
            <div className="hidden md:block overflow-x-auto rounded-lg border border-border">
              <table className="min-w-full text-sm border-collapse">
                <thead>
                  <tr className="bg-elevated">
                    <th className="px-3 py-2 text-left text-xs font-semibold text-fg-muted whitespace-nowrap border-b border-border">
                      Time
                    </th>
                    <th className="px-3 py-2 text-left text-xs font-semibold text-fg-muted whitespace-nowrap border-b border-border">
                      Action
                    </th>
                    <th className="px-3 py-2 text-left text-xs font-semibold text-fg-muted whitespace-nowrap border-b border-border">
                      User
                    </th>
                    <th className="px-3 py-2 text-left text-xs font-semibold text-fg-muted whitespace-nowrap border-b border-border">
                      Resource
                    </th>
                    <th className="px-3 py-2 text-left text-xs font-semibold text-fg-muted whitespace-nowrap border-b border-border">
                      Details
                    </th>
                  </tr>
                </thead>
                <tbody>
                  {entries.map((row) => {
                    // Pre-compute the full details string for the title tooltip.
                    const detailsFull =
                      row.details !== undefined && row.details !== null
                        ? JSON.stringify(row.details)
                        : '';
                    return (
                      <tr
                        key={row.id}
                        className="border-b border-border last:border-0 hover:bg-elevated/60"
                      >
                        <td className="px-3 py-2 text-xs text-fg-muted whitespace-nowrap">
                          {new Date(row.timestamp).toLocaleString()}
                        </td>
                        <td className="px-3 py-2">
                          <Badge variant={ACTION_BADGE[row.action] ?? 'neutral'}>
                            {ACTION_LABEL[row.action] ?? row.action}
                          </Badge>
                        </td>
                        <td className="px-3 py-2 text-fg whitespace-nowrap">
                          {row.username || row.user_id}
                        </td>
                        <td
                          className="px-3 py-2 text-fg-muted whitespace-nowrap max-w-[200px] truncate"
                          title={row.resource}
                        >
                          {row.resource}
                        </td>
                        <td
                          className="px-3 py-2 text-fg-muted max-w-[280px] truncate"
                          title={detailsFull}
                        >
                          {fmtDetails(row.details)}
                        </td>
                      </tr>
                    );
                  })}
                </tbody>
              </table>
            </div>
          </>
        )}
      </div>
    </div>
    </PageContainer>
  );
}
