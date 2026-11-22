import type { WebcamLayoutDto, WebcamShapeDto } from "./bridge";

/**
 * The four camera views the recorder offers:
 * Floating and Square are the bubble in its two shapes, and the camera can
 * dock to the left or right edge. Top and bottom docking are still valid
 * saved layouts, but they are no longer offered and load as Floating.
 */
export type CameraView = "floating" | "square" | "left" | "right";

export const CAMERA_VIEW_ITEMS: Array<{ value: CameraView; label: string }> = [
  { value: "floating", label: "Floating" },
  { value: "square", label: "Square" },
  { value: "left", label: "Docked left" },
  { value: "right", label: "Docked right" },
];

/** A saved layout the picker no longer offers loads as the circular
 *  Floating view. The shape has to change with it: a retired dock's stored
 *  shape is the rectangle default, which on a bubble would read as Square. */
export function normalizeWebcamAppearance(
  layout: WebcamLayoutDto,
  shape: WebcamShapeDto,
): { layout: WebcamLayoutDto; shape: WebcamShapeDto } {
  if (layout === "top" || layout === "bottom") return { layout: "bubble", shape: "circle" };
  return { layout, shape };
}

export function cameraViewOf(layout: WebcamLayoutDto, shape: WebcamShapeDto): CameraView {
  if (layout === "left" || layout === "right") return layout;
  return shape === "square" ? "square" : "floating";
}

/** The layout, and for bubble views the shape, that a view selects. */
export function applyCameraView(view: CameraView): { layout: WebcamLayoutDto; shape?: WebcamShapeDto } {
  switch (view) {
    case "left":
    case "right":
      return { layout: view };
    case "square":
      return { layout: "bubble", shape: "square" };
    default:
      return { layout: "bubble", shape: "circle" };
  }
}
