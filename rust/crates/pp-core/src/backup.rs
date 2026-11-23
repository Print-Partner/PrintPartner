//! The tray's Back up now (GRE-1036): one native backup (GRE-403, through
//! GRE-1371's export), saved as a new file in a folder. Only a Rust-only start
//! makes an [`Export`], so a Node-backed runtime has no [`OfflineBackup`].

use crate::ShutdownGate;
use sha2::Digest;
use std::{
    fs::File,
    io,
    path::{Path, PathBuf},
    sync::Arc,
};

/// Why a backup was not saved. Fieldless, so no path or error text can reach
/// a dialog or a log line through it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BackupFailure {
    NoSpace,
    RestoreRunning,
    Stopping,
    Failed,
}

/// The backup of a runtime whose Rust writer owns the data folder.
#[derive(Clone)]
pub struct OfflineBackup {
    export: Arc<dyn Export>,
    gate: ShutdownGate,
}

impl OfflineBackup {
    pub(crate) fn new(export: Arc<dyn Export>, gate: ShutdownGate) -> Self {
        Self { export, gate }
    }

    /// Saves one full backup in `folder` as `print-partner-backup.tar.gz`,
    /// or `print-partner-backup (n).tar.gz` for the first free n up to 999,
    /// and returns its path. Never replaces a file. Blocks until done, and
    /// holds off shutdown until the export folder is gone.
    pub fn create_in(&self, folder: &Path) -> Result<PathBuf, BackupFailure> {
        let work = self.gate.track().map_err(|_| BackupFailure::Stopping)?;
        let exported = self.export.export()?;
        let saved = save(exported.as_ref(), folder);
        drop(exported);
        drop(work);
        saved
    }
}

/// A trait so the ungated `CoreHandle` can hold the gated writer's export.
pub(crate) trait Export: Send + Sync {
    fn export(&self) -> Result<Box<dyn Exported>, BackupFailure>;
}

/// A complete archive in its export folder. Dropping it removes the folder.
pub(crate) trait Exported: Send {
    fn archive(&self) -> &Path;
    /// Lowercase hex.
    fn sha256(&self) -> &str;
}

fn save(exported: &dyn Exported, folder: &Path) -> Result<PathBuf, BackupFailure> {
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
        Err(_) => copy_then_publish(exported, folder)?,
    };
    // The archive is already complete under its name.
    if pp_os::sync_directory(folder).is_err() {
        tracing::warn!("tray_backup_directory_sync_failed");
    }
    Ok(saved)
}

/// For a folder on another drive: copies the archive into a hidden private
/// file, checks the copy's SHA-256 against the export's, then publishes it.
/// A copy that is not published is removed.
fn copy_then_publish(exported: &dyn Exported, folder: &Path) -> Result<PathBuf, BackupFailure> {
    let id = rand::random::<u128>();
    let path = folder.join(format!(".print-partner-backup-{id:032x}.partial"));
    let copy = pp_os::create_new_private_file(&path).map_err(|_| BackupFailure::Failed)?;
    let published =
        copy_checked(copy, exported, &path).and_then(|()| publish_new_name(&path, folder));
    if published.is_err() {
        let _ = std::fs::remove_file(&path);
    }
    published.map_err(|_| BackupFailure::Failed)
}

/// Closes `copy` before returning, so the caller can remove it on Windows.
fn copy_checked(mut copy: File, exported: &dyn Exported, path: &Path) -> io::Result<()> {
    io::copy(&mut File::open(exported.archive())?, &mut copy)?;
    copy.sync_all()?;
    drop(copy);
    let mut digest = sha2::Sha256::new();
    io::copy(&mut File::open(path)?, &mut digest)?;
    if hex::encode(digest.finalize()) != exported.sha256() {
        return Err(io::ErrorKind::InvalidData.into());
    }
    Ok(())
}

fn publish_new_name(from: &Path, folder: &Path) -> io::Result<PathBuf> {
    for n in 0..=999 {
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
pub(crate) mod tests;
