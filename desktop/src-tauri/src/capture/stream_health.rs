//! What ScreenCaptureKit actually sent during a take, for the recording
//! diagnostics and the `recording_failed` event.
//!
//! Mac recordings made on macOS 12.7.6 had an empty video track, and nothing
//! in the reports could say whether SCK sent no buffers at all, sent only status buffers without an image (idle, blank,
//! suspended), stopped the stream with an error, or sent frames the encoder
//! refused. These counters answer that for the next report.
//!
//! No platform code, so the serialization is tested on every host. Only
//! counts, a pixel-format slug, the stop error's numeric code and its domain
//! (an Apple constant such as `com.apple.ScreenCaptureKit.SCStreamErrorDomain`)
//! are kept; never the error message.

use std::sync::atomic::{AtomicU64, Ordering};

use parking_lot::Mutex;
use serde_json::{json, Value};

/// `SCFrameStatus` values in order: complete, idle, blank, suspended,
/// started, stopped.
pub const FRAME_STATUS_NAMES: [&str; 6] = [
    "complete",
    "idle",
    "blank",
    "suspended",
    "started",
    "stopped",
];

const MAX_DOMAIN_LEN: usize = 60;

/// Live counters, written from the SCK dispatch queue.
pub struct ScreenStreamStats {
    pixel_format: &'static str,
    screen_buffers: AtomicU64,
    frames_without_image: AtomicU64,
    status: [AtomicU64; 6],
    /// No status attachment, or a value outside the six known ones.
    status_other: AtomicU64,
    stop_error: Mutex<Option<(i64, String)>>,
}

impl ScreenStreamStats {
    pub fn new(pixel_format: &'static str) -> Self {
        Self {
            pixel_format,
            screen_buffers: AtomicU64::new(0),
            frames_without_image: AtomicU64::new(0),
            status: Default::default(),
            status_other: AtomicU64::new(0),
            stop_error: Mutex::new(None),
        }
    }

    /// One Screen sample buffer from SCK, before anything is filtered.
    pub fn note_screen_buffer(&self, has_image: bool, status: Option<i64>) {
        self.screen_buffers.fetch_add(1, Ordering::Relaxed);
        if !has_image {
            self.frames_without_image.fetch_add(1, Ordering::Relaxed);
        }
        match status
            .and_then(|s| usize::try_from(s).ok())
            .and_then(|s| self.status.get(s))
        {
            Some(counter) => counter.fetch_add(1, Ordering::Relaxed),
            None => self.status_other.fetch_add(1, Ordering::Relaxed),
        };
    }

    /// `stream:didStopWithError:`. The first error is kept: anything after
    /// it is a consequence.
    pub fn note_stop_error(&self, code: i64, domain: &str) {
        let mut guard = self.stop_error.lock();
        if guard.is_none() {
            *guard = Some((code, clean_domain(domain)));
        }
    }

    pub fn report(&self, rejected_frames: u64, rescaled_frames: u64) -> ScreenCaptureReport {
        let stop_error = self.stop_error.lock().clone();
        ScreenCaptureReport {
            pixel_format: Some(self.pixel_format),
            screen_buffers: self.screen_buffers.load(Ordering::Relaxed),
            frames_without_image: self.frames_without_image.load(Ordering::Relaxed),
            frame_status: std::array::from_fn(|i| self.status[i].load(Ordering::Relaxed)),
            frame_status_other: self.status_other.load(Ordering::Relaxed),
            stop_error_code: stop_error.as_ref().map(|(code, _)| *code),
            stop_error_domain: stop_error.map(|(_, domain)| domain),
            rejected_frames,
            rescaled_frames,
        }
    }
}

fn clean_domain(domain: &str) -> String {
    let cleaned: String = domain
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
        .take(MAX_DOMAIN_LEN)
        .collect();
    if cleaned.is_empty() {
        "unknown".to_string()
    } else {
        cleaned
    }
}

/// A snapshot of what the screen stream sent, taken when the take ends.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ScreenCaptureReport {
    /// What the stream was asked for: `bgra` or `420v`.
    pub pixel_format: Option<&'static str>,
    /// Every Screen buffer SCK sent, with or without an image.
    pub screen_buffers: u64,
    /// Status-only buffers: no image, so nothing to encode.
    pub frames_without_image: u64,
    /// Buffers per `SCFrameStatus`, in `FRAME_STATUS_NAMES` order.
    pub frame_status: [u64; 6],
    pub frame_status_other: u64,
    /// `stream:didStopWithError:` code (an `SCStreamError` such as -3801).
    pub stop_error_code: Option<i64>,
    pub stop_error_domain: Option<String>,
    /// Frames that arrived with an image but could not be encoded.
    pub rejected_frames: u64,
    /// Frames whose size differed from the stream's and were scaled to fit.
    pub rescaled_frames: u64,
}

impl ScreenCaptureReport {
    /// `health.screen_capture` in the recording diagnostics.
    pub fn to_json(&self) -> Value {
        let mut status = serde_json::Map::new();
        for (name, count) in FRAME_STATUS_NAMES.iter().zip(self.frame_status) {
            status.insert((*name).to_string(), json!(count));
        }
        status.insert("other".to_string(), json!(self.frame_status_other));
        let mut value = json!({
            "pixel_format": self.pixel_format,
            "screen_buffers": self.screen_buffers,
            "frames_without_image": self.frames_without_image,
            "frame_status": status,
            "rejected_frames": self.rejected_frames,
            "rescaled_frames": self.rescaled_frames,
        });
        if let Some(code) = self.stop_error_code {
            value["stop_error_code"] = json!(code);
        }
        if let Some(domain) = &self.stop_error_domain {
            value["stop_error_domain"] = json!(domain);
        }
        value
    }

