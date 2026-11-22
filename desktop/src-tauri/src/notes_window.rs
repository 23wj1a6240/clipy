//! Sticky-note teleprompter: a small always-on-top window the user reads
//! from while recording. It is NSWindowSharingNone on macOS and excluded from
//! capture on Windows, so no recording ever sees it, and it lives outside the
//! popover so it survives the panel closing.

use tauri::{AppHandle, LogicalPosition, LogicalSize, Manager, WebviewUrl, WebviewWindowBuilder};

const LABEL: &str = "notes";
const WIDTH: f64 = 300.0;
const HEIGHT: f64 = 340.0;
const MARGIN: f64 = 24.0;

/// Where the note opens the first time.
///
/// It used to be pinned to the top-right corner of the primary display, which
/// is exactly where the recorder popover is anchored (centered under the
/// menu-bar icon, see `tray::popover_origin`). The note is always-on-top and
/// the popover is not, so an open note sat on top of the Start recording
/// button and there was no way to reach it. Open clear of the popover
/// instead, stepping to its LEFT when the two would overlap.
///
/// Only the FIRST open is placed: the window is created once and reused, so
/// wherever the user drags it is where it stays for the rest of the session.
fn initial_position(app: &AppHandle) -> LogicalPosition<f64> {
    let main = app.get_webview_window("main");
    let Some(monitor) = main
        .as_ref()
        .and_then(|window| window.current_monitor().ok().flatten())
        .or_else(|| app.primary_monitor().ok().flatten())
    else {
        return LogicalPosition::new(80.0, 80.0);
    };

    // `work_area` already starts below the menu bar, so its top edge is the
    // highest the note should ever sit.
    let scale = monitor.scale_factor();
    let work = monitor.work_area();
    let work_x = work.position.x as f64 / scale;
    let work_y = work.position.y as f64 / scale;
    let work_w = work.size.width as f64 / scale;

    let mut x = work_x + work_w - WIDTH - MARGIN;
    if let Some(main) = main {
        if let (Ok(position), Ok(size), Ok(main_scale)) = (
            main.outer_position(),
            main.outer_size(),
            main.scale_factor(),
        ) {
            let left = position.x as f64 / main_scale;
            let right = left + size.width as f64 / main_scale;
            if x < right && x + WIDTH > left {
                x = left - WIDTH - MARGIN;
            }
        }
    }

    LogicalPosition::new(x.max(work_x + MARGIN), work_y + MARGIN)
}

fn create(app: &AppHandle) -> Result<tauri::WebviewWindow, String> {
    let position = initial_position(app);

    let window = WebviewWindowBuilder::new(app, LABEL, WebviewUrl::App("index.html#notes".into()))
        .title(crate::i18n::t("windowTitles.notes"))
        .inner_size(WIDTH, HEIGHT)
        .min_inner_size(220.0, 180.0)
        .position(position.x, position.y)
        .decorations(false)
        .transparent(true)
        // Windows draws an undecorated window's shadow as a square frame
        // around the rounded note.
        .shadow(!cfg!(target_os = "windows"))
        .always_on_top(true)
        .skip_taskbar(true)
        // Clipy is a menu-bar app, so the note is almost never in the active
        // app. Without this the first click only activates it: × needed two
        // or three clicks and a click into the text left no caret to type at.
        .accept_first_mouse(true)
        .visible(false)
        .build()
        .map_err(|e| format!("open notes window: {e}"))?;
    #[cfg(target_os = "macos")]
    {
        let _ = window.set_visible_on_all_workspaces(true);
        // This command runs on a Tokio worker, and `setSharingType:` goes
        // through NSWindowManagement, which aborts the process off the main
        // thread ("Must only be used from the main thread"). Block until it
        // lands so the note is already excluded from capture when it shows.
        let sharing_window = window.clone();
        crate::commands::run_on_main_thread_result(app, "notes window sharing", move |_| {
            if let Ok(ns_window) = sharing_window.ns_window() {
                // SAFETY: the live Tauri window owns this NSWindow, and AppKit
                // is called on the main thread.
                unsafe { crate::commands::set_window_sharing_none(ns_window) };
            }
            Ok(())
        })?;
    }
    // The note is only for the presenter, never part of the recording.
    #[cfg(target_os = "windows")]
    crate::commands::exclude_window_from_capture(&window);
    let _ = window.set_size(LogicalSize::new(WIDTH, HEIGHT));
    Ok(window)
}

/// Async on purpose: Tauri runs sync commands on the main thread, and building
/// a window there deadlocks on Windows (the window never opens and every other
/// main-thread call hangs behind it).
///
/// `focus: false` is for the automatic show when a take starts: `set_focus`
/// activates Clipy, which would pull keyboard focus off the app the user is
/// about to demo.
#[tauri::command]
pub async fn show_notes_window(app: AppHandle, focus: Option<bool>) -> Result<(), String> {
    let window = match app.get_webview_window(LABEL) {
        Some(w) => w,
        None => create(&app)?,
    };
    // Again on every show: the window is reused, and whether exclusion draws
    // it black depends on the session, which can change in between.
    #[cfg(target_os = "windows")]
    crate::commands::exclude_window_from_capture(&window);
    window.show().map_err(|e| e.to_string())?;
    if focus.unwrap_or(true) {
        window.set_focus().ok();
    }
    Ok(())
}

#[tauri::command]
pub fn hide_notes_window(app: AppHandle) -> Result<(), String> {
    if let Some(window) = app.get_webview_window(LABEL) {
        window.hide().map_err(|e| e.to_string())?;
    }
    Ok(())
}

/// Returns whether the window is visible after the toggle.
/// Async for the same reason as `show_notes_window`: it may create the window.
#[tauri::command]
pub async fn toggle_notes_window(app: AppHandle) -> Result<bool, String> {
    if let Some(window) = app.get_webview_window(LABEL) {
        if window.is_visible().unwrap_or(false) {
            window.hide().map_err(|e| e.to_string())?;
            return Ok(false);
        }
    }
    show_notes_window(app, None).await?;
    Ok(true)
}

#[tauri::command]
pub fn notes_window_visible(app: AppHandle) -> bool {
    app.get_webview_window(LABEL)
        .map(|w| w.is_visible().unwrap_or(false))
        .unwrap_or(false)
}
