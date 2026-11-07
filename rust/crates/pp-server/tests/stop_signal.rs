//! The shipped binary stops on the platform's stop signal and finishes its
//! shutdown: SIGTERM on Unix, Ctrl+Break on Windows.

#[path = "../../pp-storage/tests/common/process.rs"]
mod child_process;
#[path = "../../pp-core/tests/support/release.rs"]
mod release;
#[path = "../../pp-storage/tests/common/scratch.rs"]
mod scratch;

use std::{
    io::{BufRead, BufReader, Read},
    path::PathBuf,
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

fn server_binary() -> PathBuf {
    if std::env::var_os("NEXTEST_BIN_EXE_pp-server").is_some() {
        child_process::bin_exe("pp-server")
    } else {
        env!("CARGO_BIN_EXE_pp-server").into()
    }
}

#[test]
fn stop_signal_finishes_the_shutdown_and_releases_storage() {
    let root = scratch::Scratch::new("pp-stop-signal-");
    let bundle = release::launch(root.join("data")).bundle;
    let mut command = child_process::command(server_binary());
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
    let (sender, receiver) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let mut rest = String::new();
        let _ = stdout.read_to_string(&mut rest);
        let _ = sender.send(rest);
    });
    send_stop_signal(&child);
    let deadline = Instant::now() + Duration::from_secs(30);
    let status = loop {
        if let Some(status) = child.try_wait().unwrap() {
            break status;
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            panic!("pp-server did not stop within 30 s of the stop signal");
        }
        std::thread::sleep(Duration::from_millis(50));
    };
    let rest = receiver
        .recv_timeout(deadline.saturating_duration_since(Instant::now()))
        .expect("pp-server stdout closes after it exits");
    assert!(status.success(), "{rest}");
    let receipt: serde_json::Value = serde_json::from_str(rest.trim()).unwrap();
    assert_eq!(receipt["storage_released"], true, "{receipt}");
    assert_eq!(receipt["errors"], serde_json::json!([]), "{receipt}");
}
