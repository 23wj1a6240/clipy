#[cfg(target_os = "macos")]
use std::{
    sync::{
        atomic::{AtomicBool, Ordering},
        mpsc::{self, Sender},
        Arc, OnceLock,
    },
    thread::JoinHandle,
    time::{Duration, Instant},
};

#[cfg(target_os = "macos")]
use cap_camera_ffmpeg::CapturedFrameExt;
#[cfg(target_os = "macos")]
use ffmpeg::{format::Pixel, frame, software};
#[cfg(target_os = "windows")]
use serde::Serialize;
#[cfg(target_os = "macos")]
use tauri::Emitter;
use tauri::{AppHandle, Runtime};

pub const EVENT_NATIVE_WEBCAM_START: &str = "native-webcam-start";
pub const EVENT_NATIVE_WEBCAM_STOP: &str = "native-webcam-stop";
#[cfg(target_os = "windows")]
pub const EVENT_NATIVE_WEBCAM_FRAME: &str = "native-webcam-frame";

/// Preview budget, as a bounding box the scaled frame must fit inside.
///
/// This is not only the preview: ScreenCaptureKit records the bubble window,
/// so whatever resolution arrives here is the resolution of the camera in the
/// finished video. The old 640x360 was short of what the bubble asks for on
/// every Retina Mac — a Large bubble is 450 logical points, so 900 device
/// pixels, and a square or circular one crops the sides away and shows only
/// the middle 360 of those 360 rows. That is a 2.5x upscale, and it is what
/// made the camera read as soft.
///
/// 960x540 keeps the same 16:9 shape and hands a square crop 540 rows instead
/// of 360. Frames reach the bubble uncompressed over the local preview socket
/// (`camera_preview_ws`), about 2 MB each, which a loopback socket carries at
/// 30 fps without effort. Background effects get the same budget: their
/// alpha channel used to force a lossless PNG, which is why they once had a
/// smaller one.
#[cfg(target_os = "macos")]
const PREVIEW_MAX_WIDTH: u32 = 960;
#[cfg(target_os = "macos")]
const PREVIEW_MAX_HEIGHT: u32 = 540;
#[cfg(target_os = "macos")]
const PREVIEW_MIN_FRAME_INTERVAL: Duration = Duration::from_micros(33_333);
#[cfg(target_os = "macos")]
const PREVIEW_FRAME_JITTER: Duration = Duration::from_millis(1);
#[cfg(target_os = "macos")]
const CAMERA_START_TIMEOUT: Duration = Duration::from_secs(12);

/// A preview frame for the event bridge, which only the Windows camera
/// recorder still uses. macOS sends raw frames over `camera_preview_ws`.
#[cfg(target_os = "windows")]
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct NativeWebcamFrameDto {
    data_url: String,
    width: u32,
    height: u32,
}

#[cfg(target_os = "windows")]
impl NativeWebcamFrameDto {
    /// A preview frame from the Windows camera recorder, already JPEG-encoded.
    pub(crate) fn from_jpeg(jpeg: &[u8], width: u32, height: u32) -> Self {
        use base64::Engine;
        Self {
            data_url: format!(
                "data:image/jpeg;base64,{}",
                base64::engine::general_purpose::STANDARD.encode(jpeg)
            ),
            width,
            height,
        }
    }
}

#[cfg(target_os = "macos")]
struct NativeWebcamSession {
    stop_tx: Sender<()>,
    worker: Option<JoinHandle<()>>,
}

