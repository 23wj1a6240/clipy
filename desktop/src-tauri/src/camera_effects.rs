use serde::{Deserialize, Serialize};
use std::sync::{Arc, OnceLock};
use tauri::{AppHandle, Emitter, Manager};

#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum Background {
    #[default]
    Original,
    Blur,
    Remove,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum Canvas {
    #[default]
    Transparent,
    Ink,
    Cream,
    Sunset,
    Aurora,
    Dunes,
    Studio,
    Custom,
    /// A flat colour from `CameraEffects::canvas_rgb`.
    Solid,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum Frame {
    #[default]
    None,
    Coral,
    White,
    Amber,
    Geometric,
    Confetti,
    Botanical,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CameraEffects {
    pub background: Background,
    pub canvas: Canvas,
    pub frame: Frame,
    /// Only read when `canvas` is `Solid`. Defaults so settings written
    /// before the colour mats still load.
    #[serde(default)]
    pub canvas_rgb: [u8; 3],
}

impl CameraEffects {
    fn canvas_rgb_packed(&self) -> u32 {
        let [r, g, b] = self.canvas_rgb;
        (u32::from(r) << 16) | (u32::from(g) << 8) | u32::from(b)
    }
}

pub(crate) struct BackgroundPixels {
    pub(crate) data: Vec<u8>,
    pub(crate) width: usize,
    pub(crate) height: usize,
}

/// Opaque RGBA background pixels, for the Windows camera-only compositor.
#[cfg(target_os = "windows")]
pub(crate) type BackgroundImage = BackgroundPixels;

#[cfg(target_os = "windows")]
pub(crate) fn background_image(canvas: Canvas) -> Result<Option<Arc<BackgroundImage>>, String> {
    background_pixels(canvas)
}
static CUSTOM: OnceLock<parking_lot::Mutex<Option<Arc<BackgroundPixels>>>> = OnceLock::new();
static BUILT_INS: OnceLock<Result<Vec<Arc<BackgroundPixels>>, String>> = OnceLock::new();

fn decode_background(bytes: &[u8]) -> Result<BackgroundPixels, String> {
    if bytes.len() > 8 * 1024 * 1024 {
        return Err("Choose a PNG or JPEG smaller than 8 MB.".into());
    }
    let mut reader = image::ImageReader::new(std::io::Cursor::new(bytes))
        .with_guessed_format()
        .map_err(|e| e.to_string())?;
    if !matches!(
        reader.format(),
        Some(image::ImageFormat::Png | image::ImageFormat::Jpeg)
    ) {
        return Err("Choose a PNG or JPEG image.".into());
    }
    let mut limits = image::Limits::default();
    limits.max_image_width = Some(4096);
    limits.max_image_height = Some(4096);
    limits.max_alloc = Some(96 * 1024 * 1024);
    reader.limits(limits);
    let mut image = reader
        .decode()
        .map_err(|_| "Image could not be decoded. Maximum size is 4096 × 4096.".to_string())?
        .resize(1920, 1080, image::imageops::FilterType::Triangle)
        .to_rgba8();
    for pixel in image.pixels_mut() {
        let alpha = pixel[3] as u32;
        for (channel, base) in [10u32, 13, 22].into_iter().enumerate() {
            pixel[channel] = ((pixel[channel] as u32 * alpha + base * (255 - alpha)) / 255) as u8;
        }
        pixel[3] = 255;
    }
    let (width, height) = image.dimensions();
    Ok(BackgroundPixels {
        data: image.into_raw(),
        width: width as usize,
        height: height as usize,
    })
}

fn background_pixels(canvas: Canvas) -> Result<Option<Arc<BackgroundPixels>>, String> {
    if canvas == Canvas::Custom {
        return CUSTOM
            .get_or_init(|| parking_lot::Mutex::new(None))
            .lock()
            .clone()
            .map(Some)
            .ok_or_else(|| {
                "Your custom background is unavailable. Upload an image or choose a background."
                    .into()
            });
    }
    let index = match canvas {
        Canvas::Aurora => 0,
        Canvas::Dunes => 1,
        Canvas::Studio => 2,
        _ => return Ok(None),
    };
    let images = BUILT_INS.get_or_init(|| {
        [
            include_bytes!("../../src/assets/backgrounds/aurora.png").as_slice(),
            include_bytes!("../../src/assets/backgrounds/dunes.png").as_slice(),
            include_bytes!("../../src/assets/backgrounds/studio.png").as_slice(),
        ]
        .iter()
        .map(|bytes| decode_background(bytes).map(Arc::new))
        .collect::<Result<Vec<_>, _>>()
    });
    images
        .as_ref()
        .map(|images| Some(images[index].clone()))
        .map_err(Clone::clone)
}

#[tauri::command]
pub fn import_camera_background(app: AppHandle, bytes: Vec<u8>) -> Result<(), String> {
    let pixels = decode_background(&bytes)?;
    let directory = app.path().app_data_dir().map_err(|e| e.to_string())?;
    std::fs::create_dir_all(&directory).map_err(|e| e.to_string())?;
    let temporary = directory.join("camera-background.tmp");
    // Re-encode pixels to discard EXIF/location and all original metadata.
    use image::ImageEncoder;
    let mut png = Vec::new();
    image::codecs::png::PngEncoder::new(&mut png)
        .write_image(
            &pixels.data,
            pixels.width as u32,
            pixels.height as u32,
            image::ExtendedColorType::Rgba8,
        )
        .map_err(|e| e.to_string())?;
    let custom = CUSTOM.get_or_init(|| parking_lot::Mutex::new(None));
    let mut guard = custom.lock();
    std::fs::write(&temporary, png).map_err(|e| e.to_string())?;
    std::fs::rename(&temporary, directory.join("camera-background.png"))
        .map_err(|e| e.to_string())?;
    *guard = Some(Arc::new(pixels));
    Ok(())
}

/// Windows composites effects in the webcam webview, so it reads the stored
/// upload back instead of using the decoded pixels.
#[cfg(target_os = "windows")]
#[tauri::command]
pub fn read_camera_background(app: AppHandle) -> Result<tauri::ipc::Response, String> {
    let path = app
        .path()
        .app_data_dir()
        .map_err(|e| e.to_string())?
        .join("camera-background.png");
    std::fs::read(path).map(tauri::ipc::Response::new).map_err(|_| {
        "Your custom background is unavailable. Upload an image or choose a background.".into()
    })
}

type EffectsResult = Result<CameraEffects, String>;
static EFFECTS: OnceLock<parking_lot::Mutex<EffectsResult>> = OnceLock::new();

fn effects_state() -> &'static parking_lot::Mutex<EffectsResult> {
    EFFECTS.get_or_init(|| {
        parking_lot::Mutex::new(Err(
            "Camera settings have not loaded. Choose a background to continue.".into(),
        ))
    })
}

pub fn init(app: &AppHandle) -> Result<(), String> {
    let custom = app.path().app_data_dir().ok().and_then(|dir| {
        let path = dir.join("camera-background.png");
        if std::fs::metadata(&path).ok()?.len() > 8 * 1024 * 1024 {
            return None;
        }
        decode_background(&std::fs::read(path).ok()?)
            .ok()
            .map(Arc::new)
    });
    *CUSTOM.get_or_init(|| parking_lot::Mutex::new(None)).lock() = custom;
    let settings = app
        .path()
        .app_data_dir()
        .map_err(|e| e.to_string())
        .and_then(|directory| load_settings(&directory.join("camera-effects.json")));
    *effects_state().lock() = settings.clone();
    settings.map(|_| ())
}

fn load_settings(path: &std::path::Path) -> EffectsResult {
    match std::fs::read(path) {
        Ok(bytes) => serde_json::from_slice(&bytes).map_err(|_| {
            "Camera settings could not be read. Choose a background to reset them.".into()
        }),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(CameraEffects::default()),
        Err(_) => {
            Err("Camera settings could not be opened. Choose a background to reset them.".into())
        }
    }
}

pub fn current() -> EffectsResult {
    effects_state().lock().clone()
}

/// Touch up and Auto lighting, applied by the processor to every
/// frame it handles: the bubble preview and camera-only recordings alike.
/// `touch_up` is 0 (off) to 1.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Look {
    pub touch_up: f32,
    pub auto_lighting: bool,
}

