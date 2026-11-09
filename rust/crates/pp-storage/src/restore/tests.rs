//! Restore staging on disposable pp-testkit installations holding a copied
//! Node schema 34 database and a valid ownership marker.

use super::*;
use crate::{Limits, ownership_marker::WriterGate};
use pp_testkit::{Installation, MarkerState as KitMarker, Spec};
use rusqlite::{Connection, OpenFlags};
use std::collections::BTreeMap;
use tar::{EntryType, Header};

const SEED: &str = "INSERT INTO build_profiles(id, name) VALUES (7, 'Seven'), (29, 'Twenty-nine');
INSERT INTO plan_snapshots(id, profile_id, name, created_at, payload_json)
  VALUES (3, 29, 'before', '2026-10-01T00:00:00.000Z', '{\"layers\":2}');
INSERT INTO plan_drafts(id, profile_id, base_plan_version, state, digest_format,
  snapshot_digest, created_by, idempotency_key, created_at)
  VALUES (41, 29, 0, 'open', 'v1', 'aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa',
  'user:1', 'seed', '2026-10-01T00:00:00.000Z');";

/// A marked installation with Builds 7 and 29, a Plan snapshot, an open
/// draft and Source and export files.
fn installation() -> (Installation, PathBuf) {
    let installation = Installation::create(Spec {
        node_database: true,
        marker: KitMarker::Valid,
        ..Spec::default()
    })
    .unwrap();
    Connection::open(installation.database_path())
        .unwrap()
        .execute_batch(SEED)
        .unwrap();
    let root = installation.path().canonicalize().unwrap();
    std::fs::create_dir_all(root.join("repos/source-a")).unwrap();
    std::fs::write(
        root.join("repos/source-a/part.stl"),
        b"solid a\nendsolid a\n",
    )
    .unwrap();
    std::fs::create_dir_all(root.join("exports")).unwrap();
    std::fs::write(root.join("exports/plate.3mf"), b"plate").unwrap();
    (installation, root)
}

fn open(root: &Path) -> Result<WriterOwner> {
    Ok(WriterOwner::open_with_gate(root, Limits::default(), WriterGate::SwitchOn)?.0)
}

/// Every table's rows, sorted, and every file under the backup roots of
/// `root`, which holds a database and roots laid out like a data directory.
fn state(root: &Path) -> BTreeMap<String, String> {
    let mut state = BTreeMap::new();
    let db = Connection::open_with_flags(
        root.join(archive::DATABASE_FILE),
        OpenFlags::SQLITE_OPEN_READ_ONLY,
    )
    .unwrap();
    let tables: Vec<String> = db
        .prepare("SELECT name FROM sqlite_master WHERE type = 'table' ORDER BY name")
        .unwrap()
        .query_map([], |row| row.get(0))
        .unwrap()
        .collect::<rusqlite::Result<_>>()
        .unwrap();
    for table in tables {
        let mut statement = db.prepare(&format!("SELECT * FROM \"{table}\"")).unwrap();
        let columns = statement.column_count();
        let mut rows: Vec<String> = statement
            .query_map([], |row| {
                (0..columns)
                    .map(|index| row.get::<_, rusqlite::types::Value>(index))
                    .collect::<rusqlite::Result<Vec<_>>>()
                    .map(|values| format!("{values:?}"))
            })
            .unwrap()
            .collect::<rusqlite::Result<_>>()
            .unwrap();
        rows.sort();
        state.insert(format!("table {table}"), rows.join("\n"));
    }
    let mut pending: Vec<PathBuf> = archive::FULL_ROOTS
        .iter()
        .map(|(name, _)| root.join(name))
        .collect();
    while let Some(path) = pending.pop() {
        let Ok(metadata) = path.symlink_metadata() else {
            continue;
        };
        let name = path.strip_prefix(root).unwrap().display().to_string();
        if metadata.is_dir() {
            state.insert(format!("dir {name}"), String::new());
            pending.extend(std::fs::read_dir(&path).unwrap().map(|e| e.unwrap().path()));
        } else {
            let bytes = std::fs::read(&path).unwrap();
            state.insert(
                format!("file {name}"),
                String::from_utf8_lossy(&bytes).into(),
            );
        }
    }
    state
}

