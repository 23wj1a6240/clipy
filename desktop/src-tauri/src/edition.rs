//! Which product this binary is: the official app (Cargo feature `cloud`, on by
//! default) or the local edition (`--no-default-features`), which has no Clipy
//! account, upload, updater, crash reporting or analytics.
//!
//! Every helper here keeps the official app's behaviour exactly: its `cloud`
//! body is the expression it replaced at the call site.

use tauri::{AppHandle, Runtime};
use url::Url;

pub const CLOUD: bool = false;

/// The URL scheme this build registers. A different scheme keeps the local
/// edition and an installed official app from receiving each other's links.
/// The name people see. The local edition is its own app, Clipy Local.
pub const APP_NAME: &str = if CLOUD { "Clipy" } else { "Clipy Local" };

pub const DEEP_LINK_SCHEME: &str = if CLOUD { "clipy" } else { "clipy-local" };

/// What the WebView compares with its own build-time edition at boot, so a
/// cloud WebView inside a local binary (or the reverse) cannot ship silently.
/// The placeholder bearer of the official app's debug dev session (auth.rs).
/// It keeps a test take local instead of uploading with a token no server
/// accepts.

pub fn is_dev_bypass(_token: &str) -> bool {
    false
}

/// The local edition is its own app: the product name in translated text
/// reads "Clipy Local". The catalogs keep the official app's "Clipy".
pub fn brand_text(text: &str) -> String {
    if CLOUD {
        return text.to_string();
    }
    const NAME: &str = "Clipy";
    let mut out = String::with_capacity(text.len() + 8);
    let mut rest = text;
    while let Some(at) = rest.find(NAME) {
        let (before, after) = rest.split_at(at);
        let tail = &after[NAME.len()..];
        let starts_word = before.chars().next_back().is_none_or(|c| !c.is_ascii_alphanumeric());
        let ends_word = tail.chars().next().is_none_or(|c| !c.is_ascii_alphanumeric());
        out.push_str(before);
        out.push_str(NAME);
        if starts_word && ends_word && !tail.starts_with(" Local") {
            out.push_str(" Local");
        }
        rest = tail;
    }
    out.push_str(rest);
    out
}

#[tauri::command]
pub fn get_edition() -> &'static str {
    if CLOUD {
        "cloud"
    } else {
        "local"
    }
}

/// Whether Start is offered (tray, global hotkey, launch popover).
/// Official app: exactly the old signed-in test. Local edition: always.
pub fn can_start_recording<R: Runtime>(app: &AppHandle<R>) -> bool {
    {
        let _ = app;
        true
    }
}

/// `<scheme>://local/<id>` opens the viewer for a recording on this machine.
/// Returns the recording id for such a link.
pub fn local_recording_link(url: &Url) -> Option<String> {
    if url.scheme() != DEEP_LINK_SCHEME || url.host_str() != Some("local") {
        return None;
    }
    Some(url.path().trim_matches('/').to_string())
}

/// Every deep link the OS hands this process. The local-recording link is
/// handled here, before anything account related; in the official app every
/// other link goes to sign-in exactly as before (`AuthState::handle_deep_link`
/// still refuses a foreign scheme first).
pub async fn dispatch_deep_link<R: Runtime>(app: &AppHandle<R>, url: &Url) -> anyhow::Result<()> {
    if let Some(id) = local_recording_link(url) {
        return crate::local::viewer::show(app, &id);
    }
    {
        tracing::warn!("ignoring deep link: {}", url);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::brand_text;

    #[test]
    fn brand_names_the_app_it_is_built_as() {
        let text = "Quit Clipy. Clipy Local stays. ClipyMark, clipy.online, Clipy's server.";
        if super::CLOUD {
            assert_eq!(brand_text(text), text);
        } else {
            assert_eq!(
                brand_text(text),
                "Quit Clipy Local. Clipy Local stays. ClipyMark, clipy.online, Clipy Local's server."
            );
        }
    }
}
