//! A Rust-only `pp-server` starts without `--node`, keeps its status file, and
//! `pp-server healthcheck` reads it: live and ready while serving, gone after
//! SIGTERM. This is the start the Docker image makes.
#![cfg(all(unix, feature = "desktop_rebuild"))]

#[path = "../../pp-storage/tests/common/process.rs"]
mod child_process;
#[path = "../../pp-core/tests/support/release.rs"]
mod release;
#[path = "support/rust_only.rs"]
mod rust_only;
#[path = "../../pp-storage/tests/common/scratch.rs"]
mod scratch;

use rust_only::{Server, server_binary};
use std::{path::Path, process::Output};

fn healthcheck(probe: &str, status: &Path) -> Output {
    child_process::command(server_binary())
        .args(["healthcheck", probe, "--status-file"])
        .arg(status)
        .args(["--timeout-ms", "2000"])
        .output()
        .unwrap()
}

#[test]
fn rust_only_server_needs_no_node_and_reports_health() {
    let root = scratch::Scratch::new("pp-rust-only-health-");
    let server = Server::start(&root);
    let (data, status) = (server.data.clone(), server.status.clone());
    let origin = server.origin().to_owned();
    assert!(origin.starts_with("http://127.0.0.1:"), "{origin}");
    // The marker itself is pp-storage's to test: a workspace build unifies its
    // test-only test-unmarked-writer feature, which skips writing it.
    assert!(
        data.join("print-partner.db").is_file(),
        "Rust opened SQLite storage"
    );
    assert_eq!(server.ready["pid"], server.child.id());
    for (probe, line) in [
        ("liveness", "liveness ok (ready)\n"),
        ("readiness", "readiness ok (ready)\n"),
    ] {
        let output = healthcheck(probe, &status);
        assert!(output.status.success(), "{output:?}");
        assert_eq!(String::from_utf8_lossy(&output.stdout), line);
    }

    let (exit, printed, _) = server.stop();
    let mut lines = printed.lines();
    assert_eq!(lines.next(), Some(origin.as_str()), "origin first");
    let receipt: serde_json::Value = serde_json::from_str(lines.next().unwrap()).unwrap();
    assert!(
        exit.success()
            && receipt["storage_released"] == true
            && receipt["errors"] == serde_json::json!([]),
        "incomplete shutdown: {receipt}"
    );
    assert!(!status.exists(), "the status file leaves with the server");
    let output = healthcheck("liveness", &status);
    assert_eq!(output.status.code(), Some(1));
    assert_eq!(
        String::from_utf8_lossy(&output.stdout),
        "unhealthy: Status file missing\n"
    );
}

#[test]
fn healthcheck_help_exits_zero() {
    let output = child_process::command(server_binary())
        .args(["healthcheck", "--help"])
        .output()
        .unwrap();
    assert!(output.status.success());
    assert!(String::from_utf8_lossy(&output.stdout).starts_with("Usage: pp-server healthcheck"));
}

/// Unsupported storage must stop the executable before readiness, in either
/// runtime mode, even when a process manager launches it again.
#[tokio::test]
async fn ticket_t_8_versions_headless_refusals_exit_without_restarts() {
    use pp_storage::versions::{IncompatibleSchema, NODE_SCHEMA, OLDEST_UPGRADABLE, RUST_SCHEMA};
    use pp_testkit::{Fixture, Installation, node_schema};
    use std::time::Duration;

    for rust_only in [false, true] {
        let newest = if rust_only { RUST_SCHEMA } else { NODE_SCHEMA };
        for found in [OLDEST_UPGRADABLE - 1, RUST_SCHEMA + 1] {
            let installation = Installation::create(Fixture::NodeSchema34).unwrap();
            node_schema::restamp_schema_version(&installation.database_path(), found).unwrap();
            let before = node_schema::database_files(installation.path());
            let root = scratch::Scratch::new("pp-schema-refusal-");
            let status = root.join("status.json");
            let credential = root.join("credential.json");
            let bundle = release::launch(installation.path().to_path_buf()).bundle;
            let refusal = IncompatibleSchema {
                found,
                oldest: OLDEST_UPGRADABLE,
                newest,
            };

            for attempt in 1..=3 {
                let mut command = child_process::command(server_binary());
                command
                    .env("PP_DESKTOP_REBUILD", if rust_only { "1" } else { "0" })
                    .args(["--data".as_ref(), installation.path().as_os_str()])
                    .args(["--web".as_ref(), bundle.web_root.as_os_str()])
                    .args(["--node".as_ref(), bundle.node.as_os_str()])
                    .args(["--commit", &bundle.commit])
                    .args(["--status-file".as_ref(), status.as_os_str()])
                    .args(["--test-credential-file".as_ref(), credential.as_os_str()]);
                let output = tokio::time::timeout(
                    Duration::from_secs(30),
                    tokio::process::Command::from(command)
                        .kill_on_drop(true)
                        .output(),
                )
                .await
                .expect("unsupported storage did not terminate; possible reopen loop")
                .unwrap();
                let stderr = String::from_utf8(output.stderr).unwrap();
                assert_eq!(output.status.code(), Some(78), "{stderr}");
                let diagnostic = refusal.diagnostic();
                assert_eq!(
                    stderr.lines().filter(|line| *line == diagnostic).count(),
                    1,
                    "{stderr}"
                );
                assert!(
                    stderr.lines().any(|line| line == refusal.to_string()),
                    "{stderr}"
                );
                assert!(
                    output.stdout.is_empty(),
                    "refused start announced an origin or shutdown receipt"
                );
                assert!(!status.exists(), "refused start left a health status file");
                assert!(
                    !credential.exists(),
                    "refused start published a launch credential"
                );
                assert_eq!(
                    node_schema::database_files(installation.path()),
                    before,
                    "runtime rust_only={rust_only}, schema {found}, attempt {attempt}"
                );
                println!(
                    "rust_only={rust_only} attempt={attempt} exit=78 {diagnostic}; database unchanged; no readiness or launch credential"
                );
            }
        }
    }
}
