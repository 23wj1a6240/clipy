//! Local edition stand-in for `upload.rs`: nothing is ever uploaded. Built only
//! without the `cloud` feature. The local edition records Local takes only, so
//! the recording controller never reaches `StreamingUploader::start`; it still
//! names these types, and this keeps that code compiling with a refusal in
//! place of the network.

use std::path::PathBuf;
use std::sync::Arc;

use anyhow::{bail, Result};
use tauri::{AppHandle, Runtime};

use crate::capture_health::UploadHealth;
use crate::types::{RecordingId, ShareLinkDto};

/// Same shape as the official `UploaderHandle`, so the recording controller
/// compiles unchanged. Never constructed here.
pub struct UploaderHandle {
    pub finish: Box<dyn FnOnce() -> tokio::task::JoinHandle<Result<ShareLinkDto>> + Send>,
    pub abort: Box<dyn FnOnce() -> tokio::task::JoinHandle<Result<()>> + Send>,
    pub cancel: Arc<dyn Fn() + Send + Sync>,
    pub health: Arc<dyn Fn() -> UploadHealth + Send + Sync>,
}

#[derive(Debug)]
pub(crate) struct UploadCancelled;

impl std::fmt::Display for UploadCancelled {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("upload cancelled")
    }
}

impl std::error::Error for UploadCancelled {}

pub(crate) fn is_upload_cancelled(error: &anyhow::Error) -> bool {
    error
        .chain()
        .any(|cause| cause.downcast_ref::<UploadCancelled>().is_some())
}

pub struct StreamingUploader;

impl StreamingUploader {
    /// Same signature as the official uploader; always refuses.
    #[allow(clippy::too_many_arguments)]
    pub async fn start<R: Runtime>(
        _app: &AppHandle<R>,
        _api_base: String,
        _bearer: String,
        _recording_id: RecordingId,
        _file_path: PathBuf,
        _recording_name: Option<String>,
        _requested_max_height: Option<u32>,
        _cleanup_local_on_complete: bool,
    ) -> Result<UploaderHandle> {
        bail!("Clipy Local does not upload recordings")
    }
}
