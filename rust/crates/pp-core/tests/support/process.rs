//! Kill, stop and resume a process by PID, for tests that drive the
//! supervised Node child. Unix sends SIGKILL, SIGSTOP and SIGCONT. Windows
//! terminates the process, or suspends and resumes every thread in it with
//! `NtSuspendProcess` and `NtResumeProcess`, which is what SIGSTOP and
//! SIGCONT do for a Unix process.

pub use platform::{kill, resume, stop};

#[cfg(unix)]
mod platform {
    fn signal(pid: u32, signal: libc::c_int) -> bool {
        unsafe { libc::kill(pid as i32, signal) == 0 }
    }

    pub fn kill(pid: u32) -> bool {
        signal(pid, libc::SIGKILL)
    }

    pub fn stop(pid: u32) {
        assert!(signal(pid, libc::SIGSTOP), "SIGSTOP {pid} failed");
    }

    pub fn resume(pid: u32) -> bool {
        signal(pid, libc::SIGCONT)
    }
}

#[cfg(windows)]
mod platform {
    use windows_sys::Win32::{
        Foundation::{CloseHandle, HANDLE},
        System::Threading::{
            OpenProcess, PROCESS_SUSPEND_RESUME, PROCESS_TERMINATE, TerminateProcess,
        },
    };

    #[link(name = "ntdll")]
    unsafe extern "system" {
        fn NtSuspendProcess(process: HANDLE) -> i32;
        fn NtResumeProcess(process: HANDLE) -> i32;
    }

    /// Run `call` on a handle to `pid` with `access`; false when the
    /// process cannot be opened or `call` fails.
    fn with_process(pid: u32, access: u32, call: impl FnOnce(HANDLE) -> bool) -> bool {
        let process = unsafe { OpenProcess(access, 0, pid) };
        if process.is_null() {
            return false;
        }
        let done = call(process);
        unsafe { CloseHandle(process) };
        done
    }

    pub fn kill(pid: u32) -> bool {
        with_process(pid, PROCESS_TERMINATE, |process| unsafe {
            TerminateProcess(process, 1) != 0
        })
    }

    pub fn stop(pid: u32) {
        let stopped = with_process(pid, PROCESS_SUSPEND_RESUME, |process| unsafe {
            NtSuspendProcess(process) == 0
        });
        assert!(stopped, "NtSuspendProcess {pid} failed");
    }

    pub fn resume(pid: u32) -> bool {
        with_process(pid, PROCESS_SUSPEND_RESUME, |process| unsafe {
            NtResumeProcess(process) == 0
        })
    }
}
