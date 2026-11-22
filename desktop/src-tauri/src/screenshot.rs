//! Screenshots (the recorder's second mode): the whole main display, or a
//! drag-to-select area. The PNG lands in Pictures, goes on the clipboard, and
//! — when signed in — is uploaded to `/api/screenshots` so a share link is on
//! the clipboard instead.
//!
//! By default the capture opens in the annotation window first (the
//! `#screenshot-editor` route, which reuses the web editor's drawing code), and
//! Copy image or Upload delivers it, so what lands on the clipboard is the marked
//! up image rather than the raw one. The window stays open afterwards, showing
//! what was copied until the user closes it. Settings → Preferences turns the
//! editor off and restores the instant copy.
//!
//! macOS's own `screencapture` does the capture there, including its system
//! crosshair. Windows has no such tool, so `screenshot_win` reads the pixels
//! and the app's existing area-select overlay provides the same drag gesture.

use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};
use tauri::{AppHandle, Emitter, Manager, WebviewUrl, WebviewWindowBuilder};

/// The annotation window. One at a time: while it is open a new screenshot is
/// refused (see `EDITOR_BUSY`) rather than replacing what is being drawn on.
const EDITOR_LABEL: &str = "screenshot-editor";
/// `capture_screenshot`'s error while the annotation window is open. The
/// popover matches on it to explain instead of reporting a failure.
const EDITOR_BUSY: &str = "screenshot-editor-open";
/// Settings-store key for "open the editor after a screenshot".
pub(crate) const SCREENSHOT_EDITOR_KEY: &str = "screenshot_editor";
/// Sent to every window when an edited screenshot is copied, shared, or
/// deleted, so the popover can show the result. Payload: `Option<ScreenshotDto>`,
/// null for a deleted screenshot.
const EVENT_SCREENSHOT_FINISHED: &str = "screenshot-finished";
/// An annotated export is a PNG of at most the captured size; this is only a
/// guard against a runaway body, well above a 6K display's PNG.
const MAX_EDITED_BYTES: usize = 200 * 1024 * 1024;
const PNG_MAGIC: &[u8] = b"\x89PNG\r\n\x1a\n";
/// The server's own cap on stored annotations (`MAX_SCENE_BYTES` in
/// lib/screenshots/scene.ts).
const MAX_SCENE_BYTES: usize = 512 * 1024;

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum ScreenshotMode {
    /// The whole main display.
    Display,
    /// Drag to select.
    Area,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ScreenshotDto {
    pub path: String,
    pub share_url: Option<String>,
    /// What ended up on the clipboard: the image, the share link, or nothing.
    pub clipboard: &'static str,
    /// Set when the upload was attempted and failed; the file is still saved.
    pub upload_error: Option<String>,
    /// The upload replaced the image behind a link this screenshot already
    /// had, rather than creating one.
    pub share_updated: bool,
    /// The capture went to the annotation window instead: nothing was copied
    /// or uploaded yet, and `screenshot-finished` reports the outcome.
    pub editing: bool,
}

/// A capture waiting in the annotation window. The webview never names a
/// file: it asks for "the pending screenshot" and hands back pixels, so the
/// page cannot read or overwrite anything outside what it was given.
struct PendingScreenshot {
    id: u64,
    path: PathBuf,
    /// What ⌘↵ does: upload when the capture would have been shared without
    /// the editor (signed in, cloud mode), copy the image otherwise.
    upload_by_default: bool,
    /// Copy image or Upload has already put this capture on the clipboard.
    /// Closing the window then only closes it: copying again would replace a
    /// share link the user just got with the image.
    delivered: bool,
    /// The capture as taken, kept once an edit is written over the file.
    /// It is what a first Upload sends, so the share keeps an unmarked
    /// original and the web editor reopens the drawing as movable shapes.
    pristine: Option<Vec<u8>>,
    /// The annotations behind the last edit, staged by the window before an
    /// Upload. They travel with the render so the share stays editable.
    scene: Option<String>,
    /// The link this capture already has. Every later Upload updates it in
    /// place, so one screenshot never turns into several shares.
    shared: Option<SharedScreenshot>,
}

#[derive(Clone, Debug)]
struct SharedScreenshot {
    public_id: String,
    share_url: String,
    /// The user has been given this link. False only for a link left
    /// behind by a first upload whose annotations failed and could not be
    /// rolled back; the next Upload then announces it as new.
    announced: bool,
}

/// The local edition never uploads a screenshot; Copy and Save stay.
fn can_upload(_app: &AppHandle) -> bool {
    false
}

static PENDING: Mutex<Option<PendingScreenshot>> = Mutex::new(None);
static NEXT_ID: AtomicU64 = AtomicU64::new(1);

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PendingScreenshotDto {
    pub id: u64,
    pub upload_by_default: bool,
    pub can_upload: bool,
    /// What a reloaded window needs to carry on where it was: whether closing
    /// still has to copy, whether Upload updates a link, and the drawing so
    /// far (over the source, which is the capture as taken).
    pub delivered: bool,
    pub linked: bool,
    pub edit_written: bool,
    pub scene: Option<String>,
}

/// What the annotation window's finish button asked for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum FinishAction {
    /// Copy the image. Never creates a share link.
    Copy,
    /// Upload to `/api/screenshots` and copy the link.
    Upload,
}

impl FinishAction {
    fn from_header(value: Option<&str>) -> Result<Self, String> {
        match value {
            Some("copy") => Ok(Self::Copy),
            Some("upload") => Ok(Self::Upload),
            _ => Err("missing screenshot action".into()),
        }
    }
}

