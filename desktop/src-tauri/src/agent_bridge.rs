//! Agent bridge — lets a LOCAL AI agent (Claude Code in the user's terminal,
//! via `clipy record --source mac-screen` / `clipy session … `) trigger real
//! screen recordings through the running Clipy app.
//!
//! Transport: a Unix domain socket (NOT localhost HTTP — a UDS is unreachable
//! from browser pages, which removes the DNS-rebinding/CORS attack class
//! entirely). Protocol: one JSON
//! request line per connection, one JSON response line back, connection
//! closes. Discovery: `agent-bridge.json` next to the socket in the app data
//! dir, written atomically with 0600 permissions, containing
//! `{socketPath, token, pid, appVersion, protocolVersion}`.
//!
//! Security model (layered):
//! - the socket + discovery file are 0600 — only the same user can connect;
//! - a per-launch random bearer token must accompany every request (second
//!   factor against fd-passing/inherited-handle surprises);
//! - the FIRST agent-triggered start shows a blocking in-app consent dialog
//!   ("remember" persists to agent-consent.json; deny blocks the bridge);
//! - every agent recording uses the app's normal recording pipeline, so the
//!   standard recording indicator is always visible, and macOS TCC screen
//!   permission applies to the app exactly as for human recordings;
//! - a mandatory max duration (default 600s, cap 1800s) auto-stops the
//!   recording, so a crashed/forgotten agent can never record forever;
//! - all commands serialize through one async mutex — no start/stop races.
//!
//! Marks ("narration"): the agent can attach timestamped notes while
//! recording; they are journaled to disk as they arrive (crash-durable) and
//! ride the existing raw-upload `complete` payload as `narration`, which the
//! server turns into the transcript when the recording has no speech. The upload path
//! consults `agent_complete_extras` for this.
//!
//! The local edition (Clipy Local, no `cloud` feature) runs the same bridge
//! with no sign-in: `start` records a Local take, `stop` waits for the take to
//! finish processing on this computer and returns its AREC path, and the marks
//! go into the take folder as `agent-marks.json`, which local processing turns
//! into narration. Its discovery file sits in `Clipy Local/`, so an agent can
//! never mistake it for the official app (which uploads).

use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::time::Instant;

use anyhow::{anyhow, Context, Result};
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Manager};
#[cfg(unix)]
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
#[cfg(unix)]
use tokio::net::{UnixListener, UnixStream};

use crate::auth::AuthState;
use crate::recording::RecordingController;
use crate::types::{AudioOptionsDto, CaptureSourceDto, RecordingMode, RecordingPhase};

/// 2 (was 1): `start` accepts an optional `audio` object AND agent recordings
/// now default the mic OFF. The request change alone is additive — an old CLI
/// sends no `audio` and gets the safer default — but the version is what lets a
/// NEW CLI tell whether `includeMic:false` will actually be honoured: a v1 app
/// silently ignores the field and records the mic anyway, which a privacy flag
/// must not fail open on. Gate the flag's guarantee on `protocolVersion >= 2`.
///
/// 3 (was 2): a `start` for a window echoes `windowCapture` (`window` or
/// `area`). Agent starts record a window by itself on macOS, but an older app
/// records the window's fixed screen area, covering windows included, so the
/// CLI must not tell an agent the window is isolated unless the app says so.
const PROTOCOL_VERSION: u32 = 3;
const DEFAULT_MAX_SEC: u64 = 600;
const HARD_CAP_SEC: u64 = 1800;
/// Max marks per session. MUST NOT exceed the server's narration cap
/// (`MAX_NOTES = 200` on the server): the server rejects
/// a `complete` payload with more than that many notes, and the uploader
/// retries the resulting 5xx forever — which would wedge the app in
/// "uploading" and hang `clipy session stop`. Refuse marks past this at
/// `mark` time (with a clear error) rather than dying at upload time.
const MAX_MARKS: usize = 200;
/// Max characters per mark. Matches the server's `MAX_NOTE_CHARS = 1000`.
const MAX_MARK_CHARS: usize = 1000;
/// How long a local-edition `stop` waits for the take to finish processing
/// (transcript, moments, frames). Past it the result says so, and the AREC,
/// already usable while Preparing, keeps updating in place.
const LOCAL_PROCESSING_WAIT_SECS: u64 = 300;

/// Reported in the discovery file and `status`, so a client can tell the
/// official app (uploads) from Clipy Local (keeps everything on this computer).
const EDITION: &str = if crate::edition::CLOUD { "cloud" } else { "local" };
/// The sibling of the app data dir that holds `agent-bridge.json`. Different
/// per edition so the two apps never overwrite each other's discovery file.
const DISCOVERY_DIR_NAME: &str = if crate::edition::CLOUD { "Clipy" } else { "Clipy Local" };
/// Clipy Local has nowhere to upload to, so its agent takes are Local takes.
const AGENT_RECORDING_MODE: RecordingMode = if crate::edition::CLOUD {
    RecordingMode::Instant
} else {
    RecordingMode::Local
};

const STOPPED_FROM_APP: &str =
    "the recording was stopped from the Clipy Local app, so this agent session is no longer active";

