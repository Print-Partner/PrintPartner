//! `pp-server bootstrap-url` against a running Rust-only server: each URL signs
//! in once, an unused URL expires after 60 seconds, the wrong data directory
//! and a stopped server fail without writing anything, and no URL or token
//! reaches the server's stdout, stderr or status file.
#![cfg(all(unix, feature = "desktop_rebuild"))]

#[path = "../../pp-storage/tests/common/process.rs"]
mod child_process;
#[path = "../../pp-core/tests/support/release.rs"]
mod release;

use std::{
    io::{Read, Write},
    path::{Path, PathBuf},
    process::{Child, Output, Stdio},
    time::{Duration, Instant},
};

fn server_binary() -> PathBuf {
    if std::env::var_os("NEXTEST_BIN_EXE_pp-server").is_some() {
        child_process::bin_exe("pp-server")
    } else {
        env!("CARGO_BIN_EXE_pp-server").into()
    }
}

/// Kills and reaps the server on every exit path, including a panic.
struct Server(Child);

impl Drop for Server {
    fn drop(&mut self) {
        if let Ok(None) = self.0.try_wait() {
            let _ = self.0.kill();
        }
        let _ = self.0.wait();
    }
}

fn bootstrap_url(data: &Path) -> Output {
    child_process::command(server_binary())
        .args(["bootstrap-url", "--data"])
        .arg(data)
        .output()
        .unwrap()
}

fn fresh_url(data: &Path) -> String {
    let output = bootstrap_url(data);
    assert!(output.status.success(), "{output:?}");
    assert!(output.stderr.is_empty(), "{output:?}");
    let url = String::from_utf8(output.stdout).unwrap();
    url.strip_suffix('\n').unwrap().to_owned()
}

/// The token in a bootstrap URL, which must point at `origin`.
fn token(url: &str, origin: &str) -> String {
    let prefix = format!("{origin}/__desktop/bootstrap?token=");
    let token = url.strip_prefix(&prefix).unwrap_or_else(|| panic!("{url}"));
    assert_eq!(token.len(), 64, "{url}");
    token.to_owned()
}

/// The status line of a GET for the bootstrap URL with `token`.
fn redeem(origin: &str, token: &str) -> String {
    let authority = origin.strip_prefix("http://").unwrap();
    let mut stream = std::net::TcpStream::connect(authority).unwrap();
    stream
        .set_read_timeout(Some(Duration::from_secs(10)))
        .unwrap();
    write!(
        stream,
        "GET /__desktop/bootstrap?token={token} HTTP/1.1\r\nHost: {authority}\r\nConnection: close\r\n\r\n"
    )
    .unwrap();
    let mut response = String::new();
    stream.read_to_string(&mut response).unwrap();
    let status = response.lines().next().unwrap_or_default().to_owned();
    if status.contains(" 303 ") {
        assert!(
            response
                .lines()
                .any(|line| line.to_ascii_lowercase().starts_with("set-cookie:")),
            "a redeemed URL sets the session cookie"
        );
    }
    status
}

fn listing(directory: &Path) -> Vec<std::ffi::OsString> {
    let mut names: Vec<_> = std::fs::read_dir(directory)
        .unwrap()
        .map(|entry| entry.unwrap().file_name())
        .collect();
    names.sort();
    names
}

