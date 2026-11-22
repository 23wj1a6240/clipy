// SPDX-License-Identifier: Apache-2.0
// Copied from the Clipy monorepo (components/screenshot-editor/tools.ts) by the export; the desktop app shares it.
// Relative imports on purpose: the desktop app bundles this module from outside
// the Next project, where the `@/` alias does not exist.
import type {
  ArrowHeads,
  FillStyle,
  Point,
  RedactStyle,
  Shape,
  ShapeKind,
} from "../../lib/screenshots/types";

/**
 * `picker` samples a colour from the screenshot and `cut` removes a strip of
 * it on export; neither draws a shape.
 */
export type EditorTool = ShapeKind | "select" | "crop" | "picker" | "cut";

/** Toolbar order, shared by the web editor and the desktop screenshot window. */
export const EDITOR_TOOLS: readonly { tool: EditorTool; label: string; shortcut: string }[] = [
  { tool: "select", label: "Select", shortcut: "V" },
  { tool: "arrow", label: "Arrow", shortcut: "A" },
  { tool: "ellipse", label: "Circle", shortcut: "O" },
  { tool: "rect", label: "Box", shortcut: "R" },
  { tool: "line", label: "Line", shortcut: "L" },
  { tool: "path", label: "Path", shortcut: "Y" },
  { tool: "text", label: "Text", shortcut: "T" },
  { tool: "callout", label: "Callout", shortcut: "K" },
  { tool: "step", label: "Step number", shortcut: "N" },
  { tool: "emoji", label: "Emoji", shortcut: "E" },
  { tool: "pen", label: "Pen", shortcut: "P" },
  { tool: "highlight", label: "Highlighter", shortcut: "H" },
  { tool: "spotlight", label: "Spotlight", shortcut: "S" },
  { tool: "magnify", label: "Magnifier", shortcut: "M" },
  { tool: "blur", label: "Redact", shortcut: "B" },
  { tool: "picker", label: "Color picker", shortcut: "I" },
  { tool: "crop", label: "Crop", shortcut: "C" },
  { tool: "cut", label: "Cut out strip", shortcut: "X" },
];

/** Emoji offered by the Emoji tool, picked for bug reports and walkthroughs. */
export const EMOJI_CHOICES = [
  "👉",
  "👆",
  "✅",
  "❌",
  "⚠️",
  "🐛",
  "👀",
  "💡",
  "❓",
  "🔥",
  "👍",
  "👎",
  "⭐",
  "🎉",
  "🚀",
  "📌",
  "❤️",
  "🙈",
] as const;

export const MAGNIFY_ZOOMS = [1.5, 2, 3] as const;

/**
 * Style choices that only some tools use. They apply to the next shape drawn
 * and, when a shape of the matching kind is selected, to that shape.
 */
export interface ToolOptions {
  fill: FillStyle;
  dashed: boolean;
  heads: ArrowHeads;
  redact: RedactStyle;
  zoom: number;
  textBackground: boolean;
  emoji: string;
}

export const DEFAULT_TOOL_OPTIONS: ToolOptions = {
  fill: "outline",
  dashed: false,
  heads: "end",
  redact: "pixelate",
  zoom: 2,
  textBackground: false,
  emoji: EMOJI_CHOICES[0],
};

/** Which options make sense for a tool or a selected shape's kind. */
export function optionsFor(kind: EditorTool): (keyof ToolOptions)[] {
  switch (kind) {
    case "rect":
    case "ellipse":
      return ["fill", "dashed"];
    case "arrow":
      return ["heads", "dashed"];
    case "line":
    case "path":
      return ["dashed"];
    case "text":
      return ["textBackground"];
    case "blur":
      return ["redact"];
    case "magnify":
      return ["zoom"];
    case "emoji":
      return ["emoji"];
    default:
      return [];
  }
}

export const STROKE_STEPS = [
  { label: "Thin", value: 0.6 },
  { label: "Medium", value: 1 },
  { label: "Thick", value: 1.8 },
] as const;

/**
 * What the current tool expects you to do. Text and step are CLICKS, not
 * drags, and the old blanket "drag on the image to annotate" is what sent a
 * user dragging with the text tool and concluding it was broken.
 */