// ---------------------------------------------------------------------------
// Session state (one agent session at a time, guarded by COMMAND_LOCK)
// ---------------------------------------------------------------------------

#[derive(Clone, Serialize)]
struct Mark {
    #[serde(rename = "startMs")]
    start_ms: u64,
    text: String,
}

struct AgentSession {
    recording_id: String,
    started: Instant,
    max_sec: u64,
    title: Option<String>,
    description: Option<String>,
    marks: Vec<Mark>,
    journal_path: PathBuf,
    audio: AudioOptionsDto,
}

static SESSION: Mutex<Option<AgentSession>> = Mutex::new(None);
/// The `stop` payload of a session the watchdog auto-stopped, reported once.
static LAST_RESULT: Mutex<Option<serde_json::Value>> = Mutex::new(None);
/// Recording id of an agent session that was ended by the APP (a human pressing
/// Stop/Cancel, or a superseding recording) rather than by the agent. Set by
/// [`clear_session_for`]; consumed by `stop`/`abort`/`mark` so the CLI gets a
/// clear "stopped from the app" error instead of a bare "no session". One-shot.
static EXTERNAL_STOP: Mutex<Option<String>> = Mutex::new(None);
static COMMAND_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

/// Drop the active agent session IF it belongs to `recording_id`. Called by the
/// recording controller from EVERY stop/cancel path (human tray/menu clicks and
/// UI commands included) so a recording the user ends from the app can never
/// leave a stale `SESSION` behind — which the max-duration watchdog or a later
/// `clipy session stop` could otherwise use to stop a NEWER, unrelated
/// recording. A no-op when no session matches (the common case: web/human
/// recordings, or the agent's own stop path, which already took `SESSION`
/// before calling the controller). Records an [`EXTERNAL_STOP`] marker so the
/// agent's next command reports the app-side stop clearly.
pub fn clear_session_for(recording_id: &str) {
    let cleared = {
        let mut guard = SESSION.lock();
        match guard.as_ref() {
            Some(s) if s.recording_id == recording_id => guard.take(),
            _ => None,
        }
    };
    if let Some(session) = cleared {
        let _ = std::fs::remove_file(&session.journal_path);
        *EXTERNAL_STOP.lock() = Some(recording_id.to_string());
        tracing::info!(
            recording_id,
            "agent session ended from the app — cleared the bridge session"
        );
    }
}

/// If a prior [`clear_session_for`] flagged an app-side stop, consume it and
/// return the CLI error to send. Lets `stop`/`abort`/`mark` explain that the
/// recording was stopped from the app rather than returning a bare "no session".
fn external_stop_error() -> Option<String> {
    if EXTERNAL_STOP.lock().take().is_some() {
        Some(err_json("stopped_from_app", STOPPED_FROM_APP))
    } else {
        None
    }
}

/// Called by upload.rs while building the raw-upload `complete` body.

// ---------------------------------------------------------------------------
// Wire protocol
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
struct BridgeRequest {
    token: String,
    cmd: String,
    #[serde(default)]
    text: Option<String>,
    #[serde(default, rename = "maxSec")]
    max_sec: Option<u64>,
    #[serde(default)]
    title: Option<String>,
    #[serde(default)]
    description: Option<String>,
    /// Pre-timestamped notes for one-shot `record` flows (absolute ms from
    /// recording start), merged with live marks at upload.
    #[serde(default)]
    notes: Option<Vec<IncomingNote>>,
    /// What to capture. Omitted = the app's default (primary display), which
    /// is what every pre-0.4 CLI sends. Shapes (serde-tagged on `type`):
    /// `{"type":"display","id":N}`, `{"type":"window","id":N}`,
    /// `{"type":"area","display_id":N,"x":..,"y":..,"width":..,"height":..}`.
    /// Ids come from the `sources` command — they are not stable across
    /// launches, so an agent must enumerate immediately before starting.
    #[serde(default)]
    source: Option<CaptureSourceDto>,
    /// Audio capture. Omitted = the agent default (system audio on, mic OFF)
    /// — see [`agent_audio_options`]. Every field is individually optional so
    /// a caller can flip one without restating the others.
    #[serde(default)]
    audio: Option<BridgeAudioDto>,
}

/// Wire shape:
/// `{"includeSystemAudio":bool,"includeMic":bool,"micDeviceId":str,"reduceNoise":bool}`.
/// `Option<bool>` (not `bool`) so "absent" stays distinguishable from an
/// explicit `false` — absent falls through to the agent default, which is not
/// the same as [`AudioOptionsDto`]'s own field defaults.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct BridgeAudioDto {
    #[serde(default)]
    include_system_audio: Option<bool>,
    #[serde(default)]
    include_mic: Option<bool>,
    #[serde(default)]
    mic_device_id: Option<String>,
    #[serde(default)]
    reduce_noise: Option<bool>,
}

