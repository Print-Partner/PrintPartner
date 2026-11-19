//! `pp-server` refuses an unsupported schema on every start: exit 78, one
//! refusal on stderr, nothing on stdout, and the data directory left as it
//! was, however often a process manager starts it again.
//!
//! The default run starts the workspace test build of `pp-server`. That build
//! turns on pp-storage's `test-unmarked-writer` feature, which skips the
//! ownership marker gate but not schema admission. The ignored
//! `ticket_t_8_versions_release_binary_refuses_without_restarts` repeats the
//! same starts on the binary `Dockerfile.rust` ships, built with `cargo build
//! --locked --release -p pp-server`, after checking that its ownership gate is
//! on. That build takes minutes, so it runs only when asked for; the command is
//! in `docs/storage.md`.
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

/// The workspace test build of `pp-server`.
fn test_server_binary() -> PathBuf {
    if std::env::var_os("NEXTEST_BIN_EXE_pp-server").is_some() {
        child_process::bin_exe("pp-server")
    } else {
        env!("CARGO_BIN_EXE_pp-server").into()
    }
}

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
    let output = isolated_build(
        &cargo,
        &workspace.join("Cargo.toml"),
        "pp-server",
        &target,
        &toolchain_pin(&workspace.join("..")),
        &std::env::vars_os().collect::<Vec<_>>(),
    )
    .output()
    .unwrap();
    assert!(output.status.success(), "{}", build_failure(&output));
    target.join("release/pp-server")
}

/// The channel `rust-toolchain.toml` in `repo` pins.
fn toolchain_pin(repo: &Path) -> String {
    let pin = std::fs::read_to_string(repo.join("rust-toolchain.toml")).unwrap();
    pin.lines()
        .find_map(|line| {
            let (key, value) = line.split_once('=')?;
            (key.trim() == "channel").then(|| value.trim().trim_matches('"').to_owned())
        })
        .expect("rust-toolchain.toml names a channel")
}

/// The config files Cargo reads for a build started in `/`, besides the one
/// in `CARGO_HOME`.
const ROOT_CARGO_CONFIGS: [&str; 2] = ["/.cargo/config", "/.cargo/config.toml"];

/// `cargo build --locked --offline --release -p <package>` into `target`,
/// isolated from the test's environment and from every Cargo config file.
///
/// - The environment is cleared. The build gets `PATH`, `RUSTUP_HOME`,
///   `PP_DESKTOP_RELEASE_MANIFEST` and a numeric `CARGO_BUILD_JOBS` from
///   `parent_env`, the `toolchain` pin as `RUSTUP_TOOLCHAIN`, and empty
///   `RUSTC_WRAPPER` and `RUSTC_WORKSPACE_WRAPPER`, so no compiler wrapper
///   such as sccache starts. No `HOME` is passed.
/// - `CARGO_HOME` is a new, empty `<target>/cargo-home-*` directory for each
///   build. It holds only links to the real Cargo home's `registry` and `git`
///   caches, so Cargo never sees a `config.toml` or `credentials.toml` there.
/// - Cargo runs in `/`. It looks for `.cargo/config.toml` in its working
///   directory and every parent, not beside the manifest, so a config file in
///   the checkout or in a home directory above it is not read. That leaves
///   `/.cargo/config` and `/.cargo/config.toml`: if either exists, this
///   panics before building.
/// - `--offline` keeps Cargo off the network, so it never asks for registry
///   credentials. The test's own build has already fetched every crate that
///   `Cargo.lock` names.
fn isolated_build(
    cargo: &OsStr,
    manifest: &Path,
    package: &str,
    target: &Path,
    toolchain: &str,
    parent_env: &[(OsString, OsString)],
) -> Command {
    for config in ROOT_CARGO_CONFIGS {
        match std::fs::symlink_metadata(config) {
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            _ => panic!(
                "{config} exists or cannot be checked. Cargo reads it for every build started \
                 in /, so the isolated build would not be isolated from it; refusing to build"
            ),
        }
    }
    let parent = |name: &str| {
        parent_env
            .iter()
            .find(|(key, _)| key == name)
            .map(|(_, value)| PathBuf::from(value))
    };
    let home = parent("HOME");
    let in_home = |dir: &str| home.as_ref().map(|home| home.join(dir));
    let cargo_home =
        isolated_cargo_home(target, parent("CARGO_HOME").or_else(|| in_home(".cargo")));
    let mut command = child_process::command(cargo);
    command
        .current_dir("/")
        .env_clear()
        .env("CARGO_HOME", cargo_home)
        .env("RUSTUP_TOOLCHAIN", toolchain)
        .env("RUSTC_WRAPPER", "")
        .env("RUSTC_WORKSPACE_WRAPPER", "");
    let rustup_home = parent("RUSTUP_HOME").or_else(|| in_home(".rustup"));
    for (name, value) in [
        ("PATH", parent("PATH")),
        ("RUSTUP_HOME", rustup_home),
        (
            "PP_DESKTOP_RELEASE_MANIFEST",
            parent("PP_DESKTOP_RELEASE_MANIFEST"),
        ),
    ] {
        if let Some(value) = value {
            command.env(name, value);
        }
    }
    if let Some(jobs) =
        parent("CARGO_BUILD_JOBS").and_then(|jobs| jobs.to_str()?.parse::<u16>().ok())
    {
        command.env("CARGO_BUILD_JOBS", jobs.to_string());
    }
    command
        .args(["build", "--locked", "--offline", "--release"])
        // The isolated home has its own last-use records; never let it clean
        // the shared registry cache by them.
        .args(["--config", "cache.auto-clean-frequency=\"never\""])
        .arg("--manifest-path")
        .arg(manifest)
        .args(["-p", package, "--target-dir"])
        .arg(target);
    command
}

