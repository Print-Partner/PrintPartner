#![cfg(unix)]
//! T-8-archive (GRE-403): coherent app backup archives in Node's format 2.

use flate2::{Compression, read::GzDecoder, write::GzEncoder};
use pp_storage::{
    Limits, Setting, SettingCommand, SettingSnapshot, WriterOwner,
    archive::{self, MetadataScope, Request, Scope},
};
use rusqlite::Connection;
use std::{
    io::Read,
    os::unix::fs::symlink,
    path::{Path, PathBuf},
    sync::atomic::AtomicBool,
    time::Duration,
};
use tar::{EntryType, Header};

#[path = "common/process.rs"]
mod child_process;
#[path = "common/scratch.rs"]
mod scratch;
use scratch::Scratch;

fn set(owner: &WriterOwner, key: &str, value: &str) {
    owner
        .client()
        .submit(
            SettingCommand::Set(Setting {
                tenant: "test".into(),
                key: key.into(),
                value: value.into(),
            }),
            &AtomicBool::new(false),
            Duration::from_secs(5),
        )
        .unwrap()
        .recv()
        .unwrap()
        .unwrap();
}

fn open(data: &Path) -> WriterOwner {
    WriterOwner::open(data, Limits::default()).unwrap().0
}

/// Every archive path with its entry type, in archive order.
fn listing(path: &Path) -> Vec<(String, EntryType)> {
    let mut reader = tar::Archive::new(GzDecoder::new(std::fs::File::open(path).unwrap()));
    reader
        .entries()
        .unwrap()
        .map(|entry| {
            let entry = entry.unwrap();
            let name = String::from_utf8(entry.path_bytes().into_owned()).unwrap();
            (
                name.trim_end_matches('/').to_owned(),
                entry.header().entry_type(),
            )
        })
        .collect()
}

fn read_entry(path: &Path, name: &str) -> Vec<u8> {
    let mut reader = tar::Archive::new(GzDecoder::new(std::fs::File::open(path).unwrap()));
    for entry in reader.entries().unwrap() {
        let mut entry = entry.unwrap();
        if entry.path_bytes().as_ref() == name.as_bytes() {
            let mut bytes = Vec::new();
            entry.read_to_end(&mut bytes).unwrap();
            return bytes;
        }
    }
    panic!("{name} missing from archive");
}

fn stored(database: &Path, key: &str) -> Option<String> {
    let connection = Connection::open(database).unwrap();
    connection
        .query_row(
            "SELECT value FROM app_settings WHERE tenant_id='test' AND key=?",
            [key],
            |row| row.get(0),
        )
        .ok()
}

fn residue(directory: &Path) -> Vec<String> {
    std::fs::read_dir(directory)
        .unwrap()
        .map(|entry| entry.unwrap().file_name().into_string().unwrap())
        .collect()
}

