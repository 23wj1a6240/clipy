//! Words the Rust side shows people: the tray menu, native notifications,
//! window titles and dialogs. docs/i18n.md, "Desktop app".
//!
//! ONE SOURCE OF TRUTH. The strings are not kept here: they are the `native`
//! subtree of the shared catalog, `i18n/messages/<locale>/desktop.json`,
//! compiled in with `include_str!` (Cargo rebuilds when a file changes). The
//! webview's React UI reads the same files through use-intl, so translators
//! edit one file and `pnpm i18n:check` validates both halves.
//!
//! Native strings use plain `{name}` placeholders only, no ICU plurals or
//! selects: a count that needs a plural gets one key per form (`...One`,
//! `...Many`). Each key is looked up in the current language, then English,
//! then falls back to the key itself so a missing string is visible, never a
//! crash.
//!
//! The language is the one the webview resolved (the Settings choice, else
//! the account's, else the OS language), reported with `set_ui_locale` and
//! saved in `ui-locale` under the app data directory so the tray menu built
//! at launch, before any webview has loaded, is already in that language.
//!
//! Agent-facing text stays English and does not come from here: evidence and
//! AREC wording (`EVIDENCE_BOUNDARY`, `AUDIO_RECLAIMED`, anything written into
//! a .arec file) is read by AI agents and tooling.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{OnceLock, RwLock};

use serde_json::Value;
use tauri::{AppHandle, Emitter, Manager, Runtime, State};

/// Emitted after the language changes so the tray menu can be rebuilt.
pub const EVENT_UI_LOCALE_CHANGED: &str = "ui-locale-changed";

/// The shipped locales, the same list as `i18n/locales.ts` (`LOCALES`).
pub const LOCALES: [&str; 6] = ["en", "pt-BR", "es", "zh-CN", "ja", "hi"];
const DEFAULT_LOCALE: &str = "en";
const LOCALE_FILE: &str = "ui-locale";

const SOURCES: [(&str, &str); 6] = [
    ("en", include_str!("../../src/vendor/i18n/messages/en/desktop.json")),
    ("pt-BR", include_str!("../../src/vendor/i18n/messages/pt-BR/desktop.json")),
    ("es", include_str!("../../src/vendor/i18n/messages/es/desktop.json")),
    ("zh-CN", include_str!("../../src/vendor/i18n/messages/zh-CN/desktop.json")),
    ("ja", include_str!("../../src/vendor/i18n/messages/ja/desktop.json")),
    ("hi", include_str!("../../src/vendor/i18n/messages/hi/desktop.json")),
];

fn tables() -> &'static HashMap<&'static str, Value> {
    static TABLES: OnceLock<HashMap<&'static str, Value>> = OnceLock::new();
    TABLES.get_or_init(|| {
        SOURCES
            .iter()
            .map(|(locale, raw)| {
                let parsed = serde_json::from_str::<Value>(raw).unwrap_or_else(|err| {
                    tracing::warn!("desktop.json for {locale} does not parse: {err}");
                    Value::Null
                });
                (*locale, parsed.get("native").cloned().unwrap_or(Value::Null))
            })
            .collect()
    })
}

fn current() -> &'static RwLock<&'static str> {
    static CURRENT: OnceLock<RwLock<&'static str>> = OnceLock::new();
    CURRENT.get_or_init(|| RwLock::new(DEFAULT_LOCALE))
}

/// Maps a tag onto a shipped locale, exactly (`ja`) or by language (`pt` and
/// `pt-PT` are `pt-BR`, `es-MX` is `es`). Traditional Chinese gets English,
/// as in `normalizeLocale()` on the TypeScript side.
pub fn normalize(tag: &str) -> Option<&'static str> {
    let tag = tag.trim().replace('_', "-");
    if tag.is_empty() {
        return None;
    }
    if let Some(exact) = LOCALES.iter().find(|l| l.eq_ignore_ascii_case(&tag)) {
        return Some(exact);
    }
    let lower = tag.to_ascii_lowercase();
    let language = lower.split('-').next().unwrap_or("");
    match language {
        "en" => Some("en"),
        "pt" => Some("pt-BR"),
        "es" => Some("es"),
        "ja" => Some("ja"),
        "hi" => Some("hi"),
        "zh" => {
            let traditional = ["-tw", "-hk", "-mo", "-hant"]
                .iter()
                .any(|suffix| lower.contains(suffix));
            (!traditional).then_some("zh-CN")
        }
        _ => None,
    }
}

/// The language native strings are shown in right now.
pub fn locale() -> &'static str {
    current().read().map(|l| *l).unwrap_or(DEFAULT_LOCALE)
}

