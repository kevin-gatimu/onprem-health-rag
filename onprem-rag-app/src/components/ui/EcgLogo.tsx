/**
 * EcgLogo — animated ECG / cardiac-monitor waveform icon.
 *
 * Two identical heartbeat cycles sit side-by-side in a 56-unit-wide path.
 * A CSS translateX animation (ecg-scroll-logo, defined in theme.css) scrolls
 * the path one cycle (28 units) left in a loop — giving the illusion of a
 * live, scrolling cardiac monitor.
 *
 * A linear-gradient mask fades the left (old) trace to transparent and keeps
 * the right (new) trace fully opaque — exactly like a real bedside monitor.
 *
 * useId() is used for gradient/mask ids so a page rendering EcgLogo twice
 * won't collide on the same "#ecg-fade" attribute value.
 */
import { useId } from 'react';

// One heartbeat cycle spanning 28px (matches the 28×28 viewBox slot):
// flat → P wave → QRS complex (R spike + S trough) → T wave → flat rest
const CYCLE =
  'L 3,14 L 4,12.5 L 5,11 L 6,12.5 L 7,14 ' +        // P wave
  'L 8,15.5 L 9.5,4 L 11,24 L 12,14 ' +               // QRS: Q dip / R spike / S trough
  'L 14,14 L 14.5,12.5 L 16,9.5 L 17.5,12.5 L 18.5,14 ' + // T wave
  'L 28,14 ';                                           // flat rest

// Full path: two cycles so the scroll loop is seamless (shift cycle 2 by +28)
const PATH =
  `M 0,14 ${CYCLE}` +
  `M 28,14 ${CYCLE.replace(/(\d+(?:\.\d+)?),/g, (_, n) => `${+n + 28},`)}`;

export interface EcgLogoProps {
  size?: number;
  className?: string;
}

export function EcgLogo({ size = 28, className = '' }: EcgLogoProps) {
  // Unique ids prevent SVG defs conflicts when two EcgLogos are on screen at once.
  const uid = useId();
  const gradId   = `ecg-fade-${uid}`;
  const maskId   = `ecg-fade-mask-${uid}`;

  return (
    <svg
      width={size}
      height={size}
      viewBox="0 0 28 28"
      fill="none"
      overflow="hidden"
      className={className}
      aria-hidden="true"
    >
      <defs>
        {/* Fixed gradient mask — left edge transparent, right edge fully opaque */}
        <linearGradient id={gradId} x1="0" y1="0" x2="1" y2="0">
          <stop offset="0%"   stopColor="white" stopOpacity="0" />
          <stop offset="40%"  stopColor="white" stopOpacity="0.4" />
          <stop offset="100%" stopColor="white" stopOpacity="1" />
        </linearGradient>
        <mask id={maskId}>
          <rect x="0" y="0" width="28" height="28" fill={`url(#${gradId})`} />
        </mask>
      </defs>

      {/* Scrolling ECG trace — the keyframe ecg-scroll-logo is in theme.css */}
      <g
        mask={`url(#${maskId})`}
        className="animate-[ecg-scroll-logo_1.4s_linear_infinite]"
        style={{ filter: 'drop-shadow(0 0 2px currentColor)' }}
      >
        <path
          d={PATH}
          stroke="currentColor"
          strokeWidth="1.6"
          strokeLinecap="round"
          strokeLinejoin="round"
        />
      </g>
    </svg>
  );
}
