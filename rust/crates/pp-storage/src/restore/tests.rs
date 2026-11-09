//! Restore staging on disposable pp-testkit installations holding a copied
//! Node schema 34 database and a valid ownership marker.

use super::*;
use crate::{Limits, Setting, SettingCommand, ownership_marker::WriterGate};
use pp_testkit::{Installation, MarkerState as KitMarker, Spec};
use rusqlite::{Connection, OpenFlags};
use std::{collections::BTreeMap, sync::mpsc, thread};
use std::{sync::atomic::AtomicBool, time::Duration};
use tar::{EntryType, Header};

type Hook = Box<dyn FnOnce() + Send>;
static AFTER_WORK_DIR: Mutex<Vec<(PathBuf, Hook)>> = Mutex::new(Vec::new());

/// Takes the hook registered for `data_dir`, if any.
fn take_hook<T>(hooks: &Mutex<Vec<(PathBuf, T)>>, data_dir: &Path) -> Option<T> {
    let mut hooks = hooks.lock().unwrap();
    let index = hooks.iter().position(|(dir, _)| dir == data_dir)?;
    Some(hooks.remove(index).1)
}

/// Called by `begin_upload` between creating `.pp-restore` and its file.
pub(super) fn after_work_dir(data_dir: &Path) {
    if let Some(hook) = take_hook(&AFTER_WORK_DIR, data_dir) {
        hook();
    }
}

/// Stage directories whose next removal fails, whoever runs the test.
static FAILING_REMOVALS: Mutex<Vec<PathBuf>> = Mutex::new(Vec::new());

/// Called by `stage` before it removes an earlier stage directory.
pub(super) fn removal_fault(path: &Path) -> std::io::Result<()> {
    let mut failing = FAILING_REMOVALS.lock().unwrap();
    match failing.iter().position(|known| known == path) {
        Some(index) => {
            failing.remove(index);
            Err(std::io::Error::other("injected removal failure"))
        }
        None => Ok(()),
    }
}

type DeadlineHook = Box<dyn FnOnce(Instant) + Send>;
static ADMISSION_CLOSED: Mutex<Vec<(PathBuf, DeadlineHook)>> = Mutex::new(Vec::new());

/// Called by shutdown once admission is closed, with the drain deadline.
pub(super) fn admission_closed(data_dir: &Path, deadline: Instant) {
    if let Some(hook) = take_hook(&ADMISSION_CLOSED, data_dir) {
        hook(deadline);
    }
}

fn on_work_dir(root: &Path, hook: impl FnOnce() + Send + 'static) {
    AFTER_WORK_DIR
        .lock()
        .unwrap()
        .push((root.to_owned(), Box::new(hook)));
}

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

