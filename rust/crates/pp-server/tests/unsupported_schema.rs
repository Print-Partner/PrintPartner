//! The `pp-server` binary that ships refuses an unsupported schema on every
//! start: exit 78, one refusal on stderr, nothing on stdout, and the data
//! directory left as it was, however often a process manager starts it again.
//!
//! The binary under test is built the way `Dockerfile.rust` builds it:
//! `cargo build --locked --release -p pp-server`. The workspace test build of
//! `pp-server` cannot stand in for it. A workspace build turns on pp-storage's
//! `test-unmarked-writer` feature, which skips the ownership marker gate that
//! the image enforces.
#![cfg(all(unix, feature = "desktop_rebuild"))]

#[path = "../../pp-storage/tests/common/process.rs"]
mod child_process;
#[path = "../../pp-core/tests/support/release.rs"]
mod release;
#[path = "../../pp-storage/tests/common/scratch.rs"]
mod scratch;

use pp_core::{IncompatibleSchema, NODE_SCHEMA, OLDEST_UPGRADABLE, RUST_SCHEMA};
use pp_testkit::{Fixture, Installation, Marker, node_schema};
use std::{
    ffi::{OsStr, OsString},
    path::{Path, PathBuf},
    process::{Command, Output},
    time::Duration,
};

/// Starts per runtime mode and schema, standing in for a restart policy.
const LAUNCHES: u32 = 3;

/// The release `pp-server`, built as `Dockerfile.rust` builds it, in its own
/// target directory beside this test's. Only `-p pp-server` is built, so no
/// dev-dependency adds `test-unmarked-writer`; that feature also fails to
/// compile in a release profile. `PP_SERVER_RELEASE_BINARY` names a binary
/// built that way instead of building one.
fn release_server_binary() -> PathBuf {
    if let Some(binary) = std::env::var_os("PP_SERVER_RELEASE_BINARY") {
        return PathBuf::from(binary);
    }
    let crate_dir = std::env::var_os("CARGO_MANIFEST_DIR")
        .map_or_else(|| PathBuf::from(env!("CARGO_MANIFEST_DIR")), PathBuf::from);
    let workspace = crate_dir.join("../..").canonicalize().unwrap();
    // <target>/debug/deps/<this test>
    let target = std::env::current_exe()
        .unwrap()
        .ancestors()
        .nth(3)
        .unwrap()
        .join("pp-server-release");
    let cargo = std::env::var_os("CARGO").unwrap_or_else(|| "cargo".into());
    let parent_env: Vec<_> = std::env::vars_os().collect();
    let output = release_build(&cargo, &workspace, &target, &parent_env)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "release build failed; last {TAIL_LINES} lines of stderr, with the value of every \
         variable the build did not get redacted:\n{}",
        failure_tail(&output.stderr, &parent_env)
    );
    target.join("release/pp-server")
}

/// `cargo build --locked --release -p pp-server` into `target`, with only the
/// variables [`build_env`] admits from `parent_env` and no compiler wrapper.
/// The empty `RUSTC_WRAPPER` and `RUSTC_WORKSPACE_WRAPPER` also override a
/// wrapper named in a Cargo config file. sccache reads its storage
/// credentials from the environment and can print that environment in an
/// error, so this one build runs uncached. Hosted Linux runners have no
/// sccache, so they lose nothing.
fn release_build(
    cargo: &OsStr,
    workspace: &Path,
    target: &Path,
    parent_env: &[(OsString, OsString)],
) -> Command {
    let mut command = child_process::command(cargo);
    command
        .current_dir(workspace)
        .env_clear()
        .envs(
            parent_env
                .iter()
                .filter(|(name, _)| build_env(&name.to_string_lossy()))
                .map(|(name, value)| (name, value)),
        )
        .env("RUSTC_WRAPPER", "")
        .env("RUSTC_WORKSPACE_WRAPPER", "")
        .args(["build", "--locked", "--release", "--manifest-path"])
        .arg(workspace.join("Cargo.toml"))
        .args(["-p", "pp-server", "--target-dir"])
        .arg(target);
    command
}

