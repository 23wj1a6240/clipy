//! Windows screen pixels and image clipboard for `screenshot.rs`.
//!
//! Capture goes through GDI (`BitBlt` from the screen DC) rather than Windows
//! Graphics Capture: a screenshot is a single frame, where WGC would need a
//! capture session, a frame callback and a teardown to produce one.
//! `thumbnails_win` reads the screen the same way. As with any GDI capture,
//! protected content (DRM video) comes back black.
//!
//! The clipboard carries `CF_DIB`, the format every Windows app accepts for a
//! pasted image; the clipboard's own text format is written elsewhere, by the
//! Tauri plugin, when a share link is copied instead. Clipboard history's
//! image paste adds the "PNG" format and its text beside it.
//!
//! All FFI is contained in this module.

#![cfg(target_os = "windows")]

use std::path::Path;
use windows::core::w;
use windows::Win32::Foundation::{GlobalFree, HANDLE, HGLOBAL, HWND};
use windows::Win32::Graphics::Gdi::{
    BitBlt, CreateCompatibleBitmap, CreateCompatibleDC, DeleteDC, DeleteObject, GetDC, GetDIBits,
    ReleaseDC, SelectObject, BITMAPINFO, BITMAPINFOHEADER, BI_RGB, DIB_RGB_COLORS, HBITMAP, HDC,
    HGDIOBJ, SRCCOPY,
};
use windows::Win32::System::DataExchange::{
    CloseClipboard, EmptyClipboard, OpenClipboard, RegisterClipboardFormatW, SetClipboardData,
};
use windows::Win32::System::Memory::{GlobalAlloc, GlobalLock, GlobalUnlock, GMEM_MOVEABLE};
use windows::Win32::System::Ole::{CF_DIB, CF_UNICODETEXT};

/// RAII wrappers so every early return still releases its GDI objects. GDI
/// handles are process-wide, so leaking them degrades the whole desktop.
struct ScreenDc(HDC);
impl Drop for ScreenDc {
    fn drop(&mut self) {
        // SAFETY: Category 8 — FFI contract. From `GetDC(None)`, released once.
        unsafe {
            ReleaseDC(None, self.0);
        }
    }
}

struct MemDc(HDC);
impl Drop for MemDc {
    fn drop(&mut self) {
        // SAFETY: Category 8 — FFI contract. From `CreateCompatibleDC`, deleted once.
        unsafe {
            let _ = DeleteDC(self.0);
        }
    }
}

struct Bitmap(HBITMAP);
impl Drop for Bitmap {
    fn drop(&mut self) {
        // SAFETY: Category 8 — FFI contract. From `CreateCompatibleBitmap`, deleted once.
        unsafe {
            let _ = DeleteObject(HGDIOBJ(self.0 .0));
        }
    }
}

/// Capture `width` × `height` physical pixels at virtual-desktop coordinates
/// (`x`, `y`) and write them to `path` as a PNG.
pub fn capture_region_to_png(
    x: i32,
    y: i32,
    width: i32,
    height: i32,
    path: &Path,
) -> Result<(), String> {
    let image = capture_region(x, y, width, height)?;
    image
        .save(path)
        .map_err(|e| format!("write screenshot: {e}"))
}

