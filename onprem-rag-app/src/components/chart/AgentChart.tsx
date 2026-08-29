// ── Multi-type chart renderer for LLM statistical output ──────────────────────
// Renders a fenced ```chart block (new JSON contract OR legacy line grammar; see
// chart-schema.ts) as a crisp, theme-aware SVG. Colors come from --chart-* CSS
// vars so light/dark "just works" and PDF export stays vector-sharp.
//
// If the block has no plottable data (empty, or all values ≤ 0) parseChart
// returns null and we render NOTHING — no more empty bar graph under a "no
// records" answer.
//
// Two entry points are exported:
//   ChartView  — takes an already-parsed ParsedChart; used by the structured
//                data path (specToChart → ChartView) to skip stringifying.
//   AgentChart — default export; takes the raw fence string, calls parseChart,
//                and delegates to ChartView.
import { useEffect, useMemo, useRef, useState } from 'react';
// Submodule imports (not the `d3` barrel) so Vite tree-shakes to only what each
// chart type needs, instead of bundling the whole ~930 KB d3 meta-package.
import { select, type Selection } from 'd3-selection';
import { max, sum } from 'd3-array';
import { scaleBand, scaleLinear, scalePoint } from 'd3-scale';
import { line as d3line, area as d3area, arc as d3arc, pie as d3pie, curveMonotoneX } from 'd3-shape';
import { format } from 'd3-format';
import { parseChart, type ParsedChart, type ChartPoint } from './chart-schema';

const SERIES_VARS = [
  '--chart-series-1', '--chart-series-2', '--chart-series-3', '--chart-series-4',
  '--chart-series-5', '--chart-series-6', '--chart-series-7', '--chart-series-8',
];

/** Resolve a CSS custom property to its computed value (d3 needs a real color). */
function cssVar(el: Element, name: string, fallback: string): string {
  const v = getComputedStyle(el).getPropertyValue(name).trim();
  return v || fallback;
}

const fmtInt = format(',');
const fmtNum = format(',.4~g');
function fmtValue(v: number, unit?: string): string {
  const n = Number.isInteger(v) ? fmtInt(v) : fmtNum(v);
  if (!unit) return n;
  return unit === '%' ? `${n}%` : `${n} ${unit}`;
}

// ── KPI stat tiles (HTML) ─────────────────────────────────────────────────────

function KpiRow({ chart }: { chart: ParsedChart }) {
  // Each point in series[0] is one tile: x = label, y = value.
  const points = chart.series[0]?.data ?? [];
  return (
    <div className="flex flex-wrap gap-3 justify-center">
      {points.map((p, i) => (
        <div key={i} className="flex flex-col items-center rounded-lg bg-elevated px-4 py-3 min-w-24">
          <span className="text-2xl font-bold text-fg">{fmtValue(p.y, chart.unit)}</span>
          <span className="text-xs text-fg-muted mt-1">{String(p.x)}</span>
        </div>
      ))}
    </div>
  );
}

// ── ChartView — does the d3 work + ResizeObserver ─────────────────────────────

/**
 * Rendered chart from a pre-parsed ParsedChart. Exported so the structured-data
 * path can pass a ParsedChart directly (via specToChart) without stringifying it.
 * Handles its own container width observation and drives the d3 SVG renderer.
 */
export function ChartView({ chart }: { chart: ParsedChart }) {
  const wrapRef = useRef<HTMLDivElement>(null);
  const svgRef = useRef<SVGSVGElement>(null);
  const [width, setWidth] = useState(0);

  // Width-observe the container so the chart fits its (variable-width) chat bubble.
  useEffect(() => {
    const el = wrapRef.current;
    if (!el) return;
    const ro = new ResizeObserver((entries) => {
      const w = entries[0]?.contentRect.width ?? 0;
      if (w > 0) setWidth(Math.floor(w));
    });
    ro.observe(el);
    return () => ro.disconnect();
  }, []);

  useEffect(() => {
    if (!svgRef.current || width === 0) return;
    renderChart(svgRef.current, chart, width);
  }, [chart, width]);

  // KPI tiles are HTML, not SVG — render them separately.
  if (chart.chartType === 'kpi') {
    return (
      <div ref={wrapRef} className="w-full overflow-x-auto">
        {chart.title && <h4 className="text-sm font-semibold text-fg-muted mb-2">{chart.title}</h4>}
        <KpiRow chart={chart} />
      </div>
    );
  }

  return (
    <div ref={wrapRef} className="w-full overflow-x-auto">
      {chart.title && <h4 className="text-sm font-semibold text-fg-muted mb-2">{chart.title}</h4>}
      <svg
        ref={svgRef}
        className="block w-full"
        role="img"
        aria-label={chart.title || 'chart'}
      />
      {chart.series.length > 1 && (
        <div className="flex flex-wrap gap-x-4 gap-y-1 mt-2 justify-center">
          {chart.series.map((s, i) => (
            <span key={s.name} className="flex items-center gap-1.5 text-xs text-fg-muted">
              <span
                className="inline-block w-2 h-2 rounded-sm shrink-0"
                style={{ background: `var(${SERIES_VARS[i % SERIES_VARS.length]})` }}
              />
              {s.name}
            </span>
          ))}
        </div>
      )}
    </div>
  );
}

