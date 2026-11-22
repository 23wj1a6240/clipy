//! Windows source thumbnails for the recording picker.
//!
//! Mirrors the contract of the macOS `thumbnails` module: every entry point
//! returns `Some("data:image/png;base64,…")` capped at `THUMB_MAX_PIXEL_SIZE`
//! on its longest edge, or `None` when the source cannot be captured (window
//! closed, minimised, protected content, GDI refusal). The picker renders a
//! placeholder for `None`, so failure here is cosmetic and must never surface
//! as an error.
//!
//! Windows are grabbed with `PrintWindow(PW_RENDERFULLCONTENT)` rather than by
//! blitting the screen. `PrintWindow` asks the window to render itself, so it
//! works for windows that are occluded or partly offscreen — blitting the
//! screen would capture whatever happens to be on top of them instead, which
//! would show the user a preview of the wrong application.
//!
//! All FFI is contained in this module.

#![cfg(target_os = "windows")]

use base64::{engine::general_purpose::STANDARD, Engine};
use windows::Win32::Foundation::{HWND, RECT};
use windows::Win32::Graphics::Gdi::{
    BitBlt, CreateCompatibleBitmap, CreateCompatibleDC, DeleteDC, DeleteObject, GetDC, GetDIBits,
    ReleaseDC, SelectObject, BITMAPINFO, BITMAPINFOHEADER, BI_RGB, DIB_RGB_COLORS, HBITMAP, HDC,
    HGDIOBJ, SRCCOPY,
};
use windows::Win32::Storage::Xps::{PrintWindow, PRINT_WINDOW_FLAGS};
use windows::Win32::UI::WindowsAndMessaging::{GetWindowRect, IsIconic};

/// Longest edge of a generated thumbnail, matching the macOS module.
const THUMB_MAX_PIXEL_SIZE: u32 = 256;

/// `PW_RENDERFULLCONTENT` — render DirectComposition/modern content too.
/// Without it, Chromium and UWP windows come back blank.
const PW_RENDERFULLCONTENT: u32 = 0x0000_0002;

/// RAII wrappers so every early return still releases its GDI objects. GDI
/// handles are process-wide and leaking them degrades the whole desktop, not
/// just this app — and the picker calls this once per source, repeatedly.
struct ScreenDc(HDC);
impl Drop for ScreenDc {
    fn drop(&mut self) {
        // SAFETY: Category 8 — FFI contract. `self.0` came from `GetDC(None)`
        // and is released exactly once, here.
        unsafe {
            ReleaseDC(None, self.0);
        }
    }
}

struct MemDc(HDC);
impl Drop for MemDc {
    fn drop(&mut self) {
        // SAFETY: Category 8 — FFI contract. Created by `CreateCompatibleDC`,
        // deleted exactly once.
        unsafe {
            let _ = DeleteDC(self.0);
        }
    }
}

struct Bitmap(HBITMAP);
impl Drop for Bitmap {
    fn drop(&mut self) {
        // SAFETY: Category 8 — FFI contract. Created by
        // `CreateCompatibleBitmap`, deleted exactly once.
        unsafe {
            let _ = DeleteObject(HGDIOBJ(self.0 .0));
        }
    }
}

/// Thumbnail for one window, by raw `HWND`.
pub fn capture_window_thumbnail(hwnd: isize) -> Option<String> {
    let hwnd = HWND(hwnd as *mut std::ffi::c_void);

    // SAFETY: Category 8 — FFI contract. `hwnd` is validated by the caller
    // (`Window::is_valid`) and every handle below is freed by its RAII wrapper.
    unsafe {
        // A minimised window has no meaningful pixels; PrintWindow would
        // return an empty or stale surface.
        if IsIconic(hwnd).as_bool() {
            return None;
        }
        let mut rect = RECT::default();
        GetWindowRect(hwnd, &mut rect).ok()?;
        let width = rect.right - rect.left;
        let height = rect.bottom - rect.top;
        if width <= 0 || height <= 0 {
            return None;
        }

        let screen = ScreenDc(GetDC(None));
        if screen.0.is_invalid() {
            return None;
        }
        let mem = MemDc(CreateCompatibleDC(screen.0));
        if mem.0.is_invalid() {
            return None;
        }
        let bitmap = Bitmap(CreateCompatibleBitmap(screen.0, width, height));
        if bitmap.0.is_invalid() {
            return None;
        }
        let previous = SelectObject(mem.0, HGDIOBJ(bitmap.0 .0));

        let printed = PrintWindow(hwnd, mem.0, PRINT_WINDOW_FLAGS(PW_RENDERFULLCONTENT)).as_bool();
        let pixels = if printed {
            read_bgra(mem.0, bitmap.0, width, height)
        } else {
            None
        };

        SelectObject(mem.0, previous);
        encode(pixels?, width as u32, height as u32)
    }
}

