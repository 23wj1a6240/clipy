//! "What's new": the release notes and a checklist of what Clipy can do, in a
//! window of its own (`src/routes/WhatsNew.tsx`), plus the card the recorder
//! shows once after an update.
//!
//! Nothing here ever opens a window by itself. An update only makes
//! `pending` true; the popover shows its card while it is idle, and the
//! window opens from that card, the tray menu or Settings. A recording, a
//! countdown or an upload is never interrupted by any of it.
//!
//! The checklist is ticked by real use, not by visiting a page: the places
//! that start a Flare, switch the pen on, open clipboard history and so on
//! call `mark_tried`, and so does the window's own practice area.

use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};

use serde::Serialize;
use tauri::{AppHandle, Emitter, Manager, Runtime, WebviewUrl, WebviewWindowBuilder};
use tauri_plugin_store::StoreExt;

const LABEL: &str = "whats-new";
/// The last version whose news the user has seen (card closed, or the window
/// opened). An update to a newer version makes news pending again.
const SEEN_KEY: &str = "whats_new_seen_version";
/// Features the user has used at least once, by id.
const TRIED_KEY: &str = "features_tried";
/// Sent with the full state whenever it changes, to the popover and the
/// window alike.
const EVENT_CHANGED: &str = "whats-new-changed";
/// Sent to an open window that was asked for a specific feature.
const EVENT_FOCUS: &str = "whats-new-focus";
/// Dev and test override: treat this version as the one seen before the
/// update, so the card and the "new" list can be checked without a release.
const FROM_ENV: &str = "CLIPY_DESKTOP_WHATS_NEW_FROM";

/// Every id the checklist knows. `src/lib/whatsNew.ts` has the same list (a
/// test compares them); an id outside it is refused, so a typo in the page
/// cannot write junk into settings.json.
pub const FEATURE_IDS: [&str; 10] = [
    "flare",
    "pointer",
    "draw",
    "clipboard",
    "screenshot_markup",
    "screenshot_style",
    "window_recording",
    "custom_size",
    "local_ai",
    "agent",
];

/// One bit per entry of `FEATURE_IDS`. Read on hot paths (the cursor-trail
/// thread checks it on every Flare), so marking an already-tried feature
/// never touches the store.
static TRIED: AtomicU32 = AtomicU32::new(0);
/// Serializes writes of the tried list, so a slow write cannot land after a
/// newer one and drop a feature.
static PERSIST: std::sync::Mutex<()> = std::sync::Mutex::new(());
/// Dev builds share settings.json with the release app (see onboarding.rs),
/// so they never write the seen version; closing the card there lasts for
/// the session only.
static ACKED_THIS_SESSION: AtomicBool = AtomicBool::new(false);
/// The card's "Later": hidden until the next launch, and never written, so
/// the news is still waiting then. Kept here rather than in the card,
/// because the recorder unmounts the card for every recording.
static SNOOZED_THIS_SESSION: AtomicBool = AtomicBool::new(false);
const PERSIST_SEEN: bool = !cfg!(debug_assertions);

/// What launch found, taken once before anything is written: opening the
/// window marks the news as seen, and the window still has to know which
/// version this session updated from.
static LAUNCH: std::sync::OnceLock<LaunchNews> = std::sync::OnceLock::new();

