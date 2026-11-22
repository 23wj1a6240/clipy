//! Live "your microphone is recording nothing" check for the Windows app.
//!
//! Pure logic, no platform APIs, so it compiles and is tested on every OS.
//!
//! Why it is not the macOS check: macOS warns when the meter level stays
//! under [`super::level::MIC_INPUT_ACTIVE_LEVEL`] (about -55 dBFS peak). A
//! quiet room with a low-gain mic, or Windows' own processing, sits under that
//! while the mic works fine. The first Windows attempt also fired on every
//! take because nothing fed the level at all. So this check only looks for
//! what a working mic does not produce:
//!
//! * **Digital silence**: every sample at or below [`DIGITAL_SILENCE_PEAK`]
//!   (-120 dBFS, under one 16-bit step and far under any analog noise floor,
//!   so in practice exact zeros). That is what a device muted in Windows, a
//!   mic blocked by privacy settings, or a dead endpoint delivers.
//! * **No chunks**: WASAPI shared-mode capture delivers buffers even in
//!   silence, so a mic whose callback delivers nothing for
//!   [`NO_CHUNKS_WARN_MS`] has stopped (unplugged, a Bluetooth headset
//!   switching profiles, a dead stream). Counted at the device callback, not
//!   in the pump, so a busy pump is never mistaken for a dead mic.
//!
//! Only time while recording with the mic on and unmuted in Clipy counts; a
//! pause, a mute or a mute toggle resets the clocks. Once the mic has sent
//! real audio in this take, digital silence alone no longer warns: noise
//! gates (NVIDIA Broadcast, Krisp and similar) output exact zeros between
//! words, and the mic has already proved it works. A stopped stream still
//! warns. The warning shows at most once per take, clears as soon as the
//! problem it names is gone, and never touches the recording itself.
//!
//! Known gap: a gated virtual mic outputs exact zeros until the first word,
//! so someone who stays silent for the first 5 s on one sees the notice until
//! they speak.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

/// Peak at or below which a chunk counts as digital silence (-120 dBFS).
pub const DIGITAL_SILENCE_PEAK: f32 = 1.0e-6;
/// Continuous digital silence before warning, matching the macOS delay.
pub const SILENCE_WARN_MS: u64 = 5_000;
/// Time without a single delivered mic buffer before warning.
pub const NO_CHUNKS_WARN_MS: u64 = 5_000;
/// Longest step one tick may count. The first tick after the countdown can
/// cover all of capture setup; it must not bring the warning forward.
pub const MAX_STEP_MS: u64 = 500;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MicSilenceReason {
    DigitalSilence,
    NoChunks,
}

impl MicSilenceReason {
    /// Slug for diagnostics and analytics.
    pub fn as_str(self) -> &'static str {
        match self {
            MicSilenceReason::DigitalSilence => "digital_silence",
            MicSilenceReason::NoChunks => "no_chunks",
        }
    }
}

/// Lock-free hand-off from the capture pump to the once-a-tick watcher. The
/// pump only does atomic writes per chunk, so it can never block on it.
#[derive(Default)]
pub struct MicActivity {
    chunks: AtomicU64,
    peak_bits: AtomicU64,
    pump_live: AtomicBool,
}

impl MicActivity {
    pub fn new() -> Self {
        Self::default()
    }

    /// Called by the pump for each mic chunk it accepts.
    pub fn observe(&self, samples: &[f32]) {
        let peak = chunk_peak(samples);
        self.chunks.fetch_add(1, Ordering::AcqRel);
        // f32 bit patterns of non-negative values order like the values, so
        // fetch_max on the bits keeps the loudest peak since the last take.
        self.peak_bits
            .fetch_max(u64::from(peak.to_bits()), Ordering::AcqRel);
    }

    /// The pump has its timeline anchor and is draining mic chunks.
    pub fn mark_pump_live(&self) {
        self.pump_live.store(true, Ordering::Release);
    }

    pub fn pump_live(&self) -> bool {
        self.pump_live.load(Ordering::Acquire)
    }

    /// Chunks and the loudest peak since the previous call, then resets.
    /// The two swaps are not one atomic step, so a chunk can land its count
    /// in one tick and its peak in the next; [`MicSilenceWatch::tick`] treats
    /// any peak above the floor as signal whatever the count says.
    pub fn take(&self) -> (u64, f32) {
        let chunks = self.chunks.swap(0, Ordering::AcqRel);
        let peak = f32::from_bits(self.peak_bits.swap(0, Ordering::AcqRel) as u32);
        (chunks, peak)
    }
}

/// Absolute peak of a buffer, ignoring NaN and infinities. Never negative.
pub fn chunk_peak(samples: &[f32]) -> f32 {
    samples
        .iter()
        .copied()
        .filter(|s| s.is_finite())
        .map(f32::abs)
        .fold(0.0, f32::max)
}

