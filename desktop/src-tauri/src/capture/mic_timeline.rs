//! Places microphone chunks on the shared 48 kHz recording timeline.
//!
//! Each chunk carries the HAL capture time of its first sample (already
//! latency-compensated in `capture::audio`). This module turns that time into
//! a sample position on the timeline that video and system audio share. The
//! 70 ms smoothing window and the gap and re-anchor rules follow Cap's
//! `SourceClockState` (`crates/timestamp/src/master_clock.rs`):
//!
//! - a chunk within 70 ms of the contiguous cadence is placed on the cadence,
//!   so timestamp jitter never trims or pads audio. Bluetooth mics run an
//!   80 ms buffer with timestamps that wander by several milliseconds, and
//!   every sample of theirs is kept back to back;
//! - jitter averages out but clock drift does not: a mic whose sample clock
//!   runs fast or slow moves steadily away from real time. A one-second
//!   moving average of the offset filters the jitter out, and only once that
//!   average passes `DRIFT_TOLERANCE_SAMPLES` (20 ms) is it corrected, by
//!   dropping or padding at most 0.5% of a chunk, in practice one sample at a
//!   time. Without this the voice slid later for the rest of a recording;
//!   correcting on the raw timestamp instead cut a few
//!   milliseconds out of a Bluetooth voice on every early timestamp.
//!   Cap's mixer never trims in this window either; it only
//!   fills real gaps with silence;
//! - a larger forward step is a real gap and is kept (the mixer zero-fills);
//! - a larger backward step re-anchors to the device clock; audio that would
//!   overlap what is already placed is trimmed;
//! - a device reopen (new generation) starts a fresh cadence but may not
//!   overlap what the previous device already placed: the overlap is trimmed.

use crate::capture::audio::MicStreamGeneration;
use crate::encoder::SAMPLE_RATE;

#[derive(Default)]
pub(crate) struct MicTimeline {
    generation: Option<MicStreamGeneration>,
    next_expected_pts: Option<i64>,
    /// Moving average of how far the cadence runs ahead of the device's own
    /// capture time, in samples. Positive means the mic clock is fast.
    drift_offset: f64,
    /// Exclusive end of everything placed so far, across generations.
    pub(crate) placed_end_pts: i64,
    /// Chunks whose position was moved onto the cadence.
    pub(crate) snaps: u64,
    /// Chunks that had samples dropped to correct a fast mic clock.
    pub(crate) drift_trims: u64,
    /// Chunks placed after a short pad to correct a slow mic clock.
    pub(crate) drift_pads: u64,
    pub(crate) gaps: u64,
    /// Overlap cut from a reopened device's first chunk, or from the very
    /// first chunk when it predates the video anchor.
    pub(crate) trims: u64,
    /// Backward steps larger than the smoothing window, re-anchored to the
    /// device clock.
    pub(crate) backward_resyncs: u64,
    pub(crate) stale_drops: u64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct MicPlacement {
    pub(crate) pts: i64,
    /// Leading samples of the chunk that must not be emitted.
    pub(crate) skip: usize,
}

impl MicTimeline {
    /// Same threshold as Cap's `TS_SMOOTHING_THRESHOLD_NS`.
    pub(crate) const SMOOTHING_SAMPLES: i64 = SAMPLE_RATE as i64 * 70 / 1_000;
    /// How far the averaged offset may wander before it is corrected. Well
    /// under what viewers notice as lip-sync error, well over the residue of
    /// averaged timestamp jitter.
    pub(crate) const DRIFT_TOLERANCE_SAMPLES: f64 = SAMPLE_RATE as f64 * 20.0 / 1_000.0;
    /// Time constant of the offset average: one second of audio.
    const DRIFT_TIME_CONSTANT_SAMPLES: f64 = SAMPLE_RATE as f64;
    /// Largest correction applied to one chunk, as a fraction of its length.
    /// Ample for any real clock (it keeps up with 5,000 ppm) while keeping
    /// each correction to a sample or two.
    const MAX_CORRECTION_PER_CHUNK: f64 = 1.0 / 200.0;

