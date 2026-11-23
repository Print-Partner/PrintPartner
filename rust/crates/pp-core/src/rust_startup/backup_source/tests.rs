use super::backup_source;
use crate::{BackupFailure, OfflineBackup, ShutdownGate};
use pp_storage::{Limits, WriterOwner, archive};
use std::path::Path;
use tempfile::TempDir;

/// A data folder with one Source file, its open writer, the backup the tray
/// gets for it, and an empty Downloads folder.
fn opened() -> (TempDir, WriterOwner, OfflineBackup, TempDir) {
    let data = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(data.path().join("repos/source-a")).unwrap();
    std::fs::write(data.path().join("repos/source-a/part.stl"), b"solid part").unwrap();
    let owner = WriterOwner::open(data.path(), Limits::default()).unwrap().0;
    let backup = OfflineBackup::new(backup_source(&owner).unwrap(), ShutdownGate::new());
    (data, owner, backup, tempfile::tempdir().unwrap())
}

fn entries(folder: &Path) -> Vec<String> {
    std::fs::read_dir(folder)
        .map(|entries| {
            entries
                .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
                .collect()
        })
        .unwrap_or_default()
}

fn exports_left(data: &Path) -> Vec<String> {
    let mut left = entries(&data.join("backups"));
    left.retain(|name| name.starts_with("export-"));
    left
}

#[test]
fn ticket_t_29_tray_backup_saves_a_complete_native_archive_in_the_folder() {
    let (data, owner, backup, downloads) = opened();

    let saved = backup.create_in(downloads.path()).unwrap();

    assert_eq!(saved, downloads.path().join("print-partner-backup.tar.gz"));
    let metadata = archive::validate(&saved, archive::Limits::backup()).unwrap();
    assert_eq!(metadata.scope, archive::MetadataScope::Full);
    assert_eq!(entries(downloads.path()), ["print-partner-backup.tar.gz"]);
    assert!(exports_left(data.path()).is_empty());
    owner.shutdown().unwrap();
}

#[test]
fn ticket_t_29_tray_backup_failure_leaves_no_backup_file() {
    let (data, owner, backup, downloads) = opened();

    let missing = downloads.path().join("missing");
    assert_eq!(backup.create_in(&missing), Err(BackupFailure::Failed));
    assert!(exports_left(data.path()).is_empty());

    owner.shutdown().unwrap();
    assert_eq!(
        backup.create_in(downloads.path()),
        Err(BackupFailure::Stopping)
    );
    assert!(entries(downloads.path()).is_empty());
}
