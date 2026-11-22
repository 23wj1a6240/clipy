//! cpal-based microphone input.
//!
//! Emits 48 kHz stereo f32 PCM where the device supports it; otherwise
//! captures in the device's native config and logs a warning
//! (`av_writer.rs` wraps each chunk into a `cm::SampleBuf` with a matching
//! `cm::AudioFormatDesc`, so the input doesn't have to be 48 kHz stereo —
//! it just reports what it is).

#![allow(dead_code)]

use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};
use std::time::Duration;

use anyhow::{anyhow, Context, Result};
use cpal::{
    traits::{DeviceTrait, HostTrait, StreamTrait},
    Sample, SampleFormat, Stream, StreamConfig, SupportedStreamConfigRange,
};
use parking_lot::Mutex;
use tokio::sync::mpsc::{self, Receiver};

extern "C" {
    /// Mach host time, monotonically increasing in opaque "mach units".
    /// Same clock domain SCK / `cm::Clock::convert_host_time_to_sys_units`
    /// reports against, which is what the recording pump anchors video +
    /// system-audio PTS to. `HalClock` reads it once per stream to anchor
    /// the HAL's own buffer timestamps onto that timeline.
    fn mach_absolute_time() -> u64;
}

/// Interleaved f32 PCM chunk from the mic.
#[derive(Debug)]
pub struct AudioPcmF32 {
    pub samples: Vec<f32>,
    pub sample_rate: u32,
    pub channels: u16,
    pub generation: MicStreamGeneration,
    /// Capture time of the FIRST sample in `samples`, in mach ticks, with the
    /// device's input pipeline latency already subtracted. Derived from the
    /// HAL's own buffer timestamp (see `HalClock`), not from when the callback
    /// happened to run, so callback jitter and burst delivery do not move it.
    /// Shares a clock domain with SCK video + system-audio PTS — recording.rs
    /// anchors mic PTS off the same `start_mach` origin.
    pub mach_pts: u64,
}

/// Maps HAL input timestamps onto the mach clock the recording pump anchors
/// video and system audio to.
///
/// cpal's `StreamInstant` hides the raw host time, so the first callback
/// anchors HAL time to `mach_absolute_time()` and every later callback
/// follows the HAL clock from that anchor. The HAL stamps the first frame of
/// each buffer with the device clock, so consecutive chunks are exactly one
/// buffer apart regardless of how late the callback thread was scheduled.
struct HalClock {
    numer: u64,
    denom: u64,
    anchor: Option<(u64, cpal::StreamInstant)>,
    latency_ticks: u64,
}

impl HalClock {
    fn new(capture_latency: Duration) -> Self {
        let timebase = cidre::mach::TimeBaseInfo::new();
        let numer = u64::from(timebase.numer.max(1));
        let denom = u64::from(timebase.denom.max(1));
        let mut clock = Self {
            numer,
            denom,
            anchor: None,
            latency_ticks: 0,
        };
        clock.latency_ticks = clock.ns_to_ticks(capture_latency.as_nanos() as u64);
        clock
    }

    fn ns_to_ticks(&self, ns: u64) -> u64 {
        ((ns as u128 * self.denom as u128) / self.numer as u128) as u64
    }

    /// Mach time of the first sample in the buffer this callback delivered.
    fn first_sample_mach(
        &mut self,
        info: &cpal::InputCallbackInfo,
        buffer_frames: usize,
        sample_rate: u32,
    ) -> u64 {
        // `callback` is CoreAudio's `mHostTime` for this buffer: the device
        // clock reading for its first frame. cpal's `capture` field subtracts
        // an extra buffer duration on top of that ("double-buffer offset"),
        // which over-corrects, so it is deliberately not used here.
        let hal = info.timestamp().callback;
        let anchored = self.anchor.and_then(|(anchor_mach, anchor_hal)| {
            hal.duration_since(&anchor_hal).map(|elapsed| {
                anchor_mach.saturating_add(self.ns_to_ticks(elapsed.as_nanos() as u64))
            })
        });
        let raw = match anchored {
            Some(raw) => raw,
            None => {
                // First callback, or the HAL clock stepped backwards: the
                // buffer is complete when the callback runs, so its first
                // frame was captured one buffer-duration ago.
                let now = unsafe { mach_absolute_time() };
                let buffer_ns =
                    buffer_frames as u64 * 1_000_000_000 / u64::from(sample_rate.max(1));
                let first = now.saturating_sub(self.ns_to_ticks(buffer_ns));
                self.anchor = Some((first, hal));
                first
            }
        };
        raw.saturating_sub(self.latency_ticks)
    }
}

/// Everything needed to open one input stream, shared by the legacy
/// `MicCapturer` and the supervised path so both apply the same latency and
/// buffer policy.
struct InputStreamPlan {
    config: StreamConfig,
    sample_format: SampleFormat,
    sample_rate: u32,
    channels: u16,
    latency: crate::capture::input_latency::InputLatencyInfo,
}

fn plan_input_stream(device: &cpal::Device, device_name: &str) -> Result<InputStreamPlan> {
    let chosen = pick_config(device)?;
    let sample_format = chosen.sample_format();
    let supported_buffer = chosen.buffer_size().clone();
    let mut config: StreamConfig = chosen.into();
    let sample_rate = config.sample_rate.0;
    let channels = config.channels;

    let latency = crate::capture::input_latency::estimate_input_latency(device_name);
    if let Some(frames) = crate::capture::input_latency::wireless_buffer_frames(
        latency.transport,
        sample_rate,
        &supported_buffer,
    ) {
        config.buffer_size = cpal::BufferSize::Fixed(frames);
    }
    tracing::info!(
        device = %device_name,
        transport = ?latency.transport,
        capture_latency_ms = latency.capture_latency.as_secs_f64() * 1_000.0,
        buffer_size = ?config.buffer_size,
        "mic input latency plan"
    );

    Ok(InputStreamPlan {
        config,
        sample_format,
        sample_rate,
        channels,
        latency,
    })
}

