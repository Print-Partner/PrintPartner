//! Restore on disposable pp-testkit installations holding a copied Node
//! schema 34 database and a valid ownership marker.

use super::*;
use crate::{Limits, Setting, SettingCommand, ownership_marker::WriterGate};
use pp_testkit::{Installation, MarkerState as KitMarker, Spec};
use std::{
    collections::BTreeMap,
    sync::{Arc, Mutex, atomic::AtomicBool},
    time::Duration,
};

/// A simulated process death: activation returns without rolling back.
#[derive(Debug)]
pub(super) struct Crash;
impl fmt::Display for Crash {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("simulated crash")
    }
}
impl std::error::Error for Crash {}

type Hook = Box<dyn FnOnce() + Send>;
/// (data directory, steps to pass before failing, crash instead of fail)
static STEPS: Mutex<Vec<(PathBuf, usize, bool)>> = Mutex::new(Vec::new());
static DURING: Mutex<Vec<(PathBuf, Hook)>> = Mutex::new(Vec::new());

/// Called before each rename, before the journal commits and after it.
pub(super) fn step(data_dir: &Path) -> Result<()> {
    let mut steps = STEPS.lock().unwrap();
    let Some(index) = steps.iter().position(|(dir, ..)| dir == data_dir) else {
        return Ok(());
    };
    if steps[index].1 > 0 {
        steps[index].1 -= 1;
        return Ok(());
    }
    let (_, _, crash) = steps.remove(index);
    Err(if crash {
        Crash.into()
    } else {
        anyhow!("injected restore failure")
    })
}