#[cfg(target_os = "macos")]
impl Drop for NativeWebcamSession {
    fn drop(&mut self) {
        let _ = self.stop_tx.send(());
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

/// Camera lifecycle state machine. Camera startup takes seconds
/// (AVFoundation), and a session only becomes stoppable once it is
/// registered here — so start/stop MUST coordinate through one lock, or a
/// stop that lands during the startup window finds nothing to stop and the
/// fresh session registers afterward with nothing ever stopping it: a live
/// camera (green light on) behind a hidden bubble window.
///
/// Protocol: every lifecycle operation (start or stop) bumps `generation`
/// under the lock. A start claims its generation at the same instant it
/// takes the previous session; registration of the started camera succeeds
/// only if no other operation bumped the generation in between. A
/// superseded start tears down its own fresh session and touches nothing
/// else — the superseding operation owns the shared state.
#[cfg(target_os = "macos")]
struct WebcamLifecycle {
    state: parking_lot::Mutex<LifecycleState>,
}

#[cfg(target_os = "macos")]
struct LifecycleState {
    generation: u64,
    recording_epoch: u64,
    recording_reserved: bool,
    session: Option<NativeWebcamSession>,
}

#[cfg(target_os = "macos")]
impl WebcamLifecycle {
    fn new() -> Self {
        Self {
            state: parking_lot::Mutex::new(LifecycleState {
                generation: 0,
                recording_epoch: 0,
                recording_reserved: false,
                session: None,
            }),
        }
    }

    /// Claim a start: atomically bump + snapshot the generation and take
    /// whatever session was running. The caller drops the returned session
    /// (joining its worker) outside the lock.
    fn begin_start(&self, request_token: u64) -> Option<(u64, Option<NativeWebcamSession>)> {
        let mut state = self.state.lock();
        if state.recording_reserved || state.recording_epoch != request_token {
            return None;
        }
        state.generation += 1;
        let generation = state.generation;
        Some((generation, state.session.take()))
    }

    fn reserve_recording(&self) -> Result<Option<NativeWebcamSession>, String> {
        let mut state = self.state.lock();
        if state.recording_reserved {
            return Err(
                "The previous camera capture is still closing. Try again shortly.".to_string(),
            );
        }
        state.recording_reserved = true;
        state.recording_epoch += 1;
        state.generation += 1;
        Ok(state.session.take())
    }

    fn release_recording(&self) {
        self.state.lock().recording_reserved = false;
    }

    fn with_current_preview(&self, generation: u64, action: impl FnOnce()) -> bool {
        let state = self.state.lock();
        if state.recording_reserved || state.generation != generation {
            return false;
        }
        action();
        true
    }

    fn with_current_request(&self, token: u64, action: impl FnOnce()) -> bool {
        let state = self.state.lock();
        if state.recording_reserved || state.recording_epoch != token {
            return false;
        }
        action();
        true
    }

    fn cleanup_is_current(&self, token: u64) -> bool {
        self.state.lock().recording_epoch == token
    }

    /// Register a started camera. Fails — handing the session back for the
    /// caller to tear down — if any other start or stop happened after the
    /// caller's `begin_start` claim.
    fn try_register(
        &self,
        generation: u64,
        session: NativeWebcamSession,
    ) -> Result<(), NativeWebcamSession> {
        let mut state = self.state.lock();
        if state.recording_reserved || state.generation != generation {
            return Err(session);
        }
        state.session = Some(session);
        Ok(())
    }

    fn begin_request(&self) -> u64 {
        let mut state = self.state.lock();
        state.recording_epoch += 1;
        state.recording_epoch
    }

    fn request_token(&self) -> u64 {
        self.state.lock().recording_epoch
    }

    fn try_begin_request(&self, expected: u64) -> Option<u64> {
        let mut state = self.state.lock();
        if state.recording_epoch != expected {
            return None;
        }
        state.recording_epoch += 1;
        Some(state.recording_epoch)
    }

    /// Invalidate both queued requests and starts already in progress.
    /// Return the stop token so delayed window cleanup cannot hide a newer preview.
    fn take_for_stop(&self) -> (u64, Option<NativeWebcamSession>) {
        let mut state = self.state.lock();
        Self::stop_locked(&mut state)
    }

    fn take_for_stop_if_current(
        &self,
        expected: u64,
    ) -> Option<(u64, Option<NativeWebcamSession>)> {
        let mut state = self.state.lock();
        if state.recording_epoch != expected {
            return None;
        }
        Some(Self::stop_locked(&mut state))
    }

    fn stop_locked(state: &mut LifecycleState) -> (u64, Option<NativeWebcamSession>) {
        state.generation += 1;
        state.recording_epoch += 1;
        (state.recording_epoch, state.session.take())
    }

    fn is_active(&self) -> bool {
        self.state.lock().session.is_some()
    }
}

#[cfg(target_os = "macos")]
static NATIVE_WEBCAM_LIFECYCLE: OnceLock<WebcamLifecycle> = OnceLock::new();

#[cfg(target_os = "macos")]
static CAMERA_DEVICE_GATE: parking_lot::Mutex<()> = parking_lot::Mutex::new(());

#[cfg(target_os = "macos")]
pub(crate) fn lock_camera_device() -> parking_lot::MutexGuard<'static, ()> {
    CAMERA_DEVICE_GATE.lock()
}

#[cfg(target_os = "macos")]
pub(crate) struct RecordingCameraReservation {
    previous_preview: Option<NativeWebcamSession>,
}

#[cfg(target_os = "macos")]
impl RecordingCameraReservation {
    pub(crate) fn stop_previous_preview(&mut self) {
        drop(self.previous_preview.take());
    }
}

#[cfg(target_os = "macos")]
impl Drop for RecordingCameraReservation {
    fn drop(&mut self) {
        self.stop_previous_preview();
        lifecycle().release_recording();
    }
}

#[cfg(target_os = "macos")]
pub(crate) fn reserve_camera_recording() -> Result<RecordingCameraReservation, String> {
    Ok(RecordingCameraReservation {
        previous_preview: lifecycle().reserve_recording()?,
    })
}

pub(crate) fn begin_preview_request() -> u64 {
    #[cfg(target_os = "macos")]
    {
        lifecycle().begin_request()
    }
    #[cfg(not(target_os = "macos"))]
    {
        0
    }
}

pub(crate) fn preview_request_token() -> u64 {
    #[cfg(target_os = "macos")]
    {
        lifecycle().request_token()
    }
    #[cfg(not(target_os = "macos"))]
    {
        0
    }
}

pub(crate) fn try_begin_preview_request(expected: u64) -> Option<u64> {
    #[cfg(target_os = "macos")]
    {
        lifecycle().try_begin_request(expected)
    }
    #[cfg(not(target_os = "macos"))]
    {
        (expected == 0).then_some(0)
    }
}

pub(crate) fn preview_request_is_current(token: u64) -> bool {
    #[cfg(target_os = "macos")]
    {
        let state = lifecycle().state.lock();
        !state.recording_reserved && state.recording_epoch == token
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = token;
        true
    }
}

pub(crate) fn preview_cleanup_is_current(token: u64) -> bool {
    #[cfg(target_os = "macos")]
    {
        lifecycle().cleanup_is_current(token)
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = token;
        true
    }
}

#[cfg(target_os = "macos")]
fn lifecycle() -> &'static WebcamLifecycle {
    NATIVE_WEBCAM_LIFECYCLE.get_or_init(WebcamLifecycle::new)
}

pub fn native_webcam_active() -> bool {
    #[cfg(target_os = "macos")]
    {
        lifecycle().is_active()
    }
    #[cfg(not(target_os = "macos"))]
    {
        false
    }
}

