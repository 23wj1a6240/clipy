//! First-run setup lives in its own window, not inside the menu-bar
//! popover: macOS permission prompts and System Settings hops are a poor fit
//! for a 340px panel that closes the moment focus leaves it.
//!
//! The window is also the first thing a new user sees. A menu-bar (or tray)
//! app has no window of its own, so a first launch that only drops a popover
//! under an icon the user has never seen reads as "nothing happened". Until
//! setup is finished once, every launch reopens it; that is also what resumes
//! setup after macOS makes the user quit and reopen for Screen Recording.

use tauri::{AppHandle, Emitter, Manager, Runtime, WebviewUrl, WebviewWindowBuilder, WindowEvent};
use tauri_plugin_store::StoreExt;

const LABEL: &str = "onboarding";

/// True from the moment launch decides to show setup until its window
/// exists, so a deep link or Dock click in that gap waits for setup instead
/// of opening the popover over it.
static OPENING: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Set by `finish_onboarding` just before it closes the window, so the close
/// handler can tell finishing from the user closing setup.
static FINISHING: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// The user closed setup in this process. Lets a close stick for the
/// automatic prompt in dev builds too, where the persisted flags are off.
static DISMISSED_THIS_SESSION: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);
/// Set only by `finish_onboarding`, the last step's button. Until it is set,
/// every launch opens setup.
const COMPLETED_KEY: &str = "onboarding_completed";
/// Set when the user closes setup without finishing it, so the popover's
/// automatic "something is missing" prompt does not reopen it straight away.
const DISMISSED_KEY: &str = "onboarding_dismissed";
/// Set while setup is on screen (cleared when it is closed or finished), so a
/// relaunch in between, including macOS's own Quit & Reopen dialog, reopens
/// it even on an install that finished setup before.
const RESUME_KEY: &str = "onboarding_resume";

/// Async on purpose: Tauri runs sync commands on the main thread, and building
/// a window there deadlocks on Windows (the window never opens and every other
/// main-thread call hangs behind it).
#[tauri::command]
pub async fn show_onboarding_window(app: AppHandle) -> Result<(), String> {
    if let Some(window) = app.get_webview_window(LABEL) {
        window.show().map_err(|e| e.to_string())?;
        window.unminimize().ok();
        window.set_focus().map_err(|e| e.to_string())?;
        mark_open(&app);
        return Ok(());
    }
    let builder =
        WebviewWindowBuilder::new(&app, LABEL, WebviewUrl::App("index.html#onboarding".into()))
            .title(crate::i18n::t("windowTitles.welcome"))
            .inner_size(760.0, 540.0)
            .resizable(false)
            .maximizable(false)
            .center();
    // A full-height content view under the traffic lights, like the native
    // setup assistants. The page leaves room for the buttons and makes the
    // sidebar a drag region.
    #[cfg(target_os = "macos")]
    let builder = builder
        .title_bar_style(tauri::TitleBarStyle::Overlay)
        .hidden_title(true);
    let window = match builder.build() {
        Ok(window) => window,
        // Two callers raced to create it: the other one won, use that.
        Err(err) => match app.get_webview_window(LABEL) {
            Some(window) => {
                window.show().ok();
                window.unminimize().ok();
                window.set_focus().ok();
                mark_open(&app);
                return Ok(());
            }
            None => return Err(format!("open setup window: {err}")),
        },
    };
    let handle = app.clone();
    window.on_window_event(move |event| {
        if let WindowEvent::CloseRequested { .. } = event {
            // Closed on purpose: a later relaunch does not bring it back.
            if let Err(err) = clear_flag(&handle, RESUME_KEY) {
                tracing::warn!("could not clear setup resume: {err}");
            }
            // Finishing closes the window too; any other close is the user
            // putting setup away, finished install or not. Quitting the app
            // raises no close at all.
            if !FINISHING.swap(false, std::sync::atomic::Ordering::SeqCst) {
                DISMISSED_THIS_SESSION.store(true, std::sync::atomic::Ordering::SeqCst);
                if let Err(err) = set_flag(&handle, DISMISSED_KEY) {
                    tracing::warn!("could not record dismissed setup: {err}");
                }
            }
            // Closing setup half way must not leave a tray app with nothing
            // on screen: bring the popover back so there is still a way in.
            // Raw: setup is still the window being closed.
            crate::tray::show_main_raw(&handle);
        }
    });
    window.set_focus().ok();
    mark_open(&app);
    Ok(())
}