/// Called after admission stopped and connections closed, before the swap.
pub(super) fn during_activation(data_dir: &Path) {
    let hook = {
        let mut hooks = DURING.lock().unwrap();
        let index = hooks.iter().position(|(dir, _)| dir == data_dir);
        index.map(|index| hooks.remove(index).1)
    };
    if let Some(hook) = hook {
        hook();
    }
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

fn open(root: &Path) -> WriterOwner {
    WriterOwner::open_with_gate(root, Limits::default(), WriterGate::SwitchOn)
        .unwrap()
        .0
}

fn set(owner: &WriterOwner, value: &str) -> Result<bool> {
    let setting = Setting {
        tenant: "default".into(),
        key: "restore.test".into(),
        value: value.into(),
    };
    owner
        .client()
        .submit(
            SettingCommand::Set(setting),
            &AtomicBool::new(false),
            Duration::from_secs(1),
        )?
        .recv()?
}

/// Every table's rows, sorted, and every file under the backup roots.
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

/// Backs up `owner`, then changes a setting, deletes the Source file and
/// adds an export. Returns the backup and the state it holds.
fn backup_then_change(owner: &WriterOwner, root: &Path) -> (BackupFile, BTreeMap<String, String>) {
    set(owner, "in backup").unwrap();
    let backed_up = state(root);
    let backup = owner
        .installation()
        .unwrap()
        .backup_to_temporary("test")
        .unwrap();
    set(owner, "after backup").unwrap();
    std::fs::remove_file(root.join("repos/source-a/part.stl")).unwrap();
    std::fs::write(root.join("exports/later.3mf"), b"later").unwrap();
    assert_ne!(state(root), backed_up);
    (backup, backed_up)
}

fn stage(client: &InstallationClient, archive: &Path) -> Result<Staged> {
    let mut upload = client.begin_upload(1 << 30)?;
    upload.write(&std::fs::read(archive).unwrap())?;
    client.stage(upload)
}

fn assert_marked(root: &Path) {
    assert!(matches!(
        ownership_marker::inspect(root),
        ownership_marker::MarkerState::Valid(_)
    ));
}

fn refusal(error: &anyhow::Error) -> Option<&Refusal> {
    error.downcast_ref::<Refusal>()
}

#[test]
fn restore_runs_with_admission_stopped_and_round_trips_ids_drafts_history_and_files() {
    let (_installation, root) = installation();
    let owner = open(&root);
    let (backup, backed_up) = backup_then_change(&owner, &root);
    let settings = owner.client();
    let observed = Arc::new(Mutex::new(Vec::new()));
    let seen = observed.clone();
    DURING.lock().unwrap().push((
        root.clone(),
        Box::new(move || {
            let setting = Setting {
                tenant: "default".into(),
                key: "restore.test".into(),
                value: "during".into(),
            };
            let write = settings.submit(
                SettingCommand::Set(setting),
                &AtomicBool::new(false),
                Duration::ZERO,
            );
            let read = settings.reader(Duration::ZERO);
            let readers = settings.readers.state.lock().unwrap();
            seen.lock().unwrap().extend([
                format!(
                    "write: {}",
                    write.err().map(|e| e.to_string()).unwrap_or_default()
                ),
                format!(
                    "read: {}",
                    read.err().map(|e| e.to_string()).unwrap_or_default()
                ),
                format!(
                    "readers: active {} idle {}",
                    readers.active,
                    readers.idle.len()
                ),
                format!(
                    "queue closed: {}",
                    settings.shared.queue.lock().unwrap().closed
                ),
            ]);
        }),
    ));
    let client = owner.installation().unwrap();
    let staged = stage(&client, &backup.path).unwrap();
    assert_eq!(staged.schema_version, schema::CURRENT_VERSION);
    let activated = client.activate(&staged.restore_id, CONFIRMATION).unwrap();
    assert_eq!(activated.schema_version, schema::CURRENT_VERSION);
    assert_eq!(
        *observed.lock().unwrap(),
        [
            "write: Storage stopped",
            "read: Storage stopped",
            "readers: active 0 idle 0",
            "queue closed: true",
        ]
    );
    let restored = state(&root);
    assert_eq!(restored, backed_up);
    for (table, row) in [
        (
            "build_profiles",
            "Integer(7), Text(\"default\"), Text(\"Seven\")",
        ),
        (
            "build_profiles",
            "Integer(29), Text(\"default\"), Text(\"Twenty-nine\")",
        ),
        (
            "plan_snapshots",
            "Integer(3), Text(\"default\"), Integer(29)",
        ),
        ("plan_drafts", "Integer(41), Text(\"default\"), Integer(29)"),
    ] {
        assert!(restored[&format!("table {table}")].contains(row), "{row}");
    }
    assert_marked(&root);
    assert_eq!(std::fs::read_dir(root.join(WORK_DIR)).unwrap().count(), 0);
    // The same owner keeps serving writes and reads, and reopens.
    assert!(set(&owner, "after restore").unwrap());
    owner.client().reader(Duration::from_secs(1)).unwrap();
    owner.shutdown().unwrap();
    open(&root).shutdown().unwrap();
}

#[test]
fn an_open_reader_refuses_activation_and_keeps_the_installation() {
    let (_installation, root) = installation();
    let owner = open(&root);
    let (backup, _) = backup_then_change(&owner, &root);
    let before = state(&root);
    let client = owner.installation().unwrap();
    let staged = stage(&client, &backup.path).unwrap();
    let held = owner.client().reader(Duration::from_secs(1)).unwrap();
    let error = client
        .activate(&staged.restore_id, CONFIRMATION)
        .unwrap_err();
    assert_eq!(refusal(&error), Some(&Refusal::Busy));
    drop(held);
    assert_eq!(state(&root), before);
    assert!(set(&owner, "still writable").unwrap());
    owner.shutdown().unwrap();
}

/// Fails, then crashes, at each rename and around the journal commit. A
/// failure rolls back in place; a crash is recovered by the next open.
#[test]
fn failure_or_crash_at_every_step_leaves_the_old_or_the_new_installation() {
    let (_installation, root) = installation();
    let (backup, backed_up) = backup_then_change(&open(&root), &root);
    let old = state(&root);
    let mut running = None;
    for at in 0.. {
        for crash in [false, true] {
            let owner = running.take().unwrap_or_else(|| open(&root));
            let client = owner.installation().unwrap();
            let staged = stage(&client, &backup.path).unwrap();
            STEPS.lock().unwrap().push((root.clone(), at, crash));
            let result = client.activate(&staged.restore_id, CONFIRMATION);
            let pending = STEPS.lock().unwrap().iter().any(|(dir, ..)| dir == &root);
            assert!(!pending, "step {at} was never reached");
            let error = result.unwrap_err();
            if !crash {
                assert!(format!("{error:#}").contains("injected restore failure"));
                assert_eq!(state(&root), old, "failure at step {at}");
                assert!(set(&owner, "after backup").unwrap());
                running = Some(owner);
                continue;
            }
            assert!(error.is::<Crash>());
            let committed = Journal::read(&root)
                .unwrap()
                .is_some_and(|journal| journal.state == JournalState::Committed);
            owner.shutdown().unwrap();
            open(&root).shutdown().unwrap();
            assert!(!root.join(WORK_DIR).exists());
            assert_marked(&root);
            if committed {
                assert_eq!(state(&root), backed_up, "crash after the commit");
                assert!(at >= 6, "{at} steps before the commit");
                return;
            }
            assert_eq!(state(&root), old, "crash at step {at}");
        }
    }
}

/// A full format-2 archive holding a schema 34 database stamped `version`.
fn archive_with_schema(directory: &Path, version: u64) -> PathBuf {
    let database = directory.join(archive::DATABASE_FILE);
    pp_testkit::node_schema::write_schema_34_database(&database).unwrap();
    pp_testkit::node_schema::stamp_schema_version(&Connection::open(&database).unwrap(), version)
        .unwrap();
    let roots: Vec<_> = archive::FULL_ROOTS.iter().map(|(name, _)| *name).collect();
    let metadata = serde_json::to_vec(&serde_json::json!({
        "version": "2", "createdAt": "2026-11-04T00:00:00.000Z", "appVersion": "future",
        "formatVersion": 2, "scope": {"kind": "full", "includedRoots": roots},
    }))
    .unwrap();
    let path = directory.join("future.tar.gz");
    let mut builder = tar::Builder::new(flate2::write::GzEncoder::new(
        File::create(&path).unwrap(),
        flate2::Compression::default(),
    ));
    builder
        .append_path_with_name(&database, archive::DATABASE_FILE)
        .unwrap();
    let mut header = tar::Header::new_gnu();
    header.set_size(metadata.len() as u64);
    header.set_mode(0o644);
    header.set_cksum();
    builder
        .append_data(&mut header, archive::METADATA_FILE, metadata.as_slice())
        .unwrap();
    builder.into_inner().unwrap().finish().unwrap();
    path
}

#[test]
fn newer_schema_import_leaves_the_original_unchanged() {
    let (installation, root) = installation();
    let owner = open(&root);
    let entries = installation.entries().unwrap();
    let before = state(&root);
    let marker = std::fs::read(installation.marker_path()).unwrap();
    let scratch = tempfile::tempdir().unwrap();
    let client = owner.installation().unwrap();
    let newer = archive_with_schema(scratch.path(), schema::CURRENT_VERSION + 1);
    let error = stage(&client, &newer).unwrap_err();
    assert!(
        matches!(refusal(&error), Some(Refusal::UnsupportedSchema(detail)) if detail.contains("newer than supported")),
        "{error:#}"
    );
    assert_eq!(installation.entries().unwrap(), entries);
    assert_eq!(state(&root), before);
    assert_eq!(std::fs::read(installation.marker_path()).unwrap(), marker);
    let error = client.activate(&"0".repeat(32), CONFIRMATION).unwrap_err();
    assert_eq!(refusal(&error), Some(&Refusal::UnknownRestore));
    let error = client.activate(&"0".repeat(32), "yes").unwrap_err();
    assert_eq!(refusal(&error), Some(&Refusal::Confirmation));
    let error = client.activate("../../etc", CONFIRMATION).unwrap_err();
    assert_eq!(refusal(&error), Some(&Refusal::UnknownRestore));
    owner.shutdown().unwrap();
}

#[test]
fn unmarked_installations_refuse_restore_before_writing() {
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
    let error = client.begin_upload(4).unwrap().write(b"12345").unwrap_err();
    assert_eq!(refusal(&error), Some(&Refusal::TooLarge(4)));
    owner.shutdown().unwrap();
}

#[test]
fn a_corrupt_journal_refuses_open_without_moving_files() {
    let (_installation, root) = installation();
    open(&root).shutdown().unwrap();
    let before = state(&root);
    std::fs::create_dir(root.join(WORK_DIR)).unwrap();
    std::fs::write(
        root.join(WORK_DIR).join(JOURNAL),
        br#"{"version":1,"state":"swapping","staged":"staged-x","prior":"../..","live":[],"incoming":[]}"#,
    )
    .unwrap();
    let error = WriterOwner::open_with_gate(&root, Limits::default(), WriterGate::SwitchOn)
        .err()
        .unwrap();
    assert!(error.to_string().contains("Restore journal"), "{error:#}");
    assert_eq!(state(&root), before);
}