    pub(crate) fn place(
        &mut self,
        raw_pts: i64,
        samples: usize,
        generation: MicStreamGeneration,
    ) -> Option<MicPlacement> {
        if samples == 0 {
            return None;
        }
        if self.generation != Some(generation) {
            self.generation = Some(generation);
            self.next_expected_pts = None;
            self.drift_offset = 0.0;
        }

        let duration = i64::try_from(samples).unwrap_or(i64::MAX);
        let mut pts = raw_pts;
        let mut skip = 0usize;
        if let Some(expected) = self.next_expected_pts {
            let diff = raw_pts - expected;
            if diff.abs() < Self::SMOOTHING_SAMPLES {
                pts = expected;
                if diff != 0 {
                    self.snaps += 1;
                }
                let lead = (expected - raw_pts) as f64;
                let alpha = duration as f64 / (duration as f64 + Self::DRIFT_TIME_CONSTANT_SAMPLES);
                self.drift_offset += alpha * (lead - self.drift_offset);
                let step =
                    ((duration as f64 * Self::MAX_CORRECTION_PER_CHUNK).floor() as i64).max(1);
                if self.drift_offset > Self::DRIFT_TOLERANCE_SAMPLES {
                    // Fast clock: drop a little from the head of this chunk.
                    let trim = ((self.drift_offset - Self::DRIFT_TOLERANCE_SAMPLES).ceil() as i64)
                        .min(step)
                        .min(duration - 1);
                    if trim > 0 {
                        skip = trim as usize;
                        self.drift_offset -= trim as f64;
                        self.drift_trims += 1;
                    }
                } else if self.drift_offset < -Self::DRIFT_TOLERANCE_SAMPLES {
                    // Slow clock: leave a short pad for the mixer to fill.
                    let pad = ((-Self::DRIFT_TOLERANCE_SAMPLES - self.drift_offset).ceil() as i64)
                        .min(step);
                    pts = expected + pad;
                    self.drift_offset += pad as f64;
                    self.drift_pads += 1;
                }
            } else if diff > 0 {
                self.gaps += 1;
                self.drift_offset = 0.0;
            } else {
                self.backward_resyncs += 1;
                self.drift_offset = 0.0;
                if self.backward_resyncs == 1 || self.backward_resyncs.is_power_of_two() {
                    tracing::warn!(
                        raw_pts,
                        expected,
                        backward_resyncs = self.backward_resyncs,
                        "mic capture clock stepped backwards; re-anchoring to it"
                    );
                }
            }
        }

        // The cadence follows this chunk's position whether or not any of it
        // is emitted, as in Cap (`next_expected = ts + duration`), so after a
        // re-anchor the following chunks are judged against the device clock.
        let starts_cadence = self.next_expected_pts.is_none();
        self.next_expected_pts = Some(pts.saturating_add(duration - skip as i64));

        // Never overlap audio already placed, by this device or the one it
        // replaced. Also drops the part of a chunk captured before the video
        // anchor (negative pts). A drift trim never reaches here: it places
        // the chunk exactly at the end of what is already placed.
        if pts < self.placed_end_pts {
            let overlap = self.placed_end_pts - pts;
            if overlap + skip as i64 >= duration {
                self.stale_drops += 1;
                return None;
            }
            skip += overlap as usize;
            pts = self.placed_end_pts;
            if starts_cadence {
                self.trims += 1;
            }
        }

        let end = pts + (samples - skip) as i64;
        self.placed_end_pts = end;
        Some(MicPlacement { pts, skip })
    }
}

#[cfg(test)]
mod tests {
    use super::{MicStreamGeneration, MicTimeline};

    const CHUNK: usize = 480; // 10 ms at 48 kHz

    #[test]
    fn snaps_jitter_without_ratcheting_late() {
        let generation = MicStreamGeneration::initial();
        let mut timeline = MicTimeline::default();
        // Late by 30 samples, then early by 30: both snap onto the cadence
        // and the timeline ends exactly where real time is.
        let raw = [0i64, 480 + 30, 960 - 30, 1_440, 1_920 + 5];
        let mut placements = Vec::new();
        for raw_pts in raw {
            placements.push(timeline.place(raw_pts, CHUNK, generation).expect("placed"));
        }
        let pts: Vec<i64> = placements.iter().map(|p| p.pts).collect();
        assert_eq!(pts, vec![0, 480, 960, 1_440, 1_920]);
        assert!(placements.iter().all(|p| p.skip == 0));
        assert_eq!(timeline.placed_end_pts, 2_400);
        assert_eq!(timeline.snaps, 3);
        assert_eq!(timeline.drift_trims, 0);
        assert_eq!(timeline.drift_pads, 0);
        assert_eq!(timeline.gaps, 0);
    }

    /// Margin over the tolerance a test allows: the average lags the real
    /// offset by about one chunk of drift per time constant.
    const DRIFT_MARGIN: f64 = 96.0; // 2 ms

