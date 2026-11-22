//! Local WebSocket that carries camera preview frames to the bubble window.
//!
//! This is Cap's default camera preview transport (`frame_ws.rs` and the
//! `cameraWsPort` socket in their camera window), ported. The camera callback
//! publishes uncompressed RGBA frames into a `tokio::sync::watch` channel, so
//! only the newest frame is ever waiting, and each connected webview is sent
//! that frame as one binary message. The page draws it straight onto its
//! canvas.
//!
//! It replaces a bridge that compressed every frame to JPEG, base64-encoded
//! it into a JSON event, and decoded it again in the page. ScreenCaptureKit
//! records the bubble window, so every step between the camera and the canvas
//! was delay between the face in the video and the voice from the microphone,
//! which is recorded without passing through any of it.
//!
//! Cap binds the same kind of socket to 127.0.0.1 with no further check. This
//! one also requires a per-launch random token in the URL and an `Origin`
//! from the app's own webview, so neither another local process nor a web
//! page open in a browser can read the camera through it.
//!
//! Nothing here is platform-specific, so the tests run off-Mac.

use std::future::Future;
use std::net::{Ipv4Addr, SocketAddr};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use futures::{SinkExt, StreamExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::watch;
use tokio_tungstenite::tungstenite::handshake::server::{ErrorResponse, Request, Response};
use tokio_tungstenite::tungstenite::http::StatusCode;
use tokio_tungstenite::tungstenite::Message;

/// Path the page connects to.
pub const PATH: &str = "/camera";

/// Bytes appended after the pixels: width, height, frame number and a format
/// tag, each a little-endian `u32`. A trailer rather than a header lets the
/// pixel buffer be sent without being copied into a new allocation.
pub const TRAILER_LEN: usize = 16;

/// Format tag for tightly packed RGBA with straight (not premultiplied) alpha,
/// which is what `ImageData` expects. ASCII "RGBA".
pub const FORMAT_RGBA: u32 = u32::from_le_bytes(*b"RGBA");

/// A frame that reached no page within this long is dropped, and so is the
/// connection: a webview that stops reading must not hold a socket open.
const SEND_TIMEOUT: Duration = Duration::from_secs(5);

/// Origins the app's own webviews load from. `tauri://localhost` is the
/// bundled app on macOS; the `http(s)://tauri.localhost` pair is how Tauri
/// serves it on Windows; the last two are `devUrl` in `tauri.conf.json`.
const ALLOWED_ORIGINS: &[&str] = &[
    "tauri://localhost",
    "http://tauri.localhost",
    "https://tauri.localhost",
    #[cfg(debug_assertions)]
    "http://127.0.0.1:1420",
    #[cfg(debug_assertions)]
    "http://localhost:1420",
];

/// One RGBA frame, `width * height * 4` bytes, rows tightly packed.
pub struct PreviewFrame {
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
}

/// Packs a frame into the wire format, reusing the pixel buffer.
///
/// Returns `None` for a frame whose buffer does not hold exactly
/// `width * height * 4` bytes, which the page would otherwise misdraw.
pub fn pack(frame: PreviewFrame, frame_number: u32) -> Option<Bytes> {
    let expected = (frame.width as usize)
        .checked_mul(frame.height as usize)?
        .checked_mul(4)?;
    if frame.width == 0 || frame.height == 0 || frame.rgba.len() != expected {
        return None;
    }
    let mut data = frame.rgba;
    data.reserve_exact(TRAILER_LEN);
    data.extend_from_slice(&frame.width.to_le_bytes());
    data.extend_from_slice(&frame.height.to_le_bytes());
    data.extend_from_slice(&frame_number.to_le_bytes());
    data.extend_from_slice(&FORMAT_RGBA.to_le_bytes());
    Some(Bytes::from(data))
}

pub struct CameraPreviewServer {
    port: u16,
    token: String,
    frames: watch::Sender<Option<Bytes>>,
    frame_number: AtomicU32,
}

impl CameraPreviewServer {
    /// Binds 127.0.0.1 on a free port and returns the server with the
    /// future that accepts connections. The caller spawns that future on its
    /// tokio runtime; binding happens here so a failure is reported at once.
    pub fn bind() -> std::io::Result<(Arc<Self>, impl Future<Output = ()> + Send + 'static)> {
        let listener = std::net::TcpListener::bind(SocketAddr::from((Ipv4Addr::LOCALHOST, 0)))?;
        listener.set_nonblocking(true)?;
        let port = listener.local_addr()?.port();
        let (frames, _) = watch::channel(None);
        let server = Arc::new(Self {
            port,
            token: random_token(),
            frames,
            frame_number: AtomicU32::new(0),
        });
        let accept = Arc::clone(&server).accept_loop(listener);
        Ok((server, accept))
    }

    pub fn port(&self) -> u16 {
        self.port
    }

    /// The URL the page connects to, token included.
    pub fn url(&self) -> String {
        format!("ws://127.0.0.1:{}{}?token={}", self.port, PATH, self.token)
    }

    /// Replaces the waiting frame. Never blocks: a page that has not taken
    /// the previous frame yet simply gets this one instead.
    pub fn publish(&self, frame: PreviewFrame) -> bool {
        let number = self.frame_number.fetch_add(1, Ordering::Relaxed);
        match pack(frame, number) {
            Some(packed) => {
                self.frames.send_replace(Some(packed));
                true
            }
            None => false,
        }
    }

    /// Forgets the last frame, so a page that connects after the camera
    /// stopped does not show the previous session's face.
    pub fn clear(&self) {
        self.frames.send_replace(None);
    }

    async fn accept_loop(self: Arc<Self>, listener: std::net::TcpListener) {
        let listener = match TcpListener::from_std(listener) {
            Ok(listener) => listener,
            Err(err) => {
                tracing::error!("camera preview socket could not start: {err}");
                return;
            }
        };
        loop {
            match listener.accept().await {
                Ok((stream, _)) => {
                    let server = Arc::clone(&self);
                    tokio::spawn(async move {
                        if let Err(err) = server.serve(stream).await {
                            tracing::debug!("camera preview connection ended: {err}");
                        }
                    });
                }
                Err(err) => {
                    tracing::warn!("camera preview accept failed: {err}");
                    tokio::time::sleep(Duration::from_millis(100)).await;
                }
            }
        }
    }

    // The handshake callback's error type is fixed by tungstenite.
    #[allow(clippy::result_large_err)]
    async fn serve(&self, stream: TcpStream) -> Result<(), String> {
        let _ = stream.set_nodelay(true);
        let token = self.token.as_str();
        let mut socket =
            tokio_tungstenite::accept_hdr_async(stream, |request: &Request, response: Response| {
                if authorize(request, token) {
                    Ok(response)
                } else {
                    Err(forbidden())
                }
            })
            .await
            .map_err(|err| err.to_string())?;

        let mut frames = self.frames.subscribe();
        // Send the frame that is already waiting, if any, without waiting
        // for the camera to produce the next one.
        frames.mark_changed();
        loop {
            tokio::select! {
                changed = frames.changed() => {
                    if changed.is_err() {
                        return Ok(());
                    }
                    let frame = frames.borrow_and_update().clone();
                    let Some(frame) = frame else { continue };
                    match tokio::time::timeout(SEND_TIMEOUT, socket.send(Message::Binary(frame))).await {
                        Ok(Ok(())) => {}
                        Ok(Err(err)) => return Err(err.to_string()),
                        Err(_) => return Err("page stopped reading frames".into()),
                    }
                }
                incoming = socket.next() => match incoming {
                    None | Some(Ok(Message::Close(_))) => return Ok(()),
                    Some(Err(err)) => return Err(err.to_string()),
                    Some(Ok(_)) => {}
                },
            }
        }
    }
}

/// Accepts the handshake only for the right path, the right token and one of
/// the app's own origins.
fn authorize(request: &Request, token: &str) -> bool {
    let uri = request.uri();
    let token_ok = uri.path() == PATH
        && uri
            .query()
            .map(|query| {
                query.split('&').any(|pair| {
                    pair.strip_prefix("token=")
                        .is_some_and(|value| constant_time_eq(value.as_bytes(), token.as_bytes()))
                })
            })
            .unwrap_or(false);
    let origin_ok = request
        .headers()
        .get("origin")
        .and_then(|value| value.to_str().ok())
        .is_some_and(|origin| ALLOWED_ORIGINS.contains(&origin));
    token_ok && origin_ok
}

fn forbidden() -> ErrorResponse {
    let mut response = ErrorResponse::new(None);
    *response.status_mut() = StatusCode::FORBIDDEN;
    response
}

fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

fn random_token() -> String {
    use rand::RngCore;
    let mut bytes = [0u8; 32];
    rand::rngs::OsRng.fill_bytes(&mut bytes);
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio_tungstenite::tungstenite::client::IntoClientRequest;
    use tokio_tungstenite::tungstenite::http::HeaderValue;

    fn frame(width: u32, height: u32, value: u8) -> PreviewFrame {
        PreviewFrame {
            width,
            height,
            rgba: vec![value; (width * height * 4) as usize],
        }
    }

    async fn start() -> Arc<CameraPreviewServer> {
        let (server, accept) = CameraPreviewServer::bind().expect("bind");
        tokio::spawn(accept);
        server
    }

    async fn connect(
        url: &str,
        origin: Option<&str>,
    ) -> Result<tokio_tungstenite::WebSocketStream<TcpStream>, tokio_tungstenite::tungstenite::Error>
    {
        let mut request = url.into_client_request().expect("request");
        if let Some(origin) = origin {
            request
                .headers_mut()
                .insert("origin", HeaderValue::from_str(origin).expect("origin"));
        }
        let host = request.uri().host().expect("host").to_string();
        let port = request.uri().port_u16().expect("port");
        let stream = TcpStream::connect((host.as_str(), port))
            .await
            .expect("tcp");
        tokio_tungstenite::client_async(request, stream)
            .await
            .map(|(socket, _)| socket)
    }

    async fn next_frame(socket: &mut tokio_tungstenite::WebSocketStream<TcpStream>) -> Bytes {
        loop {
            let message = tokio::time::timeout(Duration::from_secs(5), socket.next())
                .await
                .expect("a frame within 5 s")
                .expect("socket open")
                .expect("message");
            if let Message::Binary(data) = message {
                return data;
            }
        }
    }

    fn trailer(data: &[u8]) -> [u32; 4] {
        let tail = &data[data.len() - TRAILER_LEN..];
        let word = |i: usize| u32::from_le_bytes(tail[i * 4..i * 4 + 4].try_into().unwrap());
        [word(0), word(1), word(2), word(3)]
    }

    #[test]
    fn packs_pixels_then_trailer_without_copying_the_pixels() {
        // Producers allocate the trailer's room up front, as `frame_rgba`
        // in `native_webcam.rs` does.
        let mut rgba = Vec::with_capacity(4 * 3 * 4 + TRAILER_LEN);
        rgba.resize(4 * 3 * 4, 7u8);
        let pointer = rgba.as_ptr();
        let packed = pack(
            PreviewFrame {
                width: 4,
                height: 3,
                rgba,
            },
            9,
        )
        .expect("packed");
        assert_eq!(packed.len(), 4 * 3 * 4 + TRAILER_LEN);
        assert!(packed[..48].iter().all(|byte| *byte == 7));
        assert_eq!(trailer(&packed), [4, 3, 9, FORMAT_RGBA]);
        // The trailer fits in the reserved capacity, so the pixel buffer is
        // the allocation that is sent.
        assert_eq!(packed.as_ptr(), pointer);
    }

    #[test]
    fn refuses_a_frame_whose_size_does_not_match() {
        assert!(pack(frame(0, 2, 0), 0).is_none());
        assert!(pack(
            PreviewFrame {
                width: 4,
                height: 4,
                rgba: vec![0; 10]
            },
            0
        )
        .is_none());
    }

    #[test]
    fn tokens_are_random_and_compared_exactly() {
        let a = random_token();
        assert_eq!(a.len(), 64);
        assert_ne!(a, random_token());
        assert!(constant_time_eq(a.as_bytes(), a.as_bytes()));
        assert!(!constant_time_eq(a.as_bytes(), &a.as_bytes()[1..]));
        let mut flipped = a.clone().into_bytes();
        flipped[10] ^= 1;
        assert!(!constant_time_eq(a.as_bytes(), &flipped));
    }

    #[tokio::test]
    async fn binds_only_loopback() {
        let server = start().await;
        assert!(server
            .url()
            .starts_with(&format!("ws://127.0.0.1:{}/camera?token=", server.port())));
    }

    #[tokio::test]
    async fn delivers_frames_to_the_app_webview() {
        let server = start().await;
        let mut socket = connect(&server.url(), Some("tauri://localhost"))
            .await
            .expect("accepted");
        assert!(server.publish(frame(2, 2, 42)));
        let data = next_frame(&mut socket).await;
        assert_eq!(data.len(), 16 + TRAILER_LEN);
        assert!(data[..16].iter().all(|byte| *byte == 42));
        assert_eq!(trailer(&data), [2, 2, 0, FORMAT_RGBA]);
    }

    #[tokio::test]
    async fn a_page_that_connects_late_gets_the_waiting_frame_at_once() {
        let server = start().await;
        server.publish(frame(1, 1, 5));
        let mut socket = connect(&server.url(), Some("tauri://localhost"))
            .await
            .expect("accepted");
        assert_eq!(next_frame(&mut socket).await[0], 5);
    }

    #[tokio::test]
    async fn only_the_newest_frame_is_ever_waiting() {
        let server = start().await;
        let mut socket = connect(&server.url(), Some("tauri://localhost"))
            .await
            .expect("accepted");
        // A burst while the page is not reading: frames 0..=49 replace each
        // other, so the page sees the last one next, never a backlog.
        for value in 0..50u8 {
            server.publish(frame(1, 1, value));
        }
        let mut last = next_frame(&mut socket).await;
        // At most the frame that was already in flight can precede it.
        if last[0] != 49 {
            last = next_frame(&mut socket).await;
        }
        assert_eq!(last[0], 49);
        assert_eq!(trailer(&last)[2], 49);
    }

    #[tokio::test]
    async fn clearing_forgets_the_previous_face() {
        let server = start().await;
        server.publish(frame(1, 1, 5));
        server.clear();
        let mut socket = connect(&server.url(), Some("tauri://localhost"))
            .await
            .expect("accepted");
        let nothing = tokio::time::timeout(Duration::from_millis(200), socket.next()).await;
        assert!(nothing.is_err(), "a cleared frame must not be sent");
        server.publish(frame(1, 1, 6));
        assert_eq!(next_frame(&mut socket).await[0], 6);
    }

    #[tokio::test]
    async fn rejects_a_wrong_or_missing_token() {
        let server = start().await;
        let base = format!("ws://127.0.0.1:{}{}", server.port(), PATH);
        for url in [
            base.clone(),
            format!("{base}?token="),
            format!("{base}?token=0000"),
            format!("{base}?other=1"),
            server.url().replace(PATH, "/elsewhere"),
        ] {
            assert!(
                connect(&url, Some("tauri://localhost")).await.is_err(),
                "{url} must be refused"
            );
        }
    }

    #[tokio::test]
    async fn rejects_pages_from_other_origins() {
        let server = start().await;
        for origin in [
            None,
            Some("https://evil.example"),
            Some("http://localhost:3000"),
            Some("null"),
            Some("tauri://localhost.evil.example"),
        ] {
            assert!(
                connect(&server.url(), origin).await.is_err(),
                "{origin:?} must be refused"
            );
        }
    }
}