#[test]
fn ticket_t_8_archive_snapshots_database_and_roots_in_node_format() {
    let data = Scratch::new("pp-t8-archive-data");
    let out = Scratch::new("pp-t8-archive-out");
    let owner = open(&data);
    set(&owner, "before", "committed");
    std::fs::create_dir_all(data.join("repos/source-a/rev-1")).unwrap();
    std::fs::write(data.join("repos/source-a/rev-1/part.stl"), b"solid part").unwrap();
    std::fs::create_dir_all(data.join("manifests")).unwrap();
    std::fs::write(data.join("manifests/plan.yaml"), b"plates: []").unwrap();
    std::fs::write(data.join("custom_filaments.json"), b"[]").unwrap();
    std::fs::write(out.join("outside.txt"), b"outside secret").unwrap();
    symlink(out.join("outside.txt"), data.join("repos/source-a/escape")).unwrap();
    std::fs::write(data.join("not-a-root.txt"), b"never archived").unwrap();

    let target = out.join("app.tar.gz");
    let metadata = owner
        .create_archive(&target, Request::new("3.9.0", Scope::Full))
        .unwrap();
    assert_eq!(metadata.scope, MetadataScope::Full);
    assert_eq!(
        archive::validate(&target, archive::Limits::default()).unwrap(),
        metadata
    );

    let entries = listing(&target);
    let names: Vec<_> = entries.iter().map(|(name, _)| name.as_str()).collect();
    assert_eq!(
        names,
        [
            "print-partner.db",
            "backup-metadata.json",
            "repos",
            "repos/source-a",
            "repos/source-a/rev-1",
            "repos/source-a/rev-1/part.stl",
            "sources",
            "exports",
            "thumbs",
            "covers",
            "manifests",
            "manifests/plan.yaml",
            "custom_filaments.json",
        ]
    );
    assert!(
        entries
            .iter()
            .all(|(_, kind)| matches!(kind, EntryType::Regular | EntryType::Directory))
    );
    assert_eq!(
        read_entry(&target, "repos/source-a/rev-1/part.stl"),
        b"solid part"
    );

    let json: serde_json::Value =
        serde_json::from_slice(&read_entry(&target, "backup-metadata.json")).unwrap();
    assert_eq!(json["version"], "2");
    assert_eq!(json["formatVersion"], 2);
    assert_eq!(json["appVersion"], "3.9.0");
    assert_eq!(json["scope"]["kind"], "full");
    assert_eq!(
        json["scope"]["includedRoots"],
        serde_json::json!([
            "repos",
            "sources",
            "exports",
            "thumbs",
            "covers",
            "assistant-domain",
            "manifests",
            "custom_filaments.json",
            "kit-catalog.json",
            "kit-catalog.yaml",
            "path-hints.yaml"
        ])
    );

    let restored = out.join("restored.db");
    std::fs::write(&restored, read_entry(&target, "print-partner.db")).unwrap();
    assert_eq!(stored(&restored, "before").as_deref(), Some("committed"));
    // The work directory and temporary archive are gone.
    let mut left = residue(&out);
    left.sort();
    assert_eq!(left, ["app.tar.gz", "outside.txt", "restored.db"]);

    // The writer keeps serving after the archive.
    set(&owner, "after", "works");
    assert_eq!(
        owner
            .client()
            .reader(Duration::ZERO)
            .unwrap()
            .snapshot("test", "after")
            .unwrap(),
        SettingSnapshot::Stored {
            value: "works".into()
        }
    );

    // After a restart the data is intact and a second archive succeeds.
    owner.shutdown().unwrap();
    let owner = open(&data);
    for (key, value) in [("before", "committed"), ("after", "works")] {
        assert_eq!(
            owner
                .client()
                .reader(Duration::ZERO)
                .unwrap()
                .snapshot("test", key)
                .unwrap(),
            SettingSnapshot::Stored {
                value: value.into()
            }
        );
    }
    let second = out.join("after-restart.tar.gz");
    owner
        .create_archive(&second, Request::new("3.9.0", Scope::Full))
        .unwrap();
    let restored = out.join("restored-after-restart.db");
    std::fs::write(&restored, read_entry(&second, "print-partner.db")).unwrap();
    assert_eq!(stored(&restored, "after").as_deref(), Some("works"));
    owner.shutdown().unwrap();
}

