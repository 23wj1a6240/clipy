//! ScreenCaptureKit screen + system-audio capture.
//!
//! Wraps Cap.so's MIT `scap-screencapturekit` crate. After the pivot to
//! AVAssetWriter we no longer extract pixel buffers or walk the audio
//! buffer list — the raw `cm::SampleBuf` from SCK is exactly what
//! AVAssetWriter wants, so we forward it unchanged to the writer.

#![cfg(target_os = "macos")]
#![allow(dead_code)]

use anyhow::{anyhow, Result};
use std::sync::Arc;

use cidre::{arc, cg, cm, cv};
use tokio::sync::mpsc::{self, Receiver};

use scap_screencapturekit::{
    has_permission, is_system_audio_supported, request_permission, StreamCfgBuilder,
};

use super::sck_stream::{SckStream, StreamSample};
use super::stream_health::ScreenStreamStats;

use crate::cursor_track::{
    content_placement_in_output, CaptureViewport, ContentPlacement, SharedContentPlacement,
};
use crate::types::{
    CaptureSourceDto, CaptureSourcesDto, DisplayInfoDto, WindowCaptureModeDto, WindowInfoDto,
};

#[link(name = "CoreGraphics", kind = "framework")]
extern "C" {
    fn CGRectMakeWithDictionaryRepresentation(
        dict: &cidre::cf::Dictionary,
        rect: *mut cg::Rect,
    ) -> bool;
}

/// Where SCK drew the window inside this frame, in output pixels. Read
/// from the frame's own attachments: SCK keeps the frame size fixed and
/// fits a resized window inside it, so this is the only record of the bars.
fn frame_content_placement(
    sample: &cm::SampleBuf,
    output_width: f64,
    output_height: f64,
) -> Option<ContentPlacement> {
    let attaches = sample.attaches(false)?;
    if attaches.is_empty() {
        return None;
    }
    let info = &attaches[0];
    let rect_dict = info
        .get(cidre::sc::FrameInfo::content_rect().as_cf())?
        .try_as_raw_dictionary()?;
    let mut rect = cg::Rect::zero();
    // SAFETY: `rect_dict` is a live CFDictionary owned by the sample buffer
    // and `rect` is a valid out-pointer for the duration of the call.
    if !unsafe { CGRectMakeWithDictionaryRepresentation(rect_dict, &mut rect) } {
        return None;
    }
    let scale_factor = info
        .get(cidre::sc::FrameInfo::scale_factor().as_cf())
        .and_then(|value| value.try_as_number())
        .and_then(|number| number.to_f64())
        .unwrap_or(1.0);
    content_placement_in_output(
        (
            rect.origin.x,
            rect.origin.y,
            rect.size.width,
            rect.size.height,
        ),
        scale_factor,
        output_width,
        output_height,
    )
}

/// The owning app's name, or `None` when ScreenCaptureKit has none.
///
/// cidre declares `SCRunningApplication.applicationName` as never nil, but on
/// macOS 12 it is nil for some processes (helpers and agents without a
/// bundle name). Formatting that nil `NSString` dereferenced address 0 and
/// crashed the app every time the source picker listed windows (seen on
/// macOS 12.7.6). Cap reads window owner names as
/// `Option<String>` for the same reason (`scap-targets`, `owner_name`). Read
/// the property through objc2 so a nil comes back as a null pointer we can
/// test, and copy the UTF-8 bytes out at once.
fn running_app_name(app: &cidre::sc::RunningApp) -> Option<String> {
    use objc2::msg_send;
    use objc2::runtime::AnyObject;

    let receiver = app as *const cidre::sc::RunningApp as *const AnyObject;
    // SAFETY: `receiver` is a live SCRunningApplication borrowed for this
    // call. Both selectors are plain getters returning +0 objects or C
    // strings; nil is checked before every use and the C string is copied
    // before the autorelease pool around the caller can drain.
    unsafe {
        let name: *mut AnyObject = msg_send![receiver, applicationName];
        if name.is_null() {
            return None;
        }
        let utf8: *const std::ffi::c_char = msg_send![name, UTF8String];
        if utf8.is_null() {
            return None;
        }
        let name = std::ffi::CStr::from_ptr(utf8)
            .to_string_lossy()
            .into_owned();
        (!name.is_empty()).then_some(name)
    }
}

