//! The tray's Back up now (GRE-1036): one full native backup (GRE-403, by way
//! of GRE-1371's export), saved as a new file in a folder the caller names.
//!
//! Compiled in every build. Only a Rust-only start makes an [`Export`]
//! (`rust_startup::backup_source`), so a Node-backed runtime never has an
//! [`OfflineBackup`].

use crate::ShutdownGate;
use sha2::Digest;
use std::{
    fs::File,
    io::{self, Read, Write},
    path::{Path, PathBuf},
    sync::Arc,
};
use tokio_util::sync::CancellationToken;

const MAX_SUFFIX: u32 = 999;
const CHUNK: usize = 1 << 20;

/// Why a backup was not saved. Fieldless, so no path, error text or archive
/// content can reach a dialog or a log line through it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BackupFailure {
    /// The data folder's drive lacks room for the export plus the reserve.
    NoSpace,
    /// A restore holds the installation.
    RestoreRunning,
    /// Print Partner is shutting down.
    Stopping,
    /// The archive could not be written or saved in the folder.
    Failed,
}

/// The backup of a runtime whose Rust writer owns the data folder. Get it
/// from [`crate::CoreHandle::offline_backup`].
#[derive(Clone)]
pub struct OfflineBackup {
    export: Arc<dyn Export>,
    gate: ShutdownGate,
}

impl OfflineBackup {
    pub(crate) fn new(export: Arc<dyn Export>, gate: ShutdownGate) -> Self {
        Self { export, gate }
    }

    /// Makes one full backup and saves it in `folder` as
    /// `print-partner-backup.tar.gz`, or `print-partner-backup (n).tar.gz`
    /// for the first free n up to 999. Never replaces or removes a file in
    /// `folder`. Blocks for as long as the backup takes, and holds off
    /// shutdown until the archive is saved and the export folder is gone.
    pub fn create_in(&self, folder: &Path) -> Result<PathBuf, BackupFailure> {
        let work = self.gate.track().map_err(|_| BackupFailure::Stopping)?;
        let exported = self.export.export()?;
        let saved = save(exported.as_ref(), folder, &work.cancellation());
        drop(exported);
        drop(work);
        saved
    }
}

/// The Rust writer's native backup. A trait so the ungated `CoreHandle` can
/// hold it with no `desktop_rebuild` gate.
pub(crate) trait Export: Send + Sync {
    /// Blocks until the writer has written and validated one full archive in
    /// its own export folder.
    fn export(&self) -> Result<Box<dyn Exported>, BackupFailure>;
}

/// A complete archive inside its export folder. Dropping it removes the folder.
pub(crate) trait Exported: Send {
    fn archive(&self) -> &Path;
    /// Lowercase hex SHA-256 of [`Self::archive`].
    fn sha256(&self) -> &str;
}

fn save(
    exported: &dyn Exported,
    folder: &Path,
    cancel: &CancellationToken,
) -> Result<PathBuf, BackupFailure> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(exported.archive(), std::fs::Permissions::from_mode(0o600))
            .map_err(|_| BackupFailure::Failed)?;
    }
    let saved = match publish_new_name(exported.archive(), folder) {
        Ok(saved) => saved,
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
            return Err(BackupFailure::Failed);
        }
        // Another drive, most often: copy instead.
        Err(_) => copy_then_publish(exported, folder, cancel)?,
    };
    // The archive is complete under its name; a failed sync only weakens
    // crash durability, so it neither removes the file nor fails the backup.
    if pp_os::sync_directory(folder).is_err() {
        tracing::warn!("tray_backup_directory_sync_failed");
    }
    Ok(saved)
}

/// Removes a copy that was never published.
struct Partial {
    path: PathBuf,
    published: bool,
}

impl Drop for Partial {
    fn drop(&mut self) {
        if !self.published {
            let _ = std::fs::remove_file(&self.path);
        }
    }
}

/// Copies the archive into a hidden private file in `folder`, checks its
/// SHA-256 against the export's, then publishes it under the first free name.
fn copy_then_publish(
    exported: &dyn Exported,
    folder: &Path,
    cancel: &CancellationToken,
) -> Result<PathBuf, BackupFailure> {
    let failed = |_| BackupFailure::Failed;
    let path = folder.join(format!(
        ".print-partner-backup-{:032x}.partial",
        rand::random::<u128>()
    ));
    let to = pp_os::create_new_private_file(&path).map_err(failed)?;
    let mut partial = Partial {
        path,
        published: false,
    };
    // Bound after the guard so the file closes before the guard removes it.
    let mut to = to;
    let mut from = File::open(exported.archive()).map_err(failed)?;
    let mut digest = sha2::Sha256::new();
    let mut buffer = vec![0; CHUNK];
    loop {
        if cancel.is_cancelled() {
            return Err(BackupFailure::Stopping);
        }
        let read = match from.read(&mut buffer) {
            Ok(0) => break,
            Ok(read) => read,
            Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
            Err(error) => return Err(failed(error)),
        };
        digest.update(&buffer[..read]);
        to.write_all(&buffer[..read]).map_err(failed)?;
    }
    to.sync_all().map_err(failed)?;
    drop(to);
    if hex::encode(digest.finalize()) != exported.sha256() {
        return Err(BackupFailure::Failed);
    }
    let saved = publish_new_name(&partial.path, folder).map_err(failed)?;
    partial.published = true;
    Ok(saved)
}

/// Moves `from` to the first free backup name in `folder`. Never replaces.
fn publish_new_name(from: &Path, folder: &Path) -> io::Result<PathBuf> {
    for n in 0..=MAX_SUFFIX {
        let to = folder.join(match n {
            0 => "print-partner-backup.tar.gz".to_owned(),
            n => format!("print-partner-backup ({n}).tar.gz"),
        });
        match pp_os::rename_new(from, &to) {
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
            moved => return moved.map(|()| to),
        }
    }
    Err(io::ErrorKind::AlreadyExists.into())
}

#[cfg(test)]
mod tests;