/// Thumbnail of one display.
///
/// `None` when the display's bounds cannot be resolved: the id no longer names
/// a display (one was unplugged) or Windows will not report where it is. The
/// picker then shows its placeholder. Substituting the whole virtual desktop
/// here, which is what this did for every id until the picker reported real
/// origins, put the same squashed image of all the monitors on every tile, so
/// with more than one display connected the choice was unusable.
pub fn capture_display_thumbnail(display_id: u32) -> Option<String> {
    let (src_x, src_y, width, height) = crate::capture::windows::display_bounds(display_id)?;
    let width = i32::try_from(width).ok()?;
    let height = i32::try_from(height).ok()?;
    if width < 2 || height < 2 {
        return None;
    }
    // SAFETY: Category 8 — FFI contract. Every handle is freed by its RAII
    // wrapper; `BitBlt` reads from the screen DC into our own bitmap.
    unsafe {
        // The screen DC spans the virtual desktop in its own coordinates, so a
        // display left of or above the primary has a negative origin and is
        // read from there.
        let screen = ScreenDc(GetDC(None));
        if screen.0.is_invalid() {
            return None;
        }
        let mem = MemDc(CreateCompatibleDC(screen.0));
        if mem.0.is_invalid() {
            return None;
        }
        let bitmap = Bitmap(CreateCompatibleBitmap(screen.0, width, height));
        if bitmap.0.is_invalid() {
            return None;
        }
        let previous = SelectObject(mem.0, HGDIOBJ(bitmap.0 .0));

        let copied = BitBlt(mem.0, 0, 0, width, height, screen.0, src_x, src_y, SRCCOPY).is_ok();
        let pixels = if copied {
            read_bgra(mem.0, bitmap.0, width, height)
        } else {
            None
        };

        SelectObject(mem.0, previous);
        encode(pixels?, width as u32, height as u32)
    }
}

/// Pull the bitmap's pixels back as bottom-up BGRA.
///
/// SAFETY: Category 8 — FFI contract. `dc` and `bitmap` are live handles owned
/// by the caller; the buffer is sized exactly `width * height * 4`, which is
/// what the `BITMAPINFOHEADER` below describes.
unsafe fn read_bgra(dc: HDC, bitmap: HBITMAP, width: i32, height: i32) -> Option<Vec<u8>> {
    let mut info = BITMAPINFO {
        bmiHeader: BITMAPINFOHEADER {
            biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
            biWidth: width,
            // Negative height requests a TOP-DOWN buffer, so rows arrive in the
            // order `image` expects and no vertical flip is needed.
            biHeight: -height,
            biPlanes: 1,
            biBitCount: 32,
            biCompression: BI_RGB.0,
            ..Default::default()
        },
        ..Default::default()
    };
    let len = (width as usize).checked_mul(height as usize)?.checked_mul(4)?;
    let mut buffer = vec![0u8; len];
    let scanned = GetDIBits(
        dc,
        bitmap,
        0,
        height as u32,
        Some(buffer.as_mut_ptr().cast()),
        &mut info,
        DIB_RGB_COLORS,
    );
    if scanned == 0 {
        return None;
    }
    Some(buffer)
}

/// Downscale to the thumbnail cap and encode as a PNG data URL.
fn encode(mut bgra: Vec<u8>, width: u32, height: u32) -> Option<String> {
    // GDI hands back BGRA; `image` wants RGBA. Swap in place rather than
    // allocating a second full-size buffer.
    for px in bgra.chunks_exact_mut(4) {
        px.swap(0, 2);
        // PrintWindow leaves alpha at 0 for many windows, which would encode a
        // fully transparent PNG. The picker wants an opaque preview.
        px[3] = 0xFF;
    }
    let source = image::RgbaImage::from_raw(width, height, bgra)?;

    let longest = width.max(height);
    let image = if longest > THUMB_MAX_PIXEL_SIZE {
        let scale = THUMB_MAX_PIXEL_SIZE as f32 / longest as f32;
        let target_w = ((width as f32 * scale).round() as u32).max(1);
        let target_h = ((height as f32 * scale).round() as u32).max(1);
        image::imageops::resize(
            &source,
            target_w,
            target_h,
            image::imageops::FilterType::Triangle,
        )
    } else {
        source
    };

    let mut png = std::io::Cursor::new(Vec::new());
    image
        .write_to(&mut png, image::ImageFormat::Png)
        .ok()
        .map(|_| format!("data:image/png;base64,{}", STANDARD.encode(png.into_inner())))
}