/// `SCFrameStatus` of a screen sample buffer, from its attachments.
fn frame_status(sample: &cm::SampleBuf) -> Option<i64> {
    let attaches = sample.attaches(false)?;
    if attaches.is_empty() {
        return None;
    }
    attaches[0]
        .get(cidre::sc::FrameInfo::status().as_cf())?
        .try_as_number()?
        .to_i64()
}

/// How the ScreenCaptureKit stream is configured on this macOS.
///
/// macOS 13 and later keep the configuration every working take has used:
/// BGRA in the display's color space, queue depth 8. macOS 12 follows Cap,
/// which supports 12.3 and 12.7 on the same SCK API: 420v (NV12, video range)
/// in sRGB with queue depth 5. Takes on macOS 12.7.6 recorded no video at
/// all with the BGRA setup, and 420v is the format the system
/// encoder takes directly, so on 12 frames skip the BGRA to NV12 conversion.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct StreamFormat {
    pixel_format_420v: bool,
    srgb: bool,
    queue_depth: isize,
}

impl StreamFormat {
    fn for_macos(at_least_13: bool) -> Self {
        if at_least_13 {
            Self {
                pixel_format_420v: false,
                srgb: false,
                queue_depth: 8,
            }
        } else {
            Self {
                pixel_format_420v: true,
                srgb: true,
                queue_depth: 5,
            }
        }
    }

    fn pixel_format_slug(&self) -> &'static str {
        if self.pixel_format_420v {
            "420v"
        } else {
            "bgra"
        }
    }
}

/// A window-only recording follows its window. The cursor sampler polls the
/// window's bounds by id and reads where SCK last drew it in the frame.
pub struct WindowFollow {
    pub window_id: u32,
    pub content: SharedContentPlacement,
}

fn even_u32(value: f64) -> u32 {
    (value.round() as u32).max(2) & !1
}

fn display_logical_size(display: &cidre::sc::Display) -> (f64, f64) {
    let frame = display.frame();
    (frame.size.width.max(2.0), frame.size.height.max(2.0))
}

fn display_physical_size(display: &cidre::sc::Display) -> (u32, u32) {
    (
        (display.width() as u32).max(2) & !1,
        (display.height() as u32).max(2) & !1,
    )
}

