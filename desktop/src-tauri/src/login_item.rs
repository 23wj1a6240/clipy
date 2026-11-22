//! One-time default registration; macOS remains the source of truth afterward.
use tauri::AppHandle;
use tauri_plugin_store::StoreExt;

const INITIALIZED_KEY: &str = "login_item_initialized";

#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub enum LoginItemStatus {
    Disabled,
    Enabled,
    RequiresApproval,
    Unavailable,
}

#[cfg(target_os = "macos")]
extern "C" {
    fn clipy_login_item_status() -> std::ffi::c_int;
    fn clipy_login_item_set_enabled(enabled: std::ffi::c_int) -> std::ffi::c_int;
}

#[cfg(target_os = "macos")]
fn decode_status(status: std::ffi::c_int) -> Result<LoginItemStatus, String> {
    match status {
        0 => Ok(LoginItemStatus::Disabled),
        1 => Ok(LoginItemStatus::Enabled),
        2 => Ok(LoginItemStatus::RequiresApproval),
        3 => Ok(LoginItemStatus::Unavailable),
        _ => Err(
            "Could not update or read the login item. Check Login Items in System Settings.".into(),
        ),
    }
}

#[tauri::command]
pub fn get_login_item_status() -> Result<LoginItemStatus, String> {
    #[cfg(target_os = "macos")]
    // SAFETY: the native function takes no pointers and owns its autorelease pool.
    return decode_status(unsafe { clipy_login_item_status() });
    #[cfg(not(target_os = "macos"))]
    Ok(LoginItemStatus::Unavailable)
}

fn mark_initialized(app: &AppHandle) -> Result<(), String> {
    let store = app
        .store(crate::commands::SETTINGS_STORE)
        .map_err(|e| e.to_string())?;
    store.set(INITIALIZED_KEY, serde_json::json!(true));
    store.save().map_err(|e| e.to_string())
}

#[tauri::command]
pub fn set_launch_at_login(app: AppHandle, enabled: bool) -> Result<LoginItemStatus, String> {
    if get_login_item_status()? == LoginItemStatus::Unavailable {
        return Err("Move Clipy to Applications to change launch at login.".into());
    }
    // Save BEFORE changing the OS setting so a later launch never undoes an opt-out.
    mark_initialized(&app)?;
    #[cfg(target_os = "macos")]
    // SAFETY: only a boolean integer crosses the native boundary.
    return decode_status(unsafe { clipy_login_item_set_enabled(i32::from(enabled)) });
    #[cfg(not(target_os = "macos"))]
    {
        let _ = enabled;
        Ok(LoginItemStatus::Unavailable)
    }
}

fn should_register_default(debug: bool, initialized: bool, status: LoginItemStatus) -> bool {
    !debug && !initialized && status == LoginItemStatus::Disabled
}

pub fn initialize(app: &AppHandle) -> Result<(), String> {
    // Never register a development executable or consume the first-release default.
    if cfg!(debug_assertions) {
        return Ok(());
    }
    let status = get_login_item_status()?;
    if status == LoginItemStatus::Unavailable {
        return Ok(());
    }
    let store = app
        .store(crate::commands::SETTINGS_STORE)
        .map_err(|e| e.to_string())?;
    let initialized = store
        .get(INITIALIZED_KEY)
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    if should_register_default(cfg!(debug_assertions), initialized, status) {
        // A failed attempt stays attempted. Retry belongs to the explicit UI control.
        let status = set_launch_at_login(app.clone(), true)?;
        tracing::info!(?status, "initialized launch at login");
    } else if !initialized {
        // Respect existing registration or an OS approval/denial state on upgrades.
        mark_initialized(app)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_only_applies_once_in_an_installed_release() {
        for status in [
            LoginItemStatus::Disabled,
            LoginItemStatus::Enabled,
            LoginItemStatus::RequiresApproval,
            LoginItemStatus::Unavailable,
        ] {
            assert!(!should_register_default(true, false, status));
            assert!(!should_register_default(false, true, status));
            assert_eq!(
                should_register_default(false, false, status),
                status == LoginItemStatus::Disabled
            );
        }
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn native_errors_are_not_reported_as_disabled() {
        assert!(decode_status(-1).is_err());
        assert_eq!(decode_status(2).unwrap(), LoginItemStatus::RequiresApproval);
    }
}
