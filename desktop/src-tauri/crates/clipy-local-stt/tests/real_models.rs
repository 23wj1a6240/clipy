//! Opt-in: transcribes speech generated with `espeak-ng` using the real
//! models from the default model folder. Run with
//! `CLIPY_TEST_MODELS=1 cargo test --release --test real_models -- --nocapture`.
//! Skips (passes) when the variable is unset, espeak-ng is missing, or a
//! model is not installed.

use std::process::Command;
use std::time::Instant;

use clipy_local_stt::{
    model_status, read_wav_bytes, transcribe_local, CancelToken, LocalModelId, SAMPLE_RATE,
};

fn enabled() -> bool {
    std::env::var("CLIPY_TEST_MODELS").is_ok_and(|v| v == "1")
}

/// espeak-ng speech as 16 kHz mono samples. espeak writes 22050 Hz; ffmpeg
/// resamples it when present (as the app's own FFmpeg would), otherwise a
/// linear resampler does.
fn speak(voice: &str, text: &str) -> Option<Vec<f32>> {
    let dir = tempfile::tempdir().ok()?;
    let out = dir.path().join("speech.wav");
    let ok = Command::new("espeak-ng")
        .args(["-v", voice, "-s", "150", "-w"])
        .arg(&out)
        .arg(text)
        .status()
        .ok()?
        .success();
    if !ok {
        return None;
    }
    let resampled = dir.path().join("speech16k.wav");
    let ff = Command::new("ffmpeg")
        .args(["-hide_banner", "-loglevel", "error", "-y", "-i"])
        .arg(&out)
        .args(["-ac", "1", "-ar", "16000", "-c:a", "pcm_s16le"])
        .arg(&resampled)
        .status()
        .is_ok_and(|s| s.success());
    let wav = read_wav_bytes(&std::fs::read(if ff { &resampled } else { &out }).ok()?).ok()?;
    Some(resample(&wav.samples, wav.sample_rate, SAMPLE_RATE))
}

/// How many of `words` appear in `text`.
fn hits(text: &str, words: &[&str]) -> usize {
    words.iter().filter(|w| text.contains(*w)).count()
}

fn resample(x: &[f32], from: u32, to: u32) -> Vec<f32> {
    if from == to {
        return x.to_vec();
    }
    let n = (x.len() as u64 * to as u64 / from as u64) as usize;
    let step = from as f64 / to as f64;
    (0..n)
        .map(|i| {
            let p = i as f64 * step;
            let j = p.floor() as usize;
            let f = (p - j as f64) as f32;
            let a = x[j.min(x.len() - 1)];
            let b = x[(j + 1).min(x.len() - 1)];
            a + (b - a) * f
        })
        .collect()
}

fn silence(sec: f64) -> Vec<f32> {
    vec![0.0; (sec * SAMPLE_RATE as f64) as usize]
}

fn installed(model: LocalModelId) -> bool {
    let s = model_status(model, None);
    if !s.installed {
        eprintln!(
            "skip: {} is not installed in {}",
            model.as_str(),
            s.dir.display()
        );
    }
    s.installed
}

fn have_espeak() -> bool {
    Command::new("espeak-ng").arg("--version").output().is_ok()
}

fn show(name: &str, t: &clipy_local_stt::Transcript, audio_sec: f64, took: f64) {
    eprintln!("--- {name}: {:.1} s of audio in {:.2} s ({}x real time), engine {}, language {:?}, {} gaps", audio_sec, took, (audio_sec / took).round(), t.engine, t.language, t.gaps.len());
    for s in &t.segments {
        eprintln!("[{:>7} - {:>7}] {}", s.start_ms, s.end_ms, s.text);
    }
}

const SENTENCES: [&str; 6] = [
    "Hello, this is a test of the Clipy local speech recognition.",
    "The quick brown fox jumps over the lazy dog.",
    "Open the settings page and click the record button to start a new recording.",
    "Every transcript is made on this computer, and nothing is uploaded.",
    "Please send the bug report to the support team before Friday.",
    "The meeting starts at nine thirty in the morning.",
];

