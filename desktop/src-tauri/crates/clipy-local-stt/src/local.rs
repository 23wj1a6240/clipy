//! On-device transcription with sherpa-onnx. Nothing leaves the machine.
//!
//! The recognizer runs on its own worker thread, one piece at a time, and
//! every decode is wrapped in `catch_unwind`: a piece that fails or panics
//! becomes a transcript gap and the worker is replaced, rather than the
//! whole transcript failing. A native abort inside onnxruntime cannot be
//! caught in-process, which is why pieces never exceed the model's limit.

use std::collections::{HashMap, VecDeque};
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, RecvTimeoutError};
use std::sync::Arc;
use std::thread;
use std::time::Duration;

use crate::audio::{detect_silences, duration_sec, slice_secs, SILENCE_MIN_SEC, SILENCE_NOISE_DB};
use crate::chunking::{
    group_tokens_into_segments, plan_chunks, speech_sec_in, to_timeline, tokens_in_core,
    trim_to_speech, ChunkPlan, ChunkPlanOptions, RelativeSegment,
};
use crate::engine::{PieceAnswer, Recognizer};
use crate::error::{Result, SttError};
use crate::models::{model_status, LocalModelId};
use crate::{CancelToken, Gap, Transcript};

/// Whether this build carries the sherpa-onnx engine (the `local` feature).
pub const LOCAL_ENGINE_AVAILABLE: bool = cfg!(feature = "local");

/// Piece sizes per model. Parakeet aborts on input much past 3 minutes, so a
/// piece is never longer than 180 s (about 130 s preferred, cut in a silence,
/// with 2.5 s of overlap so a word at a seam is whole in one piece). Whisper
/// reads at most 30 s, so pieces stay under 28 s with no overlap (it gives no
/// word times to de-duplicate with).
pub fn chunking_for(model: LocalModelId) -> ChunkPlanOptions {
    match model {
        LocalModelId::EnglishFast => ChunkPlanOptions {
            target_sec: 130.0,
            max_sec: 180.0,
            overlap_sec: 2.5,
            min_sec: 15.0,
        },
        LocalModelId::Multilingual => ChunkPlanOptions {
            target_sec: 24.0,
            max_sec: 28.0,
            overlap_sec: 0.0,
            min_sec: 5.0,
        },
    }
}

/// Pieces with less speech than this are skipped (Whisper invents words in silence).
const MIN_SPEECH_SEC: f64 = 0.3;
/// Workers that die in a row before the rest of the recording is given up as gaps.
const MAX_CONSECUTIVE_DEATHS: usize = 3;
const POLL: Duration = Duration::from_millis(50);

/// Transcribe 16 kHz mono samples with a downloaded model. `model_dir` is the
/// models root (the default model dir when `None`). Fails with
/// [`SttError::ModelNotInstalled`] when the model is missing; nothing is ever
/// downloaded here.
pub fn transcribe_local(
    pcm: &[f32],
    model: LocalModelId,
    model_dir: Option<&Path>,
    progress: &mut dyn FnMut(f64),
    cancel: &CancelToken,
) -> Result<Transcript> {
    let status = model_status(model, model_dir);
    if !status.installed {
        let spec = model.spec();
        return Err(SttError::ModelNotInstalled {
            model: model.as_str(),
            label: spec.label,
            size_bytes: spec.size_bytes,
        });
    }
    if !LOCAL_ENGINE_AVAILABLE {
        return Err(SttError::Engine(
            "This build of Clipy has no local speech engine.".into(),
        ));
    }
    cancel.check()?;
    let threads = thread::available_parallelism()
        .map(|n| n.get() / 2)
        .unwrap_or(1)
        .clamp(1, 4) as i32;
    let dir = status.dir.clone();
    let factory: Factory = Arc::new(move || {
        Recognizer::new(model, &dir, threads)
            .map(|r| Box::new(RecognizerDecoder(r)) as Box<dyn Decoder>)
    });
    transcribe_pieces(pcm, model, factory, progress, cancel)
}

pub(crate) trait Decoder {
    fn decode(&mut self, samples: &[f32]) -> std::result::Result<PieceAnswer, String>;
}

struct RecognizerDecoder(Recognizer);

impl Decoder for RecognizerDecoder {
    fn decode(&mut self, samples: &[f32]) -> std::result::Result<PieceAnswer, String> {
        self.0.decode(samples)
    }
}