/// A new Cargo home under `target` that links to `real`'s download caches and
/// holds nothing else. It is kept after the build, not removed.
fn isolated_cargo_home(target: &Path, real: Option<PathBuf>) -> PathBuf {
    std::fs::create_dir_all(target).unwrap();
    let home = tempfile::Builder::new()
        .prefix("cargo-home-")
        .tempdir_in(target)
        .unwrap()
        .keep();
    for cache in ["registry", "git"] {
        if let Some(source) = real.as_ref().map(|real| real.join(cache))
            && source.is_dir()
        {
            std::os::unix::fs::symlink(source, home.join(cache)).unwrap();
        }
    }
    home
}

/// What the test prints when the isolated build fails: the exit status and
/// how many lines Cargo marked `error`. Nothing from the build's output is
/// copied. A compiler or build script can echo any value it was given, and no
/// filter can tell a credential from a crate name or a path.
fn build_failure(output: &Output) -> String {
    let errors = String::from_utf8_lossy(&output.stderr)
        .lines()
        .filter(|line| line.starts_with("error"))
        .count();
    format!(
        "isolated build failed: {}; {errors} error lines. Its output is not shown. To see the \
         release build's output, run `cargo build --locked --release -p pp-server` in rust/.",
        output.status
    )
}

/// Fake, credential-shaped values. None is a real secret.
const FAKE_SECRETS: [(&str, &str); 14] = [
    ("AWS_SECRET_ACCESS_KEY", "pp-fake-aws-secret-0001"),
    ("GITHUB_TOKEN", "pp-fake-github-token-0002"),
    ("SCCACHE_REDIS_PASSWORD", "pp-fake-redis-password-0003"),
    ("CARGO_REGISTRY_TOKEN", "pp-fake-registry-token-0004"),
    (
        "CARGO_REGISTRIES_FAKE_TOKEN",
        "pp-fake-registries-token-0005",
    ),
    ("CARGO_BUILD_RUSTFLAGS", "--cfg=pp_fake_rustflags_0006"),
    ("RUSTFLAGS", "--cfg=pp_fake_rustflags_0007"),
    ("RUSTC_WRAPPER", "/nonexistent/pp-fake-wrapper-0008"),
    (
        "CARGO_BUILD_RUSTC_WRAPPER",
        "/nonexistent/pp-fake-wrapper-0009",
    ),
    ("CARGO_ENCODED_RUSTFLAGS", "--cfg=pp_fake_rustflags_0010"),
    // Planted in Cargo config files below, not in the environment.
    (
        "[env] in $HOME/.cargo/config.toml",
        "pp-fake-home-config-0011",
    ),
    (
        "[env] in $CARGO_HOME/config.toml",
        "pp-fake-cargo-home-config-0012",
    ),
    (
        "$CARGO_HOME/credentials.toml",
        "pp-fake-credentials-token-0013",
    ),
    (
        "[env] in the package's .cargo/config.toml",
        "pp-fake-package-config-0014",
    ),
];

