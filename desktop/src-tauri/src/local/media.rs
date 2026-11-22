//! Media reads for local recordings where there is no AVFoundation: the
//! Windows side of the `duration`, `frames` and audio ops that `apple.m`
//! serves on macOS, on the FFmpeg the app already links for encoding.
//!
//! Compiled into every test build so the committed fixture exercises it on
//! macOS too; only Windows uses it at runtime.

#![cfg_attr(not(target_os = "windows"), allow(dead_code))]

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use anyhow::{anyhow, bail, Context, Result};
use ffmpeg::{codec, format, frame, media, software::resampling, software::scaling, ChannelLayout, Rational};

/// Longest side of an extracted frame, as `maximumSize` on the macOS generator.
const FRAME_MAX_SIDE: u32 = 1920;
/// FFmpeg's MJPEG quantiser (2 best, 31 worst); 3 lands near the 0.85 JPEG
/// quality macOS writes.
const FRAME_JPEG_QUALITY: i32 = 3;
/// A request this close behind the last decoded frame is reached by decoding
/// on rather than seeking back to a keyframe.
const FORWARD_DECODE_WINDOW: f64 = 10.0;
/// Frame extraction gives up after this long, as the macOS generator does.
const FRAMES_TIMEOUT: Duration = Duration::from_secs(300);
/// What the server's STT resamples to anyway, so nothing is lost by sending
/// it that way. Mono at this bit rate keeps an hour near 15 MB.
pub const AUDIO_SAMPLE_RATE: u32 = 16_000;
const AUDIO_BIT_RATE: usize = 32_000;
/// How many audio packets go by between looks at the cancel file.
const CANCEL_CHECK_EVERY: usize = 200;

pub struct FrameRequest {
    pub seconds: f64,
    pub output: PathBuf,
}

/// The compressed audio written for transcription.
pub struct AudioExtract {
    pub seconds: f64,
    pub bytes: u64,
}

/// The recording's length in seconds, 0 when the container does not say.
pub fn duration_seconds(video: &Path) -> Result<f64> {
    let input = format::input(video).with_context(|| format!("open {}", video.display()))?;
    let mut duration = input.duration();
    if duration <= 0 {
        // A fragmented file may only carry per-stream durations.
        duration = input
            .streams()
            .map(|stream| {
                let base = stream.time_base();
                (stream.duration().max(0) as f64 * f64::from(base.numerator())
                    / f64::from(base.denominator().max(1))
                    * f64::from(ffmpeg::ffi::AV_TIME_BASE)) as i64
            })
            .max()
            .unwrap_or(0);
    }
    Ok((duration.max(0) as f64) / f64::from(ffmpeg::ffi::AV_TIME_BASE))
}