#[derive(Clone, Copy, Debug, Default, Eq, Ord, PartialEq, PartialOrd)]
pub struct MicStreamGeneration(u64);

impl MicStreamGeneration {
    pub const fn initial() -> Self {
        Self(0)
    }

    pub const fn next(self) -> Self {
        Self(self.0.saturating_add(1))
    }
}

struct MicReopenFade {
    remaining_frames: usize,
    total_frames: usize,
}

impl MicReopenFade {
    fn new(sample_rate: u32, generation: MicStreamGeneration) -> Self {
        let total_frames = if generation == MicStreamGeneration::initial() {
            0
        } else {
            (sample_rate / 100).max(2) as usize
        };
        Self {
            remaining_frames: total_frames,
            total_frames,
        }
    }

    fn apply(&mut self, samples: &mut [f32], channels: u16) {
        if self.remaining_frames == 0 {
            return;
        }
        for frame in samples.chunks_exact_mut(usize::from(channels.max(1))) {
            if self.remaining_frames == 0 {
                break;
            }
            let gain =
                (self.total_frames - self.remaining_frames) as f32 / (self.total_frames - 1) as f32;
            for sample in frame {
                *sample *= gain;
            }
            self.remaining_frames -= 1;
        }
    }
}

const TARGET_SAMPLE_RATE: u32 = 48_000;
const TARGET_CHANNELS: u16 = 2;
// `MIC_INPUT_ACTIVE_LEVEL` now lives in `capture::level` alongside the two
// functions that read it.
/// Depth of the bounded mic PCM queue, in cpal callback buffers.
///
/// The point of the bound is to stop an encoder stall from consuming all RAM,
/// not to keep the queue short: one buffer is ~512 frames * 2ch * 4B ≈ 4 KiB,
/// so 512 slots is ~2 MiB and ~5 s of audio. At the previous depth of 32 the
/// queue held ~340 ms, and any encoder hiccup longer than that dropped mic PCM
/// the old unbounded channel had absorbed silently — dropped chunks become
/// zero-filled gaps in the mix. Stay well inside the mixer's 10 s laggard
/// window so a source that is merely slow is buffered, not truncated.
const MIC_CHANNEL_CAPACITY: usize = 512;

#[derive(Clone, Debug)]
pub struct MicSignalCheck {
    pub device_name: String,
    pub level: f32,
    pub has_input: bool,
}

pub struct MicCapturer {
    // cpal `Stream` is not Send on some platforms; we keep it pinned to the
    // thread that owns it via an `Option` so `stop()` can drop it explicitly.
    stream: Option<Stream>,
    stop_flag: Arc<AtomicBool>,
    /// Actual chosen config, so callers can configure the encoder to match
    /// rather than hardcoding 48 kHz stereo and losing mono mic input.
    pub device_name: String,
    pub sample_rate: u32,
    pub channels: u16,
}

// SAFETY: we never touch `stream` from another thread; `stop()` drops it in
// place. Marking `Send` lets `MicCapturer` be stored inside the async
// `RecordingController`. cpal's own `scap-cpal` crate does the equivalent.
unsafe impl Send for MicCapturer {}

impl MicCapturer {
    /// Open the system default input device and start streaming. Returns a
    /// receiver that yields interleaved f32 PCM chunks roughly every
    /// 10–20 ms, plus the handle that must outlive the stream.
    pub fn start() -> Result<(Self, Receiver<AudioPcmF32>)> {
        Self::start_with_device(None)
    }

    /// Same as `start`, but pinned to a specific input device. `device_id`
    /// matches the `name()` returned by cpal — see `list_input_devices`.
    /// `None` means "system default".
    pub fn start_with_device(device_id: Option<&str>) -> Result<(Self, Receiver<AudioPcmF32>)> {
        let host = cpal::default_host();
        let device = match device_id {
            Some(target) => host
                .input_devices()
                .context("enumerate input devices")?
                .find(|d| d.name().map(|n| n == target).unwrap_or(false))
                .ok_or_else(|| anyhow!("requested mic device not found: {target}"))?,
            None => host
                .default_input_device()
                .ok_or_else(|| anyhow!("no default input device available"))?,
        };
        let device_name = device.name().unwrap_or_else(|_| "<unknown>".into());

        // Prefer 48 kHz / stereo / f32, but fall back gracefully.
        let InputStreamPlan {
            config,
            sample_format,
            sample_rate,
            channels,
            latency,
        } = plan_input_stream(&device, &device_name)?;

        if sample_rate != TARGET_SAMPLE_RATE || channels != TARGET_CHANNELS {
            tracing::warn!(
                device = %device_name,
                sample_rate,
                channels,
                "mic device does not match 48kHz stereo — encoder will need to resample"
            );
        } else {
            tracing::info!(device = %device_name, "mic capture at 48kHz stereo f32");
        }

        // A realtime callback must never block, but an unbounded bridge can
        // consume all RAM if the encoder stalls. try_send drops at the same
        // bounded capture boundary used by video/system audio.
        let (tx, rx) = mpsc::channel::<AudioPcmF32>(MIC_CHANNEL_CAPACITY);

        let stop_flag = Arc::new(AtomicBool::new(false));

        let err_fn = |err: cpal::StreamError| match err {
            // Unplugged headset / disabled endpoint: an expected event on a
            // user's machine, not a bug. Kept as a warning (Sentry breadcrumb).
            cpal::StreamError::DeviceNotAvailable => {
                tracing::warn!("cpal input stream error: {err}")
            }
            _ => tracing::error!("cpal input stream error: {err}"),
        };

        let stream = build_stream(
            &device,
            &config,
            sample_format,
            tx,
            stop_flag.clone(),
            Arc::new(AtomicBool::new(false)),
            sample_rate,
            channels,
            MicStreamGeneration::initial(),
            latency.capture_latency,
            err_fn,
        )?;

        stream.play().context("start cpal input stream")?;

        Ok((
            Self {
                stream: Some(stream),
                stop_flag,
                device_name,
                sample_rate,
                channels,
            },
            rx,
        ))
    }