fn display_scale(display: &cidre::sc::Display) -> f64 {
    let (logical_width, _) = display_logical_size(display);
    let (physical_width, _) = display_physical_size(display);
    (physical_width as f64 / logical_width).max(1.0)
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct DisplayCropGeometry {
    x: f64,
    y: f64,
    width: f64,
    height: f64,
    output_width: u32,
    output_height: u32,
}

fn display_crop_geometry(
    display_width: f64,
    display_height: f64,
    scale: f64,
    x: f64,
    y: f64,
    width: f64,
    height: f64,
) -> DisplayCropGeometry {
    let right = (x + width).clamp(0.0, display_width);
    let bottom = (y + height).clamp(0.0, display_height);
    let x = x.clamp(0.0, (display_width - 2.0).max(0.0));
    let y = y.clamp(0.0, (display_height - 2.0).max(0.0));
    let width = even_u32((right - x).max(2.0).min(display_width - x).floor()) as f64;
    let height = even_u32((bottom - y).max(2.0).min(display_height - y).floor()) as f64;

    DisplayCropGeometry {
        x,
        y,
        width,
        height,
        output_width: even_u32(width * scale),
        output_height: even_u32(height * scale),
    }
}

fn display_contains_point(
    display_x: f64,
    display_y: f64,
    display_width: f64,
    display_height: f64,
    point_x: f64,
    point_y: f64,
) -> bool {
    point_x >= display_x
        && point_x <= display_x + display_width
        && point_y >= display_y
        && point_y <= display_y + display_height
}

/// Retained CoreMedia sample buffer; forwarded unchanged to AVAssetWriter.
/// Carries its own format description + PTS, so nothing else is needed
/// for AVAssetWriter to accept it.
pub struct SampleBuf(pub arc::R<cm::SampleBuf>);

pub enum VideoSample {
    Screen(SampleBuf),
    Camera {
        frame: ffmpeg::frame::Video,
        mach_pts: u64,
    },
}

impl VideoSample {
    pub fn mach_pts(&self) -> u64 {
        match self {
            Self::Screen(sample) => cm::Clock::convert_host_time_to_sys_units(sample.0.pts()),
            Self::Camera { mach_pts, .. } => *mach_pts,
        }
    }

    pub fn to_frame(&self, pts: i64) -> Result<ffmpeg::frame::Video> {
        match self {
            Self::Screen(sample) => crate::encoder::cm_video_to_frame(&sample.0, pts),
            Self::Camera { frame, .. } => {
                let mut frame = frame.clone();
                frame.set_pts(Some(pts));
                Ok(frame)
            }
        }
    }
}

// SAFETY: cm::SampleBuf is an immutable Core Foundation object once
// produced by SCK; retaining + moving the retain across threads is safe.
unsafe impl Send for SampleBuf {}

pub struct ScreenCaptureHandles {
    pub video_rx: Receiver<VideoSample>,
    pub system_audio_rx: Option<Receiver<SampleBuf>>,
    /// Dimensions of the capture, for informing the writer config. SCK
    /// scales internally if these don't match the physical display.
    pub width: u32,
    pub height: u32,
    pub capture: Option<CaptureViewport>,
    /// Set for a window-only recording, whose viewport moves with the window.
    pub window_follow: Option<WindowFollow>,
    /// What the ScreenCaptureKit stream sent. `None` for a camera take.
    pub stream_stats: Option<Arc<ScreenStreamStats>>,
}

pub enum ScreenCapturer {
    Screen(SckStream),
    Camera(super::camera::CameraCapturer),
}

// SAFETY: SckStream is not auto-Send because the
// underlying sc::Stream holds a dispatch queue. We serialise all access
// through the recording controller's mutex and only call methods from
// tokio tasks, matching the pattern in Cap's own bindings.
unsafe impl Send for ScreenCapturer {}

/// The SCK error for a declined/stale TCC grant (SCStreamErrorDomain
/// -3801, "The user declined TCCs"). Matched on the debug string so it
/// works across cidre's error wrappers.
pub(crate) fn sck_error_is_permission_denied(debug: &str) -> bool {
    debug.contains("-3801") || debug.contains("declined TCC")
}

fn sck_error_is_user_stopped(debug: &str) -> bool {
    debug.contains("Code=-3817")
}

/// SCStreamErrorDomain -3815, "no displays or windows to capture": the
/// captured window was closed or the display was disconnected mid-take.
/// The user's environment changed, not a capture bug, so it is logged as a
/// warning rather than reported as an error. Behaviour is unchanged.
fn sck_error_is_target_gone(debug: &str) -> bool {
    debug.contains("Code=-3815")
}

/// Map an SCK failure to the typed `permission_denied:` error ONLY when it
/// is actually a permission refusal — other SCK failures (internal errors,
/// transient content-fetch issues) must not send the user to the
/// permission-repair flow.
fn classify_sck_error(context: &str, debug: String) -> anyhow::Error {
    if sck_error_is_permission_denied(&debug) {
        anyhow!("permission_denied: screen_recording (sck: {debug})")
    } else {
        anyhow!("{context} failed: {debug}")
    }
}

impl ScreenCapturer {
    pub async fn ensure_permission() -> Result<()> {
        if has_permission() {
            return Ok(());
        }
        tracing::info!("screen recording permission not granted; prompting user");
        if request_permission() {
            Ok(())
        } else {
            Err(anyhow!(
                "screen-recording permission denied — grant access in \
                 System Settings → Privacy & Security → Screen Recording"
            ))
        }
    }

    /// Enumerate available capture sources (displays + visible windows).
    pub async fn list_sources() -> Result<CaptureSourcesDto> {
        // Bail with a permission-shaped error before SCK runs so the UI
        // can render the permission gate rather than a -3801 stack trace.
        if !has_permission() {
            return Err(anyhow!("permission_denied: screen_recording"));
        }
        let content = cidre::sc::ShareableContent::current()
            .await
            .map_err(|e| classify_sck_error("screen capture content fetch", format!("{e:?}")))?;

        let displays: Vec<DisplayInfoDto> = content
            .displays()
            .iter()
            .enumerate()
            .map(|(i, d)| {
                let frame = d.frame();
                DisplayInfoDto {
                    id: d.display_id().0,
                    name: format!("Display {}", i + 1),
                    x: frame.origin.x,
                    y: frame.origin.y,
                    width: even_u32(frame.size.width),
                    height: even_u32(frame.size.height),
                }
            })
            .collect();

        let windows: Vec<WindowInfoDto> = content
            .windows()
            .iter()
            .filter_map(|w| {
                if !w.is_on_screen() || w.window_layer() != 0 {
                    return None;
                }
                let app = w.owning_app()?;
                // A nil name is skipped: see `running_app_name`.
                let app_name = running_app_name(&app)?;
                if app_name == "Window Server" {
                    return None;
                }
                let title = w.title()?.to_string();
                if title.is_empty() {
                    return None;
                }
                let frame = w.frame();
                Some(WindowInfoDto {
                    id: w.id(),
                    app_name,
                    title,
                    x: frame.origin.x,
                    y: frame.origin.y,
                    width: frame.size.width.max(0.0).round() as u32,
                    height: frame.size.height.max(0.0).round() as u32,
                })
            })
            .collect();

        Ok(CaptureSourcesDto { displays, windows })
    }

    /// Start capturing at `fps` FPS. `source` selects which display or window
    /// to capture; `None` defaults to the primary display. When
    /// `with_system_audio` is true and we're on macOS 13+, system audio is
    /// also captured. `window_capture` decides whether a window source
    /// records the window alone or the screen area under it.
    pub async fn start<R: tauri::Runtime>(
        app: &tauri::AppHandle<R>,
        fps: u32,
        with_system_audio: bool,
        source: Option<&CaptureSourceDto>,
        max_height: Option<u32>,
        window_capture: WindowCaptureModeDto,
    ) -> Result<(Self, ScreenCaptureHandles)> {
        if let Some(CaptureSourceDto::Camera { device_id }) = source {
            let (camera, handles) =
                super::camera::CameraCapturer::start(app, device_id.clone(), max_height).await?;
            return Ok((Self::Camera(camera), handles));
        }
        if !has_permission() {
            return Err(anyhow!("permission_denied: screen_recording"));
        }

        let capture_audio = with_system_audio && is_system_audio_supported();
        if with_system_audio && !capture_audio {
            tracing::warn!(
                "system audio requested but unsupported on this macOS version; \
                 continuing without it"
            );
        }

        let content = cidre::sc::ShareableContent::current()
            .await
            .map_err(|e| classify_sck_error("screen capture content fetch", format!("{e:?}")))?;

        let mut follow_window_id: Option<u32> = None;
        let (
            target,
            width,
            height,
            src_rect,
            capture_x,
            capture_y,
            capture_w,
            capture_h,
            screen_x,
            screen_y,
            screen_w,
            screen_h,
        ) = match source {
            Some(CaptureSourceDto::Camera { .. }) => {
                unreachable!("camera capture was routed before SCK")
            }
            Some(CaptureSourceDto::Window { id }) => {
                let windows = content.windows();
                let window = windows
                    .iter()
                    .find(|w| w.id() == *id)
                    .ok_or_else(|| anyhow!("window {id} not found in ShareableContent"))?;
                if !window.is_on_screen() {
                    return Err(anyhow!(
                        "window {id} is not visible; restore it and try again"
                    ));
                }
                let frame = window.frame();
                let window_center_x = frame.origin.x + frame.size.width / 2.0;
                let window_center_y = frame.origin.y + frame.size.height / 2.0;
                let displays = content.displays();
                let display = displays
                    .iter()
                    .find(|display| {
                        let display_frame = display.frame();
                        display_contains_point(
                            display_frame.origin.x,
                            display_frame.origin.y,
                            display_frame.size.width,
                            display_frame.size.height,
                            window_center_x,
                            window_center_y,
                        )
                    })
                    .ok_or_else(|| anyhow!("window {id} is not on an available display"))?
                    .retained();
                let display_frame = display.frame();
                let (display_width, display_height) = display_logical_size(&display);
                if window_capture == WindowCaptureModeDto::Window {
                    // The window's own pixels, wherever it is and whatever
                    // covers it. Sized in pixels from the display it starts
                    // on; points would record a Retina window at half
                    // resolution.
                    let (output_width, output_height) = window_output_size(
                        frame.size.width,
                        frame.size.height,
                        display_scale(&display),
                    );
                    let filter = cidre::sc::ContentFilter::with_desktop_independent_window(window);
                    follow_window_id = Some(*id);
                    (
                        filter,
                        output_width,
                        output_height,
                        None,
                        frame.origin.x,
                        frame.origin.y,
                        frame.size.width.max(2.0),
                        frame.size.height.max(2.0),
                        display_frame.origin.x,
                        display_frame.origin.y,
                        display_width,
                        display_height,
                    )
                } else {
                    let crop = display_crop_geometry(
                        display_width,
                        display_height,
                        display_scale(&display),
                        frame.origin.x - display_frame.origin.x,
                        frame.origin.y - display_frame.origin.y,
                        frame.size.width,
                        frame.size.height,
                    );
                    let empty_windows = cidre::ns::Array::<cidre::sc::Window>::new();
                    let filter = cidre::sc::ContentFilter::with_display_excluding_windows(
                        &display,
                        &empty_windows,
                    );
                    (
                        filter,
                        crop.output_width,
                        crop.output_height,
                        Some(cg::Rect::new(crop.x, crop.y, crop.width, crop.height)),
                        display_frame.origin.x + crop.x,
                        display_frame.origin.y + crop.y,
                        crop.width,
                        crop.height,
                        display_frame.origin.x,
                        display_frame.origin.y,
                        display_width,
                        display_height,
                    )
                }
            }
            Some(CaptureSourceDto::Display { id }) => {
                let display = content
                    .displays()
                    .iter()
                    .find(|d| d.display_id().0 == *id)
                    .ok_or_else(|| anyhow!("display {id} not found in ShareableContent"))?
                    .retained();
                let (width, height) = display_physical_size(&display);
                let empty_windows = cidre::ns::Array::<cidre::sc::Window>::new();
                let filter = cidre::sc::ContentFilter::with_display_excluding_windows(
                    &display,
                    &empty_windows,
                );
                let frame = display.frame();
                let (logical_width, logical_height) = display_logical_size(&display);
                (
                    filter,
                    width,
                    height,
                    None,
                    frame.origin.x,
                    frame.origin.y,
                    logical_width,
                    logical_height,
                    frame.origin.x,
                    frame.origin.y,
                    logical_width,
                    logical_height,
                )
            }
            Some(CaptureSourceDto::Area {
                display_id,
                x,
                y,
                width,
                height,
            }) => {
                let display = content
                    .displays()
                    .iter()
                    .find(|d| d.display_id().0 == *display_id)
                    .ok_or_else(|| anyhow!("display {display_id} not found in ShareableContent"))?
                    .retained();
                let (display_width, display_height) = display_logical_size(&display);
                let display_frame = display.frame();
                let crop = display_crop_geometry(
                    display_width,
                    display_height,
                    display_scale(&display),
                    *x,
                    *y,
                    *width,
                    *height,
                );
                let empty_windows = cidre::ns::Array::<cidre::sc::Window>::new();
                let filter = cidre::sc::ContentFilter::with_display_excluding_windows(
                    &display,
                    &empty_windows,
                );
                (
                    filter,
                    crop.output_width,
                    crop.output_height,
                    Some(cg::Rect::new(crop.x, crop.y, crop.width, crop.height)),
                    display_frame.origin.x + crop.x,
                    display_frame.origin.y + crop.y,
                    crop.width,
                    crop.height,
                    display_frame.origin.x,
                    display_frame.origin.y,
                    display_width,
                    display_height,
                )
            }
            None => {
                // Default: primary display
                let display = content
                    .displays()
                    .iter()
                    .next()
                    .ok_or_else(|| anyhow!("no displays available"))?
                    .retained();
                let (width, height) = display_physical_size(&display);
                let empty_windows = cidre::ns::Array::<cidre::sc::Window>::new();
                let filter = cidre::sc::ContentFilter::with_display_excluding_windows(
                    &display,
                    &empty_windows,
                );
                let frame = display.frame();
                let (logical_width, logical_height) = display_logical_size(&display);
                (
                    filter,
                    width,
                    height,
                    None,
                    frame.origin.x,
                    frame.origin.y,
                    logical_width,
                    logical_height,
                    frame.origin.x,
                    frame.origin.y,
                    logical_width,
                    logical_height,
                )
            }
        };

        // Apply the resolution cap. SCK scales internally; the policy is
        // the server's store-as-is box, not just the requested height, so
        // an ultrawide or a Native capture can't land outside it and earn
        // itself a server re-encode.
        let (out_width, out_height) =
            crate::capture::output_size::fit_capture_size(width, height, max_height);

        let stream_format = StreamFormat::for_macos(cidre::api::macos_available("13.0"));
        let mut config_builder = StreamCfgBuilder::default()
            .with_width(out_width as usize)
            .with_height(out_height as usize)
            .with_fps(fps as f32)
            .with_shows_cursor(true)
            .with_captures_audio(capture_audio)
            .with_queue_depth(stream_format.queue_depth);
        if let Some(rect) = src_rect {
            config_builder = config_builder.with_src_rect(rect);
        }
        let mut config = config_builder.build();
        if stream_format.pixel_format_420v {
            config.set_pixel_format(cv::PixelFormat::_420V);
        }
        if stream_format.srgb {
            config.set_color_space_name(cg::color_space::names::srgb());
        }
        if stream_format != StreamFormat::for_macos(true) {
            tracing::info!(
                pixel_format = stream_format.pixel_format_slug(),
                srgb = stream_format.srgb,
                queue_depth = stream_format.queue_depth,
                "screen capture: macOS 12 stream configuration"
            );
        }
        let stream_stats = Arc::new(ScreenStreamStats::new(stream_format.pixel_format_slug()));
        if follow_window_id.is_some() {
            // The frame size is fixed for the whole take (the encoder and
            // the upload are opened at it), so a window that is resized is
            // scaled into it, never cropped. On macOS 14+ SCK also keeps its
            // aspect ratio, which leaves bars that the content rect records.
            config.set_scales_to_fit(true);
        }

        // Bounded depth matches SCK's internal queue — anything deeper is
        // wasted memory, anything shallower risks the SCK dispatch queue
        // stalling when the writer is under load.
        let (video_tx, video_rx) = mpsc::channel::<VideoSample>(8);
        let (sys_audio_tx, sys_audio_rx) = if capture_audio {
            let (tx, rx) = mpsc::channel::<SampleBuf>(8);
            (Some(tx), Some(rx))
        } else {
            (None, None)
        };

        // SCK delivers CMSampleBuffers already tagged with PTS; AVAssetWriter
        // uses those directly to schedule the session, so we have zero
        // timing logic to write here.
        let video_tx_cb = video_tx.clone();
        let sys_audio_tx_cb = sys_audio_tx.clone();
        let content_placement: SharedContentPlacement = Default::default();
        let content_placement_cb = follow_window_id.map(|_| content_placement.clone());
        let (placement_width, placement_height) = (out_width as f64, out_height as f64);
        let stats_cb = stream_stats.clone();
        let callback = move |sample: StreamSample<'_>| match sample {
            StreamSample::Screen(sample_buf) => {
                // Status-only buffers (idle, blank, suspended) carry no
                // image. They were always skipped; now they are counted.
                let has_image = sample_buf.image_buf().is_some();
                stats_cb.note_screen_buffer(has_image, frame_status(sample_buf));
                if !has_image {
                    return;
                }
                if let Some(shared) = &content_placement_cb {
                    if let Some(placement) =
                        frame_content_placement(sample_buf, placement_width, placement_height)
                    {
                        let mut current = shared.lock();
                        if *current != Some(placement) {
                            *current = Some(placement);
                        }
                    }
                }
                let sbuf = SampleBuf(sample_buf.retained());
                // try_send: drop rather than stall the SCK dispatch queue
                // if the writer is temporarily behind.
                if video_tx_cb.try_send(VideoSample::Screen(sbuf)).is_err() {
                    tracing::trace!("dropped video frame — downstream lagging");
                }
            }
            StreamSample::Audio(sample_buf) => {
                if let Some(tx) = &sys_audio_tx_cb {
                    let sbuf = SampleBuf(sample_buf.retained());
                    if tx.try_send(sbuf).is_err() {
                        tracing::trace!("dropped system-audio chunk — downstream lagging");
                    }
                }
            }
        };

        let stats_stop = stream_stats.clone();
        let stop_callback = move |err: &cidre::ns::Error| {
            let domain = err.domain();
            let domain: &cidre::ns::String = &domain;
            stats_stop.note_stop_error(err.code() as i64, &domain.to_string());
            let debug_message = format!("{err:?}");
            if sck_error_is_user_stopped(&debug_message) {
                tracing::info!("sck stream stopped by the user");
            } else if sck_error_is_target_gone(&debug_message) {
                tracing::warn!("sck stream stopped: capture target went away: {debug_message}");
            } else {
                tracing::error!("sck stream stopped with error: {debug_message}");
            }
        };

        let capturer = SckStream::build(&target, &config, capture_audio, callback, stop_callback)
            .map_err(|e| classify_sck_error("Capturer::build", format!("{e:?}")))?;

        capturer
            .start()
            .await
            .map_err(|e| classify_sck_error("Capturer::start", format!("{e:?}")))?;

        Ok((
            Self::Screen(capturer),
            ScreenCaptureHandles {
                video_rx,
                system_audio_rx: sys_audio_rx,
                // Report what SCK actually emits, not the source dimensions —
                // the encoder must size its frames to the SCK output.
                width: out_width,
                height: out_height,
                capture: Some(CaptureViewport {
                    x: capture_x,
                    y: capture_y,
                    width: capture_w,
                    height: capture_h,
                    output_width: out_width,
                    output_height: out_height,
                    screen_x: Some(screen_x),
                    screen_y: Some(screen_y),
                    screen_width: Some(screen_w),
                    screen_height: Some(screen_h),
                }),
                window_follow: follow_window_id.map(|window_id| WindowFollow {
                    window_id,
                    content: content_placement,
                }),
                stream_stats: Some(stream_stats),
            },
        ))
    }

    pub async fn stop(self) -> Result<()> {
        match self {
            Self::Screen(capturer) => capturer
                .stop()
                .await
                .map_err(|e| anyhow!("Capturer::stop failed: {e:?}")),
            Self::Camera(camera) => camera.stop().await,
        }
    }
}