#[test]
fn ticket_t_8_archive_stops_walking_at_the_entry_limit() {
    use std::os::unix::fs::PermissionsExt;
    let data = Scratch::new("pp-t8-archive-data");
    let out = Scratch::new("pp-t8-archive-out");
    let owner = open(&data);
    for index in 0..10 {
        std::fs::write(data.join(format!("repos/a{index}")), b"x").unwrap();
    }
    // A walk that ran past the limit would fail on this unreadable directory
    // with a different error.
    std::fs::create_dir(data.join("repos/z-unreadable")).unwrap();
    std::fs::set_permissions(
        data.join("repos/z-unreadable"),
        std::fs::Permissions::from_mode(0o000),
    )
    .unwrap();
    let mut request = Request::new("3.9.0", Scope::Full);
    // Metadata, database, `repos`, `repos/a0` and `repos/a1`.
    request.limits.max_entries = 5;
    let error = owner
        .create_archive(&out.join("capped.tar.gz"), request)
        .unwrap_err()
        .to_string();
    assert!(error.contains("entry limit exceeded (5)"), "{error}");
    assert!(residue(&out).is_empty());
    std::fs::set_permissions(
        data.join("repos/z-unreadable"),
        std::fs::Permissions::from_mode(0o700),
    )
    .unwrap();
    owner.shutdown().unwrap();
}

#[test]
fn ticket_t_8_archive_database_only_scope_omits_roots() {
    let data = Scratch::new("pp-t8-archive-data");
    let out = Scratch::new("pp-t8-archive-out");
    let owner = open(&data);
    std::fs::create_dir_all(data.join("repos")).unwrap();
    std::fs::write(data.join("repos/file"), b"x").unwrap();
    let target = out.join("db-only.tar.gz");
    owner
        .create_archive(&target, Request::new("3.9.0", Scope::DatabaseOnly))
        .unwrap();
    let names: Vec<_> = listing(&target).into_iter().map(|(name, _)| name).collect();
    assert_eq!(names, ["print-partner.db", "backup-metadata.json"]);
    assert!(data.join("repos/file").exists());
    assert_eq!(
        archive::validate(&target, archive::Limits::default())
            .unwrap()
            .scope,
        MetadataScope::DatabaseOnly
    );
    owner.shutdown().unwrap();
}

#[test]
fn ticket_t_8_archive_refuses_unsafe_roots_space_and_destinations() {
    let data = Scratch::new("pp-t8-archive-data");
    let out = Scratch::new("pp-t8-archive-out");
    let owner = open(&data);
    let refuse = |target: &Path, request: Request| {
        owner
            .create_archive(target, request)
            .unwrap_err()
            .to_string()
    };

    std::fs::create_dir_all(out.join("elsewhere")).unwrap();
    std::fs::remove_dir(data.join("repos")).unwrap();
    symlink(out.join("elsewhere"), data.join("repos")).unwrap();
    assert!(
        refuse(&out.join("a.tar.gz"), Request::new("3.9.0", Scope::Full))
            .contains("Backup root cannot be a symbolic link: repos")
    );
    std::fs::remove_file(data.join("repos")).unwrap();

    std::fs::remove_dir(data.join("sources")).unwrap();
    std::fs::write(data.join("sources"), b"a file where a directory belongs").unwrap();
    assert!(
        refuse(&out.join("a.tar.gz"), Request::new("3.9.0", Scope::Full))
            .contains("Backup root has the wrong file type: sources")
    );
    std::fs::remove_file(data.join("sources")).unwrap();

    let mut no_space = Request::new("3.9.0", Scope::Full);
    no_space.reserve_bytes = u64::MAX / 2;
    assert!(refuse(&out.join("a.tar.gz"), no_space).contains("Insufficient disk space for backup"));

    let mut tiny = Request::new("3.9.0", Scope::Full);
    tiny.limits.max_total_bytes = 1024;
    assert!(refuse(&out.join("a.tar.gz"), tiny).contains("decompressed byte limit exceeded"));

    assert!(
        refuse(
            &data.join("inside.tar.gz"),
            Request::new("3.9.0", Scope::Full)
        )
        .contains("inside managed storage")
    );
    std::fs::write(out.join("existing.tar.gz"), b"keep me").unwrap();
    assert!(
        refuse(
            &out.join("existing.tar.gz"),
            Request::new("3.9.0", Scope::Full)
        )
        .contains("must be a new file")
    );
    assert_eq!(
        std::fs::read(out.join("existing.tar.gz")).unwrap(),
        b"keep me"
    );
    let mut left = residue(&out);
    left.sort();
    assert_eq!(left, ["elsewhere", "existing.tar.gz"]);
    owner.shutdown().unwrap();
}

