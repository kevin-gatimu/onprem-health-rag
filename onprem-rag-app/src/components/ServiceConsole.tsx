// ServiceConsole — shared compact activity panel for the live server log.
// Mobile-first: fixed max-height with internal scroll so it never grows the page
// or forces horizontal body scroll at 360 px. Reused by Settings and Models.
//
// It's a pure view over the stream store's `serverLog` ring (fed by the bridge's
// `logs://line` SSE relay). Newest lines sit at the bottom (terminal convention),
// and we auto-scroll to keep them in view — but only when the user is already
// parked near the bottom, so scrolling up to read history isn't yanked away.
import { useEffect, useRef } from 'react';
import { useStream } from '../stores/stream';
import { Button, cn } from './ui';

export interface ServiceConsoleProps {
  /** Header label; defaults to "Activity". */
  title?: string;
  /** Extra classes for the outer container. */
  className?: string;
}

// Map a log level to a foreground colour token. Errors/warnings stand out;
// everything else is muted so the panel reads as ambient background activity.
function levelClass(level: string): string {
  switch (level) {
    case 'ERROR': return 'text-danger';
    case 'WARN':  return 'text-warning';
    default:      return 'text-fg-muted'; // INFO | DEBUG | TRACE
  }
}

// HH:MM:SS in the viewer's local time — enough to place a line in time without
// the visual weight of a full date in a compact, monospace panel.
function formatTime(ts: string): string {
  const d = new Date(ts);
  return Number.isNaN(d.getTime()) ? '' : d.toLocaleTimeString([], { hour12: false });
}

// Our own module paths ("onprem_server::foundry::mod") are the overwhelming
// majority of lines here; the crate prefix is implied and just adds width.
// Third-party targets (rocket::…) are left as-is — they're already short and
// the prefix is the useful part (it's what tells them apart from our own).
function shortTarget(target: string): string {
  return target.replace(/^onprem_server::/, '');
}

export default function ServiceConsole({ title = 'Activity', className }: ServiceConsoleProps) {
  const serverLog   = useStream((s) => s.serverLog);
  const clearServerLog = useStream((s) => s.clearServerLog);

  // The scroll viewport; we read/adjust scrollTop directly rather than scrolling
  // a sentinel into view, so the "near bottom?" check and the scroll share one element.
  const viewportRef = useRef<HTMLDivElement>(null);

  useEffect(() => {
    const el = viewportRef.current;
    if (!el) return;
    // "Near bottom" = within ~40 px of the end. If so, follow new lines; otherwise
    // leave the user's scroll position alone (they're reading back through history).
    const nearBottom = el.scrollHeight - el.scrollTop - el.clientHeight < 40;
    if (nearBottom) el.scrollTop = el.scrollHeight;
  }, [serverLog]);

  return (
    <div className={cn('bg-surface border border-border rounded-lg', className)}>

      {/* ── Header: title + line count + Clear ─────────────────────────────── */}
      <div className="flex items-center justify-between px-4 py-3 border-b border-border">
        <h3 className="text-sm font-semibold text-fg">
          {title}
          {serverLog.length > 0 && (
            <span className="ml-2 text-xs font-normal text-fg-subtle">
              {serverLog.length} line{serverLog.length === 1 ? '' : 's'}
            </span>
          )}
        </h3>
        <Button
          size="sm"
          variant="ghost"
          onClick={clearServerLog}
          disabled={serverLog.length === 0}
        >
          Clear
        </Button>
      </div>

      {/* ── Log viewport: fixed height, internal vertical scroll only ──────── */}
      <div
        ref={viewportRef}
        className="overflow-y-auto max-h-56 sm:max-h-72 p-2 font-mono text-xs bg-elevated rounded-b-lg"
      >
        {serverLog.length === 0 ? (
          <p className="text-fg-subtle px-1 py-2">No activity yet.</p>
        ) : (
          serverLog.map((line) => (
            <div
              key={line.seq}
              className={cn('flex gap-1.5 leading-relaxed', levelClass(line.level))}
            >
              <span className="shrink-0 tabular-nums text-fg-subtle">{formatTime(line.ts)}</span>
              <span className="shrink-0 w-9 font-semibold">{line.level}</span>
              {/* target is the Rust module path; break-words keeps long ids inside the panel */}
              <span className="break-words min-w-0">
                <span className="text-fg-subtle">[{shortTarget(line.target)}]</span> {line.message}
              </span>
            </div>
          ))
        )}
      </div>
    </div>
  );
}
