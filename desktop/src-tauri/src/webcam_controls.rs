//! Shows and hides the camera hover pill from the native pointer position.
//!
//! The pill is a separate always-on-top window under the bubble. It used to
//! be revealed by pointer events inside the bubble's webview and held open by
//! a hover event the pill emitted back over IPC, with a JS grace timer in
//! between. Every link in that chain could drop: WKWebView hover in a window
//! that is not key, a webview busy decoding camera frames, a Rust-side hide
//! the webview never heard about (which left its "already shown" flag stuck),
//! and the crossing between two windows. The pill came up only sometimes,
//! which made changing the camera size a gamble.
//!
//! Now one thread reads the pointer and the mouse button from the OS,
//! hit-tests both window frames on the main thread, and shows or hides the
//! pill itself. The webviews take no part.

use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use tauri::{AppHandle, Manager, Runtime, WebviewWindow};

/// Poll cadence while the bubble is on screen.
const TICK: Duration = Duration::from_millis(50);
/// Poll cadence while it is not: just often enough to notice it appear.
const IDLE_TICK: Duration = Duration::from_millis(400);
/// How long the pill outlives the pointer leaving both windows.
const HIDE_GRACE: Duration = Duration::from_millis(900);
/// Points around either window that still count as "near", so a pointer that
/// skims an edge on the way from the bubble to the pill does not lose it.
const REACH_MARGIN: f64 = 18.0;

static RUNNING: AtomicBool = AtomicBool::new(false);
static POINTER_IN_CONTROLS: AtomicBool = AtomicBool::new(false);

/// True while the pointer is inside the pill. A bubble resize must not
/// re-anchor the pill out from under a click, so the position sync reads this.
pub(crate) fn pointer_in_controls() -> bool {
    POINTER_IN_CONTROLS.load(Ordering::Acquire)
}

/// Start the poller once per process. Cheap to call again.
pub(crate) fn ensure_running(app: &AppHandle) {
    if RUNNING.swap(true, Ordering::AcqRel) {
        return;
    }
    let app = app.clone();
    let spawned = std::thread::Builder::new()
        .name("webcam-controls-hover".into())
        .spawn(move || {
            let mut state = HoverState::default();
            let mut bubble_visible = false;
            loop {
                std::thread::sleep(if bubble_visible { TICK } else { IDLE_TICK });
                let (tx, rx) = std::sync::mpsc::sync_channel(1);
                let app_for_tick = app.clone();
                if app
                    .run_on_main_thread(move || {
                        let _ = tx.send(tick(&app_for_tick, state));
                    })
                    .is_err()
                {
                    break;
                }
                match rx.recv_timeout(Duration::from_secs(2)) {
                    Ok((next, visible)) => {
                        state = next;
                        bubble_visible = visible;
                    }
                    // The main thread is busy (a modal, a long native call).
                    // Try again rather than abandoning the bubble.
                    Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
                    Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => break,
                }
            }
            POINTER_IN_CONTROLS.store(false, Ordering::Release);
            RUNNING.store(false, Ordering::Release);
        });
    if spawned.is_err() {
        RUNNING.store(false, Ordering::Release);
    }
}

#[derive(Clone, Copy, Debug, Default)]
struct HoverState {
    /// Last tick the pointer was near the bubble or the pill.
    last_near: Option<Instant>,
    /// A press started on the bubble and the button is still down.
    dragging: bool,
}

