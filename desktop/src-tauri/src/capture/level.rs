//! Mic level maths — pure DSP, no platform APIs.
//!
//! These live outside `capture::audio` (macOS-only, cpal + CoreAudio) because
//! `recording.rs::PauseState::observe_mic_samples` is shared code that runs on
//! every platform. Keeping them here lets the Windows build see them without
//! pulling in the macOS mic capturer. `capture::audio` re-exports both names,
//! so existing `crate::capture::audio::samples_level` paths still resolve.

/// Normalised level at or above which we consider the mic to be hearing
/// something. Used both for the UI meter's "input detected" state and for the
/// silence watchdog.
pub const MIC_INPUT_ACTIVE_LEVEL: f32 = 0.08;

/// Peak level of an interleaved f32 PCM buffer, mapped onto a 0..=1 scale
/// across a 60 dB window. NaN/inf samples are ignored rather than poisoning
/// the fold.
pub fn samples_level(samples: &[f32]) -> f32 {
    let peak = samples
        .iter()
        .copied()
        .filter(|sample| sample.is_finite())
        .map(f32::abs)
        .fold(0.0, f32::max)
        .min(1.0);
    if peak <= 0.0 {
        0.0
    } else {
        ((20.0 * peak.log10() + 60.0) / 60.0).clamp(0.0, 1.0)
    }
}

/// Whether a level from [`samples_level`] counts as real input.
pub fn level_has_input(level: f32) -> bool {
    level >= MIC_INPUT_ACTIVE_LEVEL
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn samples_level_reports_zero_for_silent_input() {
        assert_eq!(samples_level(&[0.0; 128]), 0.0);
        assert!(!level_has_input(samples_level(&[0.0; 128])));
    }

    #[test]
    fn samples_level_reports_signal_for_voice_range_input() {
        let level = samples_level(&[0.3; 128]);
        assert!(level > 0.0);
        assert!(level_has_input(level));
    }

    #[test]
    fn samples_level_ignores_non_finite_samples() {
        let level = samples_level(&[f32::NAN, f32::INFINITY, 0.0]);
        assert_eq!(level, 0.0);
    }
}
