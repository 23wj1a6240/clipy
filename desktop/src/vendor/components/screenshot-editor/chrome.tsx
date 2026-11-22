// SPDX-License-Identifier: Apache-2.0
// Copied from the Clipy monorepo (components/screenshot-editor/chrome.tsx) by the export; the desktop app shares it.
"use client";

// Shared editor chrome: the per-tool options bar, the background panel and the
// background preview. Rendered by the web editor and by the desktop
// screenshot window, which pass their own class names (`EditorChromeTheme`)
// because the two apps have different design tokens.
//
// Relative imports and no icon library on purpose: the desktop app bundles
// this file from outside the Next project.

import { useEffect, useState, type CSSProperties, type ReactNode } from "react";
import {
  BACKDROP_ASPECTS,
  BACKDROP_FRAMES,
  BACKDROP_LIMITS,
  DEFAULT_BACKDROP,
  GRADIENT_PRESETS,
  MESH_PRESETS,
  SOLID_BACKDROPS,
  backdropIsNoop,
  gradientCss,
  gradientPreset,
  meshCss,
  meshPreset,
  type AutoColors,
} from "../../lib/screenshots/backdrop";
import { parseBackdrop } from "../../lib/screenshots/scene";
import { backdropCardColor } from "../../lib/screenshots/render";
import type { Backdrop, FillStyle, RedactStyle, Shape } from "../../lib/screenshots/types";
import {
  EMOJI_CHOICES,
  MAGNIFY_ZOOMS,
  optionsFor,
  type EditorTool,
  type ToolOptions,
} from "./tools";
import type { BackdropPreview } from "./useAnnotationEditor";

export interface EditorChromeTheme {
  /** A small segmented/toggle button. */
  chip: string;
  /** Added to `chip` when it is the active choice. */
  chipActive: string;
  /** Row label text. */
  label: string;
  /** Floating panel surface. */
  panel: string;
  /** Range and select inputs. */
  input: string;
  divider: string;
}

function Chip({
  theme,
  active,
  onClick,
  title,
  children,
  label,
  disabled,
}: {
  theme: EditorChromeTheme;
  active?: boolean;
  onClick: () => void;
  title?: string;
  label?: string;
  disabled?: boolean;
  children: ReactNode;
}) {
  return (
    <button
      type="button"
      onClick={onClick}
      title={title}
      aria-label={label}
      aria-pressed={active}
      disabled={disabled}
      className={`${theme.chip} ${active ? theme.chipActive : ""}`}
    >
      {children}
    </button>
  );
}

const FILL_CHOICES: { value: FillStyle; label: string }[] = [
  { value: "outline", label: "Outline" },
  { value: "tint", label: "Tint" },
  { value: "solid", label: "Solid" },
];

const REDACT_CHOICES: { value: RedactStyle; label: string }[] = [
  { value: "pixelate", label: "Pixelate" },
  { value: "blur", label: "Blur" },
  { value: "solid", label: "Solid bar" },
];

interface ToolOptionsBarProps {
  theme: EditorChromeTheme;
  tool: EditorTool;
  selected: Shape | null;
  options: ToolOptions;
  onOption: <K extends keyof ToolOptions>(key: K, value: ToolOptions[K]) => void;
  onDuplicate: () => void;
  onBringToFront: () => void;
  onSendToBack: () => void;
  onDelete: () => void;
  disabled?: boolean;
}

/**
 * The options that belong to the current tool, or to the selected shape when
 * the Select tool is active. With none, an empty row of the same height stays
 * so the canvas below does not move when you switch tools: it did, and the
 * first drag after a switch landed where the image had been.
 */
