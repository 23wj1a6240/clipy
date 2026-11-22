//! Each provider against a local fake server: request shape, answer parsing,
//! and errors that never carry a key or a body.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use serde_json::json;

use super::http::{client, request_json_capped};
use super::*;
use crate::error::SttError;
use crate::test_server::{Reply, TestServer};

const KEY: &str = "sk-test-SECRET";

/// Loud "speech" with silent stretches at the given seconds.
fn audio(total_sec: f64, quiet: &[(f64, f64)]) -> Vec<f32> {
    let n = (total_sec * SAMPLE_RATE as f64) as usize;
    (0..n)
        .map(|i| {
            let t = i as f64 / SAMPLE_RATE as f64;
            if quiet.iter().any(|(a, b)| t >= *a && t < *b) {
                0.0
            } else if i % 2 == 0 {
                0.4
            } else {
                -0.4
            }
        })
        .collect()
}

fn assert_no_secret(e: &SttError) {
    let s = e.to_string();
    assert!(!s.contains(KEY) && !s.contains("BODY"), "{s}");
}

#[test]
fn openai_compatible_sends_verbose_json_form_with_bearer_key() {
    let server = TestServer::start(|_| {
        Reply::json(
            200,
            json!({ "text": "hello there", "language": "english", "segments": [{ "id": 0, "start": 0.2, "end": 1.4, "text": " hello there" }] }),
        )
    });
    let pcm = audio(3.0, &[]);
    let cancel = CancelToken::new();
    let config = ProviderConfig::OpenAiCompatible {
        base_url: format!("{}/v1/", server.base()),
        api_key: KEY.into(),
        model: "whisper-1".into(),
    };
    let t = transcribe_with_provider(&pcm, &config, &cancel).unwrap();
    assert_eq!(t.engine, "openai-compatible:whisper-1");
    assert_eq!(t.language.as_deref(), Some("english"));
    assert_eq!(
        t.segments,
        vec![Segment {
            start_ms: 200,
            end_ms: 1400,
            text: "hello there".into()
        }]
    );
    let reqs = server.requests();
    assert_eq!(reqs.len(), 1);
    let r = &reqs[0];
    assert_eq!(
        (r.method.as_str(), r.url.as_str()),
        ("POST", "/v1/audio/transcriptions")
    );
    assert_eq!(
        r.header("authorization"),
        Some(format!("Bearer {KEY}").as_str())
    );
    assert!(r
        .header("content-type")
        .unwrap()
        .starts_with("multipart/form-data"));
    let body = r.body_text();
    for needle in [
        "name=\"model\"",
        "whisper-1",
        "verbose_json",
        "timestamp_granularities[]",
        "filename=\"audio.wav\"",
        "RIFF",
    ] {
        assert!(body.contains(needle), "missing {needle}");
    }
}

#[test]
fn openai_compatible_splits_large_audio_and_merges_offsets() {
    let server = TestServer::start(|_| {
        Reply::json(
            200,
            json!({ "segments": [{ "start": 0.5, "end": 1.5, "text": "piece" }] }),
        )
    });
    let pcm = audio(25.0, &[(9.0, 10.5), (19.0, 20.5)]);
    let cancel = CancelToken::new();
    let input = AudioInput::new(&pcm, &cancel);
    let max = 44 + 32_000 * 12;
    let t = openai::transcribe(&input, &server.base(), "", "m", Some(max)).unwrap();
    let starts: Vec<u64> = t.segments.iter().map(|s| s.start_ms).collect();
    assert_eq!(starts, vec![500, 10_250, 20_250]);
    let reqs = server.requests();
    assert_eq!(reqs.len(), 3);
    assert!(
        reqs.iter().all(|r| r.header("authorization").is_none()),
        "no key, no header"
    );
    assert!(reqs.iter().all(|r| r.body.len() < max as usize + 2048));
}

#[test]
fn plain_text_answer_becomes_one_segment() {
    let server = TestServer::start(|_| Reply::json(200, json!({ "text": " just text " })));
    let pcm = audio(4.0, &[]);
    let t = transcribe_with_provider(
        &pcm,
        &ProviderConfig::OpenAiCompatible {
            base_url: server.base(),
            api_key: KEY.into(),
            model: "gpt-4o-transcribe".into(),
        },
        &CancelToken::new(),
    )
    .unwrap();
    assert_eq!(
        t.segments,
        vec![Segment {
            start_ms: 0,
            end_ms: 4000,
            text: "just text".into()
        }]
    );
}

