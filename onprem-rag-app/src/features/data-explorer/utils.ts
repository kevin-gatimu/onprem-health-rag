// Formatting + presentation helpers for the Data Explorer.
// Ported from the Electron reference's local helpers, adapted to return
// design-system Badge tones instead of SCSS class names.
import type { BadgeVariant } from '../../components/ui';

/** Longest cell string rendered inline in the grid before it gets an ellipsis. */
const TRUNCATE_LEN = 80;

/** Stringify a cell value (objects → JSON) and clip to TRUNCATE_LEN with an ellipsis. */
export function truncate(value: unknown): string {
  const text =
    typeof value === 'object' && value !== null ? JSON.stringify(value) : String(value ?? '');
  return text.length > TRUNCATE_LEN ? `${text.slice(0, TRUNCATE_LEN)}…` : text;
}

/** Absolute local date-time, or "—" for null/invalid. */
export function fmtDate(iso: string | null | undefined): string {
  if (!iso) return '—';
  const d = new Date(iso);
  if (Number.isNaN(d.getTime())) return '—';
  return d.toLocaleString(undefined, {
    month: 'short',
    day: 'numeric',
    year: 'numeric',
    hour: '2-digit',
    minute: '2-digit',
  });
}

/** Coarse relative age ("3m ago", "2d ago"), or "never" for null/invalid. */
export function fmtRelative(iso: string | null | undefined): string {
  if (!iso) return 'never';
  const diff = Date.now() - new Date(iso).getTime();
  if (Number.isNaN(diff)) return 'never';
  if (diff < 0) return 'just now';
  const s = Math.floor(diff / 1000);
  if (s < 60) return `${s}s ago`;
  const m = Math.floor(s / 60);
  if (m < 60) return `${m}m ago`;
  const h = Math.floor(m / 60);
  if (h < 24) return `${h}h ago`;
  const d = Math.floor(h / 24);
  if (d < 30) return `${d}d ago`;
  const mo = Math.floor(d / 30);
  if (mo < 12) return `${mo}mo ago`;
  return `${Math.floor(mo / 12)}y ago`;
}

/** Full value for the row-detail drawer: objects pretty-printed, null → "—". */
export function formatValue(value: unknown): string {
  if (value === null || value === undefined) return '—';
  if (typeof value === 'object') return JSON.stringify(value, null, 2);
  return String(value);
}

/** Thousands-separated integer; treats null/undefined as 0. */
export function fmtNum(n: number | null | undefined): string {
  return (n ?? 0).toLocaleString();
}

/** Map a status string (table/run/connection) to a Badge tone. */
export function statusTone(status: string | undefined): BadgeVariant {
  if (status === 'completed' || status === 'indexed' || status === 'connected') return 'success';
  if (status === 'failed' || status === 'error' || status === 'disconnected') return 'error';
  return 'warning';
}
