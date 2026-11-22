//! Periodic resource samples during a desktop recording.
//!
//! A tester's 8 GB Mac ran out of memory during 5 to 10 minute takes in the
//! Chrome extension and the takes died with no trace. The desktop apps had no
//! view of their own resource use during a take either, so a recording that
//! slows a machine down or gets killed left nothing behind. This sends a
//! `recording_health` event about once a minute while a take runs, through the
//! same fire-and-forget path as the other recording events (`telemetry.rs`),
//! keyed by the take's `recordingId` so it joins the recording journey.
//!
//! What it may cost, and the rules that keep it that way:
//! - It runs as its own tokio task on a 60 s timer. It shares no lock with the
//!   capture pump: it reads the controller's state mutex (as the UI ticker
//!   does four times a second) and the uploader's progress mutex, once each.
//! - The OS reads are a handful of syscalls (no process or window
//!   enumeration) and run on a blocking thread so they cannot stall the
//!   runtime the pump also uses.
//! - Every failure leaves a property out; nothing here returns an error to
//!   the recording, and a panic would end only this task.
//! - It sends numbers and the stage only: no window titles, paths, app names
//!   or anything from the screen.
//!
//! Property keys must stay inside the server's recording-journey allowlist;
//! the relay drops anything else.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use parking_lot::Mutex;
use tauri::{AppHandle, Runtime};
use tokio::task::JoinHandle;

use crate::recording::PauseState;
use crate::telemetry::{self, Event};
use crate::types::{RecordingId, RecordingPhase, RecordingStateDto};
use crate::capture_health::UploadHealth;

/// First sample a minute in, then one a minute. A take shorter than that
/// sends nothing, which is fine: short takes are not what this is for.
pub const SAMPLE_INTERVAL: Duration = Duration::from_secs(60);

const MB: f64 = 1024.0 * 1024.0;

/// What the OS says about this process and the machine. Each field is
/// optional because each read can fail on its own.
#[derive(Debug, Default, Clone, Copy, PartialEq)]
pub struct ResourceSnapshot {
    pub process_memory_bytes: Option<u64>,
    pub system_memory_total_bytes: Option<u64>,
    pub system_memory_available_bytes: Option<u64>,
    /// User plus system CPU time this process has used since it started.
    pub process_cpu_time: Option<Duration>,
}

/// One `recording_health` event, before it is serialized.
#[derive(Debug, Clone, PartialEq)]
pub struct Sample {
    pub sample: u32,
    /// Recorded time, pauses excluded.
    pub elapsed_ms: u64,
    pub paused: bool,
    pub recorded_bytes: Option<u64>,
    pub upload: Option<UploadHealth>,
    pub resources: ResourceSnapshot,
    pub cpu_percent: Option<f64>,
}

impl Sample {
    pub fn into_event(self, recording_id: &str) -> Event {
        let backlog_mb = match (self.recorded_bytes, self.upload) {
            (Some(recorded), Some(upload)) => {
                Some(upload_backlog_mb(recorded, upload.bytes_uploaded))
            }
            _ => None,
        };
        let event = Event::new(telemetry::RECORDING_HEALTH)
            .recording(recording_id)
            .stage(if self.paused { "paused" } else { "recording" });
        // A dead upload says so; `backpressure` would otherwise read as a
        // slow network for the rest of the take.
        let event = match self.upload {
            Some(upload) if upload.failed => event.reason("upload_failed"),
            _ => event,
        };
        event
            .elapsed_ms(self.elapsed_ms)
            .measure("sample", Some(self.sample))
            .measure("recorded_bytes", self.recorded_bytes)
            .measure("bytesUploaded", self.upload.map(|u| u.bytes_uploaded))
            .measure("upload_backlog_mb", backlog_mb)
            .measure("backpressure", self.upload.map(|u| u.backpressure))
            .measure(
                "process_memory_mb",
                self.resources.process_memory_bytes.map(to_mb),
            )
            .measure(
                "system_memory_total_mb",
                self.resources.system_memory_total_bytes.map(to_mb),
            )
            .measure(
                "system_memory_available_mb",
                self.resources.system_memory_available_bytes.map(to_mb),
            )
            .measure("cpu_percent", self.cpu_percent)
    }
}