/// Audio defaults for an **agent-initiated** recording. These deliberately
/// differ from the app's interactive defaults (`AudioOptionsDto::default()`,
/// which turns both on): a human clicking Record is present at the machine and
/// intends their mic; an agent recording on the user's behalf is not the same
/// consent, and nobody asked for the room — or whatever call the user is on —
/// to be captured. Agent narration rides on `mark`s, not speech, so the mic
/// buys nothing here.
///
/// System audio stays ON by default because it captures the machine rather
/// than the room, and it gives agent recordings real STT transcripts of app
/// sounds. An explicit `includeMic: true` is honoured.
fn agent_audio_options(req: Option<&BridgeAudioDto>) -> AudioOptionsDto {
    AudioOptionsDto {
        include_system_audio: req.and_then(|a| a.include_system_audio).unwrap_or(true),
        include_mic: req.and_then(|a| a.include_mic).unwrap_or(false),
        mic_device_id: req.and_then(|a| a.mic_device_id.clone()),
        // Off unless asked for. Evidence recordings are the one place where
        // an unaltered microphone matters most: a filter that decided part of
        // what was said was room noise would be editing the evidence.
        reduce_noise: req.and_then(|a| a.reduce_noise).unwrap_or(false),
    }
}

#[derive(Deserialize)]
struct IncomingNote {
    #[serde(rename = "startMs")]
    start_ms: u64,
    text: String,
}

fn ok_json(value: serde_json::Value) -> String {
    serde_json::json!({ "ok": true, "data": value }).to_string()
}

fn err_json(code: &str, message: &str) -> String {
    serde_json::json!({ "ok": false, "error": { "code": code, "message": message } }).to_string()
}

// ---------------------------------------------------------------------------
// Bridge startup (called from lib.rs setup)
// ---------------------------------------------------------------------------

#[derive(Serialize)]
struct BridgeDiscovery<'a> {
    #[serde(rename = "socketPath")]
    socket_path: &'a str,
    token: &'a str,
    pid: u32,
    #[serde(rename = "appVersion")]
    app_version: &'a str,
    #[serde(rename = "protocolVersion")]
    protocol_version: u32,
    edition: &'a str,
}

/// `~/Library/Application Support/Clipy` (`Clipy Local` for the local
/// edition): the sibling of the app data dir. Derived from `data_dir`'s parent
/// rather than `$HOME` so it tracks whatever base Tauri resolved (and stays
/// correct under a redirected HOME in tests).
fn discovery_dir_path(data_dir: &Path) -> PathBuf {
    data_dir
        .parent()
        .map(|base| base.join(DISCOVERY_DIR_NAME))
        .unwrap_or_else(|| data_dir.to_path_buf())
}

fn random_token() -> String {
    use rand::RngCore;
    let mut bytes = [0u8; 32];
    rand::thread_rng().fill_bytes(&mut bytes);
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// Atomic 0600 write: temp file in the same dir, fsync, rename.
fn write_discovery_file(path: &Path, contents: &str) -> Result<()> {
    let tmp = path.with_extension("json.tmp");
    {
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .open(&tmp)
            .context("open discovery temp file")?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            file.set_permissions(std::fs::Permissions::from_mode(0o600))
                .context("chmod discovery temp file")?;
        }
        file.write_all(contents.as_bytes())
            .context("write discovery temp file")?;
        file.sync_all().ok();
    }
    std::fs::rename(&tmp, path).context("rename discovery file into place")?;
    Ok(())
}

/// Spawn the bridge listener. Non-fatal on failure: the app runs fine
/// without agent control; we log and move on. Unix-only transport (the
/// Windows build gets a no-op until a named-pipe transport lands).
#[cfg(unix)]
pub fn spawn(app: AppHandle) {
    tauri::async_runtime::spawn(async move {
        if let Err(err) = run(app).await {
            tracing::warn!("agent bridge failed to start (agent control disabled): {err:#}");
        }
    });
}

#[cfg(not(unix))]
pub fn spawn(_app: AppHandle) {
    tracing::info!("agent bridge: unsupported on this platform (needs a Unix domain socket)");
}

#[cfg(unix)]
async fn run(app: AppHandle) -> Result<()> {
    let data_dir = app
        .path()
        .app_data_dir()
        .context("resolve app data dir")?;
    std::fs::create_dir_all(&data_dir).context("create app data dir")?;

    let socket_path = data_dir.join("agent-bridge.sock");
    // Stale socket from a crashed previous instance blocks bind — remove it.
    // (If another live instance holds it, bind() below fails and we disable
    // the bridge for this instance rather than fighting over the socket.)
    let _ = std::fs::remove_file(&socket_path);
    let listener = UnixListener::bind(&socket_path).context("bind agent-bridge.sock")?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&socket_path, std::fs::Permissions::from_mode(0o600))
            .context("chmod agent-bridge.sock")?;
    }

    let token = random_token();
    // The discovery file lives at a FIXED, bundle-id-independent path —
    // `~/Library/Application Support/Clipy/agent-bridge.json` — because that is
    // the contract @clipy/cli reads (macBridge.ts::bridgeFilePath). It must not
    // follow app_data_dir: that is `…/online.clipy.desktop` for stable and
    // `…/online.clipy.desktop.beta` for beta, and the CLI would find neither.
    // The socket itself stays in app_data_dir; the file carries its path.
    // Clipy Local writes `Clipy Local/agent-bridge.json`, which only
    // `clipy … --local` reads.
    let discovery_dir = discovery_dir_path(&data_dir);
    std::fs::create_dir_all(&discovery_dir).context("create bridge discovery dir")?;
    let discovery_path = discovery_dir.join("agent-bridge.json");
    let discovery = serde_json::to_string_pretty(&BridgeDiscovery {
        socket_path: &socket_path.to_string_lossy(),
        token: &token,
        pid: std::process::id(),
        app_version: env!("CARGO_PKG_VERSION"),
        protocol_version: PROTOCOL_VERSION,
        edition: EDITION,
    })?;
    write_discovery_file(&discovery_path, &discovery)?;
    tracing::info!(
        "agent bridge listening on {} (discovery: {})",
        socket_path.display(),
        discovery_path.display()
    );

    loop {
        let (stream, _addr) = match listener.accept().await {
            Ok(pair) => pair,
            Err(err) => {
                tracing::warn!("agent bridge accept error: {err}");
                continue;
            }
        };
        let app = app.clone();
        let token = token.clone();
        tauri::async_runtime::spawn(async move {
            if let Err(err) = handle_connection(app, token, stream).await {
                tracing::debug!("agent bridge connection error: {err:#}");
            }
        });
    }
}

