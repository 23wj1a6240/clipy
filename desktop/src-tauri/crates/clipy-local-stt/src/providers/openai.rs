//! Any OpenAI-compatible `/audio/transcriptions` endpoint: OpenAI
//! (`whisper-1`), Groq (`whisper-large-v3-turbo`), or a self-hosted server
//! speaking the same API. Segment timestamps need
//! `response_format=verbose_json`, which OpenAI's `gpt-4o-*-transcribe`
//! models do not offer; those come back as one segment per piece.

use reqwest::blocking::multipart::Form;
use serde::Deserialize;

use super::http::{client, file_part, request_json};
use super::{
    engine_name, max_sec_for_bytes, run_in_pieces, whole_piece_segment, AudioInput, PieceOutput,
};
use crate::chunking::RelativeSegment;
use crate::error::{ProviderError, Result};
use crate::util::join_url;
use crate::Transcript;

pub const OPENAI_BASE_URL: &str = "https://api.openai.com/v1";
pub const OPENAI_DEFAULT_MODEL: &str = "whisper-1";
pub const GROQ_BASE_URL: &str = "https://api.groq.com/openai/v1";
pub const GROQ_DEFAULT_MODEL: &str = "whisper-large-v3-turbo";
/// OpenAI rejects uploads over 25 MB; stay under it with room for the form.
pub const OPENAI_MAX_UPLOAD_BYTES: u64 = 24 * 1024 * 1024;

const PROVIDER: &str = "openai-compatible";

#[derive(Deserialize)]
struct VerboseJson {
    text: Option<String>,
    language: Option<String>,
    segments: Option<Vec<Seg>>,
}

#[derive(Deserialize)]
struct Seg {
    start: f64,
    end: f64,
    text: String,
}

pub(crate) fn transcribe(
    input: &AudioInput,
    base_url: &str,
    api_key: &str,
    model: &str,
    max_upload_bytes: Option<u64>,
) -> Result<Transcript> {
    let max_sec = max_sec_for_bytes(max_upload_bytes.unwrap_or(OPENAI_MAX_UPLOAD_BYTES));
    let target_sec = 600f64.min((max_sec * 0.8).floor());
    let url = join_url(base_url, "audio/transcriptions");
    let http = client()?;
    let (segments, language) = run_in_pieces(input, target_sec, max_sec, |wav, dur_sec| {
        let form = Form::new()
            .part(
                "file",
                file_part(wav, "audio.wav", "audio/wav", input.cancel)?,
            )
            .text("model", model.to_string())
            .text("response_format", "verbose_json")
            .text("timestamp_granularities[]", "segment");
        let mut req = http.post(&url).multipart(form);
        if !api_key.is_empty() {
            req = req.bearer_auth(api_key);
        }
        let raw = request_json(PROVIDER, req, input.cancel)?;
        let parsed: VerboseJson = serde_json::from_value(raw).map_err(|_| {
            ProviderError::new(PROVIDER, Some(200), Some("unexpected answer shape"))
        })?;
        let segments = match parsed.segments {
            Some(segs) if !segs.is_empty() => segs
                .into_iter()
                .map(|s| RelativeSegment {
                    start: s.start,
                    end: s.end,
                    text: s.text,
                })
                .collect(),
            _ => whole_piece_segment(parsed.text.as_deref().unwrap_or(""), dur_sec),
        };
        Ok(PieceOutput {
            segments,
            language: parsed.language,
        })
    })?;
    Ok(Transcript {
        segments,
        engine: engine_name(PROVIDER, Some(model)),
        language,
        gaps: Vec::new(),
    })
}
