//! Camera effects for Windows camera-only recordings: backgrounds, the
//! decorative frame, Touch up and Auto lighting.
//!
//! macOS applies these natively in `camera_effects.m`. On Windows the bubble
//! applies them in its webview, but a camera-only recording never goes
//! through the bubble, so the recorder runs the same MediaPipe selfie model
//! itself through Windows ML (part of Windows 10 and 11, no runtime to ship).
//!
//! Only the model's small 256 × 256 mask is handled in Rust. The full-size
//! per-pixel work (the blur, the cut-out, the background, the soft-focus
//! Touch up and the exposure gain) is an FFmpeg filter graph, so the frame
//! budget holds whether or not this crate was built with optimisations. The
//! decorative frame touches only the border pixels and is painted in Rust,
//! as `camera_effects.m` does.

#![cfg(target_os = "windows")]

use std::collections::VecDeque;
use std::ffi::CString;
use std::path::{Path, PathBuf};
use std::sync::mpsc as std_mpsc;
use std::time::{Duration, Instant};

use ffmpeg::{filter, format::Pixel, frame, software::scaling};
use windows::core::{Interface, HSTRING};
use windows::AI::MachineLearning::{
    LearningModel, LearningModelBinding, LearningModelDevice, LearningModelDeviceKind,
    LearningModelSession, TensorFeatureDescriptor, TensorFloat,
};

use super::windows::VideoFrame;
use crate::camera_effects::{
    next_gain, Background, CameraEffects, Canvas, Frame, Look, AUTO_LIGHTING_PROBE_INTERVAL,
    AUTO_LIGHTING_TARGET_LUMA,
};

/// The model file inside the bundled `segmentation` resources.
pub const MODEL_FILE: &str = "selfie_segmenter.onnx";

/// Blur strength as a share of the frame's longest side, as in
/// `camera_effects.m`.
const BLUR_SHARE: f64 = 0.035;
/// Auto lighting adds this contrast with the exposure, as `camera_effects.m`
/// and the bubble's `cameraLook.ts` do.
const AUTO_LIGHTING_CONTRAST: f32 = 1.05;
/// Below this, the gain is close enough to 1 that macOS applies nothing.
const AUTO_LIGHTING_DEAD_BAND: f32 = 0.01;
/// The `colorlevels` instance that applies the exposure gain, addressed by
/// name so the gain can change while the graph runs.
const LIGHT_FILTER: &str = "colorlevels@light";
/// The background is blurred at a quarter size; a wide Gaussian loses nothing
/// there and costs a sixteenth.
const BLUR_DOWNSCALE: u32 = 4;
const BLUR_PASSES: u32 = 2;
const INK: [u8; 3] = [10, 13, 22];
const CREAM: [u8; 3] = [250, 246, 240];
const SUNSET_FROM: [u8; 3] = [255, 77, 41];
const SUNSET_TO: [u8; 3] = [255, 176, 33];

// Mask refinement: the bubble's `maskRefiner.ts`, plus a motion threshold so
// smoothing never makes the cut-out trail a moving person.
const TEMPORAL_WEIGHT: f32 = 0.5;
/// A per-pixel change above this is movement, not flicker.
const MOTION_THRESHOLD: f32 = 0.25;
const TEMPORAL_RESET: Duration = Duration::from_millis(250);
const GUIDED_EPS: f32 = 1e-3;
const GUIDED_RADIUS: usize = 2;
const EDGE_LOW: f32 = 0.3;
const EDGE_HIGH: f32 = 0.7;

/// A camera frame ready for the encoder, with the time it was captured.
pub type Composited = (Vec<u8>, Instant);

/// Everything the pipeline applies to a frame: the background and frame from
/// the effects settings, plus the look (Touch up and Auto lighting).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct FrameSettings {
    pub effects: CameraEffects,
    pub look: Look,
}

impl FrameSettings {
    /// Whether a frame needs the filter graph. A decorative frame alone is
    /// painted straight onto the pixels.
    fn needs_graph(&self) -> bool {
        self.effects.background != Background::Original || self.look.active()
    }
}

/// Which settings apply, read as each frame enters the pipeline. `None`
/// means no settings are loaded and the frame is recorded as captured.
pub type SettingsSource = Box<dyn Fn() -> Option<FrameSettings> + Send>;
/// Receives finished frames in capture order; returns `false` once nothing
/// is listening any more.
pub type FrameSink = Box<dyn FnMut(VideoFrame) -> bool + Send>;
pub type ErrorSink = Box<dyn Fn(String) + Send>;

/// Frames waiting at each stage. Deeper only adds delay: a frame that cannot
/// be taken is dropped at the camera, not queued.
const PIPELINE_DEPTH: usize = 2;

/// Camera effects as two stages on their own threads: segmentation, then
/// compositing.
///
/// Each frame travels with the mask computed from that same frame, so the
/// cut-out always matches the picture, even while moving; the frame keeps its
/// capture time, so the delay this adds never shifts it against the audio.
/// Frames with no effect take the same path, which keeps them in order.
pub struct EffectsPipeline {
    input: Option<std_mpsc::SyncSender<VideoFrame>>,
    threads: Vec<std::thread::JoinHandle<()>>,
}

impl EffectsPipeline {
    pub fn start(
        model_path: PathBuf,
        settings: SettingsSource,
        sink: FrameSink,
        errors: ErrorSink,
    ) -> Result<Self, String> {
        let (input, frames) = std_mpsc::sync_channel::<VideoFrame>(PIPELINE_DEPTH);
        let (segmented_tx, segmented) = std_mpsc::sync_channel::<Segmented>(PIPELINE_DEPTH);
        let segmentation = std::thread::Builder::new()
            .name("clipy-camera-segmentation".to_string())
            .spawn(move || segmentation_stage(&model_path, &settings, &frames, &segmented_tx))
            .map_err(|e| format!("segmentation thread: {e}"))?;
        let compositing = std::thread::Builder::new()
            .name("clipy-camera-compositing".to_string())
            .spawn(move || compositing_stage(&segmented, sink, errors))
            .map_err(|e| format!("compositing thread: {e}"))?;
        Ok(Self {
            input: Some(input),
            threads: vec![segmentation, compositing],
        })
    }

    /// Queues a frame; `false` when the pipeline is full and it was dropped.
    pub fn push(&self, frame: VideoFrame) -> bool {
        self.input
            .as_ref()
            .is_some_and(|input| input.try_send(frame).is_ok())
    }

    /// Lets queued frames finish, then stops both stages.
    pub fn finish(mut self) {
        self.shutdown();
    }

    fn shutdown(&mut self) {
        // Closing the input ends the first stage, which closes the second.
        self.input = None;
        for thread in self.threads.drain(..) {
            let _ = thread.join();
        }
    }
}

impl Drop for EffectsPipeline {
    fn drop(&mut self) {
        self.shutdown();
    }
}

/// A refined person mask at the model's size.
struct MaskFrame {
    data: Vec<u8>,
    width: u32,
    height: u32,
}

struct Segmented {
    frame: VideoFrame,
    settings: Option<FrameSettings>,
    /// Present when the background needs the person cut out.
    mask: Option<Result<MaskFrame, String>>,
}

fn segmentation_stage(
    model_path: &Path,
    settings: &SettingsSource,
    frames: &std_mpsc::Receiver<VideoFrame>,
    out: &std_mpsc::SyncSender<Segmented>,
) {
    // Windows ML objects live on this thread, inside its COM scope. Loaded on
    // first use and kept, since the model takes a moment to warm up.
    let com = super::windows_camera::ComScope::enter();
    let mut segmenter: Option<Result<Segmenter, String>> = None;
    let mut refiner = MaskRefiner::default();
    while let Ok(frame) = frames.recv() {
        let settings = settings();
        let mask = settings
            .filter(|settings| settings.effects.background != Background::Original)
            .map(|_| {
                let segmenter = segmenter.get_or_insert_with(|| match &com {
                    Ok(_) => Segmenter::load(model_path),
                    Err(err) => Err(err.clone()),
                });
                mask_for(segmenter, &mut refiner, &frame)
            });
        if out.send(Segmented { frame, settings, mask }).is_err() {
            break;
        }
    }
}

fn mask_for(
    segmenter: &mut Result<Segmenter, String>,
    refiner: &mut MaskRefiner,
    frame: &VideoFrame,
) -> Result<MaskFrame, String> {
    let segmenter = segmenter.as_mut().map_err(|err| err.clone())?;
    let (width, height) = segmenter.size;
    let (confidence, guide) = segmenter.segment(&frame.bgra, frame.width, frame.height)?;
    let mut data = vec![0u8; width * height];
    refiner.refine(confidence, guide, width, height, frame.captured_at, &mut data);
    Ok(MaskFrame {
        data,
        width: width as u32,
        height: height as u32,
    })
}

