//! When the Windows microphone stream should be rebuilt mid-recording.
//!
//! Beta 1.1.81: QA turned microphone access off in Windows Settings during a
//! take (Clipy said "no mic input", correctly), turned it back on, and the
//! mic stayed silent for the rest of the take. The Windows capturer opened
//! one cpal stream per take and its error callback only logged, so nothing
//! ever reopened it. The Mac already has a supervisor
//! (`capture/audio.rs`, `supervise_blocking`); this is the Windows decision
//! half, kept free of platform code so it is tested on every OS. The thread
//! that acts on it is `MicSupervisor` in `capture/windows.rs`.
//!
//! The rules, in the order [`MicReopenPolicy::decide`] applies them:
//! - The device to record is gone (the picked mic unplugged, or no default
//!   input at all): close the stream and wait. A picked mic is never swapped
//!   for an unrelated device; it is found again by exact name, then by one
//!   name containing the other (a renamed Bluetooth profile), the same rule
//!   as the Mac's `explicit_device_action` and `match_input_device`.
//! - Microphone access is off in Windows privacy settings: keep waiting, a
//!   reopen would only fail or deliver zeros. When access comes back, reopen
//!   at once.
//! - No stream (an open failed, or the device came back): retry the open
//!   every [`RETRY_OPEN_MS`].
//! - The stream reported an error: reopen, backing off from
//!   [`MIN_REOPEN_GAP_MS`] to [`FAULT_MAX_RETRY_MS`] while it keeps failing.
//! - The selection is "system default" and the default changed, or the
//!   picked mic is back under its own name: reopen on it.
//! - The callback stopped delivering for [`STALL_MS`]: reopen.
//! - The callback delivers only exact digital silence (what Windows feeds an
//!   app whose access was revoked, when the privacy check cannot tell): retry
//!   after [`SILENCE_FIRST_RETRY_MS`], backing off to
//!   [`SILENCE_MAX_RETRY_MS`], until real audio returns. Noise gates (Krisp,
//!   NVIDIA Broadcast) also send exact zeros between words; the back-off
//!   keeps a long pause on one of those to a handful of quick reopens of a
//!   stream that is silent anyway.
//!
//! Cap rebuilds its Windows microphone stream on a stream error and drops
//! frames from a stream it has replaced (`crates/recording/src/feeds/
//! microphone.rs`, `RecordingSubscription`); the silence and privacy rules
//! are Clipy's own.

// Only the Windows recorder acts on this; the Mac has its own supervisor.
#![cfg_attr(not(target_os = "windows"), allow(dead_code))]

use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Instant;

/// Wait between attempts to open a stream that is not running.
pub const RETRY_OPEN_MS: u64 = 2_000;
/// Shortest gap between two reopens of a running stream, so a stream that
/// errors on every open cannot spin.
pub const MIN_REOPEN_GAP_MS: u64 = 500;
/// No callback for this long means the stream has stopped. WASAPI shared
/// mode delivers a buffer every ~10 ms, silence included.
pub const STALL_MS: u64 = 3_000;
/// Exact digital silence before the first reopen.
pub const SILENCE_FIRST_RETRY_MS: u64 = 3_000;
/// Longest wait between silence reopens.
pub const SILENCE_MAX_RETRY_MS: u64 = 12_000;
/// After this many silence reopens in a row with no real audio, the source
/// is taken to send zeros on purpose (a hardware mute button, input volume
/// at zero, an idle virtual cable), and retries slow to
/// [`SILENCE_LONG_RETRY_MS`]. QA's case, access turned off, is caught by the
/// privacy check instead and reopens the moment access returns.
pub const SILENCE_RETRIES_BEFORE_LONG: u32 = 6;
pub const SILENCE_LONG_RETRY_MS: u64 = 60_000;
/// A callback within this window counts as "delivering".
const DELIVERING_WINDOW_MS: u64 = 1_000;
/// Longest wait between reopens of a stream that keeps erroring or
/// stalling. Without a cap on the rate, a broken driver would be rebuilt
/// twice a second for the whole take.
pub const FAULT_MAX_RETRY_MS: u64 = 15_000;
/// Reasons kept for telemetry; later reopens only add to the count.
pub const MAX_REASONS: usize = 4;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReopenReason {
    StreamError,
    DeviceReturned,
    DefaultChanged,
    Stalled,
    DigitalSilence,
    AccessRestored,
    RetryOpen,
}

