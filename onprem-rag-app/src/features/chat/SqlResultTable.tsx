import { Database } from 'lucide-react';
import type { SqlResult } from '../../lib/bridge';

function displayValue(value: unknown): string {
  if (value === null || value === undefined) return '—';
  if (typeof value === 'object') return JSON.stringify(value);
  return String(value);
}

export default function SqlResultTable({ result }: { result: Partial<SqlResult> }) {
  if (!result.columns || !result.rows) return null;

  return (
    <details className="rounded-lg border border-border bg-surface overflow-hidden">
      <summary className="cursor-pointer select-none px-3 py-2 text-xs font-medium text-fg-muted hover:text-fg">
        <span className="inline-flex items-center gap-1.5">
          <Database size={13} aria-hidden="true" />
          Live database result · {result.rows.length} {result.rows.length === 1 ? 'row' : 'rows'}
        </span>
      </summary>
      <div className="overflow-x-auto border-t border-border">
        <table className="min-w-full text-xs">
          <thead className="bg-elevated text-fg-muted">
            <tr>
              {result.columns.map((column, index) => (
                <th key={`${column}-${index}`} className="px-3 py-2 text-left font-medium whitespace-nowrap">
                  {column}
                </th>
              ))}
            </tr>
          </thead>
          <tbody>
            {result.rows.map((row, rowIndex) => (
              <tr key={rowIndex} className="border-t border-border">
                {result.columns!.map((_, columnIndex) => (
                  <td key={columnIndex} className="px-3 py-2 text-fg whitespace-nowrap max-w-64 truncate">
                    {displayValue(row[columnIndex])}
                  </td>
                ))}
              </tr>
            ))}
          </tbody>
        </table>
        {result.sql && (
          <details className="border-t border-border px-3 py-2">
            <summary className="cursor-pointer text-xs text-fg-muted">View generated query</summary>
            <pre className="mt-2 overflow-x-auto whitespace-pre-wrap text-xs text-fg-subtle">{result.sql}</pre>
          </details>
        )}
      </div>
    </details>
  );
}
