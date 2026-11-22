//! Windows screen + audio capture.
//!
//! Architecture:
//!   * Video — Windows Graphics Capture (WGC) via the `windows-capture` crate.
//!     Min Windows 10 build 1903. Frames arrive as BGRA8 in a background
//!     dispatcher-queue thread; we strip stride padding and push them down
//!     a tokio channel.
//!   * Mic — cpal's WASAPI host, default input device. Same shape as the
//!     macOS `capture::audio::MicCapturer`.
//!   * System audio — cpal's WASAPI host, default *output* device with
//!     `build_input_stream`. On Windows, calling `build_input_stream` on a
//!     render endpoint sets `AUDCLNT_STREAMFLAGS_LOOPBACK` under the hood,
//!     which is exactly what we want.
//!
//! All three streams stamp their chunks with a shared `std::time::Instant`
//! taken at the first video frame — the Windows analog of the macOS
//! `start_mach` anchor. Recording.rs subtracts that anchor to compute
//! encoder PTS in the encoder's native timebase.
//!
//! Phase B status: video + mic + loopback are wired up; window-source
//! recording (HWND picker) is deliberately out of scope for the first
//! Windows release — only display capture is supported. The UI source
//! picker shows displays + an empty windows list on Windows.

#![cfg(target_os = "windows")]
#![allow(dead_code)]

use std::sync::{
    atomic::{AtomicBool, AtomicU64, Ordering},
    Arc, Mutex as StdMutex,
};
use std::time::Instant;

use anyhow::{anyhow, bail, Context, Result};
use cpal::{
    traits::{DeviceTrait, HostTrait, StreamTrait},
    Sample, SampleFormat, Stream, StreamConfig, SupportedStreamConfigRange,
};
use tokio::sync::mpsc;
use windows_capture::{
    capture::{Context as WgcContext, GraphicsCaptureApiHandler},
    frame::Frame,
    graphics_capture_api::{GraphicsCaptureApi, InternalCaptureControl},
    monitor::Monitor,
    settings::{
        ColorFormat, CursorCaptureSettings, DirtyRegionSettings, DrawBorderSettings,
        MinimumUpdateIntervalSettings, SecondaryWindowSettings, Settings,
    },
};

use windows_capture::window::Window;

use crate::capture::failure::{wgc_start_failure, CaptureFailure};
use crate::cursor_track::CaptureViewport;
use crate::types::{CaptureSourceDto, CaptureSourcesDto, DisplayInfoDto, WindowInfoDto};

/// Maps the `u32` ids the UI round-trips onto real `HWND` handles.
///
/// `CaptureSourceDto::Window` carries a `u32`, but an `HWND` is a 64-bit
/// pointer, so the handle cannot be the id. A positional index into the
/// enumeration would be worse than useless — windows open and close between
/// listing and Start, so an index can silently resolve to a DIFFERENT window
/// than the user picked, which is a privacy failure, not a glitch.
///
/// Ids are therefore allocated once per enumeration and the handle stored
/// here; `start` looks the handle up and re-validates it before capturing.
static WINDOW_REGISTRY: std::sync::OnceLock<StdMutex<std::collections::HashMap<u32, isize>>> =
    std::sync::OnceLock::new();

fn window_registry() -> &'static StdMutex<std::collections::HashMap<u32, isize>> {
    WINDOW_REGISTRY.get_or_init(|| StdMutex::new(std::collections::HashMap::new()))
}

/// Resolve a picker id back to its `HWND`, for callers outside this module
/// (the thumbnail path). Returns `None` once the id is stale, so a caller can
/// never accidentally act on a different window than the user chose.
pub fn hwnd_for_source_id(id: u32) -> Option<isize> {
    window_registry()
        .lock()
        .expect("window registry poisoned")
        .get(&id)
        .copied()
}

/// Resolve a picker id to a window that still exists and is not hidden.
/// It does not look at whether the window is minimised: the pre-countdown
/// check restores a minimised window, and `live_window_for_source_id`
/// refuses one that is still minimised when capture starts.
fn resolve_window_for_source_id(id: u32) -> Result<(isize, Window)> {
    let hwnd = hwnd_for_source_id(id).ok_or_else(|| {
        anyhow::Error::new(CaptureFailure::new(
            "source_gone",
            "That window is no longer in the list. Pick it again, then retry.",
        ))
    })?;
    let window = Window::from_raw_hwnd(hwnd as *mut std::ffi::c_void);
    if !window.is_valid() {
        return Err(anyhow::Error::new(CaptureFailure::new(
            "source_gone",
            "That window has closed or can no longer be recorded. Pick it again, then retry.",
        )));
    }
    // Hidden since it was picked (suspended, or moved to another desktop):
    // the crop would record whatever now sits in its rectangle.
    if is_cloaked(window.as_raw_hwnd()) {
        return Err(anyhow::Error::new(CaptureFailure::new(
            "source_gone",
            "That window is hidden or on another desktop. Bring it back, then retry.",
        )));
    }
    Ok((hwnd, window))
}

fn live_window_for_source_id(id: u32) -> Result<(isize, Window)> {
    let (hwnd, window) = resolve_window_for_source_id(id)?;
    // Still minimised when capture starts: the pre-countdown restore failed,
    // or it was minimised again during the countdown. `is_valid` does not
    // catch this: a minimised window keeps WS_VISIBLE, so `IsWindowVisible`
    // still says yes, and DWM does not report it as cloaked either. Its `GetWindowRect` is the
    // off-screen placeholder Windows parks minimised windows at (about
    // -32000, -32000), which the crop used to clamp to a 2x2 corner of the
    // display: a recording of one flat colour. Refuse it instead.
    if is_minimized(window.as_raw_hwnd()) {
        return Err(anyhow::Error::new(CaptureFailure::new(
            "window_minimized",
            "That window is minimised. Restore it on screen, then retry.",
        )));
    }
    Ok((hwnd, window))
}

/// Refuse a start whose picked window has already closed, before the
/// countdown runs. Without it the user watched 3, 2, 1 for a recording that
/// was always going to fail. Other sources are checked when capture starts.
///
/// A minimised window is restored here rather than refused: picking it is a
/// clear request to record it, and the crop is read from its bounds when
/// capture starts, after the countdown, by which time it is back on screen.
/// If it cannot be restored, the check below still refuses it with a reason.
pub fn check_source_available(source: Option<&CaptureSourceDto>) -> Result<()> {
    if let Some(CaptureSourceDto::Window { id }) = source {
        let (_, window) = resolve_window_for_source_id(*id)?;
        if is_minimized(window.as_raw_hwnd()) {
            restore_minimized_window(window.as_raw_hwnd());
        }
        live_window_for_source_id(*id)?;
    }
    Ok(())
}

/// Bring a minimised window back on screen and in front, so the display
/// crop taken from its bounds shows it. Best effort: Windows may refuse the
/// foreground change when Clipy is not the foreground app (a hotkey start),
/// in which case the window is restored but can sit behind others, which is
/// the same as recording any other window that is partly covered.
fn restore_minimized_window(hwnd: *mut std::ffi::c_void) {
    use windows::Win32::Foundation::HWND;
    use windows::Win32::UI::WindowsAndMessaging::{SetForegroundWindow, ShowWindow, SW_RESTORE};

    // SAFETY: Category 8 — FFI contract. `hwnd` comes from the live
    // enumeration and was just validated; a handle that closed in between
    // makes both calls fail harmlessly.
    unsafe {
        let _ = ShowWindow(HWND(hwnd), SW_RESTORE);
        let _ = SetForegroundWindow(HWND(hwnd));
    }
    // ShowWindow returns once the window is restored, but the restore
    // animation is still drawing. A start with no countdown reads the crop
    // and grabs frames straight away, so wait for the animation (about
    // 250 ms) rather than record the window flying back into place.
    let deadline = std::time::Instant::now() + std::time::Duration::from_millis(500);
    while is_minimized(hwnd) && std::time::Instant::now() < deadline {
        std::thread::sleep(std::time::Duration::from_millis(25));
    }
    std::thread::sleep(std::time::Duration::from_millis(300));
    tracing::info!(
        restored = !is_minimized(hwnd),
        "restored a minimised window before recording it"
    );
}

/// True for a window the compositor hides although it reports itself
/// visible: a suspended Store app, a window on another virtual desktop, or
/// a shell host such as TextInputHost.exe, the touch keyboard and emoji
/// panel. Alt+Tab skips these, and so does the picker; recording one would
/// capture whatever happens to sit in its rectangle.
fn is_cloaked(hwnd: *mut std::ffi::c_void) -> bool {
    use windows::Win32::Foundation::HWND;
    use windows::Win32::Graphics::Dwm::{DwmGetWindowAttribute, DWMWA_CLOAKED};

    let mut cloaked: u32 = 0;
    // SAFETY: `hwnd` comes from the live enumeration, and the call only
    // writes one u32 into `cloaked`, whose size is passed alongside it.
    let result = unsafe {
        DwmGetWindowAttribute(
            HWND(hwnd),
            DWMWA_CLOAKED,
            (&mut cloaked as *mut u32).cast(),
            std::mem::size_of::<u32>() as u32,
        )
    };
    // A window whose state cannot be read stays listed, as it was before
    // this check existed.
    result.is_ok() && cloaked != 0
}