#[test]
fn http_errors_name_provider_and_status_only() {
    let server = TestServer::start(|r| {
        if r.url.contains("listen") {
            Reply::bytes(401, b"BODY with transcript".to_vec())
        } else {
            Reply::bytes(500, b"BODY".to_vec())
        }
    });
    let pcm = audio(1.0, &[]);
    let cancel = CancelToken::new();
    let e = transcribe_with_provider(
        &pcm,
        &ProviderConfig::Deepgram {
            api_key: KEY.into(),
            model: None,
            base_url: Some(server.base()),
        },
        &cancel,
    )
    .unwrap_err();
    assert_eq!(
        e.to_string(),
        "deepgram transcription failed (HTTP 401): check the API key"
    );
    assert_no_secret(&e);
    match ProviderConfig::groq(KEY) {
        ProviderConfig::OpenAiCompatible {
            base_url, model, ..
        } => {
            assert_eq!(base_url, "https://api.groq.com/openai/v1");
            assert_eq!(model, "whisper-large-v3-turbo");
        }
        _ => unreachable!(),
    }
    let e = transcribe_with_provider(
        &pcm,
        &ProviderConfig::Gemini {
            api_key: KEY.into(),
            model: None,
            base_url: Some(server.base()),
        },
        &cancel,
    )
    .unwrap_err();
    assert_eq!(e.to_string(), "gemini transcription failed (HTTP 500)");
    assert!(!format!("{:?}", ProviderConfig::groq(KEY)).contains(KEY));
}

#[test]
fn redirects_are_refused_and_big_answers_cut_off() {
    let server = TestServer::start(|r| {
        if r.url == "/redirect" {
            Reply::bytes(302, Vec::new()).with_header("Location", "http://127.0.0.1:1/steal")
        } else {
            Reply::chunked(200, vec![b' '; 300_000])
        }
    });
    let cancel = CancelToken::new();
    let http = client().unwrap();
    let e = request_json_capped(
        "x",
        http.get(server.url("/redirect")).header("xi-api-key", KEY),
        &cancel,
        1000,
    )
    .unwrap_err();
    assert_eq!(e.to_string(), "x transcription failed (HTTP 302)");
    assert_eq!(server.requests().len(), 1);
    let e = request_json_capped("x", http.get(server.url("/big")), &cancel, 100_000).unwrap_err();
    assert!(e.to_string().contains("larger than"), "{e}");
    let e = request_json_capped("x", http.get("http://127.0.0.1:1/"), &cancel, 100).unwrap_err();
    assert_eq!(
        e.to_string(),
        "x transcription failed: the provider could not be reached"
    );
}

#[test]
fn deepgram_posts_wav_and_reads_utterances_or_words() {
    let use_words = Arc::new(AtomicUsize::new(0));
    let flag = use_words.clone();
    let server = TestServer::start(move |_| {
        if flag.load(Ordering::SeqCst) == 0 {
            Reply::json(
                200,
                json!({ "results": {
                    "utterances": [{ "start": 0.1, "end": 0.9, "transcript": "first one" }, { "start": 1.0, "end": 1.8, "transcript": "second" }],
                    "channels": [{ "detected_language": "en", "alternatives": [{ "transcript": "x" }] }]
                }}),
            )
        } else {
            Reply::json(
                200,
                json!({ "results": { "channels": [{ "alternatives": [{ "words": [
                    { "start": 0.0, "end": 0.4, "word": "hello", "punctuated_word": "Hello" },
                    { "start": 2.0, "end": 2.4, "word": "again" }
                ]}]}]}}),
            )
        }
    });
    let pcm = audio(2.0, &[]);
    let config = ProviderConfig::Deepgram {
        api_key: KEY.into(),
        model: None,
        base_url: Some(server.base()),
    };
    let t = transcribe_with_provider(&pcm, &config, &CancelToken::new()).unwrap();
    assert_eq!(t.engine, "deepgram:nova-3");
    assert_eq!(t.language.as_deref(), Some("en"));
    assert_eq!(t.segments.len(), 2);
    let r = &server.requests()[0];
    assert!(r.url.starts_with("/v1/listen?model=nova-3&smart_format=true&punctuate=true&utterances=true&detect_language=true"), "{}", r.url);
    assert_eq!(
        r.header("authorization"),
        Some(format!("Token {KEY}").as_str())
    );
    assert_eq!(r.header("content-type"), Some("audio/wav"));
    assert_eq!(r.body.len(), 44 + 2 * 32_000);
    assert_eq!(&r.body[..4], b"RIFF");

    use_words.store(1, Ordering::SeqCst);
    let t = transcribe_with_provider(&pcm, &config, &CancelToken::new()).unwrap();
    let texts: Vec<&str> = t.segments.iter().map(|s| s.text.as_str()).collect();
    assert_eq!(texts, vec!["Hello", "again"]);
}

