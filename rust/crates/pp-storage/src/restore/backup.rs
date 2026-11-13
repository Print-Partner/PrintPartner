//! Whole-installation backup export for one open [`crate::WriterOwner`].
//!
//! Each export lives in its own folder, `<data>/backups/export-<32 hex>/`,
//! with the working copies made for it. `backups` also holds the schema
//! upgrade's rollback copies and, in a folder Node once ran, Node's archives;
//! nothing here touches those. `backups` is not a backup root, so no export
//! contains another.

use super::{InstallationClient, RestoreError, is_id, random_id};
use crate::{Envelope, Work, WriterFailure, archive};
use sha2::Digest;
use std::{
    fs::File,
    path::{Path, PathBuf},
    sync::{atomic::AtomicBool, mpsc},
    time::Duration,
};

const BACKUPS_DIR: &str = "backups";
const EXPORT_PREFIX: &str = "export-";

/// Something the user should know about a backup that was made.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BackupWarning {
    /// The archive is over 20 GiB. Keep that much free for the next one.
    Large,
}

/// A full backup archive in its own folder under `<data>/backups/`. Dropping
/// it removes the folder.
pub struct BackupFile {
    directory: PathBuf,
    pub path: PathBuf,
    pub metadata: archive::Metadata,
    pub size_bytes: u64,
    /// Lowercase hex SHA-256 of the archive file.
    pub sha256: String,
    pub warnings: Vec<BackupWarning>,
}

impl Drop for BackupFile {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.directory);
    }
}

fn internal(error: impl Into<anyhow::Error>) -> WriterFailure {
    WriterFailure::Internal(error.into())
}

/// Creates the folder for one export. `backups` must be a real directory,
/// so nothing is written through a link.
fn export_dir(data_dir: &Path) -> std::io::Result<PathBuf> {
    let backups = data_dir.join(BACKUPS_DIR);
    match pp_os::create_private_dir(&backups) {
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
        result => result?,
    }
    if !backups.symlink_metadata()?.is_dir() {
        return Err(std::io::Error::other("backups is not a directory"));
    }
    let directory = backups.join(format!("{EXPORT_PREFIX}{}", random_id()));
    pp_os::create_private_dir(&directory)?;
    Ok(directory)
}

/// Removes the export folders a killed backup left behind. It removes only
/// real directories named `export-<32 hex>` in a real `backups` directory and
/// follows no link. Storage runs it at open, before any client can export,
/// so no live export exists then. A folder it cannot remove is logged and
/// does not stop the open.
pub(super) fn sweep(data_dir: &Path) {
    let backups = data_dir.join(BACKUPS_DIR);
    let real_dir = |path: &Path| path.symlink_metadata().is_ok_and(|found| found.is_dir());
    if !real_dir(&backups) {
        return;
    }
    let Ok(entries) = std::fs::read_dir(&backups) else {
        return;
    };
    for entry in entries.flatten() {
        let name = entry.file_name();
        let export = name
            .to_str()
            .and_then(|name| name.strip_prefix(EXPORT_PREFIX));
        if export.is_some_and(is_id)
            && real_dir(&entry.path())
            && let Err(error) = std::fs::remove_dir_all(entry.path())
        {
            tracing::warn!(folder = ?name, %error, "could not remove an interrupted backup export");
        }
    }
}

impl InstallationClient {
    /// Writes a full backup under `<data>/backups/`, through the writer like
    /// `WriterOwner::create_archive`. The caller names no path. Refuses with
    /// [`RestoreError::NoSpace`], before writing anything, when the backup
    /// would leave less than the reserve free on the data volume.
    pub fn export_backup(&self, app_version: &str) -> Result<BackupFile, RestoreError> {
        let _active = self.activity.admit()?;
        let directory = export_dir(&self.data_dir).map_err(internal)?;
        let mut backup = BackupFile {
            path: directory.join("print-partner-backup.tar.gz"),
            directory,
            metadata: archive::Metadata::new(app_version, archive::Scope::Full),
            size_bytes: 0,
            sha256: String::new(),
            warnings: Vec::new(),
        };
        let mut request = archive::Request::new(app_version, archive::Scope::Full);
        request.reserve_bytes = self.sizing.reserve;
        let work = Work::Archive(Box::new(archive::Job {
            destination: backup.path.clone(),
            data_dir: self.data_dir.clone(),
            metadata: backup.metadata.clone(),
            request,
            free_space: self.sizing.free_space,
        }));
        let (reply, written) = mpsc::channel();
        self.client
            .shared
            .admit(
                Envelope::Setting { work, reply },
                &AtomicBool::new(false),
                Duration::from_secs(5),
            )
            .map_err(WriterFailure::from)?;
        written
            .recv()
            .map_err(|_| WriterFailure::Stopped)?
            .map_err(|error| match error.downcast::<archive::NoSpace>() {
                Ok(no_space) => RestoreError::NoSpace(no_space),
                Err(error) => WriterFailure::from(error).into(),
            })?;
        let mut file = File::open(&backup.path).map_err(internal)?;
        let mut digest = sha2::Sha256::new();
        backup.size_bytes = std::io::copy(&mut file, &mut digest).map_err(internal)?;
        backup.sha256 = hex::encode(digest.finalize());
        if backup.size_bytes > self.sizing.warn_over {
            backup.warnings.push(BackupWarning::Large);
        }
        Ok(backup)
    }
}
