// ── Chart data contract ───────────────────────────────────────────────────────
// The LLM (or the deterministic auto-charter in the agent executor) emits a
// fenced ```chart block. Two formats are accepted:
//
//   1. NEW — a JSON object matching CHART_SCHEMA (multi-series, typed, robust):
//        { "chartType": "hbar", "title": "Top diagnoses",
//          "unit": "patients", "series": [{ "name": "count",
//          "data": [{ "x": "Malaria", "y": 42 }] }] }
//
//   2. LEGACY — the original line grammar (single-series bar), kept so older
//      generate_chart / buildAutoCharts output still renders:
//        type: bar
//        title: Top diagnoses
//        data:
//        - label: Malaria | value: 42
//
// parseChart() normalises both into a single ParsedChart, or returns null when
// there is nothing worth drawing (no points, or every value ≤ 0). Returning null
// is deliberate: the renderer shows NOTHING rather than an empty bar chart under
// a "no records" answer — the bug this contract was built to kill.

import { z } from 'zod';

export const CHART_TYPES = [
  'bar', 'hbar', 'line', 'area', 'donut', 'grouped-bar', 'stacked-bar', 'kpi',
] as const;
export type ChartType = (typeof CHART_TYPES)[number];

const POINT_SCHEMA = z.object({
  x: z.union([z.string(), z.number()]),
  y: z.number(),
});

const SERIES_SCHEMA = z.object({
  name: z.string().default('value'),
  data: z.array(POINT_SCHEMA),
});

export const CHART_SCHEMA = z.object({
  chartType: z.enum(CHART_TYPES).default('bar'),
  title: z.string().default(''),
  unit: z.string().optional(),
  xLabel: z.string().optional(),
  yLabel: z.string().optional(),
  series: z.array(SERIES_SCHEMA).min(1),
});
export type ChartSpec = z.infer<typeof CHART_SCHEMA>;

/** One point after normalisation. */
export interface ChartPoint {
  x: string | number;
  y: number;
}

/** One series of chart data — a named array of points. */
export interface ChartSeries {
  name: string;
  data: ChartPoint[];
}

/** Normalised, render-ready chart. Always has ≥1 series with ≥1 positive point. */
export interface ParsedChart {
  chartType: ChartType;
  title: string;
  unit?: string;
  xLabel?: string;
  yLabel?: string;
  series: ChartSeries[];
}

/** True when at least one point across all series has a finite value > 0. */
function hasPlottableData(series: ParsedChart['series']): boolean {
  return series.some((s) => s.data.some((p) => Number.isFinite(p.y) && p.y > 0));
}

/** Try the NEW JSON contract. Returns null if it isn't valid JSON/schema. */
function parseJsonChart(raw: string): ParsedChart | null {
  let json: unknown;
  try {
    json = JSON.parse(raw);
  } catch {
    return null;
  }
  const result = CHART_SCHEMA.safeParse(json);
  if (!result.success) return null;

  const spec = result.data;
  const series: ChartSeries[] = spec.series.map((s) => ({
    name: s.name,
    // Coerce y to a finite number; drop points that can't be plotted.
    data: s.data
      .map((p) => ({ x: p.x, y: Number(p.y) }))
      .filter((p) => Number.isFinite(p.y)),
  }));

  if (!hasPlottableData(series)) return null;
  return {
    chartType: spec.chartType,
    title: spec.title,
    unit: spec.unit,
    xLabel: spec.xLabel,
    yLabel: spec.yLabel,
    series,
  };
}

/** Parse the LEGACY `type: / title: / - label: X | value: Y` line grammar. */
function parseLegacyChart(raw: string): ParsedChart | null {
  const lines = raw.trim().split('\n');
  let chartType: ChartType = 'bar';
  let title = '';
  const data: ChartPoint[] = [];

  for (const line of lines) {
    const trimmed = line.trim();
    if (trimmed.startsWith('type:')) {
      const t = trimmed.slice(5).trim().toLowerCase();
      chartType = (CHART_TYPES as readonly string[]).includes(t) ? (t as ChartType) : 'bar';
    } else if (trimmed.startsWith('title:')) {
      title = trimmed.slice(6).trim();
    } else if (trimmed.startsWith('- label:')) {
      // Tolerate thousands separators ("1,200") and decimals.
      const match = trimmed.match(/- label:\s*(.+?)\s*\|\s*value:\s*([\d,]+\.?\d*)/);
      if (match) {
        const y = parseFloat(match[2].replace(/,/g, ''));
        if (Number.isFinite(y)) data.push({ x: match[1].trim(), y });
      }
    }
  }

  if (data.length === 0) return null;
  const series: ChartSeries[] = [{ name: 'value', data }];
  if (!hasPlottableData(series)) return null;
  return { chartType, title, series };
}

/**
 * Parse a fenced chart block (either format) into a render-ready ParsedChart,
 * or null when there's nothing plottable. JSON is tried first (it starts with
 * `{`), then the legacy grammar.
 */
export function parseChart(raw: string): ParsedChart | null {
  const text = raw.trim();
  if (!text) return null;
  if (text.startsWith('{')) return parseJsonChart(text);
  return parseLegacyChart(text);
}
