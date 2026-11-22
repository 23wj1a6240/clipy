// SPDX-License-Identifier: Apache-2.0
// Copied from the Clipy monorepo (lib/screenshots/geometry.ts) by the export; the desktop app shares it.
/**
 * Geometry for selecting, moving and reshaping annotations. Pure functions on
 * image-pixel coordinates — no canvas, so it is unit-testable.
 */

import {
  isSegmentShape,
  isStrokeShape,
  type CalloutShape,
  type Point,
  type Rect,
  type SegmentShapeKind,
  type Shape,
} from './types';

export function rectFromPoints(a: Point, b: Point): Rect {
  return {
    x: Math.min(a.x, b.x),
    y: Math.min(a.y, b.y),
    width: Math.abs(a.x - b.x),
    height: Math.abs(a.y - b.y),
  };
}

export function clampRect(rect: Rect, bounds: { width: number; height: number }): Rect {
  const x = Math.max(0, Math.min(rect.x, bounds.width));
  const y = Math.max(0, Math.min(rect.y, bounds.height));
  return {
    x,
    y,
    width: Math.max(0, Math.min(rect.width, bounds.width - x)),
    height: Math.max(0, Math.min(rect.height, bounds.height - y)),
  };
}

export function rectContains(rect: Rect, p: Point): boolean {
  return (
    p.x >= rect.x &&
    p.x <= rect.x + rect.width &&
    p.y >= rect.y &&
    p.y <= rect.y + rect.height
  );
}

export function distance(a: Point, b: Point): number {
  return Math.hypot(a.x - b.x, a.y - b.y);
}

export function distanceToSegment(p: Point, a: Point, b: Point): number {
  const dx = b.x - a.x;
  const dy = b.y - a.y;
  const lengthSq = dx * dx + dy * dy;
  if (lengthSq === 0) return distance(p, a);
  const t = Math.max(0, Math.min(1, ((p.x - a.x) * dx + (p.y - a.y) * dy) / lengthSq));
  return distance(p, { x: a.x + t * dx, y: a.y + t * dy });
}

/** Axis-aligned bounds, ignoring stroke width. */
export function shapeBounds(shape: Shape): Rect {
  if (isSegmentShape(shape)) return rectFromPoints(shape.from, shape.to);
  if (isStrokeShape(shape)) {
    const xs = shape.points.map((p) => p.x);
    const ys = shape.points.map((p) => p.y);
    const x = Math.min(...xs);
    const y = Math.min(...ys);
    return { x, y, width: Math.max(...xs) - x, height: Math.max(...ys) - y };
  }
  if (shape.kind === 'step') {
    return {
      x: shape.at.x - shape.radius,
      y: shape.at.y - shape.radius,
      width: shape.radius * 2,
      height: shape.radius * 2,
    };
  }
  if (shape.kind === 'emoji') {
    return {
      x: shape.at.x - shape.size / 2,
      y: shape.at.y - shape.size / 2,
      width: shape.size,
      height: shape.size,
    };
  }
  if (shape.kind === 'callout') {
    const bubble = calloutBubble(shape);
    const x = Math.min(bubble.x, shape.tail.x);
    const y = Math.min(bubble.y, shape.tail.y);
    return {
      x,
      y,
      width: Math.max(bubble.x + bubble.width, shape.tail.x) - x,
      height: Math.max(bubble.y + bubble.height, shape.tail.y) - y,
    };
  }
  const text = textBlock(shape.text, shape.fontSize);
  if (!shape.background) return { x: shape.at.x, y: shape.at.y, ...text };
  return inflate({ x: shape.at.x, y: shape.at.y, ...text }, labelPadding(shape.fontSize));
}

/** Width of one line of annotation text at a font size, in image pixels. */
export type TextMeasurer = (line: string, fontSize: number) => number;

let textMeasurer: TextMeasurer | null = null;

/**
 * Lets the editor plug in the canvas's own text measurement, so selection
 * outlines and hit areas match the drawn text. Without one (the server, node
 * tests) widths are estimated from the glyph count.
 */
export function setTextMeasurer(measurer: TextMeasurer | null): void {
  textMeasurer = measurer;
}

/** Text block size, measured when a measurer is installed, estimated otherwise. */
export function textBlock(text: string, fontSize: number): { width: number; height: number } {
  const lines = text.split('\n');
  const width = textMeasurer
    ? lines.reduce((max, line) => Math.max(max, textMeasurer!(line, fontSize)), 0)
    : lines.reduce((max, line) => Math.max(max, line.length), 1) * fontSize * 0.58;
  return { width, height: lines.length * fontSize * 1.25 };
}

