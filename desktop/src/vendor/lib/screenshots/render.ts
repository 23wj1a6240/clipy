// SPDX-License-Identifier: Apache-2.0
// Copied from the Clipy monorepo (lib/screenshots/render.ts) by the export; the desktop app shares it.
/**
 * Canvas renderer for annotation scenes.
 *
 * The same code paints the live editor canvas and the exported PNG, so what
 * someone drags on screen is exactly what lands on the CDN. Callers pass a 2D
 * context; nothing here touches React or the DOM beyond creating scratch
 * canvases.
 */

import {
  autoColorsFromPixels,
  backdropIsNoop,
  gradientLine,
  gradientPreset,
  layoutBackdrop,
  meshEllipse,
  meshPreset,
  type AutoColors,
} from './backdrop';
import { calloutBubble, labelPadding, rectFromPoints } from './geometry';
import {
  stepNumber,
  type Backdrop,
  type CalloutShape,
  type Cut,
  type EmojiShape,
  type Rect,
  type SegmentShape,
  type Shape,
  type StrokeShape,
  type TextShape,
} from './types';
import type { AnnotationScene } from './types';

/**
 * Natural pixel size of the source image. Passed explicitly rather than read
 * off the element: `HTMLImageElement.width` is the *layout* width once the node
 * is in the DOM, which would silently rescale every export.
 */
export interface ImageSize {
  width: number;
  height: number;
}

type Canvas2D = CanvasRenderingContext2D;

const FONT_STACK =
  'system-ui, -apple-system, "Segoe UI", Roboto, Helvetica, Arial, sans-serif';
const EMOJI_FONT_STACK =
  '"Apple Color Emoji", "Segoe UI Emoji", "Noto Color Emoji", "Twemoji Mozilla", sans-serif';
/** How dark everything outside a spotlight goes. */
const SPOTLIGHT_DIM = 'rgba(0, 0, 0, 0.6)';

/** Black or white, whichever reads on a fill of this color. */
export function readableTextOn(hex: string): string {
  return isLightColor(hex) ? '#111111' : '#ffffff';
}

/** Perceived brightness, used to pick a readable text outline. */
function isLightColor(hex: string): boolean {
  const value = hex.replace('#', '');
  const full =
    value.length === 3
      ? value.split('').map((c) => c + c).join('')
      : value.padEnd(6, '0');
  const r = parseInt(full.slice(0, 2), 16);
  const g = parseInt(full.slice(2, 4), 16);
  const b = parseInt(full.slice(4, 6), 16);
  return (r * 299 + g * 587 + b * 114) / 1000 > 150;
}

function drawArrowHead(ctx: Canvas2D, shape: SegmentShape, reverse = false): void {
  const { strokeWidth } = shape;
  const from = reverse ? shape.to : shape.from;
  const to = reverse ? shape.from : shape.to;
  const angle = Math.atan2(to.y - from.y, to.x - from.x);
  const length = Math.hypot(to.x - from.x, to.y - from.y);
  const head = Math.min(Math.max(strokeWidth * 4, 12), Math.max(length, 1));
  const spread = Math.PI / 7;

  ctx.beginPath();
  ctx.moveTo(to.x, to.y);
  ctx.lineTo(
    to.x - head * Math.cos(angle - spread),
    to.y - head * Math.sin(angle - spread)
  );
  ctx.lineTo(
    to.x - head * Math.cos(angle + spread),
    to.y - head * Math.sin(angle + spread)
  );
  ctx.closePath();
  ctx.fillStyle = shape.color;
  ctx.fill();
}

/**
 * A rect in whole pixels, clipped to the canvas. Scratch canvases are sized
 * from it, so a stored shape far off the image can never make the browser
 * allocate a huge canvas.
 */
function clipToCanvas(ctx: Canvas2D, rect: Rect): Rect {
  const x0 = Math.max(0, Math.round(rect.x));
  const y0 = Math.max(0, Math.round(rect.y));
  const x1 = Math.min(ctx.canvas.width, Math.round(rect.x + rect.width));
  const y1 = Math.min(ctx.canvas.height, Math.round(rect.y + rect.height));
  return { x: x0, y: y0, width: Math.max(0, x1 - x0), height: Math.max(0, y1 - y0) };
}

/**
 * Redaction. Pixelates by downsampling the already-painted pixels underneath
 * and blowing them back up with smoothing off — a mosaic that survives being
 * re-encoded, unlike a CSS-style blur which can sometimes be reversed.
 */
function drawPixelate(ctx: Canvas2D, rect: Rect): void {
  const { x, y, width, height } = clipToCanvas(ctx, rect);
  if (width < 2 || height < 2) return;

  const blockSize = Math.max(6, Math.round(Math.min(width, height) / 12));
  const smallWidth = Math.max(1, Math.round(width / blockSize));
  const smallHeight = Math.max(1, Math.round(height / blockSize));

  const scratch = document.createElement('canvas');
  scratch.width = smallWidth;
  scratch.height = smallHeight;
  const scratchCtx = scratch.getContext('2d');
  if (!scratchCtx) return;

  scratchCtx.imageSmoothingEnabled = true;
  scratchCtx.drawImage(ctx.canvas, x, y, width, height, 0, 0, smallWidth, smallHeight);

  const previousSmoothing = ctx.imageSmoothingEnabled;
  ctx.imageSmoothingEnabled = false;
  ctx.drawImage(scratch, 0, 0, smallWidth, smallHeight, x, y, width, height);
  ctx.imageSmoothingEnabled = previousSmoothing;
}

