# clipy-local-stt

Timestamped transcripts from 16 kHz mono audio for the local edition of the
Clipy desktop app, Clipy Local. It is how Clipy Local transcribes on Windows,
where the OS has no on-device speech engine, and it can serve macOS as an
option.

It is the Rust twin of `packages/transcribe` (`@clipy/transcribe`):

- the same two models with the same pinned SHA-256 values, the same model
  folder and install marker (`.installed.json`), so a model downloaded by the
  CLI is reused by the app and the other way round;
- the same chunk planner and token-level merge (`src/chunking.rs` is a
  line-for-line port of `chunking.ts`; keep them in step);
- the same request shapes and limits for the hosted providers.

The crate is standalone (its own `[workspace]`) and builds and tests on Linux
with plain `cargo test`. The app is expected to use it as a path dependency.

## Public API

Every call is blocking. Call it from a plain thread or `spawn_blocking`, never
from inside an async task (the HTTP client is `reqwest::blocking`).

```rust
pub fn model_status(model: LocalModelId, dir: Option<&Path>) -> ModelStatus;
pub fn download_model(
    model: LocalModelId,
    dir: Option<&Path>,
    progress: &mut dyn FnMut(f64),   // 0..=1
    cancel: &CancelToken,
) -> Result<PathBuf>;
pub fn transcribe_local(
    pcm: &[f32],                     // 16 kHz mono, -1..1
    model: LocalModelId,
    dir: Option<&Path>,
    progress: &mut dyn FnMut(f64),
    cancel: &CancelToken,
) -> Result<Transcript>;
pub fn transcribe_with_provider(pcm: &[f32], config: &ProviderConfig, cancel: &CancelToken) -> Result<Transcript>;

pub struct Transcript { segments: Vec<Segment>, engine: String, language: Option<String>, gaps: Vec<Gap> }
pub struct Segment { start_ms: u64, end_ms: u64, text: String }
pub struct Gap { start_ms: u64, end_ms: u64, reason: String }

pub enum LocalModelId { EnglishFast, Multilingual }      // "english-fast", "multilingual"
pub enum ProviderConfig {                                // the TS TranscriptionConfig kinds
    None,
    Local { model: LocalModelId },
    OpenAiCompatible { base_url: String, api_key: String, model: String },
    Deepgram   { api_key: String, model: Option<String>, base_url: Option<String> },
    AssemblyAi { api_key: String, model: Option<String>, base_url: Option<String> },
    ElevenLabs { api_key: String, model: Option<String>, base_url: Option<String> },
    Gemini     { api_key: String, model: Option<String>, base_url: Option<String> },
}
ProviderConfig::openai(key)   // https://api.openai.com/v1, whisper-1
ProviderConfig::groq(key)     // https://api.groq.com/openai/v1, whisper-large-v3-turbo
```

