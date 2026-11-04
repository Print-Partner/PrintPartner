use pp_storage::{
    Limits, Setting, SettingCommand, SettingSnapshot, WriterOwner, lease::StorageLease,
};
use std::os::unix::fs::DirBuilderExt;
use std::{
    io::{BufRead, BufReader, Read, Write},
    path::{Path, PathBuf},
    process::{Child, Stdio},
    sync::mpsc,
    thread,
    time::Duration,
};

#[path = "common/process.rs"]
mod child_process;

struct StopProcess(Option<Child>);

impl Drop for StopProcess {
    fn drop(&mut self) {
        if let Some(child) = self.0.as_mut() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

// macOS clocks tick in microseconds, so parallel tests can read the same time.
fn temp_dir(name: &str) -> PathBuf {
    static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let serial = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let path = std::env::temp_dir().join(format!(
        "pp-t7-{name}-{}-{nanos}-{serial}",
        std::process::id()
    ));
    std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(&path)
        .unwrap();
    path
}

fn read_event_line<R: BufRead + Send + 'static>(mut stdout: R) -> (String, R) {
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        let mut line = String::new();
        let result = stdout.read_line(&mut line);
        let _ = tx.send((result.map(|_| ()), line, stdout));
    });
    let (result, line, stdout) = rx
        .recv_timeout(Duration::from_secs(30))
        .expect("node probe event timed out");
    result.expect("node probe stdout");
    (line, stdout)
}

fn read_ready<R: BufRead + Send + 'static>(mut stdout: R) -> (String, R) {
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        let mut line = String::new();
        let result = stdout.read_line(&mut line);
        let _ = tx.send((result.map(|_| ()), line, stdout));
    });
    let (result, line, stdout) = rx
        .recv_timeout(Duration::from_secs(30))
        .expect("storage-fixture ready timed out");
    result.expect("storage-fixture stdout");
    assert!(!line.is_empty(), "storage-fixture exited before ready");
    (line, stdout)
}

fn snapshot(dir: &Path, tenant: &str, key: &str) -> SettingSnapshot {
    let (owner, _) = WriterOwner::open(dir, Limits::default()).unwrap();
    let read = owner.client().reader(Duration::ZERO).unwrap();
    let value = read.snapshot(tenant, key).unwrap();
    drop(read);
    owner.shutdown().unwrap();
    value
}

#[test]
fn ticket_t_7_writer_reopens_after_clean_process_exit() {
    let dir = temp_dir("clean");
    let bin = child_process::bin_exe("storage-fixture");
    let mut child = child_process::command(bin)
        .args([
            "settings",
            dir.to_str().unwrap(),
            "2026-10-02T00:00:00.000Z",
        ])
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let stdout = child.stdout.take().unwrap();
    let mut process = StopProcess(Some(child));
    let (ready, _) = read_ready(BufReader::new(stdout));
    assert!(ready.contains("\"version\""), "{ready}");
    let status = process.0.as_mut().unwrap().wait().unwrap();
    assert!(status.success(), "{status}");
    process.0.take();
    assert_eq!(
        snapshot(&dir, "tenant-a", "discord_notify_on_update"),
        SettingSnapshot::Stored { value: "1".into() }
    );
    assert_eq!(
        snapshot(&dir, "tenant-b", "discord_notify_on_update"),
        SettingSnapshot::Stored { value: "0".into() }
    );
    assert_eq!(
        snapshot(&dir, "tenant-a", "discord_notify_on_sync"),
        SettingSnapshot::Stored { value: "1".into() }
    );
    let _ = std::fs::remove_file(dir.with_extension("backup.db"));
    std::fs::remove_dir_all(&dir).unwrap();
}

const NODE_PROBE: &str = r#"
import { readFileSync, writeSync } from "node:fs";
import Database from "better-sqlite3";
import { SqliteDatabase } from CLIENT_SPECIFIER;
import { admitDesktopDataDirectory } from CREDENTIAL_SPECIFIER;