/// Writes a full backup of `owner` to `directory`, then deletes the Source
/// file and adds an export. Returns the archive and the state it holds.
fn backup_then_change(
    owner: &WriterOwner,
    root: &Path,
    directory: &Path,
) -> (PathBuf, BTreeMap<String, String>) {
    let backed_up = state(root);
    let path = directory.join("full.tar.gz");
    owner
        .create_archive(&path, archive::Request::new("test", archive::Scope::Full))
        .unwrap();
    std::fs::remove_file(root.join("repos/source-a/part.stl")).unwrap();
    std::fs::write(root.join("exports/later.3mf"), b"later").unwrap();
    assert_ne!(state(root), backed_up);
    (path, backed_up)
}

fn stage(client: &InstallationClient, archive: &Path) -> Result<Staged> {
    let mut upload = client.begin_upload(1 << 30)?;
    upload.write(&std::fs::read(archive).unwrap())?;
    client.stage(upload)
}

fn work_entries(root: &Path) -> Vec<String> {
    let mut names: Vec<String> = match std::fs::read_dir(root.join(WORK_DIR)) {
        Ok(entries) => entries
            .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
            .collect(),
        Err(_) => return Vec::new(),
    };
    names.sort();
    names
}

fn refusal(error: &anyhow::Error) -> Option<&Refusal> {
    error.downcast_ref::<Refusal>()
}

/// A full format-2 archive holding a schema 34 database stamped `version`,
/// plus the raw `extra` entries.
fn archive_with(
    directory: &Path,
    version: u64,
    scope: &str,
    extra: &[(&[u8], EntryType)],
) -> PathBuf {
    let database = directory.join(format!("schema-{version}-{}.db", random_id()));
    pp_testkit::node_schema::write_schema_34_database(&database).unwrap();
    pp_testkit::node_schema::stamp_schema_version(&Connection::open(&database).unwrap(), version)
        .unwrap();
    let roots: Vec<_> = if scope == "full" {
        archive::FULL_ROOTS.iter().map(|(name, _)| *name).collect()
    } else {
        Vec::new()
    };
    let metadata = serde_json::to_vec(&serde_json::json!({
        "version": "2", "createdAt": "2026-11-08T00:00:00.000Z", "appVersion": "test",
        "formatVersion": 2, "scope": {"kind": scope, "includedRoots": roots},
    }))
    .unwrap();
    let path = directory.join(format!("crafted-{}.tar.gz", random_id()));
    let mut builder = tar::Builder::new(flate2::write::GzEncoder::new(
        File::create(&path).unwrap(),
        flate2::Compression::default(),
    ));
    let mut append = |name: &[u8], kind: EntryType, data: &[u8]| {
        let mut header = Header::new_old();
        header.as_old_mut().name[..name.len()].copy_from_slice(name);
        header.set_entry_type(kind);
        header.set_mode(0o644);
        header.set_size(data.len() as u64);
        if kind == EntryType::Symlink || kind == EntryType::Link {
            header.set_link_name("/etc/passwd").unwrap();
        }
        header.set_cksum();
        builder.append(&header, data).unwrap();
    };
    append(
        archive::METADATA_FILE.as_bytes(),
        EntryType::Regular,
        &metadata,
    );
    for (name, kind) in extra {
        append(name, *kind, &[]);
    }
    append(
        archive::DATABASE_FILE.as_bytes(),
        EntryType::Regular,
        &std::fs::read(&database).unwrap(),
    );
    drop(append);
    builder.into_inner().unwrap().finish().unwrap();
    path
}

#[test]
fn ticket_t_8_restore_stage_extracts_a_full_backup_and_leaves_live_files_untouched() {
    let (installation, root) = installation();
    let owner = open(&root).unwrap();
    let scratch = tempfile::tempdir().unwrap();
    let (backup, backed_up) = backup_then_change(&owner, &root, scratch.path());
    let live = state(&root);
    let entries = installation.entries().unwrap();
    let client = owner.installation().unwrap();
    let staged = stage(&client, &backup).unwrap();
    assert_eq!(staged.schema_version, schema::CURRENT_VERSION);
    assert_eq!(staged.metadata.scope, archive::MetadataScope::Full);
    assert!(is_id(&staged.restore_id));
    let staged_dir = root
        .join(WORK_DIR)
        .join(format!("{STAGED_PREFIX}{}", staged.restore_id));
    assert_eq!(state(&staged_dir), backed_up);
    assert_eq!(
        work_entries(&root),
        [format!("{STAGED_PREFIX}{}", staged.restore_id)],
        "the upload is removed"
    );
    assert_eq!(state(&root), live);
    let mut with_work = entries.clone();
    with_work.insert(WORK_DIR.into());
    assert_eq!(installation.entries().unwrap(), with_work);
    owner.shutdown().unwrap();
}

