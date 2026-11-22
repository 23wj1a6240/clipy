use std::{
    sync::{
        atomic::{AtomicBool, Ordering},
        mpsc, Arc,
    },
    time::{Duration, Instant},
};

use anyhow::{anyhow, Context, Result};
use cap_camera_ffmpeg::CapturedFrameExt;
use ffmpeg::{format::Pixel, frame, software::scaling};
use tauri::{AppHandle, Emitter, Manager, Runtime};
use tokio::{sync::oneshot, task::JoinHandle};

use super::macos::{ScreenCaptureHandles, VideoSample};
use crate::native_webcam::{
    clear_preview_frames, processed_preview_frame, publish_preview_frame,
    EVENT_NATIVE_WEBCAM_START, EVENT_NATIVE_WEBCAM_STOP,
};

static ACTIVE_DEVICE: parking_lot::Mutex<Option<String>> = parking_lot::Mutex::new(None);

pub fn is_active() -> bool {
    ACTIVE_DEVICE.lock().is_some()
}

pub fn active_device_id() -> Option<String> {
    ACTIVE_DEVICE.lock().clone()
}

pub struct CameraCapturer {
    stop_tx: mpsc::Sender<()>,
    cancelled: Arc<AtomicBool>,
    worker: Option<JoinHandle<Result<()>>>,
}

impl Drop for CameraCapturer {
    fn drop(&mut self) {
        self.cancelled.store(true, Ordering::Release);
        let _ = self.stop_tx.send(());
    }
}

struct CameraWorkerGuard<R: Runtime>(AppHandle<R>);

impl<R: Runtime> Drop for CameraWorkerGuard<R> {
    fn drop(&mut self) {
        if ACTIVE_DEVICE.lock().take().is_some() {
            clear_preview_frames();
            let hide_app = self.0.clone();
            let (hidden_tx, hidden_rx) = mpsc::sync_channel(1);
            let queued = self.0.run_on_main_thread(move || {
                let _ = hide_app.emit_to("webcam", EVENT_NATIVE_WEBCAM_STOP, ());
                for label in ["webcam", "webcam-controls"] {
                    if let Some(window) = hide_app.get_webview_window(label) {
                        let _ = window.hide();
                    }
                }
                let _ = hidden_tx.send(());
            });
            if queued.is_ok() {
                let _ = hidden_rx.recv();
            }
        }
    }
}