const mode = process.argv[2];
const dataDir = process.argv[3];
const dbPath = dataDir + "/print-partner.db";

function emit(value) {
  writeSync(1, JSON.stringify(value) + "\n");
}

function fingerprint() {
  const db = new Database(dbPath, { readonly: true, fileMustExist: true });
  try {
    const versionRow = db.prepare(
      "SELECT value FROM app_settings WHERE tenant_id = 'default' AND key = 'schema_version'",
    ).get();
    const sequences = db.prepare("SELECT name, seq FROM sqlite_sequence ORDER BY name").all();
    const counts = {
      printer_profiles: db.prepare("SELECT COUNT(*) AS n FROM printer_profiles").get().n,
      process_profiles: db.prepare("SELECT COUNT(*) AS n FROM process_profiles").get().n,
      filament_profiles: db.prepare("SELECT COUNT(*) AS n FROM filament_profiles").get().n,
    };
    return {
      version: versionRow && typeof versionRow.value === "string" ? versionRow.value : null,
      sequences,
      counts,
    };
  } finally {
    db.close();
  }
}

if (mode === "child" || mode === "bootstrap") admitDesktopDataDirectory(dataDir);

const before = mode === "bootstrap" ? null : fingerprint();
const database = new SqliteDatabase(dataDir);
try {
  database.connect();
  const after = fingerprint();
  database.close();
  emit({ event: "connected", before, after });
} catch (error) {
  let after = null;
  try { after = fingerprint(); } catch { after = null; }
  emit({
    event: "refused",
    message: error instanceof Error ? error.message : String(error),
    before,
    after,
  });
  process.exitCode = 2;
}
"#;

fn web_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../../web")
}

fn server_node(web: &Path) -> PathBuf {
    let mut candidates = Vec::new();
    if let Ok(path) = std::env::var("PP_SCHEMA_PARITY_NODE") {
        candidates.push(PathBuf::from(path));
    }
    if let Ok(path) = std::env::var("PP_PRODUCTION_NODE") {
        candidates.push(PathBuf::from(path));
    }
    candidates.push(PathBuf::from(
        "/var/lib/agent-fleet-runtime-owner/runtime-root/releases/node-project-22.23.3-bb2b080fb03ea1f2/bin/node",
    ));
    candidates.push(PathBuf::from(
        "/opt/agent-fleet/artifacts/.local/share/mise/installs/node/24.21.0/bin/node",
    ));
    candidates.push(PathBuf::from("node"));
    let mut errors = Vec::new();
    for candidate in candidates {
        let output = child_process::command(&candidate)
            .args([
                "--input-type=module",
                "-e",
                "import Database from 'better-sqlite3'; if (typeof Database !== 'function') process.exit(1);",
            ])
            .current_dir(web)
            .env("NODE_PATH", web.join("node_modules"))
            .output();
        match output {
            Ok(output) if output.status.success() => return candidate,
            Ok(output) => errors.push(format!(
                "{}: {}",
                candidate.display(),
                String::from_utf8_lossy(&output.stderr)
            )),
            Err(error) => errors.push(format!("{}: {error}", candidate.display())),
        }
    }
    panic!(
        "no Node can load better-sqlite3 from {}: {}",
        web.display(),
        errors.join(" | ")
    );
}

fn write_probe(web: &Path) -> PathBuf {
    let probe_dir = temp_dir("node-probe");
    std::os::unix::fs::symlink(web.join("node_modules"), probe_dir.join("node_modules")).unwrap();
    let probe = probe_dir.join("probe.mts");
    let client = serde_json::to_string(&web.join("apps/server/src/db/client.ts")).unwrap();
    let credential =
        serde_json::to_string(&web.join("apps/server/src/desktop-child-credential.ts")).unwrap();
    std::fs::write(
        &probe,
        NODE_PROBE
            .replace("CLIENT_SPECIFIER", &client)
            .replace("CREDENTIAL_SPECIFIER", &credential),
    )
    .unwrap();
    probe
}