/// True for a minimised window. Its `GetWindowRect` is the parking rectangle
/// Windows gives minimised windows, far off the virtual desktop, so no crop
/// derived from it describes anything the user can see.
fn is_minimized(hwnd: *mut std::ffi::c_void) -> bool {
    use windows::Win32::Foundation::HWND;
    use windows::Win32::UI::WindowsAndMessaging::IsIconic;

    // SAFETY: Category 8 — FFI contract. `hwnd` comes from the live
    // enumeration and `IsIconic` only reads the window's state.
    unsafe { IsIconic(HWND(hwnd)).as_bool() }
}

/// Display ids handed to the picker, kept stable for the life of the process.
///
/// Ids used to be enumeration indices, and unplugging a display shifts every
/// later one down a slot. The picker keeps its selection by numeric id across
/// refreshes, and thumbnails and the recorder resolve ids after the list was
/// built, so a positional id could silently come to mean a DIFFERENT display
/// than the one the user picked. Each display is therefore given an id the
/// first time it is listed, keyed by its device name (`\\.\DISPLAY2`), and
/// keeps that id; a display that goes away takes its id with it, and one that
/// comes back gets the same id again.
///
/// `None` until the first `list_sources`; callers then fall back to the
/// enumeration index, which is what the first listing assigns anyway.
static DISPLAY_REGISTRY: std::sync::OnceLock<StdMutex<Option<DisplayIds>>> =
    std::sync::OnceLock::new();

/// How a listed display id finds its monitor again.
#[derive(Clone)]
enum DisplayKey {
    /// By device name: that display, or nothing once it is gone.
    Device(String),
    /// Windows would not report a device name; by enumeration position, the
    /// best that is left.
    Index(usize),
}

#[derive(Default)]
struct DisplayIds {
    /// Every device name listed this session and the id it was given. Never
    /// shrinks, so a reconnected display gets its old id back and no id is
    /// ever reused for another display.
    by_device: std::collections::HashMap<String, u32>,
    /// The displays in the latest list.
    current: std::collections::HashMap<u32, DisplayKey>,
}

impl DisplayIds {
    /// Assign ids for one enumeration, in enumeration order. `devices[i]` is
    /// the device name of the display at index `i`, if Windows reported one.
    ///
    /// A new display takes its own index when that is free (so the first
    /// listing numbers displays 0, 1, 2 as before) and otherwise the lowest
    /// id no display has used.
    fn assign(&mut self, devices: &[Option<String>]) -> Vec<u32> {
        let mut taken: std::collections::HashSet<u32> = self.by_device.values().copied().collect();
        let mut current = std::collections::HashMap::new();
        let mut ids = Vec::with_capacity(devices.len());
        // `taken` already holds every known display's id, so a new display
        // cannot take one that a known display later in the list owns.
        let known: Vec<Option<u32>> = devices
            .iter()
            .map(|d| d.as_ref().and_then(|d| self.by_device.get(d).copied()))
            .collect();
        for (idx, device) in devices.iter().enumerate() {
            let id = match known[idx] {
                Some(id) => id,
                None => {
                    let preferred = idx as u32;
                    let id = if taken.contains(&preferred) {
                        (0..).find(|id| !taken.contains(id)).unwrap_or(preferred)
                    } else {
                        preferred
                    };
                    taken.insert(id);
                    if let Some(device) = device {
                        self.by_device.insert(device.clone(), id);
                    }
                    id
                }
            };
            current.insert(
                id,
                match device {
                    Some(device) => DisplayKey::Device(device.clone()),
                    None => DisplayKey::Index(idx),
                },
            );
            ids.push(id);
        }
        self.current = current;
        ids
    }
}

fn display_registry() -> &'static StdMutex<Option<DisplayIds>> {
    DISPLAY_REGISTRY.get_or_init(|| StdMutex::new(None))
}

const TARGET_SAMPLE_RATE: u32 = 48_000;
const TARGET_CHANNELS: u16 = 2;

/// A captured BGRA frame stripped of GPU stride padding. `recording.rs`
/// wraps the buffer in an `ffmpeg::frame::Video` with `Pixel::BGRA` and
/// passes it to `Mp4Encoder::queue_video`.
///
/// `width`/`height` describe THIS BUFFER — read from the arriving WGC frame,
/// never from the desired output size. WGC does not scale, so frames arrive at
/// native resolution; labelling them with a smaller target size made the row
/// copy use the wrong stride (sheared + cropped picture). Resizing happens
/// downstream in `WinVideoScaler`.
#[derive(Debug)]
pub struct VideoFrame {
    pub bgra: Vec<u8>,
    pub width: u32,
    pub height: u32,
    /// Wall-clock instant the frame was dequeued by the WGC callback.
    /// Subtract `started_at` (held in `ScreenCaptureHandles`) to get an
    /// offset for encoder PTS.
    pub captured_at: Instant,
}

/// Interleaved f32 PCM chunk from one of the cpal streams (mic or
/// system-audio loopback).
#[derive(Debug)]
pub struct AudioChunk {
    pub samples: Vec<f32>,
    pub sample_rate: u32,
    pub channels: u16,
    pub captured_at: Instant,
    /// Which stream of the source produced it. Zero for a source that is
    /// never reopened; a recording's mic counts up on each `MicSupervisor`
    /// rebuild, so the pump can re-anchor that lane on the first chunk of a
    /// new stream instead of placing it straight after the old one.
    pub generation: u32,
}

/// Capture handles handed to the recording pump. Shape parallels
/// `capture::macos::ScreenCaptureHandles` — `recording.rs` keys off the
/// fields directly.
/// Sub-rectangle of the captured surface to keep, in source pixels.
///
/// WGC cannot capture a region — its frame pool is bound to a whole display or
/// window — so region recording captures the display and crops each frame on
/// the way to the encoder.
#[derive(Debug, Clone, Copy)]
pub struct CropRect {
    pub x: u32,
    pub y: u32,
    pub width: u32,
    pub height: u32,
}

pub struct ScreenCaptureHandles {
    pub video_rx: mpsc::Receiver<VideoFrame>,
    pub width: u32,
    pub height: u32,
    /// `Some` only for region capture. Applied before scaling.
    pub crop: Option<CropRect>,
    /// `None` for camera-only recordings: there is no screen for the cursor,
    /// Flare or the camera bubble to map onto.
    pub capture: Option<CaptureViewport>,
    /// Anchor for PTS arithmetic — first captured frame's `Instant`. The
    /// pump subtracts this from each frame/chunk's `captured_at`.
    pub started_at: Instant,
}

/// Lifetime owner for the WGC capture session. Drop terminates the
/// dispatcher-queue thread the `windows-capture` crate spawned for us.
pub struct ScreenCapturer {
    stop: Option<Box<dyn FnOnce() + Send>>,
}

// SAFETY: `stop` is a one-shot trampoline invoked from the recording
// controller's tokio task. Same pattern as the mac counterpart.
unsafe impl Send for ScreenCapturer {}
unsafe impl Sync for ScreenCapturer {}

impl ScreenCapturer {
    /// A capture session owned by something other than WGC (the camera).
    pub(crate) fn from_stop(stop: Box<dyn FnOnce() + Send>) -> Self {
        Self { stop: Some(stop) }
    }
}

/// Flags struct moved into the WGC handler via the `windows-capture`
/// `Context<Flags>` plumbing.
#[derive(Clone)]
struct WgcFlags {
    video_tx: mpsc::Sender<VideoFrame>,
    started_at: Arc<StdMutex<Option<Instant>>>,
}

/// Inner handler the `windows-capture` crate drives.
///
/// Deliberately carries NO target width/height — stamping those onto
/// natively-sized frames is what corrupted the picture.
struct WgcHandler {
    video_tx: mpsc::Sender<VideoFrame>,
    started_at: Arc<StdMutex<Option<Instant>>>,
    frame_count: u64,
}

impl GraphicsCaptureApiHandler for WgcHandler {
    type Flags = WgcFlags;
    type Error = Box<dyn std::error::Error + Send + Sync>;

    fn new(ctx: WgcContext<Self::Flags>) -> Result<Self, Self::Error> {
        let f = ctx.flags;
        Ok(Self {
            video_tx: f.video_tx,
            started_at: f.started_at,
            frame_count: 0,
        })
    }

    fn on_frame_arrived(
        &mut self,
        frame: &mut Frame,
        capture_control: InternalCaptureControl,
    ) -> Result<(), Self::Error> {
        let now = Instant::now();
        // First-frame side-effect: stamp the shared anchor so audio chunks
        // queued before the first video frame still align (see macOS
        // `start_mach` logic).
        {
            let mut anchor = self.started_at.lock().expect("anchor poisoned");
            if anchor.is_none() {
                *anchor = Some(now);
            }
        }

        // Read BEFORE `frame.buffer()`, which borrows the frame mutably.
        let width = frame.width();
        let height = frame.height();
        let expected = (width as usize) * (height as usize) * 4;

        // `as_nopadding_buffer()` strips the GPU stride pad into a
        // contiguous width*height*4 BGRA buffer.
        let mut buffer = match frame.buffer() {
            Ok(b) => b,
            Err(e) => {
                tracing::warn!("WGC frame.buffer() failed: {e:?}");
                return Ok(());
            }
        };
        let raw = match buffer.as_nopadding_buffer() {
            Ok(r) => r,
            Err(e) => {
                tracing::warn!("WGC as_nopadding_buffer() failed: {e:?}");
                return Ok(());
            }
        };

        if raw.len() < expected {
            // Never hand downstream a buffer smaller than its own label says;
            // that is precisely the class of mismatch this code had before.
            if self.frame_count % 30 == 0 {
                tracing::warn!(
                    width,
                    height,
                    got = raw.len(),
                    expected,
                    "WGC frame buffer smaller than its dimensions; dropping"
                );
            }
            self.frame_count = self.frame_count.wrapping_add(1);
            return Ok(());
        }

        let chunk = VideoFrame {
            bgra: raw[..expected].to_vec(),
            width,
            height,
            captured_at: now,
        };

        // try_send so the WGC dispatcher thread is never blocked by a
        // slow encoder. Channel full → drop the frame (logged at 1Hz);
        // channel closed → recording stopped, halt the session.
        if let Err(err) = self.video_tx.try_send(chunk) {
            match err {
                mpsc::error::TrySendError::Full(_) => {
                    if self.frame_count % 30 == 0 {
                        tracing::warn!("WGC video channel full; dropping frame");
                    }
                }
                mpsc::error::TrySendError::Closed(_) => {
                    capture_control.stop();
                }
            }
        }

        self.frame_count = self.frame_count.wrapping_add(1);
        Ok(())
    }

