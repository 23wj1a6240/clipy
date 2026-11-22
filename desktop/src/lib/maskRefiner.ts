/**
 * Cleans up a raw segmentation mask before it becomes the cut-out's alpha.
 *
 * The selfie model predicts at 256 × 256, so its upscaled mask is soft,
 * blocky and flickers at the edges. Three passes fix that:
 * 1. Temporal smoothing damps frame-to-frame flicker.
 * 2. A fast guided filter (He & Sun) snaps the mask to edges in the camera
 *    frame. Coefficients are solved at half resolution and applied against
 *    the full-resolution frame, which keeps edge detail at a quarter the cost.
 * 3. A contrast curve turns the wide semi-transparent halo into a tight edge.
 */

// Share of the newest mask in the running average; lower is steadier but lags motion.
const TEMPORAL_WEIGHT = 0.5;
// A gap longer than this starts a fresh average so an old pose never ghosts in.
const TEMPORAL_RESET_MS = 250;
// Guided filter smoothing; smaller follows image edges more tightly.
const GUIDED_EPS = 1e-3;
// Confidence below LOW is background, above HIGH is person, smoothstep between.
const EDGE_LOW = 0.3;
const EDGE_HIGH = 0.7;

export class MaskRefiner {
  #width = 0;
  #height = 0;
  #lastTime = -Infinity;
  #previous = new Float32Array(0);
  #guide = new Float32Array(0);
  // Half-resolution working buffers.
  #smallGuide = new Float32Array(0);
  #smallMask = new Float32Array(0);
  #meanGuide = new Float32Array(0);
  #meanMask = new Float32Array(0);
  #corrGuide = new Float32Array(0);
  #corrGuideMask = new Float32Array(0);
  #a = new Float32Array(0);
  #b = new Float32Array(0);
  #scratch = new Float32Array(0);
  // Bilinear upsample lookups: source column pair and weight per output column.
  #columnLeft = new Int32Array(0);
  #columnRight = new Int32Array(0);
  #columnWeight = new Float32Array(0);
  #rowA = new Float32Array(0);
  #rowB = new Float32Array(0);

