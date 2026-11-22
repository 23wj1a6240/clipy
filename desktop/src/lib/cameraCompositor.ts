import { ImageSegmenter } from "@mediapipe/tasks-vision";
import { convertFileSrc } from "@tauri-apps/api/core";
import { resolveResource } from "@tauri-apps/api/path";
import { auroraBackground, dunesBackground, studioBackground } from "../assets/backgrounds";
import { readCameraBackground, type CameraEffects } from "./cameraEffects";
import { MaskRefiner } from "./maskRefiner";

/**
 * Windows counterpart of camera_effects.m. The camera bubble has no native
 * pipeline there, so person segmentation (MediaPipe) and compositing run in
 * the webview with the same geometry and colours as the macOS effect.
 */

// Same analysis bound as the macOS Vision pass.
const ANALYSIS_MAX = 640;
const INK = "rgb(10, 13, 22)";
const CREAM = "rgb(250, 246, 240)";
const IMAGES: Partial<Record<CameraEffects["canvas"], string>> = {
  aurora: auroraBackground,
  dunes: dunesBackground,
  studio: studioBackground,
};

export class CameraCompositor {
  readonly #analysis = document.createElement("canvas");
  readonly #mask = document.createElement("canvas");
  readonly #person = document.createElement("canvas");
  readonly #output = document.createElement("canvas");
  #maskPixels: ImageData | null = null;
  readonly #refiner = new MaskRefiner();
  #lastTimestamp = 0;
  #effects: CameraEffects | null = null;
  #image: CanvasImageSource | null = null;
  #prepareToken = 0;

  private constructor(private readonly segmenter: ImageSegmenter) {}

  static async create(): Promise<CameraCompositor> {
    const [wasmLoaderPath, wasmBinaryPath, modelAssetPath] = await Promise.all(
      ["vision_wasm_internal.js", "vision_wasm_internal.wasm", "selfie_segmenter.tflite"].map((file) =>
        resolveResource(`segmentation/${file}`).then((path) => convertFileSrc(path)),
      ),
    );
    const segmenter = await ImageSegmenter.createFromOptions(
      { wasmLoaderPath, wasmBinaryPath },
      {
        baseOptions: { modelAssetPath, delegate: "CPU" },
        runningMode: "VIDEO",
        outputConfidenceMasks: true,
        outputCategoryMask: false,
      },
    );
    return new CameraCompositor(segmenter);
  }

