//! A tiny local HTTP server for tests: records every request and answers
//! with whatever the handler returns.

use std::io::{Cursor, Read};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

#[derive(Clone, Debug)]
pub struct Recorded {
    pub method: String,
    pub url: String,
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
}

impl Recorded {
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.as_str())
    }
    pub fn body_text(&self) -> String {
        String::from_utf8_lossy(&self.body).to_string()
    }
}

pub enum Body {
    Sized(Vec<u8>),
    Chunked(Vec<u8>),
    Slow(Vec<u8>),
}

pub struct Reply {
    pub status: u16,
    pub content_type: Option<String>,
    pub extra_headers: Vec<(String, String)>,
    pub body: Body,
}

impl Reply {
    pub fn bytes(status: u16, body: Vec<u8>) -> Self {
        Reply {
            status,
            content_type: None,
            extra_headers: Vec::new(),
            body: Body::Sized(body),
        }
    }
    pub fn chunked(status: u16, body: Vec<u8>) -> Self {
        Reply {
            status,
            content_type: None,
            extra_headers: Vec::new(),
            body: Body::Chunked(body),
        }
    }
    /// Streams the body a few KB at a time with pauses, so a test can cancel mid-way.
    pub fn slow(status: u16, body: Vec<u8>) -> Self {
        Reply {
            status,
            content_type: None,
            extra_headers: Vec::new(),
            body: Body::Slow(body),
        }
    }
    pub fn json(status: u16, v: serde_json::Value) -> Self {
        Reply {
            status,
            content_type: Some("application/json".into()),
            extra_headers: Vec::new(),
            body: Body::Sized(serde_json::to_vec(&v).unwrap()),
        }
    }
    pub fn with_header(mut self, k: &str, v: &str) -> Self {
        self.extra_headers.push((k.into(), v.into()));
        self
    }
}

struct SlowReader {
    data: Cursor<Vec<u8>>,
}

impl Read for SlowReader {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        thread::sleep(Duration::from_millis(5));
        let n = buf.len().min(4096);
        self.data.read(&mut buf[..n])
    }
}

pub struct TestServer {
    server: Arc<tiny_http::Server>,
    requests: Arc<Mutex<Vec<Recorded>>>,
    port: u16,
}

impl TestServer {
    pub fn start<F>(handler: F) -> Self
    where
        F: Fn(&Recorded) -> Reply + Send + Sync + 'static,
    {
        let server = Arc::new(tiny_http::Server::http("127.0.0.1:0").unwrap());
        let port = server.server_addr().to_ip().unwrap().port();
        let requests = Arc::new(Mutex::new(Vec::new()));
        let (s, r) = (server.clone(), requests.clone());
        thread::spawn(move || {
            for mut req in s.incoming_requests() {
                let mut body = Vec::new();
                let _ = req.as_reader().read_to_end(&mut body);
                let rec = Recorded {
                    method: req.method().to_string(),
                    url: req.url().to_string(),
                    headers: req
                        .headers()
                        .iter()
                        .map(|h| (h.field.to_string(), h.value.to_string()))
                        .collect(),
                    body,
                };
                r.lock().unwrap().push(rec.clone());
                let reply = handler(&rec);
                let mut headers: Vec<tiny_http::Header> = reply
                    .extra_headers
                    .iter()
                    .map(|(k, v)| {
                        tiny_http::Header::from_bytes(k.as_bytes(), v.as_bytes()).unwrap()
                    })
                    .collect();
                if let Some(ct) = &reply.content_type {
                    headers.push(
                        tiny_http::Header::from_bytes(&b"Content-Type"[..], ct.as_bytes()).unwrap(),
                    );
                }
                let status = tiny_http::StatusCode(reply.status);
                let _ = match reply.body {
                    Body::Sized(b) => {
                        let len = b.len();
                        req.respond(tiny_http::Response::new(
                            status,
                            headers,
                            Cursor::new(b),
                            Some(len),
                            None,
                        ))
                    }
                    Body::Chunked(b) => req.respond(tiny_http::Response::new(
                        status,
                        headers,
                        Cursor::new(b),
                        None,
                        None,
                    )),
                    Body::Slow(b) => req.respond(tiny_http::Response::new(
                        status,
                        headers,
                        SlowReader {
                            data: Cursor::new(b),
                        },
                        None,
                        None,
                    )),
                };
            }
        });
        TestServer {
            server,
            requests,
            port,
        }
    }

    pub fn url(&self, path: &str) -> String {
        format!("http://127.0.0.1:{}{}", self.port, path)
    }

    pub fn base(&self) -> String {
        format!("http://127.0.0.1:{}", self.port)
    }

    pub fn requests(&self) -> Vec<Recorded> {
        self.requests.lock().unwrap().clone()
    }
}

impl Drop for TestServer {
    fn drop(&mut self) {
        self.server.unblock();
    }
}
