// Retrieval override sheet — mode (vector/hybrid), rerank toggle, top_k select.
// Writes component-local opts that the parent passes into chat().
// Unset fields fall back to server defaults. Accessible via the sliders icon in Composer.
import { Modal, cn } from '../../components/ui';
import type { RetrievalOpts, RetrievalMode } from '../../lib/bridge';

interface RetrievalSettingsProps {
  open: boolean;
  onClose: () => void;
  opts: RetrievalOpts;
  onOptsChange: (opts: RetrievalOpts) => void;
}

type ModeOption = { label: string; value: RetrievalMode | null };
const MODE_OPTIONS: ModeOption[] = [
  { label: 'Default', value: null },
  { label: 'Vector', value: 'vector' },
  { label: 'Hybrid', value: 'hybrid' },
];

const TOP_K_OPTIONS = [4, 6, 8, 10] as const;

export default function RetrievalSettings({
  open,
  onClose,
  opts,
  onOptsChange,
}: RetrievalSettingsProps) {
  const currentMode = opts.mode ?? null;

  return (
    <Modal
      open={open}
      onClose={onClose}
      title="Retrieval Settings"
      size="sm"
      footer={
        <button
          onClick={onClose}
          className="px-4 py-2 text-sm font-medium rounded-md bg-accent text-accent-fg hover:bg-accent-hover transition-colors min-h-[44px]"
        >
          Done
        </button>
      }
    >
      <div className="flex flex-col gap-5">
        {/* Mode segmented control */}
        <div className="flex flex-col gap-1.5">
          <p className="text-xs font-semibold text-fg-muted uppercase tracking-wide">
            Retrieval mode
          </p>
          <div className="flex gap-2">
            {MODE_OPTIONS.map(({ label, value }) => (
              <button
                key={label}
                onClick={() => onOptsChange({ ...opts, mode: value })}
                className={cn(
                  'flex-1 text-xs px-2 py-2 rounded-md border transition-colors min-h-[44px]',
                  currentMode === value
                    ? 'border-accent bg-accent-subtle text-accent'
                    : 'border-border text-fg-muted hover:bg-elevated hover:text-fg',
                )}
              >
                {label}
              </button>
            ))}
          </div>
        </div>

        {/* Rerank toggle */}
        <div className="flex items-center justify-between gap-3">
          <div>
            <p className="text-sm font-medium text-fg">Cross-encoder rerank</p>
            <p className="text-xs text-fg-muted mt-0.5">
              More accurate, slightly slower. Unset = server default (on).
            </p>
          </div>
          <button
            role="switch"
            aria-checked={opts.rerank === true}
            onClick={() => {
              // Cycle: null → true → false → null
              const cur = opts.rerank ?? null;
              const next = cur === null ? true : cur === true ? false : null;
              onOptsChange({ ...opts, rerank: next });
            }}
            className={cn(
              'relative flex-shrink-0 w-10 h-6 rounded-full transition-colors',
              opts.rerank === true ? 'bg-accent' : 'bg-border',
            )}
            aria-label="Toggle reranking"
          >
            <span
              className={cn(
                'absolute top-0.5 left-0.5 w-5 h-5 rounded-full bg-surface shadow transition-transform',
                opts.rerank === true ? 'translate-x-4' : 'translate-x-0',
              )}
            />
          </button>
        </div>

        {/* Top K */}
        <div className="flex flex-col gap-1.5">
          <p className="text-xs font-semibold text-fg-muted uppercase tracking-wide">
            Top K results
          </p>
          <div className="flex gap-2">
            {TOP_K_OPTIONS.map((k) => (
              <button
                key={k}
                onClick={() => onOptsChange({ ...opts, top_k: opts.top_k === k ? null : k })}
                className={cn(
                  'flex-1 text-xs px-2 py-2 rounded-md border transition-colors min-h-[44px]',
                  opts.top_k === k
                    ? 'border-accent bg-accent-subtle text-accent'
                    : 'border-border text-fg-muted hover:bg-elevated hover:text-fg',
                )}
              >
                {k}
              </button>
            ))}
          </div>
          <p className="text-xs text-fg-subtle">Unset = server default (6)</p>
        </div>
      </div>
    </Modal>
  );
}