  /**
   * `confidence` and `rgba` are both `width` × `height`; the refined mask is
   * written into the alpha channel of `out`, an RGBA buffer of the same size.
   */
  refine(
    confidence: Float32Array,
    rgba: Uint8ClampedArray,
    width: number,
    height: number,
    time: number,
    out: Uint8ClampedArray,
  ) {
    if (width !== this.#width || height !== this.#height) this.#allocate(width, height);
    const n = width * height;
    const fresh = time - this.#lastTime > TEMPORAL_RESET_MS;
    this.#lastTime = time;

    const mask = this.#previous;
    const guide = this.#guide;
    for (let i = 0, j = 0; i < n; i++, j += 4) {
      mask[i] = fresh ? confidence[i] : mask[i] + TEMPORAL_WEIGHT * (confidence[i] - mask[i]);
      guide[i] = (0.299 * rgba[j] + 0.587 * rgba[j + 1] + 0.114 * rgba[j + 2]) / 255;
    }

    const sw = width >> 1;
    const sh = height >> 1;
    const sn = sw * sh;
    const smallGuide = this.#smallGuide;
    const smallMask = this.#smallMask;
    const corrGuide = this.#corrGuide;
    const corrGuideMask = this.#corrGuideMask;
    for (let y = 0, k = 0; y < sh; y++) {
      const top = 2 * y * width;
      const bottom = top + width;
      for (let x = 0; x < sw; x++, k++) {
        const l = 2 * x;
        const g = (guide[top + l] + guide[top + l + 1] + guide[bottom + l] + guide[bottom + l + 1]) * 0.25;
        const m = (mask[top + l] + mask[top + l + 1] + mask[bottom + l] + mask[bottom + l + 1]) * 0.25;
        smallGuide[k] = g;
        smallMask[k] = m;
        corrGuide[k] = g * g;
        corrGuideMask[k] = g * m;
      }
    }

    const radius = Math.max(1, Math.round(Math.max(sw, sh) / 160));
    const meanGuide = this.#meanGuide;
    const meanMask = this.#meanMask;
    const scratch = this.#scratch;
    boxFilter(smallGuide, meanGuide, scratch, sw, sh, radius);
    boxFilter(smallMask, meanMask, scratch, sw, sh, radius);
    boxFilter(corrGuide, corrGuide, scratch, sw, sh, radius);
    boxFilter(corrGuideMask, corrGuideMask, scratch, sw, sh, radius);

    const a = this.#a;
    const b = this.#b;
    for (let k = 0; k < sn; k++) {
      const variance = corrGuide[k] - meanGuide[k] * meanGuide[k];
      const covariance = corrGuideMask[k] - meanGuide[k] * meanMask[k];
      a[k] = covariance / (variance + GUIDED_EPS);
      b[k] = meanMask[k] - a[k] * meanGuide[k];
    }
    boxFilter(a, a, scratch, sw, sh, radius);
    boxFilter(b, b, scratch, sw, sh, radius);

    // Upsample the coefficients bilinearly and apply them to the full-resolution frame.
    const span = EDGE_HIGH - EDGE_LOW;
    const maxY = sh - 1;
    const left = this.#columnLeft;
    const right = this.#columnRight;
    const weight = this.#columnWeight;
    const rowA = this.#rowA;
    const rowB = this.#rowB;
    for (let y = 0; y < height; y++) {
      let fy = (y + 0.5) / 2 - 0.5;
      fy = fy < 0 ? 0 : fy > maxY ? maxY : fy;
      const y0 = fy | 0;
      const wy = fy - y0;
      const r0 = y0 * sw;
      const r1 = (y0 < maxY ? y0 + 1 : y0) * sw;
      for (let k = 0; k < sw; k++) {
        rowA[k] = a[r0 + k] + (a[r1 + k] - a[r0 + k]) * wy;
        rowB[k] = b[r0 + k] + (b[r1 + k] - b[r0 + k]) * wy;
      }
      const row = y * width;
      for (let x = 0; x < width; x++) {
        const x0 = left[x];
        const x1 = right[x];
        const wx = weight[x];
        const ia = rowA[x0] + (rowA[x1] - rowA[x0]) * wx;
        const ib = rowB[x0] + (rowB[x1] - rowB[x0]) * wx;
        let t = (ia * guide[row + x] + ib - EDGE_LOW) / span;
        t = t < 0 ? 0 : t > 1 ? 1 : t;
        out[(row + x) * 4 + 3] = t * t * (3 - 2 * t) * 255;
      }
    }
  }

  #allocate(width: number, height: number) {
    const n = width * height;
    const sn = (width >> 1) * (height >> 1);
    this.#width = width;
    this.#height = height;
    this.#lastTime = -Infinity;
    this.#previous = new Float32Array(n);
    this.#guide = new Float32Array(n);
    this.#smallGuide = new Float32Array(sn);
    this.#smallMask = new Float32Array(sn);
    this.#meanGuide = new Float32Array(sn);
    this.#meanMask = new Float32Array(sn);
    this.#corrGuide = new Float32Array(sn);
    this.#corrGuideMask = new Float32Array(sn);
    this.#a = new Float32Array(sn);
    this.#b = new Float32Array(sn);
    this.#scratch = new Float32Array(sn);
    const sw = width >> 1;
    this.#rowA = new Float32Array(sw);
    this.#rowB = new Float32Array(sw);
    this.#columnLeft = new Int32Array(width);
    this.#columnRight = new Int32Array(width);
    this.#columnWeight = new Float32Array(width);
    for (let x = 0; x < width; x++) {
      let fx = (x + 0.5) / 2 - 0.5;
      fx = fx < 0 ? 0 : fx > sw - 1 ? sw - 1 : fx;
      const x0 = fx | 0;
      this.#columnLeft[x] = x0;
      this.#columnRight[x] = x0 < sw - 1 ? x0 + 1 : x0;
      this.#columnWeight[x] = fx - x0;
    }
  }
}

/** Mean over a (2r+1)² window, clamped at the borders. `dst` may be `src`. */
function boxFilter(
  src: Float32Array,
  dst: Float32Array,
  scratch: Float32Array,
  width: number,
  height: number,
  radius: number,
) {
  for (let y = 0; y < height; y++) {
    const row = y * width;
    let sum = 0;
    let lo = 0;
    let hi = -1;
    for (let x = 0; x < width; x++) {
      const nextHi = x + radius < width ? x + radius : width - 1;
      while (hi < nextHi) sum += src[row + ++hi];
      const nextLo = x > radius ? x - radius : 0;
      while (lo < nextLo) sum -= src[row + lo++];
      scratch[row + x] = sum / (hi - lo + 1);
    }
  }
  for (let x = 0; x < width; x++) {
    let sum = 0;
    let lo = 0;
    let hi = -1;
    for (let y = 0; y < height; y++) {
      const nextHi = y + radius < height ? y + radius : height - 1;
      while (hi < nextHi) sum += scratch[++hi * width + x];
      const nextLo = y > radius ? y - radius : 0;
      while (lo < nextLo) sum -= scratch[lo++ * width + x];
      dst[y * width + x] = sum / (hi - lo + 1);
    }
  }
}
