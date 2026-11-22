//! Transcribe a 16 kHz mono WAV with an installed model, for checking a
//! build by hand on a new platform:
//!
//! `cargo run --release --example transcribe -- speech.wav [english-fast|multilingual]`

use std::path::PathBuf;
use std::time::Instant;

use clipy_local_stt::{model_status, read_wav, transcribe_local, CancelToken, LocalModelId};

fn main() {
    let mut args = std::env::args().skip(1);
    let Some(path) = args.next().map(PathBuf::from) else {
        eprintln!("usage: transcribe <16 kHz mono wav> [english-fast|multilingual]");
        std::process::exit(2);
    };
    let model = args
        .next()
        .and_then(|m| LocalModelId::parse(&m))
        .unwrap_or(LocalModelId::EnglishFast);
    let status = model_status(model, None);
    if !status.installed {
        eprintln!(
            "{} is not installed in {}",
            status.label,
            status.dir.display()
        );
        std::process::exit(1);
    }
    let pcm = match read_wav(&path) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("{e}");
            std::process::exit(1);
        }
    };
    let started = Instant::now();
    match transcribe_local(&pcm, model, None, &mut |_| {}, &CancelToken::new()) {
        Ok(t) => {
            eprintln!(
                "{:.1} s of audio in {:.2} s",
                pcm.len() as f64 / 16_000.0,
                started.elapsed().as_secs_f64()
            );
            println!("{}", serde_json::to_string_pretty(&t).unwrap_or_default());
        }
        Err(e) => {
            eprintln!("{e}");
            std::process::exit(1);
        }
    }
}
