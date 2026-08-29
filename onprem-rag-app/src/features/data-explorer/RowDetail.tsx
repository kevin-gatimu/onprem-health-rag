// RowDetail — drawer showing one row in full: identity fields (id, source_id,
// ingested_at) plus every `data` field as label:value, objects pretty-printed.
import { Modal } from '../../components/ui';
import type { DataRow } from '../../lib/bridge';
import { fmtDate, formatValue } from './utils';

interface RowDetailProps {
  open: boolean;
  onClose: () => void;
  row: DataRow | null;
}

export default function RowDetail({ open, onClose, row }: RowDetailProps) {
  return (
    <Modal open={open} onClose={onClose} title="Row details" size="md">
      {!row ? null : (
        <div className="flex flex-col gap-4">
          {/* Identity */}
          <dl className="flex flex-col gap-2">
            <Field label="Row ID" value={row.id || '—'} mono />
            <Field label="Source ID" value={row.source_id || '—'} mono />
            <Field label="Ingested at" value={fmtDate(row.ingested_at)} mono />
          </dl>

          <div className="border-t border-border" />

          {/* Data fields */}
          <div className="flex flex-col gap-3">
            {Object.entries(row.data).map(([key, value]) => (
              <div key={key} className="flex flex-col gap-1 min-w-0">
                <span className="text-xs font-medium text-fg-muted break-words">{key}</span>
                <pre className="text-sm text-fg whitespace-pre-wrap break-words bg-elevated rounded-md p-2 overflow-x-auto">
                  {formatValue(value)}
                </pre>
              </div>
            ))}
          </div>
        </div>
      )}
    </Modal>
  );
}

function Field({ label, value, mono }: { label: string; value: string; mono?: boolean }) {
  return (
    <div className="flex flex-col gap-0.5 min-w-0">
      <span className="text-xs font-medium text-fg-muted">{label}</span>
      <span className={`text-sm text-fg break-words ${mono ? 'font-mono text-xs' : ''}`}>{value}</span>
    </div>
  );
}