/// Whether a screenshot opens in the annotation window first. On by default:
/// annotating before sharing is the reason the window exists, and the setting
/// is one switch away for anyone who wants the instant copy back.
pub(crate) fn screenshot_editor_enabled(app: &AppHandle) -> bool {
    use tauri_plugin_store::StoreExt;
    app.store(crate::commands::SETTINGS_STORE)
        .ok()
        .and_then(|s| s.get(SCREENSHOT_EDITOR_KEY))
        .and_then(|v| v.as_bool())
        .unwrap_or(true)
}

#[tauri::command]
pub fn update_screenshot_editor(app: AppHandle, enabled: bool) -> Result<(), String> {
    use tauri_plugin_store::StoreExt;
    let store = app
        .store(crate::commands::SETTINGS_STORE)
        .map_err(|e| e.to_string())?;
    store.set(SCREENSHOT_EDITOR_KEY, serde_json::json!(enabled));
    store.save().map_err(|e| e.to_string())?;
    Ok(())
}

fn screenshots_dir(app: &AppHandle) -> Result<PathBuf, String> {
    let base = app
        .path()
        .picture_dir()
        .or_else(|_| app.path().app_data_dir())
        .map_err(|e| format!("resolve screenshots folder: {e}"))?;
    let dir = base.join("Clipy Screenshots");
    std::fs::create_dir_all(&dir).map_err(|e| format!("create screenshots folder: {e}"))?;
    Ok(dir)
}

fn copy_image_to_clipboard(path: &std::path::Path) -> Result<(), String> {
    #[cfg(target_os = "macos")]
    {
        let script = format!(
            "set the clipboard to (read (POSIX file \"{}\") as «class PNGf»)",
            path.to_string_lossy().replace('"', "\\\"")
        );
        let status = std::process::Command::new("/usr/bin/osascript")
            .arg("-e")
            .arg(script)
            .status()
            .map_err(|e| format!("clipboard: {e}"))?;
        if !status.success() {
            return Err("Could not copy the screenshot to the clipboard".into());
        }
        Ok(())
    }
    #[cfg(target_os = "windows")]
    {
        crate::screenshot_win::copy_image_to_clipboard(path)
    }
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    {
        let _ = path;
        Err("Screenshots are available on macOS and Windows".into())
    }
}

/// Write the requested capture to `path`. Returns `Err("cancelled")` when the
/// user backs out of an area selection.
#[cfg(target_os = "macos")]
async fn capture_to_file(
    _app: &AppHandle,
    mode: ScreenshotMode,
    path: &std::path::Path,
    _display_id: Option<u32>,
) -> Result<(), String> {
    let mut command = tokio::process::Command::new("/usr/sbin/screencapture");
    command.arg("-x");
    if mode == ScreenshotMode::Display {
        // Main display only: without `-m` a multi-monitor Mac writes one
        // numbered file per display and only the first is tracked.
        command.arg("-m");
    }
    if mode == ScreenshotMode::Area {
        // Interactive selection; `-s` skips window mode so a drag is the
        // only gesture, like a "Custom size" recording.
        command.arg("-i").arg("-s");
    }
    command.arg(path);
    let status = command
        .status()
        .await
        .map_err(|e| format!("screencapture: {e}"))?;
    if !status.success() || !path.exists() {
        // Esc during an interactive selection exits 1 with no file.
        let _ = std::fs::remove_file(path);
        return Err("cancelled".into());
    }
    Ok(())
}

#[cfg(target_os = "windows")]
async fn capture_to_file(
    app: &AppHandle,
    mode: ScreenshotMode,
    path: &std::path::Path,
    display_id: Option<u32>,
) -> Result<(), String> {
    // Which display: the one the popover asked for (its picker's display),
    // else the one under the cursor, as the clipboard picker does. Always
    // taking the primary display meant neither mode could reach a second
    // screen; it is now only the last fallback. Physical pixels throughout,
    // the same space `capture_region_to_png` and the area overlay use.
    let bounds = display_id
        .and_then(crate::capture::windows::display_bounds)
        .map(|(x, y, w, h)| (tauri::PhysicalPosition::new(x, y), tauri::PhysicalSize::new(w, h)))
        .or_else(|| {
            app.cursor_position()
                .ok()
                .and_then(|p| app.monitor_from_point(p.x, p.y).ok().flatten())
                .or_else(|| app.primary_monitor().ok().flatten())
                .map(|monitor| (*monitor.position(), *monitor.size()))
        });
    let (origin, size) = bounds.ok_or_else(|| "no display to capture".to_string())?;

    let region = match mode {
        ScreenshotMode::Display => (origin.x, origin.y, size.width as i32, size.height as i32),
        ScreenshotMode::Area => {
            // Windows places the overlay and reports the selection in the
            // display's PHYSICAL pixels (see `commands::show_area_select` and
            // `complete_area_select`), the same space as the monitor bounds.
            let display = crate::types::SourceHighlightRectDto {
                x: origin.x as f64,
                y: origin.y as f64,
                width: size.width as f64,
                height: size.height as f64,
            };
            let selection = crate::commands::request_area_selection(
                app,
                display,
                crate::types::AreaSelectPurposeDto::Screenshot,
            )
                .await?
                .ok_or_else(|| "cancelled".to_string())?;
            // Let the overlay leave the screen before the pixels are read.
            tokio::time::sleep(std::time::Duration::from_millis(160)).await;
            (
                origin.x + selection.x.round() as i32,
                origin.y + selection.y.round() as i32,
                selection.width.round() as i32,
                selection.height.round() as i32,
            )
        }
    };

    crate::screenshot_win::capture_region_to_png(region.0, region.1, region.2, region.3, path)
}

