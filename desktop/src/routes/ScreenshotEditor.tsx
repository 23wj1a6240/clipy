import { useCallback, useEffect, useRef, useState, type MutableRefObject, type ReactNode } from "react";
import { getCurrentWindow } from "@tauri-apps/api/window";
import {
  closeScreenshotEditor,
  discardScreenshot,
  finishScreenshot,
  getPendingScreenshot,
  onAuthStateChanged,
  readPendingScreenshot,
  revealScreenshot,
  stageScreenshotScene,
  type PendingScreenshotDto,
  type ScreenshotDto,
  type ScreenshotFinishAction,
} from "../lib/bridge";
import { open } from "@tauri-apps/plugin-shell";
import { useDesktopT, type DesktopT } from "../lib/messages";
import { isWindows } from "../lib/platform";
import { markFeatureTried } from "../lib/whatsNew";
import { LOCAL_EDITION } from "../lib/edition";
// The drawing logic is the web editor's (clipy.online/s/<id>/edit), shared so
// a gesture fix lands in both. Only the chrome around the canvas is native.
import { useAnnotationEditor } from "../vendor/components/screenshot-editor/useAnnotationEditor";
import {
  EDITOR_TOOLS,
  STROKE_STEPS,
  TOOL_HINTS,
  isTextEntry,
  type EditorTool,
} from "../vendor/components/screenshot-editor/tools";
import { ANNOTATION_COLORS, parseSceneJson, sceneIsEmpty } from "../vendor/lib/screenshots/scene";
import {
  BackdropFrame,
  BackdropPanel,
  ToolOptionsBar,
  backdropFitRatio,
  panelOffset,
  type EditorChromeTheme,
} from "../vendor/components/screenshot-editor/chrome";

/** The desktop app's tokens for the shared editor chrome. */
const DESKTOP_CHROME_THEME: EditorChromeTheme = {
  chip: "inline-flex h-7 items-center justify-center rounded-md px-2 text-[12px] font-medium text-ink-secondary transition-colors hover:bg-row hover:text-ink-text disabled:opacity-40",
  chipActive: "!bg-flash-50 !text-flash-600",
  label: "text-[11px] font-medium text-ink-secondary",
  panel: "rounded-xl border border-subtle bg-surface text-flash-600 shadow-pop",
  input: "accent-[#ff5e3a]",
  divider: "border-subtle",
};

const MOD = isWindows ? "Ctrl" : "⌘";

/**
 * Ctrl+C on Windows, ⌘C on the Mac. `event.code` covers layouts whose C key
 * types another letter (Cyrillic "с", Greek "ψ"), where native copy works.
 */
function isCopyShortcut(event: KeyboardEvent): boolean {
  if (!(isWindows ? event.ctrlKey : event.metaKey) || event.shiftKey || event.altKey) return false;
  const key = event.key.toLowerCase();
  return key === "c" || (!/^[a-z]$/.test(key) && event.code === "KeyC");
}

/**
 * The annotation window a desktop screenshot opens in (`#screenshot-editor`).
 * Rust holds the capture; this page asks for it by id, draws on it, and hands
 * the flattened PNG back on Copy image or Upload. The window stays open after
 * either, with a notice of what reached the clipboard, until the user closes
 * it. Closing before anything was copied counts as Copy image, so nothing
 * drawn is thrown away by the red button and nothing is shared by it either.
 */
