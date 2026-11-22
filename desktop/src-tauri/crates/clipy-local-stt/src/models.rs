//! Downloadable speech models for on-device transcription.
//!
//! The same models, pinned checksums, folder layout and install marker as
//! `packages/transcribe/src/models.ts`, so a model downloaded by the CLI
//! is reused by the app and the other way round. Nothing is downloaded unless
//! [`download_model`] is called, which the app does only after the user
//! agrees. Every file is checked against a pinned SHA-256, and a model is
//! staged in a temporary folder that is renamed into place only once every
//! file has verified, so a model folder is either complete or absent.

use std::fs::{self, File};
use std::io::{self, BufWriter, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, RecvTimeoutError};
use std::thread;
use std::time::Duration;

use serde::Deserialize;
use sha2::{Digest, Sha256};

use crate::error::{Result, SttError};
use crate::util::{hex, iso_now, process_alive, random_hex};
use crate::CancelToken;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum LocalModelId {
    /// NVIDIA Parakeet TDT-CTC 110M, English only, int8, run as a CTC model.
    EnglishFast,
    /// OpenAI Whisper base, about 99 languages, int8.
    Multilingual,
}

impl LocalModelId {
    pub const ALL: [LocalModelId; 2] = [LocalModelId::EnglishFast, LocalModelId::Multilingual];

    /// The id shared with the TS package and the CLI (`english-fast`, `multilingual`).
    pub fn as_str(self) -> &'static str {
        match self {
            LocalModelId::EnglishFast => "english-fast",
            LocalModelId::Multilingual => "multilingual",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|m| m.as_str() == s)
    }

    pub fn spec(self) -> &'static LocalModel {
        match self {
            LocalModelId::EnglishFast => &ENGLISH_FAST,
            LocalModelId::Multilingual => &MULTILINGUAL,
        }
    }
}

#[derive(Debug)]
pub struct ArchiveMember {
    /// Path inside the archive.
    pub name: &'static str,
    /// Path inside the model folder.
    pub path: &'static str,
    pub sha256: &'static str,
}

#[derive(Debug)]
pub struct ModelFile {
    pub url: &'static str,
    pub sha256: &'static str,
    /// Path inside the model folder (for an archive, its name while it downloads).
    pub path: &'static str,
    /// Download size in bytes; the download is cut off past it.
    pub bytes: u64,
    /// Set when `url` is a `.tar.bz2`: the files to keep from it, each with its own checksum.
    pub archive: Option<&'static [ArchiveMember]>,
}

#[derive(Debug)]
pub struct LocalModel {
    pub id: &'static str,
    pub label: &'static str,
    /// SPDX id of the model weights' license.
    pub license: &'static str,
    /// Who made the model and where it comes from, for notices.
    pub attribution: &'static str,
    /// Bumped when the files change, so an older install reads as not installed.
    pub version: &'static str,
    /// Total download size in bytes.
    pub size_bytes: u64,
    pub files: &'static [ModelFile],
}

// It is only published as an archive (104 MB download, 132 MB unpacked). It
// fails on long input, so audio is fed in pieces of at most 180 seconds.
static ENGLISH_FAST: LocalModel = LocalModel {
    id: "english-fast",
    label: "English, fast (Parakeet TDT-CTC 110M)",
    license: "CC-BY-4.0",
    attribution: "NVIDIA Parakeet TDT-CTC 110M (https://huggingface.co/nvidia/parakeet-tdt_ctc-110m), CC-BY-4.0, converted to ONNX int8 by the sherpa-onnx project",
    version: "parakeet-tdt-ctc-110m-en-int8-1",
    size_bytes: 104_337_827,
    files: &[ModelFile {
        url: "https://github.com/k2-fsa/sherpa-onnx/releases/download/asr-models/sherpa-onnx-nemo-parakeet_tdt_ctc_110m-en-36000-int8.tar.bz2",
        sha256: "17f945007b52ccd8b7200ffc7c5652e9e8e961dfdf479cefcabd06cf5703630b",
        path: "parakeet.tar.bz2",
        bytes: 104_337_827,
        archive: Some(&[
            ArchiveMember {
                name: "sherpa-onnx-nemo-parakeet_tdt_ctc_110m-en-36000-int8/model.int8.onnx",
                path: "model.int8.onnx",
                sha256: "9177a9146cf32ee0cc8152276ef95116f312018d316be37ccf57f7efea81fc1a",
            },
            ArchiveMember {
                name: "sherpa-onnx-nemo-parakeet_tdt_ctc_110m-en-36000-int8/tokens.txt",
                path: "tokens.txt",
                sha256: "450e56bd2f036fe5b6aa821865838cc5aa9d8b0106134ce9a9ba0664abe6cd10",
            },
        ]),
    }],
};

