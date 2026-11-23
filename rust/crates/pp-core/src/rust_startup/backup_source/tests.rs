use super::backup_source;
use crate::backup::tests::names;
use crate::{BackupFailure, OfflineBackup, ShutdownGate};
use pp_storage::{Limits, WriterOwner, archive};
use tempfile::TempDir;

fn opened() -> (TempDir, WriterOwner, OfflineBackup, TempDir) {
    let data = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(data.path().join("repos/source-a")).unwrap();
    std::fs::write(data.path().join("repos/source-a/part.stl"), b"solid part").unwrap();
    let owner = WriterOwner::open(data.path(), Limits::default()).unwrap().0;
    let backup = OfflineBackup::new(backup_source(&owner).unwrap(), ShutdownGate::new());
    (data, owner, backup, tempfile::tempdir().unwrap())
}

#[test]
fn ticket_t_29_tray_backup_saves_a_complete_native_archive_in_the_folder() {
    let (data, owner, backup, downloads) = opened();
    let taken = downloads.path().join("print-partner-backup.tar.gz");
    std::fs::write(&taken, b"keep").unwrap();

    let saved = backup.create_in(downloads.path()).unwrap();

    assert_eq!(
        saved,
        downloads.path().join("print-partner-backup (1).tar.gz")
    );
    assert_eq!(std::fs::read(&taken).unwrap(), b"keep", "never replaced");
    let metadata = archive::validate(&saved, archive::Limits::backup()).unwrap();
    assert_eq!(metadata.scope, archive::MetadataScope::Full);
    let kept = [
        "print-partner-backup (1).tar.gz",
        "print-partner-backup.tar.gz",
    ];
    assert_eq!(names(downloads.path(), ""), kept);
    assert!(names(&data.path().join("backups"), "export-").is_empty());
    owner.shutdown().unwrap();
}

#[test]
fn ticket_t_29_tray_backup_failure_leaves_no_backup_file() {
    let (data, owner, backup, downloads) = opened();

    let missing = downloads.path().join("missing");
    assert_eq!(backup.create_in(&missing), Err(BackupFailure::Failed));
    assert!(names(&data.path().join("backups"), "export-").is_empty());

    owner.shutdown().unwrap();
    assert_eq!(
        backup.create_in(downloads.path()),
        Err(BackupFailure::Stopping)
    );
    assert!(names(downloads.path(), "").is_empty());
}
