//! Windows: rfd dialogs, the URL protocol handler, Ctrl+Break and console
//! close, and a private named pipe for focus.

use crate::focus::{Focus, Identity, LIMIT, TIMEOUT, answer};
pub use crate::rfd_dialogs::{message, notice, startup_error};
use anyhow::{Context, Result};
use pp_os::windows_security::{CurrentUser, PrivateDescriptor, SE_FILE_OBJECT};
use std::{
    io::{BufRead, BufReader, Read, Write},
    path::Path,
    process::Command,
};
use tokio::net::windows::named_pipe::{NamedPipeServer, ServerOptions};
use windows_sys::Win32::Security::SECURITY_ATTRIBUTES;

#[cfg(feature = "desktop_rebuild")]
pub(crate) const HOST: pp_core::layout::Platform = pp_core::layout::Platform::Windows;

/// The command that opens a URL or folder given as its last argument.
pub fn opener() -> Command {
    let mut command = Command::new("rundll32");
    command.arg("url.dll,FileProtocolHandler");
    command
}

/// Explorer has no reveal that exits 0, so the folder opens instead.
pub const REVEAL: &[&str] = &[];

/// Open at login is a macOS LaunchAgent, so only macOS offers it (T-31).
pub const LOGIN_AGENT: bool = false;

/// The staged runtime inside the installed resources.
pub fn bundled_stage(resources: &std::path::Path) -> anyhow::Result<std::path::PathBuf> {
    Ok(resources.join("desktop-runtime"))
}

/// The sponsor action is not offered on Windows yet.
pub fn sponsor_opener() -> Option<Command> {
    None
}

/// Only macOS reopens an app whose windows are all closed.
pub fn reopen_requested(_event: &tauri::RunEvent) -> bool {
    false
}

/// Resolves on Ctrl+Break or console close, or never when neither handler
/// can be installed. Ctrl+C is handled by the caller on every platform.
pub async fn terminate() {
    use tokio::signal::windows::{ctrl_break, ctrl_close};
    let (mut brk, mut close) = (ctrl_break().ok(), ctrl_close().ok());
    tokio::select! {
        Some(()) = async { brk.as_mut()?.recv().await } => {},
        Some(()) = async { close.as_mut()?.recv().await } => {},
        else => std::future::pending().await,
    }
}

/// The pipe of one data directory. Pipe names are global, so the name carries
/// the directory's volume serial number and file number.
fn pipe_name(identity: Identity) -> String {
    format!(
        r"\\.\pipe\printpartner-focus-{}-{}",
        identity.dev, identity.ino
    )
}

/// One pipe instance that only this user can open and remote clients cannot
/// reach. The first instance fails if the name already exists.
fn instance(
    name: &str,
    first: bool,
    descriptor: &PrivateDescriptor,
) -> std::io::Result<NamedPipeServer> {
    let mut attributes = SECURITY_ATTRIBUTES {
        nLength: size_of::<SECURITY_ATTRIBUTES>() as u32,
        lpSecurityDescriptor: descriptor.as_ptr(),
        bInheritHandle: 0,
    };
    // SAFETY: the attributes and the descriptor they point to outlive the call.
    unsafe {
        ServerOptions::new()
            .first_pipe_instance(first)
            .reject_remote_clients(true)
            .create_with_security_attributes_raw(
                name,
                (&mut attributes as *mut SECURITY_ATTRIBUTES).cast(),
            )
    }
}

/// Listens on the data directory's private pipe. A name another process
/// already holds is left alone and focus stays off.
pub(crate) struct FocusListener {
    task: tokio::task::JoinHandle<()>,
}

impl FocusListener {
    pub(crate) fn start(_data: &Path, identity: Identity, focus: Focus) -> Result<Self> {
        let name = pipe_name(identity);
        let descriptor = CurrentUser::query()?.private_descriptor(false)?;
        let mut listening = instance(&name, true, &descriptor).context("Focus pipe unavailable")?;
        let task = tokio::spawn(async move {
            loop {
                let connected = listening.connect().await;
                let Ok(next) = instance(&name, false, &descriptor) else {
                    break;
                };
                let served = std::mem::replace(&mut listening, next);
                if connected.is_err() {
                    continue;
                }
                let focus = focus.clone();
                tokio::spawn(async move {
                    let _ = tokio::time::timeout(TIMEOUT, answer(served, identity, &*focus)).await;
                });
            }
        });
        Ok(Self { task })
    }
}

impl Drop for FocusListener {
    fn drop(&mut self) {
        self.task.abort();
    }
}

/// Send `line` to the focus pipe of `data` and return the reply, or None when
/// the pipe is not private to this user and so not a desktop's.
pub(crate) fn exchange(data: &Path, line: &str) -> Result<Option<String>> {
    let pipe = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(pipe_name(Identity::of(data)?))?;
    if !CurrentUser::query()?.owns_privately(&pipe, SE_FILE_OBJECT)? {
        return Ok(None);
    }
    let line = line.to_owned();
    let (sender, receiver) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let reply = (|| {
            (&pipe).write_all(line.as_bytes())?;
            let mut reply = String::new();
            BufReader::new((&pipe).take(LIMIT)).read_line(&mut reply)?;
            Ok::<_, std::io::Error>(reply)
        })();
        let _ = sender.send(reply);
    });
    Ok(Some(
        receiver
            .recv_timeout(TIMEOUT)
            .context("Focus reply timed out")??,
    ))
}

/// Windows counterparts of the Unix socket tests in `unix::tests`: the pipe
/// is private where the socket is mode 0600, and a planted pipe is refused
/// where a planted file is.
#[cfg(test)]
mod tests {
    use super::*;
    use crate::focus::{
        FocusServer,
        tests::{ask, counter, data_dir},
    };
    use std::sync::atomic::Ordering;

    #[tokio::test]
    async fn ticket_t_30_pipe_is_private_to_this_user() {
        let data = data_dir();
        let (_, focus) = counter();
        let _server = FocusServer::start(data.path(), focus).unwrap();
        let name = pipe_name(Identity::of(data.path()).unwrap());
        let pipe = tokio::task::spawn_blocking(move || {
            std::fs::OpenOptions::new()
                .read(true)
                .write(true)
                .open(name)
                .unwrap()
        })
        .await
        .unwrap();
        assert!(
            CurrentUser::query()
                .unwrap()
                .owns_privately(&pipe, SE_FILE_OBJECT)
                .unwrap()
        );
    }

    #[tokio::test]
    async fn ticket_t_30_planted_pipe_is_kept_and_never_contacted() {
        let data = data_dir();
        let name = pipe_name(Identity::of(data.path()).unwrap());
        let _planted = ServerOptions::new()
            .first_pipe_instance(true)
            .create(&name)
            .unwrap();
        let (count, focus) = counter();
        assert!(FocusServer::start(data.path(), focus).is_err());
        assert!(
            !ask(data.path()).await.unwrap(),
            "a pipe with the default DACL is never asked"
        );
        assert_eq!(count.load(Ordering::SeqCst), 0);
    }
}
