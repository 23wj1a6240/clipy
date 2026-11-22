use std::{collections::HashMap, sync::Mutex, time::Duration};

use serde::Serialize;
use tauri::{AppHandle, Manager, State, WebviewUrl, WebviewWindow, WebviewWindowBuilder};
use tokio::sync::oneshot;

use crate::types::{DisplayInfoDto, WindowInfoDto};

#[derive(Clone, Serialize)]
pub struct WindowPickerSnapshot {
    display: DisplayInfoDto,
    windows: Vec<WindowInfoDto>,
}

struct Session {
    displays: HashMap<String, WindowPickerSnapshot>,
    result: Option<oneshot::Sender<Option<u32>>>,
    /// The popover was put away while the pick was up, so the pick must
    /// end without bringing it back.
    dismissed: bool,
}

impl Session {
    fn resolve(&mut self, selected: Option<u32>) {
        if let Some(sender) = self.result.take() {
            let _ = sender.send(selected);
        }
    }
}

#[derive(Default)]
pub struct WindowPickerState(Mutex<Option<Session>>);

pub fn init(app: &AppHandle) {
    app.manage(WindowPickerState::default());
}

/// Close the overlays and report whether the popover was dismissed meanwhile.
fn finish(app: &AppHandle) -> bool {
    let session = app.state::<WindowPickerState>().0.lock().unwrap().take();
    let Some(session) = session else {
        return false;
    };
    for label in session.displays.keys() {
        if let Some(window) = app.get_webview_window(label) {
            let _ = window.close();
        }
    }
    session.dismissed
}

/// The popover is going away (tray click, dismiss) while a pick is up. The
/// popover stays on screen during a pick, so this is reachable; without it
/// the overlays would sit over every display until the 60s timeout and
/// take the user's next click as a selection.
pub(crate) fn cancel_for_dismissal<R: tauri::Runtime>(app: &AppHandle<R>) {
    let Some(state) = app.try_state::<WindowPickerState>() else {
        return;
    };
    let mut guard = state.0.lock().unwrap();
    if let Some(session) = guard.as_mut() {
        session.dismissed = true;
        session.resolve(None);
    }
}

fn ordered_windows(windows: Vec<WindowInfoDto>, front_to_back: &[u32]) -> Vec<WindowInfoDto> {
    let mut windows = windows;
    windows.sort_by_key(|window| {
        front_to_back
            .iter()
            .position(|id| *id == window.id)
            .unwrap_or(usize::MAX)
    });
    windows
}

#[tauri::command]
pub async fn pick_window_interactively(app: AppHandle) -> Result<Option<u32>, String> {
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    return Err("Window selection is unavailable on this platform".into());

    #[cfg(any(target_os = "macos", target_os = "windows"))]
    {
        let main = app
            .get_webview_window("main")
            .ok_or("Main window missing")?;
        let (sender, receiver) = oneshot::channel();
        {
            let state = app.state::<WindowPickerState>();
            let mut state = state.0.lock().unwrap();
            if state.is_some() {
                return Err("Window selection is already open".into());
            }
            *state = Some(Session {
                displays: HashMap::new(),
                result: Some(sender),
                dismissed: false,
            });
        }
        let outcome = async {
            // The popover stays on screen with its "Select window" sheet.
            // It is not hit-testable through the overlays
            // (it sits above them), so it never needs excluding from the
            // window list.
            crate::commands::hide_source_highlight(app.clone())?;
            #[cfg(target_os = "macos")]
            let sources = crate::capture::macos::ScreenCapturer::list_sources().await;
            #[cfg(target_os = "windows")]
            let sources = crate::capture::windows::ScreenCapturer::list_sources().await;
            let sources = sources.map_err(|error| format!("List windows: {error:#}"))?;
            if sources.displays.is_empty() || sources.windows.is_empty() {
                return Err(
                    "No visible windows are available. Open a window and try again.".into(),
                );
            }
            let windows = ordered_windows(
                sources.windows,
                &crate::cursor_track::window_ids_front_to_back(),
            );
            for display in sources.displays {
                let label = format!("window-select-{}", display.id);
                app.state::<WindowPickerState>()
                    .0
                    .lock()
                    .unwrap()
                    .as_mut()
                    .ok_or("Window selection was cancelled")?
                    .displays
                    .insert(
                        label.clone(),
                        WindowPickerSnapshot {
                            display: display.clone(),
                            windows: windows.clone(),
                        },
                    );
                let overlay = WebviewWindowBuilder::new(
                    &app,
                    &label,
                    WebviewUrl::App("index.html#window-select".into()),
                )
                .title(crate::i18n::t("windowTitles.selectWindow"))
                .decorations(false)
                .transparent(true)
                .shadow(false)
                .always_on_top(true)
                .skip_taskbar(true)
                .resizable(false)
                .visible(false)
                .build()
                .map_err(|error| format!("Create window picker: {error}"))?;
                #[cfg(target_os = "macos")]
                {
                    overlay
                        .set_position(tauri::LogicalPosition::new(display.x, display.y))
                        .map_err(|error| error.to_string())?;
                    overlay
                        .set_size(tauri::LogicalSize::new(display.width, display.height))
                        .map_err(|error| error.to_string())?;
                    overlay
                        .set_visible_on_all_workspaces(true)
                        .map_err(|error| error.to_string())?;
                    let sharing_overlay = overlay.clone();
                    app.run_on_main_thread(move || {
                        if let Ok(ns_window) = sharing_overlay.ns_window() {
                            // SAFETY: the live Tauri window owns this NSWindow, and AppKit is called on the main thread.
                            unsafe { crate::commands::set_window_sharing_none(ns_window) };
                        }
                    })
                    .map_err(|error| error.to_string())?;
                }
                #[cfg(target_os = "windows")]
                {
                    overlay
                        .set_position(tauri::PhysicalPosition::new(
                            display.x as i32,
                            display.y as i32,
                        ))
                        .map_err(|error| error.to_string())?;
                    overlay
                        .set_size(tauri::PhysicalSize::new(display.width, display.height))
                        .map_err(|error| error.to_string())?;
                }
                overlay.show().map_err(|error| error.to_string())?;
            }
            // Lift the popover above the overlays and keep the keyboard on
            // it: Esc is handled there, and hover on the overlays works
            // without key status. The level is bumped past "floating" so a
            // click on empty desktop cannot pull an overlay over the sheet.
            main.set_always_on_top(true)
                .map_err(|error| error.to_string())?;
            #[cfg(target_os = "macos")]
            {
                let popover = main.clone();
                app.run_on_main_thread(move || {
                    if let Ok(ns_window) = popover.ns_window() {
                        // SAFETY: the live Tauri window owns this NSWindow, and AppKit is called on the main thread.
                        unsafe { set_window_level(ns_window, NS_MODAL_PANEL_WINDOW_LEVEL) };
                    }
                })
                .map_err(|error| error.to_string())?;
            }
            main.show().map_err(|error| error.to_string())?;
            main.set_focus().map_err(|error| error.to_string())?;
            match tokio::time::timeout(Duration::from_secs(60), receiver).await {
                Ok(Ok(selected)) => Ok(selected),
                _ => Ok(None),
            }
        }
        .await;
        let dismissed = finish(&app);
        // `set_always_on_top(false)` puts the popover back at the normal
        // level, undoing the bump above. A dismissed popover stays hidden:
        // `show` here would reopen what the user just put away.
        let restore = main
            .set_always_on_top(false)
            .and_then(|_| {
                if dismissed {
                    Ok(())
                } else {
                    main.show().and_then(|_| main.set_focus())
                }
            })
            .map_err(|error| error.to_string());
        match (outcome, restore) {
            (Err(error), _) | (_, Err(error)) => Err(error),
            (Ok(selected), Ok(())) => Ok(selected),
        }
    }
}

