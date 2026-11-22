//! Shared types for the recording + upload pipeline.
//!
//! Phase 2/3 modules (`recording`, `capture`, `encode`, `upload`) all import
//! from here. Anything serialized to the WebView UI lives here too — keep
//! field names snake_case (matches Tauri's default) and 1:1 with the matching
//! TypeScript interfaces in `desktop/src/lib/bridge.ts`.

use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use uuid::Uuid;

/// Unique identifier for a single recording session. Matches the
/// `recordingId` field expected by the server's `/api/videos/raw-upload/*`
/// endpoints (any opaque string is accepted server-side).
pub type RecordingId = String;

pub fn new_recording_id() -> RecordingId {
    Uuid::new_v4().to_string()
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum RecordingPhase {
    Idle,
    Countdown,
    Recording,
    Paused,
    Stopping,
    Uploading,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RecordingStateDto {
    pub phase: RecordingPhase,
    /// Present once a recording is in flight, including during upload.
    pub recording_id: Option<RecordingId>,
    /// Wall-clock millis elapsed since capture started (None until Recording).
    pub elapsed_ms: Option<u64>,
    /// Final encoder file size once capture has stopped.
    pub bytes_written: Option<u64>,
    pub mic_level: Option<f32>,
    pub mic_muted: bool,
    /// Whether a microphone was actually opened for this take. False when the
    /// picker's mic was off, or on Windows when the device could not be
    /// opened (the take continues without it; on macOS that refuses the
    /// take instead). The dock then shows the mic as off instead of offering
    /// to mute a mic that is not being recorded.
    #[serde(default)]
    pub mic_active: bool,
    pub mic_silence_warning: Option<String>,
}

impl RecordingStateDto {
    pub fn idle() -> Self {
        Self {
            phase: RecordingPhase::Idle,
            recording_id: None,
            elapsed_ms: None,
            bytes_written: None,
            mic_level: None,
            mic_muted: false,
            mic_active: false,
            mic_silence_warning: None,
        }
    }
}

#[derive(Clone, Debug, Serialize)]
pub struct MicSignalCheckDto {
    pub device_name: String,
    pub level: f32,
    pub has_input: bool,
}

#[derive(Clone, Debug, Serialize)]
pub struct UploadProgressDto {
    pub recording_id: RecordingId,
    pub parts_uploaded: u64,
    pub bytes_uploaded: u64,
    pub status: UploadStatus,
    pub error: Option<String>,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum UploadStatus {
    Idle,
    Uploading,
    Completing,
    Completed,
    Failed,
    Aborted,
}

#[derive(Clone, Debug, Serialize)]
pub struct ShareLinkDto {
    pub video_id: String,
    pub public_id: String,
    pub url: String,
}

#[derive(Clone, Debug, Serialize)]
pub struct RecoverableRecordingDto {
    pub recording_id: RecordingId,
    pub bytes: u64,
    pub updated_at_ms: u64,
    pub expires_at_ms: u64,
}

/// Local recording context consumed by the native Mac Studio window.
/// It keeps the desktop editor fast by pointing the WebView at the
/// local MP4 and sidecars instead of waiting on the web playback asset.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct StudioRecordingDto {
    pub recording_id: RecordingId,
    /// Stable local editor id. Native Studio uses this for project saves
    /// even if upload later returns a server public id.
    pub public_id: String,
    pub remote_public_id: Option<String>,
    pub video_id: Option<String>,
    pub share_url: Option<String>,
    pub local_file_path: PathBuf,
    pub cursor_path: Option<PathBuf>,
    pub zoom_path: Option<PathBuf>,
    pub camera_path: Option<PathBuf>,
    pub project_path: Option<PathBuf>,
    #[serde(default)]
    pub screen_charm_path: Option<PathBuf>,
    pub cursor_json: Option<serde_json::Value>,
    pub zoom_json: Option<serde_json::Value>,
    pub project_json: Option<serde_json::Value>,
    #[serde(default)]
    pub screen_charm_json: Option<serde_json::Value>,
    pub webcam: Option<WebcamCaptureDto>,
    #[serde(default)]
    pub audio: Option<AudioOptionsDto>,
    pub bytes: u64,
    pub duration_ms: u64,
}

/// Returned by `stop_recording` once the encoder has flushed its final bytes
/// to disk. Upload runs concurrently — its completion is signalled via the
/// `share-link-ready` event, not this struct.
#[derive(Clone, Debug, Serialize)]
pub struct RecordingMetadata {
    pub recording_id: RecordingId,
    pub file_path: PathBuf,
    pub bytes: u64,
    pub duration_ms: u64,
}

// ---------- Capture source selection ----------

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct DisplayInfoDto {
    pub id: u32,
    pub name: String,
    pub x: f64,
    pub y: f64,
    pub width: u32,
    pub height: u32,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct WindowInfoDto {
    pub id: u32,
    pub app_name: String,
    pub title: String,
    pub x: f64,
    pub y: f64,
    pub width: u32,
    pub height: u32,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CaptureSourcesDto {
    pub displays: Vec<DisplayInfoDto>,
    pub windows: Vec<WindowInfoDto>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum CaptureSourceDto {
    Camera {
        device_id: String,
    },
    Display {
        id: u32,
    },
    Window {
        id: u32,
    },
    Area {
        display_id: u32,
        x: f64,
        y: f64,
        width: f64,
        height: f64,
    },
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
pub struct SourceHighlightRectDto {
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
pub struct CursorPositionDto {
    pub x: f64,
    pub y: f64,
}

/// Available audio input device, surfaced to the UI's pre-record picker.
/// `id` is the cpal device name — passed back to `start_recording` as
/// `mic_device_id` to select a non-default input.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct AudioDeviceDto {
    pub id: String,
    pub name: String,
    pub is_default: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct WebcamDeviceDto {
    pub id: String,
    pub name: String,
}

/// Pre-record audio settings. Plumbed through `start_recording` so the user
/// can record system-audio-only, mic-only, or both, plus pick a non-default
/// mic input. `None` everywhere means "default behaviour" (mic on, system
/// audio on if SCK 13+ supports it, default mic device).
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AudioOptionsDto {
    /// If false, skip system audio capture even on macOS 13+.
    #[serde(default = "default_true")]
    pub include_system_audio: bool,
    /// If false, skip mic capture entirely.
    #[serde(default = "default_true")]
    pub include_mic: bool,
    /// cpal device name; `None` selects the system default mic.
    pub mic_device_id: Option<String>,
    /// Run the microphone through `denoise::NoiseSuppressor`.
    ///
    /// Off by default, and `#[serde(default)]` rather than `default_true` so
    /// that settings files and agent-bridge payloads written before this
    /// existed keep the untouched microphone they were recorded with. A
    /// filter that changes what a microphone sounds like should be something
    /// the user turned on, not something an app update did to them.
    #[serde(default)]
    pub reduce_noise: bool,
}

impl Default for AudioOptionsDto {
    /// Default = "record both system audio and mic with the OS default
    /// device". Manual impl because `#[serde(default = "...")]` only
    /// fires during deserialisation, not when constructing via
    /// `Default::default()` in the recording controller's `unwrap_or_default`.
    fn default() -> Self {
        Self {
            include_system_audio: true,
            include_mic: true,
            mic_device_id: None,
            reduce_noise: false,
        }
    }
}

fn default_true() -> bool {
    true
}

/// Pre-record video settings. Currently exposes a single resolution cap so
/// users can downscale Retina captures (4K+ native) to 1080p / 720p for
/// smaller files and faster uploads. SCK scales internally; aspect ratio
/// is preserved by clamping the longer side to `max_height`.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct VideoOptionsDto {
    /// Cap on output height in pixels; `None` keeps the source's native
    /// dimensions. Common presets the UI offers: 720, 1080, 1440, 2160.
    pub max_height: Option<u32>,
    /// How a "Specific window" recording is captured. Settings written
    /// before this existed read as the default, `Window`.
    #[serde(default)]
    pub window_capture: WindowCaptureModeDto,
}

impl Default for VideoOptionsDto {
    /// "Native" = no cap. Matches the historical behaviour before this
    /// option existed.
    fn default() -> Self {
        Self {
            max_height: None,
            window_capture: WindowCaptureModeDto::default(),
        }
    }
}

/// How a "Specific window" source is recorded on macOS.
///
/// `Window` records only the selected window's own pixels through
/// ScreenCaptureKit's desktop-independent window filter: other windows that
/// cover it never appear, and the picture follows the window when it moves.
/// Clipy's camera bubble and Flare are separate windows, so they cannot be in
/// it. `Area` is the fixed display crop: the
/// screen rectangle under the window at Start, which includes the camera and
/// Flare and anything else that enters it. Windows ignores this and always
/// records the area.
#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum WindowCaptureModeDto {
    #[default]
    Window,
    Area,
}

/// True when this recording captures one window's own pixels, so overlay
/// windows (camera bubble, Flare) cannot be part of it. macOS only.
pub fn is_window_only_capture(source: Option<&CaptureSourceDto>, video: &VideoOptionsDto) -> bool {
    cfg!(target_os = "macos")
        && matches!(source, Some(CaptureSourceDto::Window { .. }))
        && video.window_capture == WindowCaptureModeDto::Window
}

/// Why the area-select overlay is open: the same drag-to-select surface serves
/// recording and screenshots, and its wording differs.
#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum AreaSelectPurposeDto {
    #[default]
    Video,
    Screenshot,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "lowercase")]
pub enum WebcamLayoutDto {
    Bubble,
    Left,
    Right,
    Top,
    Bottom,
}

#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum WebcamShapeDto {
    #[default]
    Square,
    Pill,
    Circle,
}

#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum WebcamSizeDto {
    Small,
    /// The default. Square at Medium is the shape and size the bubble looks
    /// best at, so it is what a camera switched on for the first time gets.
    #[default]
    Medium,
    Large,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WebcamCaptureDto {
    pub enabled: bool,
    pub layout: WebcamLayoutDto,
    #[serde(default)]
    pub shape: WebcamShapeDto,
    #[serde(default)]
    pub size: WebcamSizeDto,
    #[serde(default)]
    pub device_id: Option<String>,
}

impl Default for WebcamCaptureDto {
    fn default() -> Self {
        Self {
            enabled: false,
            layout: WebcamLayoutDto::Bubble,
            shape: WebcamShapeDto::Square,
            size: WebcamSizeDto::Medium,
            device_id: None,
        }
    }
}

/// Recording mode — Cap-style choice between the existing streaming
/// share-link flow and a Studio flow that opens native Mac editing.
///
/// `Instant`: stream-upload during recording, surface a share link the
/// instant the user stops. Existing default behaviour.
///
/// `Studio`: keep the local MP4 + cursor + zoom sidecars for native
/// editing, while authenticated runs also upload for later sharing.
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum RecordingMode {
    Instant,
    Studio,
    Local,
}

impl Default for RecordingMode {
    fn default() -> Self {
        // Instant matches existing behaviour and is the lowest-friction
        // path. New users discover Studio via the mode selector.
        RecordingMode::Instant
    }
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum RecordingSourceTab {
    Display,
    Window,
    Area,
}

impl Default for RecordingSourceTab {
    fn default() -> Self {
        RecordingSourceTab::Display
    }
}

/// Who can watch the recording, chosen BEFORE recording rather than after
/// the share link already exists. Mirrors the server's `videos.access_mode`
/// literals exactly.
#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum ShareScopeDto {
    #[default]
    Public,
    Restricted,
    Private,
}

impl ShareScopeDto {
    pub fn as_api_str(self) -> &'static str {
        match self {
            ShareScopeDto::Public => "public",
            ShareScopeDto::Restricted => "restricted",
            ShareScopeDto::Private => "private",
        }
    }
}

pub const MAX_RECORDING_TITLE_LEN: usize = 200;
pub const MAX_RECORDING_DESCRIPTION_LEN: usize = 4000;

/// Pre-record notes. Both become the recording's name and description at
/// `/complete`, so the recording arrives named instead of being titled by
/// the summarizer and renamed by hand afterwards.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct RecordingNotesDto {
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub description: Option<String>,
}