/// What one ticker step observed.
#[derive(Clone, Copy, Debug)]
pub struct MicTick {
    /// Recording time since the previous tick (pause-aware).
    pub elapsed_delta_ms: u64,
    /// Recording (not paused) with the mic unmuted in Clipy, at both ends of
    /// the step, and the pump draining chunks.
    pub active: bool,
    /// Buffers the device callback delivered during the step.
    pub delivered: u64,
    /// Mic chunks the pump measured during the step.
    pub chunks: u64,
    /// Loudest absolute sample in those chunks.
    pub peak: f32,
}

/// Per-take summary for diagnostics and telemetry.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct MicSilenceSummary {
    pub warned: bool,
    pub reason: Option<MicSilenceReason>,
    /// Active time counted toward a warning: time with no delivered buffers,
    /// plus digital silence before the mic first sent real audio. Digital
    /// silence after that is not counted (see the module docs).
    pub silent_ms: u64,
    /// The mic sent real audio in an active step.
    pub signal_seen: bool,
}

#[derive(Debug, Default)]
pub struct MicSilenceWatch {
    signal_seen: bool,
    silent_streak_ms: u64,
    no_chunk_streak_ms: u64,
    showing: Option<MicSilenceReason>,
    summary: MicSilenceSummary,
}

impl MicSilenceWatch {
    pub fn new() -> Self {
        Self::default()
    }

    /// Advances one step. Returns the warning to show now, if any.
    pub fn tick(&mut self, t: MicTick) -> Option<MicSilenceReason> {
        let step = t.elapsed_delta_ms.min(MAX_STEP_MS);
        if t.peak > DIGITAL_SILENCE_PEAK {
            // Real audio, even at the edge of a mute or pause: the mic works.
            self.signal_seen = true;
            self.summary.signal_seen = true;
            self.silent_streak_ms = 0;
            self.no_chunk_streak_ms = 0;
            self.showing = None;
            return None;
        }
        if !t.active {
            self.silent_streak_ms = 0;
            self.no_chunk_streak_ms = 0;
            return self.showing;
        }
        if t.delivered == 0 {
            self.no_chunk_streak_ms = self.no_chunk_streak_ms.saturating_add(step);
            self.silent_streak_ms = 0;
            self.summary.silent_ms = self.summary.silent_ms.saturating_add(step);
            if self.no_chunk_streak_ms >= NO_CHUNKS_WARN_MS {
                self.raise(MicSilenceReason::NoChunks);
            }
            return self.showing;
        }
        // The device is delivering again: a "stopped sending" notice is no
        // longer true, whatever the samples hold.
        self.no_chunk_streak_ms = 0;
        if self.showing == Some(MicSilenceReason::NoChunks) {
            self.showing = None;
        }
        if t.chunks == 0 {
            // Delivered but not yet measured (pump behind): no evidence either
            // way, so neither count nor reset.
            return self.showing;
        }
        if self.signal_seen {
            self.silent_streak_ms = 0;
        } else {
            self.silent_streak_ms = self.silent_streak_ms.saturating_add(step);
            self.summary.silent_ms = self.summary.silent_ms.saturating_add(step);
            if self.silent_streak_ms >= SILENCE_WARN_MS {
                self.raise(MicSilenceReason::DigitalSilence);
            }
        }
        self.showing
    }

    /// Once per take: a warning that was shown and cleared never returns.
    fn raise(&mut self, reason: MicSilenceReason) {
        if self.showing.is_none() && !self.summary.warned {
            self.showing = Some(reason);
            self.summary.warned = true;
            self.summary.reason = Some(reason);
        }
    }