export const TOOL_HINTS: Partial<Record<EditorTool, string>> & { default: string } = {
  select:
    "Drag to move, grab an end to reshape, Delete to remove. Arrow keys nudge, ⌘D duplicates, [ and ] change the order.",
  crop: "Drag the area to keep. The crop applies when you save.",
  cut: "Drag down across rows or sideways across columns to cut that strip out and join the rest when you save. Click a strip to keep it after all.",
  text: "Click where the text goes, type, then ⌘↵ (or click away) to place it. Double-click any text to edit it.",
  callout:
    "Drag from what you are pointing at to where the bubble goes, then type. Double-click a callout to edit it.",
  step: "Click to drop the next numbered step.",
  emoji: "Click to place the emoji. Pick another one above.",
  path: "Click to add each point. Double-click or press Enter to finish, click the first point to close the shape.",
  spotlight: "Drag over what matters. Everything else is dimmed.",
  magnify: "Drag a lens over small details to enlarge them.",
  blur: "Drag over anything private. Pick pixelate, blur or a solid bar above.",
  picker: "Click anywhere on the screenshot to use that color. Its hex code is copied too.",
  default: "Drag on the image to annotate. Hold Shift for straight lines and squares. Press V to select and adjust.",
};

/** Single-key tool switching, mirroring the shortcuts shown in the toolbar. */
export const TOOL_SHORTCUTS: Readonly<Record<string, EditorTool>> = Object.fromEntries(
  EDITOR_TOOLS.map(({ tool, shortcut }) => [shortcut.toLowerCase(), tool])
);

const TEXT_INPUT_TYPES: ReadonlySet<string> = new Set([
  "text",
  "search",
  "email",
  "url",
  "tel",
  "password",
  "number",
]);

/**
 * True when a key press belongs to a field the user is typing in. Sliders and
 * color inputs are not text entry: shortcuts keep working after one is used,
 * which `isSliderKey` refines for the keys a focused slider owns.
 */
export function isTextEntry(target: EventTarget | null): boolean {
  const element = target as HTMLElement | null;
  if (!element || typeof element.tagName !== "string") return false;
  if (element.isContentEditable) return true;
  if (element.tagName === "TEXTAREA" || element.tagName === "SELECT") return true;
  if (element.tagName !== "INPUT") return false;
  return TEXT_INPUT_TYPES.has(((element as HTMLInputElement).type || "text").toLowerCase());
}

const SLIDER_KEYS: ReadonlySet<string> = new Set([
  "ArrowLeft",
  "ArrowRight",
  "ArrowUp",
  "ArrowDown",
  "Home",
  "End",
  "PageUp",
  "PageDown",
]);

/** A key a focused range input moves itself with; the editor leaves it alone. */
export function isSliderKey(target: EventTarget | null, key: string): boolean {
  const element = target as HTMLInputElement | null;
  return Boolean(element && element.tagName === "INPUT" && element.type === "range" && SLIDER_KEYS.has(key));
}

export function newShapeId(): string {
  if (typeof crypto !== "undefined" && "randomUUID" in crypto) {
    return crypto.randomUUID();
  }
  return `shape-${Date.now().toString(36)}-${Math.random().toString(36).slice(2, 8)}`;
}

export interface ShapeStyle {
  color: string;
  /** Toolbar weight multiplier (thin/medium/thick). */
  weight: number;
  /** Image-size multiplier so ink stays proportionate on 4K captures. */
  scale: number;
  options?: ToolOptions;
}

export function strokeWidthFor(kind: ShapeKind, style: ShapeStyle): number {
  const base = kind === "highlight" ? 22 : 4;
  return Math.max(1, base * style.scale * style.weight);
}

export function emojiSizeFor(style: ShapeStyle): number {
  return Math.max(16, 52 * style.scale * style.weight);
}

export function fontSizeFor(style: ShapeStyle): number {
  return Math.max(10, 26 * style.scale * style.weight);
}

export function stepRadiusFor(style: ShapeStyle): number {
  return Math.max(8, 20 * style.scale * style.weight);
}

/**
 * Builds the shape a drag/click starts. Returns null for tools that are not
 * shapes (select, crop) so callers can switch on it exhaustively.
 */
export function createShape(
  tool: EditorTool,
  at: Point,
  style: ShapeStyle
): Shape | null {
  if (tool === "select" || tool === "crop" || tool === "picker" || tool === "cut") return null;
  const options = style.options ?? DEFAULT_TOOL_OPTIONS;

  const base: { id: string; color: string; strokeWidth: number; dashed?: true } = {
    id: newShapeId(),
    color: style.color,
    strokeWidth: strokeWidthFor(tool, style),
  };
  if (options.dashed && optionsFor(tool).includes("dashed")) base.dashed = true;

  switch (tool) {
    case "pen":
    case "highlight":
    case "path":
      return { ...base, kind: tool, points: [at, at] };
    case "text": {
      const fontSize = fontSizeFor(style);
      return options.textBackground
        ? { ...base, kind: "text", at, text: "", fontSize, background: true }
        : { ...base, kind: "text", at, text: "", fontSize };
    }
    case "callout":
      return { ...base, kind: "callout", at, tail: at, text: "", fontSize: fontSizeFor(style) };
    case "step":
      return { ...base, kind: "step", at, radius: stepRadiusFor(style) };
    case "emoji":
      return { ...base, kind: "emoji", at, emoji: options.emoji, size: emojiSizeFor(style) };
    case "rect":
    case "ellipse":
      return options.fill !== "outline"
        ? { ...base, kind: tool, from: at, to: at, fill: options.fill }
        : { ...base, kind: tool, from: at, to: at };
    case "arrow":
      return options.heads === "both"
        ? { ...base, kind: tool, from: at, to: at, heads: "both" }
        : { ...base, kind: tool, from: at, to: at };
    case "blur":
      return options.redact !== "pixelate"
        ? { ...base, kind: tool, from: at, to: at, redact: options.redact }
        : { ...base, kind: tool, from: at, to: at };
    case "magnify":
      return { ...base, kind: tool, from: at, to: at, zoom: options.zoom };
    default:
      return { ...base, kind: tool, from: at, to: at };
  }
}