/// Outcome of a successful `start_native_webcam` call.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NativeWebcamStartOutcome {
    /// The camera started and is registered as the live session.
    Started,
    /// The camera started but a concurrent stop (or newer start) claimed
    /// the lifecycle first; the fresh session was torn down again. The
    /// superseding operation owns the bubble state — the caller must NOT
    /// run its failure cleanup (hide window / clear requested device), or
    /// it would stomp that operation's work.
    Superseded,
}

pub fn start_native_webcam<R: Runtime + 'static>(
    app: &AppHandle<R>,
    device_id: Option<String>,
    request_token: u64,
) -> Result<NativeWebcamStartOutcome, String> {
    #[cfg(target_os = "macos")]
    {
        // Claim the generation and take the old session in one lock
        // acquisition — snapshotting after a separate stop call would leave
        // a window where a concurrent stop's bump goes unseen and the
        // registration check below passes anyway (orphaned live camera).
        let Some((start_generation, old_session)) = lifecycle().begin_start(request_token) else {
            return Ok(NativeWebcamStartOutcome::Superseded);
        };
        let had_old_session = old_session.is_some();
        drop(old_session);
        clear_preview_frames();
        if had_old_session {
            lifecycle().with_current_preview(start_generation, || {
                let _ = app.emit_to("webcam", EVENT_NATIVE_WEBCAM_STOP, ());
            });
        }

        let app_for_worker = app.clone();
        let (ready_tx, ready_rx) = mpsc::sync_channel(1);
        let (stop_tx, stop_rx) = mpsc::channel();
        let cancelled = Arc::new(AtomicBool::new(false));
        let worker_cancelled = cancelled.clone();
        let worker = std::thread::spawn(move || {
            let _device = lock_camera_device();
            if worker_cancelled.load(Ordering::Acquire)
                || !lifecycle().with_current_preview(start_generation, || {})
            {
                let _ = ready_tx.send(Ok(()));
                return;
            }
            if let Err(err) = run_native_webcam_worker(
                app_for_worker,
                device_id,
                ready_tx,
                stop_rx,
                worker_cancelled,
                start_generation,
            ) {
                tracing::error!("native webcam worker failed: {err}");
            }
        });

        let ready = match ready_rx.recv_timeout(CAMERA_START_TIMEOUT) {
            Ok(ready) => ready,
            Err(mpsc::RecvTimeoutError::Timeout) => {
                cancelled.store(true, Ordering::Release);
                drop(stop_tx);
                return Err("native camera timed out before start".to_string());
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                let _ = worker.join();
                return Err("native camera worker stopped before start".to_string());
            }
        };

        match ready {
            Ok(()) => {
                let session = NativeWebcamSession {
                    stop_tx,
                    worker: Some(worker),
                };
                match lifecycle().try_register(start_generation, session) {
                    Ok(()) => Ok(NativeWebcamStartOutcome::Started),
                    Err(session) => {
                        // A stop or newer start raced in while the camera
                        // was spinning up; it found nothing to take, so
                        // registering now would orphan a live camera. Tear
                        // this one down instead (Drop sends stop + joins
                        // the worker → stop_capturing).
                        tracing::info!(
                            "native camera start superseded by a concurrent stop; tearing down"
                        );
                        drop(session);
                        Ok(NativeWebcamStartOutcome::Superseded)
                    }
                }
            }
            Err(err) => {
                let _ = worker.join();
                Err(err)
            }
        }
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = (app, device_id, request_token);
        Ok(NativeWebcamStartOutcome::Started)
    }
}

pub fn stop_native_webcam<R: Runtime>(app: &AppHandle<R>) -> u64 {
    #[cfg(target_os = "macos")]
    {
        finish_native_webcam_stop(app, lifecycle().take_for_stop())
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = app;
        0
    }
}

pub fn stop_native_webcam_if_current<R: Runtime>(app: &AppHandle<R>, expected: u64) -> Option<u64> {
    #[cfg(target_os = "macos")]
    {
        lifecycle()
            .take_for_stop_if_current(expected)
            .map(|stop| finish_native_webcam_stop(app, stop))
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = app;
        (expected == 0).then_some(0)
    }
}

#[cfg(target_os = "macos")]
fn finish_native_webcam_stop<R: Runtime>(
    app: &AppHandle<R>,
    (token, session): (u64, Option<NativeWebcamSession>),
) -> u64 {
    tracing::debug!(
        request = token,
        active = session.is_some(),
        "native webcam stop requested"
    );
    // The stop bumped the generation, so no frame of this session can be
    // published after this.
    clear_preview_frames();
    if let Some(session) = session {
        // Signal immediately, but never join AVFoundation on AppKit's thread.
        // The device gate still prevents a new capture from opening before
        // this worker has released the physical camera.
        let _ = session.stop_tx.send(());
        lifecycle().with_current_request(token, || {
            let _ = app.emit_to("webcam", EVENT_NATIVE_WEBCAM_STOP, ());
        });
        tauri::async_runtime::spawn_blocking(move || drop(session));
    }
    token
}