    /// A mic whose sample clock runs fast delivers each 480-sample chunk in a
    /// little less than 10 ms of real time. Snapping every chunk to the
    /// cadence placed the audio later and later for the whole recording.
    /// Now the offset is held at the 20 ms tolerance by one-sample trims.
    #[test]
    fn fast_mic_clock_is_held_within_tolerance() {
        let generation = MicStreamGeneration::initial();
        let mut timeline = MicTimeline::default();
        // 1,000 ppm fast, far worse than real hardware, so the test sees many
        // corrections: each chunk spans 479.52 samples of real time.
        let chunks = 30_000; // five minutes
        let mut emitted = 0i64;
        let limit = MicTimeline::DRIFT_TOLERANCE_SAMPLES + DRIFT_MARGIN;
        for index in 0..chunks {
            let raw_pts = (index as f64 * 480.0 * 0.999).round() as i64;
            let placement = timeline.place(raw_pts, CHUNK, generation).expect("placed");
            // The first emitted sample is sample `skip` of the chunk, which
            // was captured at `raw_pts + skip`.
            let lead = (placement.pts - (raw_pts + placement.skip as i64)) as f64;
            assert!(
                (0.0..=limit).contains(&lead),
                "chunk {index}: placed {lead} samples after its capture time"
            );
            assert!(
                placement.skip <= 2,
                "chunk {index}: trimmed {} samples",
                placement.skip
            );
            emitted += (CHUNK - placement.skip) as i64;
            assert_eq!(
                timeline.placed_end_pts, emitted,
                "timeline must stay contiguous"
            );
        }
        assert!(timeline.drift_trims > 0);
        assert_eq!(timeline.drift_pads, 0);
        assert_eq!(timeline.gaps, 0);
        assert_eq!(timeline.backward_resyncs, 0);
        assert_eq!(timeline.stale_drops, 0);
    }

    /// A slow clock is the mirror image: tiny pads keep it within the same
    /// tolerance. It used to drift early until the gap passed 70 ms and then
    /// leave a 70 ms hole of silence.
    #[test]
    fn slow_mic_clock_is_held_within_tolerance() {
        let generation = MicStreamGeneration::initial();
        let mut timeline = MicTimeline::default();
        let limit = MicTimeline::DRIFT_TOLERANCE_SAMPLES + DRIFT_MARGIN;
        let mut previous_end = 0i64;
        for index in 0..30_000i64 {
            let raw_pts = (index as f64 * 480.0 * 1.001).round() as i64;
            let placement = timeline.place(raw_pts, CHUNK, generation).expect("placed");
            assert_eq!(placement.skip, 0);
            let early = (raw_pts - placement.pts) as f64;
            assert!(
                (0.0..=limit).contains(&early),
                "chunk {index}: placed {early} samples before its capture time"
            );
            let pad = placement.pts - previous_end;
            assert!(
                (0..=2).contains(&pad),
                "chunk {index}: padded {pad} samples"
            );
            previous_end = timeline.placed_end_pts;
        }
        assert!(timeline.drift_pads > 0);
        assert_eq!(timeline.drift_trims, 0);
        assert_eq!(timeline.gaps, 0);
    }

    #[test]
    fn keeps_real_gaps_and_reanchors_on_backward_clock_steps() {
        let generation = MicStreamGeneration::initial();
        let mut timeline = MicTimeline::default();
        timeline.place(0, CHUNK, generation).expect("first");
        // A dropout of 200 ms is a real gap: the mixer must zero-fill it.
        let gap = timeline
            .place(480 + 9_600, CHUNK, generation)
            .expect("after gap");
        assert_eq!(gap.pts, 480 + 9_600);
        assert_eq!(timeline.gaps, 1);
        // A ~100 ms backward step re-anchors to the device clock. The chunks
        // that follow claim a time that is already filled, so they are dropped
        // until the device clock reaches the end of what was placed, and the
        // chunk that straddles it is trimmed. Nothing is shifted later to make
        // room for them, which is what used to leave the mic late for good.
        let end = timeline.placed_end_pts;
        let restart = end - 4_900;
        for k in 0..10 {
            let raw_pts = restart + k * CHUNK as i64;
            assert!(
                timeline.place(raw_pts, CHUNK, generation).is_none(),
                "chunk {k}"
            );
        }
        assert_eq!(timeline.backward_resyncs, 1);
        assert_eq!(timeline.stale_drops, 10);
        let straddling = timeline
            .place(restart + 10 * CHUNK as i64, CHUNK, generation)
            .expect("partly new");
        assert_eq!(straddling.pts, end);
        assert_eq!(straddling.skip, 100);
        assert_eq!(timeline.placed_end_pts, end + CHUNK as i64 - 100);
        // And the chunk after that is back on the device clock exactly.
        let next = timeline
            .place(restart + 11 * CHUNK as i64, CHUNK, generation)
            .expect("on cadence");
        assert_eq!(next.pts, restart + 11 * CHUNK as i64);
        assert_eq!(next.skip, 0);
    }