fn capture_region(x: i32, y: i32, width: i32, height: i32) -> Result<image::RgbaImage, String> {
    if width <= 0 || height <= 0 {
        return Err("the selected area is empty".into());
    }

    // SAFETY: Category 8 — FFI contract. Every handle is freed by its RAII
    // wrapper; `BitBlt` reads the screen DC into our own bitmap.
    let mut bgra = unsafe {
        let screen = ScreenDc(GetDC(None));
        if screen.0.is_invalid() {
            return Err("could not read the screen".into());
        }
        let mem = MemDc(CreateCompatibleDC(screen.0));
        if mem.0.is_invalid() {
            return Err("could not read the screen".into());
        }
        let bitmap = Bitmap(CreateCompatibleBitmap(screen.0, width, height));
        if bitmap.0.is_invalid() {
            return Err("could not read the screen".into());
        }
        let previous = SelectObject(mem.0, HGDIOBJ(bitmap.0 .0));
        let copied = BitBlt(mem.0, 0, 0, width, height, screen.0, x, y, SRCCOPY).is_ok();
        let pixels = if copied {
            read_bgra(mem.0, bitmap.0, width, height)
        } else {
            None
        };
        SelectObject(mem.0, previous);
        pixels.ok_or_else(|| "could not read the screen".to_string())?
    };

    // GDI hands back BGRA with no meaningful alpha; `image` wants opaque RGBA.
    for pixel in bgra.chunks_exact_mut(4) {
        pixel.swap(0, 2);
        pixel[3] = 0xFF;
    }
    image::RgbaImage::from_raw(width as u32, height as u32, bgra)
        .ok_or_else(|| "screenshot buffer did not match the screen".into())
}

/// Pull the bitmap's pixels back as top-down BGRA.
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
    let len = (width as usize)
        .checked_mul(height as usize)?
        .checked_mul(4)?;
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

/// Put the saved PNG on the clipboard as an image.
pub fn copy_image_to_clipboard(path: &Path) -> Result<(), String> {
    let image = image::open(path)
        .map_err(|e| format!("read screenshot: {e}"))?
        .to_rgb8();
    let dib = packed_dib(&image);
    // SAFETY: Category 8 — FFI contract. `dib` is a complete CF_DIB image and
    // the global it is copied into is owned by the clipboard only after
    // `SetClipboardData` succeeds; every earlier failure frees it.
    unsafe { set_clipboard(&[(CF_DIB.0 as u32, &dib)]) }
}

/// Clipboard history's paste of an image item: the PNG first (alpha intact,
/// what browsers and Office prefer), `CF_DIB` for every other app, and the
/// text that was copied with it, if any.
pub fn copy_png_and_text_to_clipboard(png: &[u8], text: &str) -> Result<(), String> {
    let image = image::load_from_memory_with_format(png, image::ImageFormat::Png)
        .map_err(|e| format!("read clipboard image: {e}"))?
        .to_rgb8();
    let dib = packed_dib(&image);
    let unicode: Vec<u8> = text
        .encode_utf16()
        .chain(std::iter::once(0))
        .flat_map(u16::to_le_bytes)
        .collect();
    // SAFETY: Category 8 — FFI contract. Registering a format name only reads
    // the string.
    let png_format = unsafe { RegisterClipboardFormatW(w!("PNG")) };
    let mut formats: Vec<(u32, &[u8])> = Vec::new();
    if png_format != 0 {
        formats.push((png_format, png));
    }
    formats.push((CF_DIB.0 as u32, &dib));
    if !text.is_empty() {
        formats.push((CF_UNICODETEXT.0 as u32, &unicode));
    }
    // SAFETY: as in `copy_image_to_clipboard`; every buffer is complete.
    unsafe { set_clipboard(&formats) }
}

