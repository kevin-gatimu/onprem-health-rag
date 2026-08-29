/**
 * LoginBackground — animated ECG paper + 3 scrolling traces for the login
 * brand panel.  Fills its absolutely-positioned parent; pointer-events: none.
 *
 * Uses d3-shape's line generator (submodule import, not the barrel, so Vite
 * tree-shakes to just the line generator rather than bundling all of d3).
 *
 * Three traces run at slightly different speeds to prevent a "marching" look:
 *   primary   — high opacity, normal speed
 *   secondary — medium opacity, slower speed
 *   tertiary  — low opacity, fastest speed
 *
 * The keyframe ecg-scroll-bg (translateX 0 → -120px) lives in theme.css so
 * Tailwind picks it up without a module.scss.  Each trace <g> applies it via
 * inline style + CSS custom properties --ecg-dur / --ecg-delay.
 */
import type { CSSProperties } from 'react';
// Submodule import — tree-shakes to just the line generator (~3 KB vs 930 KB).
import { line, curveLinear } from 'd3-shape';

// ── ECG path configuration ────────────────────────────────────────────────

const CYCLE_W  = 120;  // px per heartbeat cycle (28 normalised units → 120 px)
const N_CYCLES = 26;   // 26 × 120 = 3 120 px — covers any panel width + overshoot

// Waveform amplitudes in px (upward = negative Y in SVG coords)
const R = 24;  // R spike — most dramatic feature
const S = 20;  // S trough depth
const P = 4;   // P wave height
const T = 9;   // T wave height
const Q = 3;   // Q dip depth

function buildPoints(): [number, number][] {
  const u = CYCLE_W / 28;  // 1 normalised unit → u px
  const pts: [number, number][] = [];

  for (let i = 0; i < N_CYCLES; i++) {
    const o = i * CYCLE_W;
    pts.push(
      [o,              0],           // flat start
      [o + 3   * u,    0],           // pre-P baseline
      [o + 4   * u,   -P * 0.4],    // P wave rising
      [o + 5   * u,   -P],          // P wave peak
      [o + 6   * u,   -P * 0.4],    // P wave falling
      [o + 7   * u,    0],           // PR segment
      [o + 8   * u,    Q],           // Q dip
      [o + 9.5 * u,   -R],           // R spike
      [o + 11  * u,    S],           // S trough
      [o + 12  * u,    0],           // back to baseline
      [o + 14  * u,    0],           // ST segment
      [o + 14.5 * u,  -T * 0.25],   // T wave rising
      [o + 16  * u,   -T],           // T wave peak
      [o + 17.5 * u,  -T * 0.25],   // T wave falling
      [o + 18.5 * u,   0],           // T end
      [o + 28  * u,    0],           // flat rest
    );
  }
  return pts;
}

// Build the path string once at module load — stable reference, no re-render cost.
const ECG_PATH = (() => {
  const ecgLine = line<[number, number]>()
    .x((d) => d[0])
    .y((d) => d[1])
    .curve(curveLinear);
  return ecgLine(buildPoints()) ?? '';
})();

// ── Trace layout ─────────────────────────────────────────────────────────
// y positions are fixed SVG-px from the top; deterministic across window sizes.
const TRACES = [
  { y: 180, opacity: 0.70, dur: 2.00, delay:  0.00, strokeWidth: 1.7 }, // primary
  { y: 390, opacity: 0.32, dur: 2.40, delay: -0.75, strokeWidth: 1.4 }, // secondary
  { y: 580, opacity: 0.15, dur: 1.70, delay: -1.30, strokeWidth: 1.2 }, // tertiary
] as const;

// ── Component ─────────────────────────────────────────────────────────────

export default function LoginBackground() {
  return (
    <svg
      className="absolute inset-0 w-full h-full overflow-hidden pointer-events-none"
      xmlns="http://www.w3.org/2000/svg"
      aria-hidden="true"
    >
      <defs>
        {/* ── ECG paper grid ── */}

        {/* Minor squares: one every 15 px */}
        <pattern id="ecg-minor" width="15" height="15" patternUnits="userSpaceOnUse">
          <path
            d="M 15 0 L 0 0 L 0 15"
            fill="none"
            stroke="rgba(59,130,246,0.055)"
            strokeWidth="0.5"
          />
        </pattern>

        {/* Major squares: one every 75 px (5 × minor) */}
        <pattern id="ecg-major" width="75" height="75" patternUnits="userSpaceOnUse">
          <rect width="75" height="75" fill="url(#ecg-minor)" />
          <path
            d="M 75 0 L 0 0 L 0 75"
            fill="none"
            stroke="rgba(59,130,246,0.10)"
            strokeWidth="0.9"
          />
        </pattern>

        {/* ── Phosphor glow on traces (two-pass: blur + original) ── */}
        <filter id="ecg-glow" x="-20%" y="-300%" width="140%" height="700%">
          <feGaussianBlur in="SourceGraphic" stdDeviation="2.5" result="blur" />
          <feMerge>
            <feMergeNode in="blur" />
            <feMergeNode in="SourceGraphic" />
          </feMerge>
        </filter>

        {/* ── Left-edge fade mask (oldest trace → transparent) ── */}
        <linearGradient id="ecg-fade" x1="0" y1="0" x2="1" y2="0">
          <stop offset="0%"   stopColor="white" stopOpacity="0"    />
          <stop offset="25%"  stopColor="white" stopOpacity="0.4"  />
          <stop offset="55%"  stopColor="white" stopOpacity="0.85" />
          <stop offset="100%" stopColor="white" stopOpacity="1"    />
        </linearGradient>
        <mask id="ecg-fade-mask">
          <rect width="100%" height="100%" fill="url(#ecg-fade)" />
        </mask>
      </defs>

      {/* ECG paper grid background */}
      <rect width="100%" height="100%" fill="url(#ecg-major)" />

      {/* Scrolling traces — outer <g> positions vertically, inner <g> animates */}
      <g mask="url(#ecg-fade-mask)">
        {TRACES.map((t, i) => (
          // Outer g: static vertical position
          // Inner g: CSS scroll animation driven by --ecg-dur / --ecg-delay
          <g key={i} transform={`translate(0, ${t.y})`} opacity={t.opacity}>
            <g
              style={{
                '--ecg-dur':   `${t.dur}s`,
                '--ecg-delay': `${t.delay}s`,
                // The keyframe ecg-scroll-bg is declared in theme.css.
                animation: 'ecg-scroll-bg var(--ecg-dur) linear infinite',
                animationDelay: 'var(--ecg-delay)',
                willChange: 'transform',
              } as CSSProperties}
            >
              <path
                d={ECG_PATH}
                stroke="rgb(59,130,246)"
                strokeWidth={t.strokeWidth}
                fill="none"
                strokeLinecap="round"
                strokeLinejoin="round"
                filter="url(#ecg-glow)"
              />
            </g>
          </g>
        ))}
      </g>
    </svg>
  );
}