/// Setup is on screen. An earlier close no longer stands, and any relaunch
/// before it is closed or finished (setup's own restart, macOS's Quit &
/// Reopen dialog, a crash, a quit) brings it back, finished install or not.
fn mark_open(app: &AppHandle) {
    DISMISSED_THIS_SESSION.store(false, std::sync::atomic::Ordering::SeqCst);
    // A finish whose close never arrived must not turn the next close of
    // this new session into a finish.
    FINISHING.store(false, std::sync::atomic::Ordering::SeqCst);
    if let Err(err) = clear_flag(app, DISMISSED_KEY) {
        tracing::warn!("could not clear dismissed setup: {err}");
    }
    if let Err(err) = set_flag(app, RESUME_KEY) {
        tracing::warn!("could not record setup resume: {err}");
    }
}

/// The recorder's automatic "something is missing" prompt. Unlike the setup
/// buttons it respects a setup the user just closed: closing setup shows the
/// popover, and reopening setup from there would make the close not stick.
/// Returns whether setup was shown; when not, the popover offers its own
/// "Open setup" card.
#[tauri::command]
pub async fn offer_onboarding_window(app: AppHandle) -> Result<bool, String> {
    // Closed by the user (this session for a finished install, until it is
    // finished otherwise), or already open: the popover was asked for, so do
    // not pull focus back to setup.
    if flag(&app, DISMISSED_KEY)
        || DISMISSED_THIS_SESSION.load(std::sync::atomic::Ordering::SeqCst)
        || app.get_webview_window(LABEL).is_some()
    {
        return Ok(false);
    }
    show_onboarding_window(app).await.map(|()| true)
}

/// Setup's "Quit & Reopen" / "Restart Clipy": restart so macOS applies a
/// Screen Recording change, and come back to setup afterwards.
#[tauri::command]
pub async fn relaunch_into_setup(app: AppHandle) -> Result<(), String> {
    // Same gate as restart-to-update: a restart here must never cut off a
    // recording or an upload (desktop HARD RULE).
    let rec = app.state::<crate::recording::RecordingController>();
    let start_gate = rec.start_restart_gate().lock().await;
    if !matches!(rec.dto().phase, crate::types::RecordingPhase::Idle) {
        return Err("A recording is in progress. Stop it before restarting.".into());
    }
    if rec.upload_in_flight() {
        return Err("An upload is still finishing. Try again in a moment.".into());
    }
    // Showing setup already set this; set it again in case that save failed.
    // The restart is what applies the grant, so a failure here never blocks it.
    if let Err(err) = set_flag(&app, RESUME_KEY) {
        tracing::warn!("could not record setup resume: {err}");
    }
    crate::commands::relaunch_app(app.clone());
    // `request_restart` only queues the exit. Keep the gate held until the
    // process is gone, as restart-to-update does, so a recording cannot
    // start in between and be cut off by the restart.
    std::mem::forget(start_gate);
    Ok(())
}

/// The last step's button: remember that setup is done (so launch stops
/// reopening it), tell the recorder, then hand over to the popover. Closing
/// raises CloseRequested, whose handler (set when the window is built) brings
/// the popover back through `tray::show_main_raw` (the window being closed is
/// still setup): anchored to the tray icon and refused while a capture is live.
///
/// The event goes out only after the save succeeds, so a failed save leaves
/// both the window and the recorder believing setup is unfinished.
/// Async so it runs off the main thread: it closes the webview that invoked
/// it, and window work on the main thread can deadlock on Windows.
#[tauri::command]
pub async fn finish_onboarding(app: AppHandle) -> Result<(), String> {
    mark_completed(&app).map_err(|e| format!("Clipy couldn't save that setup is done: {e}"))?;
    let _ = app.emit("onboarding-complete", ());
    match app.get_webview_window(LABEL) {
        Some(window) => {
            FINISHING.store(true, std::sync::atomic::Ordering::SeqCst);
            window.close().map_err(|e| {
                FINISHING.store(false, std::sync::atomic::Ordering::SeqCst);
                format!("Setup is saved, but its window would not close: {e}")
            })
        }
        None => {
            crate::tray::show_main_raw(&app);
            Ok(())
        }
    }
}

