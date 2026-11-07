//! What Linux and macOS share: the focus socket and SIGTERM.

use crate::focus::{Focus, Identity, LIMIT, TIMEOUT, answer};
use anyhow::{Context, Result, ensure};
use std::{
    io::{BufRead, BufReader, Write},
    os::unix::fs::{FileTypeExt, MetadataExt, PermissionsExt},
    path::{Path, PathBuf},
};

/// The focus socket inside the 0700 data directory.
pub const SOCKET: &str = ".desktop-focus.sock";

/// Listens on `<data>/.desktop-focus.sock`, mode 0600, and answers only a peer
/// with the data directory owner's uid.
pub(crate) struct FocusListener {
    path: PathBuf,
    task: tokio::task::JoinHandle<()>,
}

impl FocusListener {
    /// A leftover socket from a crashed owner is replaced; anything else at
    /// the path is left alone and focus stays off.
    pub(crate) fn start(data: &Path, identity: Identity, focus: Focus) -> Result<Self> {
        let uid = std::fs::metadata(data)?.uid();
        let path = data.join(SOCKET);
        match std::fs::symlink_metadata(&path) {
            Ok(existing) if existing.file_type().is_socket() => {
                std::fs::remove_file(&path).context("Focus socket cleanup failed")?
            }
            Ok(_) => anyhow::bail!("Focus socket path occupied"),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error).context("Focus socket path unavailable"),
        }
        let listener =
            std::os::unix::net::UnixListener::bind(&path).context("Focus socket bind failed")?;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))?;
        listener.set_nonblocking(true)?;
        let listener = tokio::net::UnixListener::from_std(listener)?;
        let task = tokio::spawn(async move {
            while let Ok((stream, _)) = listener.accept().await {
                let focus = focus.clone();
                tokio::spawn(async move {
                    let _ = tokio::time::timeout(TIMEOUT, async {
                        ensure!(stream.peer_cred()?.uid() == uid, "Focus peer uid mismatch");
                        answer(stream, identity, &*focus).await
                    })
                    .await;
                });
            }
        });
        Ok(Self { path, task })
    }

    /// Dropping closes the socket at once, so there is nothing to wait for.
    pub(crate) async fn stop(self) {}
}

impl Drop for FocusListener {
    fn drop(&mut self) {
        self.task.abort();
        if std::fs::symlink_metadata(&self.path)
            .is_ok_and(|metadata| metadata.file_type().is_socket())
        {
            let _ = std::fs::remove_file(&self.path);
        }
    }
}

/// Send `line` to the focus socket of `data` and return the reply, or None
/// when something other than a socket sits at its path.
pub(crate) fn exchange(data: &Path, line: &str) -> Result<Option<String>> {
    let path = data.join(SOCKET);
    if !std::fs::symlink_metadata(&path)?.file_type().is_socket() {
        return Ok(None);
    }
    let mut stream = std::os::unix::net::UnixStream::connect(&path)?;
    stream.set_read_timeout(Some(TIMEOUT))?;
    stream.set_write_timeout(Some(TIMEOUT))?;
    stream.write_all(line.as_bytes())?;
    let mut reply = String::new();
    BufReader::new(std::io::Read::take(&stream, LIMIT)).read_line(&mut reply)?;
    Ok(Some(reply))
}

/// Resolves on SIGTERM, or never when the handler cannot be installed.
pub async fn terminate() {
    use tokio::signal::unix::{SignalKind, signal};
    match signal(SignalKind::terminate()) {
        Ok(mut terminate) => {
            terminate.recv().await;
        }
        Err(_) => std::future::pending().await,
    }
}

/// Windows counterparts: `windows::tests`, which check the pipe's private
/// DACL and refuse a pipe planted under the focus name.
#[cfg(test)]
mod tests {
    use super::*;
    use crate::focus::{
        FocusServer,
        tests::{ask, counter, data_dir},
    };
    use std::sync::atomic::Ordering;

    #[tokio::test]
    async fn ticket_t_30_socket_is_private_and_removed_with_its_owner() {
        let data = data_dir();
        let (_, focus) = counter();
        let server = FocusServer::start(data.path(), focus).unwrap();
        let socket = data.path().join(SOCKET);
        assert_eq!(
            std::fs::symlink_metadata(&socket).unwrap().mode() & 0o777,
            0o600
        );
        server.stop().await;
        assert!(!socket.exists(), "socket removed with its owner");
        assert!(ask(data.path()).await.is_err(), "no owner, no focus");
    }

    #[tokio::test]
    async fn ticket_t_30_stale_socket_is_replaced_and_planted_file_is_kept() {
        let data = data_dir();
        let socket = data.path().join(SOCKET);
        drop(std::os::unix::net::UnixListener::bind(&socket).unwrap());
        assert!(
            std::fs::symlink_metadata(&socket)
                .unwrap()
                .file_type()
                .is_socket()
        );
        let (count, focus) = counter();
        let server = FocusServer::start(data.path(), focus).unwrap();
        assert!(ask(data.path()).await.unwrap());
        assert_eq!(count.load(Ordering::SeqCst), 1);
        drop(server);

        std::fs::write(&socket, b"not a socket").unwrap();
        let (_, focus) = counter();
        assert!(FocusServer::start(data.path(), focus).is_err());
        assert_eq!(std::fs::read(&socket).unwrap(), b"not a socket");
        assert!(
            !ask(data.path()).await.unwrap(),
            "a planted file is never contacted"
        );
    }
}