#[cfg(unix)]
async fn handle_connection(app: AppHandle, token: String, stream: UnixStream) -> Result<()> {
    let (read_half, mut write_half) = stream.into_split();
    let mut reader = BufReader::new(read_half).take(64 * 1024); // request size cap
    let mut line = String::new();
    reader.read_line(&mut line).await.context("read request")?;

    let response = match serde_json::from_str::<BridgeRequest>(line.trim()) {
        Err(_) => err_json("bad_request", "request must be one JSON line"),
        Ok(req) => {
            // Constant-shape token check; per-launch random token.
            if req.token != token {
                err_json("unauthorized", "bad or missing bridge token")
            } else if req.cmd == "status" {
                // Read-only: must stay answerable while a stop/upload holds
                // the command lock, or health polling would hang.
                cmd_status(&app)
            } else if req.cmd == "sources" {
                // Also read-only — an agent enumerating windows must not block
                // behind an in-flight stop/upload.
                cmd_sources().await
            } else {
                // Serialize all MUTATING commands: no start/stop/mark races.
                let _guard = COMMAND_LOCK.lock().await;
                dispatch(&app, req).await
            }
        }
    };

    write_half.write_all(response.as_bytes()).await.ok();
    write_half.write_all(b"\n").await.ok();
    write_half.shutdown().await.ok();
    Ok(())
}

// ---------------------------------------------------------------------------
// Command handlers
// ---------------------------------------------------------------------------

async fn dispatch(app: &AppHandle, req: BridgeRequest) -> String {
    match req.cmd.as_str() {
        "status" => cmd_status(app),
        "sources" => cmd_sources().await,
        "start" => cmd_start(app, req).await,
        "mark" => cmd_mark(req),
        "stop" => cmd_stop(app).await,
        "abort" => cmd_abort(app).await,
        other => err_json("unknown_cmd", &format!("unknown cmd: {other}")),
    }
}

fn cmd_status(app: &AppHandle) -> String {
    let rec = app.state::<RecordingController>();
    let auth = app.state::<AuthState>();
    let dto = rec.dto();
    let session = SESSION.lock();
    let session_json = session.as_ref().map(|s| {
        serde_json::json!({
            "recordingId": s.recording_id,
            "elapsedSec": s.started.elapsed().as_secs(),
            "maxSec": s.max_sec,
            "marks": s.marks.len(),
            "audio": s.audio,
        })
    });
    ok_json(serde_json::json!({
        "appVersion": env!("CARGO_PKG_VERSION"),
        "protocolVersion": PROTOCOL_VERSION,
        "edition": EDITION,
        "phase": format!("{:?}", dto.phase),
        "signedIn": auth.bearer_token().is_some(),
        "agentSession": session_json,
    }))
}

/// Enumerate capturable displays + windows so an agent can pick one and pass
/// it back as `source` on `start`. Read-only, so it bypasses COMMAND_LOCK
/// (same reasoning as `status`: enumeration must stay answerable while a
/// stop/upload holds the lock).
async fn cmd_sources() -> String {
    match crate::commands::list_capture_sources().await {
        Ok(sources) => match serde_json::to_value(&sources) {
            Ok(value) => ok_json(value),
            Err(err) => err_json("sources_failed", &format!("serialize sources: {err}")),
        },
        Err(err) => err_json("sources_failed", &err),
    }
}

/// Clipy Local has no account: Local takes start with an empty token, exactly
/// as the start_recording command does for them.
fn agent_bearer(_app: &AppHandle) -> Result<String, String> {
    Ok(String::new())
}

/// The native checks a Local take runs before capture (screen and speech
/// permissions), the same as the start_recording command.
async fn agent_preflight(app: &AppHandle) -> Result<(), String> {
    crate::local::preflight_new_recording(app)
        .await
        .map_err(|err| err_json("start_failed", &format!("{err:#}")))
}

