//! Live transcription for Local mode: microphone PCM straight from the capture
//! pump into Apple Speech, while the recording is still running.
//!
//! The file-based path in `apple.m` re-opens the growing fragmented MP4 twice
//! per ten-second chunk. Re-opening is O(file) because AVFoundation walks every
//! `moof` to build the sample table, so catching up is O(N^2) and the worker
//! falls behind the take. Caching the asset is not available as a fix: an
//! `AVURLAsset` never sees fragments written after it was created, so a cached
//! asset would stop advancing the moment it was reused.
//!
//! So nothing here touches the file. `recording.rs` already owns the mic PCM,
//! and this takes a copy on its way to the encoder.
//!
//! The pump must never wait on speech. `push` only does a non-blocking
//! `try_send` onto a bounded queue; a feeder thread owns every call into the
//! speech daemon. If the queue is full the audio is dropped and the gap is
//! recorded rather than stalling capture — a transcript gap is recoverable,
//! a stalled audio pump is not.
//!
//! Two engines sit behind the same C surface. `speech_analyzer.swift` drives
//! Apple's SpeechAnalyzer (macOS 26+), a long-form model with per-token audio
//! ranges; `apple_stream.m` drives `SFSpeechRecognizer`. Every open tries
//! SpeechAnalyzer first and falls back to SFSpeech when it is unavailable: an
//! older macOS, an unsupported language, or a language model the OS has not
//! downloaded yet (SpeechAnalyzer asks for it in the background, so the next
//! take can use it).

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{sync_channel, Receiver, SyncSender, TrySendError};
use std::sync::{Mutex, OnceLock};
use std::thread::JoinHandle;

use anyhow::{anyhow, Result};
use serde_json::Value;

extern "C" {
    fn clipy_stt_open(
        sample_rate: f64,
        channels: std::os::raw::c_uint,
    ) -> *mut std::os::raw::c_char;
    fn clipy_stt_append(
        interleaved: *const f32,
        frames: std::os::raw::c_uint,
    ) -> std::os::raw::c_int;
    fn clipy_stt_session_seconds() -> f64;
    fn clipy_stt_rotate() -> *mut std::os::raw::c_char;
    fn clipy_stt_finish() -> *mut std::os::raw::c_char;
    fn clipy_stt_close();
    fn clipy_stt_free(value: *mut std::os::raw::c_char);

    fn clipy_sa_open(sample_rate: f64, channels: std::os::raw::c_uint)
        -> *mut std::os::raw::c_char;
    fn clipy_sa_append(
        interleaved: *const f32,
        frames: std::os::raw::c_uint,
    ) -> std::os::raw::c_int;
    fn clipy_sa_session_seconds() -> f64;
    fn clipy_sa_rotate() -> *mut std::os::raw::c_char;
    fn clipy_sa_finish() -> *mut std::os::raw::c_char;
    fn clipy_sa_close();
    fn clipy_sa_free(value: *mut std::os::raw::c_char);
}

/// One native transcription engine. Both export the same contract: `open`
/// returns NULL or an owned JSON error, `append` returns 0 (accepted), 1 (the
/// session ended; rotate) or -1 (no session), and `rotate`/`finish` return
/// owned `{"segments":[...]}` JSON on the stream clock of that open.
#[derive(Clone, Copy)]
struct Engine {
    name: &'static str,
    open: unsafe extern "C" fn(f64, std::os::raw::c_uint) -> *mut std::os::raw::c_char,
    append: unsafe extern "C" fn(*const f32, std::os::raw::c_uint) -> std::os::raw::c_int,
    session_seconds: unsafe extern "C" fn() -> f64,
    rotate: unsafe extern "C" fn() -> *mut std::os::raw::c_char,
    finish: unsafe extern "C" fn() -> *mut std::os::raw::c_char,
    close: unsafe extern "C" fn(),
    free: unsafe extern "C" fn(*mut std::os::raw::c_char),
    /// Audio after which the feeder rotates to a fresh native session.
    rotate_after_seconds: Option<f64>,
}

