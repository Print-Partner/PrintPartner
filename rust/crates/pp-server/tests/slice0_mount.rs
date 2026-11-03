//! The shipped `pp-server` binary with both rebuild gates on serves the Rust
//! product routes without a Node child. With the Cargo feature off, the same
//! runtime values keep the Node-backed runtime.

#[path = "../../pp-storage/tests/common/process.rs"]
mod child_process;

use std::{
    io::{BufRead, BufReader, Read, Write},
    net::TcpStream,
    path::{Path, PathBuf},
    process::Stdio,
    time::{Duration, Instant},
};

/// `--web`, `--node` and `--commit` for the measured release this build embeds.
fn release() -> (PathBuf, PathBuf, String) {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../..")
        .canonicalize()
        .unwrap();
    let manifest = std::env::var_os("PP_DESKTOP_RELEASE_MANIFEST")
        .map(PathBuf::from)
        .unwrap_or_else(|| root.join("rust/bundle-manifest.json"));
    let inventory: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&manifest).unwrap()).unwrap();
    if inventory["dependencies"]["web_path"] == "." {
        let node = std::env::var_os("PP_TEST_NODE")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("/usr/bin/node"));
        let commit = inventory["commit"].as_str().unwrap().to_owned();
        return (root.join("web"), node, commit);
    }
    let stage = manifest.parent().unwrap().canonicalize().unwrap();
    let identity = if stage.join("release.json").is_file() {
        stage.join("release.json")
    } else {
        stage.join("Resources/desktop-runtime/release.json")
    };
    let release: serde_json::Value =
        serde_json::from_slice(&std::fs::read(identity).unwrap()).unwrap();
    (
        stage.join(release["web"].as_str().unwrap()),
        stage.join(release["node"].as_str().unwrap()),
        release["commit"].as_str().unwrap().to_owned(),
    )
}

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

fn run(value: &str, root: &Path, rust: bool) {
    let (web, node, commit) = release();
    let credentials = root.join("credentials.json");
    let mut child = child_process::command(child_process::bin_exe("pp-server"))
        .args(["--data".as_ref(), root.join("data").as_os_str()])
        .args(["--web".as_ref(), web.as_os_str()])
        .args(["--node".as_ref(), node.as_os_str()])
        .args(["--commit", &commit])
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
    if rust {
        rust_routes(&origin, &desktop);
    }

    // The gateway's logout stops the binary, which prints its shutdown receipt.
    let response = exchange(&origin, "POST", "/auth/logout", &desktop, "");
    assert!(response.starts_with("HTTP/1.1 200"), "{response}");
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
}

/// Desktop admission alone is not a session; Rust auth answers until a user
/// registers.
fn rust_routes(origin: &str, desktop: &str) {
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
}

#[test]
fn slice0_mount_server_binary_serves_rust_routes() {
    let compiled = pp_core::DesktopRebuild::from_value(Some("1")).enabled();
    for value in ["1", "true"] {
        let root =
            std::env::temp_dir().join(format!("pp-slice0-server-{value}-{}", std::process::id()));
        std::fs::create_dir_all(&root).unwrap();
        run(value, &root, compiled);
        std::fs::remove_dir_all(&root).unwrap();
    }
}