/**
 * Redaction by blurring: the region is shrunk to a few pixels and smoothly
 * scaled back up in two steps. Like the mosaic it throws the detail away
 * rather than hiding it, so it cannot be undone, and it needs no
 * `ctx.filter`, which Safari's canvas (and so the Mac app) does not support.
 */
function drawSmoothBlur(ctx: Canvas2D, rect: Rect): void {
  const { x, y, width, height } = clipToCanvas(ctx, rect);
  if (width < 2 || height < 2) return;

  const factor = Math.max(8, Math.round(Math.min(width, height) / 6));
  const tinyWidth = Math.max(1, Math.round(width / factor));
  const tinyHeight = Math.max(1, Math.round(height / factor));
  const midWidth = Math.max(tinyWidth, Math.round(width / Math.max(2, factor / 4)));
  const midHeight = Math.max(tinyHeight, Math.round(height / Math.max(2, factor / 4)));

  const tiny = document.createElement('canvas');
  tiny.width = tinyWidth;
  tiny.height = tinyHeight;
  const tinyCtx = tiny.getContext('2d');
  const mid = document.createElement('canvas');
  mid.width = midWidth;
  mid.height = midHeight;
  const midCtx = mid.getContext('2d');
  if (!tinyCtx || !midCtx) return;

  tinyCtx.imageSmoothingEnabled = true;
  tinyCtx.drawImage(ctx.canvas, x, y, width, height, 0, 0, tinyWidth, tinyHeight);
  midCtx.imageSmoothingEnabled = true;
  midCtx.drawImage(tiny, 0, 0, tinyWidth, tinyHeight, 0, 0, midWidth, midHeight);

  const previousSmoothing = ctx.imageSmoothingEnabled;
  ctx.imageSmoothingEnabled = true;
  ctx.drawImage(mid, 0, 0, midWidth, midHeight, x, y, width, height);
  ctx.imageSmoothingEnabled = previousSmoothing;
}

/** A rounded-rectangle path made of lines and arcs only (no `roundRect`). */
export function roundRectPath(ctx: Canvas2D, rect: Rect, radius: number): void {
  const r = Math.max(0, Math.min(radius, rect.width / 2, rect.height / 2));
  const { x, y, width, height } = rect;
  ctx.moveTo(x + r, y);
  ctx.lineTo(x + width - r, y);
  if (r) ctx.arc(x + width - r, y + r, r, -Math.PI / 2, 0);
  ctx.lineTo(x + width, y + height - r);
  if (r) ctx.arc(x + width - r, y + height - r, r, 0, Math.PI / 2);
  ctx.lineTo(x + r, y + height);
  if (r) ctx.arc(x + r, y + height - r, r, Math.PI / 2, Math.PI);
  ctx.lineTo(x, y + r);
  if (r) ctx.arc(x + r, y + r, r, Math.PI, Math.PI * 1.5);
  ctx.closePath();
}

/**
 * A text measurer backed by a scratch canvas, using the exact font the
 * renderer draws annotation text with. Null where canvas text measurement is
 * unavailable.
 */
export function createCanvasTextMeasurer(): ((line: string, fontSize: number) => number) | null {
  try {
    const ctx = document.createElement('canvas').getContext('2d');
    if (!ctx || typeof ctx.measureText !== 'function') return null;
    return (line, fontSize) => {
      ctx.font = `600 ${fontSize}px ${FONT_STACK}`;
      return ctx.measureText(line).width;
    };
  } catch {
    return null;
  }
}

/** Real text width when the context can measure, the shared estimate otherwise. */
function measureLines(ctx: Canvas2D, lines: string[], fontSize: number): number {
  if (typeof ctx.measureText === 'function') {
    return lines.reduce((max, line) => Math.max(max, ctx.measureText(line).width), 0);
  }
  return lines.reduce((max, line) => Math.max(max, line.length), 1) * fontSize * 0.58;
}

/**
 * A round lens over the box showing what is under its centre, enlarged. Draws
 * from the pixels already painted, so it magnifies earlier annotations too.
 */
