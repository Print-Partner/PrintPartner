//! First-run setup on the headless server: a start on a stopped import
//! leaves it as found, and `pp-server import` ends with what its destination
//! now holds. The workspace test build skips the ownership marker gate, so
//! the refusal here is first-run's own.
#![cfg(all(unix, feature = "desktop_rebuild"))]

#[path = "../../pp-storage/tests/common/process.rs"]
mod child_process;
#[path = "../../pp-core/tests/support/release.rs"]
mod release;

use pp_testkit::{Fixture, Installation, node_installation, snapshot};
use std::{
    path::{Path, PathBuf},
    process::{Output, Stdio},
    time::{Duration, Instant},
};

fn binary() -> PathBuf {
    if std::env::var_os("NEXTEST_BIN_EXE_pp-server").is_some() {
        child_process::bin_exe("pp-server")
    } else {
        env!("CARGO_BIN_EXE_pp-server").into()
    }
}

/// A Rust-only start on `data` with a loopback listener. A start that is
/// still running after 30 seconds is killed, and its output returned.
fn start(data: &Path) -> Output {
    let bundle = release::launch(data.to_path_buf()).bundle;
    let mut child = child_process::command(binary())
        .env("PP_DESKTOP_REBUILD", "1")
        .env("PP_LISTEN_ADDR", "127.0.0.1:0")
        .args(["--data".as_ref(), data.as_os_str()])
        .args(["--web".as_ref(), bundle.web_root.as_os_str()])
        .args(["--commit", &bundle.commit])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(30);
    while child.try_wait().unwrap().is_none() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(50));
    }
    let _ = child.kill();
    child.wait_with_output().unwrap()
}

/// `pp-server import --from <from> --data <data>`.
fn import(from: &Path, data: &Path) -> Output {
    child_process::command(binary())
        .env("PP_DESKTOP_REBUILD", "1")
        .args(["import".as_ref(), "--from".as_ref(), from.as_os_str()])
        .args(["--data".as_ref(), data.as_os_str()])
        .output()
        .unwrap()
}

fn last_line(bytes: &[u8]) -> String {
    let text = String::from_utf8(bytes.to_vec()).unwrap();
    text.lines().last().unwrap_or_default().to_owned()
}

#[test]
fn ticket_t_35_server_refuses_a_stopped_import_and_leaves_it_as_found() {
    let stopped = Installation::create(Fixture::Empty).unwrap();
    std::fs::create_dir(stopped.path().join(".pp-import")).unwrap();
    std::fs::write(
        stopped.path().join(".pp-import/print-partner.db"),
        b"partial",
    )
    .unwrap();
    let before = snapshot(stopped.path()).unwrap();

    let output = start(stopped.path());

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert_eq!(output.status.code(), Some(1), "{stderr}");
    assert!(
        stderr.contains(
            "This data folder holds an import that stopped before it finished. Print Partner \
             does not open it, and the Node folder it came from is unchanged. Import again \
             into a new, empty folder."
        ),
        "{stderr}"
    );
    assert!(
        output.stdout.is_empty(),
        "a refused start announced an origin"
    );
    assert_eq!(
        snapshot(stopped.path()).unwrap(),
        before,
        "the stopped import changed"
    );
}

#[test]
fn ticket_t_35_cli_prints_the_destination_after_a_refused_import() {
    let source = Installation::create(Fixture::Empty).unwrap();
    let parent = Installation::create(Fixture::Empty).unwrap();

    let output = import(source.path(), &parent.path().join("new"));

    assert_eq!(output.status.code(), Some(1));
    assert_eq!(
        last_line(&output.stderr),
        "destination: absent or empty; import again into it or into another empty folder"
    );
}

#[test]
fn ticket_t_35_cli_prints_the_destination_after_an_import() {
    let node = Installation::create(Fixture::NodeSchema34).unwrap();
    node_installation::populate(node.path()).unwrap();
    let before = snapshot(node.path()).unwrap();
    let parent = Installation::create(Fixture::Empty).unwrap();

    let output = import(node.path(), &parent.path().join("new"));

    assert_eq!(
        output.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        last_line(&output.stdout),
        "destination: an imported installation, ready to start with --data"
    );
    assert_eq!(
        snapshot(node.path()).unwrap(),
        before,
        "the Node folder changed"
    );
}