#[cfg(target_os = "macos")]
fn run_native_webcam_worker<R: Runtime + 'static>(
    app: AppHandle<R>,
    device_id: Option<String>,
    ready_tx: mpsc::SyncSender<Result<(), String>>,
    stop_rx: mpsc::Receiver<()>,
    cancelled: Arc<AtomicBool>,
    generation: u64,
) -> Result<(), String> {
    crate::encoder::Mp4Encoder::init_global();

    let camera = match select_camera(device_id.as_deref()) {
        Ok(camera) => camera,
        Err(err) => {
            let _ = ready_tx.send(Err(err.clone()));
            return Err(err);
        }
    };
    let format = match select_format(&camera) {
        Ok(format) => format,
        Err(err) => {
            let _ = ready_tx.send(Err(err.clone()));
            return Err(err);
        }
    };
    let app_for_frames = app.clone();
    tracing::debug!(
        width = format.width(),
        height = format.height(),
        fps = format.frame_rate(),
        "native webcam capture format"
    );
    let callback_cancelled = cancelled.clone();
    let mut last_frame_sent: Option<Instant> = None;
    let mut converter: Option<PreviewConverter> = None;
    let mut preview_stats_started = Instant::now();
    let mut preview_frames_sent = 0_u64;
    let mut failed_effects = None;

    let handle = match camera.start_capturing(format, move |captured| {
        if callback_cancelled.load(Ordering::Acquire)
            || !lifecycle().with_current_preview(generation, || {})
        {
            return;
        }
        let now = Instant::now();
        if !preview_frame_due(last_frame_sent, now) {
            return;
        }
        last_frame_sent = Some(now);

        let Ok(frame) = captured.as_ffmpeg() else {
            return;
        };
        match preview_frame(frame, &mut converter) {
            Ok(frame) => {
                failed_effects = None;
                // Published under the lifecycle lock, so a frame from a
                // session that was just stopped can never follow the stop.
                lifecycle().with_current_preview(generation, || {
                    if publish_preview_frame(frame) {
                        preview_frames_sent += 1;
                    }
                });
                let elapsed = preview_stats_started.elapsed().as_secs_f64();
                if elapsed >= 5.0 {
                    tracing::debug!(
                        fps = preview_frames_sent as f64 / elapsed,
                        "native webcam preview cadence"
                    );
                    preview_frames_sent = 0;
                    preview_stats_started = Instant::now();
                }
            }
            Err(err) => {
                let settings = crate::camera_effects::current();
                if failed_effects.as_ref() != Some(&settings) {
                    let _ = app_for_frames.emit_to("webcam", "camera-effects-error", err.clone());
                    let _ = app_for_frames.emit_to(
                        "webcam-controls",
                        "camera-effects-error",
                        err.clone(),
                    );
                    failed_effects = Some(settings);
                    tracing::warn!("native webcam frame encode failed: {err}");
                }
            }
        }
    }) {
        Ok(handle) => handle,
        Err(err) => {
            let message = format!("native camera failed to start: {err}");
            let _ = ready_tx.send(Err(message.clone()));
            return Err(message);
        }
    };

    if cancelled.load(Ordering::Acquire) || !lifecycle().with_current_preview(generation, || {}) {
        handle
            .stop_capturing()
            .map_err(|err| format!("native camera failed to stop after timeout: {err}"))?;
        let _ = ready_tx.send(Ok(()));
        return Ok(());
    }

    let mut emitted = Ok(());
    lifecycle().with_current_preview(generation, || {
        emitted = app.emit_to("webcam", EVENT_NATIVE_WEBCAM_START, ());
    });
    if let Err(err) = emitted {
        let message = format!("native camera start event failed: {err}");
        let _ = ready_tx.send(Err(message.clone()));
        return Err(message);
    }
    let _ = ready_tx.send(Ok(()));

    tracing::info!(generation, "native webcam capture started");

    let _ = stop_rx.recv();
    handle
        .stop_capturing()
        .map_err(|err| format!("native camera failed to stop: {err}"))?;
    tracing::info!(generation, "native webcam capture stopped");
    Ok(())
}

#[cfg(target_os = "macos")]
fn preview_frame_due(last_sent: Option<Instant>, now: Instant) -> bool {
    last_sent.is_none_or(|last| {
        now.saturating_duration_since(last) + PREVIEW_FRAME_JITTER >= PREVIEW_MIN_FRAME_INTERVAL
    })
}

#[cfg(target_os = "macos")]
fn select_camera(device_id: Option<&str>) -> Result<cap_camera::CameraInfo, String> {
    let cameras = cap_camera::list_cameras().collect::<Vec<_>>();
    if cameras.is_empty() {
        return Err("No camera detected. Connect a camera or open the MacBook lid.".to_string());
    }

    if let Some(device_id) = device_id {
        if let Some(camera) = cameras
            .iter()
            .find(|camera| camera.device_id() == device_id)
        {
            return Ok(camera.clone());
        }
    }

    Ok(cameras
        .into_iter()
        .next()
        .expect("non-empty cameras list has first camera"))
}

#[cfg(target_os = "macos")]
fn select_format(camera: &cap_camera::CameraInfo) -> Result<cap_camera::Format, String> {
    let mut formats = camera
        .formats()
        .ok_or_else(|| "Selected camera has no supported formats.".to_string())?;
    if formats.is_empty() {
        return Err("Selected camera has no supported formats.".to_string());
    }

    formats.sort_by(|a, b| {
        let target_aspect = 16.0 / 9.0;
        let aspect_a = (a.width() as f32 / a.height() as f32 - target_aspect).abs();
        let aspect_b = (b.width() as f32 / b.height() as f32 - target_aspect).abs();
        let usable_a = a.width() <= 1920 && a.height() <= 1080 && a.frame_rate() >= 24.0;
        let usable_b = b.width() <= 1920 && b.height() <= 1080 && b.frame_rate() >= 24.0;

        usable_b
            .cmp(&usable_a)
            .then_with(|| {
                let rate_a = (a.frame_rate().round() as u32).abs_diff(30);
                let rate_b = (b.frame_rate().round() as u32).abs_diff(30);
                rate_a.cmp(&rate_b)
            })
            .then_with(|| {
                aspect_a
                    .partial_cmp(&aspect_b)
                    .unwrap_or(std::cmp::Ordering::Equal)
            })
            .then_with(|| (b.width() * b.height()).cmp(&(a.width() * a.height())))
    });

    Ok(formats.remove(0))
}