function drawMagnifier(ctx: Canvas2D, shape: SegmentShape): void {
  const rect = rectFromPoints(shape.from, shape.to);
  const radius = Math.max(rect.width, rect.height) / 2;
  if (radius < 4) return;
  const cx = rect.x + rect.width / 2;
  const cy = rect.y + rect.height / 2;
  const zoom = shape.zoom ?? 2;
  const sourceRadius = radius / zoom;
  const sx = Math.round(cx - sourceRadius);
  const sy = Math.round(cy - sourceRadius);
  const side = Math.max(1, Math.round(sourceRadius * 2));
  // Only the part of the source square that is on the canvas is copied, into
  // the same place on a side x side scratch; the rest stays transparent.
  const source = clipToCanvas(ctx, { x: sx, y: sy, width: side, height: side });
  if (source.width < 1 || source.height < 1) return;

  const scratch = document.createElement('canvas');
  scratch.width = Math.min(side, ctx.canvas.width);
  scratch.height = Math.min(side, ctx.canvas.height);
  const scratchCtx = scratch.getContext('2d');
  if (!scratchCtx) return;
  const ratioX = scratch.width / side;
  const ratioY = scratch.height / side;
  scratchCtx.drawImage(
    ctx.canvas,
    source.x,
    source.y,
    source.width,
    source.height,
    (source.x - sx) * ratioX,
    (source.y - sy) * ratioY,
    source.width * ratioX,
    source.height * ratioY
  );

  ctx.save();
  ctx.beginPath();
  ctx.arc(cx, cy, radius, 0, Math.PI * 2);
  ctx.closePath();
  if (typeof ctx.clip === 'function') ctx.clip();
  ctx.imageSmoothingEnabled = true;
  ctx.drawImage(scratch, 0, 0, scratch.width, scratch.height, cx - radius, cy - radius, radius * 2, radius * 2);
  ctx.restore();

  const ring = Math.max(3, shape.strokeWidth);
  ctx.beginPath();
  ctx.arc(cx, cy, radius, 0, Math.PI * 2);
  ctx.lineWidth = ring + Math.max(2, ring * 0.5);
  ctx.strokeStyle = 'rgba(255,255,255,0.95)';
  ctx.stroke();
  ctx.lineWidth = ring;
  ctx.strokeStyle = shape.color;
  ctx.stroke();
}

/**
 * Dims everything outside every spotlight in one pass. The dim layer is built
 * on its own canvas and each spotlight is cut out of it (`destination-out`),
 * so overlapping or nested spotlights clear their union. An even-odd fill
 * would re-darken where two of them overlap, exactly the area both point at.
 */
function drawSpotlights(ctx: Canvas2D, spots: SegmentShape[]): void {
  if (spots.length === 0) return;
  const { width, height } = ctx.canvas;
  const layer = document.createElement('canvas');
  layer.width = width;
  layer.height = height;
  const layerCtx = layer.getContext('2d');
  if (!layerCtx) return;
  layerCtx.fillStyle = SPOTLIGHT_DIM;
  layerCtx.fillRect(0, 0, width, height);
  layerCtx.globalCompositeOperation = 'destination-out';
  layerCtx.fillStyle = '#000000';
  for (const spot of spots) {
    const rect = rectFromPoints(spot.from, spot.to);
    layerCtx.beginPath();
    roundRectPath(layerCtx, rect, Math.min(rect.width, rect.height) * 0.08);
    layerCtx.fill();
  }
  ctx.drawImage(layer, 0, 0);
}

function dashFor(ctx: Canvas2D, shape: Shape): void {
  if (shape.dashed) ctx.setLineDash([shape.strokeWidth * 2.5, shape.strokeWidth * 2]);
}

function fillBox(ctx: Canvas2D, shape: SegmentShape): void {
  if (!shape.fill || shape.fill === 'outline') return;
  ctx.save();
  ctx.globalAlpha = shape.fill === 'tint' ? 0.25 : 1;
  ctx.fill();
  ctx.restore();
}

function drawPath(ctx: Canvas2D, shape: StrokeShape): void {
  dashFor(ctx, shape);
  ctx.beginPath();
  ctx.moveTo(shape.points[0].x, shape.points[0].y);
  for (const p of shape.points.slice(1)) ctx.lineTo(p.x, p.y);
  if (shape.closed) ctx.closePath();
  ctx.stroke();
}

function drawEmoji(ctx: Canvas2D, shape: EmojiShape): void {
  ctx.font = `${shape.size}px ${EMOJI_FONT_STACK}`;
  ctx.textAlign = 'center';
  ctx.textBaseline = 'middle';
  ctx.fillStyle = '#000000';
  ctx.fillText(shape.emoji, shape.at.x, shape.at.y);
  ctx.textAlign = 'start';
  ctx.textBaseline = 'alphabetic';
}

function drawLabelText(ctx: Canvas2D, shape: TextShape): void {
  ctx.font = `600 ${shape.fontSize}px ${FONT_STACK}`;
  ctx.textBaseline = 'top';
  const lines = shape.text.split('\n');
  const lineHeight = shape.fontSize * 1.25;
  const pad = labelPadding(shape.fontSize);
  const width = measureLines(ctx, lines, shape.fontSize);
  ctx.beginPath();
  roundRectPath(
    ctx,
    {
      x: shape.at.x - pad,
      y: shape.at.y - pad,
      width: width + pad * 2,
      height: lines.length * lineHeight + pad * 2,
    },
    pad * 0.9
  );
  ctx.fillStyle = shape.color;
  ctx.fill();
  ctx.fillStyle = isLightColor(shape.color) ? '#111111' : '#ffffff';
  lines.forEach((line, index) => ctx.fillText(line, shape.at.x, shape.at.y + index * lineHeight));
}