impl ReopenReason {
    /// Slug for telemetry. Never a device name.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::StreamError => "stream_error",
            Self::DeviceReturned => "device_returned",
            Self::DefaultChanged => "default_changed",
            Self::Stalled => "stalled",
            Self::DigitalSilence => "digital_silence",
            Self::AccessRestored => "access_restored",
            Self::RetryOpen => "retry_open",
        }
    }
}

/// What the supervisor saw on one poll. Times are milliseconds on one
/// monotonic clock (the supervisor's own start).
#[derive(Clone, Copy, Debug, Default)]
pub struct MicObservation {
    pub now_ms: u64,
    /// A stream is open and playing.
    pub stream_open: bool,
    /// The open stream reported an error since it was opened.
    pub errored: bool,
    /// The user picked a device (not "system default").
    pub explicit: bool,
    /// The device to record exists: the picked mic (by name), or a default
    /// input when following the default.
    pub target_present: bool,
    /// The open stream is on that device.
    pub current_is_target: bool,
    /// Windows privacy settings deny Clipy the microphone.
    pub access_denied: bool,
    /// When the open stream was opened.
    pub opened_at_ms: u64,
    /// Last callback of the open stream, if any.
    pub last_delivery_ms: Option<u64>,
    /// Last callback holding a sample that was not exactly zero.
    pub last_signal_ms: Option<u64>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MicAction {
    Keep,
    /// Close the open stream and wait (the device is gone).
    Close,
    Reopen(ReopenReason),
}

/// Telemetry for one take: successful reopens and why the first few happened.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct MicReopenStats {
    pub reopens: u32,
    pub reasons: Vec<&'static str>,
}

impl MicReopenStats {
    /// Reasons joined for one telemetry slug, in order (`stream_error-digital_silence`).
    pub fn reasons_slug(&self) -> Option<String> {
        (!self.reasons.is_empty()).then(|| self.reasons.join("-"))
    }
}

#[derive(Debug, Default)]
pub struct MicReopenPolicy {
    last_attempt_ms: Option<u64>,
    /// Consecutive silence reopens without real audio in between.
    silence_retries: u32,
    /// Consecutive error or stall reopens without real audio in between.
    fault_retries: u32,
    /// Access was denied at some point since the last successful open.
    access_was_denied: bool,
    /// The stream was closed because its device disappeared.
    closed_for_absence: bool,
    stats: MicReopenStats,
}

impl MicReopenPolicy {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn stats(&self) -> &MicReopenStats {
        &self.stats
    }

    fn since_attempt(&self, now: u64) -> u64 {
        self.last_attempt_ms
            .map_or(u64::MAX, |at| now.saturating_sub(at))
    }

    fn silence_wait_ms(&self) -> u64 {
        if self.silence_retries >= SILENCE_RETRIES_BEFORE_LONG {
            return SILENCE_LONG_RETRY_MS;
        }
        let shift = self.silence_retries.min(8);
        SILENCE_FIRST_RETRY_MS
            .saturating_mul(1u64 << shift)
            .min(SILENCE_MAX_RETRY_MS)
    }

    /// `MIN_REOPEN_GAP_MS`, doubling per fault reopen, up to
    /// `FAULT_MAX_RETRY_MS`.
    fn fault_wait_ms(&self) -> u64 {
        let shift = self.fault_retries.min(8);
        MIN_REOPEN_GAP_MS
            .saturating_mul(1u64 << shift)
            .min(FAULT_MAX_RETRY_MS)
    }

