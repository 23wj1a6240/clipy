//! A stand-in OpenAI-compatible endpoint for the tests: plain HTTP/1.1 on
//! 127.0.0.1, one request per connection, scripted answers, every request
//! recorded.

#![allow(dead_code)]

use std::sync::{Arc, Mutex};
use std::time::Duration;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

#[derive(Clone, Debug, Default)]
pub struct Seen {
    pub method: String,
    pub path: String,
    /// Header names lowercased.
    pub headers: Vec<(String, String)>,
    pub body: String,
}

impl Seen {
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(k, _)| k == name)
            .map(|(_, v)| v.as_str())
    }
}

/// What the server does for one request.
#[derive(Clone, Debug)]
pub struct Reply {
    pub status: u16,
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
    /// Wait this long before answering at all.
    pub delay: Duration,
    /// Send the headers and this much of the body, then stall this long.
    pub stall_after_headers: Option<Duration>,
    /// Declare a content length without sending that much body (closes after `body`).
    pub declared_length: Option<usize>,
    /// Send no Content-Length: the body ends when the connection closes.
    pub no_length: bool,
}

impl Reply {
    pub fn json(status: u16, body: impl Into<Vec<u8>>) -> Self {
        Reply {
            status,
            headers: vec![("Content-Type".into(), "application/json".into())],
            body: body.into(),
            delay: Duration::ZERO,
            stall_after_headers: None,
            declared_length: None,
            no_length: false,
        }
    }

    /// A chat completion whose message content is `content`.
    pub fn completion(content: &str) -> Self {
        let body = serde_json::json!({"choices": [{"message": {"content": content}}]}).to_string();
        Reply::json(200, body)
    }
}

pub struct FakeProvider {
    pub base_url: String,
    pub seen: Arc<Mutex<Vec<Seen>>>,
}

impl FakeProvider {
    pub fn requests(&self) -> Vec<Seen> {
        self.seen.lock().unwrap().clone()
    }
}

/// Start a server answering request N (0-based) with `script(N)`.
pub async fn serve(script: impl Fn(usize) -> Reply + Send + Sync + 'static) -> FakeProvider {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let seen: Arc<Mutex<Vec<Seen>>> = Arc::default();
    let script = Arc::new(script);
    let recorded = Arc::clone(&seen);
    tokio::spawn(async move {
        loop {
            let Ok((mut socket, _)) = listener.accept().await else {
                return;
            };
            let recorded = Arc::clone(&recorded);
            let script = Arc::clone(&script);
            tokio::spawn(async move {
                let mut buf = Vec::new();
                let mut chunk = [0u8; 8192];
                let header_end = loop {
                    let Ok(n) = socket.read(&mut chunk).await else {
                        return;
                    };
                    if n == 0 {
                        return;
                    }
                    buf.extend_from_slice(&chunk[..n]);
                    if let Some(i) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
                        break i + 4;
                    }
                };
                let head = String::from_utf8_lossy(&buf[..header_end]).to_string();
                let mut lines = head.split("\r\n");
                let request_line = lines.next().unwrap_or_default().to_string();
                let mut parts = request_line.split(' ');
                let method = parts.next().unwrap_or_default().to_string();
                let path = parts.next().unwrap_or_default().to_string();
                let headers: Vec<(String, String)> = lines
                    .filter_map(|l| l.split_once(':'))
                    .map(|(k, v)| (k.trim().to_ascii_lowercase(), v.trim().to_string()))
                    .collect();
                let length: usize = headers
                    .iter()
                    .find(|(k, _)| k == "content-length")
                    .and_then(|(_, v)| v.parse().ok())
                    .unwrap_or(0);
                while buf.len() < header_end + length {
                    let Ok(n) = socket.read(&mut chunk).await else {
                        return;
                    };
                    if n == 0 {
                        break;
                    }
                    buf.extend_from_slice(&chunk[..n]);
                }
                let body =
                    String::from_utf8_lossy(&buf[header_end..(header_end + length).min(buf.len())])
                        .to_string();
                let index = {
                    let mut all = recorded.lock().unwrap();
                    all.push(Seen {
                        method,
                        path,
                        headers,
                        body,
                    });
                    all.len() - 1
                };
                let reply = script(index);
                tokio::time::sleep(reply.delay).await;
                let reason = match reply.status {
                    200 => "OK",
                    _ => "Status",
                };
                let mut out = format!("HTTP/1.1 {} {reason}\r\n", reply.status);
                for (k, v) in &reply.headers {
                    out.push_str(&format!("{k}: {v}\r\n"));
                }
                if !reply.no_length {
                    out.push_str(&format!(
                        "Content-Length: {}\r\n",
                        reply.declared_length.unwrap_or(reply.body.len())
                    ));
                }
                out.push_str("Connection: close\r\n\r\n");
                let _ = socket.write_all(out.as_bytes()).await;
                if let Some(stall) = reply.stall_after_headers {
                    let _ = socket.flush().await;
                    tokio::time::sleep(stall).await;
                }
                let _ = socket.write_all(&reply.body).await;
                let _ = socket.flush().await;
                let _ = socket.shutdown().await;
            });
        }
    });
    FakeProvider {
        base_url: format!("http://127.0.0.1:{port}/v1"),
        seen,
    }
}
