/**
 * Host-platform detection for the webview.
 *
 * Deliberately reads `navigator.userAgent` rather than pulling in
 * `@tauri-apps/plugin-os`: this has to resolve *synchronously* at module load
 * so `applyPlatformAttribute()` can stamp the root element before first paint.
 * An async round-trip to Rust would let one frame render with the wrong
 * platform styling, which is exactly the flash we are trying to avoid.
 *
 * The UA strings are stable for the two webviews we ship on — WebView2
 * (Edge/Chromium) always carries "Windows NT", WKWebView always carries
 * "Macintosh". Anything else is treated as `"other"` and gets the macOS-ish
 * defaults, which is what Linux dev builds already assumed.
 */

export type HostPlatform = "windows" | "macos" | "other";

function detect(): HostPlatform {
  if (typeof navigator === "undefined") return "other";
  const ua = navigator.userAgent;
  if (ua.includes("Windows NT")) return "windows";
  if (ua.includes("Macintosh") || ua.includes("Mac OS X")) return "macos";
  return "other";
}

export const hostPlatform: HostPlatform = detect();
export const isWindows = hostPlatform === "windows";
export const isMacOS = hostPlatform === "macos";
export const deviceName = isWindows ? "PC" : "Mac";

/**
 * Stamp `data-os` on `<html>` so CSS can scope platform-specific rules.
 *
 * Used by rules that must NOT reach macOS — e.g. the explicit `<option>`
 * colours WebView2 needs, which would otherwise override WKWebView's native
 * popup rendering. Call once, as early as possible.
 */
export function applyPlatformAttribute(): void {
  if (typeof document === "undefined") return;
  document.documentElement.dataset.os = hostPlatform;
}