#[derive(Clone, Copy, Debug, Default)]
struct Observation {
    bubble_visible: bool,
    controls_visible: bool,
    over_bubble: bool,
    over_controls: bool,
    /// Within `REACH_MARGIN` of either window (includes being over one).
    near: bool,
    button_down: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Action {
    Keep,
    Reveal,
    Hide,
}

fn decide(state: HoverState, obs: Observation, now: Instant) -> (HoverState, Action) {
    if !obs.bubble_visible {
        let action = if obs.controls_visible { Action::Hide } else { Action::Keep };
        return (HoverState::default(), action);
    }
    let mut next = state;
    // A press on the bubble starts a native drag. The pill must not sit under
    // a moving window, and must not pop back up mid-drag when the pointer
    // strays off the bubble; it returns once the button is released.
    if obs.button_down && (obs.over_bubble || state.dragging) {
        next.dragging = true;
        next.last_near = None;
        let action = if obs.controls_visible { Action::Hide } else { Action::Keep };
        return (next, action);
    }
    next.dragging = false;
    if obs.near {
        next.last_near = Some(now);
        let action = if obs.controls_visible { Action::Keep } else { Action::Reveal };
        return (next, action);
    }
    if obs.controls_visible {
        let expired = state
            .last_near
            .is_none_or(|at| now.saturating_duration_since(at) >= HIDE_GRACE);
        if expired {
            next.last_near = None;
            return (next, Action::Hide);
        }
    }
    (next, Action::Keep)
}

/// One poll: runs on the main thread. Returns the next state and whether the
/// bubble is on screen (which sets the poll cadence).
fn tick<R: Runtime>(app: &AppHandle<R>, state: HoverState) -> (HoverState, bool) {
    let (Some(bubble), Some(controls)) = (
        app.get_webview_window("webcam"),
        app.get_webview_window("webcam-controls"),
    ) else {
        return (HoverState::default(), false);
    };
    let mut obs = Observation {
        bubble_visible: bubble.is_visible().unwrap_or(false),
        controls_visible: controls.is_visible().unwrap_or(false),
        ..Observation::default()
    };
    if obs.bubble_visible {
        if let (Some(pointer), Some(bubble_frame)) = (pointer_position(), window_frame(&bubble)) {
            let controls_frame = obs
                .controls_visible
                .then(|| window_frame(&controls))
                .flatten();
            let margin = REACH_MARGIN * frame_scale(&bubble);
            obs.over_controls = controls_frame.is_some_and(|frame| frame.contains(pointer, 0.0));
            obs.over_bubble = !obs.over_controls && bubble_frame.contains(pointer, 0.0);
            obs.near = obs.over_controls
                || obs.over_bubble
                || bubble_frame.contains(pointer, margin)
                || controls_frame.is_some_and(|frame| frame.contains(pointer, margin));
            obs.button_down = crate::cursor_track::current_primary_button_active();
        }
    }
    POINTER_IN_CONTROLS.store(obs.over_controls, Ordering::Release);

    let (next, action) = decide(state, obs, Instant::now());
    match action {
        Action::Reveal => {
            if let Err(err) = crate::commands::reveal_webcam_controls(app) {
                tracing::warn!("webcam controls reveal failed: {err}");
            }
        }
        Action::Hide => {
            let _ = controls.hide();
        }
        Action::Keep => {}
    }
    (next, obs.bubble_visible)
}

#[derive(Clone, Copy, Debug)]
struct Frame {
    x: f64,
    y: f64,
    width: f64,
    height: f64,
}

impl Frame {
    fn contains(&self, (px, py): (f64, f64), margin: f64) -> bool {
        px >= self.x - margin
            && px <= self.x + self.width + margin
            && py >= self.y - margin
            && py <= self.y + self.height + margin
    }
}

/// The pointer in the same space `window_frame` uses: CoreGraphics global
/// points on macOS, physical screen pixels elsewhere.
fn pointer_position() -> Option<(f64, f64)> {
    crate::cursor_track::current_cursor_position().map(|(x, y)| (f64::from(x), f64::from(y)))
}

fn window_frame<R: Runtime>(window: &WebviewWindow<R>) -> Option<Frame> {
    let position = window.outer_position().ok()?;
    let size = window.outer_size().ok()?;
    #[cfg(target_os = "macos")]
    {
        // tao derives the physical frame from AppKit points and the primary
        // screen height, which is exactly the space CGEventGetLocation reports
        // in, so converting back with the window's own scale lands there.
        let scale = window.scale_factor().ok().filter(|scale| *scale > 0.0)?;
        let position = position.to_logical::<f64>(scale);
        let size = size.to_logical::<f64>(scale);
        Some(Frame { x: position.x, y: position.y, width: size.width, height: size.height })
    }
    #[cfg(not(target_os = "macos"))]
    {
        Some(Frame {
            x: f64::from(position.x),
            y: f64::from(position.y),
            width: f64::from(size.width),
            height: f64::from(size.height),
        })
    }
}

/// Multiplier that turns `REACH_MARGIN` (points) into the frame space.
fn frame_scale<R: Runtime>(window: &WebviewWindow<R>) -> f64 {
    #[cfg(target_os = "macos")]
    {
        let _ = window;
        1.0
    }
    #[cfg(not(target_os = "macos"))]
    {
        window.scale_factor().ok().filter(|scale| *scale > 0.0).unwrap_or(1.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn obs(over_bubble: bool, over_controls: bool, near: bool) -> Observation {
        Observation {
            bubble_visible: true,
            controls_visible: false,
            over_bubble,
            over_controls,
            near: near || over_bubble || over_controls,
            button_down: false,
        }
    }

    #[test]
    fn reveals_when_the_pointer_reaches_the_bubble_and_keeps_it_while_near() {
        let now = Instant::now();
        let (state, action) = decide(HoverState::default(), obs(true, false, true), now);
        assert_eq!(action, Action::Reveal);
        assert_eq!(state.last_near, Some(now));

        let shown = Observation { controls_visible: true, ..obs(false, false, true) };
        let (state, action) = decide(state, shown, now + Duration::from_millis(100));
        assert_eq!(action, Action::Keep);
        assert_eq!(state.last_near, Some(now + Duration::from_millis(100)));
    }

    #[test]
    fn hides_only_after_the_grace_period_away_from_both_windows() {
        let now = Instant::now();
        let (state, _) = decide(HoverState::default(), obs(true, false, true), now);
        let away = Observation { controls_visible: true, ..obs(false, false, false) };
        let (state, action) = decide(state, away, now + HIDE_GRACE / 2);
        assert_eq!(action, Action::Keep);
        let (state, action) = decide(state, away, now + HIDE_GRACE);
        assert_eq!(action, Action::Hide);
        assert_eq!(state.last_near, None);
    }

    #[test]
    fn a_press_on_the_bubble_hides_the_pill_until_the_button_is_released() {
        let now = Instant::now();
        let (state, _) = decide(HoverState::default(), obs(true, false, true), now);
        let press = Observation { controls_visible: true, button_down: true, ..obs(true, false, true) };
        let (state, action) = decide(state, press, now);
        assert_eq!(action, Action::Hide);
        assert!(state.dragging);

        // Mid-drag the pointer can leave the bubble's old frame: still no pill.
        let strayed = Observation { button_down: true, ..obs(false, false, false) };
        let (state, action) = decide(state, strayed, now + Duration::from_millis(50));
        assert_eq!(action, Action::Keep);
        assert!(state.dragging);

        let released = obs(true, false, true);
        let (state, action) = decide(state, released, now + Duration::from_millis(100));
        assert_eq!(action, Action::Reveal);
        assert!(!state.dragging);
    }

    #[test]
    fn a_click_inside_the_pill_is_not_a_drag() {
        let now = Instant::now();
        let click = Observation { controls_visible: true, button_down: true, ..obs(false, true, true) };
        let (state, action) = decide(HoverState::default(), click, now);
        assert_eq!(action, Action::Keep);
        assert!(!state.dragging);
    }

    #[test]
    fn a_hidden_bubble_takes_the_pill_down_and_resets() {
        let now = Instant::now();
        let (state, _) = decide(HoverState::default(), obs(true, false, true), now);
        let gone = Observation { bubble_visible: false, controls_visible: true, ..Observation::default() };
        let (state, action) = decide(state, gone, now);
        assert_eq!(action, Action::Hide);
        assert_eq!(state.last_near, None);
        assert!(!state.dragging);
    }

    #[test]
    fn frame_margin_bridges_the_gap_between_windows() {
        let frame = Frame { x: 100.0, y: 100.0, width: 50.0, height: 50.0 };
        assert!(frame.contains((125.0, 125.0), 0.0));
        assert!(!frame.contains((125.0, 160.0), 0.0));
        assert!(frame.contains((125.0, 160.0), REACH_MARGIN));
        assert!(!frame.contains((125.0, 170.0), REACH_MARGIN));
    }
}
