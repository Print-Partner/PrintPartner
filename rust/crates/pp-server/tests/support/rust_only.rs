//! Start, wait for and stop a Rust-only `pp-server`, the start the Docker image
//! makes. The including test declares `child_process` and `release`.

use crate::{child_process, release};
use std::{
    io::Read,
    path::{Path, PathBuf},
    process::{Child, ExitStatus, Stdio},
    time::{Duration, Instant},
};

pub fn server_binary() -> PathBuf {
    if std::env::var_os("NEXTEST_BIN_EXE_pp-server").is_some() {
        child_process::bin_exe("pp-server")
    } else {
        env!("CARGO_BIN_EXE_pp-server").into()
    }
}

/// A running server. Dropping it, on any path including a panic or a failed
/// assertion, kills and reaps the child, so no server outlives the test.
pub struct Server {
    pub child: Child,
    /// `<root>/data`, the data directory.
    pub data: PathBuf,
    /// `<root>/status.json`, the status file.
    pub status: PathBuf,
    /// The status file's contents once it said `ready`.
    pub ready: serde_json::Value,
}

impl Server {
    /// Start without `--node` on `<root>/data`, keeping `<root>/status.json`,
    /// and wait until the status file says ready. Stdout and stderr are captured.
    pub fn start(root: &Path) -> Self {
        Self::start_with(root, |_| {})
    }

    pub fn start_with(root: &Path, configure: impl FnOnce(&mut std::process::Command)) -> Self {
        let (data, status) = (root.join("data"), root.join("status.json"));
        let bundle = release::launch(data.clone()).bundle;
        let mut command = child_process::command(server_binary());
        command
            .env("PP_DESKTOP_REBUILD", "1")
            .args(["--data".as_ref(), data.as_os_str()])
            .args(["--web".as_ref(), bundle.web_root.as_os_str()])
            .args(["--commit", &bundle.commit])
            .args(["--status-file".as_ref(), status.as_os_str()]);
        configure(&mut command);
        let child = command
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        let mut server = Self {
            child,
            data,
            status,
            ready: serde_json::Value::Null,
        };
        let deadline = Instant::now() + Duration::from_secs(60);
        loop {
            let state = std::fs::read(&server.status)
                .ok()
                .and_then(|bytes| serde_json::from_slice::<serde_json::Value>(&bytes).ok());
            if let Some(state) = state.filter(|state| state["state"] == "ready") {
                server.ready = state;
                return server;
            }
            if server.child.try_wait().unwrap().is_some() || Instant::now() >= deadline {
                let mut error = String::new();
                let _ = server
                    .child
                    .stderr
                    .take()
                    .unwrap()
                    .read_to_string(&mut error);
                panic!("pp-server never became ready: {error}");
            }
            std::thread::sleep(Duration::from_millis(50));
        }
    }

    pub fn origin(&self) -> &str {
        self.ready["origin"].as_str().unwrap()
    }

    /// Send SIGTERM, wait up to 30 s, and return the exit status with all of
    /// stdout and stderr.
    pub fn stop(mut self) -> (ExitStatus, String, String) {
        let pid = libc::pid_t::try_from(self.child.id()).unwrap();
        // SAFETY: plain FFI call with the child's pid; no memory is shared.
        assert_eq!(unsafe { libc::kill(pid, libc::SIGTERM) }, 0);
        let deadline = Instant::now() + Duration::from_secs(30);
        let exit = loop {
            if let Some(exit) = self.child.try_wait().unwrap() {
                break exit;
            }
            assert!(
                Instant::now() < deadline,
                "pp-server did not stop within 30 s of SIGTERM"
            );
            std::thread::sleep(Duration::from_millis(50));
        };
        let (mut stdout, mut stderr) = (String::new(), String::new());
        let _ = self
            .child
            .stdout
            .take()
            .unwrap()
            .read_to_string(&mut stdout);
        let _ = self
            .child
            .stderr
            .take()
            .unwrap()
            .read_to_string(&mut stderr);
        (exit, stdout, stderr)
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        if let Ok(None) = self.child.try_wait() {
            let _ = self.child.kill();
        }
        let _ = self.child.wait();
    }
}
