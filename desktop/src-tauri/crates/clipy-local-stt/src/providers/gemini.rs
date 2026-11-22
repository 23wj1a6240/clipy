//! Google Gemini `generateContent` with the audio inline, asked for JSON
//! segments. Gemini is a general model, not a speech recognizer, so its
//! timestamps are approximate; pieces are kept short to keep them close, and
//! every answer is validated before use.

use base64::Engine as _;
use serde_json::{json, Value};

use super::http::{client, request_json};
use super::{engine_name, run_in_pieces, AudioInput, PieceOutput};
use crate::chunking::RelativeSegment;
use crate::error::{ProviderError, Result};
use crate::util::{join_url, urlencode};
use crate::Transcript;

pub const GEMINI_BASE_URL: &str = "https://generativelanguage.googleapis.com/v1beta";
pub const GEMINI_DEFAULT_MODEL: &str = "gemini-3.5-flash-lite";
const PROVIDER: &str = "gemini";

const PROMPT: &str = concat!(
    "Transcribe all speech in this audio, verbatim, in the language spoken. ",
    "Answer with JSON only: {\"language\": \"<ISO 639-1 code>\", \"segments\": [{\"start\": <seconds>, \"end\": <seconds>, \"text\": \"<words>\"}]}. ",
    "Times are seconds from the start of this audio. Make one segment per sentence or per pause, at most 15 seconds each, in order. ",
    "If there is no speech, answer {\"segments\": []}."
);

fn response_schema() -> Value {
    json!({
        "type": "OBJECT",
        "properties": {
            "language": { "type": "STRING" },
            "segments": {
                "type": "ARRAY",
                "items": {
                    "type": "OBJECT",
                    "properties": { "start": { "type": "NUMBER" }, "end": { "type": "NUMBER" }, "text": { "type": "STRING" } },
                    "required": ["start", "end", "text"]
                }
            }
        },
        "required": ["segments"]
    })
}

/// Seconds as a number, or "SS", "MM:SS", "HH:MM:SS(.ms)" strings the model sometimes writes.
fn seconds(v: &Value) -> Option<f64> {
    let n = match v {
        Value::Number(n) => n.as_f64()?,
        Value::String(s) => {
            let s = s.trim();
            let s = s.strip_suffix('s').unwrap_or(s);
            let parts: Vec<&str> = s.split(':').collect();
            if parts.is_empty() || parts.len() > 3 {
                return None;
            }
            let mut acc = 0.0;
            for p in parts {
                let x: f64 = p.trim().parse().ok()?;
                acc = acc * 60.0 + x;
            }
            acc
        }
        _ => return None,
    };
    (n.is_finite() && n >= 0.0).then_some(n)
}

/// A validated Gemini transcript: `None` when the JSON does not match.
pub(crate) fn parse_transcript(v: &Value) -> Option<(Option<String>, Vec<RelativeSegment>)> {
    let obj = v.as_object()?;
    let language = match obj.get("language") {
        None | Some(Value::Null) => None,
        Some(Value::String(s)) => Some(s.clone()),
        Some(_) => return None,
    };
    let mut out = Vec::new();
    for s in obj.get("segments")?.as_array()? {
        let start = seconds(s.get("start")?)?;
        let end = seconds(s.get("end")?)?;
        let text = s.get("text")?.as_str()?.to_string();
        out.push(RelativeSegment { start, end, text });
    }
    Some((language.filter(|l| !l.is_empty()), out))
}

/// Pull the JSON object out of a reply, tolerating a ```json fence.
fn parse_reply(text: &str) -> Option<Value> {
    let body = match text.find("```") {
        Some(open) => {
            let after = &text[open + 3..];
            let after = after.strip_prefix("json").unwrap_or(after);
            match after.find("```") {
                Some(close) => &after[..close],
                None => text,
            }
        }
        None => text,
    };
    serde_json::from_str(body.trim()).ok()
}

