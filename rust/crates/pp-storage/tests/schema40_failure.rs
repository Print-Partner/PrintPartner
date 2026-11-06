#![cfg(feature = "desktop_rebuild")]

use pp_storage::{Limits, WriterOwner};
use rusqlite::{Connection, OpenFlags, types::Value};
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
};

const REJECT_SCHEMA40: &str = "
    CREATE TRIGGER reject_schema40 BEFORE UPDATE OF value ON app_settings
    WHEN NEW.tenant_id = 'default' AND NEW.key = 'schema_version' AND NEW.value = '40'
    BEGIN SELECT RAISE(ABORT, 'GRE-710 schema40 stamp rejected'); END;
";

#[derive(Debug, PartialEq)]
struct Snapshot {
    objects: Vec<(String, String, String, Option<String>)>,
    rows: Vec<(String, Vec<String>)>,
    user_version: i64,
}

fn immutable(path: &Path) -> Connection {
    Connection::open_with_flags(
        pp_storage::sqlite_uri::immutable_uri(path).expect("fixture paths are UTF-8"),
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_URI,
    )
    .unwrap()
}

fn read_only(path: &Path) -> Connection {
    Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY).unwrap()
}

fn snapshot(connection: Connection) -> Snapshot {
    let objects: Vec<(String, String, String, Option<String>)> = connection
        .prepare("SELECT type, name, tbl_name, sql FROM sqlite_master ORDER BY name")
        .unwrap()
        .query_map([], |row| {
            Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?))
        })
        .unwrap()
        .collect::<rusqlite::Result<_>>()
        .unwrap();
    let rows = objects
        .iter()
        .filter(|(kind, ..)| kind == "table")
        .map(|(_, name, ..)| {
            let quoted = name.replace('"', "\"\"");
            let mut statement = connection
                .prepare(&format!("SELECT * FROM \"{quoted}\""))
                .unwrap();
            let width = statement.column_count();
            let mut rows: Vec<String> = statement
                .query_map([], |row| {
                    let values = (0..width)
                        .map(|index| row.get::<_, Value>(index))
                        .collect::<rusqlite::Result<Vec<_>>>()?;
                    Ok(format!("{values:?}"))
                })
                .unwrap()
                .collect::<rusqlite::Result<_>>()
                .unwrap();
            rows.sort();
            (name.clone(), rows)
        })
        .collect();
    Snapshot {
        objects,
        rows,
        user_version: connection
            .query_row("PRAGMA user_version", [], |row| row.get(0))
            .unwrap(),
    }
}

fn version(connection: Connection) -> String {
    connection
        .query_row(
            "SELECT value FROM app_settings WHERE tenant_id='default' AND key='schema_version'",
            [],
            |row| row.get(0),
        )
        .unwrap()
}

fn scan_objects(path: &Path) -> Vec<String> {
    read_only(path)
        .prepare(
            "SELECT name FROM sqlite_master
             WHERE name GLOB '*source_scan*' OR tbl_name GLOB '*source_scan*' ORDER BY name",
        )
        .unwrap()
        .query_map([], |row| row.get(0))
        .unwrap()
        .collect::<rusqlite::Result<_>>()
        .unwrap()
}

struct Fixture {
    root: PathBuf,
    current_version: u64,
}

// Reserved URI characters in the root make every immutable open prove escaping.
// Windows forbids `?` in file names.
const RESERVED: &str = if cfg!(windows) { " #%&;=" } else { " ?#%&;=" };

impl Fixture {
    fn new() -> Self {
        // An absolute root keeps `file:///` URIs and reported backup paths
        // correct when TMPDIR is relative.
        let root = std::path::absolute(std::env::temp_dir().join(format!(
            "pp-schema40-failure{RESERVED}{}",
            hex::encode(rand::random::<[u8; 16]>())
        )))
        .unwrap();
        std::fs::create_dir(&root).unwrap();
        // Own the root before seeding so a setup panic still removes it.
        let mut fixture = Self {
            root,
            current_version: 0,
        };
        let seed = fixture.root.join("seed");
        std::fs::create_dir(&seed).unwrap();
        Connection::open(seed.join("print-partner.db"))
            .unwrap()
            .execute_batch(
                "CREATE TABLE gre710_probe(value BLOB NOT NULL);
                 INSERT INTO gre710_probe VALUES (X'00FF01');",
            )
            .unwrap();
        let (owner, ready) = WriterOwner::open(&seed, Limits::default()).unwrap();
        owner.shutdown().unwrap();
        fixture.current_version = ready.version;
        fixture
    }

    fn input(&self, schema_version: u64) -> PathBuf {
        let directory = self.root.join(format!("input{schema_version}"));
        std::fs::create_dir(&directory).unwrap();
        let database = directory.join("print-partner.db");
        std::fs::copy(
            self.root
                .join(format!("seed/backups/pre-schema{}.db", schema_version + 1)),
            &database,
        )
        .unwrap();
        Connection::open(&database)
            .unwrap()
            .execute_batch(REJECT_SCHEMA40)
            .unwrap();
        assert_eq!(version(read_only(&database)), schema_version.to_string());
        directory
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        // A second panic while unwinding aborts and hides the failed assertion.
        let result = std::fs::remove_dir_all(&self.root);
        if !std::thread::panicking() {
            result.expect("remove schema40 failure fixture");
        }
    }
}

