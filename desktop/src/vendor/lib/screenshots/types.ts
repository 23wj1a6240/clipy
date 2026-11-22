// SPDX-License-Identifier: Apache-2.0
// Copied from the Clipy monorepo (lib/screenshots/types.ts) by the export; the desktop app shares it.
/**
 * Annotation model for the screenshot editor.
 *
 * Every coordinate is in ORIGINAL image pixels, never display pixels — the
 * canvas is scaled by CSS and the same scene has to flatten identically on a
 * 400px popup preview and a 3840px export.
 *
 * Kept free of DOM/canvas imports on purpose: `parseScene` runs on the server
 * when the editor saves, so this module must be isomorphic.
 */

export interface Point {
  x: number;
  y: number;
}

export interface Rect {
  x: number;
  y: number;
  width: number;
  height: number;
}

/**
 * Shapes drawn between two dragged corners/endpoints. `magnify` is a round lens
 * over the box, `spotlight` dims everything outside it.
 */
export type SegmentShapeKind =
  | 'arrow'
  | 'line'
  | 'rect'
  | 'ellipse'
  | 'blur'
  | 'magnify'
  | 'spotlight';
/**
 * Shapes made of a point list: freehand trails, and `path`, straight segments
 * placed one click at a time.
 */
export type StrokeShapeKind = 'pen' | 'highlight' | 'path';
/** Shapes anchored at a single point. */
export type AnchorShapeKind = 'text' | 'step' | 'emoji' | 'callout';

export type ShapeKind = SegmentShapeKind | StrokeShapeKind | AnchorShapeKind;

/** How a rect, ellipse or callout is filled. Absent means `outline`. */
export type FillStyle = 'outline' | 'tint' | 'solid';
/** How a redaction hides what is under it. Absent means `pixelate`. */
export type RedactStyle = 'pixelate' | 'blur' | 'solid';
/** Which ends of an arrow get a head. Absent means `end`. */
export type ArrowHeads = 'end' | 'both';

interface ShapeBase {
  id: string;
  color: string;
  /** Stroke thickness in image pixels. */
  strokeWidth: number;
  /** Dashed outline (lines, arrows, rects, ellipses, paths). */
  dashed?: boolean;
}

export interface SegmentShape extends ShapeBase {
  kind: SegmentShapeKind;
  from: Point;
  to: Point;
  /** rect and ellipse only. */
  fill?: FillStyle;
  /** blur (the Redact tool) only. */
  redact?: RedactStyle;
  /** arrow only. */
  heads?: ArrowHeads;
  /** magnify only: how much the lens enlarges, 1.25 to 4. */
  zoom?: number;
}

export interface StrokeShape extends ShapeBase {
  kind: StrokeShapeKind;
  points: Point[];
  /** path only: joins the last point back to the first. */
  closed?: boolean;
}

export interface TextShape extends ShapeBase {
  kind: 'text';
  at: Point;
  text: string;
  fontSize: number;
  /** Draws the text on a filled label in the shape's color. */
  background?: boolean;
}

/** Text in a speech bubble whose tail points at `tail`. */
export interface CalloutShape extends ShapeBase {
  kind: 'callout';
  /** Top-left of the text inside the bubble. */
  at: Point;
  text: string;
  fontSize: number;
  tail: Point;
}

/** One emoji, centred on `at`. */
export interface EmojiShape extends ShapeBase {
  kind: 'emoji';
  at: Point;
  emoji: string;
  size: number;
}

/** Numbered badge for "do this, then this" walkthroughs and bug repros. */
export interface StepShape extends ShapeBase {
  kind: 'step';
  at: Point;
  radius: number;
}

export type Shape =
  | SegmentShape
  | StrokeShape
  | TextShape
  | StepShape
  | CalloutShape
  | EmojiShape;

export type BackdropAspect = 'auto' | '1:1' | '4:3' | '3:2' | '16:9' | '9:16' | '4:5';
export type BackdropFrame = 'none' | 'macos-light' | 'macos-dark';

/**
 * The "beautify" pass: the screenshot sits on a card, on a background, with
 * padding, rounded corners and a shadow. Sizes are fractions of the image's
 * longest side so the same look reads the same on a small and a 4K capture.
 * Applied on export only; the editor previews it around the canvas.
 */
export interface Backdrop {
  /**
   * `none` (transparent), `auto` (from the screenshot's own colours), `blur`
   * (the screenshot itself, blurred), a gradient preset (`g:<id>`), a mesh
   * preset (`m:<id>`), or a solid hex colour.
   */
  fill: string;
  /** Space around the card, 0 to 0.25. */
  padding: number;
  /** Card corner radius, 0 to 0.08. */
  radius: number;
  /** Drop shadow strength, 0 to 1. */
  shadow: number;
  /** Margin inside the card in the screenshot's edge colour, 0 to 0.1. */
  inset: number;
  aspect: BackdropAspect;
  frame: BackdropFrame;
  /** Inset colour; absent means the screenshot's own edge colour. */
  insetColor?: string;
  /** Film grain over the background, 0 to 1. Absent means none. */
  grain?: number;
}

/**
 * A strip removed from the screenshot on export, the rest joined up: rows
 * `from` to `to` (a horizontal band) or columns (a vertical band), in
 * original image pixels.
 */
export interface Cut {
  axis: 'rows' | 'columns';
  from: number;
  to: number;
}

export interface AnnotationScene {
  version: 1;
  shapes: Shape[];
  /** Non-destructive crop, in original image pixels. */
  crop: Rect | null;
  /** Absent or null: exported as the bare screenshot. */
  backdrop?: Backdrop | null;
  /** Strips cut out on export, after the crop. Absent means none. */
  cuts?: Cut[];
}

export const SCENE_VERSION = 1 as const;

export function emptyScene(): AnnotationScene {
  return { version: SCENE_VERSION, shapes: [], crop: null };
}

export function isSegmentShape(shape: Shape): shape is SegmentShape {
  return (
    shape.kind === 'arrow' ||
    shape.kind === 'line' ||
    shape.kind === 'rect' ||
    shape.kind === 'ellipse' ||
    shape.kind === 'blur' ||
    shape.kind === 'magnify' ||
    shape.kind === 'spotlight'
  );
}

export function isStrokeShape(shape: Shape): shape is StrokeShape {
  return shape.kind === 'pen' || shape.kind === 'highlight' || shape.kind === 'path';
}

export function isAnchorShape(
  shape: Shape
): shape is TextShape | StepShape | CalloutShape | EmojiShape {
  return (
    shape.kind === 'text' ||
    shape.kind === 'step' ||
    shape.kind === 'emoji' ||
    shape.kind === 'callout'
  );
}

/**
 * Step badges are numbered by their order in `shapes`, so deleting step 2
 * renumbers the rest instead of leaving a hole.
 */
export function stepNumber(shapes: Shape[], id: string): number {
  let n = 0;
  for (const shape of shapes) {
    if (shape.kind !== 'step') continue;
    n += 1;
    if (shape.id === id) return n;
  }
  return n;
}