struct Crafted {
    builder: tar::Builder<GzEncoder<Vec<u8>>>,
}

impl Crafted {
    fn new() -> Self {
        Self {
            builder: tar::Builder::new(GzEncoder::new(Vec::new(), Compression::default())),
        }
    }

    /// Writes `name` into the raw header so hostile paths survive.
    fn raw(mut self, name: &[u8], kind: EntryType, data: &[u8]) -> Self {
        let mut header = Header::new_old();
        header.as_old_mut().name[..name.len()].copy_from_slice(name);
        header.set_entry_type(kind);
        header.set_mode(0o644);
        header.set_size(if kind == EntryType::Regular {
            data.len() as u64
        } else {
            0
        });
        if kind == EntryType::Symlink || kind == EntryType::Link {
            header.set_link_name("/etc/passwd").unwrap();
        }
        header.set_cksum();
        let data = if kind == EntryType::Regular {
            data
        } else {
            &[]
        };
        self.builder.append(&header, data).unwrap();
        self
    }

    fn file(self, name: &str, data: &[u8]) -> Self {
        self.raw(name.as_bytes(), EntryType::Regular, data)
    }

    fn dir(self, name: &str) -> Self {
        self.raw(name.as_bytes(), EntryType::Directory, &[])
    }

    fn base(self, metadata: &serde_json::Value, database: &[u8]) -> Self {
        self.file("backup-metadata.json", metadata.to_string().as_bytes())
            .file("print-partner.db", database)
    }

    fn write(self, path: &Path) -> PathBuf {
        let bytes = self.builder.into_inner().unwrap().finish().unwrap();
        std::fs::write(path, bytes).unwrap();
        path.to_owned()
    }
}

fn sqlite_bytes(directory: &Path, wal: bool) -> (Vec<u8>, Vec<u8>) {
    let path = directory.join(format!("seed-{wal}.db"));
    let connection = Connection::open(&path).unwrap();
    connection
        .pragma_update(None, "journal_mode", "WAL")
        .unwrap();
    connection
        .pragma_update(None, "wal_autocheckpoint", 0)
        .unwrap();
    connection
        .execute_batch("CREATE TABLE app_settings(tenant_id,key,value); INSERT INTO app_settings VALUES('test','legacy','v1');")
        .unwrap();
    let main = std::fs::read(&path).unwrap();
    let wal_bytes = std::fs::read(directory.join(format!("seed-{wal}.db-wal"))).unwrap();
    std::mem::forget(connection);
    (main, wal_bytes)
}

fn v2(kind: &str, roots: &[&str]) -> serde_json::Value {
    serde_json::json!({
        "version": "2",
        "createdAt": "2026-10-29T12:00:00.000Z",
        "appVersion": "3.8.0",
        "formatVersion": 2,
        "scope": { "kind": kind, "includedRoots": roots },
    })
}

#[test]
fn ticket_t_8_archive_accepts_node_format_one_with_wal_and_pre_update_archives() {
    let dir = Scratch::new("pp-t8-archive-node");
    let (main, wal) = sqlite_bytes(&dir, true);
    let legacy = serde_json::json!({
        "version": "1",
        "createdAt": "2026-01-02T03:04:05.678Z",
        "appVersion": "3.3.0",
        "formatVersion": 1,
    });
    let path = Crafted::new()
        .base(&legacy, &main)
        .file("print-partner.db-wal", &wal)
        .dir("repos/")
        .file("repos/a.stl", b"x")
        .write(&dir.join("legacy.tar.gz"));
    let metadata = archive::validate(&path, archive::Limits::default()).unwrap();
    assert_eq!(metadata.scope, MetadataScope::Legacy);
    assert_eq!(metadata.app_version, "3.3.0");

    // Node's pre-update backup is database-only format 2.
    let path = Crafted::new()
        .base(&v2("database-only", &[]), &main)
        .file("print-partner.db-wal", &wal)
        .write(&dir.join("print-partner-pre-update-to-3.9.0-schema-34-to-41.tar.gz"));
    assert_eq!(
        archive::validate(&path, archive::Limits::default())
            .unwrap()
            .scope,
        MetadataScope::DatabaseOnly
    );
}