/** Padding between a label's or bubble's edge and its text. */
export function labelPadding(fontSize: number): number {
  return Math.max(4, fontSize * 0.45);
}

/** The callout's bubble, from its text block plus padding. */
export function calloutBubble(shape: Pick<CalloutShape, 'at' | 'text' | 'fontSize'>, measured?: number): Rect {
  const block = textBlock(shape.text || ' ', shape.fontSize);
  const pad = labelPadding(shape.fontSize);
  return {
    x: shape.at.x - pad,
    y: shape.at.y - pad,
    width: (measured ?? block.width) + pad * 2,
    height: block.height + pad * 2,
  };
}

/**
 * Where text goes so its block (plus a label's or bubble's padding when
 * `padded`) stays inside `bounds`: pushed left or up off the right and bottom
 * edges, never past the left or top one. A block wider than `bounds` sits at
 * its left edge.
 */
export function textInside(at: Point, text: string, fontSize: number, padded: boolean, bounds: Rect): Point {
  const block = textBlock(text || ' ', fontSize);
  const pad = padded ? labelPadding(fontSize) : 0;
  return {
    x: Math.max(bounds.x + pad, Math.min(at.x, bounds.x + bounds.width - block.width - pad)),
    y: Math.max(bounds.y + pad, Math.min(at.y, bounds.y + bounds.height - block.height - pad)),
  };
}

export function inflate(rect: Rect, by: number): Rect {
  return {
    x: rect.x - by,
    y: rect.y - by,
    width: rect.width + by * 2,
    height: rect.height + by * 2,
  };
}

/**
 * Hit test in image pixels. Outlined shapes are picked by their edge (so you
 * can click "through" a big empty box), filled ones by their area.
 */
export function hitTestShape(shape: Shape, p: Point, tolerance: number): boolean {
  const slack = Math.max(tolerance, shape.strokeWidth / 2 + tolerance);

  switch (shape.kind) {
    case 'arrow':
    case 'line':
      return distanceToSegment(p, shape.from, shape.to) <= slack;
    case 'blur':
    case 'spotlight':
      return rectContains(inflate(rectFromPoints(shape.from, shape.to), slack), p);
    case 'magnify': {
      const rect = rectFromPoints(shape.from, shape.to);
      const r = Math.max(rect.width, rect.height) / 2;
      return distance(p, centerOf(rect)) <= r + slack;
    }
    case 'rect':
      return shape.fill && shape.fill !== 'outline'
        ? rectContains(inflate(rectFromPoints(shape.from, shape.to), slack), p)
        : hitRectEdge(shape.from, shape.to, p, slack);
    case 'ellipse':
      return shape.fill && shape.fill !== 'outline'
        ? insideEllipse(shape.from, shape.to, p, slack)
        : hitEllipseEdge(shape.from, shape.to, p, slack);
    case 'pen':
    case 'highlight':
    case 'path':
      return hitPolyline(
        shape.kind === 'path' && shape.closed ? [...shape.points, shape.points[0]] : shape.points,
        p,
        slack
      );
    case 'step':
      return distance(p, shape.at) <= shape.radius + tolerance;
    case 'text':
    case 'emoji':
      return rectContains(inflate(shapeBounds(shape), tolerance), p);
    case 'callout': {
      const bubble = calloutBubble(shape);
      return (
        rectContains(inflate(bubble, tolerance), p) ||
        distanceToSegment(p, shape.tail, centerOf(bubble)) <= slack * 1.5
      );
    }
    default:
      return false;
  }
}

function centerOf(rect: Rect): Point {
  return { x: rect.x + rect.width / 2, y: rect.y + rect.height / 2 };
}

function hitPolyline(points: Point[], p: Point, slack: number): boolean {
  for (let i = 1; i < points.length; i += 1) {
    if (distanceToSegment(p, points[i - 1], points[i]) <= slack) return true;
  }
  return false;
}

function hitRectEdge(from: Point, to: Point, p: Point, slack: number): boolean {
  const outer = inflate(rectFromPoints(from, to), slack);
  const inner = inflate(rectFromPoints(from, to), -slack);
  return (
    rectContains(outer, p) &&
    !(inner.width > 0 && inner.height > 0 && rectContains(inner, p))
  );
}

