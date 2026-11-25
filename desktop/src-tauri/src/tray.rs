//! Menu-bar icon + dropdown.
//!
//! Phase 2: the menu is rebuilt whenever the recording state changes so
//! "Start Recording" flips to "Stop Recording" + a small "Cancel" item
//! while a recording is in flight. Auth state is still reflected in the
//! main window UI rather than the tray.

use parking_lot::Mutex;
use std::sync::Arc;
use tauri::{
    image::Image,
    menu::{CheckMenuItem, Menu, MenuItem, PredefinedMenuItem},
    tray::{MouseButton, MouseButtonState, TrayIcon, TrayIconBuilder, TrayIconEvent},
    AppHandle, Emitter, Listener, LogicalPosition, Manager, Rect, Runtime,
};

use crate::i18n::{t, t_with};
use crate::recording::RecordingController;
use crate::types::{
    RecordingPhase, RecordingStateDto, EVENT_PANEL_DISMISSED, EVENT_RECORDING_STATE_CHANGED,
    EVENT_REQUEST_SOURCE_PICKER,
};

const TRAY_ID: &str = "clipy-tray";

/// How the pointer's keys are written in the tray menu.
#[cfg(target_os = "macos")]
const POINTER_SHORTCUT: &str = "⌃⇧";
#[cfg(not(target_os = "macos"))]
const POINTER_SHORTCUT: &str = "Ctrl+Shift";

/// Gap between the menu-bar icon and the top of the popover, and the
/// minimum breathing room we keep from a screen edge.
const POPOVER_GAP: f64 = 6.0;
const POPOVER_EDGE_MARGIN: f64 = 8.0;

/// PHYSICAL screen rect of the menu-bar icon, captured from whichever tray
/// event fired last. macOS only tells us where the icon is while the user
/// is interacting with it, so we remember the last known rect and fall back
/// to the top-right corner of the menu bar before the first event.
///
/// Stored physical on purpose: converting to points needs the scale factor
/// of the monitor the ICON is on, which we can only look up once we have
/// physical coordinates to probe with. Converting here with the main
/// window's scale put the popover on the wrong spot in a Retina +
/// non-Retina setup.
static TRAY_ANCHOR: Mutex<Option<(f64, f64, f64, f64)>> = Mutex::new(None);

fn remember_tray_anchor(rect: Rect, fallback_scale: f64) {
    // `fallback_scale` is only consulted if the platform handed us a logical
    // rect; macOS reports physical, in which case it is ignored.
    let position = rect.position.to_physical::<f64>(fallback_scale);
    let size = rect.size.to_physical::<f64>(fallback_scale);
    *TRAY_ANCHOR.lock() = Some((position.x, position.y, size.width, size.height));
}

