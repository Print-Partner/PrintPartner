use super::*;
use crate::restore::journal::{Journal, JournalState};
use crate::restore::{CONFIRMATION, RestoreError};
use std::sync::Arc;
fn assert_marked(root: &Path) {
    assert!(matches!(
        ownership_marker::inspect(root),
        ownership_marker::MarkerState::Valid(_)
    ));
}
/// A backup copied out of the data folder. Opening storage removes the
/// export folders under `backups/`, so a test that reopens keeps its own copy.
fn keep_backup(owner: &WriterOwner) -> tempfile::NamedTempFile {
    let export = owner.installation().unwrap().export_backup("test").unwrap();
    let kept = tempfile::NamedTempFile::new().unwrap();
    std::fs::copy(&export.path, kept.path()).unwrap();
    kept
}

fn backup_then_modify(
    owner: &WriterOwner,
    root: &Path,
) -> (tempfile::NamedTempFile, BTreeMap<String, String>) {
    let backed_up = state(root);
    let backup = keep_backup(owner);
    std::fs::remove_file(root.join("repos/source-a/part.stl")).unwrap();
    std::fs::write(root.join("exports/later.3mf"), b"later").unwrap();
    (backup, backed_up)
}

#[test]
fn restore_runs_with_admission_stopped_and_round_trips_ids_drafts_history_and_files() {
    let (_installation, root) = installation();
    let owner = open(&root).unwrap();
    let (backup, backed_up) = backup_then_modify(&owner, &root);
    let settings = owner.client();
    let observed = Arc::new(Mutex::new(Vec::new()));
    let seen = observed.clone();
    let (reply, queued_during) = mpsc::channel();
    DURING.lock().unwrap().push((
        root.clone(),
        Box::new(move || {
            // Most families queue work themselves and check only `closed`.
            let control = crate::storage_control::queued(crate::WriterControl::PageCount, reply);
            settings
                .shared
                .queue
                .lock()
                .unwrap()
                .pending
                .push_back(control);
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
                    settings.shared.queue.lock().unwrap().restoring
                ),
            ]);
        }),
    ));
    let client = owner.installation().unwrap();
    let staged = stage(&client, backup.path()).unwrap();
    assert_eq!(staged.schema_version, schema::CURRENT_VERSION);
    let archive = std::fs::read(backup.path()).unwrap();
    let activated = client.activate(&staged.restore_id, CONFIRMATION).unwrap();
    assert_eq!(activated.schema_version, schema::CURRENT_VERSION);
    assert!(
        queued_during.recv().is_err(),
        "work queued during the restore ran"
    );
    assert_eq!(
        *observed.lock().unwrap(),
        [
            "write: Storage stopped",
            "read: Storage stopped",
            "readers: active 0 idle 0",
            "queue closed: true",
        ]
    );
    assert_eq!(std::fs::read(backup.path()).unwrap(), archive);
    let restored = state(&root);
    assert_eq!(restored, backed_up);
    assert_marked(&root);
    assert_eq!(std::fs::read_dir(root.join(WORK_DIR)).unwrap().count(), 0);
    // The same owner keeps serving writes and reads, and reopens.

    owner.client().reader(Duration::from_secs(1)).unwrap();
    owner.shutdown().unwrap();
    open(&root).unwrap().shutdown().unwrap();
}

#[test]
fn an_open_reader_refuses_activation_and_keeps_the_installation() {
    let (_installation, root) = installation();
    let owner = open(&root).unwrap();
    let backup = keep_backup(&owner);
    let before = state(&root);
    let client = owner.installation().unwrap();
    let staged = stage(&client, backup.path()).unwrap();
    let held = owner.client().reader(Duration::from_secs(1)).unwrap();
    let database = pp_testkit::node_schema::database_files(&root);
    let archive = std::fs::read(backup.path()).unwrap();
    let settings = owner.client();
    let queued = thread::spawn(move || {
        let (reply, result) = mpsc::channel();
        let control = crate::storage_control::queued(crate::WriterControl::PageCount, reply);
        let waiting = Instant::now();
        loop {
            let mut queue = settings.shared.queue.lock().unwrap();
            if queue.restoring {
                break queue.pending.push_back(control);
            }
            assert!(waiting.elapsed() < Duration::from_secs(30));
            drop(queue);
            thread::yield_now();
        }
        result.recv()
    });
    let error = client
        .activate(&staged.restore_id, CONFIRMATION)
        .unwrap_err();
    assert!(matches!(error, RestoreError::Refused(Refusal::Busy)));
    let pages = queued.join().unwrap();
    assert!(
        pages.is_ok_and(|pages| pages.is_ok()),
        "refusal failed queued work"
    );
    assert!(pp_testkit::node_schema::database_files(&root) == database);
    assert_eq!(std::fs::read(backup.path()).unwrap(), archive);
    drop(held);
    assert_eq!(state(&root), before);

    owner.shutdown().unwrap();
}