fn remove_probe(probe: &Path) {
    let dir = probe.parent().unwrap();
    std::fs::remove_file(probe).unwrap();
    std::fs::remove_file(dir.join("node_modules")).unwrap();
    std::fs::remove_dir(dir).unwrap();
}

fn parse_event(line: &str, stderr: &str) -> serde_json::Value {
    let line = line.trim();
    serde_json::from_str(line).unwrap_or_else(|error| {
        panic!("node probe did not return an event ({error}): {line}\n{stderr}")
    })
}

fn run_node_probe(
    mode: &str,
    dir: &Path,
    web: &Path,
    node: &Path,
    probe: &Path,
) -> serde_json::Value {
    let tsx = web.join("node_modules/tsx/dist/cli.mjs");
    let mut child = child_process::command(node)
        .arg(&tsx)
        .arg(probe)
        .arg(mode)
        .arg(dir)
        .current_dir(web)
        .env("NODE_PATH", web.join("node_modules"))
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .stdin(Stdio::piped())
        .spawn()
        .unwrap();
    let stdout = child.stdout.take().unwrap();
    let stderr = child.stderr.take().unwrap();
    let stderr_thread = thread::spawn(move || {
        let mut buffer = String::new();
        let mut stderr = stderr;
        stderr.read_to_string(&mut buffer).unwrap();
        buffer
    });
    let mut process = StopProcess(Some(child));
    let stdin = process.0.as_mut().unwrap().stdin.take().unwrap();
    let (first, _stdout) = read_event_line(BufReader::new(stdout));
    if first.is_empty() {
        let status = process.0.as_mut().unwrap().wait().unwrap();
        process.0.take();
        let stderr = stderr_thread.join().unwrap();
        panic!("node probe exited {status} before an event:\n{stderr}");
    }
    drop(stdin);
    let event_line = first;
    let status = process.0.as_mut().unwrap().wait().unwrap();
    process.0.take();
    let stderr = stderr_thread.join().unwrap();
    let event = parse_event(&event_line, &stderr);
    if event["event"] == "connected" {
        assert!(status.success(), "{status} {event} {stderr}");
    } else if event["event"] == "refused" {
        assert_eq!(status.code(), Some(2), "{status} {event} {stderr}");
    }
    event
}

fn assert_stable_fingerprint(event: &serde_json::Value) {
    assert_eq!(event["before"], event["after"], "{event}");
    let names: Vec<&str> = event["before"]["sequences"]
        .as_array()
        .unwrap()
        .iter()
        .map(|row| row["name"].as_str().unwrap())
        .collect();
    for name in ["printer_profiles", "process_profiles", "filament_profiles"] {
        assert!(names.contains(&name), "{names:?}");
    }
}

fn stamp_schema_version(dir: &Path, version: &str) {
    let connection = rusqlite::Connection::open(dir.join("print-partner.db")).unwrap();
    connection
        .busy_timeout(std::time::Duration::from_secs(5))
        .unwrap();
    let changed = connection
        .execute(
            "UPDATE app_settings SET value = ?1 WHERE tenant_id = 'default' AND key = 'schema_version'",
            [version],
        )
        .unwrap();
    assert_eq!(changed, 1);
}

// Desktop startup holds StorageLease and lets its recorded Node child create
// the database, so the copy carries Node's schema rather than WriterOwner's.
fn desktop_lease_with_node_schema(
    dir: &Path,
    web: &Path,
    node: &Path,
    probe: &Path,
) -> StorageLease {
    let lease = StorageLease::acquire(dir).unwrap();
    let event = run_node_probe("bootstrap", dir, web, node, probe);
    assert_eq!(event["event"], "connected", "{event}");
    assert_eq!(event["after"]["version"], "34", "{event}");
    lease
}