function drawCallout(ctx: Canvas2D, shape: CalloutShape): void {
  ctx.font = `600 ${shape.fontSize}px ${FONT_STACK}`;
  ctx.textBaseline = 'top';
  const lines = shape.text.split('\n');
  const lineHeight = shape.fontSize * 1.25;
  const bubble = calloutBubble(shape, measureLines(ctx, lines, shape.fontSize));
  const cx = bubble.x + bubble.width / 2;
  const cy = bubble.y + bubble.height / 2;
  const angle = Math.atan2(shape.tail.y - cy, shape.tail.x - cx);
  // Half the width of the tail where it leaves the bubble.
  const base = Math.min(bubble.height / 2, shape.fontSize * 0.5);
  const nx = -Math.sin(angle) * base;
  const ny = Math.cos(angle) * base;

  ctx.save();
  ctx.shadowColor = 'rgba(0, 0, 0, 0.28)';
  ctx.shadowBlur = shape.fontSize * 0.5;
  ctx.shadowOffsetY = shape.fontSize * 0.12;
  ctx.fillStyle = shape.color;
  // Tail first, then the bubble over its root, so the two read as one shape.
  ctx.beginPath();
  ctx.moveTo(cx + nx, cy + ny);
  ctx.lineTo(shape.tail.x, shape.tail.y);
  ctx.lineTo(cx - nx, cy - ny);
  ctx.closePath();
  ctx.fill();
  ctx.beginPath();
  roundRectPath(ctx, bubble, Math.min(bubble.height / 2, shape.fontSize * 0.7));
  ctx.fill();
  ctx.restore();

  ctx.fillStyle = isLightColor(shape.color) ? '#111111' : '#ffffff';
  lines.forEach((line, index) => ctx.fillText(line, shape.at.x, shape.at.y + index * lineHeight));
}

function drawText(ctx: Canvas2D, shape: Extract<Shape, { kind: 'text' }>): void {
  if (shape.background) {
    drawLabelText(ctx, shape);
    return;
  }
  ctx.font = `600 ${shape.fontSize}px ${FONT_STACK}`;
  ctx.textBaseline = 'top';
  ctx.lineJoin = 'round';
  ctx.lineWidth = Math.max(2, shape.fontSize * 0.18);
  ctx.strokeStyle = isLightColor(shape.color) ? 'rgba(0,0,0,0.65)' : 'rgba(255,255,255,0.9)';
  ctx.fillStyle = shape.color;

  const lineHeight = shape.fontSize * 1.25;
  shape.text.split('\n').forEach((line, index) => {
    const y = shape.at.y + index * lineHeight;
    ctx.strokeText(line, shape.at.x, y);
    ctx.fillText(line, shape.at.x, y);
  });
}

function drawStep(
  ctx: Canvas2D,
  shape: Extract<Shape, { kind: 'step' }>,
  label: number
): void {
  ctx.beginPath();
  ctx.arc(shape.at.x, shape.at.y, shape.radius, 0, Math.PI * 2);
  ctx.fillStyle = shape.color;
  ctx.fill();
  ctx.lineWidth = Math.max(2, shape.radius * 0.12);
  ctx.strokeStyle = 'rgba(255,255,255,0.92)';
  ctx.stroke();

  ctx.font = `700 ${shape.radius * 1.15}px ${FONT_STACK}`;
  ctx.textAlign = 'center';
  ctx.textBaseline = 'middle';
  ctx.fillStyle = isLightColor(shape.color) ? '#111111' : '#ffffff';
  ctx.fillText(String(label), shape.at.x, shape.at.y + shape.radius * 0.04);
  ctx.textAlign = 'start';
  ctx.textBaseline = 'alphabetic';
}