/// Where the popover's top-left corner goes: centered under the menu-bar
/// icon, then clamped so it never hangs off the screen. Pure so the
/// clamping is testable without a window server.
fn popover_origin(
    anchor: (f64, f64, f64, f64),
    window: (f64, f64),
    work_area: (f64, f64, f64, f64),
) -> (f64, f64) {
    let (anchor_x, anchor_y, anchor_w, anchor_h) = anchor;
    let (width, height) = window;
    let (work_x, work_y, work_w, work_h) = work_area;

    let x = anchor_x + anchor_w / 2.0 - width / 2.0;
    let y = anchor_y + anchor_h + POPOVER_GAP;

    let min_x = work_x + POPOVER_EDGE_MARGIN;
    let max_x = (work_x + work_w - width - POPOVER_EDGE_MARGIN).max(min_x);
    // No top margin: the work area already starts below the menu bar, and
    // POPOVER_GAP is the spacing we actually want under the icon.
    let min_y = work_y;
    let max_y = (work_y + work_h - height - POPOVER_EDGE_MARGIN).max(min_y);

    (x.clamp(min_x, max_x), y.clamp(min_y, max_y))
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum IdlePrimaryAction {
    SignIn,
    StartRecording,
}

fn idle_primary_action(signed_in: bool) -> IdlePrimaryAction {
    if signed_in {
        IdlePrimaryAction::StartRecording
    } else {
        IdlePrimaryAction::SignIn
    }
}

/// The local edition has no plan to upgrade.
fn should_offer_upgrade<R: Runtime>(_app: &AppHandle<R>) -> bool {
    false
}

pub fn init<R: Runtime>(app: &AppHandle<R>) -> tauri::Result<()> {
    let icon = app
        .default_window_icon()
        .cloned()
        .unwrap_or_else(|| Image::new_owned(vec![], 0, 0));
    let idle_icon = icon.clone().to_owned();
    let recording_icon = recording_tray_icon();
    let current_state = Arc::new(Mutex::new(RecordingStateDto::idle()));
    let current_state_for_click = current_state.clone();
    let signed_in = crate::edition::can_start_recording(app);

    let menu = build_menu(
        app,
        &RecordingStateDto::idle(),
        signed_in,
        should_offer_upgrade(app),
    )?;

    let tray = TrayIconBuilder::with_id(TRAY_ID)
        .menu(&menu)
        .show_menu_on_left_click(false)
        .icon(icon)
        .tooltip(crate::edition::APP_NAME)
        .on_tray_icon_event(move |tray, event| {
            // Every tray event carries the icon's rect — remember it so the
            // popover can anchor under the icon even when it is opened from
            // the menu or the hotkey rather than a click.
            let app = tray.app_handle();
            let scale = app
                .get_webview_window("main")
                .and_then(|window| window.scale_factor().ok())
                .unwrap_or(1.0);
            match event {
                TrayIconEvent::Click { rect, .. }
                | TrayIconEvent::DoubleClick { rect, .. }
                | TrayIconEvent::Enter { rect, .. }
                | TrayIconEvent::Move { rect, .. }
                | TrayIconEvent::Leave { rect, .. } => remember_tray_anchor(rect, scale),
                _ => {}
            }
            if let TrayIconEvent::Click {
                button: MouseButton::Left,
                button_state: MouseButtonState::Up,
                ..
            } = event
            {
                match current_state_for_click.lock().phase {
                    RecordingPhase::Recording | RecordingPhase::Paused => {
                        spawn_stop_recording(app.clone())
                    }
                    // Countdown is already committed — the overlay is up and
                    // capture is about to start. Popping the panel over it would
                    // cover the countdown, and there is nothing
                    // useful to click; Cancel lives in the right-click menu.
                    RecordingPhase::Countdown => {}
                    // Popover behaviour: clicking the icon again puts it away.
                    _ => toggle_main(app),
                }
            }
        })
        .on_menu_event(|app, event| match event.id.as_ref() {
            "start_record" => spawn_start_recording(app.clone()),
            "sign_in" => show_main(app),
            "stop_record" => spawn_stop_recording(app.clone()),
            "pause_record" => pause_recording_now(app),
            "resume_record" => resume_recording_now(app),
            "cancel_record" => spawn_cancel_recording(app.clone()),
            // Asked for by name: the popover, even over setup.
            "open_window" => show_main_raw(app),
            "whats_new" => crate::whats_new::open_from_menu(app),
            "pointer" => crate::pointer::toggle_pointer_mode(app),
            "quit" => app.exit(0),
            other => tracing::warn!("unhandled tray menu id: {}", other),
        })
        .build(app)?;

    // Rebuild the menu in response to recording-state-changed events. We
    // hold the TrayIcon in an Arc<Mutex<...>> so the listener can mutate
    // its menu without grabbing the AppHandle's tray handle each time.
    //
    // Only rebuild on *phase* changes — the recording ticker fires every
    // 250 ms with a fresh elapsed_ms, and swapping the Menu out from under
    // an open right-click popup makes the popup dismiss / become
    // unresponsive. Tooltip updates are cheap and safe to do every tick.
    let tray = Arc::new(Mutex::new(tray));
    // Menu bar text for a local recording still being processed. The
    // recording-state listener below fires on every ticker tick and must not
    // wipe it: processing outlives the recording it belongs to.
    let local_title = Arc::new(Mutex::new(None::<String>));
    let tray_for_listener = tray.clone();
    let app_for_listener = app.clone();
    let current_state_for_listener = current_state.clone();
    let idle_icon_for_listener = idle_icon.clone();
    let recording_icon_for_listener = recording_icon.clone();
    let local_title_for_listener = local_title.clone();

    app.listen_any(EVENT_RECORDING_STATE_CHANGED, move |event| {
        let payload = event.payload();
        let state: RecordingStateDto = match serde_json::from_str(payload) {
            Ok(s) => s,
            Err(err) => {
                tracing::warn!("failed to parse recording-state payload: {err:#}");
                return;
            }
        };
        let phase_changed = {
            let mut prev = current_state_for_listener.lock();
            let changed = prev.phase != state.phase;
            *prev = state.clone();
            changed
        };
        let tray = tray_for_listener.lock();
        if phase_changed {
            let signed_in = crate::edition::can_start_recording(&app_for_listener);
            let offer_upgrade = should_offer_upgrade(&app_for_listener);
            match build_menu(&app_for_listener, &state, signed_in, offer_upgrade) {
                Ok(menu) => {
                    if let Err(err) = tray.set_menu(Some(menu)) {
                        tracing::warn!("failed to apply rebuilt tray menu: {err:#}");
                    }
                }
                Err(err) => {
                    tracing::warn!("failed to rebuild tray menu: {err:#}");
                }
            }
        }
        // Tooltip carries the live elapsed/upload status so users see
        // progress without opening the menu.
        let _ = tray.set_tooltip(Some(tooltip_for(&state)));
        if matches!(
            state.phase,
            RecordingPhase::Recording | RecordingPhase::Paused
        ) {
            let _ = tray.set_icon(Some(recording_icon_for_listener.clone()));
        } else {
            let _ = tray.set_icon(Some(idle_icon_for_listener.clone()));
        }
        let _ = tray.set_title(Some(tray_title(&local_title_for_listener.lock())));
    });

    let tray_for_local = tray.clone();
    let local_title_for_local = local_title.clone();
    app.listen_any(crate::types::EVENT_LOCAL_RECORDING_PROGRESS, move |event| {
        let status: crate::local::Status = match serde_json::from_str(event.payload()) {
            Ok(s) => s,
            Err(err) => {
                tracing::warn!("failed to parse local progress payload: {err:#}");
                return;
            }
        };
        let title = local_stage_title(&status.stage).map(str::to_string);
        *local_title_for_local.lock() = title.clone();
        {
            let tray = tray_for_local.lock();
            let _ = tray.set_title(Some(tray_title(&title)));
            let _ = tray.set_tooltip(Some(status.message));
        }
        if title.as_deref() == Some(LOCAL_READY_TITLE) {
            // "Ready" is a moment, not a state. Clear it unless a newer job
            // has taken the slot since.
            let tray = tray_for_local.clone();
            let local_title = local_title_for_local.clone();
            tauri::async_runtime::spawn(async move {
                tokio::time::sleep(std::time::Duration::from_secs(4)).await;
                let mut current = local_title.lock();
                if current.as_deref() == Some(LOCAL_READY_TITLE) {
                    *current = None;
                    let _ = tray.lock().set_title(Some(String::new()));
                }
            });
        }
    });

    let tray_for_auth = tray.clone();
    let app_for_auth = app.clone();
    let current_state_for_auth = current_state.clone();
    app.listen_any(crate::auth::AUTH_STATE_EVENT, move |_| {
        let signed_in = crate::edition::can_start_recording(&app_for_auth);
        let state = current_state_for_auth.lock().clone();
        let offer_upgrade = should_offer_upgrade(&app_for_auth);
        match build_menu(&app_for_auth, &state, signed_in, offer_upgrade) {
            Ok(menu) => {
                if let Err(err) = tray_for_auth.lock().set_menu(Some(menu)) {
                    tracing::warn!("failed to apply auth-aware tray menu: {err:#}");
                }
            }
            Err(err) => tracing::warn!("failed to rebuild tray menu after auth change: {err:#}"),
        }
    });

    // The upgrade item depends on the plan, which is only known after the
    // first entitlement fetch.
    let tray_for_plan = tray.clone();
    let app_for_plan = app.clone();
    let current_state_for_plan = current_state.clone();
    app.listen_any(crate::entitlement::ENTITLEMENT_CHANGED_EVENT, move |_| {
        let signed_in = crate::edition::can_start_recording(&app_for_plan);
        let state = current_state_for_plan.lock().clone();
        let offer_upgrade = should_offer_upgrade(&app_for_plan);
        match build_menu(&app_for_plan, &state, signed_in, offer_upgrade) {
            Ok(menu) => {
                if let Err(err) = tray_for_plan.lock().set_menu(Some(menu)) {
                    tracing::warn!("failed to apply plan-aware tray menu: {err:#}");
                }
            }
            Err(err) => tracing::warn!("failed to rebuild tray menu after plan change: {err:#}"),
        }
    });

    // "Show Pointer" was turned on or off (from the menu, by Escape, or by a
    // recording starting): rebuild so its check mark is right.
    let tray_for_pointer = tray.clone();
    let app_for_pointer = app.clone();
    let current_state_for_pointer = current_state.clone();
    app.listen_any(crate::pointer::EVENT_POINTER_MODE_CHANGED, move |_| {
        let signed_in = crate::edition::can_start_recording(&app_for_pointer);
        let state = current_state_for_pointer.lock().clone();
        let offer_upgrade = should_offer_upgrade(&app_for_pointer);
        match build_menu(&app_for_pointer, &state, signed_in, offer_upgrade) {
            Ok(menu) => {
                if let Err(err) = tray_for_pointer.lock().set_menu(Some(menu)) {
                    tracing::warn!("failed to apply pointer tray menu: {err:#}");
                }
            }
            Err(err) => tracing::warn!("failed to rebuild tray menu after pointer change: {err:#}"),
        }
    });

    // The language changed in Settings (or was first resolved by the
    // webview): rebuild the menu, tooltip and menu-bar word in it.
    let tray_for_locale = tray.clone();
    let app_for_locale = app.clone();
    let current_state_for_locale = current_state.clone();
    let local_title_for_locale = local_title.clone();
    app.listen_any(crate::i18n::EVENT_UI_LOCALE_CHANGED, move |_| {
        let signed_in = app_for_locale
            .state::<crate::auth::AuthState>()
            .bearer_token()
            .is_some();
        let state = current_state_for_locale.lock().clone();
        let offer_upgrade = should_offer_upgrade(&app_for_locale);
        let tray = tray_for_locale.lock();
        match build_menu(&app_for_locale, &state, signed_in, offer_upgrade) {
            Ok(menu) => {
                if let Err(err) = tray.set_menu(Some(menu)) {
                    tracing::warn!("failed to apply translated tray menu: {err:#}");
                }
            }
            Err(err) => tracing::warn!("failed to rebuild tray menu after locale change: {err:#}"),
        }
        let _ = tray.set_tooltip(Some(tooltip_for(&state)));
        let _ = tray.set_title(Some(tray_title(&local_title_for_locale.lock())));
    });

    Ok(())
}

// Catalog keys (i18n.rs), translated when written to the menu bar so the
// comparisons below do not depend on the language.
const LOCAL_PROCESSING_TITLE: &str = "tray.localProcessing";
const LOCAL_READY_TITLE: &str = "tray.localReady";

/// The menu bar gets a glance, not a status report: one word while the
/// local recording is being worked on, one when it is done.
fn local_stage_title(stage: &str) -> Option<&'static str> {
    match stage {
        "transcribing" | "selecting" | "extracting" => Some(LOCAL_PROCESSING_TITLE),
        "ready" => Some(LOCAL_READY_TITLE),
        _ => None,
    }
}