/// Megabytes to one decimal place.
fn to_mb(bytes: u64) -> f64 {
    (bytes as f64 / MB * 10.0).round() / 10.0
}

/// Bytes on disk the server has not acknowledged yet. Never negative: the
/// file is read after the upload counter, so a part can be acknowledged
/// between the two reads.
fn upload_backlog_mb(recorded_bytes: u64, bytes_uploaded: u64) -> f64 {
    to_mb(recorded_bytes.saturating_sub(bytes_uploaded))
}

/// Share of one core this process used between two readings, as Activity
/// Monitor and Task Manager's per-process column show it (so it can exceed
/// 100 on a multi-core machine). One decimal place.
fn cpu_percent(
    previous: Option<(Instant, Duration)>,
    now: (Instant, Option<Duration>),
) -> Option<f64> {
    let (prev_at, prev_cpu) = previous?;
    let cpu = now.1?;
    let wall = now.0.checked_duration_since(prev_at)?;
    if wall.is_zero() {
        return None;
    }
    let used = cpu.checked_sub(prev_cpu)?;
    let percent = used.as_secs_f64() / wall.as_secs_f64() * 100.0;
    Some((percent * 10.0).round() / 10.0)
}

/// What a tick should do for this take.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Tick {
    Sample {
        paused: bool,
    },
    /// Still counting down (the sampler is started just before the phase
    /// flips to Recording); nothing to report yet.
    Wait,
    /// The take ended, failed, or a different take owns the state.
    End,
}

fn tick_for(state: &RecordingStateDto, recording_id: &str) -> Tick {
    if state.recording_id.as_deref() != Some(recording_id) {
        return Tick::End;
    }
    match state.phase {
        RecordingPhase::Recording => Tick::Sample { paused: false },
        RecordingPhase::Paused => Tick::Sample { paused: true },
        RecordingPhase::Countdown => Tick::Wait,
        _ => Tick::End,
    }
}

/// Start sampling a take. The task ends by itself on the first tick after
/// the take leaves Recording/Paused (Stop, Cancel, a failure), so callers do
/// not need to hold the handle; it is returned for tests and for callers that
/// want to end it sooner.
pub fn spawn<R: Runtime>(
    app: AppHandle<R>,
    recording_id: RecordingId,
    state: Arc<Mutex<RecordingStateDto>>,
    pause: Arc<PauseState>,
    started_at: Instant,
    file_path: PathBuf,
    upload: Option<Arc<dyn Fn() -> UploadHealth + Send + Sync>>,
) -> JoinHandle<()> {
    tokio::spawn(async move {
        let mut tick = tokio::time::interval_at(
            tokio::time::Instant::now() + SAMPLE_INTERVAL,
            SAMPLE_INTERVAL,
        );
        // A machine that was asleep or starved sends one late sample, not a
        // burst of catch-up samples.
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        let mut sample = 0u32;
        let mut previous_cpu: Option<(Instant, Duration)> = None;
        // The CPU baseline is the take's start, so the first sample covers the
        // first minute rather than everything since the app launched.
        if let Ok(baseline) = tokio::task::spawn_blocking(platform::process_cpu_time).await {
            previous_cpu = baseline.map(|cpu| (Instant::now(), cpu));
        }
        loop {
            tick.tick().await;
            let tick_now = {
                let state = state.lock();
                tick_for(&state, &recording_id)
            };
            let paused = match tick_now {
                Tick::Sample { paused } => paused,
                Tick::Wait => continue,
                Tick::End => break,
            };

            let elapsed_ms = pause.effective_elapsed_us(&started_at) / 1_000;
            let upload_now = upload.as_ref().map(|read| read());
            let recorded_bytes = tokio::fs::metadata(&file_path).await.ok().map(|m| m.len());
            let resources = tokio::task::spawn_blocking(read_resources)
                .await
                .unwrap_or_default();
            let read_at = Instant::now();
            let cpu = cpu_percent(previous_cpu, (read_at, resources.process_cpu_time));
            if let Some(cpu_time) = resources.process_cpu_time {
                previous_cpu = Some((read_at, cpu_time));
            }

            sample += 1;
            let event = Sample {
                sample,
                elapsed_ms,
                paused,
                recorded_bytes,
                upload: upload_now,
                resources,
                cpu_percent: cpu,
            }
            .into_event(&recording_id);
            telemetry::send(&app, event);
        }
    })
}

