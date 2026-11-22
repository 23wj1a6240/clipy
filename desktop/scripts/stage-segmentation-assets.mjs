/**
 * Stage the MediaPipe runtime for Windows camera background effects.
 *
 * macOS segments natively (camera_effects.m). Windows segments in the webcam
 * webview, which loads this wasm from bundled resources. The model is
 * committed in src-tauri/segmentation; the wasm is copied from the pinned npm
 * package so its loader always matches the JS bundle. Runs before Rust for the
 * same reason as stage-ffmpeg-dlls.mjs.
 *
 * No-op on non-Windows.
 */

import { copyFileSync, existsSync, readFileSync, statSync } from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { exit, platform } from "node:process";

const HERE = path.dirname(fileURLToPath(import.meta.url));
const DEST = path.join(HERE, "..", "src-tauri", "segmentation");
const SOURCE = path.join(HERE, "..", "node_modules", "@mediapipe", "tasks-vision", "wasm");
const RUNTIME = ["vision_wasm_internal.js", "vision_wasm_internal.wasm"];
// The bubble reads the TFLite model; the camera-only recorder reads the same
// model converted to ONNX (opset 11), which is what Windows ML loads.
const MODELS = ["selfie_segmenter.tflite", "selfie_segmenter.onnx"];

if (platform !== "win32") {
  console.log(`[stage-segmentation-assets] platform=${platform} — skipping (Windows only)`);
  exit(0);
}

for (const model of MODELS) {
  if (!existsSync(path.join(DEST, model))) {
    console.error(`[stage-segmentation-assets] ${model} is missing from ${DEST}`);
    exit(1);
  }
}

let bytes = 0;
for (const file of RUNTIME) {
  const from = path.join(SOURCE, file);
  if (!existsSync(from)) {
    console.error(`[stage-segmentation-assets] ${from} not found. Run pnpm install.`);
    exit(1);
  }
  const to = path.join(DEST, file);
  // build.rs watches this folder, so rewriting identical files would force a
  // Rust rebuild on every dev start.
  if (!existsSync(to) || !readFileSync(from).equals(readFileSync(to))) {
    copyFileSync(from, to);
  }
  bytes += statSync(from).size;
}

console.log(
  `[stage-segmentation-assets] ${RUNTIME.length} file(s) staged in ${DEST} (${(bytes / 1024 / 1024).toFixed(1)} MB)`,
);