#[cfg(not(any(target_os = "macos", target_os = "windows")))]
async fn capture_to_file(
    _app: &AppHandle,
    _mode: ScreenshotMode,
    _path: &std::path::Path,
    _display_id: Option<u32>,
) -> Result<(), String> {
    Err("Screenshots are available on macOS and Windows".into())
}

/// Take a screenshot. The recorder popover hides for the capture so it is
/// never in the picture, and comes back afterwards.
#[tauri::command]
pub async fn capture_screenshot(
    app: AppHandle,
    mode: ScreenshotMode,
    upload: bool,
    display_id: Option<u32>,
) -> Result<ScreenshotDto, String> {
    // One screenshot in the editor at a time. Replacing it would throw away
    // whatever was drawn on it, and taking a second capture with the editor
    // up would put the editor itself in the picture. Bring it forward instead.
    if let Some(editor) = app.get_webview_window(EDITOR_LABEL) {
        let _ = editor.show();
        let _ = editor.unminimize();
        let _ = editor.set_focus();
        return Err(EDITOR_BUSY.into());
    }

    let dir = screenshots_dir(&app)?;
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let path = dir.join(format!("Clipy Screenshot {secs}.png"));

    let main = app.get_webview_window("main");
    let was_visible = main
        .as_ref()
        .and_then(|w| w.is_visible().ok())
        .unwrap_or(false);
    if let Some(window) = &main {
        let _ = window.hide();
    }
    // Let the popover's fade-out finish before the display is read.
    tokio::time::sleep(std::time::Duration::from_millis(220)).await;

    let captured = capture_to_file(&app, mode, &path, display_id).await;

    let edit = captured.is_ok() && screenshot_editor_enabled(&app);
    // The editor takes focus next; bringing the popover back over it would
    // only have it close again the moment the editor opens.
    if was_visible && !edit {
        if let Some(window) = &main {
            let _ = window.show();
            let _ = window.set_focus();
        }
    }
    captured?;

    if edit {
        match open_editor(&app, path.clone(), upload).await {
            Ok(()) => {
                return Ok(ScreenshotDto {
                    path: path.to_string_lossy().into_owned(),
                    share_url: None,
                    clipboard: "none",
                    upload_error: None,
                    share_updated: false,
                    editing: true,
                })
            }
            // A window that will not open must not cost the screenshot: fall
            // through to the instant copy, exactly as with the editor off.
            Err(error) => {
                tracing::warn!("screenshot editor did not open, copying directly: {error}");
                if was_visible {
                    if let Some(window) = &main {
                        let _ = window.show();
                        let _ = window.set_focus();
                    }
                }
            }
        }
    }

    Ok(deliver(&app, &path, upload).await)
}

/// Uploads (when asked) and fills the clipboard: the share link when there is
/// one, the image otherwise.
async fn deliver(app: &AppHandle, path: &std::path::Path, upload: bool) -> ScreenshotDto {
    let mut share_url = None;
    let mut upload_error = None;
    let mut clipboard = "none";
    if upload {
        match upload_screenshot(app, path, "Screenshot").await {
            Ok(url) => {
                use tauri_plugin_clipboard_manager::ClipboardExt;
                if app.clipboard().write_text(url.clone()).is_ok() {
                    clipboard = "link";
                }
                share_url = Some(url);
            }
            Err(err) => upload_error = Some(err),
        }
    }
    if clipboard == "none" && copy_image_to_clipboard(path).is_ok() {
        clipboard = "image";
    }
    ScreenshotDto {
        path: path.to_string_lossy().into_owned(),
        share_url,
        clipboard,
        upload_error,
        share_updated: false,
        editing: false,
    }
}

/// Hands the capture to the annotation window. The caller has already
/// refused to capture while one is open.
async fn open_editor(
    app: &AppHandle,
    path: PathBuf,
    upload_by_default: bool,
) -> Result<(), String> {
    let id = NEXT_ID.fetch_add(1, Ordering::Relaxed);
    // A leftover entry here belongs to a window that is gone (it closed
    // before its screenshot loaded); nothing can finish it any more.
    *PENDING.lock() = Some(PendingScreenshot {
        id,
        path,
        upload_by_default,
        delivered: false,
        pristine: None,
        scene: None,
        shared: None,
    });

    let built = WebviewWindowBuilder::new(
        app,
        EDITOR_LABEL,
        WebviewUrl::App("index.html#screenshot-editor".into()),
    )
    .title(crate::i18n::t("windowTitles.annotate"))
    .inner_size(1120.0, 780.0)
    .min_inner_size(720.0, 520.0)
    .center()
    .build();
    match built {
        Ok(window) => {
            window.set_focus().ok();
            crate::whats_new::mark_tried(app, "screenshot_markup");
            Ok(())
        }
        Err(error) => {
            take_pending(id);
            Err(format!("open screenshot editor: {error}"))
        }
    }
}

fn pending_matching(id: u64) -> Result<PathBuf, String> {
    let pending = PENDING.lock();
    match pending.as_ref() {
        Some(shot) if shot.id == id => Ok(shot.path.clone()),
        _ => Err(ALREADY_FINISHED.into()),
    }
}

fn pending_pristine(id: u64) -> Result<Option<Vec<u8>>, String> {
    let pending = PENDING.lock();
    match pending.as_ref() {
        Some(shot) if shot.id == id => Ok(shot.pristine.clone()),
        _ => Err(ALREADY_FINISHED.into()),
    }
}

