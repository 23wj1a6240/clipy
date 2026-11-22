/**
 * Where a Flare stroke is split into separately drawn sub-paths.
 *
 * Points arrive from a 16 ms cursor poll, so a fast flick across a 2560 px
 * display can legitimately move 200 px or more between consecutive samples.
 * Splitting on distance alone turned those strokes into disconnected dashes
 * with gaps, and any sub-path left with a single point vanished outright. A
 * real break in the stroke is a break in TIME (the cursor left the capture
 * area, so no visible points were emitted) or an explicit off-then-on
 * transition; distance only matters when it is too large to be one motion.
 */
export interface TrailSample {
  readonly x: number;
  readonly y: number;
  /** `performance.now()` when the sample arrived. */
  readonly t: number;
  /** The flare was off between the previous sample and this one. */
  readonly breakBefore?: boolean;
}

/** Longest pause between samples that still reads as one stroke. The poll
 * interval is 16 ms, so anything past this means samples were withheld. */
export const TRAIL_SPLIT_MS = 120;

export const FLARE_IDLE_MS = 2000;
export const FLARE_FADE_MS = 400;

export function flareOpacity(now: number, lastDrawnAt: number): number {
  return Math.max(0, Math.min(1, 1 - (now - lastDrawnAt - FLARE_IDLE_MS) / FLARE_FADE_MS));
}

/** A jump larger than this fraction of the longer viewport edge is a display
 * hop, whatever the timing says. */
export const TRAIL_SPLIT_VIEWPORT_FRACTION = 0.5;

export function trailSplitDistance(viewportWidth: number, viewportHeight: number): number {
  return Math.max(viewportWidth, viewportHeight) * TRAIL_SPLIT_VIEWPORT_FRACTION;
}

export function isTrailBreak(
  previous: TrailSample,
  next: TrailSample,
  splitDistance: number,
): boolean {
  if (next.breakBefore) return true;
  if (next.t - previous.t > TRAIL_SPLIT_MS) return true;
  return Math.hypot(next.x - previous.x, next.y - previous.y) > splitDistance;
}

/** Include single-point strokes so a click can leave an annotation dot. */
export function splitTrail<T extends TrailSample>(points: readonly T[], splitDistance: number): T[][] {
  const subs: T[][] = [];
  let current: T[] = [];
  for (const point of points) {
    const last = current.length > 0 ? current[current.length - 1] : undefined;
    if (last && isTrailBreak(last, point, splitDistance)) {
      if (current.length > 0) subs.push(current);
      current = [];
    }
    current.push(point);
  }
  if (current.length > 0) subs.push(current);
  return subs;
}

/** A point as drawn: colour and pen width as they were when it was laid. */
export interface StrokePoint {
  readonly x: number;
  readonly y: number;
  readonly color?: string;
  readonly width?: number;
}

/** Strokes one sub-path from `splitTrail`. Shared by the recording overlay
 * and the What's new practice area, so practice looks like the real thing. */
export function strokeFlarePath(ctx: CanvasRenderingContext2D, points: readonly StrokePoint[]) {
  if (!points.length) return;
  ctx.strokeStyle = points[0].color ?? "#ff4d29";
  ctx.fillStyle = ctx.strokeStyle;
  const width = points[0].width ?? 6;
  ctx.lineWidth = width;
  if (points.length === 1) {
    ctx.beginPath();
    ctx.arc(points[0].x, points[0].y, width / 2, 0, Math.PI * 2);
    ctx.fill();
    return;
  }
  const path = new Path2D();
  path.moveTo(points[0].x, points[0].y);
  for (let i = 1; i < points.length - 1; i += 1) {
    path.quadraticCurveTo(points[i].x, points[i].y,
      (points[i].x + points[i + 1].x) / 2, (points[i].y + points[i + 1].y) / 2);
  }
  const last = points[points.length - 1];
  path.lineTo(last.x, last.y);
  ctx.stroke(path);
}