export function ToolOptionsBar({
  theme,
  tool,
  selected,
  options,
  onOption,
  onDuplicate,
  onBringToFront,
  onSendToBack,
  onDelete,
  disabled,
}: ToolOptionsBarProps) {
  const subject: EditorTool | null = tool === "select" ? selected?.kind ?? null : tool;
  const keys = subject ? optionsFor(subject) : [];
  const showSelection = tool === "select" && selected !== null;
  if (keys.length === 0 && !showSelection) {
    return (
      <div className="flex items-center" aria-hidden="true" data-testid="tool-options-placeholder">
        <span className={`${theme.chip} invisible`}>&nbsp;</span>
      </div>
    );
  }

  return (
    <div className="flex flex-wrap items-center gap-x-3 gap-y-2" role="group" aria-label="Tool options">
      {keys.includes("fill") && (
        <Segment theme={theme} label="Fill">
          {FILL_CHOICES.map((choice) => (
            <Chip
              key={choice.value}
              theme={theme}
              active={options.fill === choice.value}
              onClick={() => onOption("fill", choice.value)}
              disabled={disabled}
            >
              {choice.label}
            </Chip>
          ))}
        </Segment>
      )}
      {keys.includes("heads") && (
        <Segment theme={theme} label="Heads">
          <Chip theme={theme} active={options.heads === "end"} onClick={() => onOption("heads", "end")} disabled={disabled}>
            One end
          </Chip>
          <Chip theme={theme} active={options.heads === "both"} onClick={() => onOption("heads", "both")} disabled={disabled}>
            Both ends
          </Chip>
        </Segment>
      )}
      {keys.includes("dashed") && (
        <Segment theme={theme} label="Line">
          <Chip
            theme={theme}
            label="Solid line"
            active={!options.dashed}
            onClick={() => onOption("dashed", false)}
            disabled={disabled}
          >
            Solid
          </Chip>
          <Chip
            theme={theme}
            label="Dashed line"
            active={options.dashed}
            onClick={() => onOption("dashed", true)}
            disabled={disabled}
          >
            Dashed
          </Chip>
        </Segment>
      )}
      {keys.includes("redact") && (
        <Segment theme={theme} label="Hide with">
          {REDACT_CHOICES.map((choice) => (
            <Chip
              key={choice.value}
              theme={theme}
              active={options.redact === choice.value}
              onClick={() => onOption("redact", choice.value)}
              disabled={disabled}
            >
              {choice.label}
            </Chip>
          ))}
        </Segment>
      )}
      {keys.includes("zoom") && (
        <Segment theme={theme} label="Zoom">
          {MAGNIFY_ZOOMS.map((zoom) => (
            <Chip key={zoom} theme={theme} active={options.zoom === zoom} onClick={() => onOption("zoom", zoom)} disabled={disabled}>
              {zoom}×
            </Chip>
          ))}
        </Segment>
      )}
      {keys.includes("textBackground") && (
        <Segment theme={theme} label="Style">
          <Chip
            theme={theme}
            active={!options.textBackground}
            onClick={() => onOption("textBackground", false)}
            disabled={disabled}
          >
            Outlined
          </Chip>
          <Chip
            theme={theme}
            active={options.textBackground}
            onClick={() => onOption("textBackground", true)}
            disabled={disabled}
          >
            Label
          </Chip>
        </Segment>
      )}
      {keys.includes("emoji") && (
        <div className="flex flex-wrap items-center gap-0.5" role="group" aria-label="Emoji">
          {EMOJI_CHOICES.map((emoji) => (
            <Chip
              key={emoji}
              theme={theme}
              label={`Emoji ${emoji}`}
              active={options.emoji === emoji}
              onClick={() => onOption("emoji", emoji)}
              disabled={disabled}
            >
              <span className="text-[17px] leading-none">{emoji}</span>
            </Chip>
          ))}
        </div>
      )}
      {showSelection && (
        <Segment theme={theme} label="Selection">
          <Chip theme={theme} onClick={onDuplicate} title="Duplicate (⌘D)" disabled={disabled}>
            Duplicate
          </Chip>
          <Chip theme={theme} onClick={onBringToFront} title="Bring to front (])" disabled={disabled}>
            To front
          </Chip>
          <Chip theme={theme} onClick={onSendToBack} title="Send to back ([)" disabled={disabled}>
            To back
          </Chip>
          <Chip theme={theme} onClick={onDelete} label="Delete annotation" title="Delete annotation (⌫)" disabled={disabled}>
            Remove
          </Chip>
        </Segment>
      )}
    </div>
  );
}

function Segment({ theme, label, children }: { theme: EditorChromeTheme; label: string; children: ReactNode }) {
  return (
    <div className="flex items-center gap-1" role="group" aria-label={label}>
      <span className={theme.label}>{label}</span>
      {children}
    </div>
  );
}

/** Width the Background panel takes, matching its `w-[min(340px,...)]`. */
const PANEL_WIDTH = 340;