/// Fails, then crashes, at each rename and around the journal commit. A
/// failure rolls back in place; a crash is recovered by the next open.
#[test]
fn failure_or_crash_at_every_step_leaves_the_old_or_the_new_installation() {
    let (_installation, root) = installation();
    let owner = open(&root).unwrap();
    let (backup, backed_up) = backup_then_modify(&owner, &root);
    owner.shutdown().unwrap();
    let old = state(&root);
    let mut running = None;
    for at in 0.. {
        for crash in [false, true] {
            let owner = running.take().unwrap_or_else(|| open(&root).unwrap());
            let client = owner.installation().unwrap();
            let staged = stage(&client, backup.path()).unwrap();
            let quiesced = Arc::new(Mutex::new(None));
            let before = quiesced.clone();
            let directory = root.clone();
            DURING.lock().unwrap().push((
                root.clone(),
                Box::new(move || {
                    *before.lock().unwrap() =
                        Some(pp_testkit::node_schema::database_files(&directory));
                }),
            ));
            let directory = root.clone();
            AFTER_ROLLBACK.lock().unwrap().push((
                root.clone(),
                Box::new(move || {
                    assert!(
                        Some(pp_testkit::node_schema::database_files(&directory))
                            == *quiesced.lock().unwrap(),
                        "rollback changed quiesced SQLite bytes"
                    );
                }),
            ));
            let archive = std::fs::read(backup.path()).unwrap();
            STEPS.lock().unwrap().push((root.clone(), at, crash));
            let result = client.activate(&staged.restore_id, CONFIRMATION);
            assert_eq!(std::fs::read(backup.path()).unwrap(), archive);
            println!("step {at}, crash={crash}: archive bytes unchanged");
            let pending = STEPS.lock().unwrap().iter().any(|(dir, ..)| dir == &root);
            if pending {
                take_hook(&AFTER_ROLLBACK, &root);
                STEPS.lock().unwrap().retain(|(dir, ..)| dir != &root);
                result.unwrap();
                owner.shutdown().unwrap();
                assert_eq!(state(&root), backed_up);
                println!("exercised {at} failure and crash boundaries");
                return;
            }
            let error = result.unwrap_err();
            if !crash {
                assert!(matches!(&error, RestoreError::RolledBack));
                assert_eq!(state(&root), old, "failure at step {at}");

                running = Some(owner);
                continue;
            }
            assert!(matches!(&error, RestoreError::RestartRequired));
            assert!(owner.client().shared.queue.lock().unwrap().closed);
            let committed = Journal::read(&root)
                .unwrap()
                .is_some_and(|journal| journal.state == JournalState::Committed);
            owner.shutdown().unwrap();
            open(&root).unwrap().shutdown().unwrap();
            take_hook(&AFTER_ROLLBACK, &root);
            assert!(!root.join(WORK_DIR).exists());
            assert_marked(&root);
            if committed {
                take_hook(&AFTER_ROLLBACK, &root);
                assert_eq!(state(&root), backed_up, "crash after the commit");
                assert!(at >= 6, "{at} steps before the commit");
                std::fs::remove_file(root.join("repos/source-a/part.stl")).unwrap();
                std::fs::write(root.join("exports/later.3mf"), b"later").unwrap();
            }
            assert_eq!(state(&root), old, "crash at step {at}");
        }
    }
}

#[test]
fn refused_restore_operations_preserve_database_and_archive() {
    let (_installation, root) = installation();
    let owner = open(&root).unwrap();
    let client = owner.installation().unwrap();
    let backup = keep_backup(&owner);
    let staged = stage(&client, backup.path()).unwrap();
    let database = pp_testkit::node_schema::database_files(&root);
    let archive = std::fs::read(backup.path()).unwrap();
    let recovery = pp_testkit::snapshot(&root.join(WORK_DIR)).unwrap();
    for (id, confirmation, expected) in [
        (staged.restore_id.as_str(), "yes", Refusal::Confirmation),
        ("../../etc", CONFIRMATION, Refusal::UnknownRestore),
        (
            "00000000000000000000000000000000",
            CONFIRMATION,
            Refusal::UnknownRestore,
        ),
    ] {
        assert!(matches!(client.activate(id, confirmation),
            Err(RestoreError::Refused(refused)) if refused == expected));
        assert!(pp_testkit::node_schema::database_files(&root) == database);
        assert_eq!(std::fs::read(backup.path()).unwrap(), archive);
        assert_eq!(
            pp_testkit::snapshot(&root.join(WORK_DIR)).unwrap(),
            recovery
        );
    }
    owner.shutdown().unwrap();
    let database = pp_testkit::node_schema::database_files(&root);
    let db = archive::DATABASE_FILE;
    let work = root.join(WORK_DIR);
    for (live, incoming) in [
        (vec![], vec![db]),
        (vec![db], vec![]),
        (vec![db, db], vec![db]),
        (vec![db], vec![db, db]),
    ] {
        std::fs::write(work.join("journal.json"), serde_json::to_vec(&serde_json::json!({
            "version": 1, "state": "swapping", "staged": format!("staged-{}", staged.restore_id),
            "prior": "prior-00000000000000000000000000000000", "live": live, "incoming": incoming,
        })).unwrap()).unwrap();
        let recovery = pp_testkit::snapshot(&work).unwrap();
        let error = open(&root).err().expect("invalid journal must refuse");
        assert!(format!("{error:#}").contains("Restore journal"));
        assert!(pp_testkit::node_schema::database_files(&root) == database);
        assert_eq!(std::fs::read(backup.path()).unwrap(), archive);
        assert_eq!(pp_testkit::snapshot(&work).unwrap(), recovery);
    }
    assert!(client.export_backup("closed").is_err());
    assert!(pp_testkit::node_schema::database_files(&root) == database);
    assert_eq!(std::fs::read(backup.path()).unwrap(), archive);
}
