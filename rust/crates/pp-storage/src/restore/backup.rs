use super::*;
use crate::Work;
use sha2::Digest;
use std::{sync::atomic::AtomicBool, time::Duration};

/// A full backup archive in a private temporary directory outside managed
/// storage. Dropping it removes the directory.
pub struct BackupFile {
    directory: PathBuf,
    pub path: PathBuf,
    pub metadata: archive::Metadata,
    pub size_bytes: u64,
    pub sha256: String,
}

impl Drop for BackupFile {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.directory);
    }
}

impl InstallationClient {
    /// Writes a full backup to a new private temporary directory, outside
    /// managed storage, through the writer like `WriterOwner::create_archive`.
    pub fn backup_to_temporary(&self, app_version: &str) -> Result<BackupFile> {
        let _active = self.activity.enter()?;
        let directory = std::env::temp_dir().join(format!("pp-backup-{}", random_id()));
        ensure!(
            !directory.starts_with(&self.data_dir),
            "Backup inside managed storage"
        );
        pp_os::create_private_dir(&directory)?;
        let mut backup = BackupFile {
            path: directory.join(format!("print-partner-backup-{}.tar.gz", random_id())),
            directory,
            metadata: archive::Metadata::new(app_version, archive::Scope::Full),
            size_bytes: 0,
            sha256: String::new(),
        };
        let job = archive::Job {
            destination: backup.path.clone(),
            data_dir: self.data_dir.clone(),
            metadata: backup.metadata.clone(),
            request: archive::Request::new(app_version, archive::Scope::Full),
        };
        let reply = self.client.enqueue(
            Work::Archive(Box::new(job)),
            &AtomicBool::new(false),
            Duration::from_secs(5),
        )?;
        reply
            .recv()
            .map_err(|_| anyhow!("Writer stopped without backup result"))??;
        let mut file = File::open(&backup.path)?;
        let mut digest = sha2::Sha256::new();
        backup.size_bytes = std::io::copy(&mut file, &mut digest)?;
        backup.sha256 = hex::encode(digest.finalize());
        Ok(backup)
    }
}