/** Draws one shape onto an already-painted context. */
export function drawShape(ctx: Canvas2D, shape: Shape, label = 1): void {
  ctx.save();
  ctx.strokeStyle = shape.color;
  ctx.fillStyle = shape.color;
  ctx.lineWidth = shape.strokeWidth;
  ctx.lineCap = 'round';
  ctx.lineJoin = 'round';

  switch (shape.kind) {
    case 'line':
    case 'arrow': {
      dashFor(ctx, shape);
      ctx.beginPath();
      ctx.moveTo(shape.from.x, shape.from.y);
      ctx.lineTo(shape.to.x, shape.to.y);
      ctx.stroke();
      ctx.setLineDash([]);
      if (shape.kind === 'arrow') {
        drawArrowHead(ctx, shape);
        if (shape.heads === 'both') drawArrowHead(ctx, shape, true);
      }
      break;
    }
    case 'rect': {
      const rect = rectFromPoints(shape.from, shape.to);
      if (shape.fill && shape.fill !== 'outline') {
        ctx.beginPath();
        ctx.rect(rect.x, rect.y, rect.width, rect.height);
        fillBox(ctx, shape);
        if (shape.fill === 'solid') break;
      }
      dashFor(ctx, shape);
      ctx.strokeRect(rect.x, rect.y, rect.width, rect.height);
      break;
    }
    case 'ellipse': {
      const rect = rectFromPoints(shape.from, shape.to);
      if (rect.width < 1 || rect.height < 1) break;
      ctx.beginPath();
      ctx.ellipse(
        rect.x + rect.width / 2,
        rect.y + rect.height / 2,
        rect.width / 2,
        rect.height / 2,
        0,
        0,
        Math.PI * 2
      );
      fillBox(ctx, shape);
      if (shape.fill === 'solid') break;
      dashFor(ctx, shape);
      ctx.stroke();
      break;
    }
    case 'blur': {
      const rect = rectFromPoints(shape.from, shape.to);
      if (shape.redact === 'solid') ctx.fillRect(rect.x, rect.y, rect.width, rect.height);
      else if (shape.redact === 'blur') drawSmoothBlur(ctx, rect);
      else drawPixelate(ctx, rect);
      break;
    }
    case 'magnify':
      drawMagnifier(ctx, shape);
      break;
    case 'spotlight':
      // Painted by `drawShapes` in one pass for all spotlights.
      break;
    case 'path':
      drawPath(ctx, shape);
      break;
    case 'emoji':
      drawEmoji(ctx, shape);
      break;
    case 'callout':
      drawCallout(ctx, shape);
      break;
    case 'pen':
    case 'highlight': {
      if (shape.kind === 'highlight') {
        ctx.globalAlpha = 0.35;
        ctx.lineCap = 'butt';
      }
      ctx.beginPath();
      ctx.moveTo(shape.points[0].x, shape.points[0].y);
      for (const p of shape.points.slice(1)) ctx.lineTo(p.x, p.y);
      ctx.stroke();
      break;
    }
    case 'text':
      drawText(ctx, shape);
      break;
    case 'step':
      drawStep(ctx, shape, label);
      break;
  }

  ctx.restore();
}

export function drawShapes(ctx: Canvas2D, shapes: Shape[]): void {
  // Spotlights dim the screenshot itself, so they go down before any mark:
  // an arrow pointing into the bright area stays bright.
  drawSpotlights(
    ctx,
    shapes.filter((shape): shape is SegmentShape => shape.kind === 'spotlight')
  );
  for (const shape of shapes) {
    if (shape.kind === 'spotlight') continue;
    drawShape(ctx, shape, shape.kind === 'step' ? stepNumber(shapes, shape.id) : 1);
  }
}

/**
 * Paints image + annotations at full resolution. Crop is NOT applied here —
 * the editor shows the whole frame with a dimmed overlay so the crop stays
 * adjustable; `flattenScene` is what bakes it.
 */
export function renderScene(
  ctx: Canvas2D,
  image: CanvasImageSource,
  size: ImageSize,
  scene: AnnotationScene
): void {
  ctx.clearRect(0, 0, ctx.canvas.width, ctx.canvas.height);
  ctx.drawImage(image, 0, 0, size.width, size.height);
  drawShapes(ctx, scene.shapes);
}

/** Dims everything outside the crop and outlines the kept region. */
export function drawCropOverlay(ctx: Canvas2D, crop: Rect, lineWidth: number): void {
  const { width, height } = ctx.canvas;
  ctx.save();
  ctx.fillStyle = 'rgba(9, 9, 11, 0.55)';
  ctx.beginPath();
  ctx.rect(0, 0, width, height);
  ctx.rect(crop.x, crop.y, crop.width, crop.height);
  ctx.fill('evenodd');
  ctx.strokeStyle = '#ffffff';
  ctx.lineWidth = lineWidth;
  ctx.setLineDash([lineWidth * 4, lineWidth * 3]);
  ctx.strokeRect(crop.x, crop.y, crop.width, crop.height);
  ctx.restore();
}

/** Marching-ants box around the selected shape, plus endpoint handles. */
export function drawSelection(
  ctx: Canvas2D,
  bounds: Rect,
  handles: { x: number; y: number }[],
  lineWidth: number
): void {
  ctx.save();
  ctx.strokeStyle = '#0a84ff';
  ctx.lineWidth = lineWidth;
  ctx.setLineDash([lineWidth * 3, lineWidth * 2]);
  const pad = lineWidth * 3;
  ctx.strokeRect(
    bounds.x - pad,
    bounds.y - pad,
    bounds.width + pad * 2,
    bounds.height + pad * 2
  );
  ctx.setLineDash([]);
  for (const handle of handles) {
    ctx.beginPath();
    ctx.arc(handle.x, handle.y, lineWidth * 3, 0, Math.PI * 2);
    ctx.fillStyle = '#ffffff';
    ctx.fill();
    ctx.strokeStyle = '#0a84ff';
    ctx.stroke();
  }
  ctx.restore();
}

/**
 * Bakes image + annotations + crop into a fresh canvas ready for `toBlob`.
 * Annotations are drawn before cropping so a blur that overhangs the crop edge
 * still redacts the pixels that survive.
 */
