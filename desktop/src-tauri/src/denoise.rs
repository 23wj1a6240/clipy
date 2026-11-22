//! Stationary-noise suppression for the microphone lane.
//!
//! This is the DSP behind the recorder's "Reduce background noise" switch.
//! It is deliberately narrow: a short-time spectral attenuator that learns the
//! steady part of the room — fan, air conditioning, street hum, laptop
//! coil whine — and turns that part down, while leaving everything that is not
//! steady alone.
//!
//! What it is NOT, on purpose (each of these would move the microphone's
//! level, so all three stay out):
//!
//! * not a speech gate — nothing here decides "this block is speech" and mutes
//!   the rest. Every bin of every frame is attenuated by its own continuous
//!   gain, so the pauses between phrases keep their quiet room tone instead of
//!   dropping into digital silence.
//! * not RNNoise — no neural model, no new dependency, no per-frame inference.
//! * not automatic gain control — `GAIN_CEILING` is 1.0 and the code has no
//!   path that can produce a gain above it. This filter can only ever make the
//!   microphone quieter and can never drift its level upwards.
//!
//! It is therefore safe to run downstream of a mix whose loudness has already
//! been settled: the loudest thing in the recording, a voice, is what the
//! filter leaves closest to untouched.
//!
//! ## Shape
//!
//! Weighted overlap-add. Frames of `FRAME` samples are taken every `HOP`
//! samples, windowed with a square-root Hann window, transformed, attenuated
//! per bin, transformed back, windowed again and overlap-added. Analysis times
//! synthesis is a periodic Hann window, which sums to exactly 1.0 at 50%
//! overlap, so with every gain pinned at 1.0 the output is the input again.
//! `passes_a_clean_tone_through` holds that property down.
//!
//! The per-bin gain is a Wiener gain driven by a decision-directed a priori
//! SNR (Ephraim and Malah, 1984), over a noise estimate tracked by a decaying
//! per-bin minimum (Martin's minimum statistics, in its simple form). Both are
//! standard, both are cheap: one 1024-point FFT per 512 samples is about 94
//! transforms a second for one mono lane.
//!
//! ## Latency
//!
//! `LATENCY_SAMPLES` (1024 at 48 kHz, ~21 ms) of delay is added to the
//! microphone, and `process` is length-preserving, so the recording's sample
//! count and duration do not change — the microphone simply sits ~21 ms later
//! against the picture than it otherwise would. That is inside the ±70 ms the
//! mic timeline already snaps as jitter, and well under the threshold where a
//! late voice reads as out of sync. The last ~21 ms of microphone audio, after
//! the user has already pressed stop, stays in the filter and is not written.

use std::collections::VecDeque;
use std::f32::consts::PI;

/// Analysis window length. 1024 at 48 kHz is ~21 ms, which resolves a male
/// fundamental and its harmonics well enough that steady noise between them
/// can be taken out without chewing the voice.
pub const FRAME: usize = 1024;
/// Hop between frames. Exactly half the window: required for the square-root
/// Hann pair to reconstruct.
pub const HOP: usize = FRAME / 2;
/// Delay `process` adds to the lane, in samples. One hop is inherent to the
/// overlap-add; the second is the priming that lets `process` return exactly
/// as many samples as it was handed, for any block length.
pub const LATENCY_SAMPLES: usize = HOP * 2;