/** A copy of the shape without one optional style field. */
function without(shape: Shape, key: "dashed" | "fill" | "heads" | "redact" | "background"): Shape {
  const copy = { ...shape } as Record<string, unknown>;
  delete copy[key];
  return copy as unknown as Shape;
}

/**
 * The selected shape restyled with one tool option, or the shape unchanged
 * when that option does not apply to its kind.
 */
export function applyOption<K extends keyof ToolOptions>(
  shape: Shape,
  key: K,
  value: ToolOptions[K]
): Shape {
  if (!optionsFor(shape.kind).includes(key)) return shape;
  switch (key) {
    case "dashed": {
      if (value) return { ...shape, dashed: true };
      return without(shape, "dashed");
    }
    case "fill":
      if (shape.kind !== "rect" && shape.kind !== "ellipse") return shape;
      if (value === "outline") {
        return without(shape, "fill");
      }
      return { ...shape, fill: value as FillStyle };
    case "heads":
      if (shape.kind !== "arrow") return shape;
      if (value === "end") {
        return without(shape, "heads");
      }
      return { ...shape, heads: "both" };
    case "redact":
      if (shape.kind !== "blur") return shape;
      if (value === "pixelate") {
        return without(shape, "redact");
      }
      return { ...shape, redact: value as RedactStyle };
    case "zoom":
      return shape.kind === "magnify" ? { ...shape, zoom: value as number } : shape;
    case "textBackground":
      if (shape.kind !== "text") return shape;
      if (!value) {
        return without(shape, "background");
      }
      return { ...shape, background: true };
    case "emoji":
      return shape.kind === "emoji" ? { ...shape, emoji: value as string } : shape;
    default:
      return shape;
  }
}

/** The tool options a shape was drawn with, so selecting it syncs the bar. */
export function optionsFromShape(shape: Shape, current: ToolOptions): ToolOptions {
  const next = { ...current };
  const keys = optionsFor(shape.kind);
  if (keys.includes("dashed")) next.dashed = Boolean(shape.dashed);
  if (shape.kind === "rect" || shape.kind === "ellipse") next.fill = shape.fill ?? "outline";
  if (shape.kind === "arrow") next.heads = shape.heads ?? "end";
  if (shape.kind === "blur") next.redact = shape.redact ?? "pixelate";
  if (shape.kind === "magnify") next.zoom = shape.zoom ?? 2;
  if (shape.kind === "text") next.textBackground = Boolean(shape.background);
  if (shape.kind === "emoji") next.emoji = shape.emoji;
  return next;
}

/** The selected shape resized to a new stroke weight. */
export function applyWeight(shape: Shape, style: ShapeStyle): Shape {
  switch (shape.kind) {
    case "text":
    case "callout":
      return { ...shape, fontSize: fontSizeFor(style) };
    case "step":
      return { ...shape, radius: stepRadiusFor(style) };
    case "emoji":
      return { ...shape, size: emojiSizeFor(style) };
    default:
      return { ...shape, strokeWidth: strokeWidthFor(shape.kind, style) };
  }
}

/** Drags shorter than this (in image pixels) are treated as stray clicks. */
export function isMeaningfulShape(shape: Shape, minLength: number): boolean {
  switch (shape.kind) {
    case "text":
    case "callout":
      return shape.text.trim().length > 0;
    case "step":
    case "emoji":
      return true;
    case "pen":
    case "highlight":
      return shape.points.length > 1;
    case "path":
      return shape.points.length > 1 && shape.points.some(
        (p) => Math.hypot(p.x - shape.points[0].x, p.y - shape.points[0].y) >= minLength
      );
    default:
      return (
        Math.abs(shape.to.x - shape.from.x) >= minLength ||
        Math.abs(shape.to.y - shape.from.y) >= minLength
      );
  }
}
