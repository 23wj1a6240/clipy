// SPDX-License-Identifier: Apache-2.0
// Copied from the Clipy monorepo (components/screenshot-editor/useAnnotationEditor.ts) by the export; the desktop app shares it.
"use client";

// Relative imports on purpose: the desktop app bundles this hook from outside
// the Next project, where the `@/` alias does not exist.
import { useCallback, useEffect, useLayoutEffect, useMemo, useRef, useState } from "react";
import type {
  ChangeEvent,
  CSSProperties,
  KeyboardEvent as ReactKeyboardEvent,
  MouseEvent as ReactMouseEvent,
  PointerEvent as ReactPointerEvent,
  RefObject,
} from "react";
import { layoutBackdrop, type AutoColors, type BackdropLayout } from "../../lib/screenshots/backdrop";
import {
  clampRect,
  constrainPoint,
  distance,
  inflate,
  pickSegmentHandle,
  pickShape,
  labelPadding,
  rectFromPoints,
  setTextMeasurer,
  textBlock,
  textInside,
  shapeBounds,
  shapeHandles,
  translateShape,
  type SegmentHandle,
} from "../../lib/screenshots/geometry";
import {
  canvasToPngBlob,
  createCanvasTextMeasurer,
  drawCropOverlay,
  drawCutOverlay,
  drawSelection,
  flattenScene,
  readableTextOn,
  renderScene,
  sampleAutoColors,
} from "../../lib/screenshots/render";
import { DEFAULT_COLOR, sizeScale } from "../../lib/screenshots/scene";
import {
  isSegmentShape,
  type AnnotationScene,
  type Backdrop,
  type CalloutShape,
  type Cut,
  type Point,
  type Rect,
  type Shape,
  type TextShape,
} from "../../lib/screenshots/types";
import {
  DEFAULT_TOOL_OPTIONS,
  STROKE_STEPS,
  TOOL_SHORTCUTS,
  applyOption,
  isSliderKey,
  isTextEntry,
  applyWeight,
  createShape,
  fontSizeFor,
  isMeaningfulShape,
  newShapeId,
  optionsFromShape,
  type EditorTool,
  type ToolOptions,
} from "./tools";
import { useSceneHistory, type SceneHistory } from "./useSceneHistory";

type Gesture =
  | { kind: "draw"; start: Point }
  | { kind: "path" }
  | { kind: "callout"; tip: Point }
  | { kind: "move"; id: string; last: Point; marked: boolean }
  | { kind: "handle"; id: string; handle: SegmentHandle; marked: boolean }
  | { kind: "crop"; start: Point }
  | { kind: "cut"; start: Point };

export interface TextDraft {
  id: string;
  /** `callout` drafts keep their tail; `text` drafts may have a label. */
  kind: "text" | "callout";
  at: Point;
  value: string;
  color: string;
  fontSize: number;
  isNew: boolean;
  tail?: Point;
  background?: boolean;
}

export interface ImageSize {
  width: number;
  height: number;
}

/**
 * Layout effect in the browser, so the canvas is sized and painted before the
 * first frame and before any pointer event can read its size; plain effect on
 * the server, where layout effects only warn.
 */
const useIsomorphicLayoutEffect = typeof window === "undefined" ? useEffect : useLayoutEffect;

/** Pointer slack, in CSS pixels, for picking and grabbing handles. */
const PICK_SLACK_PX = 10;

export interface AnnotationEditorOptions {
  /** Anything an <img> can load: a same-origin URL, a blob: URL. */
  sourceUrl: string;
  initialScene: AnnotationScene;
  loadErrorMessage?: string;
}

export interface BackdropPreview {
  /** Layout of the full (uncropped) image on its background, in image pixels. */
  layout: BackdropLayout;
  backdrop: Backdrop;
  autoColors: AutoColors | null;
  /** Image pixels per CSS pixel; divide layout values by it for CSS. */
  displayScale: number;
  /** The screenshot's URL, for the blurred-screenshot background. */
  imageUrl: string;
}

export interface AnnotationEditor {
  history: SceneHistory;
  scene: AnnotationScene;
  tool: EditorTool;
  setTool: (tool: EditorTool) => void;
  color: string;
  /** Sets the color for new shapes and recolors the selected one. */
  changeColor: (color: string) => void;
  weight: number;
  /** Sets the weight for new shapes and resizes the selected one. */
  setWeight: (weight: number) => void;
  options: ToolOptions;
  /** Sets a tool option for new shapes and restyles the selected one. */
  setOption: <K extends keyof ToolOptions>(key: K, value: ToolOptions[K]) => void;
  /** The shape the Select tool has picked, if any. */
  selected: Shape | null;
  deleteSelected: () => void;
  duplicateSelected: () => void;
  bringToFront: () => void;
  sendToBack: () => void;
  /** The last color sampled with the color picker, for the hint line. */
  pickedColor: string | null;
  /** A path or text is being drawn and is not in the scene yet. */
  drafting: boolean;
  setBackdrop: (backdrop: Backdrop | null) => void;
  /** Present while the scene has a backdrop; render it around the canvas. */
  backdropPreview: BackdropPreview | null;
  autoColors: AutoColors | null;
  imageSize: ImageSize | null;
  loadError: string | null;
  cropped: boolean;
  resetCrop: () => void;
  /** How many strips will be cut out on export. */
  cutCount: number;
  resetCuts: () => void;
  clear: () => void;
  canvasRef: RefObject<HTMLCanvasElement | null>;
  canvasHandlers: {
    onPointerDown: (event: ReactPointerEvent<HTMLCanvasElement>) => void;
    onPointerMove: (event: ReactPointerEvent<HTMLCanvasElement>) => void;
    onPointerUp: () => void;
    onPointerCancel: () => void;
    onDoubleClick: (event: ReactMouseEvent<HTMLCanvasElement>) => void;
  };
  /** Present while a text annotation is being typed; render a textarea for it. */
  textInput: {
    value: string;
    style: CSSProperties;
    onChange: (event: ChangeEvent<HTMLTextAreaElement>) => void;
    onBlur: () => void;
    onKeyDown: (event: ReactKeyboardEvent<HTMLTextAreaElement>) => void;
  } | null;
  /**
   * Places a text annotation that is still being typed (and a path still being
   * drawn) and returns the scene including it. Call before exporting from
   * anything that is not a click on the page (a window close, a keyboard
   * shortcut): those never blur the text box, so without this the typed text
   * would be left out.
   */
  settleScene: () => AnnotationScene;
  /** Bakes image + annotations + crop + backdrop into a PNG; `scene` defaults to the current one. */
  exportPng: (scene?: AnnotationScene) => Promise<Blob>;
}

