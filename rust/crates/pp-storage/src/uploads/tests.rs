use super::{
    Admission, CaptureId, CapturedPayloadV1, Input, decode_import_input_version, expected,
    validate_schema,
};
use crate::{Limits, WriterOwner, schema::SchemaReady, tests::directory};
use rusqlite::{Connection, OpenFlags};
use std::path::{Path, PathBuf};

fn immutable(path: &Path) -> Connection {
    Connection::open_with_flags(
        crate::sqlite_uri::immutable_uri(path).unwrap(),
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_URI,
    )
    .unwrap()
}

fn version_at(path: &Path) -> String {
    immutable(path)
        .query_row(
            "SELECT value FROM app_settings WHERE tenant_id='default' AND key='schema_version'",
            [],
            |row| row.get(0),
        )
        .unwrap()
}

fn backups(directory: &Path) -> Vec<(String, String, Vec<u8>)> {
    let Ok(entries) = std::fs::read_dir(directory.join("backups")) else {
        return Vec::new();
    };
    let mut found: Vec<_> = entries
        .map(|entry| {
            let path = entry.unwrap().path();
            (
                path.file_name().unwrap().to_string_lossy().into_owned(),
                version_at(&path),
                std::fs::read(&path).unwrap(),
            )
        })
        .collect();
    found.sort();
    found
}

fn upgrade(label: &str, input: &[u8]) -> (PathBuf, SchemaReady) {
    let directory = directory(label);
    std::fs::write(directory.join("print-partner.db"), input).unwrap();
    let (owner, ready) = WriterOwner::open(&directory, Limits::default()).unwrap();
    owner.shutdown().unwrap();
    (directory, ready)
}

fn assert_reopen_is_noop(directory: &Path) {
    let before = backups(directory);
    let original = live_contents(&directory.join("print-partner.db"));
    let (owner, ready) = WriterOwner::open(directory, Limits::default()).unwrap();
    owner.shutdown().unwrap();
    assert_eq!(
        (ready.version, ready.previous_version, ready.backup),
        (46, 46, None)
    );
    assert_eq!(backups(directory), before);
    assert_eq!(live_contents(&directory.join("print-partner.db")), original);
    let connection = Connection::open(directory.join("print-partner.db")).unwrap();
    validate_schema(&connection, 41).unwrap();
    crate::source_scan::validate_schema(&connection, 41).unwrap();
    crate::source_sync::validate_schema(&connection, 41).unwrap();
}

#[test]
fn each_upgrade_step_backs_up_its_input_before_writing() {
    let node = include_bytes!("../../tests/fixtures/accepted-plan-node.db");
    let (from34, ready) = upgrade("uploads-from34", node);
    assert_eq!((ready.version, ready.previous_version), (46, 34));
    assert_eq!(
        ready.backup,
        Some(from34.join("backups/pre-schema34-to-46.db"))
    );
    let chain = backups(&from34);
    let names: Vec<_> = chain
        .iter()
        .map(|(name, version, _)| (name.clone(), version.clone()))
        .collect();
    let mut expected = vec![("pre-schema34-to-46.db".to_owned(), "34".to_owned())];
    expected.extend((38..=46).map(|step| (format!("pre-schema{step}.db"), (step - 1).to_string())));
    assert_eq!(names, expected);
    for (name, version) in [
        ("pre-schema38.db", 37),
        ("pre-schema39.db", 38),
        ("pre-schema40.db", 39),
        ("pre-schema41.db", 40),
    ] {
        let saved = immutable(&from34.join("backups").join(name));
        validate_schema(&saved, version).unwrap();
        crate::plan_publication::validate_schema(&saved, version).unwrap();
        crate::source_scan::validate_schema(&saved, version).unwrap();
        crate::source_sync::validate_schema(&saved, version).unwrap();
    }
    assert!(!from34.join("backups/pre-schema-run").exists());
    let tables = |connection: &Connection| -> i64 {
        connection
            .query_row(
                "SELECT count(*) FROM sqlite_master WHERE name IN ('plan_draft_headers','build_profile_details','plan_draft_header_receipts','plan_snapshot_headers')",
                [],
                |row| row.get(0),
            )
            .unwrap()
    };
    assert_eq!(
        tables(&immutable(&from34.join("backups/pre-schema43.db"))),
        0
    );
    assert_eq!(
        tables(&Connection::open(from34.join("print-partner.db")).unwrap()),
        4
    );
    assert_reopen_is_noop(&from34);

    let at37 = &chain[1].2;
    let at38 = &chain[2].2;
    let at39 = &chain[3].2;
    let at40 = &chain[4].2;
    let schema35 = directory("uploads-schema35");
    std::fs::write(schema35.join("schema37.db"), at37).unwrap();
    let raw = Connection::open(schema35.join("schema37.db")).unwrap();
    raw.execute_batch("DROP TABLE plan_apply_admissions; DROP TABLE IF EXISTS domain_events; DROP TABLE IF EXISTS print_link_grams; DROP TABLE IF EXISTS plan_snapshot_headers; DROP TABLE IF EXISTS plan_draft_header_receipts; DROP TABLE IF EXISTS plan_draft_headers; DROP TABLE IF EXISTS build_profile_details; DROP TABLE IF EXISTS printer_send_queue; UPDATE app_settings SET value='35' WHERE tenant_id='default' AND key='schema_version'")
        .unwrap();
    raw.backup("main", schema35.join("schema35.db"), None)
        .unwrap();
    let at35 = std::fs::read(schema35.join("schema35.db")).unwrap();

    for (label, input, previous) in [
        ("uploads-from35", &at35, 35),
        ("uploads-from37", at37, 37),
        ("uploads-from38", at38, 38),
        ("uploads-from39", at39, 39),
        ("uploads-from40", at40, 40),
    ] {
        let steps: Vec<u64> = (previous.max(36) + 1..=46).collect();
        let (directory, ready) = upgrade(label, input);
        assert_eq!((ready.version, ready.previous_version), (46, previous));
        // Only the legacy pass to 37 names its copy for the from and to versions.
        let name = |step: u64| match step {
            37 => format!("pre-schema{previous}-to-46.db"),
            _ => format!("pre-schema{step}.db"),
        };
        let first = name(steps[0]);
        assert_eq!(ready.backup, Some(directory.join("backups").join(&first)));
        let saved = backups(&directory);
        let names: Vec<_> = saved.iter().map(|(name, _, _)| name.clone()).collect();
        let expected: Vec<_> = steps.iter().map(|step| name(*step)).collect();
        assert_eq!(names, expected, "{label}");
        assert!(
            saved[0].2 == *input,
            "{label} backup differs from its input"
        );
        assert_eq!(saved[0].1, previous.to_string(), "{label}");
        assert_reopen_is_noop(&directory);
    }
}

#[test]
fn restored_database_gets_its_own_legacy_backup() {
    let node = include_bytes!("../../tests/fixtures/accepted-plan-node.db");
    let (directory, ready) = upgrade("uploads-restored", node);
    let earlier = directory.join("backups/pre-schema34-to-46.db");
    assert_eq!(ready.backup.as_ref(), Some(&earlier));
    let kept = std::fs::read(&earlier).unwrap();
    let scratch = directory.join("scratch.db");
    std::fs::copy(directory.join("backups/pre-schema38.db"), &scratch).unwrap();
    let raw = Connection::open(&scratch).unwrap();
    raw.execute_batch("DROP TABLE plan_apply_admissions; DROP TABLE IF EXISTS domain_events; DROP TABLE IF EXISTS print_link_grams; DROP TABLE IF EXISTS plan_snapshot_headers; DROP TABLE IF EXISTS plan_draft_header_receipts; DROP TABLE IF EXISTS plan_draft_headers; DROP TABLE IF EXISTS build_profile_details; DROP TABLE IF EXISTS printer_send_queue; UPDATE app_settings SET value='35' WHERE tenant_id='default' AND key='schema_version'")
        .unwrap();
    raw.backup("main", directory.join("schema35.db"), None)
        .unwrap();
    drop(raw);
    let at35 = std::fs::read(directory.join("schema35.db")).unwrap();
    for suffix in ["", "-wal", "-shm"] {
        let _ = std::fs::remove_file(directory.join(format!("print-partner.db{suffix}")));
    }
    std::fs::write(directory.join("print-partner.db"), &at35).unwrap();
    let (owner, ready) = WriterOwner::open(&directory, Limits::default()).unwrap();
    owner.shutdown().unwrap();
    assert_eq!((ready.version, ready.previous_version), (46, 35));
    let own = directory.join("backups/pre-schema35-to-46.db");
    assert_eq!(ready.backup.as_ref(), Some(&own));
    assert!(
        std::fs::read(&own).unwrap() == at35,
        "restored database backup differs from its input"
    );
    assert!(
        std::fs::read(&earlier).unwrap() == kept,
        "earlier backup changed"
    );
}

#[test]
fn failed_step_backup_aborts_before_the_step_writes() {
    let node = include_bytes!("../../tests/fixtures/accepted-plan-node.db");
    let (from34, _) = upgrade("uploads-abort-source", node);
    for (input, step, version) in [
        ("pre-schema38.db", 38, "37"),
        ("pre-schema39.db", 39, "38"),
        ("pre-schema41.db", 41, "40"),
    ] {
        let directory = directory(&format!("uploads-abort-{step}"));
        let database = directory.join("print-partner.db");
        std::fs::copy(from34.join("backups").join(input), &database).unwrap();
        let before = std::fs::read(&database).unwrap();
        let blocked = directory.join(format!("backups/pre-schema{step}.db"));
        std::fs::create_dir_all(&blocked).unwrap();
        let error = WriterOwner::open(&directory, Limits::default())
            .err()
            .unwrap();
        assert!(
            format!("{error:#}").contains("schema::backup: rename temporary backup"),
            "{error:#}"
        );
        assert!(
            std::fs::read(&database).unwrap() == before,
            "{step} input changed"
        );
        assert_eq!(version_at(&database), version);
        assert_eq!(
            std::fs::read_dir(directory.join("backups"))
                .unwrap()
                .count(),
            1
        );
        std::fs::remove_dir(&blocked).unwrap();
        let (owner, ready) = WriterOwner::open(&directory, Limits::default()).unwrap();
        owner.shutdown().unwrap();
        assert_eq!(ready.backup, Some(blocked.clone()));
        assert!(
            std::fs::read(&blocked).unwrap() == before,
            "{step} backup differs"
        );
    }
}

#[test]
fn fresh_database_installs_schema41_without_backups() {
    let directory = directory("uploads-fresh");
    let (owner, ready) = WriterOwner::open(&directory, Limits::default()).unwrap();
    owner.shutdown().unwrap();
    assert_eq!(
        (ready.version, ready.previous_version, ready.backup),
        (46, 0, None)
    );
    assert!(!directory.join("backups").exists());
    assert_reopen_is_noop(&directory);
}

#[test]
fn validator_matches_each_version_threshold() {
    for version in [35, 37, 38, 39] {
        let connection = expected(version).unwrap();
        for candidate in [35, 37, 38, 39] {
            let result = validate_schema(&connection, candidate);
            let installed = |v: u64| if v < 38 { 0 } else { v };
            if installed(candidate) == installed(version) {
                result.unwrap();
            } else {
                assert_eq!(
                    result.unwrap_err().to_string(),
                    "Source import schema mismatch",
                    "{version} as {candidate}"
                );
            }
        }
    }
}