#[test]
fn ticket_t_8_restore_a_new_stage_replaces_the_earlier_one_and_a_refused_stage_keeps_it() {
    let (_installation, root) = installation();
    let owner = open(&root).unwrap();
    let scratch = tempfile::tempdir().unwrap();
    let (backup, _) = backup_then_change(&owner, &root, scratch.path());
    let client = owner.installation().unwrap();
    let first = stage(&client, &backup).unwrap();
    let second = stage(&client, &backup).unwrap();
    assert_ne!(first.restore_id, second.restore_id);
    let kept = [format!("{STAGED_PREFIX}{}", second.restore_id)];
    assert_eq!(work_entries(&root), kept);
    let newer = archive_with(scratch.path(), schema::CURRENT_VERSION + 1, "full", &[]);
    stage(&client, &newer).unwrap_err();
    assert_eq!(work_entries(&root), kept);
    owner.shutdown().unwrap();
}

#[test]
fn ticket_t_8_restore_unsupported_schemas_and_scopes_refuse_and_leave_the_original_unchanged() {
    let (installation, root) = installation();
    let owner = open(&root).unwrap();
    let entries = installation.entries().unwrap();
    let before = state(&root);
    let marker = std::fs::read(installation.marker_path()).unwrap();
    let scratch = tempfile::tempdir().unwrap();
    let client = owner.installation().unwrap();
    for (version, scope, expected) in [
        (schema::CURRENT_VERSION + 1, "full", "newer than supported"),
        (30, "full", "Cannot upgrade database schema version 30"),
        (0, "full", "no schema version"),
    ] {
        let archive = archive_with(scratch.path(), version, scope, &[]);
        let error = stage(&client, &archive).unwrap_err();
        assert!(
            matches!(refusal(&error), Some(Refusal::UnsupportedSchema(detail)) if detail.contains(expected)),
            "schema {version}: {error:#}"
        );
    }
    let database_only = archive_with(scratch.path(), 34, "database-only", &[]);
    let error = stage(&client, &database_only).unwrap_err();
    assert!(
        matches!(refusal(&error), Some(Refusal::InvalidArchive(detail)) if detail.contains("full backup")),
        "{error:#}"
    );
    assert_eq!(installation.entries().unwrap(), entries);
    assert_eq!(state(&root), before);
    assert_eq!(std::fs::read(installation.marker_path()).unwrap(), marker);
    owner.shutdown().unwrap();
}

#[test]
fn ticket_t_8_restore_hostile_entries_refuse_without_writing_outside_the_stage() {
    let (installation, root) = installation();
    let owner = open(&root).unwrap();
    let entries = installation.entries().unwrap();
    let before = state(&root);
    let scratch = tempfile::tempdir().unwrap();
    let client = owner.installation().unwrap();
    let hostile: [(&[u8], EntryType); 7] = [
        (b"../escape", EntryType::Regular),
        (b"repos/../../escape", EntryType::Regular),
        (b"/tmp/escape", EntryType::Regular),
        (b"repos/link", EntryType::Symlink),
        (b"repos/hard", EntryType::Link),
        (b".pp-rust-owned.json", EntryType::Regular),
        (b"backups/escape", EntryType::Regular),
    ];
    for (name, kind) in hostile {
        let archive = archive_with(scratch.path(), 34, "full", &[(name, kind)]);
        let error = stage(&client, &archive).unwrap_err();
        assert!(
            matches!(refusal(&error), Some(Refusal::InvalidArchive(_))),
            "{}: {error:#}",
            String::from_utf8_lossy(name)
        );
        assert!(!root.join(WORK_DIR).exists());
        assert!(!root.parent().unwrap().join("escape").exists());
    }
    assert!(!Path::new("/tmp/escape").exists());
    assert_eq!(installation.entries().unwrap(), entries);
    assert_eq!(state(&root), before);
    owner.shutdown().unwrap();
}

