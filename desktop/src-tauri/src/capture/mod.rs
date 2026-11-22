//! Screen + audio capture facade. Platforms:
//!   * macOS — ScreenCaptureKit (`macos`) + cpal mic (`audio`)
//!   * Windows — Windows Graphics Capture API + WASAPI loopback (`windows`)

#![allow(dead_code)]

pub mod failure;
pub mod level;
pub mod mic_reopen;
pub mod mic_silence;
pub mod output_size;
pub mod stream_health;

#[cfg(target_os = "macos")]
pub mod macos;

#[cfg(target_os = "macos")]
pub mod sck_stream;

#[cfg(target_os = "macos")]
pub mod camera;

#[cfg(target_os = "macos")]
pub mod audio;

#[cfg(target_os = "macos")]
pub mod input_latency;

#[cfg(target_os = "macos")]
pub mod mic_timeline;

#[cfg(target_os = "windows")]
pub mod windows;

#[cfg(target_os = "windows")]
pub mod windows_camera;

#[cfg(target_os = "windows")]
pub mod windows_camera_effects;