#[cfg(target_os = "macos")]
static PREVIEW_SERVER: OnceLock<Option<Arc<crate::camera_preview_ws::CameraPreviewServer>>> =
    OnceLock::new();

/// The local socket the bubble reads camera frames from, started on first
/// use. `None` only if 127.0.0.1 could not be bound at all.
#[cfg(target_os = "macos")]
fn preview_server() -> Option<&'static Arc<crate::camera_preview_ws::CameraPreviewServer>> {
    PREVIEW_SERVER
        .get_or_init(
            || match crate::camera_preview_ws::CameraPreviewServer::bind() {
                Ok((server, accept)) => {
                    tauri::async_runtime::spawn(accept);
                    tracing::info!(port = server.port(), "camera preview socket listening");
                    Some(server)
                }
                Err(err) => {
                    tracing::error!("camera preview socket could not bind: {err}");
                    None
                }
            },
        )
        .as_ref()
}

/// URL of the camera preview socket, for the bubble window.
pub fn camera_preview_url() -> Option<String> {
    #[cfg(target_os = "macos")]
    {
        preview_server().map(|server| server.url())
    }
    #[cfg(not(target_os = "macos"))]
    {
        None
    }
}

/// Hands a frame to the bubble. Replaces any frame the page has not taken
/// yet, so the bubble always shows the newest one.
#[cfg(target_os = "macos")]
pub(crate) fn publish_preview_frame(frame: crate::camera_preview_ws::PreviewFrame) -> bool {
    preview_server().is_some_and(|server| server.publish(frame))
}

/// Forgets the last frame so the next session cannot open on an old face.
#[cfg(target_os = "macos")]
pub(crate) fn clear_preview_frames() {
    if let Some(server) = PREVIEW_SERVER.get().and_then(Option::as_ref) {
        server.clear();
    }
}

#[cfg(target_os = "macos")]
pub(crate) struct PreviewConverter {
    converter: software::scaling::Context,
    effects: crate::camera_effects::Processor,
}

/// Scales a camera frame for the bubble and applies the camera effects.
#[cfg(target_os = "macos")]
pub(crate) fn preview_frame(
    frame: frame::Video,
    converter: &mut Option<PreviewConverter>,
) -> Result<crate::camera_preview_ws::PreviewFrame, String> {
    build_preview_frame(frame, converter, true)
}

/// Scales a frame whose effects were already applied upstream, as the
/// camera recording path does.
#[cfg(target_os = "macos")]
pub(crate) fn processed_preview_frame(
    frame: frame::Video,
    converter: &mut Option<PreviewConverter>,
) -> Result<crate::camera_preview_ws::PreviewFrame, String> {
    build_preview_frame(frame, converter, false)
}

#[cfg(target_os = "macos")]
fn build_preview_frame(
    frame: frame::Video,
    converter: &mut Option<PreviewConverter>,
    apply_effects: bool,
) -> Result<crate::camera_preview_ws::PreviewFrame, String> {
    let (width, height) = scaled_size(
        frame.width(),
        frame.height(),
        PREVIEW_MAX_WIDTH,
        PREVIEW_MAX_HEIGHT,
    );
    let converter_ref = match converter {
        Some(existing)
            if existing.converter.input().format == frame.format()
                && existing.converter.input().width == frame.width()
                && existing.converter.input().height == frame.height()
                && existing.converter.output().width == width
                && existing.converter.output().height == height =>
        {
            existing
        }
        _ => {
            let scaler = software::scaling::Context::get(
                frame.format(),
                frame.width(),
                frame.height(),
                Pixel::RGBA,
                width,
                height,
                // Every camera worth using hands us more pixels than the
                // bubble shows, so this scaler only ever shrinks. FAST_BILINEAR
                // reads four neighbours and drops the rest, which on a 2x
                // reduction throws away half the detail and aliases the half it
                // keeps — visible as shimmer on hair and on anything striped
                // behind you. AREA averages every source pixel that lands in a
                // destination pixel, which is the correct filter for a
                // reduction and is why this is a sharpness fix and not a
                // quality/cost trade.
                software::scaling::flag::Flags::AREA,
            )
            .map_err(|err| format!("camera scaler: {err:?}"))?;
            converter.insert(PreviewConverter {
                converter: scaler,
                effects: Default::default(),
            })
        }
    };

    let mut scaled = frame::Video::new(Pixel::RGBA, width, height);
    converter_ref
        .converter
        .run(&frame, &mut scaled)
        .map_err(|err| format!("camera scale: {err:?}"))?;
    if apply_effects {
        scaled = converter_ref.effects.process(scaled, false)?;
    }
    if scaled.format() != Pixel::RGBA {
        return Err(format!(
            "camera preview frame is {:?}, expected RGBA",
            scaled.format()
        ));
    }
    let rgba = tight_rgba(scaled.data(0), scaled.stride(0), width, height)?;
    Ok(crate::camera_preview_ws::PreviewFrame {
        width,
        height,
        rgba,
    })
}

