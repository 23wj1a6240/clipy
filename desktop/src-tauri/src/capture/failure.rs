//! Why a recording could not start, in a form both the user and the funnel
//! can use.
//!
//! Every local start failure used to reach PostHog as one bucket,
//! `capture_unavailable`, with nothing else attached. On Windows that bucket
//! was the most common recording failure, and there was no way to tell a
//! Windows build without a capture option from a closed window, a missing
//! graphics driver or a network error.
//!
//! A `CaptureFailure` carries three things:
//! - `reason`: a stable slug for `reason_code`, so the funnel can group;
//! - `error_name`: a short slug for detail (an API variant name, an HRESULT),
//!   never a path, device name or window title;
//! - `message`: what the user reads, saying what to do next.
//!
//! This module has no platform code so its mapping is tested on every host.

use std::fmt;

#[derive(Debug, Clone)]
pub struct CaptureFailure {
    pub reason: &'static str,
    pub error_name: Option<String>,
    message: String,
    /// Counts sent beside the reason on `recording_failed` (each key must be
    /// on the server's recording-journey allowlist).
    measures: Vec<(&'static str, serde_json::Value)>,
}

impl CaptureFailure {
    pub fn new(reason: &'static str, message: impl Into<String>) -> Self {
        Self {
            reason,
            error_name: None,
            message: message.into(),
            measures: Vec::new(),
        }
    }

