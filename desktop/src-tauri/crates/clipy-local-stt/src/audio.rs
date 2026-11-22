//! Audio helpers: a small WAV reader and writer, and silence detection.
//!
//! Every engine works on the same audio: 16 kHz mono samples in [-1, 1].
//! The caller decodes the recording itself (the desktop app with its own
//! FFmpeg); [`read_wav`] is for tests and callers that already hold a PCM WAV.

use std::path::Path;

use crate::chunking::SilenceSpan;
use crate::error::{Result, SttError};

pub const SAMPLE_RATE: u32 = 16_000;

/// Decoded WAV audio, downmixed to mono.
#[derive(Clone, Debug, PartialEq)]
pub struct WavAudio {
    pub sample_rate: u32,
    pub samples: Vec<f32>,
}

/// Read a 16 kHz PCM WAV file as mono samples. 16-bit integer and 32-bit
/// float data are accepted; more than one channel is averaged. Any other
/// sample rate is refused (this crate does not resample).
pub fn read_wav(path: &Path) -> Result<Vec<f32>> {
    let bytes = std::fs::read(path)?;
    let wav = read_wav_bytes(&bytes)?;
    if wav.sample_rate != SAMPLE_RATE {
        return Err(SttError::InvalidInput(format!(
            "the WAV is {} Hz; transcription needs 16000 Hz mono audio",
            wav.sample_rate
        )));
    }
    Ok(wav.samples)
}

/// Parse a WAV held in memory (RIFF chunks, so LIST and other chunks are skipped).
pub fn read_wav_bytes(buf: &[u8]) -> Result<WavAudio> {
    let bad = |m: &str| SttError::InvalidInput(m.to_string());
    if buf.len() < 12 || &buf[0..4] != b"RIFF" || &buf[8..12] != b"WAVE" {
        return Err(bad("not a WAV file"));
    }
    let u16_at = |p: usize| u16::from_le_bytes([buf[p], buf[p + 1]]);
    let u32_at = |p: usize| u32::from_le_bytes([buf[p], buf[p + 1], buf[p + 2], buf[p + 3]]);
    let mut pos = 12usize;
    let mut fmt: Option<(u16, u16, u32, u16)> = None; // format, channels, rate, bits
    while pos + 8 <= buf.len() {
        let id = &buf[pos..pos + 4];
        let len = u32_at(pos + 4) as usize;
        let body = pos + 8;
        if id == b"fmt " {
            if body + 16 > buf.len() {
                return Err(bad("truncated WAV format chunk"));
            }
            let mut format = u16_at(body);
            // WAVE_FORMAT_EXTENSIBLE: the real format is the sub-format GUID's first two bytes.
            if format == 0xFFFE && len >= 40 && body + 26 <= buf.len() {
                format = u16_at(body + 24);
            }
            fmt = Some((
                format,
                u16_at(body + 2),
                u32_at(body + 4),
                u16_at(body + 14),
            ));
        } else if id == b"data" {
            let (format, channels, rate, bits) =
                fmt.ok_or_else(|| bad("WAV data before its format chunk"))?;
            if channels == 0 {
                return Err(bad("WAV has no channels"));
            }
            // A streamed WAV can carry a placeholder length; trust the buffer.
            let end = body.saturating_add(len).min(buf.len());
            let data = &buf[body..end];
            let ch = channels as usize;
            let samples = match (format, bits) {
                (1, 16) => downmix(
                    data.chunks_exact(2)
                        .map(|b| i16::from_le_bytes([b[0], b[1]]) as f32 / 32768.0),
                    ch,
                ),
                (3, 32) => downmix(
                    data.chunks_exact(4)
                        .map(|b| f32::from_le_bytes([b[0], b[1], b[2], b[3]])),
                    ch,
                ),
                _ => return Err(bad("only 16-bit PCM and 32-bit float WAV are supported")),
            };
            return Ok(WavAudio {
                sample_rate: rate,
                samples,
            });
        }
        pos = body.saturating_add(len).saturating_add(len % 2);
    }
    Err(bad("WAV has no data chunk"))
}

fn downmix(it: impl Iterator<Item = f32>, channels: usize) -> Vec<f32> {
    if channels == 1 {
        return it.collect();
    }
    let all: Vec<f32> = it.collect();
    all.chunks_exact(channels)
        .map(|f| f.iter().sum::<f32>() / channels as f32)
        .collect()
}

/// A complete 16-bit mono WAV file holding the given samples.
pub fn wav_from_pcm(samples: &[f32], sample_rate: u32) -> Vec<u8> {
    let data_len = (samples.len() * 2) as u32;
    let mut out = Vec::with_capacity(44 + samples.len() * 2);
    out.extend_from_slice(b"RIFF");
    out.extend_from_slice(&(36 + data_len).to_le_bytes());
    out.extend_from_slice(b"WAVEfmt ");
    out.extend_from_slice(&16u32.to_le_bytes());
    out.extend_from_slice(&1u16.to_le_bytes()); // PCM
    out.extend_from_slice(&1u16.to_le_bytes()); // mono
    out.extend_from_slice(&sample_rate.to_le_bytes());
    out.extend_from_slice(&(sample_rate * 2).to_le_bytes());
    out.extend_from_slice(&2u16.to_le_bytes());
    out.extend_from_slice(&16u16.to_le_bytes());
    out.extend_from_slice(b"data");
    out.extend_from_slice(&data_len.to_le_bytes());
    for s in samples {
        let v = (s.clamp(-1.0, 1.0) * 32767.0).round() as i16;
        out.extend_from_slice(&v.to_le_bytes());
    }
    out
}