impl Look {
    pub fn active(self) -> bool {
        self.touch_up > 0.0 || self.auto_lighting
    }
}

static LOOK: OnceLock<parking_lot::Mutex<Look>> = OnceLock::new();

fn look_state() -> &'static parking_lot::Mutex<Look> {
    LOOK.get_or_init(|| parking_lot::Mutex::new(Look::default()))
}

pub fn set_look(look: Look) {
    *look_state().lock() = Look {
        touch_up: look.touch_up.clamp(0.0, 1.0),
        ..look
    };
}

pub fn current_look() -> Look {
    *look_state().lock()
}

// The macOS processor meters a detected face, so it can use a tighter range
// than the whole-frame Windows implementation. Keep the non-macOS values in
// sync with `cameraLook.ts` until that path becomes face-aware too.
#[cfg(target_os = "macos")]
pub(crate) const AUTO_LIGHTING_TARGET_LUMA: f32 = 0.48;
#[cfg(not(target_os = "macos"))]
pub(crate) const AUTO_LIGHTING_TARGET_LUMA: f32 = 0.5;
#[cfg(target_os = "macos")]
const AUTO_LIGHTING_MIN_GAIN: f32 = 0.95;
#[cfg(not(target_os = "macos"))]
const AUTO_LIGHTING_MIN_GAIN: f32 = 0.85;
#[cfg(target_os = "macos")]
const AUTO_LIGHTING_MAX_GAIN: f32 = 1.35;
#[cfg(not(target_os = "macos"))]
const AUTO_LIGHTING_MAX_GAIN: f32 = 1.6;
const AUTO_LIGHTING_EASE: f32 = 0.15;
/// Frames between luminance probes; the gain eases toward each reading.
pub(crate) const AUTO_LIGHTING_PROBE_INTERVAL: u32 = 6;

