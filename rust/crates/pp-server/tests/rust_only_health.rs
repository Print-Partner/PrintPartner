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
use std::{
    io::{Read, Write},
    net::TcpStream,
    path::Path,
    process::Output,
    time::Duration,
};

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
    server_reports_health(None);
}

#[test]
fn explicit_wildcard_listener_reports_health() {
    server_reports_health(Some("0.0.0.0:0"));
}

#[test]
fn explicit_loopback_listener_reports_health() {
    server_reports_health(Some("127.0.0.1:0"));
}

// Linux routes the entire 127/8 range to loopback without interface aliases.
#[cfg(target_os = "linux")]
#[test]
fn explicit_specific_listener_reports_health() {
    server_reports_health(Some("127.0.0.2:0"));
}

fn redeem_bootstrap(origin: &str, credential: &Path) {
    let receipt: serde_json::Value =
        serde_json::from_slice(&std::fs::read(credential).unwrap()).unwrap();
    let url = receipt["bootstrap_url"].as_str().unwrap();
    let path = url
        .strip_prefix(origin)
        .expect("bootstrap uses reachable origin");
    let host = origin.trim_start_matches("http://");
    for expected in ["HTTP/1.1 303", "HTTP/1.1 401"] {
        let mut stream =
            TcpStream::connect_timeout(&host.parse().unwrap(), Duration::from_secs(5)).unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        stream
            .set_write_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        write!(
            stream,
            "GET {path} HTTP/1.1\r\nHost: {host}\r\nOrigin: {origin}\r\nConnection: close\r\n\r\n"
        )
        .unwrap();
        let mut response = String::new();
        stream.read_to_string(&mut response).unwrap();
        assert!(response.starts_with(expected), "bootstrap admission status");
    }
}

fn assert_ready_probes(status: &Path) {
    for (probe, line) in [
        ("liveness", "liveness ok (ready)\n"),
        ("readiness", "readiness ok (ready)\n"),
    ] {
        let output = healthcheck(probe, status);
        assert!(output.status.success(), "{output:?}");
        assert_eq!(String::from_utf8_lossy(&output.stdout), line);
    }
}

fn server_reports_health(listen: Option<&str>) {
    let root = scratch::Scratch::new("pp-rust-only-health-");
    let credential = root.join("credentials.json");
    let server = match listen {
        None => Server::start(&root),
        Some(address) => Server::start_with_listener(&root, Some(address), Some(&credential)),
    };
    let (data, status) = (server.data.clone(), server.status.clone());
    let origin = server.origin().to_owned();
    let probe_address: std::net::SocketAddr = origin.trim_start_matches("http://").parse().unwrap();
    let expected_ip = if listen == Some("127.0.0.2:0") {
        "127.0.0.2"
    } else {
        "127.0.0.1"
    };
    assert_eq!(probe_address.ip().to_string(), expected_ip);
    assert_eq!(data.join("desktop.toml").exists(), listen.is_none());
    // The marker itself is pp-storage's to test: a workspace build unifies its
    // test-only test-unmarked-writer feature, which skips writing it.
    assert!(
        data.join("print-partner.db").is_file(),
        "Rust opened SQLite storage"
    );
    assert_eq!(server.ready["pid"], server.child.id());
    if listen.is_some() {
        redeem_bootstrap(&origin, &credential);
    }
    assert_ready_probes(&status);

    let (exit, printed, logged) = server.stop();
    // Tracing events reach stderr as scrubbed JSON rows (provisional, GRE-1457).
    let ready = logged
        .lines()
        .filter_map(|line| serde_json::from_str::<serde_json::Value>(line).ok())
        .find(|row| row["message"] == "pp_server_ready")
        .unwrap_or_else(|| panic!("no pp_server_ready row on stderr: {logged}"));
    assert_eq!(ready["level"], 30, "{ready}");
    assert_eq!(ready["origin"], origin.as_str(), "{ready}");
    let mut lines = printed.lines();
    assert_eq!(
        lines.next(),
        Some(origin.as_str()),
        "reachable origin first"
    );
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
