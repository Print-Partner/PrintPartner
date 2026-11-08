//! Windows: rfd dialogs, the URL protocol handler, Ctrl+Break and console
//! close, and a private named pipe for focus.

use crate::focus::{Focus, Identity, LIMIT, TIMEOUT, answer};
pub use crate::rfd_dialogs::{message, notice, startup_error};
use anyhow::{Context, Result};
use pp_os::windows_security::{CurrentUser, PrivateDescriptor, SE_FILE_OBJECT};
use std::{
    fs::File,
    io::{BufRead, BufReader, Read, Write},
    os::windows::fs::OpenOptionsExt,
    path::Path,
    process::Command,
};
use tokio::{
    net::windows::named_pipe::{NamedPipeServer, ServerOptions},
    sync::oneshot,
    task::{JoinHandle, JoinSet},
};
use windows_sys::Win32::{
    Foundation::ERROR_PIPE_BUSY,
    Security::SECURITY_ATTRIBUTES,
    Storage::FileSystem::{SECURITY_IDENTIFICATION, SECURITY_SQOS_PRESENT},
    System::Pipes::WaitNamedPipeW,
};

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
    stop: Option<oneshot::Sender<()>>,
    task: JoinHandle<()>,
}

impl FocusListener {
    pub(crate) fn start(_data: &Path, identity: Identity, focus: Focus) -> Result<Self> {
        let name = pipe_name(identity);
        let descriptor = CurrentUser::query()?.private_descriptor(false)?;
        let mut listening = instance(&name, true, &descriptor).context("Focus pipe unavailable")?;
        let (stop, mut stopped) = oneshot::channel();
        let task = tokio::spawn(async move {
            let mut connections = JoinSet::new();
            loop {
                let connected = tokio::select! {
                    _ = &mut stopped => break,
                    Some(_) = connections.join_next() => continue,
                    connected = listening.connect() => connected,
                };
                let Ok(next) = instance(&name, false, &descriptor) else {
                    break;
                };
                let served = std::mem::replace(&mut listening, next);
                if connected.is_err() {
                    continue;
                }
                let focus = focus.clone();
                connections.spawn(async move {
                    let _ = tokio::time::timeout(TIMEOUT, answer(served, identity, &*focus)).await;
                });
            }
            connections.shutdown().await;
        });
        Ok(Self {
            stop: Some(stop),
            task,
        })
    }

    /// Close every instance of the pipe, the listening one and those still
    /// answering, so the next owner of the data directory can create it.
    pub(crate) async fn stop(mut self) {
        if let Some(stop) = self.stop.take() {
            let _ = stop.send(());
        }
        let _ = tokio::time::timeout(TIMEOUT, &mut self.task).await;
    }
}

impl Drop for FocusListener {
    fn drop(&mut self) {
        self.task.abort();
    }
}

/// Open the pipe `name` as a client. The pipe's owner may identify the
/// client but never impersonate it: whoever created the name gets the
/// connection before `owns_privately` can check them. A pipe busy with another
/// launch gets one bounded wait and one retry.
fn open_client(name: &str) -> std::io::Result<File> {
    let open = || {
        std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .security_qos_flags(SECURITY_SQOS_PRESENT | SECURITY_IDENTIFICATION)
            .open(name)
    };
    match open() {
        Err(error) if error.raw_os_error() == Some(ERROR_PIPE_BUSY as i32) => {
            let wide: Vec<u16> = name.encode_utf16().chain([0]).collect();
            // SAFETY: the name is NUL-terminated and outlives the call.
            unsafe { WaitNamedPipeW(wide.as_ptr(), BUSY_WAIT_MS) };
            open()
        }
        result => result,
    }
}

/// How long a client waits for a busy focus pipe before its one retry.
const BUSY_WAIT_MS: u32 = 500;

/// Send `line` to the focus pipe of `data` and return the reply, or None when
/// the pipe is not private to this user and so not a desktop's.
pub(crate) fn exchange(data: &Path, line: &str) -> Result<Option<String>> {
    let pipe = open_client(&pipe_name(Identity::of(data)?))?;
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

    /// The level the pipe's owner gets when it impersonates the client that
    /// connected to the server. Impersonation needs data read from the client first.
    fn impersonation_level(server: &NamedPipeServer) -> i32 {
        use std::os::windows::io::AsRawHandle;
        use windows_sys::Win32::{
            Foundation::{CloseHandle, HANDLE},
            Security::{GetTokenInformation, RevertToSelf, TOKEN_QUERY, TokenImpersonationLevel},
            System::{
                Pipes::ImpersonateNamedPipeClient,
                Threading::{GetCurrentThread, OpenThreadToken},
            },
        };
        let mut level = 0i32;
        // SAFETY: every handle is valid for the call that uses it, the thread
        // reverts before returning, and the level has the size passed.
        unsafe {
            assert_ne!(
                ImpersonateNamedPipeClient(server.as_raw_handle() as HANDLE),
                0
            );
            let mut token: HANDLE = std::ptr::null_mut();
            let opened = OpenThreadToken(GetCurrentThread(), TOKEN_QUERY, 1, &mut token);
            assert_ne!(RevertToSelf(), 0);
            assert_ne!(opened, 0);
            let mut length = 0;
            let read = GetTokenInformation(
                token,
                TokenImpersonationLevel,
                (&mut level as *mut i32).cast(),
                size_of::<i32>() as u32,
                &mut length,
            );
            CloseHandle(token);
            assert_ne!(read, 0);
        }
        level
    }

    #[tokio::test]
    async fn ticket_t_30_pipe_owner_can_identify_but_not_impersonate_the_client() {
        use tokio::io::AsyncReadExt;
        use windows_sys::Win32::Security::SecurityIdentification;
        let name = format!(r"\\.\pipe\printpartner-focus-qos-{}", std::process::id());
        let mut server = ServerOptions::new()
            .first_pipe_instance(true)
            .create(&name)
            .unwrap();
        let client = tokio::task::spawn_blocking(move || {
            let mut client = open_client(&name).unwrap();
            client.write_all(b"x").unwrap();
            client
        });
        server.connect().await.unwrap();
        server.read_exact(&mut [0; 1]).await.unwrap();
        let _client = client.await.unwrap();
        assert_eq!(impersonation_level(&server), SecurityIdentification);
    }

    #[tokio::test]
    async fn ticket_t_30_stop_closes_every_instance_for_the_next_owner() {
        let data = data_dir();
        let (_, focus) = counter();
        let server = FocusServer::start(data.path(), focus).unwrap();
        let name = pipe_name(Identity::of(data.path()).unwrap());
        // The second client can only open the pipe once the listener has
        // created the next instance, which it does in the same step that
        // hands the first connection to an answering task. So both clients
        // being connected means stop() must close an unanswered connection.
        let silent = tokio::task::spawn_blocking(move || {
            let first = open_client(&name).unwrap();
            let deadline = std::time::Instant::now() + TIMEOUT;
            loop {
                match open_client(&name) {
                    Ok(second) => return (first, second),
                    Err(_) if std::time::Instant::now() < deadline => {
                        std::thread::sleep(std::time::Duration::from_millis(10));
                    }
                    Err(error) => panic!("listener never accepted the first client: {error}"),
                }
            }
        })
        .await
        .unwrap();
        server.stop().await;
        let (_, focus) = counter();
        assert!(
            FocusServer::start(data.path(), focus).is_ok(),
            "an unanswered connection kept the pipe name"
        );
        drop(silent);
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
