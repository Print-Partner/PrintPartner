//! An older writer must refuse before it removes the last recovery copy.

#[cfg(feature = "desktop_rebuild")]
mod recovery {
    use pp_storage::{
        Limits, WriterOwner, archive,
        ownership_marker::WriterGate,
        versions::{IncompatibleSchema, RUST_SCHEMA},
    };
    use pp_testkit::{Installation, MarkerState, Snapshot, Spec, node_schema::database_files};
    use rusqlite::Connection;
    use std::path::{Path, PathBuf};

    fn open(root: &Path) -> anyhow::Result<WriterOwner> {
        WriterOwner::open_with_gate(root, Limits::default(), WriterGate::SwitchOn)
            .map(|(owner, _)| owner)
    }

    struct Recovery {
        installation: Installation,
        archive: PathBuf,
        work: PathBuf,
    }

    impl Recovery {
        fn create() -> Self {
            let installation = Installation::create(Spec {
                node_database: true,
                marker: MarkerState::Valid,
                ..Spec::default()
            })
            .unwrap();
            let root = installation.path();
            let owner = open(root).unwrap();
            let backups = root.join("backups");
            std::fs::create_dir_all(&backups).unwrap();
            let archive = backups.join("last-recovery.tar.gz");
            let scratch = tempfile::tempdir().unwrap();
            let exported = scratch.path().join("full.tar.gz");
            owner
                .create_archive(
                    &exported,
                    archive::Request::new("test", archive::Scope::Full),
                )
                .unwrap();
            std::fs::copy(&exported, &archive).unwrap();
            let client = owner.installation().unwrap();
            let bytes = std::fs::read(&archive).unwrap();
            let mut upload = client.begin_upload(bytes.len() as u64).unwrap();
            upload.write(&bytes).unwrap();
            client.stage(upload).unwrap();
            owner.shutdown().unwrap();
            let work = root.join(".pp-restore");
            // Model a crash after an upload was synced but before staging.
            std::fs::write(
                work.join("incoming-0123456789abcdef0123456789abcdef.tar.gz"),
                &bytes,
            )
            .unwrap();
            Self {
                installation,
                archive,
                work,
            }
        }

        fn root(&self) -> &Path {
            self.installation.path()
        }

        fn copies(&self) -> (Vec<u8>, Snapshot) {
            (
                std::fs::read(&self.archive).unwrap(),
                pp_testkit::snapshot(&self.work).unwrap(),
            )
        }

        fn refuse_repeatedly(&self, found: u64) {
            let database = database_files(self.root());
            let recovery = self.copies();
            for attempt in 1..=3 {
                let error = open(self.root())
                    .err()
                    .expect("unsupported schema must refuse");
                let refusal = error.downcast_ref::<IncompatibleSchema>().unwrap();
                assert_eq!(refusal.found, found);
                assert_eq!(refusal.newest, RUST_SCHEMA);
                assert!(
                    database_files(self.root()) == database,
                    "database bytes changed at attempt {attempt}"
                );
                assert!(
                    self.copies() == recovery,
                    "recovery bytes changed at attempt {attempt}"
                );
                println!(
                    "attempt {attempt}: {}; database and recovery bytes unchanged",
                    refusal.diagnostic()
                );
            }
        }
    }

    #[test]
    fn downgrade_preserves_database_archives_and_stages_on_repeated_starts() {
        let recovery = Recovery::create();
        let found = RUST_SCHEMA + 1;
        pp_testkit::node_schema::restamp_schema_version(
            &recovery.installation.database_path(),
            found,
        )
        .unwrap();
        recovery.refuse_repeatedly(found);
    }

