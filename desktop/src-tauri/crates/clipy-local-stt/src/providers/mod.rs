//! Hosted transcription with the user's own key. Request shapes and limits
//! mirror `packages/transcribe/src/providers/*`.

use std::cell::OnceCell;

use crate::audio::{
    detect_silences, duration_sec, slice_secs, wav_from_pcm, SAMPLE_RATE, SILENCE_MIN_SEC,
    SILENCE_NOISE_DB,
};
use crate::chunking::{
    merge_pieces, plan_chunks, speech_sec_in, trim_to_speech, ChunkPlanOptions, PieceResult,
    RelativeSegment, SilenceSpan,
};
use crate::error::Result;
use crate::local::transcribe_local;
use crate::models::LocalModelId;
use crate::{CancelToken, Gap, Segment, Transcript};

mod assemblyai;
mod deepgram;
mod elevenlabs;
mod gemini;
pub(crate) mod http;
mod openai;

pub use assemblyai::ASSEMBLYAI_BASE_URL;
pub use deepgram::{DEEPGRAM_BASE_URL, DEEPGRAM_DEFAULT_MODEL};
pub use elevenlabs::{ELEVENLABS_BASE_URL, ELEVENLABS_DEFAULT_MODEL};
pub use gemini::{GEMINI_BASE_URL, GEMINI_DEFAULT_MODEL};
pub use http::MAX_RESPONSE_BYTES;
pub use openai::{
    GROQ_BASE_URL, GROQ_DEFAULT_MODEL, OPENAI_BASE_URL, OPENAI_DEFAULT_MODEL,
    OPENAI_MAX_UPLOAD_BYTES,
};

/// Where the transcript comes from: the Rust form of the TS
/// `TranscriptionConfig`. `Local` runs a downloaded model on this machine;
/// every hosted kind sends the audio to that provider with the user's key.
/// `base_url` on the hosted providers is optional and only needed for a
/// regional endpoint, a proxy or a test server.
#[derive(Clone, PartialEq, Eq)]
pub enum ProviderConfig {
    None,
    Local {
        model: LocalModelId,
    },
    OpenAiCompatible {
        base_url: String,
        api_key: String,
        model: String,
    },
    Deepgram {
        api_key: String,
        model: Option<String>,
        base_url: Option<String>,
    },
    AssemblyAi {
        api_key: String,
        model: Option<String>,
        base_url: Option<String>,
    },
    ElevenLabs {
        api_key: String,
        model: Option<String>,
        base_url: Option<String>,
    },
    Gemini {
        api_key: String,
        model: Option<String>,
        base_url: Option<String>,
    },
}

impl ProviderConfig {
    /// OpenAI's own endpoint with `whisper-1`.
    pub fn openai(api_key: impl Into<String>) -> Self {
        ProviderConfig::OpenAiCompatible {
            base_url: OPENAI_BASE_URL.into(),
            api_key: api_key.into(),
            model: OPENAI_DEFAULT_MODEL.into(),
        }
    }

    /// Groq's OpenAI-compatible endpoint with `whisper-large-v3-turbo`.
    pub fn groq(api_key: impl Into<String>) -> Self {
        ProviderConfig::OpenAiCompatible {
            base_url: GROQ_BASE_URL.into(),
            api_key: api_key.into(),
            model: GROQ_DEFAULT_MODEL.into(),
        }
    }

    /// The TS `kind` string.
    pub fn kind(&self) -> &'static str {
        match self {
            ProviderConfig::None => "none",
            ProviderConfig::Local { .. } => "local",
            ProviderConfig::OpenAiCompatible { .. } => "openai-compatible",
            ProviderConfig::Deepgram { .. } => "deepgram",
            ProviderConfig::AssemblyAi { .. } => "assemblyai",
            ProviderConfig::ElevenLabs { .. } => "elevenlabs",
            ProviderConfig::Gemini { .. } => "gemini",
        }
    }
}

/// Debug never prints a key.
impl std::fmt::Debug for ProviderConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ProviderConfig::Local { model } => write!(f, "Local({})", model.as_str()),
            ProviderConfig::OpenAiCompatible {
                base_url, model, ..
            } => write!(f, "OpenAiCompatible({base_url}, {model})"),
            other => write!(f, "{}", other.kind()),
        }
    }
}

