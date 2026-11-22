//! Camera-only recording on Windows: frames read straight from the camera
//! through Media Foundation, the counterpart of `capture::camera` on macOS.
//!
//! The camera is read natively, not through the webview, for the same reasons
//! macOS does: the recording must not depend on a window staying open, and
//! Windows cameras are frequently single-client, so the bubble cannot open the
//! device a second time for its preview. The bubble instead receives preview
//! frames from here (`native-webcam-frame`), exactly as on macOS.
//!
//! Frames leave as BGRA `VideoFrame`s, the shape the Windows pump already
//! scales and encodes for screen capture, so nothing downstream changes.
//!
//! The capture thread only reads and converts: it takes the camera's own YUV
//! layout where it can (Media Foundation's RGB converter is a slow software
//! path) and converts with FFmpeg. The preview is scaled and JPEG-encoded on
//! its own thread, so a slow preview can never hold up the next camera frame.
//!
//! Background effects are applied here too (windows_camera_effects), since
//! the bubble's webview effects never reach this recording.

#![cfg(target_os = "windows")]

use std::sync::{
    atomic::{AtomicBool, AtomicU64, Ordering},
    mpsc as std_mpsc, Arc,
};
use std::time::{Duration, Instant};

use anyhow::{anyhow, Result};
use ffmpeg::{format::Pixel, frame, software::scaling};
use parking_lot::Mutex;
use tauri::{AppHandle, Emitter, Manager, Runtime};
use tokio::sync::mpsc;
use windows::core::{Interface, GUID, PWSTR};
use windows::Win32::Media::MediaFoundation::{
    IMF2DBuffer, IMFActivate, IMFMediaSource, IMFMediaType, IMFSample, IMFSourceReader,
    MFCreateAttributes, MFCreateMediaType, MFCreateSourceReaderFromMediaSource,
    MFEnumDeviceSources, MFMediaType_Video, MFShutdown, MFStartup, MFVideoFormat_NV12,
    MFVideoFormat_RGB32, MFVideoFormat_YUY2, MF_DEVSOURCE_ATTRIBUTE_SOURCE_TYPE,
    MF_DEVSOURCE_ATTRIBUTE_SOURCE_TYPE_VIDCAP_GUID,
    MF_DEVSOURCE_ATTRIBUTE_SOURCE_TYPE_VIDCAP_SYMBOLIC_LINK, MF_E_NO_MORE_TYPES,
    MF_MT_DEFAULT_STRIDE, MF_MT_FRAME_RATE, MF_MT_FRAME_SIZE, MF_MT_MAJOR_TYPE, MF_MT_SUBTYPE,
    MF_SOURCE_READERF_CURRENTMEDIATYPECHANGED, MF_SOURCE_READERF_ENDOFSTREAM,
    MF_SOURCE_READERF_ERROR, MF_SOURCE_READER_ENABLE_VIDEO_PROCESSING,
    MF_SOURCE_READER_FIRST_VIDEO_STREAM, MF_VERSION, MFSTARTUP_FULL,
};
use windows::Win32::System::Com::{
    CoInitializeEx, CoTaskMemFree, CoUninitialize, COINIT_MULTITHREADED,
};

use super::windows::{capture_dimensions, ScreenCaptureHandles, ScreenCapturer, VideoFrame};
use super::windows_camera_effects as effects;
use crate::native_webcam::{
    NativeWebcamFrameDto, EVENT_NATIVE_WEBCAM_FRAME, EVENT_NATIVE_WEBCAM_START,
    EVENT_NATIVE_WEBCAM_STOP,
};

const STREAM: u32 = MF_SOURCE_READER_FIRST_VIDEO_STREAM.0 as u32;
const START_TIMEOUT: Duration = Duration::from_secs(12);
/// How long a failed start waits for the camera thread to wind down before
/// returning without it. The thread only sees the stop flag once
/// `ReadSample` returns, and a stuck driver may never return.
const STOP_TIMEOUT: Duration = Duration::from_secs(3);
/// The picker's bubble was reading this camera through the webview a moment
/// ago. Windows can take a beat to release it, so "in use" is retried briefly.
const OPEN_RETRY_WINDOW: Duration = Duration::from_secs(4);
const OPEN_RETRY_DELAY: Duration = Duration::from_millis(150);
/// Only thins cameras running faster than ~50 fps. A 30 fps camera's frames
/// arrive with jitter, and a gap near 33 ms dropped every early one.
const MIN_FRAME_GAP: Duration = Duration::from_millis(20);
/// Same bounding box and quantiser as the macOS preview.
const PREVIEW_MAX_WIDTH: u32 = 960;
const PREVIEW_MAX_HEIGHT: u32 = 540;
const PREVIEW_JPEG_QUALITY: i32 = 5;
/// Used when the user has no resolution cap: 1080p, like the screen path.
const DEFAULT_MAX_HEIGHT: u32 = 1080;
const CAMERA_UNAVAILABLE: &str =
    "Selected camera is unavailable. Reconnect it or choose another camera.";
/// `RPC_E_CHANGED_MODE`: COM is already initialised on this thread.
const RPC_E_CHANGED_MODE: i32 = 0x8001_0106u32 as i32;

/// Set while a camera-only recording owns the camera.
static ACTIVE: AtomicBool = AtomicBool::new(false);
/// Counts starts. A camera thread that outlived a failed start (see
/// `STOP_TIMEOUT`) checks this before touching the shared state, so it can
/// never hide a later session's preview.
static SESSION: AtomicU64 = AtomicU64::new(0);

pub fn is_active() -> bool {
    ACTIVE.load(Ordering::Acquire)
}

