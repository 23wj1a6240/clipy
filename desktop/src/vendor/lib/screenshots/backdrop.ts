// SPDX-License-Identifier: Apache-2.0
// Copied from the Clipy monorepo (lib/screenshots/backdrop.ts) by the export; the desktop app shares it.
/**
 * Backgrounds for the screenshot "beautify" pass: the gradient presets, the
 * defaults, and the layout that places the card on the background.
 *
 * Pure and isomorphic: `parseScene` validates preset ids with it on the
 * server, the canvas exporter lays the image out with it, and the editor
 * previews the same layout in CSS, so what you see is what is exported.
 */

import type { Backdrop, BackdropAspect, BackdropFrame, Rect } from './types';

export interface GradientPreset {
  id: string;
  label: string;
  /** CSS angle in degrees: 0 points up, 90 points right. */
  angle: number;
  stops: readonly string[];
}

export const GRADIENT_PRESETS: readonly GradientPreset[] = [
  { id: 'sunset', label: 'Sunset', angle: 135, stops: ['#ff7a59', '#ff3d77', '#7b2ff7'] },
  { id: 'clipy', label: 'Clipy', angle: 135, stops: ['#ff9a3c', '#ff5e3a'] },
  { id: 'ocean', label: 'Ocean', angle: 135, stops: ['#2af598', '#009efd'] },
  { id: 'lagoon', label: 'Lagoon', angle: 160, stops: ['#43e8d8', '#2563eb'] },
  { id: 'grape', label: 'Grape', angle: 135, stops: ['#a18cd1', '#6d28d9'] },
  { id: 'candy', label: 'Candy', angle: 135, stops: ['#fbc2eb', '#a6c1ee'] },
  { id: 'peach', label: 'Peach', angle: 135, stops: ['#ffecd2', '#fcb69f'] },
  { id: 'mint', label: 'Mint', angle: 135, stops: ['#d4fc79', '#96e6a1'] },
  { id: 'aurora', label: 'Aurora', angle: 120, stops: ['#00c9a7', '#845ec2', '#ff6f91'] },
  { id: 'citrus', label: 'Citrus', angle: 135, stops: ['#f9d423', '#ff4e50'] },
  { id: 'sky', label: 'Sky', angle: 180, stops: ['#a1c4fd', '#c2e9fb'] },
  { id: 'rose', label: 'Rose', angle: 135, stops: ['#f43f5e', '#fb7185', '#fda4af'] },
  { id: 'forest', label: 'Forest', angle: 135, stops: ['#134e5e', '#71b280'] },
  { id: 'midnight', label: 'Midnight', angle: 135, stops: ['#0f172a', '#334155'] },
  { id: 'cosmic', label: 'Cosmic', angle: 135, stops: ['#1e1b4b', '#7c3aed', '#db2777'] },
  { id: 'steel', label: 'Steel', angle: 135, stops: ['#e2e8f0', '#94a3b8'] },
  { id: 'paper', label: 'Paper', angle: 180, stops: ['#fafaf9', '#e7e5e4'] },
  { id: 'ember', label: 'Ember', angle: 135, stops: ['#200122', '#6f0000'] },
];

/**
 * Soft multi-colour backgrounds: a base colour with blurred colour blobs.
 * Drawn as CSS radial gradients in the preview and the same ellipses on the
 * export canvas.
 */
export interface MeshPreset {
  id: string;
  label: string;
  base: string;
  /** Blob centres as fractions of the background, each fading out by half way. */
  blobs: readonly { x: number; y: number; color: string }[];
}

