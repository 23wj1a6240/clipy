use std::fmt;

pub type Result<T, E = SttError> = std::result::Result<T, E>;

#[derive(Debug, thiserror::Error)]
pub enum SttError {
    /// The chosen model has not been downloaded. Carries what the app needs
    /// to ask for consent: the model's label and its download size.
    #[error("The \"{label}\" model is not installed. It is a one-time {} download.", format_mb(*size_bytes))]
    ModelNotInstalled {
        model: &'static str,
        label: &'static str,
        size_bytes: u64,
    },

    #[error("Cancelled")]
    Cancelled,

    #[error("Checksum mismatch for {0}: the download is corrupt or was changed. Nothing was installed; try again.")]
    Checksum(String),

    #[error("The download of {what} is larger than the expected {max_bytes} bytes, so it was stopped. Nothing was installed.")]
    DownloadTooLarge { what: String, max_bytes: u64 },

    /// A model download failed for another reason (network, HTTP status,
    /// unpacking). The message never holds a response body.
    #[error("{0}")]
    Download(String),

    #[error("{0}")]
    Provider(ProviderError),

    /// The local engine failed (model would not load, every piece failed).
    #[error("{0}")]
    Engine(String),

    #[error("{0}")]
    InvalidInput(String),

    #[error("{0}")]
    Io(#[from] std::io::Error),
}

/// A hosted provider failed. Names the provider and the HTTP status only:
/// response bodies can echo transcript text and requests carry the key.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderError {
    pub provider: String,
    pub status: Option<u16>,
    pub detail: Option<String>,
}

impl ProviderError {
    pub fn new(provider: &str, status: Option<u16>, detail: Option<&str>) -> Self {
        Self {
            provider: provider.to_string(),
            status,
            detail: detail.map(str::to_string),
        }
    }
}

impl fmt::Display for ProviderError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.status {
            None => write!(
                f,
                "{} transcription failed: {}",
                self.provider,
                self.detail
                    .as_deref()
                    .unwrap_or("the provider could not be reached")
            ),
            Some(status) => {
                write!(f, "{} transcription failed (HTTP {status})", self.provider)?;
                match status {
                    401 | 403 => write!(f, ": check the API key"),
                    413 => write!(f, ": the audio is too large for this provider"),
                    429 => write!(f, ": rate limited, try again later"),
                    _ => match &self.detail {
                        Some(d) => write!(f, ": {d}"),
                        None => Ok(()),
                    },
                }
            }
        }
    }
}

impl From<ProviderError> for SttError {
    fn from(e: ProviderError) -> Self {
        SttError::Provider(e)
    }
}

pub(crate) fn format_mb(bytes: u64) -> String {
    format!("{} MB", (bytes as f64 / 1_000_000.0).round() as u64)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn provider_errors_name_status_only() {
        let e = ProviderError::new("deepgram", Some(401), Some("secret body"));
        assert_eq!(
            e.to_string(),
            "deepgram transcription failed (HTTP 401): check the API key"
        );
        let e = ProviderError::new("gemini", None, None);
        assert_eq!(
            e.to_string(),
            "gemini transcription failed: the provider could not be reached"
        );
        let e = SttError::ModelNotInstalled {
            model: "english-fast",
            label: "English",
            size_bytes: 104_337_827,
        };
        assert!(e.to_string().contains("104 MB"));
    }
}