pub(crate) fn transcribe(
    input: &AudioInput,
    api_key: &str,
    model: Option<&str>,
    base_url: Option<&str>,
    limits: Option<(f64, f64)>,
) -> Result<Transcript> {
    let model = model
        .filter(|m| !m.is_empty())
        .unwrap_or(GEMINI_DEFAULT_MODEL);
    let path = format!("models/{}:generateContent", urlencode(model));
    let url = join_url(base_url.unwrap_or(GEMINI_BASE_URL), &path);
    // Inline requests are capped at 20 MB; 5 minutes of WAV is about 13 MB once base64 encoded.
    let (max_sec, target_sec) = limits.unwrap_or((300.0, 240.0));
    let http = client()?;
    let (segments, language) = run_in_pieces(input, target_sec, max_sec, |wav, dur_sec| {
        let body = json!({
            "contents": [{
                "role": "user",
                "parts": [
                    { "inline_data": { "mime_type": "audio/wav", "data": base64::engine::general_purpose::STANDARD.encode(&wav) } },
                    { "text": PROMPT }
                ]
            }],
            "generationConfig": { "temperature": 0, "responseMimeType": "application/json", "responseSchema": response_schema() }
        });
        let req = http
            .post(&url)
            .header("x-goog-api-key", api_key)
            .header("Content-Type", "application/json")
            .body(body.to_string());
        let raw = request_json(PROVIDER, req, input.cancel)?;
        let text: String = raw
            .pointer("/candidates/0/content/parts")
            .and_then(Value::as_array)
            .map(|parts| {
                parts
                    .iter()
                    .filter_map(|p| p.get("text").and_then(Value::as_str))
                    .collect()
            })
            .unwrap_or_default();
        if text.trim().is_empty() {
            return Err(ProviderError::new(
                PROVIDER,
                Some(200),
                Some("the answer had no transcript"),
            )
            .into());
        }
        let json = parse_reply(&text).ok_or_else(|| {
            ProviderError::new(PROVIDER, Some(200), Some("the answer was not valid JSON"))
        })?;
        let (language, segs) = parse_transcript(&json).ok_or_else(|| {
            ProviderError::new(
                PROVIDER,
                Some(200),
                Some("the answer did not match the transcript format"),
            )
        })?;
        let segments = segs
            .into_iter()
            .filter(|s| !s.text.trim().is_empty() && s.start <= dur_sec + 1.0)
            .map(|s| {
                let start = s.start.min(dur_sec);
                RelativeSegment {
                    start,
                    end: s.end.max(start).min(dur_sec),
                    text: s.text,
                }
            })
            .collect();
        Ok(PieceOutput { segments, language })
    })?;
    Ok(Transcript {
        segments,
        engine: engine_name(PROVIDER, Some(model)),
        language,
        gaps: Vec::new(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn seconds_accept_numbers_and_clock_strings() {
        assert_eq!(seconds(&json!(4.5)), Some(4.5));
        assert_eq!(seconds(&json!("1:02.5")), Some(62.5));
        assert_eq!(seconds(&json!("01:00:01")), Some(3601.0));
        assert_eq!(seconds(&json!("7s")), Some(7.0));
        assert_eq!(seconds(&json!(-1)), None);
        assert_eq!(seconds(&json!("a:b")), None);
        assert_eq!(seconds(&json!(null)), None);
    }

    #[test]
    fn replies_with_fences_parse_and_bad_shapes_do_not() {
        let v = parse_reply(
            "```json\n{\"segments\": [{\"start\": 0, \"end\": \"0:02\", \"text\": \"hi\"}]}\n```",
        )
        .unwrap();
        let (lang, segs) = parse_transcript(&v).unwrap();
        assert_eq!(lang, None);
        assert_eq!(segs[0].end, 2.0);
        assert!(parse_transcript(&json!({"segments": [{"start": 0, "text": "x"}]})).is_none());
        assert!(parse_transcript(&json!({"language": 3, "segments": []})).is_none());
        assert!(parse_reply("not json").is_none());
    }
}
