//! Long-lived microphone level monitor behind the popover's mic row.
//!
//! The row used to loop the 1.2 s `check_microphone_input` probe, which opened
//! and closed a real input stream every ~1.5 s for as long as the popover was
//! up. macOS's orange indicator followed every cycle, so an idle Clipy looked
//! like it was switching the mic on and off. One stream per panel session
//! gives a steady indicator and exactly one teardown.
//!
//! Only one monitor runs at a time. `start` replaces the previous one and
//! returns a generation; `stop` takes that generation and is a no-op for
//! anything but the live monitor, so a stale stop from a superseded effect
//! cannot take the replacement down. A stopped monitor stays waitable until
//! its stream has actually closed, so `stop_and_wait` at recording start never
//! races a monitor that is still opening or shutting down.
//!
//! The whole open/read/stop lifecycle runs on one dedicated thread: the
//! Windows capturer's `Send` impl is only sound while construction and `stop`
//! happen on the same thread.

use std::sync::OnceLock;
use std::time::{Duration, Instant};

use parking_lot::Mutex;
use tauri::{AppHandle, Emitter, Runtime};
use tokio::sync::{oneshot, watch};

use crate::capture::level::samples_level;
use crate::types::EVENT_MIC_MONITOR_LEVEL;

const LEVEL_INTERVAL: Duration = Duration::from_millis(50);
/// Bound on waiting for the input stream to be released before a recording
/// opens its own. Covers a monitor whose open is still in flight: the
/// supervisor gives up on an open after 5 s and on a stop after 2 s, so a
/// shorter bound would let the recorder start against a device the monitor
/// is still about to open.
const STOP_WAIT: Duration = Duration::from_millis(7_500);

struct Handle {
    generation: u64,
    stop_tx: watch::Sender<bool>,
    done_rx: oneshot::Receiver<()>,
}

impl Handle {
    fn finished(&mut self) -> bool {
        !matches!(self.done_rx.try_recv(), Err(oneshot::error::TryRecvError::Empty))
    }
}

/// Handle bookkeeping, kept free of Tauri so it can be unit tested.
#[derive(Default)]
struct Registry {
    next_generation: u64,
    live: Option<Handle>,
    /// Told to stop, stream not yet confirmed closed.
    draining: Vec<Handle>,
}

impl Registry {
    fn install(&mut self, stop_tx: watch::Sender<bool>, done_rx: oneshot::Receiver<()>) -> u64 {
        self.next_generation += 1;
        let generation = self.next_generation;
        if let Some(previous) = self.live.replace(Handle { generation, stop_tx, done_rx }) {
            self.retire(previous);
        }
        generation
    }

    /// Stop the live monitor; with a generation, only if it is still the live one.
    fn stop(&mut self, generation: Option<u64>) {
        let stale = matches!(generation, Some(wanted) if self.live.as_ref().is_some_and(|h| h.generation != wanted));
        if stale {
            return;
        }
        if let Some(live) = self.live.take() {
            self.retire(live);
        }
    }

    fn retire(&mut self, handle: Handle) {
        let _ = handle.stop_tx.send(true);
        self.draining.retain_mut(|h| !h.finished());
        self.draining.push(handle);
    }

    /// Stop everything and hand back every receiver still worth waiting on.
    fn drain(&mut self) -> Vec<oneshot::Receiver<()>> {
        self.stop(None);
        std::mem::take(&mut self.draining).into_iter().map(|h| h.done_rx).collect()
    }
}

static REGISTRY: OnceLock<Mutex<Registry>> = OnceLock::new();

fn registry() -> &'static Mutex<Registry> {
    REGISTRY.get_or_init(|| Mutex::new(Registry::default()))
}

/// Open the mic and stream levels on `EVENT_MIC_MONITOR_LEVEL` until stopped.
/// `device_id` follows the recording's resolution: `None` tracks the OS
/// default, a name pins that device.
pub fn start<R: Runtime>(app: AppHandle<R>, device_id: Option<String>) -> u64 {
    let (stop_tx, stop_rx) = watch::channel(false);
    let (done_tx, done_rx) = oneshot::channel();
    let generation = registry().lock().install(stop_tx, done_rx);
    let spawned = std::thread::Builder::new()
        .name("mic-monitor".into())
        .spawn(move || {
            run(app, device_id, stop_rx);
            let _ = done_tx.send(());
        });
    if let Err(err) = spawned {
        tracing::warn!("mic monitor thread failed to spawn: {err}");
        registry().lock().stop(Some(generation));
    }
    generation
}

/// Stop the monitor if `generation` is still the live one.
pub fn stop(generation: u64) {
    registry().lock().stop(Some(generation));
}

/// Stop whichever monitor is running, without waiting for the release.
pub fn stop_any() {
    registry().lock().stop(None);
}

/// Stop and wait (bounded) for every monitor stream to be released, including
/// ones already told to stop. Recording start uses this so the recorder never
/// shares the device with a monitor that is still opening or shutting down.
pub async fn stop_and_wait() {
    let pending = registry().lock().drain();
    if pending.is_empty() {
        return;
    }
    let all_done = async {
        for done in pending {
            let _ = done.await;
        }
    };
    if tokio::time::timeout(STOP_WAIT, all_done).await.is_err() {
        tracing::warn!("mic monitor did not release the input within {STOP_WAIT:?}");
    }
}

