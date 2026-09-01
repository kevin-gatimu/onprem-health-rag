// ConnectionCard — displays one source database connection with its meta and actions.
// Mobile-first: content wraps gracefully at 360 px without horizontal overflow.
import {
  Database, Server, Globe, Clock,
  AlertCircle, Plug, Download, Pencil, Trash2, DatabaseZap,
} from 'lucide-react';
import { Badge, Button } from '../../components/ui';
import { useUi } from '../../stores/ui';
import type { SourceInfo } from '../../lib/bridge';

export interface ConnectionCardProps {
  source: SourceInfo;
  onTest: () => void;
  onEdit: () => void;
  onDelete: () => void;
  onManageMetadata: () => void;
  testing: boolean;
  deleting: boolean;
}

// Human-readable display labels for each source kind (uppercased per spec).
const KIND_LABEL: Record<string, string> = {
  postgres: 'POSTGRESQL',
  mysql: 'MYSQL',
  mssql: 'MSSQL',
};

// Format an ISO timestamp to a friendly local datetime; returns null for null input.
function fmtDateTime(iso: string | null): string | null {
  if (!iso) return null;
  return new Date(iso).toLocaleString(undefined, {
    dateStyle: 'medium',
    timeStyle: 'short',
  });
}

export default function ConnectionCard({
  source,
  onTest,
  onEdit,
  onDelete,
  onManageMetadata,
  testing,
  deleting,
}: ConnectionCardProps) {
  // Navigate to /ingest directly — single fixed destination, no need to thread as prop.
  const navigate = useUi((s) => s.navigate);

  const { name, kind, host, port, database, status, last_connected, error } = source;
  const lastTestedFmt = fmtDateTime(last_connected);

  return (
    <div
      className={[
        'rounded-lg border bg-surface p-4 flex flex-col gap-3',
        // Subtle accent ring signals a successfully-tested connection without saturating the UI.
        status === 'connected' ? 'border-accent/40' : 'border-border',
      ].join(' ')}
    >

      {/* ── Top row: icon tile + name + status badge ─────────────────────── */}
      <div className="flex items-start gap-3">
        <span className="flex-shrink-0 flex items-center justify-center w-9 h-9 rounded-md bg-accent-subtle text-accent">
          <Database size={18} />
        </span>
        <div className="flex-1 min-w-0">
          <div className="flex items-start justify-between gap-2 flex-wrap">
            {/* truncate caps the name at the card width on narrow screens */}
            <span className="font-semibold text-fg text-sm truncate leading-snug flex-1 min-w-0 pr-1">
              {name}
            </span>
            <Badge
              variant={
                status === 'connected' ? 'success'
                  : status === 'error' ? 'error'
                  : 'neutral'
              }
              dot
            >
              {status}
            </Badge>
          </div>
        </div>
      </div>

      {/* ── Meta rows: kind, host/database, last tested ───────────────────── */}
      <div className="flex flex-col gap-1">
        <div className="flex items-center gap-2 text-xs text-fg-muted">
          <Server size={13} className="flex-shrink-0" />
          <span>{KIND_LABEL[kind] ?? kind.toUpperCase()}</span>
        </div>
        <div className="flex items-center gap-2 text-xs text-fg-muted">
          <Globe size={13} className="flex-shrink-0" />
          {/* truncate prevents long hostnames / DB names from overflowing at 360 px */}
          <span className="truncate">{host}:{port} / {database}</span>
        </div>
        {lastTestedFmt && (
          <div className="flex items-center gap-2 text-xs text-fg-muted">
            <Clock size={13} className="flex-shrink-0" />
            <span className="truncate">Last tested: {lastTestedFmt}</span>
          </div>
        )}
      </div>

      {/* ── Error detail line — only shown when status is 'error' ─────────── */}
      {status === 'error' && error && (
        <div className="flex items-start gap-1.5 text-xs text-danger">
          <AlertCircle size={14} className="flex-shrink-0 mt-0.5" />
          {/* break-all prevents long connection error strings from blowing out the card */}
          <span className="break-all">{error}</span>
        </div>
      )}

      {/* ── Actions row ───────────────────────────────────────────────────── */}
      {/* flex-wrap lets buttons reflow to a second line at 360 px rather than overflow. */}
      <div className="flex flex-wrap gap-2 pt-3 mt-3 border-t border-border">
        <Button
          size="sm"
          variant="secondary"
          leftIcon={<Plug size={14} />}
          loading={testing}
          onClick={onTest}
          className="min-h-[44px]"
        >
          Test
        </Button>
        <Button
          size="sm"
          variant="ghost"
          leftIcon={<Download size={14} />}
          onClick={() => navigate('/ingest')}
          className="min-h-[44px]"
        >
          Ingest
        </Button>
        <Button
          size="sm"
          variant="ghost"
          leftIcon={<DatabaseZap size={14} />}
          onClick={onManageMetadata}
          className="min-h-[44px]"
        >
          Metadata
        </Button>
        <Button
          size="sm"
          variant="ghost"
          leftIcon={<Pencil size={14} />}
          onClick={onEdit}
          className="min-h-[44px]"
        >
          Edit
        </Button>
        <Button
          size="sm"
          variant="danger"
          leftIcon={<Trash2 size={14} />}
          loading={deleting}
          onClick={onDelete}
          className="min-h-[44px]"
        >
          Delete
        </Button>
      </div>

    </div>
  );
}
