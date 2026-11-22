import { Effect, EffectState, getCurrentWindow } from "@tauri-apps/api/window";
import type { RecordingModeDto } from "./bridge";
import { isMacOS } from "./platform";

/**
 * Day and night glass.
 *
 * The recorder popover looks different per destination so nobody records to
 * the wrong place: Cloud (it uploads) is light, Local AI (it stays on this
 * machine) is dark. On macOS both are frosted glass over the desktop.
 *
 * Two layers, applied separately:
 *  - `data-recorder-look` on <html> switches the sheet colours in styles.css.
 *    Synchronous, so the first frame after a switch is already right.
 *  - `data-recorder-glass` is set only once macOS has put its blur behind the
 *    window. Until then, and on Windows, the sheet stays opaque.
 */
export type RecorderLook = "day" | "night";

export function recorderLook(mode: RecordingModeDto): RecorderLook {
  return mode === "local" ? "night" : "day";
}

/** Must match `--popover-radius` in styles.css, or the blur's corners show. */
export const POPOVER_GLASS_RADIUS = 18;

export function setRecorderLookAttribute(root: HTMLElement, look: RecorderLook | null): void {
  if (look) root.dataset.recorderLook = look;
  else delete root.dataset.recorderLook;
}

export function setRecorderGlassAttribute(root: HTMLElement, on: boolean): void {
  if (on) root.dataset.recorderGlass = "on";
  else delete root.dataset.recorderGlass;
}

// The native blur goes on once per window and is never changed. In Tauri
// 2.10 on macOS every `setEffects` adds another effect view, and
// `clearEffects` does nothing there (only Windows handles it), so switching
// materials per mode would stack a new view on every switch. Day and night
// differ only in the sheet's own tint (styles.css), over one light blur.
//
// The material is `Light`, a fixed one. Apple marks it deprecated in favour
// of materials that follow the system appearance, but the tint is tuned for
// a light blur, and pinning the appearance instead is not an option: Tauri's
// `setTheme` is app-wide on macOS and would flip every open Clipy window.
let material: Promise<boolean> | null = null;

function applyMaterialOnce(): Promise<boolean> {
  material ??= getCurrentWindow()
    .setEffects({ effects: [Effect.Light], state: EffectState.Active, radius: POPOVER_GLASS_RADIUS })
    .then(
      () => true,
      (err) => {
        console.warn("recorder glass unavailable; keeping the sheet opaque", err);
        return false;
      },
    );
  return material;
}

/**
 * Turn glass on for the recorder, macOS only. `Active` keeps the blur frosted
 * when the popover is not the key window, instead of turning flat grey.
 */
export async function applyRecorderGlass(): Promise<boolean> {
  const root = document.documentElement;
  if (!isMacOS) {
    setRecorderGlassAttribute(root, false);
    return false;
  }
  const ok = await applyMaterialOnce();
  // The recorder may have unmounted while the effect was being applied.
  setRecorderGlassAttribute(root, ok && root.dataset.recorderLook !== undefined);
  return ok;
}

/**
 * Back to a plain sheet for the sign-in screen and loaders. The native blur
 * stays behind the window (it cannot be removed, see above), but those
 * screens are opaque, so it does not show.
 */
export function clearRecorderGlass(): void {
  const root = document.documentElement;
  setRecorderGlassAttribute(root, false);
  setRecorderLookAttribute(root, null);
}

/**
 * For windows that follow the system appearance instead of a recording mode
 * (the clipboard history picker): day when the OS is light, night when it is
 * dark, updated live when the user switches. Only touches the document it is
 * given, so other Clipy windows keep their own look. Returns the cleanup.
 */
export function followSystemLook(root: HTMLElement = document.documentElement): () => void {
  const query = window.matchMedia?.("(prefers-color-scheme: dark)");
  const apply = () => setRecorderLookAttribute(root, query?.matches ? "night" : "day");
  apply();
  query?.addEventListener("change", apply);
  return () => {
    query?.removeEventListener("change", apply);
    setRecorderLookAttribute(root, null);
  };
}