/// tray-icon's macOS `set_title(None)` is a no-op — it only writes when
/// given a string — so clearing has to be an explicit empty title.
fn tray_title(title: &Option<String>) -> String {
    title.as_deref().map(t).unwrap_or_default()
}

fn recording_tray_icon() -> Image<'static> {
    let size = 18usize;
    let mut rgba = vec![0u8; size * size * 4];
    let center = (size as f64 - 1.0) / 2.0;
    for y in 0..size {
        for x in 0..size {
            let dx = x as f64 - center;
            let dy = y as f64 - center;
            let offset = (y * size + x) * 4;
            if (dx * dx + dy * dy).sqrt() <= 7.0 {
                rgba[offset] = 255;
                rgba[offset + 1] = 77;
                rgba[offset + 2] = 41;
                rgba[offset + 3] = 255;
            }
        }
    }
    Image::new_owned(rgba, size as u32, size as u32)
}

fn build_menu<R: Runtime>(
    app: &AppHandle<R>,
    state: &RecordingStateDto,
    signed_in: bool,
    offer_upgrade: bool,
) -> tauri::Result<Menu<R>> {
    let separator = PredefinedMenuItem::separator(app)?;
    let open_window =
        MenuItem::with_id(app, "open_window", t("tray.openClipy"), true, None::<&str>)?;
    let quit = MenuItem::with_id(app, "quit", t("tray.quit"), true, None::<&str>)?;
    // Idle menu only: the window never opens over a recording flow.
    let whats_new = MenuItem::with_id(app, "whats_new", t("tray.whatsNew"), true, None::<&str>)?;
    // Idle menu only too: during a recording the recording's own Flare has
    // the overlay. The shortcut is shown, not bound: the pointer thread reads
    // the keys itself (`pointer.rs`).
    let pointer = CheckMenuItem::with_id(
        app,
        "pointer",
        t_with("tray.showPointer", &[("shortcut", POINTER_SHORTCUT)]),
        true,
        crate::pointer::pointer_mode_on(),
        None::<&str>,
    )?;

    match state.phase {
        RecordingPhase::Idle => {
            let primary = match idle_primary_action(signed_in) {
                IdlePrimaryAction::SignIn => {
                    MenuItem::with_id(app, "sign_in", t("tray.signIn"), true, None::<&str>)?
                }
                IdlePrimaryAction::StartRecording => {
                    MenuItem::with_id(app, "start_record", t("tray.startRecording"), true, None::<&str>)?
                }
            };
            let staged_update: Option<String> = None;
            let restart_update = staged_update
                .map(|version| {
                    MenuItem::with_id(
                        app,
                        "restart_update",
                        t_with("tray.restartUpdate", &[("version", version.as_str())]),
                        true,
                        None::<&str>,
                    )
                })
                .transpose()?;
            // The local edition has no plan to upgrade.
            let upgrade: Option<MenuItem<R>> = {
                let _ = offer_upgrade;
                None
            };
            let mut items: Vec<&dyn tauri::menu::IsMenuItem<R>> = vec![&primary];
            if let Some(upgrade) = &upgrade {
                items.push(upgrade);
            }
            items.push(&separator);
            if let Some(restart_update) = &restart_update {
                items.push(restart_update);
            }
            items.extend([&pointer as &dyn tauri::menu::IsMenuItem<R>, &separator]);
            items.extend([&open_window as &dyn tauri::menu::IsMenuItem<R>, &whats_new]);
            items.extend([&separator as &dyn tauri::menu::IsMenuItem<R>, &quit]);
            Menu::with_items(app, &items)
        }
        RecordingPhase::Countdown => {
            let starting = MenuItem::with_id(app, "starting", t("tray.starting"), false, None::<&str>)?;
            let cancel = MenuItem::with_id(app, "cancel_record", t("tray.cancel"), true, None::<&str>)?;
            Menu::with_items(
                app,
                &[
                    &starting,
                    &cancel,
                    &separator,
                    &open_window,
                    &separator,
                    &quit,
                ],
            )
        }
        RecordingPhase::Recording => {
            let elapsed = state.elapsed_ms.unwrap_or(0);
            let stop_label = t_with("tray.stopRecordingElapsed", &[("elapsed", &format_elapsed(elapsed))]);
            let stop = MenuItem::with_id(app, "stop_record", &stop_label, true, None::<&str>)?;
            let pause =
                MenuItem::with_id(app, "pause_record", t("tray.pauseRecording"), true, None::<&str>)?;
            let cancel =
                MenuItem::with_id(app, "cancel_record", t("tray.cancelDiscard"), true, None::<&str>)?;
            Menu::with_items(
                app,
                &[
                    &stop,
                    &pause,
                    &cancel,
                    &separator,
                    &open_window,
                    &separator,
                    &quit,
                ],
            )
        }
        RecordingPhase::Paused => {
            let elapsed = state.elapsed_ms.unwrap_or(0);
            let resume_label = t_with("tray.resumeRecordingElapsed", &[("elapsed", &format_elapsed(elapsed))]);
            let resume =
                MenuItem::with_id(app, "resume_record", &resume_label, true, None::<&str>)?;
            let stop = MenuItem::with_id(app, "stop_record", t("tray.stopRecording"), true, None::<&str>)?;
            let cancel =
                MenuItem::with_id(app, "cancel_record", t("tray.cancelDiscard"), true, None::<&str>)?;
            Menu::with_items(
                app,
                &[
                    &resume,
                    &stop,
                    &cancel,
                    &separator,
                    &open_window,
                    &separator,
                    &quit,
                ],
            )
        }
        RecordingPhase::Stopping => {
            let stopping = MenuItem::with_id(app, "stopping", t("tray.stopping"), false, None::<&str>)?;
            Menu::with_items(
                app,
                &[&stopping, &separator, &open_window, &separator, &quit],
            )
        }
        RecordingPhase::Uploading => {
            let uploading =
                MenuItem::with_id(app, "uploading", t("tray.uploading"), false, None::<&str>)?;
            // Escape hatch: upload retries are never-terminal, so a dead
            // network after Stop pins "Uploading…" until the user cancels.
            let cancel =
                MenuItem::with_id(app, "cancel_record", t("tray.cancelUpload"), true, None::<&str>)?;
            Menu::with_items(
                app,
                &[
                    &uploading,
                    &cancel,
                    &separator,
                    &open_window,
                    &separator,
                    &quit,
                ],
            )
        }
    }
}