#[tauri::command]
pub fn get_window_picker_snapshot(
    window: WebviewWindow,
    state: State<'_, WindowPickerState>,
) -> Result<WindowPickerSnapshot, String> {
    state
        .0
        .lock()
        .unwrap()
        .as_ref()
        .and_then(|session| session.displays.get(window.label()))
        .cloned()
        .ok_or_else(|| "Window selection is no longer open".into())
}

/// Esc pressed in the popover, which is the key window during a pick.
#[tauri::command]
pub fn cancel_window_picker(state: State<'_, WindowPickerState>) -> Result<(), String> {
    let mut guard = state.0.lock().unwrap();
    let session = guard.as_mut().ok_or("Window selection is no longer open")?;
    session.resolve(None);
    Ok(())
}

/// NSModalPanelWindowLevel: above the floating level Tauri uses for
/// always-on-top windows, below the menu bar.
#[cfg(target_os = "macos")]
const NS_MODAL_PANEL_WINDOW_LEVEL: i64 = 8;

/// `[NSWindow setLevel:]` — Tauri only exposes normal vs floating.
#[cfg(target_os = "macos")]
unsafe fn set_window_level(ns_window_ptr: *mut std::ffi::c_void, level: i64) {
    use std::ffi::c_void;
    use std::os::raw::c_char;

    if ns_window_ptr.is_null() {
        return;
    }

    extern "C" {
        fn sel_registerName(name: *const c_char) -> *const c_void;
        fn objc_msgSend();
    }

    type SendFn = unsafe extern "C" fn(*mut c_void, *const c_void, i64);
    let send: SendFn = std::mem::transmute(objc_msgSend as *const c_void);
    let sel = sel_registerName(b"setLevel:\0".as_ptr() as *const c_char);
    send(ns_window_ptr, sel, level);
}

#[tauri::command]
pub fn complete_window_picker(
    window: WebviewWindow,
    state: State<'_, WindowPickerState>,
    id: Option<u32>,
) -> Result<(), String> {
    let mut guard = state.0.lock().unwrap();
    let session = guard.as_mut().ok_or("Window selection is no longer open")?;
    let snapshot = session
        .displays
        .get(window.label())
        .ok_or("Invalid window picker")?;
    if let Some(id) = id {
        if !snapshot.windows.iter().any(|candidate| candidate.id == id) {
            return Err("Selected window is unavailable".into());
        }
    }
    session.resolve(id);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_dismissed_session_resolves_as_cancelled_once() {
        let (sender, mut receiver) = oneshot::channel();
        let mut session = Session {
            displays: HashMap::new(),
            result: Some(sender),
            dismissed: false,
        };
        session.dismissed = true;
        session.resolve(None);
        assert_eq!(receiver.try_recv(), Ok(None));
        // A later Esc or click finds nothing left to resolve.
        session.resolve(Some(7));
        assert!(session.dismissed);
    }

    #[test]
    fn frontmost_windows_win_and_unknown_ids_keep_their_order() {
        let make = |id| WindowInfoDto {
            id,
            app_name: "Test".into(),
            title: "Window".into(),
            x: 0.0,
            y: 0.0,
            width: 100,
            height: 100,
        };
        let result = ordered_windows(vec![make(1), make(2), make(3), make(4)], &[3, 1]);
        assert_eq!(
            result.iter().map(|window| window.id).collect::<Vec<_>>(),
            [3, 1, 2, 4]
        );
    }
}