#[test]
fn assemblyai_uploads_submits_and_polls() {
    let polls = Arc::new(AtomicUsize::new(0));
    let p = polls.clone();
    let server = TestServer::start(move |r| match (r.method.as_str(), r.url.as_str()) {
        ("POST", "/v2/upload") => {
            Reply::json(200, json!({ "upload_url": "https://cdn.example/u1" }))
        }
        ("POST", "/v2/transcript") => {
            Reply::json(200, json!({ "id": "job 1", "status": "queued" }))
        }
        ("GET", "/v2/transcript/job%201") => {
            if p.fetch_add(1, Ordering::SeqCst) == 0 {
                Reply::json(200, json!({ "id": "job 1", "status": "processing" }))
            } else {
                Reply::json(
                    200,
                    json!({ "id": "job 1", "status": "completed", "language_code": "de",
                    "utterances": [{ "text": "Guten Tag", "start": 100, "end": 900 }], "words": null }),
                )
            }
        }
        _ => Reply::bytes(404, Vec::new()),
    });
    let pcm = audio(1.0, &[]);
    let cancel = CancelToken::new();
    let input = AudioInput::new(&pcm, &cancel);
    let limits = assemblyai::Limits {
        poll_interval: Duration::from_millis(10),
        timeout: Duration::from_secs(10),
    };
    let t = assemblyai::transcribe(
        &input,
        KEY,
        Some("universal"),
        Some(&server.base()),
        &limits,
    )
    .unwrap();
    assert_eq!(
        t.segments,
        vec![Segment {
            start_ms: 100,
            end_ms: 900,
            text: "Guten Tag".into()
        }]
    );
    assert_eq!(t.language.as_deref(), Some("de"));
    assert_eq!(t.engine, "assemblyai:universal");
    let reqs = server.requests();
    assert_eq!(reqs.len(), 4);
    assert!(reqs.iter().all(|r| r.header("authorization") == Some(KEY)));
    assert_eq!(
        reqs[0].header("content-type"),
        Some("application/octet-stream")
    );
    let submit: serde_json::Value = serde_json::from_slice(&reqs[1].body).unwrap();
    assert_eq!(
        submit,
        json!({ "audio_url": "https://cdn.example/u1", "language_detection": true, "speech_models": ["universal"] })
    );
}

#[test]
fn assemblyai_reports_a_failed_job() {
    let server = TestServer::start(|r| match r.url.as_str() {
        "/v2/upload" => Reply::json(200, json!({ "upload_url": "u" })),
        _ => Reply::json(
            200,
            json!({ "id": "j", "status": "error", "error": "BODY detail" }),
        ),
    });
    let pcm = audio(1.0, &[]);
    let cancel = CancelToken::new();
    let input = AudioInput::new(&pcm, &cancel);
    let e = assemblyai::transcribe(
        &input,
        KEY,
        None,
        Some(&server.base()),
        &assemblyai::Limits::default(),
    )
    .unwrap_err();
    assert_eq!(
        e.to_string(),
        "assemblyai transcription failed: the provider reported an error for this audio"
    );
    assert_no_secret(&e);
}

#[test]
fn elevenlabs_sends_scribe_form_and_keeps_words_only() {
    let server = TestServer::start(|_| {
        Reply::json(
            200,
            json!({ "language_code": "fr", "text": "Bonjour à tous", "words": [
                { "text": "Bonjour", "start": 0.0, "end": 0.5, "type": "word" },
                { "text": " ", "start": 0.5, "end": 0.6, "type": "spacing" },
                { "text": "à", "start": 0.6, "end": 0.7 },
                { "text": "tous.", "start": 0.7, "end": 1.0, "type": "word" },
                { "text": "(rires)", "start": null, "end": null, "type": "word" }
            ]}),
        )
    });
    let pcm = audio(1.0, &[]);
    let config = ProviderConfig::ElevenLabs {
        api_key: KEY.into(),
        model: None,
        base_url: Some(server.base()),
    };
    let t = transcribe_with_provider(&pcm, &config, &CancelToken::new()).unwrap();
    assert_eq!(t.engine, "elevenlabs:scribe_v2");
    assert_eq!(
        t.segments,
        vec![Segment {
            start_ms: 0,
            end_ms: 1000,
            text: "Bonjour à tous.".into()
        }]
    );
    let r = &server.requests()[0];
    assert_eq!(r.url, "/v1/speech-to-text");
    assert_eq!(r.header("xi-api-key"), Some(KEY));
    let body = r.body_text();
    for needle in [
        "name=\"model_id\"",
        "scribe_v2",
        "timestamps_granularity",
        "tag_audio_events",
        "filename=\"audio.wav\"",
    ] {
        assert!(body.contains(needle), "missing {needle}");
    }
}

