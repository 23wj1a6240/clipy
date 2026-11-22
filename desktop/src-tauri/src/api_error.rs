//! Structured refusals from the Clipy API.
//!
//! Every `/api/videos/raw-upload/*` route answers a rejection with a JSON body
//! shaped `{ error, code, ... }`, where `error` is a sentence written for the
//! user. Until this module existed the uploader did `bail!("initiate failed
//! ({status}): {body}")`, so the recorder's error line showed the whole JSON
//! document — an owner hit exactly that on the free-quota 403 and saw a wall of
//! `{"error":"You've made 3 recordings…","code":"pro_required","level":…}`
//! under the Start button.
//!
//! `ApiRefusal` keeps the parsed pieces so:
//!   - `Display` is the server's sentence and nothing else, which is what the
//!     tray, the agent bridge and any `{err:#}` log line end up printing;
//!   - `code`/`level`/`quota` survive the `anyhow` context chain, so
//!     `commands::start_recording` can hand the webview a real gate card
//!     instead of red text.
//!
//! Because callers wrap this in `.context(...)`, never read the formatted error
//! string to classify a failure — walk the chain with [`refusal_of`].

use serde::Serialize;

/// A non-success HTTP answer that carried (or should have carried) a
/// user-facing explanation.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ApiRefusal {
    pub status: u16,
    /// The sentence to show a human. Always non-empty.
    pub message: String,
    /// Machine tag, e.g. `pro_required`, `upgrade_required`.
    pub code: Option<String>,
    /// For `pro_required`: `soft` | `hard` | `quota`.
    pub level: Option<String>,
    /// Free-tier usage numbers, passed through untouched so the UI can draw a
    /// meter. Deliberately opaque here — the desktop app must not need a
    /// release every time the server adds a field.
    pub quota: Option<serde_json::Value>,
}

impl std::fmt::Display for ApiRefusal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for ApiRefusal {}

/// Longest raw body we will ever quote back when the server said nothing
/// parseable. Enough to debug an HTML error page from a proxy edge, short
/// enough not to become the wall of text this module exists to prevent.
const MAX_RAW_BODY: usize = 200;

#[derive(serde::Deserialize)]
struct ErrorBody {
    #[serde(default)]
    error: Option<String>,
    /// Some routes (and most proxies) say `message` instead.
    #[serde(default)]
    message: Option<String>,
    #[serde(default)]
    code: Option<String>,
    #[serde(default)]
    level: Option<String>,
    #[serde(default)]
    quota: Option<serde_json::Value>,
}

impl ApiRefusal {
    /// Parse a failed response body. `what` names the step ("initiate",
    /// "finalize") and is used only to build a fallback sentence when the body
    /// carried no message of its own.
    pub fn parse(status: u16, body: &str, what: &str) -> Self {
        let parsed: Option<ErrorBody> = serde_json::from_str(body).ok();

        let message = parsed
            .as_ref()
            .and_then(|b| b.error.as_deref().or(b.message.as_deref()))
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string)
            .unwrap_or_else(|| fallback_message(status, body, what));

        Self {
            status,
            message,
            code: parsed.as_ref().and_then(|b| b.code.clone()),
            level: parsed.as_ref().and_then(|b| b.level.clone()),
            quota: parsed.and_then(|b| b.quota),
        }
    }

    pub fn is_auth(&self) -> bool {
        self.status == 401
    }
}

/// What to say when the server refused without an explanation — a proxy 502, an
/// HTML challenge page, an empty body. Each branch names something the user can
/// act on; the raw body is appended (clipped) only when it might be a clue.
fn fallback_message(status: u16, body: &str, what: &str) -> String {
    let base = match status {
        401 => "Your Clipy session has expired. Sign in again to keep recording.".to_string(),
        403 => "Clipy turned down this recording. Check your plan at https://clipy.online/pricing.".to_string(),
        413 => "This recording is too large to upload.".to_string(),
        426 => "This version of Clipy can no longer upload. Update from the tray menu (\"Check for updates…\").".to_string(),
        429 => "Clipy is rate-limiting uploads right now. Try again in a moment.".to_string(),
        500..=599 => format!("Clipy's servers had a problem ({status}). Your recording is safe on this Mac — try again in a moment."),
        _ => format!("Upload {what} failed ({status})."),
    };

    let snippet = body.trim();
    if snippet.is_empty() {
        return base;
    }
    let clipped: String = snippet.chars().take(MAX_RAW_BODY).collect();
    let ellipsis = if snippet.chars().count() > MAX_RAW_BODY {
        "…"
    } else {
        ""
    };
    format!("{base} (server said: {clipped}{ellipsis})")
}

/// Find the refusal inside an `anyhow` chain, past any `.context(...)` layers.
pub fn refusal_of(err: &anyhow::Error) -> Option<&ApiRefusal> {
    err.chain().find_map(|cause| cause.downcast_ref::<ApiRefusal>())
}