// ── AgentChart — parse + render ───────────────────────────────────────────────

/**
 * Top-level chart component for Markdown fence rendering.
 * Parses the raw fence body and delegates to ChartView.
 * Returns null when the block has no plottable data.
 */
export default function AgentChart({ data }: { data: string }) {
  // Memoize so ChartView only re-renders when the raw string changes.
  const parsed = useMemo(() => parseChart(data), [data]);
  if (!parsed) return null;
  return <ChartView chart={parsed} />;
}

// ── SVG renderers (d3) ────────────────────────────────────────────────────────

function renderChart(svg: SVGSVGElement, chart: ParsedChart, containerWidth: number) {
  const width = Math.max(280, Math.min(containerWidth, 720));
  select(svg).selectAll('*').remove();

  const colors = SERIES_VARS.map((v) => cssVar(svg, v, '#3b82f6'));
  const gridColor = cssVar(svg, '--chart-grid', 'rgba(148,163,184,0.16)');
  const axisColor = cssVar(svg, '--chart-axis', '#5a6f8f');
  const labelColor = cssVar(svg, '--chart-label', '#b0c0d4');
  const valueColor = cssVar(svg, '--chart-value', '#f0f4f8');

  const ctx: RenderCtx = { svg, width, colors, gridColor, axisColor, labelColor, valueColor, unit: chart.unit };

  switch (chart.chartType) {
    case 'line':
    case 'area':
      renderLine(ctx, chart, chart.chartType === 'area');
      break;
    case 'donut':
      renderDonut(ctx, chart);
      break;
    case 'bar':
      renderVerticalBar(ctx, chart);
      break;
    case 'grouped-bar':
    case 'stacked-bar':
      renderMultiBar(ctx, chart, chart.chartType === 'stacked-bar');
      break;
    case 'hbar':
    default:
      renderHorizontalBar(ctx, chart);
      break;
  }
}

interface RenderCtx {
  svg: SVGSVGElement;
  width: number;
  colors: string[];
  gridColor: string;
  axisColor: string;
  labelColor: string;
  valueColor: string;
  unit?: string;
}

/** Horizontal bars — the workhorse for ranked counts with long clinical labels. */
function renderHorizontalBar(ctx: RenderCtx, chart: ParsedChart) {
  const data = chart.series[0].data;
  const { width, colors, valueColor, labelColor } = ctx;
  const barH = 28;
  const gap = 8;
  const margin = { top: 6, right: 56, bottom: 6, left: labelWidth(data) };
  const innerW = width - margin.left - margin.right;
  const height = data.length * (barH + gap) + margin.top + margin.bottom;

  const root = initSvg(ctx, width, height, margin);
  const maxVal = max(data, (d) => d.y) ?? 1;
  const x = scaleLinear().domain([0, maxVal]).range([0, innerW]);
  const y = scaleBand<number>().domain(data.map((_, i) => i)).range([0, data.length * (barH + gap)]).padding(0.18);

  data.forEach((d, i) => {
    const yPos = y(i) ?? 0;
    root.append('rect')
      .attr('x', 0).attr('y', yPos)
      .attr('width', Math.max(0, x(d.y))).attr('height', y.bandwidth())
      .attr('rx', 4).attr('fill', colors[0]);
    root.append('text')
      .attr('x', -10).attr('y', yPos + y.bandwidth() / 2).attr('dy', '0.35em')
      .attr('text-anchor', 'end').attr('font-size', '12px').attr('fill', labelColor)
      .text(truncate(String(d.x), 28));
    root.append('text')
      .attr('x', x(d.y) + 8).attr('y', yPos + y.bandwidth() / 2).attr('dy', '0.35em')
      .attr('font-size', '12px').attr('font-weight', 600).attr('fill', valueColor)
      .text(fmtValue(d.y, ctx.unit));
  });
}