    fn on_closed(&mut self) -> Result<(), Self::Error> {
        tracing::info!(frames = self.frame_count, "WGC capture session closed");
        Ok(())
    }
}

/// WGC refuses to start at all, rather than ignoring the option, when asked
/// for a setting this Windows build does not have. Hiding the capture border
/// needs `IsBorderRequired` (Windows 11), and choosing cursor capture needs
/// `IsCursorCaptureEnabled` (Windows 10 2004). Window and Custom size takes
/// always asked to hide the border, so on every Windows 10 machine they failed
/// before a frame arrived and reached the funnel as `capture_unavailable`.
/// Ask only for what the OS supports; the defaults are a yellow border drawn
/// by Windows (never part of the captured pixels) and the cursor included.
fn border_setting(hide_border: bool) -> DrawBorderSettings {
    if hide_border && GraphicsCaptureApi::is_border_settings_supported().unwrap_or(false) {
        DrawBorderSettings::WithoutBorder
    } else {
        DrawBorderSettings::Default
    }
}

fn cursor_setting() -> CursorCaptureSettings {
    if GraphicsCaptureApi::is_cursor_settings_supported().unwrap_or(false) {
        CursorCaptureSettings::WithCursor
    } else {
        CursorCaptureSettings::Default
    }
}

/// Refuse up front, with a message that says what to do, when this Windows
/// build has no Graphics Capture at all. A probe that errors is not treated
/// as a refusal: WGC itself still gets to decide.
fn ensure_wgc_supported() -> Result<()> {
    if GraphicsCaptureApi::is_supported().unwrap_or(true) {
        return Ok(());
    }
    Err(anyhow::Error::new(wgc_start_failure("Unsupported")))
}

/// WGC started but sent nothing: the screen is off or locked, the source is
/// hidden, or a policy blocks capture.
fn no_frames_failure() -> anyhow::Error {
    anyhow::Error::new(CaptureFailure::new(
        "wgc_no_frames",
        "Windows started screen capture but sent no picture. Make sure the screen \
         is on and unlocked, then retry; some work computers block screen recording.",
    ))
}

/// Find the display a window sits on, plus that display's origin on the
/// virtual desktop.
///
/// `GetWindowRect` and `MONITORINFO::rcMonitor` are both in virtual-desktop
/// coordinates, so the origin is what converts one into the other. WGC's own
/// `Monitor` type exposes neither a position nor an `HWND`-to-monitor lookup,
/// which is why this goes to Win32 directly and then rebuilds the capture item
/// from the raw `HMONITOR`.
#[cfg(target_os = "windows")]
fn monitor_for_window(hwnd: isize) -> Result<(Monitor, (i32, i32))> {
    use windows::Win32::Foundation::HWND;
    use windows::Win32::Graphics::Gdi::{
        GetMonitorInfoW, MonitorFromWindow, MONITORINFO, MONITOR_DEFAULTTONEAREST,
    };

    // SAFETY: Category 8 — FFI contract. `hwnd` was re-validated by the caller
    // via `Window::is_valid`; `info` is fully initialised with its own size
    // before the call, as `GetMonitorInfoW` requires.
    unsafe {
        let handle = MonitorFromWindow(
            HWND(hwnd as *mut std::ffi::c_void),
            MONITOR_DEFAULTTONEAREST,
        );
        if handle.is_invalid() {
            bail!("could not resolve which display that window is on");
        }
        let mut info = MONITORINFO {
            cbSize: std::mem::size_of::<MONITORINFO>() as u32,
            ..Default::default()
        };
        if !GetMonitorInfoW(handle, &mut info).as_bool() {
            bail!("could not read the bounds of that window's display");
        }
        Ok((
            Monitor::from_raw_hmonitor(handle.0),
            (info.rcMonitor.left, info.rcMonitor.top),
        ))
    }
}

/// A display's rect on the virtual desktop as `(left, top, width, height)`,
/// in physical pixels, or `None` if Windows will not say.
///
/// Origin and size come from the same `rcMonitor`, so they are always in the
/// same units even if a caller runs without per-monitor DPI awareness.
#[cfg(target_os = "windows")]
fn monitor_rect(monitor: &Monitor) -> Option<(i32, i32, u32, u32)> {
    use windows::Win32::Graphics::Gdi::{GetMonitorInfoW, HMONITOR, MONITORINFO};

    let mut info = MONITORINFO {
        cbSize: std::mem::size_of::<MONITORINFO>() as u32,
        ..Default::default()
    };
    // SAFETY: Category 8 — FFI contract. The handle comes from a live WGC
    // `Monitor`, and `info` carries its own size as `GetMonitorInfoW` requires.
    let found = unsafe { GetMonitorInfoW(HMONITOR(monitor.as_raw_hmonitor()), &mut info) };
    if !found.as_bool() {
        return None;
    }
    let r = info.rcMonitor;
    let width = u32::try_from(r.right - r.left).ok()?;
    let height = u32::try_from(r.bottom - r.top).ok()?;
    Some((r.left, r.top, width, height))
}

/// A display's top-left on the virtual desktop, in physical pixels. Falls back
/// to the origin, which is exact for the primary display; the fallback is
/// logged so a wrong position on a secondary display is traceable.
#[cfg(target_os = "windows")]
fn monitor_origin(monitor: &Monitor) -> (i32, i32) {
    match monitor_rect(monitor) {
        Some((x, y, _, _)) => (x, y),
        None => {
            tracing::warn!("GetMonitorInfoW failed; assuming the display is at the origin");
            (0, 0)
        }
    }
}

/// The display the picker calls `display_id`. This is the one place that
/// mapping lives, so the picker's thumbnails and the recorder can never
/// disagree about which display an id means.
///
/// A listed id resolves through `DISPLAY_REGISTRY` to the display it was
/// assigned to, never by position. Before the first `list_sources` in this
/// process, ids are WGC enumeration indices, as the first listing assigns.
#[cfg(target_os = "windows")]
fn monitor_by_id(display_id: u32) -> Result<Monitor> {
    let monitors = Monitor::enumerate().map_err(|e| anyhow!("WGC Monitor::enumerate: {e:?}"))?;
    let key = display_registry()
        .lock()
        .expect("display registry poisoned")
        .as_ref()
        .map(|ids| ids.current.get(&display_id).cloned());
    match key {
        Some(Some(DisplayKey::Device(device))) => monitors
            .into_iter()
            .find(|m| m.device_name().ok().as_deref() == Some(device.as_str()))
            .ok_or_else(|| {
                anyhow::Error::new(CaptureFailure::new(
                    "source_gone",
                    "That display is no longer connected. Pick a screen, then retry.",
                ))
            }),
        Some(Some(DisplayKey::Index(idx))) => monitors
            .get(idx)
            .cloned()
            .ok_or_else(|| anyhow!("display id {display_id} out of range")),
        Some(None) => Err(anyhow::Error::new(CaptureFailure::new(
            "source_gone",
            "That display is no longer in the list. Pick a screen, then retry.",
        ))),
        None => monitors
            .get(display_id as usize)
            .cloned()
            .ok_or_else(|| anyhow!("display id {display_id} out of range")),
    }
}

/// A display's rect on the virtual desktop, in physical pixels. `None` when
/// the id no longer names a display (one was unplugged) or Windows cannot
/// report its bounds; callers must not substitute another display's pixels.
#[cfg(target_os = "windows")]
pub fn display_bounds(display_id: u32) -> Option<(i32, i32, u32, u32)> {
    monitor_rect(&monitor_by_id(display_id).ok()?)
}

/// Turn a window's virtual-desktop rect into a crop in its display's pixel
/// space, clamped to the display.
///
/// A window can hang off the edge of its display or straddle two of them. WGC
/// only ever hands us one display's pixels, so the crop is clamped to what that
/// display actually contains — the recording is then the visible part of the
/// window rather than a frame with garbage along one edge.
#[cfg(target_os = "windows")]
fn crop_for_window_on_monitor(
    // Edges rather than a `RECT`: `Window::rect()` returns windows-capture's
    // `RECT`, from its own pinned `windows` version, which is a distinct type
    // from ours even when the layout is identical.
    rect: (i32, i32, i32, i32),
    origin: (i32, i32),
    monitor: &Monitor,
) -> Result<CropRect> {
    let monitor_w = monitor
        .width()
        .map_err(|e| anyhow!("could not read display width: {e:?}"))?;
    let monitor_h = monitor
        .height()
        .map_err(|e| anyhow!("could not read display height: {e:?}"))?;
    crop_within_display(rect, origin, monitor_w, monitor_h)
}

