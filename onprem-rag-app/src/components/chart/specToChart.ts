// Adapter: converts a structured aggregation result (AggSpec + AggRow[]) into a
// ParsedChart that ChartView can render directly — without going through the
// LLM-generated fence-string path.
import type { AggSpec, AggRow } from '../../lib/bridge';
import type { ChartType, ParsedChart } from './chart-schema';

/**
 * Build a render-ready ParsedChart from a structured aggregation result.
 *
 * Chart type heuristic:
 *   - time_bucket present  → 'line'  (ordered temporal series)
 *   - ≤ 8 rows             → 'bar'   (short categorical, fits vertically)
 *   - > 8 rows             → 'hbar'  (many labels → horizontal bars)
 *
 * Unit is inferred from the metric op/field:
 *   count → "records"
 *   avg   → "avg <field>"
 *   sum   → "<field>"
 *   else  → undefined
 *
 * Returns null when there are no rows or none have a plottable value > 0.
 */
export function specToChart(
  spec: AggSpec | Record<string, unknown>,
  rows: AggRow[],
  title?: string,
): ParsedChart | null {
  if (!rows || rows.length === 0) return null;

  const data = rows.map((r) => ({ x: r.label, y: r.value }));

  // Guard: at least one point must be finite and positive to be worth drawing.
  const hasPlottable = data.some((p) => Number.isFinite(p.y) && p.y > 0);
  if (!hasPlottable) return null;

  // Chart type from temporal vs categorical
  const hasTimeBucket =
    'time_bucket' in spec && spec.time_bucket != null;
  const chartType: ChartType = hasTimeBucket
    ? 'line'
    : rows.length <= 8
    ? 'bar'
    : 'hbar';

  // Title: caller's question string, or fall back to the collection name
  const resolvedTitle =
    title ??
    (typeof spec.collection === 'string' && spec.collection ? spec.collection : 'Result');

  // Unit from metric op/field — guard spec.metric since it may be absent
  let unit: string | undefined;
  const rawMetric = 'metric' in spec ? spec.metric : undefined;
  if (rawMetric !== null && rawMetric !== undefined && typeof rawMetric === 'object') {
    const metric = rawMetric as Record<string, unknown>;
    const op = metric['op'];
    const field = metric['field'];
    if (op === 'count') {
      unit = 'records';
    } else if (op === 'avg') {
      unit = typeof field === 'string' && field ? `avg ${field}` : 'avg';
    } else if (op === 'sum') {
      unit = typeof field === 'string' && field ? field : undefined;
    }
  }

  const seriesName = unit ?? 'value';

  return {
    chartType,
    title: resolvedTitle,
    unit,
    series: [{ name: seriesName, data }],
  };
}
