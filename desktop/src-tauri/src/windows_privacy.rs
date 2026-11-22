//! Windows camera and microphone privacy switches, for the setup window.
//!
//! Windows needs no grant for screen capture or click tracking, but camera and
//! microphone access can each be switched off in Settings > Privacy &
//! security, and a desktop app like Clipy is blocked when any of these is off:
//!   * device-wide access (HKLM),
//!   * "Let apps access your camera/microphone" (HKCU),
//!   * "Let desktop apps access your camera/microphone" (HKCU, `NonPackaged`).
//!
//! Desktop apps cannot raise a permission prompt, so "allow" means sending the
//! user to the right Settings page.

#![cfg(target_os = "windows")]

use windows::core::{w, HSTRING};
use windows::Win32::Foundation::ERROR_SUCCESS;
use windows::Win32::System::Registry::{
    RegGetValueW, HKEY, HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE, RRF_RT_REG_SZ,
};

use crate::commands::PermissionStatusDto;

const CONSENT_STORE: &str =
    r"Software\Microsoft\Windows\CurrentVersion\CapabilityAccessManager\ConsentStore";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Capability {
    Camera,
    Microphone,
}

impl Capability {
    pub fn from_pane(pane: &str) -> Option<Self> {
        match pane {
            "camera" => Some(Self::Camera),
            "microphone" => Some(Self::Microphone),
            _ => None,
        }
    }

    fn store(self) -> &'static str {
        match self {
            Self::Camera => "webcam",
            Self::Microphone => "microphone",
        }
    }

    fn settings_uri(self) -> &'static str {
        match self {
            Self::Camera => "ms-settings:privacy-webcam",
            Self::Microphone => "ms-settings:privacy-microphone",
        }
    }
}

/// Whether Clipy may use the capability, from all three switches.
pub fn status(capability: Capability) -> PermissionStatusDto {
    let key = format!(r"{CONSENT_STORE}\{}", capability.store());
    let desktop_apps = format!(r"{key}\NonPackaged");
    consent([
        read_value(HKEY_LOCAL_MACHINE, &key),
        read_value(HKEY_CURRENT_USER, &key),
        read_value(HKEY_CURRENT_USER, &desktop_apps),
    ])
}

/// A switch that was never touched has no value and means "Allow".
fn consent(values: impl IntoIterator<Item = Option<String>>) -> PermissionStatusDto {
    let denied = values
        .into_iter()
        .flatten()
        .any(|value| value.trim().eq_ignore_ascii_case("Deny"));
    if denied {
        PermissionStatusDto::Denied
    } else {
        PermissionStatusDto::Granted
    }
}

fn read_value(hive: HKEY, path: &str) -> Option<String> {
    let path = HSTRING::from(path);
    let mut buffer = [0u16; 64];
    let mut size = std::mem::size_of_val(&buffer) as u32;
    // SAFETY: Category 8 — FFI contract. `size` is the byte length of
    // `buffer`, which RegGetValueW fills with a NUL-terminated string.
    let result = unsafe {
        RegGetValueW(
            hive,
            &path,
            w!("Value"),
            RRF_RT_REG_SZ,
            None,
            Some(buffer.as_mut_ptr().cast()),
            Some(&mut size),
        )
    };
    if result != ERROR_SUCCESS {
        return None;
    }
    let chars = (size as usize / 2).min(buffer.len());
    let text = String::from_utf16_lossy(&buffer[..chars]);
    Some(text.trim_end_matches('\0').to_string())
}

/// Opens the Settings page for the capability, or the privacy root.
pub fn open_settings(capability: Option<Capability>) -> Result<(), String> {
    open_uri(capability.map_or("ms-settings:privacy", Capability::settings_uri))
}

/// Opens any `ms-settings:` page.
pub fn open_uri(uri: &str) -> Result<(), String> {
    std::process::Command::new("explorer.exe")
        .arg(uri)
        .spawn()
        .map(|_| ())
        .map_err(|e| format!("open settings failed: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn value(text: &str) -> Option<String> {
        Some(text.to_string())
    }

    #[test]
    fn any_denied_switch_blocks_access() {
        assert_eq!(consent([value("Allow"), value("Allow"), value("Allow")]), PermissionStatusDto::Granted);
        assert_eq!(consent([None, None, None]), PermissionStatusDto::Granted);
        assert_eq!(consent([value("Allow"), value("Deny"), None]), PermissionStatusDto::Denied);
        assert_eq!(consent([value("Deny"), None, value("Allow")]), PermissionStatusDto::Denied);
        assert_eq!(consent([None, None, value("deny")]), PermissionStatusDto::Denied);
    }

    #[test]
    fn reads_this_machine_without_failing() {
        for capability in [Capability::Camera, Capability::Microphone] {
            assert!(matches!(
                status(capability),
                PermissionStatusDto::Granted | PermissionStatusDto::Denied
            ));
        }
        // Every Windows 10/11 install has the device-wide switch, so a read
        // that silently fails (and would always look "allowed") shows here.
        let device = read_value(HKEY_LOCAL_MACHINE, &format!(r"{CONSENT_STORE}\webcam"));
        assert!(
            matches!(device.as_deref(), Some("Allow" | "Deny")),
            "device camera switch: {device:?}"
        );
        assert_eq!(Capability::from_pane("camera"), Some(Capability::Camera));
        assert_eq!(Capability::from_pane("screen"), None);
    }
}