async fn cmd_start(app: &AppHandle, req: BridgeRequest) -> String {
    let bearer = match agent_bearer(app) {
        Ok(token) => token,
        Err(err) => return err,
    };

    match ensure_consent(app).await {
        Ok(true) => {}
        Ok(false) => return err_json("consent_denied", CONSENT_DENIED),
        Err(err) => return err_json("consent_error", &format!("{err:#}")),
    }

    let rec = app.state::<RecordingController>();
    if rec.dto().phase != RecordingPhase::Idle {
        return err_json(
            "busy",
            "a recording is already in progress (agent or human) — stop it first",
        );
    }
    if SESSION.lock().is_some() {
        return err_json("busy", "an agent session is already active");
    }

    // Reject an oversized initial notes list up front rather than silently
    // truncating it after starting the recording (live `mark`s past the cap
    // already error; keep the two paths consistent). The cap matches the
    // server's narration limit — see MAX_MARKS.
    if let Some(notes) = req.notes.as_ref() {
        if notes.len() > MAX_MARKS {
            return err_json(
                "too_many_marks",
                &format!(
                    "notes has {} entries but the per-session limit is {MAX_MARKS} — trim the list before starting",
                    notes.len()
                ),
            );
        }
    }

    let max_sec = req
        .max_sec
        .unwrap_or(DEFAULT_MAX_SEC)
        .clamp(1, HARD_CAP_SEC);

    if let Err(err) = agent_preflight(app).await {
        return err;
    }

    // `req.source` picks the display/window/area (None = the app's default,
    // primary display). Audio comes from `agent_audio_options` — the
    // agent-specific default is mic OFF, system audio ON, which is NOT the
    // app's interactive default; see that function for why. No webcam,
    // Instant mode (streams to the raw-upload pipeline + auto-shares; Local in
    // Clipy Local), no countdown: the agent wants frames now, not a 3-2-1
    // overlay.
    let audio = agent_audio_options(req.audio.as_ref());
    let recording_id = match rec
        .start(
            app,
            bearer,
            req.source.clone(),
            Some(audio.clone()),
            None,
            None,
            None,
            AGENT_RECORDING_MODE,
            0,
        )
        .await
    {
        Ok(id) => id,
        Err(err) => {
            // A quota/pro refusal reaches the agent as the server's own
            // sentence (which names the limit and the pricing URL), not as the
            // raw JSON body wrapped in uploader breadcrumbs.
            tracing::error!("agent start_recording failed: {err:#}");
            return err_json("start_failed", &crate::api_error::user_message(&err));
        }
    };
    let recording_id_str = recording_id.to_string();
    crate::whats_new::mark_tried(app, "agent");

    // Crash-durable mark journal.
    let journal_path = app
        .path()
        .app_data_dir()
        .map(|d| d.join(format!("agent-marks-{recording_id_str}.jsonl")))
        .unwrap_or_else(|_| std::env::temp_dir().join("clipy-agent-marks.jsonl"));

    let mut marks: Vec<Mark> = Vec::new();
    if let Some(notes) = req.notes {
        for n in notes.into_iter().take(MAX_MARKS) {
            let text: String = n.text.chars().take(MAX_MARK_CHARS).collect();
            if !text.trim().is_empty() {
                marks.push(Mark { start_ms: n.start_ms, text });
            }
        }
    }

    *SESSION.lock() = Some(AgentSession {
        recording_id: recording_id_str.clone(),
        started: Instant::now(),
        max_sec,
        title: req.title.clone(),
        description: req.description.clone(),
        marks,
        journal_path,
        audio: audio.clone(),
    });
    *LAST_RESULT.lock() = None;
    // Fresh session: clear any app-side-stop marker left by a previous one.
    *EXTERNAL_STOP.lock() = None;

    // Max-duration watchdog: auto-stop AND upload rather than record forever.
    let app_for_watchdog = app.clone();
    let watchdog_recording_id = recording_id_str.clone();
    tauri::async_runtime::spawn(async move {
        tokio::time::sleep(std::time::Duration::from_secs(max_sec)).await;
        let _guard = COMMAND_LOCK.lock().await;
        let still_this_session = SESSION
            .lock()
            .as_ref()
            .map(|s| s.recording_id == watchdog_recording_id)
            .unwrap_or(false);
        if !still_this_session {
            return;
        }
        // Re-verify the controller is STILL recording THIS session before
        // auto-stopping. If the user stopped it from the app and started
        // another, the controller's active id won't match — auto-stopping would
        // kill that newer, unrelated recording. Drop the stale session instead.
        let controller_matches = app_for_watchdog
            .state::<RecordingController>()
            .active_recording_id()
            .as_deref()
            == Some(watchdog_recording_id.as_str());
        if controller_matches {
            tracing::info!("agent bridge: max duration reached — auto-stopping");
            let _ = finish_session(&app_for_watchdog, true).await;
        } else {
            tracing::warn!(
                "agent bridge: watchdog fired but the app is no longer recording this session — dropping stale session"
            );
            clear_session_for(&watchdog_recording_id);
        }
    });

    // Echo the resolved audio config so the CLI can tell the user what is
    // actually being captured rather than leaving it assumed.
    let mut started = serde_json::json!({
        "recordingId": recording_id_str,
        "maxSec": max_sec,
        "state": "recording",
        "audio": audio,
    });
    if let Some(mode) = agent_window_capture(req.source.as_ref()) {
        started["windowCapture"] = serde_json::Value::from(mode);
    }
    // Where the take is being written, so an agent can read the Preparing
    // AREC even before `stop` returns.
    if AGENT_RECORDING_MODE == RecordingMode::Local {
        if let Ok(dir) = crate::local::directory(app, &recording_id_str) {
            started["local"] = serde_json::Value::Bool(true);
            started["arecPath"] = serde_json::json!(dir.join("recording.arec"));
            started["directory"] = serde_json::json!(dir);
        }
    }
    ok_json(started)
}

