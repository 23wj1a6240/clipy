//! The pointer outside a recording: Flare for screen sharing.
//!
//! While Clipy is running and nothing is being recorded, holding Control+Shift
//! and moving the mouse paints the same fading Flare trail a recording does,
//! so people watching a screen shared in Meet, Slack or Zoom can see what is
//! being pointed at. "Show Pointer" in the tray menu keeps it on without
//! holding keys until it is chosen again or Escape is pressed.
//!
//! Control+Shift and not bare Control, because bare Control is part of too
//! many everyday shortcuts to paint on whenever Clipy is open. During a
//! recording the recording's own Flare thread (`commands.rs`) owns the
//! overlay: bare Control works there as before, and Control+Shift does too,
//! because it is Control held.
//!
//! Rules this module keeps:
//! - It never touches the overlay while a recording owns it
//!   (`commands::recording_owns_cursor_trail`) or while a recording is
//!   counting down, running or paused, which covers window-only takes that
//!   have no overlay at all.
//! - A hold that becomes a shortcut (another key pressed, like Ctrl+Shift+T)
//!   stops painting until the keys are released, and a short tap never paints
//!   (`HOLD_GRACE_MS`).
//! - The overlay stays click-through and is shown on the display under the
//!   pointer; it moves when the pointer moves to another display.

use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use tauri::{AppHandle, Emitter, Manager, Runtime};

use crate::types::{RecordingPhase, SourceHighlightRectDto};

/// Sent with the new value when "Show Pointer" turns on or off, so the tray
/// menu can show its check mark.
pub const EVENT_POINTER_MODE_CHANGED: &str = "pointer-mode-changed";

/// A Control+Shift press shorter than this is a tap or the start of a
/// shortcut, not pointing. Longer than a recording Flare's 120 ms: outside a
/// recording Ctrl+Shift+V, Ctrl+Shift+T and friends are what most holds turn
/// into, and a flash of trail before every one of them would be noise.
pub const HOLD_GRACE_MS: u64 = 250;
/// How long the overlay stays up after the last stroke: the trail fades out
/// after 2 s plus 0.4 s (`FLARE_IDLE_MS` + `FLARE_FADE_MS` in
/// `src/lib/flareTrail.ts`), with a little margin.
const HIDE_AFTER: Duration = Duration::from_millis(2_700);
const POLL_ACTIVE: Duration = Duration::from_millis(16);
const POLL_IDLE: Duration = Duration::from_millis(40);

static POINTER_MODE: AtomicBool = AtomicBool::new(false);
static STARTED: AtomicBool = AtomicBool::new(false);

/// "Show Pointer" is on.
pub fn pointer_mode_on() -> bool {
    POINTER_MODE.load(Ordering::SeqCst)
}

pub fn set_pointer_mode<R: Runtime>(app: &AppHandle<R>, on: bool) {
    if POINTER_MODE.swap(on, Ordering::SeqCst) != on {
        let _ = app.emit(EVENT_POINTER_MODE_CHANGED, on);
    }
}

pub fn toggle_pointer_mode<R: Runtime>(app: &AppHandle<R>) {
    let on = !pointer_mode_on();
    if on && recording_blocks_pointer(app) {
        return;
    }
    set_pointer_mode(app, on);
}

/// Starts the watcher thread. Safe to call once at launch.
pub fn init<R: Runtime>(app: &AppHandle<R>) {
    if STARTED.swap(true, Ordering::SeqCst) {
        return;
    }
    let app = app.clone();
    if let Err(err) = std::thread::Builder::new()
        .name("clipy-pointer".to_string())
        .spawn(move || run(app))
    {
        tracing::warn!("pointer watcher did not start: {err}");
    }
}

/// A recording is counting down, running or paused, or holds the overlay.
fn recording_blocks_pointer<R: Runtime>(app: &AppHandle<R>) -> bool {
    if crate::commands::recording_owns_cursor_trail() {
        return true;
    }
    app.try_state::<crate::recording::RecordingController>()
        .is_some_and(|rec| phase_blocks_pointer(rec.dto().phase))
}

fn phase_blocks_pointer(phase: RecordingPhase) -> bool {
    matches!(
        phase,
        RecordingPhase::Countdown | RecordingPhase::Recording | RecordingPhase::Paused
    )
}

/// Turns Control+Shift readings into "painting or not".
#[derive(Clone, Copy, Debug, Default)]
struct HoldTracker {
    held_since_ms: Option<u64>,
    chorded: bool,
}

