//! A Rust-only `pp-server` starts without `--node`, keeps its status file, and
//! `pp-server healthcheck` reads it: live and ready while serving, gone after
//! SIGTERM. This is the start the Docker image makes.
#![cfg(all(unix, feature = "desktop_rebuild"))]

#[path = "../../pp-storage/tests/common/process.rs"]
mod child_process;
#[path = "../../pp-core/tests/support/release.rs"]
mod release;
#[path = "../../pp-storage/tests/common/scratch.rs"]
mod scratch;

use std::{
    io::{BufRead, BufReader, Read},
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

/// Owns the server process: dropping it, on any path including a panic or a
/// failed assertion, kills and reaps the child, so no server outlives the test.
struct Server(Child);

impl Drop for Server {
    fn drop(&mut self) {
        if let Ok(None) = self.0.try_wait() {
            let _ = self.0.kill();
        }
        let _ = self.0.wait();
    }
}

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
    let mut stdout = BufReader::new(child.stdout.take().unwrap());
    let mut stderr = child.stderr.take().unwrap();
    let (sender, receiver) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let mut origin = String::new();
        let _ = stdout.read_line(&mut origin);
        if sender.send(origin).is_err() {
            return;
        }
        let mut rest = String::new();
        let _ = stdout.read_to_string(&mut rest);
        let _ = sender.send(rest);
    });
    let Ok(origin) = receiver.recv_timeout(Duration::from_secs(60)) else {
        panic!("pp-server printed no origin within 60 s");
    };
    if !origin.starts_with("http://127.0.0.1:") {
        let mut error = String::new();
        let _ = stderr.read_to_string(&mut error);
        panic!("unexpected first line: {origin:?}, stderr: {error}");
    }
    assert!(
        data.join(".pp-rust-owned.json").is_file(),
        "Rust owns the data directory"
    );
    let recorded: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&status).unwrap()).unwrap();
    assert_eq!(recorded["state"], "ready");
    assert_eq!(recorded["pid"], child.id());
    assert_eq!(recorded["origin"], origin.trim());
    for (probe, line) in [
        ("liveness", "liveness ok (ready)\n"),
        ("readiness", "readiness ok (ready)\n"),
    ] {
        let output = healthcheck(probe, &status);
        assert!(output.status.success(), "{output:?}");
        assert_eq!(String::from_utf8_lossy(&output.stdout), line);
    }

    let pid = libc::pid_t::try_from(child.id()).unwrap();
    // SAFETY: plain FFI call with the child's pid; no memory is shared.
    assert_eq!(unsafe { libc::kill(pid, libc::SIGTERM) }, 0);
    let deadline = Instant::now() + Duration::from_secs(30);
    let exit = loop {
        if let Some(exit) = child.try_wait().unwrap() {
            break exit;
        }
        if Instant::now() >= deadline {
            panic!("pp-server did not stop within 30 s of SIGTERM");
        }
        std::thread::sleep(Duration::from_millis(50));
    };
    let rest = receiver.recv_timeout(Duration::from_secs(5)).unwrap();
    assert!(exit.success(), "{rest}");
    let receipt: serde_json::Value = serde_json::from_str(rest.trim()).unwrap();
    assert_eq!(receipt["storage_released"], true, "{receipt}");
    assert_eq!(receipt["errors"], serde_json::json!([]), "{receipt}");
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