/// The clamp itself, split out so it can be tested without a real display.
///
/// It refuses a rect that does not meet the display rather than returning the
/// nearest legal crop. Clamping an off-display rect produces a tiny corner
/// crop that the scaler blows up into one flat colour, which reaches the user
/// as a recording of a blank screen with nothing having reported an error —
/// the failure mode the desktop HARD RULE exists to prevent.
#[cfg(target_os = "windows")]
fn crop_within_display(
    rect: (i32, i32, i32, i32),
    origin: (i32, i32),
    monitor_w: u32,
    monitor_h: u32,
) -> Result<CropRect> {
    let (rect_left, rect_top, rect_right, rect_bottom) = rect;
    if monitor_w < 2 || monitor_h < 2 {
        bail!("that window's display reported an unusable size");
    }

    // Intersect in signed display-space first. Saturating at 0 before the
    // intersection would turn a rect that is entirely left of or above the
    // display into a zero-width one sitting at the origin, which is the
    // opposite of a miss.
    let left = (rect_left - origin.0).max(0);
    let top = (rect_top - origin.1).max(0);
    let right = (rect_right - origin.0).min(monitor_w as i32);
    let bottom = (rect_bottom - origin.1).min(monitor_h as i32);

    // Even dimensions: H.264 requires them, and an odd crop would make the
    // scaler round and shift the picture by half a pixel. A window thinner
    // than two pixels on screen is a miss as well: there is nothing to record.
    let width = (right - left).max(0) as u32 & !1;
    let height = (bottom - top).max(0) as u32 & !1;
    if width < 2 || height < 2 {
        return Err(anyhow::Error::new(CaptureFailure::new(
            "window_offscreen",
            "That window is not on screen any more. Bring it into view, then retry.",
        )));
    }

    Ok(CropRect {
        x: left as u32,
        y: top as u32,
        width,
        height,
    })
}

impl ScreenCapturer {
    /// Enumerate displays so the UI picker has options. Windows source
    /// (HWND) capture is deferred — returns an empty `windows` list.
    pub async fn list_sources() -> Result<CaptureSourcesDto> {
        let displays = match Monitor::enumerate() {
            Ok(monitors) => {
                // Only a successful enumeration updates the ids the picker
                // resolves against; a failed one keeps the previous list.
                let devices: Vec<Option<String>> =
                    monitors.iter().map(|m| m.device_name().ok()).collect();
                let ids = display_registry()
                    .lock()
                    .expect("display registry poisoned")
                    .get_or_insert_with(DisplayIds::default)
                    .assign(&devices);
                monitors
                    .into_iter()
                    .zip(ids)
                    .enumerate()
                    .map(|(idx, (m, id))| {
                        let name = m
                            .name()
                            .ok()
                            .filter(|s| !s.trim().is_empty())
                            .unwrap_or_else(|| format!("Display {}", idx + 1));
                        let (x, y) = monitor_origin(&m);
                        DisplayInfoDto {
                            id,
                            name,
                            x: x as f64,
                            y: y as f64,
                            width: m.width().unwrap_or(0),
                            height: m.height().unwrap_or(0),
                        }
                    })
                    .collect::<Vec<_>>()
            }
            Err(err) => {
                tracing::warn!("WGC Monitor::enumerate failed: {err:?}");
                Vec::new()
            }
        };

        // `Window::enumerate()` already drops invisible windows, child
        // windows, tool windows and our own process (see `Window::is_valid`).
        // We additionally require a title and a non-degenerate size, which is
        // what removes the invisible host/shell windows that would otherwise
        // show up as blank rows in the picker, and skip cloaked windows.
        let mut windows = Vec::new();
        let mut registry = std::collections::HashMap::new();
        match Window::enumerate() {
            Ok(found) => {
                for (idx, window) in found.into_iter().enumerate() {
                    if is_cloaked(window.as_raw_hwnd()) {
                        continue;
                    }
                    let title = window.title().unwrap_or_default();
                    if title.trim().is_empty() {
                        continue;
                    }
                    let Ok(rect) = window.rect() else { continue };
                    let width = (rect.right - rect.left).max(0) as u32;
                    let height = (rect.bottom - rect.top).max(0) as u32;
                    if width < 2 || height < 2 {
                        continue;
                    }
                    let id = idx as u32;
                    registry.insert(id, window.as_raw_hwnd() as isize);
                    windows.push(WindowInfoDto {
                        id,
                        app_name: window.process_name().unwrap_or_else(|_| "Unknown".into()),
                        title,
                        x: rect.left as f64,
                        y: rect.top as f64,
                        width,
                        height,
                    });
                }
            }
            Err(err) => tracing::warn!("WGC Window::enumerate failed: {err:?}"),
        }
        *window_registry().lock().expect("window registry poisoned") = registry;

        Ok(CaptureSourcesDto { displays, windows })
    }