#[test]
fn gemini_sends_inline_audio_and_validates_the_answer() {
    let bad = Arc::new(AtomicUsize::new(0));
    let flag = bad.clone();
    let server = TestServer::start(move |_| {
        let text = if flag.load(Ordering::SeqCst) == 0 {
            "```json\n{\"language\":\"es\",\"segments\":[{\"start\":\"0:01\",\"end\":2.5,\"text\":\"Hola\"},{\"start\":99,\"end\":100,\"text\":\"ghost\"}]}\n```"
        } else {
            "{\"segments\":[{\"start\":1}]}"
        };
        Reply::json(
            200,
            json!({ "candidates": [{ "content": { "parts": [{ "text": text }] } }] }),
        )
    });
    let pcm = audio(3.0, &[]);
    let config = ProviderConfig::Gemini {
        api_key: KEY.into(),
        model: None,
        base_url: Some(server.base()),
    };
    let t = transcribe_with_provider(&pcm, &config, &CancelToken::new()).unwrap();
    assert_eq!(t.engine, "gemini:gemini-3.5-flash-lite");
    assert_eq!(t.language.as_deref(), Some("es"));
    assert_eq!(
        t.segments,
        vec![Segment {
            start_ms: 1000,
            end_ms: 2500,
            text: "Hola".into()
        }]
    );
    let r = &server.requests()[0];
    assert_eq!(r.url, "/models/gemini-3.5-flash-lite:generateContent");
    assert_eq!(r.header("x-goog-api-key"), Some(KEY));
    let body: serde_json::Value = serde_json::from_slice(&r.body).unwrap();
    assert_eq!(
        body["contents"][0]["parts"][0]["inline_data"]["mime_type"],
        "audio/wav"
    );
    assert_eq!(
        body["generationConfig"]["responseMimeType"],
        "application/json"
    );

    bad.store(1, Ordering::SeqCst);
    let e = transcribe_with_provider(&pcm, &config, &CancelToken::new()).unwrap_err();
    assert_eq!(
        e.to_string(),
        "gemini transcription failed (HTTP 200): the answer did not match the transcript format"
    );
}

#[test]
fn none_is_one_gap_and_cancel_stops_before_sending() {
    let pcm = audio(2.5, &[]);
    let t = transcribe_with_provider(&pcm, &ProviderConfig::None, &CancelToken::new()).unwrap();
    assert_eq!(t.engine, "none");
    assert_eq!(
        t.gaps,
        vec![Gap {
            start_ms: 0,
            end_ms: 2500,
            reason: "transcription off".into()
        }]
    );

    let server = TestServer::start(|_| Reply::json(200, json!({})));
    let cancel = CancelToken::new();
    cancel.cancel();
    let e = transcribe_with_provider(
        &pcm,
        &ProviderConfig::Deepgram {
            api_key: KEY.into(),
            model: None,
            base_url: Some(server.base()),
        },
        &cancel,
    )
    .unwrap_err();
    assert!(matches!(e, SttError::Cancelled));
    assert!(server.requests().is_empty());
}

#[test]
fn cancel_while_waiting_for_a_slow_provider_returns_at_once() {
    let server = TestServer::start(|_| {
        std::thread::sleep(Duration::from_secs(3));
        Reply::json(200, json!({ "results": {} }))
    });
    let pcm = audio(1.0, &[]);
    let cancel = CancelToken::new();
    let c = cancel.clone();
    std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(200));
        c.cancel();
    });
    let started = std::time::Instant::now();
    let e = transcribe_with_provider(
        &pcm,
        &ProviderConfig::Deepgram {
            api_key: KEY.into(),
            model: None,
            base_url: Some(server.base()),
        },
        &cancel,
    )
    .unwrap_err();
    assert!(matches!(e, SttError::Cancelled));
    assert!(started.elapsed() < Duration::from_millis(1500));
}