    /// Flat keys for a funnel event. Each must be on the server's
    /// recording-journey allowlist.
    pub fn telemetry_measures(&self) -> Vec<(&'static str, Value)> {
        const STATUS_KEYS: [&str; 6] = [
            "sck_status_complete",
            "sck_status_idle",
            "sck_status_blank",
            "sck_status_suspended",
            "sck_status_started",
            "sck_status_stopped",
        ];
        let mut out: Vec<(&'static str, Value)> = vec![
            ("sck_screen_buffers", json!(self.screen_buffers)),
            ("sck_frames_without_image", json!(self.frames_without_image)),
            ("sck_status_other", json!(self.frame_status_other)),
            ("rejected_video_frames", json!(self.rejected_frames)),
            ("rescaled_video_frames", json!(self.rescaled_frames)),
        ];
        for (key, count) in STATUS_KEYS.iter().zip(self.frame_status) {
            out.push((*key, json!(count)));
        }
        if let Some(format) = self.pixel_format {
            out.push(("sck_pixel_format", json!(format)));
        }
        if let Some(code) = self.stop_error_code {
            out.push(("sck_stop_error_code", json!(code)));
        }
        if let Some(domain) = &self.stop_error_domain {
            out.push(("sck_stop_error_domain", json!(domain)));
        }
        out
    }

    /// `error_name` for a take that failed after SCK stopped the stream.
    pub fn stop_error_name(&self) -> Option<String> {
        self.stop_error_code.map(|code| format!("sck_stop_{code}"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn counts_status_only_buffers_and_statuses() {
        let stats = ScreenStreamStats::new("420v");
        stats.note_screen_buffer(true, Some(0));
        stats.note_screen_buffer(false, Some(1));
        stats.note_screen_buffer(false, Some(2));
        stats.note_screen_buffer(false, None);
        stats.note_screen_buffer(false, Some(42));
        let report = stats.report(3, 4);
        assert_eq!(report.screen_buffers, 5);
        assert_eq!(report.frames_without_image, 4);
        assert_eq!(report.frame_status, [1, 1, 1, 0, 0, 0]);
        assert_eq!(report.frame_status_other, 2);
        assert_eq!(report.rejected_frames, 3);
        assert_eq!(report.rescaled_frames, 4);
        assert_eq!(report.pixel_format, Some("420v"));
        assert_eq!(report.stop_error_code, None);
    }

    #[test]
    fn keeps_the_first_stop_error_and_cleans_its_domain() {
        let stats = ScreenStreamStats::new("bgra");
        stats.note_stop_error(-3801, "com.apple.ScreenCaptureKit.SCStreamErrorDomain");
        stats.note_stop_error(-3815, "other");
        let report = stats.report(0, 0);
        assert_eq!(report.stop_error_code, Some(-3801));
        assert_eq!(
            report.stop_error_domain.as_deref(),
            Some("com.apple.ScreenCaptureKit.SCStreamErrorDomain")
        );
        assert_eq!(report.stop_error_name().as_deref(), Some("sck_stop_-3801"));

        let odd = ScreenStreamStats::new("bgra");
        odd.note_stop_error(1, "a b/c\"<d>");
        assert_eq!(odd.report(0, 0).stop_error_domain.as_deref(), Some("abcd"));
        let empty = ScreenStreamStats::new("bgra");
        empty.note_stop_error(1, "  ");
        assert_eq!(
            empty.report(0, 0).stop_error_domain.as_deref(),
            Some("unknown")
        );
    }

    #[test]
    fn serializes_for_diagnostics_and_telemetry() {
        let stats = ScreenStreamStats::new("420v");
        stats.note_screen_buffer(false, Some(1));
        stats.note_stop_error(-3801, "com.apple.ScreenCaptureKit.SCStreamErrorDomain");
        let report = stats.report(0, 0);
        let json = report.to_json();
        assert_eq!(json["pixel_format"], "420v");
        assert_eq!(json["frames_without_image"], 1);
        assert_eq!(json["frame_status"]["idle"], 1);
        assert_eq!(json["frame_status"]["other"], 0);
        assert_eq!(json["stop_error_code"], -3801);

        let measures = report.telemetry_measures();
        let get = |key: &str| {
            measures
                .iter()
                .find(|(k, _)| *k == key)
                .map(|(_, v)| v.clone())
        };
        assert_eq!(get("sck_frames_without_image"), Some(json!(1)));
        assert_eq!(get("sck_status_idle"), Some(json!(1)));
        assert_eq!(get("sck_stop_error_code"), Some(json!(-3801)));
        assert_eq!(get("sck_pixel_format"), Some(json!("420v")));

        // No stop error: the keys are left out rather than sent as null.
        let clean = ScreenStreamStats::new("bgra").report(0, 0);
        assert!(clean.to_json().get("stop_error_code").is_none());
        assert!(!clean
            .telemetry_measures()
            .iter()
            .any(|(k, _)| *k == "sck_stop_error_code"));
    }
}