fn trimmed_bounded(value: Option<String>, max: usize) -> Option<String> {
    let trimmed = value?.trim().to_string();
    if trimmed.is_empty() {
        return None;
    }
    // Truncate by chars, never bytes: a byte slice can split a multi-byte
    // character and panic.
    Some(trimmed.chars().take(max).collect())
}

impl RecordingNotesDto {
    pub fn normalized(self) -> Self {
        Self {
            title: trimmed_bounded(self.title, MAX_RECORDING_TITLE_LEN),
            description: trimmed_bounded(self.description, MAX_RECORDING_DESCRIPTION_LEN),
        }
    }

    pub fn is_empty(&self) -> bool {
        self.title.is_none() && self.description.is_none()
    }
}

/// The server caps grants per video at 10 and re-normalizes domains itself;
/// this keeps the persisted list tidy and bounded so a settings file can
/// never grow unboundedly from repeated saves.
pub const MAX_SHARE_DOMAINS: usize = 10;

pub fn normalize_share_domains(domains: Vec<String>) -> Vec<String> {
    let mut seen: Vec<String> = Vec::new();
    for domain in domains {
        let cleaned = domain
            .trim()
            .trim_start_matches('@')
            .trim_start_matches("https://")
            .trim_start_matches("http://")
            .split('/')
            .next()
            .unwrap_or("")
            .to_ascii_lowercase();
        if !cleaned.contains('.') || seen.contains(&cleaned) {
            continue;
        }
        seen.push(cleaned);
        if seen.len() >= MAX_SHARE_DOMAINS {
            break;
        }
    }
    seen
}

