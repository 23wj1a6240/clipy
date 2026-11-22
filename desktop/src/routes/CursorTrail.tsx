import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import { useEffect, useRef } from "react";
import { ANNOTATION_WIDTH_MAX, ANNOTATION_WIDTH_MIN, getAnnotationColor, getAnnotationWidth, isAnnotationColor, type AnnotationColor } from "../lib/annotation";
import { getCursorTrailEnabled } from "../lib/bridge";
import { flareOpacity, splitTrail, strokeFlarePath, trailSplitDistance } from "../lib/flareTrail";

type CursorTrailPoint = {
  readonly x: number;
  readonly y: number;
  readonly visible: boolean;
  readonly color?: AnnotationColor;
};

type TrailPoint = CursorTrailPoint & {
  readonly t: number;
  /** The flare was off between the previous point and this one. */
  readonly breakBefore: boolean;
  /** Pen width in CSS px at the time the point was laid down. */
  readonly width?: number;
};

// Ignore sub-pixel jitter so a resting cursor does not reset the idle timer.
const MIN_STEP_PX = 1.5;
const MAX_POINTS = 240;

// Retina is plenty at 2x; a 3x canvas over a full display triples the
// per-frame stroke fill cost during an active recording.
function canvasDpr(): number {
  return Math.min(window.devicePixelRatio || 1, 2);
}

function backingSize(): { width: number; height: number } {
  const dpr = canvasDpr();
  return {
    width: Math.max(1, Math.round(window.innerWidth * dpr)),
    height: Math.max(1, Math.round(window.innerHeight * dpr)),
  };
}

