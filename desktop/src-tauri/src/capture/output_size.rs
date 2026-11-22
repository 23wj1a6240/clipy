//! Capture output-size policy — mirrors `fitsTierDimensions` and
//! `scaleFilterExpr` in `lib/uploadQuality.ts`.
//!
//! The server stores a desktop upload byte-for-byte when it already fits
//! its tier's box, and runs a full re-encode when it doesn't. Recording
//! outside the box therefore buys the user nothing: the server scales the
//! frame back to the same ceiling anyway, so the only difference is a
//! lossy transcode, a longer wait, and the extra bytes on the wire.
//!
//! Clamping at capture keeps the invariant `entitlement.rs` states — the
//! uploaded bytes ARE the stored bytes — true for every display, instead
//! of only for the ones that happen to be 16:9.
//!
//! Height alone is not enough to hold that invariant. The server's rule is
//! a box: landscape fits the tier's SHORT edge and an absolute 3840 long
//! edge, so a display wider than the tier's implied aspect (32:9
//! ultrawides, side-by-side captures) blows the width ceiling with a
//! perfectly legal height.

/// Absolute long-edge ceiling for every tier (`MAX_OUTPUT_LONG_EDGE`).
const MAX_OUTPUT_LONG_EDGE: u32 = 3840;

/// Tier short-edge cap for landscape captures. Keyed by the requested
/// height because that is exactly what the desktop sends as
/// `requestedQualityTier` (`quality_tier_for_height` in `upload.rs`),
/// including `None` (Native) meaning pro_4k.
fn tier_short_edge(max_height: Option<u32>) -> u32 {
    match max_height {
        None => 2160,
        Some(h) if h > 1440 => 2160,
        Some(h) if h > 1080 => 1440,
        _ => 1080,
    }
}

/// Tier long-edge cap, used for portrait and square captures — a height
/// cap on a 1080×2560 portrait would let it through untouched and
/// silently expand what the tier stores.
fn tier_long_edge(max_height: Option<u32>) -> u32 {
    match max_height {
        None => 3840,
        Some(h) if h > 1440 => 3840,
        Some(h) if h > 1080 => 2560,
        _ => 1920,
    }
}

/// The largest uniform downscale of `src` that the server will store
/// as-is, never upscaling. Dimensions are floored to even, as H.264's
/// NV12 chroma subsampling requires and the server's own scale
/// expression produces.
///
/// `max_height` is the user's requested cap, already entitlement-clamped
/// by `entitlement::clamp_video_options`; `None` means Native.
pub fn fit_capture_size(src_width: u32, src_height: u32, max_height: Option<u32>) -> (u32, u32) {
    let width = src_width.max(2);
    let height = src_height.max(2);

    // Landscape fits the box; portrait/square fits the tier long edge.
    // `>` (not `>=`) keeps square inputs on the portrait branch, matching
    // fitsTierDimensions.
    let mut factor: f64 = if width > height {
        let by_short = tier_short_edge(max_height) as f64 / height as f64;
        let by_long = MAX_OUTPUT_LONG_EDGE as f64 / width as f64;
        by_short.min(by_long)
    } else {
        tier_long_edge(max_height) as f64 / height as f64
    };

    // The explicit request still applies — someone asking for 720p on a
    // 1080p display gets 720p, which the tier box alone would not do.
    if let Some(cap) = max_height {
        factor = factor.min(cap as f64 / height as f64);
    }
    // Never upscale.
    if factor >= 1.0 {
        return (floor_even(width), floor_even(height));
    }

    let out_w = floor_even((width as f64 * factor) as u32);
    let out_h = floor_even((height as f64 * factor) as u32);
    (out_w.max(2), out_h.max(2))
}