pub const DEFAULT_COUNTDOWN_SECONDS: u32 = 3;
pub const MAX_COUNTDOWN_SECONDS: u32 = 10;

pub fn normalize_countdown_seconds(seconds: u32) -> u32 {
    seconds.min(MAX_COUNTDOWN_SECONDS)
}

/// Persisted pre-recording choices. Stored in the existing `settings.json`
/// Tauri store so the picker can reopen with the user's last safe choices.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RecordingSettingsDto {
    #[serde(default)]
    pub source_tab: RecordingSourceTab,
    pub source: Option<CaptureSourceDto>,
    /// Window ids are unstable across launches; app/title let the UI find
    /// the equivalent window when the raw id is stale.
    pub window_app_name: Option<String>,
    pub window_title: Option<String>,
    /// The connected displays when `source` was saved, as the picker keys
    /// them. A saved display or area is restored only while this matches,
    /// so a changed monitor setup starts again from the first display.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub display_layout: Option<String>,
    #[serde(default)]
    pub audio: AudioOptionsDto,
    #[serde(default = "default_recording_video_options")]
    pub video: VideoOptionsDto,
    #[serde(default)]
    pub webcam: WebcamCaptureDto,
    #[serde(default)]
    pub recording_mode: RecordingMode,
    #[serde(default = "default_countdown_seconds")]
    pub countdown_seconds: u32,
    /// Who the finished recording is shared with. Persisted because it is a
    /// standing preference ("everything I record is workspace-only"), not a
    /// per-recording afterthought.
    #[serde(default)]
    pub share_scope: ShareScopeDto,
    /// Email domains allowed when `share_scope` is `restricted`.
    #[serde(default)]
    pub share_domains: Vec<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CaptureSourceMetadataDto {
    #[serde(default)]
    pub source_tab: RecordingSourceTab,
    pub source: Option<CaptureSourceDto>,
    pub window_app_name: Option<String>,
    pub window_title: Option<String>,
    /// Optional reason for controlled/dev evidence. Normal user recordings
    /// leave this empty; dev E2E can mark synthetic harness captures so audit
    /// scripts do not confuse them with organic app samples.
    #[serde(default)]
    pub controlled_evidence: Option<String>,
    /// For a window source, whether the take recorded the window alone or
    /// the screen area under it. Set by the recorder, never by the picker.
    /// Absent on other sources and on sidecars written before 2026-10-06.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub window_capture: Option<WindowCaptureModeDto>,
}