/// A CF_DIB image: an info header followed by bottom-up 24-bit rows padded to
/// a 4-byte boundary. 24-bit because a 32-bit DIB's alpha is undefined for
/// this format, and apps that honour it paste a fully transparent image.
fn packed_dib(image: &image::RgbImage) -> Vec<u8> {
    let (width, height) = image.dimensions();
    let stride = (width as usize * 3).div_ceil(4) * 4;
    let header = BITMAPINFOHEADER {
        biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
        biWidth: width as i32,
        biHeight: height as i32,
        biPlanes: 1,
        biBitCount: 24,
        biCompression: BI_RGB.0,
        biSizeImage: (stride * height as usize) as u32,
        ..Default::default()
    };
    // SAFETY: Category 8 — FFI contract. `BITMAPINFOHEADER` is a plain C
    // struct, read here as its own bytes and never retained.
    let header_bytes = unsafe {
        std::slice::from_raw_parts(
            (&header as *const BITMAPINFOHEADER).cast::<u8>(),
            std::mem::size_of::<BITMAPINFOHEADER>(),
        )
    };

    let mut dib = Vec::with_capacity(header_bytes.len() + stride * height as usize);
    dib.extend_from_slice(header_bytes);
    for y in (0..height).rev() {
        let start = dib.len();
        for x in 0..width {
            let pixel = image.get_pixel(x, y).0;
            dib.extend_from_slice(&[pixel[2], pixel[1], pixel[0]]);
        }
        dib.resize(start + stride, 0);
    }
    dib
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn packs_a_bottom_up_dib_with_padded_rows() {
        let mut image = image::RgbImage::new(2, 2);
        image.put_pixel(0, 0, image::Rgb([1, 2, 3]));
        image.put_pixel(1, 0, image::Rgb([4, 5, 6]));
        image.put_pixel(0, 1, image::Rgb([7, 8, 9]));
        image.put_pixel(1, 1, image::Rgb([10, 11, 12]));

        let dib = packed_dib(&image);
        let header = std::mem::size_of::<BITMAPINFOHEADER>();
        // 2 pixels x 3 bytes, padded to a 4-byte boundary.
        let stride = 8;
        assert_eq!(dib.len(), header + stride * 2);
        assert_eq!(&dib[0..4], &(header as u32).to_le_bytes());
        assert_eq!(&dib[4..8], &2i32.to_le_bytes());
        // A positive height is what marks the rows as bottom-up.
        assert_eq!(&dib[8..12], &2i32.to_le_bytes());
        // The bottom row comes first, and each pixel is BGR.
        assert_eq!(&dib[header..header + 6], &[9, 8, 7, 12, 11, 10]);
        assert_eq!(&dib[header + 6..header + 8], &[0, 0]);
        assert_eq!(
            &dib[header + stride..header + stride + 6],
            &[3, 2, 1, 6, 5, 4]
        );
    }

    #[test]
    fn an_empty_region_is_refused_before_any_gdi_call() {
        assert!(capture_region(0, 0, 0, 10).is_err());
        assert!(capture_region(0, 0, 10, -1).is_err());
    }
}

/// Copies `bytes` into a movable global, the form `SetClipboardData` takes.
unsafe fn global_copy(bytes: &[u8]) -> Result<HGLOBAL, String> {
    let global = GlobalAlloc(GMEM_MOVEABLE, bytes.len())
        .map_err(|e| format!("clipboard memory: {e}"))?;
    let target = GlobalLock(global);
    if target.is_null() {
        let _ = GlobalFree(global);
        return Err("clipboard memory could not be locked".into());
    }
    std::ptr::copy_nonoverlapping(bytes.as_ptr(), target.cast(), bytes.len());
    // Returns an error once the lock count reaches zero, which is the success
    // case here.
    let _ = GlobalUnlock(global);
    Ok(global)
}

/// Replaces the clipboard with these formats, in this order (apps take the
/// first one they understand). Each global belongs to the clipboard once its
/// `SetClipboardData` succeeds; every other one is freed here.
unsafe fn set_clipboard(formats: &[(u32, &[u8])]) -> Result<(), String> {
    let mut globals = Vec::with_capacity(formats.len());
    for (format, bytes) in formats {
        match global_copy(bytes) {
            Ok(global) => globals.push((*format, global)),
            Err(error) => {
                for (_, global) in globals {
                    let _ = GlobalFree(global);
                }
                return Err(error);
            }
        }
    }
    if let Err(error) = OpenClipboard(HWND::default()) {
        for (_, global) in globals {
            let _ = GlobalFree(global);
        }
        return Err(format!("open clipboard: {error}"));
    }
    let mut written = EmptyClipboard().map_err(|e| format!("clear clipboard: {e}"));
    let mut pending = globals.into_iter();
    if written.is_ok() {
        for (format, global) in pending.by_ref() {
            if let Err(error) = SetClipboardData(format, HANDLE(global.0)) {
                let _ = GlobalFree(global);
                written = Err(format!("write clipboard: {error}"));
                break;
            }
        }
    }
    let _ = CloseClipboard();
    for (_, global) in pending {
        let _ = GlobalFree(global);
    }
    written
}