fn release_owner(dir: &Path, owner: WriterOwner) {
    owner.shutdown().unwrap();
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn ticket_t_7_writer_node_refuses_migration_and_writes_on_a_rust_owned_copy() {
    let web = web_root();
    let node = server_node(&web);
    let probe = write_probe(&web);
    let dir = temp_dir("node-foreign");
    let (owner, ready) = WriterOwner::open(&dir, Limits::default()).unwrap();
    let event = run_node_probe("foreign", &dir, &web, &node, &probe);
    assert_eq!(event["event"], "refused", "{event}");
    assert!(
        event["message"]
            .as_str()
            .unwrap()
            .contains("will not migrate or write"),
        "{event}"
    );
    assert_eq!(
        event["before"]["version"],
        ready.version.to_string(),
        "{event}"
    );
    assert_stable_fingerprint(&event);
    owner.shutdown().unwrap();
    stamp_schema_version(&dir, "33");
    let reopened = run_node_probe("foreign", &dir, &web, &node, &probe);
    assert_eq!(reopened["event"], "connected", "{reopened}");
    assert_eq!(reopened["before"]["version"], "33", "{reopened}");
    assert_eq!(reopened["after"]["version"], "34", "{reopened}");
    std::fs::remove_dir_all(&dir).unwrap();
    remove_probe(&probe);
}

#[test]
fn ticket_t_7_writer_node_refuses_to_upgrade_an_unsupported_rust_owned_schema() {
    let web = web_root();
    let node = server_node(&web);
    let probe = write_probe(&web);
    let dir = temp_dir("node-unsupported");
    let (owner, _) = WriterOwner::open(&dir, Limits::default()).unwrap();
    stamp_schema_version(&dir, "33");
    let event = run_node_probe("foreign", &dir, &web, &node, &probe);
    assert_eq!(event["event"], "refused", "{event}");
    assert!(
        event["message"]
            .as_str()
            .unwrap()
            .contains("will not migrate or write"),
        "{event}"
    );
    assert_eq!(event["before"]["version"], "33", "{event}");
    assert_eq!(event["after"]["version"], "33", "{event}");
    assert_stable_fingerprint(&event);
    release_owner(&dir, owner);
    remove_probe(&probe);
}

#[test]
fn ticket_t_7_writer_node_child_reopens_a_current_schema_unchanged() {
    let web = web_root();
    let node = server_node(&web);
    let probe = write_probe(&web);
    let dir = temp_dir("node-child");
    let lease = desktop_lease_with_node_schema(&dir, &web, &node, &probe);
    let event = run_node_probe("child", &dir, &web, &node, &probe);
    assert_eq!(event["event"], "connected", "{event}");
    assert_eq!(event["before"]["version"], "34", "{event}");
    assert_stable_fingerprint(&event);
    lease.release();
    std::fs::remove_dir_all(&dir).unwrap();
    remove_probe(&probe);
}

#[test]
fn ticket_t_7_writer_node_child_bootstraps_a_lease_without_a_database() {
    let web = web_root();
    let node = server_node(&web);
    let probe = write_probe(&web);
    let dir = temp_dir("node-bootstrap");
    let lease = StorageLease::acquire(&dir).unwrap();
    assert!(!dir.join("print-partner.db").exists());
    let event = run_node_probe("bootstrap", &dir, &web, &node, &probe);
    assert_eq!(event["event"], "connected", "{event}");
    assert!(event["before"].is_null(), "{event}");
    assert_eq!(event["after"]["version"], "34", "{event}");
    let names: Vec<&str> = event["after"]["sequences"]
        .as_array()
        .unwrap()
        .iter()
        .map(|row| row["name"].as_str().unwrap())
        .collect();
    for name in ["printer_profiles", "process_profiles", "filament_profiles"] {
        assert!(names.contains(&name), "{event}");
    }
    lease.release();
    std::fs::remove_dir_all(&dir).unwrap();
    remove_probe(&probe);
}

struct Contender {
    process: StopProcess,
    stdout: BufReader<std::process::ChildStdout>,
}

fn lock_from_another_process(dir: &Path) -> Contender {
    let mut child = child_process::command(child_process::bin_exe("storage-fixture"))
        .args(["contend", dir.to_str().unwrap()])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .spawn()
        .unwrap();
    let stdout = BufReader::new(child.stdout.take().unwrap());
    let (locked, stdout) = read_ready(stdout);
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(&locked).unwrap(),
        serde_json::json!({"event": "locked"})
    );
    Contender {
        process: StopProcess(Some(child)),
        stdout,
    }
}

