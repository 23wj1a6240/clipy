//! AssemblyAI: upload the audio, start a transcript job, poll until it finishes.

use std::time::{Duration, Instant};

use serde::Deserialize;

use super::http::{client, request_json, sleep, upload_body};
use super::{engine_name, AudioInput};
use crate::chunking::{group_words_into_segments, to_timeline, RelativeSegment, TimedWord};
use crate::error::{ProviderError, Result};
use crate::util::{join_url, urlencode};
use crate::Transcript;

pub const ASSEMBLYAI_BASE_URL: &str = "https://api.assemblyai.com";
const PROVIDER: &str = "assemblyai";

pub(crate) struct Limits {
    pub poll_interval: Duration,
    /// Give up after this long; long recordings take a while to process.
    pub timeout: Duration,
}

impl Default for Limits {
    fn default() -> Self {
        Limits {
            poll_interval: Duration::from_secs(3),
            timeout: Duration::from_secs(60 * 60),
        }
    }
}

#[derive(Deserialize)]
struct Upload {
    upload_url: String,
}

#[derive(Deserialize)]
struct Job {
    id: String,
    status: String,
    language_code: Option<String>,
    words: Option<Vec<Timed>>,
    utterances: Option<Vec<Timed>>,
}

/// Times are milliseconds.
#[derive(Deserialize)]
struct Timed {
    text: String,
    start: f64,
    end: f64,
}

fn shape_error() -> ProviderError {
    ProviderError::new(PROVIDER, Some(200), Some("unexpected answer shape"))
}

pub(crate) fn transcribe(
    input: &AudioInput,
    api_key: &str,
    model: Option<&str>,
    base_url: Option<&str>,
    limits: &Limits,
) -> Result<Transcript> {
    let base = base_url.unwrap_or(ASSEMBLYAI_BASE_URL);
    let http = client()?;

    let req = http
        .post(join_url(base, "v2/upload"))
        .header("authorization", api_key)
        .header("Content-Type", "application/octet-stream")
        .body(upload_body(input.wav(), input.cancel));
    let up: Upload =
        serde_json::from_value(request_json(PROVIDER, req, input.cancel)?).map_err(|_| {
            ProviderError::new(PROVIDER, Some(200), Some("upload answer had no upload_url"))
        })?;

    let mut body = serde_json::json!({ "audio_url": up.upload_url, "language_detection": true });
    if let Some(m) = model.filter(|m| !m.is_empty()) {
        body["speech_models"] = serde_json::json!([m]);
    }
    let req = http
        .post(join_url(base, "v2/transcript"))
        .header("authorization", api_key)
        .header("Content-Type", "application/json")
        .body(body.to_string());
    let mut job: Job = serde_json::from_value(request_json(PROVIDER, req, input.cancel)?)
        .map_err(|_| shape_error())?;

    let started = Instant::now();
    while job.status != "completed" {
        if job.status == "error" {
            return Err(ProviderError::new(
                PROVIDER,
                None,
                Some("the provider reported an error for this audio"),
            )
            .into());
        }
        if started.elapsed() > limits.timeout {
            return Err(ProviderError::new(
                PROVIDER,
                None,
                Some("timed out waiting for the transcript"),
            )
            .into());
        }
        sleep(limits.poll_interval, input.cancel)?;
        let path = format!("v2/transcript/{}", urlencode(&job.id));
        let req = http
            .get(join_url(base, &path))
            .header("authorization", api_key);
        job = serde_json::from_value(request_json(PROVIDER, req, input.cancel)?)
            .map_err(|_| shape_error())?;
    }

    let segments: Vec<RelativeSegment> = match job.utterances {
        Some(u) if !u.is_empty() => u
            .into_iter()
            .map(|u| RelativeSegment {
                start: u.start / 1000.0,
                end: u.end / 1000.0,
                text: u.text,
            })
            .collect(),
        _ => {
            let words: Vec<TimedWord> = job
                .words
                .unwrap_or_default()
                .into_iter()
                .map(|w| TimedWord {
                    start: w.start / 1000.0,
                    end: w.end / 1000.0,
                    text: w.text,
                })
                .collect();
            group_words_into_segments(&words, " ")
        }
    };
    Ok(Transcript {
        segments: to_timeline(segments),
        engine: engine_name(PROVIDER, model),
        language: job.language_code,
        gaps: Vec::new(),
    })
}
