//! Persistent storage for the desktop session bundle (JWT + user info).
//!
//! Originally this module used the macOS Keychain. The default Keychain
//! ACL ties the entry to the exact code-signing identity that wrote it,
//! so re-signed dev builds and freshly notarised stable builds prompted
//! the user with "Clipy wants to use your confidential information…" on
//! every launch. The session bundle is a JWT bearer with a server-side
//! TTL that the user can revoke from the web — no stronger-than-disk
//! protection is justified for that. So we store it as a JSON file in
//! the app data dir, owner-only readable by the macOS file system.
//!
//! API is kept stable (`save` / `load` / `clear`) so callers don't
//! change. Each call takes the path explicitly, computed once in
//! `lib.rs::setup` from `app.path().app_data_dir()`.

use anyhow::{Context, Result};
use std::path::Path;

pub fn save(path: &Path, data: &[u8]) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).context("create session dir")?;
    }
    write_owner_only(path, data).context("write session file")
}

pub fn load(path: &Path) -> Result<Option<Vec<u8>>> {
    match std::fs::read(path) {
        Ok(bytes) if bytes.is_empty() => Ok(None),
        Ok(bytes) => Ok(Some(bytes)),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(err) => Err(anyhow::Error::from(err).context("read session file")),
    }
}

pub fn clear(path: &Path) -> Result<()> {
    match std::fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(err) => Err(anyhow::Error::from(err).context("delete session file")),
    }
}

#[cfg(unix)]
fn write_owner_only(path: &Path, data: &[u8]) -> std::io::Result<()> {
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt;

    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(path)?;
    file.write_all(data)?;
    Ok(())
}

#[cfg(not(unix))]
fn write_owner_only(path: &Path, data: &[u8]) -> std::io::Result<()> {
    std::fs::write(path, data)
}