const ALREADY_FINISHED: &str = "This screenshot was already deleted or closed.";

/// Claims the pending screenshot for a Done or Delete. Compare and take under
/// one lock, so two presses of Done cannot both upload it and a Delete cannot
/// clear an entry that is not its own.
fn take_pending(id: u64) -> Option<PendingScreenshot> {
    let mut pending = PENDING.lock();
    if pending.as_ref().is_some_and(|shot| shot.id == id) {
        pending.take()
    } else {
        None
    }
}

/// Puts a claimed screenshot back: after a Done that failed before anything
/// was copied, so the user can press Done again, or after one that succeeded,
/// since the window stays open for another Copy or Upload.
fn restore_pending(shot: PendingScreenshot) {
    let mut pending = PENDING.lock();
    if pending.is_none() {
        *pending = Some(shot);
    }
}

fn close_editor(app: &AppHandle) {
    if let Some(window) = app.get_webview_window(EDITOR_LABEL) {
        // destroy, not close: the page intercepts close requests to finish
        // the screenshot, and this is that finish.
        let _ = window.destroy();
    }
}

/// What the annotation window is showing, or null when it has nothing (the
/// screenshot was already finished, or the window outlived a restart).
#[tauri::command]
pub fn screenshot_editor_pending(app: AppHandle) -> Option<PendingScreenshotDto> {
    pending_dto(can_upload(&app))
}

fn pending_dto(can_upload: bool) -> Option<PendingScreenshotDto> {
    PENDING.lock().as_ref().map(|shot| PendingScreenshotDto {
        id: shot.id,
        upload_by_default: shot.upload_by_default,
        can_upload,
        delivered: shot.delivered,
        linked: shot.shared.as_ref().is_some_and(|s| s.announced),
        edit_written: shot.pristine.is_some(),
        scene: shot.scene.clone(),
    })
}

/// The pending capture's PNG bytes, returned raw rather than as a JSON array.
#[tauri::command]
pub async fn screenshot_editor_source(id: u64) -> Result<tauri::ipc::Response, String> {
    // Once an edit is written the file holds the flattened render; drawing
    // over that again would bake the old marks in under the new ones.
    if let Some(pristine) = pending_pristine(id)? {
        return Ok(tauri::ipc::Response::new(pristine));
    }
    let path = pending_matching(id)?;
    tokio::fs::read(&path)
        .await
        .map(tauri::ipc::Response::new)
        .map_err(|e| format!("read screenshot: {e}"))
}

/// Copy image or Upload in the annotation window. The body is the annotated
/// PNG, or empty when nothing was drawn (the capture on disk is then used as
/// it is). The id and the action ride in `x-clipy-screenshot-id` and
/// `x-clipy-screenshot-action` headers because a raw body leaves no room for
/// arguments.
#[tauri::command]
pub async fn finish_screenshot(
    app: AppHandle,
    request: tauri::ipc::Request<'_>,
) -> Result<ScreenshotDto, String> {
    let id = request
        .headers()
        .get("x-clipy-screenshot-id")
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.parse::<u64>().ok())
        .ok_or_else(|| "missing screenshot id".to_string())?;
    let action = FinishAction::from_header(
        request
            .headers()
            .get("x-clipy-screenshot-action")
            .and_then(|value| value.to_str().ok()),
    )?;
    let mut shot = take_pending(id).ok_or_else(|| ALREADY_FINISHED.to_string())?;
    // Checked before anything is written so a refused Upload leaves the
    // capture pending and the drawing on screen.
    if action == FinishAction::Upload && !can_upload(&app) {
        restore_pending(shot);
        return Err("Sign in to Clipy to upload screenshots.".into());
    }

    let edited: &[u8] = match request.body() {
        tauri::ipc::InvokeBody::Raw(bytes) => bytes.as_slice(),
        tauri::ipc::InvokeBody::Json(_) => &[],
    };
    if !edited.is_empty() {
        let written = match validate_png(edited) {
            Ok(()) => {
                // Before the first edit the file IS the capture, and it is
                // about to be overwritten, so it must be read now or never.
                let original = match &shot.pristine {
                    Some(pristine) => Ok(pristine.clone()),
                    None => match tokio::fs::read(&shot.path).await {
                        Ok(bytes) if !bytes.is_empty() => Ok(bytes),
                        Ok(_) => Err("read screenshot: the capture is empty".to_string()),
                        Err(e) => Err(format!("read screenshot: {e}")),
                    },
                };
                match original {
                    Ok(original) => {
                        // The browser canvas writes no resolution, so a Retina
                        // capture annotated here would paste at twice its size
                        // in apps that honour DPI. Keep the capture's own.
                        let annotated = carry_png_resolution(&original, edited);
                        replace_file_atomically(&shot.path, &annotated)
                            .await
                            .map(|()| original)
                            .map_err(|e| format!("save screenshot: {e}"))
                    }
                    Err(error) => Err(error),
                }
            }
            Err(error) => Err(error),
        };
        match written {
            Ok(original) => {
                if shot.pristine.is_none() {
                    shot.pristine = Some(original);
                }
            }
            Err(error) => {
                restore_pending(shot);
                return Err(error);
            }
        }
    }

    let result = if action == FinishAction::Upload {
        let result = deliver_edited_share(&app, &mut shot).await;
        // Only a new link opens the browser; an update keeps the tab the
        // user already has.
        if let (Some(url), false) = (result.share_url.as_deref(), result.share_updated) {
            open_in_browser(&app, url);
        }
        result
    } else {
        deliver(&app, &shot.path, false).await
    };
    // The window stays up and reports the result itself, so the capture
    // stays claimable for another Copy or Upload. A window closed while the
    // upload ran has nothing left to finish it.
    if app.get_webview_window(EDITOR_LABEL).is_some() {
        restore_pending(PendingScreenshot {
            delivered: shot.delivered || result.clipboard != "none",
            ..shot
        });
    }
    let _ = app.emit(EVENT_SCREENSHOT_FINISHED, Some(&result));
    Ok(result)
}

