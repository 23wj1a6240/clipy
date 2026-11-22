//! Source thumbnails for the pre-record picker.
//!
//! On macOS 14+ thumbnails come from `SCScreenshotManager`. Older systems
//! fall back to `CGWindowListCreateImage` / `CGDisplayCreateImage`. Both paths
//! encode through ImageIO with `kCGImageDestinationImageMaxPixelSize` so
//! thumbnails cap at ~256px on the long edge and the IPC payload stays small
//! (~50 KB each) even for 4K displays.
//!
//! Why not the legacy APIs everywhere: macOS 15 attributes every legacy
//! capture call as an active screen capture for several seconds and then
//! keeps it under Control Center's "recent activity", so each picker open
//! (and the launch-time prefetch) left "Clipy is capturing your screen" in the
//! menu bar with no recording running. `SCScreenshotManager` captures are
//! momentary and do not linger.
//!
//! All FFI is contained in this module. Public entry points return base64
//! `data:` URLs ready to drop into an `<img src>` from React.

#![cfg(target_os = "macos")]

use base64::{engine::general_purpose::STANDARD, Engine};
use std::ffi::c_void;
use std::ptr;

use crate::types::CaptureSourceDto;

const THUMB_MAX_PIXEL_SIZE: i32 = 256;

#[repr(C)]
#[derive(Copy, Clone)]
struct CGPoint {
    x: f64,
    y: f64,
}

#[repr(C)]
#[derive(Copy, Clone)]
struct CGSize {
    width: f64,
    height: f64,
}

#[repr(C)]
#[derive(Copy, Clone)]
struct CGRect {
    origin: CGPoint,
    size: CGSize,
}

const CG_RECT_NULL: CGRect = CGRect {
    origin: CGPoint {
        x: f64::INFINITY,
        y: f64::INFINITY,
    },
    size: CGSize {
        width: 0.0,
        height: 0.0,
    },
};

// CGWindowListOption: kCGWindowListOptionIncludingWindow
const KCG_WINDOW_LIST_OPTION_INCLUDING_WINDOW: u32 = 1 << 3;
// CGWindowImageOption: kCGWindowImageBoundsIgnoreFraming | kCGWindowImageNominalResolution
const KCG_WINDOW_IMAGE_BOUNDS_IGNORE_FRAMING: u32 = 1 << 0;
const KCG_WINDOW_IMAGE_NOMINAL_RESOLUTION: u32 = 1 << 4;

const KCFNUMBER_SINT32_TYPE: i32 = 3;
const KCFSTRING_ENCODING_UTF8: u32 = 0x0800_0100;

type CGImageRef = *mut c_void;
type CFTypeRef = *const c_void;
type CFStringRef = *const c_void;
type CFNumberRef = *const c_void;
type CFDictionaryRef = *const c_void;
type CFMutableDataRef = *mut c_void;
type CGImageDestinationRef = *mut c_void;

#[link(name = "CoreGraphics", kind = "framework")]
extern "C" {
    fn CGWindowListCreateImage(
        screen_bounds: CGRect,
        list_option: u32,
        window_id: u32,
        image_option: u32,
    ) -> CGImageRef;
    fn CGDisplayCreateImage(display_id: u32) -> CGImageRef;
    fn CGImageRelease(image: CGImageRef);
}

#[link(name = "CoreFoundation", kind = "framework")]
extern "C" {
    fn CFRelease(cf: CFTypeRef);
    fn CFDataCreateMutable(allocator: *const c_void, capacity: isize) -> CFMutableDataRef;
    fn CFDataGetLength(data: *const c_void) -> isize;
    fn CFDataGetBytePtr(data: *const c_void) -> *const u8;
    fn CFStringCreateWithCString(
        allocator: *const c_void,
        c_str: *const u8,
        encoding: u32,
    ) -> CFStringRef;
    fn CFNumberCreate(
        allocator: *const c_void,
        number_type: i32,
        value_ptr: *const c_void,
    ) -> CFNumberRef;
    fn CFDictionaryCreate(
        allocator: *const c_void,
        keys: *const *const c_void,
        values: *const *const c_void,
        count: isize,
        key_callbacks: *const c_void,
        value_callbacks: *const c_void,
    ) -> CFDictionaryRef;

    static kCFTypeDictionaryKeyCallBacks: c_void;
    static kCFTypeDictionaryValueCallBacks: c_void;
}

