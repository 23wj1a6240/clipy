//! Local edition stand-in for `entitlement.rs`: no plan, no recording gate, no
//! resolution cap. Built only without the `cloud` feature.

use crate::types::VideoOptionsDto;

/// Same event name as the official app, for the tray listener that shared
/// code registers; nothing emits it here.
pub const ENTITLEMENT_CHANGED_EVENT: &str = "entitlement-changed";

#[derive(Default)]
pub struct EntitlementState;

impl EntitlementState {
    pub fn is_pro_cached(&self, _bearer: &str) -> bool {
        true
    }
}

/// Pass-through: nothing is capped in the local edition.
pub fn clamp_video_options(video: VideoOptionsDto, _is_pro: bool) -> VideoOptionsDto {
    video
}