/// Writes one JPEG per request and returns, in request order, the time of
/// the frame actually written or why it was not. Cancellation and the
/// timeout are reported per slot rather than as an error, so the frames that
/// did land are still indexed by the caller.
pub fn extract_frames(
    video: &Path,
    requests: &[FrameRequest],
    cancel: &Path,
) -> Result<Vec<Result<f64, String>>> {
    let mut results = vec![Err("Frame was not generated.".to_string()); requests.len()];
    if requests.is_empty() {
        return Ok(results);
    }
    let mut input = format::input(video).with_context(|| format!("open {}", video.display()))?;
    let stream = input
        .streams()
        .best(media::Type::Video)
        .ok_or_else(|| anyhow!("Recording has no video track."))?;
    let stream_index = stream.index();
    let time_base = stream.time_base();
    let mut decoder = codec::context::Context::from_parameters(stream.parameters())?
        .decoder()
        .video()
        .context("open video decoder")?;

    // Time order minimises seeks; results still land in request order.
    let mut order: Vec<usize> = (0..requests.len()).collect();
    order.sort_by(|a, b| {
        requests[*a]
            .seconds
            .partial_cmp(&requests[*b].seconds)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    let started = Instant::now();
    let mut jpeg = JpegWriter::default();
    let mut position: Option<f64> = None;
    let mut stopped: Option<&str> = None;
    for index in order {
        if stopped.is_none() && cancel.exists() {
            stopped = Some("Local frame extraction cancelled.");
        }
        if stopped.is_none() && started.elapsed() > FRAMES_TIMEOUT {
            stopped = Some("Local frame extraction timed out. Saved recording files remain on this PC.");
        }
        if let Some(reason) = stopped {
            results[index] = Err(reason.to_string());
            continue;
        }
        let target = requests[index].seconds.max(0.0);
        let seek = position.is_none_or(|at| target < at || target - at > FORWARD_DECODE_WINDOW);
        if seek {
            let ts = (target * f64::from(ffmpeg::ffi::AV_TIME_BASE)) as i64;
            if let Err(error) = input.seek(ts, ..ts) {
                results[index] = Err(format!("Could not seek local recording: {error}"));
                continue;
            }
            decoder.flush();
            position = None;
        }
        results[index] = match decode_until(&mut input, &mut decoder, stream_index, time_base, target) {
            Ok(Some((frame, at))) => {
                position = Some(at);
                jpeg.write(&frame, &requests[index].output).map(|()| at)
            }
            Ok(None) => Err("Could not decode local frame.".to_string()),
            Err(error) => Err(format!("Could not decode local frame: {error}")),
        };
    }
    Ok(results)
}

/// Decodes forward to the first frame at or after `target`, or the last
/// frame in the file when the target lies past its end.
fn decode_until(
    input: &mut format::context::Input,
    decoder: &mut codec::decoder::Video,
    stream_index: usize,
    time_base: Rational,
    target: f64,
) -> Result<Option<(frame::Video, f64)>> {
    let seconds_of = |frame: &frame::Video| {
        frame
            .timestamp()
            .or(frame.pts())
            .map(|pts| pts as f64 * f64::from(time_base.numerator()) / f64::from(time_base.denominator().max(1)))
    };
    let mut last: Option<(frame::Video, f64)> = None;
    let mut decoded = frame::Video::empty();
    let mut drain = |decoder: &mut codec::decoder::Video, last: &mut Option<(frame::Video, f64)>| {
        while decoder.receive_frame(&mut decoded).is_ok() {
            let Some(at) = seconds_of(&decoded) else { continue };
            if at + 1e-3 >= target {
                return Some((decoded.clone(), at));
            }
            *last = Some((decoded.clone(), at));
        }
        None
    };
    for (stream, packet) in input.packets() {
        if stream.index() != stream_index {
            continue;
        }
        decoder.send_packet(&packet)?;
        if let Some(hit) = drain(decoder, &mut last) {
            return Ok(Some(hit));
        }
    }
    decoder.send_eof()?;
    if let Some(hit) = drain(decoder, &mut last) {
        return Ok(Some(hit));
    }
    Ok(last)
}

/// Scales a decoded frame into the size budget and writes it as a JPEG,
/// keeping the scaler and encoder while frames stay the same size.
#[derive(Default)]
struct JpegWriter {
    ready: Option<(u32, u32, format::Pixel, scaling::Context, ffmpeg::encoder::Video)>,
    scaled: Option<frame::Video>,
    next_pts: i64,
}

impl JpegWriter {
    fn write(&mut self, frame: &frame::Video, output: &Path) -> Result<(), String> {
        let key = (frame.width(), frame.height(), frame.format());
        if self.ready.as_ref().map(|(w, h, f, _, _)| (*w, *h, *f)) != Some(key) {
            let (width, height) = fit(frame.width(), frame.height());
            let scaler = scaling::Context::get(
                frame.format(),
                frame.width(),
                frame.height(),
                format::Pixel::YUVJ420P,
                width,
                height,
                scaling::flag::Flags::AREA,
            )
            .map_err(|e| format!("frame scaler: {e}"))?;
            let codec = ffmpeg::encoder::find(codec::Id::MJPEG).ok_or("JPEG encoder is unavailable")?;
            let mut encoder = codec::context::Context::new_with_codec(codec)
                .encoder()
                .video()
                .map_err(|e| format!("JPEG context: {e}"))?;
            encoder.set_width(width);
            encoder.set_height(height);
            encoder.set_format(format::Pixel::YUVJ420P);
            encoder.set_color_range(ffmpeg::color::Range::JPEG);
            encoder.set_time_base((1, 30));
            encoder.set_qmin(FRAME_JPEG_QUALITY);
            encoder.set_qmax(FRAME_JPEG_QUALITY);
            let encoder = encoder.open_as(codec).map_err(|e| format!("JPEG encoder: {e}"))?;
            self.ready = Some((key.0, key.1, key.2, scaler, encoder));
            self.scaled = Some(frame::Video::new(format::Pixel::YUVJ420P, width, height));
        }
        let (Some((_, _, _, scaler, encoder)), Some(scaled)) = (self.ready.as_mut(), self.scaled.as_mut()) else {
            return Err("JPEG writer missing".to_string());
        };
        scaler.run(frame, scaled).map_err(|e| format!("frame scale: {e}"))?;
        scaled.set_color_range(ffmpeg::color::Range::JPEG);
        scaled.set_pts(Some(self.next_pts));
        self.next_pts += 1;
        encoder.send_frame(scaled).map_err(|e| format!("JPEG frame: {e}"))?;
        let mut packet = ffmpeg::Packet::empty();
        encoder.receive_packet(&mut packet).map_err(|e| format!("JPEG packet: {e}"))?;
        let data = packet.data().ok_or("JPEG packet is empty")?;
        std::fs::write(output, data).map_err(|_| "Could not save local frame.".to_string())
    }
}

/// The frame size inside the budget, even on both sides for 4:2:0 JPEG.
fn fit(width: u32, height: u32) -> (u32, u32) {
    let scale = (f64::from(FRAME_MAX_SIDE) / f64::from(width.max(height).max(1))).min(1.0);
    let even = |value: f64| ((value.round() as u32) & !1).max(2);
    (even(f64::from(width) * scale), even(f64::from(height) * scale))
}

/// Writes the recording's audio as 16 kHz mono AAC in an `.m4a`, the copy
/// sent for transcription. `Ok(None)` means the recording has no audio track.
pub fn extract_audio(video: &Path, output: &Path, cancel: &Path) -> Result<Option<AudioExtract>> {
    let mut input = format::input(video).with_context(|| format!("open {}", video.display()))?;
    let Some(stream) = input.streams().best(media::Type::Audio) else {
        return Ok(None);
    };
    let stream_index = stream.index();
    let mut decoder = codec::context::Context::from_parameters(stream.parameters())?
        .decoder()
        .audio()
        .context("open audio decoder")?;
    if decoder.channel_layout().is_empty() {
        decoder.set_channel_layout(ChannelLayout::default(i32::from(decoder.channels())));
    }

    let mut out = format::output(output).with_context(|| format!("create {}", output.display()))?;
    let codec = ffmpeg::encoder::find_by_name("aac")
        .ok_or_else(|| anyhow!("aac encoder not available in this ffmpeg build"))?;
    let global_header = out.format().flags().contains(format::Flags::GLOBAL_HEADER);
    let mut encoder = codec::context::Context::new_with_codec(codec).encoder().audio()?;
    if global_header {
        encoder.set_flags(codec::Flags::GLOBAL_HEADER);
    }
    let rate = AUDIO_SAMPLE_RATE as i32;
    let sample_format = format::Sample::F32(format::sample::Type::Planar);
    encoder.set_rate(rate);
    encoder.set_format(sample_format);
    encoder.set_channel_layout(ChannelLayout::MONO);
    encoder.set_bit_rate(AUDIO_BIT_RATE);
    encoder.set_time_base(Rational::new(1, rate));
    let mut encoder = encoder.open().context("open aac encoder")?;
    let frame_size = encoder.frame_size() as usize;
    if frame_size == 0 {
        bail!("AAC encoder reported a zero frame_size");
    }
    let out_index = {
        let mut stream = out.add_stream(codec)?;
        stream.set_time_base(Rational::new(1, rate));
        stream.set_parameters(&encoder);
        stream.index()
    };
    out.write_header().context("write m4a header")?;

    let mut resampler = resampling::Context::get(
        decoder.format(),
        decoder.channel_layout(),
        decoder.rate(),
        sample_format,
        ChannelLayout::MONO,
        AUDIO_SAMPLE_RATE,
    )
    .context("audio resampler")?;
    let mut lane = MonoLane {
        pending: Vec::new(),
        next_pts: 0,
        written: 0,
        packet: ffmpeg::Packet::empty(),
    };
    let mut decoded = frame::Audio::empty();
    let mut packets = 0usize;
    for (stream, packet) in input.packets() {
        if stream.index() != stream_index {
            continue;
        }
        packets += 1;
        if packets % CANCEL_CHECK_EVERY == 0 && cancel.exists() {
            bail!("Local transcription cancelled.");
        }
        decoder.send_packet(&packet)?;
        while decoder.receive_frame(&mut decoded).is_ok() {
            let mut resampled = frame::Audio::empty();
            resampler.run(&decoded, &mut resampled)?;
            lane.push(&resampled);
            lane.encode(&mut encoder, &mut out, out_index, frame_size, false)?;
        }
    }
    decoder.send_eof()?;
    while decoder.receive_frame(&mut decoded).is_ok() {
        let mut resampled = frame::Audio::empty();
        resampler.run(&decoded, &mut resampled)?;
        lane.push(&resampled);
        lane.encode(&mut encoder, &mut out, out_index, frame_size, false)?;
    }
    loop {
        let mut resampled = frame::Audio::new(sample_format, 4096, ChannelLayout::MONO);
        resampler.flush(&mut resampled)?;
        if resampled.samples() == 0 {
            break;
        }
        lane.push(&resampled);
    }
    lane.encode(&mut encoder, &mut out, out_index, frame_size, true)?;
    encoder.send_eof()?;
    lane.drain(&mut encoder, &mut out, out_index)?;
    out.write_trailer().context("write m4a trailer")?;
    let bytes = std::fs::metadata(output).map(|m| m.len()).unwrap_or(0);
    Ok(Some(AudioExtract {
        seconds: lane.written as f64 / f64::from(AUDIO_SAMPLE_RATE),
        bytes,
    }))
}

/// The recording's audio as 16 kHz mono f32 samples in -1..1, what the local
/// edition's transcription engines take. `Ok(None)` means the recording has
/// no audio track. An hour is about 230 MB of samples, held in memory for the
/// engine; nothing is written to disk.
pub fn extract_pcm(video: &Path, cancel: &Path) -> Result<Option<Vec<f32>>> {
    let mut input = format::input(video).with_context(|| format!("open {}", video.display()))?;
    let Some(stream) = input.streams().best(media::Type::Audio) else {
        return Ok(None);
    };
    let stream_index = stream.index();
    let mut decoder = codec::context::Context::from_parameters(stream.parameters())?
        .decoder()
        .audio()
        .context("open audio decoder")?;
    if decoder.channel_layout().is_empty() {
        decoder.set_channel_layout(ChannelLayout::default(i32::from(decoder.channels())));
    }
    let sample_format = format::Sample::F32(format::sample::Type::Planar);
    let mut resampler = resampling::Context::get(
        decoder.format(),
        decoder.channel_layout(),
        decoder.rate(),
        sample_format,
        ChannelLayout::MONO,
        AUDIO_SAMPLE_RATE,
    )
    .context("audio resampler")?;
    let mut pcm: Vec<f32> = Vec::new();
    let mut push = |resampled: &frame::Audio| {
        let samples = resampled.samples();
        if samples > 0 {
            pcm.extend_from_slice(&resampled.plane::<f32>(0)[..samples]);
        }
    };
    let mut decoded = frame::Audio::empty();
    let mut packets = 0usize;
    for (stream, packet) in input.packets() {
        if stream.index() != stream_index {
            continue;
        }
        packets += 1;
        if packets % CANCEL_CHECK_EVERY == 0 && cancel.exists() {
            bail!("Local transcription cancelled.");
        }
        decoder.send_packet(&packet)?;
        while decoder.receive_frame(&mut decoded).is_ok() {
            let mut resampled = frame::Audio::empty();
            resampler.run(&decoded, &mut resampled)?;
            push(&resampled);
        }
    }
    decoder.send_eof()?;
    while decoder.receive_frame(&mut decoded).is_ok() {
        let mut resampled = frame::Audio::empty();
        resampler.run(&decoded, &mut resampled)?;
        push(&resampled);
    }
    loop {
        let mut resampled = frame::Audio::new(sample_format, 4096, ChannelLayout::MONO);
        resampler.flush(&mut resampled)?;
        if resampled.samples() == 0 {
            break;
        }
        push(&resampled);
    }
    for sample in &mut pcm {
        *sample = if sample.is_finite() {
            sample.clamp(-1.0, 1.0)
        } else {
            0.0
        };
    }
    Ok(Some(pcm))
}

/// Mono samples waiting to fill encoder-sized frames.
struct MonoLane {
    pending: Vec<f32>,
    next_pts: i64,
    written: u64,
    packet: ffmpeg::Packet,
}

impl MonoLane {
    fn push(&mut self, resampled: &frame::Audio) {
        let samples = resampled.samples();
        if samples > 0 {
            self.pending.extend_from_slice(&resampled.plane::<f32>(0)[..samples]);
        }
    }

    /// Encodes every full frame, and the remainder too when `last`: the AAC
    /// encoder accepts a short frame only as the final one.
    fn encode(
        &mut self,
        encoder: &mut ffmpeg::encoder::Audio,
        out: &mut format::context::Output,
        out_index: usize,
        frame_size: usize,
        last: bool,
    ) -> Result<()> {
        while self.pending.len() >= frame_size || (last && !self.pending.is_empty()) {
            let take = self.pending.len().min(frame_size);
            let mut frame = frame::Audio::new(
                format::Sample::F32(format::sample::Type::Planar),
                take,
                ChannelLayout::MONO,
            );
            frame.set_rate(AUDIO_SAMPLE_RATE);
            frame.plane_mut::<f32>(0)[..take].copy_from_slice(&self.pending[..take]);
            frame.set_pts(Some(self.next_pts));
            self.pending.drain(..take);
            self.next_pts += take as i64;
            self.written += take as u64;
            encoder.send_frame(&frame).context("aac send_frame")?;
            self.drain(encoder, out, out_index)?;
        }
        Ok(())
    }

    fn drain(
        &mut self,
        encoder: &mut ffmpeg::encoder::Audio,
        out: &mut format::context::Output,
        out_index: usize,
    ) -> Result<()> {
        while encoder.receive_packet(&mut self.packet).is_ok() {
            self.packet.set_stream(out_index);
            let time_base = out
                .stream(out_index)
                .ok_or_else(|| anyhow!("audio stream missing"))?
                .time_base();
            self.packet
                .rescale_ts(Rational::new(1, AUDIO_SAMPLE_RATE as i32), time_base);
            self.packet.write_interleaved(out).context("aac write_interleaved")?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/local-sample.mp4")
    }

    #[test]
    fn fixture_duration_is_read() {
        crate::encoder::Mp4Encoder::init_global();
        let seconds = duration_seconds(&fixture()).unwrap();
        assert!((seconds - 1.5).abs() < 0.1, "{seconds}");
    }

    #[test]
    fn frames_land_in_request_order_with_their_actual_times() {
        crate::encoder::Mp4Encoder::init_global();
        let dir = tempfile::tempdir().unwrap();
        let requests = vec![
            FrameRequest { seconds: 1.0, output: dir.path().join("b.jpg") },
            FrameRequest { seconds: 0.2, output: dir.path().join("a.jpg") },
            // Past the end: the last frame stands in.
            FrameRequest { seconds: 9.0, output: dir.path().join("c.jpg") },
        ];
        let results = extract_frames(&fixture(), &requests, &dir.path().join("cancelled")).unwrap();
        assert_eq!(results.len(), 3);
        let at = |i: usize| *results[i].as_ref().unwrap();
        assert!((at(0) - 1.0).abs() < 0.05, "{:?}", results[0]);
        assert!((at(1) - 0.2).abs() < 0.05, "{:?}", results[1]);
        assert!(at(2) > 1.3 && at(2) <= 1.5, "{:?}", results[2]);
        for name in ["a.jpg", "b.jpg", "c.jpg"] {
            let bytes = std::fs::read(dir.path().join(name)).unwrap();
            assert!(bytes.starts_with(&[0xFF, 0xD8]), "{name} is a JPEG");
            let image = image::load_from_memory(&bytes).unwrap().to_rgb8();
            assert_eq!(image.dimensions(), (64, 36));
            // The fixture is a flat 0x336699.
            let pixel = image.get_pixel(32, 18);
            assert!((i32::from(pixel[0]) - 0x33).abs() < 12 && (i32::from(pixel[2]) - 0x99).abs() < 12, "{pixel:?}");
        }
    }

    #[test]
    fn cancelled_extraction_reports_every_remaining_slot() {
        crate::encoder::Mp4Encoder::init_global();
        let dir = tempfile::tempdir().unwrap();
        let cancel = dir.path().join("cancelled");
        std::fs::write(&cancel, b"").unwrap();
        let requests = vec![FrameRequest { seconds: 0.5, output: dir.path().join("x.jpg") }];
        let results = extract_frames(&fixture(), &requests, &cancel).unwrap();
        assert_eq!(results[0].as_ref().unwrap_err(), "Local frame extraction cancelled.");
        assert!(!dir.path().join("x.jpg").exists());
    }

    #[test]
    fn audio_is_written_as_small_mono_aac() {
        crate::encoder::Mp4Encoder::init_global();
        let dir = tempfile::tempdir().unwrap();
        let output = dir.path().join("recording.m4a");
        let extract = extract_audio(&fixture(), &output, &dir.path().join("cancelled"))
            .unwrap()
            .expect("fixture has audio");
        assert!((extract.seconds - 1.5).abs() < 0.1, "{}", extract.seconds);
        assert!(extract.bytes > 1_000 && extract.bytes < 20_000, "{}", extract.bytes);
        let written = format::input(&output).unwrap();
        let stream = written.streams().best(media::Type::Audio).unwrap();
        let decoder = codec::context::Context::from_parameters(stream.parameters())
            .unwrap()
            .decoder()
            .audio()
            .unwrap();
        assert_eq!(decoder.rate(), AUDIO_SAMPLE_RATE);
        assert_eq!(decoder.channels(), 1);
        assert!((duration_seconds(&output).unwrap() - 1.5).abs() < 0.15);
    }

    #[test]
    fn pcm_is_16k_mono_for_the_whole_recording() {
        crate::encoder::Mp4Encoder::init_global();
        let dir = tempfile::tempdir().unwrap();
        let pcm = extract_pcm(&fixture(), &dir.path().join("cancelled"))
            .unwrap()
            .expect("fixture has audio");
        let seconds = pcm.len() as f64 / f64::from(AUDIO_SAMPLE_RATE);
        assert!((seconds - 1.5).abs() < 0.1, "{seconds}");
        assert!(pcm.iter().all(|s| (-1.0..=1.0).contains(s)));
    }

    #[test]
    fn frame_budget_keeps_aspect_and_even_sides() {
        assert_eq!(fit(3840, 2160), (1920, 1080));
        assert_eq!(fit(1280, 1024), (1280, 1024));
        // Scaled by the longest side, then rounded down to even on both.
        assert_eq!(fit(1921, 1080), (1920, 1078));
    }
}