export default function ScreenshotEditorWindow() {
  const t = useDesktopT();
  // undefined while loading, null when Rust has nothing waiting.
  const [pending, setPending] = useState<PendingScreenshotDto | null | undefined>(undefined);
  const [sourceUrl, setSourceUrl] = useState<string | null>(null);
  const [loadError, setLoadError] = useState<string | null>(null);

  const load = useCallback(async () => {
    setLoadError(null);
    try {
      const next = await getPendingScreenshot();
      if (!next) {
        setPending(null);
        setSourceUrl(null);
        return;
      }
      setPending(next);
      const bytes = await readPendingScreenshot(next.id);
      const url = URL.createObjectURL(new Blob([bytes], { type: "image/png" }));
      setSourceUrl((previous) => {
        if (previous) URL.revokeObjectURL(previous);
        return url;
      });
    } catch (error) {
      setLoadError(String(error));
    }
  }, []);

  useEffect(() => {
    void load();
  }, [load]);

  // One close handler for the window's whole life. The editor installs Copy
  // image here once it is up. Before that (still loading, or the load failed) the
  // close goes to Rust, which copies whatever capture is pending as it was
  // captured. The page may not know the id yet, so it must not decide.
  const closeRef = useRef<(() => Promise<void>) | null>(null);
  useEffect(() => {
    const closeWithoutEditor = () => closeScreenshotEditor().catch(() => {});
    let unlisten: (() => void) | undefined;
    let active = true;
    void getCurrentWindow()
      .onCloseRequested((event) => {
        event.preventDefault();
        void (closeRef.current ?? closeWithoutEditor)();
      })
      .then((fn) => {
        if (active) unlisten = fn;
        else fn();
      })
      .catch(() => {});
    return () => {
      active = false;
      unlisten?.();
    };
  }, []);

  if (loadError) {
    return (
      <Centered>
        <p className="text-[14px] text-ink-text">{t("screenshotEditor.openFailed")}</p>
        <p className="max-w-md text-center text-[12px] text-ink-secondary">{loadError}</p>
        <button type="button" onClick={() => void load()} className={SECONDARY_BUTTON}>
          {t("screenshotEditor.tryAgain")}
        </button>
      </Centered>
    );
  }

  if (pending === null) {
    return (
      <Centered>
        <p className="text-[14px] text-ink-secondary">{t("screenshotEditor.nothingWaiting")}</p>
        <button
          type="button"
          onClick={() => void closeScreenshotEditor().catch(() => {})}
          className={SECONDARY_BUTTON}
        >
          {t("screenshotEditor.close")}
        </button>
      </Centered>
    );
  }

  if (!pending || !sourceUrl) {
    return (
      <Centered>
        <Spinner />
      </Centered>
    );
  }

  return <Editor pending={pending} sourceUrl={sourceUrl} closeRef={closeRef} />;
}