#[derive(Clone, Debug, PartialEq, Eq)]
struct LaunchNews {
    /// This launch is the first since an update.
    updated: bool,
    /// The version seen before. None when not known: an install that updated
    /// from a release older than this feature.
    previous: Option<String>,
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct WhatsNewStateDto {
    /// The running app's version.
    pub current_version: String,
    /// The version this session updated from, when it is known.
    pub previous_version: Option<String>,
    /// This session is the first since an update (stays true after the news
    /// is seen, so the window can still list it).
    pub updated: bool,
    /// The update's news has not been seen yet: the recorder shows its card.
    pub pending: bool,
    /// Feature ids used at least once, in `FEATURE_IDS` order.
    pub tried: Vec<String>,
}

fn bit(id: &str) -> Option<u32> {
    FEATURE_IDS.iter().position(|known| *known == id).map(|i| 1u32 << i)
}

fn tried_ids(mask: u32) -> Vec<String> {
    FEATURE_IDS
        .iter()
        .enumerate()
        .filter(|(i, _)| mask & (1 << i) != 0)
        .map(|(_, id)| (*id).to_string())
        .collect()
}

/// Release versions only ever compare by their numbers; a suffix such as
/// `-dev-20260916` is ignored. Missing parts read as 0.
fn version_parts(version: &str) -> Vec<u64> {
    let core = version.trim().trim_start_matches('v');
    let core = core.split(['-', '+']).next().unwrap_or("");
    core.split('.').map(|part| part.parse::<u64>().unwrap_or(0)).collect()
}

pub(crate) fn version_lt(a: &str, b: &str) -> bool {
    let (a, b) = (version_parts(a), version_parts(b));
    let len = a.len().max(b.len());
    for i in 0..len {
        let (x, y) = (a.get(i).copied().unwrap_or(0), b.get(i).copied().unwrap_or(0));
        if x != y {
            return x < y;
        }
    }
    false
}

/// Whether this launch follows an update, from what settings say.
///
/// No seen version means one of two things. A fresh install (setup not
/// finished) has no news; `init` stores the current version for it. An
/// install that finished setup updated from a release older than this
/// feature, which is exactly someone with news waiting.
fn launch_news(
    seen: Option<&str>,
    current: &str,
    from_override: Option<&str>,
    onboarded: bool,
) -> LaunchNews {
    if let Some(from) = from_override.map(str::trim).filter(|v| !v.is_empty()) {
        return LaunchNews { updated: true, previous: Some(from.to_string()) };
    }
    match seen {
        Some(seen) => LaunchNews {
            updated: version_lt(seen, current),
            previous: Some(seen.to_string()),
        },
        None => LaunchNews { updated: onboarded, previous: None },
    }
}

fn read_launch_news<R: Runtime>(app: &AppHandle<R>) -> LaunchNews {
    launch_news(
        seen_version(app).as_deref(),
        &current_version(app),
        std::env::var(FROM_ENV).ok().as_deref(),
        crate::onboarding::is_completed(app),
    )
}

fn current_version<R: Runtime>(app: &AppHandle<R>) -> String {
    app.package_info().version.to_string()
}

fn seen_version<R: Runtime>(app: &AppHandle<R>) -> Option<String> {
    app.store(crate::commands::SETTINGS_STORE)
        .ok()
        .and_then(|store| store.get(SEEN_KEY))
        .and_then(|value| value.as_str().map(str::to_string))
}

pub fn state<R: Runtime>(app: &AppHandle<R>) -> WhatsNewStateDto {
    let launch = LAUNCH.get_or_init(|| read_launch_news(app));
    WhatsNewStateDto {
        current_version: current_version(app),
        previous_version: launch.previous.clone(),
        updated: launch.updated,
        pending: launch.updated
            && !ACKED_THIS_SESSION.load(Ordering::SeqCst)
            && !SNOOZED_THIS_SESSION.load(Ordering::SeqCst),
        tried: tried_ids(TRIED.load(Ordering::SeqCst)),
    }
}

fn emit_changed<R: Runtime>(app: &AppHandle<R>) {
    let _ = app.emit(EVENT_CHANGED, state(app));
}

fn write_seen<R: Runtime>(app: &AppHandle<R>, version: &str) -> Result<(), String> {
    let store = app
        .store(crate::commands::SETTINGS_STORE)
        .map_err(|e| e.to_string())?;
    let previous = store.get(SEEN_KEY);
    store.set(SEEN_KEY, serde_json::json!(version));
    store.save().map_err(|e| {
        // Put the cache back, or the next save of any other setting would
        // persist what was just reported as unsaved.
        match previous {
            Some(value) => store.set(SEEN_KEY, value),
            None => {
                store.delete(SEEN_KEY);
            }
        }
        e.to_string()
    })
}

/// Called once from `setup()`: loads the tried list, and records a fresh
/// install as having seen this version, so a first run gets setup and never
/// an update card. An install that finished setup but has no seen version
/// updated from a release before this feature; it keeps no value, which
/// reads as news pending.
pub fn init(app: &AppHandle) {
    if let Some(list) = app
        .store(crate::commands::SETTINGS_STORE)
        .ok()
        .and_then(|store| store.get(TRIED_KEY))
    {
        let mask = list
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|v| v.as_str())
            .filter_map(bit)
            .fold(0, |acc, b| acc | b);
        TRIED.fetch_or(mask, Ordering::SeqCst);
    }
    let launch = LAUNCH.get_or_init(|| read_launch_news(app));
    tracing::info!(updated = launch.updated, previous = ?launch.previous, "what's new at launch");
    if PERSIST_SEEN && seen_version(app).is_none() && !crate::onboarding::is_completed(app) {
        if let Err(err) = write_seen(app, &current_version(app)) {
            tracing::warn!("could not record the first-run version: {err}");
        }
    }
}