impl CaptureSourceMetadataDto {
    pub fn normalized(mut self) -> Self {
        if self
            .window_app_name
            .as_ref()
            .is_some_and(|value| value.trim().is_empty())
        {
            self.window_app_name = None;
        }
        if self
            .window_title
            .as_ref()
            .is_some_and(|value| value.trim().is_empty())
        {
            self.window_title = None;
        }
        if self
            .controlled_evidence
            .as_ref()
            .is_some_and(|value| value.trim().is_empty())
        {
            self.controlled_evidence = None;
        }
        self
    }
}

impl From<&RecordingSettingsDto> for CaptureSourceMetadataDto {
    fn from(settings: &RecordingSettingsDto) -> Self {
        Self {
            source_tab: settings.source_tab,
            source: settings.source.clone(),
            window_app_name: settings.window_app_name.clone(),
            window_title: settings.window_title.clone(),
            controlled_evidence: None,
            window_capture: None,
        }
        .normalized()
    }
}

impl RecordingSettingsDto {
    pub fn normalized(mut self) -> Self {
        self.countdown_seconds = normalize_countdown_seconds(self.countdown_seconds);
        self.share_domains = normalize_share_domains(self.share_domains);
        // Studio is gated "coming soon": clamp any persisted
        // studio preference so no code path can start a Studio recording
        // while the mode is disabled in the UI.
        if self.recording_mode == RecordingMode::Studio {
            self.recording_mode = RecordingMode::Instant;
        }
        self
    }
}

