import { listen } from "@tauri-apps/api/event";
import { invoke } from "@tauri-apps/api/core";
import { ANNOTATION_COLORS, ANNOTATION_WIDTH_MAX, ANNOTATION_WIDTH_MIN, clearAnnotations, getAnnotationColor, getAnnotationWidth, setAnnotationColor, setAnnotationWidth, type AnnotationColor } from "../lib/annotation";
import { useCallback, useEffect, useLayoutEffect, useMemo, useRef, useState, type KeyboardEvent as ReactKeyboardEvent, type PointerEvent as ReactPointerEvent } from "react";
import { ask } from "@tauri-apps/plugin-dialog";
import { useTranslations } from "use-intl";
import { LazyStore } from "@tauri-apps/plugin-store";
import { isRegistered } from "@tauri-apps/plugin-global-shortcut";
import { isWindows } from "../lib/platform";
import { LOCAL_EDITION } from "../lib/edition";
import {
  cancelRecording,
  getAppSettings,
  getCursorTrailAvailable,
  getCursorTrailEnabled,
  getRecordingState,
  getRecordingSettings,
  getWebcamBubbleState,
  onCountdownTick,
  onMicSwitched,
  onRecordingStateChanged,
  pauseRecording,
  resumeRecording,
  setMicMuted,
  setCursorTrailEnabled,
  setRecordingControlsSize,
  setRecordingControlsHint,
  stopRecording,
  toggleRecordingWebcam,
  toggleNotesWindow,
  notesWindowVisible,
  type RecordingStateDto,
} from "../lib/bridge";
import { formatHotkey } from "../lib/hotkey";

function isPrimaryPointer(event: ReactPointerEvent<HTMLButtonElement> | ReactKeyboardEvent<HTMLButtonElement>): boolean {
  return "key" in event ? isActivationKey(event) && !event.repeat : event.button === 0;
}

function isActivationKey(event: ReactKeyboardEvent<HTMLButtonElement>): boolean {
  return event.key === "Enter" || event.key === " ";
}

function isRecordingMediaPhase(phase: RecordingStateDto["phase"]): boolean {
  return phase === "recording" || phase === "paused";
}

// Same store file the Rust side uses for app settings.
const settingsStore = new LazyStore("settings.json");
const FLARE_HINT_SEEN_KEY = "annotation_hint_seen";

const TIP_DELAY_MS = 260;
const RESTORE_CLICK_GUARD_MS = 400;

type HudTip = { text: string; y: number };