/// Built for long-form audio, so it is never rotated on a timer: a rotation
/// can split a word, and this engine has no per-task length limit to avoid.
const SPEECH_ANALYZER: Engine = Engine {
    name: "SpeechAnalyzer",
    open: clipy_sa_open,
    append: clipy_sa_append,
    session_seconds: clipy_sa_session_seconds,
    rotate: clipy_sa_rotate,
    finish: clipy_sa_finish,
    close: clipy_sa_close,
    free: clipy_sa_free,
    rotate_after_seconds: None,
};

const SF_SPEECH: Engine = Engine {
    name: "SFSpeechRecognizer",
    open: clipy_stt_open,
    append: clipy_stt_append,
    session_seconds: clipy_stt_session_seconds,
    rotate: clipy_stt_rotate,
    finish: clipy_stt_finish,
    close: clipy_stt_close,
    free: clipy_stt_free,
    rotate_after_seconds: Some(ROTATE_SECONDS),
};

/// Preference order. The last engine's open error is the one reported, since
/// it is the one the user can act on (permission, language support).
const ENGINES: [Engine; 2] = [SPEECH_ANALYZER, SF_SPEECH];

/// One `SFSpeechRecognitionTask` is not meant to run unbounded, so on that
/// engine the feeder closes the current one and opens the next at this much
/// audio. A rotation can
/// split a word; the ten-second ranges this replaces split words far more often.
const ROTATE_SECONDS: f64 = 50.0;

/// ~2.5 s of 48 kHz audio at typical cpal buffer sizes. Deep enough to ride out
/// a speech-daemon hiccup, shallow enough that a wedged feeder is visible as a
/// gap instead of unbounded memory.
const QUEUE_DEPTH: usize = 256;

/// A discontinuity larger than this between where the recording clock says a
/// chunk starts and where the fed stream expects it (a pause, or dropped audio)
/// starts a new mapping anchor.
const ANCHOR_TOLERANCE_MS: u64 = 120;

/// Milliseconds of audio handed to the recognizer, kept exact across
/// callbacks. Flooring each callback's own duration is wrong: a 512-frame
/// 48 kHz buffer is 10.667 ms, and adding 10 per callback loses 3.75 s a
/// minute, which the anchor mapping then reads as a run of discontinuities
/// and shifts every word progressively late. Total frames divide exactly.
struct FedClock {
    base_ms: u64,
    frames: u64,
    rate: u32,
}

impl FedClock {
    fn new() -> Self {
        Self {
            base_ms: 0,
            frames: 0,
            rate: 0,
        }
    }

    /// Adds `frames` at `rate` and returns the exact total in milliseconds. A
    /// rate change (a mic switch) folds what was fed so far into the base.
    fn add(&mut self, frames: u64, rate: u32) -> u64 {
        if rate != self.rate {
            self.base_ms = self.ms();
            self.frames = 0;
            self.rate = rate;
        }
        self.frames += frames;
        self.ms()
    }

    fn ms(&self) -> u64 {
        if self.rate == 0 {
            self.base_ms
        } else {
            self.base_ms + self.frames * 1000 / u64::from(self.rate)
        }
    }
}

enum Msg {
    Audio {
        samples: Vec<f32>,
        sample_rate: u32,
        channels: u16,
        recording_ms: u64,
    },
    Finish,
}

#[derive(Default)]
struct Collected {
    segments: Vec<Value>,
    warnings: Vec<String>,
    dropped_chunks: u64,
    fed_ms: u64,
    /// Set when the live transcript cannot be trusted as the whole answer:
    /// the session never opened, or a session failed after accepting audio.
    fatal: bool,
    /// Recording-clock millisecond the file-based path should resume from
    /// when a session failed mid-take. Everything before it was harvested
    /// cleanly and is kept; re-reading the file from here fills the rest.
    resume_from_ms: Option<u64>,
    finished: bool,
}

