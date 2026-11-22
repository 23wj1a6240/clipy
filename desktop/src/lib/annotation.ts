import { invoke } from "@tauri-apps/api/core";

export const ANNOTATION_COLORS = [
  { name: "Coral", value: "#ff4d29" },
  { name: "Yellow", value: "#ffcc33" },
  { name: "Green", value: "#34d399" },
  { name: "Blue", value: "#38bdf8" },
  { name: "Purple", value: "#c084fc" },
  { name: "White", value: "#ffffff" },
] as const;
export type AnnotationColor = typeof ANNOTATION_COLORS[number]["value"];
export function isAnnotationColor(value: unknown): value is AnnotationColor {
  return ANNOTATION_COLORS.some((color) => color.value === value);
}
export const getAnnotationColor = () => invoke<AnnotationColor>("get_annotation_color");
export const setAnnotationColor = (color: AnnotationColor) => invoke<void>("set_annotation_color", { color });
export const clearAnnotations = () => invoke<void>("clear_annotations");
export const ANNOTATION_WIDTH_MIN = 2;
export const ANNOTATION_WIDTH_MAX = 14;
export const getAnnotationWidth = () => invoke<number>("get_annotation_width");
export const setAnnotationWidth = (width: number) => invoke<void>("set_annotation_width", { width });