/// The start result: the recorded frame size and the capture anchor, or why
/// the recording could not begin.
type Ready = Result<(u32, u32, Instant), String>;
/// Handed to whichever reports first: the effects pipeline's first processed
/// frame, an effects error before that, or the camera thread failing to open.
type ReadySlot = Arc<Mutex<Option<std_mpsc::SyncSender<Ready>>>>;

fn report_ready(slot: &ReadySlot, ready: Ready) {
    if let Some(tx) = slot.lock().take() {
        let _ = tx.try_send(ready);
    }
}

/// Start recording from the camera whose Media Foundation symbolic link is
/// `device_id` (the first camera when empty).
pub async fn start<R: Runtime>(
    app: &AppHandle<R>,
    device_id: &str,
    max_height: Option<u32>,
) -> Result<(ScreenCapturer, ScreenCaptureHandles)> {
    crate::encoder::Mp4Encoder::init_global();
    // The bubble may still be previewing this camera. Switching it to native
    // frames releases the device while keeping its last picture on screen, so
    // the preview hands over instead of closing and reopening.
    let _ = app.emit_to("webcam", EVENT_NATIVE_WEBCAM_START, ());

    let (video_tx, video_rx) = mpsc::channel::<VideoFrame>(8);
    let (ready_tx, ready_rx) = std_mpsc::sync_channel::<Ready>(1);
    let ready: ReadySlot = Arc::new(Mutex::new(Some(ready_tx)));
    let stop = Arc::new(AtomicBool::new(false));
    let session = SESSION.fetch_add(1, Ordering::AcqRel) + 1;

    let worker_stop = Arc::clone(&stop);
    let worker_app = app.clone();
    let model_path = app
        .path()
        .resource_dir()
        .map(|dir| effects::model_path(&dir))
        .unwrap_or_default();
    let device_id = device_id.to_string();
    let target_height = max_height.unwrap_or(DEFAULT_MAX_HEIGHT);
    let worker = std::thread::Builder::new()
        .name("clipy-camera".to_string())
        .spawn(move || {
            let (preview_tx, preview_rx) = std_mpsc::sync_channel::<VideoFrame>(1);
            let preview_app = worker_app.clone();
            let preview = std::thread::Builder::new()
                .name("clipy-camera-preview".to_string())
                .spawn(move || run_preview(&preview_app, preview_rx));

            let effects_app = worker_app.clone();
            let effects_ready = Arc::clone(&ready);
            let report_effects_error: effects::ErrorSink = Box::new(move |message: String| {
                // Before any frame has been processed, a failed effect is the
                // start error, as on macOS: the recording never begins.
                report_ready(&effects_ready, Err(message.clone()));
                let _ = effects_app.emit("camera-effects-error", message);
            });
            let outcome = run(
                &device_id,
                target_height,
                model_path,
                report_effects_error,
                &video_tx,
                &preview_tx,
                &worker_stop,
                &ready,
            );
            // Closing the channel ends the preview thread.
            drop(preview_tx);
            if let Ok(preview) = preview {
                let _ = preview.join();
            }
            if let Err(err) = outcome {
                // Delivered before the first frame: that is the start error.
                report_ready(&ready, Err(err));
            }
            // A thread left behind by a failed start (see `STOP_TIMEOUT`) may
            // finish after a later session took the camera windows over.
            if SESSION.load(Ordering::Acquire) == session {
                ACTIVE.store(false, Ordering::Release);
                hide_preview(&worker_app);
            }
        })
        .map_err(|e| anyhow!("camera thread: {e}"))?;

    let ready = tokio::task::spawn_blocking(move || ready_rx.recv_timeout(START_TIMEOUT))
        .await
        .map_err(|e| anyhow!("camera start: {e}"))?;
    let ready = match ready {
        Ok(ready) => ready,
        Err(std_mpsc::RecvTimeoutError::Disconnected) => {
            Err("Camera stopped before delivering a frame.".to_string())
        }
        Err(std_mpsc::RecvTimeoutError::Timeout) => {
            Err("Camera did not deliver a frame. Check camera privacy settings and try again."
                .to_string())
        }
    };
    let (source_width, source_height, started_at) = match ready {
        Ok(ready) => ready,
        Err(message) => {
            stop.store(true, Ordering::Release);
            // Waiting for the thread releases the camera before a retry, but
            // only a bounded wait: it cannot see the stop flag while a driver
            // holds `ReadSample`, and the timeout above must stay a timeout.
            let join = tokio::task::spawn_blocking(move || worker.join());
            if tokio::time::timeout(STOP_TIMEOUT, join).await.is_err() {
                tracing::warn!("camera thread did not stop after a failed start; leaving it to finish");
            }
            return Err(anyhow!(message));
        }
    };

    ACTIVE.store(true, Ordering::Release);
    show_preview(app);

    let (width, height) = capture_dimensions(source_width, source_height, max_height);
    let capturer = ScreenCapturer::from_stop(Box::new(move || {
        stop.store(true, Ordering::Release);
        // Joining releases the device before a following recording opens it.
        if worker.join().is_err() {
            tracing::warn!("camera thread panicked while stopping");
        }
    }));
    Ok((
        capturer,
        ScreenCaptureHandles {
            video_rx,
            width,
            height,
            crop: None,
            capture: None,
            started_at,
        },
    ))
}