/// Every table's rows, sorted, and every entry under the backup roots of
/// `root`, which holds a database and roots laid out like a data directory.
fn state(root: &Path) -> BTreeMap<String, String> {
    let db = Connection::open_with_flags(
        root.join(archive::DATABASE_FILE),
        OpenFlags::SQLITE_OPEN_READ_ONLY,
    )
    .unwrap();
    let mut state = BTreeMap::new();
    let mut names = db
        .prepare("SELECT name FROM sqlite_schema WHERE type = 'table'")
        .unwrap();
    let mut tables = names.query([]).unwrap();
    while let Some(table) = tables.next().unwrap() {
        let name: String = table.get(0).unwrap();
        let mut select = db.prepare(&format!("SELECT * FROM \"{name}\"")).unwrap();
        let width = select.column_count();
        let mut found = Vec::new();
        let mut rows = select.query([]).unwrap();
        while let Some(row) = rows.next().unwrap() {
            let values: Vec<_> = (0..width).map(|i| row.get_ref(i).unwrap()).collect();
            found.push(format!("{values:?}"));
        }
        found.sort();
        state.insert(format!("table {name}"), found.join("\n"));
    }
    for (name, _) in archive::FULL_ROOTS {
        let path = root.join(name);
        if path.symlink_metadata().is_err() {
            continue;
        }
        let entries = if path.is_dir() {
            pp_testkit::snapshot(&path).unwrap()
        } else {
            [(PathBuf::new(), Some(std::fs::read(&path).unwrap()))].into()
        };
        state.insert(format!("root {name}"), format!("{entries:?}"));
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
        header.set_size(data.len() as u64);
        if matches!(kind, EntryType::Symlink | EntryType::Link) {
            header.set_link_name("/etc/passwd").unwrap();
        }
        header.set_entry_type(kind);
        header.as_old_mut().name[..name.len()].copy_from_slice(name);
        header.set_mode(0o644);
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
    // A name this build does not create is never removed by a new stage.
    std::fs::create_dir(root.join(WORK_DIR).join("staged-other")).unwrap();
    let third = stage(&client, &backup).unwrap();
    let kept = [
        format!("{STAGED_PREFIX}{}", third.restore_id),
        "staged-other".to_owned(),
    ];
    assert_eq!(work_entries(&root), kept);
    let newer = archive_with(scratch.path(), schema::CURRENT_VERSION + 1, "full", &[]);
    stage(&client, &newer).unwrap_err();
    assert_eq!(work_entries(&root), kept);
    owner.shutdown().unwrap();
}

/// Unsupported schemas and scopes, and entries that would leave the stage,
/// are refused with the live installation, marker and parent untouched.
#[test]
fn ticket_t_8_restore_refused_archives_leave_the_original_unchanged() {
    let (installation, root) = installation();
    let owner = open(&root).unwrap();
    let entries = installation.entries().unwrap();
    let before = state(&root);
    let marker = std::fs::read(installation.marker_path()).unwrap();
    let scratch = tempfile::tempdir().unwrap();
    let client = owner.installation().unwrap();
    let refuse = |version: u64, scope: &str, extra: &[(&[u8], EntryType)]| {
        let archive = archive_with(scratch.path(), version, scope, extra);
        let error = stage(&client, &archive).unwrap_err();
        assert!(work_entries(&root).is_empty());
        assert!(!root.parent().unwrap().join("escape").exists());
        refusal(&error)
            .cloned()
            .unwrap_or_else(|| panic!("{error:#}"))
    };
    for (version, expected) in [
        (schema::CURRENT_VERSION + 1, "newer than supported"),
        (30, "Cannot upgrade database schema version 30"),
        (0, "no schema version"),
    ] {
        let refused = refuse(version, "full", &[]);
        assert!(
            matches!(&refused, Refusal::UnsupportedSchema(detail) if detail.contains(expected)),
            "schema {version}: {refused}"
        );
    }
    let refused = refuse(34, "database-only", &[]);
    assert!(
        matches!(&refused, Refusal::InvalidArchive(detail) if detail.contains("full backup")),
        "{refused}"
    );
    for (name, kind) in [
        (&b"../escape"[..], EntryType::Regular),
        (b"repos/../../escape", EntryType::Regular),
        (b"/tmp/escape", EntryType::Regular),
        (b"repos/link", EntryType::Symlink),
        (b"repos/hard", EntryType::Link),
        (b".pp-rust-owned.json", EntryType::Regular),
        (b"backups/escape", EntryType::Regular),
    ] {
        let refused = refuse(34, "full", &[(name, kind)]);
        assert!(
            matches!(refused, Refusal::InvalidArchive(_)),
            "{}: {refused}",
            String::from_utf8_lossy(name)
        );
    }
    assert!(!Path::new("/tmp/escape").exists());
    // `.pp-restore` stays, empty, until the next open.
    let mut with_work = entries;
    with_work.insert(WORK_DIR.into());
    assert_eq!(installation.entries().unwrap(), with_work);
    assert_eq!(state(&root), before);
    assert_eq!(std::fs::read(installation.marker_path()).unwrap(), marker);
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
    // A failed upload stays failed, even after a write that would fit.
    let error = upload.write(b"").unwrap_err();
    assert!(error.to_string().contains("already failed"), "{error:#}");
    let error = client.stage(upload).unwrap_err();
    assert!(
        matches!(refusal(&error), Some(Refusal::InvalidArchive(detail)) if detail.contains("did not complete")),
        "{error:#}"
    );
    assert!(work_entries(&root).is_empty());
    assert_eq!(
        client.begin_upload(u64::MAX).unwrap().limit,
        MAX_UPLOAD_BYTES
    );
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

/// Stages are ephemeral until activation: the next open removes a completed
/// stage as well as an interrupted upload, and leaves live files alone.
#[test]
fn ticket_t_8_restore_reopen_removes_completed_stages_and_interrupted_uploads() {
    let (_installation, root) = installation();
    let scratch = tempfile::tempdir().unwrap();
    let owner = open(&root).unwrap();
    let (backup, _) = backup_then_change(&owner, &root, scratch.path());
    let client = owner.installation().unwrap();
    let staged = stage(&client, &backup).unwrap();
    let mut upload = client.begin_upload(1 << 20).unwrap();
    upload.write(b"partial").unwrap();
    // A crash leaves the upload behind.
    std::mem::forget(upload);
    assert_eq!(work_entries(&root).len(), 2);
    let live = state(&root);
    owner.shutdown().unwrap();
    let staged_dir = root
        .join(WORK_DIR)
        .join(format!("{STAGED_PREFIX}{}", staged.restore_id));
    assert!(staged_dir.is_dir(), "shutdown itself leaves the stage");
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

#[test]
fn ticket_t_8_restore_a_wrong_type_or_link_under_a_leftover_name_refuses_open() {
    let (_installation, root) = installation();
    open(&root).unwrap().shutdown().unwrap();
    let work = root.join(WORK_DIR);
    let id = random_id();
    let incoming = format!("{INCOMING_PREFIX}{id}{INCOMING_SUFFIX}");
    let staged = format!("{STAGED_PREFIX}{id}");
    let outside = tempfile::tempdir().unwrap();
    std::fs::write(outside.path().join("keep"), b"keep").unwrap();
    for (name, shape) in [(&incoming, "dir"), (&staged, "file"), (&staged, "link")] {
        std::fs::create_dir(&work).unwrap();
        // A valid leftover beside it is not removed either.
        std::fs::create_dir(work.join(format!("{STAGED_PREFIX}{}", random_id()))).unwrap();
        let path = work.join(name);
        match shape {
            "dir" => {
                std::fs::create_dir(&path).unwrap();
                std::fs::write(path.join("keep"), b"keep").unwrap();
            }
            "file" => std::fs::write(&path, b"keep").unwrap(),
            _ => std::os::unix::fs::symlink(outside.path(), &path).unwrap(),
        }
        let error = open(&root).err().unwrap();
        assert!(
            error.to_string().contains("wrong type"),
            "{name}: {error:#}"
        );
        assert_eq!(work_entries(&root).len(), 2, "{name}");
        std::fs::remove_dir_all(&work).unwrap();
    }
    assert_eq!(std::fs::read(outside.path().join("keep")).unwrap(), b"keep");
    open(&root).unwrap().shutdown().unwrap();
}

/// A refused stage between `begin_upload` creating `.pp-restore` and its
/// file never removes the directory under the upload.
#[test]
fn ticket_t_8_restore_an_upload_survives_a_refused_stage_racing_it() {
    let (_installation, root) = installation();
    let owner = open(&root).unwrap();
    let scratch = tempfile::tempdir().unwrap();
    let newer = archive_with(scratch.path(), schema::CURRENT_VERSION + 1, "full", &[]);
    let client = owner.installation().unwrap();
    let racing = client.clone();
    on_work_dir(&root, move || {
        stage(&racing, &newer).unwrap_err();
    });
    let mut upload = client.begin_upload(16).unwrap();
    upload.write(b"still here").unwrap();
    assert_eq!(std::fs::read(&upload.path).unwrap(), b"still here");
    drop(upload);
    owner.shutdown().unwrap();
}

#[test]
fn ticket_t_8_restore_a_failed_cleanup_of_an_earlier_stage_still_returns_the_new_one() {
    let (_installation, root) = installation();
    let owner = open(&root).unwrap();
    let client = owner.installation().unwrap();
    let scratch = tempfile::tempdir().unwrap();
    let (backup, backed_up) = backup_then_change(&owner, &root, scratch.path());
    let first = stage(&client, &backup).unwrap();
    let first_dir = root
        .join(WORK_DIR)
        .join(format!("{STAGED_PREFIX}{}", first.restore_id));
    FAILING_REMOVALS.lock().unwrap().push(first_dir.clone());
    let second = stage(&client, &backup).unwrap();
    let detail = second
        .cleanup_error
        .expect("the earlier stage could not be removed");
    assert!(detail.contains(&first.restore_id), "{detail}");
    let second_dir = root
        .join(WORK_DIR)
        .join(format!("{STAGED_PREFIX}{}", second.restore_id));
    assert_eq!(state(&second_dir), backed_up);
    assert!(first_dir.exists());
    owner.shutdown().unwrap();
    // The next open removes both.
    open(&root).unwrap().shutdown().unwrap();
    assert!(!root.join(WORK_DIR).exists());
}

/// A marked installation with an owner whose `begin_upload` is held inside
/// its work, on another thread, until the returned sender fires.
struct Held {
    _installation: Installation,
    root: PathBuf,
    owner: WriterOwner,
    client: InstallationClient,
    release: mpsc::Sender<()>,
    upload: thread::JoinHandle<Result<Upload>>,
}

fn held_upload() -> Held {
    let (installation, root) = installation();
    let owner = open(&root).unwrap();
    let client = owner.installation().unwrap();
    let (inside, entered) = mpsc::channel();
    let (release, released) = mpsc::channel::<()>();
    on_work_dir(&root, move || {
        inside.send(()).unwrap();
        released.recv().unwrap();
    });
    let uploading = client.clone();
    let upload = thread::spawn(move || uploading.begin_upload(16));
    entered.recv().unwrap();
    Held {
        _installation: installation,
        root,
        owner,
        client,
        release,
        upload,
    }
}

#[test]
fn ticket_t_8_restore_shutdown_waits_for_restore_work_and_refuses_new_work() {
    let Held {
        _installation,
        root,
        owner,
        client,
        release,
        upload,
    } = held_upload();
    let shutdown = thread::spawn(move || owner.shutdown());
    thread::sleep(Duration::from_millis(500));
    assert!(!shutdown.is_finished(), "shutdown waits for the upload");
    release.send(()).unwrap();
    let mut upload = upload.join().unwrap().unwrap();
    shutdown.join().unwrap().unwrap();
    let error = upload.write(b"late").unwrap_err();
    assert!(error.to_string().contains("closed"), "{error:#}");
    assert!(
        client
            .begin_upload(16)
            .err()
            .unwrap()
            .to_string()
            .contains("closed")
    );
    assert!(
        client
            .stage(upload)
            .unwrap_err()
            .to_string()
            .contains("closed")
    );
    open(&root).unwrap().shutdown().unwrap();
}

#[test]
fn ticket_t_8_restore_shutdown_keeps_the_lease_while_restore_work_runs_on() {
    let held = held_upload();
    let (root, release, upload) = (held.root, held.release, held.upload);
    let error = held.owner.shutdown().unwrap_err();
    assert!(
        error.to_string().contains("Restore work still active"),
        "{error:#}"
    );
    // The lease is kept, so no other owner can clear the work in flight.
    assert!(open(&root).is_err());
    release.send(()).unwrap();
    let upload = upload.join().unwrap().unwrap();
    assert!(upload.path.exists());
}

/// Shutdown closes writer, reader and restore admission before it waits,
/// then waits for reader checkouts and restore work against one deadline.
#[test]
fn ticket_t_8_restore_shutdown_refuses_new_work_at_once_and_drains_against_one_deadline() {
    let mut held = held_upload();
    let drain = Duration::from_millis(300);
    held.owner.set_drain(drain);
    let settings = held.owner.client();
    let checkout = settings.reader(Duration::from_secs(1)).unwrap();
    let (closed, admission) = mpsc::channel();
    let (resume, resumed) = mpsc::channel::<()>();
    ADMISSION_CLOSED.lock().unwrap().push((
        held.root.clone(),
        Box::new(move |deadline| {
            closed.send((Instant::now(), deadline)).unwrap();
            resumed.recv().unwrap();
        }),
    ));
    let owner = held.owner;
    let shutdown = thread::spawn(move || owner.shutdown());
    // Shutdown is held right after it closed admission, before any wait.
    let (closed_at, deadline) = admission.recv().unwrap();
    let setting = Setting {
        tenant: "default".into(),
        key: "restore.test".into(),
        value: "during shutdown".into(),
    };
    let write = settings.submit(
        SettingCommand::Set(setting),
        &AtomicBool::new(false),
        Duration::ZERO,
    );
    assert!(
        write.is_err(),
        "a write during the drain is refused at once"
    );
    assert!(settings.reader(Duration::ZERO).is_err());
    assert!(held.client.begin_upload(16).is_err());
    // One deadline, taken once admission closed and no later than `drain`
    // after it. Both the reader and the restore wait use it.
    assert!(deadline <= closed_at + drain, "{deadline:?} {closed_at:?}");
    resume.send(()).unwrap();
    let error = shutdown.join().unwrap().unwrap_err();
    assert!(error.to_string().contains("still active"), "{error:#}");
    drop(checkout);
    held.release.send(()).unwrap();
    held.upload.join().unwrap().unwrap();
}

/// Tar iteration stops at the end-of-archive block, so only reading the gzip
/// stream to its end catches a damaged CRC or length trailer.
#[test]
fn ticket_t_8_restore_a_corrupt_gzip_trailer_refuses_and_keeps_the_earlier_stage() {
    let scratch = tempfile::tempdir().unwrap();
    let good = archive_with(scratch.path(), 34, "full", &[]);
    let (_installation, root) = installation();
    let owner = open(&root).unwrap();
    let client = owner.installation().unwrap();
    let earlier = stage(&client, &good).unwrap();
    for trailer_byte in [8, 1] {
        // The trailer is the CRC-32 (8 to 5 bytes from the end), then the length.
        let mut bytes = std::fs::read(&good).unwrap();
        let at = bytes.len() - trailer_byte;
        bytes[at] ^= 0xff;
        let corrupt = scratch
            .path()
            .join(format!("corrupt-{trailer_byte}.tar.gz"));
        std::fs::write(&corrupt, bytes).unwrap();
        assert!(archive::validate(&corrupt, archive::Limits::default()).is_err());
        let error = stage(&client, &corrupt).unwrap_err();
        assert!(
            matches!(refusal(&error), Some(Refusal::InvalidArchive(detail)) if detail.contains("gzip")),
            "byte {trailer_byte} from the end: {error:#}"
        );
        assert_eq!(
            work_entries(&root),
            [format!("{STAGED_PREFIX}{}", earlier.restore_id)]
        );
    }
    owner.shutdown().unwrap();
}