/// The window's annotations, staged before every Copy or Upload. An Upload
/// sends them with the render; a reloaded window reopens with them. Sent on
/// their own because `finish_screenshot`'s body is already the PNG.
#[tauri::command]
pub fn stage_screenshot_scene(id: u64, scene: String) -> Result<(), String> {
    if scene.len() > MAX_SCENE_BYTES {
        return Err(
            "Too many annotations to upload. Remove a few, or use Copy image instead.".into(),
        );
    }
    let mut pending = PENDING.lock();
    match pending.as_mut() {
        Some(shot) if shot.id == id => {
            shot.scene = Some(scene);
            Ok(())
        }
        _ => Err(ALREADY_FINISHED.into()),
    }
}

/// Upload from the annotation window: the first one creates the link, every
/// later one replaces the image behind it. The link (or, if sharing failed,
/// the image) goes on the clipboard.
async fn deliver_edited_share(app: &AppHandle, shot: &mut PendingScreenshot) -> ScreenshotDto {
    let linked_before = shot
        .shared
        .as_ref()
        .filter(|s| s.announced)
        .map(|s| s.public_id.clone());
    let shared = share_edited(app, shot).await;
    let linked_after = shot.shared.as_ref().map(|s| s.public_id.clone());
    let mut clipboard = "none";
    let (share_url, upload_error) = match shared {
        Ok(url) => {
            use tauri_plugin_clipboard_manager::ClipboardExt;
            if app.clipboard().write_text(url.clone()).is_ok() {
                clipboard = "link";
            }
            (Some(url), None)
        }
        Err(error) => (None, Some(error)),
    };
    if clipboard == "none" && copy_image_to_clipboard(&shot.path).is_ok() {
        clipboard = "image";
    }
    ScreenshotDto {
        path: shot.path.to_string_lossy().into_owned(),
        share_updated: share_url.is_some() && linked_before.is_some() && linked_before == linked_after,
        share_url,
        clipboard,
        upload_error,
        editing: false,
    }
}

async fn share_edited(app: &AppHandle, shot: &mut PendingScreenshot) -> Result<String, String> {
    let render = tokio::fs::read(&shot.path)
        .await
        .map_err(|e| format!("read screenshot: {e}"))?;
    let scene = shot.scene.clone().unwrap_or_default();

    if let Some(shared) = shot.shared.clone() {
        match replace_shared_image(app, &shared.public_id, render.clone(), &scene).await {
            Ok(()) => {
                shot.shared = Some(SharedScreenshot {
                    announced: true,
                    ..shared.clone()
                });
                return Ok(shared.share_url);
            }
            // Deleted on the web since: a fresh link beats a dead one.
            Err(ShareError::NotFound) => shot.shared = None,
            Err(error) => return Err(error.message().to_string()),
        }
    }

    let original = shot.pristine.clone().unwrap_or_else(|| render.clone());
    let created = create_share(app, original, "Screenshot").await?;
    // Nothing was ever drawn, so the original is already the whole picture.
    if shot.pristine.is_some() {
        if let Err(error) = replace_shared_image(app, &created.public_id, render, &scene).await {
            // The link would show the capture without its annotations,
            // blurred passwords included, so it must not outlive the failure.
            return Err(match delete_share(app, &created.public_id).await {
                Ok(()) => format!("Upload failed: {}", error.message()),
                Err(_) => {
                    shot.shared = Some(SharedScreenshot {
                        announced: false,
                        ..created.clone()
                    });
                    format!(
                        "{} went up without your annotations: {}. Upload again to fix it, or delete it in your Clipy library.",
                        created.share_url,
                        error.message()
                    )
                }
            });
        }
    }
    shot.shared = Some(SharedScreenshot {
        announced: true,
        ..created.clone()
    });
    Ok(created.share_url)
}

/// An uploaded screenshot opens in the browser, the way a finished recording
/// does, so the share is visible and not only a link on the clipboard.
fn open_in_browser(app: &AppHandle, url: &str) {
    use tauri_plugin_shell::ShellExt;
    #[allow(deprecated)]
    if let Err(error) = app.shell().open(url, None) {
        tracing::warn!("screenshot share link browser open failed: {error:#}");
    }
}

/// Replaces `path` without ever leaving it half written: the bytes go to a
/// temporary file beside it, are flushed to disk, and only then renamed over
/// the original. A failure at any point (a full disk, say) leaves the capture
/// exactly as it was, so a retried Done still has it. The rename replaces an
/// existing file on both macOS and Windows.
async fn replace_file_atomically(path: &std::path::Path, bytes: &[u8]) -> std::io::Result<()> {
    use tokio::io::AsyncWriteExt;
    let name = path
        .file_name()
        .ok_or_else(|| std::io::Error::new(std::io::ErrorKind::InvalidInput, "no file name"))?;
    let temp = path.with_file_name(format!(".{}.clipy-edit", name.to_string_lossy()));
    let written = async {
        let mut file = tokio::fs::File::create(&temp).await?;
        file.write_all(bytes).await?;
        file.sync_all().await?;
        drop(file);
        tokio::fs::rename(&temp, path).await
    }
    .await;
    if written.is_err() {
        let _ = tokio::fs::remove_file(&temp).await;
    }
    written
}