/**
 * Where the Background panel goes, as a left offset in CSS pixels from its
 * button: under the button when it fits, otherwise shifted just enough to
 * stay 16px inside the window on both sides, whether the toolbar puts the
 * button on the left, the right or the middle of a narrow window.
 */
export function panelOffset(anchor: HTMLElement | null): number {
  if (!anchor || typeof window === "undefined") return 0;
  const margin = 16;
  const rect = anchor.getBoundingClientRect();
  const width = Math.min(PANEL_WIDTH, window.innerWidth - margin * 2);
  let left = rect.left;
  if (left + width > window.innerWidth - margin) left = window.innerWidth - margin - width;
  if (left < margin) left = margin;
  return Math.round(left - rect.left);
}

/**
 * CSS background for a backdrop fill, for swatches and the preview. `blur`
 * returns a neutral stand-in: the blurred screenshot is drawn by an element
 * (`BlurredImage`), since CSS cannot blur a background image on its own.
 */
export function backdropFillCss(fill: string, auto: AutoColors | null): string {
  if (fill === "none") {
    return "repeating-conic-gradient(#d4d4d8 0% 25%, #ffffff 0% 50%) 50% / 16px 16px";
  }
  if (fill === "auto" || fill === "blur") {
    const colors = auto ?? { from: "#c7d2fe", to: "#4338ca" };
    return `linear-gradient(135deg, ${colors.from}, ${colors.to})`;
  }
  const mesh = meshPreset(fill);
  if (mesh) return meshCss(mesh);
  const preset = gradientPreset(fill);
  if (preset) return gradientCss(preset);
  // Only a validated hex reaches here; anything else falls back to white.
  return /^#[0-9a-f]{3}(?:[0-9a-f]{3})?$/i.test(fill) ? fill : "#ffffff";
}

/** Film grain as an SVG noise tile, the preview's stand-in for the export's seeded grain. */
export function grainCss(amount: number): string {
  const svg =
    `<svg xmlns='http://www.w3.org/2000/svg' width='160' height='160'>` +
    `<filter id='n'><feTurbulence type='fractalNoise' baseFrequency='0.85' numOctaves='2' stitchTiles='stitch'/>` +
    `<feColorMatrix values='0 0 0 0 0  0 0 0 0 0  0 0 0 0 0  0 0 0 ${(amount * 0.55).toFixed(3)} 0'/></filter>` +
    `<rect width='100%' height='100%' filter='url(#n)'/></svg>`;
  return `url("data:image/svg+xml,${encodeURIComponent(svg)}")`;
}

/** The screenshot, scaled to cover and blurred, for the `blur` background. */
function BlurredImage({ imageUrl, blur }: { imageUrl: string; blur: string }) {
  return (
    <span
      aria-hidden
      style={{
        position: "absolute",
        inset: "-15%",
        backgroundImage: `url("${imageUrl.replace(/["\\\n]/g, "")}")`,
        backgroundSize: "cover",
        backgroundPosition: "center",
        filter: `blur(${blur})`,
        pointerEvents: "none",
      }}
    />
  );
}

const PRESETS_KEY = "clipy:screenshot:backdropPresets";
const MAX_SAVED_PRESETS = 6;

function readSavedPresets(): Backdrop[] {
  try {
    const raw = window.localStorage.getItem(PRESETS_KEY);
    const parsed = raw ? (JSON.parse(raw) as unknown) : [];
    if (!Array.isArray(parsed)) return [];
    // Storage is editable by anything on the page's origin, so a saved look
    // goes through the same validation as a scene from the server.
    return parsed
      .map((entry) => parseBackdrop(entry))
      .filter((entry): entry is Backdrop => entry !== null)
      .slice(0, MAX_SAVED_PRESETS);
  } catch {
    return [];
  }
}

function writeSavedPresets(presets: Backdrop[]): void {
  try {
    window.localStorage.setItem(PRESETS_KEY, JSON.stringify(presets));
  } catch {
    // Private windows and blocked storage: presets just do not persist.
  }
}

interface BackdropPanelProps {
  theme: EditorChromeTheme;
  backdrop: Backdrop | null;
  autoColors: AutoColors | null;
  onChange: (backdrop: Backdrop | null) => void;
  disabled?: boolean;
  /** The screenshot, for the blurred-screenshot swatch. */
  imageUrl?: string;
}