impl HoldTracker {
    fn update(&mut self, keys: crate::cursor_track::PointerKeys, now_ms: u64) -> bool {
        if !keys.control_shift {
            *self = Self::default();
            return false;
        }
        let since = *self.held_since_ms.get_or_insert(now_ms);
        if keys.other_key {
            self.chorded = true;
        }
        !self.chorded && now_ms.saturating_sub(since) >= HOLD_GRACE_MS
    }

    fn reset(&mut self) {
        *self = Self::default();
    }
}

/// A display in cursor units: logical points on macOS (where the cursor is
/// read in points), physical pixels on Windows (where it is read in pixels).
fn display_rects<R: Runtime>(app: &AppHandle<R>) -> Vec<SourceHighlightRectDto> {
    let Some(window) = app.get_webview_window("cursor-trail") else {
        return Vec::new();
    };
    let Ok(monitors) = window.available_monitors() else {
        return Vec::new();
    };
    monitors
        .iter()
        .map(|monitor| {
            let position = monitor.position();
            let size = monitor.size();
            #[cfg(target_os = "macos")]
            let scale = monitor.scale_factor().max(f64::MIN_POSITIVE);
            #[cfg(not(target_os = "macos"))]
            let scale = 1.0;
            SourceHighlightRectDto {
                x: position.x as f64 / scale,
                y: position.y as f64 / scale,
                width: size.width as f64 / scale,
                height: size.height as f64 / scale,
            }
        })
        .collect()
}

fn contains(rect: &SourceHighlightRectDto, x: f64, y: f64) -> bool {
    x >= rect.x && x < rect.x + rect.width && y >= rect.y && y < rect.y + rect.height
}

fn display_at(
    displays: &[SourceHighlightRectDto],
    x: f64,
    y: f64,
) -> Option<SourceHighlightRectDto> {
    displays.iter().copied().find(|rect| contains(rect, x, y))
}