    #[test]
    fn downgrade_reads_newer_version_from_wal_without_changing_any_sidecar() {
        let recovery = Recovery::create();
        let connection = Connection::open(recovery.installation.database_path()).unwrap();
        connection
            .execute_batch("PRAGMA journal_mode=WAL; PRAGMA wal_autocheckpoint=0;")
            .unwrap();
        let found = RUST_SCHEMA + 1;
        pp_testkit::node_schema::stamp_schema_version(&connection, found).unwrap();
        // Keep the connection open so closing it cannot checkpoint the WAL.
        // A cold journal may coexist with WAL; admission must leave it alone too.
        std::fs::write(recovery.root().join("print-partner.db-journal"), [0; 512]).unwrap();
        let files = database_files(recovery.root());
        assert!(
            files
                .iter()
                .all(|(_, bytes)| bytes.as_ref().is_some_and(|b| !b.is_empty()))
        );
        recovery.refuse_repeatedly(found);
        drop(connection);
    }

    #[test]
    fn unsupported_old_schema_also_keeps_recovery_copies() {
        let recovery = Recovery::create();
        pp_testkit::node_schema::restamp_schema_version(&recovery.installation.database_path(), 30)
            .unwrap();
        recovery.refuse_repeatedly(30);
    }

    #[test]
    fn supported_restart_cleans_stages_and_keeps_the_saved_archive() {
        let recovery = Recovery::create();
        let before = std::fs::read(&recovery.archive).unwrap();
        let owner = open(recovery.root()).unwrap();
        assert!(!recovery.work.exists());
        assert!(
            std::fs::read(&recovery.archive).unwrap() == before,
            "archive bytes changed"
        );
        owner.shutdown().unwrap();
        let database = database_files(recovery.root());
        let owner = open(recovery.root()).unwrap();
        owner.shutdown().unwrap();
        // A supported open may run schema repairs; refusal is the path that
        // promises unchanged live database bytes.
        println!(
            "supported restart changed database bytes: {}",
            database_files(recovery.root()) != database
        );
        assert!(
            std::fs::read(&recovery.archive).unwrap() == before,
            "archive bytes changed"
        );
    }
}

#[test]
fn both_runtimes_preserve_saved_recovery_archive_when_the_schema_is_newer() {
    use pp_storage::{Limits, WriterOwner, archive, ownership_marker, versions};
    use pp_testkit::{Fixture, Installation, node_schema::database_files};

    let installation = Installation::create(Fixture::NodeSchema34).unwrap();
    let (owner, _) = WriterOwner::open(installation.path(), Limits::default()).unwrap();
    let scratch = tempfile::tempdir().unwrap();
    let exported = scratch.path().join("full.tar.gz");
    owner
        .create_archive(
            &exported,
            archive::Request::new("test", archive::Scope::Full),
        )
        .unwrap();
    owner.shutdown().unwrap();
    let backups = installation.path().join("backups");
    std::fs::create_dir_all(&backups).unwrap();
    let saved = backups.join("last-recovery.tar.gz");
    std::fs::copy(&exported, &saved).unwrap();
    let found = versions::RUST_SCHEMA + 1;
    pp_testkit::node_schema::restamp_schema_version(&installation.database_path(), found).unwrap();
    let database = database_files(installation.path());
    let archive = std::fs::read(&saved).unwrap();
    for attempt in 1..=3 {
        let lease = ownership_marker::lease_unmarked(installation.path()).unwrap();
        let refusal = versions::node_refusal(&lease).unwrap().unwrap();
        assert_eq!(refusal.found, found);
        drop(lease);
        assert!(
            database_files(installation.path()) == database,
            "database bytes changed"
        );
        assert!(
            std::fs::read(&saved).unwrap() == archive,
            "archive bytes changed"
        );

        let error = WriterOwner::open(installation.path(), Limits::default())
            .err()
            .expect("newer schema must refuse");
        assert_eq!(
            error
                .downcast_ref::<versions::IncompatibleSchema>()
                .unwrap()
                .found,
            found
        );
        assert!(
            database_files(installation.path()) == database,
            "database bytes changed"
        );
        assert!(
            std::fs::read(&saved).unwrap() == archive,
            "archive bytes changed"
        );
        println!(
            "attempt {attempt}: Node and Rust refused schema {found}; database and archive bytes unchanged"
        );
    }
}
