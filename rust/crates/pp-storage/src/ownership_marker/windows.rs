//! Windows process probes for the `.desktop-owner.json` liveness check.

use windows_sys::Win32::{
    Foundation::{CloseHandle, ERROR_INVALID_PARAMETER, GetLastError, STILL_ACTIVE},
    System::Threading::{GetExitCodeProcess, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION},
};

/// Whether `pid` may be running. Only `OpenProcess` failing with
/// `ERROR_INVALID_PARAMETER` (no such process) or an exit code other than
/// `STILL_ACTIVE` proves it is not; access denied or any other failure counts
/// as running.
pub(super) fn process_running(pid: u32) -> bool {
    let process = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid) };
    if process.is_null() {
        return unsafe { GetLastError() } != ERROR_INVALID_PARAMETER;
    }
    let mut code = 0u32;
    let read = unsafe { GetExitCodeProcess(process, &mut code) } != 0;
    unsafe { CloseHandle(process) };
    !read || code == STILL_ACTIVE as u32
}

/// `None`: on Windows Node records a random per-instance identity that another
/// process cannot look up, so a running pid always counts as live.
pub(super) fn process_identity(_pid: u32) -> Option<String> {
    None
}
