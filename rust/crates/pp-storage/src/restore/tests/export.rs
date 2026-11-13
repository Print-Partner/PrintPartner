//! Backup export under `<data>/backups/`, and the free-space checks that
//! replace the fixed size limits. Sizes and free space come from lowered
//! thresholds and injected readings, never from real gigabytes.

use super::*;
use crate::WriterFailure;
use crate::restore::{BackupFile, BackupWarning, RestoreError, Sizing};

const FREE_BYTES: u64 = 4096;

fn little_free(_: &Path) -> std::io::Result<u64> {
    Ok(FREE_BYTES)
}

fn one_byte_short(archive: &Path, limits: archive::Limits) -> Result<u64> {
    Ok(archive::declared_bytes(archive, limits)? - 1)
}

/// The export folders under `backups`, sorted.
fn exports(root: &Path) -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(root.join("backups"))
        .map(|entries| {
            entries
                .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
                .filter(|name| name.starts_with("export-"))
                .collect()
        })
        .unwrap_or_default();
    names.sort();
    names
}

fn entry_paths(backup: &BackupFile) -> Vec<String> {
    let file = File::open(&backup.path).unwrap();
    tar::Archive::new(flate2::read::GzDecoder::new(file))
        .entries()
        .unwrap()
        .map(|entry| {
            entry
                .unwrap()
                .path()
                .unwrap()
                .to_string_lossy()
                .into_owned()
        })
        .collect()
}

fn staged_dir(root: &Path, staged: &Staged) -> PathBuf {
    root.join(WORK_DIR)
        .join(format!("{STAGED_PREFIX}{}", staged.restore_id))
}

#[test]
fn export_writes_beside_the_data_without_the_temporary_directory() {
    let (_installation, root) = installation();
    let owner = open(&root).unwrap();
    let client = owner.installation().unwrap();
    let live = state(&root);
    let database = pp_testkit::node_schema::database_files(&root);
    // Nothing can be created under a regular file.
    let unusable = root.join(archive::DATABASE_FILE).join("tmp");
    // SAFETY: nextest runs this test in its own process, and no other thread
    // of the test reads the environment.
    unsafe { std::env::set_var("TMPDIR", unusable) };
    assert!(tempfile::tempdir().is_err());

    let first = client.export_backup("test").unwrap();
    assert!(pp_testkit::node_schema::database_files(&root) == database);
    assert!(first.path.starts_with(root.join("backups")));
    assert_eq!(exports(&root).len(), 1);
    let bytes = std::fs::read(&first.path).unwrap();
    assert_eq!(first.size_bytes, bytes.len() as u64);
    assert_eq!(
        first.sha256,
        hex::encode(<sha2::Sha256 as sha2::Digest>::digest(&bytes))
    );
    assert_eq!(first.metadata.scope, archive::MetadataScope::Full);
    assert!(first.warnings.is_empty());
    assert_eq!(
        std::fs::read_dir(first.path.parent().unwrap())
            .unwrap()
            .count(),
        1,
        "the working copies are removed"
    );
    assert_eq!(state(&root), live);

    // A second export leaves the first in place and does not contain it.
    let second = client.export_backup("test").unwrap();
    assert_eq!(std::fs::read(&first.path).unwrap(), bytes);
    assert_eq!(exports(&root).len(), 2);
    let paths = entry_paths(&second);
    assert!(paths.iter().any(|path| path == "repos/source-a/part.stl"));
    assert!(!paths.iter().any(|path| path.starts_with("backups")));
    drop((first, second));
    assert!(exports(&root).is_empty());

    owner.shutdown().unwrap();
    assert!(matches!(
        client.export_backup("closed"),
        Err(RestoreError::Writer(WriterFailure::Stopped))
    ));
}

