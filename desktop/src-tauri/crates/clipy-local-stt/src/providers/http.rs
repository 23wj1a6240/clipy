//! Shared HTTP plumbing for the hosted providers. Errors name the provider
//! and the HTTP status only: response bodies can echo transcript text, and
//! request details carry the user's key, so neither is ever put in a message.

use std::io::{self, Read};
use std::sync::mpsc::{self, RecvTimeoutError};
use std::thread;
use std::time::Duration;

use reqwest::blocking::{Client, RequestBuilder, Response};

use crate::error::{ProviderError, Result, SttError};
use crate::CancelToken;

/// Largest provider answer read into memory. A transcript of hours of speech is a few MB.
pub const MAX_RESPONSE_BYTES: u64 = 20 * 1024 * 1024;

/// One client for every provider call. Redirects are refused: requests carry
/// the user's key in headers, and a redirect must not take it to another host.
pub(crate) fn client() -> Result<Client> {
    Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .connect_timeout(Duration::from_secs(30))
        // Long enough to upload an hour of WAV on a slow link and wait for the answer.
        .timeout(Duration::from_secs(60 * 60))
        .user_agent(concat!("clipy-local-stt/", env!("CARGO_PKG_VERSION")))
        .build()
        .map_err(|_| {
            SttError::Provider(ProviderError::new(
                "transcription",
                None,
                Some("the HTTP client could not start"),
            ))
        })
}

/// Send on a helper thread so a cancel returns at once even while the
/// provider is still working; the abandoned request finishes in the
/// background and its answer is dropped.
fn send_cancellable(provider: &str, req: RequestBuilder, cancel: &CancelToken) -> Result<Response> {
    let (tx, rx) = mpsc::channel();
    thread::Builder::new()
        .name("clipy-stt-http".into())
        .spawn(move || {
            let _ = tx.send(req.send());
        })
        .map_err(SttError::Io)?;
    loop {
        cancel.check()?;
        match rx.recv_timeout(Duration::from_millis(50)) {
            Ok(Ok(res)) => return Ok(res),
            Ok(Err(e)) => {
                cancel.check()?;
                let detail = if e.is_timeout() {
                    "the provider did not answer in time"
                } else if e.is_redirect() || e.status().is_some_and(|s| s.is_redirection()) {
                    "the provider answered with a redirect"
                } else {
                    "the provider could not be reached"
                };
                return Err(ProviderError::new(provider, None, Some(detail)).into());
            }
            Err(RecvTimeoutError::Timeout) => continue,
            Err(RecvTimeoutError::Disconnected) => {
                return Err(ProviderError::new(
                    provider,
                    None,
                    Some("the provider could not be reached"),
                )
                .into())
            }
        }
    }
}

/// Send, turn a non-2xx answer into a ProviderError (the body is never
/// read), and read the body as JSON, failing past `MAX_RESPONSE_BYTES`.
pub(crate) fn request_json(
    provider: &str,
    req: RequestBuilder,
    cancel: &CancelToken,
) -> Result<serde_json::Value> {
    request_json_capped(provider, req, cancel, MAX_RESPONSE_BYTES)
}

pub(crate) fn request_json_capped(
    provider: &str,
    req: RequestBuilder,
    cancel: &CancelToken,
    max_bytes: u64,
) -> Result<serde_json::Value> {
    let mut res = send_cancellable(provider, req, cancel)?;
    let status = res.status();
    if !status.is_success() {
        return Err(ProviderError::new(provider, Some(status.as_u16()), None).into());
    }
    let too_large = || {
        SttError::from(ProviderError::new(
            provider,
            None,
            Some(&format!(
                "the answer was larger than {} MB",
                max_bytes / (1024 * 1024)
            )),
        ))
    };
    if res.content_length().is_some_and(|n| n > max_bytes) {
        return Err(too_large());
    }
    let mut body = Vec::new();
    let mut buf = [0u8; 16 * 1024];
    loop {
        cancel.check()?;
        let n = match res.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => n,
            Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
            Err(_) => {
                cancel.check()?;
                return Err(
                    ProviderError::new(provider, None, Some("the answer was cut off")).into(),
                );
            }
        };
        if body.len() as u64 + n as u64 > max_bytes {
            return Err(too_large());
        }
        body.extend_from_slice(&buf[..n]);
    }
    serde_json::from_slice(&body).map_err(|_| {
        ProviderError::new(
            provider,
            Some(status.as_u16()),
            Some("the answer was not valid JSON"),
        )
        .into()
    })
}

/// A reader over an in-memory upload that stops once the token is cancelled,
/// so a long upload ends promptly.
pub(crate) struct CancelReader {
    data: io::Cursor<Vec<u8>>,
    cancel: CancelToken,
}

impl CancelReader {
    pub fn new(data: Vec<u8>, cancel: &CancelToken) -> Self {
        CancelReader {
            data: io::Cursor::new(data),
            cancel: cancel.clone(),
        }
    }
}

impl Read for CancelReader {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if self.cancel.is_cancelled() {
            return Err(io::Error::other("cancelled"));
        }
        let n = buf.len().min(256 * 1024);
        self.data.read(&mut buf[..n])
    }
}

/// An upload body of known length that honours cancellation.
pub(crate) fn upload_body(data: Vec<u8>, cancel: &CancelToken) -> reqwest::blocking::Body {
    let len = data.len() as u64;
    reqwest::blocking::Body::sized(CancelReader::new(data, cancel), len)
}

/// A multipart file part of known length that honours cancellation.
pub(crate) fn file_part(
    data: Vec<u8>,
    file_name: &str,
    mime: &str,
    cancel: &CancelToken,
) -> Result<reqwest::blocking::multipart::Part> {
    let len = data.len() as u64;
    reqwest::blocking::multipart::Part::reader_with_length(CancelReader::new(data, cancel), len)
        .file_name(file_name.to_string())
        .mime_str(mime)
        .map_err(|_| SttError::InvalidInput("bad MIME type".into()))
}

/// Sleep in small steps so a cancel is noticed.
pub(crate) fn sleep(d: Duration, cancel: &CancelToken) -> Result<()> {
    let step = Duration::from_millis(50);
    let mut left = d;
    while !left.is_zero() {
        cancel.check()?;
        let s = left.min(step);
        thread::sleep(s);
        left -= s;
    }
    cancel.check()
}