/// Samples of [start_sec, start_sec + dur_sec), clamped to the buffer.
pub(crate) fn slice_secs(pcm: &[f32], start_sec: f64, dur_sec: f64) -> &[f32] {
    let rate = SAMPLE_RATE as f64;
    let a = ((start_sec * rate).round().max(0.0) as usize).min(pcm.len());
    let b = (((start_sec + dur_sec) * rate).round().max(0.0) as usize).min(pcm.len());
    &pcm[a..b.max(a)]
}

pub(crate) fn duration_sec(pcm: &[f32]) -> f64 {
    pcm.len() as f64 / SAMPLE_RATE as f64
}

/// Default silence threshold and minimum length, the same as the TS package
/// and Clipy's hosted worker (ffmpeg `silencedetect=noise=-30dB:d=0.4`).
pub const SILENCE_NOISE_DB: f64 = -30.0;
pub const SILENCE_MIN_SEC: f64 = 0.4;

/// Energy-based silence detection that mirrors ffmpeg's `silencedetect` on
/// mono audio: a sample is quiet when its magnitude is under the threshold,
/// and a run of quiet samples at least `min_sec` long is a silence. A
/// silence still open at the end runs to the end of the audio.
pub fn detect_silences(pcm: &[f32], noise_db: f64, min_sec: f64) -> Vec<SilenceSpan> {
    let threshold = 10f64.powf(noise_db / 20.0) as f32;
    let min_samples = (min_sec * SAMPLE_RATE as f64).ceil().max(1.0) as usize;
    let rate = SAMPLE_RATE as f64;
    let mut spans = Vec::new();
    let mut run_start: Option<usize> = None;
    for (i, s) in pcm.iter().enumerate() {
        if s.abs() < threshold {
            if run_start.is_none() {
                run_start = Some(i);
            }
        } else if let Some(start) = run_start.take() {
            if i - start >= min_samples {
                spans.push(SilenceSpan {
                    start_sec: start as f64 / rate,
                    end_sec: i as f64 / rate,
                });
            }
        }
    }
    if let Some(start) = run_start {
        if pcm.len() - start >= min_samples {
            spans.push(SilenceSpan {
                start_sec: start as f64 / rate,
                end_sec: pcm.len() as f64 / rate,
            });
        }
    }
    spans
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tone(sec: f64, amp: f32) -> Vec<f32> {
        let n = (sec * SAMPLE_RATE as f64) as usize;
        (0..n)
            .map(|i| amp * ((i as f32) * 0.05).sin().signum())
            .collect()
    }

    #[test]
    fn wav_round_trips() {
        let pcm: Vec<f32> = (0..1600)
            .map(|i| ((i as f32) / 100.0).sin() * 0.5)
            .collect();
        let wav = wav_from_pcm(&pcm, SAMPLE_RATE);
        assert_eq!(wav.len(), 44 + 3200);
        let back = read_wav_bytes(&wav).unwrap();
        assert_eq!(back.sample_rate, 16_000);
        assert_eq!(back.samples.len(), pcm.len());
        for (a, b) in pcm.iter().zip(&back.samples) {
            assert!((a - b).abs() < 1e-4);
        }
    }

    #[test]
    fn wav_reader_skips_extra_chunks_and_downmixes_stereo() {
        let mut wav = Vec::new();
        wav.extend_from_slice(b"RIFF\0\0\0\0WAVE");
        wav.extend_from_slice(b"LIST");
        wav.extend_from_slice(&3u32.to_le_bytes());
        wav.extend_from_slice(b"abc\0"); // odd length plus pad byte
        wav.extend_from_slice(b"fmt ");
        wav.extend_from_slice(&16u32.to_le_bytes());
        wav.extend_from_slice(&1u16.to_le_bytes());
        wav.extend_from_slice(&2u16.to_le_bytes());
        wav.extend_from_slice(&16_000u32.to_le_bytes());
        wav.extend_from_slice(&64_000u32.to_le_bytes());
        wav.extend_from_slice(&4u16.to_le_bytes());
        wav.extend_from_slice(&16u16.to_le_bytes());
        wav.extend_from_slice(b"data");
        wav.extend_from_slice(&0xFFFF_FFFFu32.to_le_bytes()); // streamed placeholder length
        for (l, r) in [(16384i16, 0i16), (-16384, -16384)] {
            wav.extend_from_slice(&l.to_le_bytes());
            wav.extend_from_slice(&r.to_le_bytes());
        }
        let got = read_wav_bytes(&wav).unwrap();
        assert_eq!(got.samples, vec![0.25, -0.5]);
    }

    #[test]
    fn wav_reader_refuses_garbage_and_other_rates() {
        assert!(read_wav_bytes(b"not a wav file at all").is_err());
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("a.wav");
        std::fs::write(&p, wav_from_pcm(&[0.0; 10], 22_050)).unwrap();
        assert!(matches!(read_wav(&p), Err(SttError::InvalidInput(_))));
    }

    #[test]
    fn silences_are_found_between_speech() {
        let mut pcm = tone(1.0, 0.5);
        pcm.extend(vec![0.001; 8000]); // 0.5 s quiet
        pcm.extend(tone(1.0, 0.5));
        pcm.extend(vec![0.0; 3200]); // 0.2 s: too short
        pcm.extend(tone(0.5, 0.5));
        pcm.extend(vec![0.0; 16000]); // trailing 1 s, open at the end
        let s = detect_silences(&pcm, SILENCE_NOISE_DB, SILENCE_MIN_SEC);
        assert_eq!(s.len(), 2);
        assert!((s[0].start_sec - 1.0).abs() < 0.01 && (s[0].end_sec - 1.5).abs() < 0.01);
        assert!((s[1].end_sec - duration_sec(&pcm)).abs() < 1e-9);
    }
}