#[test]
fn validator_refuses_drifted_objects_and_columns() {
    let connection = expected(39).unwrap();
    connection
        .execute_batch("DROP TRIGGER trg_source_preclaim_refusals_cursor_insert")
        .unwrap();
    assert_eq!(
        validate_schema(&connection, 39).unwrap_err().to_string(),
        "Source import schema mismatch"
    );

    let connection = Connection::open_in_memory().unwrap();
    connection
        .execute_batch(
            "CREATE TABLE projects(id INTEGER PRIMARY KEY);
             CREATE TABLE source_revisions(id INTEGER PRIMARY KEY);
             CREATE TABLE source_docs(id INTEGER PRIMARY KEY);",
        )
        .unwrap();
    let columns = super::COLUMNS.replace("DEFAULT 1", "DEFAULT 2");
    for sql in [
        columns.as_str(),
        include_str!("schema.sql"),
        include_str!("../remote_sources/schema.sql"),
        include_str!("../remote_sources/preclaim39.sql"),
    ] {
        connection.execute_batch(sql).unwrap();
    }
    assert_eq!(
        validate_schema(&connection, 39).unwrap_err().to_string(),
        "Source revision column schema mismatch: projects.source_configuration_version"
    );
}

fn refusal(rows: &str) -> String {
    let connection = expected(39).unwrap();
    connection
        .execute_batch("PRAGMA foreign_keys=OFF; PRAGMA ignore_check_constraints=ON")
        .unwrap();
    connection.execute_batch(rows).unwrap();
    validate_schema(&connection, 39).unwrap_err().to_string()
}

#[test]
fn validator_refuses_rows_outside_the_schema_invariants() {
    for (rows, message) in [
        (
            "INSERT INTO source_import_quota VALUES('t','k',0,0,0)",
            "Orphan Source quota",
        ),
        (
            "INSERT INTO source_docs(id,input_digest) VALUES(1,'x')",
            "Partial Source document provenance",
        ),
        (
            "INSERT INTO source_revisions(id,source_configuration_version) VALUES(1,1)",
            "Partial Source revision provenance",
        ),
        (
            "INSERT INTO source_revision_observations VALUES('t','k',1,'p','j',1,NULL,NULL,1,0,'now')",
            "Invalid Source observation context",
        ),
        (
            "INSERT INTO source_preclaim_refusals VALUES('t','k',1,'other','j',0,0,'now')",
            "Invalid Source preclaim refusal",
        ),
    ] {
        assert_eq!(refusal(rows), message);
    }
    let digest = "0".repeat(64);
    let trigger: String = expected(39)
        .unwrap()
        .query_row(
            "SELECT sql FROM sqlite_master WHERE name='trg_source_preclaim_refusals_cursor_insert'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(
        refusal(&format!(
            "INSERT INTO source_revisions VALUES(1,1,'{digest}','{digest}','v');
             INSERT INTO source_revision_observations VALUES('t','k',1,'p','j',1,NULL,1,1,0,'now');
             DROP TRIGGER trg_source_preclaim_refusals_cursor_insert;
             INSERT INTO source_preclaim_refusals VALUES('t','k',1,'authority_refused','j',0,0,'now');
             {trigger};"
        )),
        "Source observation cursor collision"
    );
}

fn schema37_input(label: &str, edit: &str) -> Vec<u8> {
    let node = include_bytes!("../../tests/fixtures/accepted-plan-node.db");
    let (from34, _) = upgrade(&format!("{label}-source"), node);
    let path = from34.join("backups/pre-schema38.db");
    Connection::open(&path)
        .unwrap()
        .execute_batch(edit)
        .unwrap();
    assert_eq!(version_at(&path), "37");
    std::fs::read(&path).unwrap()
}

fn starters(path: &Path) -> i64 {
    immutable(path)
        .query_row(
            "SELECT count(*) FROM printer_profiles WHERE tenant_id='default' AND name='Generic Klipper Machine'",
            [],
            |row| row.get(0),
        )
        .unwrap()
}

// The backup API rewrites header counters for a WAL input, so backups are
// compared by schema and rows rather than by bytes.
fn contents(path: &Path) -> Vec<String> {
    connection_contents(&immutable(path))
}

fn live_contents(path: &Path) -> Vec<String> {
    connection_contents(
        &Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY).unwrap(),
    )
}

fn connection_contents(connection: &Connection) -> Vec<String> {
    let mut found: Vec<String> = connection
        .prepare("SELECT type,name,tbl_name,sql FROM sqlite_master ORDER BY name")
        .unwrap()
        .query_map([], |row| {
            Ok(format!(
                "{:?}",
                (
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, Option<String>>(3)?
                )
            ))
        })
        .unwrap()
        .collect::<rusqlite::Result<_>>()
        .unwrap();
    let tables: Vec<String> = connection
        .prepare("SELECT name FROM sqlite_master WHERE type='table' ORDER BY name")
        .unwrap()
        .query_map([], |row| row.get(0))
        .unwrap()
        .collect::<rusqlite::Result<_>>()
        .unwrap();
    for table in tables {
        let mut statement = connection
            .prepare(&format!("SELECT * FROM \"{table}\" ORDER BY rowid"))
            .or_else(|_| connection.prepare(&format!("SELECT * FROM \"{table}\"")))
            .unwrap();
        let width = statement.column_count();
        let rows = statement
            .query_map([], |row| {
                (0..width)
                    .map(|index| row.get::<_, rusqlite::types::Value>(index))
                    .collect::<rusqlite::Result<Vec<_>>>()
            })
            .unwrap()
            .map(|row| format!("{table} {:?}", row.unwrap()));
        found.extend(rows);
    }
    found
}

