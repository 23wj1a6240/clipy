import { invoke } from "@tauri-apps/api/core";
import type { DisplayInfoDto, WindowInfoDto } from "./bridge";

export interface WindowPickerSnapshot {
  display: DisplayInfoDto;
  windows: WindowInfoDto[];
}

export const pickWindowInteractively = () => invoke<number | null>("pick_window_interactively");
export const getWindowPickerSnapshot = () => invoke<WindowPickerSnapshot>("get_window_picker_snapshot");
export const completeWindowPicker = (id: number | null) => invoke<void>("complete_window_picker", { id });
/** Esc from the popover's "Select window" sheet: the overlays are not the
 *  key window while the popover is, so the cancel has to come from here. */
export const cancelWindowPicker = () => invoke<void>("cancel_window_picker");

export function windowAtPoint(windows: WindowInfoDto[], x: number, y: number): WindowInfoDto | null {
  return windows.find((candidate) => x >= candidate.x && x < candidate.x + candidate.width
    && y >= candidate.y && y < candidate.y + candidate.height) ?? null;
}

export function windowPickerRect(candidate: WindowInfoDto, display: DisplayInfoDto, viewportWidth: number, viewportHeight: number) {
  const scaleX = viewportWidth / display.width;
  const scaleY = viewportHeight / display.height;
  return { left: (candidate.x - display.x) * scaleX, top: (candidate.y - display.y) * scaleY,
    width: candidate.width * scaleX, height: candidate.height * scaleY };
}