/// Builds a decoder on the worker thread.
pub(crate) type Factory =
    Arc<dyn Fn() -> std::result::Result<Box<dyn Decoder>, String> + Send + Sync>;

enum Msg {
    Done {
        index: usize,
        answer: Option<PieceAnswer>,
        panicked: bool,
    },
    Fatal(String),
}

struct Job {
    index: usize,
    samples: Vec<f32>,
}

struct Worker {
    jobs: mpsc::Sender<Job>,
    results: mpsc::Receiver<Msg>,
    stop: Arc<AtomicBool>,
}

impl Worker {
    fn spawn(factory: Factory) -> Result<Self> {
        let (job_tx, job_rx) = mpsc::channel::<Job>();
        let (msg_tx, msg_rx) = mpsc::channel::<Msg>();
        let stop = Arc::new(AtomicBool::new(false));
        let stop_w = stop.clone();
        thread::Builder::new()
            .name("clipy-local-stt".into())
            .spawn(move || {
                let made = catch_unwind(AssertUnwindSafe(|| factory()));
                let mut decoder = match made {
                    Ok(Ok(d)) => d,
                    Ok(Err(e)) => {
                        let _ = msg_tx.send(Msg::Fatal(e));
                        return;
                    }
                    Err(_) => {
                        let _ =
                            msg_tx.send(Msg::Fatal("the speech model could not be loaded".into()));
                        return;
                    }
                };
                while let Ok(job) = job_rx.recv() {
                    if stop_w.load(Ordering::SeqCst) {
                        return;
                    }
                    let out = catch_unwind(AssertUnwindSafe(|| decoder.decode(&job.samples)));
                    let (answer, panicked) = match out {
                        Ok(Ok(a)) => (Some(a), false),
                        Ok(Err(_)) => (None, false),
                        Err(_) => (None, true),
                    };
                    if msg_tx
                        .send(Msg::Done {
                            index: job.index,
                            answer,
                            panicked,
                        })
                        .is_err()
                        || panicked
                    {
                        // A decoder that panicked may be in any state: let a fresh worker take over.
                        return;
                    }
                }
            })
            .map_err(SttError::Io)?;
        Ok(Worker {
            jobs: job_tx,
            results: msg_rx,
            stop,
        })
    }
}

impl Drop for Worker {
    fn drop(&mut self) {
        // The thread finishes any decode in flight, then sees the closed channel and exits.
        self.stop.store(true, Ordering::SeqCst);
    }
}

