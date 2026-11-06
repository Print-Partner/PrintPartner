use crate::Child;
use anyhow::{Context, Result};
use std::{process::ExitStatus, time::Duration};
use tokio::process::ChildStdin;

pub const SIGTERM_GRACE: Duration = Duration::from_secs(10);
const GROUP_REAP: Duration = Duration::from_secs(2);

#[derive(Debug)]
pub enum GroupExit {
    Observed(ExitStatus),
    Forced(ExitStatus),
}

#[derive(Debug)]
pub struct GroupStopReport {
    pub exit: GroupExit,
    pub eof_closed: bool,
    /// SIGTERM went to the group. Always false on Windows, where the closed
    /// stdin is the only stop request.
    pub sigterm_sent: bool,
    pub grace_exhausted: bool,
    pub elapsed: Duration,
    pub group_absent: bool,
    pub restarts_enabled: bool,
}

pub fn restarts_allowed(shutting_down: bool) -> bool {
    !shutting_down
}

pub fn process_alive(pid: u32) -> bool {
    crate::platform::process_alive(pid)
}

/// No process of the child's group is left: its Unix process group, or its
/// Windows Job Object.
pub fn process_group_absent(pid: u32) -> bool {
    !crate::platform::group_alive(pid)
}

/// Returns whether an existing group was killed: SIGKILL to the Unix process
/// group, or termination of the Windows Job Object.
pub fn reap_process_group(pid: u32) -> bool {
    crate::platform::kill_group(pid)
}

pub fn reject_and_arm_eof(
    eof_watchdog: bool,
    stdin: &mut Option<ChildStdin>,
    reject_new_work: &mut dyn FnMut(),
) -> bool {
    reject_new_work();
    if eof_watchdog {
        drop(stdin.take());
        true
    } else {
        false
    }
}

struct GroupKill(u32);

impl Drop for GroupKill {
    fn drop(&mut self) {
        reap_process_group(self.0);
    }
}

pub async fn drive_shutdown_group(
    child: &mut Child,
    pid: u32,
    eof_closed: bool,
    grace: Duration,
    already_exited: Option<ExitStatus>,
) -> Result<GroupStopReport> {
    let started = tokio::time::Instant::now();
    // A failed leader wait must not skip the group SIGKILL. Drop covers every
    // return, including the ones that used to leave via `?`.
    let _kill_group_on_return = GroupKill(pid);
    let mut failure = None;
    let mut exit = None;
    let mut sigterm_sent = false;
    let mut grace_exhausted = false;
    if let Some(status) = already_exited {
        exit = Some(GroupExit::Observed(status));
    } else {
        sigterm_sent = crate::platform::request_group_stop(pid);
        match tokio::time::timeout(grace, child.wait()).await {
            Ok(Ok(status)) => exit = Some(GroupExit::Observed(status)),
            Ok(Err(error)) => {
                failure = Some(anyhow::Error::from(error).context("Child reap failed"));
            }
            Err(_) => {
                grace_exhausted = true;
                reap_process_group(pid);
                let _ = child.start_kill();
                match tokio::time::timeout(GROUP_REAP, child.wait()).await {
                    Ok(Ok(status)) => exit = Some(GroupExit::Forced(status)),
                    Ok(Err(error)) => {
                        failure = Some(anyhow::Error::from(error).context("Child reap failed"));
                    }
                    Err(error) => {
                        failure = Some(anyhow::Error::from(error).context("Child reap timed out"));
                    }
                }
            }
        }
    }
    if reap_process_group(pid) {
        exit = exit.map(|exit| match exit {
            GroupExit::Observed(status) | GroupExit::Forced(status) => GroupExit::Forced(status),
        });
    }
    let group_absent = wait_until_group_absent(pid).await.is_ok();
    if let Some(error) = failure {
        return Err(if group_absent {
            error
        } else {
            error.context("Process group still present")
        });
    }
    Ok(GroupStopReport {
        exit: exit.context("Compatibility child outcome unavailable")?,
        eof_closed,
        sigterm_sent,
        grace_exhausted,
        elapsed: started.elapsed(),
        group_absent,
        restarts_enabled: false,
    })
}

async fn wait_until_group_absent(pid: u32) -> Result<()> {
    tokio::time::timeout(GROUP_REAP, async {
        loop {
            if process_group_absent(pid) {
                return;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .context("Process group still present")
}