#[test]
fn bootstrap_url_signs_in_once_expires_and_never_reaches_server_output() {
    // Under /tmp: the socket path must fit in about 100 bytes, which macOS's
    // per-user temporary directory leaves no room for.
    let root = tempfile::Builder::new()
        .prefix("ppb-")
        .tempdir_in("/tmp")
        .unwrap();
    let root = root.path();
    let data = root.join("data");
    let status = root.join("status.json");
    let bundle = release::launch(data.clone()).bundle;
    let mut server = Server(
        child_process::command(server_binary())
            .env("PP_DESKTOP_REBUILD", "1")
            .args(["--data".as_ref(), data.as_os_str()])
            .args(["--web".as_ref(), bundle.web_root.as_os_str()])
            .args(["--commit", &bundle.commit])
            .args(["--status-file".as_ref(), status.as_os_str()])
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap(),
    );
    let child = &mut server.0;
    let deadline = Instant::now() + Duration::from_secs(60);
    let origin = loop {
        let state = std::fs::read(&status)
            .ok()
            .and_then(|bytes| serde_json::from_slice::<serde_json::Value>(&bytes).ok());
        if let Some(state) = state.filter(|state| state["state"] == "ready") {
            break state["origin"].as_str().unwrap().to_owned();
        }
        if child.try_wait().unwrap().is_some() || Instant::now() >= deadline {
            let mut error = String::new();
            let _ = child.stderr.take().unwrap().read_to_string(&mut error);
            panic!("pp-server never became ready: {error}");
        }
        std::thread::sleep(Duration::from_millis(50));
    };

    // Single use: the first GET signs in, the second is refused.
    let first = token(&fresh_url(&data), &origin);
    assert!(redeem(&origin, &first).contains(" 303 "));
    assert!(redeem(&origin, &first).contains(" 401 "));

    // Expiry: an unused URL works only within 60 seconds.
    let unused = token(&fresh_url(&data), &origin);
    let minted = Instant::now();
    assert_ne!(unused, first, "each request mints a new token");

    // The wrong directory, an empty one this account owns, fails and stays empty.
    let other = root.join("other");
    std::fs::create_dir(&other).unwrap();
    let output = bootstrap_url(&other);
    assert_eq!(output.status.code(), Some(1));
    assert!(output.stdout.is_empty());
    assert_eq!(
        String::from_utf8_lossy(&output.stderr),
        format!(
            "bootstrap-url: No pp-server is running on {}\n",
            other.display()
        )
    );
    assert!(listing(&other).is_empty());

    let recorded = std::fs::read_to_string(&status).unwrap();
    std::thread::sleep(Duration::from_secs(61).saturating_sub(minted.elapsed()));
    assert!(redeem(&origin, &unused).contains(" 401 "));

    let pid = libc::pid_t::try_from(child.id()).unwrap();
    // SAFETY: plain FFI call with the child's pid; no memory is shared.
    assert_eq!(unsafe { libc::kill(pid, libc::SIGTERM) }, 0);
    let deadline = Instant::now() + Duration::from_secs(30);
    let exit = loop {
        if let Some(exit) = child.try_wait().unwrap() {
            break exit;
        }
        assert!(
            Instant::now() < deadline,
            "pp-server did not stop within 30 s"
        );
        std::thread::sleep(Duration::from_millis(50));
    };
    assert!(exit.success());
    let mut printed = String::new();
    child
        .stdout
        .take()
        .unwrap()
        .read_to_string(&mut printed)
        .unwrap();
    child
        .stderr
        .take()
        .unwrap()
        .read_to_string(&mut printed)
        .unwrap();
    for text in [&printed, &recorded] {
        for secret in [
            first.as_str(),
            unused.as_str(),
            "__desktop/bootstrap",
            "token=",
        ] {
            assert!(!text.contains(secret), "{secret:?} leaked into {text}");
        }
    }

    // A stopped server takes its socket with it, and the command fails.
    assert!(!data.join(".pp-server-bootstrap.sock").exists());
    let before = listing(&data);
    let output = bootstrap_url(&data);
    assert_eq!(output.status.code(), Some(1));
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("No pp-server is running"),
        "{output:?}"
    );
    assert_eq!(listing(&data), before);
}

#[test]
fn bootstrap_url_help_exits_zero() {
    let output = child_process::command(server_binary())
        .args(["bootstrap-url", "--help"])
        .output()
        .unwrap();
    assert!(output.status.success());
    assert!(String::from_utf8_lossy(&output.stdout).starts_with("Usage: pp-server bootstrap-url"));
}

#[test]
fn bootstrap_url_refuses_a_directory_another_account_owns() {
    // SAFETY: geteuid has no preconditions and touches no memory.
    if unsafe { libc::geteuid() } == 0 {
        return; // root owns `/`; the unit tests cover a foreign owner.
    }
    let output = bootstrap_url(Path::new("/"));
    assert_eq!(output.status.code(), Some(1));
    assert!(output.stdout.is_empty());
    assert_eq!(
        String::from_utf8_lossy(&output.stderr),
        "bootstrap-url: / belongs to another account; run this as the account that runs pp-server\n"
    );
}
