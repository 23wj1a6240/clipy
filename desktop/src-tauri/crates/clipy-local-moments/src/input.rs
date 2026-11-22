//! The planner input: the same fields the TypeScript planner takes
//! (`localMomentInput` in `@clipy/moments`), with interaction events, click
//! elements and capture geometry named as in the desktop sidecar
//! (`recording.cursor.json`).

use serde::{Deserialize, Serialize};

use crate::js;

/// Everything the planner reads. Unknown fields (the sidecar's `cursor_type`,
/// `render_x`, the TypeScript `schemaVersion` and `interactionSelection`) are
/// ignored.
#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Input {
    /// Length of the recording, in milliseconds. Must be positive.
    pub duration_ms: u64,
    /// Transcript segments (word or sentence level), on the recording clock.
    #[serde(default)]
    pub transcript: Vec<TranscriptSegment>,
    /// Interaction events from the sidecar's `events`.
    #[serde(default)]
    pub interactions: Vec<InteractionEvent>,
    /// Accessibility labels of clicked elements, from the sidecar's `click_elements`.
    #[serde(default)]
    pub click_elements: Vec<ClickElement>,
    /// Captured area in the coordinate space of the events. Without it no
    /// moment carries `x`/`y`, and pen strokes cannot be placed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub capture: Option<CaptureGeometry>,
    /// Where the captured area was over time (a window-following take), from
    /// the sidecar's `capture_timeline`. Each sample applies from its
    /// `time_ms` on; before the first one, `capture` applies. The TypeScript
    /// planner input has no such field, so leaving it empty gives exactly its
    /// output.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub capture_timeline: Vec<CaptureSample>,
    /// Key code space of `key_down`/`key_up` events, which decides how a
    /// hold-Control flare is recognised. The TypeScript local planner always
    /// reads Mac virtual key codes; a Windows sidecar records virtual-key
    /// codes (Control is 17) and should pass `dom`.
    #[serde(default)]
    pub keycode_dialect: KeycodeDialect,
}

/// One transcript segment.
#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct TranscriptSegment {
    pub start_ms: f64,
    pub end_ms: f64,
    pub text: String,
}

/// One interaction event, with the sidecar's field names.
#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq)]
pub struct InteractionEvent {
    /// `move`, `down`, `up`, `scroll`, `key_down`, `key_up` or `annotation`.
    pub kind: String,
    pub time_ms: f64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub x: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub y: Option<f64>,
    /// `down`/`up`: 1 is the primary button on the Mac.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub button: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub key_code: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub delta_x: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub delta_y: Option<f64>,
    /// `annotation` events: the pen overlay switching on (true) or off.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub active: Option<bool>,
}

/// The accessibility element under a click.
#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq)]
pub struct ClickElement {
    pub time_ms: f64,
    #[serde(default)]
    pub role: Option<String>,
    #[serde(default)]
    pub label: Option<String>,
}

/// The captured area, in the coordinate space of the interaction events.
#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize, PartialEq)]
pub struct CaptureGeometry {
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
    /// Encoded video size; only used when the area itself is unusable.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output_width: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output_height: Option<f64>,
}

/// The captured area from `time_ms` on.
#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize, PartialEq)]
pub struct CaptureSample {
    pub time_ms: f64,
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
}

/// Key code space of a sidecar's key events.
#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum KeycodeDialect {
    /// macOS virtual key codes: Control is 59 or 62, modifiers are 54 to 63.
    #[default]
    Mac,
    /// DOM `keyCode` / Windows virtual-key codes: Control is 17.
    Dom,
}

/// Why an input was refused. Names the field, never its content.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
#[error("invalid planner input: {0}")]
pub struct InputError(pub String);

/// Largest time the hosted planner accepts: 12 hours.
const MAX_TIME_MS: f64 = 43_200_000.0;
const EVENT_KINDS: [&str; 7] = [
    "move",
    "down",
    "up",
    "scroll",
    "key_down",
    "key_up",
    "annotation",
];

impl Input {
    /// The limits the TypeScript planner enforces on its input (`localMomentInput`):
    /// at most 8000 interaction events (a bounded sample), 2000 click
    /// elements and 100,000 transcript segments; times within 12 hours;
    /// transcript inside the recording. `plan_with_provider` checks this
    /// before anything is sent; `plan_rules` works on any input, including a
    /// full sidecar.
    pub fn validate(&self) -> Result<(), InputError> {
        let err = |s: &str| Err(InputError(s.to_string()));
        let time_ok = |t: f64| t.is_finite() && (0.0..=MAX_TIME_MS).contains(&t);
        let duration = self.duration_ms as f64;
        if self.duration_ms == 0 || duration > MAX_TIME_MS {
            return err("durationMs");
        }
        if self.transcript.len() > 100_000 {
            return err("transcript");
        }
        for (i, s) in self.transcript.iter().enumerate() {
            if !time_ok(s.start_ms)
                || !time_ok(s.end_ms)
                || s.start_ms > s.end_ms
                || s.end_ms > duration
            {
                return Err(InputError(format!("transcript.{i}")));
            }
            if js::utf16_len(&s.text) > 10_000 {
                return Err(InputError(format!("transcript.{i}.text")));
            }
        }
        if self.interactions.len() > 8000 {
            return err("interactions");
        }
        for (i, e) in self.interactions.iter().enumerate() {
            let int_in = |v: Option<f64>, max: f64| {
                v.is_none_or(|n| n.fract() == 0.0 && (0.0..=max).contains(&n))
            };
            let finite = |v: Option<f64>| v.is_none_or(f64::is_finite);
            if !EVENT_KINDS.contains(&e.kind.as_str())
                || !time_ok(e.time_ms)
                || !finite(e.x)
                || !finite(e.y)
                || !finite(e.delta_x)
                || !finite(e.delta_y)
                || !int_in(e.button, 255.0)
                || !int_in(e.key_code, 65_535.0)
            {
                return Err(InputError(format!("interactions.{i}")));
            }
        }
        if self.click_elements.len() > 2000 {
            return err("clickElements");
        }
        for (i, c) in self.click_elements.iter().enumerate() {
            let long = |s: &Option<String>, max: usize| {
                s.as_deref().is_some_and(|s| js::utf16_len(s) > max)
            };
            if !time_ok(c.time_ms) || long(&c.role, 256) || long(&c.label, 4000) {
                return Err(InputError(format!("clickElements.{i}")));
            }
        }
        if let Some(c) = &self.capture {
            let positive_int = |v: Option<f64>| v.is_none_or(|n| n.fract() == 0.0 && n > 0.0);
            if !c.x.is_finite()
                || !c.y.is_finite()
                || !(c.width.is_finite() && c.width > 0.0)
                || !(c.height.is_finite() && c.height > 0.0)
                || !positive_int(c.output_width)
                || !positive_int(c.output_height)
            {
                return err("capture");
            }
        }
        Ok(())
    }
}