    /// Start screen capture against the requested display (or primary if
    /// none). Returns the controller + dispatch handles. System-audio
    /// loopback is started independently by `LoopbackCapturer` in
    /// recording.rs.
    pub async fn start(
        _fps: u32,
        source: Option<&CaptureSourceDto>,
        max_height: Option<u32>,
    ) -> Result<(Self, ScreenCaptureHandles)> {
        ensure_wgc_supported()?;

        // Bounded video channel. Each frame is a full-res BGRA copy (8.3 MB
        // at 1080p), so keep this shallow: a deep queue cannot fix a pump
        // slower than capture, it only buys RAM pressure.
        let (video_tx, video_rx) = mpsc::channel::<VideoFrame>(16);
        let started_at_shared: Arc<StdMutex<Option<Instant>>> = Arc::new(StdMutex::new(None));

        // Window capture is a FIXED DISPLAY CROP taken from the selected
        // window's starting bounds, not a WGC window-capture item.
        //
        // This matches macOS, and it is the documented product decision
        // ("Flare and Camera are available on window captures").
        // The reason is architectural, not stylistic: a WGC window item
        // contains only that window's own pixels, so the camera bubble and the
        // cursor-trail overlay — which are separate top-level windows — can
        // never appear in it. Window recordings came out with no camera at all,
        // however the bubble was configured.
        //
        // The trade-off is the one macOS already makes and documents: the crop
        // does not follow the window. If it is moved, resized or closed, the
        // rectangle stays put and can capture whatever moves into it. The frame
        // handed to the encoder is only ever the crop — full-display pixels are
        // discarded in the pump and never reach the file.
        if let Some(CaptureSourceDto::Window { id }) = source {
            let (hwnd, window) = live_window_for_source_id(*id)?;
            let rect = window
                .rect()
                .map_err(|e| anyhow!("could not read window bounds: {e:?}"))?;

            // Resolve the display the window sits on, and turn its bounds into
            // a crop expressed in that display's own pixel space. `rect` is in
            // virtual-desktop coordinates, which are only the same thing on a
            // single-monitor setup with the display at the origin.
            let (monitor, monitor_origin) = monitor_for_window(hwnd)?;
            let crop = crop_for_window_on_monitor(
                (rect.left, rect.top, rect.right, rect.bottom),
                monitor_origin,
                &monitor,
            )?;
            let src_w = crop.width;
            let src_h = crop.height;
            let (width, height) = capture_dimensions(src_w, src_h, max_height);

            let settings = Settings::new(
                monitor,
                cursor_setting(),
                // The system capture indicator outlines the CAPTURED ITEM, which
                // is now the whole display. Leaving it on would ring the entire
                // screen while only the window's rectangle is being recorded —
                // the same mismatch the region path suppresses it for. Windows
                // 10 cannot hide it; see `border_setting`.
                border_setting(true),
                SecondaryWindowSettings::Default,
                MinimumUpdateIntervalSettings::Default,
                DirtyRegionSettings::Default,
                ColorFormat::Bgra8,
                WgcFlags {
                    video_tx,
                    started_at: Arc::clone(&started_at_shared),
                },
            );
            let control = WgcHandler::start_free_threaded(settings).map_err(|e| {
                tracing::warn!("WGC start_free_threaded (window): {e:?}");
                anyhow::Error::new(wgc_start_failure(&format!("{e:?}")))
            })?;
            let started_at = wait_for_anchor(&started_at_shared, std::time::Duration::from_secs(5))
                .ok_or_else(no_frames_failure)?;
            let stop: Box<dyn FnOnce() + Send> = Box::new(move || {
                if let Err(err) = control.stop() {
                    tracing::warn!("WGC CaptureControl::stop failed: {err:?}");
                }
            });
            return Ok((
                Self { stop: Some(stop) },
                ScreenCaptureHandles {
                    video_rx,
                    width,
                    height,
                    crop: Some(crop),
                    capture: Some(CaptureViewport {
                        x: 0.0,
                        y: 0.0,
                        width: src_w as f64,
                        height: src_h as f64,
                        output_width: width,
                        output_height: height,
                        // Screen-space origin, so cursor positions map into the
                        // captured frame rather than the whole desktop. This is
                        // the CROP's origin on the virtual desktop, which equals
                        // the window's own origin except where the window hangs
                        // off the edge of its display and the crop was clamped.
                        screen_x: Some(monitor_origin.0 as f64 + crop.x as f64),
                        screen_y: Some(monitor_origin.1 as f64 + crop.y as f64),
                        screen_width: Some(src_w as f64),
                        screen_height: Some(src_h as f64),
                    }),
                    started_at,
                },
            ));
        }

        let monitor = match source {
            // The camera is not a WGC item; `capture::windows_camera` reads it.
            Some(CaptureSourceDto::Camera { .. }) => {
                bail!("camera sources are started by capture::windows_camera")
            }
            Some(CaptureSourceDto::Display { id }) => monitor_by_id(*id)?,
            Some(CaptureSourceDto::Window { .. }) => unreachable!("handled above"),
            Some(CaptureSourceDto::Area { display_id, .. }) => {
                // WGC has no region mode, so region recording captures the
                // display the region sits on and crops each frame downstream
                // (see `crop` below). Resolving the display here keeps the
                // capture-item selection in one place.
                monitor_by_id(*display_id)?
            }
            None => Monitor::primary().map_err(|e| anyhow!("WGC Monitor::primary: {e:?}"))?,
        };

        let src_w = monitor.width().unwrap_or(1920).max(2);
        let src_h = monitor.height().unwrap_or(1080).max(2);
        // Cursor positions are virtual-desktop pixels, so a display that is
        // not the primary one needs its own origin added to the crop.
        let monitor_origin = monitor_origin(&monitor);

        // Region capture: clamp the requested rect into the display and derive
        // the encoder size from the CROP, not the display, so a small region
        // is not encoded as a mostly-empty full-screen frame.
        //
        // The UI clamps these against the same `DisplayInfoDto` dimensions that
        // come from `Monitor::width()/height()`, so they are already in the
        // frame's pixel space — no DPI conversion. `queue_win_video` re-clamps
        // against each frame's real size, so a mismatch degrades to a smaller
        // crop rather than reading out of bounds.
        let crop = match source {
            Some(CaptureSourceDto::Area {
                x, y, width, height, ..
            }) => {
                let cx = (x.max(0.0) as u32).min(src_w.saturating_sub(2));
                let cy = (y.max(0.0) as u32).min(src_h.saturating_sub(2));
                // Even dimensions: the H.264 encoder requires them, and the
                // scaler would otherwise round and shift the image.
                let cw = ((width.max(2.0) as u32).min(src_w - cx) & !1).max(2);
                let ch = ((height.max(2.0) as u32).min(src_h - cy) & !1).max(2);
                Some(CropRect {
                    x: cx,
                    y: cy,
                    width: cw,
                    height: ch,
                })
            }
            _ => None,
        };

        let (encode_src_w, encode_src_h) = match crop {
            Some(c) => (c.width, c.height),
            None => (src_w, src_h),
        };
        let (width, height) = capture_dimensions(encode_src_w, encode_src_h, max_height);

        // Windows draws its capture indicator around the CAPTURED ITEM. For a
        // region we capture the whole display and crop afterwards, so the
        // system border would outline the entire screen rather than the region
        // the user chose — misleading about what is actually being recorded.
        // Suppress it there and let Clipy's own highlight window mark the
        // region instead. Full-display capture keeps the system border, where
        // it is accurate.
        let border = border_setting(crop.is_some());

        let settings = Settings::new(
            monitor,
            cursor_setting(),
            border,
            // Added by windows-capture 1.5 (the manifest asked for "1.4", but
            // the lockfile resolved 1.5.0 and Settings::new grew from 5 to 8
            // parameters). Defaults preserve the previous behaviour.
            SecondaryWindowSettings::Default,
            MinimumUpdateIntervalSettings::Default,
            DirtyRegionSettings::Default,
            ColorFormat::Bgra8,
            WgcFlags {
                video_tx,
                started_at: Arc::clone(&started_at_shared),
            },
        );

        let control = WgcHandler::start_free_threaded(settings).map_err(|e| {
            tracing::warn!("WGC start_free_threaded: {e:?}");
            anyhow::Error::new(wgc_start_failure(&format!("{e:?}")))
        })?;

        // Anchor: wait briefly for the first WGC callback to land. Hard
        // deadline so a black-screen / permission-denied capture surfaces
        // fast instead of stalling the UI.
        let started_at = wait_for_anchor(&started_at_shared, std::time::Duration::from_secs(5))
            .ok_or_else(no_frames_failure)?;

        // Store the stop trampoline as a Box<dyn FnOnce> so the typed
        // `CaptureControl<WgcHandler, _>` doesn't escape this function.
        let stop: Box<dyn FnOnce() + Send> = Box::new(move || {
            if let Err(err) = control.stop() {
                tracing::warn!("WGC CaptureControl::stop failed: {err:?}");
            }
        });

        Ok((
            Self { stop: Some(stop) },
            ScreenCaptureHandles {
                video_rx,
                width,
                height,
                crop,
                capture: Some(CaptureViewport {
                    x: crop.map(|c| c.x as f64).unwrap_or(0.0),
                    y: crop.map(|c| c.y as f64).unwrap_or(0.0),
                    width: encode_src_w as f64,
                    height: encode_src_h as f64,
                    output_width: width,
                    output_height: height,
                    // Cursor positions are desktop-space, so the region's
                    // origin must be its screen origin or the pointer lands
                    // in the wrong place in the cropped frame.
                    screen_x: Some(
                        monitor_origin.0 as f64 + crop.map(|c| c.x as f64).unwrap_or(0.0),
                    ),
                    screen_y: Some(
                        monitor_origin.1 as f64 + crop.map(|c| c.y as f64).unwrap_or(0.0),
                    ),
                    screen_width: Some(encode_src_w as f64),
                    screen_height: Some(encode_src_h as f64),
                }),
                started_at,
            },
        ))
    }

    pub async fn stop(mut self) -> Result<()> {
        if let Some(stop) = self.stop.take() {
            stop();
        }
        Ok(())
    }
}

impl Drop for ScreenCapturer {
    fn drop(&mut self) {
        if let Some(stop) = self.stop.take() {
            stop();
        }
    }
}

/// Compute the encoder dimensions. Shared with the macOS backend so both
/// platforms land inside the server's store-as-is box — see
/// `capture::output_size`.
pub(crate) fn capture_dimensions(src_w: u32, src_h: u32, max_height: Option<u32>) -> (u32, u32) {
    crate::capture::output_size::fit_capture_size(src_w, src_h, max_height)
}