/** Vertical bars — for short categorical labels. */
function renderVerticalBar(ctx: RenderCtx, chart: ParsedChart) {
  const data = chart.series[0].data;
  const { width, colors, valueColor, labelColor, gridColor } = ctx;
  const cats = data.map((d) => String(d.x));

  // Decide label layout (horizontal vs rotated) from the band width so labels
  // never collide, and size the bottom margin to fit them BEFORE drawing.
  const margin = { top: 20, right: 12, bottom: 44, left: 44 };
  const innerWProbe = width - margin.left - margin.right;
  const plan = planXLabels(cats, (innerWProbe / Math.max(1, cats.length)) * 0.78);
  margin.bottom = plan.bottom;

  const height = 220 + margin.bottom;
  const innerW = width - margin.left - margin.right;
  const innerH = height - margin.top - margin.bottom;

  const root = initSvg(ctx, width, height, margin);
  const maxVal = max(data, (d) => d.y) ?? 1;
  const x = scaleBand<number>().domain(data.map((_, i) => i)).range([0, innerW]).padding(0.22);
  const y = scaleLinear().domain([0, maxVal]).range([innerH, 0]).nice();

  gridLines(root, y, innerW, gridColor, ctx.axisColor, ctx.unit);

  data.forEach((d, i) => {
    const xPos = x(i) ?? 0;
    root.append('rect')
      .attr('x', xPos).attr('y', y(d.y))
      .attr('width', x.bandwidth()).attr('height', innerH - y(d.y))
      .attr('rx', 4).attr('fill', colors[0]);
    root.append('text')
      .attr('x', xPos + x.bandwidth() / 2).attr('y', y(d.y) - 6)
      .attr('text-anchor', 'middle').attr('font-size', '11px').attr('font-weight', 600)
      .attr('fill', valueColor).text(fmtValue(d.y, ctx.unit));
  });

  drawXLabels(root, cats, (i) => (x(i) ?? 0) + x.bandwidth() / 2, innerH, labelColor, plan);
}

/** Grouped or stacked bars for multi-series categorical data. */
function renderMultiBar(ctx: RenderCtx, chart: ParsedChart, stacked: boolean) {
  const { width, colors, labelColor, gridColor } = ctx;
  const cats = chart.series[0].data.map((d) => String(d.x));

  // Size the bottom margin for the x labels (rotated when they don't fit).
  const margin = { top: 20, right: 12, bottom: 44, left: 48 };
  const innerWProbe = width - margin.left - margin.right;
  const plan = planXLabels(cats, (innerWProbe / Math.max(1, cats.length)) * 0.9);
  margin.bottom = plan.bottom;

  const height = 236 + margin.bottom;
  const innerW = width - margin.left - margin.right;
  const innerH = height - margin.top - margin.bottom;
  const root = initSvg(ctx, width, height, margin);

  const x0 = scaleBand<string>().domain(cats).range([0, innerW]).padding(0.2);

  let maxVal: number;
  if (stacked) {
    maxVal = max(cats, (_, i) => sum(chart.series, (s) => s.data[i]?.y ?? 0)) ?? 1;
  } else {
    maxVal = max(chart.series, (s) => max(s.data, (d) => d.y) ?? 0) ?? 1;
  }
  const y = scaleLinear().domain([0, maxVal]).range([innerH, 0]).nice();
  gridLines(root, y, innerW, gridColor, ctx.axisColor, ctx.unit);

  if (stacked) {
    cats.forEach((_, i) => {
      let acc = 0;
      chart.series.forEach((s, si) => {
        const v = s.data[i]?.y ?? 0;
        root.append('rect')
          .attr('x', x0(cats[i]) ?? 0).attr('y', y(acc + v))
          .attr('width', x0.bandwidth()).attr('height', Math.max(0, y(acc) - y(acc + v)))
          .attr('fill', colors[si % colors.length]);
        acc += v;
      });
    });
  } else {
    const x1 = scaleBand<string>().domain(chart.series.map((s) => s.name)).range([0, x0.bandwidth()]).padding(0.08);
    cats.forEach((_, i) => {
      chart.series.forEach((s, si) => {
        const v = s.data[i]?.y ?? 0;
        root.append('rect')
          .attr('x', (x0(cats[i]) ?? 0) + (x1(s.name) ?? 0)).attr('y', y(v))
          .attr('width', x1.bandwidth()).attr('height', innerH - y(v))
          .attr('rx', 3).attr('fill', colors[si % colors.length]);
      });
    });
  }

  drawXLabels(root, cats, (i) => (x0(cats[i]) ?? 0) + x0.bandwidth() / 2, innerH, labelColor, plan);
}