/// Copies an RGBA plane into a tightly packed buffer with straight alpha,
/// leaving room for the socket trailer so sending it never reallocates.
///
/// The background effects return premultiplied pixels where the cut-out edge
/// is partly transparent; `ImageData` in the page expects straight alpha.
/// Opaque pixels, which is every pixel without an effect, are copied as is.
#[cfg(target_os = "macos")]
fn tight_rgba(plane: &[u8], stride: usize, width: u32, height: u32) -> Result<Vec<u8>, String> {
    let row_bytes = width as usize * 4;
    let rows = height as usize;
    if rows == 0 || stride < row_bytes || plane.len() < stride * (rows - 1) + row_bytes {
        return Err("camera preview frame is smaller than its dimensions".to_string());
    }
    let mut pixels = Vec::with_capacity(row_bytes * rows + crate::camera_preview_ws::TRAILER_LEN);
    for row in plane.chunks(stride).take(rows) {
        pixels.extend_from_slice(&row[..row_bytes]);
    }
    for pixel in pixels.chunks_exact_mut(4) {
        let alpha = u16::from(pixel[3]);
        if alpha > 0 && alpha < 255 {
            for channel in &mut pixel[..3] {
                *channel = ((u16::from(*channel) * 255 / alpha).min(255)) as u8;
            }
        }
    }
    Ok(pixels)
}

#[cfg(target_os = "macos")]
fn scaled_size(width: u32, height: u32, max_width: u32, max_height: u32) -> (u32, u32) {
    if width <= max_width && height <= max_height {
        return (width.max(2) & !1, height.max(2) & !1);
    }

    let scale = (max_width as f64 / width as f64).min(max_height as f64 / height as f64);
    let scaled_width = ((width as f64 * scale).round() as u32).max(2);
    let scaled_height = ((height as f64 * scale).round() as u32).max(2);
    (scaled_width & !1, scaled_height & !1)
}

#[cfg(all(test, target_os = "macos"))]
mod tests {
    use super::*;

    fn gradient(width: u32, height: u32, format: Pixel) -> frame::Video {
        let mut input = frame::Video::new(format, width, height);
        let stride = input.stride(0);
        for (offset, byte) in input.data_mut(0).iter_mut().enumerate() {
            *byte = ((offset % stride) % 251) as u8;
        }
        input
    }

    #[test]
    fn preview_frames_are_tight_rgba_at_bounded_dimensions() {
        crate::encoder::Mp4Encoder::init_global();
        let mut converter = None;
        let started = Instant::now();
        for _ in 0..10 {
            let preview =
                processed_preview_frame(gradient(1280, 720, Pixel::RGB24), &mut converter)
                    .expect("preview frame");
            assert_eq!((preview.width, preview.height), (960, 540));
            assert_eq!(preview.rgba.len(), 960 * 540 * 4);
            // Room for the socket trailer is already there, so publishing
            // the frame never reallocates its 2 MB buffer.
            assert!(
                preview.rgba.capacity()
                    >= preview.rgba.len() + crate::camera_preview_ws::TRAILER_LEN
            );
            // An RGB camera has no alpha: every pixel arrives opaque.
            assert!(preview.rgba.chunks_exact(4).all(|pixel| pixel[3] == 255));
        }
        eprintln!("10 camera preview frames: {:?}", started.elapsed());
    }

    #[test]
    fn preview_frames_pack_for_the_socket() {
        crate::encoder::Mp4Encoder::init_global();
        let mut converter = None;
        let preview = processed_preview_frame(gradient(640, 480, Pixel::NV12), &mut converter)
            .expect("preview frame");
        let pointer = preview.rgba.as_ptr();
        let packed = crate::camera_preview_ws::pack(preview, 3).expect("packed");
        assert_eq!(
            packed.len(),
            640 * 480 * 4 + crate::camera_preview_ws::TRAILER_LEN
        );
        assert_eq!(
            packed.as_ptr(),
            pointer,
            "the pixels must be sent without a copy"
        );
    }

    #[test]
    fn tight_rgba_drops_row_padding_and_unpremultiplies_soft_edges() {
        // Two pixels a row, padded to 12 bytes: the padding must not reach
        // the page, which reads the buffer as width * 4 bytes a row.
        let plane = [
            10, 20, 30, 255, 0, 0, 0, 0, 9, 9, 9, 9, // row 0: opaque, clear, padding
            50, 25, 0, 128, 255, 255, 255, 255, 9, 9, 9, 9, // row 1: half alpha, white
        ];
        let rgba = tight_rgba(&plane, 12, 2, 2).expect("tight");
        assert_eq!(
            rgba,
            vec![10, 20, 30, 255, 0, 0, 0, 0, 99, 49, 0, 128, 255, 255, 255, 255]
        );
        assert!(
            tight_rgba(&plane, 4, 2, 2).is_err(),
            "stride shorter than a row"
        );
        assert!(
            // The last row needs no padding, so 20 bytes is a full frame.
            tight_rgba(&plane[..19], 12, 2, 2).is_err(),
            "plane shorter than the frame"
        );
        assert!(tight_rgba(&plane, 12, 2, 0).is_err());
    }