/// The next eased exposure gain after measuring `luma` (0..1).
pub(crate) fn next_gain(current: f32, luma: f32) -> f32 {
    if !luma.is_finite() || luma <= 0.0 {
        return current;
    }
    let target =
        (AUTO_LIGHTING_TARGET_LUMA / luma).clamp(AUTO_LIGHTING_MIN_GAIN, AUTO_LIGHTING_MAX_GAIN);
    current + (target - current) * AUTO_LIGHTING_EASE
}

#[tauri::command]
pub fn get_camera_effects() -> EffectsResult {
    current()
}

#[tauri::command]
pub fn set_camera_effects(app: AppHandle, effects: CameraEffects) -> Result<(), String> {
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    if effects.background != Background::Original {
        return Err("Background effects are available on macOS and Windows.".into());
    }
    if effects.background == Background::Remove {
        background_pixels(effects.canvas)?;
    }
    let directory = app.path().app_data_dir().map_err(|e| e.to_string())?;
    std::fs::create_dir_all(&directory).map_err(|e| e.to_string())?;
    let mut settings = effects_state().lock();
    let bytes = serde_json::to_vec(&effects).map_err(|e| e.to_string())?;
    let temporary = directory.join("camera-effects.json.tmp");
    std::fs::write(&temporary, bytes).map_err(|e| e.to_string())?;
    std::fs::rename(&temporary, directory.join("camera-effects.json"))
        .map_err(|e| e.to_string())?;
    *settings = Ok(effects);
    app.emit("camera-effects-changed", effects)
        .map_err(|e| e.to_string())
}