    pub fn decide(&mut self, o: &MicObservation) -> MicAction {
        let now = o.now_ms;
        if !o.target_present {
            if o.stream_open {
                self.closed_for_absence = true;
                return MicAction::Close;
            }
            return MicAction::Keep;
        }
        if o.access_denied {
            self.access_was_denied = true;
            return if o.stream_open && o.errored {
                MicAction::Close
            } else {
                MicAction::Keep
            };
        }
        if !o.stream_open {
            if self.since_attempt(now) < RETRY_OPEN_MS {
                return MicAction::Keep;
            }
            let reason = if self.access_was_denied {
                ReopenReason::AccessRestored
            } else if self.closed_for_absence {
                ReopenReason::DeviceReturned
            } else {
                ReopenReason::RetryOpen
            };
            return MicAction::Reopen(reason);
        }
        // A running stream is never rebuilt twice in quick succession.
        if self.since_attempt(now) < MIN_REOPEN_GAP_MS {
            return MicAction::Keep;
        }
        if self.access_was_denied {
            return MicAction::Reopen(ReopenReason::AccessRestored);
        }
        if o.last_signal_ms
            .is_some_and(|signal| self.last_attempt_ms.is_none_or(|attempt| signal > attempt))
        {
            // Real audio since the last reopen: the stream works.
            self.silence_retries = 0;
            self.fault_retries = 0;
        }
        if o.errored {
            if self.since_attempt(now) < self.fault_wait_ms() {
                return MicAction::Keep;
            }
            return MicAction::Reopen(ReopenReason::StreamError);
        }
        if !o.current_is_target {
            return MicAction::Reopen(if o.explicit {
                ReopenReason::DeviceReturned
            } else {
                ReopenReason::DefaultChanged
            });
        }
        let last_delivery = o.last_delivery_ms.unwrap_or(o.opened_at_ms);
        let undelivered = now.saturating_sub(last_delivery);
        if undelivered >= STALL_MS
            && self.since_attempt(now) >= RETRY_OPEN_MS.max(self.fault_wait_ms())
        {
            return MicAction::Reopen(ReopenReason::Stalled);
        }
        let delivering = undelivered < DELIVERING_WINDOW_MS;
        let quiet_since = o.last_signal_ms.unwrap_or(0).max(o.opened_at_ms);
        let wait = self.silence_wait_ms();
        if delivering && now.saturating_sub(quiet_since) >= wait && self.since_attempt(now) >= wait
        {
            return MicAction::Reopen(ReopenReason::DigitalSilence);
        }
        MicAction::Keep
    }

    /// Records the outcome of acting on `decide`'s `Reopen`.
    pub fn record_attempt(&mut self, reason: ReopenReason, opened: bool, now_ms: u64) {
        self.last_attempt_ms = Some(now_ms);
        match reason {
            ReopenReason::DigitalSilence => {
                self.silence_retries = self.silence_retries.saturating_add(1);
            }
            ReopenReason::StreamError | ReopenReason::Stalled => {
                self.fault_retries = self.fault_retries.saturating_add(1);
            }
            _ => {}
        }
        if opened {
            self.access_was_denied = false;
            self.closed_for_absence = false;
            self.stats.reopens = self.stats.reopens.saturating_add(1);
            if self.stats.reasons.len() < MAX_REASONS {
                self.stats.reasons.push(reason.as_str());
            }
        }
    }

    /// Records that the take started without a stream (the first open
    /// failed), so the next try waits [`RETRY_OPEN_MS`] like any failed open.
    pub fn record_first_open_failed(&mut self, now_ms: u64) {
        self.last_attempt_ms = Some(now_ms);
    }

    /// Records that the stream was closed after `MicAction::Close`.
    pub fn record_close(&mut self, now_ms: u64) {
        self.last_attempt_ms = Some(now_ms);
    }
}

