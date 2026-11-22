import {
  useCallback,
  useEffect,
  useRef,
  useState,
  type CSSProperties,
  type PointerEvent as ReactPointerEvent,
} from "react";
import { useTranslations } from "use-intl";
import { Icon } from "../components/recorder/ui";
import {
  cancelAreaSelect,
  completeAreaSelect,
  onAreaSelectPurpose,
  reportWebviewError,
  type AreaSelectPurpose,
} from "../lib/bridge";
import {
  RESIZE_HANDLES,
  clampPoint,
  handleCursor,
  isUsableSelection,
  moveRect,
  rectFromPoints,
  resizeRect,
  roundRect,
  type Point,
  type Rect,
  type ResizeHandle,
} from "../lib/areaSelectGeometry";

/**
 * The overlay is shared by recording and screenshots, so every string it shows
 * has to come from the mode rather than being hardcoded. It used to say
 * "record" in both, which told a user taking a screenshot that they were about
 * to start a recording.
 */
// Words are `desktop.areaSelect.<purpose>.*`.
const PURPOSES: Record<AreaSelectPurpose, AreaSelectPurpose> = {
  video: "video",
  screenshot: "screenshot",
};

type Gesture =
  | { kind: "draw"; anchor: Point }
  | { kind: "move"; start: Rect; origin: Point }
  | { kind: "resize"; handle: ResizeHandle; start: Rect; origin: Point };

const HANDLE_PX = 12;

function reportAreaSelectFailure(action: string, error: unknown) {
  const detail = error instanceof Error ? error.message : String(error);
  const stack = error instanceof Error ? error.stack : null;
  const message = `area select ${action} failed: ${detail}`;
  console.warn(message, error);
  reportWebviewError(message, stack, "area-select");
}

function handleStyle(handle: ResizeHandle): CSSProperties {
  const half = HANDLE_PX / 2;
  const style: CSSProperties = { width: HANDLE_PX, height: HANDLE_PX, cursor: handleCursor(handle) };
  if (handle.includes("n")) style.top = -half;
  if (handle.includes("s")) style.bottom = -half;
  if (handle.includes("w")) style.left = -half;
  if (handle.includes("e")) style.right = -half;
  if (handle === "n" || handle === "s") {
    style.left = "50%";
    style.marginLeft = -half;
  }
  if (handle === "e" || handle === "w") {
    style.top = "50%";
    style.marginTop = -half;
  }
  return style;
}

/** Full-display transparent overlay for the Custom size source. The first
 *  drag draws the box; it then stays up with corner and edge grips so the
 *  user can resize or move it, and only Enter / Confirm commits. The window
 *  covers the target display exactly, so pointer coordinates are
 *  display-local logical pixels — what the `area` capture source expects.
 *  Esc cancels. */