fn set_locale(locale: &'static str) {
    if let Ok(mut guard) = current().write() {
        *guard = locale;
    }
}

fn lookup(locale: &str, key: &str) -> Option<String> {
    let mut node = tables().get(locale)?;
    for segment in key.split('.') {
        node = node.get(segment)?;
    }
    node.as_str().filter(|s| !s.is_empty()).map(str::to_string)
}

fn translate_in(locale: &str, key: &str) -> String {
    let text = lookup(locale, key)
        .or_else(|| lookup(DEFAULT_LOCALE, key))
        .unwrap_or_else(|| key.to_string());
    crate::edition::brand_text(&text)
}

/// The string at `key` under `native` in desktop.json, e.g. `tray.quit`.
pub fn t(key: &str) -> String {
    translate_in(locale(), key)
}

/// Like [`t`], filling `{name}` placeholders.
pub fn t_with<V: AsRef<str>>(key: &str, values: &[(&str, V)]) -> String {
    fill(t(key), values)
}

fn fill<V: AsRef<str>>(mut text: String, values: &[(&str, V)]) -> String {
    for (name, value) in values {
        text = text.replace(&format!("{{{name}}}"), value.as_ref());
    }
    text
}

/// A start or capture failure the user reads, in the current language.
///
/// Rust writes these sentences in English as constants next to the code that
/// raises them (`capture/failure.rs`, `recording.rs`, `capture/windows.rs`).
/// Their stable code is the key under `native.errors` in desktop.json whose
/// English is that sentence; this finds it and returns the translation, so
/// the Windows-only call sites need no change. Anything else (an error chain,
/// a server refusal, which the server translates itself) is returned as is.
pub fn localize_message(english: &str) -> String {
    let code = tables()
        .get(DEFAULT_LOCALE)
        .and_then(|table| table.get("errors"))
        .and_then(Value::as_object)
        .and_then(|errors| {
            errors
                .iter()
                .find(|(_, value)| value.as_str() == Some(english))
                .map(|(key, _)| key.clone())
        });
    match code {
        Some(code) => t(&format!("errors.{code}")),
        None => english.to_string(),
    }
}

/// True when `english` is a known sentence with a `native.errors` code, so
/// [`localize_message`] translates it. For tests of the call sites.
#[cfg(test)]
pub(crate) fn has_error_code(english: &str) -> bool {
    tables()
        .get(DEFAULT_LOCALE)
        .and_then(|table| table.get("errors"))
        .and_then(Value::as_object)
        .is_some_and(|errors| errors.values().any(|value| value.as_str() == Some(english)))
}

fn locale_path<R: Runtime>(app: &AppHandle<R>) -> Option<PathBuf> {
    app.path().app_data_dir().ok().map(|dir| dir.join(LOCALE_FILE))
}

/// Restores the language saved by the last `set_ui_locale`. Called in setup,
/// before the tray menu is built.
pub fn load_persisted<R: Runtime>(app: &AppHandle<R>) {
    let saved = locale_path(app)
        .and_then(|path| std::fs::read_to_string(path).ok())
        .and_then(|raw| normalize(&raw));
    if let Some(locale) = saved {
        set_locale(locale);
    }
}

/// The webview reports the language it resolved. Unknown tags are ignored.
#[tauri::command]
pub fn set_ui_locale(app: AppHandle, locale: String) -> Result<(), String> {
    let Some(locale) = normalize(&locale) else {
        return Err(format!("unsupported locale: {locale}"));
    };
    let changed = self::locale() != locale;
    set_locale(locale);
    if let Some(path) = locale_path(&app) {
        if let Some(dir) = path.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        if let Err(err) = std::fs::write(&path, locale) {
            tracing::warn!("failed to save ui locale: {err}");
        }
    }
    if changed {
        let _ = app.emit(EVENT_UI_LOCALE_CHANGED, locale);
    }
    Ok(())
}

#[derive(serde::Deserialize)]
struct AccountLocale {
    #[serde(default)]
    locale: Option<String>,
}