/// The variables the release build sees. The rest of the test environment,
/// including nextest's library path, compiler-cache settings and any token a
/// runner sets, stays out.
fn build_env(name: &str) -> bool {
    let admitted = matches!(
        name,
        "PATH"
            | "HOME"
            | "TMPDIR"
            | "LANG"
            | "CARGO_HOME"
            | "RUSTUP_HOME"
            | "RUSTUP_TOOLCHAIN"
            | "RUSTFLAGS"
            | "PP_DESKTOP_RELEASE_MANIFEST"
    ) || ["CARGO_BUILD_", "CARGO_PROFILE_", "CARGO_NET_"]
        .iter()
        .any(|prefix| name.starts_with(prefix))
        || (name.starts_with("CARGO_TARGET_") && name != "CARGO_TARGET_TMPDIR");
    admitted && !name.contains("WRAPPER")
}

/// Lines of a failed build's stderr that the test prints.
const TAIL_LINES: usize = 40;

/// Values shorter than this stay: they are flags such as `1`, and replacing
/// them would garble every line.
const SHORTEST_REDACTED: usize = 6;

/// The last [`TAIL_LINES`] lines of `stderr`, with the value of every
/// variable in `parent_env` that [`build_env`] keeps from the build replaced
/// by `[redacted]`.
fn failure_tail(stderr: &[u8], parent_env: &[(OsString, OsString)]) -> String {
    let mut withheld: Vec<_> = parent_env
        .iter()
        .filter(|(name, _)| !build_env(&name.to_string_lossy()))
        .map(|(_, value)| value.to_string_lossy().into_owned())
        .filter(|value| value.len() >= SHORTEST_REDACTED)
        .collect();
    // Longest first, so a value that contains another is replaced whole.
    withheld.sort_by_key(|value| std::cmp::Reverse(value.len()));
    let stderr = withheld.iter().fold(
        String::from_utf8_lossy(stderr).into_owned(),
        |stderr, value| stderr.replace(value.as_str(), "[redacted]"),
    );
    let lines: Vec<_> = stderr.lines().collect();
    lines[lines.len().saturating_sub(TAIL_LINES)..].join("\n")
}

#[test]
fn ticket_t_8_release_build_gets_no_credentials_or_compiler_wrapper() {
    let parent_env: Vec<(OsString, OsString)> = [
        ("PATH", "/usr/bin"),
        ("CARGO_HOME", "/cargo"),
        ("CARGO_BUILD_JOBS", "2"),
        ("SCCACHE_REDIS_PASSWORD", "redis-secret"),
        ("SCCACHE_AZURE_CONNECTION_STRING", "azure-secret"),
        ("SCCACHE_BUCKET", "bucket-name"),
        ("AWS_SECRET_ACCESS_KEY", "aws-secret"),
        ("GITHUB_TOKEN", "github-secret"),
        ("CARGO_REGISTRY_TOKEN", "registry-secret"),
        ("RUSTC_WRAPPER", "/opt/sccache"),
        ("CARGO_BUILD_RUSTC_WRAPPER", "/opt/sccache"),
    ]
    .into_iter()
    .map(|(name, value)| (name.into(), value.into()))
    .collect();
    let command = release_build(
        OsStr::new("cargo"),
        Path::new("/workspace"),
        Path::new("/target"),
        &parent_env,
    );
    let mut env: Vec<_> = command
        .get_envs()
        .map(|(name, value)| {
            (
                name.to_string_lossy().into_owned(),
                value.map(|value| value.to_string_lossy().into_owned()),
            )
        })
        .collect();
    env.sort();
    let set = |name: &str, value: &str| (name.to_owned(), Some(value.to_owned()));
    assert_eq!(
        env,
        [
            set("CARGO_BUILD_JOBS", "2"),
            set("CARGO_HOME", "/cargo"),
            set("PATH", "/usr/bin"),
            set("RUSTC_WORKSPACE_WRAPPER", ""),
            set("RUSTC_WRAPPER", ""),
        ]
    );
}