// Pinned to a Hugging Face commit so the bytes behind the URL cannot change
// under the checksum. Base rather than tiny: tiny is a third smaller but
// noticeably worse outside English. Whisper reads at most 30 s at a time.
static MULTILINGUAL: LocalModel = LocalModel {
    id: "multilingual",
    label: "All languages (Whisper base)",
    license: "MIT",
    attribution: "OpenAI Whisper base (https://github.com/openai/whisper), MIT, converted to ONNX int8 by the sherpa-onnx project",
    version: "whisper-base-int8-1",
    size_bytes: 160_609_290,
    files: &[
        ModelFile {
            url: "https://huggingface.co/csukuangfj/sherpa-onnx-whisper-base/resolve/bb53ee204431c90d314c1cc08d28d23e5b7927cc/base-encoder.int8.onnx",
            sha256: "0b8fb1304b6109976038efff5ace81720e00386f3ff6b54ee8c75291ca0a1e11",
            path: "encoder.int8.onnx",
            bytes: 29_120_534,
            archive: None,
        },
        ModelFile {
            url: "https://huggingface.co/csukuangfj/sherpa-onnx-whisper-base/resolve/bb53ee204431c90d314c1cc08d28d23e5b7927cc/base-decoder.int8.onnx",
            sha256: "9759d217388a01b3a4c7c15533201067b48ae819c4daafc8624e64b9409dc02d",
            path: "decoder.int8.onnx",
            bytes: 130_672_026,
            archive: None,
        },
        ModelFile {
            url: "https://huggingface.co/csukuangfj/sherpa-onnx-whisper-base/resolve/bb53ee204431c90d314c1cc08d28d23e5b7927cc/base-tokens.txt",
            sha256: "b34b360dbb493e781e479794586d661700670d65564001f23024971d1f2fa126",
            path: "tokens.txt",
            bytes: 816_730,
            archive: None,
        },
    ],
};

const MARKER: &str = ".installed.json";

/// Platform cache folder for models, the same one the CLI uses:
/// `~/.cache/clipy-local/models` on Linux (or `$XDG_CACHE_HOME`),
/// `~/Library/Caches/clipy-local/models` on macOS,
/// `%LOCALAPPDATA%\clipy-local\models` on Windows.
pub fn default_model_dir() -> PathBuf {
    #[cfg(windows)]
    if let Some(base) = std::env::var_os("LOCALAPPDATA").filter(|v| !v.is_empty()) {
        return PathBuf::from(base).join("clipy-local").join("models");
    }
    dirs::cache_dir()
        .or_else(|| dirs::home_dir().map(|h| h.join(".cache")))
        .unwrap_or_else(std::env::temp_dir)
        .join("clipy-local")
        .join("models")
}

/// Final paths of a model's files inside its folder, by file name.
pub fn model_file_paths(model: LocalModelId, dir: &Path) -> Vec<(&'static str, PathBuf)> {
    spec_file_paths(model.spec(), dir)
}