fn compositing_stage(
    segmented: &std_mpsc::Receiver<Segmented>,
    mut sink: FrameSink,
    errors: ErrorSink,
) {
    let mut compositor: Option<Compositor> = None;
    // Kept outside the compositor so a rebuilt graph (a changed background
    // or Touch up strength) carries the eased exposure on, without a flash.
    let mut lighting = Lighting::default();
    // The settings an error was last reported for, so each is reported once.
    let mut reported: Option<FrameSettings> = None;
    let mut listening = true;
    let mut deliver = |frame: VideoFrame| {
        if listening {
            listening = sink(frame);
        }
    };
    while let Ok(Segmented { frame, settings, mask }) = segmented.recv() {
        let Some(settings) = settings else {
            compositor = None;
            reported = None;
            deliver(frame);
            continue;
        };
        if !settings.look.auto_lighting {
            lighting = Lighting::default();
        }
        let decorative = settings.effects.frame;
        if !settings.needs_graph() {
            compositor = None;
            reported = None;
            let mut frame = frame;
            draw_frame(&mut frame.bgra, frame.width, frame.height, decorative);
            deliver(frame);
            continue;
        }
        let needs_mask = settings.effects.background != Background::Original;
        let mask = if needs_mask {
            mask.unwrap_or_else(|| Err("no mask for this frame".to_string()))
                .map(Some)
        } else {
            Ok(None)
        };
        let composited = mask.and_then(|mask| {
            let key = CompositeKey {
                width: frame.width,
                height: frame.height,
                mask_width: mask.as_ref().map_or(0, |mask| mask.width),
                mask_height: mask.as_ref().map_or(0, |mask| mask.height),
                background: settings.effects.background,
                canvas: settings.effects.canvas,
                canvas_rgb: settings.effects.canvas_rgb,
                touch_up: touch_up_level(settings.look),
                auto_lighting: settings.look.auto_lighting,
            };
            if compositor.as_ref().map(|compositor| compositor.key) != Some(key) {
                compositor = Some(Compositor::build(key)?);
            }
            compositor
                .as_mut()
                .ok_or_else(|| "compositor missing".to_string())?
                .push(
                    &frame.bgra,
                    mask.as_ref().map(|mask| mask.data.as_slice()),
                    frame.captured_at,
                    &mut lighting,
                )
        });
        match composited {
            Ok(done) => {
                reported = None;
                for (mut bgra, captured_at) in done {
                    draw_frame(&mut bgra, frame.width, frame.height, decorative);
                    deliver(VideoFrame {
                        bgra,
                        width: frame.width,
                        height: frame.height,
                        captured_at,
                    });
                }
            }
            // As on macOS, a failed effect drops the frame rather than
            // recording the background the user chose to hide.
            Err(err) => {
                compositor = None;
                if reported != Some(settings) {
                    reported = Some(settings);
                    tracing::warn!("camera effect failed: {err}");
                    errors(format!(
                        "Camera background effect is unavailable. Choose Original to continue. ({err})"
                    ));
                }
            }
        }
    }
}

/// Touch up strength as the whole-percent step the graph is built for, 0
/// when off. Quantised so the graph is not rebuilt on float noise.
fn touch_up_level(look: Look) -> u8 {
    (look.touch_up.clamp(0.0, 1.0) * 100.0).round() as u8
}

/// The bubble's Touch up (`cameraLook.ts`): the frame is mixed with a
/// downscaled copy of itself drawn back at partial opacity. Returns the
/// downscale factor and the opacity of the soft copy.
fn touch_up_blend(level: u8) -> (f64, f64) {
    let strength = f64::from(level) / 100.0;
    (3.0 + 5.0 * strength, 0.2 + 0.5 * strength)
}

/// Auto lighting: the eased exposure gain and how many frames it has seen.
struct Lighting {
    gain: f32,
    frames: u32,
}

impl Default for Lighting {
    fn default() -> Self {
        Self {
            gain: 1.0,
            frames: 0,
        }
    }
}

/// `colorlevels` points for exposure `gain` with the auto-lighting contrast:
/// `(input black, input white, output white)`. Output is `1.05 × (gain × in
/// − 0.5) + 0.5`, as on macOS. Brightening lowers the input white point and
/// darkening lowers the output one, since an input point cannot exceed 1.
fn light_levels(gain: f32) -> (f32, f32, f32) {
    if (gain - 1.0).abs() < AUTO_LIGHTING_DEAD_BAND {
        return (0.0, 1.0, 1.0);
    }
    let slope = AUTO_LIGHTING_CONTRAST * gain;
    let black = (AUTO_LIGHTING_CONTRAST - 1.0) * 0.5 / slope;
    let white = (black + 1.0 / slope).min(1.0);
    (black, white, (white - black) * slope)
}

/// The scene's luminance behind a frame measured after `light_levels(gain)`
/// was applied (clipping aside).
fn scene_luma(measured: f32, gain: f32) -> f32 {
    if (gain - 1.0).abs() < AUTO_LIGHTING_DEAD_BAND {
        return measured;
    }
    (measured + (AUTO_LIGHTING_CONTRAST - 1.0) * 0.5) / (AUTO_LIGHTING_CONTRAST * gain)
}

/// Mean luminance (0..1) of a BGRA frame, sampled on a grid: exact enough to
/// steer exposure, cheap enough for an unoptimised build.
fn mean_luma(bgra: &[u8], width: u32) -> f32 {
    const STEP: usize = 4;
    let row_bytes = width as usize * 4;
    let (mut sum, mut count) = (0.0f64, 0u32);
    for row in bgra.chunks_exact(row_bytes.max(4)).step_by(STEP) {
        for pixel in row.chunks_exact(4 * STEP) {
            sum += 0.0722 * f64::from(pixel[0])
                + 0.7152 * f64::from(pixel[1])
                + 0.2126 * f64::from(pixel[2]);
            count += 1;
        }
    }
    if count == 0 {
        return AUTO_LIGHTING_TARGET_LUMA;
    }
    (sum / f64::from(count) / 255.0) as f32
}

/// Paints the decorative frame `camera_effects.m` gives macOS camera-only
/// recordings: a solid band for Coral, White and Amber, and the geometric,
/// confetti and botanical patterns at a wider band. Only the border is
/// touched.
fn draw_frame(bgra: &mut [u8], width: u32, height: u32, frame: Frame) {
    if frame == Frame::None {
        return;
    }
    let (width, height) = (width as usize, height as usize);
    let decorative = matches!(frame, Frame::Geometric | Frame::Confetti | Frame::Botanical);
    let border = (width.min(height) / if decorative { 18 } else { 80 }).max(3);
    let mut paint = |x: usize, y: usize| {
        let [r, g, b] = frame_pixel(frame, x, y, border);
        let at = (y * width + x) * 4;
        if let Some(pixel) = bgra.get_mut(at..at + 4) {
            pixel.copy_from_slice(&[b, g, r, 255]);
        }
    };
    for y in 0..height {
        if y < border || y >= height.saturating_sub(border) {
            for x in 0..width {
                paint(x, y);
            }
        } else {
            for x in (0..border.min(width)).chain(width.saturating_sub(border)..width) {
                paint(x, y);
            }
        }
    }
}

/// The frame's colour at a border pixel, RGB, matching `camera_effects.m`.
fn frame_pixel(frame: Frame, x: usize, y: usize, border: usize) -> [u8; 3] {
    match frame {
        Frame::None | Frame::Coral => [255, 77, 41],
        Frame::White => [238, 240, 245],
        Frame::Amber => [255, 176, 32],
        // A repeating geometric ribbon.
        Frame::Geometric => {
            let cell = border.max(1);
            if (x / cell + y / cell) % 2 == 1 {
                [87, 131, 225]
            } else {
                [255, 77, 41]
            }
        }
        // Confetti, deterministic across frames.
        Frame::Confetti => {
            let cell = (border / 2).max(4);
            let seed = (x / cell * 7 + y / cell * 13) % 4;
            if (x % cell + y % cell) % cell > cell / 3 {
                [[247, 214, 81], [137, 107, 218], [244, 118, 129], [85, 183, 156]][seed]
            } else {
                [248, 243, 232]
            }
        }
        // Alternating leafy arcs.
        Frame::Botanical => {
            let cell = border.max(6);
            let half = cell as f64 / 2.0;
            let (dx, dy) = ((x % cell) as f64 - half, (y % cell) as f64 - half);
            if dx * dx + dy * dy < (cell * cell) as f64 * 0.19 {
                [68, 120, 82]
            } else {
                [214, 231, 207]
            }
        }
    }
}

/// Runs the selfie model on Windows ML.
struct Segmenter {
    session: LearningModelSession,
    binding: LearningModelBinding,
    input_name: HSTRING,
    output_name: HSTRING,
    shape: Vec<i64>,
    channels_first: bool,
    /// Model input width and height.
    size: (usize, usize),
    scaler: Option<(u32, u32, scaling::Context)>,
    source: frame::Video,
    rgb: frame::Video,
    guide: Vec<u8>,
    input: Vec<f32>,
    output: Vec<f32>,
}

