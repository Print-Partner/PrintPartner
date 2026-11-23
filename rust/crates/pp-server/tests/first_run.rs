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

fn run_startup_until_exit_or_timeout(data: &Path) -> Output {
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

fn import(from: &Path, data: &Path) -> Output {
    child_process::command(binary())
        .env("PP_DESKTOP_REBUILD", "1")
        .args(["import".as_ref(), "--from".as_ref(), from.as_os_str()])
        .args(["--data".as_ref(), data.as_os_str()])
        .output()
        .unwrap()
}

fn leave_as_a_publish_that_failed_after_moving_every_root(destination: &Path) {
    std::fs::remove_file(destination.join("import-report.json")).unwrap();
    std::fs::remove_file(destination.join(".pp-rust-owned.json")).unwrap();
}

fn last_line(bytes: &[u8]) -> String {
    let text = String::from_utf8(bytes.to_vec()).unwrap();
    text.lines().last().unwrap_or_default().to_owned()
}

fn node_source() -> Installation {
    let node = Installation::create(Fixture::NodeSchema34).unwrap();
    node_installation::populate(node.path()).unwrap();
    node
}

/// A link named `link` in `parent`, pointing at `target`.
fn link_in(parent: &Installation, target: &Path) -> PathBuf {
    let link = parent.path().join("link");
    std::os::unix::fs::symlink(target, &link).unwrap();
    link
}

fn assert_imported(output: &Output) {
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
}

/// Starts the server on a stopped import, directly or through a link, and
/// checks it refuses before writing anything there.
fn assert_start_refuses_a_stopped_import(through_a_link: bool) {
    let stopped = Installation::create(Fixture::Empty).unwrap();
    std::fs::create_dir(stopped.path().join(".pp-import")).unwrap();
    std::fs::write(
        stopped.path().join(".pp-import/print-partner.db"),
        b"partial",
    )
    .unwrap();
    let before = snapshot(stopped.path()).unwrap();
    let links = Installation::create(Fixture::Empty).unwrap();
    let data = if through_a_link {
        link_in(&links, stopped.path())
    } else {
        stopped.path().to_path_buf()
    };

    let output = run_startup_until_exit_or_timeout(&data);

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
fn ticket_t_35_server_refuses_a_stopped_import_and_leaves_it_as_found() {
    assert_start_refuses_a_stopped_import(false);
}

#[test]
fn ticket_t_35_server_refuses_a_linked_stopped_import_and_leaves_it_as_found() {
    assert_start_refuses_a_stopped_import(true);
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
    let node = node_source();
    let before = snapshot(node.path()).unwrap();
    let parent = Installation::create(Fixture::Empty).unwrap();

    assert_imported(&import(node.path(), &parent.path().join("new")));
    assert_eq!(
        snapshot(node.path()).unwrap(),
        before,
        "the Node folder changed"
    );
}

#[test]
fn ticket_t_35_cli_imports_into_a_linked_empty_folder() {
    let node = node_source();
    let empty = Installation::create(Fixture::Empty).unwrap();
    let links = Installation::create(Fixture::Empty).unwrap();

    assert_imported(&import(node.path(), &link_in(&links, empty.path())));
    assert!(
        empty.path().join(".pp-rust-owned.json").is_file(),
        "the import did not write through the link"
    );
}

#[test]
fn ticket_t_35_cli_does_not_offer_a_folder_the_importer_refuses() {
    let node = node_source();
    let live_owner = format!("{{\"pid\": {}}}", std::process::id());
    for (name, bytes) in [
        (".desktop-lease/owner.json", "{}"),
        (".desktop-owner.json", live_owner.as_str()),
    ] {
        let destination = Installation::create(Fixture::Empty).unwrap();
        let path = destination.path().join(name);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, bytes).unwrap();

        let output = import(node.path(), destination.path());

        assert_eq!(output.status.code(), Some(1), "{name}");
        assert_eq!(
            last_line(&output.stderr),
            "destination: not an import destination",
            "{name}"
        );
    }
}

#[test]
fn ticket_t_35_cli_does_not_call_a_failed_publish_a_node_installation() {
    let node = node_source();
    let parent = Installation::create(Fixture::Empty).unwrap();
    let destination = parent.path().join("new");
    let first = import(node.path(), &destination);
    assert_eq!(
        first.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&first.stderr)
    );
    leave_as_a_publish_that_failed_after_moving_every_root(&destination);
    let before = snapshot(&destination).unwrap();

    let retry = import(node.path(), &destination);

    assert_eq!(
        retry.status.code(),
        Some(1),
        "{}",
        String::from_utf8_lossy(&retry.stderr)
    );
    assert_eq!(
        last_line(&retry.stderr),
        "destination: a database without an ownership marker: a Node installation, or an \
         import that failed while moving its copy into place; not an import destination"
    );
    assert_eq!(
        snapshot(&destination).unwrap(),
        before,
        "the refused retry changed the folder"
    );
}