/** Line / area for ordered (time) series. */
function renderLine(ctx: RenderCtx, chart: ParsedChart, filled: boolean) {
  const { width, colors, labelColor, gridColor } = ctx;
  const height = 260;
  const margin = { top: 16, right: 16, bottom: 40, left: 48 };
  const innerW = width - margin.left - margin.right;
  const innerH = height - margin.top - margin.bottom;
  const root = initSvg(ctx, width, height, margin);

  const cats = chart.series[0].data.map((d) => String(d.x));
  const x = scalePoint<string>().domain(cats).range([0, innerW]).padding(0.5);
  const maxVal = max(chart.series, (s) => max(s.data, (d) => d.y) ?? 0) ?? 1;
  const y = scaleLinear().domain([0, maxVal]).range([innerH, 0]).nice();
  gridLines(root, y, innerW, gridColor, ctx.axisColor, ctx.unit);

  chart.series.forEach((s, si) => {
    const color = colors[si % colors.length];
    const pts: Array<[string, number]> = s.data.map((d) => [String(d.x), d.y]);
    if (filled && chart.series.length === 1) {
      const areaGen = d3area<[string, number]>()
        .x((d) => x(d[0]) ?? 0).y0(innerH).y1((d) => y(d[1])).curve(curveMonotoneX);
      root.append('path').attr('d', areaGen(pts) ?? '').attr('fill', color).attr('opacity', 0.15);
    }
    const lineGen = d3line<[string, number]>()
      .x((d) => x(d[0]) ?? 0).y((d) => y(d[1])).curve(curveMonotoneX);
    root.append('path').attr('d', lineGen(pts) ?? '')
      .attr('fill', 'none').attr('stroke', color).attr('stroke-width', 2);
    pts.forEach((p) => {
      root.append('circle').attr('cx', x(p[0]) ?? 0).attr('cy', y(p[1])).attr('r', 3).attr('fill', color);
    });
  });

  // X labels — thin out when crowded so they don't overlap.
  const step = Math.ceil(cats.length / Math.max(1, Math.floor(innerW / 60)));
  cats.forEach((c, i) => {
    if (i % step !== 0) return;
    root.append('text')
      .attr('x', x(c) ?? 0).attr('y', innerH + 16)
      .attr('text-anchor', 'middle').attr('font-size', '10px').attr('fill', labelColor)
      .text(truncate(c, 10));
  });
}

/** Donut for a small part-to-whole breakdown (2–6 slices). */
function renderDonut(ctx: RenderCtx, chart: ParsedChart) {
  const data = chart.series[0].data;
  const { width, colors, valueColor, labelColor } = ctx;
  const height = 240;
  const radius = Math.min(width, height) / 2 - 8;
  const svgSel = select(ctx.svg)
    .attr('viewBox', `0 0 ${width} ${height}`)
    .attr('width', '100%')
    .attr('height', height);
  const root = svgSel.append('g').attr('transform', `translate(${width / 2},${height / 2})`);

  const total = sum(data, (d) => d.y) || 1;
  const pieGen = d3pie<ChartPoint>().value((d) => d.y).sort(null);
  const arcGen = d3arc<ReturnType<typeof pieGen>[number]>()
    .innerRadius(radius * 0.58)
    .outerRadius(radius);

  pieGen(data).forEach((slice, i) => {
    root.append('path').attr('d', arcGen(slice) ?? '').attr('fill', colors[i % colors.length]);
    const pct = (slice.data.y / total) * 100;
    if (pct >= 6) {
      const [cx, cy] = arcGen.centroid(slice);
      root.append('text')
        .attr('x', cx).attr('y', cy).attr('text-anchor', 'middle').attr('dy', '0.35em')
        .attr('font-size', '11px').attr('font-weight', 600).attr('fill', valueColor)
        .text(`${Math.round(pct)}%`);
    }
  });

  // Center total.
  root.append('text').attr('text-anchor', 'middle').attr('dy', '-0.1em')
    .attr('font-size', '18px').attr('font-weight', 700).attr('fill', valueColor).text(fmtInt(total));
  root.append('text').attr('text-anchor', 'middle').attr('dy', '1.2em')
    .attr('font-size', '10px').attr('fill', labelColor).text(chart.unit || 'total');
}