/// Where the first chunk of a rebuilt mic stream goes on a lane whose write
/// cursor is `cursor`, given the position its capture time maps to: on the
/// capture clock, so the mic stays in sync with the video after a reopen,
/// but never behind audio already written (that would overlap it). The gap
/// between the two streams stays as silence.
pub fn position_after_reopen(cursor: i64, wall_pts: i64) -> i64 {
    cursor.max(wall_pts)
}

/// Written by the device callback, read by the supervisor: when the stream
/// last delivered a buffer and when a buffer last held real audio. Atomic
/// stores only, so the callback never waits on the supervisor.
#[derive(Debug)]
pub struct MicSignalClock {
    epoch: Instant,
    /// Milliseconds since `epoch` plus one; zero means never.
    last_delivery: AtomicU64,
    last_signal: AtomicU64,
}

impl Default for MicSignalClock {
    fn default() -> Self {
        Self::new()
    }
}

impl MicSignalClock {
    pub fn new() -> Self {
        Self {
            epoch: Instant::now(),
            last_delivery: AtomicU64::new(0),
            last_signal: AtomicU64::new(0),
        }
    }

    pub fn now_ms(&self) -> u64 {
        self.epoch.elapsed().as_millis() as u64
    }

    /// Called from the device callback for each buffer.
    pub fn note(&self, samples: &[f32]) {
        let stamp = self.now_ms().saturating_add(1);
        self.last_delivery.store(stamp, Ordering::Release);
        if super::mic_silence::chunk_peak(samples) > super::mic_silence::DIGITAL_SILENCE_PEAK {
            self.last_signal.store(stamp, Ordering::Release);
        }
    }

    pub fn last_delivery_ms(&self) -> Option<u64> {
        read_stamp(&self.last_delivery)
    }

    pub fn last_signal_ms(&self) -> Option<u64> {
        read_stamp(&self.last_signal)
    }
}

fn read_stamp(stamp: &AtomicU64) -> Option<u64> {
    match stamp.load(Ordering::Acquire) {
        0 => None,
        value => Some(value - 1),
    }
}

/// The connected device a picked microphone maps to: the exact name when
/// present; then the same name once Windows' instance prefix is ignored
/// (a mic replugged into another USB port comes back as
/// "Microphone (2- Yeti Stereo Microphone)"); then a case-insensitive
/// containment match either way, so a Bluetooth headset that returns under a
/// renamed profile is found again. The last rule is the Mac's
/// `match_input_device` and Cap's `find_mic_by_label_or_fuzzy`; unrelated
/// devices are never matched.
pub fn match_input_device<'a>(devices: &'a [String], target: &str) -> Option<&'a str> {
    if let Some(exact) = devices.iter().find(|name| name.as_str() == target) {
        return Some(exact.as_str());
    }
    let target_key = endpoint_key(target);
    if target_key.trim().is_empty() {
        return None;
    }
    if let Some(same) = devices.iter().find(|name| endpoint_key(name) == target_key) {
        return Some(same.as_str());
    }
    devices
        .iter()
        .find(|name| {
            let key = endpoint_key(name);
            !key.trim().is_empty() && (key.contains(&target_key) || target_key.contains(&key))
        })
        .map(String::as_str)
}

