// Collapsible "Sources (N)" panel rendered under an assistant bubble.
// Grounded, auditable answers are the product's core value — always show this.
import { Fragment, useState } from 'react';
import { ChevronDown, ChevronUp } from 'lucide-react';
import type { Passage } from '../../lib/bridge';
import { Badge } from '../../components/ui';

interface CitationsProps {
  citations: Passage[];
}

export default function Citations({ citations }: CitationsProps) {
  const [open, setOpen] = useState(false);
  const [expandedIds, setExpandedIds] = useState<Set<string>>(new Set());

  if (citations.length === 0) return null;

  function toggleExpand(id: string) {
    setExpandedIds((prev) => {
      const next = new Set(prev);
      if (next.has(id)) next.delete(id);
      else next.add(id);
      return next;
    });
  }

  return (
    <div className="mt-2 border-t border-border pt-2">
      <button
        onClick={() => setOpen((v) => !v)}
        className="flex items-center gap-1.5 text-xs text-fg-muted hover:text-fg transition-colors min-h-[44px] py-2"
        aria-expanded={open}
      >
        {open ? <ChevronUp size={13} aria-hidden="true" /> : <ChevronDown size={13} aria-hidden="true" />}
        <span>Sources ({citations.length})</span>
      </button>

      {open && (
        <div className="flex flex-col gap-2 mt-1">
          {citations.map((p) => (
            <div key={p.id} className="rounded-md border border-border bg-elevated p-2 text-xs">
              <div className="flex items-center gap-2 flex-wrap">
                <span className="font-medium text-fg truncate max-w-[160px]">{p.source_id}</span>
                <span className="text-fg-subtle" aria-hidden="true">·</span>
                <span className="text-fg-muted truncate max-w-[120px]">{p.row_pk}</span>
                <Badge variant="neutral">{p.score.toFixed(3)}</Badge>
                {p.reranked && <Badge variant="info">reranked</Badge>}
                <button
                  onClick={() => toggleExpand(p.id)}
                  className="ml-auto text-fg-muted hover:text-fg min-h-[32px] px-1 flex items-center"
                  aria-label={expandedIds.has(p.id) ? 'Collapse source' : 'Expand source'}
                >
                  {expandedIds.has(p.id)
                    ? <ChevronUp size={12} aria-hidden="true" />
                    : <ChevronDown size={12} aria-hidden="true" />}
                </button>
              </div>

              {expandedIds.has(p.id) && (
                <div className="mt-2 space-y-1.5">
                  {p.text && (
                    <p className="text-fg-muted whitespace-pre-wrap leading-relaxed">{p.text}</p>
                  )}
                  {Object.keys(p.fields).length > 0 && (
                    <dl className="mt-1 grid grid-cols-[auto_1fr] gap-x-3 gap-y-0.5">
                      {Object.entries(p.fields).map(([k, v]) => (
                        <Fragment key={k}>
                          <dt className="text-fg-subtle font-medium">{k}</dt>
                          <dd className="text-fg-muted break-all">
                            {typeof v === 'object' ? JSON.stringify(v) : String(v ?? '')}
                          </dd>
                        </Fragment>
                      ))}
                    </dl>
                  )}
                </div>
              )}
            </div>
          ))}
        </div>
      )}
    </div>
  );
}
