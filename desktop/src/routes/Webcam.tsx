import CameraFrameOverlay from "../components/CameraFrameOverlay";
import { useEffect, useRef, useState } from "react";
import { getCurrentWindow } from "@tauri-apps/api/window";
import {
  getAppSettings,
  getCameraPreviewUrl,
  getWebcamBubbleState,
  onCameraLookChanged,
  onNativeWebcamFrame,
  onNativeWebcamStart,
  onNativeWebcamStop,
  onRecordingStateChanged,
  onWebcamLayoutChanged,
  onWebcamStart,
  onWebcamStop,
  saveWebcamSidecar,
  reportWebviewError,
  type NativeWebcamFrameDto,
  type RecordingStateDto,
  type WebcamLayoutDto,
  type WebcamShapeDto,
} from "../lib/bridge";
import { isMacOS, isWindows } from "../lib/platform";
import { canvasBackingSize, coverSourceRect } from "../lib/cameraCanvas";
import { webcamShapeForLayout } from "../lib/webcamAppearance";
import {
  DEFAULT_CAMERA_LOOK,
  autoLightingGain,
  easeGain,
  lightingFilter,
  meanLuminance,
  touchUpAlpha,
  touchUpDownscale,
  type CameraLook,
} from "../lib/cameraLook";
import { DEFAULT_CAMERA_EFFECTS, getCameraEffects, onCameraEffectsChanged, onCameraEffectsError } from "../lib/cameraEffects";
import type { CameraCompositor } from "../lib/cameraCompositor";
import { connectCameraPreview, type CameraPreviewFrame } from "../lib/cameraPreviewSocket";
import { englishT, useDesktopT, type DesktopT } from "../lib/messages";

// The camera bubble is captured into recordings, so its notices follow the
// app language (desktop.webcam.*).
const CAMERA_EFFECT_UNAVAILABLE = englishT("webcam.effectUnavailable");

/**
 * Webcam bubble — a transparent always-on-top window the user can drag
 * around the screen while recording. SCK captures the window as part of
 * the display, so the bubble ends up in the final video without us
 * writing any compositing code.
 *
 * IMPORTANT: we draw video frames into a `<canvas>` instead of binding
 * `srcObject` to a `<video>` element. WKWebView renders HTML5 video
 * through a separate CAVideoLayer path that ScreenCaptureKit captures
 * inconsistently — the live pixels show on screen but read back as
 * empty when the host window is transparent + borderless. Drawing to a
 * canvas pushes the frames into the standard CALayer compositor,
 * which SCK captures reliably. Cap.so uses the same workaround.
 *
 * Permission flow: Rust requests AVFoundation Camera permission and
 * streams native camera frames into this window: raw RGBA over the local
 * preview socket on macOS (Cap's transport, `lib/cameraPreviewSocket.ts`),
 * JPEG events from the Windows camera recorder. `getUserMedia` remains only
 * as a fallback for platforms without the native bridge.
 */