/// Delete in the annotation window: the capture is removed from Pictures and
/// nothing is copied or shared.
#[tauri::command]
pub async fn discard_screenshot(app: AppHandle, id: u64) -> Result<(), String> {
    let shot = take_pending(id).ok_or_else(|| ALREADY_FINISHED.to_string())?;
    // Remove first, and only then close: a Delete that left the file behind
    // (locked by another app on Windows, say) must say so and stay retryable
    // instead of reporting success. A file that is already gone is deleted.
    match tokio::fs::remove_file(&shot.path).await {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => {
            restore_pending(shot);
            return Err(format!("Couldn't delete the screenshot: {error}"));
        }
    }
    close_editor(&app);
    let _ = app.emit(EVENT_SCREENSHOT_FINISHED, None::<ScreenshotDto>);
    Ok(())
}

/// Closes the annotation window from a state with no drawing to hand back:
/// the screenshot is still loading, its load failed, nothing is pending, or
/// it was already copied or shared. Closing counts as Copy image, so a capture
/// that was never delivered is copied as it was captured. It never uploads: a
/// share link only comes from pressing Upload.
/// Rust decides this rather than the page, because a close can arrive before
/// the page has even learned which screenshot it holds. There is only ever
/// one editor window, and new captures are refused while it is open, so the
/// pending entry is always this window's. The page cannot destroy itself (its
/// close is intercepted), so it asks.
#[tauri::command]
pub async fn close_screenshot_editor(app: AppHandle) {
    let shot = PENDING.lock().take();
    match shot {
        Some(shot) if !shot.delivered => {
            let result = deliver(&app, &shot.path, false).await;
            close_editor(&app);
            let _ = app.emit(EVENT_SCREENSHOT_FINISHED, Some(&result));
        }
        _ => close_editor(&app),
    }
}

/// Copies the `pHYs` (physical resolution) chunk from `original` into
/// `annotated` when the annotated PNG has none. macOS writes 144 dpi on a
/// Retina capture; a canvas export writes nothing, which apps that honour DPI
/// read as 72 and show at double size. Anything that does not parse as a PNG
/// is returned unchanged: this can only add metadata, never lose pixels.
fn carry_png_resolution(original: &[u8], annotated: &[u8]) -> Vec<u8> {
    fn chunks(png: &[u8]) -> Option<Vec<(&[u8; 4], std::ops::Range<usize>)>> {
        if !png.starts_with(PNG_MAGIC) {
            return None;
        }
        let mut out = Vec::new();
        let mut at = PNG_MAGIC.len();
        while at + 12 <= png.len() {
            let len = u32::from_be_bytes(png[at..at + 4].try_into().ok()?) as usize;
            let end = at.checked_add(12)?.checked_add(len)?;
            if end > png.len() {
                return None;
            }
            let kind: &[u8; 4] = png[at + 4..at + 8].try_into().ok()?;
            out.push((kind, at..end));
            if kind == b"IEND" {
                break;
            }
            at = end;
        }
        Some(out)
    }

    let unchanged = || annotated.to_vec();
    let (Some(from), Some(into)) = (chunks(original), chunks(annotated)) else {
        return unchanged();
    };
    let Some((_, phys)) = from.iter().find(|(kind, _)| *kind == b"pHYs") else {
        return unchanged();
    };
    if into.iter().any(|(kind, _)| *kind == b"pHYs") {
        return unchanged();
    }
    // pHYs must come before the first IDAT; right after IHDR always is.
    let Some((_, ihdr)) = into.first().filter(|(kind, _)| *kind == b"IHDR") else {
        return unchanged();
    };
    let mut out = Vec::with_capacity(annotated.len() + phys.len());
    out.extend_from_slice(&annotated[..ihdr.end]);
    out.extend_from_slice(&original[phys.clone()]);
    out.extend_from_slice(&annotated[ihdr.end..]);
    out
}

fn validate_png(bytes: &[u8]) -> Result<(), String> {
    if bytes.len() > MAX_EDITED_BYTES {
        return Err("The annotated screenshot is too large to save.".into());
    }
    if !bytes.starts_with(PNG_MAGIC) {
        return Err("The annotated screenshot is not a PNG.".into());
    }
    Ok(())
}

async fn upload_screenshot(
    app: &AppHandle,
    path: &std::path::Path,
    name: &str,
) -> Result<String, String> {
    let bytes = tokio::fs::read(path)
        .await
        .map_err(|e| format!("read screenshot: {e}"))?;
    create_share(app, bytes, name).await.map(|shared| shared.share_url)
}

#[derive(Debug, PartialEq, Eq)]
enum ShareError {
    /// The screenshot behind the link no longer exists.
    NotFound,
    /// It was saved from the web editor since this window last uploaded.
    Conflict,
    Failed(String),
}

impl ShareError {
    fn message(&self) -> &str {
        match self {
            Self::NotFound => "the screenshot was deleted",
            Self::Conflict => "this screenshot was changed on the web. Upload again to replace it",
            Self::Failed(message) => message,
        }
    }