#[test]
fn too_little_free_space_refuses_a_backup_and_a_stage_and_leaves_nothing() {
    let (_installation, root) = installation();
    let owner = open(&root).unwrap();
    let client = owner.installation().unwrap();
    let mut tight = client.clone();
    tight.sizing = Sizing {
        reserve: 0,
        free_space: little_free,
        ..tight.sizing
    };
    let database = pp_testkit::node_schema::database_files(&root);

    let Err(RestoreError::NoSpace(refused)) = tight.export_backup("test") else {
        panic!("a backup larger than the free space was written");
    };
    assert!(refused.needed > FREE_BYTES && refused.available == FREE_BYTES);
    assert!(exports(&root).is_empty());

    let error = tight.begin_upload(FREE_BYTES + 1).err().unwrap();
    let expected = archive::NoSpace {
        needed: FREE_BYTES + 1,
        available: FREE_BYTES,
    };
    assert_eq!(refusal(&error), Some(&Refusal::NoSpace(expected)));
    assert!(work_entries(&root).is_empty());

    let backup = client.export_backup("test").unwrap();
    let declared = archive::declared_bytes(&backup.path, archive::Limits::free_disk()).unwrap();
    assert!(declared > FREE_BYTES);
    let mut upload = client.begin_upload(backup.size_bytes).unwrap();
    upload.write(&std::fs::read(&backup.path).unwrap()).unwrap();
    let error = tight.stage(upload).unwrap_err();
    let expected = archive::NoSpace {
        needed: declared,
        available: FREE_BYTES,
    };
    assert_eq!(refusal(&error), Some(&Refusal::NoSpace(expected)));
    assert!(work_entries(&root).is_empty(), "nothing was extracted");
    assert!(pp_testkit::node_schema::database_files(&root) == database);
    owner.shutdown().unwrap();
}

#[test]
fn a_backup_over_the_warning_threshold_succeeds_with_a_warning_and_stages() {
    let (_installation, root) = installation();
    let owner = open(&root).unwrap();
    let mut client = owner.installation().unwrap();
    let live = state(&root);
    client.sizing.warn_over = 1024;
    let backup = client.export_backup("test").unwrap();
    assert!(backup.size_bytes > 1024);
    assert_eq!(backup.warnings, [BackupWarning::Large]);
    let staged = stage(&client, &backup.path).unwrap();
    assert_eq!(state(&staged_dir(&root, &staged)), live);
    owner.shutdown().unwrap();
}

#[test]
fn extraction_stops_before_writing_more_than_the_headers_declared() {
    let (_installation, root) = installation();
    let owner = open(&root).unwrap();
    let mut client = owner.installation().unwrap();
    let backup = client.export_backup("test").unwrap();
    let declared = archive::declared_bytes(&backup.path, archive::Limits::free_disk()).unwrap();
    let live = state(&root);
    client.sizing.declared_bytes = one_byte_short;
    let error = stage(&client, &backup.path).unwrap_err();
    assert_eq!(
        refusal(&error),
        Some(&Refusal::LargerThanDeclared(declared - 1))
    );
    assert!(work_entries(&root).is_empty(), "the stage was removed");
    assert_eq!(state(&root), live);
    owner.shutdown().unwrap();
}

#[test]
fn open_removes_only_the_export_folders_a_killed_backup_left() {
    let (_installation, root) = installation();
    let owner = open(&root).unwrap();
    let killed = owner.installation().unwrap().export_backup("test").unwrap();
    let left = killed.path.parent().unwrap().to_owned();
    // A killed process runs no destructor.
    std::mem::forget(killed);
    owner.shutdown().unwrap();
    assert!(left.join("print-partner-backup.tar.gz").is_file());

    let backups = root.join("backups");
    let outside = tempfile::tempdir().unwrap();
    std::fs::write(outside.path().join("keep"), b"keep").unwrap();
    let id = "0123456789abcdef0123456789abcdef";
    std::os::unix::fs::symlink(outside.path(), backups.join(format!("export-{id}"))).unwrap();
    std::fs::write(
        backups.join("export-fedcba9876543210fedcba9876543210"),
        b"file",
    )
    .unwrap();
    std::fs::create_dir(backups.join("export-not-an-id")).unwrap();
    std::fs::write(backups.join("print-partner-2026.tar.gz"), b"node").unwrap();
    let kept = |root: &Path| {
        let mut names: Vec<String> = std::fs::read_dir(root.join("backups"))
            .unwrap()
            .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        names
    };
    let mut before = kept(&root);
    before.retain(|name| *name != left.file_name().unwrap().to_string_lossy());
    assert!(before.iter().any(|name| name.starts_with("pre-schema")));

    open(&root).unwrap().shutdown().unwrap();
    assert!(!left.exists());
    assert_eq!(kept(&root), before);
    assert_eq!(std::fs::read(outside.path().join("keep")).unwrap(), b"keep");
}