/// How much of the previous frame's smoothed periodogram is kept. Higher means
/// a steadier noise estimate and a slower reaction to a genuinely new room.
const POWER_SMOOTHING: f32 = 0.85;
/// Frames per sub-window of the minimum tracker, ~171 ms.
const MINIMUM_SUBWINDOW: usize = 16;
/// Sub-windows kept. The noise estimate is therefore the smallest smoothed
/// power seen in roughly the last 1.4 seconds.
///
/// The length is the whole design of this tracker and is why it is a window
/// rather than a decaying minimum that falls to any new low immediately. A
/// decaying minimum takes a held vowel to be the new floor within a couple of
/// frames and attenuates the voice itself — the first version of this module
/// did exactly that, and `keeps_speech_bursts_while_cutting_the_gaps` is the
/// test that caught it. A window only lowers the floor once every frame in it
/// is quiet, which speech is not. The cost is the known limit of every
/// minimum-statistics denoiser: a single unchanging sound held for longer than
/// the window does eventually become "the room". `GAIN_FLOOR` bounds what that
/// costs to -14 dB.
const MINIMUM_WINDOWS: usize = 8;
/// Minimum statistics underestimates the noise floor, because a minimum is not
/// a mean. The usual correction is a small fixed multiplier.
const MINIMUM_BIAS: f32 = 1.8;
/// Weight on the previous frame's estimate in the decision-directed a priori
/// SNR. This is the term that suppresses musical noise, and 0.92 is within the
/// 0.9-0.98 range the literature settles on.
const DECISION_DIRECTED: f32 = 0.92;
/// The quietest a bin may be made: -14 dB. A floor is what keeps the result
/// sounding like a quieter room rather than like a bin-by-bin robot, and it is
/// why this is an attenuator and not a gate.
const GAIN_FLOOR: f32 = 0.2;
/// The loudest a bin may be made. One. Not a knob.
const GAIN_CEILING: f32 = 1.0;
/// Keeps divisions finite when a bin is exactly zero.
const EPSILON: f32 = 1e-12;

/// A real-input FFT, done as a complex FFT of the same size. Radix-2,
/// iterative, twiddles precomputed once.
struct Fft {
    twiddles: Vec<(f32, f32)>,
    reversed: Vec<usize>,
}

impl Fft {
    fn new(size: usize) -> Self {
        debug_assert!(size.is_power_of_two());
        let twiddles = (0..size / 2)
            .map(|k| {
                let angle = -2.0 * PI * k as f32 / size as f32;
                (angle.cos(), angle.sin())
            })
            .collect();
        let bits = size.trailing_zeros();
        let reversed = (0..size)
            .map(|index| index.reverse_bits() >> (usize::BITS - bits))
            .collect();
        Self { twiddles, reversed }
    }

    /// In-place forward transform. `inverse` conjugates the twiddles and
    /// scales by 1/N, so `transform(x, false)` then `transform(x, true)`
    /// returns `x`.
    fn transform(&self, re: &mut [f32], im: &mut [f32], inverse: bool) {
        let size = re.len();
        for index in 0..size {
            let target = self.reversed[index];
            if index < target {
                re.swap(index, target);
                im.swap(index, target);
            }
        }
        let mut length = 2;
        while length <= size {
            let step = size / length;
            for start in (0..size).step_by(length) {
                for offset in 0..length / 2 {
                    let (twiddle_re, twiddle_im) = self.twiddles[offset * step];
                    let twiddle_im = if inverse { -twiddle_im } else { twiddle_im };
                    let top = start + offset;
                    let bottom = top + length / 2;
                    let product_re = re[bottom] * twiddle_re - im[bottom] * twiddle_im;
                    let product_im = re[bottom] * twiddle_im + im[bottom] * twiddle_re;
                    re[bottom] = re[top] - product_re;
                    im[bottom] = im[top] - product_im;
                    re[top] += product_re;
                    im[top] += product_im;
                }
            }
            length <<= 1;
        }
        if inverse {
            let scale = 1.0 / size as f32;
            for (r, i) in re.iter_mut().zip(im.iter_mut()) {
                *r *= scale;
                *i *= scale;
            }
        }
    }
}