/// Pixel size of a window-only capture: the window's points at the scale
/// of the display it starts on, even for H.264.
fn window_output_size(width_points: f64, height_points: f64, scale: f64) -> (u32, u32) {
    let scale = if scale.is_finite() && scale >= 1.0 {
        scale
    } else {
        1.0
    };
    (
        even_u32(width_points.max(2.0) * scale),
        even_u32(height_points.max(2.0) * scale),
    )
}

#[cfg(test)]
mod tests {
    use super::{
        display_contains_point, display_crop_geometry, sck_error_is_permission_denied,
        sck_error_is_target_gone, sck_error_is_user_stopped, window_output_size,
    };

    #[test]
    fn window_only_capture_is_sized_in_pixels_not_points() {
        assert_eq!(window_output_size(800.0, 601.0, 2.0), (1_600, 1_202));
        assert_eq!(window_output_size(1_255.0, 703.0, 1.0), (1_254, 702));
        assert_eq!(window_output_size(1.0, 1.0, 2.0), (4, 4));
        assert_eq!(window_output_size(800.0, 600.0, f64::NAN), (800, 600));
    }

    #[test]
    fn window_crop_uses_initial_display_coordinates_and_scale() {
        let crop = display_crop_geometry(1_920.0, 1_080.0, 2.0, 320.0, 180.0, 800.0, 600.0);

        assert_eq!((crop.x, crop.y), (320.0, 180.0));
        assert_eq!((crop.width, crop.height), (800.0, 600.0));
        assert_eq!((crop.output_width, crop.output_height), (1_600, 1_200));
    }