function insideEllipse(from: Point, to: Point, p: Point, slack: number): boolean {
  const rect = rectFromPoints(from, to);
  const rx = rect.width / 2 + slack;
  const ry = rect.height / 2 + slack;
  const c = centerOf(rect);
  return ((p.x - c.x) / rx) ** 2 + ((p.y - c.y) / ry) ** 2 <= 1;
}

function hitEllipseEdge(from: Point, to: Point, p: Point, slack: number): boolean {
  const rect = rectFromPoints(from, to);
  const rx = rect.width / 2;
  const ry = rect.height / 2;
  if (rx <= 0 || ry <= 0) return false;
  const cx = rect.x + rx;
  const cy = rect.y + ry;
  const outer = ((p.x - cx) / (rx + slack)) ** 2 + ((p.y - cy) / (ry + slack)) ** 2 <= 1;
  const inner =
    rx - slack > 0 &&
    ry - slack > 0 &&
    ((p.x - cx) / (rx - slack)) ** 2 + ((p.y - cy) / (ry - slack)) ** 2 <= 1;
  return outer && !inner;
}

/**
 * Topmost shape under the point, in the order things are drawn: spotlights
 * are painted under every other annotation (`drawShapes`), so every other
 * annotation is tried first, newest first, and a spotlight only when nothing
 * drawn over it is hit.
 */
export function pickShape(
  shapes: Shape[],
  p: Point,
  tolerance: number
): Shape | null {
  for (const spotlights of [false, true]) {
    for (let i = shapes.length - 1; i >= 0; i -= 1) {
      if ((shapes[i].kind === 'spotlight') !== spotlights) continue;
      if (hitTestShape(shapes[i], p, tolerance)) return shapes[i];
    }
  }
  return null;
}

export function translateShape(shape: Shape, dx: number, dy: number): Shape {
  if (isSegmentShape(shape)) {
    return {
      ...shape,
      from: { x: shape.from.x + dx, y: shape.from.y + dy },
      to: { x: shape.to.x + dx, y: shape.to.y + dy },
    };
  }
  if (isStrokeShape(shape)) {
    return {
      ...shape,
      points: shape.points.map((p) => ({ x: p.x + dx, y: p.y + dy })),
    };
  }
  if (shape.kind === 'callout') {
    return {
      ...shape,
      at: { x: shape.at.x + dx, y: shape.at.y + dy },
      tail: { x: shape.tail.x + dx, y: shape.tail.y + dy },
    };
  }
  return { ...shape, at: { x: shape.at.x + dx, y: shape.at.y + dy } };
}

/** `tail` is the tip of a callout's pointer, dragged on its own. */
export type SegmentHandle = 'from' | 'to' | 'tail';

/** Grab points shown on the selected shape. */
export function shapeHandles(shape: Shape): Point[] {
  if (isSegmentShape(shape)) return [shape.from, shape.to];
  if (shape.kind === 'callout') return [shape.tail];
  return [];
}

/** Which grab point of the selected shape is under the pointer, if any. */
export function pickSegmentHandle(
  shape: Shape,
  p: Point,
  tolerance: number
): SegmentHandle | null {
  if (shape.kind === 'callout') return distance(p, shape.tail) <= tolerance ? 'tail' : null;
  if (!isSegmentShape(shape)) return null;
  if (distance(p, shape.from) <= tolerance) return 'from';
  if (distance(p, shape.to) <= tolerance) return 'to';
  return null;
}

/** Kinds whose box Shift turns into a square (magnify is always round). */
const SQUARE_KINDS: ReadonlySet<SegmentShapeKind> = new Set<SegmentShapeKind>([
  'rect',
  'ellipse',
  'blur',
  'spotlight',
  'magnify',
]);

/**
 * Shift while drawing: lines and arrows snap to 45 degree steps, boxes become
 * squares. Returns the adjusted end point.
 */
export function constrainPoint(kind: SegmentShapeKind | 'path', from: Point, to: Point): Point {
  const dx = to.x - from.x;
  const dy = to.y - from.y;
  if (kind !== 'path' && SQUARE_KINDS.has(kind)) {
    const side = Math.max(Math.abs(dx), Math.abs(dy));
    return { x: from.x + Math.sign(dx || 1) * side, y: from.y + Math.sign(dy || 1) * side };
  }
  const length = Math.hypot(dx, dy);
  const step = Math.PI / 4;
  const angle = Math.round(Math.atan2(dy, dx) / step) * step;
  return { x: from.x + Math.cos(angle) * length, y: from.y + Math.sin(angle) * length };
}
