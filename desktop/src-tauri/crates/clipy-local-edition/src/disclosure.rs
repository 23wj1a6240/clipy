//! What the local edition tells the user and the AREC about where data went.
//!
//! The record is kept per recording in `disclosure.json` beside the AREC and
//! is written *before* each request leaves the computer, so a document can
//! never understate what was sent, even when the request then failed. It only
//! grows: a Retry with other settings adds to it.

use serde::{Deserialize, Serialize};

use crate::settings::TranscriptionPlan;
use clipy_local_moments::PlannerError;

/// File name of the record, in the recording's folder.
pub const FILE: &str = "disclosure.json";

/// AREC status when every local step finished but moment selection did not.
/// In the local edition the rules always answer, so this is only reached
/// when something unexpected failed.
pub const SELECTION_UNAVAILABLE_STATUS: &str = "Partial: moment selection unavailable. Highlight gestures found in the interaction sidecar and overview screenshots were extracted on this computer; saved evidence remains usable.";

/// The capture-gap line for [`SELECTION_UNAVAILABLE_STATUS`].
pub const SELECTION_UNAVAILABLE_GAP: &str = "- Moment selection was unavailable, so spoken references, clicks and pointer rests have no screenshots. The transcript is unaffected. Retry to add them.\n";

/// Where something was sent: a display name and the host.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Recipient {
    pub name: String,
    pub host: String,
}

impl Recipient {
    /// A model on this computer (Ollama, LM Studio, or a custom URL on the
    /// loopback interface): nothing leaves the machine.
    pub fn is_this_computer(&self) -> bool {
        let host = self.host.trim_matches(|c| c == '[' || c == ']');
        host.eq_ignore_ascii_case("localhost")
            || host
                .parse::<std::net::IpAddr>()
                .is_ok_and(|ip| ip.is_loopback())
    }

    fn describe(&self) -> String {
        format!("{} ({})", self.name, self.host)
    }
}

/// What happened to spoken cues on the last run, for the capture gaps.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "outcome", rename_all = "camelCase")]
pub enum SpokenOutcome {
    /// No model provider: rules only, by choice.
    NotConfigured,
    /// A provider is chosen but could not run; the message says why.
    Unusable { message: String },
    /// There was no transcript, so no provider was asked.
    NoTranscript,
    /// The provider answered; its spoken cues are in the plan.
    Answered { recipient: Recipient },
    /// The provider was asked and did not give usable cues.
    Failed {
        recipient: Recipient,
        reason: String,
    },
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Disclosure {
    /// Providers that received the recording's audio.
    #[serde(default)]
    pub audio: Vec<Recipient>,
    /// Model providers that received the transcript text, click labels and a
    /// bounded sample of pointer and key events.
    #[serde(default)]
    pub text: Vec<Recipient>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub spoken: Option<SpokenOutcome>,
}

impl Disclosure {
    /// An unreadable record reads as empty. The app writes it before every
    /// send, so this only loses information if the disk lost it.
    pub fn from_bytes(bytes: Option<&[u8]>) -> Self {
        bytes
            .and_then(|b| serde_json::from_slice(b).ok())
            .unwrap_or_default()
    }

    pub fn to_bytes(&self) -> Vec<u8> {
        serde_json::to_vec_pretty(self).unwrap_or_default()
    }

    /// Record that the audio is about to go to `recipient`.
    pub fn add_audio(&mut self, recipient: Recipient) {
        if !self.audio.contains(&recipient) {
            self.audio.push(recipient);
        }
    }

    /// Record that the transcript text and interaction sample are about to
    /// go to `recipient`.
    pub fn add_text(&mut self, recipient: Recipient) {
        if !self.text.contains(&recipient) {
            self.text.push(recipient);
        }
    }

