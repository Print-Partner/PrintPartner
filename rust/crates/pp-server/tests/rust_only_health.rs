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