function Editor({
  pending,
  sourceUrl,
  closeRef,
}: {
  pending: PendingScreenshotDto;
  sourceUrl: string;
  closeRef: MutableRefObject<(() => Promise<void>) | null>;
}) {
  const t = useDesktopT();
  // A reloaded window picks up its drawing where it left off.
  const [initialScene] = useState(() => parseSceneJson(pending.scene));
  const editor = useAnnotationEditor({
    sourceUrl,
    initialScene,
    loadErrorMessage: t("screenshotEditor.loadFailed"),
  });
  const { history, scene, tool, imageSize, canvasRef, canvasHandlers, textInput } = editor;
  const [backdropOpen, setBackdropOpen] = useState(false);
  const [panelLeft, setPanelLeft] = useState(0);
  const backdropRef = useRef<HTMLDivElement>(null);
  useEffect(() => {
    if (!backdropOpen) return;
    function onPointerDown(event: PointerEvent) {
      if (!backdropRef.current?.contains(event.target as Node)) setBackdropOpen(false);
    }
    function onKeyDown(event: KeyboardEvent) {
      if (event.key === "Escape") setBackdropOpen(false);
    }
    window.addEventListener("pointerdown", onPointerDown);
    window.addEventListener("keydown", onKeyDown);
    return () => {
      window.removeEventListener("pointerdown", onPointerDown);
      window.removeEventListener("keydown", onKeyDown);
    };
  }, [backdropOpen]);
  const customColor = !(ANNOTATION_COLORS as readonly string[]).includes(editor.color);
  // Turning a background on ticks the What's New checklist entry for it.
  const setBackdrop = editor.setBackdrop;
  const changeBackdrop = useCallback(
    (next: Parameters<typeof setBackdrop>[0]) => {
      setBackdrop(next);
      if (next) void markFeatureTried("screenshot_style").catch(() => {});
    },
    [setBackdrop]
  );
  // The canvas area does not scroll, so with a background the canvas shrinks
  // to leave room for the padding around it.
  const fit = backdropFitRatio(editor.backdropPreview);
  const canvasFitStyle = editor.backdropPreview
    ? {
        maxHeight: `calc((100vh - 190px) * ${fit.height.toFixed(4)})`,
        maxWidth: `calc((100vw - 2rem) * ${fit.width.toFixed(4)})`,
      }
    : undefined;
  const [busy, setBusy] = useState<ScreenshotFinishAction | "delete" | null>(null);
  const [error, setError] = useState<string | null>(null);
  // The last Copy image or Upload. Stays on screen until dismissed.
  const [result, setResult] = useState<ScreenshotDto | null>(null);
  // Something reached the clipboard. Outlives a dismissed notice, and is
  // seeded from Rust so a reloaded window still knows not to copy on close.
  const [delivered, setDelivered] = useState(pending.delivered);
  // This screenshot has a link; Upload now updates it instead of making one.
  const [linked, setLinked] = useState(pending.linked);
  const deliveredRef = useRef(pending.delivered);
  // A close whose copy failed. The next close closes anyway, so a failure
  // that keeps repeating cannot trap the window.
  const closeFailedRef = useRef(false);
  // Once an edit has been written over the capture, an empty scene must be
  // exported too, or clearing the drawing would leave the old one on disk.
  const editWrittenRef = useRef(pending.editWritten);
  // State alone cannot stop a second finish: a close and a click, or an
  // autorepeating shortcut, can both land before React re-renders.
  const busyRef = useRef(false);
  // Signing in (or out) while the editor is open changes whether Upload is
  // available, so follow the app's auth state rather than the opening value.
  const [canUpload, setCanUpload] = useState(pending.canUpload);
  useEffect(() => {
    // The local edition never uploads (Rust reports canUpload false too).
    if (LOCAL_EDITION) return;
    let unlisten: (() => void) | undefined;
    let active = true;
    void onAuthStateChanged((state) => setCanUpload(state.signed_in))
      .then((fn) => {
        if (active) unlisten = fn;
        else fn?.();
      })
      .catch(() => {});
    return () => {
      active = false;
      unlisten?.();
    };
  }, []);
  // ⌘↵ keeps doing what the old single button did: share when the capture
  // would have been shared without the editor, copy otherwise.
  const defaultAction: ScreenshotFinishAction =
    pending.uploadByDefault && canUpload ? "upload" : "copy";

  const finish = useCallback(async (action: ScreenshotFinishAction): Promise<boolean> => {
    if (busyRef.current) return false;
    busyRef.current = true;
    setBusy(action);
    setError(null);
    setResult(null);
    try {
      // A close or a shortcut does not blur the text box, so place any text
      // still being typed before reading the scene.
      const final = editor.settleScene();
      // Nothing drawn: keep the capture byte for byte rather than re-encode it.
      const png = sceneIsEmpty(final) && !editWrittenRef.current
        ? null
        : new Uint8Array(await (await editor.exportPng(final)).arrayBuffer());
      // The share keeps the drawing as editable shapes, like a web save, and
      // a reloaded window reopens with it.
      await stageScreenshotScene(pending.id, JSON.stringify(final));
      const shot = await finishScreenshot(pending.id, png, action);
      if (png) editWrittenRef.current = true;
      if (shot.shareUrl) setLinked(true);
      if (shot.clipboard !== "none") {
        deliveredRef.current = true;
        setDelivered(true);
      }
      setResult(shot);
      return true;
    } catch (err) {
      setError(String(err));
      return false;
    } finally {
      setBusy(null);
      busyRef.current = false;
    }
  }, [editor, pending.id]);

  // Once copied or shared, closing just closes: the clipboard already holds
  // what the user chose.
  const close = useCallback(async () => {
    if (busyRef.current) return;
    if (!deliveredRef.current && !closeFailedRef.current && !(await finish("copy"))) {
      closeFailedRef.current = true;
      return;
    }
    // Held until the window is gone, so an Upload pressed in between cannot
    // start and then lose its window.
    busyRef.current = true;
    await closeScreenshotEditor().catch(() => {
      busyRef.current = false;
    });
  }, [finish]);

  const remove = useCallback(async () => {
    if (busyRef.current) return;
    busyRef.current = true;
    setBusy("delete");
    setError(null);
    try {
      await discardScreenshot(pending.id);
    } catch (err) {
      setError(String(err));
      setBusy(null);
      busyRef.current = false;
    }
  }, [pending.id]);

  // The red button copies the screenshot instead of dropping the drawing. It
  // never uploads: a public link only comes from pressing Upload.
  const finishRef = useRef(finish);
  finishRef.current = finish;
  const closeWindowRef = useRef(close);
  closeWindowRef.current = close;
  const defaultActionRef = useRef(defaultAction);
  defaultActionRef.current = defaultAction;
  useEffect(() => {
    const closeWindow = () => closeWindowRef.current();
    closeRef.current = closeWindow;
    return () => {
      if (closeRef.current === closeWindow) closeRef.current = null;
    };
  }, [closeRef]);

  const disabled = !imageSize || busy !== null;
  // The shortcuts follow the buttons: nothing runs before the image is up.
  const readyRef = useRef(false);
  readyRef.current = imageSize !== null;

  // ⌘↵ / Ctrl+Enter runs the default action. ⌘C / Ctrl+C copies the image,
  // as in every other screenshot editor; the webview has nothing selected to
  // copy, so those keys used to do nothing. Inside the text box the keys are
  // left to it (place the text, copy what is selected), and so is a text
  // selection in the result notice, the only selectable text here.
  useEffect(() => {
    function onKeyDown(event: KeyboardEvent) {
      if (event.repeat || !readyRef.current) return;
      // Sliders and color inputs in the Background panel are not text entry,
      // so the export shortcuts keep working after one has been used.
      if (isTextEntry(event.target)) return;
      let action: ScreenshotFinishAction;
      if (event.key === "Enter" && (event.metaKey || event.ctrlKey)) {
        action = defaultActionRef.current;
      } else if (isCopyShortcut(event)) {
        const selection = window.getSelection();
        if (selection && !selection.isCollapsed && selection.toString().trim()) return;
        action = "copy";
      } else {
        return;
      }
      event.preventDefault();
      void finishRef.current(action);
    }
    window.addEventListener("keydown", onKeyDown);
    return () => window.removeEventListener("keydown", onKeyDown);
  }, []);

  const buttonClass = (action: ScreenshotFinishAction) =>
    action === defaultAction ? PRIMARY_BUTTON : SECONDARY_BUTTON;
  const shortcutHint = (action: ScreenshotFinishAction) =>
    action === defaultAction ? ` (${MOD}↵)` : "";

  return (
    // Unselectable chrome: a drag that leaves the canvas would otherwise
    // select hint text, and ⌘C / Ctrl+C would then copy that, not the image.
    <main className="flex h-screen select-none flex-col bg-canvas text-ink-text">
      <header className="flex flex-wrap items-center gap-2 border-b border-subtle bg-surface px-3 py-2">
        <div className="flex items-center gap-0.5" role="toolbar" aria-label={t("screenshotEditor.toolbar.tools")}>
          {EDITOR_TOOLS.map(({ tool: value, label, shortcut }) => (
            <ToolbarButton
              key={value}
              label={label}
              title={`${label} (${shortcut})`}
              pressed={tool === value}
              disabled={!imageSize}
              onClick={() => editor.setTool(value)}
            >
              <ToolIcon tool={value} />
            </ToolbarButton>
          ))}
        </div>

        <Divider />

        <div className="flex items-center gap-1.5" role="group" aria-label={t("screenshotEditor.toolbar.color")}>
          {ANNOTATION_COLORS.map((swatch) => (
            <button
              key={swatch}
              type="button"
              disabled={!imageSize}
              onClick={() => editor.changeColor(swatch)}
              title={swatch}
              aria-label={t("screenshotEditor.toolbar.colorSwatch", { color: swatch })}
              aria-pressed={editor.color === swatch}
              className={`h-5 w-5 rounded-full border-2 transition-transform disabled:opacity-40 ${
                editor.color === swatch ? "scale-110 border-ink-text" : "border-subtle hover:scale-105"
              }`}
              style={{ backgroundColor: swatch }}
            />
          ))}
          <label
            title={t("screenshotEditor.toolbar.customColor")}
            className={`relative h-5 w-5 cursor-pointer overflow-hidden rounded-full border-2 transition-transform ${
              customColor ? "scale-110 border-ink-text" : "border-subtle hover:scale-105"
            }`}
            style={{
              background: customColor
                ? editor.color
                : "conic-gradient(#ff3b30, #ffcc00, #34c759, #0a84ff, #af52de, #ff3b30)",
            }}
          >
            <span className="sr-only">{t("screenshotEditor.toolbar.customColor")}</span>
            <input
              type="color"
              value={editor.color}
              disabled={!imageSize}
              onChange={(event) => editor.changeColor(event.target.value)}
              className="absolute inset-0 h-full w-full cursor-pointer opacity-0"
            />
          </label>
        </div>

        <Divider />

        <div className="flex items-center gap-0.5" role="group" aria-label={t("screenshotEditor.toolbar.strokeWeight")}>
          {STROKE_STEPS.map((step) => (
            <ToolbarButton
              key={step.label}
              label={t("screenshotEditor.toolbar.stroke", { weight: step.label })}
              title={t("screenshotEditor.toolbar.stroke", { weight: step.label })}
              pressed={editor.weight === step.value}
              disabled={!imageSize}
              onClick={() => editor.setWeight(step.value)}
            >
              <span
                className="rounded-full bg-current"
                style={{ width: 4 + step.value * 4, height: 4 + step.value * 4 }}
              />
            </ToolbarButton>
          ))}
        </div>

        <Divider />

        <div ref={backdropRef} className="relative">
          <button
            type="button"
            disabled={!imageSize}
            onClick={() => {
              setPanelLeft(panelOffset(backdropRef.current));
              setBackdropOpen((open) => !open);
            }}
            aria-expanded={backdropOpen}
            title={t("screenshotEditor.toolbar.backgroundTitle")}
            className={`inline-flex h-8 items-center gap-1.5 rounded-md px-2 text-[12px] font-semibold transition-colors disabled:opacity-40 ${
              scene.backdrop ? "bg-flash-50 text-flash-600" : "text-ink-secondary hover:bg-row hover:text-ink-text"
            }`}
          >
            <Glyph>
              <rect x="3" y="3" width="18" height="18" rx="3" />
              <rect x="7" y="7" width="10" height="10" rx="1.5" />
            </Glyph>
            {t("screenshotEditor.toolbar.background")}
          </button>
          {backdropOpen && (
            // Placed so it stays inside the window wherever the button is.
            <div className="absolute top-full z-30 mt-2" style={{ left: panelLeft }}>
              <BackdropPanel
                theme={DESKTOP_CHROME_THEME}
                backdrop={scene.backdrop ?? null}
                autoColors={editor.autoColors}
                onChange={changeBackdrop}
                disabled={!imageSize}
                imageUrl={sourceUrl}
              />
            </div>
          )}
        </div>

        <div className="ml-auto flex items-center gap-0.5">
          <ToolbarButton
            label={t("screenshotEditor.toolbar.undo")}
            title={t("screenshotEditor.toolbar.undoTitle", { keys: `${MOD}Z` })}
            disabled={!imageSize || !history.canUndo}
            onClick={history.undo}
          >
            <Glyph d="M9 14 4 9l5-5M4 9h10.5a5.5 5.5 0 0 1 0 11H11" />
          </ToolbarButton>
          <ToolbarButton
            label={t("screenshotEditor.toolbar.redo")}
            title={t("screenshotEditor.toolbar.redoTitle", { keys: `⇧${MOD}Z` })}
            disabled={!imageSize || !history.canRedo}
            onClick={history.redo}
          >
            <Glyph d="m15 14 5-5-5-5M20 9H9.5a5.5 5.5 0 0 0 0 11H13" />
          </ToolbarButton>
          <ToolbarButton
            label={t("screenshotEditor.toolbar.clear")}
            title={t("screenshotEditor.toolbar.clear")}
            disabled={!imageSize || (scene.shapes.length === 0 && !editor.cropped && editor.cutCount === 0)}
            onClick={editor.clear}
          >
            <Glyph d="M3 6h18M8 6V4h8v2M19 6l-1 14H6L5 6" />
          </ToolbarButton>
        </div>
      </header>

      <div className="border-b border-subtle bg-surface px-3 py-1.5">
          <ToolOptionsBar
            theme={DESKTOP_CHROME_THEME}
            tool={tool}
            selected={editor.selected}
            options={editor.options}
            onOption={editor.setOption}
            onDuplicate={editor.duplicateSelected}
            onBringToFront={editor.bringToFront}
            onSendToBack={editor.sendToBack}
            onDelete={editor.deleteSelected}
            disabled={!imageSize}
          />
      </div>

      <section className="relative flex min-h-0 flex-1 items-center justify-center overflow-hidden bg-raised p-4">
        {result && <ResultNotice result={result} onDismiss={() => setResult(null)} />}
        {editor.loadError ? (
          <p className="text-[13px] text-flash-600">{editor.loadError}</p>
        ) : (
          <>
            {!imageSize && <Spinner />}
            {/* The inline-block wrapper shrink-wraps the canvas so the text
                box can be placed in canvas coordinates. */}
            <BackdropFrame preview={editor.backdropPreview}>
            <div className="relative inline-block max-h-full max-w-full leading-none">
              <canvas
                ref={canvasRef}
                aria-label={t("screenshotEditor.canvas")}
                style={canvasFitStyle}
                className={`block h-auto max-h-[calc(100vh-190px)] w-auto max-w-full touch-none ${
                  scene.backdrop ? "" : "rounded-md shadow-soft"
                } ${imageSize ? "" : "hidden"} ${
                  tool === "select" ? "cursor-default" : tool === "picker" ? "cursor-copy" : "cursor-crosshair"
                }`}
                {...canvasHandlers}
              />
              {textInput && (
                <textarea
                  autoFocus
                  value={textInput.value}
                  onChange={textInput.onChange}
                  onBlur={textInput.onBlur}
                  onKeyDown={textInput.onKeyDown}
                  placeholder={t("screenshotEditor.textPlaceholder", { keys: `${MOD}↵` })}
                  style={textInput.style}
                  className="absolute z-10 min-h-[2em] w-56 max-w-[70%] select-text resize-none rounded-md border border-flash-500 bg-surface/95 p-1 font-semibold leading-tight shadow-pop outline-none"
                />
              )}
            </div>
            </BackdropFrame>
          </>
        )}
      </section>

      <footer className="flex items-center gap-3 border-t border-subtle bg-surface px-4 py-2.5">
        <p className="mr-auto min-w-0 truncate text-[12px] text-ink-secondary">
          {error ? (
            <span className="select-text text-flash-600">{error}</span>
          ) : (
            <>
              {hintFor(tool, t)}
              {editor.pickedColor && (
                <span className="ml-2 font-medium text-ink-text">
                  {t("screenshotEditor.picked", { color: editor.pickedColor })}
                </span>
              )}
              {editor.cropped && (
                <button
                  type="button"
                  onClick={editor.resetCrop}
                  className="ml-2 font-medium text-flash-600 hover:underline"
                >
                  {t("screenshotEditor.resetCrop")}
                </button>
              )}
              {editor.cutCount > 0 && (
                <button
                  type="button"
                  onClick={editor.resetCuts}
                  className="ml-2 font-medium text-flash-600 hover:underline"
                >
                  {t("screenshotEditor.resetCuts")}
                </button>
              )}
            </>
          )}
        </p>
        {/* After a copy or upload, deleting the local file would not take
            back the clipboard or the link, so the slot closes the window. */}
        {delivered ? (
          <button type="button" onClick={() => void close()} disabled={busy !== null} className={SECONDARY_BUTTON}>
            {t("screenshotEditor.close")}
          </button>
        ) : (
          <button
            type="button"
            onClick={() => void remove()}
            disabled={busy !== null}
            title={t("screenshotEditor.deleteTitle")}
            className={SECONDARY_BUTTON}
          >
            {busy === "delete" ? t("screenshotEditor.deleting") : t("screenshotEditor.delete")}
          </button>
        )}
        <button
          type="button"
          onClick={() => void finish("copy")}
          disabled={disabled}
          title={t("screenshotEditor.copyTitle", {
            keys: `${MOD}C${defaultAction === "copy" ? `, ${MOD}↵` : ""}`,
          })}
          className={buttonClass("copy")}
        >
          {busy === "copy" ? t("screenshotEditor.copying") : t("screenshotEditor.copy")}
        </button>
        {!LOCAL_EDITION && <button
          type="button"
          onClick={() => void finish("upload")}
          disabled={disabled || !canUpload}
          title={
            !canUpload
              ? t("screenshotEditor.signInToUpload")
              : linked
                ? t("screenshotEditor.updateTitle", { shortcut: shortcutHint("upload") })
                : t("screenshotEditor.uploadTitle", { shortcut: shortcutHint("upload") })
          }
          className={buttonClass("upload")}
        >
          {busy === "upload"
            ? linked
              ? t("screenshotEditor.updating")
              : t("screenshotEditor.uploading")
            : linked
              ? t("screenshotEditor.updateLink")
              : t("screenshotEditor.upload")}
        </button>}
      </footer>
    </main>
  );
}

