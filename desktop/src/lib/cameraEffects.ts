import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";

export type CameraEffects = {
  background: "original" | "blur" | "remove";
  canvas: "transparent" | "ink" | "cream" | "sunset" | "aurora" | "dunes" | "studio" | "custom" | "solid";
  /** The colour behind the cut-out when `canvas` is "solid". */
  canvasRgb?: [number, number, number];
  frame: "none" | "coral" | "white" | "amber" | "geometric" | "confetti" | "botanical";
};

export const DEFAULT_CAMERA_EFFECTS: CameraEffects = { background: "original", canvas: "transparent", frame: "none" };
export const getCameraEffects = () => invoke<CameraEffects>("get_camera_effects");
export const importCameraBackground = (bytes: number[]) => invoke<void>("import_camera_background", { bytes });
/** Windows only: the stored custom background, re-encoded as PNG. */
export const readCameraBackground = () => invoke<ArrayBuffer>("read_camera_background");
export const setCameraEffects = (effects: CameraEffects) => invoke<void>("set_camera_effects", { effects });
export const onCameraEffectsChanged = (callback: (effects: CameraEffects) => void) =>
  listen<CameraEffects>("camera-effects-changed", (event) => callback(event.payload));
export const onCameraEffectsError = (callback: (message: string) => void) =>
  listen<string>("camera-effects-error", (event) => callback(event.payload));

export const cameraFrameColors = { none: "transparent", coral: "#FF4D29", white: "#EEF0F5", amber: "#FFB020", geometric: "#5783e1", confetti: "#896bda", botanical: "#447852" };
