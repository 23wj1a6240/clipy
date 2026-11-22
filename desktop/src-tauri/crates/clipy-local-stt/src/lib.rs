//! Timestamped transcripts from 16 kHz mono audio, for the local edition of
//! the Clipy desktop app.
//!
//! The Rust twin of the `@clipy/transcribe` TypeScript package: the same
//! models with the same pinned checksums, the same model folder (so a model
//! downloaded by the CLI is reused here and the other way round), the same
//! chunking and merging, and the same request shapes for the hosted
//! providers.
//!
//! - [`transcribe_local`]: a downloaded sherpa-onnx model run in this
//!   process. Nothing leaves the machine.
//! - [`transcribe_with_provider`]: the audio is sent to a provider with the
//!   user's own key.
//! - [`download_model`] / [`model_status`]: nothing is downloaded unless the
//!   caller asks, which the app does only after the user agrees.
//!
//! Every public call is blocking. Call it from a plain thread (or
//! `spawn_blocking`), never from inside an async task: the HTTP client is
//! `reqwest::blocking`, which runs its own small runtime.
//!
//! Nothing here logs. Errors name the provider and HTTP status, never a key,
//! a response body or transcript text.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use serde::{Deserialize, Serialize};

pub mod audio;
pub mod chunking;
mod engine;
mod error;
mod local;
pub mod models;
pub mod providers;
mod util;

#[cfg(test)]
mod test_server;

pub use audio::{read_wav, read_wav_bytes, wav_from_pcm, SAMPLE_RATE};
pub use error::{Result, SttError};
pub use local::{transcribe_local, LOCAL_ENGINE_AVAILABLE};
pub use models::{default_model_dir, download_model, model_status, LocalModelId, ModelStatus};
pub use providers::{transcribe_with_provider, ProviderConfig};

/// One spoken stretch, in milliseconds from the start of the recording.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Segment {
    pub start_ms: u64,
    pub end_ms: u64,
    pub text: String,
}

/// A stretch of the recording with no transcript, and why.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Gap {
    pub start_ms: u64,
    pub end_ms: u64,
    pub reason: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Transcript {
    pub segments: Vec<Segment>,
    /// Which engine produced the segments, e.g. `local:english-fast` or `deepgram:nova-3`.
    pub engine: String,
    /// Language code when the engine reports one.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub language: Option<String>,
    pub gaps: Vec<Gap>,
}

/// Shared cancellation flag. Clone it, hand one copy to the call and keep
/// the other to cancel from any thread.
#[derive(Clone, Debug, Default)]
pub struct CancelToken(Arc<AtomicBool>);

impl CancelToken {
    pub fn new() -> Self {
        Self::default()
    }
    pub fn cancel(&self) {
        self.0.store(true, Ordering::SeqCst);
    }
    pub fn is_cancelled(&self) -> bool {
        self.0.load(Ordering::SeqCst)
    }
    pub(crate) fn check(&self) -> Result<()> {
        if self.is_cancelled() {
            Err(SttError::Cancelled)
        } else {
            Ok(())
        }
    }
}