fn read_resources() -> ResourceSnapshot {
    let (system_memory_total_bytes, system_memory_available_bytes) = platform::system_memory();
    ResourceSnapshot {
        process_memory_bytes: platform::process_memory(),
        system_memory_total_bytes,
        system_memory_available_bytes,
        process_cpu_time: platform::process_cpu_time(),
    }
}

#[cfg(target_os = "macos")]
mod platform {
    //! Encoding and capture run inside this process (FFmpeg is linked, not
    //! spawned), so the app's own numbers cover the recorder. The webview's
    //! WebKit processes are separate and not counted.
    use std::mem::MaybeUninit;
    use std::time::Duration;

    /// `phys_footprint`: the number Activity Monitor shows as Memory and the
    /// one the kernel weighs under memory pressure.
    pub fn process_memory() -> Option<u64> {
        let mut info = MaybeUninit::<libc::rusage_info_v2>::zeroed();
        // SAFETY: the buffer is a zeroed rusage_info_v2, the flavor names
        // that struct, and the call writes at most its size.
        let rc = unsafe {
            libc::proc_pid_rusage(
                libc::getpid(),
                libc::RUSAGE_INFO_V2,
                info.as_mut_ptr().cast(),
            )
        };
        if rc != 0 {
            return None;
        }
        // SAFETY: the call succeeded and filled the struct.
        let footprint = unsafe { info.assume_init() }.ri_phys_footprint;
        (footprint > 0).then_some(footprint)
    }

    /// Total RAM from `hw.memsize`; available from `kern.memorystatus_level`,
    /// the percentage of memory the kernel considers free under its pressure
    /// model (what drives Activity Monitor's Memory Pressure graph). It moves
    /// in 1% steps (about 80 MB on an 8 GB Mac) and is not the same measure
    /// as Windows' `ullAvailPhys`; compare it within a platform.
    pub fn system_memory() -> (Option<u64>, Option<u64>) {
        let total = sysctl_u64(c"hw.memsize");
        let level = sysctl_u32(c"kern.memorystatus_level").filter(|level| *level <= 100);
        let available = match (total, level) {
            (Some(total), Some(level)) => Some(total / 100 * u64::from(level)),
            _ => None,
        };
        (total, available)
    }

    pub fn process_cpu_time() -> Option<Duration> {
        let mut usage = MaybeUninit::<libc::rusage>::zeroed();
        // SAFETY: getrusage writes one rusage into the buffer.
        if unsafe { libc::getrusage(libc::RUSAGE_SELF, usage.as_mut_ptr()) } != 0 {
            return None;
        }
        // SAFETY: the call succeeded.
        let usage = unsafe { usage.assume_init() };
        Some(timeval(usage.ru_utime) + timeval(usage.ru_stime))
    }

    fn timeval(value: libc::timeval) -> Duration {
        Duration::from_secs(value.tv_sec.max(0) as u64)
            + Duration::from_micros(value.tv_usec.max(0) as u64)
    }

