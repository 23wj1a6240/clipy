//! Whether Windows can keep our overlays out of a capture without drawing them
//! as a black box.
//!
//! `SetWindowDisplayAffinity(WDA_EXCLUDEFROMCAPTURE)` hides the recording
//! dock, its hints, the webcam toolbar and the other user-only windows from
//! every screen capture. It only works when the window is drawn on a local
//! monitor by the Desktop Window Manager. Where that is not true the window is
//! drawn with no content, which is a solid black box both on the user's screen
//! and in the recording (QA report on a Windows 10 22H2 VM):
//!
//! - Before Windows 10 2004 (build 19041), `WDA_EXCLUDEFROMCAPTURE` acts as
//!   `WDA_MONITOR`, which shows the window with no content everywhere but a
//!   physical monitor (Microsoft's `SetWindowDisplayAffinity` docs).
//! - The affinity "works only when the Desktop Window Manager (DWM) is
//!   composing the desktop" (same docs).
//! - In a Remote Desktop session, including Hyper-V's enhanced session, the
//!   desktop the user sees is not a local monitor, so the protected window is
//!   sent as black. `SM_REMOTESESSION` misses RemoteFX vGPU sessions, which
//!   Microsoft says to detect by comparing our session id with
//!   `GlassSessionId` ("Detecting the Remote Desktop Services environment").
//! - On a desktop streamed by capturing the screen (Parsec, Shadow, spacedesk,
//!   other virtual display drivers) the viewer only ever sees a capture, so an
//!   excluded window is missing or black for them. Cap keeps exclusion off on
//!   these (`apps/desktop-gpui/src/platform/windows/capture_exclusion.rs`) and
//!   this module follows its markers, but only for the adapter behind the
//!   window's own monitor.
//!
//! The decision is made when a window is shown. A session that moves to
//! Remote Desktop while the dock is already up keeps the old affinity until
//! the dock is shown again (the next recording).
//!
//! In any of those cases the window is left capturable (`WDA_NONE`): it is
//! drawn normally and appears in the recording, which is better than a black
//! box the user cannot read. `CLIPY_WINDOW_CAPTURE_EXCLUSION` overrides the
//! detection (`on`/`always`/`1` keeps exclusion, `off`/`never`/`0` drops it)
//! for support and QA.

/// Environment variable that overrides the detection.
pub(crate) const ENV_OVERRIDE: &str = "CLIPY_WINDOW_CAPTURE_EXCLUSION";

/// First build where `WDA_EXCLUDEFROMCAPTURE` exists (Windows 10 2004).
pub(crate) const FIRST_EXCLUDING_BUILD: u32 = 19041;

/// Words in the BIOS strings of a cloud PC that is only ever seen through a
/// stream. Shadow reports `S:102` for protected windows. Plain VM hardware
/// (Hyper-V, VMware, VirtualBox, QEMU, EC2) is deliberately absent: a VM seen
/// through its own console draws excluded windows normally.
const STREAMED_COMPUTER_MARKERS: &[&str] = &["shadow"];

/// Words in the name of a display adapter that exists to be streamed or
/// mirrored by capturing it.
const VIRTUAL_DISPLAY_MARKERS: &[&str] = &[
    "parsec",
    "spacedesk",
    "iddsample",
    "virtual display",
    "usbmmidd",
    "amyuni",
    "shadow",
];

/// What the environment variable asks for, if anything.
fn parse_override(value: &str) -> Option<bool> {
    match value.trim().to_ascii_lowercase().as_str() {
        "on" | "always" | "1" => Some(true),
        "off" | "never" | "0" => Some(false),
        _ => None,
    }
}

