use serde::Serialize;
use tauri::{AppHandle, Emitter, Manager};
use tauri_plugin_global_shortcut::{GlobalShortcutExt, Shortcut};
use tauri_plugin_store::StoreExt;

#[cfg(target_os = "macos")]
const PREFIX: &str = "Meta+Alt+";
#[cfg(not(target_os = "macos"))]
const PREFIX: &str = "Control+Alt+";
/// Screenshots use ⌘⇧2 (Alt+Shift+2 on Windows, where Ctrl+Shift
/// digits belong to too many apps). macOS keeps ⌘⇧3/4/5 for itself.
#[cfg(target_os = "macos")]
const SHOT_PREFIX: &str = "Meta+Shift+";
#[cfg(not(target_os = "macos"))]
const SHOT_PREFIX: &str = "Alt+Shift+";
/// Clipboard history opens on ⇧⌘V (ClipBook's and Paste's key) and
/// Ctrl+Shift+V (ClipClip's), the nearest thing to plain paste that does not
/// replace it. Both cost apps their paste-as-plain-text key. Win+V is
/// reserved by Windows, and Ctrl+Alt+V is AltGr+V, which types @ on
/// Hungarian and other layouts.
#[cfg(target_os = "macos")]
const CLIPBOARD_PREFIX: &str = "Meta+Shift+";
#[cfg(not(target_os = "macos"))]
const CLIPBOARD_PREFIX: &str = "Control+Shift+";
/// Global actions and their default keys. The user can rebind each one
/// (Preferences → Shortcuts); the choice lives in the settings store under
/// `recorder_shortcut_<action>`, where an empty string means "off". The
/// recording controls only act mid-recording; the screenshot keys only while
/// idle. An empty default code ships the action off: ⌘⇧1 is zoom-to-fit in
/// Figma and others, too common to take system-wide unasked. Clipboard history
/// works in any phase and is unbound while the feature is switched off.
const ACTIONS: [(&str, &str); 7] = [
    ("pause", "KeyP"),
    ("mic", "KeyM"),
    ("camera", "KeyC"),
    ("annotation", "KeyD"),
    ("screenshot_area", "Digit2"),
    ("screenshot_display", ""),
    (crate::clipboard_history::SHORTCUT_ACTION, "KeyV"),
];

/// Asks the popover to take a screenshot, so the capture goes through the
/// same signed-in, local-mode, and setup checks as its own button.
const EVENT_SCREENSHOT_SHORTCUT: &str = "screenshot-shortcut";

fn screenshot_mode(action: &str) -> Option<&'static str> {
    match action {
        "screenshot_area" => Some("area"),
        "screenshot_display" => Some("display"),
        _ => None,
    }
}

#[derive(Serialize)]
pub struct ActionShortcut {
    action: &'static str,
    hotkey: String,
    registered: bool,
}

fn store_key(action: &str) -> String {
    format!("recorder_shortcut_{action}")
}

fn default_hotkey(action: &str, code: &str) -> Option<String> {
    if code.is_empty() {
        None
    } else if action == crate::clipboard_history::SHORTCUT_ACTION {
        Some(format!("{CLIPBOARD_PREFIX}{code}"))
    } else if screenshot_mode(action).is_some() {
        Some(format!("{SHOT_PREFIX}{code}"))
    } else {
        Some(format!("{PREFIX}{code}"))
    }
}

/// The configured hotkey for an action: the stored override when present
/// (empty string = disabled → `None`), otherwise the default.
pub fn hotkey_for(app: &AppHandle, action: &str) -> Option<String> {
    let default = ACTIONS
        .iter()
        .find(|(name, _)| *name == action)
        .map(|(name, code)| default_hotkey(name, code))?;
    let stored = app
        .store(crate::commands::SETTINGS_STORE)
        .ok()
        .and_then(|store| store.get(store_key(action)))
        .and_then(|value| value.as_str().map(str::to_owned));
    match stored {
        Some(value) if value.is_empty() => None,
        Some(value) if crate::commands::parse_hotkey(&value).is_some() => Some(value),
        _ => default,
    }
}

fn configured(app: &AppHandle) -> Vec<(&'static str, Option<String>)> {
    ACTIONS
        .iter()
        .map(|(action, _)| (*action, hotkey_for(app, action)))
        .collect()
}

/// Actions whose key should not be held right now.
fn dormant(app: &AppHandle, action: &str) -> bool {
    action == crate::clipboard_history::SHORTCUT_ACTION && !crate::clipboard_history::is_enabled(app)
}

