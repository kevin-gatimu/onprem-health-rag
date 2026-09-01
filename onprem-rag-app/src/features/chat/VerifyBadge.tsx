// Faithfulness verdict for a streamed answer (plan 25).
//
// Renders the post-stream `chat://verify` report: whether each clinical claim in
// the answer is backed by the retrieved passages. Collapsed to a one-line badge by
// default — the detail only matters when something is wrong, and a permanent wall
// of "supported" rows would train people to ignore it.
//
// Deliberate omission: a `skipped` verdict with nothing else to say renders
// nothing at all. The server already filters those out, and a badge reading "not
// checked" under every answer is worse than silence — it is the same signal as no
// badge, but it looks like a finding.
import { useState } from 'react';
import { ShieldCheck, ShieldAlert, ShieldX, ChevronDown, ChevronRight } from 'lucide-react';
import type { VerifyReport } from '../../lib/bridge';

/** Colour + icon + wording per verdict. Kept in one place so the badge, the border, and the detail rows can never disagree. */
function presentation(report: VerifyReport) {
  const n = report.unsupported;
  switch (report.status) {
    case 'supported':
      return {
        tone: 'text-success border-success/30 bg-success-subtle',
        icon: <ShieldCheck size={13} aria-hidden="true" />,
        label: `All ${report.claims.length} claim${report.claims.length === 1 ? '' : 's'} supported by sources`,
      };
    case 'partial':
      return {
        tone: 'text-warning border-warning/30 bg-warning-subtle',
        icon: <ShieldAlert size={13} aria-hidden="true" />,
        label: `${n} of ${report.claims.length} claims not supported by sources`,
      };
    case 'unsupported':
      return {
        tone: 'text-danger border-danger/30 bg-danger-subtle',
        icon: <ShieldX size={13} aria-hidden="true" />,
        label: `No claim in this answer is supported by the sources`,
      };
    default:
      return {
        tone: 'text-fg-muted border-border bg-elevated',
        icon: <ShieldAlert size={13} aria-hidden="true" />,
        label: report.reason ? `Not verified — ${report.reason}` : 'Not verified',
      };
  }
}

export default function VerifyBadge({ report }: { report: VerifyReport }) {
  const [open, setOpen] = useState(false);

  const overflow = report.citation_overflow ?? [];
  // Nothing to say: no verdict and no bad markers. Render nothing rather than a
  // badge that means "we did not look".
  if (report.status === 'skipped' && overflow.length === 0) return null;

  const { tone, icon, label } = presentation(report);
  const hasDetail = report.claims.length > 0;

  return (
    <div className="flex flex-col gap-1 self-start">
      <button
        type="button"
        onClick={() => hasDetail && setOpen((o) => !o)}
        aria-expanded={hasDetail ? open : undefined}
        className={`flex items-center gap-1.5 rounded-md border px-2 py-1 text-xs ${tone} ${
          hasDetail ? 'cursor-pointer' : 'cursor-default'
        }`}
      >
        {icon}
        <span>{label}</span>
        {hasDetail &&
          (open ? (
            <ChevronDown size={12} aria-hidden="true" />
          ) : (
            <ChevronRight size={12} aria-hidden="true" />
          ))}
      </button>

      {/* Dangling [N] markers are a separate, deterministic finding: the answer
          pointed at a source that was never returned. Shown regardless of verdict. */}
      {overflow.length > 0 && (
        <p className="text-xs text-warning px-2">
          Cites {overflow.length === 1 ? 'a source' : 'sources'}{' '}
          {overflow.map((n) => `[${n}]`).join(', ')} that {overflow.length === 1 ? 'was' : 'were'} not
          returned.
        </p>
      )}

      {open && hasDetail && (
        <ul className="flex flex-col gap-1 rounded-md border border-border bg-surface px-3 py-2">
          {report.claims.map((c, i) => (
            <li key={i} className="flex items-start gap-2 text-xs">
              <span
                className={`mt-0.5 shrink-0 ${c.supported ? 'text-success' : 'text-danger'}`}
                aria-hidden="true"
              >
                {c.supported ? '✓' : '✕'}
              </span>
              <span className="min-w-0 flex-1 text-fg-muted">
                {c.claim}
                {c.passages.length > 0 && (
                  <span className="text-fg-subtle"> {c.passages.map((n) => `[${n}]`).join('')}</span>
                )}
              </span>
            </li>
          ))}
        </ul>
      )}
    </div>
  );
}
