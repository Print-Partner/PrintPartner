//! The shipped `pp-server` binary with both rebuild gates on serves the Rust
//! product routes without a Node child. With the Cargo feature off, the same
//! runtime values keep the Node-backed runtime.

#[path = "../../pp-storage/tests/common/process.rs"]
mod child_process;
#[path = "../../pp-core/tests/support/process_tree.rs"]
mod process_tree;
#[path = "../../pp-core/tests/support/release.rs"]
mod release;

use std::{
    io::{BufRead, BufReader, Read, Write},
    net::TcpStream,
    path::Path,
    process::Stdio,
    time::{Duration, Instant},
};

/// One HTTP/1.1 exchange; returns the raw response.
fn exchange(origin: &str, method: &str, path: &str, cookie: &str, body: &str) -> String {
    let host = origin.trim_start_matches("http://");
    let mut stream = TcpStream::connect(host).unwrap();
    stream
        .set_read_timeout(Some(Duration::from_secs(30)))
        .unwrap();
    write!(
        stream,
        "{method} {path} HTTP/1.1\r\nHost: {host}\r\nOrigin: {origin}\r\nCookie: {cookie}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    )
    .unwrap();
    let mut response = String::new();
    stream.read_to_string(&mut response).unwrap();
    response
}

fn cookie(response: &str) -> String {
    response
        .lines()
        .find_map(|line| line.strip_prefix("set-cookie: "))
        .unwrap()
        .split(';')
        .next()
        .unwrap()
        .to_owned()
}

/// Asserts the live process tree under `pp-server`: no child in Rust-only
/// mode, the receipt's Node child otherwise.
fn check_children(server: u32, node: &serde_json::Value) {
    let children = process_tree::descendants(server);
    match node.as_u64() {
        None => assert_eq!(children, [], "Rust-only pp-server started a process"),
        Some(pid) => assert!(
            children.iter().any(|(child, _)| u64::from(*child) == pid),
            "Node child {pid} missing from {children:?}"
        ),
    }
}

/// Runs the journey and returns the gateway's proxy-attempt count, read from
/// `/__runtime` just before logout.
fn run(value: &str, root: &Path, rust: bool) -> u64 {
    let bundle = release::launch(root.join("data")).bundle;
    let credentials = root.join("credentials.json");
    let mut child = child_process::command(child_process::bin_exe("pp-server"))
        .args(["--data".as_ref(), root.join("data").as_os_str()])
        .args(["--web".as_ref(), bundle.web_root.as_os_str()])
        .args(["--node".as_ref(), bundle.node.as_os_str()])
        .args(["--commit", &bundle.commit])
        .args(["--test-credential-file".as_ref(), credentials.as_os_str()])
        .env("PP_DESKTOP_REBUILD", value)
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    let mut stdout = BufReader::new(child.stdout.take().unwrap());
    let mut origin = String::new();
    stdout.read_line(&mut origin).unwrap();
    let origin = origin.trim().to_owned();
    let receipt: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&credentials).unwrap()).unwrap();
    assert_eq!(receipt["child_pid"].is_null(), rust, "{receipt}");
    check_children(child.id(), &receipt["child_pid"]);
    let bootstrap = receipt["bootstrap_url"].as_str().unwrap();
    let response = exchange(
        &origin,
        "GET",
        bootstrap.strip_prefix(&origin).unwrap(),
        "",
        "",
    );
    assert!(response.starts_with("HTTP/1.1 303"), "{response}");
    let desktop = cookie(&response);
    let cookies = if rust {
        rust_routes(&origin, &desktop)
    } else {
        desktop
    };
    check_children(child.id(), &receipt["child_pid"]);
    let response = exchange(&origin, "GET", "/__runtime", &cookies, "");
    assert!(response.starts_with("HTTP/1.1 200"), "{response}");
    let runtime: serde_json::Value =
        serde_json::from_str(response.split("\r\n\r\n").nth(1).unwrap()).unwrap();
    let proxy_attempts = runtime["gateway"]["compat_forwards"].as_u64().unwrap();

    // The gateway's logout stops the binary, which prints its shutdown receipt.
    // With Rust routes, Rust revokes its session first.
    let response = exchange(&origin, "POST", "/auth/logout", &cookies, "");
    assert!(response.starts_with("HTTP/1.1 200"), "{response}");
    assert_eq!(
        response.contains("set-cookie: pp_session=;"),
        rust,
        "{response}"
    );
    let mut rest = String::new();
    stdout.read_to_string(&mut rest).unwrap();
    let deadline = Instant::now() + Duration::from_secs(30);
    let status = loop {
        if let Some(status) = child.try_wait().unwrap() {
            break status;
        }
        assert!(Instant::now() < deadline, "pp-server did not stop");
        std::thread::sleep(Duration::from_millis(50));
    };
    assert!(status.success(), "{rest}");
    let receipt: serde_json::Value = serde_json::from_str(rest.trim()).unwrap();
    assert_eq!(receipt["storage_released"], true, "{receipt}");
    assert_eq!(receipt["errors"], serde_json::json!([]), "{receipt}");
    proxy_attempts
}

