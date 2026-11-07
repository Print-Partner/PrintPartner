//! The shipped binary stops on the platform's stop signal and finishes its
//! shutdown: SIGTERM on Unix, Ctrl+Break on Windows.

#[path = "../../pp-storage/tests/common/process.rs"]
mod child_process;
#[path = "../../pp-core/tests/support/release.rs"]
mod release;

use std::{
    io::{BufRead, BufReader, Read},
    process::{Child, Command, Stdio},
    time::{Duration, Instant},
};

#[cfg(unix)]
fn own_process_group(_command: &mut Command) {}

#[cfg(unix)]
fn send_stop_signal(child: &Child) {
    let status = Command::new("kill")
        .args(["-TERM", &child.id().to_string()])
        .status()
        .unwrap();
    assert!(status.success(), "kill -TERM failed");
}

/// Ctrl+Break reaches a process group; a new group keeps it away from the
/// test itself.
#[cfg(windows)]
fn own_process_group(command: &mut Command) {
    use std::os::windows::process::CommandExt;
    command.creation_flags(windows_sys::Win32::System::Threading::CREATE_NEW_PROCESS_GROUP);
}

#[cfg(windows)]
fn send_stop_signal(child: &Child) {
    use windows_sys::Win32::System::Console::{CTRL_BREAK_EVENT, GenerateConsoleCtrlEvent};
    // SAFETY: plain FFI call with a process group id; no memory is shared.
    let sent = unsafe { GenerateConsoleCtrlEvent(CTRL_BREAK_EVENT, child.id()) };
    assert_ne!(sent, 0, "Ctrl+Break: {}", std::io::Error::last_os_error());
}

#[test]
fn stop_signal_finishes_the_shutdown_and_releases_storage() {
    let root = std::env::temp_dir().join(format!("pp-stop-signal-{}", std::process::id()));
    std::fs::create_dir_all(&root).unwrap();
    let bundle = release::launch(root.join("data")).bundle;
    let mut command = child_process::command(child_process::bin_exe("pp-server"));
    command
        .args(["--data".as_ref(), root.join("data").as_os_str()])
        .args(["--web".as_ref(), bundle.web_root.as_os_str()])
        .args(["--node".as_ref(), bundle.node.as_os_str()])
        .args(["--commit", &bundle.commit])
        .stdout(Stdio::piped());
    own_process_group(&mut command);
    let mut child = command.spawn().unwrap();
    let mut stdout = BufReader::new(child.stdout.take().unwrap());
    let mut origin = String::new();
    stdout.read_line(&mut origin).unwrap();
    assert!(origin.starts_with("http://"), "{origin}");
    send_stop_signal(&child);
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
    std::fs::remove_dir_all(&root).unwrap();
}