fn tooltip_for(state: &RecordingStateDto) -> String {
    match state.phase {
        RecordingPhase::Idle => crate::edition::APP_NAME.to_string(),
        RecordingPhase::Countdown => t("tooltip.starting"),
        RecordingPhase::Recording => t_with(
            "tooltip.recording",
            &[("elapsed", &format_elapsed(state.elapsed_ms.unwrap_or(0)))],
        ),
        RecordingPhase::Paused => t_with(
            "tooltip.paused",
            &[("elapsed", &format_elapsed(state.elapsed_ms.unwrap_or(0)))],
        ),
        RecordingPhase::Stopping => t("tooltip.stopping"),
        RecordingPhase::Uploading => t("tooltip.uploading"),
    }
}

fn format_elapsed(ms: u64) -> String {
    let total_secs = ms / 1000;
    let mins = total_secs / 60;
    let secs = total_secs % 60;
    format!("{mins:02}:{secs:02}")
}

/// Park the main window under the menu-bar icon. Called before every show
/// so the popover follows the icon across monitors and menu-bar layout
/// changes instead of being positioned once at launch.
pub(crate) fn anchor_main_to_tray<R: Runtime>(app: &AppHandle<R>) {
    let Some(window) = app.get_webview_window("main") else {
        return;
    };
    let window_scale = window.scale_factor().unwrap_or(1.0);
    let Ok(size) = window.outer_size() else {
        return;
    };
    // The window's own monitor scale is the right divisor for its size —
    // macOS keeps a window's point size constant as it moves between
    // displays, so this stays correct on the target monitor.
    let width = size.width as f64 / window_scale;
    let height = size.height as f64 / window_scale;

    let anchor_physical = *TRAY_ANCHOR.lock();
    // Before the first tray event we do not know where the icon is. The
    // menu-bar icon always lives at the top-right, so aim there.
    let monitor = anchor_physical
        .and_then(|(x, y, w, _)| window.monitor_from_point(x + w / 2.0, y).ok().flatten())
        .or_else(|| window.primary_monitor().ok().flatten());
    let Some(monitor) = monitor else {
        return;
    };
    // Points on the monitor the ICON is on — not the one the window
    // happens to be on right now.
    let monitor_scale = monitor.scale_factor();
    let work = monitor.work_area();
    let work_area = (
        work.position.x as f64 / monitor_scale,
        work.position.y as f64 / monitor_scale,
        work.size.width as f64 / monitor_scale,
        work.size.height as f64 / monitor_scale,
    );
    let anchor = match anchor_physical {
        Some((x, y, w, h)) => (
            x / monitor_scale,
            y / monitor_scale,
            w / monitor_scale,
            h / monitor_scale,
        ),
        None => (
            work_area.0 + work_area.2 - POPOVER_EDGE_MARGIN,
            work_area.1,
            0.0,
            0.0,
        ),
    };

    let (x, y) = popover_origin(anchor, (width, height), work_area);
    let _ = window.set_position(LogicalPosition::new(x, y));
}