/// How an agent `start` records a window, for the CLI to report. Agent starts
/// pass no video options, so they take the defaults, the same as `start`.
fn agent_window_capture(source: Option<&CaptureSourceDto>) -> Option<&'static str> {
    matches!(source, Some(CaptureSourceDto::Window { .. })).then(|| {
        if crate::types::is_window_only_capture(source, &crate::types::VideoOptionsDto::default()) {
            "window"
        } else {
            "area"
        }
    })
}

fn cmd_mark(req: BridgeRequest) -> String {
    let Some(text) = req.text.as_deref().map(str::trim).filter(|t| !t.is_empty()) else {
        return err_json("bad_request", "mark needs non-empty text");
    };
    let mut guard = SESSION.lock();
    let Some(session) = guard.as_mut() else {
        return err_json("no_session", "no active agent recording session");
    };
    if session.marks.len() >= MAX_MARKS {
        return err_json(
            "too_many_marks",
            &format!(
                "mark limit reached for this session ({MAX_MARKS} max) — this mark was not recorded"
            ),
        );
    }
    let mark = Mark {
        start_ms: session.started.elapsed().as_millis() as u64,
        text: text.chars().take(MAX_MARK_CHARS).collect(),
    };
    // Journal first (crash durability), then the in-memory list. sync_data
    // per mark: marks are rare and small; losing the last one to a crash is
    // exactly what the journal exists to prevent.
    if let Ok(line) = serde_json::to_string(&mark) {
        if let Ok(mut f) = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&session.journal_path)
        {
            let _ = writeln!(f, "{line}");
            let _ = f.flush();
            let _ = f.sync_data();
        }
    }
    session.marks.push(mark.clone());
    ok_json(serde_json::json!({
        "atSeconds": (mark.start_ms as f64 / 100.0).round() / 10.0,
        "totalMarks": session.marks.len(),
    }))
}

async fn cmd_stop(app: &AppHandle) -> String {
    if SESSION.lock().is_none() {
        // The watchdog may have auto-stopped already: report that result once.
        if let Some(result) = LAST_RESULT.lock().take() {
            return ok_json(result);
        }
        if let Some(err) = external_stop_error() {
            return err;
        }
        return err_json("no_session", "no active agent recording session");
    }
    match finish_session(app, false).await {
        Ok(result) => ok_json(result),
        Err(err) => err_json("stop_failed", &format!("{err:#}")),
    }
}

async fn cmd_abort(app: &AppHandle) -> String {
    let session = SESSION.lock().take();
    let Some(session) = session else {
        if let Some(err) = external_stop_error() {
            return err;
        }
        return err_json("no_session", "no active agent recording session");
    };
    let _ = std::fs::remove_file(&session.journal_path);
    let rec = app.state::<RecordingController>();
    match rec.cancel(app).await {
        Ok(()) => ok_json(serde_json::json!({ "aborted": true })),
        Err(err) => err_json("abort_failed", &format!("{err:#}")),
    }
}

/// Take the active session for stopping. Callers hold COMMAND_LOCK.
fn take_live_session(app: &AppHandle) -> Result<AgentSession> {
    let session = SESSION
        .lock()
        .take()
        .ok_or_else(|| anyhow!("no active session"))?;

    // Only act if the controller is STILL recording this exact session. If the
    // app already stopped it (human Stop/Cancel) or a different recording is now
    // active, this session is stale — never stop a recording the agent didn't
    // start. The stop/cancel paths normally clear SESSION via clear_session_for;
    // this is the backstop that also protects the agent's own explicit stop.
    let rec = app.state::<RecordingController>();
    if rec.active_recording_id().as_deref() != Some(session.recording_id.as_str()) {
        let _ = std::fs::remove_file(&session.journal_path);
        return Err(anyhow!(STOPPED_FROM_APP));
    }
    Ok(session)
}

fn sorted_marks(marks: &[Mark]) -> Vec<Mark> {
    let mut sorted = marks.to_vec();
    sorted.sort_by_key(|m| m.start_ms);
    sorted
}