impl CameraCapturer {
    pub async fn start<R: Runtime>(
        app: &AppHandle<R>,
        device_id: String,
        max_height: Option<u32>,
    ) -> Result<(Self, ScreenCaptureHandles)> {
        let reservation =
            crate::native_webcam::reserve_camera_recording().map_err(|err| anyhow!(err))?;
        let (stop_tx, stop_rx) = mpsc::channel();
        let (ready_tx, ready_rx) = oneshot::channel();
        let (video_tx, video_rx) = tokio::sync::mpsc::channel(8);
        let camera_app = app.clone();
        let active_device_id = device_id.clone();
        let cancelled = Arc::new(AtomicBool::new(false));
        let worker_cancelled = cancelled.clone();
        let worker = tokio::task::spawn_blocking(move || {
            let mut reservation = reservation;
            let _guard = CameraWorkerGuard(camera_app.clone());
            reservation.stop_previous_preview();
            // The reservation already stopped the preview from publishing;
            // drop its last frame so the bubble opens on this camera.
            clear_preview_frames();
            let _device = crate::native_webcam::lock_camera_device();
            if let Err(err) = crate::commands::ensure_camera_permission() {
                let _ = ready_tx.send(Err(err));
                return Ok(());
            }
            if worker_cancelled.load(Ordering::Acquire) {
                return Ok(());
            }
            crate::encoder::Mp4Encoder::init_global();
            let camera = cap_camera::list_cameras().find(|camera| camera.device_id() == device_id);
            let Some(camera) = camera else {
                let _ = ready_tx.send(Err(
                    "Selected camera is unavailable. Reconnect it or choose another camera."
                        .to_string(),
                ));
                return Ok(());
            };
            let format = match select_recording_format(&camera, max_height) {
                Ok(format) => format,
                Err(err) => {
                    let _ = ready_tx.send(Err(err));
                    return Ok(());
                }
            };
            let (first_frame_tx, first_frame_rx) = mpsc::sync_channel(1);
            let mut callback_ready = Some(first_frame_tx);
            let frame_app = camera_app.clone();
            let mut converter: Option<scaling::Context> = None;
            let mut preview_encoder = None;
            let mut effects = crate::camera_effects::Processor::default();
            let mut failed_effects = None;
            let mut last_frame = None;
            let handle = camera.start_capturing(format, move |captured| {
                if worker_cancelled.load(Ordering::Acquire) {
                    return;
                }
                let now = Instant::now();
                if last_frame.is_some_and(|last: Instant| {
                    now.duration_since(last) < Duration::from_millis(32)
                }) {
                    return;
                }
                last_frame = Some(now);
                let result = captured
                    .as_ffmpeg()
                    .map_err(|err| anyhow!("camera frame conversion: {err}"))
                    .and_then(|frame| prepare_frame(frame, max_height, &mut converter))
                    .and_then(|frame| effects.process(frame, true).map_err(anyhow::Error::msg));
                let frame = match result {
                    Ok(frame) => frame,
                    Err(err) => {
                        let settings = crate::camera_effects::current();
                        if failed_effects.as_ref() != Some(&settings) {
                            let _ = frame_app.emit("camera-effects-error", err.to_string());
                            failed_effects = Some(settings);
                            tracing::warn!("camera recording frame failed: {err:#}");
                        }
                        if let Some(tx) = callback_ready.take() {
                            let _ = tx.send(Err(err.to_string()));
                        }
                        return;
                    }
                };
                failed_effects = None;
                if let Some(tx) = callback_ready.take() {
                    let _ = tx.send(Ok((frame.width(), frame.height())));
                }
                if let Ok(preview) = processed_preview_frame(frame.clone(), &mut preview_encoder) {
                    publish_preview_frame(preview);
                }
                let mach_pts = cidre::cm::Clock::convert_host_time_to_sys_units(
                    captured.native().sample_buf().pts(),
                );
                let _ = video_tx.try_send(VideoSample::Camera { frame, mach_pts });
            });
            let handle = match handle {
                Ok(handle) => handle,
                Err(err) => {
                    let _ = ready_tx.send(Err(format!("Camera failed to start: {err}")));
                    return Ok(());
                }
            };
            let first_frame = first_frame_rx
                .recv_timeout(Duration::from_secs(12))
                .unwrap_or_else(|_| Err("Camera did not deliver a frame.".to_string()));
            let failed = first_frame.is_err();
            let _ = ready_tx.send(first_frame);
            if failed {
                return handle.stop_capturing().map_err(|err| anyhow!(err));
            }
            let _ = stop_rx.recv();
            handle
                .stop_capturing()
                .map_err(|err| anyhow!("Camera failed to stop: {err}"))
        });
        let session = Self {
            stop_tx,
            cancelled,
            worker: Some(worker),
        };
        let ready = tokio::time::timeout(Duration::from_secs(12), ready_rx).await;
        let dimensions = match ready {
            Ok(Ok(Ok(dimensions))) => dimensions,
            other => {
                let error = match other {
                    Ok(Ok(Err(err))) => err,
                    Ok(Err(_)) => "Camera stopped before delivering a frame.".to_string(),
                    Err(_) => {
                        "Camera did not deliver a frame. Check camera permission and try again."
                            .to_string()
                    }
                    Ok(Ok(Ok(_))) => unreachable!(),
                };
                let _ = session.stop().await;
                return Err(anyhow!(error));
            }
        };
        *ACTIVE_DEVICE.lock() = Some(active_device_id);
        let show_app = app.clone();
        app.run_on_main_thread(move || {
            if let Some(window) = show_app.get_webview_window("webcam") {
                let _ = window.show();
                let _ = crate::cursor_track::set_ignore_cursor_events(&window, false);
            }
            let _ = show_app.emit_to("webcam", EVENT_NATIVE_WEBCAM_START, ());
        })?;
        Ok((
            session,
            ScreenCaptureHandles {
                video_rx,
                system_audio_rx: None,
                width: dimensions.0,
                height: dimensions.1,
                capture: None,
                window_follow: None,
                stream_stats: None,
            },
        ))
    }

    pub async fn stop(mut self) -> Result<()> {
        self.cancelled.store(true, Ordering::Release);
        let _ = self.stop_tx.send(());
        if let Some(worker) = self.worker.take() {
            tokio::time::timeout(Duration::from_secs(3), worker)
                .await
                .context("Camera is taking too long to close; waiting for macOS to release it")?
                .context("join camera capture worker")??;
        }
        Ok(())
    }
}

fn recording_format_score(
    width: u32,
    height: u32,
    fps: f32,
    max_height: Option<u32>,
) -> (bool, u64, std::cmp::Reverse<u32>, std::cmp::Reverse<u64>) {
    let (output_width, output_height) =
        super::output_size::fit_capture_size(width, height, max_height);
    (
        fps >= 24.0,
        u64::from(output_width) * u64::from(output_height),
        std::cmp::Reverse((fps.round() as u32).abs_diff(30)),
        std::cmp::Reverse(u64::from(width) * u64::from(height)),
    )
}

fn select_recording_format(
    camera: &cap_camera::CameraInfo,
    max_height: Option<u32>,
) -> Result<cap_camera::Format, String> {
    camera
        .formats()
        .unwrap_or_default()
        .into_iter()
        .max_by_key(|format| {
            recording_format_score(
                format.width(),
                format.height(),
                format.frame_rate(),
                max_height,
            )
        })
        .ok_or_else(|| "Selected camera has no supported formats.".to_string())
}