#[test]
fn ticket_t_8_archive_validation_rejects_traversal_links_and_bad_metadata() {
    let dir = Scratch::new("pp-t8-archive-hostile");
    let (main, _) = sqlite_bytes(&dir, true);
    let full_roots = [
        "repos",
        "sources",
        "exports",
        "thumbs",
        "covers",
        "assistant-domain",
        "manifests",
        "custom_filaments.json",
        "kit-catalog.json",
        "kit-catalog.yaml",
        "path-hints.yaml",
    ];
    let full = v2("full", &full_roots);
    let check = |name: &str, crafted: Crafted, expected: &str| {
        let path = crafted.write(&dir.join(format!("{name}.tar.gz")));
        let error = format!(
            "{:#}",
            archive::validate(&path, archive::Limits::default()).unwrap_err()
        );
        assert!(error.contains(expected), "{name}: {error}");
    };
    check(
        "parent",
        Crafted::new()
            .base(&full, &main)
            .file("repos/../../etc/x", b"x"),
        "Unsafe backup entry path",
    );
    check(
        "absolute",
        Crafted::new().base(&full, &main).file("/etc/passwd", b"x"),
        "Unsafe backup entry path",
    );
    check(
        "drive",
        Crafted::new().base(&full, &main).file("C:repos", b"x"),
        "Unsafe backup entry path",
    );
    check(
        "backslash",
        Crafted::new().base(&full, &main).file("repos\\x", b"x"),
        "Unsafe backup entry path",
    );
    check(
        "dot",
        Crafted::new().base(&full, &main).file("repos/./x", b"x"),
        "Unsafe backup entry path",
    );
    check(
        "empty-part",
        Crafted::new().base(&full, &main).file("repos//x", b"x"),
        "Unsafe backup entry path",
    );
    check(
        "symlink",
        Crafted::new()
            .base(&full, &main)
            .dir("repos/")
            .raw(b"repos/link", EntryType::Symlink, &[]),
        "Unsupported backup entry type",
    );
    check(
        "hardlink",
        Crafted::new()
            .base(&full, &main)
            .dir("repos/")
            .raw(b"repos/link", EntryType::Link, &[]),
        "Unsupported backup entry type",
    );
    check(
        "core-symlink",
        Crafted::new()
            .file("backup-metadata.json", full.to_string().as_bytes())
            .raw(b"print-partner.db", EntryType::Symlink, &[]),
        "must be a regular file",
    );
    check(
        "file-root-dir",
        Crafted::new()
            .base(&full, &main)
            .dir("custom_filaments.json/"),
        "must be a regular file",
    );
    check(
        "dir-root-file",
        Crafted::new().base(&full, &main).file("repos", b"x"),
        "Backup directory root has invalid type",
    );
    check(
        "unexpected",
        Crafted::new()
            .base(&full, &main)
            .file("secrets/token", b"x"),
        "Unexpected backup entry",
    );
    check(
        "duplicate",
        Crafted::new()
            .base(&full, &main)
            .file("print-partner.db", &main),
        "Duplicate backup entry",
    );
    check(
        "out-of-scope",
        Crafted::new()
            .base(&v2("database-only", &[]), &main)
            .dir("repos/"),
        "outside the declared scope",
    );
    check(
        "no-metadata",
        Crafted::new().file("print-partner.db", &main),
        "missing backup-metadata.json",
    );
    check(
        "no-database",
        Crafted::new().file("backup-metadata.json", full.to_string().as_bytes()),
        "missing print-partner.db",
    );
    let mut newer = full.clone();
    newer["formatVersion"] = 3.into();
    newer["version"] = "3".into();
    check(
        "newer",
        Crafted::new().base(&newer, &main),
        "newer than supported 2",
    );
    let mut mismatched = full.clone();
    mismatched["version"] = "1".into();
    check(
        "mismatch",
        Crafted::new().base(&mismatched, &main),
        "version fields do not match",
    );
    check(
        "partial-full",
        Crafted::new().base(&v2("full", &["repos"]), &main),
        "scope roots do not match its kind",
    );
    check(
        "bad-sqlite",
        Crafted::new().base(&full, b"not a database at all"),
        "invalid SQLite database",
    );
    // Shorter than the 16-byte header: a short read is a refusal, not an I/O error.
    check(
        "tiny-sqlite",
        Crafted::new().base(&full, b"SQLi"),
        "invalid SQLite database",
    );
    let mut corrupt = main.clone();
    corrupt[100..].iter_mut().for_each(|byte| *byte = 0xff);
    check(
        "corrupt-sqlite",
        Crafted::new().base(&full, &corrupt),
        "integrity_check failed",
    );

    let limited = |name: &str, limits: archive::Limits, expected: &str| {
        let path = Crafted::new()
            .base(&full, &main)
            .dir("repos/")
            .file("repos/a", &[0u8; 4096])
            .write(&dir.join(format!("{name}.tar.gz")));
        let error = archive::validate(&path, limits).unwrap_err().to_string();
        assert!(error.contains(expected), "{name}: {error}");
    };
    let defaults = archive::Limits::default();
    limited(
        "entries",
        archive::Limits {
            max_entries: 3,
            ..defaults
        },
        "entry limit exceeded",
    );
    limited(
        "entry-bytes",
        archive::Limits {
            max_entry_bytes: 4095,
            ..defaults
        },
        "exceeds decompressed size limit",
    );
    limited(
        "total",
        archive::Limits {
            max_total_bytes: 4096,
            ..defaults
        },
        "decompressed byte limit exceeded",
    );
    limited(
        "ratio",
        archive::Limits {
            max_decompression_ratio: 1,
            ..defaults
        },
        "decompression ratio",
    );
    // A hostile archive never writes outside the private validation copy.
    assert!(!dir.join("etc").exists() && !Path::new("/etc/x").exists());
}