/// What a finished live session produced.
pub struct LiveResult {
    pub segments: Vec<Value>,
    pub gaps: Vec<String>,
    /// Milliseconds of microphone audio actually handed to the recognizer.
    pub fed_ms: u64,
    /// True when the session ran well enough to be the transcript. False means
    /// the caller should fall back to reading the recording file.
    pub usable: bool,
    /// When not usable because a session failed mid-take: the recording-clock
    /// point to resume the file path from. `segments` up to it are good.
    pub resume_from_ms: Option<u64>,
}

struct Session {
    tx: SyncSender<Msg>,
    collected: std::sync::Arc<Mutex<Collected>>,
    done: std::sync::Arc<(Mutex<bool>, std::sync::Condvar)>,
    join: Option<JoinHandle<()>>,
}

static SESSION: OnceLock<Mutex<Option<Session>>> = OnceLock::new();
/// Read by the capture pump on every microphone chunk, so the common case
/// (cloud recording, no live session) costs an atomic load and nothing else.
static ACTIVE: AtomicBool = AtomicBool::new(false);

fn slot() -> &'static Mutex<Option<Session>> {
    SESSION.get_or_init(|| Mutex::new(None))
}

fn take_string(engine: &Engine, raw: *mut std::os::raw::c_char) -> Option<String> {
    if raw.is_null() {
        return None;
    }
    // SAFETY: Category 8 — FFI contract. Every pointer returned by an engine's
    // entry points is a `strdup`'d NUL-terminated C string that the same
    // engine's `free` owns, and it is read once before being freed here.
    let owned = unsafe { std::ffi::CStr::from_ptr(raw) }
        .to_string_lossy()
        .into_owned();
    unsafe { (engine.free)(raw) };
    Some(owned)
}

/// Opens the first engine that accepts this format. Returns the error of the
/// last one tried when none does.
fn open_engine(sample_rate: u32, channels: u16) -> std::result::Result<Engine, String> {
    let mut last_error = String::from("no transcription engine is available");
    for engine in ENGINES {
        // SAFETY: Category 8 — FFI contract. Returns NULL on success or an
        // owned JSON error string freed by the same engine.
        let error = take_string(&engine, unsafe {
            (engine.open)(f64::from(sample_rate), u32::from(channels))
        });
        match error {
            None => {
                tracing::info!(engine = engine.name, "live transcription opened");
                return Ok(engine);
            }
            Some(error) => {
                tracing::info!(
                    engine = engine.name,
                    "live transcription engine unavailable: {error}"
                );
                last_error = error;
            }
        }
    }
    Err(last_error)
}

/// True when a live session is running and should be fed.
pub fn is_active() -> bool {
    ACTIVE.load(Ordering::Relaxed)
}

/// Milliseconds of microphone audio handed to the recognizer so far, for the
/// progress the Local panel shows while the take is still running.
pub fn progress_ms() -> u64 {
    let Ok(guard) = slot().lock() else { return 0 };
    let Some(session) = guard.as_ref() else {
        return 0;
    };
    session.collected.lock().map(|c| c.fed_ms).unwrap_or(0)
}

/// Starts a live session. Any failure leaves no session behind, so the caller
/// silently keeps the file-based transcript path.
pub fn begin() -> Result<()> {
    let mut guard = slot()
        .lock()
        .map_err(|_| anyhow!("live transcription lock poisoned"))?;
    if guard.is_some() {
        return Err(anyhow!("a live transcription session is already running"));
    }
    let (tx, rx) = sync_channel::<Msg>(QUEUE_DEPTH);
    let collected = std::sync::Arc::new(Mutex::new(Collected::default()));
    let done = std::sync::Arc::new((Mutex::new(false), std::sync::Condvar::new()));
    let feeder_collected = collected.clone();
    let feeder_done = done.clone();
    let join = std::thread::Builder::new()
        .name("clipy-live-stt".into())
        .spawn(move || {
            feed(rx, feeder_collected, feeder_done);
        })?;
    *guard = Some(Session {
        tx,
        collected,
        done,
        join: Some(join),
    });
    ACTIVE.store(true, Ordering::Relaxed);
    Ok(())
}