/// True while the popover must stay off screen: it would sit on top of the
/// thing being captured, and the user could click it mid-take.
pub(crate) fn capture_in_progress<R: Runtime>(app: &AppHandle<R>) -> bool {
    app.try_state::<RecordingController>().is_some_and(|rec| {
        matches!(
            rec.dto().phase,
            RecordingPhase::Countdown | RecordingPhase::Recording | RecordingPhase::Paused
        )
    })
}

/// Put the popover away from outside the webview. Emits first so the
/// frontend can end its picker session — hiding the window natively would
/// otherwise strand the source highlight and the camera preview.
pub(crate) fn dismiss_main<R: Runtime>(app: &AppHandle<R>) {
    let _ = app.emit(EVENT_PANEL_DISMISSED, ());
    crate::window_picker::cancel_for_dismissal(app);
    // Not while a capture runs: the same dismiss fires when a recording
    // starts, and the recording owns the camera bubble from then on.
    if !capture_in_progress(app) {
        crate::commands::teardown_picker_session(app);
    }
    if let Some(window) = app.get_webview_window("main") {
        let _ = window.hide();
    }
}

/// Tray left-click when idle: open the popover, or put it away if it is
/// already up.
fn toggle_main<R: Runtime>(app: &AppHandle<R>) {
    if let Some(window) = app.get_webview_window("main") {
        if window.is_visible().unwrap_or(false) && window.is_focused().unwrap_or(false) {
            dismiss_main(app);
            return;
        }
    }
    // A click on the icon is a request for the popover itself.
    show_main_raw(app);
}