/// Transcribe 16 kHz mono samples with `config`. `None` returns one gap over
/// the whole recording; `Local` uses the default model folder (call
/// [`transcribe_local`] directly for another folder or for progress).
pub fn transcribe_with_provider(
    pcm: &[f32],
    config: &ProviderConfig,
    cancel: &CancelToken,
) -> Result<Transcript> {
    cancel.check()?;
    let total_ms = (duration_sec(pcm) * 1000.0).round() as u64;
    let input = AudioInput::new(pcm, cancel);
    match config {
        ProviderConfig::None => Ok(Transcript {
            segments: Vec::new(),
            engine: "none".into(),
            language: None,
            gaps: if total_ms > 0 {
                vec![Gap {
                    start_ms: 0,
                    end_ms: total_ms,
                    reason: "transcription off".into(),
                }]
            } else {
                Vec::new()
            },
        }),
        ProviderConfig::Local { model } => transcribe_local(pcm, *model, None, &mut |_| {}, cancel),
        ProviderConfig::OpenAiCompatible {
            base_url,
            api_key,
            model,
        } => openai::transcribe(&input, base_url, api_key, model, None),
        ProviderConfig::Deepgram {
            api_key,
            model,
            base_url,
        } => deepgram::transcribe(&input, api_key, model.as_deref(), base_url.as_deref()),
        ProviderConfig::AssemblyAi {
            api_key,
            model,
            base_url,
        } => assemblyai::transcribe(
            &input,
            api_key,
            model.as_deref(),
            base_url.as_deref(),
            &assemblyai::Limits::default(),
        ),
        ProviderConfig::ElevenLabs {
            api_key,
            model,
            base_url,
        } => elevenlabs::transcribe(&input, api_key, model.as_deref(), base_url.as_deref()),
        ProviderConfig::Gemini {
            api_key,
            model,
            base_url,
        } => gemini::transcribe(&input, api_key, model.as_deref(), base_url.as_deref(), None),
    }
}

/// What every hosted engine is handed: the samples and a way to find their silences.
pub(crate) struct AudioInput<'a> {
    pub pcm: &'a [f32],
    pub cancel: &'a CancelToken,
    silences: OnceCell<Vec<SilenceSpan>>,
}

impl<'a> AudioInput<'a> {
    pub fn new(pcm: &'a [f32], cancel: &'a CancelToken) -> Self {
        AudioInput {
            pcm,
            cancel,
            silences: OnceCell::new(),
        }
    }
    pub fn duration_sec(&self) -> f64 {
        duration_sec(self.pcm)
    }
    /// Silence spans, computed once on first use.
    pub fn silences(&self) -> &[SilenceSpan] {
        self.silences
            .get_or_init(|| detect_silences(self.pcm, SILENCE_NOISE_DB, SILENCE_MIN_SEC))
    }
    /// The whole recording as a 16-bit WAV.
    pub fn wav(&self) -> Vec<u8> {
        wav_from_pcm(self.pcm, SAMPLE_RATE)
    }
}

pub(crate) struct PieceOutput {
    pub segments: Vec<RelativeSegment>,
    pub language: Option<String>,
}

/// Longest WAV (header included) that fits in `max_bytes`, in seconds.
pub(crate) fn max_sec_for_bytes(max_bytes: u64) -> f64 {
    ((max_bytes - 44) / (SAMPLE_RATE as u64 * 2)) as f64
}

/// Send the audio to a provider in pieces small enough for its limit, cut in
/// silences, one at a time, and merge the results. Pieces with no speech are
/// skipped (no request, no gap).
pub(crate) fn run_in_pieces(
    input: &AudioInput,
    target_sec: f64,
    max_sec: f64,
    mut run: impl FnMut(Vec<u8>, f64) -> Result<PieceOutput>,
) -> Result<(Vec<Segment>, Option<String>)> {
    let total = input.duration_sec();
    let silences: &[SilenceSpan] = if total > max_sec {
        input.silences()
    } else {
        &[]
    };
    let opts = ChunkPlanOptions {
        target_sec,
        max_sec,
        overlap_sec: 0.0,
        min_sec: 15f64.min(target_sec / 2.0),
    };
    let plan = plan_chunks(silences, total, &opts);
    let mut pieces = Vec::new();
    let mut language = None;
    for p in &plan {
        input.cancel.check()?;
        if plan.len() > 1
            && speech_sec_in(
                silences,
                p.extract_start_sec,
                p.extract_start_sec + p.extract_dur_sec,
            ) < 0.3
        {
            continue;
        }
        let wav = wav_from_pcm(
            slice_secs(input.pcm, p.extract_start_sec, p.extract_dur_sec),
            SAMPLE_RATE,
        );
        let out = run(wav, p.extract_dur_sec)?;
        if language.is_none() {
            language = out.language;
        }
        pieces.push(PieceResult {
            plan: *p,
            segments: out.segments,
        });
    }
    Ok((merge_pieces(&pieces), language))
}

/// A provider that only returned plain text: one segment over the speech in the piece.
pub(crate) fn whole_piece_segment(text: &str, dur_sec: f64) -> Vec<RelativeSegment> {
    let t = text.trim();
    if t.is_empty() {
        return Vec::new();
    }
    let (s, e) = trim_to_speech(&[], 0.0, dur_sec);
    vec![RelativeSegment {
        start: s,
        end: e.max(s),
        text: t.to_string(),
    }]
}

pub(crate) fn engine_name(provider: &str, model: Option<&str>) -> String {
    match model {
        Some(m) if !m.is_empty() => format!("{provider}:{m}"),
        _ => provider.to_string(),
    }
}

#[cfg(test)]
mod tests;