    /// Only the API's own answer counts as a deletion: a 404 from anything
    /// in between would otherwise mint a second link for the same capture.
    fn from_response(status: reqwest::StatusCode, body: &serde_json::Value) -> Self {
        let error = body.get("error").and_then(|v| v.as_str());
        match status.as_u16() {
            404 if error == Some("Screenshot not found") => Self::NotFound,
            409 => Self::Conflict,
            _ => Self::Failed(
                error
                    .map(str::to_owned)
                    .unwrap_or_else(|| format!("upload failed ({status})")),
            ),
        }
    }
}

fn png_part(bytes: Vec<u8>) -> Result<reqwest::multipart::Part, String> {
    reqwest::multipart::Part::bytes(bytes)
        .file_name("screenshot.png")
        .mime_str("image/png")
        .map_err(|e| e.to_string())
}

async fn create_share(app: &AppHandle, bytes: Vec<u8>, name: &str) -> Result<SharedScreenshot, String> {
    let form = reqwest::multipart::Form::new()
        .part("file", png_part(bytes)?)
        .text("name", name.to_string());
    let body = send_screenshot_form(app, reqwest::Method::POST, "/api/screenshots", Some(form))
        .await
        .map_err(|error| error.message().to_string())?;
    let field = |key: &str| body.get(key).and_then(|v| v.as_str()).map(str::to_owned);
    match (field("publicId"), field("shareUrl")) {
        (Some(public_id), Some(share_url)) => Ok(SharedScreenshot {
            public_id,
            share_url,
            announced: false,
        }),
        _ => Err("upload response missing link".into()),
    }
}

/// Swaps the image behind an existing link, the same call the web editor's
/// Save makes. `scene` is the annotations over the share's original.
async fn replace_shared_image(
    app: &AppHandle,
    public_id: &str,
    render: Vec<u8>,
    scene: &str,
) -> Result<(), ShareError> {
    let form = reqwest::multipart::Form::new()
        .part("file", png_part(render).map_err(ShareError::Failed)?)
        .text("annotations", scene.to_string());
    send_screenshot_form(
        app,
        reqwest::Method::PUT,
        &format!("/api/screenshots/{public_id}/image"),
        Some(form),
    )
    .await
    .map(|_| ())
}

async fn delete_share(app: &AppHandle, public_id: &str) -> Result<(), ShareError> {
    send_screenshot_form(
        app,
        reqwest::Method::DELETE,
        &format!("/api/screenshots/{public_id}"),
        None,
    )
    .await
    .map(|_| ())
}

/// The one place a screenshot would leave the machine. The local edition sends
/// nothing; `can_upload` already hides Upload, so this only answers a stale
/// request.
async fn send_screenshot_form(
    _app: &AppHandle,
    _method: reqwest::Method,
    _path: &str,
    _form: Option<reqwest::multipart::Form>,
) -> Result<serde_json::Value, ShareError> {
    Err(ShareError::Failed(
        "Clipy Local does not upload screenshots".into(),
    ))
}