`dir` is the models root; `None` means `default_model_dir()`:
`%LOCALAPPDATA%\clipy-local\models` on Windows,
`~/Library/Caches/clipy-local/models` on macOS,
`$XDG_CACHE_HOME` or `~/.cache/clipy-local/models` on Linux (the CLI's folder).
Each model lives in `<root>/<model id>`.

Helpers: `read_wav(path)` (16 kHz PCM WAV, 16-bit or float, channels averaged),
`read_wav_bytes`, `wav_from_pcm`, `CancelToken`, `LOCAL_ENGINE_AVAILABLE`.
`ProviderConfig`'s `Debug` never prints a key. `Transcript` serializes with
camelCase keys (`startMs`), the shape the TS package returns.

Errors (`SttError`): `ModelNotInstalled { model, label, size_bytes }` (the app
asks for consent with the label and size; nothing is downloaded automatically),
`Cancelled`, `Checksum`, `DownloadTooLarge`, `Download`, `Provider` (names the
provider and HTTP status only, never a key or a response body), `Engine`,
`InvalidInput`, `Io`.

### Example: Windows local edition

```rust
let pcm: Vec<f32> = media::extract_pcm(&video)?;   // the app's FFmpeg: 16 kHz mono f32
let cancel = CancelToken::new();
let model = LocalModelId::EnglishFast;
let transcript = match clipy_local_stt::transcribe_local(&pcm, model, Some(&models_dir), &mut |p| report(p), &cancel) {
    Err(SttError::ModelNotInstalled { label, size_bytes, .. }) => { /* ask, then download_model(...) */ }
    other => other?,
};
```

## Models

| Id | Model | Download | Pieces |
|---|---|---|---|
| `english-fast` | NVIDIA Parakeet TDT-CTC 110M, int8, English (CC-BY-4.0), from the sherpa-onnx `asr-models` release, `.tar.bz2` | 104 MB | at most 180 s, about 130 s, cut in silences, 2.5 s overlap, merged token by token |
| `multilingual` | OpenAI Whisper base, int8, about 99 languages (MIT), pinned to Hugging Face commit `bb53ee2` | 161 MB | under 28 s, no overlap, silent pieces skipped |

Download: each file streams to a staging folder `.<id>.partial-<pid>-<hex>`
while it is hashed, is cut off past its pinned size, and is checked against its
SHA-256; archive members are extracted one by one and each checked. Only when
every file verifies is the marker written and the folder renamed into place.
Cancel or any failure removes the staging folder. Leftover staging folders of
dead processes (the CLI's or the app's) are removed on the next download.
Redirects are followed for model downloads (CDNs); no credential is ever sent.

## Engine binding: `sherpa-onnx-sys` 2.0.2

Options evaluated:

| | `sherpa-rs` 0.6.8 (thewh1teagle, MIT) | `sherpa-onnx` 2.0.2 (k2-fsa, Apache-2.0) | `sherpa-onnx-sys` 2.0.2 (k2-fsa, Apache-2.0) |
|---|---|---|---|
| sherpa-onnx version | 1.12.9 | 2.0.2 | 2.0.2 (same as the CLI's `sherpa-onnx-node` ^2.0.1) |
| NeMo CTC (Parakeet) | no wrapper (`nemo_ctc` is always zeroed), would need raw sys calls | yes | yes (raw config) |
| Whisper | yes | yes | yes |
| Detected language | yes | no (the result struct drops `lang`) | yes (we read the result JSON) |
| Windows | static or shared, MD/MT choice through its own build | static MT archive or shared DLLs | same as `sherpa-onnx` |
| Linux static | needs `RUSTFLAGS=-C relocation-model=dynamic-no-pic` | works | works |

Chosen: **`sherpa-onnx-sys` 2.0.2** with a small safe wrapper (`src/engine.rs`,
about 150 lines: create recognizer, accept waveform, decode, read the result
JSON). It is k2-fsa's own crate, the same native version the CLI runs, covers
both model kinds, and gives us the Whisper language the safe crate drops. Its
build script downloads k2-fsa's prebuilt archive for the target; no C++
toolchain or system install is needed on Windows x64 MSVC or macOS arm64.

Features: `default = ["local", "static"]`. `local` turns the engine on (without
it the crate still downloads models and runs the hosted providers, and
`transcribe_local` returns an `Engine` error). `static` links sherpa-onnx and
onnxruntime into the binary; `shared` links their shared libraries instead.

### Size it adds (measured from the v2.0.2 archives)

| Target | Mode | Added | Compressed |
|---|---|---|---|
| Linux x64 | static | +30.9 MB to a stripped release binary (31.5 MB vs 0.6 MB without `local`) | 12.5 MB gzip |
| Windows x64 | shared (recommended) | `onnxruntime.dll` 17.0 MB + `sherpa-onnx-c-api.dll` 4.6 MB = 21.6 MB next to the exe | 8.1 MB (the archive, bz2); expect about the same in the NSIS installer |
| Windows x64 | static (MT) | not measured: the static archive alone is a 127 MB download | |
| macOS arm64 | shared | `libonnxruntime.dylib` 28.8 MB + `libsherpa-onnx-c-api.dylib` 4.9 MB | 9.0 MB (bz2) |
| macOS arm64 | static | not measured; expect about the Linux figure | |

The macOS DMG sits at about 22 to 26 MB, so this engine would roughly double
it. macOS already transcribes on-device with Apple Speech, so `local` stays off
there.

### Windows linking risk

- **Use `default-features = false, features = ["local", "shared"]` on
  Windows.** k2-fsa only publishes `MT` (static CRT) Windows builds. The static
  archive links `/MT` C++ objects into a Rust binary that is `/MD` by default;
  if anything else in the app links `/MD` C++ code with the MSVC STL, the link
  fails with `LNK2038` (RuntimeLibrary mismatch), and even without that it
  pulls both `LIBCMT` and `MSVCRT` (`LNK4098`). Switching the whole app to
  `+crt-static` would avoid it but changes every other dependency. The shared
  DLLs carry their own static CRT behind a DLL boundary, which is safe.
- **Ship the two DLLs next to `clipy-desktop.exe`**, staged like the FFmpeg
  DLLs (`scripts/stage-ffmpeg-dlls.mjs` pattern). The build script copies them
  into `target/<profile>/` for `cargo run`/`cargo test`, but not into the
  Tauri bundle. `onnxruntime_providers_shared.dll` is only needed for GPU
  providers and can be left out (CPU only here; unverified).
- **`C:\Windows\System32\onnxruntime.dll`**: Windows 11 ships its own, older
  onnxruntime. The loader searches the application folder first, so the
  bundled copy wins when it sits next to the exe; it must not be put anywhere
  else (for example a subfolder added with `SetDllDirectory` later in startup).
- **Paths**: model paths are passed to the C API as UTF-8. A profile folder
  with non-ASCII characters (`C:\Users\José\AppData\Local`) is untested on
  Windows.
- **Crash isolation**: the recognizer runs in-process on its own thread with
  `catch_unwind` around every decode, so a Rust panic or an engine error costs
  one piece (a transcript gap) and the worker is replaced. A native abort
  inside onnxruntime cannot be caught in-process (the TS package runs a child
  process for that reason). Pieces never exceed the model's limit, which is
  what caused the known Parakeet abort. If aborts show up in the field, move
  the engine into a helper process.

### Build-time download, proxies and pinning

The `sherpa-onnx-sys` build script downloads its archive from GitHub with
`ureq` and webpki roots, and does not check a checksum. Behind a TLS-inspecting
proxy the download fails with
`invalid peer certificate: UnknownIssuer`. In CI and in sandboxes, download the
archive with a trusted tool, check it, and point the build at it:

```sh
export SHERPA_ONNX_ARCHIVE_DIR=/path/to/archives   # holds sherpa-onnx-v2.0.2-<target>.tar.bz2
# or SHERPA_ONNX_LIB_DIR=/path/to/extracted/lib
```

SHA-256 of the archives used here:

| Archive | SHA-256 |
|---|---|
| `sherpa-onnx-v2.0.2-linux-x64-static-lib.tar.bz2` | `082743c80ec9d3fd739df174b9e46ab08ac77c9f57a1d2b09977a7fd5324727d` |
| `sherpa-onnx-v2.0.2-linux-x64-shared-lib.tar.bz2` | `e99e08d851148b252d141714005eb5a7ad2c49b54a738281704a3dab6f70a376` |
| `sherpa-onnx-v2.0.2-win-x64-shared-MT-Release-lib.tar.bz2` | `14db8ff397f56e4b102e958a2f29c3a13a2eb40643a3a4d0373b65b2b82c4bb3` |
| `sherpa-onnx-v2.0.2-osx-arm64-shared-lib.tar.bz2` | `1da59e61906f1c8dba86d80c799e34ef20eb89e90b49ea7d8bf7ef1257fb73ed` |

The build script also needs the cargo target folder to be named `target` (it
finds its cache from `OUT_DIR`).

## Chunking and silence

Silence detection is the same energy rule the TS package and Clipy's hosted
worker use (ffmpeg `silencedetect=noise=-30dB:d=0.4`), reimplemented over the
samples: a sample under -30 dBFS is quiet, a quiet run of at least 0.4 s is a
silence. Silero VAD was considered and not used: it is another model to
download, verify and ask consent for; cuts only need a pause, which the energy
rule finds in normal speech; and the same rule keeps CLI and app transcripts
identical. Its weakness is constant background noise (a fan, music), where no
silence is found and pieces are cut at the target length; the 2.5 s overlap
and the token-level merge keep those cuts clean for Parakeet. Revisit Silero if
real recordings show many hard cuts.

## Hosted providers

| Kind | Request | Limits |
|---|---|---|
| `openai-compatible` | `POST {base}/audio/transcriptions`, multipart `file`, `model`, `response_format=verbose_json`, `timestamp_granularities[]=segment`, `Authorization: Bearer` (omitted for an empty key) | pieces under 24 MB of WAV (786 s), about 600 s, cut in silences |
| `deepgram` | `POST {base}/v1/listen?model=nova-3&smart_format&punctuate&utterances&detect_language`, `Authorization: Token`, WAV body | whole recording |
| `assemblyai` | `POST /v2/upload`, `POST /v2/transcript` (`language_detection`, optional `speech_models`), poll `GET /v2/transcript/{id}` every 3 s | gives up after 60 min |
| `elevenlabs` | `POST {base}/v1/speech-to-text`, multipart `model_id=scribe_v2`, `file`, word timestamps, `xi-api-key` | whole recording |
| `gemini` | `POST {base}/models/gemini-3.5-flash-lite:generateContent`, inline WAV, JSON schema, `x-goog-api-key`; every answer validated | 5-minute pieces, about 4 minutes |

All: rustls, no redirects (a redirect could carry the key to another host),
answers capped at 20 MB, cancellation returns at once (requests run on a
helper thread; uploads stop at the next 256 KB).

## Tests

```sh
cargo test                                  # 49 unit tests, local fake HTTP server, no network
cargo clippy --all-targets -- -D warnings
cargo test --no-default-features            # without the engine

# Opt-in, real models from the default folder and espeak-ng speech:
CLIPY_TEST_MODELS=1 cargo test --release --test real_models -- --nocapture --test-threads=1

# By hand on a new platform:
cargo run --release --example transcribe -- speech16k.wav english-fast
```

Measured on the Linux sandbox (4 cores, 2 recognizer threads, release build,
espeak-ng speech): english-fast 8.7 s of audio in 0.9 s including model load;
266 s of audio (more than one Parakeet piece) in 20 s, 54 segments, no gaps, every
repeated sentence kept exactly once across the cut; multilingual 76 s of
English in 8.1 s and a Spanish clip detected as `es`.