fn find_marker(text: &str, markers: &[&'static str]) -> Option<&'static str> {
    let lower = text.to_ascii_lowercase();
    markers
        .iter()
        .copied()
        .find(|marker| lower.contains(marker))
}

/// Why capture exclusion would draw a black box here, or `None` when it is
/// safe to use. The probes are only called when an earlier check has not
/// already answered, so an override or an old build costs no system calls.
fn blocker_with(
    override_value: Option<&str>,
    build: Option<u32>,
    dwm_composing: impl FnOnce() -> Option<bool>,
    remote_session: impl FnOnce() -> bool,
    streamed_computer: impl FnOnce() -> Option<String>,
    virtual_display: impl FnOnce() -> Option<String>,
) -> Option<String> {
    match override_value.and_then(parse_override) {
        Some(true) => return None,
        Some(false) => return Some(format!("{ENV_OVERRIDE} override")),
        None => {}
    }
    // An unreadable build counts as new enough, as it always has.
    if let Some(build) = build.filter(|build| *build < FIRST_EXCLUDING_BUILD) {
        return Some(format!(
            "Windows build {build} predates WDA_EXCLUDEFROMCAPTURE ({FIRST_EXCLUDING_BUILD})"
        ));
    }
    // Only a definite "not composing" counts; a failed query does not.
    if dwm_composing() == Some(false) {
        return Some("desktop composition is off".to_string());
    }
    if remote_session() {
        return Some("remote desktop session".to_string());
    }
    if let Some(marker) = streamed_computer() {
        return Some(format!("streamed computer ({marker})"));
    }
    virtual_display().map(|adapter| format!("virtual display adapter ({adapter})"))
}

/// Why capture exclusion would draw `hwnd` as a black box right now, or
/// `None` when it is safe. The session is read again on every call, since it
/// can move between the console and Remote Desktop while the app runs; what
/// cannot change while the app runs (build, BIOS strings, our session id) is
/// read once. Only the adapter behind the window's own monitor counts, so a
/// spare virtual display does not put the clipboard picker on a physical
/// monitor into recordings.
#[cfg(target_os = "windows")]
pub(crate) fn blocker(hwnd: windows::Win32::Foundation::HWND) -> Option<String> {
    use std::sync::OnceLock;
    static BUILD: OnceLock<Option<u32>> = OnceLock::new();
    static STREAMED: OnceLock<Option<String>> = OnceLock::new();
    let reason = blocker_with(
        std::env::var(ENV_OVERRIDE).ok().as_deref(),
        *BUILD.get_or_init(crate::commands::windows_build),
        probes::dwm_composing,
        probes::remote_session,
        || STREAMED.get_or_init(probes::streamed_computer).clone(),
        || probes::virtual_display(hwnd),
    );
    log_change(&reason);
    reason
}

/// Logs the decision whenever it changes, so support logs say why the toolbar
/// was recorded without repeating it on every dock show.
#[cfg(target_os = "windows")]
fn log_change(reason: &Option<String>) {
    static LAST: parking_lot::Mutex<Option<Option<String>>> = parking_lot::Mutex::new(None);
    let mut last = LAST.lock();
    if last.as_ref() == Some(reason) {
        return;
    }
    match reason {
        Some(reason) => tracing::info!(
            %reason,
            "capture exclusion off: Clipy's overlays stay visible and are recorded"
        ),
        None => tracing::info!("capture exclusion on: Clipy's overlays are left out of recordings"),
    }
    *last = Some(reason.clone());
}

#[cfg(target_os = "windows")]
mod probes {
    use windows::core::{w, PCWSTR};
    use windows::Win32::Foundation::{ERROR_SUCCESS, HWND};
    use windows::Win32::Graphics::Dwm::DwmIsCompositionEnabled;
    use windows::Win32::Graphics::Gdi::{
        EnumDisplayDevicesW, GetMonitorInfoW, MonitorFromWindow, DISPLAY_DEVICEW,
        DISPLAY_DEVICE_ATTACHED_TO_DESKTOP, MONITORINFO, MONITORINFOEXW, MONITOR_DEFAULTTONEAREST,
    };
    use windows::Win32::System::Registry::{
        RegGetValueW, HKEY_LOCAL_MACHINE, RRF_RT_REG_DWORD, RRF_RT_REG_SZ,
    };
    use windows::Win32::System::RemoteDesktop::ProcessIdToSessionId;
    use windows::Win32::System::Threading::GetCurrentProcessId;
    use windows::Win32::UI::WindowsAndMessaging::{GetSystemMetrics, SM_REMOTESESSION};

    use super::{find_marker, STREAMED_COMPUTER_MARKERS, VIRTUAL_DISPLAY_MARKERS};

    pub(super) fn dwm_composing() -> Option<bool> {
        // SAFETY: Category 8 — FFI contract. No arguments; returns a BOOL.
        unsafe { DwmIsCompositionEnabled() }
            .ok()
            .map(|enabled| enabled.as_bool())
    }

    /// A Remote Desktop session, including the RemoteFX vGPU sessions that
    /// `SM_REMOTESESSION` reports as local: Microsoft's documented check is
    /// that our session is not the one attached to the physical console
    /// (`GlassSessionId`).
    pub(super) fn remote_session() -> bool {
        // SAFETY: Category 8 — FFI contract. Reads one system metric.
        if unsafe { GetSystemMetrics(SM_REMOTESESSION) } != 0 {
            return true;
        }
        let mut glass: u32 = 0;
        let mut size = std::mem::size_of::<u32>() as u32;
        // SAFETY: Category 8 — FFI contract. `size` is the byte length of
        // `glass`, which RegGetValueW fills with a REG_DWORD.
        let read = unsafe {
            RegGetValueW(
                HKEY_LOCAL_MACHINE,
                w!(r"SYSTEM\CurrentControlSet\Control\Terminal Server"),
                w!("GlassSessionId"),
                RRF_RT_REG_DWORD,
                None,
                Some((&mut glass as *mut u32).cast()),
                Some(&mut size),
            )
        };
        if read != ERROR_SUCCESS {
            return false;
        }
        // A process never changes session, so this is asked once.
        static SESSION: std::sync::OnceLock<Option<u32>> = std::sync::OnceLock::new();
        let session = *SESSION.get_or_init(|| {
            let mut session: u32 = 0;
            // SAFETY: Category 8 — FFI contract. `session` is a valid out
            // pointer.
            unsafe { ProcessIdToSessionId(GetCurrentProcessId(), &mut session) }
                .ok()
                .map(|()| session)
        });
        let Some(session) = session else {
            return false;
        };
        session != glass
    }

    pub(super) fn streamed_computer() -> Option<String> {
        [
            "SystemManufacturer",
            "SystemProductName",
            "SystemFamily",
            "BIOSVendor",
        ]
        .into_iter()
        .find_map(|name| {
            let text = bios_value(name)?;
            let marker = find_marker(&text, STREAMED_COMPUTER_MARKERS)?;
            Some(format!("{name} \"{text}\" matched \"{marker}\""))
        })
    }

    fn bios_value(name: &str) -> Option<String> {
        let name: Vec<u16> = name.encode_utf16().chain([0]).collect();
        let mut buffer = [0u16; 256];
        let mut size = std::mem::size_of_val(&buffer) as u32;
        // SAFETY: Category 8 — FFI contract. `size` is the byte length of
        // `buffer`, which RegGetValueW fills with a NUL-terminated string; a
        // longer value fails with ERROR_MORE_DATA and is skipped.
        let read = unsafe {
            RegGetValueW(
                HKEY_LOCAL_MACHINE,
                w!(r"HARDWARE\DESCRIPTION\System\BIOS"),
                PCWSTR(name.as_ptr()),
                RRF_RT_REG_SZ,
                None,
                Some(buffer.as_mut_ptr().cast()),
                Some(&mut size),
            )
        };
        if read != ERROR_SUCCESS {
            return None;
        }
        let chars = (size as usize / 2).min(buffer.len());
        Some(wide_text(&buffer[..chars]))
    }

    /// The display adapter behind the monitor `hwnd` is on, when its name
    /// marks it as streamed or virtual. If the monitor cannot be resolved,
    /// every adapter attached to the desktop is checked, as Cap does.
    pub(super) fn virtual_display(hwnd: HWND) -> Option<String> {
        let monitor_device = monitor_device(hwnd);
        for index in 0u32.. {
            let mut device = DISPLAY_DEVICEW {
                cb: std::mem::size_of::<DISPLAY_DEVICEW>() as u32,
                ..Default::default()
            };
            // SAFETY: Category 8 — FFI contract. `device.cb` is set to the
            // struct's size, as EnumDisplayDevicesW requires; it fails once
            // `index` passes the last adapter.
            if !unsafe { EnumDisplayDevicesW(PCWSTR::null(), index, &mut device, 0) }.as_bool() {
                return None;
            }
            if device.StateFlags & DISPLAY_DEVICE_ATTACHED_TO_DESKTOP == 0 {
                continue;
            }
            if monitor_device
                .as_ref()
                .is_some_and(|wanted| *wanted != wide_text(&device.DeviceName))
            {
                continue;
            }
            let name = wide_text(&device.DeviceString);
            if let Some(marker) = find_marker(&name, VIRTUAL_DISPLAY_MARKERS) {
                return Some(format!("\"{name}\" matched \"{marker}\""));
            }
        }
        None
    }

    /// The GDI device name (`\\.\DISPLAY2`) of the monitor `hwnd` is on.
    fn monitor_device(hwnd: HWND) -> Option<String> {
        // SAFETY: Category 8 — FFI contract. `hwnd` is a live window of this
        // process; NEAREST always yields a monitor for a valid window.
        let monitor = unsafe { MonitorFromWindow(hwnd, MONITOR_DEFAULTTONEAREST) };
        if monitor.is_invalid() {
            return None;
        }
        let mut info = MONITORINFOEXW::default();
        info.monitorInfo.cbSize = std::mem::size_of::<MONITORINFOEXW>() as u32;
        // SAFETY: Category 8 — FFI contract. `cbSize` declares the extended
        // struct, so GetMonitorInfoW may fill `szDevice`; the pointer cast is
        // the documented way to pass MONITORINFOEXW.
        let ok = unsafe {
            GetMonitorInfoW(
                monitor,
                (&mut info as *mut MONITORINFOEXW).cast::<MONITORINFO>(),
            )
        };
        ok.as_bool()
            .then(|| wide_text(&info.szDevice))
            .filter(|name| !name.is_empty())
    }

    fn wide_text(value: &[u16]) -> String {
        let end = value.iter().position(|c| *c == 0).unwrap_or(value.len());
        String::from_utf16_lossy(&value[..end]).trim().to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn untouched<T>() -> T {
        panic!("an earlier check should have answered")
    }

    #[test]
    fn healthy_windows_keeps_exclusion() {
        for build in [Some(19041), Some(19045), Some(22631), Some(26100), None] {
            assert_eq!(
                blocker_with(None, build, || Some(true), || false, || None, || None),
                None,
                "{build:?}"
            );
        }
    }

    #[test]
    fn builds_before_2004_never_exclude() {
        for build in [18362, 18363, 19040] {
            let reason = blocker_with(
                None,
                Some(build),
                untouched,
                untouched,
                untouched,
                untouched,
            );
            assert!(reason.unwrap().contains(&build.to_string()));
        }
    }

    #[test]
    fn remote_desktop_never_excludes() {
        assert_eq!(
            blocker_with(
                None,
                Some(19045),
                || Some(true),
                || true,
                untouched,
                untouched
            ),
            Some("remote desktop session".to_string())
        );
    }

    #[test]
    fn composition_off_never_excludes_but_an_unknown_answer_does() {
        assert_eq!(
            blocker_with(
                None,
                Some(19045),
                || Some(false),
                untouched,
                untouched,
                untouched
            ),
            Some("desktop composition is off".to_string())
        );
        assert_eq!(
            blocker_with(None, Some(19045), || None, || false, || None, || None),
            None
        );
    }

    #[test]
    fn streamed_desktops_never_exclude() {
        for computer in ["Shadow", "SHADOW COMPUTER"] {
            assert!(blocker_with(
                None,
                Some(22631),
                || Some(true),
                || false,
                || find_marker(computer, STREAMED_COMPUTER_MARKERS).map(str::to_string),
                untouched,
            )
            .is_some());
        }
        for adapter in [
            "Parsec Virtual Display Adapter",
            "spacedesk Graphics Adapter",
            "IddSampleDriver Device",
            "Virtual Display Driver",
            "usbmmidd",
            "Amyuni",
        ] {
            assert!(
                blocker_with(
                    None,
                    Some(22631),
                    || Some(true),
                    || false,
                    || None,
                    || find_marker(adapter, VIRTUAL_DISPLAY_MARKERS).map(str::to_string),
                )
                .is_some(),
                "{adapter}"
            );
        }
    }

    #[test]
    fn plain_vms_and_real_hardware_keep_exclusion() {
        for name in [
            "Microsoft Corporation Virtual Machine",
            "Microsoft Hyper-V Video",
            "VMware SVGA 3D",
            "VirtualBox Graphics Adapter (WDDM)",
            "QEMU Standard PC (Q35 + ICH9, 2009)",
            "Amazon EC2",
            "Microsoft Basic Display Adapter",
            "Microsoft Remote Display Adapter",
            "NVIDIA GeForce RTX 3080",
            "Intel(R) UHD Graphics 770",
            "Dell Inc.",
            "LENOVO",
        ] {
            assert_eq!(find_marker(name, STREAMED_COMPUTER_MARKERS), None, "{name}");
            assert_eq!(find_marker(name, VIRTUAL_DISPLAY_MARKERS), None, "{name}");
        }
    }

    #[test]
    fn override_wins_without_probing() {
        for value in ["on", " ALWAYS ", "1"] {
            assert_eq!(
                blocker_with(
                    Some(value),
                    Some(18362),
                    untouched,
                    untouched,
                    untouched,
                    untouched
                ),
                None,
                "{value}"
            );
        }
        for value in ["off", "Never", "0"] {
            assert_eq!(
                blocker_with(
                    Some(value),
                    Some(22631),
                    untouched,
                    untouched,
                    untouched,
                    untouched
                ),
                Some(format!("{ENV_OVERRIDE} override")),
                "{value}"
            );
        }
    }

    #[test]
    fn unknown_override_falls_back_to_detection() {
        for value in ["", "auto", "yes please"] {
            assert_eq!(parse_override(value), None, "{value}");
            assert_eq!(
                blocker_with(
                    Some(value),
                    Some(19045),
                    || Some(true),
                    || true,
                    untouched,
                    untouched
                ),
                Some("remote desktop session".to_string())
            );
        }
    }
}