/// One microphone lane's noise suppressor. Single channel, 48 kHz, streaming.
///
/// One instance per recording. `reset` is what a microphone change calls: a new
/// device is a new noise floor, and carrying the old one across would attenuate
/// the wrong frequencies until the estimate drifted over.
pub struct NoiseSuppressor {
    fft: Fft,
    window: Vec<f32>,
    /// The last `FRAME` input samples, oldest first.
    history: Vec<f32>,
    /// Input samples received since the last frame was taken.
    pending: usize,
    /// The half-frame of overlap-add tail still waiting for its partner.
    overlap: Vec<f32>,
    /// Finished output samples, primed so `process` never underruns.
    ready: VecDeque<f32>,
    smoothed: Vec<f32>,
    /// The running minimum of the sub-window being filled, per bin.
    minimum: Vec<f32>,
    /// The completed sub-window minima, oldest first: `MINIMUM_WINDOWS` rows
    /// of one value per bin.
    minimum_history: VecDeque<Vec<f32>>,
    /// Frames counted into the current sub-window.
    subwindow_frames: usize,
    previous_gain: Vec<f32>,
    previous_power: Vec<f32>,
    started: bool,
    /// Per-frame scratch, held so the audio path allocates nothing.
    scratch_re: Vec<f32>,
    scratch_im: Vec<f32>,
    gains: Vec<f32>,
    smoothed_gains: Vec<f32>,
}

impl Default for NoiseSuppressor {
    fn default() -> Self {
        Self::new()
    }
}

impl NoiseSuppressor {
    pub fn new() -> Self {
        let bins = FRAME / 2 + 1;
        // Square-root of a periodic Hann window. Applied on the way in and on
        // the way out, so the round trip is a Hann window, which is what sums
        // to unity at this hop.
        let window = (0..FRAME)
            .map(|n| (0.5 - 0.5 * (2.0 * PI * n as f32 / FRAME as f32).cos()).max(0.0).sqrt())
            .collect();
        Self {
            fft: Fft::new(FRAME),
            window,
            history: vec![0.0; FRAME],
            pending: 0,
            overlap: vec![0.0; HOP],
            ready: VecDeque::from(vec![0.0; LATENCY_SAMPLES - HOP]),
            smoothed: vec![0.0; bins],
            minimum: vec![0.0; bins],
            minimum_history: VecDeque::new(),
            subwindow_frames: 0,
            previous_gain: vec![1.0; bins],
            previous_power: vec![0.0; bins],
            started: false,
            scratch_re: vec![0.0; FRAME],
            scratch_im: vec![0.0; FRAME],
            gains: vec![1.0; bins],
            smoothed_gains: vec![1.0; bins],
        }
    }

    /// Forgets the learned noise floor and the overlap tail. Called when the
    /// microphone is reopened under a new generation.
    pub fn reset(&mut self) {
        *self = Self::new();
    }

    /// Drops the audio still inside the filter but keeps what it learned about
    /// the room. Called where the input stream is cut (a pause drops chunks),
    /// so the `LATENCY_SAMPLES` from before the cut are not played after it,
    /// without spending ~1.4 s relearning the noise floor.
    pub fn discard_buffered(&mut self) {
        self.history.fill(0.0);
        self.pending = 0;
        self.overlap.fill(0.0);
        self.ready.clear();
        self.ready.extend(std::iter::repeat_n(0.0, LATENCY_SAMPLES - HOP));
    }

    /// How many completed sub-windows the noise estimate is built from, out of
    /// `MINIMUM_WINDOWS`. Zero means it knows nothing about the room yet.
    ///
    /// The mixer's tests need this rather than a "has it started" flag: a reset
    /// is immediately followed by more audio, which marks the filter started
    /// again within the same call, so only the depth of the estimate can tell
    /// a filter that was just cleared from one that has been running.
    #[cfg(test)]
    pub fn windows_learned(&self) -> usize {
        self.minimum_history.len()
    }

    /// Filters one block in place. The block comes back the same length,
    /// carrying audio from `LATENCY_SAMPLES` earlier in the stream.
    ///
    /// Any non-finite input sample is read as silence. A NaN that reached the
    /// FFT would otherwise poison every bin of the frame and, through the
    /// overlap tail, the frame after it.
    pub fn process(&mut self, block: &mut [f32]) {
        for sample in block.iter() {
            // `history` holds the last FRAME samples with the newest hop being
            // filled at its tail, so the window shifts once per frame rather
            // than once per sample. Shifting per sample is FRAME times the
            // memory traffic and measured about a third of this module's total
            // cost, which is not worth paying in a recorder's hot path.
            self.history[FRAME - HOP + self.pending] =
                if sample.is_finite() { *sample } else { 0.0 };
            self.pending += 1;
            if self.pending == HOP {
                self.pending = 0;
                self.advance();
                self.history.copy_within(HOP.., 0);
            }
        }
        for sample in block.iter_mut() {
            *sample = self.ready.pop_front().unwrap_or(0.0);
        }
    }

