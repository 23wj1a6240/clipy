/** @type {import('tailwindcss').Config} */
// Mirrors Clipy's "Flash" design system.
// Keep semantic token names aligned with the web app so a designer reviewing
// either side reads the same vocabulary (ink/flash/stone/spark/link).
// A recorder sheet colour: RGB channels from `--rc-<name>`, times an optional
// `--rc-<name>-a` alpha. The channels keep opacity modifiers working
// (`bg-surface/60`); the alpha lets glass make a token see-through (rows on
// frosted glass) without touching any component.
const sheet = (name) => `rgb(var(--rc-${name}) / calc(<alpha-value> * var(--rc-${name}-a, 1)))`;

export default {
  content: [
    "./index.html",
    "./src/**/*.{ts,tsx}",
  ],
  theme: {
    extend: {
      fontFamily: {
        // Gabarito is the single brand face (display + UI, design-system
        // §4). Preflight applies this to <html>, so every view inherits.
        sans: [
          '"Gabarito Variable"',
          "ui-sans-serif",
          "system-ui",
          "-apple-system",
          "sans-serif",
        ],
      },
      colors: {
        flash: {
          50: "#FFF1EC",
          400: "#FF7352",
          500: "#FF4D29",
          600: "#E63C1C",
        },
        spark: {
          50: "#FFF6E0",
          400: "#FFC44D",
          500: "#FFB020",
        },
        link: {
          500: "#00C2FF",
        },
        ink: {
          600: "#363D4E",
          700: "#252B3A",
          800: "#1A1F2D",
          900: "#11151F",
          950: "#0A0D16",
        },
        stone: {
          100: "#EEF0F5",
          300: "#C3C8D4",
          400: "#9CA3B5",
          500: "#7B8192",
        },
        // Recorder sheet (Loom-shaped panel). Mirrors the web app's light
        // tokens in styles/globals.css. Each one is a CSS variable, set in
        // src/styles.css, so the popover can change look per recording mode:
        // light for Cloud, dark for Local AI (`data-recorder-look`). Every
        // other window keeps the light values on :root.
        canvas: sheet("canvas"),
        surface: sheet("surface"),
        raised: sheet("raised"),
        subtle: sheet("subtle"),
        strong: sheet("strong"),
        row: sheet("row"),
        "row-hover": sheet("row-hover"),
        note: "#fff5b8",
        "note-deep": "#f5df6b",
        "ink-text": sheet("ink-text"),
        "ink-secondary": sheet("ink-secondary"),
        "ink-muted": sheet("ink-muted"),
        // Text links and the selected-item tint in menus. Blue on the light
        // sheet, a lighter blue on the dark one.
        "sheet-link": sheet("link"),
        "sheet-link-soft": sheet("link-soft"),
        // The sheet's own background, for rings and gaps that must match it.
        "sheet-bg": sheet("sheet"),
        // Status colours: text, a soft fill and a border for each tone. Dark
        // text on a pale tint by day, light text on a deep tint by night.
        "status-ok": sheet("ok"),
        "status-ok-soft": sheet("ok-soft"),
        "status-ok-line": sheet("ok-line"),
        "status-warn": sheet("warn"),
        "status-warn-soft": sheet("warn-soft"),
        "status-warn-line": sheet("warn-line"),
        "status-error": sheet("error"),
        "status-error-soft": sheet("error-soft"),
        "status-error-line": sheet("error-line"),
      },
      borderRadius: {
        sm: "6px",
        md: "10px",
        lg: "14px",
        xl: "20px",
        "2xl": "28px",
      },
      boxShadow: {
        glow: "0 0 0 4px rgba(255,77,41,0.18), 0 8px 32px rgba(255,77,41,0.28)",
        pop: "0 10px 30px -10px rgba(10,13,22,0.3), 0 0 0 1px rgba(10,13,22,0.06)",
        soft: "0 6px 20px rgba(10,13,22,0.16)",
        modal: "0 24px 60px rgba(10,13,22,0.45)",
      },
      backgroundImage: {
        "gradient-flash":
          "linear-gradient(135deg, #FF4D29 0%, #FF2778 55%, #9B1EFF 100%)",
      },
      keyframes: {
        "slide-in": {
          from: { opacity: "0", transform: "translateX(24px)" },
          to: { opacity: "1", transform: "translateX(0)" },
        },
        "slide-back": {
          from: { opacity: "0", transform: "translateX(-24px)" },
          to: { opacity: "1", transform: "translateX(0)" },
        },
        "pop-in": {
          from: { opacity: "0", transform: "translateY(-6px) scale(0.98)" },
          to: { opacity: "1", transform: "translateY(0) scale(1)" },
        },
        "fade-up": {
          from: { opacity: "0", transform: "translateY(6px)" },
          to: { opacity: "1", transform: "translateY(0)" },
        },
        "record-pulse": {
          "0%, 100%": { opacity: "1", transform: "scale(1)" },
          "50%": { opacity: "0.7", transform: "scale(0.92)" },
        },
      },
      animation: {
        "record-pulse": "record-pulse 1.4s ease-in-out infinite",
        "slide-in": "slide-in 200ms cubic-bezier(0.2,0.8,0.2,1) both",
        "slide-back": "slide-back 200ms cubic-bezier(0.2,0.8,0.2,1) both",
        "pop-in": "pop-in 140ms ease-out both",
        "fade-up": "fade-up 220ms ease-out both",
      },
    },
  },
  plugins: [],
};