#[test]
fn ticket_t_8_release_build_failure_redacts_withheld_values() {
    let parent_env: Vec<(OsString, OsString)> = [
        ("PATH", "/usr/local/bin"),
        ("SCCACHE_REDIS_PASSWORD", "redis-secret"),
        ("AWS_SECRET_ACCESS_KEY", "aws-secret-key"),
        ("SHORT", "1"),
    ]
    .into_iter()
    .map(|(name, value)| (name.into(), value.into()))
    .collect();
    let mut stderr = String::new();
    for line in 0..50 {
        stderr.push_str(&format!("   Compiling crate-{line}\n"));
    }
    stderr.push_str(
        "sccache: error: SCCACHE_REDIS_PASSWORD=redis-secret \
         AWS_SECRET_ACCESS_KEY=aws-secret-key PATH=/usr/local/bin SHORT=1\n",
    );
    stderr.push_str("error: could not compile `pp-server`\n");
    let tail = failure_tail(stderr.as_bytes(), &parent_env);
    let lines: Vec<_> = tail.lines().collect();
    assert_eq!(lines.len(), TAIL_LINES);
    assert_eq!(lines[0], "   Compiling crate-12");
    assert_eq!(
        lines[TAIL_LINES - 2],
        "sccache: error: SCCACHE_REDIS_PASSWORD=[redacted] \
         AWS_SECRET_ACCESS_KEY=[redacted] PATH=/usr/local/bin SHORT=1"
    );
    assert_eq!(
        lines[TAIL_LINES - 1],
        "error: could not compile `pp-server`"
    );
}

/// One start, bounded so a start that never exits fails the test with
/// `hung` instead of hanging it.
async fn launch(binary: &Path, env: &[(&str, &str)], args: &[OsString], hung: &str) -> Output {
    let mut command = child_process::command(binary);
    command
        .env_remove("PP_LISTEN_ADDR")
        .envs(env.iter().copied())
        .args(args);
    tokio::time::timeout(
        Duration::from_secs(30),
        tokio::process::Command::from(command)
            .kill_on_drop(true)
            .output(),
    )
    .await
    .expect(hung)
    .unwrap()
}

fn args(pairs: &[(&str, &Path)]) -> Vec<OsString> {
    pairs
        .iter()
        .flat_map(|(flag, value)| [OsString::from(flag), value.as_os_str().to_owned()])
        .collect()
}

/// A schema 34 Node installation, optionally marked Rust-owned, restamped to
/// `found`.
fn installation_at(found: u64, rust_owned: bool) -> Installation {
    let installation = Installation::create(Fixture::NodeSchema34).unwrap();
    if rust_owned {
        installation.write_marker(&Marker::default()).unwrap();
    }
    node_schema::restamp_schema_version(&installation.database_path(), found).unwrap();
    installation
}

/// The storage lease's lock file. Taking the lease creates it, empty, and it
/// stays; [`pp_testkit::allowed_in_empty`] counts it as no data.
const LEASE_LOCK: &str = "printpartner.lock";

/// The entries of `root` added, removed or rewritten since `before`.
fn changed(before: &pp_testkit::Snapshot, root: &Path) -> Vec<String> {
    let after = pp_testkit::snapshot(root).unwrap();
    let mut paths: Vec<_> = before.keys().chain(after.keys()).collect();
    paths.sort();
    paths.dedup();
    paths
        .into_iter()
        .filter(|path| before.get(*path) != after.get(*path))
        .map(|path| path.display().to_string())
        .collect()
}

/// After the first start, `root` differs from `before` at most by an empty
/// lease lock file. Returns the snapshot every later start must match.
fn only_the_lease_lock_was_added(
    before: &pp_testkit::Snapshot,
    root: &Path,
) -> pp_testkit::Snapshot {
    let changes = changed(before, root);
    let after = pp_testkit::snapshot(root).unwrap();
    assert!(
        changes.is_empty()
            || (changes == [LEASE_LOCK]
                && !before.contains_key(Path::new(LEASE_LOCK))
                && after[Path::new(LEASE_LOCK)].as_deref() == Some(&[][..])),
        "the first start changed {changes:?}"
    );
    after
}

