//! The interaction sidecar (`recording.cursor.json`) and the transcript, as
//! the moment planner's [`Input`]; and a plan back as the v2 JSON that the
//! app's `validate_moments` already reads for the hosted plan.
//!
//! Field names follow the sidecar the desktop app writes (`cursor_track.rs`):
//! `events` (`kind`, `time_ms`, `x`, `y`, `button`, `key_code`, `delta_x`,
//! `delta_y`, `active`), `click_elements` (`time_ms`, `role`, `label`),
//! `capture` and `capture_timeline` (`x`, `y`, `width`, `height`,
//! `output_width`, `output_height`; timeline samples add `time_ms`). Anything
//! else in the sidecar (cursor shapes, source metadata, render positions) is
//! never copied, so it cannot reach a provider.

use clipy_local_moments::{
    CaptureGeometry, CaptureSample, ClickElement, Input, InteractionEvent, KeycodeDialect, Plan,
    TranscriptSegment,
};
use serde_json::{json, Value};

use crate::text::{clip_bytes, clip_utf16, utf16_len};

/// Interaction events a provider request carries (the planner's input
/// limit). Gesture evidence (button, key and annotation edges) is kept
/// first; scrolls and moves are strided into what is left.
pub const PROVIDER_EVENT_BUDGET: usize = 8000;
/// Click labels a provider request carries.
pub const PROVIDER_CLICK_ELEMENTS: usize = 2000;
/// Longest reason the app's `validate_moments` accepts, in bytes.
pub const MAX_REASON_BYTES: usize = 1000;

/// The key code space of the desktop sidecar. The Windows recorder writes
/// macOS virtual key codes too (`current_keyboard_state` in
/// `cursor_track.rs` maps Control to 59), so a flare is recognised the same
/// way on both platforms. `KeycodeDialect::Dom` is for sidecars that record
/// Windows virtual-key codes, which this app does not write.
pub const SIDECAR_KEYCODES: KeycodeDialect = KeycodeDialect::Mac;

const MAX_TIME_MS: f64 = 43_200_000.0;
const KINDS: [&str; 7] = [
    "move",
    "down",
    "up",
    "scroll",
    "key_down",
    "key_up",
    "annotation",
];
const MANDATORY: [&str; 5] = ["down", "up", "key_down", "key_up", "annotation"];

fn finite(value: &Value, key: &str) -> Option<f64> {
    value
        .get(key)
        .and_then(Value::as_f64)
        .filter(|n| n.is_finite())
}

fn time(value: &Value) -> Option<f64> {
    finite(value, "time_ms").filter(|t| (0.0..=MAX_TIME_MS).contains(t))
}

fn integer_in(value: &Value, key: &str, max: f64) -> Option<f64> {
    finite(value, key).filter(|n| n.fract() == 0.0 && (0.0..=max).contains(n))
}

fn event(value: &Value) -> Option<InteractionEvent> {
    let kind = value.get("kind")?.as_str()?;
    if !KINDS.contains(&kind) {
        return None;
    }
    Some(InteractionEvent {
        kind: kind.to_string(),
        time_ms: time(value)?,
        x: finite(value, "x"),
        y: finite(value, "y"),
        button: integer_in(value, "button", 255.0),
        key_code: integer_in(value, "key_code", 65_535.0),
        delta_x: finite(value, "delta_x"),
        delta_y: finite(value, "delta_y"),
        active: value.get("active").and_then(Value::as_bool),
    })
}

/// Every usable sidecar event, in time order (stable for equal times).
fn events(sidecar: &Value) -> Vec<InteractionEvent> {
    let mut events: Vec<InteractionEvent> = sidecar
        .get("events")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(event)
        .collect();
    events.sort_by(|a, b| a.time_ms.total_cmp(&b.time_ms));
    events
}

/// Transcript segments from the app's `{startMs, endMs, text}` values, inside
/// the recording and within the planner's per-segment limit.
fn transcript(segments: &[Value], duration_ms: u64) -> Vec<TranscriptSegment> {
    let duration = duration_ms as f64;
    segments
        .iter()
        .filter_map(|s| {
            let start = finite(s, "startMs")?.max(0.0);
            let end = finite(s, "endMs")?.min(duration);
            let text = s.get("text")?.as_str()?;
            if start > end || start >= duration || start > MAX_TIME_MS {
                return None;
            }
            Some(TranscriptSegment {
                start_ms: start,
                end_ms: end,
                text: if utf16_len(text) > 10_000 {
                    clip_utf16(text, 10_000)
                } else {
                    text.to_string()
                },
            })
        })
        .take(100_000)
        .collect()
}

