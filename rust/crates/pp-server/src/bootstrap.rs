//! `pp-server bootstrap-url`: the running server hands a fresh single-use
//! sign-in URL to the data directory's owner over a Unix socket in that
//! directory. The socket has mode 0600, and the server also checks the peer's
//! uid, so another account, root included, gets nothing. The URL goes only to
//! the requesting terminal: the server never prints or logs it.

use anyhow::{Context, Result, bail, ensure};
use std::{
    io::{Read, Write},
    os::unix::fs::{FileTypeExt, MetadataExt, PermissionsExt},
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

/// The socket's name inside the data directory.
pub(crate) const SOCKET: &str = ".pp-server-bootstrap.sock";
pub(crate) const USAGE: &str = "Usage: pp-server bootstrap-url --data <dir>

Asks the pp-server running on <dir> for a fresh sign-in URL and prints it.
The URL works once, within 60 seconds. Run it as the account that owns <dir>,
for example: docker exec <container> pp-server bootstrap-url --data /data";

const REQUEST: &[u8] = b"bootstrap-url\n";
const URL_PREFIX: &str = "url ";
const REFUSED: &[u8] = b"refused\n";
const UNAVAILABLE: &[u8] = b"unavailable\n";
const TIMEOUT: Duration = Duration::from_secs(5);

fn effective_uid() -> u32 {
    // SAFETY: geteuid has no preconditions and touches no memory.
    unsafe { libc::geteuid() }
}

/// Mints one URL; fails once the runtime is stopping.
pub(crate) type Mint = Arc<dyn Fn() -> Result<String> + Send + Sync>;

/// The listening socket. [`Listener::close`] stops answering and removes it.
pub(crate) struct Listener {
    path: PathBuf,
    task: tokio::task::JoinHandle<()>,
}

impl Listener {
    pub(crate) fn close(self) {
        self.task.abort();
        let _ = std::fs::remove_file(&self.path);
    }
}

/// Listen on `<data_dir>/.pp-server-bootstrap.sock`. The caller holds the data
/// directory's storage lease, so a socket already at that name is a leftover
/// of a stopped server and is replaced; any other entry there is left alone
/// and the listener is refused.
pub(crate) fn listen(data_dir: &Path, mint: Mint) -> Result<Listener> {
    listen_for(data_dir, mint, effective_uid())
}

fn listen_for(data_dir: &Path, mint: Mint, owner: u32) -> Result<Listener> {
    let path = data_dir.join(SOCKET);
    match std::fs::symlink_metadata(&path) {
        Ok(metadata) if metadata.file_type().is_socket() && metadata.uid() == owner => {
            std::fs::remove_file(&path).context("Remove the stopped server's sign-in socket")?;
        }
        Ok(_) => bail!("{} exists and is not this account's socket", path.display()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error).context("Inspect the sign-in socket path"),
    }
    let listener = tokio::net::UnixListener::bind(&path).context("Bind the sign-in socket")?;
    // A socket without group or other write permission is already closed to
    // other accounts while this narrows it.
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))
        .context("Restrict the sign-in socket")?;
    let task = tokio::spawn(async move {
        while let Ok((stream, _)) = listener.accept().await {
            let mint = mint.clone();
            tokio::spawn(async move {
                let _ = tokio::time::timeout(TIMEOUT, answer(stream, owner, mint)).await;
            });
        }
    });
    Ok(Listener { path, task })
}

async fn answer(mut stream: tokio::net::UnixStream, owner: u32, mint: Mint) -> std::io::Result<()> {
    // Read the request before answering, so a refusal is not lost to a reset
    // from unread bytes.
    let mut request = Vec::with_capacity(REQUEST.len());
    (&mut stream)
        .take(REQUEST.len() as u64)
        .read_to_end(&mut request)
        .await?;
    if stream.peer_cred()?.uid() != owner || request != REQUEST {
        return stream.write_all(REFUSED).await;
    }
    match mint() {
        Ok(url) => {
            stream
                .write_all(format!("{URL_PREFIX}{url}\n").as_bytes())
                .await
        }
        Err(_) => stream.write_all(UNAVAILABLE).await,
    }
}

pub(crate) enum Command {
    Help,
    Request { data: PathBuf },
}