/// Brings the popover up, unless first-run setup is open: then setup comes
/// forward instead, so nothing covers it (a sign-in deep link, a second
/// launch, a Dock click, the signed-out shortcut, the sign-in menu item).
/// Paths where the popover itself was asked for use `show_main_raw`: a click
/// on the tray icon, "Open Clipy", a request to record, the screenshot
/// shortcut's explanation, the post-recording restore, setup's own close and
/// the forced-update gate.
pub(crate) fn show_main<R: Runtime>(app: &AppHandle<R>) {
    if crate::onboarding::focus_if_open(app) {
        return;
    }
    show_main_raw(app);
}

pub(crate) fn show_main_raw<R: Runtime>(app: &AppHandle<R>) {
    // Authoritative guard: every path back to the popover (tray, Dock
    // reopen, single-instance relaunch, deep link, forced update) funnels
    // through here, so refusing while a capture is live is what actually
    // keeps it off the recording.
    if capture_in_progress(app) {
        tracing::debug!("show_main refused: capture in progress");
        return;
    }
    anchor_main_to_tray(app);
    if let Some(window) = app.get_webview_window("main") {
        #[cfg(target_os = "macos")]
        if let Err(err) = window.set_visible_on_all_workspaces(true) {
            tracing::warn!("failed to show Clipy popover on all Spaces: {err:#}");
        }
        // unminimize first in case the window was minimized — show()
        // alone doesn't restore from a minimized state.
        let _ = window.unminimize();
        let _ = window.show();
        let _ = window.set_focus();
        // After a recording cycle the main window is sometimes left
        // behind other apps and `show()+set_focus()` doesn't bring it
        // back to the front (especially with macOSPrivateApi=true).
        // Bouncing always-on-top forces the window server to re-order
        // it; we set it back immediately so the window doesn't stay
        // pinned above others.
        let _ = window.set_always_on_top(true);
        let _ = window.set_always_on_top(false);
    }
}