#[test]
fn ticket_t_8_archive_node_validates_rust_archives() {
    let data = Scratch::new("pp-t8-archive-data");
    let out = Scratch::new("pp-t8-archive-out");
    let owner = open(&data);
    set(&owner, "node", "reads this");
    std::fs::write(data.join("repos/part.stl"), b"solid part").unwrap();
    owner
        .create_archive(&out.join("full.tar.gz"), Request::new("3.9.0", Scope::Full))
        .unwrap();
    owner
        .create_archive(
            &out.join("db-only.tar.gz"),
            Request::new("3.9.0", Scope::DatabaseOnly),
        )
        .unwrap();
    owner.shutdown().unwrap();

    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../..");
    let service = root.join("web/apps/server/src/services/backup-restore.ts");
    let script = out.join("validate.mts");
    std::fs::write(
        &script,
        format!(
            "import {{ validateBackup }} from {service:?};\n\
             for (const path of process.argv.slice(2)) {{\n\
               const metadata = await validateBackup(path);\n\
               console.log(metadata.scope.kind, metadata.scope.includedRoots.length);\n\
             }}\n",
            service = service.to_str().unwrap()
        ),
    )
    .unwrap();
    let stdout = run_node(
        &script,
        &[&out.join("full.tar.gz"), &out.join("db-only.tar.gz")],
    );
    assert_eq!(stdout, "full 11\ndatabase-only 0\n");
}

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../..")
}