#[link(name = "ImageIO", kind = "framework")]
extern "C" {
    fn CGImageDestinationCreateWithData(
        data: CFMutableDataRef,
        ut_type: CFStringRef,
        count: isize,
        options: *const c_void,
    ) -> CGImageDestinationRef;
    fn CGImageDestinationAddImage(
        dest: CGImageDestinationRef,
        image: CGImageRef,
        properties: CFDictionaryRef,
    );
    fn CGImageDestinationFinalize(dest: CGImageDestinationRef) -> bool;
}

/// Whether the `SCScreenshotManager` path is usable on this system.
pub fn sck_thumbnails_available() -> bool {
    cidre::api::macos_available("14.0")
}

/// `SCScreenshotManager` thumbnail for a display or window. `None` when the
/// source is gone or ScreenCaptureKit refuses; callers show a placeholder.
pub async fn capture_source_thumbnail_sck(source: &CaptureSourceDto) -> Option<String> {
    use cidre::{ns, sc};

    if matches!(source, CaptureSourceDto::Camera { .. }) {
        return None;
    }

    let content = match sc::ShareableContent::current().await {
        Ok(content) => content,
        Err(err) => {
            tracing::debug!("thumbnail shareable content failed: {err:?}");
            return None;
        }
    };
    let (filter, size) = match source {
        CaptureSourceDto::Camera { .. } => return None,
        CaptureSourceDto::Display { id } | CaptureSourceDto::Area { display_id: id, .. } => {
            let displays = content.displays();
            let display = displays.iter().find(|d| d.display_id().0 == *id)?;
            let empty_windows = ns::Array::<sc::Window>::new();
            (
                sc::ContentFilter::with_display_excluding_windows(display, &empty_windows),
                display.frame().size,
            )
        }
        CaptureSourceDto::Window { id } => {
            let windows = content.windows();
            let window = windows.iter().find(|w| w.id() == *id)?;
            (
                sc::ContentFilter::with_desktop_independent_window(window),
                window.frame().size,
            )
        }
    };
    let (width, height) = thumbnail_dimensions(size.width, size.height);
    if width == 0 || height == 0 {
        return None;
    }

    let mut cfg = sc::StreamCfg::new();
    cfg.set_width(width);
    cfg.set_height(height);
    cfg.set_shows_cursor(false);
    // SAFETY: gated by `sck_thumbnails_available` (macOS 14+) at the call site.
    let image = match unsafe { sc::ScreenshotManager::capture_image(&filter, &cfg) }.await {
        Ok(image) => image,
        Err(err) => {
            tracing::debug!("SCScreenshotManager thumbnail failed: {err:?}");
            return None;
        }
    };
    let image_ref = &*image as *const cidre::cg::Image as CGImageRef;
    // SAFETY: `image` outlives the encode; ImageIO only reads the CGImage.
    unsafe { cg_image_to_png_data_url(image_ref) }
}

/// Output size for an SCK thumbnail: long edge capped at
/// `THUMB_MAX_PIXEL_SIZE`, aspect preserved, never upscaled.
fn thumbnail_dimensions(width: f64, height: f64) -> (usize, usize) {
    if !(width > 0.0) || !(height > 0.0) {
        return (0, 0);
    }
    let scale = (THUMB_MAX_PIXEL_SIZE as f64 / width.max(height)).min(1.0);
    (
        (width * scale).round().max(1.0) as usize,
        (height * scale).round().max(1.0) as usize,
    )
}

/// Legacy `CGWindowListCreateImage` thumbnail (pre-macOS 14 only), or `None`
/// if the window can't be captured (off-screen, dead, sharing disabled).
pub fn capture_window_thumbnail(window_id: u32) -> Option<String> {
    unsafe {
        let img = CGWindowListCreateImage(
            CG_RECT_NULL,
            KCG_WINDOW_LIST_OPTION_INCLUDING_WINDOW,
            window_id,
            KCG_WINDOW_IMAGE_BOUNDS_IGNORE_FRAMING | KCG_WINDOW_IMAGE_NOMINAL_RESOLUTION,
        );
        if img.is_null() {
            return None;
        }
        let result = cg_image_to_png_data_url(img);
        CGImageRelease(img);
        result
    }
}