pub(crate) fn parse(mut args: impl Iterator<Item = String>) -> Result<Command> {
    let mut data = None;
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--help" | "-h" => return Ok(Command::Help),
            "--data" => data = Some(PathBuf::from(args.next().context("--data needs a value")?)),
            _ => bail!("Unknown argument {arg:?}\n\n{USAGE}"),
        }
    }
    Ok(Command::Request {
        data: data.with_context(|| format!("--data is required\n\n{USAGE}"))?,
    })
}

/// Ask the server running on `data` for a URL. Checks ownership before it
/// connects and writes nothing to the directory.
pub(crate) fn request(data: &Path) -> Result<String> {
    request_as(data, effective_uid())
}

fn request_as(data: &Path, uid: u32) -> Result<String> {
    let not_running = || format!("No pp-server is running on {}", data.display());
    let metadata = std::fs::metadata(data)
        .with_context(|| format!("Cannot read the data directory {}", data.display()))?;
    ensure!(metadata.is_dir(), "{} is not a directory", data.display());
    ensure!(
        metadata.uid() == uid,
        "{} belongs to another account; run this as the account that runs pp-server",
        data.display()
    );
    let path = data.join(SOCKET);
    let socket = match std::fs::symlink_metadata(&path) {
        Ok(socket) => socket,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => bail!(not_running()),
        Err(error) => return Err(error).context("Cannot read the sign-in socket"),
    };
    ensure!(
        socket.file_type().is_socket() && socket.uid() == uid && socket.mode() & 0o077 == 0,
        "{} is not an owner-only pp-server socket; refusing to use it",
        path.display()
    );
    let mut stream = match std::os::unix::net::UnixStream::connect(&path) {
        Ok(stream) => stream,
        Err(error) if error.kind() == std::io::ErrorKind::ConnectionRefused => bail!(not_running()),
        Err(error) => return Err(error).context("Cannot reach pp-server"),
    };
    stream.set_read_timeout(Some(TIMEOUT))?;
    stream.set_write_timeout(Some(TIMEOUT))?;
    stream.write_all(REQUEST)?;
    let mut answer = String::new();
    stream
        .take(4096)
        .read_to_string(&mut answer)
        .context("pp-server did not answer")?;
    if let Some(url) = answer
        .strip_prefix(URL_PREFIX)
        .and_then(|rest| rest.strip_suffix('\n'))
    {
        return Ok(url.to_owned());
    }
    match answer.as_bytes() {
        REFUSED => bail!("pp-server refused this account"),
        UNAVAILABLE => bail!("pp-server is stopping; start it again and retry"),
        _ => bail!("pp-server sent an unexpected answer"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::DirBuilderExt;

    fn directory() -> tempfile::TempDir {
        let directory = tempfile::Builder::new()
            .prefix("pp-bootstrap-")
            .tempdir()
            .unwrap();
        std::fs::set_permissions(directory.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        directory
    }

    fn counting_mint() -> (Mint, Arc<std::sync::atomic::AtomicUsize>) {
        let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let seen = calls.clone();
        let mint: Mint = Arc::new(move || {
            let call = seen.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            Ok(format!(
                "http://127.0.0.1:1/__desktop/bootstrap?token=t{call}"
            ))
        });
        (mint, calls)
    }

    async fn request_off_thread(data: &Path, uid: u32) -> Result<String> {
        let data = data.to_owned();
        tokio::task::spawn_blocking(move || request_as(&data, uid))
            .await
            .unwrap()
    }

    #[tokio::test]
    async fn the_owner_gets_a_fresh_url_per_request_from_an_owner_only_socket() {
        let directory = directory();
        let (mint, calls) = counting_mint();
        let listener = listen(directory.path(), mint).unwrap();
        let socket = std::fs::symlink_metadata(directory.path().join(SOCKET)).unwrap();
        assert_eq!(socket.mode() & 0o777, 0o600);
        let uid = effective_uid();
        let first = request_off_thread(directory.path(), uid).await.unwrap();
        let second = request_off_thread(directory.path(), uid).await.unwrap();
        assert_eq!(first, "http://127.0.0.1:1/__desktop/bootstrap?token=t0");
        assert_eq!(second, "http://127.0.0.1:1/__desktop/bootstrap?token=t1");
        assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 2);
        listener.close();
        assert!(!directory.path().join(SOCKET).exists());
    }

    #[tokio::test]
    async fn a_peer_with_another_uid_is_refused_and_nothing_is_minted() {
        let directory = directory();
        let (mint, calls) = counting_mint();
        // The server's owner differs from the connecting process: the peer
        // check, not the file mode, refuses it.
        let listener = listen_for(directory.path(), mint, effective_uid() + 1).unwrap();
        let error = request_off_thread(directory.path(), effective_uid())
            .await
            .unwrap_err();
        assert_eq!(error.to_string(), "pp-server refused this account");
        assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 0);
        listener.close();
    }

    #[tokio::test]
    async fn a_malformed_request_is_refused() {
        let directory = directory();
        let (mint, calls) = counting_mint();
        let listener = listen(directory.path(), mint).unwrap();
        let path = directory.path().join(SOCKET);
        let answer = tokio::task::spawn_blocking(move || {
            let mut stream = std::os::unix::net::UnixStream::connect(path).unwrap();
            stream.write_all(b"bootstrap-urX\n").unwrap();
            let mut answer = String::new();
            stream.read_to_string(&mut answer).unwrap();
            answer
        })
        .await
        .unwrap();
        assert_eq!(answer, "refused\n");
        assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 0);
        listener.close();
    }

    #[tokio::test]
    async fn a_stopping_runtime_answers_unavailable() {
        let directory = directory();
        let mint: Mint = Arc::new(|| bail!("Runtime is stopping"));
        let listener = listen(directory.path(), mint).unwrap();
        let error = request_off_thread(directory.path(), effective_uid())
            .await
            .unwrap_err();
        assert_eq!(
            error.to_string(),
            "pp-server is stopping; start it again and retry"
        );
        listener.close();
    }

    #[tokio::test]
    async fn a_leftover_socket_is_replaced_but_another_entry_is_kept() {
        let directory = directory();
        let path = directory.path().join(SOCKET);
        drop(std::os::unix::net::UnixListener::bind(&path).unwrap());
        let (mint, _) = counting_mint();
        listen(directory.path(), mint.clone()).unwrap().close();
        std::fs::DirBuilder::new()
            .mode(0o700)
            .create(&path)
            .unwrap();
        let error = listen(directory.path(), mint).err().unwrap();
        assert!(error.to_string().contains("is not this account's socket"));
        assert!(path.is_dir());
    }

    #[test]
    fn with_no_server_the_request_fails_and_writes_nothing() {
        let directory = directory();
        std::fs::write(directory.path().join("print-partner.db"), b"").unwrap();
        let error = request_as(directory.path(), effective_uid()).unwrap_err();
        assert_eq!(
            error.to_string(),
            format!("No pp-server is running on {}", directory.path().display())
        );
        // A socket left by a stopped server refuses connections the same way.
        let path = directory.path().join(SOCKET);
        drop(std::os::unix::net::UnixListener::bind(&path).unwrap());
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
        let error = request_as(directory.path(), effective_uid()).unwrap_err();
        assert!(error.to_string().starts_with("No pp-server is running"));
        let mut names: Vec<_> = std::fs::read_dir(directory.path())
            .unwrap()
            .map(|entry| entry.unwrap().file_name())
            .collect();
        names.sort();
        assert_eq!(names, [SOCKET, "print-partner.db"]);
    }

    #[test]
    fn another_accounts_directory_is_refused_before_connecting() {
        let directory = directory();
        let error = request_as(directory.path(), effective_uid() + 1).unwrap_err();
        assert!(
            error.to_string().ends_with(
                "belongs to another account; run this as the account that runs pp-server"
            ),
            "{error}"
        );
    }

    #[tokio::test]
    async fn a_socket_open_to_others_is_not_used() {
        let directory = directory();
        let (mint, calls) = counting_mint();
        let listener = listen(directory.path(), mint).unwrap();
        let path = directory.path().join(SOCKET);
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o660)).unwrap();
        let error = request_off_thread(directory.path(), effective_uid())
            .await
            .unwrap_err();
        assert!(
            error
                .to_string()
                .contains("is not an owner-only pp-server socket")
        );
        assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 0);
        listener.close();
    }

    #[test]
    fn parse_requires_data() {
        assert!(matches!(
            parse(["--data".to_owned(), "/data".to_owned()].into_iter()).unwrap(),
            Command::Request { data } if data == Path::new("/data")
        ));
        assert!(matches!(
            parse(["--help".to_owned()].into_iter()).unwrap(),
            Command::Help
        ));
        assert!(parse(std::iter::empty()).is_err());
        assert!(parse(["--port".to_owned(), "1".to_owned()].into_iter()).is_err());
    }
}