#[cfg(target_os = "macos")]
extern "C" {
    fn clipy_camera_face_state_create() -> *mut std::ffi::c_void;
    fn clipy_camera_face_state_destroy(state: *mut std::ffi::c_void);
    fn clipy_camera_effect(
        face_state: *mut std::ffi::c_void,
        input: *const u8,
        width: usize,
        height: usize,
        stride: usize,
        output: *mut u8,
        output_stride: usize,
        mode: i32,
        canvas: i32,
        frame: i32,
        canvas_rgb: u32,
        opaque: bool,
        background: *const u8,
        background_width: usize,
        background_height: usize,
        touch_up: f32,
        exposure_gain: f32,
        luma_out: *mut f32,
    ) -> i32;
}

#[cfg(target_os = "macos")]
pub struct Processor {
    face_state: *mut std::ffi::c_void,
    converter: Option<ffmpeg::software::scaling::Context>,
    /// RGBA back to the input format, for a preview frame that only needed
    /// the look: it keeps the JPEG path instead of the far dearer PNG one.
    back_converter: Option<ffmpeg::software::scaling::Context>,
    /// Eased auto-lighting gain, per processor so preview and recording
    /// each settle on their own frames.
    gain: f32,
    frame: u32,
}

#[cfg(target_os = "macos")]
impl Default for Processor {
    fn default() -> Self {
        Self {
            // Each processor owns one retained native cache, released by Drop.
            face_state: unsafe { clipy_camera_face_state_create() },
            converter: None,
            back_converter: None,
            gain: 1.0,
            frame: 0,
        }
    }
}

#[cfg(target_os = "macos")]
impl Drop for Processor {
    fn drop(&mut self) {
        // The synchronous processing call never retains this owned pointer.
        unsafe { clipy_camera_face_state_destroy(self.face_state) };
    }
}

#[cfg(target_os = "macos")]
impl Processor {
    pub fn process(
        &mut self,
        input: ffmpeg::frame::Video,
        opaque: bool,
    ) -> Result<ffmpeg::frame::Video, String> {
        self.process_with_settings(input, opaque, current())
    }