impl Segmenter {
    fn load(path: &Path) -> Result<Self, String> {
        let path = plain_path(path);
        if !path.is_file() {
            return Err(format!("segmentation model missing at {}", path.display()));
        }
        let model = LearningModel::LoadFromFilePath(&HSTRING::from(path.as_os_str()))
            .map_err(|e| format!("load segmentation model: {e}"))?;
        let input = model
            .InputFeatures()
            .and_then(|features| features.GetAt(0))
            .map_err(|e| format!("segmentation model input: {e}"))?;
        let output = model
            .OutputFeatures()
            .and_then(|features| features.GetAt(0))
            .map_err(|e| format!("segmentation model output: {e}"))?;
        let input_name = input.Name().map_err(|e| e.to_string())?;
        let output_name = output.Name().map_err(|e| e.to_string())?;
        let descriptor: TensorFeatureDescriptor = input
            .cast()
            .map_err(|e| format!("segmentation model input is not a tensor: {e}"))?;
        let dims = descriptor.Shape().map_err(|e| e.to_string())?;
        let mut shape = vec![0i64; dims.Size().map_err(|e| e.to_string())? as usize];
        dims.GetMany(0, &mut shape).map_err(|e| e.to_string())?;
        // A free batch dimension is reported as -1.
        for dim in &mut shape {
            if *dim < 1 {
                *dim = 1;
            }
        }
        let (channels_first, size) = match shape.as_slice() {
            [1, 3, h, w] => (true, (*w as usize, *h as usize)),
            [1, h, w, 3] => (false, (*w as usize, *h as usize)),
            other => return Err(format!("unexpected segmentation input shape {other:?}")),
        };

        // The model is small enough that the CPU beats a GPU round trip.
        let device = LearningModelDevice::Create(LearningModelDeviceKind::Cpu)
            .map_err(|e| format!("segmentation device: {e}"))?;
        let session = LearningModelSession::CreateFromModelOnDevice(&model, &device)
            .map_err(|e| format!("segmentation session: {e}"))?;
        let binding = LearningModelBinding::CreateFromSession(&session)
            .map_err(|e| format!("segmentation binding: {e}"))?;
        let pixels = size.0 * size.1;
        Ok(Self {
            session,
            binding,
            input_name,
            output_name,
            shape,
            channels_first,
            size,
            scaler: None,
            source: frame::Video::empty(),
            rgb: frame::Video::new(Pixel::RGB24, size.0 as u32, size.1 as u32),
            guide: vec![0; pixels * 3],
            input: vec![0.0; pixels * 3],
            output: vec![0.0; pixels],
        })
    }

    /// Returns the person confidence and the RGB frame it was computed from,
    /// both at the model's size.
    fn segment(&mut self, bgra: &[u8], width: u32, height: u32) -> Result<(&[f32], &[u8]), String> {
        let (model_width, model_height) = self.size;
        if self.scaler.as_ref().map(|(w, h, _)| (*w, *h)) != Some((width, height)) {
            let scaler = scaling::Context::get(
                Pixel::BGRA,
                width,
                height,
                Pixel::RGB24,
                model_width as u32,
                model_height as u32,
                scaling::flag::Flags::AREA,
            )
            .map_err(|e| format!("segmentation scaler: {e}"))?;
            self.scaler = Some((width, height, scaler));
            self.source = frame::Video::new(Pixel::BGRA, width, height);
        }
        copy_into_plane(&mut self.source, 0, bgra, width as usize * 4);
        let (_, _, scaler) = self.scaler.as_mut().ok_or("segmentation scaler missing")?;
        scaler
            .run(&self.source, &mut self.rgb)
            .map_err(|e| format!("segmentation scale: {e}"))?;
        copy_from_plane(&self.rgb, 0, &mut self.guide, model_width * 3);

        let pixels = model_width * model_height;
        for (index, rgb) in self.guide.chunks_exact(3).enumerate() {
            for (channel, value) in rgb.iter().enumerate() {
                let slot = if self.channels_first {
                    channel * pixels + index
                } else {
                    index * 3 + channel
                };
                self.input[slot] = *value as f32 / 255.0;
            }
        }

        let tensor = TensorFloat::CreateFromShapeArrayAndDataArray(&self.shape, &self.input)
            .map_err(|e| format!("segmentation input: {e}"))?;
        self.binding
            .Bind(&self.input_name, &tensor)
            .map_err(|e| format!("segmentation bind: {e}"))?;
        let result = self
            .session
            .Evaluate(&self.binding, &HSTRING::new())
            .map_err(|e| format!("segmentation run: {e}"))?;
        let output: TensorFloat = result
            .Outputs()
            .and_then(|outputs| outputs.Lookup(&self.output_name))
            .and_then(|value| value.cast())
            .map_err(|e| format!("segmentation output: {e}"))?;
        let values = output.GetAsVectorView().map_err(|e| e.to_string())?;
        let count = values
            .GetMany(0, &mut self.output)
            .map_err(|e| format!("segmentation output: {e}"))? as usize;
        if count < pixels {
            return Err(format!("segmentation output has {count} values, expected {pixels}"));
        }
        Ok((&self.output, &self.guide))
    }
}

/// The bubble's mask refinement (`maskRefiner.ts`), at the model's size:
/// temporal smoothing, a guided filter against the camera frame, and a
/// contrast curve that tightens the soft edge.
#[derive(Default)]
struct MaskRefiner {
    last: Option<Instant>,
    previous: Vec<f32>,
    guide: Vec<f32>,
    mean_guide: Vec<f32>,
    mean_mask: Vec<f32>,
    corr_guide: Vec<f32>,
    corr_guide_mask: Vec<f32>,
    a: Vec<f32>,
    b: Vec<f32>,
    scratch: Vec<f32>,
}

impl MaskRefiner {
    fn refine(
        &mut self,
        confidence: &[f32],
        rgb: &[u8],
        width: usize,
        height: usize,
        now: Instant,
        out: &mut [u8],
    ) {
        let pixels = width * height;
        let fresh = self.previous.len() != pixels
            || self
                .last
                .map_or(true, |last| now.saturating_duration_since(last) > TEMPORAL_RESET);
        self.last = Some(now);
        for buffer in [
            &mut self.previous,
            &mut self.guide,
            &mut self.mean_guide,
            &mut self.mean_mask,
            &mut self.corr_guide,
            &mut self.corr_guide_mask,
            &mut self.a,
            &mut self.b,
            &mut self.scratch,
        ] {
            buffer.resize(pixels, 0.0);
        }

        for (i, pixel) in rgb.chunks_exact(3).take(pixels).enumerate() {
            let value = confidence[i].clamp(0.0, 1.0);
            // Only small changes are smoothed; a large one is real movement
            // and is taken at once, so the cut-out does not trail behind.
            let change = value - self.previous[i];
            let mask = if fresh || change.abs() > MOTION_THRESHOLD {
                value
            } else {
                self.previous[i] + TEMPORAL_WEIGHT * change
            };
            self.previous[i] = mask;
            let luma =
                (0.299 * pixel[0] as f32 + 0.587 * pixel[1] as f32 + 0.114 * pixel[2] as f32) / 255.0;
            self.guide[i] = luma;
            self.corr_guide[i] = luma * luma;
            self.corr_guide_mask[i] = luma * mask;
        }

        box_filter(&self.guide, &mut self.mean_guide, &mut self.scratch, width, height);
        box_filter(&self.previous, &mut self.mean_mask, &mut self.scratch, width, height);
        box_filter_in_place(&mut self.corr_guide, &mut self.scratch, width, height);
        box_filter_in_place(&mut self.corr_guide_mask, &mut self.scratch, width, height);
        for i in 0..pixels {
            let variance = self.corr_guide[i] - self.mean_guide[i] * self.mean_guide[i];
            let covariance = self.corr_guide_mask[i] - self.mean_guide[i] * self.mean_mask[i];
            self.a[i] = covariance / (variance + GUIDED_EPS);
            self.b[i] = self.mean_mask[i] - self.a[i] * self.mean_guide[i];
        }
        box_filter_in_place(&mut self.a, &mut self.scratch, width, height);
        box_filter_in_place(&mut self.b, &mut self.scratch, width, height);

        // Left soft: the edge is sharpened only after the mask is scaled up to
        // the frame (see `edge_curve`), which is what keeps it from stepping.
        for i in 0..pixels {
            let value = (self.a[i] * self.guide[i] + self.b[i]).clamp(0.0, 1.0);
            out[i] = (value * 255.0).round() as u8;
        }
    }
}