    fn sysctl_u64(name: &std::ffi::CStr) -> Option<u64> {
        let mut value: u64 = 0;
        let mut size = std::mem::size_of::<u64>();
        // SAFETY: value and size describe an 8-byte buffer.
        let rc = unsafe {
            libc::sysctlbyname(
                name.as_ptr(),
                (&mut value as *mut u64).cast(),
                &mut size,
                std::ptr::null_mut(),
                0,
            )
        };
        (rc == 0 && size == std::mem::size_of::<u64>()).then_some(value)
    }

    fn sysctl_u32(name: &std::ffi::CStr) -> Option<u32> {
        let mut value: u32 = 0;
        let mut size = std::mem::size_of::<u32>();
        // SAFETY: value and size describe a 4-byte buffer.
        let rc = unsafe {
            libc::sysctlbyname(
                name.as_ptr(),
                (&mut value as *mut u32).cast(),
                &mut size,
                std::ptr::null_mut(),
                0,
            )
        };
        (rc == 0 && size == std::mem::size_of::<u32>()).then_some(value)
    }
}

#[cfg(target_os = "windows")]
mod platform {
    //! Capture (WGC) and encoding (libx264 through linked FFmpeg) run inside
    //! this process, so its own numbers cover the recorder. WebView2's
    //! msedgewebview2 processes are separate and not counted.
    use std::time::Duration;

    use windows::Win32::Foundation::FILETIME;
    use windows::Win32::System::ProcessStatus::{
        K32GetProcessMemoryInfo, PROCESS_MEMORY_COUNTERS, PROCESS_MEMORY_COUNTERS_EX,
    };
    use windows::Win32::System::SystemInformation::{GlobalMemoryStatusEx, MEMORYSTATUSEX};
    use windows::Win32::System::Threading::{GetCurrentProcess, GetProcessTimes};

    /// Private bytes: memory committed to this process alone. Not the working
    /// set, which Windows trims under memory pressure, so it would fall exactly
    /// when the machine runs short and hide the growth this event is for. The
    /// nearest Windows match to macOS `phys_footprint`.
    pub fn process_memory() -> Option<u64> {
        let mut counters = PROCESS_MEMORY_COUNTERS_EX {
            cb: std::mem::size_of::<PROCESS_MEMORY_COUNTERS_EX>() as u32,
            ..Default::default()
        };
        // SAFETY: the pseudo-handle needs no closing; cb tells the API the
        // buffer is the larger _EX struct, which starts with the base layout.
        let ok = unsafe {
            K32GetProcessMemoryInfo(
                GetCurrentProcess(),
                (&mut counters as *mut PROCESS_MEMORY_COUNTERS_EX)
                    .cast::<PROCESS_MEMORY_COUNTERS>(),
                counters.cb,
            )
        };
        ok.as_bool().then_some(counters.PrivateUsage as u64)
    }

    pub fn system_memory() -> (Option<u64>, Option<u64>) {
        let mut status = MEMORYSTATUSEX {
            dwLength: std::mem::size_of::<MEMORYSTATUSEX>() as u32,
            ..Default::default()
        };
        // SAFETY: dwLength is set as the API requires.
        match unsafe { GlobalMemoryStatusEx(&mut status) } {
            Ok(()) => (Some(status.ullTotalPhys), Some(status.ullAvailPhys)),
            Err(_) => (None, None),
        }
    }

    pub fn process_cpu_time() -> Option<Duration> {
        let mut creation = FILETIME::default();
        let mut exit = FILETIME::default();
        let mut kernel = FILETIME::default();
        let mut user = FILETIME::default();
        // SAFETY: four valid out-pointers, pseudo-handle for this process.
        unsafe {
            GetProcessTimes(
                GetCurrentProcess(),
                &mut creation,
                &mut exit,
                &mut kernel,
                &mut user,
            )
        }
        .ok()?;
        Some(filetime(kernel) + filetime(user))
    }