/** What text has to stay inside: the crop when there is one, else the image. */
function textBounds(scene: AnnotationScene, image: ImageSize | null): Rect | null {
  return scene.crop ?? (image ? { x: 0, y: 0, width: image.width, height: image.height } : null);
}

/** Where a draft's text lands: at the draft's point, moved back on if it would run off the edge. */
function draftPosition(draft: TextDraft, text: string, bounds: Rect | null): Point {
  if (!bounds) return draft.at;
  return textInside(draft.at, text, draft.fontSize, draft.kind === "callout" || !!draft.background, bounds);
}

/** The scene with a text draft applied: added, replaced, or (when emptied) removed. */
export function applyTextDraft(
  current: AnnotationScene,
  draft: TextDraft,
  image: ImageSize | null
): AnnotationScene {
  const value = draft.value.trim();
  if (!value) {
    return draft.isNew
      ? current
      : { ...current, shapes: current.shapes.filter((s) => s.id !== draft.id) };
  }
  const base = { id: draft.id, color: draft.color, fontSize: draft.fontSize, strokeWidth: 1 };
  const at = draftPosition(draft, value, textBounds(current, image));
  let shape: TextShape | CalloutShape;
  if (draft.kind === "callout") {
    shape = { ...base, kind: "callout", at, text: value, tail: draft.tail ?? draft.at };
  } else {
    shape = draft.background
      ? { ...base, kind: "text", at, text: value, background: true }
      : { ...base, kind: "text", at, text: value };
  }
  return draft.isNew
    ? { ...current, shapes: [...current.shapes, shape] }
    : { ...current, shapes: current.shapes.map((s) => (s.id === draft.id ? shape : s)) };
}

/**
 * Where the text box goes while a text annotation is typed, in CSS pixels
 * (`k` is CSS pixels per image pixel). Plain text gets a box kept on the
 * canvas. A callout or a labelled text takes the shape it will have: the box
 * covers the bubble or label exactly, in its color, sized to the text, so
 * what you type sits where it will be drawn. Given `bounds`, it moves back
 * inside them as the text grows, the same way the placed shape will.
 */
export function textInputStyle(draft: TextDraft, k: number, bounds: Rect | null = null): CSSProperties {
  const fontSize = `${Math.max(11, draft.fontSize * k)}px`;
  if (draft.kind !== "callout" && !draft.background) {
    return {
      // Kept on the canvas: near the right or bottom edge the box would
      // otherwise run off it, and a background's card clips overflow.
      left: `max(0px, min(${draft.at.x * k}px, calc(100% - min(14rem, 70%))))`,
      top: `max(0px, min(${draft.at.y * k}px, calc(100% - 2.5em)))`,
      fontSize,
      color: draft.color,
    };
  }
  // The font never drops below 11px on screen, so the box is sized at the
  // scale the text is actually shown at, or a zoomed-out view clips it.
  const shown = Math.max(k, 11 / Math.max(draft.fontSize, 1));
  const pad = labelPadding(draft.fontSize);
  const block = textBlock(draft.value || "Type here", draft.fontSize);
  // A little slack so the next character never wraps before the box grows.
  const slack = draft.fontSize * 0.8;
  const at = draftPosition(draft, draft.value, bounds);
  return {
    left: `${(at.x - pad) * k}px`,
    top: `${(at.y - pad) * k}px`,
    width: `${(block.width + slack + pad * 2) * shown}px`,
    height: `${(block.height + pad * 2) * shown}px`,
    maxWidth: "none",
    minHeight: 0,
    padding: `${pad * shown}px`,
    lineHeight: 1.25,
    fontSize: `${draft.fontSize * shown}px`,
    whiteSpace: "pre",
    overflow: "hidden",
    color: readableTextOn(draft.color),
    background: draft.color,
    borderRadius: `${Math.min(pad * 0.9, draft.fontSize * 0.7) * shown}px`,
  };
}

/**
 * Where a callout placed with a click (no drag) puts its bubble: up and to the
 * right of the tip, flipped below or to the left when that would leave the
 * image, so the bubble and its text box stay on the screenshot.
 */