/// The probe's build script. It sees every variable Cargo inherited and every
/// `[env]` entry from a config file Cargo read. It writes one line per check
/// to `checks`, never the environment itself: `absent` or `present` for each
/// fake value, whether `PP_FAKE_CONFIG` was set, and whether `CARGO_HOME`
/// starts with `home_prefix`.
fn probe_build_script(home_prefix: &Path) -> String {
    let needles: Vec<_> = FAKE_SECRETS.iter().map(|(_, value)| *value).collect();
    format!(
        "const NEEDLES: [&str; {}] = {needles:?};\n\
         fn main() {{\n\
         \x20   let values: Vec<String> = std::env::vars_os()\n\
         \x20       .map(|(_, value)| value.to_string_lossy().into_owned())\n\
         \x20       .collect();\n\
         \x20   let mut checks = String::new();\n\
         \x20   for needle in NEEDLES {{\n\
         \x20       let present = values.iter().any(|value| value.contains(needle));\n\
         \x20       checks.push_str(if present {{ \"present\\n\" }} else {{ \"absent\\n\" }});\n\
         \x20   }}\n\
         \x20   let config = std::env::var_os(\"PP_FAKE_CONFIG\").is_some();\n\
         \x20   checks.push_str(if config {{ \"config read\\n\" }} else {{ \"no config\\n\" }});\n\
         \x20   let home = std::env::var(\"CARGO_HOME\").unwrap_or_default();\n\
         \x20   let isolated = home.starts_with({:?});\n\
         \x20   checks.push_str(if isolated {{ \"isolated home\\n\" }} else {{ \"other home\\n\" }});\n\
         \x20   std::fs::write(concat!(env!(\"CARGO_MANIFEST_DIR\"), \"/checks\"), checks).unwrap();\n\
         }}\n",
        needles.len(),
        home_prefix.display().to_string(),
    )
}

