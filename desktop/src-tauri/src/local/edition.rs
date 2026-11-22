//! The local edition's processing: moment selection on this computer (plus
//! the user's own model provider, when one is set) and, on Windows,
//! transcription with a downloaded model or the user's own key. Built only
//! without the `cloud` feature; `local_edition_moments` and
//! `local_edition_transcribe` in `mod.rs` call in here.
//!
//! Every request that leaves the computer is recorded in the recording's
//! `disclosure.json` *before* it is sent, and the AREC's evidence boundary is
//! written from that record (`clipy_local_edition::disclosure`).

use std::path::Path;

use anyhow::{Context, Result};
use clipy_local_edition::disclosure::{self, failure_reason, Disclosure, SpokenOutcome};
use clipy_local_edition::moments::{plan_rules, plan_with_provider, PlannerError};
use clipy_local_edition::settings::{transcription_plan, MomentProvider};
use clipy_local_edition::sidecar;
use serde_json::Value;
use tauri::{AppHandle, Runtime};

use super::settings::{load_keys, load_settings};
use super::{status, validate_moments, Moment, Transcript, DEVICE};

/// This recording's record of what left the computer.
pub(super) fn read_disclosure(dir: &Path) -> Disclosure {
    Disclosure::from_bytes(std::fs::read(dir.join(disclosure::FILE)).ok().as_deref())
}

/// Change the record and write it to disk (atomically) before returning.
/// When the AREC's data statement changes, the AREC already on disk is
/// rewritten too, also before returning: callers record a request here before
/// sending it, so an agent reading `recording.arec` mid-request (or after the
/// app was killed during one) never sees less than what was sent.
pub(super) fn update_disclosure(dir: &Path, change: impl FnOnce(&mut Disclosure)) -> Result<()> {
    let mut record = read_disclosure(dir);
    let before = record.evidence_boundary(DEVICE);
    change(&mut record);
    let after = record.evidence_boundary(DEVICE);
    let temp = dir.join(format!("{}.tmp", disclosure::FILE));
    std::fs::write(&temp, record.to_bytes()).context("save disclosure.json")?;
    std::fs::rename(&temp, dir.join(disclosure::FILE)).context("save disclosure.json")?;
    if before != after {
        refresh_arec_boundary(dir, &before, &after)?;
    }
    Ok(())
}

/// Rewrites the data statement in the AREC on disk, atomically. No AREC yet
/// means the next write uses the new record anyway. An AREC without the
/// expected statement is an error, so the caller does not send.
fn refresh_arec_boundary(dir: &Path, before: &str, after: &str) -> Result<()> {
    let path = dir.join("recording.arec");
    let doc = match std::fs::read_to_string(&path) {
        Ok(doc) => doc,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error).context("read recording.arec"),
    };
    let Some(updated) = disclosure::refresh_boundary(&doc, before, after) else {
        anyhow::bail!(
            "recording.arec does not carry the expected data statement, so nothing was sent"
        );
    };
    let temp = dir.join("recording.arec.tmp");
    std::fs::write(&temp, updated).context("update recording.arec")?;
    std::fs::rename(&temp, &path).context("update recording.arec")?;
    Ok(())
}

/// The status line while the recording is transcribed, for the settings in
/// use.
pub(super) fn transcribing_message<R: Runtime>(app: &AppHandle<R>) -> String {
    disclosure::transcribing_message(DEVICE, &transcription_plan(&load_settings(app)))
}

