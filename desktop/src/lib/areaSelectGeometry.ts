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

export interface Bounds {
  width: number;
  height: number;
}

/** Corner and edge grips, named by the side(s) they move. */
export type ResizeHandle = "n" | "s" | "e" | "w" | "ne" | "nw" | "se" | "sw";

export const RESIZE_HANDLES: readonly ResizeHandle[] = ["nw", "n", "ne", "e", "se", "s", "sw", "w"];

/** Below this a drag is a stray click, and a grip cannot shrink the box past it. */
export const MIN_SELECTION_PX = 8;

export function clampPoint(point: Point, bounds: Bounds): Point {
  return {
    x: Math.max(0, Math.min(bounds.width, point.x)),
    y: Math.max(0, Math.min(bounds.height, point.y)),
  };
}

export function rectFromPoints(a: Point, b: Point): Rect {
  return {
    x: Math.min(a.x, b.x),
    y: Math.min(a.y, b.y),
    width: Math.abs(b.x - a.x),
    height: Math.abs(b.y - a.y),
  };
}

export function isUsableSelection(rect: Rect, min = MIN_SELECTION_PX): boolean {
  return rect.width >= min && rect.height >= min;
}

/** Slide the whole box by a delta without letting any edge leave the display. */
export function moveRect(start: Rect, delta: Point, bounds: Bounds): Rect {
  const maxX = Math.max(0, bounds.width - start.width);
  const maxY = Math.max(0, bounds.height - start.height);
  return {
    ...start,
    x: Math.max(0, Math.min(maxX, start.x + delta.x)),
    y: Math.max(0, Math.min(maxY, start.y + delta.y)),
  };
}

/** Drag one grip. The opposite edge stays put, the dragged edge follows the
 *  pointer, and the box never inverts or collapses below `min`. */
export function resizeRect(
  start: Rect,
  handle: ResizeHandle,
  delta: Point,
  bounds: Bounds,
  min = MIN_SELECTION_PX,
): Rect {
  let left = start.x;
  let top = start.y;
  let right = start.x + start.width;
  let bottom = start.y + start.height;

  if (handle.includes("w")) left = Math.max(0, Math.min(right - min, left + delta.x));
  if (handle.includes("e")) right = Math.min(bounds.width, Math.max(left + min, right + delta.x));
  if (handle.includes("n")) top = Math.max(0, Math.min(bottom - min, top + delta.y));
  if (handle.includes("s")) bottom = Math.min(bounds.height, Math.max(top + min, bottom + delta.y));

  return { x: left, y: top, width: right - left, height: bottom - top };
}

export function roundRect(rect: Rect): Rect {
  return {
    x: Math.round(rect.x),
    y: Math.round(rect.y),
    width: Math.round(rect.width),
    height: Math.round(rect.height),
  };
}

export function handleCursor(handle: ResizeHandle): string {
  switch (handle) {
    case "n":
    case "s":
      return "ns-resize";
    case "e":
    case "w":
      return "ew-resize";
    case "ne":
    case "sw":
      return "nesw-resize";
    case "nw":
    case "se":
      return "nwse-resize";
  }
}

interface DisplayLike {
  id: number;
  x: number;
  y: number;
  width: number;
  height: number;
}

/** The display whose bounds contain `point` (global coordinates), if any. A
 *  point on a shared edge resolves to the first display listed. */
export function displayContaining<D extends DisplayLike>(displays: readonly D[], point: Point | null): D | null {
  if (!point) return null;
  return (
    displays.find(
      (d) => point.x >= d.x && point.x < d.x + d.width && point.y >= d.y && point.y < d.y + d.height,
    ) ?? null
  );
}
