// SPDX-License-Identifier: Apache-2.0
// Copied from the Clipy monorepo (lib/screenshots/scene.ts) by the export; the desktop app shares it.
/**
 * Scene (de)serialization + defaults.
 *
 * `parseScene` is the trust boundary: the editor POSTs its scene alongside the
 * flattened PNG so the screenshot stays re-editable, which means arbitrary JSON
 * reaches the server and is later replayed into a canvas in someone's browser.
 * Everything is rebuilt field by field — nothing from the input object is
 * spread through — and every number/string is range-checked.
 */

import {
  BACKDROP_ASPECTS,
  BACKDROP_FRAMES,
  BACKDROP_LIMITS,
  isHexColor,
  isValidBackdropFill,
} from './backdrop';
import {
  SCENE_VERSION,
  emptyScene,
  type AnnotationScene,
  type Backdrop,
  type BackdropAspect,
  type BackdropFrame,
  type Cut,
  type FillStyle,
  type Point,
  type Rect,
  type RedactStyle,
  type Shape,
  type ShapeKind,
} from './types';

export const MAX_SHAPES = 250;
export const MAX_STROKE_POINTS = 4000;
export const MAX_TEXT_LENGTH = 500;
/** Long enough for any emoji ZWJ sequence (family, flags, skin tones). */
export const MAX_EMOJI_LENGTH = 16;
export const MAX_CUTS = 20;
/** Serialized scene cap, enforced on the raw request field before parsing. */
export const MAX_SCENE_BYTES = 512 * 1024;

const COORD_LIMIT = 100_000;
const MAX_STROKE_WIDTH = 400;
const MAX_FONT_SIZE = 1000;

export const ANNOTATION_COLORS = [
  '#ff3b30',
  '#ff9500',
  '#ffcc00',
  '#34c759',
  '#0a84ff',
  '#af52de',
  '#ffffff',
  '#111111',
] as const;

export const DEFAULT_COLOR = ANNOTATION_COLORS[0];

const SHAPE_KINDS: ReadonlySet<ShapeKind> = new Set<ShapeKind>([
  'arrow',
  'line',
  'rect',
  'ellipse',
  'blur',
  'pen',
  'highlight',
  'text',
  'step',
  'magnify',
  'spotlight',
  'path',
  'emoji',
  'callout',
]);

const FILL_STYLES: ReadonlySet<string> = new Set<FillStyle>(['outline', 'tint', 'solid']);
const REDACT_STYLES: ReadonlySet<string> = new Set<RedactStyle>(['pixelate', 'blur', 'solid']);
const ASPECTS: ReadonlySet<string> = new Set<BackdropAspect>(BACKDROP_ASPECTS.map((a) => a.value));
const FRAMES: ReadonlySet<string> = new Set<BackdropFrame>(BACKDROP_FRAMES.map((f) => f.value));
// Control characters and the bidi overrides that could visually reorder text.
const UNSAFE_CHARS = /[\u0000-\u001f\u007f\u202a-\u202e\u2066-\u2069]/g;
// The same for text, which keeps its line breaks and tabs.
const UNSAFE_TEXT_CHARS = /[\u0000-\u0008\u000b-\u001f\u007f\u202a-\u202e\u2066-\u2069]/g;

const HEX_COLOR = /^#(?:[0-9a-f]{3}|[0-9a-f]{6})$/i;

function isShapeKind(value: string): value is ShapeKind {
  return SHAPE_KINDS.has(value as ShapeKind);
}

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === 'object' && value !== null && !Array.isArray(value);
}

function num(value: unknown, fallback: number, limit = COORD_LIMIT): number {
  if (typeof value !== 'number' || !Number.isFinite(value)) return fallback;
  return Math.min(limit, Math.max(-limit, value));
}

function positive(value: unknown, fallback: number, limit: number): number {
  const parsed = num(value, fallback, limit);
  return parsed > 0 ? parsed : fallback;
}

function color(value: unknown): string {
  return typeof value === 'string' && HEX_COLOR.test(value.trim())
    ? value.trim().toLowerCase()
    : DEFAULT_COLOR;
}