fn box_filter_in_place(values: &mut Vec<f32>, scratch: &mut Vec<f32>, width: usize, height: usize) {
    let source = std::mem::take(values);
    let mut target = vec![0.0; source.len()];
    box_filter(&source, &mut target, scratch, width, height);
    *values = target;
}

/// Mean over a (2r+1)² window, clamped at the borders.
fn box_filter(source: &[f32], target: &mut [f32], scratch: &mut [f32], width: usize, height: usize) {
    let radius = GUIDED_RADIUS;
    for y in 0..height {
        let row = &source[y * width..(y + 1) * width];
        let (mut sum, mut lo, mut hi) = (0.0f32, 0usize, 0usize);
        for x in 0..width {
            let next_hi = (x + radius + 1).min(width);
            while hi < next_hi {
                sum += row[hi];
                hi += 1;
            }
            let next_lo = x.saturating_sub(radius);
            while lo < next_lo {
                sum -= row[lo];
                lo += 1;
            }
            scratch[y * width + x] = sum / (hi - lo) as f32;
        }
    }
    for x in 0..width {
        let (mut sum, mut lo, mut hi) = (0.0f32, 0usize, 0usize);
        for y in 0..height {
            let next_hi = (y + radius + 1).min(height);
            while hi < next_hi {
                sum += scratch[hi * width + x];
                hi += 1;
            }
            let next_lo = y.saturating_sub(radius);
            while lo < next_lo {
                sum -= scratch[lo * width + x];
                lo += 1;
            }
            target[y * width + x] = sum / (hi - lo) as f32;
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
struct CompositeKey {
    width: u32,
    height: u32,
    /// Zero with an Original background, which needs no mask.
    mask_width: u32,
    mask_height: u32,
    background: Background,
    canvas: Canvas,
    canvas_rgb: [u8; 3],
    /// Touch up strength in whole percent, 0 when off.
    touch_up: u8,
    auto_lighting: bool,
}

/// The FFmpeg graph that puts the cut-out person over the background, then
/// applies Touch up and the exposure gain.
struct Compositor {
    key: CompositeKey,
    graph: filter::Graph,
    background: Option<frame::Video>,
    /// The exposure gain the graph currently applies.
    gain: f32,
    next_pts: i64,
    /// Capture times of frames still inside the graph, by pts.
    pending: VecDeque<(i64, Instant)>,
}

/// The filter graph for `key` and, for a replaced background, the static
/// background it is fed.
fn graph_spec(key: CompositeKey) -> Result<(String, Option<frame::Video>), String> {
    let CompositeKey { width, height, mask_width, mask_height, .. } = key;
    let source = |name: &str, w: u32, h: u32, format: &str| {
        format!("buffer@{name}=video_size={w}x{h}:pix_fmt={format}:time_base=1/30:pixel_aspect=1/1")
    };
    let camera = source("camera", width, height, "bgra");
    // The soft mask is scaled up smoothly first and its edge tightened at
    // full size, so the edge follows a smooth curve instead of the
    // model's 256-pixel grid.
    let mask = format!(
        "{}[m];[m]scale={width}:{height}:flags=bicubic,format=gray,lut=c0='{}'[mask]",
        source("mask", mask_width, mask_height, "gray"),
        edge_curve()
    );
    // Composited in YUV, where FFmpeg's overlay is far cheaper than in RGB.
    let overlay = "[fg];[back][fg]overlay=format=yuv420:eof_action=pass";

    let (mut spec, background) = match key.background {
        // Stays BGRA: only Touch up needs the YUV detour.
        Background::Original => (camera, None),
        Background::Blur => {
            let blur_width = even(width / BLUR_DOWNSCALE);
            let blur_height = even(height / BLUR_DOWNSCALE);
            let radius = box_radius(width, height).min(blur_width.min(blur_height) / 2 - 1).max(1);
            let chroma_radius = (radius / 2).clamp(1, (blur_width.min(blur_height) / 4).max(2) - 1);
            // No alpha before the blur, so the background is always opaque;
            // two box passes approximate the Gaussian and mirror at the edges
            // instead of fading to black there.
            (
                format!(
                    "{camera},format=yuva420p[cam];{mask};[cam]split[person][scene];\
                     [scene]scale={blur_width}:{blur_height}:flags=fast_bilinear,format=yuv420p,\
                     boxblur=luma_radius={radius}:luma_power={BLUR_PASSES}:\
                     chroma_radius={chroma_radius}:chroma_power={BLUR_PASSES},\
                     scale={width}:{height}:flags=bilinear[back];\
                     [person][mask]alphamerge{overlay}"
                ),
                None,
            )
        }
        Background::Remove => (
            format!(
                "{camera},format=yuva420p[cam];{}[back];{mask};[cam][mask]alphamerge{overlay}",
                source("background", width, height, "yuv420p")
            ),
            Some(to_yuv420(&canvas_frame(key)?)?),
        ),
    };

    let in_yuv = key.background != Background::Original;
    if key.touch_up > 0 {
        // The bubble's soft focus: a downscaled copy drawn back over the
        // frame. `blend` keeps `all_opacity` of its first input, so the
        // sharp frame gets what the soft copy leaves.
        let (downscale, soft_opacity) = touch_up_blend(key.touch_up);
        let soft_width = even((f64::from(width) / downscale).round() as u32);
        let soft_height = even((f64::from(height) / downscale).round() as u32);
        if !in_yuv {
            spec.push_str(",format=yuv420p");
        }
        spec.push_str(&format!(
            "[mix];[mix]split[sharp][soft];\
             [soft]scale={soft_width}:{soft_height}:flags=bilinear,scale={width}:{height}:flags=bilinear[softened];\
             [sharp][softened]blend=all_mode=normal:all_opacity={:.3},format=bgra",
            1.0 - soft_opacity
        ));
    } else if in_yuv {
        spec.push_str(",format=bgra");
    }
    if key.auto_lighting {
        // Identity until the first measured gain arrives by command.
        spec.push_str(&format!(",{LIGHT_FILTER}"));
    }
    spec.push_str(",buffersink@out");
    Ok((spec, background))
}

impl Compositor {
    fn build(key: CompositeKey) -> Result<Self, String> {
        let (spec, background) = graph_spec(key)?;
        let mut graph = filter::Graph::new();
        let threads = std::thread::available_parallelism().map_or(1, |n| n.get().min(4));
        // SAFETY: Category 8 — FFI contract. Set on a graph with no filters
        // yet, which is when FFmpeg reads it.
        unsafe { (*graph.as_mut_ptr()).nb_threads = threads as _ };
        graph
            .parse(&spec)
            .map_err(|e| format!("camera effect graph: {e}"))?;
        graph
            .validate()
            .map_err(|e| format!("camera effect graph: {e}"))?;
        Ok(Self {
            key,
            graph,
            background,
            gain: 1.0,
            next_pts: 0,
            pending: VecDeque::new(),
        })
    }

    /// Runs one camera frame (with its person mask when the background
    /// needs one) through the graph, measuring and easing the auto-lighting
    /// gain on the way out.
    fn push(
        &mut self,
        bgra: &[u8],
        mask: Option<&[u8]>,
        captured_at: Instant,
        lighting: &mut Lighting,
    ) -> Result<Vec<Composited>, String> {
        let CompositeKey { width, height, mask_width, mask_height, .. } = self.key;
        if self.key.auto_lighting && self.gain != lighting.gain {
            self.set_gain(lighting.gain)?;
        }
        let pts = self.next_pts;
        self.next_pts += 1;

        // Adding a frame hands its data to the graph, so each tick needs fresh ones.
        let mut camera = frame::Video::new(Pixel::BGRA, width, height);
        copy_into_plane(&mut camera, 0, bgra, width as usize * 4);
        camera.set_pts(Some(pts));
        self.add("buffer@camera", &camera)?;
        if mask_width > 0 {
            let mask = mask.ok_or("no mask for this frame")?;
            let mut mask_frame = frame::Video::new(Pixel::GRAY8, mask_width, mask_height);
            copy_into_plane(&mut mask_frame, 0, mask, mask_width as usize);
            mask_frame.set_pts(Some(pts));
            self.add("buffer@mask", &mask_frame)?;
        }
        if let Some(background) = &self.background {
            let mut background = background.clone();
            background.set_pts(Some(pts));
            self.add("buffer@background", &background)?;
        }
        self.pending.push_back((pts, captured_at));

        let mut finished = Vec::new();
        loop {
            let mut output = frame::Video::empty();
            let pulled = self
                .graph
                .get("buffersink@out")
                .ok_or("camera effect output missing")?
                .sink()
                .frame(&mut output);
            if pulled.is_err() {
                break;
            }
            let at = match output.pts() {
                Some(pts) => {
                    while self.pending.front().is_some_and(|(queued, _)| *queued < pts) {
                        self.pending.pop_front();
                    }
                    self.pending
                        .front()
                        .filter(|(queued, _)| *queued == pts)
                        .map(|(_, at)| *at)
                        .unwrap_or(captured_at)
                }
                None => captured_at,
            };
            let mut pixels = Vec::new();
            copy_from_plane(&output, 0, &mut pixels, width as usize * 4);
            if self.key.auto_lighting {
                // As on macOS: probe every few frames and ease toward the
                // gain that would bring the scene to mid-grey. The frame was
                // measured after the gain, so that is undone first.
                if lighting.frames % AUTO_LIGHTING_PROBE_INTERVAL == 0 {
                    let scene = scene_luma(mean_luma(&pixels, width), self.gain);
                    lighting.gain = next_gain(lighting.gain, scene);
                }
                lighting.frames = lighting.frames.wrapping_add(1);
            }
            finished.push((pixels, at));
        }
        // Never hold on to times for frames the graph has already dropped.
        while self.pending.len() > 8 {
            self.pending.pop_front();
        }
        Ok(finished)
    }

    /// Points the running `colorlevels` at the exposure for `gain`.
    fn set_gain(&mut self, gain: f32) -> Result<(), String> {
        let (black, white, output) = light_levels(gain);
        let mut light = self
            .graph
            .get(LIGHT_FILTER)
            .ok_or("camera lighting filter missing")?;
        let commands = [
            ("rimin", black),
            ("gimin", black),
            ("bimin", black),
            ("rimax", white),
            ("gimax", white),
            ("bimax", white),
            ("romax", output),
            ("gomax", output),
            ("bomax", output),
        ];
        for (option, value) in commands {
            let command = CString::new(option).map_err(|e| e.to_string())?;
            let argument = CString::new(format!("{value:.5}")).map_err(|e| e.to_string())?;
            // SAFETY: Category 8 — FFI contract. The filter context belongs
            // to this graph, both strings are NUL-terminated, and no response
            // buffer is requested.
            let status = unsafe {
                ffmpeg::ffi::avfilter_process_command(
                    light.as_mut_ptr(),
                    command.as_ptr(),
                    argument.as_ptr(),
                    std::ptr::null_mut(),
                    0,
                    0,
                )
            };
            if status < 0 {
                return Err(format!("camera lighting {option}: {}", ffmpeg::Error::from(status)));
            }
        }
        self.gain = gain;
        Ok(())
    }

    fn add(&mut self, name: &str, frame: &frame::Video) -> Result<(), String> {
        self.graph
            .get(name)
            .ok_or_else(|| format!("camera effect input {name} missing"))?
            .source()
            .add(frame)
            .map_err(|e| format!("camera effect input: {e}"))
    }
}

/// The background behind the cut-out, as a full-size BGRA frame.
fn canvas_frame(key: CompositeKey) -> Result<frame::Video, String> {
    let (width, height) = (key.width as usize, key.height as usize);
    let mut canvas = frame::Video::new(Pixel::BGRA, key.width, key.height);
    let solid = |rgb: [u8; 3]| Some([rgb[2], rgb[1], rgb[0], 255]);
    // A camera-only video has nothing to show through, so "clear" is ink, as
    // on macOS.
    let fill = match key.canvas {
        Canvas::Transparent | Canvas::Ink => solid(INK),
        Canvas::Cream => solid(CREAM),
        Canvas::Solid => solid(key.canvas_rgb),
        Canvas::Sunset | Canvas::Aurora | Canvas::Dunes | Canvas::Studio | Canvas::Custom => None,
    };
    if let Some(pixel) = fill {
        let row: Vec<u8> = pixel.repeat(width);
        fill_rows(&mut canvas, |_, target| target.copy_from_slice(&row));
        return Ok(canvas);
    }
    if key.canvas == Canvas::Sunset {
        fill_rows(&mut canvas, |y, target| sunset_row(y, width, height, target));
        return Ok(canvas);
    }

    let image = crate::camera_effects::background_image(key.canvas)?
        .ok_or("camera background image is unavailable")?;
    cover(&image, key.width, key.height, &mut canvas)?;
    Ok(canvas)
}

/// The static background, converted once for the YUV graph.
fn to_yuv420(bgra: &frame::Video) -> Result<frame::Video, String> {
    let mut yuv = frame::Video::new(Pixel::YUV420P, bgra.width(), bgra.height());
    scaling::Context::get(
        Pixel::BGRA,
        bgra.width(),
        bgra.height(),
        Pixel::YUV420P,
        bgra.width(),
        bgra.height(),
        scaling::flag::Flags::BILINEAR,
    )
    .and_then(|mut scaler| scaler.run(bgra, &mut yuv))
    .map_err(|e| format!("camera background conversion: {e}"))?;
    Ok(yuv)
}

/// Core Image's gradient runs from its origin at the bottom-left to the top
/// right, so on screen it goes from bottom-left to top-right.
fn sunset_row(y: usize, width: usize, height: usize, target: &mut [u8]) {
    let (w, h) = (width as f32, height as f32);
    let length = w * w + h * h;
    let up = h - y as f32 - 0.5;
    for (x, pixel) in target.chunks_exact_mut(4).enumerate() {
        let t = (((x as f32 + 0.5) * w + up * h) / length).clamp(0.0, 1.0);
        let mix = |from: u8, to: u8| (from as f32 + (to as f32 - from as f32) * t).round() as u8;
        pixel[0] = mix(SUNSET_FROM[2], SUNSET_TO[2]);
        pixel[1] = mix(SUNSET_FROM[1], SUNSET_TO[1]);
        pixel[2] = mix(SUNSET_FROM[0], SUNSET_TO[0]);
        pixel[3] = 255;
    }
}

/// Scale an RGBA image to cover `width` × `height` and centre-crop it.
fn cover(
    image: &crate::camera_effects::BackgroundImage,
    width: u32,
    height: u32,
    canvas: &mut frame::Video,
) -> Result<(), String> {
    let (image_width, image_height) = (image.width as u32, image.height as u32);
    let factor = (width as f64 / image_width as f64).max(height as f64 / image_height as f64);
    let scaled_width = ((image_width as f64 * factor).ceil() as u32).max(width);
    let scaled_height = ((image_height as f64 * factor).ceil() as u32).max(height);

    let mut source = frame::Video::new(Pixel::RGBA, image_width, image_height);
    copy_into_plane(&mut source, 0, &image.data, image.width * 4);
    let mut scaled = frame::Video::new(Pixel::BGRA, scaled_width, scaled_height);
    scaling::Context::get(
        Pixel::RGBA,
        image_width,
        image_height,
        Pixel::BGRA,
        scaled_width,
        scaled_height,
        scaling::flag::Flags::BILINEAR,
    )
    .and_then(|mut scaler| scaler.run(&source, &mut scaled))
    .map_err(|e| format!("camera background scale: {e}"))?;

    let left = ((scaled_width - width) / 2) as usize;
    let top = ((scaled_height - height) / 2) as usize;
    let row_bytes = width as usize * 4;
    let scaled_stride = scaled.stride(0);
    let scaled_data = scaled.data(0);
    fill_rows(canvas, |y, target| {
        let start = (top + y) * scaled_stride + left * 4;
        target.copy_from_slice(&scaled_data[start..start + row_bytes]);
    });
    Ok(())
}

fn fill_rows(frame: &mut frame::Video, mut fill: impl FnMut(usize, &mut [u8])) {
    let (width, height) = (frame.width() as usize, frame.height() as usize);
    let stride = frame.stride(0);
    let data = frame.data_mut(0);
    for y in 0..height {
        fill(y, &mut data[y * stride..y * stride + width * 4]);
    }
}

fn copy_into_plane(frame: &mut frame::Video, plane: usize, source: &[u8], row_bytes: usize) {
    let stride = frame.stride(plane);
    let data = frame.data_mut(plane);
    for (y, row) in source.chunks_exact(row_bytes).enumerate() {
        data[y * stride..y * stride + row_bytes].copy_from_slice(row);
    }
}

/// Rows are appended rather than written into a zero-filled buffer: zeroing
/// megabytes element by element is what made this slow in dev builds.
fn copy_from_plane(frame: &frame::Video, plane: usize, target: &mut Vec<u8>, row_bytes: usize) {
    let height = frame.height() as usize;
    let stride = frame.stride(plane);
    let data = frame.data(plane);
    target.clear();
    target.reserve(row_bytes * height);
    for y in 0..height {
        target.extend_from_slice(&data[y * stride..y * stride + row_bytes]);
    }
}

/// FFmpeg expression for the edge contrast curve on an 8-bit mask: a
/// smoothstep from `EDGE_LOW` to `EDGE_HIGH`, as the bubble applies.
fn edge_curve() -> String {
    let low = EDGE_LOW * 255.0;
    let span = (EDGE_HIGH - EDGE_LOW) * 255.0;
    format!("st(0,clip((val-{low:.2})/{span:.2},0,1));255*ld(0)*ld(0)*(3-2*ld(0))")
}

/// Box radius at the reduced blur size whose repeated passes match a Gaussian
/// of `BLUR_SHARE` of the frame: n passes of width 2r+1 have variance
/// n((2r+1)^2 - 1)/12.
fn box_radius(width: u32, height: u32) -> u32 {
    let sigma = width.max(height) as f64 * BLUR_SHARE / BLUR_DOWNSCALE as f64;
    let span = (12.0 * sigma * sigma / BLUR_PASSES as f64 + 1.0).sqrt();
    ((span - 1.0) / 2.0).round().max(1.0) as u32
}

fn even(value: u32) -> u32 {
    (value & !1).max(2)
}

/// Where the bundled model lives next to the executable.
pub fn model_path(resource_dir: &Path) -> PathBuf {
    resource_dir.join("segmentation").join(MODEL_FILE)
}

/// Tauri reports resource paths in the `\\?\C:\…` form; Windows ML is only
/// given plain drive paths, which is what that form stands for.
fn plain_path(path: &Path) -> PathBuf {
    let text = path.as_os_str().to_string_lossy();
    match text.strip_prefix(r"\\?\") {
        Some(rest) if !rest.starts_with(r"UNC\") => PathBuf::from(rest),
        _ => path.to_path_buf(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(background: Background, canvas: Canvas) -> CompositeKey {
        CompositeKey {
            width: 64,
            height: 36,
            mask_width: if background == Background::Original { 0 } else { 16 },
            mask_height: if background == Background::Original { 0 } else { 16 },
            background,
            canvas,
            canvas_rgb: [1, 2, 3],
            touch_up: 0,
            auto_lighting: false,
        }
    }

    fn times(frames: &[VideoFrame]) -> Vec<Instant> {
        frames.iter().map(|frame| frame.captured_at).collect()
    }

    /// The YUV round trip moves colours by a step or two.
    fn assert_close(pixel: &[u8], expected: [u8; 4]) {
        for (channel, expected) in pixel.iter().zip(expected) {
            assert!((*channel as i32 - expected as i32).abs() <= 4, "{pixel:?} vs {expected:?}");
        }
    }

    fn run(compositor: &mut Compositor, mask_value: u8, frames: usize) -> Vec<Composited> {
        let bgra = [200u8, 100, 50, 255].repeat(64 * 36);
        let mask = vec![mask_value; 16 * 16];
        run_pixels(compositor, &bgra, Some(&mask), frames, &mut Lighting::default())
    }

    fn run_pixels(
        compositor: &mut Compositor,
        bgra: &[u8],
        mask: Option<&[u8]>,
        frames: usize,
        lighting: &mut Lighting,
    ) -> Vec<Composited> {
        let mut out = Vec::new();
        for _ in 0..frames {
            out.extend(compositor.push(bgra, mask, Instant::now(), lighting).unwrap());
        }
        out
    }

    #[test]
    fn canvas_replaces_the_background_and_keeps_the_person() {
        crate::encoder::Mp4Encoder::init_global();
        let mut compositor = Compositor::build(key(Background::Remove, Canvas::Solid)).unwrap();
        let background = run(&mut compositor, 0, 3);
        assert!(!background.is_empty(), "graph produced no frames");
        assert_eq!(background[0].0.len(), 64 * 36 * 4);
        assert_close(&background[0].0[..4], [3, 2, 1, 255]);

        let mut compositor = Compositor::build(key(Background::Remove, Canvas::Ink)).unwrap();
        let person = run(&mut compositor, 255, 3);
        assert_close(&person[0].0[..4], [200, 100, 50, 255]);
    }

    #[test]
    fn edge_is_tightened_after_scaling() {
        crate::encoder::Mp4Encoder::init_global();
        // Soft values outside the edge band snap to background or person.
        let mut compositor = Compositor::build(key(Background::Remove, Canvas::Solid)).unwrap();
        assert_close(&run(&mut compositor, 60, 3)[0].0[..4], [3, 2, 1, 255]);
        let mut compositor = Compositor::build(key(Background::Remove, Canvas::Solid)).unwrap();
        assert_close(&run(&mut compositor, 200, 3)[0].0[..4], [200, 100, 50, 255]);
        assert!(edge_curve().starts_with("st(0,clip((val-76.50)/102.00,0,1))"));
    }

    #[test]
    fn blur_keeps_a_flat_scene_flat() {
        crate::encoder::Mp4Encoder::init_global();
        let mut compositor = Compositor::build(key(Background::Blur, Canvas::Transparent)).unwrap();
        let frames = run(&mut compositor, 0, 3);
        assert!(!frames.is_empty());
        let middle = (18 * 64 + 32) * 4;
        assert_close(&frames[0].0[..4], [200, 100, 50, 255]);
        assert_close(&frames[0].0[middle..middle + 4], [200, 100, 50, 255]);
    }

    #[test]
    fn blur_radius_matches_the_macos_strength() {
        // 3.5% of 1920 is a sigma of 16.8 at quarter size.
        assert_eq!(box_radius(1920, 1080), 20);
        assert_eq!(box_radius(64, 36), 1);
    }

    #[test]
    fn full_hd_compositing_cost() {
        crate::encoder::Mp4Encoder::init_global();
        let bgra = [90u8, 120, 160, 255].repeat(1920 * 1080);
        let mask: Vec<u8> = (0..256 * 256).map(|i| if i % 256 < 128 { 255 } else { 0 }).collect();
        for background in [Background::Blur, Background::Remove] {
            let mut compositor = Compositor::build(CompositeKey {
                width: 1920,
                height: 1080,
                mask_width: 256,
                mask_height: 256,
                background,
                canvas: Canvas::Sunset,
                canvas_rgb: [0, 0, 0],
                touch_up: 0,
                auto_lighting: false,
            })
            .unwrap();
            let mut lighting = Lighting::default();
            for _ in 0..3 {
                compositor.push(&bgra, Some(&mask), Instant::now(), &mut lighting).unwrap();
            }
            let started = Instant::now();
            let mut frames = 0;
            for _ in 0..10 {
                frames += compositor
                    .push(&bgra, Some(&mask), Instant::now(), &mut lighting)
                    .unwrap()
                    .len();
            }
            let per_frame = started.elapsed() / 10;
            eprintln!("{background:?} 1080p composite: {per_frame:?} per frame ({frames} out)");
            assert!(frames >= 9);
        }
    }

    /// Pushes `count` frames through a pipeline and returns what came out
    /// (capture times, in order) and every error it reported.
    fn run_pipeline(
        model: PathBuf,
        settings: Option<FrameSettings>,
        count: usize,
    ) -> (Vec<Instant>, Vec<VideoFrame>, Vec<String>) {
        crate::encoder::Mp4Encoder::init_global();
        let (frames_tx, frames_rx) = std_mpsc::channel();
        let (errors_tx, errors_rx) = std_mpsc::channel();
        let pipeline = EffectsPipeline::start(
            model,
            Box::new(move || settings),
            Box::new(move |frame: VideoFrame| {
                assert_eq!(frame.bgra.len(), 320 * 180 * 4);
                frames_tx.send(frame).is_ok()
            }),
            Box::new(move |message| {
                let _ = errors_tx.send(message);
            }),
        )
        .unwrap();
        let mut pushed = Vec::new();
        for _ in 0..count {
            let captured_at = Instant::now();
            let frame = VideoFrame {
                bgra: [90u8, 120, 160, 255].repeat(320 * 180),
                width: 320,
                height: 180,
                captured_at,
            };
            if pipeline.push(frame) {
                pushed.push(captured_at);
            }
            // Slower than the pipeline, so nothing is dropped for being full.
            std::thread::sleep(Duration::from_millis(60));
        }
        pipeline.finish();
        (pushed, frames_rx.try_iter().collect(), errors_rx.try_iter().collect())
    }

    fn blur() -> FrameSettings {
        FrameSettings {
            effects: CameraEffects {
                background: Background::Blur,
                canvas: Canvas::Transparent,
                frame: Frame::None,
                canvas_rgb: [0, 0, 0],
            },
            look: Look::default(),
        }
    }

    #[test]
    fn extended_resource_paths_load() {
        assert_eq!(plain_path(Path::new(r"\\?\S:\app\model.onnx")), PathBuf::from(r"S:\app\model.onnx"));
        assert_eq!(plain_path(Path::new(r"\\?\UNC\host\share\m.onnx")), PathBuf::from(r"\\?\UNC\host\share\m.onnx"));
        assert_eq!(plain_path(Path::new(r"C:\m.onnx")), PathBuf::from(r"C:\m.onnx"));

        let path = model_path(Path::new(env!("CARGO_MANIFEST_DIR")));
        if !path.is_file() {
            return;
        }
        let verbatim = PathBuf::from(format!(r"\\?\{}", path.display()));
        crate::encoder::Mp4Encoder::init_global();
        assert!(Segmenter::load(&verbatim).is_ok());
    }

    #[test]
    fn pipeline_passes_frames_through_in_order_without_an_effect() {
        let (pushed, delivered, errors) = run_pipeline(PathBuf::from("unused.onnx"), None, 5);
        assert_eq!(times(&delivered), pushed);
        assert!(errors.is_empty());
    }

    #[test]
    fn pipeline_drops_frames_and_reports_once_when_the_effect_fails() {
        let (_, delivered, errors) =
            run_pipeline(PathBuf::from("does-not-exist.onnx"), Some(blur()), 4);
        assert!(delivered.is_empty(), "no frame may be recorded without its effect");
        assert_eq!(errors.len(), 1, "{errors:?}");
        assert!(errors[0].contains("missing"));
    }

    #[test]
    fn pipeline_composites_every_frame_with_its_own_mask() {
        let path = model_path(Path::new(env!("CARGO_MANIFEST_DIR")));
        if !path.is_file() {
            return;
        }
        let (pushed, delivered, errors) = run_pipeline(path, Some(blur()), 8);
        assert!(errors.is_empty(), "{errors:?}");
        let delivered = times(&delivered);
        // The compositor may hold the newest frame back until the next arrives.
        assert!(delivered.len() + 1 >= pushed.len(), "{} of {}", delivered.len(), pushed.len());
        assert_eq!(delivered[..], pushed[..delivered.len()]);
    }

    /// Renders the portrait as a 1080p camera frame cut out over green, plus
    /// a zoomed crop of the hair edge, for judging edge quality by eye. Run
    /// with `CLIPY_SEGMENTATION_PORTRAIT` and `CLIPY_CUTOUT_OUT` set.
    #[test]
    #[ignore]
    fn render_portrait_cutout() {
        let (Ok(portrait), Ok(out)) = (
            std::env::var("CLIPY_SEGMENTATION_PORTRAIT"),
            std::env::var("CLIPY_CUTOUT_OUT"),
        ) else {
            return;
        };
        crate::encoder::Mp4Encoder::init_global();
        let (width, height) = (1920u32, 1080u32);
        let person = image::imageops::resize(
            &image::open(portrait).unwrap().to_rgba8(),
            865,
            1080,
            image::imageops::FilterType::Triangle,
        );
        let mut camera = image::RgbaImage::from_pixel(width, height, image::Rgba([120, 110, 100, 255]));
        image::imageops::overlay(&mut camera, &person, 528, 0);
        let bgra: Vec<u8> = camera.pixels().flat_map(|p| [p[2], p[1], p[0], 255]).collect();

        let path = model_path(Path::new(env!("CARGO_MANIFEST_DIR")));
        let mut segmenter = Segmenter::load(&path).unwrap();
        let (mask_width, mask_height) = segmenter.size;
        let (confidence, guide) = segmenter.segment(&bgra, width, height).unwrap();
        let mut mask = vec![0u8; mask_width * mask_height];
        MaskRefiner::default().refine(confidence, guide, mask_width, mask_height, Instant::now(), &mut mask);

        let mut compositor = Compositor::build(CompositeKey {
            width,
            height,
            mask_width: mask_width as u32,
            mask_height: mask_height as u32,
            background: Background::Remove,
            canvas: Canvas::Solid,
            canvas_rgb: [0, 200, 0],
            touch_up: 0,
            auto_lighting: false,
        })
        .unwrap();
        let frames = run_pixels(&mut compositor, &bgra, Some(&mask), 2, &mut Lighting::default());
        let rgba: Vec<u8> = frames[0].0.chunks_exact(4).flat_map(|p| [p[2], p[1], p[0], 255]).collect();
        let result = image::RgbaImage::from_raw(width, height, rgba).unwrap();
        result.save(&out).unwrap();
        let crop = image::imageops::crop_imm(&result, 800, 0, 320, 320).to_image();
        image::imageops::resize(&crop, 640, 640, image::imageops::FilterType::Nearest)
            .save(out.replace(".png", "-zoom.png"))
            .unwrap();
    }

    #[test]
    fn smoothing_follows_real_movement_and_damps_flicker() {
        let (width, height) = (16, 16);
        let rgb = vec![128u8; width * height * 3];
        let start = Instant::now();
        let mut refiner = MaskRefiner::default();
        let mut out = vec![0u8; width * height];
        refiner.refine(&vec![1.0; width * height], &rgb, width, height, start, &mut out);
        assert_eq!(out[8 * width + 8], 255);
        // The person moved away: taken at once, not faded over several frames.
        let later = start + Duration::from_millis(33);
        refiner.refine(&vec![0.0; width * height], &rgb, width, height, later, &mut out);
        assert_eq!(out[8 * width + 8], 0);
        // A small wobble is still smoothed.
        let settled = later + Duration::from_millis(33);
        refiner.refine(&vec![0.2; width * height], &rgb, width, height, settled, &mut out);
        assert!(refiner.previous[8 * width + 8] < 0.2);
    }

    /// Where a composite's time goes, stage by stage. Run on demand with
    /// `cargo test --lib composite_stage_costs -- --ignored --nocapture`.
    #[test]
    #[ignore]
    fn composite_stage_costs() {
        crate::encoder::Mp4Encoder::init_global();
        let (w, h) = (1920u32, 1080u32);
        let bgra = [90u8, 120, 160, 255].repeat((w * h) as usize);
        let time = |label: &str, tail: &str| {
            let mut graph = filter::Graph::new();
            unsafe { (*graph.as_mut_ptr()).nb_threads = 4 };
            graph
                .parse(&format!(
                    "buffer@camera=video_size={w}x{h}:pix_fmt=bgra:time_base=1/30:pixel_aspect=1/1,{tail},buffersink@out"
                ))
                .unwrap();
            graph.validate().unwrap();
            let mut total = Duration::ZERO;
            for pts in 0..12 {
                let mut camera = frame::Video::new(Pixel::BGRA, w, h);
                copy_into_plane(&mut camera, 0, &bgra, w as usize * 4);
                camera.set_pts(Some(pts));
                let started = Instant::now();
                graph.get("buffer@camera").unwrap().source().add(&camera).unwrap();
                let mut out = frame::Video::empty();
                let _ = graph.get("buffersink@out").unwrap().sink().frame(&mut out);
                if pts >= 2 {
                    total += started.elapsed();
                }
            }
            eprintln!("{label:>28}: {:?}", total / 10);
        };
        time("null", "null");
        time("to yuva420p", "format=yuva420p");
        time("to yuv420p to bgra", "format=yuv420p,format=bgra");
        time("quarter scale", "scale=480:270:flags=fast_bilinear");
        time("quarter blur round trip", "scale=480:270:flags=fast_bilinear,format=yuv420p,boxblur=luma_radius=20:luma_power=2:chroma_radius=10:chroma_power=2,scale=1920:1080:flags=bilinear");
        time("gray upscale", "scale=256:256,format=gray,scale=1920:1080:flags=bilinear");

        let started = Instant::now();
        for _ in 0..10 {
            let mut camera = frame::Video::new(Pixel::BGRA, w, h);
            copy_into_plane(&mut camera, 0, &bgra, w as usize * 4);
            let mut back = Vec::new();
            copy_from_plane(&camera, 0, &mut back, w as usize * 4);
        }
        eprintln!("{:>28}: {:?}", "rust copies in+out", started.elapsed() / 10);
    }

    #[test]
    fn frames_keep_their_capture_time() {
        crate::encoder::Mp4Encoder::init_global();
        let mut compositor = Compositor::build(key(Background::Remove, Canvas::Cream)).unwrap();
        let bgra = [0u8; 4].repeat(64 * 36);
        let mask = vec![0u8; 16 * 16];
        let first = Instant::now();
        let second = first + Duration::from_millis(33);
        let mut lighting = Lighting::default();
        let mut out = compositor.push(&bgra, Some(&mask), first, &mut lighting).unwrap();
        out.extend(compositor.push(&bgra, Some(&mask), second, &mut lighting).unwrap());
        assert_eq!(out.first().map(|(_, at)| *at), Some(first));
        assert_close(&out[0].0[..4], [240, 246, 250, 255]);
    }

    #[test]
    fn sunset_runs_from_bottom_left_to_top_right() {
        let mut row = vec![0u8; 8];
        sunset_row(9, 2, 10, &mut row);
        assert!(row[2] == 255 && row[1] < 100, "bottom-left is red-orange: {row:?}");
        sunset_row(0, 2, 10, &mut row);
        assert!(row[1] > 150, "top-right is amber: {row:?}");
    }

    #[test]
    fn refiner_keeps_a_confident_person_and_drops_the_background() {
        let (width, height) = (16, 16);
        let confidence: Vec<f32> = (0..width * height)
            .map(|i| if i % width < 8 { 1.0 } else { 0.0 })
            .collect();
        let rgb: Vec<u8> = (0..width * height)
            .flat_map(|i| if i % width < 8 { [220u8, 200, 180] } else { [20, 30, 40] })
            .collect();
        let mut out = vec![0u8; width * height];
        MaskRefiner::default().refine(&confidence, &rgb, width, height, Instant::now(), &mut out);
        assert_eq!(out[8 * width + 1], 255);
        assert_eq!(out[8 * width + 14], 0);
    }

    /// Needs the converted model in `src-tauri/segmentation`.
    #[test]
    fn bundled_model_loads_and_runs_on_windows_ml() {
        let path = model_path(Path::new(env!("CARGO_MANIFEST_DIR")));
        if !path.is_file() {
            eprintln!("skipping: {} not present", path.display());
            return;
        }
        crate::encoder::Mp4Encoder::init_global();
        let mut segmenter = Segmenter::load(&path).unwrap();
        let (model_width, model_height) = segmenter.size;
        assert_eq!((model_width, model_height), (256, 256));
        let bgra = [128u8, 128, 128, 255].repeat(320 * 180);
        let started = Instant::now();
        let (confidence, guide) = segmenter.segment(&bgra, 320, 180).unwrap();
        assert_eq!(guide.len(), model_width * model_height * 3);
        assert!(confidence.iter().all(|value| value.is_finite()));
        eprintln!("segmentation took {:?}", started.elapsed());

        // Opt-in check against a real photo of a person.
        let Ok(portrait) = std::env::var("CLIPY_SEGMENTATION_PORTRAIT") else {
            return;
        };
        let image = image::open(portrait).unwrap().to_rgba8();
        let (width, height) = image.dimensions();
        let bgra: Vec<u8> = image
            .pixels()
            .flat_map(|pixel| [pixel[2], pixel[1], pixel[0], 255])
            .collect();
        let started = Instant::now();
        let (confidence, _) = segmenter.segment(&bgra, width, height).unwrap();
        let person = confidence.iter().filter(|value| **value > 0.5).count() as f64
            / confidence.len() as f64;
        eprintln!("portrait: {:.1}% person in {:?}", person * 100.0, started.elapsed());
        assert!((0.3..0.7).contains(&person), "person share {person}");
    }

    #[test]
    fn frames_are_painted_only_on_the_border() {
        let (width, height) = (64u32, 36u32);
        let mut bgra = [255u8; 4].repeat(64 * 36);
        draw_frame(&mut bgra, width, height, Frame::Coral);
        // The band is max(3, 36 / 80) = 3 pixels, in BGRA.
        assert_eq!(&bgra[..4], &[41, 77, 255, 255]);
        let inside = (3 * 64 + 3) * 4;
        assert_eq!(&bgra[inside..inside + 4], &[255, 255, 255, 255]);
        let centre = (18 * 64 + 32) * 4;
        assert_eq!(&bgra[centre..centre + 4], &[255, 255, 255, 255]);
        let right_edge = (18 * 64 + 63) * 4;
        assert_eq!(&bgra[right_edge..right_edge + 4], &[41, 77, 255, 255]);

        let mut none = [255u8; 4].repeat(64 * 36);
        draw_frame(&mut none, width, height, Frame::None);
        assert!(none.iter().all(|byte| *byte == 255));

        // Decorative frames alternate along the band.
        let mut geometric = [0u8; 4].repeat(64 * 36);
        draw_frame(&mut geometric, width, height, Frame::Geometric);
        let first_cell = (64 + 1) * 4;
        assert_eq!(&geometric[first_cell..first_cell + 4], &[41, 77, 255, 255]);
        let next_cell = (64 + 3) * 4;
        assert_eq!(&geometric[next_cell..next_cell + 4], &[225, 131, 87, 255]);
    }

    #[test]
    fn touch_up_softens_edges_but_not_flat_colour() {
        crate::encoder::Mp4Encoder::init_global();
        let key = CompositeKey {
            touch_up: 100,
            ..key(Background::Original, Canvas::Transparent)
        };
        let mut lighting = Lighting::default();
        let flat = [200u8, 100, 50, 255].repeat(64 * 36);
        let mut compositor = Compositor::build(key).unwrap();
        let out = run_pixels(&mut compositor, &flat, None, 3, &mut lighting);
        assert!(!out.is_empty(), "graph produced no frames");
        assert_close(&out[0].0[..4], [200, 100, 50, 255]);

        // Two halves: the seam softens, the far sides stay as they were.
        let split: Vec<u8> = (0..64 * 36)
            .flat_map(|i| if i % 64 < 32 { [200u8, 100, 50, 255] } else { [20, 40, 60, 255] })
            .collect();
        let mut compositor = Compositor::build(key).unwrap();
        let out = run_pixels(&mut compositor, &split, None, 3, &mut lighting);
        let pixel = |x: usize| &out[0].0[(18 * 64 + x) * 4..(18 * 64 + x) * 4 + 4];
        assert_close(pixel(2), [200, 100, 50, 255]);
        assert_close(pixel(61), [20, 40, 60, 255]);
        let seam = pixel(31);
        assert!(seam[0] > 30 && seam[0] < 190, "seam is mixed: {seam:?}");
    }

    #[test]
    fn auto_lighting_eases_exposure_toward_mid_grey() {
        crate::encoder::Mp4Encoder::init_global();
        let key = CompositeKey {
            auto_lighting: true,
            ..key(Background::Original, Canvas::Transparent)
        };
        let mut lighting = Lighting::default();
        let dark = [30u8, 30, 30, 255].repeat(64 * 36);
        let mut compositor = Compositor::build(key).unwrap();
        let out = run_pixels(&mut compositor, &dark, None, 48, &mut lighting);
        assert!(out.len() >= 40, "{} frames", out.len());
        assert!(lighting.gain > 1.2, "gain eased up: {}", lighting.gain);
        let (first, last) = (out[0].0[0], out[out.len() - 1].0[0]);
        assert!(last > first + 5, "{first} -> {last}");

        // A bright scene brings it back down.
        let bright = [240u8, 240, 240, 255].repeat(64 * 36);
        let out = run_pixels(&mut compositor, &bright, None, 96, &mut lighting);
        assert!(lighting.gain < 1.0, "gain eased down: {}", lighting.gain);
        assert!(out[out.len() - 1].0[0] < 235);
    }

    #[test]
    fn light_levels_reach_full_output_in_both_directions() {
        assert_eq!(light_levels(1.0), (0.0, 1.0, 1.0));
        // 1.05 × (1.25 × in − 0.5) + 0.5 = 1.3125 × in − 0.025
        let (black, white, output) = light_levels(1.25);
        assert!((black - 0.025 / 1.3125).abs() < 1e-5);
        assert!((white - (black + 1.0 / 1.3125)).abs() < 1e-5 && white < 1.0);
        assert!((output - 1.0).abs() < 1e-5);
        let (black, white, output) = light_levels(0.85);
        assert!((white - 1.0).abs() < 1e-6);
        assert!((output - (1.0 - black) * 0.8925).abs() < 1e-5 && output < 1.0);
        // The scene's luminance is recovered from a lit frame.
        assert!((scene_luma(0.3, 1.0) - 0.3).abs() < 1e-6);
        assert!((scene_luma(1.3125 * 0.3 - 0.025, 1.25) - 0.3).abs() < 1e-5);
    }

    #[test]
    fn mean_luma_samples_the_frame() {
        let grey = [128u8, 128, 128, 255].repeat(64 * 36);
        assert!((mean_luma(&grey, 64) - 128.0 / 255.0).abs() < 1e-3);
        assert_eq!(mean_luma(&[], 64), AUTO_LIGHTING_TARGET_LUMA);
    }

    #[test]
    fn pipeline_keeps_the_frame_and_look_with_an_original_background() {
        let coral = FrameSettings {
            effects: CameraEffects {
                frame: Frame::Coral,
                ..CameraEffects::default()
            },
            look: Look::default(),
        };
        let (pushed, delivered, errors) =
            run_pipeline(PathBuf::from("unused.onnx"), Some(coral), 4);
        assert!(errors.is_empty(), "{errors:?}");
        assert_eq!(times(&delivered), pushed);
        assert_eq!(&delivered[0].bgra[..4], &[41, 77, 255, 255]);

        let touched = FrameSettings {
            look: Look {
                touch_up: 0.5,
                auto_lighting: true,
            },
            ..coral
        };
        let (pushed, delivered, errors) =
            run_pipeline(PathBuf::from("unused.onnx"), Some(touched), 4);
        assert!(errors.is_empty(), "{errors:?}");
        assert!(!delivered.is_empty() && delivered.len() <= pushed.len());
        assert_eq!(&delivered[0].bgra[..4], &[41, 77, 255, 255]);
        // Only the border is painted; the picture came through the graph.
        let centre = (90 * 320 + 160) * 4;
        assert_close(&delivered[0].bgra[centre..centre + 4], [90, 120, 160, 255]);
    }
}