fn spec_file_paths(spec: &LocalModel, dir: &Path) -> Vec<(&'static str, PathBuf)> {
    let mut out = Vec::new();
    for f in spec.files {
        match f.archive {
            Some(members) => out.extend(members.iter().map(|m| (m.path, dir.join(m.path)))),
            None => out.push((f.path, dir.join(f.path))),
        }
    }
    out
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ModelStatus {
    pub installed: bool,
    /// The model's own folder (`<root>/<model id>`), whether or not it exists.
    pub dir: PathBuf,
    pub label: &'static str,
    pub size_bytes: u64,
}

/// Whether `model` is installed under `root` (the default model dir when `None`).
pub fn model_status(model: LocalModelId, root: Option<&Path>) -> ModelStatus {
    spec_status(model.spec(), root)
}

fn spec_status(spec: &LocalModel, root: Option<&Path>) -> ModelStatus {
    let root = root
        .map(Path::to_path_buf)
        .unwrap_or_else(default_model_dir);
    let dir = root.join(spec.id);
    #[derive(Deserialize)]
    struct Marker {
        version: Option<String>,
    }
    let installed = fs::read(dir.join(MARKER))
        .ok()
        .and_then(|b| serde_json::from_slice::<Marker>(&b).ok())
        .is_some_and(|m| m.version.as_deref() == Some(spec.version))
        && spec_file_paths(spec, &dir).iter().all(|(_, p)| p.is_file());
    ModelStatus {
        installed,
        dir,
        label: spec.label,
        size_bytes: spec.size_bytes,
    }
}

/// Download, verify and install a model under `root` (the default model dir
/// when `None`). Returns its folder. Safe to call again: an installed model is
/// returned as is. `progress` receives 0..=1.
pub fn download_model(
    model: LocalModelId,
    root: Option<&Path>,
    progress: &mut dyn FnMut(f64),
    cancel: &CancelToken,
) -> Result<PathBuf> {
    download_spec(model.spec(), root, progress, cancel)
}

pub(crate) fn download_spec(
    spec: &LocalModel,
    root: Option<&Path>,
    progress: &mut dyn FnMut(f64),
    cancel: &CancelToken,
) -> Result<PathBuf> {
    let status = spec_status(spec, root);
    if status.installed {
        progress(1.0);
        return Ok(status.dir);
    }
    let root = status
        .dir
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or_default();
    fs::create_dir_all(&root)?;
    remove_abandoned_stages(&root);
    let stage = root.join(format!(
        ".{}.partial-{}-{}",
        spec.id,
        std::process::id(),
        random_hex()
    ));
    fs::create_dir_all(&stage)?;

    let result = (|| -> Result<()> {
        let client = download_client()?;
        let has_archive = spec.files.iter().any(|f| f.archive.is_some());
        let total: u64 = spec.files.iter().map(|f| f.bytes).sum::<u64>().max(1);
        let share = if has_archive { 0.85 } else { 1.0 };
        let mut done = 0u64;
        progress(0.0);
        for f in spec.files {
            cancel.check()?;
            let dest = stage.join(f.path);
            download_verified(
                &client,
                f.url,
                &dest,
                f.sha256,
                spec.label,
                f.bytes,
                cancel,
                &mut |n| {
                    done += n;
                    progress((done as f64 / total as f64 * share).min(share));
                },
            )?;
            if let Some(members) = f.archive {
                extract_archive_members(&dest, members, &stage, spec.label, cancel)?;
                fs::remove_file(&dest)?;
            }
        }
        let marker = serde_json::json!({ "model": spec.id, "version": spec.version, "installedAt": iso_now() });
        fs::write(
            stage.join(MARKER),
            serde_json::to_vec_pretty(&marker).unwrap_or_default(),
        )?;
        cancel.check()?;
        if status.dir.exists() {
            fs::remove_dir_all(&status.dir)?;
        }
        fs::rename(&stage, &status.dir)?;
        Ok(())
    })();
    match result {
        Ok(()) => {
            progress(1.0);
            Ok(status.dir)
        }
        Err(e) => {
            let _ = fs::remove_dir_all(&stage);
            if cancel.is_cancelled() {
                Err(SttError::Cancelled)
            } else {
                Err(e)
            }
        }
    }
}

fn download_client() -> Result<reqwest::blocking::Client> {
    // Redirects are followed: the model hosts serve from a CDN, and no key or
    // other credential is sent with these requests. The total timeout is a
    // backstop only: a stalled connection is caught by `STALL_TIMEOUT`.
    reqwest::blocking::Client::builder()
        .redirect(reqwest::redirect::Policy::limited(10))
        .connect_timeout(Duration::from_secs(30))
        .timeout(Duration::from_secs(2 * 60 * 60))
        .user_agent(concat!("clipy-local-stt/", env!("CARGO_PKG_VERSION")))
        .build()
        .map_err(|_| SttError::Download("Could not start the model download".into()))
}

enum Chunk {
    Head(u16, Option<u64>),
    Data(Vec<u8>),
    End,
    Failed(&'static str),
}

const POLL: Duration = Duration::from_millis(50);
/// A download that receives nothing for this long is given up.
const STALL_TIMEOUT: Duration = Duration::from_secs(60);

/// Stream `url` to `dest`, hashing as it goes. A server that sends more than
/// `max_bytes` (the file's expected size) is cut off at that size instead of
/// filling the disk.
#[allow(clippy::too_many_arguments)]
fn download_verified(
    client: &reqwest::blocking::Client,
    url: &str,
    dest: &Path,
    sha256: &str,
    label: &str,
    max_bytes: u64,
    cancel: &CancelToken,
    on_bytes: &mut dyn FnMut(u64),
) -> Result<()> {
    // The request and every read run on a helper thread, so a cancel or a
    // stalled connection is noticed at once instead of after a blocked read.
    let (tx, rx) = mpsc::sync_channel::<Chunk>(8);
    let req = client.get(url);
    thread::Builder::new()
        .name("clipy-model-download".into())
        .spawn(move || {
            let mut res = match req.send() {
                Ok(r) => r,
                Err(e) => {
                    let _ = tx.send(Chunk::Failed(if e.is_timeout() {
                        "the server did not answer in time"
                    } else {
                        "the server could not be reached"
                    }));
                    return;
                }
            };
            if tx
                .send(Chunk::Head(res.status().as_u16(), res.content_length()))
                .is_err()
            {
                return;
            }
            let mut buf = vec![0u8; 64 * 1024];
            loop {
                match res.read(&mut buf) {
                    Ok(0) => {
                        let _ = tx.send(Chunk::End);
                        return;
                    }
                    Ok(n) => {
                        if tx.send(Chunk::Data(buf[..n].to_vec())).is_err() {
                            return;
                        }
                    }
                    Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
                    Err(_) => {
                        let _ = tx.send(Chunk::Failed("the connection dropped"));
                        return;
                    }
                }
            }
        })?;
    let next = || -> Result<Chunk> {
        let mut waited = Duration::ZERO;
        loop {
            cancel.check()?;
            match rx.recv_timeout(POLL) {
                Ok(c) => return Ok(c),
                Err(RecvTimeoutError::Timeout) => {
                    waited += POLL;
                    if waited >= STALL_TIMEOUT {
                        return Err(SttError::Download(format!(
                            "Could not download {label}: the connection stalled"
                        )));
                    }
                }
                Err(RecvTimeoutError::Disconnected) => {
                    return Err(SttError::Download(format!(
                        "Could not download {label}: the connection dropped"
                    )))
                }
            }
        }
    };
    match next()? {
        Chunk::Head(status, len) => {
            if !(200..300).contains(&status) {
                return Err(SttError::Download(format!(
                    "Could not download {label} (HTTP {status})"
                )));
            }
            if len.is_some_and(|n| n > max_bytes) {
                return Err(SttError::DownloadTooLarge {
                    what: label.to_string(),
                    max_bytes,
                });
            }
        }
        Chunk::Failed(why) => {
            return Err(SttError::Download(format!(
                "Could not download {label}: {why}"
            )))
        }
        _ => return Err(SttError::Download(format!("Could not download {label}"))),
    }
    let mut hash = Sha256::new();
    let mut out = BufWriter::new(File::create(dest)?);
    let mut received = 0u64;
    loop {
        let data = match next()? {
            Chunk::Data(d) => d,
            Chunk::End => break,
            Chunk::Failed(why) => {
                return Err(SttError::Download(format!(
                    "Could not download {label}: {why}"
                )))
            }
            Chunk::Head(..) => continue,
        };
        received += data.len() as u64;
        if received > max_bytes {
            return Err(SttError::DownloadTooLarge {
                what: label.to_string(),
                max_bytes,
            });
        }
        hash.update(&data);
        out.write_all(&data)?;
        on_bytes(data.len() as u64);
    }
    out.flush()?;
    drop(out);
    if hex(&hash.finalize()) != sha256.to_ascii_lowercase() {
        return Err(SttError::Checksum(label.to_string()));
    }
    Ok(())
}

/// A reader that stops with an error once the token is cancelled.
struct CancelRead<'a, R> {
    inner: R,
    cancel: &'a CancelToken,
}

impl<R: Read> Read for CancelRead<'_, R> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if self.cancel.is_cancelled() {
            return Err(io::Error::new(io::ErrorKind::Interrupted, "cancelled"));
        }
        self.inner.read(buf)
    }
}

