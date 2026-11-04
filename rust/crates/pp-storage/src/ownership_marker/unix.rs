//! Unix process probes for the `.desktop-owner.json` liveness check.

/// Whether `pid` may be running. Only `ESRCH` from `kill(pid, 0)` proves it is
/// not; any other result, including `EPERM`, counts as running.
pub(super) fn process_running(pid: u32) -> bool {
    let Ok(pid) = libc::pid_t::try_from(pid) else {
        return true;
    };
    let probed = unsafe { libc::kill(pid, 0) };
    probed == 0 || std::io::Error::last_os_error().raw_os_error() != Some(libc::ESRCH)
}

/// Node's process identity: `<boot id>:<start time>` from `/proc`. `None`
/// where `/proc` is unavailable, as on macOS.
pub(super) fn process_identity(pid: u32) -> Option<String> {
    let boot = std::fs::read_to_string("/proc/sys/kernel/random/boot_id").ok()?;
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    let start = stat
        .get(stat.rfind(')')? + 2..)?
        .split_whitespace()
        .nth(19)?;
    (!boot.trim().is_empty() && start.bytes().all(|b| b.is_ascii_digit()))
        .then(|| format!("{}:{start}", boot.trim()))
}
