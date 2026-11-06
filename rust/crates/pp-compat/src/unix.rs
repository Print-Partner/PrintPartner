//! Unix supervision: a process group per child, signals, a parent-death
//! watcher process, and a private Unix socket the child listens on.

use std::{
    fs::{File, OpenOptions},
    os::{
        fd::{AsRawFd, OwnedFd},
        unix::{
            fs::{MetadataExt, OpenOptionsExt},
            process::ExitStatusExt,
        },
    },
    path::{Path, PathBuf},
    process::ExitStatus,
};
pub use tokio::process::Child;
use tokio::process::Command;

pub(crate) use crate::parent_death::ParentDeathGuard;

/// Environment names the child inherits from the core.
pub(crate) const INHERITED_ENV: &[&str] = &["PATH", "HOME", "TMPDIR", "LANG", "LC_ALL", "TZ"];

/// The parent end of the watcher pipe that the spawned child must not keep.
pub(crate) type ChildSetup = OwnedFd;

/// Put the child in its own process group, arm the parent-death watcher, and
/// hand it the storage lock as descriptor 198.
pub(crate) fn configure_child(
    command: &mut Command,
    storage_lock: &File,
) -> std::io::Result<(ChildSetup, ParentDeathGuard)> {
    crate::parent_death::configure_child(command, storage_lock.as_raw_fd())
}

pub(crate) fn spawn(command: &mut Command, _setup: &ChildSetup) -> std::io::Result<Child> {
    command.spawn()
}

/// Close the watcher pipe's read end in the parent. `pre_exec` already put
/// the child in its group.
pub(crate) fn adopt(
    setup: ChildSetup,
    _guard: &mut ParentDeathGuard,
    _child: &Child,
) -> std::io::Result<()> {
    drop(setup);
    Ok(())
}

/// Ask every process in the group to stop with SIGTERM.
pub(crate) fn request_group_stop(pid: u32) -> bool {
    if pid > 0 {
        unsafe { libc::kill(-(pid as i32), libc::SIGTERM) };
    }
    true
}

/// SIGKILL the group. True when the group existed.
pub(crate) fn kill_group(pid: u32) -> bool {
    pid > 0 && unsafe { libc::kill(-(pid as i32), libc::SIGKILL) } == 0
}

pub(crate) fn group_alive(pid: u32) -> bool {
    if pid == 0 {
        return false;
    }
    let result = unsafe { libc::kill(-(pid as i32), 0) };
    result == 0 || std::io::Error::last_os_error().raw_os_error() != Some(libc::ESRCH)
}

pub(crate) fn process_alive(pid: u32) -> bool {
    if pid == 0 {
        return false;
    }
    let result = unsafe { libc::kill(pid as i32, 0) };
    result == 0 || std::io::Error::last_os_error().raw_os_error() != Some(libc::ESRCH)
}

/// The child ended from the stop signals the parent sends.
pub(crate) fn stopped_by_parent(status: &ExitStatus) -> bool {
    status.signal() == Some(libc::SIGKILL) || status.signal() == Some(libc::SIGTERM)
}

pub(crate) fn signal(status: &ExitStatus) -> Option<i32> {
    status.signal()
}

/// Open `path` for reading without following a final link or blocking on a FIFO.
pub(crate) fn open_nofollow(path: &Path) -> std::io::Result<File> {
    OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(path)
}

/// Node takes Unix paths as they are.
pub(crate) fn node_path(path: &Path) -> PathBuf {
    path.to_path_buf()
}

/// `--import` takes an absolute Unix path.
pub(crate) fn import_specifier(path: &Path) -> std::io::Result<std::ffi::OsString> {
    Ok(path.into())
}

/// Junctions exist only on Windows.
pub(crate) fn is_junction(_path: &Path) -> std::io::Result<bool> {
    Ok(false)
}

/// A regular file owned by this user with mode 0600.
pub(crate) fn private_file(file: &File) -> std::io::Result<bool> {
    let metadata = file.metadata()?;
    Ok(metadata.is_file()
        && metadata.uid() == unsafe { libc::geteuid() }
        && metadata.mode() & 0o7777 == 0o600)
}

/// `<runtime_dir>/<generation>.sock`. The child binds it and makes it 0600
/// inside the private runtime directory.
pub(crate) struct Channel {
    path: PathBuf,
}

pub(crate) type Stream = tokio::net::UnixStream;

impl Channel {
    pub(crate) fn bind(runtime_dir: &Path, generation: &str) -> std::io::Result<Self> {
        Ok(Self::unbound(
            runtime_dir.join(format!("{}.sock", &generation[..12])),
        ))
    }