/** What the last Copy image or Upload did. Only the user dismisses it. */
function ResultNotice({ result, onDismiss }: { result: ScreenshotDto; onDismiss: () => void }) {
  const t = useDesktopT();
  const link = result.clipboard === "link" ? result.shareUrl : null;
  let title: string;
  let detail: string | null = null;
  if (link) {
    title = result.shareUpdated
      ? t("screenshotEditor.result.linkUpdated")
      : t("screenshotEditor.result.linkCopied");
  } else if (result.uploadError) {
    title =
      result.clipboard === "image"
        ? t("screenshotEditor.result.uploadFailedCopied")
        : t("screenshotEditor.result.uploadFailed");
    detail = result.uploadError;
  } else if (result.clipboard === "image") {
    title = t("screenshotEditor.result.copied");
  } else {
    title = t("screenshotEditor.result.notCopied");
  }
  const failed = Boolean(result.uploadError) || result.clipboard === "none";

  return (
    <div
      role="status"
      aria-live="polite"
      className="absolute left-1/2 top-3 z-20 flex w-[min(520px,calc(100%-24px))] select-text -translate-x-1/2 items-start gap-3 rounded-xl border border-subtle bg-surface px-4 py-3 shadow-pop"
    >
      <span
        aria-hidden
        className={`mt-1 h-2 w-2 shrink-0 rounded-full ${failed ? "bg-flash-500" : "bg-emerald-500"}`}
      />
      <div className="min-w-0 flex-1 text-[13px]">
        <p className="font-semibold text-ink-text">{title}</p>
        {detail && <p className="mt-0.5 break-words text-[12px] text-ink-secondary">{detail}</p>}
        {link ? (
          <button
            type="button"
            onClick={() => void open(link).catch(() => {})}
            title={t("screenshotEditor.result.openInBrowser")}
            className="mt-0.5 block max-w-full truncate text-[12px] font-medium text-[#2f5fd0] hover:underline"
          >
            {link}
          </button>
        ) : (
          <button
            type="button"
            onClick={() => void revealScreenshot(result.path).catch(() => {})}
            className="mt-0.5 text-[12px] font-medium text-[#2f5fd0] hover:underline"
          >
            {isWindows ? t("screenshotEditor.result.showInExplorer") : t("screenshotEditor.result.showInFinder")}
          </button>
        )}
      </div>
      <button
        type="button"
        onClick={onDismiss}
        aria-label={t("screenshotEditor.result.dismiss")}
        title={t("screenshotEditor.result.dismiss")}
        className="-mr-1 inline-flex h-6 w-6 shrink-0 items-center justify-center rounded-md text-ink-secondary hover:bg-row hover:text-ink-text"
      >
        <Glyph d="M6 6l12 12M18 6 6 18" />
      </button>
    </div>
  );
}

