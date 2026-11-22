//! Local edition stand-in for `auth.rs`: there is no Clipy account. Built only
//! without the `cloud` feature (`lib.rs` swaps it in with `#[path]`), and it
//! exposes only what shared code still names. Nobody is ever signed in.

use std::path::PathBuf;

/// Same event name as the official app, so a listener that shared code
/// registers compiles; nothing emits it here.
pub(crate) const AUTH_STATE_EVENT: &str = "auth-state-changed";

pub struct AuthState;

impl AuthState {
    pub fn new(
        _api_base: String,
        _pending_path: Option<PathBuf>,
        _session_path: Option<PathBuf>,
    ) -> Self {
        Self
    }

    /// Always `None`: the local edition has no session.
    pub fn bearer_token(&self) -> Option<String> {
        None
    }
}

/// Mirrors `auth::is_offline_error`, used when logging a failed deep link.
/// No local deep link involves the network.
pub fn is_offline_error(_err: &anyhow::Error) -> bool {
    false
}