    pub fn stop(mut self) -> Result<()> {
        self.stop_flag.store(true, Ordering::Release);
        if let Some(stream) = self.stream.take() {
            // Dropping the stream halts the audio thread and closes the
            // sender side, which terminates the forwarder task.
            drop(stream);
        }
        tracing::info!("mic capture stopped");
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Supervised mic capture
// ---------------------------------------------------------------------------
//
// The plain `MicCapturer` pins whatever device it opened for the whole
// recording; if that device disappears mid-recording (user unplugs the
// earphones whose mic was active) cpal only LOGS the stream error and the
// rest of the recording has a silent mic. The supervisor owns the live
// stream plus a watcher task that rebuilds it when:
//   - the default stream errors or disappears → reopen the current default,
//     while an explicitly selected device is muted until that exact device
//     returns, and
//   - the selection is "system default" and the OS default changes
//     (AirPods connected mid-recording) → follow it, like native apps, and
//   - an explicitly-picked device comes back → reopen that exact device.
//
// A swap needs no downstream coordination: chunks are self-describing
// (`sample_rate`/`channels` travel per chunk and the mixer resamples per
// chunk) and PTS comes from each stream's HAL capture clock (`HalClock`),
// so the mixer just sees a few-ms gap on the shared timeline.

const SUPERVISOR_POLL: std::time::Duration = std::time::Duration::from_millis(300);
const SUPERVISOR_STOP_GRANULARITY: std::time::Duration = std::time::Duration::from_millis(100);

/// Lives entirely on the dedicated supervisor thread: created, swapped, and
/// dropped there — never crosses threads, so no `unsafe impl Send` needed
/// (unlike the legacy `MicCapturer`, whose stream moves into async state).
struct SupervisedStream {
    stream: Option<Stream>,
    stop_flag: Arc<AtomicBool>,
    err_flag: Arc<AtomicBool>,
    device_name: String,
    generation: MicStreamGeneration,
}

/// How long a halted input stream is kept alive before it is freed.
///
/// `AudioOutputUnitStop` does not promise the HAL has left our render callback
/// by the time it returns, and dropping the cpal `Stream` frees the boxed
/// closure CoreAudio still holds a pointer to. A callback that lands in that
/// window dereferences freed memory and the process dies inside
/// `HALC_ProxyIOContext::IOWorkLoop`. That is the 0.1.56 "Clipy quit
/// unexpectedly" crash: EXC_BAD_ACCESS on `com.apple.audio.IOThread.client`
/// with frame 0 in our own binary, reproduced by switching between an external
/// mic and the built-in one mid-recording. Halting first and freeing a few
/// poll cycles later closes the window. Callbacks are only tens of
/// milliseconds apart, so this is a very wide margin, and it still fits inside
/// the two-second budget `MicSupervisor::stop` allows the worker.
const STREAM_RETIRE_GRACE: std::time::Duration = std::time::Duration::from_millis(750);

impl SupervisedStream {
    /// Silence the lane and stop the AudioUnit, handing the stream back so the
    /// caller can free it once CoreAudio has certainly left our callback.
    ///
    /// Deliberately never drops the stream inline — see `STREAM_RETIRE_GRACE`.
    /// The returned stream is stopped, so it no longer competes for the
    /// device's route even while it waits to be freed.
    fn halt(&mut self) -> Option<Stream> {
        self.stop_flag.store(true, Ordering::Release);
        let stream = self.stream.take();
        if let Some(stream) = stream.as_ref() {
            let _ = stream.pause();
        }
        stream
    }
}

impl Drop for SupervisedStream {
    fn drop(&mut self) {
        // Defensive: every path in `supervise_blocking` retires its stream
        // explicitly, so this normally finds nothing. If it does find one
        // (an unwind), stop it and wait before freeing rather than handing
        // CoreAudio a dangling closure.
        if let Some(stream) = self.halt() {
            std::thread::sleep(STREAM_RETIRE_GRACE);
            drop(stream);
        }
    }
}

/// Stopped input streams waiting out `STREAM_RETIRE_GRACE` before being freed.
/// Lives on the supervisor thread, like every `Stream` it holds.
struct RetiredStreams<S = Stream> {
    pending: Vec<(std::time::Instant, S)>,
}

impl<S> Default for RetiredStreams<S> {
    fn default() -> Self {
        Self {
            pending: Vec::new(),
        }
    }
}

impl<S> RetiredStreams<S> {
    fn retire(&mut self, stream: S) {
        self.pending.push((std::time::Instant::now(), stream));
    }

    /// Frees anything stopped longer ago than the grace period.
    fn collect(&mut self) {
        let now = std::time::Instant::now();
        self.pending
            .retain(|(halted_at, _)| now.duration_since(*halted_at) < STREAM_RETIRE_GRACE);
    }

    /// Thread teardown: wait out the newest entry's grace once, then free all.
    fn drain_blocking(&mut self) {
        if let Some(newest) = self.pending.iter().map(|(at, _)| *at).max() {
            let waited = newest.elapsed();
            if waited < STREAM_RETIRE_GRACE {
                std::thread::sleep(STREAM_RETIRE_GRACE - waited);
            }
        }
        self.pending.clear();
    }
}

/// What the supervisor did, reported to the recorder for the HUD + notification.
///
/// This is an enum rather than a formatted string because the two states have
/// different UI lifetimes: a switch is transient ("Mic → AirPods", fades), while
/// a pause persists until the exact device returns and the HUD must keep saying
/// so. Encoding that distinction in prose the recorder has to re-parse is how a
/// persistent state ends up rendered as a 4-second toast.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MicSupervisorEvent<'a> {
    /// Capture is live on `device`.
    Switched { device: &'a str },
    /// The explicitly-selected `device` is gone. Capture is muted and stays
    /// muted until that exact device returns; it is never substituted.
    Paused { device: &'a str },
}

impl MicSupervisorEvent<'_> {
    pub fn device(&self) -> &str {
        match self {
            Self::Switched { device } | Self::Paused { device } => device,
        }
    }

    pub fn is_paused(&self) -> bool {
        matches!(self, Self::Paused { .. })
    }
}

pub type MicSupervisorCallback = Box<dyn Fn(MicSupervisorEvent<'_>) + Send + Sync>;

pub struct MicSupervisor {
    /// Silences every present and future stream's callback immediately.
    mute_all: Arc<AtomicBool>,
    stop: Arc<AtomicBool>,
    worker_done: Option<std::sync::mpsc::Receiver<()>>,
}

impl MicSupervisor {
    /// Open the requested (or default) input device and start the watcher.
    /// `on_switch` fires with the new device name after every successful
    /// mid-recording swap — the recorder surfaces it as a notification.
    pub fn start(
        device_id: Option<String>,
        on_switch: MicSupervisorCallback,
    ) -> Result<(Self, Receiver<AudioPcmF32>)> {
        let (fwd_tx, fwd_rx) = mpsc::channel::<AudioPcmF32>(MIC_CHANNEL_CAPACITY);
        let mute_all = Arc::new(AtomicBool::new(false));
        let stop = Arc::new(AtomicBool::new(false));

        let (init_tx, init_rx) = std::sync::mpsc::channel::<Result<()>>();
        let (done_tx, done_rx) = std::sync::mpsc::channel();
        {
            let fwd_tx = fwd_tx.clone();
            let mute_all = mute_all.clone();
            let stop = stop.clone();
            let worker = std::thread::Builder::new()
                .name("mic-supervisor".into())
                .spawn(move || {
                    match open_supervised_stream(
                        device_id.as_deref(),
                        &fwd_tx,
                        &mute_all,
                        MicStreamGeneration::initial(),
                    ) {
                        Ok(initial) => {
                            let _ = init_tx.send(Ok(()));
                            supervise_blocking(
                                initial, device_id, fwd_tx, mute_all, stop, on_switch,
                            );
                        }
                        Err(err) => {
                            let _ = init_tx.send(Err(err));
                        }
                    }
                    let _ = done_tx.send(());
                })
                .context("spawn mic supervisor thread")?;
            drop(worker);
        }

        match init_rx.recv_timeout(std::time::Duration::from_secs(5)) {
            Ok(Ok(())) => Ok((
                Self {
                    mute_all,
                    stop,
                    worker_done: Some(done_rx),
                },
                fwd_rx,
            )),
            Ok(Err(err)) => Err(err),
            Err(err) => {
                mute_all.store(true, Ordering::Release);
                stop.store(true, Ordering::Release);
                Err(anyhow!("mic supervisor init timed out: {err}"))
            }
        }
    }

    pub fn stop(mut self) -> Result<()> {
        self.mute_all.store(true, Ordering::Release);
        self.stop.store(true, Ordering::Release);
        if self
            .worker_done
            .take()
            .is_some_and(|done| done.recv_timeout(Duration::from_secs(2)).is_err())
        {
            tracing::warn!("mic supervisor stop timed out; worker remains cancelled");
        }
        tracing::info!("mic supervisor stopped");
        Ok(())
    }
}

impl Drop for MicSupervisor {
    fn drop(&mut self) {
        self.mute_all.store(true, Ordering::Release);
        self.stop.store(true, Ordering::Release);
    }
}

fn default_input_name() -> Option<String> {
    cpal::default_host()
        .default_input_device()
        .and_then(|d| d.name().ok())
}

fn input_device_names() -> Vec<String> {
    cpal::default_host()
        .input_devices()
        .map(|devices| devices.filter_map(|d| d.name().ok()).collect())
        .unwrap_or_default()
}

/// The connected device an explicitly selected mic maps to: the exact name
/// when present, otherwise a case-insensitive containment match either way.
/// Bluetooth headsets come back under a slightly different name after a
/// profile switch ("AirPods Pro" vs "Alex's AirPods Pro"), and someone who
/// picked that headset wants it back, not a muted recording. Same rule as
/// Cap's `find_mic_by_label_or_fuzzy`.
fn match_input_device<'a>(devices: &'a [String], target: &str) -> Option<&'a str> {
    if let Some(exact) = devices.iter().find(|name| name.as_str() == target) {
        return Some(exact.as_str());
    }
    let target_lower = target.to_lowercase();
    if target_lower.trim().is_empty() {
        return None;
    }
    devices
        .iter()
        .find(|name| {
            let name_lower = name.to_lowercase();
            name_lower.contains(&target_lower) || target_lower.contains(&name_lower)
        })
        .map(String::as_str)
}

fn open_supervised_stream(
    device_id: Option<&str>,
    fwd_tx: &mpsc::Sender<AudioPcmF32>,
    mute_all: &Arc<AtomicBool>,
    generation: MicStreamGeneration,
) -> Result<SupervisedStream> {
    let host = cpal::default_host();
    let device = match device_id {
        Some(target) => host
            .input_devices()
            .context("enumerate input devices")?
            .find(|d| d.name().map(|n| n == target).unwrap_or(false))
            .ok_or_else(|| anyhow!("requested mic device not found: {target}"))?,
        None => host
            .default_input_device()
            .ok_or_else(|| anyhow!("no default input device available"))?,
    };
    let device_name = device.name().unwrap_or_else(|_| "<unknown>".into());

    let InputStreamPlan {
        config,
        sample_format,
        sample_rate,
        channels,
        latency,
    } = plan_input_stream(&device, &device_name)?;
    tracing::info!(
        device = %device_name,
        sample_rate,
        channels,
        "mic supervisor opening input stream"
    );

    let stream_stop = Arc::new(AtomicBool::new(false));

    let err_flag = Arc::new(AtomicBool::new(false));
    let err_flag_cb = err_flag.clone();
    let stop_flag_cb = stream_stop.clone();
    let err_fn = move |err: cpal::StreamError| {
        // A route failure can leave callbacks delivering invalid/stale PCM
        // until the supervisor reopens the stream. Silence that lane now.
        stop_flag_cb.store(true, Ordering::Release);
        tracing::warn!("cpal input stream error (supervisor will rebuild): {err}");
        err_flag_cb.store(true, Ordering::Release);
    };

    let stream = build_stream(
        &device,
        &config,
        sample_format,
        fwd_tx.clone(),
        stream_stop.clone(),
        mute_all.clone(),
        sample_rate,
        channels,
        generation,
        latency.capture_latency,
        err_fn,
    )?;

    stream.play().context("start cpal input stream")?;

    Ok(SupervisedStream {
        stream: Some(stream),
        stop_flag: stream_stop,
        err_flag,
        device_name,
        generation,
    })
}

/// Runs on the dedicated supervisor thread. Blocking sleeps in
/// SUPERVISOR_STOP_GRANULARITY steps keep stop latency low while the
/// device checks themselves happen once per SUPERVISOR_POLL.
fn supervise_blocking(
    mut current: SupervisedStream,
    explicit: Option<String>,
    fwd_tx: mpsc::Sender<AudioPcmF32>,
    mute_all: Arc<AtomicBool>,
    stop: Arc<AtomicBool>,
    on_switch: MicSupervisorCallback,
) {
    let mut explicit_paused = false;
    let mut retired = RetiredStreams::default();
    'outer: loop {
        let mut waited = std::time::Duration::ZERO;
        while waited < SUPERVISOR_POLL {
            std::thread::sleep(SUPERVISOR_STOP_GRANULARITY);
            waited += SUPERVISOR_STOP_GRANULARITY;
            if stop.load(Ordering::Acquire) {
                break 'outer;
            }
        }
        // Free anything halted long enough ago that CoreAudio cannot still be
        // inside its callback.
        retired.collect();

        let errored = current.err_flag.load(Ordering::Acquire);
        if let Some(target) = explicit.as_deref() {
            let devices = input_device_names();
            let matched = match_input_device(&devices, target).map(str::to_owned);
            let action = explicit_device_action(
                matched.is_some(),
                explicit_paused,
                errored,
                matched.as_deref() == Some(current.device_name.as_str()),
            );
            match action {
                ExplicitDeviceAction::Keep | ExplicitDeviceAction::Wait => continue,
                ExplicitDeviceAction::Pause => {
                    explicit_paused = true;
                    mute_all.store(true, Ordering::Release);
                    if let Some(stream) = current.halt() {
                        retired.retire(stream);
                    }
                    tracing::warn!(
                        "selected microphone disconnected; capture muted until it returns"
                    );
                    on_switch(MicSupervisorEvent::Paused { device: target });
                    continue;
                }
                ExplicitDeviceAction::Reopen => {
                    let reopen_name = matched.as_deref().unwrap_or(target);
                    // Stop the old AudioUnit before negotiating the new
                    // route. Two RUNNING inputs can compete for a Bluetooth
                    // profile while its format is changing; a halted one does
                    // not, so it is safe to let it wait out its grace period
                    // here rather than freeing it under CoreAudio's feet.
                    if let Some(stream) = current.halt() {
                        retired.retire(stream);
                    }
                    match open_supervised_stream(
                        Some(reopen_name),
                        &fwd_tx,
                        &mute_all,
                        current.generation.next(),
                    ) {
                        Ok(next) => {
                            if stop.load(Ordering::Acquire) {
                                break 'outer;
                            }
                            let previous = std::mem::replace(&mut current, next);
                            let from = previous.device_name.clone();
                            drop(previous);
                            explicit_paused = false;
                            mute_all.store(false, Ordering::Release);
                            tracing::info!(
                                from = %from,
                                to = %current.device_name,
                                renamed = reopen_name != target,
                                errored,
                                "mic supervisor reopened selected input device"
                            );
                            on_switch(MicSupervisorEvent::Switched {
                                device: &current.device_name,
                            });
                        }
                        Err(err) => {
                            explicit_paused = true;
                            mute_all.store(true, Ordering::Release);
                            tracing::warn!("selected mic rebuild failed; remaining muted");
                            tracing::debug!(error = ?err, "selected mic rebuild failure details");
                        }
                    }
                    continue;
                }
            }
        }

        let default = default_input_name();
        let need_switch = default_device_needs_reopen(
            current.stream.is_some(),
            errored,
            default.as_deref(),
            &current.device_name,
        );
        if !need_switch {
            continue;
        }

        // An explicitly-picked device never reaches here — `explicit_device_action`
        // above owns that case and refuses to substitute anything else. This tail
        // only follows the OS default.
        if let Some(stream) = current.halt() {
            retired.retire(stream);
        }
        match open_supervised_stream(None, &fwd_tx, &mute_all, current.generation.next()) {
            Ok(next) => {
                let previous = std::mem::replace(&mut current, next);
                let from = previous.device_name.clone();
                drop(previous);
                tracing::info!(
                    from = %from,
                    to = %current.device_name,
                    errored,
                    "mic supervisor switched input device"
                );
                on_switch(MicSupervisorEvent::Switched {
                    device: &current.device_name,
                });
            }
            Err(err) => {
                tracing::warn!("mic supervisor rebuild failed (retrying): {err:#}");
            }
        }
    }
    // Halt the live stream and free everything through the same grace period,
    // so the teardown path cannot hand CoreAudio a dangling closure either.
    if let Some(stream) = current.halt() {
        retired.retire(stream);
    }
    retired.drain_blocking();
}