/// Runs the camera session on this thread. An error means it never started;
/// once it has, it runs until stopped and returns `Ok`. The start is
/// reported through `ready` by the first frame to come out of the effects
/// pipeline, so a background that cannot be applied fails the start instead
/// of recording nothing.
fn run(
    device_id: &str,
    target_height: u32,
    model_path: std::path::PathBuf,
    report_effects_error: effects::ErrorSink,
    video_tx: &mpsc::Sender<VideoFrame>,
    preview_tx: &std_mpsc::SyncSender<VideoFrame>,
    stop: &AtomicBool,
    ready: &ReadySlot,
) -> Result<(), String> {
    // Declared first so they are dropped last, after the session.
    let _com = ComScope::enter()?;
    let _mf = MfScope::start()?;

    let mut converter = BgraConverter::default();
    let mut frame = Vec::new();
    let mut session = open_first_frame(device_id, target_height, stop, &mut converter, &mut frame)?;
    let anchor = Instant::now();

    // Every frame reaches the encoder and the bubble through the effects
    // pipeline, which applies the current background the way macOS does.
    let closed = Arc::new(AtomicBool::new(false));
    let sink_closed = Arc::clone(&closed);
    let sink_ready = Arc::clone(ready);
    let video_tx = video_tx.clone();
    let preview_tx = preview_tx.clone();
    let sink: effects::FrameSink = Box::new(move |frame: VideoFrame| {
        // The first processed frame is the proof the recording works; its
        // capture time anchors the timeline.
        report_ready(&sink_ready, Ok((frame.width, frame.height, frame.captured_at)));
        // Dropped when the preview thread is still busy with the last one.
        let _ = preview_tx.try_send(VideoFrame {
            bgra: frame.bgra.clone(),
            width: frame.width,
            height: frame.height,
            captured_at: frame.captured_at,
        });
        match video_tx.try_send(frame) {
            Ok(()) => true,
            Err(mpsc::error::TrySendError::Full(_)) => {
                tracing::debug!("camera frame dropped: encoder is behind");
                true
            }
            Err(mpsc::error::TrySendError::Closed(_)) => {
                sink_closed.store(true, Ordering::Release);
                false
            }
        }
    });
    let pipeline = effects::EffectsPipeline::start(
        model_path,
        Box::new(|| {
            crate::camera_effects::current()
                .ok()
                .map(|effects| effects::FrameSettings {
                    effects,
                    look: crate::camera_effects::current_look(),
                })
        }),
        sink,
        report_effects_error,
    )?;
    let emit = |frame: &[u8], width: u32, height: u32, at: Instant| -> bool {
        let queued = pipeline.push(VideoFrame {
            bgra: frame.to_vec(),
            width,
            height,
            captured_at: at,
        });
        if !queued {
            tracing::debug!("camera frame dropped: effects are behind");
        }
        !closed.load(Ordering::Acquire)
    };

    if !emit(&frame, session.width, session.height, anchor) {
        return Ok(());
    }
    let mut last_frame = anchor;
    while !stop.load(Ordering::Acquire) {
        // SAFETY: Category 8 — FFI contract. The session's COM objects live on
        // this thread, inside the COM and Media Foundation scopes above.
        match unsafe { session.next_frame(&mut converter, &mut frame) } {
            Ok(Read::Frame) => {
                let now = Instant::now();
                if now.duration_since(last_frame) < MIN_FRAME_GAP {
                    continue;
                }
                last_frame = now;
                if !emit(&frame, session.width, session.height, now) {
                    break;
                }
            }
            Ok(Read::Empty) => {}
            Ok(Read::Ended) => {
                tracing::warn!("camera stream ended during recording");
                break;
            }
            Err(err) => {
                tracing::warn!("camera read failed during recording: {err}");
                break;
            }
        }
    }
    // Frames already in the pipeline still reach the recording.
    pipeline.finish();
    Ok(())
}

/// Open the camera and read its first frame, retrying a device that is still
/// being released by the picker's preview.
fn open_first_frame(
    device_id: &str,
    target_height: u32,
    stop: &AtomicBool,
    converter: &mut BgraConverter,
    frame: &mut Vec<u8>,
) -> Result<CameraSession, String> {
    let started = Instant::now();
    loop {
        // SAFETY: Category 8 — FFI contract. Called on the camera thread inside
        // its COM and Media Foundation scopes.
        let attempt = unsafe { CameraSession::open(device_id, target_height) }.and_then(|mut session| {
            // A camera may report a few empty ticks before its first frame.
            for _ in 0..90 {
                match unsafe { session.next_frame(converter, frame) }? {
                    Read::Frame => return Ok(session),
                    Read::Empty => continue,
                    Read::Ended => break,
                }
            }
            Err("Camera did not deliver a frame.".to_string())
        });
        match attempt {
            Ok(session) => return Ok(session),
            Err(err)
                if err != CAMERA_UNAVAILABLE
                    && started.elapsed() < OPEN_RETRY_WINDOW
                    && !stop.load(Ordering::Acquire) =>
            {
                tracing::info!("camera not ready yet, retrying: {err}");
                std::thread::sleep(OPEN_RETRY_DELAY);
            }
            Err(err) => return Err(err),
        }
    }
}

enum Read {
    Frame,
    Empty,
    Ended,
}

/// The pixel layouts this reader asks Media Foundation for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SourceFormat {
    Nv12,
    Yuy2,
    Rgb32,
}

impl SourceFormat {
    fn subtype(self) -> GUID {
        match self {
            Self::Nv12 => MFVideoFormat_NV12,
            Self::Yuy2 => MFVideoFormat_YUY2,
            Self::Rgb32 => MFVideoFormat_RGB32,
        }
    }

    fn from_subtype(subtype: &GUID) -> Option<Self> {
        [Self::Nv12, Self::Yuy2, Self::Rgb32]
            .into_iter()
            .find(|format| format.subtype() == *subtype)
    }

