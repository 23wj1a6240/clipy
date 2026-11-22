//! Per-recording intent captured in the panel before Start.
//!
//! The Notes panel's title/description and the Public panel's share scope are
//! chosen *before* recording, but they are only needed at `/complete`, which
//! runs at the end of the upload. Rather than thread them through
//! `RecordingController::start` and both platform capture paths, the command
//! stashes them here keyed by recording id and
//! `upload::complete_request_body` reads them back — the same shape
//! `agent_bridge::agent_complete_extras` already uses for agent recordings.

use std::collections::HashMap;

use parking_lot::Mutex;
use serde::{Deserialize, Serialize};

use crate::types::{normalize_share_domains, RecordingNotesDto, ShareScopeDto};

/// Bounded so a long session cannot grow this map without limit. Only the
/// in-flight recording and a few recent ones are ever read.
const MAX_TRACKED: usize = 8;

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct RecordingIntentDto {
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub share_scope: ShareScopeDto,
    #[serde(default)]
    pub share_domains: Vec<String>,
}

impl RecordingIntentDto {
    pub fn normalized(self) -> Self {
        let notes = RecordingNotesDto {
            title: self.title,
            description: self.description,
        }
        .normalized();
        Self {
            title: notes.title,
            description: notes.description,
            share_scope: self.share_scope,
            share_domains: if self.share_scope == ShareScopeDto::Restricted {
                normalize_share_domains(self.share_domains)
            } else {
                // Domains only mean anything in `restricted`. Dropping them
                // otherwise keeps us from sending the server an allowlist it
                // would ignore — and from looking like we did something.
                Vec::new()
            },
        }
    }
}

static INTENTS: Mutex<Option<HashMap<String, RecordingIntentDto>>> = Mutex::new(None);
/// The user's standing share-scope preference, mirrored from the settings
/// store. The fallback for every upload with no per-recording intent.
static DEFAULT_SHARING: Mutex<Option<DefaultSharing>> = Mutex::new(None);

/// The standing sharing preference. Scope and domains travel together: a
/// `restricted` scope with no domains denies every viewer it was meant to
/// allow, so caching the scope alone is worse than caching nothing.
#[derive(Clone, Debug, PartialEq)]
pub struct DefaultSharing {
    pub scope: ShareScopeDto,
    pub domains: Vec<String>,
}

/// Stashed once the controller has minted a recording id. Safe to record
/// after the capture has started: the intent is only read by `/complete`,
/// which cannot run until the user stops.
pub fn remember(recording_id: &str, intent: RecordingIntentDto) {
    let intent = intent.normalized();
    let mut guard = INTENTS.lock();
    let map = guard.get_or_insert_with(HashMap::new);
    if map.len() >= MAX_TRACKED {
        map.clear();
    }
    map.insert(recording_id.to_string(), intent);
}

pub fn intent_for(recording_id: &str) -> Option<RecordingIntentDto> {
    INTENTS
        .lock()
        .as_ref()
        .and_then(|map| map.get(recording_id).cloned())
}

/// Mirror of the persisted sharing preference, refreshed whenever the
/// settings store is read or written.
pub fn set_default_sharing(scope: ShareScopeDto, domains: Vec<String>) {
    *DEFAULT_SHARING.lock() = Some(DefaultSharing {
        scope,
        // Same rule as a per-recording intent: domains only mean anything
        // in `restricted`.
        domains: if scope == ShareScopeDto::Restricted {
            normalize_share_domains(domains)
        } else {
            Vec::new()
        },
    });
}

/// The scope to send when a recording carries no intent of its own — a
/// recovered orphan upload (which mints a fresh id), an agent/tray/hotkey
/// start, or a recording whose intent did not survive an app restart.
///
/// `None` means "say nothing and let the server keep the account default",
/// which is the only honest answer before the settings store has been read.
pub fn default_sharing() -> Option<DefaultSharing> {
    DEFAULT_SHARING.lock().clone()
}

/// Back to "the settings store has not been read yet".
#[cfg(test)]
pub(crate) fn reset_default_sharing() {
    *DEFAULT_SHARING.lock() = None;
}

/// Serializes the tests that assert on this module's process-wide state:
/// cargo runs tests in one binary on parallel threads.
#[cfg(test)]
pub(crate) static TEST_LOCK: Mutex<()> = Mutex::new(());

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalizing_trims_notes_and_scopes_domains() {
        let _guard = TEST_LOCK.lock();
        let intent = RecordingIntentDto {
            title: Some("  Bug repro  ".to_string()),
            description: Some("   ".to_string()),
            share_scope: ShareScopeDto::Restricted,
            share_domains: vec!["@Clipy.Online".to_string(), "bad".to_string()],
        }
        .normalized();
        assert_eq!(intent.title.as_deref(), Some("Bug repro"));
        assert!(intent.description.is_none());
        assert_eq!(intent.share_domains, vec!["clipy.online".to_string()]);
    }

    #[test]
    fn domains_are_dropped_outside_restricted() {
        let _guard = TEST_LOCK.lock();
        let intent = RecordingIntentDto {
            share_scope: ShareScopeDto::Public,
            share_domains: vec!["clipy.online".to_string()],
            ..Default::default()
        }
        .normalized();
        assert!(intent.share_domains.is_empty());
    }

    #[test]
    fn remembered_intent_reads_back_by_recording_id() {
        let _guard = TEST_LOCK.lock();
        remember(
            "rec-walkthrough",
            RecordingIntentDto {
                title: Some("Walkthrough".to_string()),
                ..Default::default()
            },
        );
        assert_eq!(
            intent_for("rec-walkthrough").and_then(|i| i.title),
            Some("Walkthrough".to_string())
        );
    }

    #[test]
    fn unknown_recording_has_no_intent() {
        let _guard = TEST_LOCK.lock();
        assert!(intent_for("never-started").is_none());
    }

    #[test]
    fn default_sharing_is_absent_until_settings_are_read() {
        let _guard = TEST_LOCK.lock();
        reset_default_sharing();
        // Before the store is read there is no preference to assert, and
        // guessing one would be worse than letting the server decide.
        assert!(default_sharing().is_none());

        set_default_sharing(ShareScopeDto::Private, Vec::new());
        assert_eq!(
            default_sharing().map(|sharing| sharing.scope),
            Some(ShareScopeDto::Private)
        );
        reset_default_sharing();
    }

    #[test]
    fn a_restricted_default_keeps_its_domains() {
        let _guard = TEST_LOCK.lock();
        // Scope without domains is `restricted` with an empty allowlist,
        // which denies every viewer the preference was meant to allow.
        set_default_sharing(
            ShareScopeDto::Restricted,
            vec!["@Clipy.Online".to_string(), "bad".to_string()],
        );
        let sharing = default_sharing().expect("cached");
        assert_eq!(sharing.scope, ShareScopeDto::Restricted);
        assert_eq!(sharing.domains, vec!["clipy.online".to_string()]);
        reset_default_sharing();
    }

    #[test]
    fn domains_are_dropped_from_a_non_restricted_default() {
        let _guard = TEST_LOCK.lock();
        set_default_sharing(ShareScopeDto::Public, vec!["clipy.online".to_string()]);
        assert!(default_sharing().expect("cached").domains.is_empty());
        reset_default_sharing();
    }
}
