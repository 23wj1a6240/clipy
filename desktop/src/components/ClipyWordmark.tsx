/** The canonical Clipy mark + wordmark, kept byte-for-byte in step with the
 *  web app's `components/brand/ClipyMark.tsx` / `ClipyWordmark.tsx`. The
 *  desktop app is a separate Vite bundle so it cannot import them; if the
 *  brand changes, change it in both places. */

interface MarkProps {
  size?: number;
  className?: string;
}

export function ClipyMark({ size = 40, className }: MarkProps) {
  return (
    <svg
      width={size}
      height={size}
      viewBox="0 0 64 64"
      fill="none"
      className={className}
      aria-label="Clipy"
    >
      <defs>
        <linearGradient
          id="clipyMarkGrad"
          x1="20"
          y1="18"
          x2="45"
          y2="49"
          gradientUnits="userSpaceOnUse"
        >
          <stop offset="0" stopColor="#FF6A3D" />
          <stop offset="0.55" stopColor="#FF4D29" />
          <stop offset="1" stopColor="#FF2778" />
        </linearGradient>
      </defs>
      <g transform="translate(32 31.5) rotate(-18) scale(1.38) translate(-16.5 -20)">
        <path d="M6 4 L6 32 L13 25 L18 36 L22 34.5 L18 23 L27 23 Z" fill="url(#clipyMarkGrad)" />
      </g>
      <circle cx="48.3" cy="48.3" r="4.5" fill="#FF4D29" />
    </svg>
  );
}

interface WordmarkProps {
  size?: number;
  className?: string;
  showMark?: boolean;
}

export function ClipyWordmark({ size = 40, className = "", showMark = true }: WordmarkProps) {
  return (
    <span
      className={`inline-flex items-center ${className}`}
      style={{ gap: Math.round(size * 0.08) }}
    >
      {showMark && <ClipyMark size={size} />}
      <span
        className="font-extrabold text-current"
        style={{
          fontSize: Math.round(size * 0.82),
          lineHeight: 1,
          letterSpacing: "-0.045em",
        }}
      >
        <span className="text-flash-500">c</span>lipy
      </span>
    </span>
  );
}