export default function Webcam() {
  const t = useDesktopT();
  const canvasRef = useRef<HTMLCanvasElement | null>(null);
  const videoRef = useRef<HTMLVideoElement | null>(null);
  const streamRef = useRef<MediaStream | null>(null);
  const rafIdRef = useRef<number | null>(null);
  const recordingStateRef = useRef<RecordingStateDto | null>(null);
  const mediaRecorderRef = useRef<MediaRecorder | null>(null);
  const cameraChunksRef = useRef<Blob[]>([]);
  const cameraRecordingIdRef = useRef<string | null>(null);
  const cameraMimeTypeRef = useRef<string>("video/webm");
  const nativeFrameGenerationRef = useRef(0);
  const mirroredRef = useRef(true);
  const lookRef = useRef<CameraLook>(DEFAULT_CAMERA_LOOK);
  const lookStateRef = useRef<LookState>({ gain: 1, frame: 0 });
  // Preview socket health, kept apart from `error`: other events clear
  // `error` (an effect change, a camera restart), but only frames coming back
  // end an outage.
  const [previewUnavailable, setPreviewUnavailable] = useState(false);
  const [active, setActive] = useState(false);
  const [nativeMode, setNativeMode] = useState(false);
  const [webFallbackEnabled, setWebFallbackEnabled] = useState(false);
  const [deviceId, setDeviceId] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [layout, setLayout] = useState<WebcamLayoutDto>("bubble");
  const [shape, setShape] = useState<WebcamShapeDto>("square");
  const [effects, setEffects] = useState(DEFAULT_CAMERA_EFFECTS);
  // Windows only: effects are composited here (camera_effects.m does it on macOS).
  const compositorLoadRef = useRef<Promise<CameraCompositor> | null>(null);
  const compositorRef = useRef<CameraCompositor | null>(null);

  useEffect(() => {
    if (!isWindows) return;
    if (effects.background === "original") {
      compositorRef.current = null;
      return;
    }
    let stale = false;
    compositorLoadRef.current ??= import("../lib/cameraCompositor")
      .then(({ CameraCompositor }) => CameraCompositor.create())
      .catch((err) => {
        compositorLoadRef.current = null;
        throw err;
      });
    compositorLoadRef.current
      .then(async (compositor) => {
        await compositor.prepare(effects);
        if (!stale) compositorRef.current = compositor;
      })
      .catch((err) => {
        if (stale) return;
        compositorRef.current = null;
        reportWebviewError(`camera effect failed: ${webcamDebugMessage(err)}`, err instanceof Error ? err.stack ?? null : null, "webcam.effects");
        setError(CAMERA_EFFECT_UNAVAILABLE);
      });
    return () => {
      stale = true;
    };
  }, [effects]);

  useEffect(() => () => {
    compositorLoadRef.current?.then((compositor) => compositor.close()).catch(() => {});
  }, []);

  useEffect(() => {
    let disposed = false;
    const unlisteners: Array<() => void> = [];
    const register = (unlisten: () => void) => disposed ? unlisten() : unlisteners.push(unlisten);
    getCameraEffects().then((value) => { if (!disposed) setEffects(value); }).catch((error) => { if (!disposed) setError(String(error)); });
    onCameraEffectsChanged((value) => { setEffects(value); setError(null); }).then(register);
    onCameraEffectsError(setError).then(register);
    // The look is an app setting; a change lands on the next drawn frame.
    // On macOS the native processor applies it to the frames it sends here
    // (and to camera-only recordings), so the canvas must not apply it a
    // second time. Windows draws from getUserMedia, so it applies it here.
    if (!isMacOS) {
      getAppSettings()
        .then((settings) => {
          if (disposed) return;
          lookRef.current = {
            touchUpEnabled: settings.touchUpEnabled,
            touchUpStrength: settings.touchUpStrength,
            autoLighting: settings.autoLighting,
          };
        })
        .catch(() => {});
      onCameraLookChanged((look) => { lookRef.current = look; }).then(register);
    }
    return () => { disposed = true; unlisteners.forEach((unlisten) => unlisten()); };
  }, []);

  useEffect(() => {
    let unlistenStart: (() => void) | undefined;
    let unlistenStop: (() => void) | undefined;
    let unlistenNativeStart: (() => void) | undefined;
    let unlistenNativeStop: (() => void) | undefined;
    let unlistenNativeFrame: (() => void) | undefined;
    let unlistenLayout: (() => void) | undefined;
    let unlistenRecording: (() => void) | undefined;
    let disposed = false;

    // Fails CLOSED. `webFallbackEnabled` is what makes this window open the
    // camera itself via WKWebView getUserMedia — the same physical device Rust
    // is opening through AVFoundation, which is the leading suspect for the
    // hard abort on "Camera bubble". It used to be
    // set true on ANY invoke failure, and true during the multi-second window
    // where the bubble is shown but the native camera hasn't started yet
    // (`active: true, native: false`) — so an error, a reload, or plain bad
    // timing sent the webview racing Rust for the camera.
    //
    // Capture is native-only ON macOS, so the fallback buys nothing there: if
    // the native camera can't start, the caller already surfaces an error and
    // hides the bubble. Never enable it on macOS.
    //
    // Windows has NO native camera bridge at all — `start_native_webcam` is a
    // no-op stub that reports success without opening anything, so
    // `onNativeWebcamFrame` never fires. With the fallback also disabled, the
    // bubble window rendered nothing, forever. Nothing competes for the device
    // there, so the race that motivated the macOS ban cannot occur, and
    // getUserMedia is the ONLY frame source. WGC captures the bubble window as
    // part of the display, exactly as SCK does on macOS.
    const reconcileBubbleState = (fallbackDeviceId: string | null) => {
      setWebFallbackEnabled(!isMacOS);
      getWebcamBubbleState()
        .then((state) => {
          if (disposed || !state.active) return;
          setDeviceId(state.deviceId);
          setNativeMode(state.native);
          setLayout(state.layout);
          setShape(state.shape);
          mirroredRef.current = state.mirrored;
          setActive(true);
        })
        .catch(() => {
          if (disposed) return;
          setDeviceId(fallbackDeviceId);
          setNativeMode(false);
          setActive(true);
        });
    };

    onWebcamStart((nextDeviceId) => {
      setDeviceId(nextDeviceId);
      setError(null);
      reconcileBubbleState(nextDeviceId);
    }).then((fn) => {
      unlistenStart = fn;
      reconcileBubbleState(null);
    });
    onWebcamStop(() => {
      nativeFrameGenerationRef.current += 1;
      setNativeMode(false);
      setWebFallbackEnabled(false);
      setActive(false);
    }).then((fn) => {
      unlistenStop = fn;
    });
    onNativeWebcamStart(() => {
      setNativeMode(true);
      setWebFallbackEnabled(false);
      setActive(true);
      setError(null);
    }).then((fn) => {
      unlistenNativeStart = fn;
    });
    onNativeWebcamStop(() => {
      nativeFrameGenerationRef.current += 1;
      setNativeMode(false);
      setWebFallbackEnabled(false);
      setActive(false);
    }).then((fn) => {
      unlistenNativeStop = fn;
    });
    onNativeWebcamFrame((frame) => {
      if (disposed) return;
      setNativeMode(true);
      setWebFallbackEnabled(false);
      setActive(true);
      setError(null);
      drawNativeFrame(frame);
    }).then((fn) => {
      unlistenNativeFrame = fn;
    });
    onWebcamLayoutChanged((appearance) => {
      setLayout(appearance.layout);
      setShape(appearance.shape);
      mirroredRef.current = appearance.mirrored;
    }).then((fn) => {
      unlistenLayout = fn;
    });
    onRecordingStateChanged((state) => {
      recordingStateRef.current = state;
      if (
        (state.phase === "recording" || state.phase === "paused") &&
        state.recording_id
      ) {
        startCameraSidecar(state.recording_id);
        // Keep the camera sidecar's timeline aligned with the video's
        // pause-excluded timeline: pause/resume the MediaRecorder with
        // the recording phase (paused wall-time never hits the webm).
        const recorder = mediaRecorderRef.current;
        try {
          if (state.phase === "paused" && recorder?.state === "recording") {
            recorder.pause();
          } else if (state.phase === "recording" && recorder?.state === "paused") {
            recorder.resume();
          }
        } catch (err) {
          console.warn("camera sidecar pause/resume failed", err);
        }
      } else {
        stopCameraSidecar();
      }
    }).then((fn) => {
      unlistenRecording = fn;
    });
    return () => {
      disposed = true;
      unlistenStart?.();
      unlistenStop?.();
      unlistenNativeStart?.();
      unlistenNativeStop?.();
      unlistenNativeFrame?.();
      unlistenLayout?.();
      unlistenRecording?.();
    };
  }, []);

  // macOS: read raw frames from the preview socket while the native camera
  // runs. Each frame is drawn the moment it arrives; the socket only ever
  // holds the newest one, so the bubble never works through a backlog.
  //
  // If no frames can arrive, the bubble says so instead of sitting blank or
  // frozen: it is recorded with the screen, so a silent failure would put a
  // dead camera in the video with nothing telling the user why.
  useEffect(() => {
    if (!isMacOS || !active || !nativeMode) return;
    const dispose = connectCameraPreview({
      getUrl: getCameraPreviewUrl,
      onFrame: drawPreviewFrame,
      onUnavailable: (reason) => {
        reportWebviewError(`camera preview socket: ${reason}`, null, "webcam.previewSocket");
        setPreviewUnavailable(true);
      },
      onRecovered: () => setPreviewUnavailable(false),
    });
    return () => {
      dispose();
      setPreviewUnavailable(false);
    };
  }, [active, nativeMode]);

  const drawPreviewFrame = (frame: CameraPreviewFrame) => {
    const canvas = canvasRef.current;
    if (!canvas) return;
    previewScratch ??= document.createElement("canvas");
    if (previewScratch.width !== frame.width || previewScratch.height !== frame.height) {
      previewScratch.width = frame.width;
      previewScratch.height = frame.height;
    }
    const context = previewScratch.getContext("2d");
    if (!context) return;
    context.putImageData(new ImageData(frame.pixels, frame.width, frame.height), 0, 0);
    drawMirroredCover(canvas, previewScratch, frame.width, frame.height, mirroredRef.current, lookRef.current, lookStateRef.current);
  };

  const drawNativeFrame = (frame: NativeWebcamFrameDto) => {
    const generation = nativeFrameGenerationRef.current;
    const image = new Image();
    image.onload = () => {
      if (generation !== nativeFrameGenerationRef.current) return;
      const canvas = canvasRef.current;
      if (!canvas) return;
      drawMirroredCover(canvas, image, frame.width, frame.height, mirroredRef.current, lookRef.current, lookStateRef.current);
    };
    image.onerror = () => {
      reportWebviewError("native webcam frame decode failed", null, "webcam.nativeFrame");
    };
    image.src = frame.dataUrl;
  };

  useEffect(() => {
    if (!active || nativeMode || !webFallbackEnabled) return;

    let cancelled = false;

    const drawFrame = () => {
      const canvas = canvasRef.current;
      const video = videoRef.current;
      if (!video || cancelled) return;
      // The canvas unmounts while an error shows; keep the loop alive so
      // drawing resumes once the error clears.
      if (!canvas) {
        rafIdRef.current = requestAnimationFrame(drawFrame);
        return;
      }

      // Canvas sizing has ONE source, `backingSizeFor`, shared with
      // `drawMirroredCover`. This loop used to size the canvas itself in
      // layout points, so the two disagreed about the backing store as soon as
      // either changed — and the compositor then composited at half the
      // resolution the bubble is actually drawn at on a Retina display.
      if (video.readyState >= 2 && video.videoWidth > 0) {
        const compositor = compositorRef.current;
        const { width: w, height: h } = backingSizeFor(canvas);
        if (compositor && w > 0 && h > 0) {
          // Composite at the size the cover crop displays, then crop and mirror as usual.
          const scale = Math.max(w / video.videoWidth, h / video.videoHeight);
          const width = Math.max(1, Math.round(video.videoWidth * scale));
          const height = Math.max(1, Math.round(video.videoHeight * scale));
          try {
            drawMirroredCover(canvas, compositor.render(video, width, height), width, height, mirroredRef.current, lookRef.current, lookStateRef.current);
          } catch (err) {
            compositorRef.current = null;
            reportWebviewError(`camera effect failed: ${webcamDebugMessage(err)}`, err instanceof Error ? err.stack ?? null : null, "webcam.effects");
            setError(CAMERA_EFFECT_UNAVAILABLE);
          }
        } else {
          drawMirroredCover(canvas, video, video.videoWidth, video.videoHeight, mirroredRef.current, lookRef.current, lookStateRef.current);
        }
      }

      rafIdRef.current = requestAnimationFrame(drawFrame);
    };

    (async () => {
      try {
        setError(null);
        const stream = await openWebcamStream(deviceId);
        if (cancelled) {
          stream.getTracks().forEach((t) => t.stop());
          return;
        }
        streamRef.current = stream;

        // Off-DOM video element drives the camera capture; we never
        // mount it, so WKWebView's video-layer path never kicks in.
        const video = document.createElement("video");
        video.muted = true;
        video.autoplay = true;
        video.playsInline = true;
        video.srcObject = stream;
        videoRef.current = video;
        await video.play().catch(() => {});

        const state = recordingStateRef.current;
        if (
          (state?.phase === "recording" || state?.phase === "paused") &&
          state.recording_id
        ) {
          startCameraSidecar(state.recording_id);
        }

        rafIdRef.current = requestAnimationFrame(drawFrame);
      } catch (err) {
        const cause = err instanceof WebcamOpenError ? err.lastCause : err;
        const message = webcamUserMessage(cause);
        reportWebviewError(
          `webcam getUserMedia failed: ${webcamDebugMessage(err)}`,
          err instanceof Error ? err.stack : null,
          "webcam.getUserMedia",
          // No camera, or camera access denied, is the user's setup, not a
          // bug; the bubble already tells them what to do.
          err instanceof WebcamOpenError && err.expected ? "warning" : "error",
        );
        setError(message);
      }
    })();

    return () => {
      cancelled = true;
      if (rafIdRef.current !== null) {
        cancelAnimationFrame(rafIdRef.current);
        rafIdRef.current = null;
      }
      const stream = streamRef.current;
      if (stream) {
        stopCameraSidecar();
        stream.getTracks().forEach((t) => t.stop());
        streamRef.current = null;
      }
      const video = videoRef.current;
      if (video) {
        video.pause();
        video.srcObject = null;
        videoRef.current = null;
      }
    };
  }, [active, deviceId, nativeMode, webFallbackEnabled]);

  function startCameraSidecar(recordingId: string) {
    if (mediaRecorderRef.current || cameraRecordingIdRef.current === recordingId) return;
    const stream = streamRef.current;
    if (!stream) return;
    if (typeof MediaRecorder === "undefined") return;

    const candidates = [
      "video/webm;codecs=vp9",
      "video/webm;codecs=vp8",
      "video/webm",
    ];
    const mimeType =
      candidates.find((candidate) => MediaRecorder.isTypeSupported(candidate)) ?? "";

    try {
      const recorder = new MediaRecorder(stream, {
        ...(mimeType ? { mimeType } : {}),
        videoBitsPerSecond: 700_000,
      });
      cameraChunksRef.current = [];
      cameraRecordingIdRef.current = recordingId;
      cameraMimeTypeRef.current = recorder.mimeType || mimeType || "video/webm";
      recorder.ondataavailable = (event) => {
        if (event.data.size > 0) cameraChunksRef.current.push(event.data);
      };
      recorder.onstop = () => {
        void persistCameraSidecar(recordingId, cameraMimeTypeRef.current);
      };
      recorder.start(1000);
      mediaRecorderRef.current = recorder;
    } catch (err) {
      console.warn("camera sidecar recorder failed", err);
      mediaRecorderRef.current = null;
      cameraRecordingIdRef.current = null;
      cameraChunksRef.current = [];
    }
  }

  function stopCameraSidecar() {
    const recorder = mediaRecorderRef.current;
    if (!recorder) return;
    mediaRecorderRef.current = null;
    if (recorder.state !== "inactive") {
      recorder.stop();
    }
  }

  async function persistCameraSidecar(recordingId: string, mimeType: string) {
    const chunks = cameraChunksRef.current;
    cameraChunksRef.current = [];
    cameraRecordingIdRef.current = null;
    if (chunks.length === 0) return;

    try {
      const blob = new Blob(chunks, { type: mimeType });
      const dataBase64 = await blobToBase64(blob);
      await saveWebcamSidecar(recordingId, dataBase64, mimeType);
    } catch (err) {
      console.warn("camera sidecar save failed", err);
    }
  }

  // Messages are stored in English (also sent to telemetry) and shown here in
  // the app language.
  const shownError = error
    ? translateWebcamMessage(error, t)
    : previewUnavailable
      ? t("webcam.previewUnavailable")
      : null;

  // Start dragging explicitly so every child is a handle. The hover pill
  // under the bubble is shown and hidden natively from the pointer position
  // (webcam_controls.rs), so this webview does not track hover at all.
  return (
    <div
      onPointerDown={(event) => {
        if (event.button !== 0) return;
        event.preventDefault();
        void getCurrentWindow().startDragging()
          .catch((err) => reportWebviewError(String(err), null, "webcam.drag"));
      }}
      className="relative flex h-full w-full items-center justify-center bg-transparent"
      style={{ cursor: "grab" }}
    >
      <div
        data-webcam-shape={webcamShapeForLayout(layout, shape)}
        data-webcam-cutout={effects.background === "remove" && effects.canvas === "transparent" ? "true" : undefined}
        className={`webcam-bubble webcam-layout-${layout} relative h-full w-full overflow-hidden`}
      >
        <CameraFrameOverlay frame={effects.frame} shape={webcamShapeForLayout(layout, shape)} />
        {shownError ? (
          <div
            className="flex h-full w-full items-center justify-center bg-ink-900/90 px-3 text-center text-[11px] leading-tight text-stone-300"
          >
            {shownError}
          </div>
        ) : !active ? (
          <div className="h-full w-full bg-transparent" />
        ) : (
          <canvas
            ref={canvasRef}
            className="pointer-events-none h-full w-full"
          />
        )}
      </div>
    </div>
  );
}

