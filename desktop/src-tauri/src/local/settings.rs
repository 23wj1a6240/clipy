//! Local edition settings commands, the Rust half of
//! `src/edition-local/settings-api.ts`: `get_local_settings`,
//! `save_local_settings`, `local_model_status`, `download_local_model` (plus
//! `cancel_local_model_download`) and the `local-model-progress` event.
//!
//! Built only without the `cloud` feature. The validation and the shapes are
//! in `clipy_local_edition::settings`, tested there; this file is the I/O:
//! the preset, model names and base URL go to `settings.json` under
//! `local_edition`, keys go to the owner-only `local-keys.json` in the app
//! data folder through `keychain.rs`. No key is ever returned, logged or put
//! in an error.

use std::path::PathBuf;

use clipy_local_edition::settings::{
    apply_save, to_dto, KeyFile, LocalModelId, LocalModelStatusDto, LocalSettingsDto, Platform,
    SaveLocalSettingsRequest, StoredSettings, STORE_KEY,
};
use tauri::{AppHandle, Manager, Runtime};
use tauri_plugin_store::StoreExt;

use crate::commands::SETTINGS_STORE;

/// The key file, beside `session.json` in the app data folder.
const KEY_FILE: &str = "local-keys.json";

/// Saves read, change and write two files; one at a time.
static SAVE: parking_lot::Mutex<()> = parking_lot::const_mutex(());

pub(crate) const PLATFORM: Platform = Platform::current();

/// What `settings.json` holds, or the defaults.
pub(crate) fn load_settings<R: Runtime>(app: &AppHandle<R>) -> StoredSettings {
    let value = app
        .store(SETTINGS_STORE)
        .ok()
        .and_then(|store| store.get(STORE_KEY));
    StoredSettings::from_value(value.as_ref(), PLATFORM)
}

fn key_path<R: Runtime>(app: &AppHandle<R>) -> Option<PathBuf> {
    app.path().app_data_dir().ok().map(|dir| dir.join(KEY_FILE))
}

/// The key file. Unreadable reads as empty: the user is asked again.
pub(crate) fn load_keys<R: Runtime>(app: &AppHandle<R>) -> KeyFile {
    let bytes = key_path(app).and_then(|path| crate::keychain::load(&path).ok().flatten());
    KeyFile::from_bytes(bytes.as_deref())
}

fn save_keys<R: Runtime>(app: &AppHandle<R>, keys: &KeyFile) -> Result<(), String> {
    let path = key_path(app).ok_or_else(|| "Could not find the app data folder.".to_string())?;
    let result = if keys.is_empty() {
        crate::keychain::clear(&path)
    } else {
        crate::keychain::save(&path, &keys.to_bytes())
    };
    result.map_err(|_| "Could not save the key file.".to_string())
}

#[tauri::command]
pub fn get_local_settings(app: AppHandle) -> Result<LocalSettingsDto, String> {
    Ok(to_dto(&load_settings(&app), &load_keys(&app)))
}

#[tauri::command]
pub fn save_local_settings(
    app: AppHandle,
    settings: SaveLocalSettingsRequest,
) -> Result<LocalSettingsDto, String> {
    let _one_at_a_time = SAVE.lock();
    let current = load_settings(&app);
    let keys = load_keys(&app);
    let (next, next_keys) = apply_save(&current, &keys, &settings, PLATFORM)?;
    // Keys first: a key saved for settings that then fail to save is bound
    // to a preset and URL that are not stored, so it is never used.
    if next_keys != keys {
        save_keys(&app, &next_keys)?;
    }
    let store = app
        .store(SETTINGS_STORE)
        .map_err(|_| "Could not open the settings file.".to_string())?;
    store.set(STORE_KEY, next.to_value());
    store
        .save()
        .map_err(|_| "Could not save the settings file.".to_string())?;
    Ok(to_dto(&next, &next_keys))
}

#[tauri::command]
pub fn local_model_status() -> Vec<LocalModelStatusDto> {
    LocalModelId::ALL
        .iter()
        .map(|&id| LocalModelStatusDto::new(id, downloads::installed(id), downloads::running(id)))
        .collect()
}

#[tauri::command]
pub async fn download_local_model(app: AppHandle, model: LocalModelId) -> Result<(), String> {
    downloads::download(app, model).await
}

/// Stops a running download (command 5 of the contract). Nothing is
/// installed, and the download's own caller rejects with "The download was
/// cancelled."
#[tauri::command]
pub fn cancel_local_model_download(model: LocalModelId) {
    downloads::cancel(model);
}

/// Model downloads on Windows, through `clipy_local_stt::download_model`.
#[cfg(target_os = "windows")]
mod downloads {
    use std::collections::BTreeMap;
    use std::sync::Arc;
    use std::time::{Duration, Instant};