export function clickedCalloutPosition(
  tip: Point,
  fontSize: number,
  bounds: ImageSize | null
): Point {
  const pad = fontSize * 0.5;
  const width = bounds?.width ?? Infinity;
  const height = bounds?.height ?? Infinity;
  const bubbleWidth = fontSize * 8;
  let x = tip.x + fontSize * 1.5;
  let y = tip.y - fontSize * 3;
  if (x + bubbleWidth > width) x = tip.x - fontSize * 1.5 - bubbleWidth;
  if (y - pad < 0) y = tip.y + fontSize * 1.8;
  return {
    x: Math.max(pad, Math.min(x, width - bubbleWidth)),
    y: Math.max(pad, Math.min(y, height - fontSize * 1.25 - pad)),
  };
}

/**
 * The undo-coalescing key for a background change: which fields changed.
 * Turning the background on or off gets no key, so it is always its own step.
 */
function backdropChangeKey(previous: Backdrop | null, next: Backdrop | null): string | undefined {
  if (!previous || !next) return undefined;
  const keys = new Set([...Object.keys(previous), ...Object.keys(next)]) as Set<keyof Backdrop>;
  const changed = [...keys].filter((key) => previous[key] !== next[key]).sort();
  return changed.length > 0 ? `backdrop:${changed.join(",")}` : undefined;
}

/** Drops the floating point of a path still being drawn, and double-click doubles. */
function finishedPathPoints(points: Point[], slack: number): Point[] {
  const fixed = points.slice(0, -1);
  const result: Point[] = [];
  for (const p of fixed) {
    const last = result[result.length - 1];
    if (!last || distance(last, p) > slack) result.push(p);
  }
  return result;
}

function toHex(r: number, g: number, b: number): string {
  return `#${[r, g, b].map((c) => c.toString(16).padStart(2, "0")).join("")}`;
}

/**
 * Everything the screenshot editor does except how it looks: image loading,
 * pointer gestures, the text overlay, keyboard shortcuts and export. The web
 * editor (`ScreenshotEditor`) and the desktop screenshot window both render
 * on top of it, so a fix to a gesture lands in both at once.
 */