function blobToBase64(blob: Blob): Promise<string> {
  return new Promise((resolve, reject) => {
    const reader = new FileReader();
    reader.onerror = () => reject(reader.error ?? new Error("camera sidecar read failed"));
    reader.onload = () => {
      const result = String(reader.result ?? "");
      resolve(result.includes(",") ? result.split(",", 2)[1] : result);
    };
    reader.readAsDataURL(blob);
  });
}

async function openWebcamStream(deviceId: string | null): Promise<MediaStream> {
  const base: MediaTrackConstraints = {
    width: { ideal: 1280 },
    height: { ideal: 720 },
  };
  const attempts: Array<boolean | MediaTrackConstraints> = deviceId
    ? [{ ...base, deviceId: { exact: deviceId } }, base, true]
    : [base, true];
  const causes: unknown[] = [];

  for (const video of attempts) {
    try {
      return await navigator.mediaDevices.getUserMedia({ video, audio: false });
    } catch (err) {
      causes.push(err);
    }
  }

  throw new WebcamOpenError(causes);
}

/** getUserMedia DOMException names that describe the machine, not a bug:
 *  no camera attached, the chosen one is gone, or access was denied. */
const EXPECTED_WEBCAM_FAILURES = new Set([
  "NotFoundError",
  "OverconstrainedError",
  "NotAllowedError",
  "SecurityError",
]);