impl Default for RecordingSettingsDto {
    fn default() -> Self {
        Self {
            source_tab: RecordingSourceTab::Display,
            source: None,
            window_app_name: None,
            window_title: None,
            display_layout: None,
            audio: AudioOptionsDto::default(),
            video: default_recording_video_options(),
            webcam: WebcamCaptureDto::default(),
            recording_mode: RecordingMode::default(),
            countdown_seconds: DEFAULT_COUNTDOWN_SECONDS,
            share_scope: ShareScopeDto::default(),
            share_domains: Vec::new(),
        }
    }
}

fn default_countdown_seconds() -> u32 {
    DEFAULT_COUNTDOWN_SECONDS
}

fn default_recording_video_options() -> VideoOptionsDto {
    VideoOptionsDto {
        // 720p default: uploads are stored
        // as-encoded now, so the default bounds storage; users can raise
        // it in Settings.
        max_height: Some(720),
        window_capture: WindowCaptureModeDto::default(),
    }
}

// ---------- Tauri event names (single source of truth) ----------

pub const EVENT_RECORDING_STATE_CHANGED: &str = "recording-state-changed";
pub const EVENT_UPLOAD_PROGRESS: &str = "upload-progress";
pub const EVENT_SHARE_LINK_READY: &str = "share-link-ready";
pub const EVENT_RECORDING_AUDIO_WARNING: &str = "recording-audio-warning";
pub const EVENT_STUDIO_RECORDING_UPDATED: &str = "studio-recording-updated";
pub const EVENT_COUNTDOWN_TICK: &str = "countdown-tick";
/// Mic supervisor swapped the input device mid-recording. Payload:
/// `{ device: String }`. The recording HUD shows it transiently — the
/// guaranteed-visible surface; the native notification is best-effort.
pub const EVENT_MIC_SWITCHED: &str = "mic-switched";
pub const EVENT_MIC_CHECK_LEVEL: &str = "mic-check-level";
/// Level from the popover's long-lived mic monitor (`mic_monitor`); `0` is
/// emitted once when it stops.
pub const EVENT_MIC_MONITOR_LEVEL: &str = "mic-monitor-level";
pub const EVENT_WEBCAM_START: &str = "webcam-start";
pub const EVENT_WEBCAM_STOP: &str = "webcam-stop";
/// Carries an `AreaSelectPurposeDto` to the `area-select` window on every show.
/// That window is created at startup and only shown and hidden afterwards, so
/// its webview mounts once and cannot read the mode itself.
pub const EVENT_AREA_SELECT_PURPOSE: &str = "area-select-purpose";