/// Hands one mic chunk to the feeder. Called from the capture pump, so it never
/// blocks, never allocates beyond the copy, and never calls into Speech.
pub fn push(samples: &[f32], sample_rate: u32, channels: u16, recording_ms: u64) {
    if !ACTIVE.load(Ordering::Relaxed) || samples.is_empty() || sample_rate == 0 || channels == 0 {
        return;
    }
    let Ok(guard) = slot().lock() else { return };
    let Some(session) = guard.as_ref() else {
        return;
    };
    match session.tx.try_send(Msg::Audio {
        samples: samples.to_vec(),
        sample_rate,
        channels,
        recording_ms,
    }) {
        Ok(()) => {}
        Err(TrySendError::Full(_)) => {
            if let Ok(mut collected) = session.collected.lock() {
                collected.dropped_chunks += 1;
            }
        }
        Err(TrySendError::Disconnected(_)) => {}
    }
}

/// Ends the session and returns `(segments, gaps)` on the recording clock.
/// Returns `None` when no session was running.
pub fn finish() -> Option<LiveResult> {
    let mut session = {
        let mut guard = slot().lock().ok()?;
        let taken = guard.take()?;
        ACTIVE.store(false, Ordering::Relaxed);
        taken
    };
    // Queue the sentinel; a full queue means the feeder is still draining, so
    // fall back to a blocking send rather than losing the tail of the take.
    if session.tx.try_send(Msg::Finish).is_err() {
        let _ = session.tx.send(Msg::Finish);
    }
    drop(session.tx);
    let (lock, cv) = &*session.done;
    if let Ok(mut finished) = lock.lock() {
        // The feeder only has the queue left to drain plus one harvest, which
        // the ObjC side already bounds at 30 s per session.
        let deadline = std::time::Duration::from_secs(60);
        let start = std::time::Instant::now();
        while !*finished && start.elapsed() < deadline {
            let (next, timeout) = cv
                .wait_timeout(finished, std::time::Duration::from_millis(200))
                .ok()?;
            finished = next;
            if timeout.timed_out() && start.elapsed() >= deadline {
                break;
            }
        }
    }
    if let Some(join) = session.join.take() {
        let _ = join.join();
    }
    let collected = session.collected.lock().ok()?;
    let mut gaps = collected.warnings.clone();
    if collected.dropped_chunks > 0 {
        gaps.push(format!(
            "0 ms: {} microphone buffers were dropped while transcribing live; those moments may be missing from the transcript.",
            collected.dropped_chunks
        ));
    }
    Some(LiveResult {
        segments: collected.segments.clone(),
        gaps,
        fed_ms: collected.fed_ms,
        resume_from_ms: collected.resume_from_ms,
        // Audio reached the recognizer and the session opened: an empty
        // transcript now means silence, not failure, and re-reading the file
        // would only spend O(N^2) to reach the same answer.
        usable: collected.fed_ms > 0 && !collected.fatal,
    })
}

/// Abandons a session without waiting for its result (cancel or discard).
pub fn abort() {
    let Ok(mut guard) = slot().lock() else { return };
    if let Some(mut session) = guard.take() {
        ACTIVE.store(false, Ordering::Relaxed);
        drop(session.tx);
        // SAFETY: Category 8 — FFI contract. Cancels the task and clears the
        // native session; both engines are safe to close with none open, so
        // this does not need to know which one the feeder picked.
        for engine in ENGINES {
            unsafe { (engine.close)() };
        }
        if let Some(join) = session.join.take() {
            let _ = join.join();
        }
    }
}

/// Maps a stream-relative millisecond onto the recording clock using the
/// anchors recorded whenever fed audio and the recording clock diverged.
fn map_stream_ms(anchors: &[(u64, u64)], stream_ms: u64) -> u64 {
    let mut mapped = stream_ms;
    for (at_stream_ms, recording_ms) in anchors {
        if stream_ms >= *at_stream_ms {
            mapped = recording_ms + (stream_ms - at_stream_ms);
        } else {
            break;
        }
    }
    mapped
}