/** Every failed attempt from `openWebcamStream`, so the user message can
 *  come from the real DOMException rather than the joined debug text. */
class WebcamOpenError extends Error {
  readonly lastCause: unknown;
  readonly expected: boolean;

  constructor(causes: unknown[]) {
    super(causes.map(webcamDebugMessage).join(" | ") || "Could not start webcam.");
    this.name = "WebcamOpenError";
    this.lastCause = causes[causes.length - 1];
    this.expected =
      causes.length > 0 &&
      causes.every(
        (cause) => cause instanceof DOMException && EXPECTED_WEBCAM_FAILURES.has(cause.name),
      );
  }
}

const WEBCAM_MESSAGE_KEYS = ["effectUnavailable", "permission", "busy", "notFound", "failed"] as const;

function webcamUserMessage(err: unknown): string {
  if (err instanceof DOMException) {
    if (err.name === "NotAllowedError" || err.name === "SecurityError") {
      return englishT("webcam.permission");
    }
    if (err.name === "NotReadableError") {
      return englishT("webcam.busy");
    }
    if (err.name === "NotFoundError" || err.name === "OverconstrainedError") {
      return englishT("webcam.notFound");
    }
  }
  return englishT("webcam.failed");
}

/** An English bubble message in the app language; anything else as is. */
function translateWebcamMessage(message: string, t: DesktopT): string {
  const key = WEBCAM_MESSAGE_KEYS.find((k) => englishT(`webcam.${k}`) === message);
  return key ? t(`webcam.${key}`) : message;
}