fn default_device_needs_reopen(
    stream_active: bool,
    errored: bool,
    default: Option<&str>,
    current: &str,
) -> bool {
    !stream_active || errored || default != Some(current)
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ExplicitDeviceAction {
    Keep,
    Pause,
    Wait,
    Reopen,
}

fn explicit_device_action(
    target_present: bool,
    paused: bool,
    errored: bool,
    current_is_target: bool,
) -> ExplicitDeviceAction {
    if !target_present {
        return if paused {
            ExplicitDeviceAction::Wait
        } else {
            ExplicitDeviceAction::Pause
        };
    }
    if paused || errored || !current_is_target {
        ExplicitDeviceAction::Reopen
    } else {
        ExplicitDeviceAction::Keep
    }
}

/// Enumerate input devices for the pre-record UI. The `id` returned for
/// each entry is the cpal device name — the same value the UI feeds back
/// into `start_recording` via `AudioOptionsDto::mic_device_id`.
pub fn list_input_devices() -> Result<Vec<(String, bool)>> {
    let host = cpal::default_host();
    let default_name = host
        .default_input_device()
        .and_then(|d| d.name().ok())
        .unwrap_or_default();
    let mut out = Vec::new();
    for device in host.input_devices().context("enumerate input devices")? {
        if let Ok(name) = device.name() {
            let is_default = !default_name.is_empty() && name == default_name;
            out.push((name, is_default));
        }
    }
    Ok(out)
}

// Moved to `capture::level` so the shared `PauseState::observe_mic_samples`
// can use them on Windows, where this module is not compiled. Re-exported
// here so `crate::capture::audio::{samples_level, level_has_input}` keeps
// working for every existing macOS caller.
pub use super::level::{level_has_input, samples_level};

pub async fn check_input_signal(
    device_id: Option<&str>,
    duration: Duration,
) -> Result<MicSignalCheck> {
    check_input_signal_with_updates(device_id, duration, |_| {}).await
}

pub async fn check_input_signal_with_updates<F>(
    device_id: Option<&str>,
    duration: Duration,
    mut on_level: F,
) -> Result<MicSignalCheck>
where
    F: FnMut(f32) + Send,
{
    let device_name = Arc::new(Mutex::new(
        device_id
            .map(str::to_owned)
            .or_else(default_input_name)
            .unwrap_or_else(|| "System default".to_string()),
    ));
    let device_name_for_switch = device_name.clone();
    let on_switch: MicSupervisorCallback = Box::new(move |event| {
        // A pause means the selected device vanished — keep reporting the name
        // the caller asked for rather than overwriting it with a pause label.
        if let MicSupervisorEvent::Switched { device } = event {
            *device_name_for_switch.lock() = device.to_string();
        }
    });
    let requested_device = device_id.map(str::to_owned);
    let (capturer, mut rx) =
        tokio::task::spawn_blocking(move || MicSupervisor::start(requested_device, on_switch))
            .await
            .context("join mic supervisor startup")??;
    let deadline = tokio::time::Instant::now() + duration;
    let mut max_level = 0.0f32;
    let mut last_level_update = tokio::time::Instant::now() - Duration::from_millis(100);
    loop {
        tokio::select! {
            _ = tokio::time::sleep_until(deadline) => break,
            maybe_chunk = rx.recv() => {
                let Some(chunk) = maybe_chunk else {
                    break;
                };
                let level = samples_level(&chunk.samples);
                max_level = max_level.max(level);
                if last_level_update.elapsed() >= Duration::from_millis(50) {
                    on_level(level);
                    last_level_update = tokio::time::Instant::now();
                }
            }
        }
    }
    if let Err(err) = tokio::task::spawn_blocking(move || capturer.stop())
        .await
        .context("join mic supervisor stop")?
    {
        tracing::warn!("mic signal check stop failed: {err:#}");
    }
    let checked_device_name = device_name.lock().clone();
    Ok(MicSignalCheck {
        device_name: checked_device_name,
        level: max_level,
        has_input: level_has_input(max_level),
    })
}

// ---------------------------------------------------------------------------

fn try_reserve_audio(tx: &mpsc::Sender<AudioPcmF32>) -> Option<mpsc::Permit<'_, AudioPcmF32>> {
    tx.try_reserve().ok()
}