/**
 * The shared hints say "when you save"; here the crop applies on Copy or
 * Upload. The other tools' hints come from the web editor (TOOL_HINTS),
 * which is shared with clipy.online and still English.
 */
function hintFor(tool: EditorTool, t: DesktopT): string {
  if (tool === "crop") return t("screenshotEditor.hints.crop");
  if (tool === "cut") return t("screenshotEditor.hints.cut");
  const hint = TOOL_HINTS[tool] ?? TOOL_HINTS.default;
  return isWindows ? hint.replace("⌘↵", "Ctrl+Enter") : hint;
}

const PRIMARY_BUTTON =
  "rounded-full bg-flash-500 px-4 py-1.5 text-[13px] font-semibold text-white hover:bg-flash-600 disabled:opacity-50";

const SECONDARY_BUTTON =
  "rounded-full border border-strong px-4 py-1.5 text-[13px] font-semibold text-ink-text hover:bg-row disabled:opacity-50";

function Centered({ children }: { children: ReactNode }) {
  return (
    <main className="flex h-screen flex-col items-center justify-center gap-3 bg-canvas p-6">
      {children}
    </main>
  );
}

function Spinner() {
  const t = useDesktopT();
  return (
    <span
      aria-label={t("screenshotEditor.loading")}
      className="h-6 w-6 animate-spin rounded-full border-2 border-strong border-t-flash-500"
    />
  );
}