function webcamDebugMessage(err: unknown): string {
  if (err instanceof DOMException) {
    return `${err.name}: ${err.message}`;
  }
  if (err instanceof Error) {
    return err.message;
  }
  return String(err);
}

/**
 * The canvas's backing store in DEVICE pixels, not layout points, so the
 * bubble is drawn at the resolution the window really occupies. Everything
 * that needs to know how big the canvas is goes through here — the draw loop
 * composites at this size and `drawMirroredCover` allocates it — because two
 * places computing it independently is how they came to disagree.
 */
function backingSizeFor(canvas: HTMLCanvasElement) {
  return canvasBackingSize(
    canvas.clientWidth,
    canvas.clientHeight,
    typeof window === "undefined" ? 1 : window.devicePixelRatio || 1,
  );
}

/** Per-bubble state for the look: the eased exposure gain and a frame
 *  counter that paces the luminance probe. */
interface LookState {
  gain: number;
  frame: number;
}

/** Frames between luminance probes; exposure eases toward each reading. */
const LOOK_PROBE_INTERVAL = 6;
const LOOK_PROBE_SIZE = 16;
let softCanvas: HTMLCanvasElement | null = null;
/** Holds the latest preview-socket frame at its own size before the cover
 *  crop draws it into the bubble. */
let previewScratch: HTMLCanvasElement | null = null;
let probeCanvas: HTMLCanvasElement | null = null;