fn click_elements(sidecar: &Value) -> Vec<ClickElement> {
    let short =
        |v: Option<&Value>, max: usize| v.and_then(Value::as_str).map(|s| clip_utf16(s, max));
    sidecar
        .get("click_elements")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|c| {
            Some(ClickElement {
                time_ms: time(c)?,
                role: short(c.get("role"), 256),
                label: short(c.get("label"), 4000),
            })
        })
        .collect()
}

fn rect(value: &Value) -> Option<(f64, f64, f64, f64)> {
    let (x, y) = (finite(value, "x")?, finite(value, "y")?);
    let width = finite(value, "width").filter(|w| *w > 0.0)?;
    let height = finite(value, "height").filter(|h| *h > 0.0)?;
    Some((x, y, width, height))
}

fn capture(sidecar: &Value) -> Option<CaptureGeometry> {
    let value = sidecar.get("capture")?;
    let (x, y, width, height) = rect(value)?;
    let positive_int = |key| finite(value, key).filter(|n| n.fract() == 0.0 && *n > 0.0);
    Some(CaptureGeometry {
        x,
        y,
        width,
        height,
        output_width: positive_int("output_width"),
        output_height: positive_int("output_height"),
    })
}

fn capture_timeline(sidecar: &Value) -> Vec<CaptureSample> {
    let mut samples: Vec<CaptureSample> = sidecar
        .get("capture_timeline")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|s| {
            let (x, y, width, height) = rect(s)?;
            Some(CaptureSample {
                time_ms: time(s)?,
                x,
                y,
                width,
                height,
            })
        })
        .collect();
    samples.sort_by(|a, b| a.time_ms.total_cmp(&b.time_ms));
    samples
}

fn input(segments: &[Value], sidecar: &Value, duration_ms: u64) -> Input {
    Input {
        duration_ms,
        transcript: transcript(segments, duration_ms),
        interactions: events(sidecar),
        click_elements: click_elements(sidecar),
        capture: capture(sidecar),
        capture_timeline: capture_timeline(sidecar),
        keycode_dialect: SIDECAR_KEYCODES,
    }
}

/// Everything, for `plan_rules`, which never leaves the computer.
pub fn rules_input(segments: &[Value], sidecar: &Value, duration_ms: u64) -> Input {
    input(segments, sidecar, duration_ms)
}

/// What a model provider may receive: the transcript, click labels, capture
/// geometry and a bounded sample of interaction events. Passes
/// `Input::validate` for any recording up to twelve hours.
pub fn provider_input(segments: &[Value], sidecar: &Value, duration_ms: u64) -> Input {
    let mut input = input(segments, sidecar, duration_ms);
    let all = std::mem::take(&mut input.interactions);
    if all.len() > PROVIDER_EVENT_BUDGET {
        let mandatory: Vec<usize> = (0..all.len())
            .filter(|&i| MANDATORY.contains(&all[i].kind.as_str()))
            .take(PROVIDER_EVENT_BUDGET)
            .collect();
        let remaining = PROVIDER_EVENT_BUDGET - mandatory.len();
        let scrolls: Vec<usize> = (0..all.len())
            .filter(|&i| all[i].kind == "scroll")
            .collect();
        let moves: Vec<usize> = (0..all.len()).filter(|&i| all[i].kind == "move").collect();
        let scrolls = stride(&scrolls, (remaining / 2).min(scrolls.len()));
        let moves = stride(&moves, remaining - scrolls.len());
        let mut keep: Vec<usize> = mandatory;
        keep.extend(scrolls);
        keep.extend(moves);
        keep.sort_unstable();
        input.interactions = keep.into_iter().map(|i| all[i].clone()).collect();
    } else {
        input.interactions = all;
    }
    input.click_elements.truncate(PROVIDER_CLICK_ELEMENTS);
    input
}

fn stride(items: &[usize], budget: usize) -> Vec<usize> {
    if budget == 0 || items.is_empty() {
        return Vec::new();
    }
    let step = items.len().div_ceil(budget).max(1);
    items.iter().step_by(step).copied().take(budget).collect()
}