function Divider() {
  return <span className="mx-1 h-6 w-px bg-subtle" aria-hidden />;
}

function ToolbarButton({
  label,
  title,
  pressed,
  disabled,
  onClick,
  children,
}: {
  label: string;
  title: string;
  pressed?: boolean;
  disabled?: boolean;
  onClick: () => void;
  children: ReactNode;
}) {
  return (
    <button
      type="button"
      disabled={disabled}
      onClick={onClick}
      title={title}
      aria-label={label}
      aria-pressed={pressed}
      className={`inline-flex h-8 w-8 items-center justify-center rounded-md transition-colors disabled:opacity-40 ${
        pressed ? "bg-flash-50 text-flash-600" : "text-ink-secondary hover:bg-row hover:text-ink-text"
      }`}
    >
      {children}
    </button>
  );
}

function Glyph({ d, children }: { d?: string; children?: ReactNode }) {
  return (
    <svg
      width={18}
      height={18}
      viewBox="0 0 24 24"
      fill="none"
      stroke="currentColor"
      strokeWidth={1.9}
      strokeLinecap="round"
      strokeLinejoin="round"
      aria-hidden
    >
      {d && <path d={d} />}
      {children}
    </svg>
  );
}

function ToolIcon({ tool }: { tool: EditorTool }) {
  switch (tool) {
    case "select":
      return <Glyph d="M5 3l14 7-6 2-2 6z" />;
    case "arrow":
      return <Glyph d="M7 17 17 7M8 7h9v9" />;
    case "ellipse":
      return (
        <Glyph>
          <circle cx="12" cy="12" r="8" />
        </Glyph>
      );
    case "rect":
      return (
        <Glyph>
          <rect x="4" y="4" width="16" height="16" rx="2" />
        </Glyph>
      );
    case "text":
      return <Glyph d="M5 6V4h14v2M12 4v16M9 20h6" />;
    case "step":
      return (
        <Glyph>
          <circle cx="12" cy="12" r="8" />
          <path d="M11 9l1.5-1V16" />
        </Glyph>
      );
    case "pen":
      return <Glyph d="M4 20l4-1 11-11-3-3L5 16zM14 6l3 3" />;
    case "highlight":
      return <Glyph d="M9 11l-5 5v3h6l5-5M9 11l6-6 5 5-6 6zM4 21h16" />;
    case "line":
      return <Glyph d="M5 19 19 5" />;
    case "blur":
      return (
        <Glyph>
          <rect x="4" y="4" width="16" height="16" rx="2" />
          <path d="M4 10h16M4 16h16M10 4v16M16 4v16" />
        </Glyph>
      );
    case "crop":
      return <Glyph d="M6 2v14a2 2 0 0 0 2 2h14M18 22V8a2 2 0 0 0-2-2H2" />;
    case "path":
      return <Glyph d="M4 18 9 7l6 9 5-11M4 18h.01M9 7h.01M15 16h.01M20 5h.01" />;
    case "callout":
      return <Glyph d="M4 5h16v11H10l-4 4v-4H4z" />;
    case "emoji":
      return (
        <Glyph>
          <circle cx="12" cy="12" r="9" />
          <path d="M8.5 14.5a4.5 4.5 0 0 0 7 0M9 9.5h.01M15 9.5h.01" />
        </Glyph>
      );
    case "spotlight":
      return (
        <Glyph>
          <rect x="3" y="3" width="18" height="18" rx="2" strokeDasharray="2 3" />
          <circle cx="12" cy="12" r="4.5" />
        </Glyph>
      );
    case "magnify":
      return (
        <Glyph>
          <circle cx="10.5" cy="10.5" r="6.5" />
          <path d="m20 20-4.8-4.8M10.5 8v5M8 10.5h5" />
        </Glyph>
      );
    case "cut":
      return (
        <Glyph>
          <circle cx="6" cy="6" r="3" />
          <circle cx="6" cy="18" r="3" />
          <path d="M20 4 8.1 15.9M14.5 14.5 20 20M8.1 8.1 12 12" />
        </Glyph>
      );
    case "picker":
      return <Glyph d="m14 7 3 3M4 20l1-4 9.5-9.5a2.1 2.1 0 0 1 3 3L8 19zM17.5 3.5a2.1 2.1 0 0 1 3 3l-2 2-3-3z" />;
  }
}