/** Touch up and auto lighting, applied after the frame is on the canvas so
 *  every frame source (native, getUserMedia, composited) gets the same
 *  treatment. The blend reads the canvas back into a small copy and draws it
 *  over itself; exposure goes on as a CSS filter, which WebKit composites
 *  into the window pixels ScreenCaptureKit records. */
function applyCameraLook(
  canvas: HTMLCanvasElement,
  ctx: CanvasRenderingContext2D,
  width: number,
  height: number,
  look: CameraLook,
  state: LookState,
) {
  const alpha = touchUpAlpha(look);
  if (alpha > 0) {
    const downscale = touchUpDownscale(look);
    const softWidth = Math.max(1, Math.round(width * downscale));
    const softHeight = Math.max(1, Math.round(height * downscale));
    softCanvas ??= document.createElement("canvas");
    if (softCanvas.width !== softWidth || softCanvas.height !== softHeight) {
      softCanvas.width = softWidth;
      softCanvas.height = softHeight;
    }
    const soft = softCanvas.getContext("2d");
    if (soft) {
      soft.imageSmoothingEnabled = true;
      soft.imageSmoothingQuality = "high";
      soft.clearRect(0, 0, softWidth, softHeight);
      soft.drawImage(canvas, 0, 0, width, height, 0, 0, softWidth, softHeight);
      ctx.save();
      ctx.globalAlpha = alpha;
      ctx.imageSmoothingEnabled = true;
      ctx.imageSmoothingQuality = "high";
      ctx.drawImage(softCanvas, 0, 0, softWidth, softHeight, 0, 0, width, height);
      ctx.restore();
    }
  }

  if (!look.autoLighting) {
    if (state.gain !== 1 || canvas.style.filter) {
      state.gain = 1;
      canvas.style.filter = "";
    }
    return;
  }
  state.frame += 1;
  if (state.frame % LOOK_PROBE_INTERVAL !== 0) return;
  probeCanvas ??= document.createElement("canvas");
  if (probeCanvas.width !== LOOK_PROBE_SIZE) {
    probeCanvas.width = LOOK_PROBE_SIZE;
    probeCanvas.height = LOOK_PROBE_SIZE;
  }
  const probe = probeCanvas.getContext("2d", { willReadFrequently: true });
  if (!probe) return;
  probe.clearRect(0, 0, LOOK_PROBE_SIZE, LOOK_PROBE_SIZE);
  probe.drawImage(canvas, 0, 0, width, height, 0, 0, LOOK_PROBE_SIZE, LOOK_PROBE_SIZE);
  const luma = meanLuminance(probe.getImageData(0, 0, LOOK_PROBE_SIZE, LOOK_PROBE_SIZE).data);
  state.gain = easeGain(state.gain, autoLightingGain(luma));
  const filter = lightingFilter(state.gain);
  if (canvas.style.filter !== filter) canvas.style.filter = filter;
}