fn pick_config(device: &cpal::Device) -> Result<cpal::SupportedStreamConfig> {
    let mut ranges: Vec<SupportedStreamConfigRange> = device
        .supported_input_configs()
        .context("enumerate input configs")?
        .collect();

    if ranges.is_empty() {
        return Err(anyhow!("device exposes no input configs"));
    }

    // Score: prefer f32 stereo 48kHz. Lower score wins.
    ranges.sort_by_key(|range| {
        let fmt_penalty = if range.sample_format() == SampleFormat::F32 {
            0
        } else {
            10
        };
        let chan_penalty = (range.channels() as i32 - TARGET_CHANNELS as i32).abs();
        let rate_penalty = if range.min_sample_rate().0 <= TARGET_SAMPLE_RATE
            && range.max_sample_rate().0 >= TARGET_SAMPLE_RATE
        {
            0
        } else {
            let mid = (range.min_sample_rate().0 + range.max_sample_rate().0) / 2;
            ((mid as i64) - (TARGET_SAMPLE_RATE as i64)).abs() as i32
        };
        fmt_penalty + chan_penalty + rate_penalty
    });

    let chosen = ranges.into_iter().next().unwrap();
    // Clamp the sample rate into our target if the range spans it.
    let rate = if chosen.min_sample_rate().0 <= TARGET_SAMPLE_RATE
        && chosen.max_sample_rate().0 >= TARGET_SAMPLE_RATE
    {
        cpal::SampleRate(TARGET_SAMPLE_RATE)
    } else {
        chosen.max_sample_rate()
    };
    Ok(chosen.with_sample_rate(rate))
}