const INSET_SWATCHES = ["#ffffff", "#f4f4f5", "#e4e4e7", "#27272a", "#000000"] as const;

/**
 * Background, padding, corners, shadow, inset, aspect ratio and window frame,
 * plus saved looks. Every change is live in the preview and lands in the
 * exported image.
 */
export function BackdropPanel({ theme, backdrop, autoColors, onChange, disabled, imageUrl }: BackdropPanelProps) {
  const [saved, setSaved] = useState<Backdrop[]>([]);
  useEffect(() => setSaved(readSavedPresets()), []);
  const current = backdrop ?? DEFAULT_BACKDROP;
  const update = (patch: Partial<Backdrop>) => onChange({ ...current, ...patch });

  const fills = [
    "none",
    "auto",
    "blur",
    ...GRADIENT_PRESETS.map((preset) => `g:${preset.id}`),
    ...MESH_PRESETS.map((preset) => `m:${preset.id}`),
    ...SOLID_BACKDROPS,
  ];
  const fillLabel = (fill: string) =>
    fill === "none"
      ? "Transparent"
      : fill === "auto"
        ? "Colors from the screenshot"
        : fill === "blur"
          ? "Blurred screenshot"
          : gradientPreset(fill)?.label ?? meshPreset(fill)?.label ?? fill;

  const slider = (key: "padding" | "radius" | "shadow" | "inset" | "grain", label: string) => (
    <label className="flex items-center gap-2">
      <span className={`w-16 shrink-0 ${theme.label}`}>{label}</span>
      <input
        type="range"
        min={0}
        max={BACKDROP_LIMITS[key]}
        step={BACKDROP_LIMITS[key] / 50}
        value={current[key] ?? 0}
        disabled={disabled || !backdrop}
        onChange={(event) => update({ [key]: Number(event.target.value) })}
        aria-label={label}
        className={`min-w-0 flex-1 ${theme.input}`}
      />
    </label>
  );

  return (
    <div className={`flex w-[min(340px,calc(100vw-2rem))] flex-col gap-3 p-3 ${theme.panel}`}>
      <div className="flex items-center justify-between gap-2">
        <span className={theme.label}>Background</span>
        <Chip
          theme={theme}
          label="Show background"
          active={backdrop !== null}
          onClick={() => onChange(backdrop ? null : DEFAULT_BACKDROP)}
          disabled={disabled}
        >
          {backdrop ? "On" : "Off"}
        </Chip>
      </div>

      <div className="grid grid-cols-8 gap-1.5" role="group" aria-label="Background fill">
        {fills.map((fill) => (
          <button
            key={fill}
            type="button"
            title={fillLabel(fill)}
            aria-label={`Background ${fillLabel(fill)}`}
            aria-pressed={backdrop !== null && current.fill === fill}
            disabled={disabled}
            onClick={() => onChange({ ...current, fill })}
            className={`relative h-7 w-7 rounded-md border transition-transform hover:scale-105 disabled:opacity-40 ${
              backdrop !== null && current.fill === fill
                ? "border-transparent ring-2 ring-offset-1 ring-current"
                : "border-black/10"
            }`}
            style={{ background: backdropFillCss(fill, autoColors), overflow: "hidden" }}
          >
            {fill === "blur" && imageUrl && <BlurredImage imageUrl={imageUrl} blur="3px" />}
            {fill === "auto" && (
              <span className="absolute inset-0 flex items-center justify-center text-[10px] font-bold text-white drop-shadow">
                A
              </span>
            )}
          </button>
        ))}
      </div>

      <div className="flex flex-col gap-2">
        {slider("padding", "Padding")}
        {slider("radius", "Corners")}
        {slider("shadow", "Shadow")}
        {slider("inset", "Inset")}
        {slider("grain", "Grain")}
      </div>

      <div className="flex flex-wrap items-center gap-1" role="group" aria-label="Inset color">
        <span className={`w-16 shrink-0 ${theme.label}`}>Inset color</span>
        <Chip
          theme={theme}
          active={backdrop !== null && !current.insetColor}
          onClick={() => {
            const next = { ...current };
            delete next.insetColor;
            onChange(next);
          }}
          disabled={disabled || !backdrop}
          title="The color at the edge of the screenshot"
        >
          Auto
        </Chip>
        {INSET_SWATCHES.map((swatch) => (
          <button
            key={swatch}
            type="button"
            aria-label={`Inset color ${swatch}`}
            aria-pressed={current.insetColor === swatch}
            disabled={disabled || !backdrop}
            onClick={() => update({ insetColor: swatch, inset: current.inset > 0 ? current.inset : 0.03 })}
            className={`h-5 w-5 rounded-full border transition-transform hover:scale-110 disabled:opacity-40 ${
              current.insetColor === swatch ? "ring-2 ring-current ring-offset-1" : "border-black/15"
            }`}
            style={{ backgroundColor: swatch }}
          />
        ))}
        <label
          title="Custom inset color"
          className="relative h-5 w-5 cursor-pointer overflow-hidden rounded-full border border-black/15"
          style={{
            background: "conic-gradient(#ff3b30, #ffcc00, #34c759, #0a84ff, #af52de, #ff3b30)",
          }}
        >
          <span className="sr-only">Custom inset color</span>
          <input
            type="color"
            value={current.insetColor ?? "#ffffff"}
            disabled={disabled || !backdrop}
            onChange={(event) =>
              update({ insetColor: event.target.value, inset: current.inset > 0 ? current.inset : 0.03 })
            }
            className="absolute inset-0 h-full w-full cursor-pointer opacity-0"
          />
        </label>
      </div>

      <div className="flex flex-wrap items-center gap-1" role="group" aria-label="Aspect ratio">
        <span className={`w-16 shrink-0 ${theme.label}`}>Size</span>
        {BACKDROP_ASPECTS.map((aspect) => (
          <Chip
            key={aspect.value}
            theme={theme}
            active={backdrop !== null && current.aspect === aspect.value}
            onClick={() => update({ aspect: aspect.value })}
            disabled={disabled || !backdrop}
          >
            {aspect.label}
          </Chip>
        ))}
      </div>

      <div className="flex flex-wrap items-center gap-1" role="group" aria-label="Window frame">
        <span className={`w-16 shrink-0 ${theme.label}`}>Frame</span>
        {BACKDROP_FRAMES.map((frame) => (
          <Chip
            key={frame.value}
            theme={theme}
            active={backdrop !== null && current.frame === frame.value}
            onClick={() => update({ frame: frame.value })}
            disabled={disabled || !backdrop}
          >
            {frame.label}
          </Chip>
        ))}
      </div>

      <div className={`flex flex-wrap items-center gap-1 border-t pt-3 ${theme.divider}`}>
        <span className={`w-16 shrink-0 ${theme.label}`}>Looks</span>
        {saved.map((preset, index) => (
          <button
            key={index}
            type="button"
            title="Apply this saved look"
            aria-label={`Saved look ${index + 1}`}
            disabled={disabled}
            onClick={() => onChange(preset)}
            className="h-7 w-7 rounded-md border border-black/10 transition-transform hover:scale-105"
            style={{ background: backdropFillCss(preset.fill, autoColors) }}
          />
        ))}
        <Chip
          theme={theme}
          disabled={disabled || !backdrop}
          title="Save this background as a look you can reapply"
          onClick={() => {
            if (!backdrop) return;
            const next = [backdrop, ...saved.filter((p) => JSON.stringify(p) !== JSON.stringify(backdrop))].slice(
              0,
              MAX_SAVED_PRESETS
            );
            setSaved(next);
            writeSavedPresets(next);
          }}
        >
          Save look
        </Chip>
      </div>
    </div>
  );
}