#[test]
fn english_fast_transcribes_short_speech() {
    if !enabled() || !have_espeak() || !installed(LocalModelId::EnglishFast) {
        return;
    }
    let mut pcm = silence(0.5);
    pcm.extend(speak("en-us", &SENTENCES[..2].join(" ")).expect("espeak-ng"));
    pcm.extend(silence(0.5));
    let sec = pcm.len() as f64 / SAMPLE_RATE as f64;
    let started = Instant::now();
    let t = transcribe_local(
        &pcm,
        LocalModelId::EnglishFast,
        None,
        &mut |_| {},
        &CancelToken::new(),
    )
    .unwrap();
    show(
        "english-fast, short",
        &t,
        sec,
        started.elapsed().as_secs_f64(),
    );
    let text = t
        .segments
        .iter()
        .map(|s| s.text.to_lowercase())
        .collect::<Vec<_>>()
        .join(" ");
    // espeak-ng's robotic voice is harder than real speech; most words must come through.
    let words = [
        "hello",
        "test",
        "speech",
        "recognition",
        "quick",
        "brown",
        "fox",
        "lazy",
        "dog",
    ];
    let got = hits(&text, &words);
    assert!(got >= 6, "only {got} of {} words in {text:?}", words.len());
    assert!(t.gaps.is_empty());
    assert_eq!(t.language.as_deref(), Some("en"));
    assert!(t
        .segments
        .iter()
        .all(|s| s.end_ms as f64 <= sec * 1000.0 + 500.0));
}

#[test]
fn english_fast_handles_audio_longer_than_one_piece() {
    if !enabled() || !have_espeak() || !installed(LocalModelId::EnglishFast) {
        return;
    }
    // About four minutes: more than Parakeet's 180 s limit, so it must be cut.
    let mut pcm = Vec::new();
    let mut round = 0;
    while pcm.len() < 240 * SAMPLE_RATE as usize {
        for (i, s) in SENTENCES.iter().enumerate() {
            pcm.extend(speak("en-us", s).expect("espeak-ng"));
            pcm.extend(silence(if (i + round) % 3 == 0 { 1.2 } else { 0.6 }));
        }
        round += 1;
    }
    let sec = pcm.len() as f64 / SAMPLE_RATE as f64;
    let mut last = 0.0;
    let started = Instant::now();
    let t = transcribe_local(
        &pcm,
        LocalModelId::EnglishFast,
        None,
        &mut |p| last = p,
        &CancelToken::new(),
    )
    .unwrap();
    let took = started.elapsed().as_secs_f64();
    eprintln!(
        "--- english-fast, long: {sec:.1} s of audio in {took:.2} s, {} segments, {} gaps",
        t.segments.len(),
        t.gaps.len()
    );
    for s in t.segments.iter().take(4) {
        eprintln!("[{:>7} - {:>7}] {}", s.start_ms, s.end_ms, s.text);
    }
    assert_eq!(last, 1.0);
    assert!(t.gaps.is_empty());
    for w in t.segments.windows(2) {
        assert!(w[0].end_ms <= w[1].start_ms);
    }
    let text = t
        .segments
        .iter()
        .map(|s| s.text.to_lowercase())
        .collect::<Vec<_>>()
        .join(" ");
    let said = text.matches("record button").count();
    eprintln!("\"record button\" appears {said} times for {round} spoken rounds");
    // Each round says it once; the overlap at a cut must never double one.
    assert!(said + 2 >= round && said <= round, "{said} vs {round}");
    assert!(t.segments.last().unwrap().end_ms as f64 > (sec - 20.0) * 1000.0);
}

#[test]
fn multilingual_transcribes_and_detects_language() {
    if !enabled() || !have_espeak() || !installed(LocalModelId::Multilingual) {
        return;
    }
    let mut pcm = silence(0.3);
    pcm.extend(
        speak(
            "es",
            "Hola, buenos días. Esta es una prueba del reconocimiento de voz.",
        )
        .expect("espeak-ng"),
    );
    pcm.extend(silence(0.3));
    let sec = pcm.len() as f64 / SAMPLE_RATE as f64;
    let started = Instant::now();
    let t = transcribe_local(
        &pcm,
        LocalModelId::Multilingual,
        None,
        &mut |_| {},
        &CancelToken::new(),
    )
    .unwrap();
    show("multilingual", &t, sec, started.elapsed().as_secs_f64());
    assert!(!t.segments.is_empty());
    assert_eq!(t.language.as_deref(), Some("es"));

    // Longer than one 28 s Whisper window: several pieces, in order, none overlapping.
    let mut pcm = Vec::new();
    while pcm.len() < 70 * SAMPLE_RATE as usize {
        for s in &SENTENCES[..3] {
            pcm.extend(speak("en-us", s).expect("espeak-ng"));
            pcm.extend(silence(0.8));
        }
    }
    let sec = pcm.len() as f64 / SAMPLE_RATE as f64;
    let started = Instant::now();
    let t = transcribe_local(
        &pcm,
        LocalModelId::Multilingual,
        None,
        &mut |_| {},
        &CancelToken::new(),
    )
    .unwrap();
    show(
        "multilingual, long",
        &t,
        sec,
        started.elapsed().as_secs_f64(),
    );
    assert!(t.segments.len() >= 3);
    assert!(t.gaps.is_empty());
    assert_eq!(t.language.as_deref(), Some("en"));
    for w in t.segments.windows(2) {
        assert!(w[0].end_ms <= w[1].start_ms);
    }
}