pub(crate) fn transcribe_pieces(
    pcm: &[f32],
    model: LocalModelId,
    factory: Factory,
    progress: &mut dyn FnMut(f64),
    cancel: &CancelToken,
) -> Result<Transcript> {
    let total = duration_sec(pcm);
    let silences = detect_silences(pcm, SILENCE_NOISE_DB, SILENCE_MIN_SEC);
    let plan = plan_chunks(&silences, total, &chunking_for(model));
    let with_speech: Vec<ChunkPlan> = plan
        .iter()
        .copied()
        .filter(|p| {
            speech_sec_in(
                &silences,
                p.extract_start_sec,
                p.extract_start_sec + p.extract_dur_sec,
            ) >= MIN_SPEECH_SEC
        })
        .collect();
    let skipped = plan.len() - with_speech.len();
    progress(0.0);

    let mut answers: HashMap<usize, Option<PieceAnswer>> = HashMap::new();
    let mut queue: VecDeque<ChunkPlan> = with_speech.iter().copied().collect();
    let mut worker: Option<Worker> = None;
    let mut deaths_in_a_row = 0usize;
    let report = |answers: &HashMap<usize, Option<PieceAnswer>>, progress: &mut dyn FnMut(f64)| {
        progress((answers.len() + skipped) as f64 / plan.len().max(1) as f64);
    };

    while let Some(piece) = queue.front().copied() {
        cancel.check()?;
        if deaths_in_a_row > MAX_CONSECUTIVE_DEATHS {
            for p in queue.drain(..) {
                answers.insert(p.index, None);
            }
            break;
        }
        if worker.is_none() {
            worker = Some(Worker::spawn(factory.clone())?);
        }
        let w = worker.as_ref().expect("worker just spawned");
        let samples = slice_secs(pcm, piece.extract_start_sec, piece.extract_dur_sec).to_vec();
        if w.jobs
            .send(Job {
                index: piece.index,
                samples,
            })
            .is_err()
        {
            // The worker is gone before it got this piece (failed to start).
            worker = drain_fatal(worker.take())?;
            deaths_in_a_row += 1;
            continue;
        }
        loop {
            if cancel.is_cancelled() {
                drop(worker.take());
                return Err(SttError::Cancelled);
            }
            let w = worker.as_ref().expect("worker present while waiting");
            match w.results.recv_timeout(POLL) {
                Ok(Msg::Done {
                    index,
                    answer,
                    panicked,
                }) => {
                    queue.pop_front();
                    if answer.is_some() {
                        deaths_in_a_row = 0;
                    }
                    answers.insert(index, answer);
                    if panicked {
                        worker = None;
                        deaths_in_a_row += 1;
                    }
                    report(&answers, progress);
                    break;
                }
                Ok(Msg::Fatal(msg)) => {
                    return Err(SttError::Engine(format!(
                        "Local transcription failed: {msg}"
                    )))
                }
                Err(RecvTimeoutError::Timeout) => continue,
                Err(RecvTimeoutError::Disconnected) => {
                    // The worker died mid-piece: that piece counts as failed.
                    queue.pop_front();
                    answers.insert(piece.index, None);
                    worker = None;
                    deaths_in_a_row += 1;
                    report(&answers, progress);
                    break;
                }
            }
        }
    }
    drop(worker);

    // Token-level merge for models with token times (Parakeet), so a sentence
    // running across a cut is kept once; one segment per piece otherwise (Whisper).
    let mut timed: Vec<(String, f64)> = Vec::new();
    let mut whole_pieces: Vec<RelativeSegment> = Vec::new();
    let mut gaps: Vec<Gap> = Vec::new();
    let mut langs: HashMap<String, usize> = HashMap::new();
    let mut succeeded = 0usize;
    for p in &with_speech {
        let Some(Some(a)) = answers.get(&p.index) else {
            let start = p.keep_start_sec.max(p.extract_start_sec);
            let end = p.keep_end_sec.min(p.extract_start_sec + p.extract_dur_sec);
            gaps.push(Gap {
                start_ms: (start * 1000.0).round() as u64,
                end_ms: (end * 1000.0).round() as u64,
                reason: "the local model could not transcribe this part".into(),
            });
            continue;
        };
        succeeded += 1;
        if let Some(l) = &a.lang {
            *langs.entry(l.clone()).or_default() += 1;
        }
        if !a.tokens.is_empty() && a.timestamps.len() == a.tokens.len() {
            timed.extend(tokens_in_core(p, &a.tokens, &a.timestamps));
        } else {
            let text = a.text.trim();
            let (s, e) = trim_to_speech(
                &silences,
                p.extract_start_sec,
                p.extract_start_sec + p.extract_dur_sec,
            );
            if !text.is_empty() {
                whole_pieces.push(RelativeSegment {
                    start: s,
                    end: e,
                    text: text.to_string(),
                });
            }
        }
    }
    if !with_speech.is_empty() && succeeded == 0 {
        return Err(SttError::Engine(
            "Local transcription failed on every part of this recording.".into(),
        ));
    }
    timed.sort_by(|a, b| a.1.total_cmp(&b.1));
    let (tokens, times): (Vec<String>, Vec<f64>) = timed.into_iter().unzip();
    let mut segs = group_tokens_into_segments(&tokens, &times);
    segs.extend(whole_pieces);
    let segments = to_timeline(segs);
    gaps.sort_by_key(|g| g.start_ms);
    progress(1.0);

    let language = match model {
        LocalModelId::EnglishFast => Some("en".to_string()),
        LocalModelId::Multilingual => {
            let mut v: Vec<(String, usize)> = langs.into_iter().collect();
            v.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
            v.into_iter().next().map(|(l, _)| l)
        }
    };
    Ok(Transcript {
        segments,
        engine: format!("local:{}", model.as_str()),
        language,
        gaps,
    })
}