#[cfg(target_os = "macos")]
mod platform {
    use crate::capture::audio::{AudioPcmF32, MicSupervisor};
    use anyhow::Result;
    use tokio::sync::mpsc::Receiver;

    pub type Capturer = MicSupervisor;
    pub type Chunk = AudioPcmF32;

    pub fn open(device_id: Option<String>) -> Result<(Capturer, Receiver<Chunk>)> {
        MicSupervisor::start(device_id, Box::new(|_| {}))
    }
}

#[cfg(target_os = "windows")]
mod platform {
    use crate::capture::windows::{AudioChunk, MicCapturer};
    use anyhow::Result;
    use tokio::sync::mpsc::Receiver;

    pub type Capturer = MicCapturer;
    pub type Chunk = AudioChunk;

    pub fn open(device_id: Option<String>) -> Result<(Capturer, Receiver<Chunk>)> {
        MicCapturer::start_with_device(device_id.as_deref())
    }
}

/// Open, read and stop entirely on the calling (monitor) thread.
#[cfg(any(target_os = "macos", target_os = "windows"))]
fn run<R: Runtime>(app: AppHandle<R>, device_id: Option<String>, mut stop_rx: watch::Receiver<bool>) {
    let runtime = match tokio::runtime::Builder::new_current_thread().build() {
        Ok(runtime) => runtime,
        Err(err) => {
            tracing::warn!("mic monitor runtime failed to build: {err}");
            return;
        }
    };
    // The Windows capturer spawns its forwarding task during open, so the
    // runtime has to be current before the device is touched. The task is
    // queued until `block_on` drives the runtime below.
    let entered = runtime.enter();
    let opened = platform::open(device_id);
    drop(entered);
    let (capturer, mut rx) = match opened {
        Ok(opened) => opened,
        Err(err) => {
            tracing::warn!("mic monitor could not open the input: {err:#}");
            return;
        }
    };
    runtime.block_on(async {
        let mut last_emit = Instant::now() - LEVEL_INTERVAL;
        while !*stop_rx.borrow() {
            tokio::select! {
                changed = stop_rx.changed() => {
                    if changed.is_err() {
                        break;
                    }
                }
                chunk = rx.recv() => {
                    let Some(chunk) = chunk else { break };
                    if last_emit.elapsed() >= LEVEL_INTERVAL {
                        let _ = app.emit(EVENT_MIC_MONITOR_LEVEL, samples_level(&chunk.samples));
                        last_emit = Instant::now();
                    }
                }
            }
        }
    });
    if let Err(err) = capturer.stop() {
        tracing::warn!("mic monitor stop failed: {err:#}");
    }
    let _ = app.emit(EVENT_MIC_MONITOR_LEVEL, 0.0f32);
}

#[cfg(not(any(target_os = "macos", target_os = "windows")))]
fn run<R: Runtime>(_app: AppHandle<R>, _device_id: Option<String>, _stop_rx: watch::Receiver<bool>) {}

#[cfg(test)]
mod tests {
    use super::*;

    struct Spawned {
        generation: u64,
        stop_rx: watch::Receiver<bool>,
        done_tx: oneshot::Sender<()>,
    }

    fn install(registry: &mut Registry) -> Spawned {
        let (stop_tx, stop_rx) = watch::channel(false);
        let (done_tx, done_rx) = oneshot::channel();
        let generation = registry.install(stop_tx, done_rx);
        Spawned { generation, stop_rx, done_tx }
    }

    #[test]
    fn start_stops_the_previous_monitor_and_keeps_it_waitable() {
        let mut registry = Registry::default();
        let first = install(&mut registry);
        let second = install(&mut registry);
        assert!(*first.stop_rx.borrow());
        assert!(!*second.stop_rx.borrow());
        assert_eq!(registry.live.as_ref().map(|h| h.generation), Some(second.generation));
        assert_eq!(registry.draining.len(), 1);
    }

    #[test]
    fn stale_stop_leaves_the_replacement_running() {
        let mut registry = Registry::default();
        let first = install(&mut registry);
        let second = install(&mut registry);
        registry.stop(Some(first.generation));
        assert!(!*second.stop_rx.borrow());
        registry.stop(Some(second.generation));
        assert!(*second.stop_rx.borrow());
        assert!(registry.live.is_none());
    }

    #[test]
    fn drain_stops_the_live_monitor_and_returns_every_unfinished_receiver() {
        let mut registry = Registry::default();
        let first = install(&mut registry);
        let second = install(&mut registry);
        // The first stream reports closed; the second is still shutting down.
        first.done_tx.send(()).unwrap();
        let third = install(&mut registry);
        assert_eq!(registry.draining.len(), 1, "finished handles are pruned on install");
        let pending = registry.drain();
        assert!(*third.stop_rx.borrow());
        assert_eq!(pending.len(), 2);
        assert!(registry.live.is_none() && registry.draining.is_empty());
        drop(second);
    }

    #[test]
    fn drain_with_nothing_running_is_empty() {
        let mut registry = Registry::default();
        assert!(registry.drain().is_empty());
    }
}