function drawMirroredCover(
  canvas: HTMLCanvasElement,
  source: CanvasImageSource,
  sourceWidth: number,
  sourceHeight: number,
  mirrored = true,
  look: CameraLook = DEFAULT_CAMERA_LOOK,
  lookState: LookState = { gain: 1, frame: 0 },
) {
  const { width, height } = backingSizeFor(canvas);
  if (canvas.width !== width || canvas.height !== height) {
    canvas.width = width;
    canvas.height = height;
  }

  const ctx = canvas.getContext("2d");
  if (!ctx || sourceWidth <= 0 || sourceHeight <= 0) return;

  const { sx, sy, sw, sh } = coverSourceRect(sourceWidth, sourceHeight, width, height);

  ctx.save();
  ctx.clearRect(0, 0, width, height);
  // The camera frame arrives smaller than the bubble on a large size, so the
  // one resample left in the chain is this one. Ask for the good filter.
  ctx.imageSmoothingEnabled = true;
  ctx.imageSmoothingQuality = "high";
  if (mirrored) {
    ctx.translate(width, 0);
    ctx.scale(-1, 1);
  }
  ctx.drawImage(source, sx, sy, sw, sh, 0, 0, width, height);
  ctx.restore();
  applyCameraLook(canvas, ctx, width, height, look, lookState);
}
