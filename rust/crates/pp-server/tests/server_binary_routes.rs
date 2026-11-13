//! The shipped `pp-server` binary with both rebuild gates on serves the Rust
//! product routes without a Node child. With the Cargo feature off, the same
//! runtime values keep the Node-backed runtime.

#[path = "../../pp-storage/tests/common/process.rs"]
mod child_process;
#[path = "../../pp-core/tests/support/process_tree.rs"]
mod process_tree;
#[path = "../../pp-core/tests/support/release.rs"]
mod release;
#[path = "support/stop.rs"]
mod stop;

use std::{
    io::{BufRead, BufReader, Read, Write},
    net::TcpStream,
    path::Path,
    process::Stdio,
    time::{Duration, Instant},
};

/// One HTTP/1.1 exchange; returns the raw response.
fn exchange(origin: &str, method: &str, path: &str, cookie: &str, body: &str) -> String {
    exchange_with(origin, method, path, cookie, body, "")
}

/// `exchange` with extra header lines, each ending in `\r\n`.
fn exchange_with(
    origin: &str,
    method: &str,
    path: &str,
    cookie: &str,
    body: &str,
    headers: &str,
) -> String {
    let host = origin.trim_start_matches("http://");
    let mut stream = TcpStream::connect(host).unwrap();
    stream
        .set_read_timeout(Some(Duration::from_secs(30)))
        .unwrap();
    write!(
        stream,
        "{method} {path} HTTP/1.1\r\nHost: {host}\r\nOrigin: {origin}\r\nCookie: {cookie}\r\n{headers}Content-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
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

fn run(value: &str, root: &Path, rust: bool) {
    let bundle = release::launch(root.join("data")).bundle;
    let credentials = root.join("credentials.json");
    let mut command = child_process::command(child_process::bin_exe("pp-server"));
    stop::own_process_group(&mut command);
    let mut child = command
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
        desktop.clone()
    };
    check_children(child.id(), &receipt["child_pid"]);

    // With Rust routes, logout revokes the Rust session and the headless
    // server keeps serving until its stop signal (GRE-1453). The Node-backed
    // runtime's logout still stops the binary. Either way it prints its
    // shutdown receipt.
    let response = exchange(&origin, "POST", "/auth/logout", &cookies, "");
    assert!(response.starts_with("HTTP/1.1 200"), "{response}");
    assert_eq!(
        response.contains("set-cookie: pp_session=;"),
        rust,
        "{response}"
    );
    if rust {
        // Logout no longer ends the child, so a failed check must.
        let checked = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            signed_out_keeps_serving(&origin, &desktop, &cookies, &mut child)
        }));
        if let Err(panic) = checked {
            let _ = child.kill();
            let _ = child.wait();
            std::panic::resume_unwind(panic);
        }
        stop::send_stop_signal(&child);
    }
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

/// After logout the headless server still runs: health reports signed out,
/// the sign-in page loads, the old Rust session authorizes nothing, and
/// signing in again works without a restart.
fn signed_out_keeps_serving(
    origin: &str,
    desktop: &str,
    signed_out: &str,
    child: &mut std::process::Child,
) {
    let running = |child: &mut std::process::Child| child.try_wait().unwrap().is_none();
    assert!(running(child), "pp-server stopped on logout");
    let response = exchange(origin, "GET", "/health", desktop, "");
    assert!(response.starts_with("HTTP/1.1 200"), "{response}");
    assert!(response.contains(r#""authenticated":false"#), "{response}");
    let response = exchange_with(
        origin,
        "GET",
        "/login",
        desktop,
        "",
        "Accept: text/html\r\n",
    );
    assert!(response.starts_with("HTTP/1.1 200"), "{response}");
    assert!(response.contains("content-type: text/html"), "{response}");
    for path in ["/auth/me", "/api/v1/plans"] {
        let response = exchange(origin, "GET", path, signed_out, "");
        assert!(response.starts_with("HTTP/1.1 401"), "{path}: {response}");
    }
    let response = exchange(
        origin,
        "POST",
        "/auth/login",
        desktop,
        r#"{"email":"server@example.test","password":"password-1234"}"#,
    );
    assert!(response.starts_with("HTTP/1.1 200"), "{response}");
    let again = format!("{desktop}; {}", cookie(&response));
    let response = exchange(origin, "GET", "/api/v1/plans", &again, "");
    assert!(response.starts_with("HTTP/1.1 200"), "{response}");
    assert!(running(child), "pp-server stopped after signing in again");
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
    // Paths no Rust route serves get a Rust 404.
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