/// After a failed send: if the worker reported a fatal load error, surface it.
fn drain_fatal(worker: Option<Worker>) -> Result<Option<Worker>> {
    if let Some(w) = worker {
        if let Ok(Msg::Fatal(msg)) = w.results.recv_timeout(Duration::from_secs(1)) {
            return Err(SttError::Engine(format!(
                "Local transcription failed: {msg}"
            )));
        }
    }
    Ok(None)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::audio::SAMPLE_RATE;
    use std::sync::atomic::AtomicUsize;

    /// Speech-like noise (loud) with quiet gaps at the given seconds.
    fn recording(total_sec: f64, quiet: &[(f64, f64)]) -> Vec<f32> {
        let n = (total_sec * SAMPLE_RATE as f64) as usize;
        (0..n)
            .map(|i| {
                let t = i as f64 / SAMPLE_RATE as f64;
                if quiet.iter().any(|(a, b)| t >= *a && t < *b) {
                    0.0
                } else if i % 2 == 0 {
                    0.4
                } else {
                    -0.4
                }
            })
            .collect()
    }

    /// Answers each piece with one word per 2 s of its audio.
    struct Fake {
        fail_len_over_sec: Option<f64>,
        panic_on_call: Option<usize>,
        calls: Arc<AtomicUsize>,
    }

    impl Decoder for Fake {
        fn decode(&mut self, samples: &[f32]) -> std::result::Result<PieceAnswer, String> {
            let call = self.calls.fetch_add(1, Ordering::SeqCst);
            if Some(call) == self.panic_on_call {
                panic!("simulated crash");
            }
            let dur = samples.len() as f64 / SAMPLE_RATE as f64;
            if self.fail_len_over_sec.is_some_and(|m| dur > m) {
                return Err("too long".into());
            }
            let words = (dur / 2.0).floor() as usize;
            Ok(PieceAnswer {
                text: "x".into(),
                tokens: (0..words).map(|i| format!(" w{i}")).collect(),
                timestamps: (0..words).map(|i| i as f64 * 2.0).collect(),
                lang: None,
            })
        }
    }

    fn factory(
        fail_len_over_sec: Option<f64>,
        panic_on_call: Option<usize>,
    ) -> (Factory, Arc<AtomicUsize>) {
        let calls = Arc::new(AtomicUsize::new(0));
        let c = calls.clone();
        let f: Factory = Arc::new(move || {
            Ok(Box::new(Fake {
                fail_len_over_sec,
                panic_on_call,
                calls: c.clone(),
            }) as Box<dyn Decoder>)
        });
        (f, calls)
    }

    #[test]
    fn long_audio_is_fed_in_pieces_under_the_limit_and_merged() {
        let pcm = recording(400.0, &[(128.0, 130.0), (262.0, 264.0)]);
        let (f, calls) = factory(Some(180.0), None);
        let mut prog = Vec::new();
        let t = transcribe_pieces(
            &pcm,
            LocalModelId::EnglishFast,
            f,
            &mut |p| prog.push(p),
            &CancelToken::new(),
        )
        .unwrap();
        assert_eq!(calls.load(Ordering::SeqCst), 3);
        assert!(t.gaps.is_empty());
        assert_eq!(t.engine, "local:english-fast");
        assert_eq!(t.language.as_deref(), Some("en"));
        assert!(!t.segments.is_empty());
        for w in t.segments.windows(2) {
            assert!(w[0].end_ms <= w[1].start_ms);
        }
        assert_eq!(*prog.last().unwrap(), 1.0);
    }

    #[test]
    fn a_failed_or_crashed_piece_becomes_a_gap() {
        let pcm = recording(400.0, &[(128.0, 130.0), (262.0, 264.0)]);
        let (f, calls) = factory(None, Some(1)); // second piece panics
        let t = transcribe_pieces(
            &pcm,
            LocalModelId::EnglishFast,
            f,
            &mut |_| {},
            &CancelToken::new(),
        )
        .unwrap();
        assert_eq!(
            calls.load(Ordering::SeqCst),
            3,
            "a fresh worker took the third piece"
        );
        assert_eq!(t.gaps.len(), 1);
        assert_eq!((t.gaps[0].start_ms, t.gaps[0].end_ms), (129_000, 263_000));
        assert!(!t.segments.is_empty());
    }

    #[test]
    fn every_piece_failing_is_an_error_and_a_load_failure_is_fatal() {
        let pcm = recording(60.0, &[]);
        let (f, _) = factory(Some(1.0), None);
        let err = transcribe_pieces(
            &pcm,
            LocalModelId::EnglishFast,
            f,
            &mut |_| {},
            &CancelToken::new(),
        )
        .unwrap_err();
        assert!(err.to_string().contains("every part"), "{err}");
        let f: Factory = Arc::new(|| Err("the speech model could not be loaded".into()));
        let err = transcribe_pieces(
            &pcm,
            LocalModelId::EnglishFast,
            f,
            &mut |_| {},
            &CancelToken::new(),
        )
        .unwrap_err();
        assert_eq!(
            err.to_string(),
            "Local transcription failed: the speech model could not be loaded"
        );
    }

    #[test]
    fn silent_audio_and_silent_whisper_pieces_are_skipped() {
        let (f, calls) = factory(None, None);
        let t = transcribe_pieces(
            &vec![0.0; 16_000 * 60],
            LocalModelId::Multilingual,
            f,
            &mut |_| {},
            &CancelToken::new(),
        )
        .unwrap();
        assert!(t.segments.is_empty() && t.gaps.is_empty());
        assert_eq!(calls.load(Ordering::SeqCst), 0);
        // 100 s with the middle 50 s silent: Whisper pieces over pure silence are not sent.
        struct TextOnly(Arc<AtomicUsize>);
        impl Decoder for TextOnly {
            fn decode(&mut self, _: &[f32]) -> std::result::Result<PieceAnswer, String> {
                self.0.fetch_add(1, Ordering::SeqCst);
                Ok(PieceAnswer {
                    text: " words ".into(),
                    tokens: vec!["words".into()],
                    timestamps: vec![],
                    lang: Some("de".into()),
                })
            }
        }
        let pcm = recording(100.0, &[(25.0, 75.0)]);
        let calls = Arc::new(AtomicUsize::new(0));
        let c = calls.clone();
        let f: Factory = Arc::new(move || Ok(Box::new(TextOnly(c.clone())) as Box<dyn Decoder>));
        let t = transcribe_pieces(
            &pcm,
            LocalModelId::Multilingual,
            f,
            &mut |_| {},
            &CancelToken::new(),
        )
        .unwrap();
        assert_eq!(t.language.as_deref(), Some("de"));
        assert!(!t.segments.is_empty());
        let sent = calls.load(Ordering::SeqCst);
        let planned = plan_chunks(
            &detect_silences(&pcm, -30.0, 0.4),
            100.0,
            &chunking_for(LocalModelId::Multilingual),
        )
        .len();
        assert!(sent < planned, "{sent} of {planned}");
        // Whisper answers carry no token times here, so each piece is one segment trimmed to speech.
        assert!(t
            .segments
            .iter()
            .all(|s| s.end_ms <= 25_000 || s.start_ms >= 75_000));
    }

    #[test]
    fn cancel_returns_promptly() {
        struct Slow;
        impl Decoder for Slow {
            fn decode(&mut self, _: &[f32]) -> std::result::Result<PieceAnswer, String> {
                thread::sleep(Duration::from_millis(400));
                Ok(PieceAnswer::default())
            }
        }
        let f: Factory = Arc::new(|| Ok(Box::new(Slow) as Box<dyn Decoder>));
        let pcm = recording(400.0, &[]);
        let cancel = CancelToken::new();
        let c = cancel.clone();
        let started = std::time::Instant::now();
        let err = transcribe_pieces(
            &pcm,
            LocalModelId::EnglishFast,
            f,
            &mut |p| {
                if p > 0.0 {
                    c.cancel()
                }
            },
            &cancel,
        )
        .unwrap_err();
        assert!(matches!(err, SttError::Cancelled));
        assert!(
            started.elapsed() < Duration::from_millis(1500),
            "{:?}",
            started.elapsed()
        );
    }

    #[test]
    fn missing_model_names_label_and_size() {
        let dir = tempfile::tempdir().unwrap();
        let err = transcribe_local(
            &[0.0; 16],
            LocalModelId::Multilingual,
            Some(dir.path()),
            &mut |_| {},
            &CancelToken::new(),
        )
        .unwrap_err();
        match err {
            SttError::ModelNotInstalled {
                model, size_bytes, ..
            } => {
                assert_eq!(model, "multilingual");
                assert_eq!(size_bytes, 160_609_290);
            }
            other => panic!("{other}"),
        }
    }
}
