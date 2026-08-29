// Formatting helpers for the Chat feature.

/** Coarse relative timestamp for conversation list items ("3m ago", "2d ago"). */
export function fmtWhen(iso: string): string {
  const diff = Date.now() - new Date(iso).getTime();
  if (Number.isNaN(diff)) return '';
  if (diff < 0) return 'just now';
  const s = Math.floor(diff / 1000);
  if (s < 60) return 'just now';
  const m = Math.floor(s / 60);
  if (m < 60) return `${m}m ago`;
  const h = Math.floor(m / 60);
  if (h < 24) return `${h}h ago`;
  const d = Math.floor(h / 24);
  if (d < 7) return `${d}d ago`;
  const date = new Date(iso);
  return date.toLocaleDateString(undefined, { month: 'short', day: 'numeric' });
}

/** Truncate at a character boundary, appending … if the string was cut. */
export function truncateTitle(s: string, maxLen = 40): string {
  if (s.length <= maxLen) return s;
  return s.slice(0, maxLen) + '…';
}
