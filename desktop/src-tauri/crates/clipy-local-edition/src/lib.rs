//! The part of the Clipy Local desktop edition that does not need the app.
//! Everything here is pure and tested on any machine; the app only does the
//! I/O around it (`src/edition_local/local_settings.rs` and the local edition
//! seams in `src/local/mod.rs`).
//!
//! - [`settings`]: the settings the WebView edits (`settings-api.ts`), how a
//!   save is validated and applied, and where keys live (never in a DTO).
//! - [`sidecar`]: `recording.cursor.json` and the transcript to the moment
//!   planner's [`clipy_local_moments::Input`], and the plan back to the v2
//!   JSON the app already validates.
//! - [`disclosure`]: what the AREC and the status line say left the
//!   computer, from what was actually sent for this recording.
//! - [`transcript`]: transcript gaps and segments in the app's format.

pub mod disclosure;
pub mod settings;
pub mod sidecar;
mod text;
pub mod transcript;

pub use clipy_local_moments as moments;
#[cfg(feature = "stt")]
pub use clipy_local_stt as stt;
