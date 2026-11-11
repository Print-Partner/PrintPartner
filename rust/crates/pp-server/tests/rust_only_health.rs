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
        Some(address) => Server::start_with(&root, |command| {
            command.env("PP_LISTEN_ADDR", address);
            command.args(["--test-credential-file".as_ref(), credential.as_os_str()]);
        }),
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

    let (exit, printed, _) = server.stop();
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

/// A database newer than this build stops the headless start once, with exit
/// code 78 and the incompatible-storage lines, and stays byte for byte as it was.
#[test]
fn rust_only_server_exits_78_on_incompatible_storage() {
    let installation = pp_testkit::Installation::create(pp_testkit::Fixture::NodeSchema34).unwrap();
    pp_testkit::node_schema::restamp_schema_version(&installation.database_path(), 999).unwrap();
    let before = pp_testkit::node_schema::database_files(installation.path());
    let root = scratch::Scratch::new("pp-rust-only-incompatible-");
    let bundle = release::launch(installation.path().to_path_buf()).bundle;
    let output = child_process::command(server_binary())
        .env("PP_DESKTOP_REBUILD", "1")
        .args(["--data".as_ref(), installation.path().as_os_str()])
        .args(["--web".as_ref(), bundle.web_root.as_os_str()])
        .args(["--commit", &bundle.commit])
        .args([
            "--status-file".as_ref(),
            root.join("status.json").as_os_str(),
        ])
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&output.stderr);
    println!("{stderr}");
    assert_eq!(output.status.code(), Some(78), "{stderr}");
    let mut lines = stderr
        .lines()
        .skip_while(|line| !line.starts_with("incompatible_storage: "));
    assert!(
        lines.next().is_some_and(|line| line
            .starts_with("incompatible_storage: found=999 supported=31..=")
            && line.ends_with(" direction=newer")),
        "{stderr}"
    );
    assert!(
        lines.next().is_some_and(|line| line.starts_with(
            "Incompatible storage: database schema version 999 is newer than supported version"
        )),
        "{stderr}"
    );
    assert!(output.stdout.is_empty(), "no origin was announced");
    assert_eq!(
        pp_testkit::node_schema::database_files(installation.path()),
        before
    );
}