/// Windows 11 puts a new tray icon behind the `^` overflow. The taskbar
/// settings page is where "Other system tray icons" keeps it visible.
#[tauri::command]
pub fn open_tray_settings() -> Result<(), String> {
    #[cfg(target_os = "windows")]
    {
        crate::windows_privacy::open_uri("ms-settings:taskbar")
    }
    #[cfg(not(target_os = "windows"))]
    {
        Err("taskbar settings exist on Windows only".into())
    }
}

/// If setup is on screen, bring it forward and report true. Sign-in finishing
/// in the browser calls this instead of popping the recorder over a setup
/// window that is still mid-flow.
pub fn focus_if_open<R: Runtime>(app: &AppHandle<R>) -> bool {
    // The window only exists while setup is open (closing destroys it).
    // Do not test visibility: a minimized window reports not visible on
    // macOS, and that is exactly the one to bring back.
    let Some(window) = app.get_webview_window(LABEL) else {
        // Launch is still building it: let it come up on its own.
        return OPENING.load(std::sync::atomic::Ordering::SeqCst);
    };
    // Same rule as `tray::show_main`: nothing comes forward over a live
    // capture. Reporting true keeps the caller from trying the popover.
    if crate::tray::capture_in_progress(app) {
        return true;
    }
    window.unminimize().ok();
    window.show().ok();
    window.set_focus().ok();
    true
}

// The launch gate below runs in release builds only; dev builds open setup
// on demand (see `lib.rs`), so these read as unused there.
fn flag<R: Runtime>(app: &AppHandle<R>, key: &str) -> bool {
    // Dev builds neither write nor read them (see PERSIST_FLAGS).
    if !PERSIST_FLAGS {
        return false;
    }
    app.store(crate::commands::SETTINGS_STORE)
        .ok()
        .and_then(|store| store.get(key))
        .and_then(|value| value.as_bool())
        .unwrap_or(false)
}

/// Dev builds share settings.json with the release app on the same Mac (same
/// identifier) but skip the launch gate, so they neither write nor read its
/// flags: dev and release must not change each other's setup behavior.
const PERSIST_FLAGS: bool = !cfg!(debug_assertions);

fn set_flag(app: &AppHandle, key: &str) -> Result<(), String> {
    if !PERSIST_FLAGS {
        return Ok(());
    }
    let store = app
        .store(crate::commands::SETTINGS_STORE)
        .map_err(|e| e.to_string())?;
    let previous = store.get(key);
    if previous.as_ref().and_then(|value| value.as_bool()) == Some(true) {
        return Ok(());
    }
    store.set(key, serde_json::json!(true));
    store.save().map_err(|e| {
        // Put the cache back as it was: the next save of any other setting
        // would otherwise persist what we just reported as unsaved.
        restore(&store, key, previous);
        e.to_string()
    })
}

fn clear_flag(app: &AppHandle, key: &str) -> Result<(), String> {
    if !PERSIST_FLAGS {
        return Ok(());
    }
    let store = app
        .store(crate::commands::SETTINGS_STORE)
        .map_err(|e| e.to_string())?;
    let Some(previous) = store.get(key) else {
        return Ok(());
    };
    store.delete(key);
    store.save().map_err(|e| {
        restore(&store, key, Some(previous));
        e.to_string()
    })
}

fn restore(
    store: &tauri_plugin_store::Store<tauri::Wry>,
    key: &str,
    previous: Option<serde_json::Value>,
) {
    match previous {
        Some(value) => store.set(key, value),
        None => {
            store.delete(key);
        }
    }
}

/// Whether setup's last step was ever completed. Release builds only, like
/// every flag here; dev builds always read false.
pub(crate) fn is_completed<R: Runtime>(app: &AppHandle<R>) -> bool {
    flag(app, COMPLETED_KEY)
}