fn backup_bytes(directory: &Path) -> BTreeMap<String, Vec<u8>> {
    std::fs::read_dir(directory.join("backups"))
        .unwrap()
        .map(|entry| {
            let entry = entry.unwrap();
            (
                entry.file_name().into_string().unwrap(),
                std::fs::read(entry.path()).unwrap(),
            )
        })
        .collect()
}

fn assert_stamp_refused(directory: &Path) {
    let error = WriterOwner::open(directory, Limits::default())
        .err()
        .expect("schema40 stamp must fail public open");
    assert!(
        format!("{error:#}").contains("GRE-710 schema40 stamp rejected"),
        "{error:#}"
    );
    assert_eq!(
        version(read_only(&directory.join("print-partner.db"))),
        "39"
    );
    assert_eq!(
        scan_objects(&directory.join("print-partner.db")),
        Vec::<String>::new()
    );
}

// Retry migrates to whatever the seed reached, so schema bumps need no edit here.
fn assert_retry_succeeds(fixture: &Fixture, directory: &Path) {
    let database = directory.join("print-partner.db");
    Connection::open(&database)
        .unwrap()
        .execute_batch("DROP TRIGGER reject_schema40")
        .unwrap();
    let input = snapshot(read_only(&database));
    let (owner, ready) = WriterOwner::open(directory, Limits::default()).unwrap();
    owner.shutdown().unwrap();
    assert_eq!(
        (ready.previous_version, ready.version),
        (39, fixture.current_version)
    );
    assert_eq!(
        ready.backup,
        // The owner reports the canonical data directory, which on Windows
        // is a verbatim path without 8.3 short names.
        Some(
            std::fs::canonicalize(directory)
                .unwrap()
                .join("backups")
                .join("pre-schema40.db")
        )
    );
    assert_eq!(
        version(read_only(&database)),
        fixture.current_version.to_string()
    );
    assert_eq!(
        snapshot(immutable(&directory.join("backups/pre-schema40.db"))),
        input
    );
    assert!(!directory.join("backups/pre-schema-run").exists());
    let at40 = directory.join("backups/pre-schema41.db");
    assert_eq!(version(immutable(&at40)), "40");
    for path in [&database, &at40] {
        let objects = scan_objects(path);
        assert!(objects.contains(&"source_scan_executions".to_owned()));
        assert!(objects.contains(&"source_scan_execution_receipt".to_owned()));
        assert_eq!(
            read_only(path)
                .query_row("SELECT hex(value) FROM gre710_probe", [], |row| row
                    .get::<_, String>(0))
                .unwrap(),
            "00FF01"
        );
    }
}

#[test]
fn failed_schema40_rolls_back_from39_and_retries() {
    let fixture = Fixture::new();
    let directory = fixture.input(39);
    let database = directory.join("print-partner.db");
    let input = snapshot(read_only(&database));
    let backup = directory.join("backups/pre-schema40.db");
    assert_stamp_refused(&directory);
    assert_eq!(snapshot(read_only(&database)), input);
    assert_eq!(snapshot(immutable(&backup)), input);
    let backups = backup_bytes(&directory);
    assert_eq!(
        backups.keys().collect::<Vec<_>>(),
        ["pre-schema-run", "pre-schema40.db"]
    );
    assert_eq!(backups["pre-schema-run"], b"pre-schema40.db\n");

    assert_stamp_refused(&directory);
    assert_eq!(snapshot(read_only(&database)), input);
    assert_eq!(snapshot(immutable(&backup)), input);
    assert_eq!(backup_bytes(&directory), backups);
    assert_retry_succeeds(&fixture, &directory);
}

#[test]
fn failed_schema40_preserves_committed38_and39_and_their_backups() {
    let fixture = Fixture::new();
    let directory = fixture.input(37);
    let at38 = fixture.input(38);
    let at39 = fixture.input(39);
    let expected = [
        (38, snapshot(read_only(&directory.join("print-partner.db")))),
        (39, snapshot(read_only(&at38.join("print-partner.db")))),
        (40, snapshot(read_only(&at39.join("print-partner.db")))),
    ];
    assert_stamp_refused(&directory);
    assert_eq!(
        snapshot(read_only(&directory.join("print-partner.db"))),
        expected[2].1
    );
    for (step, input) in &expected {
        let backup = directory.join(format!("backups/pre-schema{step}.db"));
        assert_eq!(version(immutable(&backup)), (step - 1).to_string());
        assert_eq!(&snapshot(immutable(&backup)), input);
    }
    let backups = backup_bytes(&directory);
    assert_eq!(
        backups.keys().collect::<Vec<_>>(),
        [
            "pre-schema-run",
            "pre-schema38.db",
            "pre-schema39.db",
            "pre-schema40.db"
        ]
    );
    assert_eq!(backups["pre-schema-run"], b"pre-schema38.db\n");
    assert_stamp_refused(&directory);
    assert_eq!(
        snapshot(read_only(&directory.join("print-partner.db"))),
        expected[2].1
    );
    assert_eq!(backup_bytes(&directory), backups);
    assert_retry_succeeds(&fixture, &directory);
    let retried = backup_bytes(&directory);
    for name in ["pre-schema38.db", "pre-schema39.db"] {
        assert_eq!(retried[name], backups[name]);
    }
}