fn build_stream<E>(
    device: &cpal::Device,
    config: &StreamConfig,
    sample_format: SampleFormat,
    tx: mpsc::Sender<AudioPcmF32>,
    stop_flag: Arc<AtomicBool>,
    mute_flag: Arc<AtomicBool>,
    sample_rate: u32,
    channels: u16,
    generation: MicStreamGeneration,
    capture_latency: Duration,
    err_fn: E,
) -> Result<Stream>
where
    E: FnMut(cpal::StreamError) + Send + 'static,
{
    // Each supported sample format uses the same shape: copy into f32, tag
    // with the HAL capture time mapped onto the mach clock so the recording
    // pump can align mic PTS against SCK's video/system-audio anchor.
    macro_rules! build_typed {
        ($t:ty) => {{
            let tx = tx.clone();
            let stop_flag = stop_flag.clone();
            let mute_flag = mute_flag.clone();
            let mut dropped_chunks = 0_u64;
            let mut hal_clock = HalClock::new(capture_latency);
            let mut reopen_fade = MicReopenFade::new(sample_rate, generation);
            device
                .build_input_stream(
                    config,
                    move |data: &[$t], info: &cpal::InputCallbackInfo| {
                        if stop_flag.load(Ordering::Acquire) || mute_flag.load(Ordering::Acquire) {
                            return;
                        }
                        // Keep the HAL anchor warm even when the chunk is
                        // dropped below, so a later chunk is not re-anchored
                        // on a late callback.
                        let buffer_frames = data.len() / usize::from(channels.max(1));
                        let mach_pts =
                            hal_clock.first_sample_mach(info, buffer_frames, sample_rate);
                        let Some(permit) = try_reserve_audio(&tx) else {
                            dropped_chunks += 1;
                            if dropped_chunks == 1 || dropped_chunks.is_power_of_two() {
                                tracing::warn!(
                                    dropped_chunks,
                                    queue_capacity = MIC_CHANNEL_CAPACITY,
                                    "mic callback dropped PCM because the encoder queue is full"
                                );
                            }
                            return;
                        };
                        let mut samples: Vec<f32> =
                            data.iter().map(|s| f32::from_sample(*s)).collect();
                        reopen_fade.apply(&mut samples, channels);

                        let chunk = AudioPcmF32 {
                            samples,
                            sample_rate,
                            channels,
                            generation,
                            mach_pts,
                        };
                        if stop_flag.load(Ordering::Acquire) || mute_flag.load(Ordering::Acquire) {
                            return;
                        }
                        permit.send(chunk);
                    },
                    err_fn,
                    None,
                )
                .context("build_input_stream")?
        }};
    }

    let stream = match sample_format {
        SampleFormat::F32 => build_typed!(f32),
        SampleFormat::I16 => build_typed!(i16),
        SampleFormat::U16 => build_typed!(u16),
        SampleFormat::I32 => build_typed!(i32),
        SampleFormat::U8 => build_typed!(u8),
        other => return Err(anyhow!("unsupported cpal sample format: {other:?}")),
    };
    Ok(stream)
}