    pub fn with_measures(mut self, measures: Vec<(&'static str, serde_json::Value)>) -> Self {
        self.measures = measures;
        self
    }

    pub fn measures(&self) -> &[(&'static str, serde_json::Value)] {
        &self.measures
    }

    pub fn with_error_name(mut self, error_name: impl Into<String>) -> Self {
        self.error_name = Some(error_name.into());
        self
    }

    pub fn message(&self) -> &str {
        &self.message
    }

    /// Give an untyped start error a reason without changing what the user
    /// reads. A server refusal and an error that already has a reason are
    /// returned untouched, so the most specific reason always wins.
    ///
    /// The tagged failure also gets an `error_name` read from the error
    /// (`untyped_error_name_of`), so no tagged start failure reaches PostHog
    /// with that field empty.
    pub fn tag(reason: &'static str, err: anyhow::Error) -> anyhow::Error {
        if crate::api_error::refusal_of(&err).is_some() || of(&err).is_some() {
            return err;
        }
        let error_name = untyped_error_name_of(&err);
        anyhow::Error::new(Self::new(reason, format!("{err:#}")).with_error_name(error_name))
    }
}

impl fmt::Display for CaptureFailure {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for CaptureFailure {}

/// The failure anywhere in the error chain, context layers included.
pub fn of(err: &anyhow::Error) -> Option<&CaptureFailure> {
    err.downcast_ref::<CaptureFailure>()
}

/// Shown when a start could not reach Clipy at all (beta 1.1.81: the network
/// dropped, Start was pressed, and nothing said why).
pub const OFFLINE_START: &str = "You're offline. Connect to the internet and try again.";

/// Shown when Recover could not reach Clipy. The local file is kept.
pub const OFFLINE_RECOVER: &str =
    "You're offline. Connect to the internet and try Recover again.";

/// The start failure for a refused or failed `/initiate`. The reason stays
/// `upload_initiate_failed` for the funnel; a connection that never reached
/// the server gets `error_name` `offline` and a sentence the user can act on
/// instead of reqwest's error chain. A server refusal passes through as is.
pub fn initiate_failure(err: anyhow::Error) -> anyhow::Error {
    if crate::api_error::is_offline(&err) {
        return anyhow::Error::new(
            CaptureFailure::new("upload_initiate_failed", OFFLINE_START).with_error_name("offline"),
        );
    }
    CaptureFailure::tag("upload_initiate_failed", err.context("start streaming upload"))
}

// windows-capture needs UniversalApiContract 8, which is Windows 10 1903.
const UPDATE_WINDOWS: &str = "Screen recording needs Windows 10 version 1903 or later. \
     Update Windows in Settings > Windows Update, then try again.";

const OPTION_UNSUPPORTED: &str = "This version of Windows refused a screen capture setting. \
     Update Windows in Settings > Windows Update, then try again.";

/// Map a failed Windows Graphics Capture start onto a reason and a message.
///
/// Takes the `Debug` text of windows-capture's error, because its error type
/// is generic over our handler and only exists in Windows builds. The text
/// holds only enum variant names and HRESULTs; the user's own data never
/// appears in it, and only names from the list below (plus the HRESULT) are
/// ever copied into `error_name`.
pub fn wgc_start_failure(debug: &str) -> CaptureFailure {
    // Checked in this order: the option variants contain "Unsupported" too.
    const KNOWN: &[(&str, &str)] = &[
        ("BorderConfigUnsupported", "wgc_option_unsupported"),
        ("CursorConfigUnsupported", "wgc_option_unsupported"),
        ("SecondaryWindowsUnsupported", "wgc_option_unsupported"),
        ("MinimumUpdateIntervalUnsupported", "wgc_option_unsupported"),
        ("DirtyRegionUnsupported", "wgc_option_unsupported"),
        ("FeatureLevelNotSatisfied", "wgc_directx_unavailable"),
        ("ItemConvertFailed", "wgc_item_failed"),
        ("FailedToInitWinRT", "wgc_runtime_failed"),
        (
            "FailedToCreateDispatcherQueueController",
            "wgc_runtime_failed",
        ),
        (
            "FailedToSetDispatcherQueueCompletedHandler",
            "wgc_runtime_failed",
        ),
        ("FailedToJoinThread", "wgc_runtime_failed"),
        ("AlreadyStarted", "wgc_runtime_failed"),
        ("WindowError", "wgc_item_failed"),
        ("DirectXError", "wgc_directx_unavailable"),
    ];
    let found = KNOWN
        .iter()
        .find(|(name, _)| debug.contains(name))
        .copied()
        .or_else(|| {
            // The bare `Unsupported` variant: no Graphics Capture at all.
            (debug.contains("(Unsupported)") || debug == "Unsupported")
                .then_some(("Unsupported", "wgc_unsupported"))
        });
    let hresult = hresult_of(debug);
    let error_name = match (found.map(|(name, _)| name), hresult.as_deref()) {
        (Some(name), Some(hr)) => Some(format!("{name}_{hr}")),
        (Some(name), None) => Some(name.to_string()),
        (None, Some(hr)) => Some(format!("hresult_{hr}")),
        (None, None) => None,
    };
    let reason = found
        .map(|(_, reason)| reason)
        .unwrap_or("wgc_start_failed");
    let message = match reason {
        "wgc_unsupported" => UPDATE_WINDOWS,
        "wgc_option_unsupported" => OPTION_UNSUPPORTED,
        "wgc_directx_unavailable" => {
            "Windows could not start screen capture on this graphics card. \
             Update your graphics driver, or record on the computer itself if this is Remote Desktop."
        }
        "wgc_item_failed" => {
            "Windows could not open that screen or window for capture. \
             Pick the source again, then retry."
        }
        _ => {
            "Windows could not start screen capture. Restart Clipy and retry, \
             or update Windows and your graphics driver if it keeps happening."
        }
    };
    let failure = CaptureFailure::new(reason, message);
    match error_name {
        Some(name) => failure.with_error_name(name),
        None => failure,
    }
}

/// A slug for an error nobody gave a `CaptureFailure` name: an I/O error's
/// kind when there is one, else `untyped_error_name` over the error's text.
pub fn untyped_error_name_of(err: &anyhow::Error) -> String {
    if let Some(io) = err.chain().find_map(|e| e.downcast_ref::<std::io::Error>()) {
        return format!("io_{:?}", io.kind());
    }
    untyped_error_name(&format!("{err:#}"))
}

/// Name an untyped start error from its text. Only names from the fixed list
/// below and numeric codes (an SCStreamError `Code=-3801`, an HRESULT) are
/// ever copied out; the text itself, which can hold a window title, a path or
/// a device name, never is. Unknown text reads `unclassified`, so the field is
/// never empty and a report can still count what is left to name.
pub fn untyped_error_name(text: &str) -> String {
    // Most specific first: a context layer ("start screen capture") wraps
    // the step that actually failed ("Capturer::start failed").
    const KNOWN: &[(&str, &str)] = &[
        ("permission_denied", "permission_denied"),
        ("not found in ShareableContent", "source_not_found"),
        ("is not visible", "window_not_visible"),
        ("is not on an available display", "window_off_display"),
        ("no displays available", "no_displays"),
        ("Capturer::build", "sck_build_failed"),
        ("Capturer::start", "sck_start_failed"),
        ("content fetch", "sck_content_fetch_failed"),
        ("webcam bubble activation failed", "webcam_bubble_failed"),
        ("Microphone unavailable", "mic_unavailable"),
        ("open ffmpeg MP4 encoder", "encoder_open_failed"),
        ("Mp4Encoder", "encoder_failed"),
        ("start streaming upload", "upload_initiate_failed"),
        ("Not enough free disk space", "disk_full"),
        ("start screen capture", "screen_capture_failed"),
    ];
    let base = KNOWN
        .iter()
        .find(|(needle, _)| text.contains(needle))
        .map(|(_, name)| *name)
        .unwrap_or("unclassified");
    if let Some(code) = sck_code_of(text) {
        return format!("{base}_code{code}");
    }
    if let Some(hr) = hresult_of(text) {
        return format!("{base}_{hr}");
    }
    base.to_string()
}

/// `Error Domain=<domain> Code=-3801 "..."` → `-3801` (NSError, which is how
/// cidre reports an SCStreamError). Only a `Code=` that directly follows an
/// `Error Domain=` token counts, and at most 6 digits are kept, so digits in
/// a window title or source name ("Code=4155551234") are never copied.
fn sck_code_of(text: &str) -> Option<String> {
    let domain = text.find("Error Domain=")?;
    let after_domain = &text[domain + "Error Domain=".len()..];
    let token_end = after_domain.find(' ')?;
    let rest = &after_domain[token_end + 1..];
    let rest = rest.strip_prefix("Code=")?;
    let start = text.len() - rest.len();
    let code: String = text[start..]
        .chars()
        .enumerate()
        .take_while(|(i, c)| c.is_ascii_digit() || (*i == 0 && *c == '-'))
        .map(|(_, c)| c)
        .take(7)
        .collect();
    code.chars().any(|c| c.is_ascii_digit()).then_some(code)
}

/// `HRESULT(0x887A0004)` → `887a0004`.
fn hresult_of(debug: &str) -> Option<String> {
    let start = debug.find("HRESULT(0x")? + "HRESULT(0x".len();
    let hex: String = debug[start..]
        .chars()
        .take_while(|c| c.is_ascii_hexdigit())
        .take(8)
        .collect();
    (!hex.is_empty()).then(|| hex.to_ascii_lowercase())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn untyped_errors_get_a_name_and_never_their_text() {
        assert_eq!(
            untyped_error_name(
                "start screen capture: Capturer::start failed: Error Domain=com.apple.ScreenCaptureKit.SCStreamErrorDomain Code=-3805 \"My Secret Doc.pdf\""
            ),
            "sck_start_failed_code-3805"
        );
        assert_eq!(
            untyped_error_name("start screen capture: window 42 not found in ShareableContent"),
            "source_not_found"
        );
        assert_eq!(
            untyped_error_name(
                "permission_denied: screen_recording (sck: Error Domain=com.apple.ScreenCaptureKit.SCStreamErrorDomain Code=-3801 \"denied\")"
            ),
            "permission_denied_code-3801"
        );
        // Digits in user text (a window title) are never copied.
        assert_eq!(
            untyped_error_name(
                "start screen capture: window \"Call Code=4155551234\" is not visible"
            ),
            "window_not_visible"
        );
        assert_eq!(
            untyped_error_name("Microphone unavailable. Turn Microphone off to record without it."),
            "mic_unavailable"
        );
        assert_eq!(
            untyped_error_name("something new: /Users/a/secret"),
            "unclassified"
        );
        let name =
            untyped_error_name("start screen capture: Capturer::start failed: Code=1 \"/Users/a\"");
        assert!(!name.contains('/') && !name.contains("Users"), "{name}");
    }

    #[test]
    fn untyped_io_errors_are_named_by_kind() {
        let err = anyhow::Error::new(std::io::Error::from(std::io::ErrorKind::PermissionDenied))
            .context("create recordings dir");
        assert_eq!(untyped_error_name_of(&err), "io_PermissionDenied");
    }

    #[test]
    fn tagging_an_untyped_error_fills_error_name() {
        let tagged = CaptureFailure::tag(
            "screen_capture_failed",
            anyhow::anyhow!("Capturer::start failed: Error Domain=x Code=-3802"),
        );
        let failure = of(&tagged).expect("tagged");
        assert_eq!(failure.reason, "screen_capture_failed");
        assert_eq!(
            failure.error_name.as_deref(),
            Some("sck_start_failed_code-3802")
        );
    }

    #[test]
    fn option_variants_are_not_read_as_no_capture_at_all() {
        let f = wgc_start_failure("GraphicsCaptureApiError(BorderConfigUnsupported)");
        assert_eq!(f.reason, "wgc_option_unsupported");
        assert_eq!(f.error_name.as_deref(), Some("BorderConfigUnsupported"));
        assert!(f.message().contains("Windows Update"));
    }

    #[test]
    fn bare_unsupported_means_the_api_is_missing() {
        let f = wgc_start_failure("GraphicsCaptureApiError(Unsupported)");
        assert_eq!(f.reason, "wgc_unsupported");
        assert!(f.message().contains("version 1903"));
    }

    #[test]
    fn hresult_is_kept_and_nothing_else_from_the_text() {
        let f = wgc_start_failure(
            "DirectXError(WindowsError(Error { code: HRESULT(0x887A0004), message: \"C:\\\\Users\\\\someone\" }))",
        );
        assert_eq!(f.reason, "wgc_directx_unavailable");
        assert_eq!(f.error_name.as_deref(), Some("DirectXError_887a0004"));
    }

    #[test]
    fn unknown_errors_fall_back_to_a_generic_reason() {
        let f = wgc_start_failure("SomethingNew");
        assert_eq!(f.reason, "wgc_start_failed");
        assert_eq!(f.error_name, None);
        let f = wgc_start_failure("Error { code: HRESULT(0x80070005) }");
        assert_eq!(f.error_name.as_deref(), Some("hresult_80070005"));
    }

    #[test]
    fn the_failure_survives_context_layers() {
        let err = anyhow::Error::new(CaptureFailure::new("wgc_no_frames", "no frames"))
            .context("start Windows screen capture");
        assert_eq!(of(&err).map(|f| f.reason), Some("wgc_no_frames"));
    }

    #[test]
    fn tag_keeps_the_text_and_never_overrides_a_specific_reason() {
        let plain = CaptureFailure::tag(
            "encoder_open_failed",
            anyhow::anyhow!("boom").context("open ffmpeg MP4 encoder"),
        );
        assert_eq!(of(&plain).map(|f| f.reason), Some("encoder_open_failed"));
        assert_eq!(plain.to_string(), "open ffmpeg MP4 encoder: boom");

        let specific = anyhow::Error::new(CaptureFailure::new("source_gone", "gone"))
            .context("start Windows screen capture");
        let tagged = CaptureFailure::tag("screen_capture_failed", specific);
        assert_eq!(of(&tagged).map(|f| f.reason), Some("source_gone"));
    }
}