/// The plan as `{"schemaVersion":2,"moments":[...]}`, the shape the app's
/// `validate_moments` reads: reasons within its byte limit, pointers inside
/// 0..1, and no moment with an empty reason.
pub fn plan_json(plan: &Plan) -> String {
    let unit = |v: Option<f64>| v.filter(|n| n.is_finite()).map(|n| n.clamp(0.0, 1.0));
    let moments: Vec<Value> = plan
        .moments
        .iter()
        .filter(|m| !m.reason.trim().is_empty())
        .map(|m| {
            let mut value = json!({
                "timestamp_ms": m.timestamp_ms,
                "frame_at_ms": m.frame_at_ms,
                "reason": clip_bytes(&m.reason, MAX_REASON_BYTES),
                "source": m.source,
            });
            if let (Some(x), Some(y)) = (unit(m.x), unit(m.y)) {
                value["x"] = json!(x);
                value["y"] = json!(y);
            }
            value
        })
        .collect();
    json!({"schemaVersion": 2, "moments": moments}).to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use clipy_local_moments::{plan_rules, MomentSource, Planner};

    fn sidecar(events: Vec<Value>) -> Value {
        json!({
            "events": events,
            "sample_hz": 60,
            "coord_space": "logical_macos",
            "capture": {"x": 0.0, "y": 0.0, "width": 1000.0, "height": 500.0,
                        "output_width": 2000, "output_height": 1000, "screen_x": 0.0},
            "capture_source": {"kind": "display", "title": "PRIVATE WINDOW TITLE"},
            "click_elements": [{"time_ms": 1000.0, "role": "AXButton", "label": "Save"}],
        })
    }

    #[test]
    fn events_keep_sidecar_fields_and_drop_everything_else() {
        let s = sidecar(vec![
            json!({"kind":"down","time_ms":1000.0,"button":1,"x":100,"y":50,"cursor_type":"pointer","render_x":3.0}),
            json!({"kind":"teleport","time_ms":5.0}),
            json!({"kind":"move","time_ms":-1.0,"x":1,"y":1}),
            json!({"kind":"key_down","time_ms":20.0,"key_code":59.5}),
            json!({"kind":"up","time_ms":1100.0,"button":1,"x":100,"y":50}),
        ]);
        let input = rules_input(&[], &s, 5000);
        assert_eq!(input.interactions.len(), 3);
        assert_eq!(input.interactions[0].kind, "key_down");
        assert_eq!(
            input.interactions[0].key_code, None,
            "non-integer key code dropped"
        );
        assert_eq!(input.interactions[1].button, Some(1.0));
        assert_eq!(input.click_elements[0].label.as_deref(), Some("Save"));
        let capture = input.capture.unwrap();
        assert_eq!(
            (capture.width, capture.output_width),
            (1000.0, Some(2000.0))
        );
        assert_eq!(input.keycode_dialect, KeycodeDialect::Mac);
        let text = serde_json::to_string(&input).unwrap();
        assert!(
            !text.contains("PRIVATE")
                && !text.contains("cursor_type")
                && !text.contains("render_x")
        );
    }

    #[test]
    fn provider_input_is_bounded_and_keeps_gesture_edges() {
        let mut events = Vec::new();
        for i in 0..20_000 {
            events.push(json!({"kind":"move","time_ms":i as f64,"x":i % 1000,"y":5}));
        }
        for i in 0..3000 {
            let t = i as f64 * 6.0 + 0.5;
            events.push(json!({"kind":"key_down","time_ms":t,"key_code":56}));
            events.push(json!({"kind":"key_up","time_ms":t + 1.0,"key_code":56}));
        }
        for i in 0..500 {
            events.push(json!({"kind":"scroll","time_ms":i as f64 * 10.0,"x":1,"y":1,"delta_x":0,"delta_y":3}));
        }
        let segments = vec![json!({"startMs":0,"endMs":25_000,"text":"past the end"})];
        let input = provider_input(&segments, &sidecar(events), 20_000);
        // The stride rounds up, so moves fill most but not all of the rest.
        let sent = input.interactions.len();
        assert!(sent <= PROVIDER_EVENT_BUDGET && sent > 7_500, "{sent}");
        assert_eq!(
            input
                .interactions
                .iter()
                .filter(|e| e.kind.starts_with("key"))
                .count(),
            6000
        );
        assert!(input.interactions.iter().any(|e| e.kind == "scroll"));
        assert!(input
            .interactions
            .windows(2)
            .all(|w| w[0].time_ms <= w[1].time_ms));
        assert_eq!(
            input.transcript[0].end_ms, 20_000.0,
            "clamped into the recording"
        );
        input.validate().expect("valid for the provider");
        // Rules see everything.
        assert_eq!(
            rules_input(&segments, &sidecar(vec![]), 20_000)
                .interactions
                .len(),
            0
        );
    }

    #[test]
    fn small_recordings_send_every_event() {
        let events: Vec<Value> = (0..10)
            .map(|i| json!({"kind":"move","time_ms":i as f64,"x":1,"y":1}))
            .collect();
        let input = provider_input(&[], &sidecar(events), 1000);
        assert_eq!(input.interactions.len(), 10);
        input.validate().unwrap();
    }

    #[test]
    fn transcript_segments_outside_the_recording_are_dropped() {
        let segments = vec![
            json!({"startMs":10,"endMs":5,"text":"backwards"}),
            json!({"startMs":2000,"endMs":2500,"text":"after"}),
            json!({"startMs":"x","endMs":5,"text":"bad"}),
            json!({"startMs":0,"endMs":500,"text":"ok"}),
        ];
        let input = provider_input(&segments, &Value::Null, 1000);
        assert_eq!(input.transcript.len(), 1);
        assert_eq!(input.transcript[0].text, "ok");
        assert!(input.capture.is_none());
        input.validate().unwrap();
    }

    #[test]
    fn capture_timeline_is_passed_through_in_order() {
        let mut s = sidecar(vec![]);
        s["capture_timeline"] = json!([
            {"time_ms": 500.0, "x": 10.0, "y": 0.0, "width": 800.0, "height": 400.0, "output_width": 1600, "output_height": 800},
            {"time_ms": 100.0, "x": 5.0, "y": 0.0, "width": 800.0, "height": 400.0, "output_width": 1600, "output_height": 800},
            {"time_ms": 900.0, "x": 5.0, "y": 0.0, "width": 0.0, "height": 400.0}
        ]);
        let input = rules_input(&[], &s, 1000);
        let times: Vec<f64> = input.capture_timeline.iter().map(|c| c.time_ms).collect();
        assert_eq!(times, vec![100.0, 500.0]);
    }

    #[test]
    fn a_control_hold_becomes_a_flare_with_a_valid_plan() {
        // Control is 59 in the sidecar on Mac and Windows alike.
        let s = sidecar(vec![
            json!({"kind":"key_down","time_ms":1000.0,"key_code":59}),
            json!({"kind":"move","time_ms":1100.0,"x":200,"y":100}),
            json!({"kind":"move","time_ms":1900.0,"x":400,"y":200}),
            json!({"kind":"move","time_ms":2400.0,"x":420,"y":210}),
            json!({"kind":"key_up","time_ms":2500.0,"key_code":59}),
        ]);
        let plan = plan_rules(&rules_input(&[], &s, 10_000));
        assert_eq!(plan.planner, Planner::Rules);
        let flare = plan
            .moments
            .iter()
            .find(|m| m.source == MomentSource::Flare)
            .expect("a flare moment");
        assert!(flare.frame_at_ms > flare.timestamp_ms);
        let parsed: Value = serde_json::from_str(&plan_json(&plan)).unwrap();
        assert_eq!(parsed["schemaVersion"], 2);
        let moments = parsed["moments"].as_array().unwrap();
        assert_eq!(moments.len(), plan.moments.len());
        for m in moments {
            // The fields the app's `Moment` (deny_unknown_fields) accepts.
            for key in m.as_object().unwrap().keys() {
                assert!(
                    ["timestamp_ms", "frame_at_ms", "reason", "source", "x", "y"]
                        .contains(&key.as_str()),
                    "{key}"
                );
            }
            assert!(m["timestamp_ms"].as_u64().unwrap() < 10_000);
            assert!(m["frame_at_ms"].as_u64().unwrap() < 10_000);
            assert!(["spoken", "fused", "flare", "draw", "click", "hover"]
                .contains(&m["source"].as_str().unwrap()));
        }
    }

    #[test]
    fn plan_json_respects_the_app_limits() {
        use clipy_local_moments::Moment;
        let plan = Plan {
            planner: Planner::Rules,
            moments: vec![
                Moment {
                    timestamp_ms: 1,
                    frame_at_ms: 2,
                    reason: "é".repeat(800),
                    source: MomentSource::Click,
                    x: Some(1.2),
                    y: Some(-0.1),
                },
                Moment {
                    timestamp_ms: 3,
                    frame_at_ms: 3,
                    reason: "  ".into(),
                    source: MomentSource::Hover,
                    x: None,
                    y: None,
                },
            ],
        };
        let parsed: Value = serde_json::from_str(&plan_json(&plan)).unwrap();
        let moments = parsed["moments"].as_array().unwrap();
        assert_eq!(moments.len(), 1);
        assert!(moments[0]["reason"].as_str().unwrap().len() <= MAX_REASON_BYTES);
        assert_eq!(moments[0]["x"], 1.0);
        assert_eq!(moments[0]["y"], 0.0);
        assert_eq!(moments[0]["source"], "click");
    }
}