/// Records that a feature was used. Cheap and safe from any thread: an
/// already-tried feature is one atomic read, and the first use is written in
/// the background, never on the caller's thread (the recording start path
/// and the cursor-trail thread both call this).
pub fn mark_tried<R: Runtime>(app: &AppHandle<R>, id: &str) {
    let Some(bit) = bit(id) else {
        tracing::warn!("unknown feature id: {id}");
        return;
    };
    if TRIED.fetch_or(bit, Ordering::SeqCst) & bit != 0 {
        return;
    }
    let app = app.clone();
    tauri::async_runtime::spawn_blocking(move || {
        {
            let _guard = PERSIST.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
            // Written from the bits at write time, under the lock, so the
            // last write always carries every feature marked before it.
            let ids = tried_ids(TRIED.load(Ordering::SeqCst));
            match app.store(crate::commands::SETTINGS_STORE) {
                Ok(store) => {
                    store.set(TRIED_KEY, serde_json::json!(ids));
                    if let Err(err) = store.save() {
                        tracing::warn!("could not save tried features: {err}");
                    }
                }
                Err(err) => tracing::warn!("could not open settings: {err}"),
            }
        }
        emit_changed(&app);
    });
}

#[tauri::command]
pub fn get_whats_new_state(app: AppHandle) -> WhatsNewStateDto {
    state(&app)
}

/// The news for this version has been seen: the card's close button, or the
/// window opening. A failed save still hides the card for this session.
#[tauri::command]
pub fn acknowledge_whats_new(app: AppHandle) -> Result<(), String> {
    acknowledge(&app)
}

fn acknowledge<R: Runtime>(app: &AppHandle<R>) -> Result<(), String> {
    ACKED_THIS_SESSION.store(true, Ordering::SeqCst);
    let result = if PERSIST_SEEN {
        write_seen(app, &current_version(app))
    } else {
        Ok(())
    };
    emit_changed(app);
    result.map_err(|e| format!("Clipy couldn't save that: {e}"))
}

/// "Later" on the card: hide it until the next launch without marking the
/// update as seen.
#[tauri::command]
pub fn snooze_whats_new(app: AppHandle) {
    SNOOZED_THIS_SESSION.store(true, Ordering::SeqCst);
    emit_changed(&app);
}

#[tauri::command]
pub fn mark_feature_tried(app: AppHandle, feature: String) -> Result<(), String> {
    if bit(&feature).is_none() {
        return Err(format!("unknown feature: {feature}"));
    }
    mark_tried(&app, &feature);
    Ok(())
}

/// Opens the window, on a feature when one is named. Async on purpose, like
/// setup: building a window from a sync command runs on the main thread and
/// deadlocks on Windows.
#[tauri::command]
pub async fn show_whats_new_window(app: AppHandle, feature: Option<String>) -> Result<(), String> {
    open_window(app, feature)
}

/// The tray menu's entry. Spawned: building a window must not run on the
/// main thread (see `show_whats_new_window`).
pub fn open_from_menu<R: Runtime>(app: &AppHandle<R>) {
    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        if let Err(err) = open_window(app, None) {
            tracing::warn!("open What's new: {err}");
        }
    });
}

fn open_window<R: Runtime>(app: AppHandle<R>, feature: Option<String>) -> Result<(), String> {
    // Nothing comes forward over a live capture (the same rule as the
    // popover). Every entry point is idle-only already; this is the backstop.
    if crate::tray::capture_in_progress(&app) {
        return Err("Finish the recording first.".into());
    }
    let feature = feature.filter(|id| bit(id).is_some());
    show_or_build(&app, feature)?;
    // Seeing the window is seeing the news, so this comes only after it is
    // on screen: a window that failed to open must leave the card in place.
    // Nothing to write when there was no news. Not `pending`: news hidden
    // with Later is still unseen, and opening the window does show it.
    let launch = LAUNCH.get_or_init(|| read_launch_news(&app));
    if launch.updated && !ACKED_THIS_SESSION.load(Ordering::SeqCst) {
        if let Err(err) = acknowledge(&app) {
            tracing::warn!("{err}");
        }
    }
    Ok(())
}