    /// FILETIME durations count 100 ns intervals.
    fn filetime(value: FILETIME) -> Duration {
        let ticks = (u64::from(value.dwHighDateTime) << 32) | u64::from(value.dwLowDateTime);
        Duration::from_nanos(ticks.saturating_mul(100))
    }
}

#[cfg(not(any(target_os = "macos", target_os = "windows")))]
mod platform {
    //! No desktop recorder ships here; the module still builds so the pure
    //! parts above are tested on Linux CI.
    use std::time::Duration;

    pub fn process_memory() -> Option<u64> {
        None
    }

    pub fn system_memory() -> (Option<u64>, Option<u64>) {
        (None, None)
    }

    pub fn process_cpu_time() -> Option<Duration> {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn sample() -> Sample {
        Sample {
            sample: 3,
            elapsed_ms: 180_250,
            paused: false,
            recorded_bytes: Some(300 * 1024 * 1024),
            upload: Some(UploadHealth {
                bytes_uploaded: 280 * 1024 * 1024,
                backpressure: false,
                failed: false,
            }),
            resources: ResourceSnapshot {
                process_memory_bytes: Some(512 * 1024 * 1024 + 300 * 1024),
                system_memory_total_bytes: Some(8 * 1024 * 1024 * 1024),
                system_memory_available_bytes: Some(900 * 1024 * 1024),
                process_cpu_time: Some(Duration::from_secs(40)),
            },
            cpu_percent: Some(37.5),
        }
    }

    fn properties(event: Event) -> serde_json::Value {
        event.properties()
    }

    #[test]
    fn a_sample_carries_the_allowlisted_keys() {
        let props = properties(sample().into_event("rec-1"));
        assert_eq!(props["recordingId"], json!("rec-1"));
        assert_eq!(props["stage"], json!("recording"));
        assert_eq!(props["sample"], json!(3));
        assert_eq!(props["elapsed_ms"], json!(180_250));
        assert_eq!(props["recorded_bytes"], json!(300 * 1024 * 1024));
        assert_eq!(props["bytesUploaded"], json!(280 * 1024 * 1024));
        assert_eq!(props["upload_backlog_mb"], json!(20.0));
        assert_eq!(props["backpressure"], json!(false));
        assert_eq!(props["process_memory_mb"], json!(512.3));
        assert_eq!(props["system_memory_total_mb"], json!(8192.0));
        assert_eq!(props["system_memory_available_mb"], json!(900.0));
        assert_eq!(props["cpu_percent"], json!(37.5));
    }

    #[test]
    fn only_allowlisted_keys_are_sent() {
        // Mirrors `recordingJourneyProperties` for `recording_health` plus the
        // fields every desktop event carries. A new key must be allowlisted
        // server-side first or the relay drops it.
        let allowed = [
            "source",
            "client_version",
            "os",
            "os_version",
            "recordingId",
            "stage",
            "reason_code",
            "elapsed_ms",
            "sample",
            "recorded_bytes",
            "bytesUploaded",
            "upload_backlog_mb",
            "backpressure",
            "process_memory_mb",
            "system_memory_total_mb",
            "system_memory_available_mb",
            "cpu_percent",
        ];
        let props = properties(sample().into_event("rec-1"));
        for key in props.as_object().unwrap().keys() {
            assert!(allowed.contains(&key.as_str()), "{key} is not allowlisted");
        }
    }

    #[test]
    fn a_failed_upload_is_named_in_reason_code() {
        let mut failed = sample();
        failed.upload = Some(UploadHealth {
            bytes_uploaded: 10,
            backpressure: false,
            failed: true,
        });
        let props = properties(failed.into_event("rec-1"));
        assert_eq!(props["reason_code"], json!("upload_failed"));
        assert_eq!(props["backpressure"], json!(false));
        assert!(properties(sample().into_event("rec-1"))
            .get("reason_code")
            .is_none());
    }

    #[test]
    fn a_paused_take_says_so() {
        let mut paused = sample();
        paused.paused = true;
        assert_eq!(
            properties(paused.into_event("rec-1"))["stage"],
            json!("paused")
        );
    }

    #[test]
    fn missing_readings_are_left_out_not_sent_as_null() {
        let bare = Sample {
            sample: 1,
            elapsed_ms: 60_000,
            paused: false,
            recorded_bytes: None,
            upload: None,
            resources: ResourceSnapshot::default(),
            cpu_percent: None,
        };
        let props = properties(bare.into_event("rec-1"));
        for key in [
            "recorded_bytes",
            "bytesUploaded",
            "upload_backlog_mb",
            "backpressure",
            "process_memory_mb",
            "system_memory_total_mb",
            "system_memory_available_mb",
            "cpu_percent",
        ] {
            assert!(props.get(key).is_none(), "{key} should be absent");
        }
        assert_eq!(props["sample"], json!(1));
    }

    #[test]
    fn a_measurement_cannot_overwrite_a_funnel_field() {
        let props = properties(
            Event::new(telemetry::RECORDING_HEALTH)
                .recording("rec-1")
                .measure("recordingId", Some("other")),
        );
        assert_eq!(props["recordingId"], json!("rec-1"));
    }

    #[test]
    fn backlog_never_goes_negative() {
        assert_eq!(upload_backlog_mb(1024 * 1024, 2 * 1024 * 1024), 0.0);
        assert_eq!(upload_backlog_mb(5 * 1024 * 1024, 1024 * 1024), 4.0);
    }

    #[test]
    fn cpu_percent_is_cpu_time_over_wall_time() {
        let start = Instant::now();
        let later = start + Duration::from_secs(60);
        let previous = Some((start, Duration::from_secs(10)));
        // 30 s of CPU over 60 s of wall time is half a core.
        assert_eq!(
            cpu_percent(previous, (later, Some(Duration::from_secs(40)))),
            Some(50.0)
        );
        // Two busy cores read as 200, as Activity Monitor shows it.
        assert_eq!(
            cpu_percent(previous, (later, Some(Duration::from_secs(130)))),
            Some(200.0)
        );
    }

    #[test]
    fn cpu_percent_needs_two_good_readings() {
        let start = Instant::now();
        let later = start + Duration::from_secs(60);
        assert_eq!(
            cpu_percent(None, (later, Some(Duration::from_secs(5)))),
            None
        );
        let previous = Some((start, Duration::from_secs(10)));
        assert_eq!(cpu_percent(previous, (later, None)), None);
        assert_eq!(
            cpu_percent(previous, (start, Some(Duration::from_secs(11)))),
            None
        );
        // A counter that went backwards is a bad reading, not negative CPU.
        assert_eq!(
            cpu_percent(previous, (later, Some(Duration::from_secs(5)))),
            None
        );
    }

    #[test]
    fn the_sampler_runs_only_while_its_own_take_records() {
        let mut state = RecordingStateDto::idle();
        state.recording_id = Some("rec-1".to_string());
        state.phase = RecordingPhase::Recording;
        assert_eq!(tick_for(&state, "rec-1"), Tick::Sample { paused: false });
        state.phase = RecordingPhase::Paused;
        assert_eq!(tick_for(&state, "rec-1"), Tick::Sample { paused: true });
        state.phase = RecordingPhase::Countdown;
        assert_eq!(tick_for(&state, "rec-1"), Tick::Wait);
        for phase in [
            RecordingPhase::Stopping,
            RecordingPhase::Uploading,
            RecordingPhase::Idle,
        ] {
            state.phase = phase;
            assert_eq!(tick_for(&state, "rec-1"), Tick::End);
        }
        // A later take must never be reported under this one's id.
        state.phase = RecordingPhase::Recording;
        state.recording_id = Some("rec-2".to_string());
        assert_eq!(tick_for(&state, "rec-1"), Tick::End);
    }
}