    #[test]
    fn trims_overlap_from_a_reopened_device() {
        let first = MicStreamGeneration::initial();
        let next = first.next();
        let mut timeline = MicTimeline::default();
        timeline.place(0, CHUNK, first).expect("old device");
        // The new device started while the old stream was still draining:
        // its first chunk overlaps the last 100 samples already placed.
        let overlap = timeline.place(380, CHUNK, next).expect("new device");
        assert_eq!(overlap.pts, 480);
        assert_eq!(overlap.skip, 100);
        assert_eq!(timeline.trims, 1);
        // A reopened device whose first chunk is entirely behind what was
        // placed is stale.
        assert!(timeline.place(100, 200, next.next()).is_none());
        assert_eq!(timeline.stale_drops, 1);
        // Audio captured before the video anchor is cut, not shifted.
        let mut fresh = MicTimeline::default();
        let head = fresh.place(-100, CHUNK, first).expect("pre-anchor");
        assert_eq!(head.pts, 0);
        assert_eq!(head.skip, 100);
    }

    /// Wireless mics run an 80 ms capture buffer (`input_latency.rs`), so each
    /// chunk is longer than the 70 ms smoothing window, and their HAL
    /// timestamps jitter by several milliseconds. None of that is drift: every
    /// sample must be kept, back to back, with no trimming and no padding.
    /// Trimming here cut a few milliseconds out of the voice on every early
    /// timestamp, which is what a choppy Bluetooth mic sounds like.
    #[test]
    fn jittery_long_chunks_keep_every_sample() {
        let generation = MicStreamGeneration::initial();
        let mut timeline = MicTimeline::default();
        let chunk = 3_840usize; // 80 ms at 48 kHz
        let mut state = 0x2545_F491_4F6C_DD1Du64;
        let mut emitted = 0i64;
        for index in 0..750i64 {
            // Deterministic jitter in [-480, +480] samples (±10 ms).
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            let jitter = if index == 0 {
                0
            } else {
                (state % 961) as i64 - 480
            };
            let raw_pts = index * chunk as i64 + jitter;
            let placement = timeline.place(raw_pts, chunk, generation).expect("placed");
            assert_eq!(placement.skip, 0, "chunk {index} lost audio to jitter");
            assert_eq!(placement.pts, emitted, "chunk {index} left a hole");
            emitted += chunk as i64;
        }
        assert_eq!(timeline.drift_trims, 0);
        assert_eq!(timeline.drift_pads, 0);
        assert_eq!(timeline.gaps, 0);
        assert_eq!(timeline.backward_resyncs, 0);
    }

    /// Real Bluetooth hardware does both at once: long, jittery chunks from a
    /// clock that also drifts. The jitter must cost nothing and the drift
    /// must still be held, with each correction a tiny fraction of a chunk.
    #[test]
    fn drifting_jittery_long_chunks_are_corrected_gently() {
        let generation = MicStreamGeneration::initial();
        let mut timeline = MicTimeline::default();
        let chunk = 3_840usize; // 80 ms
        let mut state = 0x9E37_79B9_7F4A_7C15u64;
        let mut true_offset_max = 0f64;
        for index in 0..7_500i64 {
            // ten minutes, 200 ppm fast, ±10 ms jitter
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            let jitter = if index == 0 {
                0
            } else {
                (state % 961) as i64 - 480
            };
            let true_pts = index as f64 * chunk as f64 * (1.0 - 200e-6);
            let raw_pts = true_pts.round() as i64 + jitter;
            let placement = timeline.place(raw_pts, chunk, generation).expect("placed");
            assert!(
                placement.skip <= chunk / 200,
                "chunk {index}: trimmed {}",
                placement.skip
            );
            let offset = (placement.pts + placement.skip as i64) as f64 - true_pts;
            true_offset_max = true_offset_max.max(offset.abs());
        }
        // Measured against the clock without jitter, the audio stays within
        // the tolerance plus what the average has not caught up on yet.
        assert!(
            true_offset_max <= MicTimeline::DRIFT_TOLERANCE_SAMPLES + 240.0,
            "audio wandered {true_offset_max} samples from real time"
        );
        assert!(timeline.drift_trims > 0);
        assert_eq!(timeline.gaps, 0);
        assert_eq!(timeline.backward_resyncs, 0);
    }

    #[test]
    fn empty_chunks_are_ignored() {
        let mut timeline = MicTimeline::default();
        assert!(timeline
            .place(0, 0, MicStreamGeneration::initial())
            .is_none());
        assert_eq!(timeline.placed_end_pts, 0);
    }
}