fn mark_completed(app: &AppHandle) -> Result<(), String> {
    set_flag(app, COMPLETED_KEY)?;
    // Leftovers of an unfinished setup mean nothing once it is done.
    for key in [DISMISSED_KEY, RESUME_KEY] {
        if let Err(err) = clear_flag(app, key) {
            tracing::warn!("could not clear {key}: {err}");
        }
    }
    Ok(())
}

/// What launch should do about setup.
#[cfg_attr(debug_assertions, allow(dead_code))]
#[derive(Debug, PartialEq, Eq)]
enum LaunchSetup {
    /// Every step of setup was completed: its last button was pressed.
    Done,
    Show,
}

/// Inputs to the launch decision, read from settings.
#[cfg_attr(debug_assertions, allow(dead_code))]
#[derive(Clone, Copy, Default)]
struct LaunchState {
    /// Set only by `finish_onboarding`, the last step's button.
    completed: bool,
    /// Setup was on screen when the app went away (its own restart, macOS's
    /// Quit & Reopen, a crash, a quit).
    resume: bool,
}

/// Setup is the launch surface until its last step is completed. Nothing
/// else marks it done: not a relaunch, not closing it half way, not an
/// install that ran before, not a sign-in or grants already in place. The
/// only way past it is `finish_onboarding`.
#[cfg_attr(debug_assertions, allow(dead_code))]
fn launch_setup(state: LaunchState) -> LaunchSetup {
    // A finished install that reopened setup to fix a grant comes back to
    // it too.
    if state.resume || !state.completed {
        LaunchSetup::Show
    } else {
        LaunchSetup::Done
    }
}

/// What the caller should put on screen after the launch decision.
#[cfg_attr(debug_assertions, allow(dead_code))]
#[derive(Debug, PartialEq, Eq)]
pub enum LaunchSurface {
    /// Setup is done. The usual launch: popover when signed out, nothing
    /// when signed in.
    Usual,
    Setup,
}

/// Called once from `setup()`. Says what to put on screen.
#[cfg_attr(debug_assertions, allow(dead_code))]
pub fn on_launch(app: &AppHandle) -> LaunchSurface {
    let resume = flag(app, RESUME_KEY);
    if resume {
        if let Err(err) = clear_flag(app, RESUME_KEY) {
            tracing::warn!("could not clear resumed setup: {err}");
        }
    }
    let completed = flag(app, COMPLETED_KEY);
    match launch_setup(LaunchState { completed, resume }) {
        LaunchSetup::Done => {
            // For a finished install a close only quiets the automatic
            // prompt for that session.
            if let Err(err) = clear_flag(app, DISMISSED_KEY) {
                tracing::warn!("could not clear dismissed setup: {err}");
            }
            LaunchSurface::Usual
        }
        LaunchSetup::Show => {
            OPENING.store(true, std::sync::atomic::Ordering::SeqCst);
            let handle = app.clone();
            tauri::async_runtime::spawn(async move {
                let shown = show_onboarding_window(handle.clone()).await;
                OPENING.store(false, std::sync::atomic::Ordering::SeqCst);
                if let Err(err) = shown {
                    tracing::error!("open setup window at launch: {err}");
                    // Never launch to nothing.
                    crate::tray::show_main_raw(&handle);
                }
            });
            LaunchSurface::Setup
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn launch(f: impl FnOnce(&mut LaunchState)) -> LaunchSetup {
        let mut state = LaunchState::default();
        f(&mut state);
        launch_setup(state)
    }

    #[test]
    fn setup_shows_until_its_last_step_is_done() {
        // A first run, a relaunch half way, a closed setup and an install
        // that ran before all look the same here: not completed.
        assert_eq!(launch(|_| {}), LaunchSetup::Show);
    }

    #[test]
    fn a_finished_setup_never_reopens_by_itself() {
        assert_eq!(launch(|s| s.completed = true), LaunchSetup::Done);
    }

    #[test]
    fn setups_own_restart_always_comes_back_to_setup() {
        assert_eq!(launch(|s| s.resume = true), LaunchSetup::Show);
        // A finished install that reopened setup to fix a grant.
        assert_eq!(
            launch(|s| {
                s.completed = true;
                s.resume = true;
            }),
            LaunchSetup::Show
        );
    }
}