function point(value: unknown): Point | null {
  if (!isRecord(value)) return null;
  if (typeof value.x !== 'number' || typeof value.y !== 'number') return null;
  if (!Number.isFinite(value.x) || !Number.isFinite(value.y)) return null;
  return { x: num(value.x, 0), y: num(value.y, 0) };
}

function shapeId(value: unknown, index: number): string {
  return typeof value === 'string' && value.length > 0 && value.length <= 64
    ? value
    : `shape-${index}`;
}

function parseShape(value: unknown, index: number): Shape | null {
  if (!isRecord(value)) return null;
  const kind = typeof value.kind === 'string' ? value.kind : '';
  if (!isShapeKind(kind)) return null;

  // Optional style fields are only written when set, so a scene saved before
  // they existed round-trips byte for byte.
  const base: { id: string; color: string; strokeWidth: number; dashed?: true } = {
    id: shapeId(value.id, index),
    color: color(value.color),
    strokeWidth: positive(value.strokeWidth, 4, MAX_STROKE_WIDTH),
  };
  if (value.dashed === true) base.dashed = true;

  if (kind === 'pen' || kind === 'highlight' || kind === 'path') {
    if (!Array.isArray(value.points)) return null;
    const points: Point[] = [];
    for (const raw of value.points.slice(0, MAX_STROKE_POINTS)) {
      const parsed = point(raw);
      if (parsed) points.push(parsed);
    }
    if (points.length < 2) return null;
    if (kind === 'path' && value.closed === true) return { ...base, kind, points, closed: true };
    return { ...base, kind, points };
  }

  if (kind === 'text' || kind === 'callout') {
    const at = point(value.at);
    const text =
      typeof value.text === 'string'
        ? value.text.replace(UNSAFE_TEXT_CHARS, '').slice(0, MAX_TEXT_LENGTH)
        : '';
    if (!at || text.trim().length === 0) return null;
    const fontSize = positive(value.fontSize, 28, MAX_FONT_SIZE);
    if (kind === 'callout') {
      const tail = point(value.tail) ?? { x: at.x, y: at.y + fontSize * 3 };
      return { ...base, kind, at, text, fontSize, tail };
    }
    if (value.background === true) return { ...base, kind, at, text, fontSize, background: true };
    return { ...base, kind, at, text, fontSize };
  }

  if (kind === 'emoji') {
    const at = point(value.at);
    const emoji =
      typeof value.emoji === 'string'
        ? value.emoji.replace(UNSAFE_CHARS, '').slice(0, MAX_EMOJI_LENGTH)
        : '';
    if (!at || emoji.trim().length === 0) return null;
    return { ...base, kind, at, emoji, size: positive(value.size, 48, MAX_FONT_SIZE) };
  }

  if (kind === 'step') {
    const at = point(value.at);
    if (!at) return null;
    return {
      ...base,
      kind,
      at,
      radius: positive(value.radius, 22, MAX_STROKE_WIDTH),
    };
  }

  const from = point(value.from);
  const to = point(value.to);
  if (!from || !to) return null;
  const shape: Extract<Shape, { from: Point }> = { ...base, kind, from, to };
  if ((kind === 'rect' || kind === 'ellipse') && typeof value.fill === 'string') {
    if (FILL_STYLES.has(value.fill) && value.fill !== 'outline') {
      shape.fill = value.fill as FillStyle;
    }
  }
  if (kind === 'blur' && typeof value.redact === 'string') {
    if (REDACT_STYLES.has(value.redact) && value.redact !== 'pixelate') {
      shape.redact = value.redact as RedactStyle;
    }
  }
  if (kind === 'arrow' && value.heads === 'both') shape.heads = 'both';
  if (kind === 'magnify') {
    shape.zoom = Math.min(4, Math.max(1.25, num(value.zoom, 2, 4)));
  }
  return shape;
}

function fraction(value: unknown, limit: number): number {
  const parsed = num(value, 0, limit);
  return Math.min(limit, Math.max(0, parsed));
}