  /** Loads the canvas image for `effects`; the latest call wins. */
  async prepare(effects: CameraEffects): Promise<void> {
    const token = ++this.#prepareToken;
    const image = effects.background === "remove" ? await loadCanvasImage(effects.canvas) : null;
    if (token !== this.#prepareToken) return;
    this.#effects = effects;
    this.#image = image;
  }

  /** Composites one frame at `width` × `height`, the video scaled uniformly. */
  render(video: HTMLVideoElement, width: number, height: number): HTMLCanvasElement {
    const effects = this.#effects;
    const output = sized(this.#output, width, height);
    const context = output.getContext("2d");
    if (!effects || !context) throw new Error("Camera compositor is not ready");

    const mask = this.#segment(video, Math.max(width, height));
    const person = sized(this.#person, width, height);
    const personContext = person.getContext("2d")!;
    personContext.globalCompositeOperation = "copy";
    personContext.drawImage(video, 0, 0, width, height);
    personContext.globalCompositeOperation = "destination-in";
    personContext.drawImage(mask, 0, 0, width, height);
    personContext.globalCompositeOperation = "source-over";

    context.clearRect(0, 0, width, height);
    this.#drawBackground(context, video, width, height, effects);
    context.drawImage(person, 0, 0);
    return output;
  }

  close() {
    this.segmenter.close();
  }

  /** `displaySize` is the composite's longest side; refining beyond it is wasted work. */
  #segment(video: HTMLVideoElement, displaySize: number): HTMLCanvasElement {
    const longest = Math.max(video.videoWidth, video.videoHeight);
    const scale = Math.min(1, Math.min(ANALYSIS_MAX, Math.max(displaySize, 256)) / longest);
    const analysis = sized(this.#analysis, Math.round(video.videoWidth * scale), Math.round(video.videoHeight * scale));
    const analysisContext = analysis.getContext("2d", { willReadFrequently: true })!;
    analysisContext.drawImage(video, 0, 0, analysis.width, analysis.height);

    // VIDEO mode requires strictly increasing timestamps.
    this.#lastTimestamp = Math.max(performance.now(), this.#lastTimestamp + 1);
    const result = this.segmenter.segmentForVideo(analysis, this.#lastTimestamp);
    try {
      const confidence = result.confidenceMasks?.[0];
      if (!confidence) throw new Error("Segmentation returned no mask");
      const mask = sized(this.#mask, confidence.width, confidence.height);
      const maskContext = mask.getContext("2d")!;
      if (this.#maskPixels?.width !== mask.width || this.#maskPixels.height !== mask.height) {
        this.#maskPixels = maskContext.createImageData(mask.width, mask.height);
      }
      const pixels = this.#maskPixels.data;
      const values = confidence.getAsFloat32Array();
      if (mask.width === analysis.width && mask.height === analysis.height) {
        const frame = analysisContext.getImageData(0, 0, analysis.width, analysis.height).data;
        this.#refiner.refine(values, frame, mask.width, mask.height, this.#lastTimestamp, pixels);
      } else {
        for (let i = 0; i < values.length; i++) pixels[i * 4 + 3] = values[i] * 255;
      }
      maskContext.putImageData(this.#maskPixels, 0, 0);
      return mask;
    } finally {
      result.close();
    }
  }

  #drawBackground(
    context: CanvasRenderingContext2D,
    video: HTMLVideoElement,
    width: number,
    height: number,
    effects: CameraEffects,
  ) {
    if (effects.background === "blur") {
      const radius = Math.max(width, height) * 0.035;
      // Overdraw past the edges so the blur does not fade out there (CI clamps to extent).
      context.filter = `blur(${radius}px)`;
      context.drawImage(video, -2 * radius, -2 * radius, width + 4 * radius, height + 4 * radius);
      context.filter = "none";
      return;
    }
    switch (effects.canvas) {
      case "transparent":
        return;
      case "cream":
        context.fillStyle = CREAM;
        break;
      case "solid": {
        const [r, g, b] = effects.canvasRgb ?? [0, 0, 0];
        context.fillStyle = `rgb(${r}, ${g}, ${b})`;
        break;
      }
      case "sunset": {
        // Core Image's origin is bottom-left, so (0,0)→(w,h) runs bottom-left to top-right.
        const gradient = context.createLinearGradient(0, height, width, 0);
        gradient.addColorStop(0, "rgb(255, 77, 41)");
        gradient.addColorStop(1, "rgb(255, 176, 33)");
        context.fillStyle = gradient;
        break;
      }
      default:
        context.fillStyle = INK;
    }
    context.fillRect(0, 0, width, height);
    const image = this.#image;
    if (!image || effects.canvas === "ink" || effects.canvas === "cream" || effects.canvas === "solid" || effects.canvas === "sunset") return;
    const [imageWidth, imageHeight] = image instanceof HTMLImageElement
      ? [image.naturalWidth, image.naturalHeight]
      : [(image as ImageBitmap).width, (image as ImageBitmap).height];
    const factor = Math.max(width / imageWidth, height / imageHeight);
    const drawWidth = imageWidth * factor;
    const drawHeight = imageHeight * factor;
    context.drawImage(image, (width - drawWidth) / 2, (height - drawHeight) / 2, drawWidth, drawHeight);
  }
}

async function loadCanvasImage(canvas: CameraEffects["canvas"]): Promise<CanvasImageSource | null> {
  if (canvas === "custom") {
    return createImageBitmap(new Blob([await readCameraBackground()], { type: "image/png" }));
  }
  const url = IMAGES[canvas];
  if (!url) return null;
  const image = new Image();
  image.src = url;
  await image.decode();
  return image;
}

function sized(canvas: HTMLCanvasElement, width: number, height: number): HTMLCanvasElement {
  if (canvas.width !== width) canvas.width = width;
  if (canvas.height !== height) canvas.height = height;
  return canvas;
}