/// Whether the error chain holds a request that never reached the server: a
/// DNS lookup that failed ("No such host is known", os error 11001 on
/// Windows), no route, a refused connection. A typed check on reqwest's own
/// error, kept through `.context()` layers, never a match on the text.
pub fn is_offline(err: &anyhow::Error) -> bool {
    refusal_of(err).is_none()
        && err
            .chain()
            .any(|cause| cause.downcast_ref::<reqwest::Error>().is_some_and(|e| e.is_connect()))
}

/// The sentence to put in front of a user for any error. A structured refusal
/// contributes only its own message — the `.context()` breadcrumbs above it
/// ("start streaming upload: initiate raw streaming upload: …") are debugging
/// aids, not copy.
pub fn user_message(err: &anyhow::Error) -> String {
    match refusal_of(err) {
        Some(refusal) => refusal.message.clone(),
        None => format!("{err:#}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn a_request_that_cannot_connect_reads_as_offline() {
        // Port 1 on loopback: nothing listens, so the connect itself fails,
        // as it does when DNS or the network is gone.
        let err = reqwest::Client::builder()
            .no_proxy()
            .build()
            .unwrap()
            .post("http://127.0.0.1:1/api/videos/raw-upload/initiate")
            .send()
            .await
            .unwrap_err();
        let wrapped = anyhow::Error::new(err)
            .context("initiate request")
            .context("initiate raw streaming upload")
            .context("start recovery upload");
        assert!(is_offline(&wrapped));
        assert!(!is_offline(&anyhow::anyhow!("error sending request: dns error")));
        let refused: anyhow::Error = ApiRefusal::parse(403, QUOTA_BODY, "initiate").into();
        assert!(!is_offline(&refused.context("start streaming upload")));
    }

    const QUOTA_BODY: &str = r#"{"error":"You've made 3 recordings and recorded 2h 0m — both free allowances are used up. Upgrade to Clipy Pro at https://clipy.online/pricing to keep recording.","code":"pro_required","level":"quota","quota":{"pro":false,"recordings":{"used":3,"limit":3,"remaining":0,"ratio":1},"minutes":{"used":120,"limit":120,"remaining":0,"ratio":1},"exceeded":true,"breach":"both"}}"#;

    #[test]
    fn quota_refusal_shows_only_the_sentence() {
        let refusal = ApiRefusal::parse(403, QUOTA_BODY, "initiate");
        assert!(refusal.message.starts_with("You've made 3 recordings"));
        assert!(!refusal.message.contains('{'));
        assert_eq!(refusal.code.as_deref(), Some("pro_required"));
        assert_eq!(refusal.level.as_deref(), Some("quota"));
        assert_eq!(
            refusal.quota.as_ref().unwrap()["recordings"]["used"],
            serde_json::json!(3)
        );
    }

    #[test]
    fn refusal_survives_context_layers() {
        let err = anyhow::Error::new(ApiRefusal::parse(403, QUOTA_BODY, "initiate"))
            .context("initiate raw streaming upload")
            .context("start streaming upload");
        let found = refusal_of(&err).expect("refusal should survive context");
        assert_eq!(found.level.as_deref(), Some("quota"));
        // The whole point: the user sentence carries no breadcrumbs.
        assert!(!user_message(&err).contains("start streaming upload"));
    }

    #[test]
    fn unparseable_body_falls_back_to_a_readable_sentence() {
        let refusal = ApiRefusal::parse(502, "<html>bad gateway</html>", "finalize");
        assert!(refusal.message.starts_with("Clipy's servers had a problem (502)"));
        assert!(refusal.message.contains("bad gateway"));
        assert!(refusal.code.is_none());
    }

    #[test]
    fn a_long_opaque_body_is_clipped() {
        let body = "x".repeat(5_000);
        let refusal = ApiRefusal::parse(500, &body, "chunk");
        assert!(refusal.message.chars().count() < MAX_RAW_BODY + 200);
        assert!(refusal.message.ends_with("…)"));
    }

    #[test]
    fn empty_body_says_something_useful() {
        let refusal = ApiRefusal::parse(401, "", "initiate");
        assert!(refusal.is_auth());
        assert_eq!(
            refusal.message,
            "Your Clipy session has expired. Sign in again to keep recording."
        );
    }

    #[test]
    fn json_without_a_message_still_falls_back() {
        let refusal = ApiRefusal::parse(400, r#"{"code":"bad_request"}"#, "chunk");
        assert!(refusal.message.starts_with("Upload chunk failed (400)"));
        assert_eq!(refusal.code.as_deref(), Some("bad_request"));
    }

    #[test]
    fn plain_errors_keep_their_full_context_chain() {
        let err = anyhow::anyhow!("socket closed").context("initiate request");
        assert_eq!(user_message(&err), "initiate request: socket closed");
    }
}