export default function CursorTrail() {
  const canvasRef = useRef<HTMLCanvasElement | null>(null);
  const pointsRef = useRef<TrailPoint[]>([]);
  const lastDrawnAtRef = useRef(0);
  const hadContentRef = useRef(false);
  // Set by an invisible sample; the next visible point starts a new stroke.
  const breakPendingRef = useRef(false);

  useEffect(() => {
    let disposed = false;
    let animation = 0;
    const unlisteners: UnlistenFn[] = [];
    let color: AnnotationColor = "#ff4d29";
    let width = 6;
    let enabled = false;
    let drawing = false;
    let receivedMode = false;
    let receivedColor = false;
    const settled = document.createElement("canvas");
    let hasSettled = false;
    const previousHtmlBackground = document.documentElement.style.background;
    const previousBodyBackground = document.body.style.background;
    const previousBodyMargin = document.body.style.margin;
    document.documentElement.style.background = "transparent";
    document.body.style.background = "transparent";
    document.body.style.margin = "0";

    const resize = () => {
      const canvas = canvasRef.current;
      if (!canvas) return;
      const backing = backingSize();
      canvas.width = backing.width;
      canvas.height = backing.height;
      canvas.style.width = `${window.innerWidth}px`;
      canvas.style.height = `${window.innerHeight}px`;
      pointsRef.current = [];
      settled.width = canvas.width;
      settled.height = canvas.height;
      hasSettled = false;
    };

    // The overlay window is reused across recordings and moved to whichever
    // display is captured. Moving from a Retina display to a 1x one (or back)
    // changes devicePixelRatio without a resize event, and a backing store
    // sized for the old density draws every stroke at half or double its
    // coordinates, away from the cursor.
    const syncBacking = (canvas: HTMLCanvasElement) => {
      const backing = backingSize();
      if (canvas.width !== backing.width || canvas.height !== backing.height) resize();
    };

    const draw = () => {
      const canvas = canvasRef.current;
      const ctx = canvas?.getContext("2d");
      if (!canvas || !ctx) return;
      syncBacking(canvas);
      const now = performance.now();
      const points = pointsRef.current;
      const opacity = flareOpacity(now, lastDrawnAtRef.current);
      if (opacity === 0) {
        points.length = 0;
        if (hasSettled) settled.getContext("2d")?.clearRect(0, 0, settled.width, settled.height);
        hasSettled = false;
      }

      // Flare idle (the common case while recording): the canvas is
      // already clear — do zero paint work until the next point arrives.
      const hasContent = hasSettled || points.length > 0;
      if (!hasContent && !hadContentRef.current) {
        if (!disposed) animation = window.requestAnimationFrame(draw);
        return;
      }
      hadContentRef.current = hasContent;

      const dpr = canvasDpr();
      ctx.clearRect(0, 0, canvas.width, canvas.height);
      ctx.save();
      ctx.globalAlpha = opacity;
      if (hasSettled) ctx.drawImage(settled, 0, 0);
      ctx.scale(dpr, dpr);
      ctx.lineCap = "round";
      ctx.lineJoin = "round";

      const splitDistance = trailSplitDistance(window.innerWidth, window.innerHeight);
      for (const sub of splitTrail(points, splitDistance)) {
        strokeFlarePath(ctx, sub);
      }
      ctx.globalAlpha = 1;

      ctx.restore();
      if (!disposed) {
        animation = window.requestAnimationFrame(draw);
      }
    };

    resize();
    window.addEventListener("resize", resize);
    const addPoint = (payload: CursorTrailPoint) => {
      if (canvasRef.current) syncBacking(canvasRef.current);
      if (!payload.visible) {
        breakPendingRef.current = true;
        return;
      }
      const t = performance.now();
      if (flareOpacity(t, lastDrawnAtRef.current) === 0) {
        pointsRef.current = [];
        settled.getContext("2d")?.clearRect(0, 0, settled.width, settled.height);
        hasSettled = false;
        breakPendingRef.current = true;
      }
      const points = pointsRef.current;
      const last = points.length > 0 ? points[points.length - 1] : undefined;
      const dist = last
        ? Math.hypot(payload.x - last.x, payload.y - last.y)
        : 0;
      // Ignore stationary input without extending the drawing deadline.
      if (last && dist < MIN_STEP_PX && !breakPendingRef.current) {
        return;
      }
      lastDrawnAtRef.current = t;
      const breakBefore = breakPendingRef.current || (last !== undefined && last.color !== (payload.color ?? color));
      breakPendingRef.current = false;
      const point: TrailPoint = { ...payload, color: payload.color ?? color, width, t, breakBefore };
      points.push(point);
      if (points.length > MAX_POINTS) {
        // Rasterize old geometry instead of dropping visible strokes or
        // growing an unbounded point list during continuous annotation.
        const archived = settled.getContext("2d");
        if (archived) {
          archived.save();
          archived.scale(canvasDpr(), canvasDpr());
          archived.lineCap = "round";
          archived.lineJoin = "round";
          for (const sub of splitTrail(points, trailSplitDistance(window.innerWidth, window.innerHeight))) {
            strokeFlarePath(archived, sub);
          }
          archived.restore();
          hasSettled = true;
          points.splice(0, points.length - 1);
        }
      }
    };
    const register = (promise: Promise<UnlistenFn>) => {
      void promise.then((off) => { if (disposed) off(); else unlisteners.push(off); }).catch((error) => console.warn("Annotation events unavailable", error));
    };
    register(listen<CursorTrailPoint>("cursor-trail-point", ({ payload }) => addPoint(payload)));
    register(listen<number>("annotation-width", ({ payload }) => {
      if (Number.isFinite(payload) && payload >= ANNOTATION_WIDTH_MIN && payload <= ANNOTATION_WIDTH_MAX) {
        width = payload;
        breakPendingRef.current = true;
      }
    }));
    void getAnnotationWidth().then((value) => { if (!disposed && Number.isFinite(value)) width = value; }).catch(() => {});
    register(listen<string>("annotation-color", ({ payload }) => {
      receivedColor = true;
      if (isAnnotationColor(payload)) { color = payload; breakPendingRef.current = true; }
    }));
    register(listen<boolean>("annotation-mode", ({ payload }) => {
      receivedMode = true;
      enabled = payload;
      drawing = false;
      breakPendingRef.current = true;
    }));
    const clear = () => {
      pointsRef.current = [];
      settled.getContext("2d")?.clearRect(0, 0, settled.width, settled.height);
      hasSettled = false;
      breakPendingRef.current = true;
    };
    register(listen("annotation-clear", clear));
    void getAnnotationColor().then((value) => { if (!disposed && !receivedColor && isAnnotationColor(value)) color = value; }).catch(() => {});
    void getCursorTrailEnabled().then((value) => { if (!disposed && !receivedMode) enabled = value; }).catch(() => {});
    const canvas = canvasRef.current;
    const point = (event: PointerEvent) => ({ x: event.offsetX, y: event.offsetY, visible: true, color });
    const down = (event: PointerEvent) => {
      if (!enabled || event.button !== 0) return;
      event.preventDefault();
      drawing = true;
      breakPendingRef.current = true;
      canvas?.setPointerCapture(event.pointerId);
      addPoint(point(event));
    };
    const move = (event: PointerEvent) => {
      if (enabled && drawing) addPoint(point(event));
    };
    const up = () => { drawing = false; breakPendingRef.current = true; };
    canvas?.addEventListener("pointerdown", down);
    canvas?.addEventListener("pointermove", move);
    canvas?.addEventListener("pointerup", up);
    canvas?.addEventListener("pointercancel", up);
    canvas?.addEventListener("lostpointercapture", up);
    animation = window.requestAnimationFrame(draw);

    return () => {
      disposed = true;
      window.cancelAnimationFrame(animation);
      window.removeEventListener("resize", resize);
      document.documentElement.style.background = previousHtmlBackground;
      document.body.style.background = previousBodyBackground;
      document.body.style.margin = previousBodyMargin;
      unlisteners.forEach((off) => off());
      canvas?.removeEventListener("pointerdown", down);
      canvas?.removeEventListener("pointermove", move);
      canvas?.removeEventListener("pointerup", up);
      canvas?.removeEventListener("pointercancel", up);
      canvas?.removeEventListener("lostpointercapture", up);
    };
  }, []);

  return (
    <main className="h-full w-full overflow-hidden bg-transparent">
      <canvas ref={canvasRef} className="h-full w-full cursor-crosshair touch-none" aria-label="Annotation canvas" />
    </main>
  );
}