pub(crate) async fn request_source_picker<R: Runtime>(app: &AppHandle<R>) {
    // Asking to record (tray item or shortcut) is a request for the popover
    // itself, even with setup open; a missing grant shows its Setup card.
    show_main_raw(app);
    if let Err(err) = app.emit(EVENT_REQUEST_SOURCE_PICKER, ()) {
        tracing::warn!("failed to emit source-picker request (immediate): {err:#}");
    }
    tokio::time::sleep(std::time::Duration::from_millis(300)).await;
    if let Err(err) = app.emit(EVENT_REQUEST_SOURCE_PICKER, ()) {
        tracing::warn!("failed to emit source-picker request (delayed): {err:#}");
    }
}

fn spawn_start_recording<R: Runtime>(app: AppHandle<R>) {
    tauri::async_runtime::spawn(async move {
        if !crate::edition::can_start_recording(&app) {
            tracing::warn!("start_record menu fired while signed out — surfacing main window");
            show_main(&app);
            return;
        }
        request_source_picker(&app).await;
    });
}

fn pause_recording_now<R: Runtime>(app: &AppHandle<R>) {
    let rec = app.state::<RecordingController>();
    if let Err(err) = rec.pause(app) {
        tracing::error!("pause recording failed: {err:#}");
    }
}