/// Pull the listed members out of a verified `.tar.bz2`, streaming each one
/// to a temporary file while hashing it, and rename it into place only when
/// its checksum matches.
fn extract_archive_members(
    archive_path: &Path,
    members: &[ArchiveMember],
    out_dir: &Path,
    label: &str,
    cancel: &CancelToken,
) -> Result<()> {
    let unpack_err = || SttError::Download(format!("The {label} archive could not be unpacked"));
    let file = File::open(archive_path)?;
    let reader = CancelRead {
        inner: bzip2::read::BzDecoder::new(io::BufReader::new(file)),
        cancel,
    };
    let mut tar = tar::Archive::new(reader);
    let mut found = vec![false; members.len()];
    let entries = tar.entries().map_err(|_| unpack_err())?;
    for entry in entries {
        cancel.check()?;
        let mut entry = entry.map_err(|_| {
            if cancel.is_cancelled() {
                SttError::Cancelled
            } else {
                unpack_err()
            }
        })?;
        if !entry.header().entry_type().is_file() {
            continue;
        }
        let name = match entry.path() {
            Ok(p) => p.to_string_lossy().replace('\\', "/"),
            Err(_) => continue,
        };
        let name = name.strip_prefix("./").unwrap_or(&name).to_string();
        let Some(i) = members.iter().position(|m| m.name == name) else {
            continue;
        };
        let m = &members[i];
        let tmp = out_dir.join(format!(".{}.{}.tmp", m.path, random_hex()));
        let mut hash = Sha256::new();
        {
            let mut out = BufWriter::new(File::create(&tmp)?);
            let mut buf = vec![0u8; 64 * 1024];
            loop {
                let n = match entry.read(&mut buf) {
                    Ok(0) => break,
                    Ok(n) => n,
                    Err(_) => {
                        let _ = fs::remove_file(&tmp);
                        cancel.check()?;
                        return Err(unpack_err());
                    }
                };
                hash.update(&buf[..n]);
                out.write_all(&buf[..n])?;
            }
            out.flush()?;
        }
        if hex(&hash.finalize()) != m.sha256.to_ascii_lowercase() {
            let _ = fs::remove_file(&tmp);
            return Err(SttError::Checksum(format!("{label} ({})", m.path)));
        }
        fs::rename(&tmp, out_dir.join(m.path))?;
        found[i] = true;
        if found.iter().all(|f| *f) {
            break;
        }
    }
    let missing: Vec<&str> = members
        .iter()
        .zip(&found)
        .filter(|(_, f)| !**f)
        .map(|(m, _)| m.path)
        .collect();
    if !missing.is_empty() {
        return Err(SttError::Download(format!(
            "The {label} archive is missing {}",
            missing.join(", ")
        )));
    }
    Ok(())
}