    fn pixel(self) -> Pixel {
        match self {
            Self::Nv12 => Pixel::NV12,
            Self::Yuy2 => Pixel::YUYV422,
            // B, G, R, then a padding byte.
            Self::Rgb32 => Pixel::BGRZ,
        }
    }

    /// Row step of a 1-D buffer when the media type does not say; RGB is
    /// bottom-up by default, YUV top-down.
    fn default_stride(self, width: u32) -> i32 {
        let width = width as i32;
        match self {
            Self::Nv12 => width,
            Self::Yuy2 => width * 2,
            Self::Rgb32 => -(width * 4),
        }
    }

    /// Output layouts to try, cheapest first. The camera's own layout needs no
    /// conversion by Media Foundation, whose RGB converter is slow; decoded
    /// MJPEG comes out as YUY2.
    fn preference(native: Option<Self>) -> Vec<Self> {
        let mut order = Vec::with_capacity(3);
        for format in native.into_iter().chain([Self::Yuy2, Self::Nv12, Self::Rgb32]) {
            if !order.contains(&format) {
                order.push(format);
            }
        }
        order
    }
}

/// Shuts the media source down however the session ends.
struct SourceGuard(IMFMediaSource);

impl Drop for SourceGuard {
    fn drop(&mut self) {
        // SAFETY: Category 8 — FFI contract. Shutdown is required once per
        // activated source and is called exactly once, here.
        unsafe {
            let _ = self.0.Shutdown();
        }
    }
}

struct CameraSession {
    // Field order is drop order: the reader goes before its source.
    reader: IMFSourceReader,
    _source: SourceGuard,
    format: SourceFormat,
    width: u32,
    height: u32,
    /// Row step of an unlocked 1-D buffer; negative means bottom-up rows.
    default_stride: i32,
}