#[test]
fn ticket_t_8_restore_unmarked_installations_and_oversized_uploads_refuse_before_writing() {
    let installation = Installation::create(pp_testkit::Fixture::NodeSchema34).unwrap();
    let root = installation.path().canonicalize().unwrap();
    let owner = WriterOwner::open_with_gate(&root, Limits::default(), WriterGate::Unchecked)
        .unwrap()
        .0;
    let client = owner.installation().unwrap();
    let error = client.begin_upload(4).err().unwrap();
    assert_eq!(refusal(&error), Some(&Refusal::Unmarked));
    assert!(!root.join(WORK_DIR).exists());
    installation
        .write_marker(&pp_testkit::Marker::default())
        .unwrap();
    let mut upload = client.begin_upload(4).unwrap();
    upload.write(b"1234").unwrap();
    let error = upload.write(b"5").unwrap_err();
    assert_eq!(refusal(&error), Some(&Refusal::TooLarge(4)));
    assert_eq!(std::fs::metadata(&upload.path).unwrap().len(), 4);
    drop(upload);
    assert!(work_entries(&root).is_empty());
    owner.shutdown().unwrap();
}

#[test]
fn ticket_t_8_restore_a_linked_work_directory_refuses_staging() {
    let (_installation, root) = installation();
    let owner = open(&root).unwrap();
    let outside = tempfile::tempdir().unwrap();
    std::os::unix::fs::symlink(outside.path(), root.join(WORK_DIR)).unwrap();
    let error = owner.installation().unwrap().begin_upload(4).err().unwrap();
    assert!(error.to_string().contains("not a directory"), "{error:#}");
    assert_eq!(std::fs::read_dir(outside.path()).unwrap().count(), 0);
    owner.shutdown().unwrap();
}

#[test]
fn ticket_t_8_restore_reopen_clears_an_interrupted_stage() {
    let (_installation, root) = installation();
    let scratch = tempfile::tempdir().unwrap();
    let owner = open(&root).unwrap();
    let (backup, _) = backup_then_change(&owner, &root, scratch.path());
    let client = owner.installation().unwrap();
    stage(&client, &backup).unwrap();
    let mut upload = client.begin_upload(1 << 20).unwrap();
    upload.write(b"partial").unwrap();
    // A crash leaves the upload and the stage behind.
    std::mem::forget(upload);
    assert_eq!(work_entries(&root).len(), 2);
    let live = state(&root);
    owner.shutdown().unwrap();
    open(&root).unwrap().shutdown().unwrap();
    assert!(!root.join(WORK_DIR).exists());
    assert_eq!(state(&root), live);
}

#[test]
fn ticket_t_8_restore_an_unknown_work_entry_or_linked_work_directory_refuses_open_and_is_kept() {
    let (_installation, root) = installation();
    open(&root).unwrap().shutdown().unwrap();
    let before = state(&root);
    let work = root.join(WORK_DIR);
    for name in [
        "journal.json",
        "prior-00000000000000000000000000000000",
        "staged-x",
    ] {
        std::fs::create_dir(&work).unwrap();
        std::fs::write(work.join(name), b"keep").unwrap();
        let error = open(&root).err().unwrap();
        assert!(error.to_string().contains("unexpected entry"), "{error:#}");
        assert_eq!(std::fs::read(work.join(name)).unwrap(), b"keep");
        assert_eq!(state(&root), before);
        std::fs::remove_dir_all(&work).unwrap();
    }
    let outside = tempfile::tempdir().unwrap();
    std::fs::write(outside.path().join("staged-x"), b"keep").unwrap();
    std::os::unix::fs::symlink(outside.path(), &work).unwrap();
    let error = open(&root).err().unwrap();
    assert!(error.to_string().contains("not a directory"), "{error:#}");
    assert_eq!(
        std::fs::read(outside.path().join("staged-x")).unwrap(),
        b"keep"
    );
    std::fs::remove_file(&work).unwrap();
    open(&root).unwrap().shutdown().unwrap();
}