export function useAnnotationEditor({
  sourceUrl,
  initialScene,
  loadErrorMessage = "Couldn't load this screenshot. Reload the page and try again.",
}: AnnotationEditorOptions): AnnotationEditor {
  const canvasRef = useRef<HTMLCanvasElement | null>(null);
  const imageRef = useRef<HTMLImageElement | null>(null);
  const gestureRef = useRef<Gesture | null>(null);
  const textDraftRef = useRef<TextDraft | null>(null);
  /** The drawing tool the color picker returns to. */
  const lastDrawToolRef = useRef<EditorTool>("arrow");

  const history = useSceneHistory(initialScene);
  const { scene } = history;

  const [imageSize, setImageSize] = useState<ImageSize | null>(null);
  const [loadError, setLoadError] = useState<string | null>(null);
  const [tool, setToolState] = useState<EditorTool>("arrow");
  const [color, setColor] = useState<string>(DEFAULT_COLOR);
  const [weight, setWeightState] = useState<number>(STROKE_STEPS[1].value);
  const [options, setOptions] = useState<ToolOptions>(DEFAULT_TOOL_OPTIONS);
  const [selectedId, setSelectedId] = useState<string | null>(null);
  const [draft, setDraft] = useState<Shape | null>(null);
  const [cropDraft, setCropDraft] = useState<Rect | null>(null);
  const [cutDraft, setCutDraft] = useState<Cut | null>(null);
  const [textDraft, setTextDraft] = useState<TextDraft | null>(null);
  const [pickedColor, setPickedColor] = useState<string | null>(null);
  const [autoColors, setAutoColors] = useState<AutoColors | null>(null);
  /** Image pixels per CSS pixel — the canvas is laid out smaller than it is. */
  const [displayScale, setDisplayScale] = useState(1);

  const draftRef = useRef<Shape | null>(null);
  draftRef.current = draft;
  const hasBackdrop = Boolean(scene.backdrop);

  const scale = imageSize ? sizeScale(imageSize.width, imageSize.height) : 1;
  const style = useMemo(
    () => ({ color, weight, scale, options }),
    [color, options, scale, weight]
  );

  // Selection outlines and hit areas for text use the canvas's real widths.
  useEffect(() => {
    setTextMeasurer(createCanvasTextMeasurer());
  }, []);

  useEffect(() => {
    let cancelled = false;
    const image = new Image();
    image.decoding = "async";
    image.onload = () => {
      if (cancelled) return;
      imageRef.current = image;
      setImageSize({ width: image.naturalWidth, height: image.naturalHeight });
      setAutoColors(sampleAutoColors(image, image.naturalWidth, image.naturalHeight));
    };
    image.onerror = () => {
      if (cancelled) return;
      setLoadError(loadErrorMessage);
    };
    image.src = sourceUrl;
    return () => {
      cancelled = true;
    };
  }, [loadErrorMessage, sourceUrl]);

  // The canvas is a replaced element sized by CSS, so every pointer coordinate
  // and every overlay line width has to be converted through this ratio.
  useEffect(() => {
    const canvas = canvasRef.current;
    if (!canvas || !imageSize) return;
    const measure = () => {
      const rect = canvas.getBoundingClientRect();
      if (rect.width > 0) setDisplayScale(canvas.width / rect.width);
    };
    measure();
    const observer = new ResizeObserver(measure);
    observer.observe(canvas);
    return () => observer.disconnect();
    // Re-attached when the background toggles: the preview wraps the canvas
    // and changes its size, and this guarantees the observer is on the live
    // node even if a host ever remounts it.
  }, [imageSize, hasBackdrop]);

  const hiddenTextId = textDraft && !textDraft.isNew ? textDraft.id : null;
  const selected = useMemo(
    () => scene.shapes.find((shape) => shape.id === selectedId) ?? null,
    [scene.shapes, selectedId]
  );

  useIsomorphicLayoutEffect(() => {
    const canvas = canvasRef.current;
    const image = imageRef.current;
    if (!canvas || !image || !imageSize) return;
    const ctx = canvas.getContext("2d");
    if (!ctx) return;

    if (canvas.width !== imageSize.width) canvas.width = imageSize.width;
    if (canvas.height !== imageSize.height) canvas.height = imageSize.height;

    const shapes = scene.shapes.filter((shape) => shape.id !== hiddenTextId);
    if (draft) shapes.push(draft);
    // A callout being typed shows its bubble and tail behind the text box.
    if (textDraft?.kind === "callout") {
      shapes.push({
        id: `${textDraft.id}-preview`,
        kind: "callout",
        color: textDraft.color,
        strokeWidth: 1,
        at: textDraft.at,
        tail: textDraft.tail ?? textDraft.at,
        text: textDraft.value || " ",
        fontSize: textDraft.fontSize,
      });
    }
    renderScene(ctx, image, imageSize, { ...scene, shapes });

    const overlayWidth = Math.max(1, 1.5 * displayScale);
    const cuts = cutDraft ? [...(scene.cuts ?? []), cutDraft] : scene.cuts ?? [];
    if (cuts.length > 0) drawCutOverlay(ctx, cuts, overlayWidth);
    const activeCrop = cropDraft ?? scene.crop;
    if (activeCrop) drawCropOverlay(ctx, activeCrop, overlayWidth);

    if (selected && tool === "select") {
      drawSelection(
        ctx,
        inflate(shapeBounds(selected), selected.strokeWidth / 2),
        shapeHandles(selected),
        overlayWidth
      );
    }
  }, [cropDraft, cutDraft, displayScale, draft, hiddenTextId, imageSize, scene, selected, textDraft, tool]);

  const toImagePoint = useCallback((event: ReactMouseEvent<HTMLCanvasElement>): Point => {
    const canvas = event.currentTarget;
    const rect = canvas.getBoundingClientRect();
    return {
      x: ((event.clientX - rect.left) * canvas.width) / Math.max(1, rect.width),
      y: ((event.clientY - rect.top) * canvas.height) / Math.max(1, rect.height),
    };
  }, []);

  // A click outside the textarea both fires its onBlur and runs the canvas
  // pointerdown handler, so the commit path has to be idempotent — otherwise a
  // new text annotation gets added twice. The ref is the claim token.
  useEffect(() => {
    textDraftRef.current = textDraft;
  }, [textDraft]);

  const closeTextDraft = useCallback(() => {
    textDraftRef.current = null;
    setTextDraft(null);
  }, []);

  const sceneRef = useRef(scene);
  sceneRef.current = scene;

  const commitTextDraft = useCallback(() => {
    const active = textDraftRef.current;
    if (!active) return;
    closeTextDraft();
    history.commit((current) => applyTextDraft(current, active, imageSize));
  }, [closeTextDraft, history, imageSize]);

  /** The path being drawn, finished: committed when it has two real points. */
  const pathResult = useCallback(
    (closed: boolean): Shape | null => {
      const current = draftRef.current;
      if (!current || current.kind !== "path") return null;
      const points = finishedPathPoints(current.points, PICK_SLACK_PX * displayScale * 0.5);
      const shape: Shape =
        closed && points.length >= 3 ? { ...current, points, closed: true } : { ...current, points };
      return isMeaningfulShape(shape, 4 * displayScale) ? shape : null;
    },
    [displayScale]
  );

  const finishPath = useCallback(
    (closed = false) => {
      if (gestureRef.current?.kind !== "path") return;
      const shape = pathResult(closed);
      gestureRef.current = null;
      setDraft(null);
      if (shape) history.commit((current) => ({ ...current, shapes: [...current.shapes, shape] }));
    },
    [history, pathResult]
  );

  const cancelPath = useCallback(() => {
    if (gestureRef.current?.kind !== "path") return;
    gestureRef.current = null;
    setDraft(null);
  }, []);

  const settleScene = useCallback((): AnnotationScene => {
    let next = sceneRef.current;
    if (gestureRef.current?.kind === "path") {
      const shape = pathResult(false);
      finishPath(false);
      if (shape) next = { ...next, shapes: [...next.shapes, shape] };
    }
    const active = textDraftRef.current;
    if (!active) return next;
    commitTextDraft();
    return applyTextDraft(next, active, imageSize);
  }, [commitTextDraft, finishPath, imageSize, pathResult]);

  const setTool = useCallback(
    (next: EditorTool) => {
      finishPath(false);
      if (next !== "picker" && next !== "select" && next !== "crop" && next !== "cut") {
        lastDrawToolRef.current = next;
      }
      setToolState(next);
    },
    [finishPath]
  );

  const sampleColor = useCallback((point: Point): string | null => {
    const image = imageRef.current;
    if (!image) return null;
    try {
      const scratch = document.createElement("canvas");
      scratch.width = 1;
      scratch.height = 1;
      const ctx = scratch.getContext("2d");
      if (!ctx || typeof ctx.getImageData !== "function") return null;
      ctx.drawImage(image, Math.floor(point.x), Math.floor(point.y), 1, 1, 0, 0, 1, 1);
      const [r, g, b] = ctx.getImageData(0, 0, 1, 1).data;
      return toHex(r, g, b);
    } catch {
      return null;
    }
  }, []);

  const selectShape = useCallback((shape: Shape | null) => {
    setSelectedId(shape?.id ?? null);
    if (shape) setOptions((current) => optionsFromShape(shape, current));
  }, []);

  function handlePointerDown(event: ReactPointerEvent<HTMLCanvasElement>) {
    if (!imageSize) return;
    if (textDraft) commitTextDraft();
    const point = toImagePoint(event);
    const slack = PICK_SLACK_PX * displayScale;

    // Text is the one tool whose gesture is a click that opens an input, and
    // that makes the browser's own default a bug: pointerdown is followed by a
    // compatibility mousedown whose default action moves focus to the nearest
    // focusable ancestor. A canvas is not focusable, so focus lands on <body>,
    // the textarea we just mounted blurs, and onBlur commits an empty draft —
    // the tool looked completely dead. preventDefault suppresses the compat
    // mouse events, so the caret stays where it was put. No capture either:
    // there is no drag to track.
    if (tool === "text") {
      event.preventDefault();
      const shape = createShape("text", point, style);
      if (!shape || shape.kind !== "text") return;
      setTextDraft({
        id: shape.id,
        kind: "text",
        at: point,
        value: "",
        color: shape.color,
        fontSize: shape.fontSize,
        isNew: true,
        background: shape.background,
      });
      return;
    }

    if (tool === "picker") {
      const hex = sampleColor(point);
      if (hex) {
        setColor(hex);
        setPickedColor(hex);
        void navigator.clipboard?.writeText?.(hex).catch(() => {});
      }
      setToolState(lastDrawToolRef.current);
      return;
    }

    // A path is built one click at a time and survives pointerup.
    if (tool === "path") {
      const current = draftRef.current;
      if (gestureRef.current?.kind === "path" && current?.kind === "path") {
        const fixed = current.points.slice(0, -1);
        if (fixed.length >= 2 && distance(point, fixed[0]) <= slack * 1.4) {
          finishPath(true);
          return;
        }
        const previous = fixed[fixed.length - 1];
        const at = event.shiftKey && previous ? constrainPoint("path", previous, point) : point;
        setDraft({ ...current, points: [...fixed, at, at] });
        return;
      }
      const shape = createShape("path", point, style);
      if (!shape) return;
      gestureRef.current = { kind: "path" };
      setDraft(shape);
      return;
    }

    event.currentTarget.setPointerCapture(event.pointerId);

    if (tool === "select") {
      const handle = selected ? pickSegmentHandle(selected, point, slack * 1.4) : null;
      if (selected && handle) {
        gestureRef.current = { kind: "handle", id: selected.id, handle, marked: false };
        return;
      }
      const hit = pickShape(scene.shapes, point, slack);
      selectShape(hit);
      gestureRef.current = hit
        ? { kind: "move", id: hit.id, last: point, marked: false }
        : null;
      return;
    }

    if (tool === "crop") {
      gestureRef.current = { kind: "crop", start: point };
      setCropDraft({ x: point.x, y: point.y, width: 0, height: 0 });
      return;
    }

    if (tool === "cut") {
      gestureRef.current = { kind: "cut", start: point };
      setCutDraft(null);
      return;
    }

    if (tool === "callout") {
      gestureRef.current = { kind: "callout", tip: point };
      setDraft({
        id: newShapeId(),
        kind: "callout",
        color,
        strokeWidth: 1,
        at: point,
        tail: point,
        text: "Text",
        fontSize: fontSizeFor(style),
      });
      return;
    }

    const shape = createShape(tool, point, style);
    if (!shape) return;

    if (shape.kind === "step" || shape.kind === "emoji") {
      history.commit((current) => ({ ...current, shapes: [...current.shapes, shape] }));
      setSelectedId(shape.id);
      return;
    }

    gestureRef.current = { kind: "draw", start: point };
    setDraft(shape);
  }

  function handlePointerMove(event: ReactPointerEvent<HTMLCanvasElement>) {
    const gesture = gestureRef.current;
    if (!gesture) return;
    const point = toImagePoint(event);

    if (gesture.kind === "path") {
      setDraft((current) => {
        if (!current || current.kind !== "path") return current;
        const fixed = current.points.slice(0, -1);
        const previous = fixed[fixed.length - 1];
        const at = event.shiftKey && previous ? constrainPoint("path", previous, point) : point;
        return { ...current, points: [...fixed, at] };
      });
      return;
    }

    if (gesture.kind === "callout") {
      setDraft((current) =>
        current && current.kind === "callout" ? { ...current, at: point } : current
      );
      return;
    }

    if (gesture.kind === "draw") {
      setDraft((current) => {
        if (!current) return current;
        if (current.kind === "pen" || current.kind === "highlight") {
          const last = current.points[current.points.length - 1];
          // Thin the trail: raw pointer events would blow past the stored
          // point budget on a long stroke.
          if (Math.hypot(point.x - last.x, point.y - last.y) < displayScale) return current;
          return { ...current, points: [...current.points, point] };
        }
        if (!isSegmentShape(current)) return current;
        // The magnifier is always round; Shift squares boxes and snaps lines.
        const to =
          event.shiftKey || current.kind === "magnify"
            ? constrainPoint(current.kind, current.from, point)
            : point;
        return { ...current, to };
      });
      return;
    }

    if (gesture.kind === "crop") {
      setCropDraft(rectFromPoints(gesture.start, point));
      return;
    }

    if (gesture.kind === "cut") {
      const dx = Math.abs(point.x - gesture.start.x);
      const dy = Math.abs(point.y - gesture.start.y);
      if (Math.max(dx, dy) < 4 * displayScale) return;
      // The drag direction picks the axis: down cuts rows, sideways columns.
      setCutDraft(
        dy >= dx
          ? { axis: "rows", from: Math.min(gesture.start.y, point.y), to: Math.max(gesture.start.y, point.y) }
          : { axis: "columns", from: Math.min(gesture.start.x, point.x), to: Math.max(gesture.start.x, point.x) }
      );
      return;
    }

    if (!gesture.marked) {
      history.mark();
      gesture.marked = true;
    }

    if (gesture.kind === "move") {
      const dx = point.x - gesture.last.x;
      const dy = point.y - gesture.last.y;
      gesture.last = point;
      history.setScene((current) => ({
        ...current,
        shapes: current.shapes.map((shape) =>
          shape.id === gesture.id ? translateShape(shape, dx, dy) : shape
        ),
      }));
      return;
    }

    history.setScene((current) => ({
      ...current,
      shapes: current.shapes.map((shape) => {
        if (shape.id !== gesture.id) return shape;
        if (gesture.handle === "tail") {
          return shape.kind === "callout" ? { ...shape, tail: point } : shape;
        }
        if (!isSegmentShape(shape)) return shape;
        const anchor = gesture.handle === "from" ? shape.to : shape.from;
        const at =
          event.shiftKey || shape.kind === "magnify"
            ? constrainPoint(shape.kind, anchor, point)
            : point;
        return gesture.handle === "from" ? { ...shape, from: at } : { ...shape, to: at };
      }),
    }));
  }

  function handlePointerUp() {
    const gesture = gestureRef.current;
    // A path keeps going until it is finished.
    if (gesture?.kind === "path") return;
    gestureRef.current = null;

    if (gesture?.kind === "draw" && draft) {
      const minLength = 4 * displayScale;
      if (isMeaningfulShape(draft, minLength)) {
        history.commit((current) => ({ ...current, shapes: [...current.shapes, draft] }));
      }
      setDraft(null);
      return;
    }

    if (gesture?.kind === "callout" && draft?.kind === "callout") {
      setDraft(null);
      const tip = gesture.tip;
      // A click without a drag puts the bubble up and to the right of the tip.
      const dragged = distance(tip, draft.at) >= 8 * displayScale;
      const at = dragged ? draft.at : clickedCalloutPosition(tip, draft.fontSize, imageSize);
      setTextDraft({
        id: draft.id,
        kind: "callout",
        at,
        tail: tip,
        value: "",
        color: draft.color,
        fontSize: draft.fontSize,
        isNew: true,
      });
      return;
    }

    if (gesture?.kind === "cut" && imageSize) {
      const pending = cutDraft;
      setCutDraft(null);
      if (pending) {
        const limit = pending.axis === "rows" ? imageSize.height : imageSize.width;
        const cut: Cut = {
          axis: pending.axis,
          from: Math.max(0, pending.from),
          to: Math.min(limit, pending.to),
        };
        if (cut.to - cut.from >= 4) {
          history.commit((current) => ({ ...current, cuts: [...(current.cuts ?? []), cut] }));
        }
        return;
      }
      // A click on a strip that is set to be cut keeps it after all.
      const at = gesture.start;
      const cuts = scene.cuts ?? [];
      let index = -1;
      for (let i = cuts.length - 1; i >= 0; i -= 1) {
        const c = cuts[i];
        if (c.axis === "rows" ? at.y >= c.from && at.y <= c.to : at.x >= c.from && at.x <= c.to) {
          index = i;
          break;
        }
      }
      if (index >= 0) {
        history.commit((current) => {
          const remaining = (current.cuts ?? []).filter((_, i) => i !== index);
          const next: AnnotationScene = { ...current, cuts: remaining };
          if (remaining.length === 0) delete next.cuts;
          return next;
        });
      }
      return;
    }

    if (gesture?.kind === "crop" && cropDraft && imageSize) {
      const rect = clampRect(cropDraft, imageSize);
      setCropDraft(null);
      if (rect.width >= 8 && rect.height >= 8) {
        history.commit((current) => ({ ...current, crop: rect }));
      }
    }
  }

  function handleDoubleClick(event: ReactMouseEvent<HTMLCanvasElement>) {
    if (!imageSize) return;
    if (gestureRef.current?.kind === "path") {
      finishPath(false);
      return;
    }
    const point = toImagePoint(event);
    const hit = pickShape(scene.shapes, point, PICK_SLACK_PX * displayScale);
    if (!hit || (hit.kind !== "text" && hit.kind !== "callout")) return;
    setToolState("select");
    setSelectedId(hit.id);
    setTextDraft({
      id: hit.id,
      kind: hit.kind,
      at: hit.at,
      value: hit.text,
      color: hit.color,
      fontSize: hit.fontSize,
      isNew: false,
      tail: hit.kind === "callout" ? hit.tail : undefined,
      background: hit.kind === "text" ? hit.background : undefined,
    });
  }

  // Edits from the keyboard, the options bar, the weight buttons and the
  // color swatches act on a shape only while it is visibly selected: the
  // Select tool shows its outline. With a drawing tool active they are
  // settings for the next shape, and never change one picked earlier.
  const editingSelected = tool === "select" ? selected : null;
  const editingId = editingSelected?.id ?? null;

  // A selection whose shape is gone (undo, Clear, delete) is dropped, so no
  // shortcut can act on a shape that no longer exists.
  useEffect(() => {
    if (selectedId && !selected) setSelectedId(null);
  }, [selected, selectedId]);

  const updateSelected = useCallback(
    (update: (shape: Shape) => Shape, coalesceKey?: string) => {
      if (!editingId) return;
      history.commit(
        (current) => {
          let changed = false;
          const shapes = current.shapes.map((shape) => {
            if (shape.id !== editingId) return shape;
            const next = update(shape);
            if (next !== shape) changed = true;
            return next;
          });
          // Returning the same scene records no undo step and keeps Redo.
          return changed ? { ...current, shapes } : current;
        },
        coalesceKey ? { coalesceKey } : undefined
      );
    },
    [editingId, history]
  );

  const deleteSelected = useCallback(() => {
    if (!editingId) return;
    history.commit((current) => {
      const shapes = current.shapes.filter((shape) => shape.id !== editingId);
      return shapes.length === current.shapes.length ? current : { ...current, shapes };
    });
    setSelectedId(null);
  }, [editingId, history]);

  const duplicateSelected = useCallback(() => {
    if (!editingSelected) return;
    const offset = 16 * scale;
    const copy = { ...translateShape(editingSelected, offset, offset), id: newShapeId() };
    history.commit((current) => ({ ...current, shapes: [...current.shapes, copy] }));
    setSelectedId(copy.id);
  }, [editingSelected, history, scale]);

  const reorderSelected = useCallback(
    (toFront: boolean) => {
      if (!editingId) return;
      history.commit((current) => {
        const index = current.shapes.findIndex((s) => s.id === editingId);
        if (index < 0) return current;
        const shape = current.shapes[index];
        if (index === (toFront ? current.shapes.length - 1 : 0)) return current;
        const rest = current.shapes.filter((s) => s.id !== editingId);
        const moved = toFront ? [...rest, shape] : [shape, ...rest];
        // Step badges are numbered by their order in the list, so moving one
        // would renumber the walkthrough. Steps keep their sequence: the step
        // slots in the new order are refilled in the original step order.
        const steps = current.shapes.filter((s) => s.kind === "step");
        let next = 0;
        const shapes = moved.map((s) => (s.kind === "step" ? steps[next++] : s));
        return { ...current, shapes };
      });
    },
    [editingId, history]
  );

  const nudgeSelected = useCallback(
    (dx: number, dy: number) =>
      updateSelected((shape) => translateShape(shape, dx, dy), `nudge:${editingId}`),
    [editingId, updateSelected]
  );

  useEffect(() => {
    function onKeyDown(event: KeyboardEvent) {
      const typing = isTextEntry(event.target);

      if (event.key === "Escape") {
        if (textDraft) closeTextDraft();
        else if (gestureRef.current?.kind === "path") cancelPath();
        else if (cropDraft) setCropDraft(null);
        else if (cutDraft) {
          gestureRef.current = null;
          setCutDraft(null);
        }
        else setSelectedId(null);
        return;
      }
      if (typing || isSliderKey(event.target, event.key)) return;

      // Plain Enter only: Cmd/Ctrl+Enter is the desktop window's export
      // shortcut, which settles the path itself.
      if (
        event.key === "Enter" &&
        !event.metaKey &&
        !event.ctrlKey &&
        !event.altKey &&
        gestureRef.current?.kind === "path"
      ) {
        event.preventDefault();
        finishPath(false);
        return;
      }

      if ((event.metaKey || event.ctrlKey) && !event.altKey && event.key.toLowerCase() === "z") {
        event.preventDefault();
        if (event.shiftKey) history.redo();
        else history.undo();
        return;
      }
      if ((event.metaKey || event.ctrlKey) && !event.altKey && event.key.toLowerCase() === "d") {
        if (editingId) {
          event.preventDefault();
          duplicateSelected();
        }
        return;
      }
      // [ and ] come through AltGr (Ctrl+Alt on Windows) or Option on some
      // layouts, so they are checked before modified keys are ignored.
      if (
        editingId &&
        (event.key === "]" || event.key === "[") &&
        !event.metaKey &&
        (!event.ctrlKey || event.altKey)
      ) {
        event.preventDefault();
        reorderSelected(event.key === "]");
        return;
      }
      if (event.metaKey || event.ctrlKey || event.altKey) return;

      if (event.key === "Delete" || event.key === "Backspace") {
        if (editingId) {
          event.preventDefault();
          deleteSelected();
        }
        return;
      }

      if (editingId && event.key.startsWith("Arrow")) {
        event.preventDefault();
        const step = (event.shiftKey ? 10 : 1) * Math.max(1, displayScale);
        const dx = event.key === "ArrowLeft" ? -step : event.key === "ArrowRight" ? step : 0;
        const dy = event.key === "ArrowUp" ? -step : event.key === "ArrowDown" ? step : 0;
        nudgeSelected(dx, dy);
        return;
      }

      const next = TOOL_SHORTCUTS[event.key.toLowerCase()];
      if (next) {
        event.preventDefault();
        setTool(next);
      }
    }

    window.addEventListener("keydown", onKeyDown);
    return () => window.removeEventListener("keydown", onKeyDown);
  }, [
    cancelPath,
    closeTextDraft,
    cropDraft,
    cutDraft,
    deleteSelected,
    displayScale,
    duplicateSelected,
    editingId,
    finishPath,
    history,
    nudgeSelected,
    reorderSelected,
    setTool,
    textDraft,
  ]);

  const changeColor = useCallback(
    (next: string) => {
      setColor(next);
      // A drag through the color picker fires on every move: one undo step.
      updateSelected((shape) => ({ ...shape, color: next }), `color:${editingId}`);
    },
    [editingId, updateSelected]
  );

  const setWeight = useCallback(
    (next: number) => {
      setWeightState(next);
      updateSelected((shape) => applyWeight(shape, { color, weight: next, scale }));
    },
    [color, scale, updateSelected]
  );

  const setOption = useCallback(
    <K extends keyof ToolOptions>(key: K, value: ToolOptions[K]) => {
      setOptions((current) => ({ ...current, [key]: value }));
      updateSelected((shape) => applyOption(shape, key, value));
    },
    [updateSelected]
  );

  const setBackdrop = useCallback(
    (next: Backdrop | null) => {
      history.commit(
        (current) => ({ ...current, backdrop: next }),
        // Slider drags and repeated picks of the same control fold into one
        // undo step; turning the background on or off, or touching another
        // control, is its own step.
        { coalesceKey: backdropChangeKey(sceneRef.current.backdrop ?? null, next) }
      );
    },
    [history]
  );

  const backdropPreview = useMemo<BackdropPreview | null>(() => {
    if (!scene.backdrop || !imageSize) return null;
    return {
      layout: layoutBackdrop(imageSize.width, imageSize.height, scene.backdrop, Infinity),
      backdrop: scene.backdrop,
      autoColors,
      displayScale,
      imageUrl: sourceUrl,
    };
  }, [autoColors, displayScale, imageSize, scene.backdrop, sourceUrl]);

  const getImage = useCallback(
    () =>
      imageRef.current && imageSize
        ? { source: imageRef.current as CanvasImageSource, ...imageSize }
        : null,
    [imageSize]
  );

  const exportPng = useCallback(
    async (override?: AnnotationScene): Promise<Blob> => {
      const source = getImage();
      if (!source) throw new Error("The screenshot is still loading.");
      return canvasToPngBlob(
        flattenScene(
          source.source,
          { width: source.width, height: source.height },
          override ?? scene
        )
      );
    },
    [getImage, scene]
  );

  const textareaScale = 1 / Math.max(displayScale, 0.0001);
  const textInput: AnnotationEditor["textInput"] = textDraft
    ? {
        value: textDraft.value,
        style: textInputStyle(textDraft, textareaScale, textBounds(scene, imageSize)),
        onChange: (event) => {
          const value = event.target.value;
          setTextDraft((current) => (current ? { ...current, value } : current));
        },
        onBlur: () => commitTextDraft(),
        onKeyDown: (event) => {
          if (event.key === "Enter" && (event.metaKey || event.ctrlKey)) {
            event.preventDefault();
            commitTextDraft();
          }
          if (event.key === "Escape") {
            event.preventDefault();
            closeTextDraft();
          }
        },
      }
    : null;

  return {
    history,
    scene,
    tool,
    setTool,
    color,
    changeColor,
    weight,
    setWeight,
    options,
    setOption,
    selected,
    deleteSelected,
    duplicateSelected,
    bringToFront: () => reorderSelected(true),
    sendToBack: () => reorderSelected(false),
    pickedColor,
    drafting: textDraft !== null || draft?.kind === "path",
    setBackdrop,
    backdropPreview,
    autoColors,
    imageSize,
    loadError,
    cropped: scene.crop !== null,
    resetCrop: () => history.commit((current) => ({ ...current, crop: null })),
    cutCount: scene.cuts?.length ?? 0,
    resetCuts: () =>
      history.commit((current) => {
        if (!current.cuts) return current;
        const next = { ...current };
        delete next.cuts;
        return next;
      }),
    // The background is a look, not an annotation, so it survives Clear.
    // Anything still being drawn (a path, a text box, a cut strip) is part of
    // what is being removed too, or Save would put it straight back.
    clear: () => {
      cancelPath();
      closeTextDraft();
      setCutDraft(null);
      if (gestureRef.current?.kind === "cut") gestureRef.current = null;
      setSelectedId(null);
      history.commit((current) => {
        const next: AnnotationScene = { ...current, shapes: [], crop: null };
        delete next.cuts;
        return next;
      });
    },
    canvasRef,
    canvasHandlers: {
      onPointerDown: handlePointerDown,
      onPointerMove: handlePointerMove,
      onPointerUp: handlePointerUp,
      onPointerCancel: handlePointerUp,
      onDoubleClick: handleDoubleClick,
    },
    textInput,
    settleScene,
    exportPng,
  };
}