    pub(crate) fn unbound(path: PathBuf) -> Self {
        Self { path }
    }

    /// The address the child receives as `socket_path`.
    pub(crate) fn name(&self) -> &Path {
        &self.path
    }

    pub(crate) async fn connect(&self) -> std::io::Result<Stream> {
        Stream::connect(&self.path).await
    }

    pub(crate) async fn close(&self) -> std::io::Result<()> {
        match tokio::fs::remove_file(&self.path).await {
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
            result => result,
        }
    }
}

/// A test child's end of the channel: it binds `<dir>/node.sock` and accepts
/// the connections the core dials, as Node does.
pub(crate) struct FakeChild(tokio::net::UnixListener);

pub(crate) type FakeStream = Stream;

pub(crate) fn fake_channel(dir: &Path) -> std::io::Result<(Channel, FakeChild)> {
    let path = dir.join("node.sock");
    let listener = tokio::net::UnixListener::bind(&path)?;
    Ok((Channel::unbound(path), FakeChild(listener)))
}

impl FakeChild {
    pub(crate) async fn accept(&mut self) -> std::io::Result<FakeStream> {
        Ok(self.0.accept().await?.0)
    }
}

/// The interpreter for test child scripts: `/bin/sh -c` with a fixed `PATH`.
#[cfg(test)]
pub(crate) fn test_shell(script: &str) -> Command {
    let mut command = Command::new("/bin/sh");
    command
        .arg("-c")
        .arg(script)
        .env_clear()
        .env("PATH", "/usr/bin:/bin");
    command
}

/// Fixtures for the supervision tests in `lib.rs`.
#[cfg(test)]
pub(crate) mod test_support {
    use super::{Channel, Child, ParentDeathGuard};
    use std::{
        os::unix::process::ExitStatusExt,
        path::Path,
        process::{ExitStatus, Stdio},
    };
    use tokio::process::Command;

    /// Exits 0 at once.
    pub(crate) const EXITS: &str = "exit 0";

    /// Exits 0 on SIGTERM after printing `terminated`, and prints `ready` once
    /// the trap is set. The group SIGTERM can land in a forked child before it
    /// execs `sleep`; the child's inherited trap swallows it and a `sleep 30`
    /// then outlives the grace period (GRE-719). Short sleeps bound that to
    /// 0.1 s, and the shell still runs its own trap once the current sleep ends.
    pub(crate) const COOPERATIVE: &str =
        "trap 'echo terminated; exit 0' TERM; echo ready; while :; do sleep 0.1; done";

    /// Ignores SIGTERM and stdin closing.
    pub(crate) const STUBBORN: &str = "trap '' TERM; while true; do sleep 30; done";

    pub(crate) fn exited(code: u8) -> ExitStatus {
        ExitStatus::from_raw(i32::from(code) << 8)
    }

    pub(crate) fn crashed() -> ExitStatus {
        ExitStatus::from_raw(libc::SIGABRT)
    }

    /// Ended by the parent's group SIGKILL.
    pub(crate) fn killed() -> ExitStatus {
        ExitStatus::from_raw(libc::SIGKILL)
    }

    /// Ended by the parent's group SIGTERM.
    pub(crate) fn terminated() -> ExitStatus {
        ExitStatus::from_raw(libc::SIGTERM)
    }

    /// Spawn `command` as the leader of its own process group with piped stdin
    /// and stdout. The group needs no guard.
    pub(crate) fn spawn_group(command: &mut Command) -> (Child, Option<ParentDeathGuard>) {
        let child = command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .process_group(0)
            .kill_on_drop(true)
            .spawn()
            .unwrap();
        (child, None)
    }

    pub(crate) type Listener = tokio::net::UnixListener;

    /// A channel whose socket is bound, so closing it has a file to remove.
    pub(crate) fn channel(runtime_dir: &Path, _generation: &str) -> (Channel, Listener) {
        let path = runtime_dir.join("compat.sock");
        let listener = Listener::bind(&path).unwrap();
        (Channel::unbound(path), listener)
    }

    /// Open a client connection to the channel. After `close` the socket file
    /// is gone, so this fails with `NotFound`.
    pub(crate) async fn channel_open(channel: &Channel) -> std::io::Result<()> {
        channel.connect().await.map(drop)
    }

    /// Accept one connection to the channel and hold it unanswered.
    pub(crate) async fn hold_connection(
        listener: Listener,
        _name: &Path,
    ) -> tokio::net::UnixStream {
        listener.accept().await.unwrap().0
    }
}