impl CameraSession {
    unsafe fn open(device_id: &str, target_height: u32) -> Result<Self, String> {
        let source = SourceGuard(activate_camera(device_id)?);

        let mut attributes = None;
        MFCreateAttributes(&mut attributes, 1).map_err(|e| format!("camera attributes: {e}"))?;
        let attributes = attributes.ok_or("camera attributes missing")?;
        // Only needed when a camera cannot give us one of the YUV layouts.
        attributes
            .SetUINT32(&MF_SOURCE_READER_ENABLE_VIDEO_PROCESSING, 1)
            .map_err(|e| format!("camera attributes: {e}"))?;
        let reader = MFCreateSourceReaderFromMediaSource(&source.0, &attributes)
            .map_err(|e| format!("Camera failed to start: {e}"))?;

        // Best mode first; a mode that will not produce any layout we read
        // falls through to the next one.
        let mut last_error = "Camera has no usable video format.".to_string();
        for mode in ranked_native_types(&reader, target_height)? {
            if let Err(e) = reader.SetCurrentMediaType(STREAM, None, &mode.media_type) {
                last_error = format!("Camera mode {}x{} rejected: {e}", mode.width, mode.height);
                continue;
            }
            for format in SourceFormat::preference(mode.format) {
                let output = MFCreateMediaType().and_then(|output| {
                    output.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Video)?;
                    output.SetGUID(&MF_MT_SUBTYPE, &format.subtype())?;
                    output.SetUINT64(&MF_MT_FRAME_SIZE, pack_size(mode.width, mode.height))?;
                    Ok(output)
                });
                match output.and_then(|output| reader.SetCurrentMediaType(STREAM, None, &output)) {
                    Ok(()) => {
                        let mut session = Self {
                            reader,
                            _source: source,
                            format,
                            width: mode.width,
                            height: mode.height,
                            default_stride: format.default_stride(mode.width),
                        };
                        session.refresh_format()?;
                        tracing::info!(
                            width = session.width,
                            height = session.height,
                            format = ?session.format,
                            "camera recording format"
                        );
                        return Ok(session);
                    }
                    Err(e) => {
                        last_error = format!(
                            "Camera mode {}x{} cannot produce {format:?}: {e}",
                            mode.width, mode.height
                        );
                    }
                }
            }
        }
        Err(last_error)
    }

    unsafe fn refresh_format(&mut self) -> Result<(), String> {
        let current = self
            .reader
            .GetCurrentMediaType(STREAM)
            .map_err(|e| format!("camera format: {e}"))?;
        let (width, height) = frame_size(&current)?;
        let format = current
            .GetGUID(&MF_MT_SUBTYPE)
            .ok()
            .and_then(|subtype| SourceFormat::from_subtype(&subtype))
            .ok_or("camera switched to an unsupported pixel format")?;
        self.format = format;
        self.width = width;
        self.height = height;
        self.default_stride = current
            .GetUINT32(&MF_MT_DEFAULT_STRIDE)
            .map(|stride| stride as i32)
            .unwrap_or_else(|_| format.default_stride(width));
        Ok(())
    }

    /// Blocks for the next sample and converts it into `out` as BGRA.
    unsafe fn next_frame(
        &mut self,
        converter: &mut BgraConverter,
        out: &mut Vec<u8>,
    ) -> Result<Read, String> {
        let mut flags = 0u32;
        let mut sample: Option<IMFSample> = None;
        self.reader
            .ReadSample(STREAM, 0, None, Some(&mut flags), None, Some(&mut sample))
            .map_err(|e| format!("camera read: {e}"))?;
        let flags = flags as i32;
        if flags & MF_SOURCE_READERF_ERROR.0 != 0 {
            return Err("Camera reported an error.".to_string());
        }
        if flags & MF_SOURCE_READERF_ENDOFSTREAM.0 != 0 {
            return Ok(Read::Ended);
        }
        if flags & MF_SOURCE_READERF_CURRENTMEDIATYPECHANGED.0 != 0 {
            self.refresh_format()?;
        }
        let Some(sample) = sample else {
            return Ok(Read::Empty);
        };
        let buffer = sample
            .ConvertToContiguousBuffer()
            .map_err(|e| format!("camera buffer: {e}"))?;
        let layout = FrameLayout {
            format: self.format,
            width: self.width,
            height: self.height,
        };

        // A 2-D buffer reports the top row and a signed pitch itself.
        if let Ok(buffer_2d) = buffer.cast::<IMF2DBuffer>() {
            let mut scanline0 = std::ptr::null_mut();
            let mut pitch = 0i32;
            buffer_2d
                .Lock2D(&mut scanline0, &mut pitch)
                .map_err(|e| format!("camera buffer lock: {e}"))?;
            let converted = converter.convert(layout, scanline0, pitch as isize, None, out);
            let _ = buffer_2d.Unlock2D();
            converted?;
            return Ok(Read::Frame);
        }

        let mut data = std::ptr::null_mut();
        let mut length = 0u32;
        buffer
            .Lock(&mut data, None, Some(&mut length))
            .map_err(|e| format!("camera buffer lock: {e}"))?;
        let pitch = self.default_stride as isize;
        // Bottom-up buffers start with the last row.
        let scanline0 = if pitch < 0 {
            data.wrapping_add(pitch.unsigned_abs() * (self.height as usize).saturating_sub(1))
        } else {
            data
        };
        let converted = converter.convert(layout, scanline0, pitch, Some(length as usize), out);
        let _ = buffer.Unlock();
        converted?;
        Ok(Read::Frame)
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
struct FrameLayout {
    format: SourceFormat,
    width: u32,
    height: u32,
}

/// One plane of a locked camera buffer: where its first row starts relative
/// to scan line 0, the signed step between rows, and how much of a row is used.
struct PlaneSpan {
    start: isize,
    step: isize,
    rows: usize,
    row_bytes: usize,
}

impl FrameLayout {
    fn planes(self, pitch: isize) -> Result<Vec<PlaneSpan>, String> {
        let (width, height) = (self.width as usize, self.height as usize);
        let chroma_width = width.div_ceil(2) * 2;
        let planes = match self.format {
            SourceFormat::Nv12 => {
                // NV12 is always top-down: full-size luma, then half-height
                // interleaved chroma straight after it.
                if pitch <= 0 {
                    return Err("camera NV12 frame has an unexpected row order".to_string());
                }
                vec![
                    PlaneSpan { start: 0, step: pitch, rows: height, row_bytes: width },
                    PlaneSpan {
                        start: pitch * height as isize,
                        step: pitch,
                        rows: height.div_ceil(2),
                        row_bytes: chroma_width,
                    },
                ]
            }
            SourceFormat::Yuy2 => vec![PlaneSpan {
                start: 0,
                step: pitch,
                rows: height,
                row_bytes: chroma_width * 2,
            }],
            SourceFormat::Rgb32 => vec![PlaneSpan {
                start: 0,
                step: pitch,
                rows: height,
                row_bytes: width * 4,
            }],
        };
        if planes.iter().any(|plane| plane.row_bytes > pitch.unsigned_abs()) {
            return Err("camera frame does not match its format".to_string());
        }
        Ok(planes)
    }
}

/// Converts camera buffers to BGRA with FFmpeg, reusing its frames and scaler
/// while the layout stays the same.
#[derive(Default)]
struct BgraConverter {
    layout: Option<FrameLayout>,
    scaler: Option<scaling::Context>,
    source: Option<frame::Video>,
    output: Option<frame::Video>,
}

impl BgraConverter {
    /// `length` is the locked allocation when known (a 1-D buffer), so a short
    /// buffer is refused instead of read past. For bottom-up RGB, `scanline0`
    /// is the top row inside that allocation.
    unsafe fn convert(
        &mut self,
        layout: FrameLayout,
        scanline0: *const u8,
        pitch: isize,
        length: Option<usize>,
        out: &mut Vec<u8>,
    ) -> Result<(), String> {
        if scanline0.is_null() || layout.width < 2 || layout.height < 2 {
            return Err("camera frame is empty".to_string());
        }
        let planes = layout.planes(pitch)?;
        if let Some(length) = length {
            let needed = planes
                .iter()
                .map(|plane| {
                    let span = pitch.unsigned_abs() * (plane.rows - 1) + plane.row_bytes;
                    match layout.format {
                        // Bottom-up RGB is addressed from its last row back.
                        SourceFormat::Rgb32 => span,
                        _ => plane.start as usize + span,
                    }
                })
                .max()
                .unwrap_or(0);
            if length < needed {
                return Err("camera frame is shorter than its format".to_string());
            }
        }

        if self.layout != Some(layout) {
            let pixel = layout.format.pixel();
            self.scaler = Some(
                scaling::Context::get(
                    pixel,
                    layout.width,
                    layout.height,
                    Pixel::BGRA,
                    layout.width,
                    layout.height,
                    scaling::flag::Flags::FAST_BILINEAR,
                )
                .map_err(|e| format!("camera converter: {e}"))?,
            );
            self.source = Some(frame::Video::new(pixel, layout.width, layout.height));
            self.output = Some(frame::Video::new(Pixel::BGRA, layout.width, layout.height));
            self.layout = Some(layout);
        }
        let (Some(scaler), Some(source), Some(output)) =
            (self.scaler.as_mut(), self.source.as_mut(), self.output.as_mut())
        else {
            return Err("camera converter missing".to_string());
        };

        for (index, plane) in planes.iter().enumerate() {
            let stride = source.stride(index);
            if plane.row_bytes > stride {
                return Err("camera frame is wider than its converter".to_string());
            }
            let data = source.data_mut(index);
            for y in 0..plane.rows {
                let row = std::slice::from_raw_parts(
                    scanline0.offset(plane.start + plane.step * y as isize),
                    plane.row_bytes,
                );
                data[y * stride..y * stride + plane.row_bytes].copy_from_slice(row);
            }
        }
        scaler
            .run(source, output)
            .map_err(|e| format!("camera conversion: {e}"))?;

        let (width, height) = (layout.width as usize, layout.height as usize);
        let row_bytes = width * 4;
        let stride = output.stride(0);
        let data = output.data(0);
        out.resize(row_bytes * height, 0);
        for y in 0..height {
            out[y * row_bytes..(y + 1) * row_bytes]
                .copy_from_slice(&data[y * stride..y * stride + row_bytes]);
        }
        Ok(())
    }
}

/// Find the camera by symbolic link (first camera when `device_id` is empty)
/// and activate it.
unsafe fn activate_camera(device_id: &str) -> Result<IMFMediaSource, String> {
    let mut attributes = None;
    MFCreateAttributes(&mut attributes, 1).map_err(|e| format!("camera attributes: {e}"))?;
    let attributes = attributes.ok_or("camera attributes missing")?;
    attributes
        .SetGUID(
            &MF_DEVSOURCE_ATTRIBUTE_SOURCE_TYPE,
            &MF_DEVSOURCE_ATTRIBUTE_SOURCE_TYPE_VIDCAP_GUID,
        )
        .map_err(|e| format!("camera enumeration: {e}"))?;

    let mut list: *mut Option<IMFActivate> = std::ptr::null_mut();
    let mut count = 0u32;
    MFEnumDeviceSources(&attributes, &mut list, &mut count)
        .map_err(|e| format!("camera enumeration: {e}"))?;
    if list.is_null() {
        return Err(CAMERA_UNAVAILABLE.to_string());
    }

    let mut chosen = None;
    for index in 0..count as usize {
        let Some(device) = (*list.add(index)).as_ref() else {
            continue;
        };
        let matches = device_id.is_empty()
            || symbolic_link(device).is_some_and(|link| link.eq_ignore_ascii_case(device_id));
        if matches && chosen.is_none() {
            chosen = Some(device.clone());
        }
    }
    // Each entry holds a reference, and the array itself is CoTaskMem.
    for index in 0..count as usize {
        std::ptr::drop_in_place(list.add(index));
    }
    CoTaskMemFree(Some(list.cast()));

    let device = chosen.ok_or_else(|| CAMERA_UNAVAILABLE.to_string())?;
    device
        .ActivateObject::<IMFMediaSource>()
        .map_err(|e| format!("Camera failed to start: {e}"))
}

unsafe fn symbolic_link(device: &IMFActivate) -> Option<String> {
    let mut value = PWSTR::null();
    let mut length = 0u32;
    device
        .GetAllocatedString(
            &MF_DEVSOURCE_ATTRIBUTE_SOURCE_TYPE_VIDCAP_SYMBOLIC_LINK,
            &mut value,
            &mut length,
        )
        .ok()?;
    let link = value.to_string().ok();
    CoTaskMemFree(Some(value.0.cast()));
    link
}

struct CameraMode {
    media_type: IMFMediaType,
    format: Option<SourceFormat>,
    width: u32,
    height: u32,
}

/// Native camera modes, best first.
unsafe fn ranked_native_types(
    reader: &IMFSourceReader,
    target_height: u32,
) -> Result<Vec<CameraMode>, String> {
    let mut modes = Vec::new();
    for index in 0.. {
        let native = match reader.GetNativeMediaType(STREAM, index) {
            Ok(native) => native,
            Err(e) if e.code() == MF_E_NO_MORE_TYPES => break,
            Err(e) => return Err(format!("camera formats: {e}")),
        };
        let Ok((width, height)) = frame_size(&native) else {
            continue;
        };
        let fps = native
            .GetUINT64(&MF_MT_FRAME_RATE)
            .ok()
            .map(|rate| {
                let (numerator, denominator) = ((rate >> 32) as f64, (rate & 0xFFFF_FFFF) as f64);
                if denominator > 0.0 { numerator / denominator } else { 0.0 }
            })
            .unwrap_or(0.0);
        let format = native
            .GetGUID(&MF_MT_SUBTYPE)
            .ok()
            .and_then(|subtype| SourceFormat::from_subtype(&subtype));
        let score = format_score(height, fps, target_height, format.is_some());
        modes.push((
            score,
            CameraMode {
                media_type: native,
                format,
                width,
                height,
            },
        ));
    }
    if modes.is_empty() {
        return Err("Camera has no usable video format.".to_string());
    }
    modes.sort_by(|a, b| b.0.cmp(&a.0));
    Ok(modes.into_iter().map(|(_, mode)| mode).collect())
}

/// Ordering key for a camera mode: smooth frame rate first, then the largest
/// height that fits the cap (or the smallest above it), then closest to 30 fps,
/// then a layout we read without a decoder.
fn format_score(height: u32, fps: f64, target_height: u32, uncompressed: bool) -> (bool, bool, u32, i64, bool) {
    let smooth = fps >= 23.5;
    let fits = height <= target_height;
    let size = if fits { height } else { u32::MAX - height };
    (smooth, fits, size, -((fps - 30.0).abs() * 100.0) as i64, uncompressed)
}

unsafe fn frame_size(media_type: &IMFMediaType) -> Result<(u32, u32), String> {
    let packed = media_type
        .GetUINT64(&MF_MT_FRAME_SIZE)
        .map_err(|e| format!("camera frame size: {e}"))?;
    let (width, height) = ((packed >> 32) as u32, packed as u32);
    if width < 2 || height < 2 {
        return Err("camera reported an unusable frame size".to_string());
    }
    Ok((width, height))
}

fn pack_size(width: u32, height: u32) -> u64 {
    ((width as u64) << 32) | height as u64
}

/// Encodes the latest frame for the bubble's self-view until the camera
/// thread closes the channel.
fn run_preview<R: Runtime>(app: &AppHandle<R>, frames: std_mpsc::Receiver<VideoFrame>) {
    let mut encoder = None;
    let mut reported = false;
    while let Ok(frame) = frames.recv() {
        match encode_preview(&frame, &mut encoder) {
            Ok(preview) => {
                let _ = app.emit_to("webcam", EVENT_NATIVE_WEBCAM_FRAME, preview);
            }
            Err(err) if !reported => {
                reported = true;
                tracing::warn!("camera preview frame failed: {err}");
            }
            Err(_) => {}
        }
    }
}

struct PreviewEncoder {
    input: (u32, u32),
    scaler: scaling::Context,
    encoder: ffmpeg::encoder::Video,
    source: frame::Video,
    scaled: frame::Video,
    next_pts: i64,
}

/// The preview size inside the budget, even-sized for 4:2:0 JPEG.
fn preview_size(width: u32, height: u32) -> (u32, u32) {
    let scale = (PREVIEW_MAX_WIDTH as f64 / width as f64)
        .min(PREVIEW_MAX_HEIGHT as f64 / height as f64)
        .min(1.0);
    let even = |value: f64| ((value.round() as u32) & !1).max(2);
    (even(width as f64 * scale), even(height as f64 * scale))
}

fn encode_preview(
    frame: &VideoFrame,
    state: &mut Option<PreviewEncoder>,
) -> Result<NativeWebcamFrameDto, String> {
    let input = (frame.width, frame.height);
    if state.as_ref().map(|state| state.input) != Some(input) {
        let (width, height) = preview_size(frame.width, frame.height);
        let codec = ffmpeg::encoder::find(ffmpeg::codec::Id::MJPEG)
            .ok_or("camera JPEG encoder is unavailable")?;
        let mut encoder = ffmpeg::codec::context::Context::new_with_codec(codec)
            .encoder()
            .video()
            .map_err(|e| format!("camera JPEG context: {e}"))?;
        encoder.set_width(width);
        encoder.set_height(height);
        encoder.set_format(Pixel::YUVJ420P);
        encoder.set_color_range(ffmpeg::color::Range::JPEG);
        encoder.set_time_base((1, 30));
        encoder.set_qmin(PREVIEW_JPEG_QUALITY);
        encoder.set_qmax(PREVIEW_JPEG_QUALITY);
        let encoder = encoder
            .open_as(codec)
            .map_err(|e| format!("camera JPEG encoder: {e}"))?;
        // A reduction, so AREA averages instead of aliasing (as on macOS).
        let scaler = scaling::Context::get(
            Pixel::BGRA,
            frame.width,
            frame.height,
            Pixel::YUVJ420P,
            width,
            height,
            scaling::flag::Flags::AREA,
        )
        .map_err(|e| format!("camera preview scaler: {e}"))?;
        *state = Some(PreviewEncoder {
            input,
            scaler,
            encoder,
            source: frame::Video::new(Pixel::BGRA, frame.width, frame.height),
            scaled: frame::Video::new(Pixel::YUVJ420P, width, height),
            next_pts: 0,
        });
    }
    let state = state.as_mut().ok_or("camera preview encoder missing")?;

    let row_bytes = frame.width as usize * 4;
    let stride = state.source.stride(0);
    let data = state.source.data_mut(0);
    for (y, row) in frame.bgra.chunks_exact(row_bytes).enumerate() {
        data[y * stride..y * stride + row_bytes].copy_from_slice(row);
    }
    state
        .scaler
        .run(&state.source, &mut state.scaled)
        .map_err(|e| format!("camera preview scale: {e}"))?;
    state.scaled.set_color_range(ffmpeg::color::Range::JPEG);
    state.scaled.set_pts(Some(state.next_pts));
    state.next_pts += 1;
    state
        .encoder
        .send_frame(&state.scaled)
        .map_err(|e| format!("camera JPEG frame: {e}"))?;
    let mut packet = ffmpeg::Packet::empty();
    state
        .encoder
        .receive_packet(&mut packet)
        .map_err(|e| format!("camera JPEG packet: {e}"))?;
    let jpeg = packet.data().ok_or("camera JPEG packet is empty")?;
    Ok(NativeWebcamFrameDto::from_jpeg(
        jpeg,
        state.scaled.width(),
        state.scaled.height(),
    ))
}

fn show_preview<R: Runtime>(app: &AppHandle<R>) {
    let show_app = app.clone();
    let _ = app.run_on_main_thread(move || {
        if let Some(window) = show_app.get_webview_window("webcam") {
            let _ = window.show();
            let _ = crate::cursor_track::set_ignore_cursor_events(&window, false);
        }
        let _ = show_app.emit_to("webcam", EVENT_NATIVE_WEBCAM_START, ());
    });
}

fn hide_preview<R: Runtime>(app: &AppHandle<R>) {
    let hide_app = app.clone();
    let _ = app.run_on_main_thread(move || {
        let _ = hide_app.emit_to("webcam", EVENT_NATIVE_WEBCAM_STOP, ());
        for label in ["webcam", "webcam-controls"] {
            if let Some(window) = hide_app.get_webview_window(label) {
                let _ = window.hide();
            }
        }
    });
}

pub(super) struct ComScope(bool);

impl ComScope {
    pub(super) fn enter() -> Result<Self, String> {
        // SAFETY: Category 8 — FFI contract. Balanced by `Drop` only when this
        // call took a reference.
        let result = unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) };
        if result.0 == RPC_E_CHANGED_MODE {
            Ok(Self(false))
        } else if result.is_err() {
            Err(format!("Camera failed to start: COM {result:?}"))
        } else {
            Ok(Self(true))
        }
    }
}