    fn process_with_settings(
        &mut self,
        input: ffmpeg::frame::Video,
        opaque: bool,
        settings: EffectsResult,
    ) -> Result<ffmpeg::frame::Video, String> {
        use ffmpeg::{format::Pixel, frame::Video, software::scaling};
        let settings = settings?;
        let look = current_look();
        if settings.background == Background::Original
            && (!opaque || settings.frame == Frame::None)
            && !look.active()
        {
            return Ok(input);
        }
        if !look.auto_lighting {
            self.gain = 1.0;
        }
        let measure = look.auto_lighting && self.frame % AUTO_LIGHTING_PROBE_INTERVAL == 0;
        self.frame = self.frame.wrapping_add(1);
        let mut luma: f32 = -1.0;
        let background = if settings.background == Background::Remove {
            background_pixels(settings.canvas)?
        } else {
            None
        };
        let (width, height) = (input.width(), input.height());
        if !self.converter.as_ref().is_some_and(|converter| {
            converter.input().format == input.format()
                && converter.input().width == width
                && converter.input().height == height
        }) {
            self.converter = Some(
                scaling::Context::get(
                    input.format(),
                    width,
                    height,
                    Pixel::RGBA,
                    width,
                    height,
                    scaling::flag::Flags::FAST_BILINEAR,
                )
                .map_err(|e| e.to_string())?,
            );
        }
        let mut rgba = Video::new(Pixel::RGBA, width, height);
        self.converter
            .as_mut()
            .ok_or("Camera effect converter is unavailable")?
            .run(&input, &mut rgba)
            .map_err(|e| e.to_string())?;
        let mut output = Video::new(Pixel::RGBA, width, height);
        let output_stride = output.stride(0);
        // Both buffers own width × height RGBA pixels with FFmpeg-provided strides; the native call is synchronous and retains neither pointer.
        let status = unsafe {
            clipy_camera_effect(
                self.face_state,
                rgba.data(0).as_ptr(),
                width as usize,
                height as usize,
                rgba.stride(0),
                output.data_mut(0).as_mut_ptr(),
                output_stride,
                settings.background as i32,
                settings.canvas as i32,
                settings.frame as i32,
                settings.canvas_rgb_packed(),
                opaque,
                background
                    .as_ref()
                    .map_or(std::ptr::null(), |pixels| pixels.data.as_ptr()),
                background.as_ref().map_or(0, |pixels| pixels.width),
                background.as_ref().map_or(0, |pixels| pixels.height),
                look.touch_up,
                self.gain,
                if measure { &mut luma } else { std::ptr::null_mut() },
            )
        };
        if status != 0 {
            return Err(
                "Camera background effect is unavailable. Choose Original to continue.".into(),
            );
        }
        if measure {
            self.gain = next_gain(self.gain, luma);
        }
        if !opaque && settings.background == Background::Original {
            // Only the look was applied, so there is no alpha to carry: hand
            // the frame back in its own format and the JPEG encoder keeps it.
            if !self.back_converter.as_ref().is_some_and(|converter| {
                converter.output().format == input.format()
                    && converter.output().width == width
                    && converter.output().height == height
            }) {
                self.back_converter = Some(
                    scaling::Context::get(
                        Pixel::RGBA,
                        width,
                        height,
                        input.format(),
                        width,
                        height,
                        scaling::flag::Flags::FAST_BILINEAR,
                    )
                    .map_err(|e| e.to_string())?,
                );
            }
            let mut back = Video::new(input.format(), width, height);
            self.back_converter
                .as_mut()
                .ok_or("Camera look converter is unavailable")?
                .run(&output, &mut back)
                .map_err(|e| e.to_string())?;
            back.set_color_range(input.color_range());
            back.set_pts(input.pts());
            return Ok(back);
        }
        if opaque {
            for pixel in output.data_mut(0).chunks_exact_mut(4) {
                pixel.swap(0, 2);
            }
            output.set_format(Pixel::BGRA);
        }
        output.set_pts(input.pts());
        Ok(output)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gallery_assets_decode_and_custom_input_is_bounded() {
        for canvas in [Canvas::Aurora, Canvas::Dunes, Canvas::Studio] {
            let image = background_pixels(canvas).unwrap().unwrap();
            assert!(image.width > 0 && image.width <= 1920 && image.height <= 1080);
            assert_eq!(image.data.len(), image.width * image.height * 4);
        }
        assert!(decode_background(b"not an image").is_err());
        assert!(decode_background(&vec![0; 8 * 1024 * 1024 + 1]).is_err());
        use image::ImageEncoder;
        let mut oversized = Vec::new();
        image::codecs::png::PngEncoder::new(&mut oversized)
            .write_image(
                &vec![255; 4097 * 4],
                4097,
                1,
                image::ExtendedColorType::Rgba8,
            )
            .unwrap();
        assert!(decode_background(&oversized).is_err());
        let mut transparent = Vec::new();
        image::codecs::png::PngEncoder::new(&mut transparent)
            .write_image(&[255, 0, 0, 0], 1, 1, image::ExtendedColorType::Rgba8)
            .unwrap();
        assert_eq!(
            &decode_background(&transparent).unwrap().data[..4],
            &[10, 13, 22, 255]
        );
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn decorative_frames_are_baked_for_camera_only_and_deferred_until_bubble_crop() {
        for opaque in [false, true] {
            for frame in [Frame::Geometric, Frame::Confetti, Frame::Botanical] {
                let mut input = ffmpeg::frame::Video::new(ffmpeg::format::Pixel::RGBA, 100, 100);
                input.data_mut(0).fill(255);
                let output = Processor::default()
                    .process_with_settings(
                        input,
                        opaque,
                        Ok(CameraEffects {
                            frame,
                            ..CameraEffects::default()
                        }),
                    )
                    .unwrap();
                if opaque {
                    assert_ne!(&output.data(0)[..3], &[255, 255, 255]);
                } else {
                    assert_eq!(&output.data(0)[..3], &[255, 255, 255]);
                }
                assert_eq!(output.data(0)[3], 255);
                let center = output.stride(0) * 50 + 50 * 4;
                assert_eq!(&output.data(0)[center..center + 4], &[255, 255, 255, 255]);
            }
        }
    }

    #[test]
    fn new_settings_default_to_original_but_stored_settings_require_all_fields() {
        assert!(serde_json::from_str::<CameraEffects>("{}").is_err());
        assert_eq!(CameraEffects::default().background, Background::Original);
        assert_eq!(CameraEffects::default().frame, Frame::None);
    }

    #[test]
    fn camera_effects_round_trip_and_reject_unknown_native_modes() {
        let settings = CameraEffects {
            background: Background::Remove,
            canvas: Canvas::Sunset,
            frame: Frame::Coral,
            canvas_rgb: [0, 0, 0],
        };
        assert_eq!(
            serde_json::from_slice::<CameraEffects>(&serde_json::to_vec(&settings).unwrap())
                .unwrap(),
            settings
        );
        assert!(serde_json::from_str::<CameraEffects>(r#"{"background":"unknown"}"#).is_err());
        assert!(serde_json::from_str::<CameraEffects>(r#"{"frame":"unknown"}"#).is_err());
    }

    #[test]
    fn missing_settings_allow_first_launch_but_malformed_or_unreadable_settings_fail_closed() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("camera-effects.json");
        assert_eq!(load_settings(&path).unwrap(), CameraEffects::default());
        for content in [
            r#"{"background":"blur""#,
            "{}",
            r#"{"background":"original","canvas":"transparent","frame":"none","unexpected":true}"#,
        ] {
            std::fs::write(&path, content).unwrap();
            let settings = load_settings(&path);
            assert!(settings.is_err());
            #[cfg(target_os = "macos")]
            for opaque in [false, true] {
                let frame = ffmpeg::frame::Video::empty();
                assert!(Processor::default()
                    .process_with_settings(frame, opaque, settings.clone())
                    .is_err());
            }
        }
        std::fs::remove_file(&path).unwrap();
        std::fs::create_dir(&path).unwrap();
        assert!(load_settings(&path).is_err());
        std::fs::remove_dir(&path).unwrap();
        std::fs::write(
            &path,
            serde_json::to_vec(&CameraEffects::default()).unwrap(),
        )
        .unwrap();
        let settings = load_settings(&path);
        assert_eq!(settings, Ok(CameraEffects::default()));
        #[cfg(target_os = "macos")]
        assert!(Processor::default()
            .process_with_settings(ffmpeg::frame::Video::empty(), false, settings)
            .is_ok());
    }

    #[test]
    fn exposure_eases_toward_a_bounded_target_and_ignores_bad_readings() {
        use super::next_gain;
        #[cfg(target_os = "macos")]
        {
            assert!((next_gain(1.0, 0.25) - 1.0525).abs() < 1e-6);
            assert!((next_gain(1.0, 0.9) - (1.0 + (0.95 - 1.0) * 0.15)).abs() < 1e-6);
            assert_eq!(next_gain(1.0, 0.48), 1.0);
        }
        #[cfg(not(target_os = "macos"))]
        {
            assert!((next_gain(1.0, 0.25) - 1.09).abs() < 1e-6);
            assert!((next_gain(1.0, 0.9) - (1.0 + (0.85 - 1.0) * 0.15)).abs() < 1e-6);
            assert_eq!(next_gain(1.0, 0.5), 1.0);
        }
        assert_eq!(next_gain(1.2, 0.0), 1.2);
        assert_eq!(next_gain(1.2, f32::NAN), 1.2);
    }

    #[test]
    fn a_look_is_active_only_with_touch_up_or_auto_lighting() {
        use super::Look;
        assert!(!Look::default().active());
        assert!(Look { touch_up: 0.3, auto_lighting: false }.active());
        assert!(Look { touch_up: 0.0, auto_lighting: true }.active());
    }
}