#[tauri::command]
pub fn reveal_screenshot(path: String) -> Result<(), String> {
    #[cfg(target_os = "macos")]
    {
        std::process::Command::new("/usr/bin/open")
            .arg("-R")
            .arg(&path)
            .status()
            .map_err(|e| e.to_string())?;
        Ok(())
    }
    #[cfg(target_os = "windows")]
    {
        // Explorer reports its own exit code inconsistently, so only the
        // spawn is checked.
        std::process::Command::new("explorer.exe")
            .arg(format!("/select,{path}"))
            .spawn()
            .map_err(|e| e.to_string())?;
        Ok(())
    }
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    {
        let _ = path;
        Err("Unsupported platform".into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `PENDING` is process-wide and tests run in parallel.
    static PENDING_TEST: Mutex<()> = Mutex::new(());

    #[test]
    fn edited_screenshot_must_be_a_png() {
        assert!(validate_png(b"\x89PNG\r\n\x1a\n rest of the file").is_ok());
        assert!(validate_png(b"GIF89a").is_err());
        assert!(validate_png(b"").is_err());
    }

    /// A PNG from its chunks; CRCs are placeholders (the copy is verbatim,
    /// so it never recomputes them).
    fn png(chunks: &[(&[u8; 4], &[u8])]) -> Vec<u8> {
        let mut out = PNG_MAGIC.to_vec();
        for (kind, data) in chunks {
            out.extend_from_slice(&(data.len() as u32).to_be_bytes());
            out.extend_from_slice(*kind);
            out.extend_from_slice(data);
            out.extend_from_slice(&[0xAB; 4]);
        }
        out
    }

    #[test]
    fn annotated_screenshot_keeps_the_capture_resolution() {
        let retina = [0, 0, 0x16, 0x25, 0, 0, 0x16, 0x25, 1];
        let original = png(&[
            (b"IHDR", &[1; 13]),
            (b"pHYs", &retina),
            (b"IDAT", &[2; 8]),
            (b"IEND", &[]),
        ]);
        let annotated = png(&[(b"IHDR", &[3; 13]), (b"IDAT", &[4; 20]), (b"IEND", &[])]);

        let fixed = carry_png_resolution(&original, &annotated);

        assert_eq!(
            fixed,
            png(&[
                (b"IHDR", &[3; 13]),
                (b"pHYs", &retina),
                (b"IDAT", &[4; 20]),
                (b"IEND", &[]),
            ])
        );
    }

    #[test]
    fn resolution_is_never_invented_or_duplicated() {
        let plain = png(&[(b"IHDR", &[1; 13]), (b"IDAT", &[2; 8]), (b"IEND", &[])]);
        let with_dpi = png(&[
            (b"IHDR", &[1; 13]),
            (b"pHYs", &[0; 9]),
            (b"IDAT", &[2; 8]),
            (b"IEND", &[]),
        ]);
        // No resolution on the capture (a Windows grab): nothing to carry.
        assert_eq!(carry_png_resolution(&plain, &plain), plain);
        // The export already has one: leave it alone.
        assert_eq!(carry_png_resolution(&with_dpi, &with_dpi), with_dpi);
        // An unreadable original or export passes through untouched.
        assert_eq!(carry_png_resolution(b"not a png", &plain), plain);
        let truncated = &with_dpi[..20];
        assert_eq!(carry_png_resolution(&with_dpi, truncated), truncated.to_vec());
    }

    #[tokio::test]
    async fn an_edit_replaces_the_capture_whole_or_not_at_all() {
        let dir = tempfile::tempdir().unwrap();
        let shot = dir.path().join("Clipy Screenshot 1.png");
        std::fs::write(&shot, b"original capture").unwrap();

        replace_file_atomically(&shot, b"annotated").await.unwrap();
        assert_eq!(std::fs::read(&shot).unwrap(), b"annotated");
        // Nothing left behind beside it.
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 1);

        // A replace that cannot complete leaves the target untouched and
        // cleans up its temporary file. (Renaming a file over a directory
        // fails, standing in for a write that dies part way.)
        let blocked = dir.path().join("blocked.png");
        std::fs::create_dir(&blocked).unwrap();
        std::fs::write(blocked.join("keep"), b"x").unwrap();
        assert!(replace_file_atomically(&blocked, b"annotated").await.is_err());
        assert!(blocked.is_dir());
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 2);
    }

    #[test]
    fn a_screenshot_is_finished_at_most_once() {
        let _guard = PENDING_TEST.lock();
        let shot = |id| PendingScreenshot {
            id,
            path: PathBuf::from("/tmp/one.png"),
            upload_by_default: true,
            delivered: false,
            pristine: None,
            scene: None,
            shared: None,
        };
        *PENDING.lock() = Some(shot(41));
        assert_eq!(pending_matching(41).unwrap(), PathBuf::from("/tmp/one.png"));
        assert_eq!(pending_dto(true).map(|p| p.id), Some(41));

        // A stale id cannot claim, and cannot clear, someone else's entry.
        assert!(take_pending(40).is_none());
        assert_eq!(pending_dto(true).map(|p| p.id), Some(41));

        // The first Done claims it; a second press finds nothing to upload.
        let claimed = take_pending(41).expect("first claim");
        assert!(take_pending(41).is_none());
        assert!(pending_matching(41).is_err());

        // A Done that failed before copying anything can be pressed again.
        restore_pending(claimed);
        assert!(take_pending(41).is_some());
        assert!(pending_dto(true).is_none());
    }

    #[test]
    fn only_the_api_saying_so_counts_as_a_deleted_share() {
        let said = |error: &str| serde_json::json!({ "error": error });
        assert_eq!(
            ShareError::from_response(reqwest::StatusCode::NOT_FOUND, &said("Screenshot not found")),
            ShareError::NotFound
        );
        // A 404 from a proxy, or the wrong route, must not mint a new link.
        assert!(matches!(
            ShareError::from_response(reqwest::StatusCode::NOT_FOUND, &serde_json::Value::Null),
            ShareError::Failed(_)
        ));
        assert_eq!(
            ShareError::from_response(reqwest::StatusCode::CONFLICT, &said("Reload and edit again.")),
            ShareError::Conflict
        );
        assert_eq!(
            ShareError::from_response(reqwest::StatusCode::PAYLOAD_TOO_LARGE, &said("Image too large.")),
            ShareError::Failed("Image too large.".into())
        );
    }

    #[test]
    fn a_reloaded_editor_gets_back_what_it_had() {
        let _guard = PENDING_TEST.lock();
        *PENDING.lock() = Some(PendingScreenshot {
            id: 77,
            path: PathBuf::from("/tmp/two.png"),
            upload_by_default: false,
            delivered: false,
            pristine: None,
            scene: None,
            shared: None,
        });
        stage_screenshot_scene(77, "{\"shapes\":[]}".into()).unwrap();
        assert!(stage_screenshot_scene(76, "{}".into()).is_err());
        assert!(stage_screenshot_scene(77, "x".repeat(MAX_SCENE_BYTES + 1)).is_err());

        {
            let mut pending = PENDING.lock();
            let shot = pending.as_mut().unwrap();
            shot.delivered = true;
            shot.pristine = Some(b"capture".to_vec());
            shot.shared = Some(SharedScreenshot {
                public_id: "abc".into(),
                share_url: "https://clipy.online/s/abc".into(),
                announced: true,
            });
        }
        let dto = pending_dto(true).unwrap();
        assert!(dto.delivered && dto.linked && dto.edit_written);
        assert_eq!(dto.scene.as_deref(), Some("{\"shapes\":[]}"));
        // The source is the capture as taken, not the flattened render.
        assert_eq!(pending_pristine(77).unwrap(), Some(b"capture".to_vec()));
        *PENDING.lock() = None;
    }

    #[test]
    fn the_finish_action_must_be_named() {
        assert_eq!(FinishAction::from_header(Some("copy")), Ok(FinishAction::Copy));
        assert_eq!(FinishAction::from_header(Some("upload")), Ok(FinishAction::Upload));
        assert!(FinishAction::from_header(Some("share")).is_err());
        assert!(FinishAction::from_header(None).is_err());
    }
}