fn wait_for_anchor(
    anchor: &Arc<StdMutex<Option<Instant>>>,
    deadline: std::time::Duration,
) -> Option<Instant> {
    let start = Instant::now();
    while start.elapsed() < deadline {
        if let Some(v) = *anchor.lock().expect("anchor poisoned") {
            return Some(v);
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    None
}

// ===========================================================================
// Mic capture (cpal default input device)
// ===========================================================================

pub struct MicCapturer {
    stream: Option<Stream>,
    stop_flag: Arc<AtomicBool>,
    /// Buffers the device has delivered, counted in the cpal callback. Lets
    /// the recorder tell "the mic sends nothing" apart from "the pump is
    /// behind" (`capture::mic_silence`).
    delivered: Arc<AtomicU64>,
    pub device_name: String,
    pub sample_rate: u32,
    pub channels: u16,
}

// SAFETY: cpal Stream isn't Send on Windows; we never move it across
// threads after construction. `stop()` consumes self on the same task
// that constructed it.
unsafe impl Send for MicCapturer {}

impl MicCapturer {
    pub fn start() -> Result<(Self, mpsc::Receiver<AudioChunk>)> {
        Self::start_with_device(None)
    }

    pub fn start_with_device(
        device_id: Option<&str>,
    ) -> Result<(Self, mpsc::Receiver<AudioChunk>)> {
        let host = cpal::default_host();
        let device = match device_id {
            Some(target) => host
                .input_devices()
                .context("enumerate input devices")?
                .find(|d| d.name().map(|n| n == target).unwrap_or(false))
                .ok_or_else(|| anyhow!("requested mic device not found: {target}"))?,
            None => host
                .default_input_device()
                .ok_or_else(|| anyhow!("no default input device available"))?,
        };
        let device_name = device.name().unwrap_or_else(|_| "<unknown>".into());

        let chosen = pick_input_config(&device)?;
        let sample_format = chosen.sample_format();
        let config: StreamConfig = chosen.into();
        let sample_rate = config.sample_rate.0;
        let channels = config.channels;

        if sample_rate != TARGET_SAMPLE_RATE || channels != TARGET_CHANNELS {
            tracing::info!(
                device = %device_name,
                sample_rate,
                channels,
                "mic device does not match 48kHz stereo; the pump resamples and remixes"
            );
        } else {
            tracing::info!(device = %device_name, "Windows mic capture at 48kHz stereo f32");
        }

        let (tx_inner, rx_inner) = mpsc::unbounded_channel::<AudioChunk>();
        let (fwd_tx, fwd_rx) = mpsc::channel::<AudioChunk>(32);
        tokio::spawn(forward(rx_inner, fwd_tx));

        let stop_flag = Arc::new(AtomicBool::new(false));
        let delivered = Arc::new(AtomicU64::new(0));
        let err_fn = |err: cpal::StreamError| match err {
            // Unplugged headset / disabled endpoint: an expected event on a
            // user's machine, not a bug. Kept as a warning (Sentry breadcrumb).
            cpal::StreamError::DeviceNotAvailable => {
                tracing::warn!("cpal mic stream error: {err}")
            }
            _ => tracing::error!("cpal mic stream error: {err}"),
        };

        let stream = build_capture_stream(
            &device,
            &config,
            sample_format,
            tx_inner,
            stop_flag.clone(),
            Some(delivered.clone()),
            None,
            0,
            sample_rate,
            channels,
            err_fn,
        )?;
        stream.play().context("start mic cpal stream")?;

        Ok((
            Self {
                stream: Some(stream),
                stop_flag,
                delivered,
                device_name,
                sample_rate,
                channels,
            },
            fwd_rx,
        ))
    }

    /// Shared count of buffers the device has delivered (see `delivered`).
    pub fn delivered_chunks(&self) -> Arc<AtomicU64> {
        Arc::clone(&self.delivered)
    }

    pub fn stop(mut self) -> Result<()> {
        self.stop_flag.store(true, Ordering::Release);
        if let Some(stream) = self.stream.take() {
            drop(stream);
        }
        tracing::info!("Windows mic capture stopped");
        Ok(())
    }
}

// ===========================================================================
// Supervised mic capture for recordings (beta 1.1.81)
// ===========================================================================
//
// `MicCapturer` opens one stream and keeps it for its whole life, which is
// right for the pre-flight test and the level monitor. A recording uses
// `MicSupervisor` instead: a thread owns the stream and rebuilds it when it
// errors, when the picked device leaves and comes back, when the system
// default changes, and while it delivers only exact digital silence, which is
// what Windows feeds an app whose microphone access was turned off. QA turned
// access off and on during a take on beta 1.1.81 and the mic stayed silent
// for the rest of it. The decisions are `capture::mic_reopen` (tested on
// every OS); this is only the thread that acts on them.
//
// A rebuild needs nothing from the pump: chunks carry their own sample rate
// and channel count (the mic lane resamples per chunk), every stream sends
// into the same channel, and chunks are stamped with the capture clock, so
// the pump sees a short gap on the shared timeline. Each stream's chunks
// carry its generation, and the mic lane re-anchors to the capture clock on
// the first chunk of a new one, so a reopen never shifts the mic against the
// video. The capture pump is never blocked:
// opening and closing happen on this thread only.

const MIC_SUPERVISOR_POLL: std::time::Duration = std::time::Duration::from_millis(250);
const MIC_SUPERVISOR_STOP_STEP: std::time::Duration = std::time::Duration::from_millis(50);

/// One open microphone stream. Created, polled and dropped on the
/// supervisor thread only, so the non-`Send` cpal `Stream` never crosses
/// threads.
struct SupervisedMicStream {
    stream: Stream,
    stop_flag: Arc<AtomicBool>,
    err_flag: Arc<AtomicBool>,
    device_name: String,
    opened_at_ms: u64,
    generation: u32,
}

impl SupervisedMicStream {
    fn close(self) {
        // Callbacks already in flight drop their buffer; WASAPI joins its
        // capture thread when the stream is dropped.
        self.stop_flag.store(true, Ordering::Release);
        drop(self.stream);
    }
}

fn open_supervised_mic(
    device_id: Option<&str>,
    tx: &mpsc::UnboundedSender<AudioChunk>,
    delivered: &Arc<AtomicU64>,
    clock: &Arc<crate::capture::mic_reopen::MicSignalClock>,
    generation: u32,
    error_reported: &Arc<AtomicBool>,
) -> Result<SupervisedMicStream> {
    let host = cpal::default_host();
    let device = match device_id {
        Some(target) => host
            .input_devices()
            .context("enumerate input devices")?
            .find(|d| d.name().map(|n| n == target).unwrap_or(false))
            .ok_or_else(|| anyhow!("requested mic device not found: {target}"))?,
        None => host
            .default_input_device()
            .ok_or_else(|| anyhow!("no default input device available"))?,
    };
    let device_name = device.name().unwrap_or_else(|_| "<unknown>".into());
    let chosen = pick_input_config(&device)?;
    let sample_format = chosen.sample_format();
    let config: StreamConfig = chosen.into();
    let sample_rate = config.sample_rate.0;
    let channels = config.channels;

    let stop_flag = Arc::new(AtomicBool::new(false));
    let err_flag = Arc::new(AtomicBool::new(false));
    let err_stop = Arc::clone(&stop_flag);
    let err_seen = Arc::clone(&err_flag);
    let take_reported = Arc::clone(error_reported);
    let err_fn = move |err: cpal::StreamError| {
        // Take no more buffers from this stream; the supervisor rebuilds it.
        err_stop.store(true, Ordering::Release);
        // Once per stream: an error can repeat on every callback.
        if !err_seen.swap(true, Ordering::AcqRel) {
            // Error level (a Sentry event) once per take; a broken driver
            // that fails every rebuild is a warning after that.
            let first_in_take = !take_reported.swap(true, Ordering::AcqRel);
            match err {
                cpal::StreamError::DeviceNotAvailable => {
                    tracing::warn!("cpal mic stream error (supervisor will rebuild): {err}")
                }
                _ if first_in_take => {
                    tracing::error!("cpal mic stream error (supervisor will rebuild): {err}")
                }
                _ => tracing::warn!("cpal mic stream error again (supervisor will rebuild): {err}"),
            }
        }
    };

    let stream = build_capture_stream(
        &device,
        &config,
        sample_format,
        tx.clone(),
        Arc::clone(&stop_flag),
        Some(Arc::clone(delivered)),
        Some(Arc::clone(clock)),
        generation,
        sample_rate,
        channels,
        err_fn,
    )?;
    stream.play().context("start mic cpal stream")?;
    tracing::info!(
        device = %device_name,
        sample_rate,
        channels,
        generation,
        "Windows mic stream opened"
    );
    Ok(SupervisedMicStream {
        stream,
        stop_flag,
        err_flag,
        device_name,
        opened_at_ms: clock.now_ms(),
        generation,
    })
}

/// The microphone for a recording, rebuilt when it stops working. Same
/// channel and `delivered_chunks` shape as `MicCapturer`.
pub struct MicSupervisor {
    stop: Arc<AtomicBool>,
    delivered: Arc<AtomicU64>,
    stats: Arc<StdMutex<crate::capture::mic_reopen::MicReopenStats>>,
    worker_done: Option<std::sync::mpsc::Receiver<()>>,
}

impl MicSupervisor {
    /// Starts the supervisor thread and tries the first open on it.
    ///
    /// The supervisor runs even when that first open fails (access already
    /// off at Start, the picked mic unplugged, a driver error): it keeps
    /// retrying, so allowing access or plugging the mic in brings audio into
    /// the take. The first open's error comes back beside the supervisor so
    /// the recorder can warn and name the cause (`windows_mic_error_code`
    /// reads the same messages as before). `Err` only when the thread itself
    /// cannot be spawned.
    ///
    /// Blocks for up to `FIRST_OPEN_WAIT` on the first open, so call it off
    /// the async runtime (`spawn_blocking`). A Bluetooth headset switching to
    /// its hands-free profile can take seconds; past the wait the take starts
    /// without an error and the stream joins when it opens.
    pub fn start(
        device_id: Option<&str>,
    ) -> Result<(Self, mpsc::Receiver<AudioChunk>, Option<anyhow::Error>)> {
        const FIRST_OPEN_WAIT: std::time::Duration = std::time::Duration::from_secs(10);
        let (tx_inner, rx_inner) = mpsc::unbounded_channel::<AudioChunk>();
        let (fwd_tx, fwd_rx) = mpsc::channel::<AudioChunk>(32);
        tokio::spawn(forward(rx_inner, fwd_tx));

        let stop = Arc::new(AtomicBool::new(false));
        let delivered = Arc::new(AtomicU64::new(0));
        let stats = Arc::new(StdMutex::new(
            crate::capture::mic_reopen::MicReopenStats::default(),
        ));
        let clock = Arc::new(crate::capture::mic_reopen::MicSignalClock::new());
        let error_reported = Arc::new(AtomicBool::new(false));
        let explicit = device_id.map(str::to_owned);

        let (init_tx, init_rx) = std::sync::mpsc::channel::<Option<anyhow::Error>>();
        let (done_tx, done_rx) = std::sync::mpsc::channel::<()>();
        {
            let stop = Arc::clone(&stop);
            let delivered = Arc::clone(&delivered);
            let stats = Arc::clone(&stats);
            let _worker = std::thread::Builder::new()
                .name("mic-supervisor".into())
                .spawn(move || {
                    let initial = match open_supervised_mic(
                        explicit.as_deref(),
                        &tx_inner,
                        &delivered,
                        &clock,
                        0,
                        &error_reported,
                    ) {
                        Ok(initial) => {
                            let _ = init_tx.send(None);
                            Some(initial)
                        }
                        Err(err) => {
                            let _ = init_tx.send(Some(err));
                            None
                        }
                    };
                    supervise_mic(
                        initial,
                        explicit,
                        tx_inner,
                        delivered,
                        clock,
                        error_reported,
                        stop,
                        stats,
                    );
                    let _ = done_tx.send(());
                })
                .context("spawn mic supervisor thread")?;
        }

        let first_error = match init_rx.recv_timeout(FIRST_OPEN_WAIT) {
            Ok(result) => result,
            Err(_) => {
                tracing::warn!(
                    "Windows mic is slow to open; the take starts and the mic joins when it opens"
                );
                None
            }
        };
        Ok((
            Self {
                stop,
                delivered,
                stats,
                worker_done: Some(done_rx),
            },
            fwd_rx,
            first_error,
        ))
    }

    /// Buffers delivered by whichever stream is open (see `MicCapturer`).
    pub fn delivered_chunks(&self) -> Arc<AtomicU64> {
        Arc::clone(&self.delivered)
    }

    /// Reopens so far, for the recording's diagnostics.
    pub fn reopen_stats(&self) -> Arc<StdMutex<crate::capture::mic_reopen::MicReopenStats>> {
        Arc::clone(&self.stats)
    }

    pub fn stop(mut self) -> Result<()> {
        self.stop.store(true, Ordering::Release);
        if self
            .worker_done
            .take()
            .is_some_and(|done| done.recv_timeout(std::time::Duration::from_secs(2)).is_err())
        {
            tracing::warn!("Windows mic supervisor stop timed out; worker remains cancelled");
        }
        tracing::info!("Windows mic capture stopped");
        Ok(())
    }
}

impl Drop for MicSupervisor {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
    }
}

#[allow(clippy::too_many_arguments)]
fn supervise_mic(
    initial: Option<SupervisedMicStream>,
    explicit: Option<String>,
    tx: mpsc::UnboundedSender<AudioChunk>,
    delivered: Arc<AtomicU64>,
    clock: Arc<crate::capture::mic_reopen::MicSignalClock>,
    error_reported: Arc<AtomicBool>,
    stop: Arc<AtomicBool>,
    stats: Arc<StdMutex<crate::capture::mic_reopen::MicReopenStats>>,
) {
    use crate::capture::mic_reopen::{match_input_device, MicAction, MicObservation, MicReopenPolicy};

    let mut next_generation = 1u32;
    let mut policy = MicReopenPolicy::new();
    if initial.is_none() {
        // The first open failed: the next try waits the normal retry gap.
        policy.record_first_open_failed(clock.now_ms());
    }
    let mut current = initial;
    'outer: loop {
        let mut waited = std::time::Duration::ZERO;
        while waited < MIC_SUPERVISOR_POLL {
            std::thread::sleep(MIC_SUPERVISOR_STOP_STEP);
            waited += MIC_SUPERVISOR_STOP_STEP;
            if stop.load(Ordering::Acquire) {
                break 'outer;
            }
        }

        let host = cpal::default_host();
        let current_name = current.as_ref().map(|c| c.device_name.clone());
        // Where the stream should be, and whether it is there. A failed
        // enumeration is skipped, never read as "the device is gone".
        let (target_present, current_is_target, reopen_on) = match explicit.as_deref() {
            Some(target) => {
                // A name that cannot be read right now is the same as a
                // failed enumeration: dropping it would read as the picked
                // mic being gone and close a working stream.
                let devices: Vec<String> = match host
                    .input_devices()
                    .map_err(|err| err.to_string())
                    .and_then(|devices| {
                        devices
                            .map(|d| d.name().map_err(|err| err.to_string()))
                            .collect::<std::result::Result<Vec<_>, _>>()
                    }) {
                    Ok(devices) => devices,
                    Err(err) => {
                        tracing::debug!("mic supervisor: listing input devices failed: {err}");
                        continue;
                    }
                };
                let matched = match_input_device(&devices, target).map(str::to_owned);
                (
                    matched.is_some(),
                    matched.is_some() && matched == current_name,
                    matched,
                )
            }
            None => {
                let default = host.default_input_device().and_then(|d| d.name().ok());
                (
                    // No default right now: keep a stream that is open.
                    default.is_some() || current.is_some(),
                    default.is_none() || default == current_name,
                    None,
                )
            }
        };

        let now = clock.now_ms();
        let opened_at_ms = current.as_ref().map_or(now, |c| c.opened_at_ms);
        let observation = MicObservation {
            now_ms: now,
            stream_open: current.is_some(),
            errored: current
                .as_ref()
                .is_some_and(|c| c.err_flag.load(Ordering::Acquire)),
            explicit: explicit.is_some(),
            target_present,
            current_is_target,
            access_denied: crate::windows_privacy::status(
                crate::windows_privacy::Capability::Microphone,
            ) == crate::commands::PermissionStatusDto::Denied,
            opened_at_ms,
            last_delivery_ms: clock
                .last_delivery_ms()
                .filter(|at| current.is_some() && *at >= opened_at_ms),
            last_signal_ms: clock.last_signal_ms(),
        };

        match policy.decide(&observation) {
            MicAction::Keep => {}
            MicAction::Close => {
                if let Some(stream) = current.take() {
                    stream.close();
                }
                policy.record_close(now);
                if observation.access_denied {
                    tracing::warn!(
                        "microphone access is off in Windows settings; waiting for it to return"
                    );
                } else {
                    tracing::warn!("selected microphone disconnected; waiting for it to return");
                }
            }
            MicAction::Reopen(reason) => {
                if let Some(stream) = current.take() {
                    stream.close();
                }
                let opened = open_supervised_mic(
                    reopen_on.as_deref(),
                    &tx,
                    &delivered,
                    &clock,
                    next_generation,
                    &error_reported,
                );
                next_generation = next_generation.wrapping_add(1);
                let ok = opened.is_ok();
                match opened {
                    Ok(next) => {
                        if stop.load(Ordering::Acquire) {
                            next.close();
                            break 'outer;
                        }
                        tracing::info!(reason = reason.as_str(), "Windows mic stream reopened");
                        current = Some(next);
                    }
                    Err(err) => {
                        tracing::debug!(
                            reason = reason.as_str(),
                            "Windows mic reopen failed (retrying): {err:#}"
                        );
                    }
                }
                policy.record_attempt(reason, ok, clock.now_ms());
                if let Ok(mut shared) = stats.lock() {
                    *shared = policy.stats().clone();
                }
            }
        }
    }
    if let Some(stream) = current.take() {
        stream.close();
    }
}