impl Drop for ComScope {
    fn drop(&mut self) {
        if self.0 {
            // SAFETY: Category 8 — FFI contract. Matches the successful
            // `CoInitializeEx` above, on the same thread.
            unsafe { CoUninitialize() };
        }
    }
}

struct MfScope;

impl MfScope {
    fn start() -> Result<Self, String> {
        // SAFETY: Category 8 — FFI contract. Balanced by `MFShutdown` in `Drop`.
        unsafe { MFStartup(MF_VERSION, MFSTARTUP_FULL) }
            .map_err(|e| format!("Camera failed to start: Media Foundation {e}"))?;
        Ok(Self)
    }
}

impl Drop for MfScope {
    fn drop(&mut self) {
        // SAFETY: Category 8 — FFI contract. Matches the `MFStartup` above.
        unsafe {
            let _ = MFShutdown();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn convert(
        format: SourceFormat,
        width: u32,
        height: u32,
        memory: &[u8],
        top_row: usize,
        pitch: isize,
    ) -> Result<Vec<u8>, String> {
        crate::encoder::Mp4Encoder::init_global();
        let mut out = Vec::new();
        let layout = FrameLayout { format, width, height };
        // SAFETY: `top_row` and `pitch` stay inside `memory` for every test.
        unsafe {
            BgraConverter::default().convert(
                layout,
                memory.as_ptr().add(top_row),
                pitch,
                Some(memory.len()),
                &mut out,
            )
        }?;
        Ok(out)
    }

    #[test]
    fn camera_modes_prefer_smooth_then_largest_within_the_cap() {
        let best_1080 = format_score(1080, 30.0, 1080, false);
        assert!(best_1080 > format_score(720, 30.0, 1080, true));
        assert!(best_1080 > format_score(2160, 30.0, 1080, true));
        assert!(format_score(720, 30.0, 1080, true) > format_score(1080, 5.0, 1080, true));
        assert!(format_score(720, 30.0, 720, true) > format_score(1080, 30.0, 720, true));
        // Nothing fits: the smallest mode above the cap wins.
        assert!(format_score(720, 30.0, 480, true) > format_score(1080, 30.0, 480, true));
        assert!(format_score(720, 30.0, 720, true) > format_score(720, 60.0, 720, true));
        // Otherwise equal, a layout read without a decoder wins.
        assert!(format_score(720, 30.0, 720, true) > format_score(720, 30.0, 720, false));
    }

    #[test]
    fn native_yuv_layouts_are_asked_for_before_rgb() {
        use SourceFormat::*;
        assert_eq!(SourceFormat::preference(Some(Nv12)), [Nv12, Yuy2, Rgb32]);
        assert_eq!(SourceFormat::preference(None), [Yuy2, Nv12, Rgb32]);
        assert_eq!(SourceFormat::preference(Some(Rgb32)), [Rgb32, Yuy2, Nv12]);
    }

    #[test]
    fn bottom_up_rgb32_becomes_top_down_opaque_bgra() {
        // Rows in memory: bottom (blue-ish) first, then top (red-ish).
        let memory = [
            200u8, 10, 10, 0, 200, 10, 10, 0, //
            10, 10, 200, 0, 10, 10, 200, 0,
        ];
        let out = convert(SourceFormat::Rgb32, 2, 2, &memory, 8, -8).unwrap();
        assert_eq!(&out[..4], &[10, 10, 200, 255]);
        assert_eq!(&out[8..12], &[200, 10, 10, 255]);
        // A buffer shorter than two rows is refused.
        assert!(convert(SourceFormat::Rgb32, 2, 3, &memory, 16, -8).is_err());
    }

    #[test]
    fn nv12_grey_converts_to_grey() {
        // 2x2 luma at 128, one chroma pair at neutral.
        let memory = [128u8, 128, 128, 128, 128, 128, 128, 128];
        let out = convert(SourceFormat::Nv12, 2, 2, &memory, 0, 2).unwrap();
        assert_eq!(out.len(), 16);
        for pixel in out.chunks_exact(4) {
            for channel in &pixel[..3] {
                assert!((*channel as i32 - 128).abs() < 16, "{pixel:?}");
            }
            assert_eq!(pixel[3], 255);
        }
        // Chroma plane missing from the buffer.
        assert!(convert(SourceFormat::Nv12, 2, 2, &memory[..4], 0, 2).is_err());
    }

    #[test]
    fn preview_stays_inside_the_macos_budget() {
        assert_eq!(preview_size(1920, 1080), (960, 540));
        assert_eq!(preview_size(640, 480), (640, 480));
        assert_eq!(preview_size(1280, 1024), (674, 540));
        let frame = VideoFrame {
            bgra: vec![20u8, 40, 60, 255].repeat(1280 * 720),
            width: 1280,
            height: 720,
            captured_at: Instant::now(),
        };
        crate::encoder::Mp4Encoder::init_global();
        let mut state = None;
        assert!(encode_preview(&frame, &mut state).is_ok());
        assert!(encode_preview(&frame, &mut state).is_ok());
    }
}