/// Rules always; the model provider's spoken cues when one is set and
/// answers. A provider that fails costs its spoken cues, not the take: the
/// rules answer and the AREC says why spoken references are missing.
pub(super) async fn moments<R: Runtime>(
    app: &AppHandle<R>,
    dir: &Path,
    transcript: &Transcript,
    interactions: &Value,
    duration: u64,
) -> Result<Vec<Moment>> {
    if duration == 0 {
        return Ok(Vec::new());
    }
    let rules = || {
        plan_rules(&sidecar::rules_input(
            &transcript.segments,
            interactions,
            duration,
        ))
    };
    let settings = load_settings(app);
    let keys = load_keys(app);
    let (plan, outcome) = match clipy_local_edition::settings::moment_provider(&settings, &keys) {
        MomentProvider::RulesOnly => (rules(), SpokenOutcome::NotConfigured),
        MomentProvider::Unusable { message } => {
            tracing::warn!("model provider settings unusable; moments from the rules only");
            (rules(), SpokenOutcome::Unusable { message })
        }
        MomentProvider::Ready { .. } if transcript.segments.is_empty() => {
            (rules(), SpokenOutcome::NoTranscript)
        }
        MomentProvider::Ready { config, recipient } => {
            let input = sidecar::provider_input(&transcript.segments, interactions, duration);
            match input.validate() {
                Err(error) => {
                    tracing::warn!("recording is outside the planner's limits; rules only");
                    let reason = failure_reason(&PlannerError::from(error));
                    (rules(), SpokenOutcome::Failed { recipient, reason })
                }
                Ok(()) => {
                    // Recorded before the request, so the AREC never
                    // understates what was sent, even if it then fails.
                    update_disclosure(dir, |d| d.add_text(recipient.clone()))?;
                    status(
                        app,
                        dir,
                        "selecting",
                        &format!(
                            "Asking {} ({}) for spoken moments",
                            recipient.name, recipient.host
                        ),
                    )?;
                    match plan_with_provider(&input, &config).await {
                        Ok(plan) => (plan, SpokenOutcome::Answered { recipient }),
                        Err(error) => {
                            // `describe` is safe to log: no key, transcript or body.
                            tracing::warn!(
                                error = %error.describe(),
                                "model provider gave no spoken moments; using the rules"
                            );
                            let reason = failure_reason(&error);
                            (rules(), SpokenOutcome::Failed { recipient, reason })
                        }
                    }
                }
            }
        }
    };
    update_disclosure(dir, |d| d.spoken = Some(outcome))?;
    validate_moments(&sidecar::plan_json(&plan), duration)
}