/// The account's language (`GET /api/me/locale`), so a language chosen on the
/// web or another device follows the account. `None` when signed out, never
/// chosen, or the lookup failed: the caller falls back to the OS language.
#[tauri::command]
pub async fn get_account_locale(
    cfg: State<'_, crate::config::Config>,
    auth: State<'_, crate::auth::AuthState>,
) -> Result<Option<String>, String> {
    let Some(token) = auth.bearer_token() else {
        return Ok(None);
    };
    let url = format!("{}/api/me/locale", cfg.api_base.trim_end_matches('/'));
    let response = reqwest::Client::new()
        .get(&url)
        .header(reqwest::header::USER_AGENT, crate::config::USER_AGENT)
        .bearer_auth(token)
        .timeout(std::time::Duration::from_secs(8))
        .send()
        .await
        .and_then(|r| r.error_for_status());
    let Ok(response) = response else {
        return Ok(None);
    };
    let parsed: AccountLocale = response.json().await.map_err(|e| e.to_string())?;
    Ok(parsed.locale.as_deref().and_then(normalize).map(str::to_string))
}

/// Saves the language on the account (`PUT /api/me/locale`) so the web app
/// follows; `None` clears it (follow the device).
#[tauri::command]
pub async fn save_account_locale(
    cfg: State<'_, crate::config::Config>,
    auth: State<'_, crate::auth::AuthState>,
    locale: Option<String>,
) -> Result<(), String> {
    let Some(token) = auth.bearer_token() else {
        return Ok(());
    };
    let locale = locale.as_deref().and_then(normalize);
    let url = format!("{}/api/me/locale", cfg.api_base.trim_end_matches('/'));
    reqwest::Client::new()
        .put(&url)
        .header(reqwest::header::USER_AGENT, crate::config::USER_AGENT)
        .bearer_auth(token)
        .json(&serde_json::json!({ "locale": locale }))
        .timeout(std::time::Duration::from_secs(8))
        .send()
        .await
        .and_then(|r| r.error_for_status())
        .map_err(|e| e.to_string())?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn leaves(prefix: &str, node: &Value, out: &mut Vec<String>) {
        match node {
            Value::Object(map) => {
                for (key, child) in map {
                    let path = if prefix.is_empty() { key.clone() } else { format!("{prefix}.{key}") };
                    leaves(&path, child, out);
                }
            }
            Value::String(_) => out.push(prefix.to_string()),
            _ => {}
        }
    }

    fn placeholders(text: &str) -> Vec<String> {
        let mut names: Vec<String> = text
            .split('{')
            .skip(1)
            .filter_map(|rest| rest.split('}').next())
            .map(str::to_string)
            .collect();
        names.sort();
        names
    }

    #[test]
    fn every_catalog_parses_and_english_has_the_native_table() {
        for (locale, _) in SOURCES {
            assert!(!tables()[locale].is_null() || locale != "en", "{locale}");
        }
        assert!(lookup("en", "tray.quit").is_some());
    }

    #[test]
    fn translations_keep_english_keys_and_placeholders() {
        let mut keys = Vec::new();
        leaves("", &tables()["en"], &mut keys);
        assert!(!keys.is_empty());
        for locale in LOCALES {
            for key in &keys {
                if let Some(text) = lookup(locale, key) {
                    assert_eq!(
                        placeholders(&text),
                        placeholders(&lookup("en", key).unwrap()),
                        "{locale} {key}"
                    );
                }
            }
        }
    }

    #[test]
    fn falls_back_to_english_then_to_the_key() {
        assert_eq!(translate_in("ja", "no.such.key"), "no.such.key");
        assert_eq!(
            translate_in("xx", "tray.quit"),
            crate::edition::brand_text(&lookup("en", "tray.quit").unwrap())
        );
    }

    #[test]
    fn known_failure_sentences_have_a_code_and_others_pass_through() {
        assert_eq!(
            localize_message("You're offline. Connect to the internet and try again."),
            lookup(locale(), "errors.offline").unwrap()
        );
        assert_eq!(localize_message("open ffmpeg MP4 encoder: boom"), "open ffmpeg MP4 encoder: boom");
        assert!(has_error_code("You're offline. Connect to the internet and try again."));
        assert!(!has_error_code("open ffmpeg MP4 encoder: boom"));
    }

    #[test]
    fn fills_placeholders() {
        assert_eq!(fill("Hi {name}, {name}".into(), &[("name", "Ana")]), "Hi Ana, Ana");
    }

    #[test]
    fn normalizes_os_tags() {
        assert_eq!(normalize("ja-JP"), Some("ja"));
        assert_eq!(normalize("pt_PT"), Some("pt-BR"));
        assert_eq!(normalize("es-419"), Some("es"));
        assert_eq!(normalize("zh-Hans-CN"), Some("zh-CN"));
        assert_eq!(normalize("zh-TW"), None);
        assert_eq!(normalize("zh-Hant"), None);
        assert_eq!(normalize("hi-IN"), Some("hi"));
        assert_eq!(normalize("fr-FR"), None);
        assert_eq!(normalize("PT-br"), Some("pt-BR"));
    }
}