    /// How much of the minimum tracker's window has actually been observed,
    /// from 0.0 at the first frame to 1.0 once it holds a full ~1.4 seconds.
    fn confidence(&self) -> f32 {
        self.minimum_history.len() as f32 / MINIMUM_WINDOWS as f32
    }

    /// Transforms the current history window, attenuates it and overlap-adds
    /// one hop of finished output.
    fn advance(&mut self) {
        let bins = FRAME / 2 + 1;
        // The scratch buffers live on the struct; `take` hands them over for
        // the duration so the estimator can borrow the rest of `self` freely,
        // and they go back at the end of the frame.
        let mut re = std::mem::take(&mut self.scratch_re);
        let mut im = std::mem::take(&mut self.scratch_im);
        let mut gains = std::mem::take(&mut self.gains);
        let mut smoothed_gains = std::mem::take(&mut self.smoothed_gains);
        for (index, (real, imaginary)) in re.iter_mut().zip(im.iter_mut()).enumerate() {
            *real = self.history[index] * self.window[index];
            *imaginary = 0.0;
        }
        self.fft.transform(&mut re, &mut im, false);

        for bin in 0..bins {
            let power = re[bin] * re[bin] + im[bin] * im[bin];
            self.smoothed[bin] = if self.started {
                POWER_SMOOTHING * self.smoothed[bin] + (1.0 - POWER_SMOOTHING) * power
            } else {
                power
            };
            let smoothed = self.smoothed[bin];
            self.minimum[bin] = if self.subwindow_frames == 0 {
                smoothed
            } else {
                self.minimum[bin].min(smoothed)
            };
            // The floor is the smallest smoothed power across the completed
            // sub-windows and the one being filled — a minimum over the whole
            // tracker window, not over this frame alone.
            let tracked = self
                .minimum_history
                .iter()
                .map(|window| window[bin])
                .fold(self.minimum[bin], f32::min);
            let noise = tracked * MINIMUM_BIAS + EPSILON;

            let posterior = power / noise;
            let carried = self.previous_gain[bin] * self.previous_gain[bin] * self.previous_power[bin] / noise;
            let prior = DECISION_DIRECTED * carried
                + (1.0 - DECISION_DIRECTED) * (posterior - 1.0).max(0.0);
            let gain = (prior / (1.0 + prior)).clamp(GAIN_FLOOR, GAIN_CEILING);
            let gain = if gain.is_finite() { gain } else { GAIN_CEILING };
            // Fade the filter in over the first full tracker window. Until the
            // window has quiet frames in it, its minimum is whatever is being
            // said, so the estimator's honest answer at second zero is "all of
            // this is noise" — and acting on that took 14 dB off the opening
            // phrase of every recording. Trusting the estimate in proportion to
            // how much of its window exists means a recording can begin mid-
            // sentence and lose nothing.
            gains[bin] = 1.0 + self.confidence() * (gain - 1.0);
            self.previous_power[bin] = power;
        }
        self.started = true;
        self.subwindow_frames += 1;
        if self.subwindow_frames == MINIMUM_SUBWINDOW {
            self.subwindow_frames = 0;
            self.minimum_history.push_back(self.minimum.clone());
            if self.minimum_history.len() > MINIMUM_WINDOWS {
                self.minimum_history.pop_front();
            }
        }

        // Three-tap smoothing across neighbouring bins. An isolated bin left
        // loud between two quiet ones is exactly what is heard as musical
        // noise; averaging the gain curve costs nothing and removes most of it.
        smoothed_gains[0] = gains[0];
        smoothed_gains[bins - 1] = gains[bins - 1];
        for bin in 1..bins - 1 {
            smoothed_gains[bin] = (gains[bin - 1] + gains[bin] + gains[bin + 1]) / 3.0;
        }
        for bin in 0..bins {
            let gain = smoothed_gains[bin];
            self.previous_gain[bin] = gain;
            re[bin] *= gain;
            im[bin] *= gain;
            // The spectrum of a real signal is conjugate-symmetric, and it has
            // to stay that way or the inverse transform returns something
            // complex. Mirror every bin except DC and Nyquist, which have no
            // partner.
            if bin > 0 && bin < FRAME / 2 {
                re[FRAME - bin] *= gain;
                im[FRAME - bin] *= gain;
            }
        }

        self.fft.transform(&mut re, &mut im, true);
        for (index, tail) in self.overlap.iter().enumerate() {
            let sample = tail + re[index] * self.window[index];
            self.ready.push_back(if sample.is_finite() { sample } else { 0.0 });
        }
        for index in 0..HOP {
            self.overlap[index] = re[HOP + index] * self.window[HOP + index];
        }

        self.scratch_re = re;
        self.scratch_im = im;
        self.gains = gains;
        self.smoothed_gains = smoothed_gains;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE_RATE: f32 = 48_000.0;

    fn rms(samples: &[f32]) -> f32 {
        if samples.is_empty() {
            return 0.0;
        }
        (samples.iter().map(|s| (*s as f64) * (*s as f64)).sum::<f64>() / samples.len() as f64)
            .sqrt() as f32
    }

    fn db(ratio: f32) -> f32 {
        20.0 * ratio.max(1e-12).log10()
    }

    fn tone(frequency: f32, samples: usize, amplitude: f32) -> Vec<f32> {
        (0..samples)
            .map(|n| amplitude * (2.0 * PI * frequency * n as f32 / SAMPLE_RATE).sin())
            .collect()
    }

    /// Deterministic white-ish noise. A real RNG is not worth a dependency and
    /// a fixed sequence makes the attenuation assertions reproducible.
    fn noise(samples: usize, amplitude: f32) -> Vec<f32> {
        let mut state = 0x2545_F491_4F6C_DD1Du64;
        (0..samples)
            .map(|_| {
                state ^= state << 13;
                state ^= state >> 7;
                state ^= state << 17;
                // Top 24 bits, mapped onto [-1, 1).
                amplitude * ((state >> 40) as f32 / 8_388_608.0 - 1.0)
            })
            .collect()
    }

    fn run(suppressor: &mut NoiseSuppressor, input: &[f32]) -> Vec<f32> {
        let mut output = input.to_vec();
        // Deliberately not a multiple of the hop: the mixer's block is 3840
        // samples and a stop can leave any partial tail, so the filter has to
        // be correct for block lengths that do not line up with its frames.
        for block in output.chunks_mut(3_840) {
            suppressor.process(block);
        }
        output
    }

    #[test]
    fn preserves_block_length_for_any_chunking() {
        let mut suppressor = NoiseSuppressor::new();
        for size in [1usize, 7, 480, 512, 3_840, 9_001] {
            let mut block = vec![0.1f32; size];
            suppressor.process(&mut block);
            assert_eq!(block.len(), size, "block of {size} changed length");
        }
    }

    #[test]
    fn the_window_pair_reconstructs_exactly() {
        // Analysis window times synthesis window, overlapped at HOP, must sum
        // to one for every sample. That identity is the whole reason a gain of
        // 1.0 returns the input untouched, and it is worth asserting directly:
        // if it drifted, every recording would come back quietly comb-filtered
        // and no single listening test would obviously show why.
        let suppressor = NoiseSuppressor::new();
        for n in 0..HOP {
            let sum = suppressor.window[n] * suppressor.window[n]
                + suppressor.window[n + HOP] * suppressor.window[n + HOP];
            assert!(
                (sum - 1.0).abs() < 1e-5,
                "overlap at {n} summed to {sum}, not 1.0"
            );
        }
    }

    #[test]
    fn a_clean_signal_survives_before_the_room_is_learned() {
        // Nothing has been learned in the first frames, so the gain sits at
        // its ceiling and the filter is the identity. This is the streaming
        // form of the reconstruction property above.
        let mut suppressor = NoiseSuppressor::new();
        let input = tone(1_000.0, 4_800, 0.3);
        let output = run(&mut suppressor, &input);
        let measured = LATENCY_SAMPLES..4_000;
        let shifted: Vec<f32> = measured
            .clone()
            .map(|i| input[i - LATENCY_SAMPLES])
            .collect();
        let loss = db(rms(&output[measured]) / rms(&shifted));
        assert!(loss > -1.0, "a fresh filter cost a clean tone {loss:.2} dB");
    }

    #[test]
    fn attenuates_steady_room_noise() {
        let mut suppressor = NoiseSuppressor::new();
        let input = noise(144_000, 0.05);
        let output = run(&mut suppressor, &input);
        // Measure after the estimate has had two seconds to converge.
        let settled = 96_000..144_000;
        let reduction = db(rms(&output[settled.clone()]) / rms(&input[settled]));
        assert!(
            reduction < -8.0,
            "steady noise was only reduced by {reduction:.2} dB"
        );
    }

    /// Speech-shaped bursts: 400 ms of a two-harmonic voice, 400 ms of room.
    /// A real voice is not stationary, and that non-stationarity is the only
    /// thing separating it from a fan — so the test signal has to have it.
    fn burst_speech(samples: usize, amplitude: f32) -> Vec<f32> {
        let fundamental = tone(180.0, samples, amplitude);
        let harmonic = tone(540.0, samples, amplitude * 0.6);
        (0..samples)
            .map(|n| {
                let speaking = (n / 19_200) % 2 == 0;
                if speaking {
                    fundamental[n] + harmonic[n]
                } else {
                    0.0
                }
            })
            .collect()
    }

    #[test]
    fn keeps_speech_bursts_while_cutting_the_gaps() {
        // The property the switch actually promises: during a phrase the voice
        // comes back close to intact, and in the pause between phrases the
        // room is pushed down.
        let room = noise(192_000, 0.04);
        let voice = burst_speech(192_000, 0.2);
        let input: Vec<f32> = voice.iter().zip(&room).map(|(v, r)| v + r).collect();
        let mut suppressor = NoiseSuppressor::new();
        let output = run(&mut suppressor, &input);

        // Both windows are measured after the estimate has converged, and the
        // output is read one latency later than the input it came from.
        let speaking = 153_600 + LATENCY_SAMPLES..172_800;
        let pausing = 172_800 + LATENCY_SAMPLES..192_000;
        let shift = |range: std::ops::Range<usize>| {
            range
                .map(|i| input[i - LATENCY_SAMPLES])
                .collect::<Vec<f32>>()
        };
        let speech_loss = db(rms(&output[speaking.clone()]) / rms(&shift(speaking)));
        let pause_loss = db(rms(&output[pausing.clone()]) / rms(&shift(pausing)));

        assert!(speech_loss > -3.0, "speech lost {speech_loss:.2} dB");
        assert!(pause_loss < -8.0, "the pause only lost {pause_loss:.2} dB");
    }

    #[test]
    fn does_not_damage_the_opening_phrase() {
        // A recording can start mid-sentence. Before the fade-in existed, the
        // estimator's first window contained nothing but that sentence, called
        // all of it noise, and took 13.7 dB off it.
        let room = noise(48_000, 0.04);
        let voice = burst_speech(48_000, 0.2);
        let input: Vec<f32> = voice.iter().zip(&room).map(|(v, r)| v + r).collect();
        let mut suppressor = NoiseSuppressor::new();
        let output = run(&mut suppressor, &input);
        let opening = LATENCY_SAMPLES..19_200;
        let shifted: Vec<f32> = opening
            .clone()
            .map(|i| input[i - LATENCY_SAMPLES])
            .collect();
        let loss = db(rms(&output[opening]) / rms(&shifted));
        assert!(loss > -3.0, "the first phrase lost {loss:.2} dB");
    }

    #[test]
    fn never_amplifies() {
        // The one invariant that matters for the level regression this filter
        // must not repeat: no input can come out louder than it went in.
        let mut suppressor = NoiseSuppressor::new();
        let input: Vec<f32> = tone(440.0, 96_000, 0.4)
            .iter()
            .zip(noise(96_000, 0.1))
            .map(|(t, n)| t + n)
            .collect();
        let output = run(&mut suppressor, &input);
        let peak_in = input.iter().fold(0.0f32, |acc, s| acc.max(s.abs()));
        let peak_out = output.iter().fold(0.0f32, |acc, s| acc.max(s.abs()));
        assert!(peak_out <= peak_in, "peak rose from {peak_in} to {peak_out}");
        assert!(rms(&output) <= rms(&input), "RMS rose");
    }

    #[test]
    fn silence_stays_exactly_silent() {
        let mut suppressor = NoiseSuppressor::new();
        let output = run(&mut suppressor, &vec![0.0f32; 48_000]);
        assert!(
            output.iter().all(|s| *s == 0.0),
            "silence picked up {} of energy",
            rms(&output)
        );
    }

    #[test]
    fn non_finite_input_cannot_poison_the_stream() {
        let mut suppressor = NoiseSuppressor::new();
        let mut input = tone(1_000.0, 48_000, 0.3);
        input[100] = f32::NAN;
        input[101] = f32::INFINITY;
        input[20_000] = f32::NEG_INFINITY;
        let output = run(&mut suppressor, &input);
        assert!(output.iter().all(|s| s.is_finite()), "output went non-finite");
    }

    #[test]
    fn delay_is_the_documented_latency() {
        // An impulse in, LATENCY_SAMPLES later out. The recording's sample
        // count does not change, so this is the whole of the A/V cost.
        let mut suppressor = NoiseSuppressor::new();
        let mut input = vec![0.0f32; 48_000];
        input[10_000] = 1.0;
        let output = run(&mut suppressor, &input);
        let peak = output
            .iter()
            .enumerate()
            .fold((0usize, 0.0f32), |acc, (i, s)| {
                if s.abs() > acc.1 {
                    (i, s.abs())
                } else {
                    acc
                }
            })
            .0;
        assert_eq!(output.len(), input.len());
        let delay = peak as i64 - 10_000;
        assert!(
            (delay - LATENCY_SAMPLES as i64).abs() <= HOP as i64,
            "impulse moved {delay} samples, not {LATENCY_SAMPLES}"
        );
    }

    #[test]
    fn discarding_drops_buffered_audio_but_keeps_the_room() {
        let mut suppressor = NoiseSuppressor::new();
        run(&mut suppressor, &noise(96_000, 0.05));
        run(&mut suppressor, &tone(440.0, 1_000, 0.3));
        let learned = suppressor.windows_learned();
        assert!(learned > 0);

        suppressor.discard_buffered();
        assert_eq!(suppressor.windows_learned(), learned, "the room was forgotten");
        assert_eq!(suppressor.ready.len(), NoiseSuppressor::new().ready.len());
        // Silence in, silence out: nothing from before the cut comes back.
        let output = run(&mut suppressor, &vec![0.0f32; LATENCY_SAMPLES * 2]);
        assert!(output.iter().all(|s| *s == 0.0), "audio from before the cut was replayed");
    }

    #[test]
    fn reset_forgets_the_learned_room() {
        let mut suppressor = NoiseSuppressor::new();
        run(&mut suppressor, &noise(96_000, 0.05));
        suppressor.reset();
        let fresh = NoiseSuppressor::new();
        assert_eq!(suppressor.ready.len(), fresh.ready.len());
        assert!(suppressor.smoothed.iter().all(|p| *p == 0.0));
        assert!(suppressor.previous_gain.iter().all(|g| *g == 1.0));
        assert!(suppressor.minimum_history.is_empty());
        assert!(!suppressor.started);
    }
}
