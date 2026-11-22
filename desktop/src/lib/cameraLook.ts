/**
 * The camera look the bubble draws with: "Touch up my appearance"
 * and "Auto lighting". Both are off by default.
 *
 * Touch up is a soft-focus blend: the frame is drawn, then a downscaled
 * copy of it is drawn back over it at partial opacity. Skin texture and
 * small blemishes average out while edges stay readable. Auto lighting
 * measures the frame's mean luminance and lifts or lowers exposure toward a
 * mid-grey target, eased so a hand across the lens does not flash.
 */
export interface CameraLook {
  touchUpEnabled: boolean;
  /** 0..1, only counts while `touchUpEnabled`. */
  touchUpStrength: number;
  autoLighting: boolean;
}

export const DEFAULT_CAMERA_LOOK: CameraLook = {
  touchUpEnabled: false,
  touchUpStrength: 0.5,
  autoLighting: false,
};

export function clampStrength(value: number): number {
  if (!Number.isFinite(value)) return DEFAULT_CAMERA_LOOK.touchUpStrength;
  return Math.max(0, Math.min(1, value));
}

/** Opacity of the softened copy drawn over the frame. Never fully opaque:
 *  at full strength the frame still reads as a camera, not a painting. */
export function touchUpAlpha(look: CameraLook): number {
  if (!look.touchUpEnabled) return 0;
  return 0.2 + 0.5 * clampStrength(look.touchUpStrength);
}

/** How far the softened copy is downscaled before being drawn back. A
 *  smaller copy resamples to a wider blur. */
export function touchUpDownscale(look: CameraLook): number {
  if (!look.touchUpEnabled) return 1;
  return 1 / (3 + 5 * clampStrength(look.touchUpStrength));
}

/** Mid-grey on a 0..1 luminance scale, where faces tend to sit. */
export const AUTO_LIGHTING_TARGET_LUMA = 0.5;
export const AUTO_LIGHTING_MIN_GAIN = 0.85;
export const AUTO_LIGHTING_MAX_GAIN = 1.6;

/** The brightness multiplier that moves `meanLuma` toward the target. */
export function autoLightingGain(meanLuma: number): number {
  if (!Number.isFinite(meanLuma) || meanLuma <= 0) return 1;
  return Math.max(AUTO_LIGHTING_MIN_GAIN, Math.min(AUTO_LIGHTING_MAX_GAIN, AUTO_LIGHTING_TARGET_LUMA / meanLuma));
}

/** Ease the applied gain toward the measured one so exposure drifts rather
 *  than steps. */
export function easeGain(current: number, target: number, rate = 0.15): number {
  return current + (target - current) * rate;
}

/** Mean luminance (0..1) of an RGBA byte buffer, weighted by alpha so the
 *  transparent canvas behind a cut-out does not read as darkness. */
export function meanLuminance(rgba: Uint8ClampedArray): number {
  let sum = 0;
  let weight = 0;
  for (let i = 0; i + 3 < rgba.length; i += 4) {
    const alpha = rgba[i + 3] / 255;
    if (alpha === 0) continue;
    sum += (0.2126 * rgba[i] + 0.7152 * rgba[i + 1] + 0.0722 * rgba[i + 2]) * alpha;
    weight += alpha;
  }
  if (weight === 0) return AUTO_LIGHTING_TARGET_LUMA;
  return sum / weight / 255;
}

/** The CSS filter the bubble canvas wears for a given exposure gain. Empty
 *  when nothing needs adjusting so the browser skips the filter pass. */
export function lightingFilter(gain: number): string {
  if (Math.abs(gain - 1) < 0.01) return "";
  return `brightness(${gain.toFixed(3)}) contrast(1.05)`;
}