/**
 * How much of the previewed layout the screenshot itself takes, per axis. A
 * host whose canvas area cannot scroll shrinks the canvas by these so the
 * padded preview still fits.
 */
export function backdropFitRatio(preview: BackdropPreview | null): { width: number; height: number } {
  if (!preview) return { width: 1, height: 1 };
  const { layout } = preview;
  return {
    width: layout.image.width / Math.max(1, layout.width),
    height: layout.image.height / Math.max(1, layout.height),
  };
}

/**
 * Previews the backdrop around the live canvas with the same layout the export
 * uses (`layoutBackdrop`), so padding, corners, aspect ratio and frame read the
 * same on screen as in the PNG. With no backdrop it renders its children as is.
 */
export function BackdropFrame({
  preview,
  children,
}: {
  preview: BackdropPreview | null;
  children: ReactNode;
}) {
  // The same three wrappers render with or without a background (as
  // `display: contents` when there is none). Switching between a fragment and
  // nested divs would remount the canvas inside, and the editor's resize
  // observer and every ref to it would be left on a detached node.
  const hidden: CSSProperties = { display: "contents" };
  // A background that changes nothing is exported as the bare screenshot, so
  // the preview shows the bare canvas too (no shadow the export would drop).
  if (!preview || backdropIsNoop(preview.backdrop)) {
    return (
      <BackdropShell outer={hidden} card={hidden} content={hidden}>
        {children}
      </BackdropShell>
    );
  }
  const { layout, backdrop, autoColors, imageUrl } = preview;
  const k = 1 / Math.max(preview.displayScale, 0.0001);
  const px = (value: number) => `${Math.round(value * k * 2) / 2}px`;
  const right = layout.width - layout.card.x - layout.card.width;
  const bottom = layout.height - layout.card.y - layout.card.height;
  const inset = layout.image.x - layout.card.x;
  const dark = backdrop.frame === "macos-dark";

  const outer: CSSProperties = {
    display: "inline-block",
    position: "relative",
    overflow: "hidden",
    maxWidth: "100%",
    lineHeight: 0,
    background: backdrop.fill === "blur" ? "#e4e4e7" : backdropFillCss(backdrop.fill, autoColors),
    padding: `${px(layout.card.y)} ${px(right)} ${px(bottom)} ${px(layout.card.x)}`,
  };
  const card: CSSProperties = {
    position: "relative",
    borderRadius: px(layout.radius),
    overflow: "hidden",
    background: backdropCardColor(backdrop, autoColors),
    boxShadow:
      backdrop.shadow > 0
        ? `0 ${px(layout.shadowBlur * 0.35)} ${px(layout.shadowBlur)} rgba(0,0,0,${0.18 + backdrop.shadow * 0.32})`
        : undefined,
  };

  const titleBar =
    layout.titleBar > 0 ? (
      <div
        aria-hidden
        className="flex items-center"
        style={{
          height: px(layout.titleBar),
          gap: px(layout.titleBar * 0.18 * 1.2),
          paddingLeft: px(layout.titleBar * 0.37),
          background: dark ? "#2a2a2e" : "#ececee",
        }}
      >
        {["#ff5f57", "#febc2e", "#28c840"].map((colour) => (
          <span
            key={colour}
            className="rounded-full"
            style={{ width: px(layout.titleBar * 0.36), height: px(layout.titleBar * 0.36), background: colour }}
          />
        ))}
      </div>
    ) : null;

  const grain = backdrop.fill !== "none" ? backdrop.grain ?? 0 : 0;
  const underlay =
    (backdrop.fill === "blur" && imageUrl) || grain > 0 ? (
      <span aria-hidden style={{ position: "absolute", inset: 0, overflow: "hidden", pointerEvents: "none" }}>
        {backdrop.fill === "blur" && imageUrl && (
          <BlurredImage imageUrl={imageUrl} blur={px(layout.width / 48)} />
        )}
        {grain > 0 && (
          <span style={{ position: "absolute", inset: 0, backgroundImage: grainCss(grain) }} />
        )}
      </span>
    ) : null;

  return (
    <BackdropShell
      outer={outer}
      card={card}
      content={{ padding: px(inset) }}
      titleBar={titleBar}
      underlay={underlay}
    >
      {children}
    </BackdropShell>
  );
}

/** One element tree for both states, so React never remounts the children. */
function BackdropShell({
  outer,
  card,
  content,
  titleBar = null,
  underlay = null,
  children,
}: {
  outer: CSSProperties;
  card: CSSProperties;
  content: CSSProperties;
  titleBar?: ReactNode;
  underlay?: ReactNode;
  children: ReactNode;
}) {
  return (
    <div style={outer} data-backdrop-preview={outer.display === "contents" ? undefined : ""}>
      {underlay}
      <div style={card}>
        {titleBar}
        <div style={content}>{children}</div>
      </div>
    </div>
  );
}
