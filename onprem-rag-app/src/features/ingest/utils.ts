/** Format a byte count to a human-readable string (B / KB / MB). */
export function fmtBytes(b: number): string {
  if (b < 1024) return `${b} B`;
  if (b < 1024 * 1024) return `${(b / 1024).toFixed(1)} KB`;
  return `${(b / 1024 / 1024).toFixed(1)} MB`;
}

/**
 * Format an ISO timestamp as "date time · relative".
 * Returns "—" for null / invalid dates.
 */
export function fmtWhen(iso: string | null): string {
  if (!iso) return '—';
  const d = new Date(iso);
  if (isNaN(d.getTime())) return '—';
  const diffMs = Date.now() - d.getTime();
  const mins = Math.floor(diffMs / 60000);
  let rel: string;
  if (mins < 1) rel = 'just now';
  else if (mins < 60) rel = `${mins}m ago`;
  else if (mins < 1440) rel = `${Math.floor(mins / 60)}h ago`;
  else rel = `${Math.floor(mins / 1440)}d ago`;
  return `${d.toLocaleDateString()} ${d.toLocaleTimeString([], { hour: '2-digit', minute: '2-digit' })} · ${rel}`;
}

/** Log entry level → display icon character. */
export const LOG_ICONS: Record<string, string> = {
  info: '·',
  success: '✓',
  warn: '⚠',
  error: '✕',
};
