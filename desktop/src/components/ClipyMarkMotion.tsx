type ClipyMarkMotionProps = {
  className?: string;
};

export default function ClipyMarkMotion({ className = "" }: ClipyMarkMotionProps) {
  return (
    <div className={`clipy-mark-motion ${className}`} aria-hidden>
      <span className="clipy-mark-glow" />
      <svg
        className="clipy-mark-motion-svg"
        viewBox="0 0 64 64"
        fill="none"
        focusable="false"
      >
        <defs>
          <linearGradient
            id="clipyDesktopMarkGrad"
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
        <g className="clipy-mark-cursor" transform="translate(32 31.5) rotate(-18) scale(1.38) translate(-16.5 -20)">
          <path
            d="M6 4 L6 32 L13 25 L18 36 L22 34.5 L18 23 L27 23 Z"
            fill="url(#clipyDesktopMarkGrad)"
          />
        </g>
        <circle className="clipy-mark-dot" cx="48.3" cy="48.3" r="4.5" fill="#FF4D29" />
      </svg>
      <span className="clipy-mark-scan clipy-mark-scan-a" />
      <span className="clipy-mark-scan clipy-mark-scan-b" />
    </div>
  );
}
