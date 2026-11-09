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
    process::{Child, Stdio},
    time::{Duration, Instant},
};

/// SIGTERM reaches only the process it names, so Unix needs no group.
#[cfg(unix)]
fn own_process_group<T>(_command: &mut T) {}

#[cfg(unix)]
fn send_stop_signal(child: &Child) {
    let pid = libc::pid_t::try_from(child.id()).unwrap();
    // SAFETY: plain FFI call with the child's pid; no memory is shared.
    let sent = unsafe { libc::kill(pid, libc::SIGTERM) };
    assert_eq!(sent, 0, "SIGTERM: {}", std::io::Error::last_os_error());
}

/// Ctrl+Break reaches a process group; a new group keeps it away from the
/// test itself.
#[cfg(windows)]
fn own_process_group(command: &mut impl std::os::windows::process::CommandExt) {
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

/// Ends the test: kills the child so it cannot outlive a failure.
fn fail(child: &mut Child, message: &str) -> ! {
    let _ = child.kill();
    let _ = child.wait();
    panic!("{message}");
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
    // Startup includes release verification and the 32 s readiness budget.
    let Ok(origin) = receiver.recv_timeout(Duration::from_secs(60)) else {
        fail(&mut child, "pp-server printed no origin within 60 s");
    };
    if !origin.starts_with("http://") {
        fail(&mut child, &format!("unexpected first line: {origin:?}"));
    }
    send_stop_signal(&child);
    let deadline = Instant::now() + Duration::from_secs(30);
    let status = loop {
        if let Some(status) = child.try_wait().unwrap() {
            break status;
        }
        if Instant::now() >= deadline {
            fail(
                &mut child,
                "pp-server did not stop within 30 s of the stop signal",
            );
        }
        std::thread::sleep(Duration::from_millis(50));
    };
    let rest = receiver
        .recv_timeout(Duration::from_secs(5))
        .expect("pp-server stdout closes within 5 s of its exit");
    assert!(status.success(), "{rest}");
    let receipt: serde_json::Value = serde_json::from_str(rest.trim()).unwrap();
    assert_eq!(receipt["storage_released"], true, "{receipt}");
    assert_eq!(receipt["errors"], serde_json::json!([]), "{receipt}");
}