/// Clipy Local: stop the capture, hand the marks to local processing, and wait
/// for the take to be processed into its AREC. Callers hold COMMAND_LOCK.
async fn finish_session(app: &AppHandle, auto_stopped: bool) -> Result<serde_json::Value> {
    let session = take_live_session(app)?;
    let rec = app.state::<RecordingController>();
    let dir = crate::local::directory(app, &session.recording_id)?;

    // Written before stop(): processing reads it after transcription, which
    // waits for the metadata.json that stop() writes. A failed write costs the
    // narration, never the recording.
    let notes: Vec<crate::local::NarrationNote> = sorted_marks(&session.marks)
        .into_iter()
        .map(|m| crate::local::NarrationNote {
            start_ms: m.start_ms,
            text: m.text,
        })
        .collect();
    if !notes.is_empty() {
        if let Err(err) = crate::local::save_agent_marks(&dir, &notes) {
            tracing::warn!("agent marks could not be saved for local processing: {err:#}");
        }
    }

    if session.title.is_some() || session.description.is_some() {
        let details = crate::local::AgentDetails {
            title: session.title.clone(),
            description: session.description.clone(),
        };
        if let Err(err) = crate::local::save_agent_details(&dir, &details) {
            tracing::warn!("agent title and description could not be saved: {err:#}");
        }
    }

    let metadata = rec.stop(app).await.context("stop recording")?;
    let _ = std::fs::remove_file(&session.journal_path);

    let deadline = Instant::now() + std::time::Duration::from_secs(LOCAL_PROCESSING_WAIT_SECS);
    let timed_out = loop {
        if crate::local::read_status(&dir).is_some_and(|s| local_take_settled(&s.stage)) {
            break false;
        }
        if Instant::now() >= deadline {
            break true;
        }
        tokio::time::sleep(std::time::Duration::from_millis(250)).await;
    };
    let result = local_stop_result(
        &session.recording_id,
        &dir,
        crate::local::read_status(&dir).as_ref(),
        timed_out,
        auto_stopped,
        metadata.bytes,
    );
    if auto_stopped {
        *LAST_RESULT.lock() = Some(result.clone());
    }
    Ok(result)
}

/// The two stages local processing ends in. `failed` still leaves a Partial
/// AREC with whatever evidence was saved.
fn local_take_settled(stage: &str) -> bool {
    matches!(stage, "ready" | "failed")
}

/// What a local-edition `stop` returns: where the take and its AREC are, and
/// the processing stage it reached.
fn local_stop_result(
    recording_id: &str,
    dir: &Path,
    status: Option<&crate::local::Status>,
    timed_out: bool,
    auto_stopped: bool,
    size_bytes: u64,
) -> serde_json::Value {
    let arec = dir.join("recording.arec");
    let stage = status.map_or("processing", |s| s.stage.as_str());
    let message = if timed_out {
        format!(
            "Clipy Local is still processing this recording ({stage}). {} is usable now and is updated in place; read it again for the transcript and frames.",
            arec.display()
        )
    } else {
        status.map(|s| s.message.clone()).unwrap_or_default()
    };
    let mut result = serde_json::json!({
        "local": true,
        "recordingId": recording_id,
        "arecPath": arec,
        "directory": dir,
        "status": stage,
        "message": message,
        "autoStopped": auto_stopped,
        "sizeBytes": size_bytes,
    });
    if timed_out {
        result["timedOut"] = serde_json::Value::Bool(true);
    }
    result
}

// ---------------------------------------------------------------------------
// Consent
// ---------------------------------------------------------------------------

const CONSENT_MESSAGE: &str = "A local AI agent (for example Claude Code) is asking Clipy Local to record your screen.\n\nThe normal recording indicator will always be visible, recordings stop at their time limit, and only programs running as your user can ask. Recordings stay on this computer.\n\nAllow agent-triggered recordings?";
const CONSENT_TITLE: &str = "Clipy Local: allow AI agents to record?";
const CONSENT_DENIED: &str = "the user declined agent-triggered recordings (allow them again by deleting agent-consent.json in the Clipy Local app data folder)";

#[derive(Serialize, Deserialize)]
struct ConsentFile {
    status: String, // "granted" | "denied"
}