export const MESH_PRESETS: readonly MeshPreset[] = [
  {
    id: 'dawn',
    label: 'Dawn mesh',
    base: '#ffd6c9',
    blobs: [
      { x: 0.1, y: 0.15, color: '#ff8a65' },
      { x: 0.85, y: 0.1, color: '#f06292' },
      { x: 0.7, y: 0.9, color: '#ba68c8' },
      { x: 0.15, y: 0.85, color: '#ffb74d' },
    ],
  },
  {
    id: 'lagoon',
    label: 'Lagoon mesh',
    base: '#c8f1ff',
    blobs: [
      { x: 0.05, y: 0.1, color: '#4dd0e1' },
      { x: 0.9, y: 0.2, color: '#5c6bc0' },
      { x: 0.8, y: 0.95, color: '#26a69a' },
      { x: 0.2, y: 0.9, color: '#81d4fa' },
    ],
  },
  {
    id: 'orchid',
    label: 'Orchid mesh',
    base: '#ead7ff',
    blobs: [
      { x: 0.1, y: 0.2, color: '#ce93d8' },
      { x: 0.9, y: 0.1, color: '#7986cb' },
      { x: 0.85, y: 0.85, color: '#f48fb1' },
      { x: 0.1, y: 0.95, color: '#9575cd' },
    ],
  },
  {
    id: 'meadow',
    label: 'Meadow mesh',
    base: '#e6f9d4',
    blobs: [
      { x: 0.1, y: 0.1, color: '#aed581' },
      { x: 0.9, y: 0.25, color: '#4db6ac' },
      { x: 0.75, y: 0.9, color: '#dce775' },
      { x: 0.15, y: 0.85, color: '#81c784' },
    ],
  },
  {
    id: 'nebula',
    label: 'Nebula mesh',
    base: '#14102b',
    blobs: [
      { x: 0.15, y: 0.2, color: '#5b2a86' },
      { x: 0.85, y: 0.15, color: '#1f4e8c' },
      { x: 0.8, y: 0.85, color: '#9d1d6b' },
      { x: 0.2, y: 0.9, color: '#2d1b69' },
    ],
  },
  {
    id: 'sorbet',
    label: 'Sorbet mesh',
    base: '#fff1d6',
    blobs: [
      { x: 0.1, y: 0.1, color: '#ffcc80' },
      { x: 0.9, y: 0.15, color: '#ff8a80' },
      { x: 0.85, y: 0.9, color: '#ffe082' },
      { x: 0.1, y: 0.9, color: '#f8bbd0' },
    ],
  },
];

export const SOLID_BACKDROPS = ['#ffffff', '#f4f4f5', '#18181b', '#000000'] as const;

export const BACKDROP_ASPECTS: readonly { value: BackdropAspect; label: string }[] = [
  { value: 'auto', label: 'Auto' },
  { value: '16:9', label: '16:9' },
  { value: '4:3', label: '4:3' },
  { value: '3:2', label: '3:2' },
  { value: '1:1', label: '1:1' },
  { value: '4:5', label: '4:5' },
  { value: '9:16', label: '9:16' },
];

export const BACKDROP_FRAMES: readonly { value: BackdropFrame; label: string }[] = [
  { value: 'none', label: 'None' },
  { value: 'macos-light', label: 'Window, light' },
  { value: 'macos-dark', label: 'Window, dark' },
];

export const BACKDROP_LIMITS = {
  padding: 0.25,
  radius: 0.08,
  shadow: 1,
  inset: 0.1,
  grain: 1,
} as const;

/** What turning the background on gives you: one tap to a finished look. */
export const DEFAULT_BACKDROP: Backdrop = {
  fill: 'g:sunset',
  padding: 0.08,
  radius: 0.018,
  shadow: 0.6,
  inset: 0,
  aspect: 'auto',
  frame: 'none',
};

/**
 * The largest canvas the exporter will make. Safari and WKWebView (the Mac
 * app) refuse canvases above roughly 16.7 million pixels, and the result would
 * not fit the 15 MB upload cap anyway, so a bigger layout is scaled down.
 */
export const MAX_OUTPUT_PIXELS = 16_777_216;

const HEX_COLOR = /^#(?:[0-9a-f]{3}|[0-9a-f]{6})$/i;

export function gradientPreset(fill: string): GradientPreset | null {
  if (!fill.startsWith('g:')) return null;
  const id = fill.slice(2);
  return GRADIENT_PRESETS.find((preset) => preset.id === id) ?? null;
}

