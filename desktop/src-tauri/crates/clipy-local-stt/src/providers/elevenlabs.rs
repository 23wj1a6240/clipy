//! ElevenLabs Scribe `/v1/speech-to-text`: word timestamps grouped into segments.

use reqwest::blocking::multipart::Form;
use serde::Deserialize;

use super::http::{client, file_part, request_json};
use super::{engine_name, AudioInput};
use crate::chunking::{group_words_into_segments, to_timeline, TimedWord};
use crate::error::{ProviderError, Result};
use crate::util::join_url;
use crate::Transcript;

pub const ELEVENLABS_BASE_URL: &str = "https://api.elevenlabs.io";
pub const ELEVENLABS_DEFAULT_MODEL: &str = "scribe_v2";
const PROVIDER: &str = "elevenlabs";

#[derive(Deserialize)]
struct Answer {
    language_code: Option<String>,
    words: Option<Vec<Word>>,
}

#[derive(Deserialize)]
struct Word {
    text: String,
    start: Option<f64>,
    end: Option<f64>,
    #[serde(rename = "type")]
    kind: Option<String>,
}

pub(crate) fn transcribe(
    input: &AudioInput,
    api_key: &str,
    model: Option<&str>,
    base_url: Option<&str>,
) -> Result<Transcript> {
    let model = model
        .filter(|m| !m.is_empty())
        .unwrap_or(ELEVENLABS_DEFAULT_MODEL);
    let form = Form::new()
        .text("model_id", model.to_string())
        .part(
            "file",
            file_part(input.wav(), "audio.wav", "audio/wav", input.cancel)?,
        )
        .text("timestamps_granularity", "word")
        .text("tag_audio_events", "false");
    let req = client()?
        .post(join_url(
            base_url.unwrap_or(ELEVENLABS_BASE_URL),
            "v1/speech-to-text",
        ))
        .header("xi-api-key", api_key)
        .multipart(form);
    let parsed: Answer = serde_json::from_value(request_json(PROVIDER, req, input.cancel)?)
        .map_err(|_| ProviderError::new(PROVIDER, Some(200), Some("unexpected answer shape")))?;
    // Words come with "spacing" entries between them; keep only words, times in seconds.
    let words: Vec<TimedWord> = parsed
        .words
        .unwrap_or_default()
        .into_iter()
        .filter(|w| w.kind.as_deref().unwrap_or("word") == "word")
        .filter_map(|w| {
            Some(TimedWord {
                start: w.start?,
                end: w.end?,
                text: w.text,
            })
        })
        .collect();
    Ok(Transcript {
        segments: to_timeline(group_words_into_segments(&words, " ")),
        engine: engine_name(PROVIDER, Some(model)),
        language: parsed.language_code,
        gaps: Vec::new(),
    })
}