/// Result of a short pre-flight microphone test, mirroring the macOS
/// `capture::audio::MicSignalCheck`.
#[derive(Clone, Debug)]
pub struct MicSignalCheck {
    pub device_name: String,
    pub level: f32,
    pub has_input: bool,
}

/// Open the mic briefly and report the loudest level observed.
///
/// Replaces a stub that returned `has_input: true` unconditionally. A check
/// that cannot fail hides the condition it exists to detect, and it also
/// returned instantly — which is why the Test button appeared to flash and
/// vanish without testing anything.
pub async fn check_input_signal_with_updates<F>(
    device_id: Option<&str>,
    duration: std::time::Duration,
    mut on_level: F,
) -> Result<MicSignalCheck>
where
    F: FnMut(f32) + Send,
{
    let requested = device_id.map(str::to_owned);
    // cpal stream construction blocks on COM/WASAPI; keep it off the async
    // executor exactly as the macOS path does.
    let (capturer, mut rx) = tokio::task::spawn_blocking(move || {
        MicCapturer::start_with_device(requested.as_deref())
    })
    .await
    .context("join mic check startup")??;

    let device_name = capturer.device_name.clone();
    let deadline = tokio::time::Instant::now() + duration;
    let mut max_level = 0.0f32;
    let mut last_update = tokio::time::Instant::now() - std::time::Duration::from_millis(100);

    loop {
        tokio::select! {
            _ = tokio::time::sleep_until(deadline) => break,
            maybe_chunk = rx.recv() => {
                let Some(chunk) = maybe_chunk else { break };
                let level = crate::capture::level::samples_level(&chunk.samples);
                max_level = max_level.max(level);
                if last_update.elapsed() >= std::time::Duration::from_millis(50) {
                    on_level(level);
                    last_update = tokio::time::Instant::now();
                }
            }
        }
    }

    if let Err(err) = tokio::task::spawn_blocking(move || capturer.stop())
        .await
        .context("join mic check stop")?
    {
        tracing::warn!("mic signal check stop failed: {err:#}");
    }

    Ok(MicSignalCheck {
        device_name,
        level: max_level,
        has_input: crate::capture::level::level_has_input(max_level),
    })
}

// ===========================================================================
// System-audio loopback (cpal default output device → input stream)
// ===========================================================================
//
// WASAPI exposes loopback by opening the render endpoint in capture mode
// with `AUDCLNT_STREAMFLAGS_LOOPBACK`. cpal's wasapi backend wires this
// up automatically when you call `build_input_stream` on a device whose
// data-flow is Render (output). The chunks we get are bit-identical to
// whatever's mixing out the speakers.

pub struct LoopbackCapturer {
    stream: Option<Stream>,
    stop_flag: Arc<AtomicBool>,
    pub sample_rate: u32,
    pub channels: u16,
}

unsafe impl Send for LoopbackCapturer {}

impl LoopbackCapturer {
    pub fn start() -> Result<(Self, mpsc::Receiver<AudioChunk>)> {
        let host = cpal::default_host();
        let device = host
            .default_output_device()
            .ok_or_else(|| anyhow!("no default output device available for loopback"))?;
        let device_name = device.name().unwrap_or_else(|_| "<unknown>".into());

        // For loopback the cpal "input" config we need actually comes from
        // the render endpoint's output config — that's the format the OS
        // mixes audio into. We just take its config verbatim.
        let supported = device
            .default_output_config()
            .context("default_output_config (loopback)")?;
        let sample_format = supported.sample_format();
        let config: StreamConfig = supported.into();
        let sample_rate = config.sample_rate.0;
        let channels = config.channels;

        if sample_rate != TARGET_SAMPLE_RATE {
            tracing::info!(
                device = %device_name,
                sample_rate,
                "loopback device sample rate is not 48kHz; the pump resamples it"
            );
        }

        let (tx_inner, rx_inner) = mpsc::unbounded_channel::<AudioChunk>();
        let (fwd_tx, fwd_rx) = mpsc::channel::<AudioChunk>(32);
        tokio::spawn(forward(rx_inner, fwd_tx));

        let stop_flag = Arc::new(AtomicBool::new(false));
        let err_fn = |err: cpal::StreamError| match err {
            // Unplugged headset / disabled endpoint: an expected event on a
            // user's machine, not a bug. Kept as a warning (Sentry breadcrumb).
            cpal::StreamError::DeviceNotAvailable => {
                tracing::warn!("cpal loopback stream error: {err}")
            }
            _ => tracing::error!("cpal loopback stream error: {err}"),
        };

        let stream = build_capture_stream(
            &device,
            &config,
            sample_format,
            tx_inner,
            stop_flag.clone(),
            None,
            None,
            0,
            sample_rate,
            channels,
            err_fn,
        )?;
        stream.play().context("start loopback cpal stream")?;

        tracing::info!(
            device = %device_name,
            sample_rate,
            channels,
            "Windows loopback capture started"
        );

        Ok((
            Self {
                stream: Some(stream),
                stop_flag,
                sample_rate,
                channels,
            },
            fwd_rx,
        ))
    }