fn prepare_frame(
    frame: frame::Video,
    max_height: Option<u32>,
    converter: &mut Option<scaling::Context>,
) -> Result<frame::Video> {
    let (width, height) =
        super::output_size::fit_capture_size(frame.width(), frame.height(), max_height);
    let reset = converter.as_ref().is_none_or(|scaler| {
        scaler.input().format != frame.format()
            || scaler.input().width != frame.width()
            || scaler.input().height != frame.height()
    });
    if reset {
        *converter = Some(scaling::Context::get(
            frame.format(),
            frame.width(),
            frame.height(),
            Pixel::BGRA,
            width,
            height,
            scaling::flag::Flags::BILINEAR,
        )?);
    }
    let mut output = frame::Video::new(Pixel::BGRA, width, height);
    converter
        .as_mut()
        .ok_or_else(|| anyhow!("Camera converter missing"))?
        .run(&frame, &mut output)?;
    Ok(output)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn camera_format_selection_respects_quality_and_avoids_unneeded_downscale() {
        assert!(
            recording_format_score(3840, 2160, 30.0, Some(2160))
                > recording_format_score(1920, 1080, 30.0, Some(2160))
        );
        assert!(
            recording_format_score(3840, 2160, 30.0, None)
                > recording_format_score(1920, 1080, 30.0, None)
        );
        assert!(
            recording_format_score(1280, 720, 30.0, Some(720))
                > recording_format_score(3840, 2160, 30.0, Some(720))
        );
        assert!(
            recording_format_score(1920, 1080, 30.0, None)
                > recording_format_score(3840, 2160, 15.0, None)
        );
    }

    #[tokio::test]
    async fn camera_stop_cancels_callbacks_and_does_not_wait_forever() {
        let (stop_tx, stop_rx) = mpsc::channel();
        let cancelled = Arc::new(AtomicBool::new(false));
        let worker = tokio::spawn(std::future::pending::<Result<()>>());
        let session = CameraCapturer {
            stop_tx,
            cancelled: cancelled.clone(),
            worker: Some(worker),
        };
        let started = Instant::now();
        assert!(session.stop().await.is_err());
        assert!(started.elapsed() < Duration::from_secs(6));
        assert!(cancelled.load(Ordering::Acquire));
        assert!(stop_rx.try_recv().is_ok());
    }

    #[test]
    fn camera_frames_respect_resolution_cap_and_preserve_unmirrored_pixels() {
        crate::encoder::Mp4Encoder::init_global();
        let source = frame::Video::new(Pixel::NV12, 1920, 1080);
        let scaled = prepare_frame(source, Some(720), &mut None).unwrap();
        assert_eq!(
            (scaled.width(), scaled.height(), scaled.format()),
            (1280, 720, Pixel::BGRA)
        );

        let mut frame = frame::Video::new(Pixel::BGRA, 4, 2);
        let stride = frame.stride(0);
        frame.data_mut(0)[..16]
            .copy_from_slice(&[1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16]);
        frame.data_mut(0)[stride..stride + 16].fill(42);
        let frame = prepare_frame(frame, None, &mut None).unwrap();
        let stride = frame.stride(0);
        assert_eq!(
            &frame.data(0)[..16],
            &[1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16]
        );
        assert_eq!(&frame.data(0)[stride..stride + 16], &[42; 16]);
    }

    #[test]
    fn camera_frames_encode_to_playable_mp4_without_screen_or_audio() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("camera.mp4");
        let mut encoder = crate::encoder::Mp4Encoder::open(
            &path,
            crate::encoder::Config {
                width: 320,
                height: 180,
                fps: 30,
                video_bitrate_bps: 1_500_000,
            },
        )
        .unwrap();
        let mut converter = None;
        for index in 0..30 {
            let mut source = frame::Video::new(Pixel::UYVY422, 640, 360);
            source.data_mut(0).fill(128);
            let mut output = prepare_frame(source, Some(180), &mut converter).unwrap();
            output.set_pts(Some(index * 33_333));
            encoder.queue_video(output).unwrap();
        }
        encoder.finish().unwrap();
        let probe = std::process::Command::new("ffprobe")
            .args([
                "-v",
                "error",
                "-select_streams",
                "v:0",
                "-count_frames",
                "-show_entries",
                "stream=codec_name,width,height,nb_read_frames",
                "-of",
                "json",
            ])
            .arg(&path)
            .output()
            .unwrap();
        assert!(
            probe.status.success(),
            "{}",
            String::from_utf8_lossy(&probe.stderr)
        );
        let json: serde_json::Value = serde_json::from_slice(&probe.stdout).unwrap();
        let stream = &json["streams"][0];
        assert_eq!(stream["codec_name"], "h264");
        assert_eq!(stream["width"], 320);
        assert_eq!(stream["height"], 180);
        assert_eq!(stream["nb_read_frames"], "30");
    }
}