/// Folds one session's result into `collected`. Returns true when the
/// session did not end cleanly — the recognizer failed or timed out after
/// accepting audio — in which case whatever it did report is not the whole
/// story and the caller must hand the rest of the take to the file path.
fn harvest(
    engine: &Engine,
    raw: *mut std::os::raw::c_char,
    open_base_ms: u64,
    anchors: &[(u64, u64)],
    collected: &Mutex<Collected>,
) -> bool {
    fold_harvest(
        take_string(engine, raw).as_deref(),
        open_base_ms,
        anchors,
        collected,
    )
}

fn fold_harvest(
    text: Option<&str>,
    open_base_ms: u64,
    anchors: &[(u64, u64)],
    collected: &Mutex<Collected>,
) -> bool {
    let Some(text) = text else {
        return true;
    };
    let Ok(value) = serde_json::from_str::<Value>(text) else {
        return true;
    };
    let Ok(mut collected) = collected.lock() else {
        return true;
    };
    if let Some(error) = value["error"].as_str() {
        collected.warnings.push(format!("0 ms: {error}"));
        return true;
    }
    if let Some(warning) = value["warning"].as_str() {
        collected.warnings.push(format!("0 ms: {warning}"));
        // The whole window of a failed session goes back to the file path
        // (`fail_session`), so keeping what it did report would put those
        // words in the transcript twice. With SpeechAnalyzer's unrotated
        // session that window can be the entire take.
        return true;
    }
    for segment in value["segments"].as_array().into_iter().flatten() {
        let start = segment["startMs"].as_u64().unwrap_or(0) + open_base_ms;
        let end = segment["endMs"].as_u64().unwrap_or(start) + open_base_ms;
        let Some(text) = segment["text"].as_str() else {
            continue;
        };
        if text.trim().is_empty() {
            continue;
        }
        let start = map_stream_ms(anchors, start);
        let end = map_stream_ms(anchors, end).max(start);
        collected.segments.push(serde_json::json!({
            "startMs": start,
            "endMs": end,
            "text": text,
        }));
    }
    false
}

/// A session that failed after accepting audio may have dropped speech
/// anywhere inside its window, so the whole window goes back to the file
/// path: everything harvested before it stands, and the file is read from
/// where the failed session began.
fn fail_session(collected: &Mutex<Collected>, anchors: &[(u64, u64)], session_start_ms: u64) {
    if let Ok(mut collected) = collected.lock() {
        collected.fatal = true;
        collected.resume_from_ms = Some(map_stream_ms(anchors, session_start_ms));
    }
}

