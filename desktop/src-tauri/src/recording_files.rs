//! Deleting a recording's files from disk: the MP4 and every sidecar written
//! beside it. Moved out of `upload.rs` unchanged; local and recovered
//! recordings use it too.

use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};

async fn remove_uploaded_media(file_path: &Path) -> Result<()> {
    remove_uploaded_path(file_path, "recording").await?;
    Ok(())
}

async fn remove_uploaded_sidecars(mp4_path: &Path) -> Result<()> {
    let paths = recording_sidecar_paths(mp4_path)?;
    for path in paths {
        remove_uploaded_path(&path, "recording sidecar").await?;
    }
    Ok(())
}

fn recording_sidecar_paths(mp4_path: &Path) -> Result<Vec<PathBuf>> {
    let Some(dir) = mp4_path.parent() else {
        bail!("recording path has no parent");
    };
    let Some(stem) = mp4_path.file_stem().and_then(|stem| stem.to_str()) else {
        bail!("recording path has no usable stem");
    };
    Ok([
        "cursor.json",
        "zoom.json",
        "input-events.json",
        "screencharm-project.json",
        "record-info.json",
        "player-props.json",
        "screencharm.json",
        "camera.webm",
        "camera.mp4",
        "studio.json",
        "project.json",
    ]
    .into_iter()
    .map(|suffix| dir.join(format!("{stem}.{suffix}")))
    .collect())
}

pub(crate) async fn remove_local_recording_artifacts(mp4_path: &Path) -> Result<()> {
    let mut paths = recording_sidecar_paths(mp4_path)?;
    paths.push(mp4_path.to_path_buf());
    for path in paths {
        match tokio::fs::symlink_metadata(&path).await {
            Ok(metadata) if metadata.is_dir() => {
                bail!("recording artifact is a directory: {}", path.display())
            }
            Ok(_) => {}
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
            Err(err) => return Err(err).with_context(|| format!("inspect {}", path.display())),
        }
    }
    remove_uploaded_media(mp4_path).await?;
    if let Err(err) = remove_uploaded_sidecars(mp4_path).await {
        tracing::warn!(
            recording = %mp4_path.display(),
            "recording media deleted but a sidecar remains for orphan cleanup: {err:#}"
        );
    }
    Ok(())
}

async fn remove_uploaded_path(path: &Path, label: &str) -> std::io::Result<()> {
    match tokio::fs::remove_file(path).await {
        Ok(()) => {
            tracing::info!(path = %path.display(), "deleted uploaded {label}");
            Ok(())
        }
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(err) => {
            tracing::warn!(
                path = %path.display(),
                "failed to delete uploaded {label}: {err:#}"
            );
            Err(err)
        }
    }
}