/** Validates a backdrop from untrusted input (a saved scene, local storage). */
export function parseBackdrop(value: unknown): Backdrop | null {
  if (!isRecord(value)) return null;
  const rawFill = typeof value.fill === 'string' ? value.fill.trim().toLowerCase() : '';
  if (!isValidBackdropFill(rawFill)) return null;
  const backdrop: Backdrop = {
    fill: rawFill,
    padding: fraction(value.padding, BACKDROP_LIMITS.padding),
    radius: fraction(value.radius, BACKDROP_LIMITS.radius),
    shadow: fraction(value.shadow, BACKDROP_LIMITS.shadow),
    inset: fraction(value.inset, BACKDROP_LIMITS.inset),
    aspect:
      typeof value.aspect === 'string' && ASPECTS.has(value.aspect)
        ? (value.aspect as BackdropAspect)
        : 'auto',
    frame:
      typeof value.frame === 'string' && FRAMES.has(value.frame)
        ? (value.frame as BackdropFrame)
        : 'none',
  };
  if (typeof value.insetColor === 'string' && isHexColor(value.insetColor.trim())) {
    backdrop.insetColor = value.insetColor.trim().toLowerCase();
  }
  const grain = fraction(value.grain, BACKDROP_LIMITS.grain);
  if (grain > 0) backdrop.grain = grain;
  return backdrop;
}

function parseCuts(value: unknown): Cut[] {
  if (!Array.isArray(value)) return [];
  const cuts: Cut[] = [];
  for (const raw of value.slice(0, MAX_CUTS)) {
    if (!isRecord(raw)) continue;
    if (raw.axis !== 'rows' && raw.axis !== 'columns') continue;
    if (typeof raw.from !== 'number' || typeof raw.to !== 'number') continue;
    if (!Number.isFinite(raw.from) || !Number.isFinite(raw.to)) continue;
    const a = Math.max(0, num(raw.from, 0));
    const b = Math.max(0, num(raw.to, 0));
    const from = Math.min(a, b);
    const to = Math.max(a, b);
    if (to - from < 1) continue;
    cuts.push({ axis: raw.axis, from, to });
  }
  return cuts;
}

function parseCrop(value: unknown): Rect | null {
  if (!isRecord(value)) return null;
  const width = num(value.width, 0);
  const height = num(value.height, 0);
  if (width < 1 || height < 1) return null;
  return { x: num(value.x, 0), y: num(value.y, 0), width, height };
}

/** Never throws — invalid input degrades to an empty scene, never a 500. */
export function parseScene(value: unknown): AnnotationScene {
  if (!isRecord(value)) return emptyScene();
  const rawShapes = Array.isArray(value.shapes) ? value.shapes : [];
  const shapes: Shape[] = [];
  for (const [index, raw] of rawShapes.slice(0, MAX_SHAPES).entries()) {
    const shape = parseShape(raw, index);
    if (shape) shapes.push(shape);
  }
  const scene: AnnotationScene = { version: SCENE_VERSION, shapes, crop: parseCrop(value.crop) };
  // Only written when set, so a scene without a background stores exactly as
  // it did before backgrounds existed.
  const backdrop = parseBackdrop(value.backdrop);
  if (backdrop) scene.backdrop = backdrop;
  const cuts = parseCuts(value.cuts);
  if (cuts.length > 0) scene.cuts = cuts;
  return scene;
}

/** Parses a scene out of a JSON string (the multipart field the editor sends). */
export function parseSceneJson(raw: string | null | undefined): AnnotationScene {
  if (!raw) return emptyScene();
  if (raw.length > MAX_SCENE_BYTES) return emptyScene();
  try {
    return parseScene(JSON.parse(raw));
  } catch {
    return emptyScene();
  }
}

export function sceneIsEmpty(scene: AnnotationScene): boolean {
  return (
    scene.shapes.length === 0 &&
    scene.crop === null &&
    !scene.backdrop &&
    (scene.cuts?.length ?? 0) === 0
  );
}

/**
 * Annotation sizes scale with the screenshot: a 4px stroke that reads well on a
 * 1000px capture is invisible on a 4K one.
 */
export function sizeScale(imageWidth: number, imageHeight: number): number {
  const longest = Math.max(imageWidth, imageHeight, 1);
  return Math.max(0.5, Math.min(4, longest / 1400));
}