export default function AreaSelect() {
  const t = useTranslations("desktop.areaSelect");
  const [rect, setRect] = useState<Rect | null>(null);
  const [adjusting, setAdjusting] = useState(false);
  // Defaults to the recording wording: the overlay has always been shown for
  // recording, and a failed query should not relabel a recording as a capture.
  const [purpose, setPurpose] = useState<AreaSelectPurpose>("video");

  // This window is created at startup and then only shown and hidden, so the
  // component mounts once; Rust sends the mode on every show.
  useEffect(() => {
    let cancelled = false;
    let unlisten: (() => void) | undefined;
    onAreaSelectPurpose((next) => {
      if (!cancelled) setPurpose(next);
    })
      .then((fn) => {
        if (cancelled) fn();
        else unlisten = fn;
      })
      .catch((error) => reportAreaSelectFailure("purpose", error));
    return () => {
      cancelled = true;
      unlisten?.();
    };
  }, []);

  const copyPurpose = PURPOSES[purpose] ?? "video";
  const copy = {
    title: t(`${copyPurpose}.title`),
    hint: t(`${copyPurpose}.hint`),
    confirm: t(`${copyPurpose}.confirm`),
  };
  const gestureRef = useRef<Gesture | null>(null);
  const rectRef = useRef<Rect | null>(null);
  rectRef.current = rect;

  const bounds = () => ({ width: window.innerWidth, height: window.innerHeight });

  const reset = useCallback(() => {
    gestureRef.current = null;
    setRect(null);
    setAdjusting(false);
  }, []);

  const cancel = useCallback(() => {
    reset();
    cancelAreaSelect().catch((error) => reportAreaSelectFailure("cancel", error));
  }, [reset]);

  const confirm = useCallback(() => {
    const current = rectRef.current;
    if (!current || !isUsableSelection(current)) return;
    reset();
    completeAreaSelect(roundRect(current)).catch((error) =>
      reportAreaSelectFailure("complete", error),
    );
  }, [reset]);

  useEffect(() => {
    const onKeyDown = (event: KeyboardEvent) => {
      if (event.key === "Escape") {
        event.preventDefault();
        cancel();
      } else if (event.key === "Enter") {
        event.preventDefault();
        confirm();
      }
    };
    // Losing focus means the user clicked somewhere else (the picker
    // dialog, another app, another display) — cancel the whole session so
    // the overlay never sits stranded on a monitor dimming it, suppressing
    // the source highlight, and bleeding into re-captured thumbnails.
    const onBlur = () => cancel();
    window.addEventListener("keydown", onKeyDown);
    window.addEventListener("blur", onBlur);
    return () => {
      window.removeEventListener("keydown", onKeyDown);
      window.removeEventListener("blur", onBlur);
    };
  }, [cancel, confirm]);

  const pointOf = (event: ReactPointerEvent<HTMLElement>): Point =>
    clampPoint({ x: event.clientX, y: event.clientY }, bounds());

  const beginGesture = (event: ReactPointerEvent<HTMLElement>, gesture: Gesture) => {
    event.stopPropagation();
    event.currentTarget.setPointerCapture(event.pointerId);
    gestureRef.current = gesture;
  };

  const onBackdropPointerDown = (event: ReactPointerEvent<HTMLDivElement>) => {
    if (event.button !== 0) return;
    const point = pointOf(event);
    beginGesture(event, { kind: "draw", anchor: point });
    setAdjusting(false);
    setRect({ ...point, width: 0, height: 0 });
  };

  const onBoxPointerDown = (event: ReactPointerEvent<HTMLDivElement>) => {
    const current = rectRef.current;
    if (event.button !== 0 || !current) return;
    beginGesture(event, { kind: "move", start: current, origin: pointOf(event) });
  };

  const onHandlePointerDown = (handle: ResizeHandle) => (event: ReactPointerEvent<HTMLDivElement>) => {
    const current = rectRef.current;
    if (event.button !== 0 || !current) return;
    beginGesture(event, { kind: "resize", handle, start: current, origin: pointOf(event) });
  };

  const onPointerMove = (event: ReactPointerEvent<HTMLElement>) => {
    const gesture = gestureRef.current;
    if (!gesture) return;
    const point = pointOf(event);
    if (gesture.kind === "draw") {
      setRect(rectFromPoints(gesture.anchor, point));
      return;
    }
    const delta = { x: point.x - gesture.origin.x, y: point.y - gesture.origin.y };
    setRect(
      gesture.kind === "move"
        ? moveRect(gesture.start, delta, bounds())
        : resizeRect(gesture.start, gesture.handle, delta, bounds()),
    );
  };

  const onPointerUp = () => {
    const gesture = gestureRef.current;
    if (!gesture) return;
    gestureRef.current = null;
    const current = rectRef.current;
    if (gesture.kind === "draw" && (!current || !isUsableSelection(current))) {
      // Stray click — keep the overlay up so the user can draw again.
      setRect(null);
      return;
    }
    setAdjusting(true);
  };

  return (
    <div
      className="fixed inset-0 cursor-crosshair select-none overflow-hidden"
      onPointerDown={onBackdropPointerDown}
      onPointerMove={onPointerMove}
      onPointerUp={onPointerUp}
      onPointerCancel={onPointerUp}
    >
      {/* Uniform dim before a drag starts; once a box exists the dim comes
          from the selection's giant box-shadow so the chosen area stays
          bright. */}
      {!rect && <div className="absolute inset-0 bg-black/35" aria-hidden />}

      {/* The "Custom size recording" card sits in the middle of the
          dimmed display until the first drag. Pointer-down is swallowed so
          a click on the card cannot start a box under it. */}
      {!rect && (
        <div
          role="dialog"
          aria-label={copy.title}
          className="absolute left-1/2 top-1/2 z-10 w-[320px] -translate-x-1/2 -translate-y-1/2 cursor-default rounded-2xl bg-surface p-4 text-left text-ink-text shadow-modal"
          onPointerDown={(event) => event.stopPropagation()}
        >
          <div className="flex items-start justify-between gap-4">
            <div className="min-w-0">
              <p className="text-[16px] font-bold">{copy.title}</p>
              <p className="mt-0.5 text-[13px] text-ink-secondary">{t("drag")}</p>
            </div>
            <button
              type="button"
              onClick={cancel}
              className="shrink-0 text-[13px] font-medium text-[#2f5fd0] hover:underline"
            >
              {t("cancel")}
            </button>
          </div>
          <p className="mt-3 flex items-center gap-2 rounded-xl bg-[#e9effc] px-3 py-2 text-[12px] font-medium text-[#2f5fd0]">
            <Icon name="info" size={15} className="shrink-0" />
            {copy.hint}
          </p>
        </div>
      )}

      {adjusting && (
        <div className="pointer-events-none absolute left-1/2 top-10 z-10 -translate-x-1/2 rounded-full border border-white/15 bg-ink-950/90 px-4 py-2 text-xs font-medium text-stone-100 shadow-lg">
          Drag the corners to resize, drag inside to move — Enter to confirm, Esc to cancel
        </div>
      )}

      {rect && (
        <div
          data-testid="area-select-box"
          className={`absolute border-[1.5px] border-flash-500 ${adjusting ? "cursor-move" : "pointer-events-none"}`}
          style={{
            left: rect.x,
            top: rect.y,
            width: rect.width,
            height: rect.height,
            boxShadow: "0 0 0 100000px rgba(0, 0, 0, 0.35)",
          }}
          onPointerDown={adjusting ? onBoxPointerDown : undefined}
        >
          {/* Move and resize events are captured on the grip or box that
              started them and bubble up to the backdrop's move/up handlers,
              so the children only decide WHICH gesture begins. */}
          {adjusting &&
            RESIZE_HANDLES.map((handle) => (
              <div
                key={handle}
                role="presentation"
                data-handle={handle}
                className="absolute rounded-full border-[1.5px] border-flash-500 bg-white shadow"
                style={handleStyle(handle)}
                onPointerDown={onHandlePointerDown(handle)}
              />
            ))}

          <span className="pointer-events-none absolute -bottom-7 right-0 rounded bg-ink-950/90 px-2 py-0.5 text-[11px] tabular-nums text-stone-100">
            {Math.round(rect.width)}×{Math.round(rect.height)}
          </span>

          {adjusting && (
            <div
              className="absolute left-1/2 top-full mt-3 flex -translate-x-1/2 cursor-default items-center gap-2"
              onPointerDown={(event) => event.stopPropagation()}
            >
              <button
                type="button"
                className="rounded-full bg-flash-500 px-4 py-1.5 text-xs font-semibold text-white shadow-lg hover:bg-flash-600"
                onClick={confirm}
              >
                {copy.confirm}
              </button>
              <button
                type="button"
                className="rounded-full border border-white/20 bg-ink-950/90 px-3 py-1.5 text-xs font-medium text-stone-100 shadow-lg hover:bg-ink-900"
                onClick={cancel}
              >
                {t("cancel")}
              </button>
            </div>
          )}
        </div>
      )}
    </div>
  );
}