async fn ensure_consent(app: &AppHandle) -> Result<bool> {
    let path = app
        .path()
        .app_data_dir()
        .context("resolve app data dir")?
        .join("agent-consent.json");

    if let Ok(raw) = std::fs::read_to_string(&path) {
        if let Ok(consent) = serde_json::from_str::<ConsentFile>(&raw) {
            return Ok(consent.status == "granted");
        }
    }

    // Blocking dialog must not sit on the async runtime.
    let app_for_dialog = app.clone();
    let granted = tokio::task::spawn_blocking(move || {
        use tauri_plugin_dialog::{DialogExt, MessageDialogButtons, MessageDialogKind};
        app_for_dialog
            .dialog()
            .message(crate::i18n::t("agentConsent.body"))
            .title(crate::i18n::t("agentConsent.title"))
            // Info, not Warning: on macOS the Warning kind forces the generic
            // yellow caution triangle and suppresses the app icon, so the
            // dialog reads as an anonymous system alert rather than as Clipy
            // asking. Info lets the Clipy icon through.
            .kind(MessageDialogKind::Info)
            .buttons(MessageDialogButtons::OkCancelCustom(
                crate::i18n::t("agentConsent.allow"),
                crate::i18n::t("agentConsent.dontAllow"),
            ))
            .blocking_show()
    })
    .await
    .context("consent dialog task")?;

    let consent = ConsentFile {
        status: if granted { "granted" } else { "denied" }.to_string(),
    };
    if let Ok(raw) = serde_json::to_string_pretty(&consent) {
        let _ = write_discovery_file(&path, &raw);
    }
    Ok(granted)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_window_start_says_how_the_window_is_recorded() {
        let window = CaptureSourceDto::Window { id: 7 };
        let expected = if cfg!(target_os = "macos") {
            "window"
        } else {
            "area"
        };
        assert_eq!(agent_window_capture(Some(&window)), Some(expected));
        assert_eq!(
            agent_window_capture(Some(&CaptureSourceDto::Display { id: 1 })),
            None
        );
        assert_eq!(agent_window_capture(None), None);
    }

    fn parse(json: &str) -> Option<BridgeAudioDto> {
        serde_json::from_str(json).expect("audio dto parses")
    }

    #[test]
    fn absent_audio_leaves_the_mic_off() {
        let audio = agent_audio_options(None);
        assert!(!audio.include_mic, "agent recordings must not capture the mic by default");
        assert!(audio.include_system_audio);
        assert!(audio.mic_device_id.is_none());
    }

    #[test]
    fn explicit_mic_request_is_honoured() {
        let dto = parse(r#"{"includeMic":true,"micDeviceId":"MacBook Pro Microphone"}"#);
        let audio = agent_audio_options(dto.as_ref());
        assert!(audio.include_mic);
        assert!(audio.include_system_audio, "system audio stays on unless asked otherwise");
        assert_eq!(audio.mic_device_id.as_deref(), Some("MacBook Pro Microphone"));
    }

    #[test]
    fn partial_audio_object_does_not_reenable_the_mic() {
        // Flipping only system audio must not drag the mic back on via
        // AudioOptionsDto's own field defaults.
        let dto = parse(r#"{"includeSystemAudio":false}"#);
        let audio = agent_audio_options(dto.as_ref());
        assert!(!audio.include_system_audio);
        assert!(!audio.include_mic);
    }

    #[test]
    fn each_edition_has_its_own_discovery_folder() {
        let data_dir = Path::new("/Users/me/Library/Application Support/online.clipy.local");
        let expected = if crate::edition::CLOUD { "Clipy" } else { "Clipy Local" };
        assert_eq!(
            discovery_dir_path(data_dir),
            Path::new("/Users/me/Library/Application Support").join(expected)
        );
        assert_eq!(EDITION, if crate::edition::CLOUD { "cloud" } else { "local" });
        let mode = if crate::edition::CLOUD {
            RecordingMode::Instant
        } else {
            RecordingMode::Local
        };
        assert_eq!(AGENT_RECORDING_MODE, mode);
    }

    fn local_status(stage: &str, message: &str, dir: &Path) -> crate::local::Status {
        crate::local::Status {
            updated_at_ms: 1,
            id: "take".into(),
            stage: stage.into(),
            message: message.into(),
            directory: dir.into(),
            arec_path: Some(dir.join("recording.arec")),
            bytes: 0,
            kept: false,
            expires_at_ms: None,
            arec_copied: false,
        }
    }

    #[test]
    fn local_stop_reports_the_arec_path_and_stage() {
        let dir = Path::new("/tmp/local-recordings/0b9d2c55-8a8e-4f3b-9d6f-0c1b2a3d4e5f");
        let status = local_status("ready", "Ready", dir);
        let json = local_stop_result("rec-1", dir, Some(&status), false, false, 4096);
        assert_eq!(json["local"], true);
        assert_eq!(json["recordingId"], "rec-1");
        assert_eq!(json["status"], "ready");
        assert_eq!(json["message"], "Ready");
        assert_eq!(json["sizeBytes"], 4096);
        assert_eq!(json["autoStopped"], false);
        assert_eq!(
            json["arecPath"].as_str(),
            Some(dir.join("recording.arec").to_str().unwrap())
        );
        assert_eq!(json["directory"].as_str(), dir.to_str());
        assert!(json.get("timedOut").is_none());
        assert!(json.get("shareUrl").is_none(), "a local take has no share link");
    }

    #[test]
    fn a_local_stop_that_outwaits_processing_says_where_the_arec_is() {
        let dir = Path::new("/tmp/local-recordings/take");
        let status = local_status("transcribing", "Transcribing", dir);
        let json = local_stop_result("rec-2", dir, Some(&status), true, true, 1);
        assert_eq!(json["timedOut"], true);
        assert_eq!(json["status"], "transcribing");
        assert_eq!(json["autoStopped"], true);
        let message = json["message"].as_str().unwrap();
        assert!(message.contains("recording.arec"), "{message}");
        assert!(!message.contains('\u{2014}'), "no em dash in human-facing text");

        let unknown = local_stop_result("rec-3", dir, None, true, false, 1);
        assert_eq!(unknown["status"], "processing");
    }

    #[test]
    fn only_ready_and_failed_end_local_processing() {
        for stage in ["ready", "failed"] {
            assert!(local_take_settled(stage), "{stage}");
        }
        for stage in ["recording", "transcribing", "selecting", "extracting"] {
            assert!(!local_take_settled(stage), "{stage}");
        }
    }
}