fn resume_recording_now<R: Runtime>(app: &AppHandle<R>) {
    let rec = app.state::<RecordingController>();
    if let Err(err) = rec.resume(app) {
        tracing::error!("resume recording failed: {err:#}");
    }
}

fn spawn_stop_recording<R: Runtime>(app: AppHandle<R>) {
    tauri::async_runtime::spawn(async move {
        let rec = app.state::<RecordingController>();
        if let Err(err) = rec.stop(&app).await {
            if crate::recording::is_benign_stop_error(&err) {
                tracing::info!("stop recording: {err:#}");
            } else {
                tracing::error!("stop recording failed: {err:#}");
            }
        }
    });
}

fn spawn_cancel_recording<R: Runtime>(app: AppHandle<R>) {
    tauri::async_runtime::spawn(async move {
        let rec = app.state::<RecordingController>();
        if let Err(err) = rec.cancel(&app).await {
            tracing::error!("cancel recording failed: {err:#}");
        }
    });
}

/// Clears the session; the auth-state event rebuilds this menu and sends the
/// window back to the sign-in screen. The item only exists while idle, but a
/// recording can start between opening the menu and clicking, so check again.

#[allow(dead_code)]
fn _typecheck_tray_param(_t: &TrayIcon) {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn signed_out_tray_prompts_for_sign_in_before_recording() {
        assert_eq!(idle_primary_action(false), IdlePrimaryAction::SignIn);
    }

    #[test]
    fn signed_in_tray_can_start_recording() {
        assert_eq!(idle_primary_action(true), IdlePrimaryAction::StartRecording);
    }

    #[test]
    fn menu_bar_shows_one_word_per_local_phase_and_clears_otherwise() {
        for stage in ["transcribing", "selecting", "extracting"] {
            assert_eq!(local_stage_title(stage), Some(LOCAL_PROCESSING_TITLE));
        }
        assert_eq!(local_stage_title("ready"), Some(LOCAL_READY_TITLE));
        assert_eq!(local_stage_title("recording"), None);
        assert_eq!(local_stage_title("failed"), None);
        // Clearing must be an explicit empty string; `None` is ignored by
        // tray-icon on macOS and would leave the last word stuck.
        assert_eq!(tray_title(&None), "");
        assert_eq!(tray_title(&Some(LOCAL_READY_TITLE.into())), "Ready");
    }

    #[test]
    fn popover_hangs_centered_under_the_menu_bar_icon() {
        let (x, y) = popover_origin((1200.0, 0.0, 24.0, 24.0), (400.0, 640.0), (0.0, 25.0, 1680.0, 1025.0));

        assert_eq!(x, 1012.0);
        assert_eq!(y, 30.0);
    }

    #[test]
    fn popover_never_hangs_off_the_right_edge() {
        // Icon sits in the far-right corner: a naive center would push half
        // the panel off screen.
        let (x, _) = popover_origin((1656.0, 0.0, 24.0, 24.0), (400.0, 640.0), (0.0, 25.0, 1680.0, 1025.0));

        assert_eq!(x, 1272.0);
    }

    #[test]
    fn popover_stays_on_a_secondary_monitor_with_a_negative_origin() {
        let (x, y) = popover_origin(
            (-900.0, -1080.0, 24.0, 24.0),
            (400.0, 640.0),
            (-1920.0, -1055.0, 1920.0, 1055.0),
        );

        assert_eq!(x, -1088.0);
        assert_eq!(y, -1050.0);
    }
}