export function flattenScene(
  image: CanvasImageSource,
  size: ImageSize,
  scene: AnnotationScene
): HTMLCanvasElement {
  const full = document.createElement('canvas');
  full.width = size.width;
  full.height = size.height;
  const ctx = full.getContext('2d');
  if (!ctx) throw new Error('Canvas 2D is unavailable in this browser.');
  renderScene(ctx, image, size, scene);

  const content = applyCuts(applyCrop(full, scene.crop), scene.cuts ?? [], scene.crop);
  if (!scene.backdrop || backdropIsNoop(scene.backdrop)) return content;
  // Auto colors come from the original screenshot, as in the editor's
  // preview, not from the annotated copy: a spotlight or a dark redaction
  // would otherwise darken the background compared with what was shown.
  const needsAuto =
    scene.backdrop.fill === 'auto' || (scene.backdrop.inset > 0 && !scene.backdrop.insetColor);
  const auto = needsAuto ? sampleAutoColors(image, size.width, size.height) : undefined;
  return composeBackdrop(content, scene.backdrop, auto);
}

/**
 * Cut ranges on one axis, moved into the cropped image's coordinates,
 * clipped to it, and merged where they overlap. Kept ranges are what is left.
 */
export function keptRanges(
  cuts: Cut[],
  axis: Cut['axis'],
  offset: number,
  length: number
): { from: number; to: number }[] {
  const removed = cuts
    .filter((cut) => cut.axis === axis)
    .map((cut) => ({
      from: Math.max(0, Math.round(cut.from - offset)),
      to: Math.min(length, Math.round(cut.to - offset)),
    }))
    .filter((range) => range.to > range.from)
    .sort((a, b) => a.from - b.from);
  const kept: { from: number; to: number }[] = [];
  let cursor = 0;
  for (const range of removed) {
    if (range.from > cursor) kept.push({ from: cursor, to: range.from });
    cursor = Math.max(cursor, range.to);
  }
  if (cursor < length) kept.push({ from: cursor, to: length });
  return kept;
}

/**
 * Removes the cut strips and joins what is left. Rows and columns are cut
 * independently; a cut that would remove the whole image is ignored.
 */
function applyCuts(source: HTMLCanvasElement, cuts: Cut[], crop: Rect | null): HTMLCanvasElement {
  if (cuts.length === 0) return source;
  const offsetX = crop ? Math.max(0, Math.round(crop.x)) : 0;
  const offsetY = crop ? Math.max(0, Math.round(crop.y)) : 0;
  const rows = keptRanges(cuts, 'rows', offsetY, source.height);
  const columns = keptRanges(cuts, 'columns', offsetX, source.width);
  const height = rows.reduce((sum, r) => sum + r.to - r.from, 0);
  const width = columns.reduce((sum, c) => sum + c.to - c.from, 0);
  if (width < 1 || height < 1) return source;
  if (width === source.width && height === source.height) return source;

  const out = document.createElement('canvas');
  out.width = width;
  out.height = height;
  const ctx = out.getContext('2d');
  if (!ctx) throw new Error('Canvas 2D is unavailable in this browser.');
  let y = 0;
  for (const row of rows) {
    let x = 0;
    const h = row.to - row.from;
    for (const column of columns) {
      const w = column.to - column.from;
      ctx.drawImage(source, column.from, row.from, w, h, x, y, w, h);
      x += w;
    }
    y += h;
  }
  return out;
}

/** Hatched bands over strips that will be cut, with dashed edges. */
export function drawCutOverlay(ctx: Canvas2D, cuts: Cut[], lineWidth: number): void {
  const { width, height } = ctx.canvas;
  ctx.save();
  for (const cut of cuts) {
    const rect =
      cut.axis === 'rows'
        ? { x: 0, y: cut.from, width, height: cut.to - cut.from }
        : { x: cut.from, y: 0, width: cut.to - cut.from, height };
    ctx.fillStyle = 'rgba(255, 59, 48, 0.28)';
    ctx.fillRect(rect.x, rect.y, rect.width, rect.height);
    ctx.strokeStyle = '#ff3b30';
    ctx.lineWidth = lineWidth;
    ctx.setLineDash([lineWidth * 4, lineWidth * 3]);
    ctx.beginPath();
    if (cut.axis === 'rows') {
      ctx.moveTo(0, cut.from);
      ctx.lineTo(width, cut.from);
      ctx.moveTo(0, cut.to);
      ctx.lineTo(width, cut.to);
    } else {
      ctx.moveTo(cut.from, 0);
      ctx.lineTo(cut.from, height);
      ctx.moveTo(cut.to, 0);
      ctx.lineTo(cut.to, height);
    }
    ctx.stroke();
  }
  ctx.restore();
}

function applyCrop(full: HTMLCanvasElement, crop: Rect | null): HTMLCanvasElement {
  if (!crop) return full;

  const x = Math.max(0, Math.round(crop.x));
  const y = Math.max(0, Math.round(crop.y));
  const width = Math.min(Math.round(crop.width), full.width - x);
  const height = Math.min(Math.round(crop.height), full.height - y);
  if (width < 1 || height < 1) return full;

  const cropped = document.createElement('canvas');
  cropped.width = width;
  cropped.height = height;
  const croppedCtx = cropped.getContext('2d');
  if (!croppedCtx) throw new Error('Canvas 2D is unavailable in this browser.');
  croppedCtx.drawImage(full, x, y, width, height, 0, 0, width, height);
  return cropped;
}

