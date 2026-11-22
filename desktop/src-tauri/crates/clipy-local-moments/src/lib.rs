//! Picks the moments of a screen recording worth a screenshot, on the user's
//! machine.
//!
//! - Rules (always, offline): hold-to-highlight flares, pen drawings, click
//!   clusters with their accessibility labels, and pointer rests, from the
//!   interaction sidecar. [`plan_rules`].
//! - Spoken cues (optional): a model the user configured reads the transcript
//!   for "this button", "it breaks here" and similar, and the cues are fused
//!   with the same evidence. [`plan_with_provider`].
//!
//! A Rust port of the rules-only and model paths of `planMoments` in
//! `@clipy/moments`, held to its output by golden files (`tests/golden/`,
//! written by `packages/moments/scripts/make-golden.mjs`).

mod evidence;
mod fusion;
mod input;
mod js;
mod plan;
mod provider;

pub use input::{
    CaptureGeometry, CaptureSample, ClickElement, Input, InputError, InteractionEvent,
    KeycodeDialect, TranscriptSegment,
};
pub use plan::{
    plan_rules, Moment, MomentSource, Plan, Planner, GESTURE_MOMENTS_MAX, PLAN_MOMENTS_MAX,
    SPOKEN_MOMENTS_MAX,
};
pub use provider::{
    plan_with_provider, ConfigError, PlannerError, Preset, ProviderConfig, ATTEMPTS,
    ATTEMPT_TIMEOUT, DEFAULT_OPENROUTER_MODEL, MAX_COMPLETION_BYTES, RETRY_DELAYS, SYSTEM_PROMPT,
};
