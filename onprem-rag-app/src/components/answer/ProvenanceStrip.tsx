// ProvenanceStrip — compact horizontal rung list shown below each assistant answer.
// Shared between the agents and chat features.
//
// Compact form: "Link ✓ → Deterministic SQL ✓ → Validate ✓ → Execute ✓ · 212 ms"
// Misses show their PHI-free reason on hover. Expand to see backend-specific detail
// (sql + explanation for source_sql, spec + pipeline for document_db, etc.).
import { useState } from 'react';
import { ChevronDown, ChevronUp, CheckCircle, XCircle, MinusCircle, Zap } from 'lucide-react';
import type { Provenance } from '../../lib/bridge';

interface Props {
  provenance: Provenance;
}

export default function ProvenanceStrip({ provenance }: Props) {
  const [expanded, setExpanded] = useState(false);

  // Sum all elapsed_ms values for a total.
  const totalMs = Object.values(provenance.elapsed_ms).reduce((a, b) => a + b, 0);
  const totalLabel = totalMs > 0 ? `${Math.round(totalMs)} ms` : null;

  return (
    <div className="mt-1 border-t border-border pt-1 text-xs text-fg-muted">
      {/* Compact rung strip */}
      <button
        onClick={() => setExpanded((v) => !v)}
        className="flex items-center gap-1.5 w-full min-h-[36px] hover:text-fg transition-colors"
        aria-expanded={expanded}
      >
        <div className="flex items-center gap-1 flex-1 flex-wrap">
          {provenance.path.map((rung, i) => (
            <span key={i} className="flex items-center gap-0.5">
              {i > 0 && <span className="text-fg-subtle mx-0.5">→</span>}
              <RungIcon result={rung.result} />
              <span
                className={rung.result === 'miss' ? 'text-warning' : ''}
                title={rung.reason}
              >
                {rung.rung}
              </span>
            </span>
          ))}
        </div>
        {totalLabel && <span className="shrink-0 text-fg-subtle">{totalLabel}</span>}
        {expanded
          ? <ChevronUp size={12} aria-hidden="true" />
          : <ChevronDown size={12} aria-hidden="true" />}
      </button>

      {/* Expanded detail */}
      {expanded && (
        <div className="mt-1 space-y-1">
          {/* Backend badge */}
          <div className="flex items-center gap-2 flex-wrap">
            <BackendBadge backend={provenance.backend} />
            {provenance.service_line && (
              <span className="px-1.5 py-0.5 rounded bg-elevated text-fg-muted font-mono">
                {provenance.service_line}
              </span>
            )}
            {provenance.scope.length > 0 && (
              <span className="text-fg-subtle">
                scope: {provenance.scope.join(', ')}
              </span>
            )}
          </div>

          {/* Per-step timing */}
          {Object.keys(provenance.elapsed_ms).length > 0 && (
            <div className="flex flex-wrap gap-x-3 gap-y-0.5 font-mono text-fg-subtle">
              {Object.entries(provenance.elapsed_ms).map(([step, ms]) => (
                <span key={step}>{step}: {Math.round(ms)} ms</span>
              ))}
            </div>
          )}

          {/* Miss reasons */}
          {provenance.path.filter((r) => r.result === 'miss' && r.reason).map((r, i) => (
            <p key={i} className="text-warning">
              {r.rung}: {r.reason}
            </p>
          ))}
        </div>
      )}
    </div>
  );
}

function RungIcon({ result }: { result: string }) {
  switch (result) {
    case 'hit':     return <CheckCircle size={11} className="text-success shrink-0" aria-hidden="true" />;
    case 'miss':    return <XCircle size={11} className="text-warning shrink-0" aria-hidden="true" />;
    case 'skipped': return <MinusCircle size={11} className="text-fg-subtle shrink-0" aria-hidden="true" />;
    default:        return null;
  }
}

function BackendBadge({ backend }: { backend: string }) {
  const label: Record<string, string> = {
    source_sql:   'Live SQL',
    document_db:  'DocumentDB',
    semantic:     'Semantic',
    hybrid:       'Hybrid',
    none:         'No data',
  };
  return (
    <span className="flex items-center gap-1 px-1.5 py-0.5 rounded bg-elevated text-fg-muted">
      <Zap size={10} aria-hidden="true" />
      {label[backend] ?? backend}
    </span>
  );
}
