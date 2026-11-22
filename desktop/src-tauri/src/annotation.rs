use tauri::{AppHandle, Emitter};
use tauri_plugin_store::StoreExt;

const COLORS: [&str; 6] = [
    "#ff4d29", "#ffcc33", "#34d399", "#38bdf8", "#c084fc", "#ffffff",
];

#[tauri::command]
pub fn get_annotation_color(app: AppHandle) -> String {
    app.store(crate::commands::SETTINGS_STORE)
        .ok()
        .and_then(|store| store.get("annotation_color"))
        .and_then(|value| {
            value
                .as_str()
                .filter(|color| COLORS.contains(color))
                .map(str::to_owned)
        })
        .unwrap_or_else(|| COLORS[0].to_string())
}

#[tauri::command]
pub fn set_annotation_color(app: AppHandle, color: String) -> Result<(), String> {
    if !COLORS.contains(&color.as_str()) {
        return Err("Unknown annotation color".into());
    }
    let store = app
        .store(crate::commands::SETTINGS_STORE)
        .map_err(|e| e.to_string())?;
    store.set("annotation_color", serde_json::json!(color));
    store.save().map_err(|e| e.to_string())?;
    app.emit("annotation-color", color)
        .map_err(|e| e.to_string())
}

const WIDTH_MIN: f64 = 2.0;
const WIDTH_MAX: f64 = 14.0;
const WIDTH_DEFAULT: f64 = 6.0;

#[tauri::command]
pub fn get_annotation_width(app: AppHandle) -> f64 {
    app.store(crate::commands::SETTINGS_STORE)
        .ok()
        .and_then(|store| store.get("annotation_width"))
        .and_then(|value| value.as_f64())
        .filter(|width| width.is_finite() && (WIDTH_MIN..=WIDTH_MAX).contains(width))
        .unwrap_or(WIDTH_DEFAULT)
}

#[tauri::command]
pub fn set_annotation_width(app: AppHandle, width: f64) -> Result<(), String> {
    if !width.is_finite() || !(WIDTH_MIN..=WIDTH_MAX).contains(&width) {
        return Err("Pen width must be between 2 and 14".into());
    }
    let store = app
        .store(crate::commands::SETTINGS_STORE)
        .map_err(|e| e.to_string())?;
    store.set("annotation_width", serde_json::json!(width));
    store.save().map_err(|e| e.to_string())?;
    app.emit("annotation-width", width).map_err(|e| e.to_string())
}

#[tauri::command]
pub fn clear_annotations(app: AppHandle) -> Result<(), String> {
    app.emit_to("cursor-trail", "annotation-clear", ())
        .map_err(|e| e.to_string())
}

#[cfg(target_os = "macos")]
pub unsafe fn set_window_level_below_controls(window: *mut std::ffi::c_void) {
    use std::ffi::{c_char, c_void};
    extern "C" {
        fn sel_registerName(name: *const c_char) -> *const c_void;
        fn objc_msgSend();
    }
    type SetLevel = unsafe extern "C" fn(*mut c_void, *const c_void, isize);
    let send: SetLevel = std::mem::transmute(objc_msgSend as *const c_void);
    // Normal app windows are level 0; recorder controls are floating level 3.
    send(window, sel_registerName(b"setLevel:\0".as_ptr().cast()), 2);
}

/// Lets an overlay show on every Space, another app's full-screen Space
/// included: CanJoinAllSpaces (1 << 0) plus FullScreenAuxiliary (1 << 8).
/// Tao's `set_visible_on_all_workspaces` sets only the first, which keeps the
/// window out of a full-screen presentation. MoveToActiveSpace (1 << 1)
/// cannot be combined with CanJoinAllSpaces, so it is cleared.
#[cfg(target_os = "macos")]
pub unsafe fn join_all_spaces_including_fullscreen(window: *mut std::ffi::c_void) {
    use std::ffi::{c_char, c_void};
    extern "C" {
        fn sel_registerName(name: *const c_char) -> *const c_void;
        fn objc_msgSend();
    }
    const CAN_JOIN_ALL_SPACES: usize = 1 << 0;
    const MOVE_TO_ACTIVE_SPACE: usize = 1 << 1;
    const FULL_SCREEN_AUXILIARY: usize = 1 << 8;
    type Get = unsafe extern "C" fn(*mut c_void, *const c_void) -> usize;
    type Set = unsafe extern "C" fn(*mut c_void, *const c_void, usize);
    let get: Get = std::mem::transmute(objc_msgSend as *const c_void);
    let set: Set = std::mem::transmute(objc_msgSend as *const c_void);
    let current = get(window, sel_registerName(b"collectionBehavior\0".as_ptr().cast()));
    let wanted = (current & !MOVE_TO_ACTIVE_SPACE) | CAN_JOIN_ALL_SPACES | FULL_SCREEN_AUXILIARY;
    if wanted != current {
        set(
            window,
            sel_registerName(b"setCollectionBehavior:\0".as_ptr().cast()),
            wanted,
        );
    }
}