const FALLBACK_AUTO: AutoColors = { edge: '#ffffff', from: '#c7d2fe', to: '#4338ca' };

/**
 * Samples a 24px thumbnail of the image for the `auto` background and inset
 * colours. Any failure (a tainted canvas, a test stub) falls back to a neutral
 * indigo rather than failing the export.
 */
export function sampleAutoColors(image: CanvasImageSource, width: number, height: number): AutoColors {
  try {
    const side = 24;
    const scratch = document.createElement('canvas');
    scratch.width = side;
    scratch.height = side;
    const ctx = scratch.getContext('2d', { willReadFrequently: true } as CanvasRenderingContext2DSettings);
    if (!ctx || typeof ctx.getImageData !== 'function') return FALLBACK_AUTO;
    ctx.drawImage(image, 0, 0, width, height, 0, 0, side, side);
    return autoColorsFromPixels(ctx.getImageData(0, 0, side, side).data, side, side);
  } catch {
    return FALLBACK_AUTO;
  }
}

function transparentOf(hex: string): string {
  const value = hex.replace('#', '');
  const full = value.length === 3 ? value.split('').map((c) => c + c).join('') : value;
  const r = parseInt(full.slice(0, 2), 16);
  const g = parseInt(full.slice(2, 4), 16);
  const b = parseInt(full.slice(4, 6), 16);
  return `rgba(${r}, ${g}, ${b}, 0)`;
}

/** The screenshot itself, scaled to cover the background and heavily blurred. */
function paintBlurredScreenshot(ctx: Canvas2D, content: HTMLCanvasElement, width: number, height: number): void {
  const scale = Math.max(width / content.width, height / content.height);
  const coverW = content.width * scale;
  const coverH = content.height * scale;
  const tinyW = Math.max(1, Math.round(width / 48));
  const tinyH = Math.max(1, Math.round(height / 48));
  const tiny = document.createElement('canvas');
  tiny.width = tinyW;
  tiny.height = tinyH;
  const tinyCtx = tiny.getContext('2d');
  const mid = document.createElement('canvas');
  mid.width = tinyW * 4;
  mid.height = tinyH * 4;
  const midCtx = mid.getContext('2d');
  if (!tinyCtx || !midCtx) return;
  tinyCtx.imageSmoothingEnabled = true;
  tinyCtx.drawImage(
    content,
    0,
    0,
    content.width,
    content.height,
    ((width - coverW) / 2) * (tinyW / width),
    ((height - coverH) / 2) * (tinyH / height),
    coverW * (tinyW / width),
    coverH * (tinyH / height)
  );
  midCtx.imageSmoothingEnabled = true;
  midCtx.drawImage(tiny, 0, 0, tinyW, tinyH, 0, 0, mid.width, mid.height);
  ctx.imageSmoothingEnabled = true;
  ctx.drawImage(mid, 0, 0, mid.width, mid.height, 0, 0, width, height);
  // A light wash keeps the card readable over a busy screenshot.
  ctx.fillStyle = 'rgba(255, 255, 255, 0.12)';
  ctx.fillRect(0, 0, width, height);
}

/** A small seeded random generator, so grain is the same on every export. */
function mulberry32(seed: number): () => number {
  let a = seed >>> 0;
  return () => {
    a = (a + 0x6d2b79f5) >>> 0;
    let t = a;
    t = Math.imul(t ^ (t >>> 15), t | 1);
    t ^= t + Math.imul(t ^ (t >>> 7), t | 61);
    return ((t ^ (t >>> 14)) >>> 0) / 4294967296;
  };
}

/** Monochrome film grain over the background, tiled from one seeded tile. */
function paintGrain(ctx: Canvas2D, width: number, height: number, amount: number): void {
  if (amount <= 0 || typeof ctx.createPattern !== 'function') return;
  const side = 128;
  const tile = document.createElement('canvas');
  tile.width = side;
  tile.height = side;
  const tileCtx = tile.getContext('2d');
  if (!tileCtx || typeof tileCtx.createImageData !== 'function') return;
  const image = tileCtx.createImageData(side, side);
  const random = mulberry32(0xc1197);
  const alpha = Math.round(amount * 90);
  for (let i = 0; i < image.data.length; i += 4) {
    const v = random() < 0.5 ? 0 : 255;
    image.data[i] = v;
    image.data[i + 1] = v;
    image.data[i + 2] = v;
    image.data[i + 3] = Math.round(random() * alpha);
  }
  tileCtx.putImageData(image, 0, 0);
  const pattern = ctx.createPattern(tile, 'repeat');
  if (!pattern) return;
  ctx.fillStyle = pattern;
  ctx.fillRect(0, 0, width, height);
}