pub fn init(app: &AppHandle, stop_hotkey: &str) {
    for (action, hotkey) in configured(app) {
        if dormant(app, action) {
            continue;
        }
        let Some(key) = hotkey else { continue };
        if crate::commands::parse_hotkey(&key) == crate::commands::parse_hotkey(stop_hotkey) {
            continue;
        }
        if let Some(shortcut) = crate::commands::parse_hotkey(&key) {
            if app.global_shortcut().is_registered(shortcut) {
                continue;
            }
            if let Err(error) = app.global_shortcut().register(shortcut) {
                tracing::warn!("recorder shortcut {key} unavailable: {error}");
            }
        }
    }
}

/// Whether `shortcut` is one of the recorder actions' CURRENT keys, so the
/// start/stop hotkey cannot be set on top of one.
pub fn reserved_for(app: &AppHandle, shortcut: Shortcut) -> bool {
    configured(app).iter().any(|(_, hotkey)| {
        hotkey.as_deref().and_then(crate::commands::parse_hotkey) == Some(shortcut)
    })
}

/// Whether pressing the action's key reaches it: a key is set, registration
/// succeeded (another app can own it), start/stop does not win it, and no
/// earlier action shares it (`handle` takes the first match).
fn live(app: &AppHandle, all: &[(&'static str, Option<String>)], action: &str, stop: &str) -> bool {
    let key_of = |name: &str| {
        all.iter()
            .find(|(other, _)| *other == name)
            .and_then(|(_, key)| key.as_deref().and_then(crate::commands::parse_hotkey))
    };
    let Some(shortcut) = key_of(action) else { return false };
    let first_owner = all.iter().find(|(other, key)| {
        !dormant(app, other) && key.as_deref().and_then(crate::commands::parse_hotkey) == Some(shortcut)
    });
    app.global_shortcut().is_registered(shortcut)
        && crate::commands::parse_hotkey(stop) != Some(shortcut)
        && first_owner.is_some_and(|(owner, _)| *owner == action)
}

pub fn is_live(app: &AppHandle, action: &str) -> bool {
    let stop = crate::commands::current_hotkey_after_default_migration(app);
    live(app, &configured(app), action, &stop)
}

#[tauri::command]
pub fn get_recorder_shortcuts(app: AppHandle) -> Vec<ActionShortcut> {
    let stop = crate::commands::current_hotkey_after_default_migration(&app);
    let all = configured(&app);
    all.iter()
        .map(|(action, hotkey)| {
            let (action, hotkey) = (*action, hotkey.clone());
            let registered = live(&app, &all, action, &stop);
            ActionShortcut {
                action,
                hotkey: hotkey.unwrap_or_default(),
                registered,
            }
        })
        .collect()
}

/// Rebind (or clear, with an empty `hotkey`) one recording-time action. The
/// old key is released, the new one registered, and the choice persisted;
/// a key that is already the start/stop hotkey or another action's is
/// refused so two things can never fire from one press.
#[tauri::command]
pub fn set_recorder_shortcut(app: AppHandle, action: String, hotkey: String) -> Result<(), String> {
    if !ACTIONS.iter().any(|(name, _)| *name == action) {
        return Err("Unknown recorder action".into());
    }
    let hotkey = hotkey.trim().to_string();
    let new_shortcut = if hotkey.is_empty() {
        None
    } else {
        Some(crate::commands::parse_hotkey(&hotkey).ok_or_else(|| format!("invalid hotkey: {hotkey}"))?)
    };
    let stop = crate::commands::current_hotkey_after_default_migration(&app);
    if let Some(shortcut) = new_shortcut {
        if crate::commands::parse_hotkey(&stop) == Some(shortcut) {
            return Err("That is the start/stop shortcut".into());
        }
        if configured(&app).iter().any(|(other, other_key)| {
            *other != action
                && other_key.as_deref().and_then(crate::commands::parse_hotkey) == Some(shortcut)
        }) {
            return Err("Another recording control already uses that shortcut".into());
        }
    }
    let old_shortcut = hotkey_for(&app, &action).and_then(|key| crate::commands::parse_hotkey(&key));
    if let Some(shortcut) = new_shortcut.filter(|_| !dormant(&app, &action)) {
        if !app.global_shortcut().is_registered(shortcut) {
            app.global_shortcut()
                .register(shortcut)
                .map_err(|e| format!("failed to register shortcut: {e}"))?;
        }
    }
    let store = app
        .store(crate::commands::SETTINGS_STORE)
        .map_err(|e| e.to_string())?;
    store.set(store_key(&action), serde_json::json!(hotkey));
    if let Err(error) = store.save() {
        if let Some(shortcut) = new_shortcut.filter(|s| Some(*s) != old_shortcut) {
            let _ = app.global_shortcut().unregister(shortcut);
        }
        return Err(error.to_string());
    }
    if let Some(old) = old_shortcut.filter(|old| Some(*old) != new_shortcut) {
        if !shared_with_other(&app, &action, old) {
            let _ = app.global_shortcut().unregister(old);
        }
    }
    Ok(())
}

/// Whether the start/stop hotkey or another action already uses `shortcut`.
/// An install whose custom key predates an action can share one; that key
/// belongs to the other owner and must never be released on its behalf.
fn shared_with_other(app: &AppHandle, action: &str, shortcut: Shortcut) -> bool {
    let stop = crate::commands::current_hotkey_after_default_migration(app);
    crate::commands::parse_hotkey(&stop) == Some(shortcut)
        || configured(app).iter().any(|(other, key)| {
            *other != action
                && !dormant(app, other)
                && key.as_deref().and_then(crate::commands::parse_hotkey) == Some(shortcut)
        })
}

/// Takes the action's configured key, for when a feature is switched on.
pub fn register_action(app: &AppHandle, action: &str) -> Result<(), String> {
    let Some(shortcut) = hotkey_for(app, action).and_then(|key| crate::commands::parse_hotkey(&key)) else {
        return Ok(());
    };
    if shared_with_other(app, action, shortcut) {
        return Err("That shortcut is already used by another Clipy shortcut. Pick a new key for Clipboard under Shortcuts first.".into());
    }
    if app.global_shortcut().is_registered(shortcut) {
        return Ok(());
    }
    app.global_shortcut()
        .register(shortcut)
        .map_err(|e| format!("failed to register shortcut: {e}"))
}

pub fn unregister_action(app: &AppHandle, action: &str) {
    if let Some(shortcut) = hotkey_for(app, action).and_then(|key| crate::commands::parse_hotkey(&key)) {
        if !shared_with_other(app, action, shortcut) {
            let _ = app.global_shortcut().unregister(shortcut);
        }
    }
}

/// Brings the popover up when a screenshot shortcut cannot capture yet
/// (setup unfinished, recording gate), so the key never does nothing silently.
#[tauri::command]
pub fn show_screenshot_panel(app: AppHandle) {
    // The popover carries the reason (setup card, recording gate): raw.
    crate::tray::show_main_raw(&app);
}

pub fn handle(app: &AppHandle, shortcut: &Shortcut) -> bool {
    let stop = crate::commands::current_hotkey_after_default_migration(app);
    if crate::commands::parse_hotkey(&stop).as_ref() == Some(shortcut) {
        return false;
    }
    let Some((action, _)) = configured(app).into_iter().find(|(_, hotkey)| {
        hotkey.as_deref().and_then(crate::commands::parse_hotkey).as_ref() == Some(shortcut)
    }) else {
        return false;
    };
    let app = app.clone();
    if action == crate::clipboard_history::SHORTCUT_ACTION {
        tauri::async_runtime::spawn(async move {
            if let Err(error) = crate::clipboard_history::toggle_picker(app).await {
                tracing::warn!("clipboard history did not open: {error}");
            }
        });
        return true;
    }
    tauri::async_runtime::spawn(async move {
        use crate::types::RecordingPhase;
        let rec = app.state::<crate::recording::RecordingController>();
        let state = rec.dto();
        if let Some(mode) = screenshot_mode(action) {
            // Mid-recording the popover is busy and the capture would
            // fight the recorder for the screen; the key does nothing then.
            if state.phase != RecordingPhase::Idle {
                return;
            }
            if let Err(error) = app.emit_to("main", EVENT_SCREENSHOT_SHORTCUT, mode) {
                tracing::warn!("screenshot shortcut not delivered: {error}");
            }
            return;
        }
        if !matches!(
            state.phase,
            RecordingPhase::Recording | RecordingPhase::Paused
        ) {
            return;
        }
        let result = match action {
            "pause" => if state.phase == RecordingPhase::Paused {
                rec.resume(&app)
            } else {
                rec.pause(&app)
            }
            .map_err(|e| e.to_string()),
            // No microphone was opened for this take, so there is nothing to
            // mute. Toggling the flag anyway would flip the dock between two
            // "off" looks and log a mute that changed nothing.
            "mic" if !state.mic_active => Ok(()),
            "mic" => rec
                .set_mic_muted(!state.mic_muted)
                .map_err(|e| e.to_string()),
            "camera" => crate::commands::toggle_recording_webcam(app.clone())
                .await
                .map(|_| ()),
            "annotation" => crate::commands::set_cursor_trail_enabled(
                app.clone(),
                !crate::commands::get_cursor_trail_enabled(),
            )
            .map(|_| ()),
            _ => Ok(()),
        };
        if let Err(error) = result {
            tracing::warn!("recorder action {action} failed: {error}");
            let _ = app.emit("recorder-action-error", error);
        }
    });
    true
}

pub fn play_stop_sound() {
    #[cfg(target_os = "macos")]
    {
        std::thread::spawn(|| {
            let _ = std::process::Command::new("/usr/bin/afplay")
                .arg("/System/Library/Sounds/Pop.aiff")
                .stdin(std::process::Stdio::null())
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .status();
        });
    }
}