export function meshPreset(fill: string): MeshPreset | null {
  if (!fill.startsWith('m:')) return null;
  const id = fill.slice(2);
  return MESH_PRESETS.find((preset) => preset.id === id) ?? null;
}

export function isHexColor(value: string): boolean {
  return HEX_COLOR.test(value);
}

export function isValidBackdropFill(fill: string): boolean {
  return (
    fill === 'none' ||
    fill === 'auto' ||
    fill === 'blur' ||
    HEX_COLOR.test(fill) ||
    gradientPreset(fill) !== null ||
    meshPreset(fill) !== null
  );
}

/**
 * The ellipse a CSS `radial-gradient(at x y, ...)` uses by default
 * (farthest-corner): the farthest-side ellipse scaled by root 2 so it passes
 * through the farthest corner. Shared so the export matches the preview.
 */
export function meshEllipse(
  blob: { x: number; y: number },
  width: number,
  height: number
): { cx: number; cy: number; rx: number; ry: number } {
  const cx = blob.x * width;
  const cy = blob.y * height;
  const sideX = Math.max(cx, width - cx, 1);
  const sideY = Math.max(cy, height - cy, 1);
  return { cx, cy, rx: sideX * Math.SQRT2, ry: sideY * Math.SQRT2 };
}

/**
 * The preset as CSS. The first CSS background layer is drawn on top, while
 * the export paints the blobs in array order (the last one on top), so the
 * layers are listed in reverse for the two to stack the same way.
 */
export function meshCss(preset: MeshPreset): string {
  return [
    ...[...preset.blobs].reverse().map(
      (blob) => `radial-gradient(at ${blob.x * 100}% ${blob.y * 100}%, ${blob.color} 0%, transparent 50%)`
    ),
    preset.base,
  ].join(', ');
}

const ASPECT_RATIOS: Record<Exclude<BackdropAspect, 'auto'>, number> = {
  '1:1': 1,
  '4:3': 4 / 3,
  '3:2': 3 / 2,
  '16:9': 16 / 9,
  '9:16': 9 / 16,
  '4:5': 4 / 5,
};

export interface BackdropLayout {
  /** Output canvas size. */
  width: number;
  height: number;
  /** The card (window frame + inset + image), in output pixels. */
  card: Rect;
  /** Where the screenshot itself goes, in output pixels. */
  image: Rect;
  /** Window title bar height at the top of the card; 0 when there is no frame. */
  titleBar: number;
  radius: number;
  /** Shadow blur radius in output pixels. */
  shadowBlur: number;
  /** Output pixels per screenshot pixel; below 1 only for a huge layout. */
  scale: number;
}

/** True when the backdrop changes nothing, so the bare screenshot is exported. */
export function backdropIsNoop(backdrop: Backdrop | null | undefined): boolean {
  if (!backdrop) return true;
  return (
    backdrop.padding <= 0 &&
    backdrop.radius <= 0 &&
    backdrop.inset <= 0 &&
    backdrop.frame === 'none' &&
    backdrop.aspect === 'auto'
  );
}

export function titleBarHeight(longest: number): number {
  return Math.round(Math.min(80, Math.max(24, longest * 0.03)));
}

/**
 * Places a `contentWidth` x `contentHeight` screenshot on its background.
 * The card is centred; an aspect ratio grows the background, never crops.
 */