#[test]
fn step_backups_precede_starter_seed_writes() {
    let input = schema37_input(
        "uploads-seed",
        "UPDATE printer_profiles SET name='Renamed starter' WHERE tenant_id='default' AND name='Generic Klipper Machine'",
    );

    let blocked = directory("uploads-seed-blocked");
    let database = blocked.join("print-partner.db");
    std::fs::write(&database, &input).unwrap();
    assert_eq!(starters(&database), 0);
    let original = live_contents(&database);
    std::fs::create_dir_all(blocked.join("backups/pre-schema38.db")).unwrap();
    let error = WriterOwner::open(&blocked, Limits::default())
        .err()
        .unwrap();
    assert!(
        format!("{error:#}").contains("schema::backup: rename temporary backup"),
        "{error:#}"
    );
    assert!(
        std::fs::read(&database).unwrap() == input,
        "refused open changed the database"
    );
    assert_eq!(version_at(&database), "37");
    assert_eq!(starters(&database), 0);

    let (directory, ready) = upgrade("uploads-seed-open", &input);
    let saved = directory.join("backups/pre-schema38.db");
    assert_eq!(ready.backup, Some(saved.clone()));
    assert_eq!(starters(&saved), 0);
    assert_eq!(contents(&saved), original);
    assert_eq!(starters(&directory.join("backups/pre-schema39.db")), 0);
    assert_eq!(starters(&directory.join("backups/pre-schema40.db")), 0);
    assert_eq!(starters(&directory.join("backups/pre-schema41.db")), 0);
    // The seeded row may still sit in the WAL, which an immutable reader skips.
    let live: i64 = Connection::open(directory.join("print-partner.db"))
        .unwrap()
        .query_row(
            "SELECT count(*) FROM printer_profiles WHERE tenant_id='default' AND name='Generic Klipper Machine'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(live, 1);
    assert_reopen_is_noop(&directory);
}

#[test]
fn step_refuses_drifted_existing_columns_before_its_stamp() {
    for (label, edit, column) in [
        (
            "uploads-column-type",
            "ALTER TABLE projects ADD COLUMN source_configuration_version TEXT",
            "projects.source_configuration_version",
        ),
        (
            "uploads-column-foreign-key",
            "ALTER TABLE source_docs ADD COLUMN source_revision_id INTEGER",
            "source_docs.source_revision_id",
        ),
    ] {
        let input = schema37_input(label, edit);
        let directory = directory(&format!("{label}-open"));
        let database = directory.join("print-partner.db");
        std::fs::write(&database, &input).unwrap();
        let original = live_contents(&database);
        for _ in 0..2 {
            let error = WriterOwner::open(&directory, Limits::default())
                .err()
                .unwrap();
            assert_eq!(version_at(&database), "37", "{label}");
            assert_eq!(live_contents(&database), original, "{label}");
            assert!(
                format!("{error:#}")
                    .contains(&format!("Source revision column schema mismatch: {column}")),
                "{label}: {error:#}"
            );
        }
    }

    let input = schema37_input(
        "uploads-column-kept",
        "ALTER TABLE projects ADD COLUMN source_configuration_version INTEGER NOT NULL DEFAULT 1",
    );
    let (directory, ready) = upgrade("uploads-column-kept-open", &input);
    assert_eq!((ready.version, ready.previous_version), (46, 37));
    assert_reopen_is_noop(&directory);
}

fn live(directory: &Path, sql: &str) -> rusqlite::types::Value {
    Connection::open(directory.join("print-partner.db"))
        .unwrap()
        .query_row(sql, [], |row| row.get(0))
        .unwrap()
}

// Fixture revision 1 is its project's active revision.
fn tenant_mismatch_with_provenance() -> String {
    let digest = |c: char| c.to_string().repeat(64);
    format!(
        "{}UPDATE source_revisions SET tenant_id='old-tenant',source_configuration_version=3,activation_observation_digest='{}',input_digest='{}',producer_version='probe' WHERE id=1;",
        super::COLUMNS,
        digest('a'),
        digest('b')
    )
}

const REVISION: &str = "SELECT tenant_id||':'||source_configuration_version||':'||producer_version FROM source_revisions WHERE id=1";

#[test]
fn steps_run_after_the_tenant_repair() {
    let input = schema37_input("uploads-repair", &tenant_mismatch_with_provenance());
    let (directory, ready) = upgrade("uploads-repair-open", &input);
    assert_eq!((ready.version, ready.previous_version), (46, 37));
    assert_eq!(
        live(&directory, REVISION),
        "default:3:probe".to_owned().into()
    );
    let saved = immutable(&directory.join("backups/pre-schema38.db"));
    let tenant: String = saved
        .query_row(
            "SELECT tenant_id FROM source_revisions WHERE id=1",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(tenant, "old-tenant");
    assert_reopen_is_noop(&directory);
}

#[test]
fn steps_run_after_missing_common_tables_are_restored() {
    let input = schema37_input("uploads-restore", "DROP TABLE source_docs");
    let (directory, ready) = upgrade("uploads-restore-open", &input);
    assert_eq!((ready.version, ready.previous_version), (46, 37));
    assert_eq!(
        live(&directory, "SELECT count(*) FROM source_docs"),
        0.into()
    );
    assert_reopen_is_noop(&directory);
}

#[test]
fn renamed_starter_and_tenant_repair_upgrade_with_the_seed_last() {
    let input = schema37_input(
        "uploads-repair-seed",
        &format!(
            "{}UPDATE printer_profiles SET name='Renamed starter' WHERE tenant_id='default' AND name='Generic Klipper Machine';",
            tenant_mismatch_with_provenance()
        ),
    );
    let directory = directory("uploads-repair-seed-open");
    let database = directory.join("print-partner.db");
    std::fs::write(&database, &input).unwrap();
    let original = live_contents(&database);
    assert_eq!(starters(&database), 0);
    let (owner, ready) = WriterOwner::open(&directory, Limits::default()).unwrap();
    owner.shutdown().unwrap();
    assert_eq!((ready.version, ready.previous_version), (46, 37));
    let first = directory.join("backups/pre-schema38.db");
    assert_eq!(ready.backup, Some(first.clone()));
    assert_eq!(contents(&first), original);
    assert_eq!(starters(&directory.join("backups/pre-schema39.db")), 0);
    assert_eq!(starters(&directory.join("backups/pre-schema40.db")), 0);
    assert_eq!(starters(&directory.join("backups/pre-schema41.db")), 0);
    assert_eq!(
        live(&directory, REVISION),
        "default:3:probe".to_owned().into()
    );
    let seeded = "SELECT count(*) FROM printer_profiles WHERE tenant_id='default' AND name='Generic Klipper Machine'";
    assert_eq!(live(&directory, seeded), 1.into());
    assert_reopen_is_noop(&directory);
}

fn schema39_input(label: &str) -> PathBuf {
    let directory = directory(label);
    let (owner, _) = WriterOwner::open(&directory, Limits::default()).unwrap();
    owner.shutdown().unwrap();
    let connection = Connection::open(directory.join("print-partner.db")).unwrap();
    connection.execute_batch(
        "DROP TABLE IF EXISTS source_sync_targets;
         DROP TABLE IF EXISTS source_sync_batches;
         DROP TABLE IF EXISTS source_scan_executions;
         DROP TABLE IF EXISTS domain_events; DROP TABLE IF EXISTS print_link_grams; DROP TABLE IF EXISTS plan_snapshot_headers; DROP TABLE IF EXISTS plan_draft_header_receipts; DROP TABLE IF EXISTS plan_draft_headers; DROP TABLE IF EXISTS build_profile_details; DROP TABLE IF EXISTS printer_send_queue; UPDATE app_settings SET value='39' WHERE tenant_id='default' AND key='schema_version';
         INSERT INTO app_settings(tenant_id,key,value) VALUES('default','scan-migration-marker','preserved');"
    ).unwrap();
    directory
}

#[test]
fn schema40_backup_preserves_the_logical_input() {
    let directory = schema39_input("scan-schema40-backup");
    let database = directory.join("print-partner.db");
    let original = live_contents(&database);
    let (owner, ready) = WriterOwner::open(&directory, Limits::default()).unwrap();
    owner.shutdown().unwrap();
    let backup = directory.join("backups/pre-schema40.db");
    assert_eq!(ready.backup, Some(backup.clone()));
    assert_eq!((ready.version, ready.previous_version), (46, 39));
    assert_eq!(version_at(&backup), "39");
    assert_eq!(contents(&backup), original);
    crate::source_scan::validate_schema(
        &Connection::open_with_flags(&database, OpenFlags::SQLITE_OPEN_READ_ONLY).unwrap(),
        40,
    )
    .unwrap();
    assert_reopen_is_noop(&directory);
}

#[test]
fn schema40_blocked_backup_leaves_the_database_unchanged() {
    let directory = schema39_input("scan-schema40-blocked");
    let database = directory.join("print-partner.db");
    let original = live_contents(&database);
    let bytes = std::fs::read(&database).unwrap();
    let blocked = directory.join("backups/pre-schema40.db");
    std::fs::create_dir_all(&blocked).unwrap();
    let error = WriterOwner::open(&directory, Limits::default())
        .err()
        .unwrap();
    assert!(
        format!("{error:#}").contains("schema::backup: rename temporary backup"),
        "{error:#}"
    );
    assert_eq!(version_at(&database), "39");
    assert_eq!(live_contents(&database), original);
    assert_eq!(std::fs::read(&database).unwrap(), bytes);
    assert_eq!(
        std::fs::read_dir(directory.join("backups"))
            .unwrap()
            .count(),
        1
    );
    std::fs::remove_dir(&blocked).unwrap();
    let (owner, ready) = WriterOwner::open(&directory, Limits::default()).unwrap();
    owner.shutdown().unwrap();
    assert_eq!(ready.backup, Some(blocked.clone()));
    assert_eq!(contents(&blocked), original);
}

#[test]
fn schema40_validation_rolls_back_a_drifted_existing_table() {
    let directory = schema39_input("scan-schema40-drift");
    let database = directory.join("print-partner.db");
    let connection = Connection::open(&database).unwrap();
    let drift = include_str!("../source_scan/schema.sql")
        .replace("CHECK (generation > 0)", "CHECK (generation >= 0)");
    connection.execute_batch(&drift).unwrap();
    connection
        .execute_batch("DROP INDEX source_scan_execution_receipt")
        .unwrap();
    drop(connection);
    let original = live_contents(&database);
    for _ in 0..2 {
        let error = crate::schema::initialize(&database, 39, "2026-10-17T00:00:00Z")
            .err()
            .unwrap();
        assert!(
            format!("{error:#}").contains("Local scan schema mismatch"),
            "{error:#}"
        );
        assert_eq!(version_at(&database), "39");
        assert_eq!(live_contents(&database), original);
        assert_eq!(
            contents(&directory.join("backups/pre-schema40.db")),
            original
        );
    }
    let error = WriterOwner::open(&directory, Limits::default())
        .err()
        .unwrap();
    assert!(
        format!("{error:#}").contains("Unexpected Local scan schema"),
        "{error:#}"
    );
    assert_eq!(live_contents(&database), original);
}

#[test]
fn schema40_reopen_refuses_journal_drift_without_writes() {
    let directory = directory("scan-schema40-reopen-drift");
    let (owner, _) = WriterOwner::open(&directory, Limits::default()).unwrap();
    owner.shutdown().unwrap();
    let database = directory.join("print-partner.db");
    Connection::open(&database)
        .unwrap()
        .execute_batch("DROP INDEX source_scan_execution_receipt")
        .unwrap();
    let original = live_contents(&database);
    let bytes = std::fs::read(&database).unwrap();
    let error = WriterOwner::open(&directory, Limits::default())
        .err()
        .unwrap();
    assert!(
        format!("{error:#}").contains("Local scan schema mismatch"),
        "{error:#}"
    );
    assert_eq!(version_at(&database), "46");
    assert_eq!(live_contents(&database), original);
    assert_eq!(std::fs::read(&database).unwrap(), bytes);
    assert!(!directory.join("backups").exists());
}

fn schema40_input(label: &str) -> PathBuf {
    let directory = directory(label);
    let (owner, _) = WriterOwner::open(&directory, Limits::default()).unwrap();
    owner.shutdown().unwrap();
    Connection::open(directory.join("print-partner.db"))
        .unwrap()
        .execute_batch(
            "DROP TABLE source_sync_targets;
             DROP TABLE source_sync_batches;
             DROP TABLE IF EXISTS domain_events; DROP TABLE IF EXISTS print_link_grams; DROP TABLE IF EXISTS plan_snapshot_headers; DROP TABLE IF EXISTS plan_draft_header_receipts; DROP TABLE IF EXISTS plan_draft_headers; DROP TABLE IF EXISTS build_profile_details; DROP TABLE IF EXISTS printer_send_queue; UPDATE app_settings SET value='40' WHERE tenant_id='default' AND key='schema_version';
             INSERT INTO app_settings(tenant_id,key,value) VALUES('default','sync-migration-marker','preserved');",
        )
        .unwrap();
    directory
}

#[test]
fn schema41_backup_preserves_the_logical_input() {
    let directory = schema40_input("sync-schema41-backup");
    let database = directory.join("print-partner.db");
    let original = live_contents(&database);
    let (owner, ready) = WriterOwner::open(&directory, Limits::default()).unwrap();
    owner.shutdown().unwrap();
    let backup = directory.join("backups/pre-schema41.db");
    assert_eq!(ready.backup, Some(backup.clone()));
    assert_eq!((ready.version, ready.previous_version), (46, 40));
    assert_eq!(version_at(&backup), "40");
    assert_eq!(contents(&backup), original);
    assert_reopen_is_noop(&directory);
}

#[test]
fn schema41_validation_rolls_back_a_drifted_existing_table() {
    let directory = schema40_input("sync-schema41-drift");
    let database = directory.join("print-partner.db");
    let connection = Connection::open(&database).unwrap();
    let drift = include_str!("../source_sync/schema.sql")
        .replace("target_count <= 1000", "target_count <= 10000");
    connection.execute_batch(&drift).unwrap();
    connection
        .execute_batch("DROP INDEX source_sync_targets_receipt")
        .unwrap();
    drop(connection);
    let original = live_contents(&database);
    for _ in 0..2 {
        let error = crate::schema::initialize(&database, 40, "2026-10-17T00:00:00Z")
            .err()
            .unwrap();
        assert!(
            format!("{error:#}").contains("Source Sync schema mismatch"),
            "{error:#}"
        );
        assert_eq!(version_at(&database), "40");
        assert_eq!(live_contents(&database), original);
        assert_eq!(
            contents(&directory.join("backups/pre-schema41.db")),
            original
        );
    }
    let error = WriterOwner::open(&directory, Limits::default())
        .err()
        .unwrap();
    assert!(
        format!("{error:#}").contains("Unexpected Source Sync schema"),
        "{error:#}"
    );
    assert_eq!(live_contents(&database), original);
}

#[test]
fn capture_payload_round_trip() {
    let identifier = "c".repeat(128);
    let capture_id = CaptureId::new(identifier.clone()).unwrap();
    assert_eq!(capture_id.as_str(), identifier);
    assert_eq!(
        serde_json::from_str::<CaptureId>(&serde_json::to_string(&capture_id).unwrap()).unwrap(),
        capture_id
    );
    assert_eq!(format!("{capture_id:?}"), "CaptureId([redacted])");

    let files = CapturedPayloadV1::files(vec!["models/a.stl".to_string()]);
    let files_wire = serde_json::json!({
        "kind": "files",
        "paths": ["models/a.stl"],
        "policy": {
            "policy_version": 1,
            "relative_paths_transition": "node_json_array_string_filter_boolean_v1",
            "label_selection": "trimmed_relative_then_slash_filename_then_ordinal_stl_v1",
            "backslash_boundary": "replace_with_slash_at_label_boundary_v1",
            "canonical_paths": "source_path_nfc_case_fold_prefix_v1",
            "max_raw_input_bytes": 268_435_456,
            "max_prepared_bytes": 1_073_741_824,
            "max_files": 10_000,
            "max_multipart_parts": 10_001,
            "max_metadata_bytes": 65_536
        }
    });
    assert_eq!(serde_json::to_value(&files).unwrap(), files_wire);
    let files_decoded: CapturedPayloadV1 = serde_json::from_value(files_wire.clone()).unwrap();
    assert_eq!(files_decoded, files);
    files_decoded.validate().unwrap();
    assert_eq!(files_decoded.input_version(), 2);
    assert_eq!(decode_import_input_version(&files_wire).unwrap(), 2);
    assert_eq!(format!("{files:?}"), "CapturedPayloadV1([redacted])");

    let zip = CapturedPayloadV1::zip("pack.zip".to_string());
    let zip_wire = serde_json::json!({
        "kind": "zip",
        "path": "pack.zip",
        "policy": {
            "policy_version": 1,
            "archive_labels": "nfc_at_acquisition",
            "canonical_paths": "source_path_nfc_case_fold_prefix_v1",
            "max_raw_input_bytes": 268_435_456,
            "max_prepared_bytes": 1_073_741_824,
            "max_zip_entries": 10_000
        }
    });
    assert_eq!(serde_json::to_value(&zip).unwrap(), zip_wire);
    assert_eq!(
        serde_json::from_value::<CapturedPayloadV1>(zip_wire.clone()).unwrap(),
        zip
    );
    assert_eq!(decode_import_input_version(&zip_wire).unwrap(), 2);

    let supplied = serde_json::json!({"kind": "files", "paths": ["models/a.stl"]});
    assert_eq!(decode_import_input_version(&supplied).unwrap(), 1);
    assert_eq!(
        serde_json::from_value::<Input>(supplied).unwrap().version(),
        1
    );
    let supplied_zip = serde_json::json!({"kind": "zip", "path": "pack.zip"});
    assert_eq!(decode_import_input_version(&supplied_zip).unwrap(), 1);
}

#[test]
fn capture_payload_rejects_invalid_policy_and_identity() {
    let error = CaptureId::new("").unwrap_err();
    assert!(
        format!("{error:#}").contains("Invalid capture identifier"),
        "{error:#}"
    );
    let error = CaptureId::new("c".repeat(129)).unwrap_err();
    assert!(
        format!("{error:#}").contains("Invalid capture identifier"),
        "{error:#}"
    );
    let error = CaptureId::new("capture\ncontrol").unwrap_err();
    assert!(
        format!("{error:#}").contains("Invalid capture identifier"),
        "{error:#}"
    );
    assert!(serde_json::from_str::<CaptureId>("\"\"").is_err());
    assert!(serde_json::from_str::<CaptureId>("\"capture\\ncontrol\"").is_err());

    let mut tampered =
        serde_json::to_value(CapturedPayloadV1::zip("pack.zip".to_string())).unwrap();
    tampered["policy"]["max_zip_entries"] = serde_json::json!(9_999);
    let decoded: CapturedPayloadV1 = serde_json::from_value(tampered.clone()).unwrap();
    let error = decoded.validate().unwrap_err();
    assert!(
        format!("{error:#}").contains("Unsupported capture policy"),
        "{error:#}"
    );
    let error = decode_import_input_version(&tampered).unwrap_err();
    assert!(
        format!("{error:#}").contains("Unsupported capture policy"),
        "{error:#}"
    );

    let mut unknown_policy =
        serde_json::to_value(CapturedPayloadV1::files(vec!["a.stl".to_string()])).unwrap();
    unknown_policy["policy"]["future"] = serde_json::json!(true);
    assert!(serde_json::from_value::<CapturedPayloadV1>(unknown_policy.clone()).is_err());
    assert!(decode_import_input_version(&unknown_policy).is_err());

    let mut unknown_label =
        serde_json::to_value(CapturedPayloadV1::zip("pack.zip".to_string())).unwrap();
    unknown_label["policy"]["archive_labels"] = serde_json::json!("future");
    assert!(serde_json::from_value::<CapturedPayloadV1>(unknown_label).is_err());
    assert!(
        decode_import_input_version(&serde_json::json!({"kind": "future", "path": "pack.zip"}))
            .is_err()
    );

    let captured =
        serde_json::to_value(CapturedPayloadV1::files(vec!["models/a.stl".to_string()])).unwrap();
    let admission = serde_json::json!({
        "key": "captured-op",
        "target": {"kind": "existing", "source_id": 1},
        "input": captured,
        "reserved_bytes": 1,
        "max_input_bytes": 1,
        "max_prepared_bytes": 1
    });
    assert!(serde_json::from_value::<Admission>(admission).is_err());
}

#[rustfmt::skip]
mod capture_binding {
    use super::super::{
        Admission, AdmissionLimits, CaptureId, CapturedInputV2, CapturedPayloadV1, File, Input,
        Target, decode_import_input_version,
    };
    use crate::{
        Limits, WriterOwner,
        auth::{AuthPolicy, FirstUserTenant, RegistrationPolicy, SessionTenantPolicy},
        catalog::CreateSource,
        jobs::Credential,
        tests::directory,
    };
    use rusqlite::Connection;
    use std::path::Path;
    use std::sync::atomic::AtomicBool;
    #[derive(Clone)]
    struct Binding {
        id: String, actor: String, key: String, source_id: i64, create_name: Option<String>,
        reserved: u64, max_input: u64, max_prepared: u64, path: String, size: u64, hash: String,
        kind: String, zip: bool,
    }
    impl Binding {
        fn fixed() -> Self {
            Self {
                id: "capture-binding".into(), actor: "physical-owner".into(), key: "bind-op".into(),
                source_id: 7, create_name: None, reserved: 3_506_438_144, max_input: 268_435_456,
                max_prepared: 1_073_741_824, path: "part.stl".into(), size: 3, hash: "b".repeat(64),
                kind: "input".into(), zip: false,
            }
        }
        fn target(&self) -> Target {
            match &self.create_name {
                Some(name) => Target::Create { metadata: Box::new(CreateSource { name: name.clone(), ..Default::default() }) },
                None => Target::Existing { source_id: self.source_id },
            }
        }
        fn limits(&self) -> AdmissionLimits {
            AdmissionLimits { reserved_bytes: self.reserved, max_input_bytes: self.max_input, max_prepared_bytes: self.max_prepared }
        }
        fn files(&self) -> Vec<File> {
            vec![File { path: self.path.clone(), size: self.size, sha256: self.hash.clone(), kind: self.kind.clone() }]
        }
        fn payload(&self) -> CapturedPayloadV1 {
            if self.zip { CapturedPayloadV1::zip(self.path.clone()) } else { CapturedPayloadV1::files(vec![self.path.clone()]) }
        }
        fn try_capture(&self, payload: CapturedPayloadV1) -> anyhow::Result<CapturedInputV2> {
            let (target, files) = (self.target(), self.files());
            CapturedInputV2::bind(CaptureId::new(self.id.clone())?, payload, &self.actor, &self.key, &target, self.limits(), &files)
        }
        fn capture(&self) -> CapturedInputV2 { self.try_capture(self.payload()).unwrap() }
        fn check(&self, input: &CapturedInputV2) -> anyhow::Result<()> {
            let (target, files) = (self.target(), self.files());
            input.validate(&self.actor, &self.key, Some(&target), self.limits(), &files)
        }
    }
    fn digests(input: &CapturedInputV2) -> (String, String, String) {
        let value = serde_json::to_value(input).unwrap();
        let text = |name: &str| value[name].as_str().unwrap().to_string();
        (text("policy_digest"), text("target_digest"), text("manifest_digest"))
    }
    fn assert_rebound(base: &Binding, changed: Binding, message: &str) {
        let left = base.capture();
        let (policy, target, manifest) = digests(&left);
        let (next_policy, next_target, next_manifest) = digests(&changed.capture());
        assert_eq!(policy, next_policy);
        assert_ne!(manifest, next_manifest);
        let same_target = changed.source_id == base.source_id && changed.create_name == base.create_name;
        assert_eq!(target == next_target, same_target);
        assert_eq!(changed.check(&left).unwrap_err().to_string(), message);
    }
    fn vary(base: &Binding, field: &str) -> (Binding, &'static str) {
        let mut next = base.clone();
        let message = match field {
            "actor" => { next.actor = "other-actor".into(); "Capture manifest digest mismatch" }
            "key" => { next.key = "other-op".into(); "Capture manifest digest mismatch" }
            "source" => { next.source_id += 1; "Capture target digest mismatch" }
            "create" => { next.create_name = Some("Other".into()); "Capture target digest mismatch" }
            "reserved" => { next.reserved += 1; "Capture manifest digest mismatch" }
            "max_input" => { next.max_input -= 1; "Capture manifest digest mismatch" }
            "max_prepared" => { next.max_prepared -= 1; "Capture manifest digest mismatch" }
            "path" => { next.path = "other.stl".into(); "Capture manifest digest mismatch" }
            "size" => { next.size += 1; "Capture manifest digest mismatch" }
            "hash" => { next.hash = "a".repeat(64); "Capture manifest digest mismatch" }
            "kind" => { next.kind = "stl".into(); "Capture manifest digest mismatch" }
            _ => unreachable!("{field}"),
        };
        (next, message)
    }
    const JOURNAL_SQL: &str = "SELECT \
(SELECT ifnull(group_concat(operation_key||char(9)||actor||char(9)||intent_digest||char(9)||document,char(10)),'') FROM (SELECT operation_key,actor,intent_digest,document FROM source_import_operations ORDER BY operation_key))||char(10)||\
(SELECT ifnull(group_concat(operation_key||char(9)||reserved_bytes||char(9)||retained_bytes||char(9)||settled,char(10)),'') FROM (SELECT operation_key,reserved_bytes,retained_bytes,settled FROM source_import_quota ORDER BY operation_key))||char(10)||\
(SELECT ifnull(group_concat(id||char(9)||kind||char(9)||state,char(10)),'') FROM (SELECT id,kind,state FROM durable_jobs ORDER BY id))||char(10)||\
(SELECT count(*)||char(9)||(SELECT count(*) FROM source_revision_attempts) FROM source_revisions)";
    fn journal_and_files(root: &Path) -> String {
        let connection = Connection::open(root.join("print-partner.db")).unwrap();
        let mut text: String = connection.query_row(JOURNAL_SQL, [], |row| row.get(0)).unwrap();
        let mut pending = vec![root.to_path_buf()];
        let mut files = Vec::new();
        while let Some(dir) = pending.pop() {
            for entry in std::fs::read_dir(dir).unwrap() {
                let path = entry.unwrap().path();
                if path.is_dir() { pending.push(path); continue; }
                let name = path.strip_prefix(root).unwrap().to_string_lossy().into_owned();
                if !name.ends_with(".db") && !name.ends_with("-wal") && !name.ends_with("-shm") {
                    files.push((name, std::fs::read(path).unwrap()));
                }
            }
        }
        files.sort();
        text.push_str(&format!("{files:?}"));
        text
    }
    fn credential(owner: &WriterOwner) -> Credential { Credential::PhysicalOwner(owner.job_physical_owner()) }
    fn supplied(base: &Binding) -> Admission {
        Admission {
            key: "supplied-op".into(),
            target: Target::Create { metadata: Box::new(CreateSource { name: "Binding".into(), ..Default::default() }) },
            input: Input::Files { paths: vec![base.path.clone()] },
            reserved_bytes: base.reserved, max_input_bytes: base.max_input, max_prepared_bytes: base.max_prepared,
        }
    }
    #[test]
    fn capture_binding_is_stable() {
        let base = Binding::fixed();
        let bound = base.capture();
        assert_eq!(bound, base.capture());
        base.check(&bound).unwrap();
        assert_eq!(format!("{bound:?}"), "CapturedInputV2 { capture_version: 1, capture_id: CaptureId([redacted]), payload: CapturedPayloadV1([redacted]), policy_digest: Sha256Digest([redacted]), target_digest: Sha256Digest([redacted]), manifest_digest: Sha256Digest([redacted]) }");
        assert_eq!(digests(&bound), (
            "7163d9f0439e36702b872b901c23a8f40f37683c30ce246faff1cbd666d5129a".into(),
            "706e746ac8c566c92496ded15d60f6d59a3417439af6454f65c36fbedd7f3c60".into(),
            "641bd6aa58d3ce758f16d274b8a2a91669b98f1f6e31ff0a03ea2efbe1bc9d88".into(),
        ));
        let wire = serde_json::to_value(&bound).unwrap();
        assert_eq!(decode_import_input_version(&wire["payload"]).unwrap(), 2);
        assert_eq!(decode_import_input_version(&serde_json::json!({"kind": "files", "paths": ["part.stl"]})).unwrap(), 1);
        let mut other = base.clone();
        other.id = "other-capture".into();
        let (policy, target, manifest) = digests(&bound);
        let (other_policy, other_target, other_manifest) = digests(&other.capture());
        assert_eq!((policy, target), (other_policy, other_target));
        assert_ne!(manifest, other_manifest);
        let mut zip = base;
        zip.zip = true;
        zip.path = "pack.zip".into();
        let zip_bound = zip.capture();
        assert_eq!(zip_bound, zip.capture());
        zip.check(&zip_bound).unwrap();
    }
    #[test]
    fn capture_binding_rejects_changed_actor_target_policy_or_inventory() {
        let root = directory("capture-binding");
        let (owner, _) = WriterOwner::open(&root, Limits::default()).unwrap();
        let client = owner.imports(AuthPolicy {
            registration: RegistrationPolicy::Open,
            session_tenant: SessionTenantPolicy::AccountTenant,
            first_user: FirstUserTenant::NewUser,
        }, 16 * 1024 * 1024 * 1024).unwrap();
        let base = Binding::fixed();
        let files = base.files();
        let cancelled = AtomicBool::new(false);
        let operation = client.admit(credential(&owner), supplied(&base), files.clone(), 0, client.accounting_epoch(), &cancelled).unwrap();
        assert_eq!((operation.input_version, operation.actor.as_str(), operation.key.as_str()), (1, "physical-owner", "supplied-op"));
        let before = journal_and_files(&root);
        let epoch = client.accounting_epoch();
        let file = &operation.requested_files[0];
        let mut live = base;
        live.key = operation.key.clone();
        live.source_id = operation.source_id;
        live.path = file.path.clone();
        live.size = file.size;
        live.hash = file.sha256.clone();
        live.kind = file.kind.clone();
        let bound = live.capture();
        live.check(&bound).unwrap();
        for field in ["actor", "key", "source", "create", "reserved", "max_input", "max_prepared", "path", "size", "hash", "kind"] {
            let (changed, message) = vary(&live, field);
            assert_rebound(&live, changed, message);
        }
        for (zip, field) in [(false, "max_files"), (true, "max_zip_entries")] {
            let mut value = serde_json::to_value(if zip { CapturedPayloadV1::zip("pack.zip".into()) } else { CapturedPayloadV1::files(vec!["part.stl".into()]) }).unwrap();
            value["policy"][field] = serde_json::json!(9_999);
            assert_eq!(live.try_capture(serde_json::from_value(value).unwrap()).unwrap_err().to_string(), "Unsupported capture policy");
        }
        for (pointer, value, message) in [
            ("/payload/policy/max_files", serde_json::json!(9_999), "Unsupported capture policy"),
            ("/policy_digest", serde_json::json!("0".repeat(64)), "Capture policy digest mismatch"),
            ("/capture_version", serde_json::json!(2), "Unsupported capture version"),
        ] {
            let mut wire = serde_json::to_value(&bound).unwrap();
            *wire.pointer_mut(pointer).unwrap() = value;
            assert_eq!(live.check(&serde_json::from_value(wire).unwrap()).unwrap_err().to_string(), message);
        }
        for (pointer, value) in [("/manifest_digest", serde_json::json!("zz")), ("/target_digest", serde_json::json!("A".repeat(64)))] {
            let mut wire = serde_json::to_value(&bound).unwrap();
            *wire.pointer_mut(pointer).unwrap() = value;
            assert!(serde_json::from_value::<CapturedInputV2>(wire).is_err());
        }
        let replay = client.admit(credential(&owner), supplied(&live), files, 0, epoch, &cancelled).unwrap();
        assert_eq!(replay, operation);
        assert_eq!(client.accounting_epoch(), epoch);
        assert_eq!(journal_and_files(&root), before);
        assert_eq!(client.get(credential(&owner), "supplied-op".into()).unwrap(), operation);
        owner.shutdown().unwrap();
    }
}

mod capture_manifest {
    use super::super::{
        Admission, AdmissionLimits, CaptureId, CapturedPayloadV1, File, Input, Target,
        capture_manifest, inspect_capture_manifest, resolve_capture_manifest,
    };
    use crate::{
        Limits, WriterOwner,
        auth::{AuthPolicy, FirstUserTenant, RegistrationPolicy, SessionTenantPolicy},
        catalog::CreateSource,
        jobs::Credential,
        tests::directory,
    };
    use rusqlite::Connection;
    use serde_json::Value;
    use sha2::{Digest, Sha256};
    use std::path::Path;
    use std::sync::atomic::AtomicBool;

    fn limits() -> AdmissionLimits {
        AdmissionLimits {
            reserved_bytes: 3_506_438_144,
            max_input_bytes: 268_435_456,
            max_prepared_bytes: 1_073_741_824,
        }
    }

    fn files() -> Vec<File> {
        vec![File {
            path: "nested/part.stl".into(),
            size: 4,
            sha256: hex::encode(Sha256::digest(b"mesh")),
            kind: "input".into(),
        }]
    }

    fn bind_existing() -> capture_manifest::BoundCaptureManifest {
        capture_manifest::bind_capture_manifest(
            CaptureId::new("ab".repeat(32)).unwrap(),
            CapturedPayloadV1::files(vec!["nested/part.stl".into()]),
            "actor",
            "operation-key",
            &Target::Existing { source_id: 42 },
            limits(),
            &files(),
        )
        .unwrap()
    }

    const JOURNAL_SQL: &str = "SELECT \
(SELECT ifnull(group_concat(operation_key||char(9)||actor||char(9)||intent_digest||char(9)||document,char(10)),'') FROM (SELECT operation_key,actor,intent_digest,document FROM source_import_operations ORDER BY operation_key))||char(10)||\
(SELECT ifnull(group_concat(operation_key||char(9)||reserved_bytes||char(9)||retained_bytes||char(9)||settled,char(10)),'') FROM (SELECT operation_key,reserved_bytes,retained_bytes,settled FROM source_import_quota ORDER BY operation_key))||char(10)||\
(SELECT ifnull(group_concat(id||char(9)||kind||char(9)||state,char(10)),'') FROM (SELECT id,kind,state FROM durable_jobs ORDER BY id))||char(10)||\
(SELECT count(*)||char(9)||(SELECT count(*) FROM source_revision_attempts) FROM source_revisions)";

    fn journal_and_files(root: &Path) -> String {
        let connection = Connection::open(root.join("print-partner.db")).unwrap();
        let mut text: String = connection
            .query_row(JOURNAL_SQL, [], |row| row.get(0))
            .unwrap();
        let mut pending = vec![root.to_path_buf()];
        let mut files = Vec::new();
        while let Some(dir) = pending.pop() {
            for entry in std::fs::read_dir(dir).unwrap() {
                let path = entry.unwrap().path();
                if path.is_dir() {
                    pending.push(path);
                    continue;
                }
                let name = path
                    .strip_prefix(root)
                    .unwrap()
                    .to_string_lossy()
                    .into_owned();
                if !name.ends_with(".db") && !name.ends_with("-wal") && !name.ends_with("-shm") {
                    files.push((name, std::fs::read(path).unwrap()));
                }
            }
        }
        files.sort();
        text.push_str(&format!("{files:?}"));
        text
    }

    fn credential(owner: &WriterOwner) -> Credential {
        Credential::PhysicalOwner(owner.job_physical_owner())
    }

    fn admit_journal(
        root_label: &str,
    ) -> (std::path::PathBuf, WriterOwner, super::super::Operation) {
        let root = directory(root_label);
        let (owner, _) = WriterOwner::open(&root, Limits::default()).unwrap();
        let client = owner
            .imports(
                AuthPolicy {
                    registration: RegistrationPolicy::Open,
                    session_tenant: SessionTenantPolicy::AccountTenant,
                    first_user: FirstUserTenant::NewUser,
                },
                16 * 1024 * 1024 * 1024,
            )
            .unwrap();
        let cancelled = AtomicBool::new(false);
        let operation = client
            .admit(
                credential(&owner),
                Admission {
                    key: "capture-resolve".into(),
                    target: Target::Create {
                        metadata: Box::new(CreateSource {
                            name: "CaptureResolve".into(),
                            ..Default::default()
                        }),
                    },
                    input: Input::Files {
                        paths: vec!["nested/part.stl".into()],
                    },
                    reserved_bytes: limits().reserved_bytes,
                    max_input_bytes: limits().max_input_bytes,
                    max_prepared_bytes: limits().max_prepared_bytes,
                },
                files(),
                0,
                client.accounting_epoch(),
                &cancelled,
            )
            .unwrap();
        (root, owner, operation)
    }

    fn bind_for_operation(
        operation: &super::super::Operation,
    ) -> capture_manifest::BoundCaptureManifest {
        capture_manifest::bind_capture_manifest(
            CaptureId::new("ab".repeat(32)).unwrap(),
            CapturedPayloadV1::files(
                operation
                    .requested_files
                    .iter()
                    .map(|file| file.path.clone())
                    .collect(),
            ),
            &operation.actor,
            &operation.key,
            &Target::Existing {
                source_id: operation.source_id,
            },
            AdmissionLimits {
                reserved_bytes: operation.reserved_bytes,
                max_input_bytes: operation.max_input_bytes,
                max_prepared_bytes: operation.max_prepared_bytes,
            },
            &operation.requested_files,
        )
        .unwrap()
    }

    #[test]
    fn capture_manifest_inventory_round_trip() {
        let bound = bind_existing();
        let inventory = inspect_capture_manifest(bound.bytes()).unwrap();
        assert_eq!(inventory.paths(), &["nested/part.stl".to_string()]);
        assert_eq!(inventory.max_input_bytes(), limits().max_input_bytes);

        let wire: Value = serde_json::from_slice(bound.bytes()).unwrap();
        let keys: Vec<_> = wire.as_object().unwrap().keys().cloned().collect();
        assert_eq!(
            keys,
            [
                "manifest_version",
                "capture_id",
                "operation_key",
                "actor",
                "target",
                "target_digest",
                "input",
                "admission_limits",
                "policy_digest",
                "requested_files",
                "requested_digest",
            ]
        );
        assert_eq!(wire["manifest_version"], 1);
        assert_eq!(wire["actor"], "actor");
        assert_eq!(wire["operation_key"], "operation-key");
        assert_eq!(wire["target"]["source_id"], 42);
        assert_eq!(
            wire["policy_digest"],
            "7163d9f0439e36702b872b901c23a8f40f37683c30ce246faff1cbd666d5129a"
        );
        assert_eq!(
            wire["requested_digest"],
            hex::encode(Sha256::digest(serde_json::to_vec(&files()).unwrap()))
        );

        let rebound = capture_manifest::bind_capture_manifest(
            CaptureId::new("ab".repeat(32)).unwrap(),
            CapturedPayloadV1::files(vec!["nested/part.stl".into()]),
            "actor",
            "operation-key",
            &Target::Existing { source_id: 42 },
            limits(),
            &files(),
        )
        .unwrap();
        assert_eq!(rebound.bytes(), bound.bytes());
        assert_eq!(rebound.input(), bound.input());
        let _ = bound.into_parts();
    }

    #[test]
    fn capture_manifest_rejects_unknown_fields_and_version() {
        let bound = bind_existing();
        let value: Value = serde_json::from_slice(bound.bytes()).unwrap();

        let mut unknown = value.clone();
        unknown
            .as_object_mut()
            .unwrap()
            .insert("future".into(), Value::Bool(true));
        let error = inspect_capture_manifest(&serde_json::to_vec(&unknown).unwrap()).unwrap_err();
        assert!(
            format!("{error:#}").contains("Invalid capture manifest"),
            "{error:#}"
        );

        let mut missing = value.clone();
        missing.as_object_mut().unwrap().remove("actor");
        let error = inspect_capture_manifest(&serde_json::to_vec(&missing).unwrap()).unwrap_err();
        assert!(
            format!("{error:#}").contains("Invalid capture manifest"),
            "{error:#}"
        );

        let mut future = value.clone();
        future["manifest_version"] = Value::from(2);
        let error = inspect_capture_manifest(&serde_json::to_vec(&future).unwrap()).unwrap_err();
        assert!(
            format!("{error:#}").contains("Invalid capture manifest"),
            "{error:#}"
        );

        assert!(inspect_capture_manifest(br#"{}"#).is_err());
        assert!(inspect_capture_manifest(br#"[]"#).is_err());

        let mut bad_limits = value.clone();
        bad_limits["admission_limits"]["max_input_bytes"] = Value::from(268_435_457u64);
        let error =
            inspect_capture_manifest(&serde_json::to_vec(&bad_limits).unwrap()).unwrap_err();
        assert!(
            format!("{error:#}").contains("Invalid import limits"),
            "{error:#}"
        );

        let mut stale = value;
        stale["requested_files"][0]["path"] = Value::from("other.stl");
        let error = inspect_capture_manifest(&serde_json::to_vec(&stale).unwrap()).unwrap_err();
        assert!(
            format!("{error:#}").contains("Capture inventory mismatch")
                || format!("{error:#}").contains("Capture requested digest mismatch"),
            "{error:#}"
        );
    }

    #[test]
    fn capture_manifest_resolves_exact_journal() {
        let (_root, owner, operation) = admit_journal("capture-manifest-resolve");
        let bound = bind_for_operation(&operation);
        let inventory = resolve_capture_manifest(bound.bytes(), &operation).unwrap();
        assert_eq!(
            inventory.paths(),
            operation
                .requested_files
                .iter()
                .map(|file| file.path.clone())
                .collect::<Vec<_>>()
        );
        assert_eq!(inventory.max_input_bytes(), operation.max_input_bytes);
        assert_eq!(inspect_capture_manifest(bound.bytes()).unwrap(), inventory);
        owner.shutdown().unwrap();
    }

    #[test]
    fn capture_manifest_mismatch_preserves_journal() {
        let (root, owner, operation) = admit_journal("capture-manifest-mismatch");
        let before = journal_and_files(&root);
        let bound = bind_for_operation(&operation);
        let value: Value = serde_json::from_slice(bound.bytes()).unwrap();

        let mut wrong_actor = value.clone();
        wrong_actor["actor"] = Value::from("other-actor");
        let error =
            resolve_capture_manifest(&serde_json::to_vec(&wrong_actor).unwrap(), &operation)
                .unwrap_err();
        assert!(
            format!("{error:#}").contains("Capture") && format!("{error:#}").contains("mismatch"),
            "{error:#}"
        );

        let mut wrong_key = value.clone();
        wrong_key["operation_key"] = Value::from("other-key");
        assert!(
            resolve_capture_manifest(&serde_json::to_vec(&wrong_key).unwrap(), &operation).is_err()
        );

        let mut wrong_target = value.clone();
        wrong_target["target"]["source_id"] = Value::from(operation.source_id + 1);
        assert!(
            resolve_capture_manifest(&serde_json::to_vec(&wrong_target).unwrap(), &operation)
                .is_err()
        );

        let mut stale_inventory = value.clone();
        stale_inventory["requested_files"][0]["path"] = Value::from("other.stl");
        let error =
            resolve_capture_manifest(&serde_json::to_vec(&stale_inventory).unwrap(), &operation)
                .unwrap_err();
        assert!(
            format!("{error:#}").contains("Capture inventory mismatch")
                || format!("{error:#}").contains("Capture requested digest mismatch"),
            "{error:#}"
        );

        let mut stale_digest = value;
        stale_digest["requested_digest"] = Value::from("0".repeat(64));
        let error =
            resolve_capture_manifest(&serde_json::to_vec(&stale_digest).unwrap(), &operation)
                .unwrap_err();
        assert!(
            format!("{error:#}").contains("Capture requested digest mismatch"),
            "{error:#}"
        );

        assert_eq!(journal_and_files(&root), before);
        assert_eq!(
            resolve_capture_manifest(bound.bytes(), &operation)
                .unwrap()
                .paths(),
            &operation
                .requested_files
                .iter()
                .map(|file| file.path.clone())
                .collect::<Vec<_>>()[..]
        );
        assert_eq!(journal_and_files(&root), before);
        owner.shutdown().unwrap();
    }
}

mod capture_preflight {
    use super::super::{
        Admission, AdmissionLimits, CaptureId, CapturedInputV2, CapturedPayloadV1, File, Input,
        Operation, State, Target,
    };
    use crate::{
        Limits, WriterOwner,
        auth::{AuthPolicy, FirstUserTenant, RegistrationPolicy, SessionTenantPolicy},
        catalog::CreateSource,
        jobs::Credential,
        tests::directory,
    };
    use rusqlite::{Connection, params};
    use sha2::{Digest, Sha256};
    use std::path::Path;
    use std::sync::atomic::AtomicBool;

    fn policy() -> AuthPolicy {
        AuthPolicy {
            registration: RegistrationPolicy::Open,
            session_tenant: SessionTenantPolicy::AccountTenant,
            first_user: FirstUserTenant::NewUser,
        }
    }

    fn limits() -> AdmissionLimits {
        AdmissionLimits {
            reserved_bytes: 3_506_438_144,
            max_input_bytes: 268_435_456,
            max_prepared_bytes: 1_073_741_824,
        }
    }

    fn files() -> Vec<File> {
        vec![File {
            path: "nested/part.stl".into(),
            size: 4,
            sha256: "a".repeat(64),
            kind: "input".into(),
        }]
    }

    fn credential(owner: &WriterOwner) -> Credential {
        Credential::PhysicalOwner(owner.job_physical_owner())
    }

    const JOURNAL_SQL: &str = "SELECT \
(SELECT ifnull(group_concat(operation_key||char(9)||actor||char(9)||intent_digest||char(9)||document,char(10)),'') FROM (SELECT operation_key,actor,intent_digest,document FROM source_import_operations ORDER BY operation_key))||char(10)||\
(SELECT ifnull(group_concat(operation_key||char(9)||reserved_bytes||char(9)||retained_bytes||char(9)||settled,char(10)),'') FROM (SELECT operation_key,reserved_bytes,retained_bytes,settled FROM source_import_quota ORDER BY operation_key))||char(10)||\
(SELECT ifnull(group_concat(id||char(9)||kind||char(9)||state,char(10)),'') FROM (SELECT id,kind,state FROM durable_jobs ORDER BY id))||char(10)||\
(SELECT count(*)||char(9)||(SELECT count(*) FROM source_revision_attempts) FROM source_revisions)";

    fn journal_and_files(root: &Path) -> String {
        let connection = Connection::open(root.join("print-partner.db")).unwrap();
        let mut text: String = connection
            .query_row(JOURNAL_SQL, [], |row| row.get(0))
            .unwrap();
        let mut pending = vec![root.to_path_buf()];
        let mut files = Vec::new();
        while let Some(dir) = pending.pop() {
            for entry in std::fs::read_dir(dir).unwrap() {
                let path = entry.unwrap().path();
                if path.is_dir() {
                    pending.push(path);
                    continue;
                }
                let name = path
                    .strip_prefix(root)
                    .unwrap()
                    .to_string_lossy()
                    .into_owned();
                if !name.ends_with(".db") && !name.ends_with("-wal") && !name.ends_with("-shm") {
                    files.push((name, std::fs::read(path).unwrap()));
                }
            }
        }
        files.sort();
        text.push_str(&format!("{files:?}"));
        text
    }

    fn create_target(name: &str) -> Target {
        Target::Create {
            metadata: Box::new(CreateSource {
                name: name.into(),
                ..Default::default()
            }),
        }
    }

    const QUOTA: u64 = 16 * 1024 * 1024 * 1024;

    #[test]
    fn capture_preflight_refuses_stale_target_or_quota() {
        let root = directory("capture-preflight-refuse");
        let (owner, _) = WriterOwner::open(&root, Limits::default()).unwrap();
        let client = owner.imports(policy(), QUOTA).unwrap();
        let payload = CapturedPayloadV1::files(vec!["nested/part.stl".into()]);

        let missing = client
            .preflight_capture(
                credential(&owner),
                "capture-missing".into(),
                Target::Existing { source_id: 9_999 },
                payload.clone(),
                limits(),
                0,
            )
            .err()
            .unwrap()
            .to_string();
        assert!(
            missing.contains("Source not found") || missing.contains("not found"),
            "{missing}"
        );

        let over_quota = client
            .preflight_capture(
                credential(&owner),
                "capture-quota".into(),
                create_target("CaptureQuota"),
                payload.clone(),
                AdmissionLimits {
                    reserved_bytes: QUOTA + 1,
                    ..limits()
                },
                0,
            )
            .err()
            .unwrap()
            .to_string();
        assert_eq!(over_quota, "Import quota exceeded");

        let cancelled = AtomicBool::new(false);
        let reserved = limits().reserved_bytes;
        client
            .admit(
                credential(&owner),
                Admission {
                    key: "capture-supplied".into(),
                    target: create_target("CaptureSupplied"),
                    input: Input::Files {
                        paths: vec!["nested/part.stl".into()],
                    },
                    reserved_bytes: reserved,
                    max_input_bytes: limits().max_input_bytes,
                    max_prepared_bytes: limits().max_prepared_bytes,
                },
                files(),
                0,
                client.accounting_epoch(),
                &cancelled,
            )
            .unwrap();
        let conflict = client
            .preflight_capture(
                credential(&owner),
                "capture-supplied".into(),
                create_target("CaptureConflict"),
                payload.clone(),
                limits(),
                0,
            )
            .err()
            .unwrap()
            .to_string();
        assert_eq!(conflict, "Import idempotency conflict");

        let pending = client
            .preflight_capture(
                credential(&owner),
                "capture-pending".into(),
                create_target("CapturePending"),
                payload,
                limits(),
                QUOTA - 2 * reserved + 1,
            )
            .err()
            .unwrap()
            .to_string();
        assert_eq!(pending, "Owner import quota exceeded");
        owner.shutdown().unwrap();
    }

    #[test]
    fn capture_preflight_has_no_journal_side_effects() {
        let root = directory("capture-preflight-clean");
        let (owner, _) = WriterOwner::open(&root, Limits::default()).unwrap();
        let client = owner.imports(policy(), QUOTA).unwrap();
        let before = journal_and_files(&root);
        let epoch = client.accounting_epoch();
        let payload = CapturedPayloadV1::files(vec!["nested/part.stl".into()]);

        let refused = client.preflight_capture(
            credential(&owner),
            "capture-stale".into(),
            Target::Existing { source_id: 42 },
            payload.clone(),
            limits(),
            0,
        );
        assert!(refused.is_err());
        assert_eq!(journal_and_files(&root), before);
        assert_eq!(client.accounting_epoch(), epoch);

        let preflight = client
            .preflight_capture(
                credential(&owner),
                "capture-ok".into(),
                create_target("CaptureOk"),
                payload,
                limits(),
                0,
            )
            .unwrap();
        assert!(preflight.replay().is_none());
        assert_eq!(journal_and_files(&root), before);
        assert_eq!(client.accounting_epoch(), epoch);
        owner.shutdown().unwrap();
    }

    #[test]
    fn capture_preflight_replays_matching_captured_journal() {
        let root = directory("capture-preflight-replay");
        let (owner, _) = WriterOwner::open(&root, Limits::default()).unwrap();
        let client = owner.imports(policy(), QUOTA).unwrap();
        let cancelled = AtomicBool::new(false);
        let admitted = client
            .admit(
                credential(&owner),
                Admission {
                    key: "capture-seed-source".into(),
                    target: create_target("CaptureReplaySource"),
                    input: Input::Files {
                        paths: vec!["nested/part.stl".into()],
                    },
                    reserved_bytes: limits().reserved_bytes,
                    max_input_bytes: limits().max_input_bytes,
                    max_prepared_bytes: limits().max_prepared_bytes,
                },
                files(),
                0,
                client.accounting_epoch(),
                &cancelled,
            )
            .unwrap();
        let target = Target::Existing {
            source_id: admitted.source_id,
        };
        let payload = CapturedPayloadV1::files(vec!["nested/part.stl".into()]);
        let inventory = files();
        let key = "capture-replay";
        let actor = "physical-owner";
        let captured = CapturedInputV2::bind(
            CaptureId::new("capture-replay-id").unwrap(),
            payload.clone(),
            actor,
            key,
            &target,
            limits(),
            &inventory,
        )
        .unwrap();
        let requested_digest = hex::encode(Sha256::digest(serde_json::to_vec(&inventory).unwrap()));
        let prior = Operation {
            tenant: "default".into(),
            actor: actor.into(),
            key: key.into(),
            intent_digest: "c".repeat(64),
            source_id: admitted.source_id,
            job_id: "d".repeat(64),
            input_version: 2,
            input: Input::Captured(captured),
            requested_files: inventory,
            requested_digest,
            state: State::Admitted,
            basis: admitted.basis.clone(),
            default_rules: true,
            original_rules: None,
            reserved_bytes: limits().reserved_bytes,
            max_input_bytes: limits().max_input_bytes,
            max_prepared_bytes: limits().max_prepared_bytes,
            owned: None,
            artifact: None,
            receipt: None,
            cleanup_settled: false,
            authority_revision: None,
            observation_cursor: None,
        };
        let connection = Connection::open(root.join("print-partner.db")).unwrap();
        connection
            .execute(
                "INSERT INTO source_import_operations(tenant,operation_key,actor,intent_digest,source_id,job_id,state,document_version,document) VALUES(?1,?2,?3,?4,?5,?6,'admitted',1,?7)",
                params![
                    prior.tenant,
                    prior.key,
                    prior.actor,
                    prior.intent_digest,
                    prior.source_id,
                    prior.job_id,
                    serde_json::to_string(&prior).unwrap(),
                ],
            )
            .unwrap();
        connection
            .execute(
                "INSERT INTO source_import_quota VALUES(?1,?2,?3,0,0)",
                params![
                    prior.tenant,
                    prior.key,
                    i64::try_from(prior.reserved_bytes).unwrap()
                ],
            )
            .unwrap();
        drop(connection);

        let before = journal_and_files(&root);
        let epoch = client.accounting_epoch();
        let preflight = client
            .preflight_capture(credential(&owner), key.into(), target, payload, limits(), 0)
            .unwrap();
        let replay = preflight.replay().expect("captured journal replay");
        assert_eq!(replay.key, prior.key);
        assert_eq!(replay.source_id, prior.source_id);
        assert_eq!(replay.input, prior.input);
        assert_eq!(journal_and_files(&root), before);
        assert_eq!(client.accounting_epoch(), epoch);
        owner.shutdown().unwrap();
    }
}

mod capture_admission {
    use super::super::{
        Admission, AdmissionLimits, CaptureId, CapturedInputV2, CapturedPayloadV1, File, Input,
        Target,
    };
    use crate::{
        Limits, WriterOwner,
        auth::{AuthPolicy, FirstUserTenant, RegistrationPolicy, SessionTenantPolicy},
        catalog::CreateSource,
        jobs::Credential,
        tests::directory,
    };
    use rusqlite::{Connection, params};
    use std::path::Path;
    use std::sync::atomic::AtomicBool;

    fn policy() -> AuthPolicy {
        AuthPolicy {
            registration: RegistrationPolicy::Open,
            session_tenant: SessionTenantPolicy::AccountTenant,
            first_user: FirstUserTenant::NewUser,
        }
    }

    fn limits() -> AdmissionLimits {
        AdmissionLimits {
            reserved_bytes: 3_506_438_144,
            max_input_bytes: 268_435_456,
            max_prepared_bytes: 1_073_741_824,
        }
    }

    fn files() -> Vec<File> {
        vec![File {
            path: "nested/part.stl".into(),
            size: 4,
            sha256: "a".repeat(64),
            kind: "input".into(),
        }]
    }

    fn credential(owner: &WriterOwner) -> Credential {
        Credential::PhysicalOwner(owner.job_physical_owner())
    }

    const JOURNAL_SQL: &str = "SELECT \
(SELECT ifnull(group_concat(operation_key||char(9)||actor||char(9)||intent_digest||char(9)||document,char(10)),'') FROM (SELECT operation_key,actor,intent_digest,document FROM source_import_operations ORDER BY operation_key))||char(10)||\
(SELECT ifnull(group_concat(operation_key||char(9)||reserved_bytes||char(9)||retained_bytes||char(9)||settled,char(10)),'') FROM (SELECT operation_key,reserved_bytes,retained_bytes,settled FROM source_import_quota ORDER BY operation_key))||char(10)||\
(SELECT ifnull(group_concat(id||char(9)||kind||char(9)||state,char(10)),'') FROM (SELECT id,kind,state FROM durable_jobs ORDER BY id))||char(10)||\
(SELECT count(*)||char(9)||(SELECT count(*) FROM source_revision_attempts) FROM source_revisions)";

    fn journal_and_files(root: &Path) -> String {
        let connection = Connection::open(root.join("print-partner.db")).unwrap();
        let mut text: String = connection
            .query_row(JOURNAL_SQL, [], |row| row.get(0))
            .unwrap();
        let mut pending = vec![root.to_path_buf()];
        let mut files = Vec::new();
        while let Some(dir) = pending.pop() {
            for entry in std::fs::read_dir(dir).unwrap() {
                let path = entry.unwrap().path();
                if path.is_dir() {
                    pending.push(path);
                    continue;
                }
                let name = path
                    .strip_prefix(root)
                    .unwrap()
                    .to_string_lossy()
                    .into_owned();
                if !name.ends_with(".db") && !name.ends_with("-wal") && !name.ends_with("-shm") {
                    files.push((name, std::fs::read(path).unwrap()));
                }
            }
        }
        files.sort();
        text.push_str(&format!("{files:?}"));
        text
    }

    fn create_target(name: &str) -> Target {
        Target::Create {
            metadata: Box::new(CreateSource {
                name: name.into(),
                ..Default::default()
            }),
        }
    }

    const QUOTA: u64 = 16 * 1024 * 1024 * 1024;

    #[test]
    fn captured_admission_replays_once() {
        let root = directory("capture-admission-replay");
        let (owner, _) = WriterOwner::open(&root, Limits::default()).unwrap();
        let client = owner.imports(policy(), QUOTA).unwrap();
        let payload = CapturedPayloadV1::files(vec!["nested/part.stl".into()]);
        let inventory = files();
        let key = "capture-admit-replay";
        let capture_id = CaptureId::new("capture-admit-replay-id").unwrap();

        let preflight = client
            .preflight_capture(
                credential(&owner),
                key.into(),
                create_target("CaptureAdmitReplay"),
                payload.clone(),
                limits(),
                0,
            )
            .unwrap();
        let prepared = preflight
            .prepare(
                capture_id.clone(),
                key.into(),
                payload.clone(),
                limits(),
                inventory.clone(),
            )
            .unwrap();
        assert!(!prepared.manifest().is_empty());

        let first = client
            .admit_prepared(prepared, 0, client.accounting_epoch())
            .unwrap();
        assert_eq!(first.input_version, 2);
        assert!(matches!(first.input, Input::Captured(_)));

        let connection = Connection::open(root.join("print-partner.db")).unwrap();
        let rows: i64 = connection
            .query_row("SELECT COUNT(*) FROM source_import_operations", [], |row| {
                row.get(0)
            })
            .unwrap();
        let quotas: i64 = connection
            .query_row("SELECT COUNT(*) FROM source_import_quota", [], |row| {
                row.get(0)
            })
            .unwrap();
        let jobs: i64 = connection
            .query_row("SELECT COUNT(*) FROM durable_jobs", [], |row| row.get(0))
            .unwrap();
        drop(connection);
        assert_eq!((rows, quotas, jobs), (1, 1, 1));

        let preflight = client
            .preflight_capture(
                credential(&owner),
                key.into(),
                create_target("CaptureAdmitReplay"),
                payload.clone(),
                limits(),
                0,
            )
            .unwrap();
        let prepared = preflight
            .prepare(capture_id, key.into(), payload, limits(), inventory)
            .unwrap();
        let replay = client
            .admit_prepared(prepared, 0, client.accounting_epoch())
            .unwrap();
        assert_eq!(replay, first);

        let connection = Connection::open(root.join("print-partner.db")).unwrap();
        assert_eq!(
            connection
                .query_row("SELECT COUNT(*) FROM source_import_operations", [], |row| {
                    row.get::<_, i64>(0)
                })
                .unwrap(),
            1
        );
        assert_eq!(
            connection
                .query_row("SELECT COUNT(*) FROM source_import_quota", [], |row| {
                    row.get::<_, i64>(0)
                })
                .unwrap(),
            1
        );
        assert_eq!(
            connection
                .query_row("SELECT COUNT(*) FROM durable_jobs", [], |row| {
                    row.get::<_, i64>(0)
                })
                .unwrap(),
            1
        );
        drop(connection);

        let refused = client
            .admit(
                credential(&owner),
                Admission {
                    key: "credential-captured".into(),
                    target: create_target("CredentialCaptured"),
                    input: Input::Captured(
                        CapturedInputV2::bind(
                            CaptureId::new("credential-captured-id").unwrap(),
                            CapturedPayloadV1::files(vec!["nested/part.stl".into()]),
                            "physical-owner",
                            "credential-captured",
                            &create_target("CredentialCaptured"),
                            limits(),
                            &files(),
                        )
                        .unwrap(),
                    ),
                    reserved_bytes: limits().reserved_bytes,
                    max_input_bytes: limits().max_input_bytes,
                    max_prepared_bytes: limits().max_prepared_bytes,
                },
                files(),
                0,
                client.accounting_epoch(),
                &AtomicBool::new(false),
            )
            .err()
            .unwrap()
            .to_string();
        assert_eq!(refused, "Captured imports require prepared admission");
        owner.shutdown().unwrap();
    }

    #[test]
    fn captured_admission_stale_basis_rolls_back() {
        let root = directory("capture-admission-stale");
        let (owner, _) = WriterOwner::open(&root, Limits::default()).unwrap();
        let client = owner.imports(policy(), QUOTA).unwrap();
        let cancelled = AtomicBool::new(false);
        let seeded = client
            .admit(
                credential(&owner),
                Admission {
                    key: "capture-stale-seed".into(),
                    target: create_target("CaptureStaleSeed"),
                    input: Input::Files {
                        paths: vec!["nested/part.stl".into()],
                    },
                    reserved_bytes: limits().reserved_bytes,
                    max_input_bytes: limits().max_input_bytes,
                    max_prepared_bytes: limits().max_prepared_bytes,
                },
                files(),
                0,
                client.accounting_epoch(),
                &cancelled,
            )
            .unwrap();
        let target = Target::Existing {
            source_id: seeded.source_id,
        };
        let payload = CapturedPayloadV1::files(vec!["nested/part.stl".into()]);
        let preflight = client
            .preflight_capture(
                credential(&owner),
                "capture-stale-admit".into(),
                target,
                payload.clone(),
                limits(),
                0,
            )
            .unwrap();
        assert!(preflight.replay().is_none());

        let connection = Connection::open(root.join("print-partner.db")).unwrap();
        connection
            .execute(
                "DELETE FROM projects WHERE tenant_id=?1 AND id=?2",
                params!["default", seeded.source_id],
            )
            .unwrap();
        drop(connection);

        let before = journal_and_files(&root);
        let prepared = preflight
            .prepare(
                CaptureId::new("capture-stale-admit-id").unwrap(),
                "capture-stale-admit".into(),
                payload,
                limits(),
                files(),
            )
            .unwrap();
        let refused = client
            .admit_prepared(prepared, 0, client.accounting_epoch())
            .err()
            .unwrap()
            .to_string();
        assert!(
            refused.contains("Source not found") || refused.contains("not found"),
            "{refused}"
        );
        assert_eq!(journal_and_files(&root), before);
        owner.shutdown().unwrap();
    }
}

mod capture_correlation {
    use super::super::{
        AdmissionLimits, CaptureId, CaptureJournalCorrelation, CapturedPayloadV1, File, OwnedInput,
        Phase, ResolvedCapturedClaim, State, Target,
    };
    use crate::{
        Limits, WriterOwner,
        auth::{AuthPolicy, FirstUserTenant, RegistrationPolicy, SessionTenantPolicy},
        catalog::CreateSource,
        jobs::{Credential, JobKind, PersistentState, ServerWorkerClient, WorkerAdmission},
        tests::directory,
    };
    use rusqlite::Connection;

    fn policy() -> AuthPolicy {
        AuthPolicy {
            registration: RegistrationPolicy::Open,
            session_tenant: SessionTenantPolicy::AccountTenant,
            first_user: FirstUserTenant::NewUser,
        }
    }

    fn limits() -> AdmissionLimits {
        AdmissionLimits {
            reserved_bytes: 3_506_438_144,
            max_input_bytes: 268_435_456,
            max_prepared_bytes: 1_073_741_824,
        }
    }

    fn files() -> Vec<File> {
        vec![File {
            path: "nested/part.stl".into(),
            size: 4,
            sha256: "a".repeat(64),
            kind: "input".into(),
        }]
    }

    fn credential(owner: &WriterOwner) -> Credential {
        Credential::PhysicalOwner(owner.job_physical_owner())
    }

    fn create_target(name: &str) -> Target {
        Target::Create {
            metadata: Box::new(CreateSource {
                name: name.into(),
                ..Default::default()
            }),
        }
    }

    fn worker(owner: &WriterOwner) -> ServerWorkerClient {
        owner
            .job_worker_with_policy(
                policy(),
                WorkerAdmission {
                    kinds: vec![(JobKind::SuppliedSourceImport, 1)],
                    total: 1,
                    per_resource: 1,
                    lease_seconds: 60,
                },
            )
            .unwrap()
    }

    const QUOTA: u64 = 16 * 1024 * 1024 * 1024;

    fn journal_snapshot(root: &std::path::Path) -> String {
        let connection = Connection::open(root.join("print-partner.db")).unwrap();
        connection
            .query_row(
                "SELECT \
(SELECT ifnull(group_concat(operation_key||char(9)||actor||char(9)||intent_digest||char(9)||state||char(9)||document,char(10)),'') FROM (SELECT operation_key,actor,intent_digest,state,document FROM source_import_operations ORDER BY operation_key))||char(10)||\
(SELECT ifnull(group_concat(id||char(9)||kind||char(9)||state||char(9)||document,char(10)),'') FROM (SELECT id,kind,state,document FROM durable_jobs ORDER BY id))",
                [],
                |row| row.get(0),
            )
            .unwrap()
    }

    #[test]
    fn capture_correlation_distinguishes_absent_match_conflict() {
        let root = directory("capture-correlation-absent-match");
        let (owner, _) = WriterOwner::open(&root, Limits::default()).unwrap();
        let client = owner.imports(policy(), QUOTA).unwrap();
        let payload = CapturedPayloadV1::files(vec!["nested/part.stl".into()]);
        let inventory = files();
        let key = "capture-correlate-key";
        let capture_id = CaptureId::new("capture-correlate-id").unwrap();

        let prepared = client
            .preflight_capture(
                credential(&owner),
                key.into(),
                create_target("CaptureCorrelate"),
                payload.clone(),
                limits(),
                0,
            )
            .unwrap()
            .prepare(
                capture_id.clone(),
                key.into(),
                payload.clone(),
                limits(),
                inventory.clone(),
            )
            .unwrap();
        let manifest = prepared.manifest().to_vec();
        let mut conflicted = inventory.clone();
        conflicted[0].sha256 = "b".repeat(64);
        let mut tampered: serde_json::Value = serde_json::from_slice(&manifest).unwrap();
        tampered["requested_files"][0]["sha256"] = "b".repeat(64).into();
        let tampered = serde_json::to_vec(&tampered).unwrap();

        let before_admit = journal_snapshot(&root);
        assert_eq!(
            client
                .correlate_capture_manifest(manifest.clone(), inventory.clone())
                .unwrap(),
            CaptureJournalCorrelation::Absent
        );
        assert_eq!(
            client
                .correlate_capture_manifest(manifest.clone(), conflicted.clone())
                .unwrap(),
            CaptureJournalCorrelation::MismatchRepairRequired
        );
        assert_eq!(
            client
                .correlate_capture_manifest(tampered, inventory.clone())
                .err()
                .unwrap()
                .to_string(),
            "Capture requested digest mismatch"
        );
        assert_eq!(journal_snapshot(&root), before_admit);

        let admitted = client
            .admit_prepared(prepared, 0, client.accounting_epoch())
            .unwrap();
        let after_admit = journal_snapshot(&root);
        let epoch = client.accounting_epoch();
        assert_ne!(before_admit, after_admit);
        assert_eq!(admitted.input_version, 2);

        assert_eq!(
            client
                .correlate_capture_manifest(manifest.clone(), inventory.clone())
                .unwrap(),
            CaptureJournalCorrelation::ExactAdmitted(ResolvedCapturedClaim {
                manifest: manifest.clone(),
                inventory: inventory.clone(),
                tenant: admitted.tenant.clone(),
                operation_key: key.into(),
                job_id: admitted.job_id.clone(),
            })
        );
        assert_eq!(
            client
                .correlate_capture_manifest(manifest, conflicted)
                .unwrap(),
            CaptureJournalCorrelation::MismatchRepairRequired
        );
        assert_eq!(journal_snapshot(&root), after_admit);
        assert_eq!(client.accounting_epoch(), epoch);
        owner.shutdown().unwrap();
    }

    #[test]
    fn resolved_capture_claim_rejects_changed_inventory() {
        let root = directory("capture-correlation-changed-inventory");
        let (owner, _) = WriterOwner::open(&root, Limits::default()).unwrap();
        let client = owner.imports(policy(), QUOTA).unwrap();
        let payload = CapturedPayloadV1::files(vec!["nested/part.stl".into()]);
        let inventory = files();
        let key = "capture-claim-inventory";
        let prepared = client
            .preflight_capture(
                credential(&owner),
                key.into(),
                create_target("CaptureClaimInventory"),
                payload.clone(),
                limits(),
                0,
            )
            .unwrap()
            .prepare(
                CaptureId::new("capture-claim-inventory-id").unwrap(),
                key.into(),
                payload,
                limits(),
                inventory.clone(),
            )
            .unwrap();
        let manifest = prepared.manifest().to_vec();
        let admitted = client
            .admit_prepared(prepared, 0, client.accounting_epoch())
            .unwrap();

        let CaptureJournalCorrelation::ExactAdmitted(mut claim) = client
            .correlate_capture_manifest(manifest.clone(), inventory.clone())
            .unwrap()
        else {
            panic!("expected exact admitted claim");
        };
        // Keep the journal valid so load and validate_operation succeed. Only
        // the retained claim inventory diverges, forcing capture_claim_matches.
        claim.inventory[0].sha256 = "c".repeat(64);

        let before = journal_snapshot(&root);
        let epoch = client.accounting_epoch();
        let refused = worker(&owner)
            .claim_resolved_import(claim)
            .err()
            .unwrap()
            .to_string();
        assert_eq!(refused, "Captured import requires repair");
        assert_eq!(journal_snapshot(&root), before);
        assert_eq!(client.accounting_epoch(), epoch);
        // The queued job is untouched, so the original claim still resolves.
        assert_eq!(
            client
                .correlate_capture_manifest(manifest.clone(), inventory.clone())
                .unwrap(),
            CaptureJournalCorrelation::ExactAdmitted(ResolvedCapturedClaim {
                manifest,
                inventory,
                tenant: admitted.tenant.clone(),
                operation_key: key.into(),
                job_id: admitted.job_id.clone(),
            })
        );
        owner.shutdown().unwrap();
    }

    #[test]
    fn resolved_capture_claim_runs_admitted_job() {
        let root = directory("capture-correlation-claim-success");
        let (owner, _) = WriterOwner::open(&root, Limits::default()).unwrap();
        let client = owner.imports(policy(), QUOTA).unwrap();
        let payload = CapturedPayloadV1::files(vec!["nested/part.stl".into()]);
        let inventory = files();
        let key = "capture-claim-success";
        let prepared = client
            .preflight_capture(
                credential(&owner),
                key.into(),
                create_target("CaptureClaimSuccess"),
                payload.clone(),
                limits(),
                0,
            )
            .unwrap()
            .prepare(
                CaptureId::new("capture-claim-success-id").unwrap(),
                key.into(),
                payload,
                limits(),
                inventory.clone(),
            )
            .unwrap();
        let manifest = prepared.manifest().to_vec();
        let admitted = client
            .admit_prepared(prepared, 0, client.accounting_epoch())
            .unwrap();
        let CaptureJournalCorrelation::ExactAdmitted(claim) = client
            .correlate_capture_manifest(manifest, inventory)
            .unwrap()
        else {
            panic!("expected exact admitted claim");
        };

        let claimed = worker(&owner)
            .claim_resolved_import(claim)
            .unwrap()
            .expect("valid resolved claim is claimable");
        assert_eq!(claimed.job.job_id, admitted.job_id);
        assert_eq!(claimed.job.state, PersistentState::Running);
        assert!(claimed.source_work.is_some());
        assert_eq!(admitted.key, key);
        owner.shutdown().unwrap();
    }

    #[test]
    fn capture_correlation_reports_exact_owned_or_later() {
        let root = directory("capture-correlation-owned-or-later");
        let (owner, _) = WriterOwner::open(&root, Limits::default()).unwrap();
        let client = owner.imports(policy(), QUOTA).unwrap();
        let payload = CapturedPayloadV1::files(vec!["nested/part.stl".into()]);
        let inventory = files();
        let key = "capture-owned-or-later";
        let prepared = client
            .preflight_capture(
                credential(&owner),
                key.into(),
                create_target("CaptureOwnedOrLater"),
                payload.clone(),
                limits(),
                0,
            )
            .unwrap()
            .prepare(
                CaptureId::new("capture-owned-or-later-id").unwrap(),
                key.into(),
                payload,
                limits(),
                inventory.clone(),
            )
            .unwrap();
        let manifest = prepared.manifest().to_vec();
        let admitted = client
            .admit_prepared(prepared, 0, client.accounting_epoch())
            .unwrap();
        let CaptureJournalCorrelation::ExactAdmitted(claim) = client
            .correlate_capture_manifest(manifest.clone(), inventory.clone())
            .unwrap()
        else {
            panic!("expected exact admitted claim");
        };

        let worker = worker(&owner);
        let claimed = worker
            .claim_resolved_import(claim)
            .unwrap()
            .expect("valid resolved claim is claimable");
        let owned = worker
            .import_phase(
                &claimed.lease,
                claimed.source_work.as_ref().unwrap(),
                Phase::Owned(OwnedInput {
                    locator: format!(".pp-imports/{}", admitted.job_id),
                    digest: admitted.requested_digest.clone(),
                    files: admitted.requested_files.clone(),
                }),
            )
            .unwrap();
        assert_eq!(owned.state, State::OwnedInputReady);

        let before = journal_snapshot(&root);
        assert!(matches!(
            client
                .correlate_capture_manifest(manifest, inventory)
                .unwrap(),
            CaptureJournalCorrelation::ExactOwnedOrLater
        ));
        assert_eq!(journal_snapshot(&root), before);
        assert_eq!(admitted.key, key);
        owner.shutdown().unwrap();
    }
}