function paintBackground(
  ctx: Canvas2D,
  fill: string,
  width: number,
  height: number,
  auto: AutoColors,
  content: HTMLCanvasElement
): void {
  if (fill === 'none') return;
  if (fill === 'blur') {
    paintBlurredScreenshot(ctx, content, width, height);
    return;
  }
  const mesh = meshPreset(fill);
  if (mesh) {
    ctx.fillStyle = mesh.base;
    ctx.fillRect(0, 0, width, height);
    for (const blob of mesh.blobs) {
      const { cx, cy, rx, ry } = meshEllipse(blob, width, height);
      ctx.save();
      ctx.translate(cx, cy);
      ctx.scale(rx, ry);
      const gradient = ctx.createRadialGradient(0, 0, 0, 0, 0, 1);
      gradient.addColorStop(0, blob.color);
      gradient.addColorStop(0.5, transparentOf(blob.color));
      gradient.addColorStop(1, transparentOf(blob.color));
      ctx.fillStyle = gradient;
      ctx.fillRect(-cx / rx, -cy / ry, width / rx, height / ry);
      ctx.restore();
    }
    return;
  }
  const preset = gradientPreset(fill);
  const stops = preset ? preset.stops : fill === 'auto' ? [auto.from, auto.to] : null;
  if (!stops) {
    ctx.fillStyle = fill;
    ctx.fillRect(0, 0, width, height);
    return;
  }
  const line = gradientLine(preset ? preset.angle : 135, width, height);
  const gradient = ctx.createLinearGradient(line.x0, line.y0, line.x1, line.y1);
  stops.forEach((stop, index) => gradient.addColorStop(index / Math.max(1, stops.length - 1), stop));
  ctx.fillStyle = gradient;
  ctx.fillRect(0, 0, width, height);
}

function drawTitleBar(ctx: Canvas2D, card: Rect, height: number, dark: boolean): void {
  ctx.fillStyle = dark ? '#2a2a2e' : '#ececee';
  ctx.fillRect(card.x, card.y, card.width, height);
  ctx.fillStyle = dark ? 'rgba(255,255,255,0.08)' : 'rgba(0,0,0,0.08)';
  ctx.fillRect(card.x, card.y + height - Math.max(1, height / 40), card.width, Math.max(1, height / 40));
  const r = height * 0.18;
  const gap = r * 3.2;
  ['#ff5f57', '#febc2e', '#28c840'].forEach((colour, index) => {
    ctx.beginPath();
    ctx.arc(card.x + height * 0.55 + index * gap, card.y + height / 2, r, 0, Math.PI * 2);
    ctx.fillStyle = colour;
    ctx.fill();
  });
}

/**
 * The card behind the screenshot: the inset margin takes the screenshot's own
 * edge colour so the card reads as part of the image; otherwise it matches
 * the window frame. Shared with the editor's CSS preview.
 */
export function backdropCardColor(backdrop: Backdrop, auto: AutoColors | null): string {
  if (backdrop.inset > 0) return backdrop.insetColor ?? auto?.edge ?? '#ffffff';
  return backdrop.frame === 'macos-dark' ? '#1c1c1e' : '#ffffff';
}

/**
 * Puts the flattened screenshot on its card and background: shadow, rounded
 * corners, optional window frame and inset, all laid out by `layoutBackdrop`.
 */
export function composeBackdrop(
  content: HTMLCanvasElement,
  backdrop: Backdrop,
  autoColors?: AutoColors
): HTMLCanvasElement {
  const layout = layoutBackdrop(content.width, content.height, backdrop);
  const out = document.createElement('canvas');
  out.width = layout.width;
  out.height = layout.height;
  const ctx = out.getContext('2d');
  if (!ctx) throw new Error('Canvas 2D is unavailable in this browser.');

  const needsAuto = backdrop.fill === 'auto' || (backdrop.inset > 0 && !backdrop.insetColor);
  const auto =
    autoColors ?? (needsAuto ? sampleAutoColors(content, content.width, content.height) : FALLBACK_AUTO);
  paintBackground(ctx, backdrop.fill, layout.width, layout.height, auto, content);
  if (backdrop.fill !== 'none') paintGrain(ctx, layout.width, layout.height, backdrop.grain ?? 0);

  const { card, image, radius } = layout;
  const cardColor = backdropCardColor(backdrop, auto);

  // The shadow is cast by an opaque card drawn under the image.
  ctx.save();
  if (backdrop.shadow > 0) {
    ctx.shadowColor = `rgba(0, 0, 0, ${0.18 + backdrop.shadow * 0.32})`;
    ctx.shadowBlur = layout.shadowBlur;
    ctx.shadowOffsetY = layout.shadowBlur * 0.35;
  }
  ctx.beginPath();
  roundRectPath(ctx, card, radius);
  ctx.fillStyle = cardColor;
  ctx.fill();
  ctx.restore();

  ctx.save();
  ctx.beginPath();
  roundRectPath(ctx, card, radius);
  ctx.clip();
  if (layout.titleBar > 0) {
    drawTitleBar(ctx, card, layout.titleBar, backdrop.frame === 'macos-dark');
  }
  ctx.imageSmoothingEnabled = true;
  ctx.drawImage(content, 0, 0, content.width, content.height, image.x, image.y, image.width, image.height);
  ctx.restore();
  return out;
}

export function canvasToPngBlob(canvas: HTMLCanvasElement): Promise<Blob> {
  return new Promise((resolve, reject) => {
    canvas.toBlob((blob) => {
      if (blob) resolve(blob);
      else reject(new Error('Could not export the screenshot.'));
    }, 'image/png');
  });
}
