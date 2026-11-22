import { useEffect, useRef, useState } from "react";
import { useTranslations } from "use-intl";
import {
  getRecordingState,
  getWebcamBubbleState,
  hideWebcamBubble,
  onWebcamLayoutChanged,
  toggleRecordingWebcam,
  updateWebcamAppearance,
  type WebcamSizeDto,
} from "../lib/bridge";
import { DEFAULT_WEBCAM_SIZE } from "../lib/webcamAppearance";

const SIZES: Array<{ value: WebcamSizeDto; dot: string }> = [
  { value: "small", dot: "h-2 w-2" },
  { value: "medium", dot: "h-3 w-3" },
  { value: "large", dot: "h-4 w-4" },
];

/**
 * The hover pill under the camera: turn it off, or pick a size. Effects
 * and shape live on the recorder's Effects sheet, so nothing else is here.
 * Showing and hiding is native (webcam_controls.rs reads the pointer), so
 * this window reports nothing about hover.
 */
export default function WebcamControls() {
  const t = useTranslations("desktop.webcamControls");
  const busyRef = useRef(false);
  const [size, setSize] = useState<WebcamSizeDto>(DEFAULT_WEBCAM_SIZE);
  // Camera-only takes keep capturing when the bubble is hidden, so the
  // button must say so; the dock draws the same line.
  const [cameraOnly, setCameraOnly] = useState(false);

  useEffect(() => {
    let disposed = false;
    let off: (() => void) | undefined;
    getWebcamBubbleState().then((state) => { if (!disposed) { setSize(state.size); setCameraOnly(state.cameraOnly); } }).catch(() => {});
    onWebcamLayoutChanged((appearance) => setSize(appearance.size)).then((fn) => {
      if (disposed) fn(); else off = fn;
    });
    return () => { disposed = true; off?.(); };
  }, []);

  const pickSize = async (value: WebcamSizeDto) => {
    if (busyRef.current) return;
    busyRef.current = true;
    try {
      const current = await getWebcamBubbleState();
      await updateWebcamAppearance(current.shape, value, current.mirrored);
      setSize(value);
    } catch {
      // The bubble keeps its old size; nothing to show in a 44px pill.
    } finally {
      busyRef.current = false;
    }
  };

  const turnOff = async () => {
    if (busyRef.current) return;
    busyRef.current = true;
    try {
      const state = await getRecordingState();
      if (state.phase === "recording" || state.phase === "paused") await toggleRecordingWebcam();
      else await hideWebcamBubble();
    } catch {
      // Same as above: a failed toggle leaves the camera as it was.
    } finally {
      busyRef.current = false;
    }
  };

  return (
    <main
      role="toolbar"
      aria-label={t("label")}
      className="flex h-full w-full items-center justify-center bg-transparent"
    >
      <div className="flex h-9 items-center gap-1 rounded-full bg-[#2a2b31] px-2 text-white shadow-[0_2px_5px_rgba(0,0,0,0.35),0_0_0_1px_rgba(255,255,255,0.06)]">
        <button
          type="button"
          aria-label={cameraOnly ? t("hidePreview") : t("turnOff")}
          title={cameraOnly ? t("hidePreviewHint") : t("turnOff")}
          onClick={() => void turnOff()}
          className="flex h-7 w-7 items-center justify-center rounded-full hover:bg-white/10"
        >
          <svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2.4" strokeLinecap="round" aria-hidden>
            <path d="M6 6l12 12M18 6 6 18" />
          </svg>
        </button>
        <div role="radiogroup" aria-label={t("size")} className="flex items-center gap-1">
          {SIZES.map((option) => (
            <button
              key={option.value}
              type="button"
              role="radio"
              aria-checked={size === option.value}
              aria-label={t(option.value)}
              title={t(option.value)}
              onClick={() => void pickSize(option.value)}
              className="flex h-7 w-7 items-center justify-center rounded-full hover:bg-white/10"
            >
              <span aria-hidden className={`block rounded-full transition-colors ${option.dot} ${size === option.value ? "bg-white" : "bg-white/40"}`} />
            </button>
          ))}
        </div>
      </div>
    </main>
  );
}