fn transfer_setting(key: &str, value: &str) -> SettingCommand {
    SettingCommand::Set(Setting {
        tenant: "tenant-a".into(),
        key: key.into(),
        value: value.into(),
    })
}

fn submit(owner: &WriterOwner, command: SettingCommand) -> pp_storage::WriteReply {
    owner
        .client()
        .submit(
            command,
            &std::sync::atomic::AtomicBool::new(false),
            Duration::from_secs(5),
        )
        .unwrap()
}

fn read_setting(owner: &WriterOwner, key: &str) -> SettingSnapshot {
    let read = owner.client().reader(Duration::ZERO).unwrap();
    read.snapshot("tenant-a", key).unwrap()
}

#[test]
fn ticket_t_7_writer_waits_for_another_process_and_never_interleaves() {
    let dir = temp_dir("contend-commit");
    let (owner, _) = WriterOwner::open(&dir, Limits::default()).unwrap();
    let mut contender = lock_from_another_process(&dir);
    assert_eq!(
        read_setting(&owner, "transfer-debit"),
        SettingSnapshot::Missing
    );

    let reply = submit(
        &owner,
        SettingCommand::CompareAndSet {
            setting: Setting {
                tenant: "tenant-a".into(),
                key: "transfer-debit".into(),
                value: "rust".into(),
            },
            expected: SettingSnapshot::Missing,
        },
    );
    assert!(
        matches!(
            reply.recv_timeout(Duration::from_millis(500)),
            Err(mpsc::RecvTimeoutError::Timeout)
        ),
        "writer ran inside another process's transaction"
    );

    let process = contender.process.0.as_mut().unwrap();
    writeln!(process.stdin.as_mut().unwrap(), "commit").unwrap();
    let (committed, _) = read_ready(contender.stdout);
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(&committed).unwrap(),
        serde_json::json!({"event": "committed"})
    );
    assert!(process.wait().unwrap().success());
    contender.process.0.take();

    // The compare runs after the whole foreign transaction, so it sees the debit.
    assert!(!reply.recv_timeout(Duration::from_secs(5)).unwrap().unwrap());
    let one = SettingSnapshot::Stored { value: "1".into() };
    assert_eq!(read_setting(&owner, "transfer-debit"), one);
    assert_eq!(read_setting(&owner, "transfer-credit"), one);
    eprintln!("foreign commit completed, waiting CAS returned false, both transfer rows present");
    owner.shutdown().unwrap();
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn ticket_t_7_writer_bounds_busy_and_rolls_back_a_killed_process() {
    assert_busy_timeout_and_killed_transaction_rollback(Duration::ZERO);
}

#[test]
fn ticket_t_7_writer_busy_timeout_survives_a_delayed_reply_collector() {
    assert_busy_timeout_and_killed_transaction_rollback(Duration::from_millis(10_200));
}

fn assert_write_lock_released(dir: &Path, bound: Duration) {
    let connection = rusqlite::Connection::open(dir.join("print-partner.db")).unwrap();
    connection.busy_timeout(Duration::ZERO).unwrap();
    let started = std::time::Instant::now();
    let released = loop {
        match connection.execute_batch("BEGIN IMMEDIATE") {
            Ok(()) => break started.elapsed(),
            Err(rusqlite::Error::SqliteFailure(code, _))
                if code.code == rusqlite::ErrorCode::DatabaseBusy =>
            {
                let waited = started.elapsed();
                assert!(
                    waited < bound,
                    "killed transaction kept the write lock for {waited:?}"
                );
                thread::sleep(Duration::from_millis(10));
            }
            Err(error) => panic!("{error:#}"),
        }
    };
    connection.execute_batch("ROLLBACK").unwrap();
    assert!(
        released < bound,
        "killed transaction kept the write lock for {released:?}"
    );
    eprintln!("killed transaction released the write lock after {released:?}");
}

