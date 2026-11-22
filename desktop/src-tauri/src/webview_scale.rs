//! WebView2 renders at display DPI x the Windows "Text size" setting, while
//! Tauri sizes windows in DPI-only logical pixels. Above 100% text size every
//! CSS pixel is larger than a logical pixel, so every window is too small for
//! its content by that factor. Pinning the rasterization scale to the display
//! DPI keeps CSS pixels equal to logical pixels, as they are on macOS.

use tauri::{
    plugin::{Builder, TauriPlugin},
    Runtime,
};

pub fn init<R: Runtime>() -> TauriPlugin<R> {
    let builder = Builder::new("webview-scale");
    #[cfg(target_os = "windows")]
    let builder = builder.on_webview_ready(|webview| {
        let scale = webview.window().scale_factor().unwrap_or(1.0);
        if let Err(err) =
            webview.with_webview(move |platform| pin_to_display_scale(platform, scale))
        {
            tracing::warn!("webview scale setup failed: {err}");
        }
    });
    builder.build()
}

/// Also called when a window changes monitor DPI, since automatic scale
/// detection is turned off.
#[cfg(target_os = "windows")]
pub fn pin_to_display_scale(platform: tauri::webview::PlatformWebview, scale: f64) {
    use webview2_com::Microsoft::Web::WebView2::Win32::ICoreWebView2Controller3;
    use windows_core::Interface;

    let Ok(controller) = platform.controller().cast::<ICoreWebView2Controller3>() else {
        return;
    };
    // SAFETY: COM calls on the live controller Tauri owns; `with_webview` runs
    // this on the thread that created it.
    unsafe {
        let _ = controller.SetShouldDetectMonitorScaleChanges(false);
        let _ = controller.SetRasterizationScale(scale);
    }
}
