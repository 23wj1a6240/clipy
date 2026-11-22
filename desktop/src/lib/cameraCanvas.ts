/**
 * Geometry for the camera bubble's canvas.
 *
 * Pulled out of `routes/Webcam.tsx` so it can be tested without a WebView: the
 * bubble is drawn into a canvas precisely because ScreenCaptureKit cannot read
 * back a `<video>` layer reliably, which means these numbers are not preview
 * decoration — they are the camera's resolution in the finished recording.
 */

/**
 * Backing-store size for a canvas that occupies `cssWidth` x `cssHeight`
 * layout points on a display with `pixelRatio` device pixels per point.
 *
 * The bubble used to set `canvas.width = canvas.clientWidth`, i.e. one backing
 * pixel per layout point. On every Retina Mac that is half the resolution the
 * window actually occupies, and the compositor stretched the result 2x before
 * ScreenCaptureKit ever saw it — a blur applied after all the work of getting
 * sharp frames out of the camera.
 *
 * The ratio is capped because the cost is quadratic and nothing is gained
 * past the display's own density: no shipping Mac exceeds 2, and a stray
 * larger value from a zoomed WebView would allocate for detail that cannot be
 * shown.
 */
export const MAX_CANVAS_PIXEL_RATIO = 3;

export function canvasBackingSize(
  cssWidth: number,
  cssHeight: number,
  pixelRatio: number,
): { width: number; height: number } {
  const ratio =
    Number.isFinite(pixelRatio) && pixelRatio > 0
      ? Math.min(pixelRatio, MAX_CANVAS_PIXEL_RATIO)
      : 1;
  return {
    width: Math.max(1, Math.round(Math.max(0, cssWidth) * ratio)),
    height: Math.max(1, Math.round(Math.max(0, cssHeight) * ratio)),
  };
}

/**
 * The source rectangle that fills a `targetWidth` x `targetHeight` canvas with
 * a `sourceWidth` x `sourceHeight` camera frame without distorting it —
 * `object-fit: cover`, centred, which is what keeps a landscape camera looking
 * like a person rather than a stretched one inside a circle or a pill.
 */
export function coverSourceRect(
  sourceWidth: number,
  sourceHeight: number,
  targetWidth: number,
  targetHeight: number,
): { sx: number; sy: number; sw: number; sh: number } {
  if (sourceWidth <= 0 || sourceHeight <= 0 || targetWidth <= 0 || targetHeight <= 0) {
    return { sx: 0, sy: 0, sw: Math.max(0, sourceWidth), sh: Math.max(0, sourceHeight) };
  }
  const sourceAspect = sourceWidth / sourceHeight;
  const targetAspect = targetWidth / targetHeight;
  if (sourceAspect > targetAspect) {
    const sw = sourceHeight * targetAspect;
    return { sx: (sourceWidth - sw) / 2, sy: 0, sw, sh: sourceHeight };
  }
  if (sourceAspect < targetAspect) {
    const sh = sourceWidth / targetAspect;
    return { sx: 0, sy: (sourceHeight - sh) / 2, sw: sourceWidth, sh };
  }
  return { sx: 0, sy: 0, sw: sourceWidth, sh: sourceHeight };
}