fn assert_busy_timeout_and_killed_transaction_rollback(collect_after: Duration) {
    let dir = temp_dir("contend-kill");
    let (owner, _) = WriterOwner::open(&dir, Limits::default()).unwrap();
    let mut contender = lock_from_another_process(&dir);

    let started = std::time::Instant::now();
    let reply = submit(&owner, transfer_setting("rust-write", "blocked"));
    thread::sleep(collect_after);
    let busy = reply
        .recv_timeout(
            (started + Duration::from_secs(15))
                .saturating_duration_since(std::time::Instant::now()),
        )
        .expect("busy write did not reply within the caller's 15-second deadline")
        .unwrap_err();
    let waited = started.elapsed();
    eprintln!("monotonic admission busy refusal delivered after {waited:?}");
    assert!(
        matches!(
            busy.downcast_ref::<rusqlite::Error>(),
            Some(rusqlite::Error::SqliteFailure(code, _))
                if code.code == rusqlite::ErrorCode::DatabaseBusy
        ),
        "{busy:#}"
    );
    assert_eq!(read_setting(&owner, "rust-write"), SettingSnapshot::Missing);

    let process = contender.process.0.as_mut().unwrap();
    process.kill().unwrap();
    assert!(!process.wait().unwrap().success());
    contender.process.0.take();

    assert_eq!(read_setting(&owner, "rust-write"), SettingSnapshot::Missing);

    // The 2s bar is lock release. The later submit's synchronous=FULL commit
    // stays inside the caller's 5s wait and is not part of this measurement.
    assert_write_lock_released(&dir, Duration::from_secs(2));
    assert!(
        submit(&owner, transfer_setting("rust-write", "after-kill"))
            .recv_timeout(Duration::from_secs(5))
            .unwrap()
            .unwrap()
    );
    assert_eq!(
        read_setting(&owner, "transfer-debit"),
        SettingSnapshot::Missing
    );
    assert_eq!(
        read_setting(&owner, "transfer-credit"),
        SettingSnapshot::Missing
    );
    assert_eq!(
        read_setting(&owner, "rust-write"),
        SettingSnapshot::Stored {
            value: "after-kill".into()
        }
    );
    owner.shutdown().unwrap();
    assert_eq!(
        snapshot(&dir, "tenant-a", "transfer-debit"),
        SettingSnapshot::Missing
    );
    assert_eq!(
        snapshot(&dir, "tenant-a", "transfer-credit"),
        SettingSnapshot::Missing
    );
    assert_eq!(
        snapshot(&dir, "tenant-a", "rust-write"),
        SettingSnapshot::Stored {
            value: "after-kill".into()
        }
    );
    let (owner, _) = WriterOwner::open(&dir, Limits::default()).unwrap();
    assert_eq!(
        read_setting(&owner, "transfer-debit"),
        SettingSnapshot::Missing
    );
    assert_eq!(
        read_setting(&owner, "transfer-credit"),
        SettingSnapshot::Missing
    );
    assert_eq!(
        read_setting(&owner, "rust-write"),
        SettingSnapshot::Stored {
            value: "after-kill".into()
        }
    );
    owner.shutdown().unwrap();
    eprintln!(
        "foreign holder killed and reaped, next command committed, both abandoned rows absent, after-kill value persisted through owner reopen"
    );
    std::fs::remove_dir_all(&dir).unwrap();
    assert!(
        waited >= Duration::from_millis(4500),
        "admission and reply collection took {waited:?}, before the configured 5s timeout"
    );
    if collect_after.is_zero() {
        assert!(
            waited < Duration::from_secs(10),
            "admission and reply delivery took {waited:?}, outside the 10s host watchdog"
        );
    }
}