    pub fn stop(mut self) -> Result<()> {
        self.stop_flag.store(true, Ordering::Release);
        if let Some(stream) = self.stream.take() {
            drop(stream);
        }
        tracing::info!("Windows loopback capture stopped");
        Ok(())
    }
}

// ===========================================================================
// Shared cpal plumbing
// ===========================================================================

pub fn list_input_devices() -> Result<Vec<(String, bool)>> {
    let host = cpal::default_host();
    let default_name = host
        .default_input_device()
        .and_then(|d| d.name().ok())
        .unwrap_or_default();
    let mut out = Vec::new();
    for device in host.input_devices().context("enumerate input devices")? {
        if let Ok(name) = device.name() {
            let is_default = !default_name.is_empty() && name == default_name;
            out.push((name, is_default));
        }
    }
    Ok(out)
}

async fn forward(mut rx: mpsc::UnboundedReceiver<AudioChunk>, tx: mpsc::Sender<AudioChunk>) {
    while let Some(chunk) = rx.recv().await {
        if tx.send(chunk).await.is_err() {
            break;
        }
    }
}

fn pick_input_config(device: &cpal::Device) -> Result<cpal::SupportedStreamConfig> {
    let mut ranges: Vec<SupportedStreamConfigRange> = device
        .supported_input_configs()
        .context("enumerate input configs")?
        .collect();

    if ranges.is_empty() {
        bail!("device exposes no input configs");
    }

    ranges.sort_by_key(|range| {
        let fmt_penalty = if range.sample_format() == SampleFormat::F32 {
            0
        } else {
            10
        };
        let chan_penalty = (range.channels() as i32 - TARGET_CHANNELS as i32).abs();
        let rate_penalty = if range.min_sample_rate().0 <= TARGET_SAMPLE_RATE
            && range.max_sample_rate().0 >= TARGET_SAMPLE_RATE
        {
            0
        } else {
            let mid = (range.min_sample_rate().0 + range.max_sample_rate().0) / 2;
            ((mid as i64) - (TARGET_SAMPLE_RATE as i64)).abs() as i32
        };
        fmt_penalty + chan_penalty + rate_penalty
    });

    let chosen = ranges.into_iter().next().unwrap();
    let rate = if chosen.min_sample_rate().0 <= TARGET_SAMPLE_RATE
        && chosen.max_sample_rate().0 >= TARGET_SAMPLE_RATE
    {
        cpal::SampleRate(TARGET_SAMPLE_RATE)
    } else {
        chosen.max_sample_rate()
    };
    Ok(chosen.with_sample_rate(rate))
}

#[allow(clippy::too_many_arguments)]
fn build_capture_stream<E>(
    device: &cpal::Device,
    config: &StreamConfig,
    sample_format: SampleFormat,
    tx: mpsc::UnboundedSender<AudioChunk>,
    stop_flag: Arc<AtomicBool>,
    delivered: Option<Arc<AtomicU64>>,
    signal: Option<Arc<crate::capture::mic_reopen::MicSignalClock>>,
    generation: u32,
    sample_rate: u32,
    channels: u16,
    err_fn: E,
) -> Result<Stream>
where
    E: FnMut(cpal::StreamError) + Send + 'static,
{
    macro_rules! build_typed {
        ($t:ty) => {{
            let tx = tx.clone();
            let stop_flag = stop_flag.clone();
            let delivered = delivered.clone();
            let signal = signal.clone();
            device
                .build_input_stream(
                    config,
                    move |data: &[$t], _info: &cpal::InputCallbackInfo| {
                        if stop_flag.load(Ordering::Acquire) {
                            return;
                        }
                        let captured_at = Instant::now();
                        let samples: Vec<f32> = data.iter().map(|s| f32::from_sample(*s)).collect();
                        if let Some(signal) = signal.as_ref() {
                            signal.note(&samples);
                        }
                        let chunk = AudioChunk {
                            samples,
                            sample_rate,
                            channels,
                            captured_at,
                            generation,
                        };
                        let _ = tx.send(chunk);
                        if let Some(delivered) = delivered.as_ref() {
                            delivered.fetch_add(1, Ordering::Relaxed);
                        }
                    },
                    err_fn,
                    None,
                )
                .context("build_input_stream")?
        }};
    }

    let stream = match sample_format {
        SampleFormat::F32 => build_typed!(f32),
        SampleFormat::I16 => build_typed!(i16),
        SampleFormat::U16 => build_typed!(u16),
        SampleFormat::I32 => build_typed!(i32),
        SampleFormat::U8 => build_typed!(u8),
        other => bail!("unsupported cpal sample format: {other:?}"),
    };
    Ok(stream)
}

#[cfg(test)]
mod display_id_tests {
    use super::{DisplayIds, DisplayKey};

    fn devices(names: &[&str]) -> Vec<Option<String>> {
        names
            .iter()
            .map(|n| (!n.is_empty()).then(|| n.to_string()))
            .collect()
    }

    #[test]
    fn first_listing_numbers_displays_by_position() {
        let mut ids = DisplayIds::default();
        assert_eq!(ids.assign(&devices(&["A", "B", "C"])), vec![0, 1, 2]);
    }

    #[test]
    fn unplugging_a_display_never_moves_another_into_its_id() {
        let mut ids = DisplayIds::default();
        ids.assign(&devices(&["A", "B", "C"]));
        assert_eq!(ids.assign(&devices(&["B", "C"])), vec![1, 2]);
        assert!(!ids.current.contains_key(&0));
    }

    #[test]
    fn a_new_display_never_reuses_a_gone_displays_id() {
        let mut ids = DisplayIds::default();
        ids.assign(&devices(&["A", "B"]));
        ids.assign(&devices(&["B"]));
        assert_eq!(ids.assign(&devices(&["D", "B"])), vec![2, 1]);
    }

    #[test]
    fn a_reconnected_display_gets_its_old_id_back() {
        let mut ids = DisplayIds::default();
        ids.assign(&devices(&["A", "B"]));
        ids.assign(&devices(&["B"]));
        assert_eq!(ids.assign(&devices(&["A", "B"])), vec![0, 1]);
    }

    #[test]
    fn a_new_display_ahead_of_a_known_one_cannot_take_its_id() {
        let mut ids = DisplayIds::default();
        ids.assign(&devices(&["B"]));
        assert_eq!(ids.assign(&devices(&["A", "B"])), vec![1, 0]);
    }

    #[test]
    fn a_display_without_a_device_name_resolves_by_position() {
        let mut ids = DisplayIds::default();
        assert_eq!(ids.assign(&devices(&["A", ""])), vec![0, 1]);
        assert!(matches!(ids.current.get(&1), Some(DisplayKey::Index(1))));
    }
}

#[cfg(test)]
mod window_crop_tests {
    use super::{crop_within_display, CaptureFailure};

    #[test]
    fn a_window_on_the_display_crops_to_itself() {
        let crop = crop_within_display((100, 50, 900, 650), (0, 0), 1920, 1080).unwrap();
        assert_eq!((crop.x, crop.y, crop.width, crop.height), (100, 50, 800, 600));
    }

    #[test]
    fn a_window_hanging_off_an_edge_crops_to_the_visible_part() {
        let crop = crop_within_display((-200, -100, 600, 400), (0, 0), 1920, 1080).unwrap();
        assert_eq!((crop.x, crop.y, crop.width, crop.height), (0, 0, 600, 400));

        let crop = crop_within_display((1700, 900, 2400, 1400), (0, 0), 1920, 1080).unwrap();
        assert_eq!((crop.x, crop.y, crop.width, crop.height), (1700, 900, 220, 180));
    }

    #[test]
    fn a_window_on_a_second_display_crops_in_that_displays_space() {
        let crop = crop_within_display((2020, 100, 2820, 700), (1920, 0), 1920, 1080).unwrap();
        assert_eq!((crop.x, crop.y, crop.width, crop.height), (100, 100, 800, 600));
    }

    // A minimised window parks at roughly (-32000, -32000). Clamping that to
    // the nearest legal crop gave a 2x2 corner of the display, which the
    // scaler blew up into a blank-looking recording.
    #[test]
    fn a_minimised_windows_parking_rect_is_refused_not_clamped() {
        let err = crop_within_display((-32000, -32000, -31840, -31972), (0, 0), 1920, 1080)
            .expect_err("a parked rect must not resolve to a crop");
        assert_eq!(
            err.downcast_ref::<CaptureFailure>().map(|f| f.reason),
            Some("window_offscreen")
        );
    }

    #[test]
    fn a_window_entirely_off_the_display_is_refused() {
        assert!(crop_within_display((3000, 100, 3800, 700), (0, 0), 1920, 1080).is_err());
        assert!(crop_within_display((100, 2000, 900, 2600), (0, 0), 1920, 1080).is_err());
    }
}
