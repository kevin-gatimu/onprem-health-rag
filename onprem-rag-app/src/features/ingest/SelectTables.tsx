// SelectTables — Step 4: review AI analysis, pick tables + exclude columns,
// then kick off the ingestion job. Mobile-first with internal scroll on table list.
import {
  Sparkles, AlertTriangle, ChevronDown, ChevronRight, Eye, EyeOff, Play,
} from 'lucide-react';
import { useIngestion } from '../../stores/ingestion';
import { startIngest } from '../../lib/bridge';
import { toast } from '../../stores/ui';
import { Badge, Button } from '../../components/ui';

export default function SelectTables() {
  const sourceId      = useIngestion((s) => s.sourceId);
  const schema        = useIngestion((s) => s.schema);
  const analysis      = useIngestion((s) => s.analysis);
  const analysisError = useIngestion((s) => s.analysisError);
  const selections    = useIngestion((s) => s.selections);
  const toggleTable   = useIngestion((s) => s.toggleTable);
  const toggleColumn  = useIngestion((s) => s.toggleColumn);
  const toggleExpand  = useIngestion((s) => s.toggleExpand);
  const selectAll     = useIngestion((s) => s.selectAll);
  const startJob      = useIngestion((s) => s.startJob);
  const markComplete  = useIngestion((s) => s.markComplete);

  const selectedCount = Object.values(selections).filter((v) => v.selected).length;
  const totalRows = schema
    .filter((t) => selections[t.name]?.selected)
    .reduce((sum, t) => sum + t.row_count, 0);

  function handleStart() {
    if (!sourceId) return;
    const selectedTables = Object.entries(selections)
      .filter(([, v]) => v.selected)
      .map(([name]) => name);

    if (selectedTables.length === 0) {
      toast.warning('Select at least one table to ingest.');
      return;
    }

    const excludedCols: Record<string, string[]> = {};
    for (const name of selectedTables) {
      const sel = selections[name];
      if (sel && sel.excluded_columns.length > 0) {
        excludedCols[name] = [...sel.excluded_columns];
      }
    }

    startJob();
    void startIngest(
      sourceId,
      selectedTables,
      Object.keys(excludedCols).length > 0 ? excludedCols : undefined,
    ).catch((err: unknown) => {
      toast.error(`Ingestion error: ${(err as Error).message ?? String(err)}`);
      markComplete();
    });
  }

  return (
    // pb-32 reserves space below the sticky start bar so content isn't hidden.
    <div className="flex flex-col gap-4 pb-32">

      {/* ── AI analysis card or skip note ── */}
      {analysis ? (
        <div className="rounded-lg border border-accent/30 bg-accent-subtle/10 p-4 flex flex-col gap-2">
          <div className="flex items-center gap-2 text-accent text-sm font-semibold">
            <Sparkles size={15} />
            <span>AI Schema Analysis</span>
          </div>
          <p className="text-sm text-fg">{analysis.summary}</p>
          {analysis.data_quality_notes.length > 0 && (
            <ul className="flex flex-col gap-1 mt-1">
              {analysis.data_quality_notes.map((note, i) => (
                <li key={i} className="flex items-start gap-1.5 text-xs text-warning">
                  <AlertTriangle size={11} className="flex-shrink-0 mt-0.5" />
                  <span>{note}</span>
                </li>
              ))}
            </ul>
          )}
        </div>
      ) : (
        <div className="flex items-start gap-2 p-3 rounded-md border border-warning/20 bg-warning-subtle/20 text-sm text-warning">
          <AlertTriangle size={14} className="flex-shrink-0 mt-0.5" />
          <span>
            {analysisError
              ?? 'AI analysis skipped — all tables pre-selected; review manually.'}
          </span>
        </div>
      )}

      {/* ── Table selection ── */}
      <div className="rounded-lg border border-border bg-surface flex flex-col">

        {/* Section header + bulk actions */}
        <div className="flex items-center gap-3 px-4 py-3 border-b border-border">
          <div className="flex-1 min-w-0">
            <p className="text-sm font-semibold text-fg">Select tables to ingest</p>
            <p className="text-xs text-fg-muted mt-0.5">
              {selectedCount} of {schema.length} selected · {totalRows.toLocaleString()} rows
            </p>
          </div>
          <div className="flex items-center gap-2">
            <button
              onClick={() => selectAll(true)}
              className="px-2.5 py-1 text-xs font-medium rounded border border-border bg-elevated hover:bg-border text-fg transition-colors min-h-[32px]"
            >
              All
            </button>
            <button
              onClick={() => selectAll(false)}
              className="px-2.5 py-1 text-xs font-medium rounded border border-border bg-elevated hover:bg-border text-fg transition-colors min-h-[32px]"
            >
              None
            </button>
          </div>
        </div>

        {/* Table list — scrolls internally to avoid a very long page on phones */}
        <div className="overflow-y-auto max-h-96 sm:max-h-[60vh]">
          <div className="divide-y divide-border">
            {schema.map((table) => {
              const sel = selections[table.name];
              if (!sel) return null;
              const isSuggested = analysis?.suggested_tables.includes(table.name) ?? false;
              const piiCols = analysis?.pii_columns[table.name] ?? [];

              return (
                <div
                  key={table.name}
                  className={`flex flex-col transition-colors ${sel.selected ? 'bg-accent-subtle/5' : ''}`}
                >
                  {/* Table row */}
                  <div className="flex items-center gap-2 px-4 py-2 min-h-[52px]">
                    <label className="flex items-center justify-center min-h-[44px] min-w-[44px] cursor-pointer">
                      <input
                        type="checkbox"
                        checked={sel.selected}
                        onChange={() => toggleTable(table.name)}
                        className="w-4 h-4 accent-[var(--color-accent)] cursor-pointer"
                      />
                    </label>
                    <span className="flex-1 text-sm font-medium text-fg truncate min-w-0">
                      {table.name}
                    </span>
                    <div className="flex items-center gap-1.5 flex-wrap justify-end">
                      {isSuggested && <Badge variant="info">AI suggested</Badge>}
                      {piiCols.length > 0 && <Badge variant="warning">PII columns</Badge>}
                      <span className="text-xs text-fg-muted whitespace-nowrap">
                        {table.row_count.toLocaleString()} rows
                      </span>
                    </div>
                    <button
                      onClick={() => toggleExpand(table.name)}
                      title={sel.expanded ? 'Hide columns' : 'Show columns'}
                      className="flex items-center gap-1 min-h-[44px] px-2 text-xs text-fg-muted hover:text-fg transition-colors flex-shrink-0"
                    >
                      {sel.expanded ? <ChevronDown size={14} /> : <ChevronRight size={14} />}
                      <span className="hidden sm:inline">{table.columns.length} cols</span>
                    </button>
                  </div>

                  {/* Expanded column list */}
                  {sel.expanded && (
                    <div className="overflow-x-auto border-t border-border/50 bg-base/50">
                      <div className="min-w-[320px] divide-y divide-border/40">
                        {table.columns.map((col) => {
                          const excluded = sel.excluded_columns.includes(col.name);
                          const isPii =
                            col.likely_pii ||
                            (analysis?.pii_columns[table.name] ?? []).includes(col.name);
                          return (
                            <div
                              key={col.name}
                              className={`flex items-center gap-2 px-4 py-1 text-xs ${excluded ? 'opacity-50' : ''}`}
                            >
                              <button
                                onClick={() => toggleColumn(table.name, col.name)}
                                title={excluded ? 'Include column' : 'Exclude column'}
                                className="flex items-center justify-center min-h-[44px] min-w-[44px] text-fg-muted hover:text-fg transition-colors flex-shrink-0"
                              >
                                {excluded ? <EyeOff size={13} /> : <Eye size={13} />}
                              </button>
                              <span className={`font-mono ${excluded ? 'line-through' : ''} text-fg truncate min-w-0 flex-1`}>
                                {col.name}
                              </span>
                              <span className="text-fg-muted whitespace-nowrap">{col.type}</span>
                              {col.is_primary_key && <Badge variant="neutral">PK</Badge>}
                              {isPii && <Badge variant="warning">PII</Badge>}
                            </div>
                          );
                        })}
                      </div>
                    </div>
                  )}
                </div>
              );
            })}
          </div>
        </div>
      </div>

      {/* ── Sticky start bar ── */}
      <div
        className="sticky bottom-0 z-10 -mx-4 px-4 py-3 bg-surface border-t border-border flex items-center justify-between gap-3 sm:-mx-6 sm:px-6"
        style={{ paddingBottom: 'calc(0.75rem + env(safe-area-inset-bottom, 0px))' }}
      >
        <span className="text-sm text-fg-muted">
          <strong className="text-fg">{selectedCount}</strong> tables ·{' '}
          <strong className="text-fg">{totalRows.toLocaleString()}</strong> rows selected
        </span>
        <Button
          variant="primary"
          leftIcon={<Play size={15} />}
          onClick={handleStart}
          disabled={selectedCount === 0}
          className="flex-shrink-0"
        >
          Start Ingestion
        </Button>
      </div>

    </div>
  );
}
