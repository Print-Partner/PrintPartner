//! Live descendants of a process, read from the OS process table, so a
//! launcher test can show which child processes a runtime really started.
//! Shared by the runtime and `pp-server` launcher tests.

/// `(pid, command name)` of every descendant of `root`, zombies included:
/// a reaped-late child still proves it was spawned.
pub fn descendants(root: u32) -> Vec<(u32, String)> {
    let table = table();
    let mut found = Vec::new();
    let mut parents = vec![root];
    while let Some(parent) = parents.pop() {
        for (pid, ppid, name) in &table {
            if *ppid == parent && *pid != parent {
                found.push((*pid, name.clone()));
                parents.push(*pid);
            }
        }
    }
    found
}

/// `(pid, ppid, command name)` from `/proc/<pid>/stat`.
#[cfg(target_os = "linux")]
fn table() -> Vec<(u32, u32, String)> {
    std::fs::read_dir("/proc")
        .unwrap()
        .filter_map(|entry| {
            let pid: u32 = entry.ok()?.file_name().to_str()?.parse().ok()?;
            // A process can exit between listing and reading; skip it.
            let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
            let (open, close) = (stat.find('(')?, stat.rfind(')')?);
            let mut fields = stat[close + 1..].split_whitespace();
            let _state = fields.next()?;
            let ppid = fields.next()?.parse().ok()?;
            Some((pid, ppid, stat[open + 1..close].to_owned()))
        })
        .collect()
}

/// `(pid, ppid, command name)` from `ps`, without the `ps` process itself.
#[cfg(not(target_os = "linux"))]
fn table() -> Vec<(u32, u32, String)> {
    let child = std::process::Command::new("ps")
        .args(["-A", "-o", "pid=", "-o", "ppid=", "-o", "comm="])
        .stdout(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    let own = child.id();
    let output = child.wait_with_output().unwrap();
    assert!(output.status.success(), "ps failed: {:?}", output.status);
    String::from_utf8_lossy(&output.stdout)
        .lines()
        .filter_map(|line| {
            let mut parts = line.split_whitespace();
            let pid: u32 = parts.next()?.parse().ok()?;
            let ppid = parts.next()?.parse().ok()?;
            (pid != own).then(|| (pid, ppid, parts.collect::<Vec<_>>().join(" ")))
        })
        .collect()
}
