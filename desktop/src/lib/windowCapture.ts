import type { WindowCaptureModeDto } from "./bridge";
import { englishT, type DesktopT } from "./messages";

/**
 * How a "Specific window" take is recorded, and what the picker says about
 * it. Mirrors `is_window_only_capture` in `src-tauri/src/types.rs`: the Mac
 * records the window by itself unless the user chose the screen area in
 * Settings; the Windows app always records the area.
 */
export function isWindowOnlyCapture(state: {
  isWindows: boolean;
  cameraOnly: boolean;
  sourceTab: string;
  windowCapture: WindowCaptureModeDto;
}): boolean {
  return !state.isWindows && !state.cameraOnly && state.sourceTab === "window" && state.windowCapture === "window";
}

/**
 * The "Specific window" hint in the source menu. Always two lines: an
 * unbroken hint at this width wrapped and unwrapped as the popover refit
 * itself, which made the menu bounce (see SOURCE_CHOICE_ITEMS in Home).
 */
export function windowSourceHint(windowCapture: WindowCaptureModeDto, isWindows: boolean, t: DesktopT = englishT): string {
  return !isWindows && windowCapture === "window" ? t("windowSource.hintWindow") : t("windowSource.hintArea");
}

/** One line under the chosen window saying what the take will contain. */
export function windowSourceNote(windowCapture: WindowCaptureModeDto, t: DesktopT = englishT): string {
  return windowCapture === "window" ? t("windowSource.noteWindow") : t("windowSource.noteArea");
}

/** Shown on the camera row when the camera cannot be in the recording. */
export function windowOnlyCameraNote(t: DesktopT = englishT): string {
  return t("windowSource.cameraNote");
}