    /// The AREC's "Evidence boundaries" statement for this recording.
    pub fn evidence_boundary(&self, device: &str) -> String {
        let remote: Vec<&Recipient> = self.text.iter().filter(|r| !r.is_this_computer()).collect();
        let on_device: Vec<&Recipient> =
            self.text.iter().filter(|r| r.is_this_computer()).collect();
        let mut out = String::new();
        if self.audio.is_empty() {
            out.push_str(&format!(
                "Video, audio and screenshots stay on this {device}."
            ));
        } else {
            out.push_str(&format!(
                "Video and screenshots stay on this {device}. The recording's audio was sent to {} for transcription, with your own key.",
                list(&self.audio.iter().collect::<Vec<_>>())
            ));
        }
        if remote.is_empty() {
            out.push_str(&format!(
                " The transcript and the interaction sidecar stay on this {device}."
            ));
        } else {
            out.push_str(&format!(
                " The transcript text, click labels and a bounded sample of pointer and key events were sent to {} to pick spoken moments.",
                list(&remote)
            ));
        }
        if !on_device.is_empty() {
            out.push_str(&format!(
                " A model running on this computer, {}, read the transcript text, click labels and a bounded sample of pointer and key events.",
                list(&on_device)
            ));
        }
        if self.audio.is_empty() && remote.is_empty() {
            out.push_str(" Nothing from this recording left this computer.");
        }
        out
    }