/// Legacy `CGDisplayCreateImage` thumbnail (pre-macOS 14 only).
pub fn capture_display_thumbnail(display_id: u32) -> Option<String> {
    unsafe {
        let img = CGDisplayCreateImage(display_id);
        if img.is_null() {
            return None;
        }
        let result = cg_image_to_png_data_url(img);
        CGImageRelease(img);
        result
    }
}

unsafe fn cg_image_to_png_data_url(image: CGImageRef) -> Option<String> {
    let data = CFDataCreateMutable(ptr::null(), 0);
    if data.is_null() {
        return None;
    }

    let ut_type = CFStringCreateWithCString(
        ptr::null(),
        b"public.png\0".as_ptr(),
        KCFSTRING_ENCODING_UTF8,
    );
    if ut_type.is_null() {
        CFRelease(data as CFTypeRef);
        return None;
    }

    let dest = CGImageDestinationCreateWithData(data, ut_type, 1, ptr::null());
    CFRelease(ut_type as CFTypeRef);
    if dest.is_null() {
        CFRelease(data as CFTypeRef);
        return None;
    }

    // Build {kCGImageDestinationImageMaxPixelSize: 256}. The constant
    // string isn't reliably exported by ImageIO's C ABI under all SDKs,
    // so we synthesize the literal — it's stable Apple-public API.
    let key = CFStringCreateWithCString(
        ptr::null(),
        b"kCGImageDestinationImageMaxPixelSize\0".as_ptr(),
        KCFSTRING_ENCODING_UTF8,
    );
    let max_size: i32 = THUMB_MAX_PIXEL_SIZE;
    let value = CFNumberCreate(
        ptr::null(),
        KCFNUMBER_SINT32_TYPE,
        &max_size as *const i32 as *const c_void,
    );

    let properties: CFDictionaryRef = if !key.is_null() && !value.is_null() {
        let keys: [*const c_void; 1] = [key];
        let values: [*const c_void; 1] = [value];
        CFDictionaryCreate(
            ptr::null(),
            keys.as_ptr(),
            values.as_ptr(),
            1,
            &kCFTypeDictionaryKeyCallBacks as *const _ as *const c_void,
            &kCFTypeDictionaryValueCallBacks as *const _ as *const c_void,
        )
    } else {
        ptr::null()
    };

    CGImageDestinationAddImage(dest, image, properties);
    let ok = CGImageDestinationFinalize(dest);

    if !key.is_null() {
        CFRelease(key as CFTypeRef);
    }
    if !value.is_null() {
        CFRelease(value as CFTypeRef);
    }
    if !properties.is_null() {
        CFRelease(properties as CFTypeRef);
    }
    CFRelease(dest as CFTypeRef);

    if !ok {
        CFRelease(data as CFTypeRef);
        return None;
    }

    let len = CFDataGetLength(data);
    let ptr_bytes = CFDataGetBytePtr(data);
    if ptr_bytes.is_null() || len <= 0 {
        CFRelease(data as CFTypeRef);
        return None;
    }
    let bytes = std::slice::from_raw_parts(ptr_bytes, len as usize).to_vec();
    CFRelease(data as CFTypeRef);

    Some(format!("data:image/png;base64,{}", STANDARD.encode(&bytes)))
}

#[cfg(test)]
mod tests {
    use super::thumbnail_dimensions;

    #[test]
    fn caps_the_long_edge_and_keeps_aspect() {
        assert_eq!(thumbnail_dimensions(2560.0, 1440.0), (256, 144));
        assert_eq!(thumbnail_dimensions(1080.0, 1920.0), (144, 256));
    }

    #[test]
    fn never_upscales_small_sources() {
        assert_eq!(thumbnail_dimensions(120.0, 80.0), (120, 80));
    }

    #[test]
    fn rejects_empty_sources() {
        assert_eq!(thumbnail_dimensions(0.0, 900.0), (0, 0));
        assert_eq!(thumbnail_dimensions(f64::NAN, 900.0), (0, 0));
    }
}
