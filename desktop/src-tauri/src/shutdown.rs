//! Quitting with a recording or an upload still in flight.
//!
//! Both quit routes — the tray's Quit item and the OS quit (Cmd-Q, the Dock
//! menu, a logout) — end up in `RunEvent::ExitRequested`, because
//! `AppHandle::exit` raises it too. Until this existed nothing was listening:
//! the process died, the uploader's never-terminal retry loop died with it,
//! and the funnel recorded an upload that started and then simply stopped
//! existing. That silence is a large part of why the desktop apps look like
//! they lose a fifth of their recordings.
//!
//! The recording itself is already safe — it is on disk under
//! `recordings/<id>.mp4` and the recovery scan finds it — so the only thing
//! owed here is the report, and it is bounded: a quit must not wait on the
//! network.

use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use tauri::{AppHandle, Manager, Runtime};

use crate::recording::RecordingController;
use crate::telemetry::{self, Event};
use crate::types::RecordingPhase;

/// How long a quit may wait for the report. Long enough for one request on a
/// working connection, short enough that nobody perceives it.
const EXIT_REPORT_BUDGET: Duration = Duration::from_millis(1200);

static REPORTED: AtomicBool = AtomicBool::new(false);

/// Called from the `ExitRequested` handler. Returns true when it has taken
/// over the exit: the caller must `prevent_exit()`, and this re-requests the
/// exit once the report is sent or the budget runs out.
///
/// Idempotent — the re-request comes back through the same handler, and a
/// second quit while the first is in flight must not stack another delay.
pub fn report_exit_with_work_in_flight<R: Runtime>(app: &AppHandle<R>, code: Option<i32>) -> bool {
    // `prevent_exit` is ignored for a restart, and restart-to-update is
    // already gated on nothing being in flight.
    if code == Some(tauri::RESTART_EXIT_CODE) {
        return false;
    }
    let Some(controller) = app.try_state::<RecordingController>() else {
        return false;
    };

    let state = controller.dto();
    let stage = match state.phase {
        RecordingPhase::Countdown | RecordingPhase::Recording | RecordingPhase::Paused => {
            "recording"
        }
        RecordingPhase::Stopping => "stopping",
        RecordingPhase::Uploading => "upload",
        // A detached Studio upload keeps running after the phase is back to
        // Idle, so the phase alone is not the whole answer.
        RecordingPhase::Idle if controller.upload_in_flight() => "upload",
        RecordingPhase::Idle => return false,
    };

    if REPORTED.swap(true, Ordering::SeqCst) {
        return false;
    }

    let event = Event::new(telemetry::RECORDING_ABANDONED)
        .maybe_recording(state.recording_id.as_deref())
        .stage(stage)
        .reason("app_quit");
    let elapsed = state.elapsed_ms;

    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        let event = match elapsed {
            Some(ms) => event.elapsed_ms(ms),
            None => event,
        };
        telemetry::send_bounded(&app, event, EXIT_REPORT_BUDGET).await;
        app.exit(code.unwrap_or(0));
    });
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_restart_is_never_deferred() {
        // Guards the one case where `prevent_exit` is ignored: deferring there
        // would drop the exit on the floor instead of delaying it.
        assert_eq!(tauri::RESTART_EXIT_CODE, i32::MAX);
    }

    #[test]
    fn only_the_first_quit_defers() {
        REPORTED.store(false, Ordering::SeqCst);
        assert!(!REPORTED.swap(true, Ordering::SeqCst));
        assert!(REPORTED.swap(true, Ordering::SeqCst));
        REPORTED.store(false, Ordering::SeqCst);
    }
}