    #[test]
    fn preview_never_upscales_a_small_camera() {
        let (max_width, max_height) = (PREVIEW_MAX_WIDTH, PREVIEW_MAX_HEIGHT);
        // A 640x480 camera is already inside the budget. Stretching it up to
        // fill the new box would cost bandwidth for invented detail.
        assert_eq!(scaled_size(640, 480, max_width, max_height), (640, 480));
        assert_eq!(scaled_size(1280, 720, max_width, max_height), (960, 540));
        assert_eq!(scaled_size(1920, 1080, max_width, max_height), (960, 540));
        // A portrait camera is bounded by the box's height and keeps its
        // aspect ratio, rather than being squeezed into 16:9.
        assert_eq!(scaled_size(1080, 1920, max_width, max_height), (304, 540));
        for (width, height) in [(1280u32, 720u32), (1920, 1080), (1080, 1920), (640, 480)] {
            let (scaled_width, scaled_height) = scaled_size(width, height, max_width, max_height);
            assert!(scaled_width <= max_width && scaled_height <= max_height);
            assert!(scaled_width % 2 == 0 && scaled_height % 2 == 0);
        }
    }

    #[test]
    fn preview_keeps_thirty_fps_with_callback_jitter() {
        let start = Instant::now();
        let mut last = None;
        let mut sent = 0;
        for index in 0..300_u64 {
            let jitter = if index % 2 == 0 { 0 } else { 500 };
            let now = start + Duration::from_micros(index * 33_333 + jitter);
            if preview_frame_due(last, now) {
                sent += 1;
                last = Some(now);
            }
        }
        assert_eq!(sent, 300);
    }

    #[test]
    fn preview_bounds_high_rate_sources_and_recovers_after_a_stall() {
        let start = Instant::now();
        let mut last = None;
        let mut sent = 0;
        for index in 0..600_u64 {
            let now = start + Duration::from_micros(index * 16_667);
            if preview_frame_due(last, now) {
                sent += 1;
                last = Some(now);
            }
        }
        assert_eq!(sent, 300);
        assert!(preview_frame_due(last, start + Duration::from_secs(30)));
    }

    /// A session whose worker just waits for the stop signal — lets the
    /// lifecycle protocol be exercised without touching camera hardware.
    fn dummy_session() -> NativeWebcamSession {
        let (stop_tx, stop_rx) = mpsc::channel();
        let worker = std::thread::spawn(move || {
            let _ = stop_rx.recv();
        });
        NativeWebcamSession {
            stop_tx,
            worker: Some(worker),
        }
    }

    #[test]
    fn stop_between_claim_and_register_rejects_registration() {
        let lifecycle = WebcamLifecycle::new();
        let (generation, old) = lifecycle.begin_start(0).unwrap();
        assert!(old.is_none());

        // Teardown lands while the camera is still spinning up.
        assert!(lifecycle.take_for_stop().1.is_none());

        let rejected = lifecycle.try_register(generation, dummy_session());
        assert!(rejected.is_err(), "stale start must not register");
        assert!(!lifecycle.is_active());
    }

    #[test]
    fn newer_start_supersedes_older_without_stopping_it() {
        let lifecycle = WebcamLifecycle::new();
        let (gen_a, _) = lifecycle.begin_start(0).unwrap();
        let (gen_b, _) = lifecycle.begin_start(0).unwrap();

        // B registers first; A's late registration is refused and A gets
        // its own session back — the registered (B) session is untouched.
        assert!(lifecycle.try_register(gen_b, dummy_session()).is_ok());
        assert!(lifecycle.try_register(gen_a, dummy_session()).is_err());
        assert!(
            lifecycle.is_active(),
            "superseding start must stay registered"
        );
    }

    #[test]
    fn stop_after_registration_takes_the_session() {
        let lifecycle = WebcamLifecycle::new();
        let (generation, _) = lifecycle.begin_start(0).unwrap();
        assert!(lifecycle.try_register(generation, dummy_session()).is_ok());

        let (_, taken) = lifecycle.take_for_stop();
        assert!(taken.is_some(), "stop must take the registered session");
        assert!(!lifecycle.is_active());
    }

    #[test]
    fn start_after_completed_stop_registers() {
        let lifecycle = WebcamLifecycle::new();
        let (gen_a, _) = lifecycle.begin_start(0).unwrap();
        assert!(lifecycle.try_register(gen_a, dummy_session()).is_ok());
        drop(lifecycle.take_for_stop().1);

        let request = lifecycle.begin_request();
        let (gen_b, old) = lifecycle.begin_start(request).unwrap();
        assert!(old.is_none());
        assert!(lifecycle.try_register(gen_b, dummy_session()).is_ok());
        assert!(lifecycle.is_active());
    }

    #[test]
    fn stop_rejects_a_request_queued_before_native_start() {
        let lifecycle = WebcamLifecycle::new();
        let queued_request = lifecycle.begin_request();
        let (stop_request, session) = lifecycle.take_for_stop();
        assert!(session.is_none());
        assert!(lifecycle.begin_start(queued_request).is_none());
        assert!(!lifecycle.with_current_request(queued_request, || panic!("stale show")));
        assert!(lifecycle.with_current_request(stop_request, || {}));
    }

    #[test]
    fn newer_preview_rejects_delayed_stop_cleanup() {
        let lifecycle = WebcamLifecycle::new();
        let (stop_request, _) = lifecycle.take_for_stop();
        let request = lifecycle.begin_request();
        assert!(!lifecycle.cleanup_is_current(stop_request));
        assert!(lifecycle.begin_start(request).is_some());
        assert!(lifecycle.with_current_request(request, || {}));
    }

    #[test]
    fn queued_hud_request_cannot_begin_after_stop_or_invalidate_cleanup() {
        let lifecycle = WebcamLifecycle::new();
        let admission = lifecycle.request_token();
        let (stop_token, _) = lifecycle.take_for_stop();

        assert_eq!(lifecycle.try_begin_request(admission), None);
        assert_eq!(lifecycle.request_token(), stop_token);
        assert!(lifecycle.cleanup_is_current(stop_token));
    }

