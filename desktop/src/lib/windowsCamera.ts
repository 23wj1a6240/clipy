/**
 * Windows lists every camera twice: the webview's opaque, origin-scoped ids,
 * which the camera bubble opens through `getUserMedia`, and Media Foundation
 * symbolic links, which the native camera-only recorder opens. The two can
 * never be compared directly, so this matches a webview camera to its Media
 * Foundation device.
 *
 * Only identity that names the device counts: the USB vendor and product ids
 * Chromium appends to a label, or the label itself when exactly one device
 * carries it. The two lists are enumerated by different stacks and nothing
 * guarantees they come out in the same order, so a position is never used to
 * pick a camera; an ambiguous match returns `null` rather than risking a
 * recording from the wrong device.
 */
export type CameraDevice = { id: string; name: string };

// Chromium appends the USB vendor and product ids to a camera's label.
const USB_IDS = /\(([0-9a-f]{4}):([0-9a-f]{4})\)\s*$/i;

/** The Media Foundation id for `browserId`, or `null` when it cannot be told apart. */
export function nativeCameraId(
  browserId: string,
  browserDevices: readonly CameraDevice[],
  nativeDevices: readonly CameraDevice[],
): string | null {
  if (nativeDevices.length === 1) return nativeDevices[0].id;
  const browser = browserDevices.find((device) => device.id === browserId);
  if (!browser) return null;

  const usb = browser.name.match(USB_IDS);
  if (usb) {
    const needle = `vid_${usb[1]}&pid_${usb[2]}`.toLowerCase();
    const matches = nativeDevices.filter((device) => device.id.toLowerCase().includes(needle));
    if (matches.length === 1) return matches[0].id;
  }

  const label = browser.name.replace(USB_IDS, "").trim().toLowerCase();
  const named = nativeDevices.filter((device) => device.name.trim().toLowerCase() === label);
  if (named.length === 1) return named[0].id;

  return null;
}