/// Rust-only starts use the image's environment and arguments, with a loopback
/// listener: `pp-server --data … --web … --commit … --status-file …`.
/// Node-backed starts add `--node`, which that mode requires, and a test
/// credential file, which a successful start writes.
#[tokio::test]
async fn ticket_t_8_versions_headless_refusals_exit_without_restarts() {
    let binary = release_server_binary();
    production_gate_is_on(&binary).await;

    for rust_only in [false, true] {
        let newest = if rust_only { RUST_SCHEMA } else { NODE_SCHEMA };
        for found in [OLDEST_UPGRADABLE - 1, RUST_SCHEMA + 1] {
            // The Node-backed runtime refuses a Rust-owned directory before
            // reading its schema, and the Rust-only one refuses an unmarked
            // non-empty directory, so each mode gets the directory it opens.
            let installation = installation_at(found, rust_only);
            let mut expected = pp_testkit::snapshot(installation.path()).unwrap();
            let root = scratch::Scratch::new("pp-schema-refusal-");
            let status = root.join("status.json");
            let credential = root.join("credential.json");
            let bundle = release::launch(installation.path().to_path_buf()).bundle;
            let commit = PathBuf::from(&bundle.commit);
            let mut argv = args(&[
                ("--data", installation.path()),
                ("--web", &bundle.web_root),
                ("--commit", &commit),
                ("--status-file", &status),
            ]);
            let env: &[(&str, &str)] = if rust_only {
                &[
                    ("PP_DESKTOP_REBUILD", "1"),
                    ("PP_LISTEN_ADDR", "127.0.0.1:0"),
                ]
            } else {
                argv.extend(args(&[
                    ("--node", &bundle.node),
                    ("--test-credential-file", &credential),
                ]));
                &[("PP_DESKTOP_REBUILD", "0")]
            };
            let refusal = IncompatibleSchema {
                found,
                oldest: OLDEST_UPGRADABLE,
                newest,
            };
            let (diagnostic, recovery) = (refusal.diagnostic(), refusal.to_string());

            for attempt in 1..=LAUNCHES {
                let output = launch(
                    &binary,
                    env,
                    &argv,
                    "pp-server did not exit; possible reopen loop",
                )
                .await;
                let stderr = String::from_utf8(output.stderr).unwrap();
                assert_eq!(output.status.code(), Some(78), "{stderr}");
                let count = |wanted: &str| stderr.lines().filter(|line| *line == wanted).count();
                assert_eq!(count(&diagnostic), 1, "{stderr}");
                assert_eq!(count(&recovery), 1, "{stderr}");
                assert!(
                    output.stdout.is_empty(),
                    "refused start announced an origin or shutdown receipt"
                );
                assert!(!status.exists(), "refused start left a health status file");
                assert!(
                    !credential.exists(),
                    "refused start published a launch credential"
                );
                if attempt == 1 {
                    expected = only_the_lease_lock_was_added(&expected, installation.path());
                }
                assert_eq!(
                    changed(&expected, installation.path()),
                    Vec::<String>::new(),
                    "rust_only={rust_only} schema={found} attempt={attempt}: data directory changed"
                );
                println!(
                    "release pp-server rust_only={rust_only} attempt={attempt} exit=78 {diagnostic}; \
                     recovery line once; data unchanged; no origin, status file or credential"
                );
            }
        }
    }
}

/// The binary under test refuses an unmarked Node installation in Rust-only
/// mode, as the image does. A build with `test-unmarked-writer` skips that
/// gate and would start, so this shows the launches above ran the shipped
/// gate.
async fn production_gate_is_on(binary: &Path) {
    let installation = installation_at(NODE_SCHEMA, false);
    let before = pp_testkit::snapshot(installation.path()).unwrap();
    let root = scratch::Scratch::new("pp-schema-refusal-gate-");
    let status = root.join("status.json");
    let bundle = release::launch(installation.path().to_path_buf()).bundle;
    let commit = PathBuf::from(&bundle.commit);
    let output = launch(
        binary,
        &[
            ("PP_DESKTOP_REBUILD", "1"),
            ("PP_LISTEN_ADDR", "127.0.0.1:0"),
        ],
        &args(&[
            ("--data", installation.path()),
            ("--web", &bundle.web_root),
            ("--commit", &commit),
            ("--status-file", &status),
        ]),
        "pp-server opened an unmarked installation: the binary under test skips the ownership gate, as a test-unmarked-writer build does",
    )
    .await;
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert_eq!(output.status.code(), Some(1), "{stderr}");
    assert!(
        stderr.contains("Rust does not take over an existing installation"),
        "{stderr}"
    );
    assert!(output.stdout.is_empty(), "{stderr}");
    only_the_lease_lock_was_added(&before, installation.path());
    println!(
        "release pp-server enforces the ownership marker gate: {}",
        stderr.trim()
    );
}