    use clipy_local_edition::settings::{
        LocalModelId, LocalModelProgressDto, EVENT_LOCAL_MODEL_PROGRESS,
    };
    use clipy_local_stt::{CancelToken, SttError};
    use tauri::{AppHandle, Emitter};
    use tokio::sync::watch;

    /// At most this often while bytes arrive; stage changes always go out.
    const PROGRESS_EVERY: Duration = Duration::from_millis(250);

    type Outcome = Option<Result<(), String>>;

    struct Job {
        cancel: CancelToken,
        done: watch::Receiver<Outcome>,
    }

    static JOBS: parking_lot::Mutex<BTreeMap<LocalModelId, Arc<Job>>> =
        parking_lot::const_mutex(BTreeMap::new());

    pub(super) fn installed(model: LocalModelId) -> bool {
        clipy_local_stt::model_status(model.to_stt(), None).installed
    }

    pub(super) fn running(model: LocalModelId) -> bool {
        JOBS.lock().contains_key(&model)
    }

    pub(super) fn cancel(model: LocalModelId) {
        if let Some(job) = JOBS.lock().get(&model) {
            job.cancel.cancel();
        }
    }

    /// Start the download, or join the one already running for `model`.
    pub(super) async fn download(app: AppHandle, model: LocalModelId) -> Result<(), String> {
        let mut done = {
            let mut jobs = JOBS.lock();
            match jobs.get(&model) {
                Some(job) => job.done.clone(),
                None => {
                    let (tx, rx) = watch::channel(None);
                    let cancel = CancelToken::new();
                    jobs.insert(
                        model,
                        Arc::new(Job {
                            cancel: cancel.clone(),
                            done: rx.clone(),
                        }),
                    );
                    let thread_app = app.clone();
                    let spawned = std::thread::Builder::new()
                        .name("clipy-model-download".into())
                        .spawn(move || run(thread_app, model, cancel, tx));
                    if spawned.is_err() {
                        jobs.remove(&model);
                        return Err("Could not start the download.".into());
                    }
                    rx
                }
            }
        };
        loop {
            // Cloned out first: the borrow guard must not live across an await.
            let current: Outcome = done.borrow().clone();
            if let Some(outcome) = current {
                return outcome;
            }
            if done.changed().await.is_err() {
                // The download thread ended without an answer (it panicked):
                // forget the job so the next call can start again.
                JOBS.lock()
                    .retain(|id, job| *id != model || job.done.has_changed().is_ok());
                let last: Outcome = done.borrow().clone();
                return last.unwrap_or_else(|| Err("The download stopped unexpectedly.".into()));
            }
        }
    }

    fn run(app: AppHandle, model: LocalModelId, cancel: CancelToken, tx: watch::Sender<Outcome>) {
        let mut last_sent: Option<Instant> = None;
        let mut last_stage = None;
        let result = clipy_local_stt::download_model(
            model.to_stt(),
            None,
            &mut |fraction| {
                let progress = LocalModelProgressDto::running(model, fraction);
                let due = last_sent.is_none_or(|at| at.elapsed() >= PROGRESS_EVERY);
                if due || last_stage != Some(progress.stage) {
                    last_sent = Some(Instant::now());
                    last_stage = Some(progress.stage);
                    let _ = app.emit(EVENT_LOCAL_MODEL_PROGRESS, progress);
                }
            },
            &cancel,
        );
        let outcome = match result {
            Ok(_) => {
                let _ = app.emit(
                    EVENT_LOCAL_MODEL_PROGRESS,
                    LocalModelProgressDto::installed(model),
                );
                Ok(())
            }
            Err(error) => {
                // Download errors name the model and what went wrong, never a
                // response body; nothing here is a key.
                let message = match error {
                    SttError::Cancelled => "The download was cancelled.".to_string(),
                    other => other.to_string(),
                };
                tracing::warn!(model = model.id(), "model download failed: {message}");
                let _ = app.emit(
                    EVENT_LOCAL_MODEL_PROGRESS,
                    LocalModelProgressDto::failed(model, message.clone()),
                );
                Err(message)
            }
        };
        // Removed before the answer goes out, so a call that arrives after
        // it starts afresh (and finds the model installed at once).
        JOBS.lock().remove(&model);
        let _ = tx.send(Some(outcome));
    }
}

/// The Mac transcribes with Apple Speech and downloads no model; the
/// WebView shows the model rows on Windows only.
#[cfg(not(target_os = "windows"))]
mod downloads {
    use clipy_local_edition::settings::LocalModelId;
    use tauri::AppHandle;

    pub(super) fn installed(_model: LocalModelId) -> bool {
        false
    }

    pub(super) fn running(_model: LocalModelId) -> bool {
        false
    }

    pub(super) fn cancel(_model: LocalModelId) {}

    pub(super) async fn download(_app: AppHandle, _model: LocalModelId) -> Result<(), String> {
        Err("This Mac transcribes on the device with Apple Speech, so there is no model to download.".into())
    }
}
