//! Deepgram pre-recorded `/v1/listen`, with utterances for segment boundaries.

use serde::Deserialize;

use super::http::{client, request_json, upload_body};
use super::{engine_name, AudioInput};
use crate::chunking::{group_words_into_segments, to_timeline, RelativeSegment, TimedWord};
use crate::error::{ProviderError, Result};
use crate::util::join_url;
use crate::Transcript;

pub const DEEPGRAM_BASE_URL: &str = "https://api.deepgram.com";
pub const DEEPGRAM_DEFAULT_MODEL: &str = "nova-3";
const PROVIDER: &str = "deepgram";

#[derive(Deserialize)]
struct Answer {
    results: Results,
}

#[derive(Deserialize)]
struct Results {
    utterances: Option<Vec<Utterance>>,
    channels: Option<Vec<Channel>>,
}

#[derive(Deserialize)]
struct Utterance {
    start: f64,
    end: f64,
    transcript: String,
}

#[derive(Deserialize)]
struct Channel {
    detected_language: Option<String>,
    alternatives: Option<Vec<Alternative>>,
}

#[derive(Deserialize)]
struct Alternative {
    words: Option<Vec<Word>>,
}

#[derive(Deserialize)]
struct Word {
    start: f64,
    end: f64,
    word: String,
    punctuated_word: Option<String>,
}

pub(crate) fn transcribe(
    input: &AudioInput,
    api_key: &str,
    model: Option<&str>,
    base_url: Option<&str>,
) -> Result<Transcript> {
    let model = model
        .filter(|m| !m.is_empty())
        .unwrap_or(DEEPGRAM_DEFAULT_MODEL);
    let url = join_url(base_url.unwrap_or(DEEPGRAM_BASE_URL), "v1/listen");
    let query = [
        ("model", model),
        ("smart_format", "true"),
        ("punctuate", "true"),
        ("utterances", "true"),
        ("detect_language", "true"),
    ];
    let req = client()?
        .post(&url)
        .query(&query)
        .header("Authorization", format!("Token {api_key}"))
        .header("Content-Type", "audio/wav")
        .body(upload_body(input.wav(), input.cancel));
    let raw = request_json(PROVIDER, req, input.cancel)?;
    let parsed: Answer = serde_json::from_value(raw)
        .map_err(|_| ProviderError::new(PROVIDER, Some(200), Some("unexpected answer shape")))?;
    let channel = parsed.results.channels.and_then(|c| c.into_iter().next());
    let language = channel.as_ref().and_then(|c| c.detected_language.clone());
    let segments: Vec<RelativeSegment> = match parsed.results.utterances {
        Some(u) if !u.is_empty() => u
            .into_iter()
            .map(|u| RelativeSegment {
                start: u.start,
                end: u.end,
                text: u.transcript,
            })
            .collect(),
        _ => {
            let words: Vec<TimedWord> = channel
                .and_then(|c| c.alternatives)
                .and_then(|a| a.into_iter().next())
                .and_then(|a| a.words)
                .unwrap_or_default()
                .into_iter()
                .map(|w| TimedWord {
                    start: w.start,
                    end: w.end,
                    text: w.punctuated_word.unwrap_or(w.word),
                })
                .collect();
            group_words_into_segments(&words, " ")
        }
    };
    Ok(Transcript {
        segments: to_timeline(segments),
        engine: engine_name(PROVIDER, Some(model)),
        language,
        gaps: Vec::new(),
    })
}
