//! The plan: fused moments projected to what the desktop app screenshots.
//! A port of the v2 projection in `planLocalMoments` (`localMomentPlan.ts`).

use serde::{Deserialize, Serialize};

use crate::evidence::{self, Evidence, Geometry};
use crate::fusion::{self, Cue, FuseInput, FusedSource};
use crate::input::Input;
use crate::js;

/// Spoken cues a model may return; they share the budget with clicks and rests.
pub const SPOKEN_MOMENTS_MAX: usize = 30;
/// Flares and drawings kept outside that budget.
pub const GESTURE_MOMENTS_MAX: usize = 30;
/// Most moments in one plan.
pub const PLAN_MOMENTS_MAX: usize = 96;

/// Where a moment came from.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum MomentSource {
    /// A spoken cue with no gesture to anchor it.
    Spoken,
    /// A spoken cue fused with the click it talks about.
    Fused,
    /// A hold-Control highlight, alone or under a spoken cue.
    Flare,
    /// A pen drawing, alone or under a spoken cue.
    Draw,
    /// A click nobody talked about.
    Click,
    /// A pointer rest, alone or under a spoken cue.
    Hover,
}

/// One moment to screenshot.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Moment {
    /// Where the moment sits on the recording clock (what the reason refers to).
    pub timestamp_ms: u64,
    /// The instant to screenshot: a gesture's end, the click, a rest's settle point.
    pub frame_at_ms: u64,
    /// Why, in one sentence (at most 1000 UTF-16 code units).
    pub reason: String,
    pub source: MomentSource,
    /// Pointer at `frame_at_ms`, normalized 0..1 in capture space. Only when
    /// the input has capture geometry.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub x: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub y: Option<f64>,
}

/// Which planner produced a plan, for the AREC to state.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Planner {
    /// Rules only: nothing left the machine (or a model was configured but
    /// never asked, because there was no transcript).
    #[serde(rename = "rules")]
    Rules,
    /// A model answered and its spoken cues are in the plan.
    #[serde(rename = "rules+model")]
    RulesAndModel,
}

/// Moments in timestamp order, at most one per timestamp.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Plan {
    pub moments: Vec<Moment>,
    pub planner: Planner,
}

/// Plan from rules alone: flares, drawings, clicks and pointer rests. Offline
/// and deterministic. Works on any input; `Input::validate` is not required.
pub fn plan_rules(input: &Input) -> Plan {
    let evidence = evidence::build(input);
    Plan {
        moments: project(input, &evidence, &[]),
        planner: Planner::Rules,
    }
}

/// Fuse `cues` with the evidence and project the result.
pub(crate) fn project(input: &Input, evidence: &Evidence, cues: &[Cue]) -> Vec<Moment> {
    let duration = input.duration_ms as f64;
    let geometry = Geometry {
        rect: evidence.capture_rect,
        timeline: &evidence.capture_timeline,
        frame_width: input.capture.as_ref().and_then(|c| c.output_width),
        frame_height: input.capture.as_ref().and_then(|c| c.output_height),
    };
    // Without capture geometry raw positions still anchor frames, but they are
    // never reported: a coordinate in an unknown space is not evidence.
    let coords_trusted = evidence.capture_rect.is_some();
    let normalize_at = |x: f64, y: f64, t: f64| {
        if coords_trusted {
            geometry.normalize(x, y, t)
        } else {
            Some((x, y))
        }
    };
    let segments: Vec<(f64, f64, String)> = input
        .transcript
        .iter()
        .map(|s| (s.start_ms / 1000.0, s.end_ms / 1000.0, s.text.clone()))
        .collect();
    let fused = fusion::fuse(&FuseInput {
        deixis: cues,
        clicks: &evidence.clicks,
        moves: &evidence.moves,
        hovers: &evidence.hovers,
        strokes: &evidence.pen_strokes,
        flares: &evidence.flares,
        segments: &segments,
        duration_ms: duration,
        max_moments: SPOKEN_MOMENTS_MAX,
        max_annotation_moments: GESTURE_MOMENTS_MAX,
        normalize_at: &normalize_at,
    });

    let last = duration - 1.0;
    let clamp = |t: f64| js::round(t).min(last).max(0.0) as u64;
    let all: Vec<Moment> = fused
        .into_iter()
        .map(|m| {
            let (x, y) = match (coords_trusted, m.x, m.y) {
                (true, Some(x), Some(y)) => (Some(x), Some(y)),
                _ => (None, None),
            };
            Moment {
                timestamp_ms: clamp(m.t_ms),
                frame_at_ms: clamp(m.frame_at_ms),
                reason: js::slice_utf16(&m.caption, 1000),
                source: match m.source {
                    FusedSource::Deixis => MomentSource::Spoken,
                    FusedSource::Fused => MomentSource::Fused,
                    FusedSource::Flare => MomentSource::Flare,
                    FusedSource::Draw => MomentSource::Draw,
                    FusedSource::Click => MomentSource::Click,
                    FusedSource::Hover => MomentSource::Hover,
                },
                x,
                y,
            }
        })
        .collect();
    // One moment per timestamp, compared with the previous moment in fused order.
    let mut kept = Vec::with_capacity(all.len());
    for (i, moment) in all.iter().enumerate() {
        if i == 0 || moment.timestamp_ms != all[i - 1].timestamp_ms {
            kept.push(moment.clone());
        }
    }
    kept.truncate(PLAN_MOMENTS_MAX);
    kept
}
