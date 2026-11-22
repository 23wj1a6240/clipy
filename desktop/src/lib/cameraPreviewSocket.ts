/**
 * Camera preview frames for the bubble window, read from the app's local
 * socket (`camera_preview_ws.rs`). This is Cap's default camera preview
 * transport: raw pixels over a loopback WebSocket, newest frame only, drawn
 * straight onto the canvas.
 *
 * It replaced JPEG frames that were base64-encoded into Tauri events and
 * decoded again here. ScreenCaptureKit records the bubble window, so every
 * step between the camera and the canvas delayed the face in the video
 * against the voice, which reaches the recording without any of them.
 *
 * Wire format: `width * height * 4` bytes of RGBA (straight alpha, rows
 * tightly packed), then a 16-byte trailer of little-endian u32s:
 * width, height, frame number, format tag ("RGBA").
 */

export const CAMERA_PREVIEW_TRAILER_LENGTH = 16;
/** "RGBA" read as a little-endian u32. */
export const CAMERA_PREVIEW_FORMAT_RGBA = 0x41424752;
/** Larger than any camera the app opens (it caps at 1920x1080 and the
 *  preview at 960x540); a frame claiming more is corrupt. */
const MAX_DIMENSION = 4096;

export interface CameraPreviewFrame {
  width: number;
  height: number;
  frameNumber: number;
  /** A view onto the received buffer; nothing is copied. */
  pixels: ImageData["data"];
}

/** Reads one socket message. Returns null for anything malformed, so a bad
 *  frame is skipped instead of drawn as garbage. */
export function parseCameraPreviewFrame(buffer: ArrayBuffer): CameraPreviewFrame | null {
  if (!(buffer instanceof ArrayBuffer) || buffer.byteLength < CAMERA_PREVIEW_TRAILER_LENGTH) {
    return null;
  }
  const trailer = new DataView(buffer, buffer.byteLength - CAMERA_PREVIEW_TRAILER_LENGTH);
  const width = trailer.getUint32(0, true);
  const height = trailer.getUint32(4, true);
  const frameNumber = trailer.getUint32(8, true);
  const format = trailer.getUint32(12, true);
  if (format !== CAMERA_PREVIEW_FORMAT_RGBA) return null;
  if (width === 0 || height === 0 || width > MAX_DIMENSION || height > MAX_DIMENSION) return null;
  const length = width * height * 4;
  if (length !== buffer.byteLength - CAMERA_PREVIEW_TRAILER_LENGTH) return null;
  return { width, height, frameNumber, pixels: new Uint8ClampedArray(buffer, 0, length) };
}

/** The part of `WebSocket` this module uses, so tests can supply a fake. */
export interface CameraPreviewWebSocket {
  binaryType: BinaryType;
  onopen: ((event: Event) => void) | null;
  onmessage: ((event: MessageEvent) => void) | null;
  onclose: ((event: CloseEvent) => void) | null;
  onerror: ((event: Event) => void) | null;
  close(): void;
}

export interface CameraPreviewConnection {
  /** Resolves the socket URL, token included. `null` means the app has no
   *  preview socket (it could not bind one), which retrying will not fix. */
  getUrl: () => Promise<string | null>;
  onFrame: (frame: CameraPreviewFrame) => void;
  /** Called when the preview cannot be shown: no socket at all, or
   *  `UNAVAILABLE_AFTER_FAILURES` connections in a row that delivered
   *  nothing. Reconnecting continues in the second case. Called once per
   *  outage, not once per failed attempt. */
  onUnavailable?: (reason: string) => void;
  /** Called when frames arrive again after `onUnavailable`, so the page can
   *  take down whatever it showed for the outage. */
  onRecovered?: () => void;
  createSocket?: (url: string) => CameraPreviewWebSocket;
}

/** Delays between reconnect attempts; the last one repeats. */
export const RECONNECT_DELAYS_MS = [100, 250, 500, 1000, 2000] as const;
export const UNAVAILABLE_AFTER_FAILURES = 5;

/**
 * Keeps one connection to the preview socket open until the returned
 * function is called, reconnecting with backoff if it drops (the app can
 * restart the camera, and a webview reload drops the socket).
 */
export function connectCameraPreview(connection: CameraPreviewConnection): () => void {
  const createSocket =
    connection.createSocket ?? ((url: string) => new WebSocket(url) as CameraPreviewWebSocket);
  let disposed = false;
  let socket: CameraPreviewWebSocket | null = null;
  let timer: ReturnType<typeof setTimeout> | null = null;
  let failures = 0;
  let reportedUnavailable = false;

  const reportUnavailable = (reason: string) => {
    if (reportedUnavailable) return;
    reportedUnavailable = true;
    connection.onUnavailable?.(reason);
  };

  const scheduleReconnect = () => {
    if (disposed || timer !== null) return;
    const delay = RECONNECT_DELAYS_MS[Math.min(failures, RECONNECT_DELAYS_MS.length - 1)];
    failures += 1;
    if (failures >= UNAVAILABLE_AFTER_FAILURES) {
      reportUnavailable("Camera preview connection keeps failing.");
    }
    timer = setTimeout(() => {
      timer = null;
      void open();
    }, delay);
  };

  const open = async () => {
    let url: string | null;
    try {
      url = await connection.getUrl();
    } catch {
      scheduleReconnect();
      return;
    }
    if (disposed) return;
    if (!url) {
      reportUnavailable("Camera preview is unavailable.");
      return;
    }
    let next: CameraPreviewWebSocket;
    try {
      next = createSocket(url);
    } catch {
      scheduleReconnect();
      return;
    }
    socket = next;
    next.binaryType = "arraybuffer";
    next.onmessage = (event) => {
      if (disposed || socket !== next) return;
      const frame = event.data instanceof ArrayBuffer ? parseCameraPreviewFrame(event.data) : null;
      if (!frame) return;
      // A connection that delivers frames is healthy; start the backoff over,
      // and let the next outage be reported again.
      failures = 0;
      if (reportedUnavailable) {
        reportedUnavailable = false;
        connection.onRecovered?.();
      }
      connection.onFrame(frame);
    };
    const dropped = () => {
      if (socket !== next) return;
      socket = null;
      detach(next);
      scheduleReconnect();
    };
    next.onclose = dropped;
    next.onerror = dropped;
  };

  void open();

  return () => {
    disposed = true;
    if (timer !== null) {
      clearTimeout(timer);
      timer = null;
    }
    if (socket) {
      const current = socket;
      socket = null;
      detach(current);
      try {
        current.close();
      } catch {
        // Already closed.
      }
    }
  };
}

function detach(socket: CameraPreviewWebSocket) {
  socket.onopen = null;
  socket.onmessage = null;
  socket.onclose = null;
  socket.onerror = null;
}