fn show_or_build<R: Runtime>(app: &AppHandle<R>, feature: Option<String>) -> Result<(), String> {
    if let Some(window) = app.get_webview_window(LABEL) {
        window.show().map_err(|e| e.to_string())?;
        window.unminimize().ok();
        window.set_focus().ok();
        if let Some(feature) = feature {
            let _ = app.emit_to(LABEL, EVENT_FOCUS, feature);
        }
        return Ok(());
    }
    let route = match &feature {
        Some(id) => format!("index.html#whats-new/{id}"),
        None => "index.html#whats-new".to_string(),
    };
    let builder = WebviewWindowBuilder::new(app, LABEL, WebviewUrl::App(route.into()))
        .title(crate::i18n::t("windowTitles.whatsNew"))
        .inner_size(860.0, 600.0)
        .resizable(false)
        .maximizable(false)
        .center();
    // The same full-height layout as setup: the page leaves room for the
    // traffic lights and makes its sidebar a drag region.
    #[cfg(target_os = "macos")]
    let builder = builder
        .title_bar_style(tauri::TitleBarStyle::Overlay)
        .hidden_title(true);
    match builder.build() {
        Ok(window) => {
            window.set_focus().ok();
            Ok(())
        }
        // Two callers raced to create it: the other one won, use that.
        Err(err) => match app.get_webview_window(LABEL) {
            Some(window) => {
                window.show().map_err(|e| e.to_string())?;
                window.set_focus().ok();
                Ok(())
            }
            None => Err(format!("open What's new: {err}")),
        },
    }
}

/// Shows the recorder popover from the window ("Open the recorder"), the
/// way a click on the tray icon would.
#[tauri::command]
pub fn show_recorder_from_whats_new(app: AppHandle) {
    crate::tray::show_main_raw(&app);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn versions_compare_by_number_not_text() {
        assert!(version_lt("0.1.9", "0.1.10"));
        assert!(version_lt("0.1.74", "0.1.75"));
        assert!(!version_lt("0.1.75", "0.1.75"));
        assert!(!version_lt("0.2.0", "0.1.99"));
        assert!(version_lt("0.1", "0.1.1"));
        // A dev or beta suffix does not make a version newer or older.
        assert!(!version_lt("0.1.64-dev-20260916171917", "0.1.64"));
        assert!(version_lt("v0.1.63", "0.1.64-dev-1"));
    }

    fn news(seen: Option<&str>, from: Option<&str>, onboarded: bool) -> (bool, Option<String>) {
        let n = launch_news(seen, "0.1.75", from, onboarded);
        (n.updated, n.previous)
    }

    #[test]
    fn news_follows_an_update_only() {
        assert_eq!(news(Some("0.1.74"), None, true), (true, Some("0.1.74".into())));
        assert_eq!(news(Some("0.1.75"), None, true), (false, Some("0.1.75".into())));
        // A downgrade has nothing new to tell.
        assert_eq!(news(Some("0.1.80"), None, true), (false, Some("0.1.80".into())));
    }

    #[test]
    fn no_seen_version_is_news_only_for_a_finished_setup() {
        // Updated from a release older than this feature.
        assert_eq!(news(None, None, true), (true, None));
        // A fresh install: setup comes first, never an update card.
        assert_eq!(news(None, None, false), (false, None));
    }

    #[test]
    fn the_override_always_has_news() {
        assert_eq!(news(Some("0.1.75"), Some("0.1.71"), true), (true, Some("0.1.71".into())));
        assert_eq!(news(None, Some(" 0.1.71 "), false), (true, Some("0.1.71".into())));
        assert_eq!(news(Some("0.1.75"), Some("  "), true), (false, Some("0.1.75".into())));
    }

    #[test]
    fn only_known_features_have_a_bit() {
        assert_eq!(bit("flare"), Some(1));
        assert_eq!(bit("agent"), Some(1 << 9));
        assert_eq!(bit("nope"), None);
        assert_eq!(tried_ids(0b10_0000_0101), vec!["flare", "draw", "agent"]);
    }
}