export function layoutBackdrop(
  contentWidth: number,
  contentHeight: number,
  backdrop: Backdrop,
  maxPixels = MAX_OUTPUT_PIXELS
): BackdropLayout {
  const longest = Math.max(contentWidth, contentHeight, 1);
  const inset = Math.round(backdrop.inset * longest);
  const titleBar = backdrop.frame === 'none' ? 0 : titleBarHeight(longest);
  const cardWidth = contentWidth + inset * 2;
  const cardHeight = contentHeight + inset * 2 + titleBar;
  const padding = Math.round(backdrop.padding * longest);

  let width = cardWidth + padding * 2;
  let height = cardHeight + padding * 2;
  if (backdrop.aspect !== 'auto') {
    const ratio = ASPECT_RATIOS[backdrop.aspect];
    if (width / height < ratio) width = Math.round(height * ratio);
    else height = Math.round(width / ratio);
  }

  const scale = Math.min(1, Math.sqrt(maxPixels / Math.max(1, width * height)));
  const s = (value: number) => value * scale;
  const cardX = (width - cardWidth) / 2;
  const cardY = (height - cardHeight) / 2;

  return {
    width: Math.max(1, Math.floor(s(width))),
    height: Math.max(1, Math.floor(s(height))),
    card: { x: s(cardX), y: s(cardY), width: s(cardWidth), height: s(cardHeight) },
    image: {
      x: s(cardX + inset),
      y: s(cardY + inset + titleBar),
      width: s(contentWidth),
      height: s(contentHeight),
    },
    titleBar: s(titleBar),
    radius: s(Math.min(backdrop.radius * longest, Math.min(cardWidth, cardHeight) / 2)),
    shadowBlur: s(backdrop.shadow * longest * 0.04),
    scale,
  };
}

/** Start and end points for a CSS-style angled gradient across a box. */
export function gradientLine(
  angle: number,
  width: number,
  height: number
): { x0: number; y0: number; x1: number; y1: number } {
  const radians = (angle * Math.PI) / 180;
  const dx = Math.sin(radians);
  const dy = -Math.cos(radians);
  // CSS gradient length: the box's extent along the gradient direction.
  const half = (Math.abs(width * dx) + Math.abs(height * dy)) / 2;
  const cx = width / 2;
  const cy = height / 2;
  return { x0: cx - dx * half, y0: cy - dy * half, x1: cx + dx * half, y1: cy + dy * half };
}

export function gradientCss(preset: GradientPreset): string {
  return `linear-gradient(${preset.angle}deg, ${preset.stops.join(', ')})`;
}

/** The two colours the `auto` background is built from. */
export interface AutoColors {
  /** Average colour of the screenshot's outer edge, used for the inset. */
  edge: string;
  from: string;
  to: string;
}

function hex(n: number): string {
  return Math.max(0, Math.min(255, Math.round(n))).toString(16).padStart(2, '0');
}

function mix(rgb: [number, number, number], target: number, amount: number): string {
  return `#${rgb.map((c) => hex(c + (target - c) * amount)).join('')}`;
}

/**
 * Builds the `auto` background from RGBA pixels of a small thumbnail of the
 * screenshot: a gradient in its own average hue, lighter to darker, so the card
 * looks like it belongs to the image.
 */
export function autoColorsFromPixels(data: ArrayLike<number>, width: number, height: number): AutoColors {
  let r = 0;
  let g = 0;
  let b = 0;
  let n = 0;
  let er = 0;
  let eg = 0;
  let eb = 0;
  let en = 0;
  for (let y = 0; y < height; y += 1) {
    for (let x = 0; x < width; x += 1) {
      const i = (y * width + x) * 4;
      r += data[i];
      g += data[i + 1];
      b += data[i + 2];
      n += 1;
      if (x === 0 || y === 0 || x === width - 1 || y === height - 1) {
        er += data[i];
        eg += data[i + 1];
        eb += data[i + 2];
        en += 1;
      }
    }
  }
  const avg: [number, number, number] = n ? [r / n, g / n, b / n] : [128, 128, 128];
  const edge: [number, number, number] = en ? [er / en, eg / en, eb / en] : avg;
  // Push the average away from grey so a mostly-white page still gets colour.
  const mean = (avg[0] + avg[1] + avg[2]) / 3;
  const vivid: [number, number, number] = [
    mean + (avg[0] - mean) * 2.2,
    mean + (avg[1] - mean) * 2.2,
    mean + (avg[2] - mean) * 2.2,
  ];
  return {
    edge: `#${edge.map(hex).join('')}`,
    from: mix(vivid, 255, 0.35),
    to: mix(vivid, 0, 0.45),
  };
}