fn floor_even(n: u32) -> u32 {
    n & !1
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The server's keep-as-is predicate, transcribed from
    /// `fitsTierDimensions`. Every case below must satisfy it, or the
    /// recording gets re-encoded server-side.
    fn server_stores_as_is(width: u32, height: u32, max_height: Option<u32>) -> bool {
        if width > height {
            height <= tier_short_edge(max_height) && width <= MAX_OUTPUT_LONG_EDGE
        } else {
            height <= tier_long_edge(max_height)
        }
    }

    #[test]
    fn every_clamped_size_is_stored_as_is() {
        let displays = [
            (1728, 1080),  // MacBook Pro 14" logical
            (1920, 1080),  // 1080p
            (2560, 1440),  // 1440p
            (3024, 1964),  // MacBook Pro 14" physical
            (3456, 2234),  // MacBook Pro 16" physical
            (3840, 2160),  // 4K
            (5120, 2880),  // 5K iMac
            (5120, 1440),  // 32:9 ultrawide
            (7680, 2160),  // side-by-side 4K
            (1080, 2560),  // portrait
            (1440, 1440),  // square
        ];
        for (w, h) in displays {
            for cap in [Some(720), Some(1080), Some(1440), Some(2160), None] {
                let (out_w, out_h) = fit_capture_size(w, h, cap);
                assert!(
                    server_stores_as_is(out_w, out_h, cap),
                    "{w}x{h} cap={cap:?} produced {out_w}x{out_h}, which the server re-encodes"
                );
                assert_eq!(out_w & 1, 0, "{w}x{h} cap={cap:?}: odd width {out_w}");
                assert_eq!(out_h & 1, 0, "{w}x{h} cap={cap:?}: odd height {out_h}");
            }
        }
    }

    #[test]
    fn aspect_ratio_is_preserved() {
        for (w, h) in [(5120u32, 2880u32), (3456, 2234), (5120, 1440), (1080, 2560)] {
            for cap in [Some(1080), Some(2160), None] {
                let (out_w, out_h) = fit_capture_size(w, h, cap);
                let src = w as f64 / h as f64;
                let out = out_w as f64 / out_h as f64;
                assert!(
                    (src - out).abs() < 0.02,
                    "{w}x{h} cap={cap:?}: aspect {src:.3} became {out:.3}"
                );
            }
        }
    }

    #[test]
    fn never_upscales() {
        let (w, h) = fit_capture_size(1280, 720, Some(2160));
        assert_eq!((w, h), (1280, 720));
        let (w, h) = fit_capture_size(1280, 720, None);
        assert_eq!((w, h), (1280, 720));
    }

    #[test]
    fn honours_an_explicit_request_below_the_tier_box() {
        // 720p on a 1080p display: the box would allow 1080, the request
        // does not.
        assert_eq!(fit_capture_size(1920, 1080, Some(720)), (1280, 720));
    }

    #[test]
    fn native_stops_at_the_tier_box_instead_of_the_panel() {
        // 5K iMac: the server would have downscaled this to exactly
        // 3840x2160 anyway — now it arrives that way, unencoded.
        assert_eq!(fit_capture_size(5120, 2880, None), (3840, 2160));
        // 16" MBP: height is the binding edge, not width.
        assert_eq!(fit_capture_size(3456, 2234, None), (3340, 2160));
    }

    #[test]
    fn width_is_the_binding_edge_on_ultrawides() {
        // 32:9 at the 1440 tier: height is legal, width is not. This is
        // the case a height-only clamp missed.
        assert_eq!(fit_capture_size(5120, 1440, Some(1440)), (3840, 1080));
    }

    #[test]
    fn typical_captures_are_untouched() {
        // The common path must stay byte-identical to before the clamp.
        assert_eq!(fit_capture_size(1728, 1080, Some(1080)), (1728, 1080));
        assert_eq!(fit_capture_size(1920, 1080, Some(1080)), (1920, 1080));
        assert_eq!(fit_capture_size(2560, 1440, Some(1440)), (2560, 1440));
        assert_eq!(fit_capture_size(3840, 2160, Some(2160)), (3840, 2160));
    }
}
