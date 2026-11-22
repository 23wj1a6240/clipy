import type { WebcamLayoutDto, WebcamShapeDto, WebcamSizeDto } from "./bridge";

/** Square and Medium are the bubble's defaults, and these must agree with
 *  `WebcamShapeDto`/`WebcamSizeDto`'s `#[default]` on the Rust side — these
 *  are only the value a view holds before saved settings arrive. */
export const DEFAULT_WEBCAM_SHAPE = "square" as const;
export const DEFAULT_WEBCAM_SIZE: WebcamSizeDto = "medium";

export function webcamShapeForLayout(
  layout: WebcamLayoutDto,
  shape: WebcamShapeDto = DEFAULT_WEBCAM_SHAPE,
): WebcamShapeDto | "rectangle" {
  return layout === "bubble" ? shape : "rectangle";
}