/// Remove staging folders left by a download whose process died before it
/// could clean up (killed, power loss). A folder whose process is still
/// alive belongs to a download in progress and is left alone. The name
/// pattern is the TS package's, so the CLI and the app clean up each other's
/// leftovers.
fn remove_abandoned_stages(root: &Path) {
    let Ok(entries) = fs::read_dir(root) else {
        return;
    };
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().to_string();
        let Some(pid) = stage_pid(&name) else {
            continue;
        };
        if pid == std::process::id() || process_alive(pid) {
            continue;
        }
        let _ = fs::remove_dir_all(entry.path());
    }
}

/// The process id in `.<model>.partial-<pid>-<hex>`.
fn stage_pid(name: &str) -> Option<u32> {
    let rest = name.strip_prefix('.')?;
    let (_, tail) = rest.rsplit_once(".partial-")?;
    let (pid, hexpart) = tail.split_once('-')?;
    if hexpart.is_empty() || !hexpart.chars().all(|c| c.is_ascii_hexdigit()) || pid.is_empty() {
        return None;
    }
    pid.parse().ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_server::{Reply, TestServer};

    fn sha(b: &[u8]) -> String {
        hex(&Sha256::digest(b))
    }

    fn leak(s: String) -> &'static str {
        Box::leak(s.into_boxed_str())
    }

    fn plain_spec(
        url: String,
        body: &[u8],
        bytes: u64,
        sha256: Option<String>,
    ) -> &'static LocalModel {
        let files: &'static [ModelFile] = Box::leak(Box::new([ModelFile {
            url: leak(url),
            sha256: leak(sha256.unwrap_or_else(|| sha(body))),
            path: "weights.bin",
            bytes,
            archive: None,
        }]));
        Box::leak(Box::new(LocalModel {
            id: "test-model",
            label: "Test model",
            license: "MIT",
            attribution: "test",
            version: "test-1",
            size_bytes: bytes,
            files,
        }))
    }

    fn leftovers(root: &Path) -> Vec<String> {
        fs::read_dir(root)
            .map(|r| {
                r.flatten()
                    .map(|e| e.file_name().to_string_lossy().to_string())
                    .collect()
            })
            .unwrap_or_default()
    }

    #[test]
    fn installs_verified_files_with_a_marker_the_cli_reads() {
        let body = b"model weights".repeat(1000);
        let server = TestServer::start({
            let body = body.clone();
            move |_| Reply::bytes(200, body.clone())
        });
        let spec = plain_spec(server.url("/w"), &body, body.len() as u64, None);
        let root = tempfile::tempdir().unwrap();
        let mut seen = Vec::new();
        let dir = download_spec(
            spec,
            Some(root.path()),
            &mut |p| seen.push(p),
            &CancelToken::new(),
        )
        .unwrap();
        assert_eq!(dir, root.path().join("test-model"));
        assert_eq!(fs::read(dir.join("weights.bin")).unwrap(), body);
        let marker: serde_json::Value =
            serde_json::from_slice(&fs::read(dir.join(MARKER)).unwrap()).unwrap();
        assert_eq!(marker["version"], "test-1");
        assert_eq!(marker["model"], "test-model");
        assert!(spec_status(spec, Some(root.path())).installed);
        assert_eq!(*seen.last().unwrap(), 1.0);
        assert_eq!(leftovers(root.path()), vec!["test-model".to_string()]);
        // Already installed: no request.
        let before = server.requests().len();
        download_spec(spec, Some(root.path()), &mut |_| {}, &CancelToken::new()).unwrap();
        assert_eq!(server.requests().len(), before);
    }

    #[test]
    fn checksum_failure_installs_nothing() {
        let body = b"tampered".to_vec();
        let server = TestServer::start(move |_| Reply::bytes(200, body.clone()));
        let spec = plain_spec(server.url("/w"), b"original", 8, Some(sha(b"original")));
        let root = tempfile::tempdir().unwrap();
        let err =
            download_spec(spec, Some(root.path()), &mut |_| {}, &CancelToken::new()).unwrap_err();
        assert!(matches!(err, SttError::Checksum(_)), "{err}");
        assert!(
            leftovers(root.path()).is_empty(),
            "{:?}",
            leftovers(root.path())
        );
        assert!(!spec_status(spec, Some(root.path())).installed);
    }

    #[test]
    fn a_server_sending_more_than_the_pinned_size_is_cut_off() {
        let big = vec![7u8; 200_000];
        // Declared length too large.
        let server = TestServer::start({
            let big = big.clone();
            move |_| Reply::bytes(200, big.clone())
        });
        let spec = plain_spec(server.url("/w"), &big, 1000, None);
        let root = tempfile::tempdir().unwrap();
        let err =
            download_spec(spec, Some(root.path()), &mut |_| {}, &CancelToken::new()).unwrap_err();
        assert!(
            matches!(
                err,
                SttError::DownloadTooLarge {
                    max_bytes: 1000,
                    ..
                }
            ),
            "{err}"
        );
        // No declared length: cut off while streaming.
        let server = TestServer::start({
            let big = big.clone();
            move |_| Reply::chunked(200, big.clone())
        });
        let spec = plain_spec(server.url("/w"), &big, 1000, None);
        let err =
            download_spec(spec, Some(root.path()), &mut |_| {}, &CancelToken::new()).unwrap_err();
        assert!(matches!(err, SttError::DownloadTooLarge { .. }), "{err}");
        assert!(leftovers(root.path()).is_empty());
    }

    #[test]
    fn http_errors_name_the_status() {
        let server = TestServer::start(|_| Reply::bytes(404, b"nope".to_vec()));
        let spec = plain_spec(server.url("/w"), b"x", 1, None);
        let root = tempfile::tempdir().unwrap();
        let err =
            download_spec(spec, Some(root.path()), &mut |_| {}, &CancelToken::new()).unwrap_err();
        assert_eq!(err.to_string(), "Could not download Test model (HTTP 404)");
    }

    #[test]
    fn cancel_mid_download_removes_the_stage() {
        let body = vec![1u8; 4_000_000];
        let server = TestServer::start({
            let body = body.clone();
            move |_| Reply::slow(200, body.clone())
        });
        let spec = plain_spec(server.url("/w"), &body, body.len() as u64, None);
        let root = tempfile::tempdir().unwrap();
        let cancel = CancelToken::new();
        let c2 = cancel.clone();
        let mut stage_seen = false;
        let root_path = root.path().to_path_buf();
        let err = download_spec(
            spec,
            Some(root.path()),
            &mut |p| {
                if p > 0.0 {
                    stage_seen |= leftovers(&root_path)
                        .iter()
                        .any(|n| n.contains(".partial-"));
                    c2.cancel();
                }
            },
            &cancel,
        )
        .unwrap_err();
        assert!(matches!(err, SttError::Cancelled), "{err}");
        assert!(stage_seen);
        assert!(
            leftovers(root.path()).is_empty(),
            "{:?}",
            leftovers(root.path())
        );
    }

    #[test]
    fn abandoned_stages_of_dead_processes_are_swept() {
        let root = tempfile::tempdir().unwrap();
        let dead = root
            .path()
            .join(".english-fast.partial-2147480000-abcd1234");
        let mine = root.path().join(format!(
            ".english-fast.partial-{}-abcd1234",
            std::process::id()
        ));
        let other = root.path().join("english-fast");
        for d in [&dead, &mine, &other] {
            fs::create_dir_all(d).unwrap();
        }
        remove_abandoned_stages(root.path());
        assert!(!dead.exists());
        assert!(mine.exists() && other.exists());
        assert_eq!(stage_pid(".multilingual.partial-12-0f"), Some(12));
        assert_eq!(stage_pid(".multilingual.partial-12-"), None);
        assert_eq!(stage_pid("english-fast"), None);
    }

    fn tar_bz2(entries: &[(&str, &[u8])]) -> Vec<u8> {
        let mut builder = tar::Builder::new(Vec::new());
        for (name, data) in entries {
            let mut h = tar::Header::new_gnu();
            h.set_size(data.len() as u64);
            h.set_mode(0o644);
            h.set_entry_type(tar::EntryType::Regular);
            h.set_cksum();
            builder.append_data(&mut h, name, *data).unwrap();
        }
        let tar = builder.into_inner().unwrap();
        let mut enc = bzip2::write::BzEncoder::new(Vec::new(), bzip2::Compression::fast());
        enc.write_all(&tar).unwrap();
        enc.finish().unwrap()
    }

    fn archive_spec(
        url: String,
        archive: &[u8],
        members: Vec<(&'static str, &'static str, String)>,
    ) -> &'static LocalModel {
        let members: &'static [ArchiveMember] = Box::leak(
            members
                .into_iter()
                .map(|(name, path, sha256)| ArchiveMember {
                    name,
                    path,
                    sha256: leak(sha256),
                })
                .collect::<Vec<_>>()
                .into_boxed_slice(),
        );
        let files: &'static [ModelFile] = Box::leak(Box::new([ModelFile {
            url: leak(url),
            sha256: leak(sha(archive)),
            path: "a.tar.bz2",
            bytes: archive.len() as u64,
            archive: Some(members),
        }]));
        Box::leak(Box::new(LocalModel {
            id: "archived",
            label: "Archived",
            license: "MIT",
            attribution: "test",
            version: "a-1",
            size_bytes: archive.len() as u64,
            files,
        }))
    }

    #[test]
    fn archive_members_are_extracted_and_each_verified() {
        let archive = tar_bz2(&[
            ("pkg/readme.txt", b"skip me"),
            ("./pkg/model.onnx", b"onnx bytes"),
            ("pkg/tokens.txt", b"a 0\nb 1\n"),
        ]);
        let server = TestServer::start({
            let a = archive.clone();
            move |_| Reply::bytes(200, a.clone())
        });
        let spec = archive_spec(
            server.url("/a"),
            &archive,
            vec![
                ("pkg/model.onnx", "model.onnx", sha(b"onnx bytes")),
                ("pkg/tokens.txt", "tokens.txt", sha(b"a 0\nb 1\n")),
            ],
        );
        let root = tempfile::tempdir().unwrap();
        let dir = download_spec(spec, Some(root.path()), &mut |_| {}, &CancelToken::new()).unwrap();
        assert_eq!(fs::read(dir.join("model.onnx")).unwrap(), b"onnx bytes");
        assert!(!dir.join("a.tar.bz2").exists() && !dir.join("readme.txt").exists());

        // A member whose checksum does not match fails the install.
        let spec = archive_spec(
            server.url("/a"),
            &archive,
            vec![("pkg/model.onnx", "model.onnx", sha(b"other"))],
        );
        let root = tempfile::tempdir().unwrap();
        let err =
            download_spec(spec, Some(root.path()), &mut |_| {}, &CancelToken::new()).unwrap_err();
        assert!(
            matches!(err, SttError::Checksum(ref w) if w.contains("model.onnx")),
            "{err}"
        );
        assert!(leftovers(root.path()).is_empty());

        // A missing member is named.
        let spec = archive_spec(
            server.url("/a"),
            &archive,
            vec![("pkg/absent.bin", "absent.bin", sha(b""))],
        );
        let err =
            download_spec(spec, Some(root.path()), &mut |_| {}, &CancelToken::new()).unwrap_err();
        assert!(err.to_string().contains("missing absent.bin"), "{err}");
    }

    #[test]
    fn pinned_specs_match_the_ts_package() {
        let e = LocalModelId::EnglishFast.spec();
        assert_eq!(
            e.files[0].sha256,
            "17f945007b52ccd8b7200ffc7c5652e9e8e961dfdf479cefcabd06cf5703630b"
        );
        assert!(e.files[0].archive.unwrap()[0]
            .name
            .starts_with("sherpa-onnx-nemo-parakeet_tdt_ctc_110m-en-36000-int8/"));
        let m = LocalModelId::Multilingual.spec();
        assert_eq!(m.files.iter().map(|f| f.bytes).sum::<u64>(), m.size_bytes);
        assert_eq!(
            LocalModelId::parse("multilingual"),
            Some(LocalModelId::Multilingual)
        );
        assert!(default_model_dir().ends_with("clipy-local/models"));
    }

    #[test]
    fn status_requires_marker_version_and_files() {
        let root = tempfile::tempdir().unwrap();
        let dir = root.path().join("english-fast");
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("model.int8.onnx"), b"x").unwrap();
        fs::write(dir.join("tokens.txt"), b"x").unwrap();
        assert!(!model_status(LocalModelId::EnglishFast, Some(root.path())).installed);
        fs::write(
            dir.join(MARKER),
            br#"{"version":"parakeet-tdt-ctc-110m-en-int8-0"}"#,
        )
        .unwrap();
        assert!(!model_status(LocalModelId::EnglishFast, Some(root.path())).installed);
        fs::write(
            dir.join(MARKER),
            br#"{"model":"english-fast","version":"parakeet-tdt-ctc-110m-en-int8-1"}"#,
        )
        .unwrap();
        assert!(model_status(LocalModelId::EnglishFast, Some(root.path())).installed);
        fs::remove_file(dir.join("tokens.txt")).unwrap();
        assert!(!model_status(LocalModelId::EnglishFast, Some(root.path())).installed);
    }
}