fn backup_service() -> String {
    repo_root()
        .join("web/apps/server/src/services/backup-restore.ts")
        .to_str()
        .unwrap()
        .to_owned()
}

/// Runs a TypeScript script with the pinned parity Node and tsx.
fn run_node(script: &Path, args: &[&Path]) -> String {
    let root = repo_root();
    let node = std::env::var_os("PP_SCHEMA_PARITY_NODE").unwrap_or_else(|| "node".into());
    let output = child_process::command(&node)
        .current_dir(&root)
        .arg("--conditions=development")
        .arg("--import")
        .arg(root.join("web/node_modules/tsx/dist/loader.mjs"))
        .arg(script)
        .args(args)
        .output()
        .expect("run Node");
    assert!(
        output.status.success(),
        "Node script failed.\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );
    String::from_utf8(output.stdout).unwrap()
}

#[test]
fn ticket_t_8_archive_validates_archives_from_node_create_backup() {
    let dir = Scratch::new("pp-t8-archive-node-create");
    let data = dir.join("data");
    std::fs::create_dir(&data).unwrap();
    let output = dir.join("node.tar.gz");
    let script = dir.join("create.mts");
    // A path over 100 bytes makes node-tar write a PAX header.
    let long = format!("repos/{}/{}/part.stl", "a".repeat(60), "b".repeat(60));
    std::fs::write(
        &script,
        format!(
            r#"import {{ createRequire }} from "node:module";
import {{ mkdirSync, writeFileSync }} from "node:fs";
import {{ dirname, join }} from "node:path";
import {{ createBackup }} from {service:?};
const Database = createRequire({service:?})("better-sqlite3");
const [dataDir, output] = process.argv.slice(2);
const dbPath = join(dataDir, "print-partner.db");
const sqlite = new Database(dbPath);
sqlite.pragma("journal_mode = WAL");
sqlite.exec("CREATE TABLE app_settings(tenant_id TEXT, key TEXT, value TEXT); INSERT INTO app_settings VALUES('test', 'node', 'wrote this')");
const long = join(dataDir, {long:?});
mkdirSync(dirname(long), {{ recursive: true }});
writeFileSync(long, "solid node");
mkdirSync(join(dataDir, "manifests"));
writeFileSync(join(dataDir, "manifests", "plan.yaml"), "plates: []");
writeFileSync(join(dataDir, "custom_filaments.json"), "[]");
await createBackup(
  {{
    dbPath,
    ping: () => sqlite.prepare("SELECT 1").get() !== undefined,
    backupToFile: (destination) => sqlite.backup(destination).then(() => undefined),
  }},
  dataDir,
  "3.8.0",
  output,
);
sqlite.close();
console.log("created");
"#,
            service = backup_service(),
        ),
    )
    .unwrap();
    assert_eq!(run_node(&script, &[&data, &output]), "created\n");

    let metadata = archive::validate(&output, archive::Limits::default()).unwrap();
    assert_eq!(metadata.scope, MetadataScope::Full);
    assert_eq!(metadata.format_version, 2);
    assert_eq!(metadata.app_version, "3.8.0");
    let entries = listing(&output);
    assert!(
        entries
            .iter()
            .all(|(_, kind)| matches!(kind, EntryType::Regular | EntryType::Directory)),
        "{entries:?}"
    );
    for expected in [
        "print-partner.db",
        "backup-metadata.json",
        "repos",
        long.as_str(),
        "manifests/plan.yaml",
        "custom_filaments.json",
    ] {
        assert!(
            entries.iter().any(|(name, _)| name == expected),
            "{expected} missing from {entries:?}"
        );
    }
    assert_eq!(read_entry(&output, &long), b"solid node");
    let restored = dir.join("restored.db");
    std::fs::write(&restored, read_entry(&output, "print-partner.db")).unwrap();
    assert_eq!(stored(&restored, "node").as_deref(), Some("wrote this"));
}