fn run<R: Runtime>(app: AppHandle<R>) {
    let started = Instant::now();
    let mut tracker = HoldTracker::default();
    let mut placement: Option<crate::commands::IdleTrailPlacement> = None;
    let mut displays: Vec<SourceHighlightRectDto> = Vec::new();
    let mut was_painting = false;
    let mut last_paint: Option<Instant> = None;
    // After a failed show, wait before asking the main thread again rather
    // than retrying on every poll.
    let mut retry_at: Option<Instant> = None;
    #[cfg(target_os = "windows")]
    let mut raise_ticks: u32 = 0;

    loop {
        std::thread::sleep(if placement.is_some() || was_painting {
            POLL_ACTIVE
        } else {
            POLL_IDLE
        });

        if recording_blocks_pointer(&app) {
            // The recording has the overlay (or a take with none is running).
            // Forget ours without touching the window, and stop "Show Pointer"
            // so it does not come back on by itself after the take.
            set_pointer_mode(&app, false);
            tracker.reset();
            placement = None;
            was_painting = false;
            last_paint = None;
            continue;
        }

        if pointer_mode_on() && crate::cursor_track::current_escape_key_active() {
            set_pointer_mode(&app, false);
        }

        let now_ms = started.elapsed().as_millis() as u64;
        let keys = crate::cursor_track::current_pointer_keys();
        let holding = tracker.update(keys, now_ms);
        let painting = holding || pointer_mode_on();
        if was_painting && !painting && keys.control_shift && placement.is_some() {
            // The hold turned into a shortcut: take back what it drew
            // instead of leaving it to fade over whatever the shortcut opens.
            let _ = app.emit_to("cursor-trail", "annotation-clear", ());
        }

        // Recording show or teardown replaced or hid our window.
        if placement.is_some_and(|p| p.generation != crate::commands::cursor_trail_generation()) {
            placement = None;
        }

        if painting {
            let Some((cx, cy)) = crate::cursor_track::current_cursor_position() else {
                was_painting = false;
                continue;
            };
            let (x, y) = (cx as f64, cy as f64);
            let on_current = placement.is_some_and(|p| contains(&p.rect, x, y));
            if !on_current && retry_at.is_none_or(|at| Instant::now() >= at) {
                if display_at(&displays, x, y).is_none() {
                    displays = display_rects(&app);
                }
                match display_at(&displays, x, y) {
                    Some(rect) => match crate::commands::show_idle_cursor_trail(&app, rect) {
                        Ok(placed) => {
                            placement = Some(placed);
                            retry_at = None;
                            // A new display starts a new stroke.
                            was_painting = false;
                        }
                        Err(err) => {
                            tracing::debug!("pointer overlay not shown: {err}");
                            placement = None;
                            retry_at = Some(Instant::now() + Duration::from_secs(1));
                        }
                    },
                    None => placement = None,
                }
            }
            if let Some(p) = placement {
                if !was_painting {
                    crate::whats_new::mark_tried(&app, "pointer");
                }
                #[cfg(target_os = "windows")]
                {
                    if !was_painting || raise_ticks % 16 == 0 {
                        crate::commands::raise_idle_cursor_trail(&app);
                    }
                    raise_ticks = raise_ticks.wrapping_add(1);
                }
                let _ = app.emit_to(
                    "cursor-trail",
                    "cursor-trail-point",
                    serde_json::json!({
                        "x": (x - p.rect.x) / p.point_scale,
                        "y": (y - p.rect.y) / p.point_scale,
                        "visible": true,
                    }),
                );
                last_paint = Some(Instant::now());
                was_painting = true;
            } else {
                was_painting = false;
            }
            continue;
        }

        if was_painting {
            // Lift the pen so the next hold starts a fresh stroke.
            let _ = app.emit_to(
                "cursor-trail",
                "cursor-trail-point",
                serde_json::json!({ "x": 0, "y": 0, "visible": false }),
            );
            was_painting = false;
        }
        if placement.is_some() && last_paint.is_none_or(|at| at.elapsed() >= HIDE_AFTER) {
            crate::commands::hide_idle_cursor_trail(&app);
            placement = None;
            last_paint = None;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cursor_track::PointerKeys;

    const HELD: PointerKeys = PointerKeys {
        control_shift: true,
        other_key: false,
    };
    const CHORD: PointerKeys = PointerKeys {
        control_shift: true,
        other_key: true,
    };
    const UP: PointerKeys = PointerKeys {
        control_shift: false,
        other_key: false,
    };

    #[test]
    fn a_hold_paints_after_the_grace() {
        let mut t = HoldTracker::default();
        assert!(!t.update(HELD, 1_000));
        assert!(!t.update(HELD, 1_000 + HOLD_GRACE_MS - 1));
        assert!(t.update(HELD, 1_000 + HOLD_GRACE_MS));
        assert!(t.update(HELD, 5_000));
        assert!(!t.update(UP, 5_016));
    }

    #[test]
    fn a_shortcut_never_paints_until_the_keys_are_released() {
        let mut t = HoldTracker::default();
        assert!(!t.update(HELD, 0));
        assert!(!t.update(CHORD, 50));
        // The letter is up again, Control+Shift still down: still a shortcut.
        assert!(!t.update(HELD, 500));
        assert!(!t.update(UP, 520));
        assert!(!t.update(HELD, 600));
        assert!(t.update(HELD, 600 + HOLD_GRACE_MS));
    }

    #[test]
    fn a_key_pressed_mid_hold_stops_the_trail() {
        let mut t = HoldTracker::default();
        assert!(!t.update(HELD, 0));
        assert!(t.update(HELD, 300));
        assert!(!t.update(CHORD, 316));
        assert!(!t.update(HELD, 332));
    }

    #[test]
    fn recordings_block_the_pointer_but_finishing_ones_do_not() {
        assert!(phase_blocks_pointer(RecordingPhase::Countdown));
        assert!(phase_blocks_pointer(RecordingPhase::Recording));
        assert!(phase_blocks_pointer(RecordingPhase::Paused));
        assert!(!phase_blocks_pointer(RecordingPhase::Idle));
        assert!(!phase_blocks_pointer(RecordingPhase::Stopping));
        assert!(!phase_blocks_pointer(RecordingPhase::Uploading));
    }

    #[test]
    fn the_overlay_goes_on_the_display_under_the_pointer() {
        let left = SourceHighlightRectDto {
            x: -1440.0,
            y: 0.0,
            width: 1440.0,
            height: 900.0,
        };
        let main = SourceHighlightRectDto {
            x: 0.0,
            y: 0.0,
            width: 1728.0,
            height: 1117.0,
        };
        let displays = [main, left];
        assert_eq!(display_at(&displays, 10.0, 10.0).map(|r| r.x), Some(0.0));
        assert_eq!(
            display_at(&displays, -5.0, 400.0).map(|r| r.x),
            Some(-1440.0)
        );
        // Right and bottom edges belong to the next display, not this one.
        assert!(display_at(&displays, 1728.0, 10.0).is_none());
        assert!(display_at(&displays, -5.0, 900.0).is_none());
    }
}