/// A probe package goes through [`isolated_build`]. Fake credentials sit in
/// the parent environment and in three config files; each file also names a
/// missing compiler wrapper. The probe's build script reports which fake
/// values reached it. Then the probe is made to fail with a fake credential in
/// its compiler error, and the test's failure message must not carry it.
#[test]
fn ticket_t_8_release_build_keeps_credentials_out_of_the_build_and_its_output() {
    let root = scratch::Scratch::new("pp-isolated-build-");
    let home = root.join("home");
    let cargo_home = root.join("cargo-home");
    let probe = home.join("probe");
    let target = root.join("target");
    let secret = |index: usize| FAKE_SECRETS[index].1;
    let config = |value: &str| {
        format!(
            "[env]\nPP_FAKE_CONFIG = \"{value}\"\n[build]\nrustc-wrapper = \
             \"/nonexistent/pp-fake-config-wrapper\"\n[registry]\ntoken = \"{value}\"\n\
             global-credential-providers = [\"/nonexistent/pp-fake-provider\"]\n"
        )
    };
    let write = |path: PathBuf, text: &str| {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, text).unwrap();
    };
    write(home.join(".cargo/config.toml"), &config(secret(10)));
    write(cargo_home.join("config.toml"), &config(secret(11)));
    write(
        cargo_home.join("credentials.toml"),
        &format!("[registry]\ntoken = \"{}\"\n", secret(12)),
    );
    write(probe.join(".cargo/config.toml"), &config(secret(13)));
    write(
        probe.join("Cargo.toml"),
        "[package]\nname = \"pp-isolated-build-probe\"\nversion = \"0.0.0\"\n\
         edition = \"2021\"\n\n[workspace]\n",
    );
    write(
        probe.join("Cargo.lock"),
        "# This file is automatically @generated by Cargo.\n# It is not intended for manual \
         editing.\nversion = 4\n\n[[package]]\nname = \"pp-isolated-build-probe\"\n\
         version = \"0.0.0\"\n",
    );
    write(
        probe.join("build.rs"),
        &probe_build_script(&target.join("cargo-home-")),
    );
    write(probe.join("src/lib.rs"), "");

    let real = |name: &str| std::env::var_os(name);
    let real_home = real("HOME").map(PathBuf::from);
    let rustup_home = real("RUSTUP_HOME")
        .map(PathBuf::from)
        .or_else(|| real_home.map(|home| home.join(".rustup")));
    let mut parent_env: Vec<(OsString, OsString)> = FAKE_SECRETS[..10]
        .iter()
        .map(|(name, value)| (name.into(), value.into()))
        .collect();
    parent_env.extend([
        ("HOME".into(), home.clone().into_os_string()),
        ("CARGO_HOME".into(), cargo_home.clone().into_os_string()),
        ("CARGO_BUILD_JOBS".into(), "2".into()),
        ("PATH".into(), real("PATH").unwrap()),
    ]);
    if let Some(rustup_home) = rustup_home {
        parent_env.push(("RUSTUP_HOME".into(), rustup_home.into_os_string()));
    }
    let workspace = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..");
    let cargo = real("CARGO").unwrap_or_else(|| "cargo".into());
    let build = || {
        isolated_build(
            &cargo,
            &probe.join("Cargo.toml"),
            "pp-isolated-build-probe",
            &target,
            &toolchain_pin(&workspace.join("..")),
            &parent_env,
        )
    };
    let cargo_home_of = |command: &Command| {
        command
            .get_envs()
            .find(|(name, _)| *name == "CARGO_HOME")
            .and_then(|(_, value)| value)
            .map(PathBuf::from)
            .unwrap()
    };

    let command = build();
    let mut names: Vec<_> = command
        .get_envs()
        .map(|(name, _)| name.to_string_lossy().into_owned())
        .collect();
    names.sort();
    let mut expected = vec![
        "CARGO_BUILD_JOBS",
        "CARGO_HOME",
        "PATH",
        "RUSTC_WORKSPACE_WRAPPER",
        "RUSTC_WRAPPER",
        "RUSTUP_TOOLCHAIN",
    ];
    if parent_env.iter().any(|(name, _)| name == "RUSTUP_HOME") {
        expected.push("RUSTUP_HOME");
        expected.sort();
    }
    assert_eq!(names, expected);
    let first_home = cargo_home_of(&command);
    assert_eq!(
        std::fs::read_dir(&first_home).unwrap().count(),
        0,
        "a new Cargo home starts empty when the real one has no caches"
    );

    let output = build().output().unwrap();
    assert!(output.status.success(), "{}", build_failure(&output));
    let checks = std::fs::read_to_string(probe.join("checks")).unwrap();
    let checks: Vec<_> = checks.lines().collect();
    assert_eq!(checks.len(), FAKE_SECRETS.len() + 2);
    for ((planted_in, _), check) in FAKE_SECRETS.iter().zip(&checks) {
        assert_eq!(
            *check, "absent",
            "the build received the fake value planted in {planted_in}"
        );
    }
    assert_eq!(
        checks[FAKE_SECRETS.len()..],
        ["no config", "isolated home"],
        "the build read a Cargo config file or used another Cargo home"
    );

    let leaked = "pp-fake-compiler-output-0015";
    write(
        probe.join("src/lib.rs"),
        &format!("compile_error!(\"token={leaked}\");\n"),
    );
    let mut command = build();
    assert_ne!(
        cargo_home_of(&command),
        first_home,
        "each build gets a new Cargo home"
    );
    let output = command.output().unwrap();
    assert!(!output.status.success(), "the failing probe built");
    assert!(
        String::from_utf8_lossy(&output.stderr).contains(leaked),
        "the probe's compiler error should carry the fake value this check looks for"
    );
    let message = build_failure(&output);
    assert!(
        !message.contains(leaked) && !message.contains("pp-isolated-build-probe"),
        "the failure message copies build output"
    );
    assert!(!message.contains("; 0 error lines"), "{message}");
}

