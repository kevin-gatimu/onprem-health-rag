// OverviewBar — the four overview stat tiles + Refresh + admin Clear-all.
// Mobile: tiles are a 2×2 grid and actions wrap below. md+: tiles sit in a row
// with the actions pushed to the right.
import { useState } from 'react';
import { Database, Table2, Rows4, Boxes, RefreshCw, Trash2 } from 'lucide-react';
import { useQueryClient } from '@tanstack/react-query';
import { clearAllRecords } from '../../lib/bridge';
import { toast } from '../../stores/ui';
import { Button } from '../../components/ui';
import { fmtNum } from './utils';

export interface OverviewTotals {
  connections: number;
  tables: number;
  rows: number;
  vectors: number;
}

interface OverviewBarProps {
  totals: OverviewTotals;
  isAdmin: boolean;
  /** Called after a successful clear-all so the shell can drop its selection. */
  onAfterClearAll: () => void;
}

export default function OverviewBar({ totals, isAdmin, onAfterClearAll }: OverviewBarProps) {
  const queryClient = useQueryClient();
  const [refreshing, setRefreshing] = useState(false);
  const [clearing, setClearing] = useState(false);

  async function handleRefresh() {
    setRefreshing(true);
    try {
      await Promise.all([
        queryClient.invalidateQueries({ queryKey: ['ingest-history'] }),
        queryClient.invalidateQueries({ queryKey: ['records'] }),
        queryClient.invalidateQueries({ queryKey: ['table-info'] }),
      ]);
    } finally {
      setRefreshing(false);
    }
  }

  async function handleClearAll() {
    if (
      !window.confirm(
        'Clear ALL indexed data?\n\nThis removes raw rows, vector lineage, and stored vectors for every indexed table. The source databases are not touched.',
      )
    )
      return;
    setClearing(true);
    try {
      await clearAllRecords();
      onAfterClearAll();
      queryClient.invalidateQueries({ queryKey: ['ingest-history'] });
      queryClient.invalidateQueries({ queryKey: ['records'] });
      queryClient.invalidateQueries({ queryKey: ['table-info'] });
      queryClient.invalidateQueries({ queryKey: ['stats'] });
      toast.success('All indexed data cleared.');
    } catch (err) {
      toast.error(`Failed to clear data: ${(err as Error).message}`);
    } finally {
      setClearing(false);
    }
  }

  const tiles = [
    { label: 'Connections', value: totals.connections, icon: <Database size={18} /> },
    { label: 'Tables', value: totals.tables, icon: <Table2 size={18} /> },
    { label: 'Rows', value: totals.rows, icon: <Rows4 size={18} /> },
    { label: 'Vectors', value: totals.vectors, icon: <Boxes size={18} /> },
  ];

  return (
    <div className="flex flex-col gap-3 lg:flex-row lg:items-center lg:justify-between">
      {/* Stat tiles: 2×2 on phones, single row on md+. */}
      <div className="grid grid-cols-2 gap-3 md:flex md:flex-row md:flex-wrap lg:flex-1">
        {tiles.map((t) => (
          <div
            key={t.label}
            className="flex items-center gap-3 rounded-lg border border-border bg-surface px-4 py-3 md:min-w-[140px] md:flex-1"
          >
            <span className="text-accent flex-shrink-0">{t.icon}</span>
            <div className="flex flex-col min-w-0">
              <span className="text-lg font-semibold text-fg leading-tight">{fmtNum(t.value)}</span>
              <span className="text-xs text-fg-muted truncate">{t.label}</span>
            </div>
          </div>
        ))}
      </div>

      {/* Actions */}
      <div className="flex items-center gap-2">
        <Button
          variant="ghost"
          size="sm"
          loading={refreshing}
          leftIcon={<RefreshCw size={14} />}
          onClick={handleRefresh}
          className="min-h-[44px]"
        >
          Refresh
        </Button>
        {isAdmin && totals.tables > 0 && (
          <Button
            variant="danger"
            size="sm"
            loading={clearing}
            leftIcon={<Trash2 size={14} />}
            onClick={handleClearAll}
            className="min-h-[44px]"
            title="Clear all indexed data"
          >
            Clear all
          </Button>
        )}
      </div>
    </div>
  );
}