    pub fn summary(&self) -> MicSilenceSummary {
        self.summary
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const TICK: u64 = 250;

    fn step(w: &mut MicSilenceWatch, active: bool, delivered: u64, chunks: u64, peak: f32) -> Option<MicSilenceReason> {
        w.tick(MicTick { elapsed_delta_ms: TICK, active, delivered, chunks, peak })
    }

    fn run(w: &mut MicSilenceWatch, ticks: u64, active: bool, chunks: u64, peak: f32) -> Option<MicSilenceReason> {
        let mut last = None;
        for _ in 0..ticks {
            last = step(w, active, chunks, chunks, peak);
        }
        last
    }

    #[test]
    fn quiet_room_noise_never_warns() {
        let mut w = MicSilenceWatch::new();
        // -100 dBFS: a 24-bit interface at minimum gain in a quiet room.
        assert_eq!(run(&mut w, 400, true, 25, 1.0e-5), None);
        // One 16-bit step.
        assert_eq!(run(&mut w, 400, true, 25, 3.1e-5), None);
        assert!(!w.summary().warned);
        assert_eq!(w.summary().silent_ms, 0);
    }

    #[test]
    fn digital_silence_warns_after_five_seconds() {
        let mut w = MicSilenceWatch::new();
        assert_eq!(run(&mut w, 19, true, 25, 0.0), None);
        assert_eq!(run(&mut w, 1, true, 25, 0.0), Some(MicSilenceReason::DigitalSilence));
        let s = w.summary();
        assert!(s.warned);
        assert_eq!(s.reason, Some(MicSilenceReason::DigitalSilence));
        assert_eq!(s.silent_ms, 5_000);
    }

    #[test]
    fn a_long_first_step_does_not_bring_the_warning_forward() {
        let mut w = MicSilenceWatch::new();
        // Capture setup made the first step 3 s long.
        let first = w.tick(MicTick { elapsed_delta_ms: 3_000, active: true, delivered: 25, chunks: 25, peak: 0.0 });
        assert_eq!(first, None);
        assert_eq!(run(&mut w, 17, true, 25, 0.0), None);
        assert_eq!(run(&mut w, 1, true, 25, 0.0), Some(MicSilenceReason::DigitalSilence));
    }

    #[test]
    fn mute_or_pause_resets_and_never_warns() {
        let mut w = MicSilenceWatch::new();
        for _ in 0..10 {
            assert_eq!(run(&mut w, 16, true, 25, 0.0), None);
            // Muted in Clipy (pump zeroes the samples) or paused (no chunks).
            assert_eq!(run(&mut w, 40, false, 25, 0.0), None);
            assert_eq!(run(&mut w, 40, false, 0, 0.0), None);
        }
        assert!(!w.summary().warned);
    }

    #[test]
    fn no_chunks_warns_and_recovers() {
        let mut w = MicSilenceWatch::new();
        assert_eq!(run(&mut w, 19, true, 0, 0.0), None);
        assert_eq!(run(&mut w, 1, true, 0, 0.0), Some(MicSilenceReason::NoChunks));
        assert_eq!(run(&mut w, 1, true, 25, 0.2), None);
        assert_eq!(w.summary().reason, Some(MicSilenceReason::NoChunks));
    }

    #[test]
    fn no_chunks_notice_clears_when_buffers_return_even_silent() {
        let mut w = MicSilenceWatch::new();
        run(&mut w, 4, true, 25, 0.3);
        assert_eq!(run(&mut w, 20, true, 0, 0.0), Some(MicSilenceReason::NoChunks));
        // The headset is back and gated (zeros): the stream works again.
        assert_eq!(run(&mut w, 1, true, 25, 0.0), None);
        assert_eq!(run(&mut w, 100, true, 25, 0.0), None);
    }

    #[test]
    fn a_busy_pump_is_not_a_dead_mic() {
        let mut w = MicSilenceWatch::new();
        // The device keeps delivering; the pump measures nothing for 10 s.
        for _ in 0..40 {
            assert_eq!(step(&mut w, true, 25, 0, 0.0), None);
        }
        assert!(!w.summary().warned);
    }

    #[test]
    fn a_peak_without_a_count_still_counts_as_signal() {
        let mut w = MicSilenceWatch::new();
        run(&mut w, 19, true, 25, 0.0);
        // The chunk's count went into the last tick, its peak into this one.
        assert_eq!(step(&mut w, true, 25, 0, 0.4), None);
        assert!(w.summary().signal_seen);
        assert_eq!(run(&mut w, 40, true, 25, 0.0), None);
    }

    #[test]
    fn warning_clears_on_real_audio_and_shows_once_per_take() {
        let mut w = MicSilenceWatch::new();
        assert_eq!(run(&mut w, 20, true, 25, 0.0), Some(MicSilenceReason::DigitalSilence));
        assert_eq!(run(&mut w, 8, true, 25, 0.0), Some(MicSilenceReason::DigitalSilence));
        assert_eq!(run(&mut w, 1, true, 25, 0.05), None);
        assert_eq!(run(&mut w, 100, true, 0, 0.0), None);
        assert!(w.summary().warned);
    }

    #[test]
    fn gated_mic_after_speech_does_not_warn_on_zeros() {
        let mut w = MicSilenceWatch::new();
        run(&mut w, 4, true, 25, 0.3);
        assert_eq!(run(&mut w, 400, true, 25, 0.0), None);
        assert!(!w.summary().warned);
        assert!(w.summary().signal_seen);
        assert_eq!(run(&mut w, 20, true, 0, 0.0), Some(MicSilenceReason::NoChunks));
    }

    #[test]
    fn interrupted_silence_does_not_accumulate() {
        let mut w = MicSilenceWatch::new();
        for _ in 0..20 {
            assert_eq!(run(&mut w, 12, true, 25, 0.0), None);
            assert_eq!(run(&mut w, 12, true, 0, 0.0), None);
        }
    }

    #[test]
    fn activity_hand_off_reports_and_resets() {
        let a = MicActivity::new();
        a.observe(&[0.0, -0.25, 0.1]);
        a.observe(&[f32::NAN, 0.05]);
        let (chunks, peak) = a.take();
        assert_eq!(chunks, 2);
        assert!((peak - 0.25).abs() < 1e-6);
        assert_eq!(a.take(), (0, 0.0));
    }
}