/// One start, bounded so a start that never exits fails the test with
/// `hung` instead of hanging it. The server gets only `env`, so its output
/// cannot carry a value from the test's environment.
async fn launch(binary: &Path, env: &[(&str, &str)], args: &[OsString], hung: &str) -> Output {
    let mut command = child_process::command(binary);
    command.env_clear().envs(env.iter().copied()).args(args);
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

/// The 12 starts on the workspace test build of `pp-server`. Its
/// `test-unmarked-writer` feature skips the ownership gate, not schema
/// admission, so the refusal checks hold for it too. Part of the default run.
#[tokio::test]
async fn ticket_t_8_versions_headless_refusals_exit_without_restarts() {
    refusals_exit_without_restarts(&test_server_binary(), "test-build").await;
}

/// The same 12 starts on the binary `Dockerfile.rust` ships, after checking
/// that its ownership gate is on. Builds the release `pp-server` first, which
/// takes minutes, so it runs only when asked for:
/// `cargo test --locked --manifest-path rust/Cargo.toml -p pp-server --test
/// unsupported_schema -- --ignored --exact
/// ticket_t_8_versions_release_binary_refuses_without_restarts --nocapture`.
#[tokio::test]
#[ignore = "builds the release pp-server; run it with the command in docs/storage.md"]
async fn ticket_t_8_versions_release_binary_refuses_without_restarts() {
    let binary = release_server_binary();
    production_gate_is_on(&binary).await;
    refusals_exit_without_restarts(&binary, "release").await;
}

/// Starts `binary` [`LAUNCHES`] times for each runtime mode and each of schema
/// 30 and 51. Rust-only starts use the image's environment and arguments, with
/// a loopback listener: `pp-server --data … --web … --commit … --status-file
/// …`. Node-backed starts add `--node`, which that mode requires, and a test
/// credential file, which a successful start writes.
async fn refusals_exit_without_restarts(binary: &Path, build: &str) {
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
                    binary,
                    env,
                    &argv,
                    "pp-server did not exit; possible reopen loop",
                )
                .await;
                let stderr = String::from_utf8_lossy(&output.stderr);
                let count = |wanted: &str| stderr.lines().filter(|line| *line == wanted).count();
                // The facts, not the server's output.
                let facts = format!(
                    "rust_only={rust_only} schema={found} attempt={attempt}: exit {:?}, \
                     diagnostic line {} times, recovery line {} times, {} stderr lines",
                    output.status.code(),
                    count(&diagnostic),
                    count(&recovery),
                    stderr.lines().count()
                );
                assert_eq!(output.status.code(), Some(78), "{facts}");
                assert_eq!(count(&diagnostic), 1, "{facts}");
                assert_eq!(count(&recovery), 1, "{facts}");
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
                    "{build} pp-server rust_only={rust_only} attempt={attempt} exit=78 {diagnostic}; \
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
    let refused = String::from_utf8_lossy(&output.stderr)
        .contains("Rust does not take over an existing installation");
    let facts = format!(
        "exit {:?}, ownership refusal printed: {refused}, stdout {} bytes",
        output.status.code(),
        output.stdout.len()
    );
    assert_eq!(output.status.code(), Some(1), "{facts}");
    assert!(refused, "{facts}");
    assert!(output.stdout.is_empty(), "{facts}");
    only_the_lease_lock_was_added(&before, installation.path());
    println!(
        "release pp-server enforces the ownership marker gate: exit 1, \
         \"Rust does not take over an existing installation\""
    );
}
