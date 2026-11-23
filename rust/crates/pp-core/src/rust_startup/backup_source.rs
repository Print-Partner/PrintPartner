//! The Rust-only runtime's [`Export`]: GRE-1371's `export_backup`, which runs
//! GRE-403's archive writer. Here because `rust_startup` is already gated.

use crate::backup::{BackupFailure, Export, Exported};
use pp_storage::{
    WriterFailure, WriterOwner,
    restore::{BackupFile, InstallationClient, Refusal, RestoreError},
};
use std::{path::Path, sync::Arc};

/// The client holds no `Arc<Mutex<WriterOwner>>`, and closing the writer does
/// not wait for it; once closed, it answers [`BackupFailure::Stopping`].
pub(crate) fn backup_source(writer: &WriterOwner) -> anyhow::Result<Arc<dyn Export>> {
    Ok(Arc::new(writer.installation()?))
}

impl Export for InstallationClient {
    fn export(&self) -> Result<Box<dyn Exported>, BackupFailure> {
        match self.export_backup(env!("CARGO_PKG_VERSION")) {
            Ok(file) => Ok(Box::new(file)),
            Err(RestoreError::Refused(Refusal::NoSpace(_))) => Err(BackupFailure::NoSpace),
            Err(RestoreError::Refused(Refusal::Busy)) => Err(BackupFailure::RestoreRunning),
            Err(RestoreError::Writer(WriterFailure::Stopped)) => Err(BackupFailure::Stopping),
            Err(_) => Err(BackupFailure::Failed),
        }
    }
}

impl Exported for BackupFile {
    fn archive(&self) -> &Path {
        &self.path
    }
    fn sha256(&self) -> &str {
        &self.sha256
    }
}

#[cfg(test)]
mod tests;