pub const EVENT_WEBCAM_LAYOUT_CHANGED: &str = "webcam-layout-changed";
pub const EVENT_REQUEST_SOURCE_PICKER: &str = "request-source-picker";
/// The popover is being put away by something other than the webview (tray
/// toggle, recording start). The frontend ends its picker session — persist
/// settings, drop the source highlight and the camera preview — instead of
/// leaving them running behind a hidden window.
pub const EVENT_PANEL_DISMISSED: &str = "panel-dismissed";
/// Rust brought the popover back on its own after a recording, to show the
/// stopping/uploading tail. The frontend must not treat this as the user
/// opening a picker session: the source highlight stays down until they
/// actually interact with the picker again.
pub const EVENT_PANEL_RESTORED: &str = "panel-restored";
/// A local recording's background processing moved to a new stage
/// (transcribing → selecting → extracting → ready | failed). Payload is
/// `local::Status`, the same shape `status.json` holds on disk.
pub const EVENT_LOCAL_RECORDING_PROGRESS: &str = "local-recording-progress";

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn camera_source_requires_explicit_device_id_and_round_trips() {
        let json = r#"{"type":"camera","device_id":"camera-1"}"#;
        let source: CaptureSourceDto = serde_json::from_str(json).unwrap();
        assert!(
            matches!(&source, CaptureSourceDto::Camera { device_id } if device_id == "camera-1")
        );
        assert_eq!(serde_json::to_string(&source).unwrap(), json);
        assert!(serde_json::from_str::<CaptureSourceDto>(r#"{"type":"camera"}"#).is_err());
    }

    #[test]
    fn capture_source_display_serializes_to_ts_shape() {
        let src = CaptureSourceDto::Display { id: 42 };
        let json = serde_json::to_string(&src).unwrap();
        assert_eq!(json, r#"{"type":"display","id":42}"#);
    }

    #[test]
    fn capture_source_window_serializes_to_ts_shape() {
        let src = CaptureSourceDto::Window { id: 7 };
        let json = serde_json::to_string(&src).unwrap();
        assert_eq!(json, r#"{"type":"window","id":7}"#);
    }

    #[test]
    fn capture_source_round_trips() {
        for src in [
            CaptureSourceDto::Display { id: 1 },
            CaptureSourceDto::Window { id: 999 },
            CaptureSourceDto::Area {
                display_id: 1,
                x: 120.0,
                y: 80.0,
                width: 1280.0,
                height: 720.0,
            },
        ] {
            let json = serde_json::to_string(&src).unwrap();
            let back: CaptureSourceDto = serde_json::from_str(&json).unwrap();
            match (&src, &back) {
                (CaptureSourceDto::Display { id: a }, CaptureSourceDto::Display { id: b }) => {
                    assert_eq!(a, b)
                }
                (CaptureSourceDto::Window { id: a }, CaptureSourceDto::Window { id: b }) => {
                    assert_eq!(a, b)
                }
                (
                    CaptureSourceDto::Area {
                        display_id: ad,
                        x: ax,
                        y: ay,
                        width: aw,
                        height: ah,
                    },
                    CaptureSourceDto::Area {
                        display_id: bd,
                        x: bx,
                        y: by,
                        width: bw,
                        height: bh,
                    },
                ) => {
                    assert_eq!(ad, bd);
                    assert_eq!(ax, bx);
                    assert_eq!(ay, by);
                    assert_eq!(aw, bw);
                    assert_eq!(ah, bh);
                }
                _ => panic!("variant changed across round-trip"),
            }
        }
    }

    #[test]
    fn capture_sources_dto_shape() {
        let dto = CaptureSourcesDto {
            displays: vec![DisplayInfoDto {
                id: 1,
                name: "Display 1".into(),
                x: 0.0,
                y: 0.0,
                width: 2560,
                height: 1600,
            }],
            windows: vec![WindowInfoDto {
                id: 2,
                app_name: "Chrome".into(),
                title: "Gmail".into(),
                x: 10.0,
                y: 20.0,
                width: 800,
                height: 600,
            }],
        };
        let json = serde_json::to_string(&dto).unwrap();
        // Must use snake_case — the TS bridge reads `app_name`, not `appName`.
        assert!(json.contains(r#""app_name":"Chrome""#), "got {json}");
        assert!(json.contains(r#""width":2560"#), "got {json}");
    }

    #[test]
    fn recording_phase_serializes_to_bare_string() {
        // Must be a bare string, NOT `{"phase":"recording"}` — the TS
        // bridge types `RecordingStateDto.phase` as a string literal
        // union, and `phase === "recording"` checks in React rely on it.
        assert_eq!(
            serde_json::to_string(&RecordingPhase::Recording).unwrap(),
            r#""recording""#
        );
        assert_eq!(
            serde_json::to_string(&RecordingPhase::Idle).unwrap(),
            r#""idle""#
        );
    }

    /// Regression test for the root cause of the "empty main window"
    /// symptom: if `RecordingPhase` had an internal tag, the phase field
    /// would serialize as an object and every `phase === "idle"` check in
    /// React would fail, hiding the source picker and record button.
    #[test]
    fn recording_state_dto_has_bare_string_phase() {
        let json = serde_json::to_string(&RecordingStateDto::idle()).unwrap();
        assert!(
            json.contains(r#""phase":"idle""#),
            "phase must serialize as a bare string; got: {json}"
        );
        assert!(
            !json.contains(r#""phase":{"#),
            "phase must NOT serialize as a nested object; got: {json}"
        );
    }

    #[test]
    fn idle_state_has_no_recording_id() {
        let s = RecordingStateDto::idle();
        assert_eq!(s.phase, RecordingPhase::Idle);
        assert!(s.recording_id.is_none());
        assert!(s.elapsed_ms.is_none());
        assert!(s.bytes_written.is_none());
        assert!(s.mic_level.is_none());
        assert!(!s.mic_muted);
        assert!(s.mic_silence_warning.is_none());
    }

    #[test]
    fn recording_settings_default_matches_picker_defaults() {
        let settings = RecordingSettingsDto::default();
        assert_eq!(settings.source_tab, RecordingSourceTab::Display);
        assert!(settings.source.is_none());
        assert!(settings.audio.include_system_audio);
        assert!(settings.audio.include_mic);
        assert_eq!(settings.video.max_height, Some(720));
        assert_eq!(settings.video.window_capture, WindowCaptureModeDto::Window);
        assert!(!settings.webcam.enabled);
        assert_eq!(settings.webcam.layout, WebcamLayoutDto::Bubble);
        assert!(settings.webcam.device_id.is_none());
        assert_eq!(settings.recording_mode, RecordingMode::Instant);
        assert_eq!(settings.countdown_seconds, DEFAULT_COUNTDOWN_SECONDS);
        assert_eq!(settings.share_scope, ShareScopeDto::Public);
        assert!(settings.share_domains.is_empty());
    }

    #[test]
    fn recording_settings_serializes_to_camel_case() {
        let settings = RecordingSettingsDto {
            source_tab: RecordingSourceTab::Area,
            source: Some(CaptureSourceDto::Area {
                display_id: 1,
                x: 10.0,
                y: 20.0,
                width: 800.0,
                height: 600.0,
            }),
            window_app_name: None,
            window_title: None,
            display_layout: Some("1@0,0,1440x900".to_string()),
            audio: AudioOptionsDto::default(),
            video: VideoOptionsDto {
                max_height: Some(720),
                window_capture: WindowCaptureModeDto::Area,
            },
            webcam: WebcamCaptureDto {
                enabled: true,
                layout: WebcamLayoutDto::Right,
                shape: WebcamShapeDto::Pill,
                size: WebcamSizeDto::Large,
                device_id: Some("camera-1".to_string()),
            },
            recording_mode: RecordingMode::Studio,
            countdown_seconds: 5,
            share_scope: ShareScopeDto::Restricted,
            share_domains: vec!["clipy.online".to_string()],
        };
        let json = serde_json::to_string(&settings).unwrap();
        assert!(json.contains(r#""sourceTab":"area""#), "got {json}");
        assert!(json.contains(r#""recordingMode":"studio""#), "got {json}");
        assert!(json.contains(r#""countdownSeconds":5"#), "got {json}");
        assert!(json.contains(r#""maxHeight":720"#), "got {json}");
        assert!(json.contains(r#""windowCapture":"area""#), "got {json}");
        assert!(json.contains(r#""deviceId":"camera-1""#), "got {json}");
        assert!(json.contains(r#""shape":"pill""#), "got {json}");
        assert!(json.contains(r#""size":"large""#), "got {json}");
        assert!(!json.contains("source_tab"), "got {json}");
        assert!(json.contains(r#""shareScope":"restricted""#), "got {json}");
        assert!(json.contains(r#""shareDomains":["clipy.online"]"#), "got {json}");
        assert!(json.contains(r#""displayLayout":"1@0,0,1440x900""#), "got {json}");
    }

    #[test]
    fn recording_settings_without_display_layout_still_load() {
        let settings: RecordingSettingsDto = serde_json::from_str(
            r#"{"sourceTab":"display","source":{"type":"display","id":2},"windowAppName":null,"windowTitle":null}"#,
        )
        .unwrap();
        assert!(settings.display_layout.is_none());
        let json = serde_json::to_string(&settings).unwrap();
        assert!(!json.contains("displayLayout"), "got {json}");
    }

    #[test]
    fn share_scope_api_strings_match_server_literals() {
        // These three strings are the server's `videos.access_mode` values.
        // Renaming one here silently downgrades
        // every Mac recording to the server's default, so pin them.
        assert_eq!(ShareScopeDto::Public.as_api_str(), "public");
        assert_eq!(ShareScopeDto::Restricted.as_api_str(), "restricted");
        assert_eq!(ShareScopeDto::Private.as_api_str(), "private");
    }

    #[test]
    fn share_domains_are_deduped_lowercased_and_bounded() {
        let domains = normalize_share_domains(vec![
            "  @Clipy.Online ".to_string(),
            "clipy.online".to_string(),
            "https://Example.com/team".to_string(),
            "not-a-domain".to_string(),
            String::new(),
        ]);
        assert_eq!(domains, vec!["clipy.online", "example.com"]);

        let many: Vec<String> = (0..40).map(|i| format!("d{i}.example")).collect();
        assert_eq!(normalize_share_domains(many).len(), MAX_SHARE_DOMAINS);
    }

    #[test]
    fn recording_notes_normalize_trims_empties_and_bounds_length() {
        let notes = RecordingNotesDto {
            title: Some("   ".to_string()),
            description: Some("x".repeat(MAX_RECORDING_DESCRIPTION_LEN + 500)),
        }
        .normalized();
        assert!(notes.title.is_none());
        assert_eq!(
            notes.description.unwrap().chars().count(),
            MAX_RECORDING_DESCRIPTION_LEN
        );
        assert!(RecordingNotesDto::default().is_empty());
    }

    #[test]
    fn recording_notes_truncate_on_character_boundaries() {
        // Byte truncation would panic on a multi-byte character.
        let notes = RecordingNotesDto {
            title: Some("é".repeat(MAX_RECORDING_TITLE_LEN + 20)),
            description: None,
        }
        .normalized();
        assert_eq!(notes.title.unwrap().chars().count(), MAX_RECORDING_TITLE_LEN);
    }

    #[test]
    fn no_recording_filter_is_on_unless_the_user_turned_it_on() {
        // Capture sources may default on — a microphone and system audio are
        // what the user asked to record. A FILTER changes what those sources
        // sound or look like, and none may be on by default.
        assert!(!AudioOptionsDto::default().reduce_noise);

        // And a settings file written before the filter existed reads as off,
        // rather than an app update quietly changing how someone's microphone
        // sounds.
        let legacy: AudioOptionsDto = serde_json::from_str(
            r#"{"includeSystemAudio":true,"includeMic":true,"micDeviceId":null}"#,
        )
        .expect("deserialize legacy audio settings");
        assert!(!legacy.reduce_noise);
    }

    #[test]
    fn window_recordings_default_to_the_window_alone() {
        // A "Specific window" take records only that
        // window unless the user picks the screen area in Settings. Settings
        // saved before the option existed (every install that recorded a
        // window as a fixed area) read as the new default too.
        assert_eq!(
            VideoOptionsDto::default().window_capture,
            WindowCaptureModeDto::Window
        );
        let legacy: VideoOptionsDto = serde_json::from_str(r#"{"maxHeight":1080}"#)
            .expect("deserialize legacy video settings");
        assert_eq!(legacy.max_height, Some(1080));
        assert_eq!(legacy.window_capture, WindowCaptureModeDto::Window);

        let area: VideoOptionsDto =
            serde_json::from_str(r#"{"maxHeight":null,"windowCapture":"area"}"#)
                .expect("deserialize area");
        assert_eq!(area.window_capture, WindowCaptureModeDto::Area);
    }

    #[test]
    fn only_a_window_source_in_window_mode_is_window_only() {
        let window = CaptureSourceDto::Window { id: 7 };
        let display = CaptureSourceDto::Display { id: 1 };
        let window_mode = VideoOptionsDto::default();
        let area_mode = VideoOptionsDto {
            window_capture: WindowCaptureModeDto::Area,
            ..VideoOptionsDto::default()
        };

        // The Windows app records every window as a fixed area.
        assert_eq!(
            is_window_only_capture(Some(&window), &window_mode),
            cfg!(target_os = "macos")
        );
        assert!(!is_window_only_capture(Some(&window), &area_mode));
        assert!(!is_window_only_capture(Some(&display), &window_mode));
        assert!(!is_window_only_capture(None, &window_mode));
    }

    #[test]
    fn legacy_webcam_settings_default_to_elevated_square_medium() {
        let webcam: WebcamCaptureDto =
            serde_json::from_str(r#"{"enabled":true,"layout":"bubble","deviceId":null}"#)
                .expect("deserialize legacy webcam settings");

        assert_eq!(webcam.shape, WebcamShapeDto::Square);
        // What the test has been called all along, and now what it asserts.
        assert_eq!(webcam.size, WebcamSizeDto::Medium);
    }

    #[test]
    fn countdown_seconds_are_clamped() {
        assert_eq!(normalize_countdown_seconds(0), 0);
        assert_eq!(normalize_countdown_seconds(3), 3);
        assert_eq!(normalize_countdown_seconds(999), MAX_COUNTDOWN_SECONDS);
    }
}