    #[test]
    fn window_crop_is_clipped_to_the_selected_display() {
        let crop = display_crop_geometry(1_920.0, 1_080.0, 1.0, -24.0, 1_060.0, 640.0, 480.0);

        assert_eq!((crop.x, crop.y), (0.0, 1_060.0));
        assert_eq!((crop.width, crop.height), (616.0, 20.0));
        assert_eq!((crop.output_width, crop.output_height), (616, 20));
    }

    #[test]
    fn offscreen_window_center_does_not_match_an_unrelated_display() {
        assert!(!display_contains_point(
            0.0, 0.0, 1_920.0, 1_080.0, -640.0, 540.0
        ));
    }

    #[test]
    fn classifies_user_declined_tcc_as_permission_denied() {
        // Verbatim error text for a declined screen-recording prompt.
        let real = r#"Retained(Error(Error Domain=com.apple.ScreenCaptureKit.SCStreamErrorDomain Code=-3801 "The user declined TCCs for application, window, display capture" UserInfo={NSLocalizedDescription=The user declined TCCs for application, window, display capture}))"#;
        assert!(sck_error_is_permission_denied(real));
    }

    #[test]
    fn non_permission_sck_errors_are_not_classified_as_denied() {
        assert!(!sck_error_is_permission_denied(
            "Error Domain=com.apple.ScreenCaptureKit.SCStreamErrorDomain Code=-3808 no shareable content"
        ));
        assert!(!sck_error_is_permission_denied("connection interrupted"));
    }

    #[test]
    fn user_stopped_stream_is_not_an_unexpected_capture_error() {
        assert!(sck_error_is_user_stopped(
            r#"Error Domain=com.apple.ScreenCaptureKit.SCStreamErrorDomain Code=-3817 \"The user stopped the stream\""#
        ));
        assert!(!sck_error_is_user_stopped(
            "Error Domain=com.apple.ScreenCaptureKit.SCStreamErrorDomain Code=-3815 no displays"
        ));
    }

    #[test]
    fn closed_capture_target_is_recognised() {
        // Verbatim shape from a Spanish-locale error report.
        assert!(sck_error_is_target_gone(
            r#"Error(Error Domain=com.apple.ScreenCaptureKit.SCStreamErrorDomain Code=-3815 "No se encontraron pantallas ni ventanas por capturar" UserInfo={NSLocalizedDescription=No se encontraron pantallas ni ventanas por capturar})"#
        ));
        assert!(!sck_error_is_target_gone(
            "Error Domain=com.apple.ScreenCaptureKit.SCStreamErrorDomain Code=-3817 user stopped"
        ));
    }
}