// ── SVG helpers ───────────────────────────────────────────────────────────────

// A d3 <g> selection, used as the drawing root for every SVG chart.
type GSelection = Selection<SVGGElement, unknown, null, undefined>;

function initSvg(
  ctx: RenderCtx,
  width: number,
  height: number,
  margin: { top: number; right: number; bottom: number; left: number },
): GSelection {
  return select(ctx.svg)
    .attr('viewBox', `0 0 ${width} ${height}`)
    .attr('width', '100%').attr('height', height)
    .append('g').attr('transform', `translate(${margin.left},${margin.top})`);
}

/** Horizontal gridlines + y-axis value labels. */
function gridLines(
  root: GSelection,
  y: ReturnType<typeof scaleLinear<number, number>>,
  innerW: number,
  gridColor: string,
  axisColor: string,
  unit?: string,
) {
  const ticks = y.ticks(4);
  ticks.forEach((t) => {
    root.append('line')
      .attr('x1', 0).attr('x2', innerW).attr('y1', y(t)).attr('y2', y(t))
      .attr('stroke', gridColor).attr('stroke-width', 1);
    root.append('text')
      .attr('x', -8).attr('y', y(t)).attr('dy', '0.32em').attr('text-anchor', 'end')
      .attr('font-size', '10px').attr('fill', axisColor)
      .text(unit === '%' ? `${t}%` : fmtInt(t));
  });
}

function truncate(s: string, n: number): string {
  return s.length <= n ? s : `${s.slice(0, n - 1)}…`;
}

/** Left margin wide enough for the longest (truncated) label in a horizontal bar. */
function labelWidth(data: ChartPoint[]): number {
  const longest = data.reduce((m, d) => Math.max(m, truncate(String(d.x), 28).length), 0);
  return Math.min(220, Math.max(70, longest * 6.5 + 16));
}

// Rough px width of a label at 11px (avg glyph ≈ 6px). Used to decide whether
// x-axis labels fit horizontally under their band or must be rotated.
const CHAR_PX = 6;

/**
 * Plan x-axis category labels for a vertical/grouped/stacked bar chart. When the
 * widest label fits inside a band it stays horizontal; otherwise labels are
 * rotated -35° so adjacent labels never collide. Returns whether to rotate,
 * the truncated max-chars cap, and the bottom margin the labels need — callers
 * must size the chart with this margin BEFORE drawing so nothing is clipped.
 */
function planXLabels(
  cats: string[],
  bandWidth: number,
): { rotate: boolean; maxChars: number; bottom: number } {
  const maxChars = 16;
  const truncatedLongest = cats.reduce((m, c) => Math.max(m, Math.min(c.length, maxChars)), 0);
  const longestPx = truncatedLongest * CHAR_PX;
  const rotate = longestPx > bandWidth;
  const bottom = rotate
    ? Math.min(90, Math.max(44, Math.round(longestPx * 0.6) + 20))
    : 32;
  return { rotate, maxChars, bottom };
}

/** Draw the x-axis category labels (horizontal or rotated) under a bar chart. */
function drawXLabels(
  root: GSelection,
  cats: string[],
  centerX: (i: number) => number,
  innerH: number,
  color: string,
  plan: { rotate: boolean; maxChars: number },
) {
  cats.forEach((c, i) => {
    const label = truncate(c, plan.maxChars);
    const cx = centerX(i);
    const t = root.append('text')
      .attr('font-size', '11px').attr('fill', color).text(label);
    if (plan.rotate) {
      t.attr('x', cx).attr('y', innerH + 14)
        .attr('text-anchor', 'end')
        .attr('transform', `rotate(-35, ${cx}, ${innerH + 14})`);
    } else {
      t.attr('x', cx).attr('y', innerH + 16).attr('text-anchor', 'middle');
    }
  });
}