export default function StopControls() {
  const t = useTranslations("desktop.stop");
  const [state, setState] = useState<RecordingStateDto | null>(null);
  const [localMode, setLocalMode] = useState(false);
  useEffect(() => {
    getRecordingSettings().then((settings) => setLocalMode(settings.recordingMode === "local")).catch(() => {});
  }, [state?.recording_id]);
  const [countdown, setCountdown] = useState<number | null>(null);
  const [busy, setBusy] = useState(false);
  const [annotationColor, setColor] = useState<AnnotationColor>("#ff4d29");
  const [actionShortcuts, setActionShortcuts] = useState<Record<string, string>>({});
  const [actionError, setActionError] = useState<string | null>(null);
  const shortcutHint = (action: string, text: string) => `${text}${actionShortcuts[action] ? ` · ${actionShortcuts[action]}` : ""}`;
  const [stopShortcut, setStopShortcut] = useState<string | null>(null);
  const dockRef = useRef<HTMLDivElement | null>(null);
  // Collapsed to a peek. Local state, not a stored setting: the pre-recording
  // preference is Settings -> Recording controls, and this is the in-the-moment
  // "get out of my way" that should not outlive the take. On this app the dock
  // is already excluded from the capture on both platforms, so collapsing is
  // about the user's own screen, never about what lands in the video.
  const [collapsed, setCollapsed] = useState(false);
  // Which screen edge the collapsed dock is notched against. Rust picks the
  // nearer one and moves the window flush to it; this only styles the tab.
  const [notchEdge, setNotchEdge] = useState<"left" | "right">("left");
  const peekRef = useRef<HTMLButtonElement | null>(null);
  const collapseButtonRef = useRef<HTMLButtonElement | null>(null);
  // Set only for keyboard toggles: the focused button unmounts on toggle, so
  // focus is handed to its counterpart instead of falling to <body>.
  const collapseFocusRef = useRef(false);
  // Restoring grows and re-centres the window, so whatever dock button lands
  // under the pointer would take the second half of a double-click on the
  // peek — Stop included. Pointerdowns that soon after a restore are dropped.
  const restoredAtRef = useRef(0);
  const [muted, setMuted] = useState(false);
  // False when no mic was opened for this take (picker mic off, or the device
  // failed). The button stays visible, off and disabled with the reason.
  const [micActive, setMicActive] = useState(false);
  // Mic supervisor readout — the HUD is the surface the user actually watches
  // mid-recording (native notifications may be unauthorized/suppressed).
  // A swap is transient ("Mic → AirPods", fades after 4s); a pause persists
  // until the exact device returns, because the mic stays muted the whole time
  // and a message that fades would leave the user believing they are recorded.
  const [micSwitch, setMicSwitch] = useState<string | null>(null);
  const [micPausedDevice, setMicPausedDevice] = useState<string | null>(null);
  // Webcam bubble on/off — mirrors the actual bubble window; instant mode
  // composites it into the captured pixels.
  const [cameraOn, setCameraOn] = useState(false);
  const [cameraOnly, setCameraOnly] = useState(false);
  // False during a window-only take: the bubble is another window and
  // cannot be in it. The button stays visible, disabled with the reason.
  const [cameraRecordable, setCameraRecordable] = useState(true);
  const [flareOn, setFlareOn] = useState(false);
  const [flareAvailable, setFlareAvailable] = useState(false);
  // Pen width (a wedge slider). Persisted through Rust so the overlay
  // and the next recording pick it up.
  const [penWidth, setPenWidth] = useState(6);
  // The sticky note is its own capture-excluded window; the dock only
  // toggles it.
  const [notesOpen, setNotesOpen] = useState(false);
  useEffect(() => {
    notesWindowVisible().then(setNotesOpen).catch(() => {});
  }, []);

  const wedgeRef = useRef<HTMLDivElement | null>(null);
  const wedgeDragRef = useRef(false);
  const widthFromPointer = (clientY: number): number => {
    const track = wedgeRef.current;
    if (!track) return penWidth;
    const rect = track.getBoundingClientRect();
    const t = Math.max(0, Math.min(1, (clientY - rect.top) / Math.max(1, rect.height)));
    return Math.round(ANNOTATION_WIDTH_MIN + (1 - t) * (ANNOTATION_WIDTH_MAX - ANNOTATION_WIDTH_MIN));
  };
  const applyPenWidth = (next: number) => {
    setPenWidth(next);
    void setAnnotationWidth(next).catch((error) => setActionError(String(error)));
  };
  const busyRef = useRef(false);
  const micSwitchTimerRef = useRef<number | null>(null);

  // Native title bubbles are unreliable on an unfocused overlay.
  const [tip, setTip] = useState<HudTip | null>(null);
  const tipTimerRef = useRef<number | null>(null);
  const clearTipTimer = () => {
    if (tipTimerRef.current !== null) {
      window.clearTimeout(tipTimerRef.current);
      tipTimerRef.current = null;
    }
  };
  const hideTip = useCallback(() => {
    clearTipTimer();
    setTip(null);
  }, []);
  const tipProps = (text: string) => ({
    onPointerEnter: (event: ReactPointerEvent<HTMLElement>) => {
      const rect = event.currentTarget.getBoundingClientRect();
      const y = rect.top + rect.height / 2;
      clearTipTimer();
      tipTimerRef.current = window.setTimeout(() => {
        tipTimerRef.current = null;
        setTip({ text, y });
      }, TIP_DELAY_MS);
    },
    onPointerLeave: hideTip,
    onFocus: (event: React.FocusEvent<HTMLElement>) => {
      const rect = event.currentTarget.getBoundingClientRect();
      setTip({ text, y: rect.top + rect.height / 2 });
    },
    onBlur: hideTip,
  });

  // One-time annotation coach-mark on the user's first recording.
  const [coach, setCoach] = useState<{ y: number } | null>(null);
  const coachShownRef = useRef(false);
  const coachTimerRef = useRef<number | null>(null);
  const flareButtonRef = useRef<HTMLButtonElement | null>(null);
  const dismissCoach = useCallback((persist: boolean) => {
    if (coachTimerRef.current !== null) {
      window.clearTimeout(coachTimerRef.current);
      coachTimerRef.current = null;
    }
    setCoach(null);
    if (persist) {
      settingsStore
        .set(FLARE_HINT_SEEN_KEY, true)
        .then(() => settingsStore.save())
        .catch(() => {});
    }
  }, []);

  const applyRecordingState = useCallback((next: RecordingStateDto) => {
    setState(next);
    if (
      next.phase !== "countdown" &&
      next.phase !== "recording" &&
      next.phase !== "paused"
    ) {
      busyRef.current = false;
      setBusy(false);
      setMuted(false);
      setMicActive(false);
      setMicSwitch(null);
      setCameraOn(false);
      setCameraOnly(false);
      setCameraRecordable(true);
      setFlareOn(false);
      setFlareAvailable(false);
    } else if (next.phase === "recording") {
      setMuted(next.mic_muted);
      setMicActive(next.mic_active);
      getWebcamBubbleState().then((camera) => {
        setCameraOn(camera.active);
        setCameraOnly(camera.cameraOnly);
        setCameraRecordable(camera.recordable !== false);
      }).catch(() => {});
      getCursorTrailEnabled().then(setFlareOn).catch(() => {});
      getCursorTrailAvailable().then(setFlareAvailable).catch(() => {});
    } else {
      setMuted(next.mic_muted);
      setMicActive(next.mic_active);
      getWebcamBubbleState().then((camera) => {
        setCameraOn(camera.active);
        setCameraOnly(camera.cameraOnly);
        setCameraRecordable(camera.recordable !== false);
      }).catch(() => {});
      getCursorTrailEnabled().then(setFlareOn).catch(() => {});
      getCursorTrailAvailable().then(setFlareAvailable).catch(() => {});
    }
    if (next.phase !== "countdown") setCountdown(null);
  }, []);

  useEffect(() => {
    let unlistenState: (() => void) | undefined;
    let unlistenCountdown: (() => void) | undefined;
    let unlistenMic: (() => void) | undefined;
    getRecordingState().then(applyRecordingState).catch(() => {});
    onRecordingStateChanged(applyRecordingState).then((fn) => {
      unlistenState = fn;
    });
    onCountdownTick((value) => {
      setCountdown(value);
    }).then((fn) => {
      unlistenCountdown = fn;
    });
    onMicSwitched(({ device, paused }) => {
      if (micSwitchTimerRef.current !== null) {
        window.clearTimeout(micSwitchTimerRef.current);
        micSwitchTimerRef.current = null;
      }
      if (paused) {
        // No timer: this holds until the device comes back.
        setMicSwitch(null);
        setMicPausedDevice(device);
        return;
      }
      setMicPausedDevice(null);
      setMicSwitch(device);
      micSwitchTimerRef.current = window.setTimeout(() => {
        setMicSwitch(null);
        micSwitchTimerRef.current = null;
      }, 4000);
    }).then((fn) => {
      unlistenMic = fn;
    });
    return () => {
      unlistenState?.();
      unlistenCountdown?.();
      unlistenMic?.();
      if (micSwitchTimerRef.current !== null) {
        window.clearTimeout(micSwitchTimerRef.current);
      }
    };
  }, [applyRecordingState]);

  useEffect(() => {
    const phase = state?.phase ?? "idle";
    if (!isRecordingMediaPhase(phase)) return;
    let disposed = false;
    const poll = () => {
      getRecordingState()
        .then((next) => {
          if (!disposed) applyRecordingState(next);
        })
        .catch(() => {});
    };
    const id = window.setInterval(poll, 1000);
    poll();
    return () => {
      disposed = true;
      window.clearInterval(id);
    };
  }, [state?.phase, applyRecordingState]);

  const phase = state?.phase ?? "idle";
  const isCountdown = phase === "countdown";
  const isRecording = phase === "recording";
  const isPaused = phase === "paused";

  useEffect(() => {
    let disposed = false;
    getAppSettings().then(async ({ hotkey }) => {
      const registered = await isRegistered(hotkey);
      if (!disposed) setStopShortcut(registered ? formatHotkey(hotkey) : null);
    }).catch(() => {});
    return () => { disposed = true; };
  }, [phase]);

  useEffect(() => {
    let disposed = false;
    const off: (() => void)[] = [];
    const keep = (promise: Promise<() => void>) => void promise.then((fn) => disposed ? fn() : off.push(fn)).catch(() => {});
    keep(listen<boolean>("annotation-mode", ({ payload }) => { if (!disposed) setFlareOn(payload); }));
    keep(listen<AnnotationColor>("annotation-color", ({ payload }) => { if (!disposed) setColor(payload); }));
    keep(listen<string>("recorder-action-error", ({ payload }) => { if (!disposed) setActionError(payload); }));
    void getAnnotationColor().then((color) => { if (!disposed) setColor(color); }).catch(() => {});
    void getAnnotationWidth().then((width) => { if (!disposed) setPenWidth(width); }).catch(() => {});
    void invoke<{ action: string; hotkey: string; registered: boolean }[]>("get_recorder_shortcuts")
      .then((items) => { if (!disposed) setActionShortcuts(Object.fromEntries(items.filter((item) => item.registered).map((item) => [item.action, formatHotkey(item.hotkey)]))); }).catch(() => {});
    return () => { disposed = true; off.forEach((fn) => fn()); };
  }, [phase]);

  // This window is sized to the dock, so the document never has anything to
  // scroll. Any overflow is sub-pixel rounding between CSS and physical pixels.
  // macOS draws overlay scrollbars that take no space, so it never showed. On
  // Windows a ~17px classic scrollbar appears instead, squeezes the 56px dock
  // sideways, forces a horizontal scrollbar too, and cuts the dock short.
  useLayoutEffect(() => {
    const root = document.documentElement;
    const body = document.body;
    const previous = [root.style.overflow, body.style.overflow];
    root.style.overflow = "hidden";
    body.style.overflow = "hidden";
    return () => {
      root.style.overflow = previous[0];
      body.style.overflow = previous[1];
    };
  }, []);

  // Every take starts with the dock showing. A collapse is a decision about
  // the recording in front of you, so carrying it into the next one would
  // leave a new recording with no visible controls and no obvious reason why.
  useEffect(() => {
    setCollapsed(false);
  }, [state?.recording_id]);

  useEffect(() => {
    if (!collapseFocusRef.current) return;
    collapseFocusRef.current = false;
    (collapsed ? peekRef : collapseButtonRef).current?.focus();
  }, [collapsed]);

  // The pen can be turned on from its hotkey while collapsed. The overlay then
  // takes every click on the screen, so the dock has to come back with Done.
  useEffect(() => {
    if (flareOn) setCollapsed(false);
  }, [flareOn]);

  useLayoutEffect(() => {
    const update = () => {
      const rect = dockRef.current?.getBoundingClientRect();
      const height = rect?.height ?? 1;
      // The floor tracks what is actually rendered: the peek is slimmer than
      // any dock button, so clamping to the dock's width would leave a wide
      // transparent window swallowing clicks around it.
      const notched = collapsed && isRecordingMediaPhase(phase);
      const floor = notched ? 24 : 64;
      const width = Math.max(floor, Math.ceil(rect?.width ?? floor));
      void setRecordingControlsSize(height, width, notched)
        .then((edge) => {
          if (edge) setNotchEdge(edge);
        })
        .catch((error) => {
          console.warn("recording controls sizing failed", error);
        });
    };
    update();
    const observer = new ResizeObserver(update);
    if (dockRef.current) observer.observe(dockRef.current);
    window.addEventListener("resize", update);
    return () => {
      observer.disconnect();
      window.removeEventListener("resize", update);
    };
  }, [phase, flareAvailable, flareOn, collapsed]);

  useEffect(() => {
    if (phase !== "recording" || !flareAvailable || coachShownRef.current) return;
    let cancelled = false;
    settingsStore
      .get<boolean>(FLARE_HINT_SEEN_KEY)
      .then((seen) => {
        if (cancelled || seen === true || coachShownRef.current) return;
        coachShownRef.current = true;
        // Let the bar settle before pointing at it.
        window.setTimeout(() => {
          if (cancelled) return;
          const button = flareButtonRef.current;
          // Dock collapsed in the meantime: nothing to point at, so leave the
          // coach-mark for a later phase change rather than spending it here.
          if (!button) {
            coachShownRef.current = false;
            return;
          }
          const rect = button.getBoundingClientRect();
          setCoach({ y: rect.top + rect.height / 2 });
          coachTimerRef.current = window.setTimeout(() => dismissCoach(true), 14000);
        }, 900);
      })
      .catch(() => {});
    return () => {
      cancelled = true;
    };
  }, [phase, flareAvailable, dismissCoach]);

  useEffect(() => {
    if (!isRecordingMediaPhase(phase)) {
      hideTip();
      setCoach(null);
    }
  }, [phase, hideTip]);

  const micLevel = Math.max(0, Math.min(1, state?.mic_level ?? 0));
  const micWarning = state?.mic_silence_warning ?? null;
  const micOff = !micActive || muted;
  const elapsed = useMemo(
    () => formatElapsed(state?.elapsed_ms ?? null),
    [state?.elapsed_ms],
  );
  // The dock is a narrow column, so it carries the timer only. Longer
  // transient messages (mic swapped, no input) go to the wide strip beside
  // it, where the hover tooltips already live.
  const readout = isCountdown
    ? countdown === null
      ? "…"
      : String(countdown)
    : elapsed;
  const notice = isCountdown
    ? null
    : micPausedDevice !== null
      ? t("micPaused", { device: micPausedDevice })
      : micSwitch !== null
        ? t("micSwitched", { device: micSwitch })
        : micWarning !== null
          ? t("noMicInput")
          : null;

  useEffect(() => {
    const hint = !isCountdown && !isRecording && !isPaused ? null
      : tip ?? (actionError ? { text: actionError, y: window.innerHeight / 2 } : null) ?? (coach ? {
        text: t("coach"),
        y: coach.y,
      } : notice ? { text: notice, y: window.innerHeight / 2 } : null);
    void setRecordingControlsHint(hint).catch((error) => console.warn("recording hint failed", error));
  }, [tip, coach, notice, actionError, isCountdown, isRecording, isPaused, t]);

  const handleStopRequest = async () => {
    if (busyRef.current) return;
    busyRef.current = true;
    setBusy(true);
    try {
      if (isCountdown) {
        await cancelRecording();
      } else {
        await stopRecording();
      }
    } catch (err) {
      console.warn("stop overlay failed", err);
      if (!String(err).includes("no recording in progress")) {
        busyRef.current = false;
        setBusy(false);
      }
    }
  };

  const handleStopPointerDown = (event: ReactPointerEvent<HTMLButtonElement>) => {
    if (!isPrimaryPointer(event)) return;
    event.preventDefault();
    void handleStopRequest();
  };

  const handleStopKeyDown = (event: ReactKeyboardEvent<HTMLButtonElement>) => {
    if (!isActivationKey(event)) return;
    event.preventDefault();
    void handleStopRequest();
  };

  const discardCurrentRecording = async () => {
    if (busyRef.current) return;
    const confirmed = await ask(t("discardConfirm"), {
      title: "Clipy",
      kind: "warning",
      okLabel: t("discard"),
      cancelLabel: t("keepRecording"),
    });
    if (!confirmed) return;
    busyRef.current = true;
    setBusy(true);
    try {
      await cancelRecording();
    } catch (err) {
      console.warn("cancel recording failed", err);
      busyRef.current = false;
      setBusy(false);
    }
  };

  const handleDiscardPointerDown = (event: ReactPointerEvent<HTMLButtonElement>) => {
    if (!isPrimaryPointer(event)) return;
    event.preventDefault();
    void discardCurrentRecording();
  };

  const handleDiscardKeyDown = (event: ReactKeyboardEvent<HTMLButtonElement>) => {
    if (!isActivationKey(event)) return;
    event.preventDefault();
    void discardCurrentRecording();
  };

  const handleCameraPointerDown = async (event: ReactPointerEvent<HTMLButtonElement> | ReactKeyboardEvent<HTMLButtonElement>) => {
    if (!isPrimaryPointer(event)) return;
    event.preventDefault();
    if (!cameraRecordable) return;
    try {
      const next = await toggleRecordingWebcam();
      setCameraOn(next);
    } catch (err) {
      console.warn("camera toggle failed", err);
    }
  };

  const handleFlarePointerDown = async (event: ReactPointerEvent<HTMLButtonElement> | ReactKeyboardEvent<HTMLButtonElement>) => {
    if (!isPrimaryPointer(event)) return;
    event.preventDefault();
    dismissCoach(true);
    try {
      const next = await setCursorTrailEnabled(!flareOn);
      setFlareOn(next);
    } catch (err) {
      setActionError(String(err));
    }
  };

  const handlePause = async (e: ReactPointerEvent<HTMLButtonElement> | ReactKeyboardEvent<HTMLButtonElement>) => {
              if (!isPrimaryPointer(e)) return;
              e.preventDefault();
              try {
                if (isPaused) {
                  await resumeRecording();
                } else {
                  await pauseRecording();
                }
              } catch (err) {
                console.warn("pause/resume failed", err);
              }
            };

  const handleMic = async (e: ReactPointerEvent<HTMLButtonElement> | ReactKeyboardEvent<HTMLButtonElement>) => {
                if (!isPrimaryPointer(e)) return;
                e.preventDefault();
                if (!micActive) return;
                const next = !muted;
                try {
                  await setMicMuted(next);
                  setMuted(next);
                } catch (err) {
                  console.warn("mic mute failed", err);
                }
              };

  const handleCollapse = (next: boolean) => (event: ReactPointerEvent<HTMLButtonElement> | ReactKeyboardEvent<HTMLButtonElement>) => {
    if (!isPrimaryPointer(event)) return;
    event.preventDefault();
    if (next) {
      // The coach-mark points at the pen button, which is about to disappear.
      dismissCoach(false);
      // A live pen overlay turns every click on the screen into ink, and its
      // Done button leaves with the palette. Colour and width are persisted,
      // so turning the pen back on after restoring picks up where it was.
      if (flareOn) {
        setFlareOn(false);
        void setCursorTrailEnabled(false).then(setFlareOn).catch((error) => setActionError(String(error)));
      }
    } else {
      restoredAtRef.current = performance.now();
    }
    collapseFocusRef.current = "key" in event;
    setCollapsed(next);
  };

  if (!isCountdown && !isRecording && !isPaused) {
    return <main className="h-full w-full bg-transparent" />;
  }

  const dockButton = (active = false) =>
    `hud-btn inline-flex h-11 w-11 shrink-0 items-center justify-center rounded-xl transition ${active ? "hud-btn-active" : ""}`;

  return (
    <main
      onPointerDownCapture={(event) => {
        if (performance.now() - restoredAtRef.current < RESTORE_CLICK_GUARD_MS) {
          event.preventDefault();
          event.stopPropagation();
          return;
        }
        hideTip();
        setActionError(null);
      }}
      className={`flex w-full items-start gap-2 overflow-hidden bg-transparent ${collapsed && notchEdge === "right" ? "justify-end" : "justify-start"}`}
    >
      <div ref={dockRef} className="flex items-start gap-2">
      {collapsed ? (
        /* What is left when the dock is collapsed: a notch flush against the
           screen edge, like the extension's rail. Small enough to ignore,
           reachable enough to stop the take: hiding the controls must never
           strand a live recording behind a hotkey the user has to remember. */
        <div className={`flex h-[52px] w-6 shrink-0 ${notchEdge === "right" ? "justify-end" : "justify-start"}`}>
          <button
            type="button"
            ref={peekRef}
            onPointerDown={handleCollapse(false)}
            onKeyDown={handleCollapse(false)}
            aria-label={isPaused ? t("showControlsPaused") : t("showControls")}
            {...tipProps(isPaused ? t("showControlsPausedTip") : t("showControlsTip"))}
            className={`hud-peek hud-peek-${notchEdge} inline-flex h-full shrink-0 items-center justify-center text-white`}
          >
            <span aria-hidden className={`hud-peek-dot ${isPaused ? "hud-peek-dot-paused" : ""}`} />
          </button>
        </div>
      ) : (
      <section
        data-tauri-drag-region
        className="hud-dock flex w-[68px] shrink-0 flex-col items-center gap-2 overflow-hidden rounded-[34px] pb-4 pt-3 text-white"
      >
        {/* Stop leads: the action people reach for gets the shortest travel.
            All dock actions fire on pointerdown: the window is always-on-top
            and unfocused, and waiting for a full click risks the
            mousedown/mouseup pair being split by macOS focus handling. */}
        <button
          type="button"
          onPointerDown={handleStopPointerDown}
          onKeyDown={handleStopKeyDown}
          disabled={busy}
          aria-label={isCountdown ? t("cancelRecording") : t("stopRecording")}
          {...tipProps(
            isCountdown ? t("cancelCountdown") : `${LOCAL_EDITION || localMode ? t("stopLocal") : t("stopShare")}${stopShortcut ? ` · ${stopShortcut}` : ""}`,
          )}
          className="hud-stop inline-flex h-12 w-12 shrink-0 items-center justify-center rounded-[14px] transition disabled:opacity-70"
        >
          <span className={busy ? "recording-controls-spinner" : "hud-stop-square"} aria-hidden />
        </button>
        <p
          data-tauri-drag-region
          className="hud-time w-full truncate px-1 text-center text-[14px] font-semibold leading-[1.15] tabular-nums"
        >
          {readout}
        </p>

        {!isCountdown && (
          <button
            type="button"
            onPointerDown={handlePause}
            onKeyDown={handlePause}
            aria-label={isPaused ? t("resume") : t("pause")}
            {...tipProps(shortcutHint("pause", isPaused ? t("resume") : t("pause")))}
            aria-pressed={isPaused}
            className="hud-pause inline-flex h-12 w-12 shrink-0 items-center justify-center rounded-[14px] transition"
          >
            {isPaused ? (
              <svg width="20" height="20" viewBox="0 0 12 12" aria-hidden>
                <path d="M2.5 1.5l8 4.5-8 4.5z" fill="currentColor" />
              </svg>
            ) : (
              <svg width="20" height="20" viewBox="0 0 12 12" aria-hidden>
                <rect x="2" y="1.5" width="3" height="9" rx="1" fill="currentColor" />
                <rect x="7" y="1.5" width="3" height="9" rx="1" fill="currentColor" />
              </svg>
            )}
          </button>
        )}

        <button
          type="button"
          onPointerDown={handleDiscardPointerDown}
          onKeyDown={handleDiscardKeyDown}
          disabled={busy}
          aria-label={isCountdown ? t("cancelRecording") : t("discardRecording")}
          {...tipProps(isCountdown ? t("cancelNothingSaved") : t("discardNothingSaved"))}
          className={`${dockButton()} disabled:opacity-60`}
        >
          <svg width="18" height="18" viewBox="0 0 16 16" aria-hidden>
            <path d="M3.5 4.5h9M6.2 4.5V3.2h3.6v1.3M5.1 6.2l.4 6.1c.1.8.5 1.2 1.3 1.2h2.4c.8 0 1.2-.4 1.3-1.2l.4-6.1" fill="none" stroke="currentColor" strokeWidth="1.35" strokeLinecap="round" strokeLinejoin="round" />
          </svg>
        </button>

        {!isCountdown && <span className="hud-sep" aria-hidden />}

        {!isCountdown && (
          <button
            type="button"
            onPointerDown={(e) => { if (!isPrimaryPointer(e)) return; e.preventDefault(); void toggleNotesWindow().then(setNotesOpen).catch(() => {}); }}
            aria-label={notesOpen ? t("hideNotes") : t("showNotes")}
            aria-pressed={notesOpen}
            {...tipProps(notesOpen ? t("hideNotesTip") : t("showNotesTip"))}
            className={dockButton(notesOpen)}
          >
            <svg width="18" height="18" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="1.8" strokeLinecap="round" strokeLinejoin="round" aria-hidden>
              <path d="M6 3h9l5 5v13H6z" /><path d="M15 3v5h5" /><path d="M9 12h6" /><path d="M9 16h6" />
            </svg>
          </button>
        )}

        {!isCountdown && flareAvailable && (
          <button
            type="button"
            ref={flareButtonRef}
            onPointerDown={handleFlarePointerDown}
            onKeyDown={handleFlarePointerDown}
            aria-label={flareOn ? t("stopAnnotating") : t("annotate")}
            {...tipProps(shortcutHint("annotation", flareOn ? t("stopAnnotatingTip") : t("annotateTip")))}
            aria-pressed={flareOn}
            className={dockButton(flareOn)}
          >
            <svg width="18" height="18" viewBox="0 0 16 16" fill="none" stroke="currentColor" strokeWidth="1.6" aria-hidden>
              <path d="m10.8 2.2 3 3-8.5 8.5-3.8.8.8-3.8zM9.2 3.8l3 3" strokeLinejoin="round" />
            </svg>
          </button>
        )}

        {/* Windows camera-only: the camera is the recording, so there is nothing to turn off. */}
        {!isCountdown && !(isWindows && cameraOnly) && (
          <button
            type="button"
            onPointerDown={handleCameraPointerDown}
            onKeyDown={handleCameraPointerDown}
            aria-label={
              !cameraRecordable
                ? t("cameraNotRecorded")
                : cameraOnly ? (cameraOn ? t("hidePreview") : t("showPreview")) : cameraOn ? t("turnCameraOff") : t("turnCameraOn")
            }
            // aria-disabled rather than disabled, so the tooltip with the
            // reason still shows on hover.
            aria-disabled={!cameraRecordable || undefined}
            {...tipProps(
              !cameraRecordable
                ? t("cameraNotRecordedTip")
                : shortcutHint("camera",
                    cameraOnly
                      ? cameraOn ? t("hidePreviewTip") : t("showPreview")
                      : cameraOn ? t("hideCameraTip") : t("showCameraTip"),
                  ),
            )}
            aria-pressed={cameraOn}
            className={`${dockButton(!cameraOn)}${cameraRecordable ? "" : " cursor-default opacity-40"}`}
          >
            <svg width="18" height="18" viewBox="0 0 16 16" aria-hidden>
              <rect x="1.5" y="4" width="9" height="8" rx="1.5" fill="none" stroke="currentColor" strokeWidth="1.4" />
              <path d="M10.5 7l4-2.2v6.4l-4-2.2" fill="none" stroke="currentColor" strokeWidth="1.4" strokeLinejoin="round" />
              {!cameraOn && <line x1="1.5" y1="14.5" x2="14.5" y2="1.5" stroke="currentColor" strokeWidth="1.6" strokeLinecap="round" />}
            </svg>
          </button>
        )}

        {!isCountdown && (
          <div className="flex shrink-0 flex-col items-center">
            <button
              type="button"
              onPointerDown={handleMic}
              onKeyDown={handleMic}
              aria-label={
                !micActive
                  ? t("noMic")
                  : muted ? t("unmute") : t("mute")
              }
              aria-disabled={!micActive || undefined}
              {...tipProps(
                !micActive
                  ? t("noMicTip")
                  : shortcutHint("mic", muted ? t("unmuteTip") : t("muteTip")),
              )}
              aria-pressed={micActive ? muted : undefined}
              className={`${dockButton(micOff)}${micActive ? "" : " cursor-default opacity-40"}`}
            >
              <svg width="18" height="18" viewBox="0 0 16 16" aria-hidden>
                <path d="M8 1.5a2 2 0 012 2v4a2 2 0 11-4 0v-4a2 2 0 012-2z" fill="currentColor" />
                <path d="M3.5 7.5a4.5 4.5 0 009 0M8 12v2.5" stroke="currentColor" strokeWidth="1.4" strokeLinecap="round" fill="none" />
                {micOff && <line x1="2" y1="14" x2="14" y2="2" stroke="currentColor" strokeWidth="1.6" strokeLinecap="round" />}
              </svg>
            </button>
            <div aria-hidden className="flex h-3 w-6 items-end justify-center gap-0.5">
              {[0.18, 0.38, 0.62].map((threshold) => (
                <span
                  key={threshold}
                  className={`w-1 rounded-full transition-all duration-100 ${
                    micOff || micLevel < threshold ? (micWarning ? "bg-flash-500/35" : "bg-white/20") : "bg-flash-400"
                  }`}
                  style={{ height: `${Math.max(3, Math.round(3 + micLevel * 14 * threshold))}px` }}
                />
              ))}
            </div>
          </div>
        )}

        {/* Collapse last: it is the least-reached control, and putting it at
            the foot keeps Stop's travel short. Hidden during the countdown,
            where there is nothing yet to get out of the way of. */}
        {!isCountdown && (
          <button
            type="button"
            ref={collapseButtonRef}
            onPointerDown={handleCollapse(true)}
            onKeyDown={handleCollapse(true)}
            aria-label={t("hideControls")}
            {...tipProps(t("hideControlsTip"))}
            className={dockButton()}
          >
            <svg width="18" height="18" viewBox="0 0 16 16" aria-hidden>
              <path
                d="M7 4L3.5 8 7 12M12 4L8.5 8 12 12"
                stroke="currentColor"
                strokeWidth="1.6"
                strokeLinecap="round"
                strokeLinejoin="round"
                fill="none"
              />
            </svg>
          </button>
        )}
      </section>
      )}

      {/* Pen palette: a flyout beside the rail with colours, a width
          wedge, Done. Only while annotating, and it goes away with the dock:
          collapsing means "get off my screen", and a palette floating beside
          the peek would be the one piece left contradicting that. Collapsing
          turns the pen off, and turning it on restores the dock. */}
      {flareOn && !isCountdown && !collapsed && (
        <section className="hud-palette flex w-[104px] shrink-0 flex-col items-center gap-2 rounded-[26px] px-2 pb-2 pt-3 text-white" aria-label={t("pen")}>
          <div className="flex w-full items-stretch justify-center gap-2">
            <div className="flex flex-col items-center gap-2 py-1" role="radiogroup" aria-label={t("penColor")}>
              {ANNOTATION_COLORS.map(({ name, value }) => (
                <button
                  key={value}
                  type="button"
                  role="radio"
                  aria-checked={annotationColor === value}
                  aria-label={t("colorPen", { color: t(`colors.${name.toLowerCase() as Lowercase<typeof name>}`) })}
                  onClick={() => void setAnnotationColor(value).then(() => setColor(value)).catch((error) => setActionError(String(error)))}
                  className={`hud-swatch h-7 w-7 rounded-full ${annotationColor === value ? "hud-swatch-active" : ""}`}
                  style={{ backgroundColor: value }}
                />
              ))}
            </div>
            <div
              ref={wedgeRef}
              role="slider"
              aria-label={t("penWidth")}
              aria-orientation="vertical"
              aria-valuemin={ANNOTATION_WIDTH_MIN}
              aria-valuemax={ANNOTATION_WIDTH_MAX}
              aria-valuenow={penWidth}
              tabIndex={0}
              className="hud-wedge-track relative w-9 cursor-pointer touch-none select-none"
              onPointerDown={(e) => {
                wedgeDragRef.current = true;
                e.currentTarget.setPointerCapture(e.pointerId);
                applyPenWidth(widthFromPointer(e.clientY));
              }}
              onPointerMove={(e) => { if (wedgeDragRef.current) applyPenWidth(widthFromPointer(e.clientY)); }}
              onPointerUp={() => { wedgeDragRef.current = false; }}
              onKeyDown={(e) => {
                if (e.key === "ArrowUp" || e.key === "Home") applyPenWidth(Math.min(ANNOTATION_WIDTH_MAX, e.key === "Home" ? ANNOTATION_WIDTH_MAX : penWidth + 1));
                if (e.key === "ArrowDown" || e.key === "End") applyPenWidth(Math.max(ANNOTATION_WIDTH_MIN, e.key === "End" ? ANNOTATION_WIDTH_MIN : penWidth - 1));
              }}
            >
              <div className="hud-wedge" aria-hidden />
              <div className="hud-wedge-knob" style={{ top: `${Math.round((1 - (penWidth - ANNOTATION_WIDTH_MIN) / (ANNOTATION_WIDTH_MAX - ANNOTATION_WIDTH_MIN)) * 100)}%` }} aria-hidden />
            </div>
          </div>
          <button
            type="button"
            onClick={() => void clearAnnotations().catch((error) => setActionError(String(error)))}
            aria-label={t("clearAnnotations")}
            className="hud-palette-link"
          >
            {t("clear")}
          </button>
          <button
            type="button"
            onPointerDown={handleFlarePointerDown}
            onKeyDown={handleFlarePointerDown}
            className="hud-done"
          >
            {t("done")}
          </button>
        </section>
      )}
      </div>
    </main>
  );
}

export function formatElapsed(ms: number | null): string {
  const total = Math.max(0, Math.floor((ms ?? 0) / 1000));
  const hours = Math.floor(total / 3600);
  const minutes = Math.floor((total % 3600) / 60);
  const seconds = total % 60;
  if (hours > 0) {
    return `${hours.toString().padStart(2, "0")}:${minutes
      .toString()
      .padStart(2, "0")}:${seconds.toString().padStart(2, "0")}`;
  }
  return `${minutes.toString().padStart(2, "0")}:${seconds
    .toString()
    .padStart(2, "0")}`;
}