#[cfg(test)]
mod tests {
    use super::{
        default_device_needs_reopen, explicit_device_action, level_has_input, match_input_device,
        samples_level, try_reserve_audio, AudioPcmF32, ExplicitDeviceAction, MicReopenFade,
        MicStreamGeneration, RetiredStreams, SupervisedStream, STREAM_RETIRE_GRACE,
    };

    #[test]
    fn stopped_default_stream_retries_even_when_original_device_returns() {
        assert!(default_device_needs_reopen(
            false,
            false,
            Some("Mac mic"),
            "Mac mic"
        ));
        assert!(default_device_needs_reopen(
            true,
            true,
            Some("Mac mic"),
            "Mac mic"
        ));
        assert!(default_device_needs_reopen(
            true,
            false,
            Some("Headset"),
            "Mac mic"
        ));
        assert!(default_device_needs_reopen(true, false, None, "Mac mic"));
        assert!(!default_device_needs_reopen(
            true,
            false,
            Some("Mac mic"),
            "Mac mic"
        ));
    }

    #[test]
    fn stopping_input_suppresses_callbacks_and_is_idempotent() {
        use std::sync::{
            atomic::{AtomicBool, Ordering},
            Arc,
        };
        let stop_flag = Arc::new(AtomicBool::new(false));
        let mut input = SupervisedStream {
            stream: None,
            stop_flag: stop_flag.clone(),
            err_flag: Arc::new(AtomicBool::new(false)),
            device_name: "Mac mic".into(),
            generation: MicStreamGeneration::initial(),
        };
        assert!(input.halt().is_none());
        assert!(stop_flag.load(Ordering::Acquire));
        assert!(input.halt().is_none());
        assert!(stop_flag.load(Ordering::Acquire));
        assert!(input.stream.is_none());
    }