/// Desktop admission alone is not a session; Rust auth answers until a user
/// registers.
fn rust_routes(origin: &str, desktop: &str) -> String {
    // The UI turns on its rebuild behavior only when health advertises this.
    let response = exchange(origin, "GET", "/health", desktop, "");
    assert!(response.starts_with("HTTP/1.1 200"), "{response}");
    assert!(
        response.contains(r#""tenant_feature_toggles""#),
        "{response}"
    );
    let response = exchange(origin, "GET", "/api/v1/plans", desktop, "");
    assert!(response.starts_with("HTTP/1.1 401"), "{response}");
    assert!(response.contains("Authentication required"), "{response}");
    let response = exchange(
        origin,
        "POST",
        "/auth/register",
        desktop,
        r#"{"email":"server@example.test","password":"password-1234"}"#,
    );
    assert!(response.starts_with("HTTP/1.1 200"), "{response}");
    let both = format!("{desktop}; {}", cookie(&response));
    let response = exchange(origin, "GET", "/api/v1/plans", &both, "");
    assert!(response.starts_with("HTTP/1.1 200"), "{response}");
    assert!(response.contains(r#""profiles":[]"#), "{response}");
    // Paths no Rust route serves get a Rust 404 instead of a proxy attempt.
    for (method, path) in [
        ("GET", "/api/v1/printers"),
        ("POST", "/api/v1/integrations"),
    ] {
        let response = exchange(origin, method, path, &both, "{}");
        assert!(
            response.starts_with("HTTP/1.1 404"),
            "{method} {path}: {response}"
        );
    }
    both
}

/// The generated journey report the operation inventory quotes. Never edit it
/// by hand: rerun this test with `PP_JOURNEY_REPORT=1`.
const JOURNEY_REPORT: &str = "docs/parity/journey-route-misses.json";

#[test]
fn slice0_mount_server_binary_serves_rust_routes() {
    let compiled = pp_core::DesktopRebuild::from_value(Some("1")).enabled();
    let mut runs = Vec::new();
    for value in ["1", "true"] {
        let root =
            std::env::temp_dir().join(format!("pp-slice0-server-{value}-{}", std::process::id()));
        std::fs::create_dir_all(&root).unwrap();
        let proxy_attempts = run(value, &root, compiled);
        std::fs::remove_dir_all(&root).unwrap();
        if compiled {
            assert_eq!(proxy_attempts, 0, "PP_DESKTOP_REBUILD={value}");
        }
        runs.push(
            serde_json::json!({"PP_DESKTOP_REBUILD": value, "proxy_attempts": proxy_attempts}),
        );
    }
    if std::env::var_os("PP_JOURNEY_REPORT").is_some_and(|value| value == "1") {
        assert!(
            compiled,
            "the journey report needs --features pp-core/desktop_rebuild"
        );
        let report = serde_json::json!({
            "generated_by": "PP_JOURNEY_REPORT=1 cargo nextest run (slice0_mount_server_binary_serves_rust_routes)",
            "journey": "pp-server binary, desktop_rebuild feature on: health, register, Plan reads, two unrouted paths, logout",
            "counted_at": "gateway route-miss path (Gateway::compat_forwards, /__runtime gateway.compat_forwards)",
            "runs": runs,
        });
        let path = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../..")
            .join(JOURNEY_REPORT);
        std::fs::write(
            path,
            format!("{}\n", serde_json::to_string_pretty(&report).unwrap()),
        )
        .unwrap();
    }
}
