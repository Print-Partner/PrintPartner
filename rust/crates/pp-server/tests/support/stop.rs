//! The platform's stop signal for a `pp-server` child: SIGTERM on Unix,
//! Ctrl+Break on Windows. Shared by the binary tests through `#[path]`.

use std::process::Child;

/// SIGTERM reaches only the process it names, so Unix needs no group.
#[cfg(unix)]
pub(crate) fn own_process_group<T>(_command: &mut T) {}

#[cfg(unix)]
pub(crate) fn send_stop_signal(child: &Child) {
    let pid = libc::pid_t::try_from(child.id()).unwrap();
    // SAFETY: plain FFI call with the child's pid; no memory is shared.
    let sent = unsafe { libc::kill(pid, libc::SIGTERM) };
    assert_eq!(sent, 0, "SIGTERM: {}", std::io::Error::last_os_error());
}

/// Ctrl+Break reaches a process group; a new group keeps it away from the
/// test itself.
#[cfg(windows)]
pub(crate) fn own_process_group(command: &mut impl std::os::windows::process::CommandExt) {
    command.creation_flags(windows_sys::Win32::System::Threading::CREATE_NEW_PROCESS_GROUP);
}

#[cfg(windows)]
pub(crate) fn send_stop_signal(child: &Child) {
    use windows_sys::Win32::System::Console::{CTRL_BREAK_EVENT, GenerateConsoleCtrlEvent};
    // SAFETY: plain FFI call with a process group id; no memory is shared.
    let sent = unsafe { GenerateConsoleCtrlEvent(CTRL_BREAK_EVENT, child.id()) };
    assert_ne!(sent, 0, "Ctrl+Break: {}", std::io::Error::last_os_error());
}