    /// A halted stream must outlive the callback CoreAudio may still be
    /// running: freeing it inline is the 0.1.56 mic-switch segfault.
    #[test]
    fn retired_streams_are_freed_only_after_the_grace_period() {
        use std::sync::Arc;

        let mut retired = RetiredStreams::<Arc<()>>::default();
        let live = Arc::new(());
        retired.retire(Arc::clone(&live));

        retired.collect();
        assert_eq!(Arc::strong_count(&live), 2, "freed inside the grace window");

        // Pretend the grace period has elapsed.
        retired.pending[0].0 = std::time::Instant::now() - STREAM_RETIRE_GRACE * 2;
        retired.collect();
        assert_eq!(
            Arc::strong_count(&live),
            1,
            "not freed after the grace window"
        );
    }

    #[test]
    fn draining_retired_streams_frees_everything() {
        use std::sync::Arc;

        let mut retired = RetiredStreams::<Arc<()>>::default();
        let live = Arc::new(());
        retired.retire(Arc::clone(&live));
        retired.pending[0].0 = std::time::Instant::now() - STREAM_RETIRE_GRACE * 2;
        retired.drain_blocking();
        assert_eq!(Arc::strong_count(&live), 1);
    }

    #[test]
    fn device_reopen_fades_stereo_once_across_callback_boundaries() {
        for rate in [8_000, 16_000, 44_100, 48_000, 96_000] {
            let frames = rate as usize / 100;
            let mut fade = MicReopenFade::new(rate, MicStreamGeneration::initial().next());
            let mut first = vec![1.0; 14];
            fade.apply(&mut first, 2);
            assert_eq!(&first[..2], &[0.0, 0.0]);
            let mut rest = vec![1.0; (frames + 5 - 7) * 2];
            fade.apply(&mut rest, 2);
            let output = [first, rest].concat();
            for (index, frame) in output.chunks_exact(2).enumerate() {
                let expected = (index as f32 / (frames - 1) as f32).min(1.0);
                assert!((frame[0] - expected).abs() < 0.000_01);
                assert_eq!(frame[0], frame[1]);
            }
            let mut later = [0.4, -0.2];
            fade.apply(&mut later, 2);
            assert_eq!(later, [0.4, -0.2]);
        }
    }

    #[test]
    fn initial_microphone_capture_is_not_attenuated() {
        let mut fade = MicReopenFade::new(48_000, MicStreamGeneration::initial());
        let mut samples = [0.5, -0.5];
        fade.apply(&mut samples, 1);
        assert_eq!(samples, [0.5, -0.5]);
    }

    fn names(list: &[&str]) -> Vec<String> {
        list.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn selected_mic_matches_its_exact_name_first() {
        let devices = names(&["AirPods Pro", "Alex's AirPods Pro"]);
        assert_eq!(
            match_input_device(&devices, "AirPods Pro"),
            Some("AirPods Pro")
        );
    }

    #[test]
    fn selected_mic_reconnects_under_a_renamed_bluetooth_profile() {
        let devices = names(&["MacBook Pro Microphone", "Alex's AirPods Pro"]);
        assert_eq!(
            match_input_device(&devices, "AirPods Pro"),
            Some("Alex's AirPods Pro")
        );
        assert_eq!(
            match_input_device(&names(&["airpods pro"]), "Alex's AirPods Pro"),
            Some("airpods pro")
        );
    }

    #[test]
    fn selected_mic_never_substitutes_an_unrelated_device() {
        let devices = names(&["MacBook Pro Microphone", "Shure MV7"]);
        assert_eq!(match_input_device(&devices, "AirPods Pro"), None);
        assert_eq!(match_input_device(&devices, ""), None);
    }

    #[test]
    fn realtime_audio_enqueue_drops_when_bounded_channel_is_full() {
        let (tx, mut rx) = tokio::sync::mpsc::channel(1);
        let chunk = || AudioPcmF32 {
            samples: vec![0.0; 16],
            sample_rate: 48_000,
            channels: 2,
            generation: MicStreamGeneration::initial(),
            mach_pts: 1,
        };

        try_reserve_audio(&tx).expect("first permit").send(chunk());
        assert!(try_reserve_audio(&tx).is_none());
        assert_eq!(rx.try_recv().expect("first chunk").samples.len(), 16);
        assert!(rx.try_recv().is_err());
    }

    #[test]
    fn microphone_stream_generation_advances_for_each_reopen() {
        let initial = MicStreamGeneration::initial();
        let reopened = initial.next();

        assert_ne!(initial, reopened);
        assert_ne!(reopened, reopened.next());
    }

    #[test]
    fn explicit_device_never_falls_back_when_it_disappears() {
        assert_eq!(
            explicit_device_action(false, false, false, true),
            ExplicitDeviceAction::Pause
        );
        assert_eq!(
            explicit_device_action(false, true, true, true),
            ExplicitDeviceAction::Wait
        );
    }

    #[test]
    fn explicit_device_reopens_only_after_the_exact_device_returns() {
        assert_eq!(
            explicit_device_action(true, true, false, true),
            ExplicitDeviceAction::Reopen
        );
        assert_eq!(
            explicit_device_action(true, false, false, true),
            ExplicitDeviceAction::Keep
        );
    }

    #[test]
    fn samples_level_reports_zero_for_silent_input() {
        let samples = [0.0f32; 128];
        let level = samples_level(&samples);
        assert_eq!(level, 0.0);
        assert!(!level_has_input(level));
    }

    #[test]
    fn samples_level_reports_signal_for_voice_range_input() {
        let samples = [0.03f32, -0.04, 0.02, -0.01];
        let level = samples_level(&samples);
        assert!(level > 0.0);
        assert!(level_has_input(level));
    }
}
