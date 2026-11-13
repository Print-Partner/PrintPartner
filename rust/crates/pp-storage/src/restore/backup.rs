//! Whole-installation backup export for one open [`crate::WriterOwner`].

use super::{InstallationClient, RestoreError, random_id};
use crate::{Envelope, Work, WriterFailure, archive};
use sha2::Digest;
use std::{
    fs::File,
    path::PathBuf,
    sync::{atomic::AtomicBool, mpsc},
    time::Duration,
};

/// A full backup archive in a private temporary directory outside managed
/// storage. Dropping it removes the directory.
pub struct BackupFile {
    directory: PathBuf,
    pub path: PathBuf,
    pub metadata: archive::Metadata,
    pub size_bytes: u64,
    /// Lowercase hex SHA-256 of the archive file.
    pub sha256: String,
}

impl Drop for BackupFile {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.directory);
    }
}

fn internal(error: impl Into<anyhow::Error>) -> WriterFailure {
    WriterFailure::Internal(error.into())
}

impl InstallationClient {
    /// Writes a full backup to a new private temporary directory, through
    /// the writer like `WriterOwner::create_archive`. The caller names no
    /// path.
    pub fn backup_to_temporary(&self, app_version: &str) -> Result<BackupFile, RestoreError> {
        let _active = self.activity.enter().map_err(|_| WriterFailure::Stopped)?;
        let directory = std::env::temp_dir().join(format!("pp-backup-{}", random_id()));
        if directory.starts_with(&self.data_dir) {
            let inside = anyhow::anyhow!("Temporary directory is inside managed storage");
            return Err(internal(inside).into());
        }
        pp_os::create_private_dir(&directory).map_err(internal)?;
        let mut backup = BackupFile {
            path: directory.join(format!("print-partner-backup-{}.tar.gz", random_id())),
            directory,
            metadata: archive::Metadata::new(app_version, archive::Scope::Full),
            size_bytes: 0,
            sha256: String::new(),
        };
        let work = Work::Archive(Box::new(archive::Job {
            destination: backup.path.clone(),
            data_dir: self.data_dir.clone(),
            metadata: backup.metadata.clone(),
            request: archive::Request::new(app_version, archive::Scope::Full),
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
            .map_err(WriterFailure::from)?;
        let mut file = File::open(&backup.path).map_err(internal)?;
        let mut digest = sha2::Sha256::new();
        backup.size_bytes = std::io::copy(&mut file, &mut digest).map_err(internal)?;
        backup.sha256 = hex::encode(digest.finalize());
        Ok(backup)
    }
}