/// Windows: wait for the seal, then transcribe with the chosen model or key.
/// A setting the user can fix (no model downloaded, no key, nothing chosen)
/// ends the transcript with a gap that says so, and Retry transcribes again
/// once it is fixed. An engine or provider error fails the take with Retry.
#[cfg(target_os = "windows")]
pub(super) fn transcribe<R: Runtime>(
    app: &AppHandle<R>,
    dir: &Path,
    cancel: &std::sync::atomic::AtomicBool,
    video: &Path,
    transcript: &mut Transcript,
    transcript_path: &Path,
) -> Result<()> {
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::time::Duration;

    use anyhow::bail;
    use clipy_local_edition::settings::{stt_config, TranscriptionPlan};
    use clipy_local_edition::transcript as gaps;
    use clipy_local_stt::{CancelToken, SttError};
    use serde_json::json;

    use super::{media, native, write_json};

    while !dir.join("metadata.json").exists() {
        if cancel.load(Ordering::SeqCst) {
            bail!("Local processing cancelled. Saved files remain on this {DEVICE}.");
        }
        std::thread::sleep(Duration::from_secs(1));
    }
    native(json!({"op":"foreground"}))?;
    let duration_ms = (native(json!({"op":"duration", "video":video}))?["seconds"]
        .as_f64()
        .unwrap_or(0.0)
        * 1000.0) as u64;
    let redo = gaps::needs_retry(&transcript.gaps);
    // A retry after a finished transcript keeps it, unless it ended on a
    // setting the user has had the chance to fix.
    if duration_ms > 0 && transcript.through_ms >= duration_ms && !redo {
        return Ok(());
    }
    // Pieces are not resumable: start over.
    *transcript = Transcript::default();
    let finish = |transcript: &mut Transcript, gap: &str, through_ms: u64| -> Result<()> {
        transcript.gaps.push(gap.to_string());
        transcript.through_ms = through_ms;
        write_json(transcript_path, transcript)
    };

    let settings = load_settings(app);
    let plan = transcription_plan(&settings);
    let provider = match &plan {
        TranscriptionPlan::Off | TranscriptionPlan::Apple => {
            return finish(transcript, gaps::NOT_SET_UP_GAP, duration_ms);
        }
        TranscriptionPlan::Local(model) => {
            if !clipy_local_stt::model_status(model.to_stt(), None).installed {
                return finish(transcript, &gaps::model_missing_gap(*model), duration_ms);
            }
            None
        }
        TranscriptionPlan::Provider { kind, recipient } => {
            let keys = load_keys(app);
            match stt_config(
                *kind,
                settings.transcription.model.as_deref(),
                keys.transcription_key(*kind),
            ) {
                Ok(config) => Some((config, recipient.clone())),
                Err(message) => {
                    return finish(transcript, &gaps::missing_key_gap(&message), duration_ms)
                }
            }
        }
    };

    status(
        app,
        dir,
        "transcribing",
        "Preparing audio for transcription",
    )?;
    let Some(pcm) = media::extract_pcm(video, &dir.join("cancelled"))? else {
        return finish(transcript, gaps::NO_AUDIO_GAP, duration_ms);
    };
    if cancel.load(Ordering::SeqCst) {
        bail!("Local processing cancelled. Saved files remain on this {DEVICE}.");
    }
    let pcm_ms = pcm.len() as u64 * 1000 / u64::from(media::AUDIO_SAMPLE_RATE);
    let through_ms = if duration_ms > 0 { duration_ms } else { pcm_ms };

    if let Some((_, recipient)) = &provider {
        // Recorded before any audio leaves the computer.
        update_disclosure(dir, |d| d.add_audio(recipient.clone()))?;
        status(
            app,
            dir,
            "transcribing",
            &format!(
                "Transcribing with {} ({}) using your key",
                recipient.name, recipient.host
            ),
        )?;
    } else {
        status(
            app,
            dir,
            "transcribing",
            &format!("Transcribing on this {DEVICE}"),
        )?;
    }

    // The engine and the providers poll a CancelToken; the job is cancelled
    // through an AtomicBool. A watcher on a scoped thread joins the two.
    let token = CancelToken::new();
    let finished = AtomicBool::new(false);
    let result = std::thread::scope(|scope| {
        scope.spawn(|| {
            while !finished.load(Ordering::SeqCst) {
                if cancel.load(Ordering::SeqCst) {
                    token.cancel();
                    return;
                }
                std::thread::sleep(Duration::from_millis(200));
            }
        });
        let result = match (&plan, &provider) {
            (_, Some((config, _))) => {
                clipy_local_stt::transcribe_with_provider(&pcm, config, &token)
            }
            (TranscriptionPlan::Local(model), None) => {
                let mut announced = 0u64;
                clipy_local_stt::transcribe_local(
                    &pcm,
                    model.to_stt(),
                    None,
                    &mut |fraction| {
                        let percent = (fraction.clamp(0.0, 1.0) * 100.0) as u64;
                        if percent >= announced + 5 {
                            announced = percent;
                            let _ = status(
                                app,
                                dir,
                                "transcribing",
                                &format!("Transcribing on this {DEVICE}: {percent}%"),
                            );
                        }
                    },
                    &token,
                )
            }
            _ => Err(SttError::InvalidInput("No transcription is set up.".into())),
        };
        finished.store(true, Ordering::SeqCst);
        result
    });
    drop(pcm);

    match result {
        Ok(result) => {
            transcript.segments = gaps::segment_values(
                result
                    .segments
                    .into_iter()
                    .map(|s| (s.start_ms, s.end_ms, s.text)),
                through_ms,
            );
            transcript.gaps.extend(
                result
                    .gaps
                    .iter()
                    .map(|g| gaps::gap_line(g.start_ms, g.end_ms, &g.reason)),
            );
            if transcript.segments.is_empty() && transcript.gaps.is_empty() {
                transcript.gaps.push(gaps::NO_SPEECH_GAP.to_string());
            }
            transcript.through_ms = through_ms;
            write_json(transcript_path, transcript)
        }
        Err(SttError::Cancelled) => {
            bail!("Local processing cancelled. Saved files remain on this {DEVICE}.")
        }
        // Deleted between the check above and the run.
        Err(SttError::ModelNotInstalled { .. }) => match plan {
            TranscriptionPlan::Local(model) => {
                finish(transcript, &gaps::model_missing_gap(model), through_ms)
            }
            _ => bail!(
                "The transcription model is missing. Your recording remains on this {DEVICE}."
            ),
        },
        // Provider errors name the provider and the HTTP status only, never
        // the key or a response body.
        Err(error) => bail!(
            "{}. Your recording remains on this {DEVICE}; Retry to transcribe again.",
            error.to_string().trim_end_matches('.')
        ),
    }
}