    /// Capture-gap lines about spoken cues, without the list marker.
    pub fn capture_gaps(&self) -> Vec<String> {
        let rules = "Moments come from the rules on this computer: highlights, drawings, clicks and pointer rests.";
        match &self.spoken {
            None | Some(SpokenOutcome::Answered { .. }) => Vec::new(),
            Some(SpokenOutcome::NotConfigured) => vec![format!(
                "Spoken references were not selected (no model provider configured). {rules}"
            )],
            Some(SpokenOutcome::Unusable { message }) => vec![format!(
                "Spoken references were not selected: {} {rules}",
                sentence(message)
            )],
            Some(SpokenOutcome::NoTranscript) => vec![format!(
                "Spoken references were not selected because there is no transcript to read. {rules}"
            )],
            Some(SpokenOutcome::Failed { recipient, reason }) => vec![format!(
                "Spoken references were not selected: {} {}. {rules} Retry to ask again.",
                recipient.describe(),
                reason.trim_end_matches('.')
            )],
        }
    }
}

fn sentence(text: &str) -> String {
    let text = text.trim();
    if text.ends_with(['.', '!', '?']) {
        text.to_string()
    } else {
        format!("{text}.")
    }
}

fn list(recipients: &[&Recipient]) -> String {
    let names: Vec<String> = recipients.iter().map(|r| r.describe()).collect();
    match names.as_slice() {
        [] => String::new(),
        [one] => one.clone(),
        [rest @ .., last] => format!("{} and {last}", rest.join(", ")),
    }
}

/// Why a model provider gave no spoken cues, in a few words that follow its
/// name ("OpenRouter (openrouter.ai) did not answer in time"). Never holds a
/// key, the transcript or the provider's answer.
pub fn failure_reason(error: &PlannerError) -> String {
    match error {
        PlannerError::Http { status, .. } if matches!(status, 401 | 403) => {
            format!("answered HTTP {status}; check the API key in Settings")
        }
        PlannerError::Http { status, .. } => format!("answered HTTP {status}"),
        PlannerError::Redirect { status } => {
            format!("answered with a redirect (HTTP {status}), which is not followed")
        }
        PlannerError::EmptyCompletion { .. } | PlannerError::Unparseable { .. } => {
            "returned an answer that could not be read".into()
        }
        PlannerError::TooLarge { .. } => "returned an answer that was too large".into(),
        PlannerError::InvalidOutput { .. } => "returned moments that could not be used".into(),
        PlannerError::Timeout => "did not answer in time".into(),
        PlannerError::Network { .. } => "could not be reached".into(),
        PlannerError::InvalidInput(_) => {
            "was not asked, because this recording is outside the planner's limits".into()
        }
        PlannerError::InvalidConfig(_) => "was not asked, because its settings are unusable".into(),
    }
}

/// The status line while a recording is transcribed.
pub fn transcribing_message(device: &str, plan: &TranscriptionPlan) -> String {
    match plan {
        TranscriptionPlan::Apple => format!("Transcribing on this {device} in the background"),
        TranscriptionPlan::Local(model) => format!(
            "Waiting for the recording to finish, then transcribing on this {device} with {}",
            model.label()
        ),
        TranscriptionPlan::Provider { recipient, .. } => format!(
            "Waiting for the recording to finish, then transcribing with {} using your key",
            recipient.name
        ),
        TranscriptionPlan::Off => {
            "No transcription is set up, so this recording is not transcribed".into()
        }
    }
}

/// Rewrites the evidence-boundary statement in an AREC that is already on
/// disk, so a reader never sees less than what was sent: an agent reading the
/// handoff file while a request is in flight, or after the app was killed
/// during one. `None` means the document does not carry `before`, and the
/// caller must not send until it can be corrected.
pub fn refresh_boundary(doc: &str, before: &str, after: &str) -> Option<String> {
    if before == after {
        return Some(doc.to_string());
    }
    if before.is_empty() || !doc.contains(before) {
        return None;
    }
    Some(doc.replace(before, after))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::settings::LocalModelId;

    fn r(name: &str, host: &str) -> Recipient {
        Recipient {
            name: name.into(),
            host: host.into(),
        }
    }

    fn assert_house_style(text: &str) {
        assert!(!text.contains('\u{2014}'), "no em dashes: {text}");
        assert!(!text.to_lowercase().contains("for developers"), "{text}");
        assert!(!text.contains("Clipy"), "names no hosted service: {text}");
        assert!(!text.contains("OpenRouter") || text.contains("openrouter.ai"));
    }

    #[test]
    fn nothing_sent_says_nothing_left() {
        let d = Disclosure::default();
        let text = d.evidence_boundary("Mac");
        assert_eq!(
            text,
            "Video, audio and screenshots stay on this Mac. The transcript and the interaction sidecar stay on this Mac. Nothing from this recording left this computer."
        );
        assert_house_style(&text);
    }

    #[test]
    fn model_provider_is_named_with_what_it_received() {
        let mut d = Disclosure::default();
        d.add_text(r("OpenRouter", "openrouter.ai"));
        d.add_text(r("OpenRouter", "openrouter.ai"));
        let text = d.evidence_boundary("PC");
        assert_eq!(
            text,
            "Video, audio and screenshots stay on this PC. The transcript text, click labels and a bounded sample of pointer and key events were sent to OpenRouter (openrouter.ai) to pick spoken moments."
        );
        assert_house_style(&text);
    }

    #[test]
    fn audio_provider_is_named() {
        let mut d = Disclosure::default();
        d.add_audio(r("Deepgram", "api.deepgram.com"));
        let text = d.evidence_boundary("PC");
        assert!(text.starts_with("Video and screenshots stay on this PC. The recording's audio was sent to Deepgram (api.deepgram.com) for transcription, with your own key."));
        assert!(text.contains("The transcript and the interaction sidecar stay on this PC."));
        assert!(!text.contains("Nothing from this recording left"));
        d.add_text(r("Groq", "api.groq.com"));
        d.add_text(r("OpenAI", "api.openai.com"));
        let text = d.evidence_boundary("PC");
        assert!(text.contains("were sent to Groq (api.groq.com) and OpenAI (api.openai.com)"));
        assert_house_style(&text);
    }

    #[test]
    fn a_model_on_this_computer_is_not_a_send() {
        let mut d = Disclosure::default();
        d.add_text(r("Ollama", "127.0.0.1"));
        d.add_text(r("custom model provider", "localhost"));
        let text = d.evidence_boundary("Mac");
        assert!(text.contains("Nothing from this recording left this computer."));
        assert!(text.contains("A model running on this computer, Ollama (127.0.0.1) and custom model provider (localhost), read"));
        assert!(r("x", "[::1]").is_this_computer());
        assert!(!r("x", "192.168.1.5").is_this_computer());
        assert_house_style(&text);
    }

    #[test]
    fn record_round_trips_and_tolerates_junk() {
        let mut d = Disclosure::default();
        d.add_audio(r("Gemini", "generativelanguage.googleapis.com"));
        d.spoken = Some(SpokenOutcome::Failed {
            recipient: r("OpenAI", "api.openai.com"),
            reason: "did not answer in time".into(),
        });
        assert_eq!(Disclosure::from_bytes(Some(&d.to_bytes())), d);
        assert_eq!(Disclosure::from_bytes(Some(b"{")), Disclosure::default());
        assert_eq!(Disclosure::from_bytes(None), Disclosure::default());
    }

    #[test]
    fn capture_gaps_say_why_spoken_cues_are_missing() {
        let mut d = Disclosure::default();
        assert!(d.capture_gaps().is_empty());
        d.spoken = Some(SpokenOutcome::NotConfigured);
        assert_eq!(d.capture_gaps(), vec!["Spoken references were not selected (no model provider configured). Moments come from the rules on this computer: highlights, drawings, clicks and pointer rests.".to_string()]);
        d.spoken = Some(SpokenOutcome::Unusable {
            message: "OpenRouter needs an API key. Add it in Settings.".into(),
        });
        assert!(d.capture_gaps()[0].starts_with("Spoken references were not selected: OpenRouter needs an API key. Add it in Settings. Moments"));
        d.spoken = Some(SpokenOutcome::Failed {
            recipient: r("OpenRouter", "openrouter.ai"),
            reason: failure_reason(&PlannerError::Timeout),
        });
        assert_eq!(d.capture_gaps()[0], "Spoken references were not selected: OpenRouter (openrouter.ai) did not answer in time. Moments come from the rules on this computer: highlights, drawings, clicks and pointer rests. Retry to ask again.");
        d.spoken = Some(SpokenOutcome::Answered {
            recipient: r("OpenRouter", "openrouter.ai"),
        });
        assert!(d.capture_gaps().is_empty());
        d.spoken = Some(SpokenOutcome::NoTranscript);
        for line in d.capture_gaps() {
            assert_house_style(&line);
        }
    }

    #[test]
    fn failure_reasons_never_carry_provider_text() {
        assert_eq!(
            failure_reason(&PlannerError::Http {
                status: 401,
                transient: false
            }),
            "answered HTTP 401; check the API key in Settings"
        );
        assert_eq!(
            failure_reason(&PlannerError::Http {
                status: 503,
                transient: true
            }),
            "answered HTTP 503"
        );
        assert_eq!(
            failure_reason(&PlannerError::InvalidOutput {
                issues: vec!["secret".into()]
            }),
            "returned moments that could not be used"
        );
        assert_eq!(
            failure_reason(&PlannerError::Network { code: "connect" }),
            "could not be reached"
        );
    }

    #[test]
    fn transcribing_messages() {
        assert_eq!(
            transcribing_message("Mac", &TranscriptionPlan::Apple),
            "Transcribing on this Mac in the background"
        );
        assert_eq!(
            transcribing_message("PC", &TranscriptionPlan::Local(LocalModelId::EnglishFast)),
            "Waiting for the recording to finish, then transcribing on this PC with English, fast (Parakeet TDT-CTC 110M)"
        );
        let provider = TranscriptionPlan::Provider {
            kind: crate::settings::TranscriptionKind::Deepgram,
            recipient: r("Deepgram", "api.deepgram.com"),
        };
        assert_eq!(
            transcribing_message("PC", &provider),
            "Waiting for the recording to finish, then transcribing with Deepgram using your key"
        );
        for text in [
            transcribing_message("PC", &TranscriptionPlan::Off),
            SELECTION_UNAVAILABLE_STATUS.to_string(),
            SELECTION_UNAVAILABLE_GAP.to_string(),
        ] {
            assert_house_style(&text);
        }
    }

    #[test]
    fn refresh_boundary_replaces_the_statement_in_place() {
        let doc = "# Local recording\n\n## Evidence boundaries\n\nIntro. OLD STATEMENT\n\n## Transcript\n";
        let out = refresh_boundary(doc, "OLD STATEMENT", "NEW STATEMENT").unwrap();
        assert_eq!(out, doc.replace("OLD STATEMENT", "NEW STATEMENT"));
        assert!(!out.contains("OLD STATEMENT"));
    }

    #[test]
    fn refresh_boundary_refuses_a_document_without_the_old_statement() {
        assert_eq!(refresh_boundary("no statement here", "OLD", "NEW"), None);
        assert_eq!(refresh_boundary("anything", "", "NEW"), None);
    }

    #[test]
    fn refresh_boundary_is_a_no_op_when_nothing_changed() {
        assert_eq!(
            refresh_boundary("doc", "SAME", "SAME").as_deref(),
            Some("doc")
        );
    }

    #[test]
    fn a_text_recipient_changes_the_statement_the_arec_must_carry() {
        let before = Disclosure::default().evidence_boundary("Mac");
        let mut d = Disclosure::default();
        d.add_text(Recipient {
            name: "OpenRouter".into(),
            host: "openrouter.ai".into(),
        });
        let after = d.evidence_boundary("Mac");
        assert_ne!(
            before, after,
            "sending text must change the AREC's statement"
        );
        assert!(after.contains("openrouter.ai"));
    }
}