/// Lowercase, with every Windows instance prefix (`(2- `, `(13- `) reduced
/// to `(`.
fn endpoint_key(name: &str) -> String {
    let lower = name.to_lowercase();
    let mut out = String::with_capacity(lower.len());
    let mut rest = lower.as_str();
    while let Some(open) = rest.find('(') {
        out.push_str(&rest[..=open]);
        rest = &rest[open + 1..];
        let digits = rest.chars().take_while(char::is_ascii_digit).count();
        if digits > 0 && rest[digits..].starts_with("- ") {
            rest = &rest[digits + 2..];
        }
    }
    out.push_str(rest);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A healthy default-device stream opened at t=0 that has been
    /// delivering real audio up to `now`.
    fn healthy(now: u64) -> MicObservation {
        MicObservation {
            now_ms: now,
            stream_open: true,
            errored: false,
            explicit: false,
            target_present: true,
            current_is_target: true,
            access_denied: false,
            opened_at_ms: 0,
            last_delivery_ms: Some(now),
            last_signal_ms: Some(now),
        }
    }

    #[test]
    fn a_working_stream_is_kept() {
        let mut policy = MicReopenPolicy::new();
        for now in (0..60_000).step_by(250) {
            assert_eq!(policy.decide(&healthy(now)), MicAction::Keep);
        }
    }

    #[test]
    fn a_stream_error_reopens_and_is_counted() {
        let mut policy = MicReopenPolicy::new();
        let o = MicObservation {
            errored: true,
            ..healthy(5_000)
        };
        assert_eq!(
            policy.decide(&o),
            MicAction::Reopen(ReopenReason::StreamError)
        );
        policy.record_attempt(ReopenReason::StreamError, true, 5_000);
        assert_eq!(policy.stats().reopens, 1);
        assert_eq!(
            policy.stats().reasons_slug().as_deref(),
            Some("stream_error")
        );
        // The new stream errors at once: not rebuilt again within the gap.
        let again = MicObservation {
            errored: true,
            opened_at_ms: 5_000,
            ..healthy(5_200)
        };
        assert_eq!(policy.decide(&again), MicAction::Keep);
    }

    /// QA's case, when Windows feeds zeros instead of erroring: access off,
    /// then on. Every sample is exactly zero from t=10 s.
    #[test]
    fn exact_silence_retries_until_audio_returns() {
        let mut policy = MicReopenPolicy::new();
        let mut opened_at = 0;
        let mut reopened_at = Vec::new();
        for now in (10_000..40_000).step_by(250) {
            let o = MicObservation {
                opened_at_ms: opened_at,
                last_delivery_ms: Some(now),
                last_signal_ms: Some(10_000),
                ..healthy(now)
            };
            if let MicAction::Reopen(reason) = policy.decide(&o) {
                assert_eq!(reason, ReopenReason::DigitalSilence);
                policy.record_attempt(reason, true, now);
                opened_at = now;
                reopened_at.push(now);
            }
        }
        assert_eq!(
            reopened_at.first(),
            Some(&13_000),
            "first retry after 3 s of zeros"
        );
        let gaps: Vec<u64> = reopened_at.windows(2).map(|w| w[1] - w[0]).collect();
        assert_eq!(gaps[..2], [6_000, 12_000], "backs off");
        assert!(gaps.iter().all(|gap| *gap <= SILENCE_MAX_RETRY_MS));

        // Audio comes back after a reopen: the back-off resets.
        let now = 40_000;
        let o = MicObservation {
            opened_at_ms: opened_at,
            ..healthy(now)
        };
        assert_eq!(policy.decide(&o), MicAction::Keep);
        let later = MicObservation {
            opened_at_ms: opened_at,
            last_signal_ms: Some(now),
            ..healthy(now + SILENCE_FIRST_RETRY_MS)
        };
        assert_eq!(
            policy.decide(&later),
            MicAction::Reopen(ReopenReason::DigitalSilence),
            "next silence starts from the shortest wait again"
        );
    }

    #[test]
    fn access_denied_waits_then_reopens_the_moment_it_returns() {
        let mut policy = MicReopenPolicy::new();
        let denied = MicObservation {
            access_denied: true,
            last_signal_ms: Some(1_000),
            ..healthy(20_000)
        };
        assert_eq!(
            policy.decide(&denied),
            MicAction::Keep,
            "no point reopening while denied"
        );
        let allowed = MicObservation {
            last_signal_ms: Some(1_000),
            ..healthy(20_250)
        };
        assert_eq!(
            policy.decide(&allowed),
            MicAction::Reopen(ReopenReason::AccessRestored)
        );
        policy.record_attempt(ReopenReason::AccessRestored, true, 20_250);
        let after = MicObservation {
            opened_at_ms: 20_250,
            ..healthy(20_500)
        };
        assert_eq!(policy.decide(&after), MicAction::Keep);
    }

    #[test]
    fn a_denied_open_keeps_retrying_once_access_is_back() {
        let mut policy = MicReopenPolicy::new();
        // The stream errored and was closed while access was off.
        let closed = MicObservation {
            stream_open: false,
            access_denied: true,
            ..healthy(10_000)
        };
        assert_eq!(policy.decide(&closed), MicAction::Keep);
        let back = MicObservation {
            stream_open: false,
            ..healthy(10_500)
        };
        assert_eq!(
            policy.decide(&back),
            MicAction::Reopen(ReopenReason::AccessRestored)
        );
        policy.record_attempt(ReopenReason::AccessRestored, false, 10_500);
        // Failed: the next try waits RETRY_OPEN_MS.
        let soon = MicObservation {
            stream_open: false,
            ..healthy(11_000)
        };
        assert_eq!(policy.decide(&soon), MicAction::Keep);
        let later = MicObservation {
            stream_open: false,
            ..healthy(12_500)
        };
        assert_eq!(
            policy.decide(&later),
            MicAction::Reopen(ReopenReason::AccessRestored)
        );
        policy.record_attempt(ReopenReason::AccessRestored, true, 12_500);
        assert_eq!(policy.stats().reopens, 1, "only a successful open counts");
    }

    #[test]
    fn a_picked_mic_that_disappears_is_closed_and_reopened_when_back() {
        let mut policy = MicReopenPolicy::new();
        let gone = MicObservation {
            explicit: true,
            target_present: false,
            ..healthy(8_000)
        };
        assert_eq!(policy.decide(&gone), MicAction::Close);
        policy.record_close(8_000);
        let still_gone = MicObservation {
            stream_open: false,
            ..gone
        };
        assert_eq!(
            policy.decide(&still_gone),
            MicAction::Keep,
            "never substituted"
        );
        let back = MicObservation {
            explicit: true,
            stream_open: false,
            ..healthy(15_000)
        };
        assert_eq!(
            policy.decide(&back),
            MicAction::Reopen(ReopenReason::DeviceReturned)
        );
        policy.record_attempt(ReopenReason::DeviceReturned, true, 15_000);
        assert_eq!(policy.stats().reasons, vec!["device_returned"]);
    }

    #[test]
    fn following_the_default_switches_when_it_changes() {
        let mut policy = MicReopenPolicy::new();
        let changed = MicObservation {
            current_is_target: false,
            ..healthy(4_000)
        };
        assert_eq!(
            policy.decide(&changed),
            MicAction::Reopen(ReopenReason::DefaultChanged)
        );
    }

    #[test]
    fn a_stopped_callback_is_reopened() {
        let mut policy = MicReopenPolicy::new();
        let stalled = MicObservation {
            last_delivery_ms: Some(1_000),
            last_signal_ms: Some(1_000),
            ..healthy(1_000 + STALL_MS)
        };
        assert_eq!(
            policy.decide(&stalled),
            MicAction::Reopen(ReopenReason::Stalled)
        );
        // A stream that never delivered at all counts from its open.
        let mut fresh = MicReopenPolicy::new();
        let never = MicObservation {
            opened_at_ms: 2_000,
            last_delivery_ms: None,
            last_signal_ms: None,
            ..healthy(2_000 + STALL_MS)
        };
        assert_eq!(
            fresh.decide(&never),
            MicAction::Reopen(ReopenReason::Stalled)
        );
    }

    #[test]
    fn telemetry_keeps_the_first_reasons_and_counts_the_rest() {
        let mut policy = MicReopenPolicy::new();
        for i in 0..(MAX_REASONS as u64 + 3) {
            policy.record_attempt(ReopenReason::StreamError, true, i * 1_000);
        }
        assert_eq!(policy.stats().reopens, MAX_REASONS as u32 + 3);
        assert_eq!(policy.stats().reasons.len(), MAX_REASONS);
        assert!(
            policy.stats().reasons_slug().unwrap().len() <= 80,
            "fits the analytics slug limit"
        );
        assert_eq!(MicReopenStats::default().reasons_slug(), None);
    }

    /// A broken driver that errors on every open is retried less and less
    /// often, not twice a second for the whole take.
    #[test]
    fn a_stream_that_keeps_erroring_backs_off() {
        let mut policy = MicReopenPolicy::new();
        let mut reopened_at = Vec::new();
        for now in (1_000..120_000).step_by(250) {
            let o = MicObservation {
                errored: true,
                opened_at_ms: reopened_at.last().copied().unwrap_or(0),
                last_delivery_ms: None,
                last_signal_ms: None,
                ..healthy(now)
            };
            if let MicAction::Reopen(reason) = policy.decide(&o) {
                assert_eq!(reason, ReopenReason::StreamError);
                policy.record_attempt(reason, true, now);
                reopened_at.push(now);
            }
        }
        let gaps: Vec<u64> = reopened_at.windows(2).map(|w| w[1] - w[0]).collect();
        assert!(
            gaps.windows(2).all(|w| w[1] >= w[0]),
            "never speeds up: {gaps:?}"
        );
        assert_eq!(gaps.last(), Some(&FAULT_MAX_RETRY_MS));
        assert!(
            reopened_at.len() < 20,
            "{} reopens in two minutes",
            reopened_at.len()
        );

        // Audio after a reopen resets the back-off.
        let last = *reopened_at.last().unwrap();
        let o = MicObservation {
            errored: true,
            opened_at_ms: last,
            last_signal_ms: Some(last + 100),
            ..healthy(last + MIN_REOPEN_GAP_MS)
        };
        assert_eq!(
            policy.decide(&o),
            MicAction::Reopen(ReopenReason::StreamError)
        );
    }

    #[test]
    fn a_stream_that_keeps_stalling_backs_off() {
        let mut policy = MicReopenPolicy::new();
        let mut opened_at = 0;
        let mut reopened_at = Vec::new();
        for now in (0..180_000).step_by(250) {
            let o = MicObservation {
                opened_at_ms: opened_at,
                last_delivery_ms: None,
                last_signal_ms: None,
                ..healthy(now)
            };
            if let MicAction::Reopen(reason) = policy.decide(&o) {
                assert_eq!(reason, ReopenReason::Stalled);
                policy.record_attempt(reason, true, now);
                opened_at = now;
                reopened_at.push(now);
            }
        }
        let gaps: Vec<u64> = reopened_at.windows(2).map(|w| w[1] - w[0]).collect();
        assert_eq!(gaps.last(), Some(&FAULT_MAX_RETRY_MS));
    }

    #[test]
    fn a_reopened_stream_lands_on_the_capture_clock() {
        // 80 ms reopen gap at 48 kHz: placed after it, not straight after
        // the old stream (which would play the mic early from then on).
        assert_eq!(position_after_reopen(48_000, 48_000 + 3_840), 51_840);
        // A late first chunk never writes over what is already there.
        assert_eq!(position_after_reopen(48_000, 47_000), 48_000);
    }

    /// A source that always sends zeros (hardware mute, idle virtual
    /// cable) is retried less often once it has clearly chosen to.
    #[test]
    fn a_source_that_always_sends_zeros_slows_to_once_a_minute() {
        let mut policy = MicReopenPolicy::new();
        let mut opened_at = 0;
        let mut reopened_at = Vec::new();
        for now in (0..600_000).step_by(250) {
            let o = MicObservation {
                opened_at_ms: opened_at,
                last_signal_ms: None,
                ..healthy(now)
            };
            if let MicAction::Reopen(reason) = policy.decide(&o) {
                assert_eq!(reason, ReopenReason::DigitalSilence);
                policy.record_attempt(reason, true, now);
                opened_at = now;
                reopened_at.push(now);
            }
        }
        let gaps: Vec<u64> = reopened_at.windows(2).map(|w| w[1] - w[0]).collect();
        assert_eq!(gaps.last(), Some(&SILENCE_LONG_RETRY_MS));
        assert!(
            reopened_at.len() < 20,
            "{} reopens in ten minutes",
            reopened_at.len()
        );
    }

    #[test]
    fn a_first_open_that_failed_is_retried_after_the_normal_gap() {
        let mut policy = MicReopenPolicy::new();
        policy.record_first_open_failed(0);
        let closed = |now| MicObservation {
            stream_open: false,
            ..healthy(now)
        };
        assert_eq!(policy.decide(&closed(1_000)), MicAction::Keep);
        assert_eq!(
            policy.decide(&closed(RETRY_OPEN_MS)),
            MicAction::Reopen(ReopenReason::RetryOpen)
        );
        // Access off at Start: wait, then reopen the moment it returns.
        let mut denied = MicReopenPolicy::new();
        denied.record_first_open_failed(0);
        let off = MicObservation {
            access_denied: true,
            ..closed(5_000)
        };
        assert_eq!(denied.decide(&off), MicAction::Keep);
        assert_eq!(
            denied.decide(&closed(5_250)),
            MicAction::Reopen(ReopenReason::AccessRestored)
        );
        // The picked mic unplugged at Start: closed until it is back.
        let mut gone = MicReopenPolicy::new();
        gone.record_first_open_failed(0);
        let absent = MicObservation {
            target_present: false,
            explicit: true,
            ..closed(5_000)
        };
        assert_eq!(gone.decide(&absent), MicAction::Keep);
        assert_eq!(
            gone.decide(&MicObservation {
                explicit: true,
                ..closed(6_000)
            }),
            MicAction::Reopen(ReopenReason::RetryOpen)
        );
    }

    #[test]
    fn the_signal_clock_tells_zeros_from_audio() {
        let clock = MicSignalClock::new();
        assert_eq!(clock.last_delivery_ms(), None);
        assert_eq!(clock.last_signal_ms(), None);
        clock.note(&[0.0; 480]);
        assert!(clock.last_delivery_ms().is_some());
        assert_eq!(clock.last_signal_ms(), None, "exact zeros are not signal");
        clock.note(&[0.0, 0.002, -0.001]);
        assert!(clock.last_signal_ms().is_some());
    }

    #[test]
    fn device_matching_is_exact_first_then_by_containment() {
        let devices = vec![
            "Microphone (Realtek(R) Audio)".to_string(),
            "Headset (Alex's AirPods Pro)".to_string(),
        ];
        assert_eq!(
            match_input_device(&devices, "Microphone (Realtek(R) Audio)"),
            Some("Microphone (Realtek(R) Audio)")
        );
        assert_eq!(
            match_input_device(&devices, "AirPods Pro"),
            Some("Headset (Alex's AirPods Pro)")
        );
        assert_eq!(match_input_device(&devices, "Yeti"), None);
        // Replugged into another port: Windows adds an instance prefix.
        let replugged = vec!["Microphone (2- Yeti Stereo Microphone)".to_string()];
        assert_eq!(
            match_input_device(&replugged, "Microphone (Yeti Stereo Microphone)"),
            Some("Microphone (2- Yeti Stereo Microphone)")
        );
        assert_eq!(
            match_input_device(
                &["Microphone (Yeti Stereo Microphone)".to_string()],
                "Microphone (2- Yeti Stereo Microphone)"
            ),
            Some("Microphone (Yeti Stereo Microphone)")
        );
        // Two of the same mic: the exact one wins.
        let both = vec![
            "Microphone (2- Yeti Stereo Microphone)".to_string(),
            "Microphone (Yeti Stereo Microphone)".to_string(),
        ];
        assert_eq!(
            match_input_device(&both, "Microphone (Yeti Stereo Microphone)"),
            Some("Microphone (Yeti Stereo Microphone)")
        );
        assert_eq!(
            endpoint_key("Line (12- USB Audio) (3- x)"),
            "line (usb audio) (x)"
        );
        assert_eq!(endpoint_key("Mic (2-x)"), "mic (2-x)");
        assert_eq!(match_input_device(&devices, "  "), None);
    }
}