fn feed(
    rx: Receiver<Msg>,
    collected: std::sync::Arc<Mutex<Collected>>,
    done: std::sync::Arc<(Mutex<bool>, std::sync::Condvar)>,
) {
    let mut format: Option<(u32, u16)> = None;
    // Stream ms already handed to closed `open` lifetimes. The native side
    // restarts its own base at every open, so this carries across them.
    let mut open_base_ms: u64 = 0;
    let mut clock = FedClock::new();
    let mut fed_ms: u64 = 0;
    // Stream ms where the session currently accepting audio began, so a
    // failure can name where the file path has to take over.
    let mut session_start_ms: u64 = 0;
    let mut anchors: Vec<(u64, u64)> = Vec::new();
    // The engine whose session is open, if any.
    let mut open: Option<Engine> = None;

    let note = |message: String| {
        if let Ok(mut collected) = collected.lock() {
            collected.warnings.push(format!("0 ms: {message}"));
        }
    };

    while let Ok(msg) = rx.recv() {
        match msg {
            Msg::Audio {
                samples,
                sample_rate,
                channels,
                recording_ms,
            } => {
                let changed = format.is_some_and(|f| f != (sample_rate, channels));
                if changed {
                    // A mic switch mid-take changes the PCM format, which the
                    // open session cannot describe. Close it out and open the
                    // next one on the new format.
                    if let Some(engine) = open.take() {
                        // SAFETY: Category 8 — FFI contract, session is open.
                        let failed = harvest(
                            &engine,
                            unsafe { (engine.finish)() },
                            open_base_ms,
                            &anchors,
                            &collected,
                        );
                        if failed {
                            fail_session(&collected, &anchors, session_start_ms);
                            break;
                        }
                    }
                    open_base_ms = fed_ms;
                    format = None;
                }
                if format.is_none() {
                    match open_engine(sample_rate, channels) {
                        Ok(engine) => open = Some(engine),
                        Err(error) => {
                            note(format!("Live transcription could not start: {error}"));
                            if let Ok(mut collected) = collected.lock() {
                                collected.fatal = true;
                            }
                            break;
                        }
                    }
                    format = Some((sample_rate, channels));
                    session_start_ms = fed_ms;
                    anchors.push((fed_ms, recording_ms));
                }
                let Some(engine) = open else { break };
                // A pause, or audio dropped by a full queue, moves the
                // recording clock without moving the fed stream. Anchor so the
                // segments after it still land where they were spoken.
                let expected = anchors
                    .last()
                    .map(|(at_stream, at_recording)| at_recording + (fed_ms - at_stream))
                    .unwrap_or(fed_ms);
                if recording_ms.abs_diff(expected) > ANCHOR_TOLERANCE_MS {
                    anchors.push((fed_ms, recording_ms));
                }
                let frames = samples.len() / usize::from(channels).max(1);
                if frames == 0 {
                    continue;
                }
                // SAFETY: Category 8 — FFI contract. `samples` outlives the
                // call; the native side copies into its own buffer before
                // returning and never retains this pointer.
                let accepted =
                    unsafe { (engine.append)(samples.as_ptr(), frames as std::os::raw::c_uint) };
                if accepted == 1 {
                    // The session finished on its own before the rotation
                    // window was up. Harvest it and open the next one, then
                    // re-append this chunk so the audio is not lost.
                    // SAFETY: Category 8 — FFI contract, session is open.
                    let failed = harvest(
                        &engine,
                        unsafe { (engine.rotate)() },
                        open_base_ms,
                        &anchors,
                        &collected,
                    );
                    if failed {
                        fail_session(&collected, &anchors, session_start_ms);
                        // SAFETY: Category 8 — FFI contract; tears down the
                        // session `rotate` just opened.
                        unsafe { (engine.close)() };
                        open = None;
                        break;
                    }
                    session_start_ms = fed_ms;
                    // SAFETY: Category 8 — FFI contract, as above.
                    if unsafe { (engine.append)(samples.as_ptr(), frames as std::os::raw::c_uint) }
                        != 0
                    {
                        continue;
                    }
                } else if accepted != 0 {
                    continue;
                }
                fed_ms = clock.add(frames as u64, sample_rate);
                if let Ok(mut collected) = collected.lock() {
                    collected.fed_ms = fed_ms;
                }
                let Some(limit) = engine.rotate_after_seconds else {
                    continue;
                };
                // SAFETY: Category 8 — FFI contract, returns -1 when closed.
                if unsafe { (engine.session_seconds)() } >= limit {
                    // SAFETY: Category 8 — FFI contract, session is open.
                    let failed = harvest(
                        &engine,
                        unsafe { (engine.rotate)() },
                        open_base_ms,
                        &anchors,
                        &collected,
                    );
                    if failed {
                        fail_session(&collected, &anchors, session_start_ms);
                        // SAFETY: Category 8 — FFI contract; tears down the
                        // session `rotate` just opened.
                        unsafe { (engine.close)() };
                        open = None;
                        break;
                    }
                    session_start_ms = fed_ms;
                }
            }
            Msg::Finish => break,
        }
    }

    if let Some(engine) = open {
        // SAFETY: Category 8 — FFI contract. Ends the session and returns its
        // remaining segments; safe even if the session already failed.
        let failed = harvest(
            &engine,
            unsafe { (engine.finish)() },
            open_base_ms,
            &anchors,
            &collected,
        );
        if failed {
            fail_session(&collected, &anchors, session_start_ms);
        }
    }
    if let Ok(mut collected) = collected.lock() {
        collected.finished = true;
    }
    let (lock, cv) = &*done;
    if let Ok(mut finished) = lock.lock() {
        *finished = true;
    }
    cv.notify_all();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fed_clock_does_not_lose_the_fraction_each_callback_drops() {
        // 512 frames at 48 kHz is 10.667 ms; flooring per callback loses
        // 0.667 ms every time. One minute is 5625 callbacks.
        let mut clock = FedClock::new();
        let mut last = 0;
        for _ in 0..5_625 {
            last = clock.add(512, 48_000);
        }
        assert_eq!(last, 60_000);
    }

    #[test]
    fn fed_clock_carries_the_total_across_a_rate_change() {
        let mut clock = FedClock::new();
        clock.add(48_000, 48_000);
        assert_eq!(clock.add(44_100, 44_100), 2_000);
    }

    #[test]
    fn stream_time_maps_straight_through_without_anchors() {
        assert_eq!(map_stream_ms(&[], 1_500), 1_500);
    }

    #[test]
    fn an_anchor_shifts_everything_after_it_onto_the_recording_clock() {
        // 4 s of audio fed, then a 10 s pause: the next fed second is 14 s into
        // the recording, so a segment 500 ms into it belongs at 14.5 s.
        let anchors = vec![(0, 0), (4_000, 14_000)];
        assert_eq!(map_stream_ms(&anchors, 2_000), 2_000);
        assert_eq!(map_stream_ms(&anchors, 4_500), 14_500);
    }

    #[test]
    fn a_later_anchor_wins_over_an_earlier_one() {
        let anchors = vec![(0, 0), (4_000, 14_000), (9_000, 30_000)];
        assert_eq!(map_stream_ms(&anchors, 9_250), 30_250);
    }

    #[test]
    fn a_clean_harvest_keeps_its_segments_on_the_recording_clock() {
        let collected = Mutex::new(Collected::default());
        let failed = fold_harvest(
            Some(r#"{"segments":[{"startMs":500,"endMs":900,"text":"hello"}]}"#),
            1_000,
            &[(0, 2_000)],
            &collected,
        );
        assert!(!failed);
        let collected = collected.lock().unwrap();
        assert_eq!(collected.segments.len(), 1);
        assert_eq!(collected.segments[0]["startMs"], 3_500);
        assert_eq!(collected.segments[0]["endMs"], 3_900);
    }

    #[test]
    fn a_failed_session_drops_its_segments_so_the_file_path_does_not_repeat_them() {
        // `fail_session` hands this session's whole window back to the file
        // path, which transcribes those words again.
        let collected = Mutex::new(Collected::default());
        let failed = fold_harvest(
            Some(
                r#"{"segments":[{"startMs":0,"endMs":400,"text":"hello"}],"warning":"ended early"}"#,
            ),
            0,
            &[],
            &collected,
        );
        assert!(failed);
        let collected = collected.lock().unwrap();
        assert!(collected.segments.is_empty());
        assert_eq!(collected.warnings, vec!["0 ms: ended early".to_string()]);
    }

    #[test]
    fn a_missing_or_unreadable_harvest_counts_as_failed() {
        let collected = Mutex::new(Collected::default());
        assert!(fold_harvest(None, 0, &[], &collected));
        assert!(fold_harvest(Some("not json"), 0, &[], &collected));
        assert!(fold_harvest(
            Some(r#"{"error":"no session"}"#),
            0,
            &[],
            &collected
        ));
    }

    #[test]
    fn speech_analyzer_is_tried_first_and_only_sfspeech_rotates() {
        assert_eq!(ENGINES[0].name, "SpeechAnalyzer");
        assert_eq!(ENGINES[0].rotate_after_seconds, None);
        assert_eq!(ENGINES[1].name, "SFSpeechRecognizer");
        assert_eq!(ENGINES[1].rotate_after_seconds, Some(ROTATE_SECONDS));
    }

    #[test]
    fn a_recording_that_starts_late_still_lands_on_the_recording_clock() {
        // Local mode opens the session on the first mic chunk, which can be
        // some way into the take; segment zero must not collapse to zero.
        let anchors = vec![(0, 2_400)];
        assert_eq!(map_stream_ms(&anchors, 0), 2_400);
        assert_eq!(map_stream_ms(&anchors, 1_000), 3_400);
    }
}