    #[test]
    fn old_hide_cannot_stop_a_newer_preview() {
        let lifecycle = WebcamLifecycle::new();
        let old_hide = lifecycle.request_token();
        let preview = lifecycle.begin_request();
        let (generation, _) = lifecycle.begin_start(preview).unwrap();
        assert!(lifecycle.try_register(generation, dummy_session()).is_ok());

        assert!(lifecycle.take_for_stop_if_current(old_hide).is_none());
        assert!(lifecycle.is_active());
        assert_eq!(lifecycle.request_token(), preview);
        assert!(lifecycle.with_current_preview(generation, || {}));
    }

    #[test]
    fn current_hide_stops_preview_and_invalidates_pending_start() {
        let lifecycle = WebcamLifecycle::new();
        let preview = lifecycle.begin_request();
        let (generation, _) = lifecycle.begin_start(preview).unwrap();
        assert!(lifecycle.try_register(generation, dummy_session()).is_ok());

        let (stop_token, session) = lifecycle.take_for_stop_if_current(preview).unwrap();
        assert!(session.is_some());
        assert!(!lifecycle.is_active());
        assert!(lifecycle.cleanup_is_current(stop_token));
        assert!(lifecycle.begin_start(preview).is_none());
        assert!(!lifecycle.with_current_preview(generation, || panic!("late preview")));
    }

    #[test]
    fn conditional_hud_request_can_clean_up_during_camera_recording() {
        let lifecycle = WebcamLifecycle::new();
        lifecycle.reserve_recording().unwrap();
        let admission = lifecycle.request_token();
        let hud = lifecycle.try_begin_request(admission).unwrap();

        assert_ne!(hud, admission);
        assert!(lifecycle.cleanup_is_current(hud));
        assert!(lifecycle.begin_start(hud).is_none());
        let (stop_token, session) = lifecycle.take_for_stop_if_current(hud).unwrap();
        assert!(session.is_none());
        assert!(lifecycle.cleanup_is_current(stop_token));
    }

    #[test]
    fn camera_recording_reservation_does_not_block_preview_dismissal() {
        let lifecycle = WebcamLifecycle::new();
        lifecycle.reserve_recording().unwrap();
        let (stop_request, _) = lifecycle.take_for_stop();
        assert!(lifecycle.cleanup_is_current(stop_request));
        assert!(!lifecycle.with_current_request(stop_request, || panic!("preview restarted")));
    }

    #[test]
    fn begin_start_takes_over_running_session() {
        let lifecycle = WebcamLifecycle::new();
        let (gen_a, _) = lifecycle.begin_start(0).unwrap();
        assert!(lifecycle.try_register(gen_a, dummy_session()).is_ok());

        let (gen_b, old) = lifecycle.begin_start(0).unwrap();
        assert!(old.is_some(), "restart must take the previous session");
        drop(old);
        assert!(lifecycle.try_register(gen_b, dummy_session()).is_ok());
    }

    #[test]
    fn camera_reservation_rejects_inflight_preview_and_stale_requests_after_release() {
        let lifecycle = WebcamLifecycle::new();
        let (generation, _) = lifecycle.begin_start(0).unwrap();
        assert!(lifecycle.reserve_recording().unwrap().is_none());
        assert!(lifecycle.begin_start(0).is_none());
        assert!(lifecycle.begin_start(1).is_none());
        assert!(lifecycle.try_register(generation, dummy_session()).is_err());
        assert!(!lifecycle.with_current_preview(generation, || panic!("stale preview emitted")));
        assert!(!lifecycle.with_current_request(0, || panic!("stale request ran cleanup")));
        assert!(lifecycle.reserve_recording().is_err());

        lifecycle.release_recording();
        assert!(lifecycle.begin_start(0).is_none());
        assert!(
            !lifecycle.with_current_request(0, || panic!("old request revived after recording"))
        );
        assert!(lifecycle.begin_start(1).is_some());
    }

    #[test]
    fn camera_device_admission_waits_for_preview_shutdown_before_recording() {
        let lifecycle = Arc::new(WebcamLifecycle::new());
        let (generation, _) = lifecycle.begin_start(0).unwrap();
        let (started_tx, started_rx) = mpsc::channel();
        let (stop_tx, stop_rx) = mpsc::channel();
        let preview_lifecycle = lifecycle.clone();
        let preview = std::thread::spawn(move || {
            let _device = lock_camera_device();
            assert!(preview_lifecycle.with_current_preview(generation, || {}));
            started_tx.send(()).unwrap();
            stop_rx.recv().unwrap();
            assert!(
                !preview_lifecycle.with_current_preview(generation, || panic!("late camera frame"))
            );
        });
        started_rx.recv_timeout(Duration::from_secs(2)).unwrap();
        lifecycle.reserve_recording().unwrap();
        let (recording_tx, recording_rx) = mpsc::channel();
        let recording = std::thread::spawn(move || {
            let _device = lock_camera_device();
            recording_tx.send(()).unwrap();
        });
        assert!(recording_rx
            .recv_timeout(Duration::from_millis(50))
            .is_err());
        assert!(lifecycle.begin_start(1).is_none());
        stop_tx.send(()).unwrap();
        preview.join().unwrap();
        recording_rx.recv_timeout(Duration::from_secs(2)).unwrap();
        recording.join().unwrap();
        lifecycle.release_recording();
        assert!(lifecycle.begin_start(1).is_some());
    }
}
