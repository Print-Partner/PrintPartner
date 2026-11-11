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

/// The whole request: the client sends exactly this, then shuts down its
/// write half. Anything else, longer, shorter or empty, is refused.
const REQUEST: &[u8] = b"bootstrap-url\n";
/// The server reads at most this much, so it can tell an over-long request
/// from the exact one without buffering what a client sends.
const MAX_REQUEST: u64 = 64;
const URL_PREFIX: &str = "url ";
const REFUSED: &[u8] = b"refused\n";
const UNAVAILABLE: &[u8] = b"unavailable\n";
const FAILED: &[u8] = b"failed\n";
const TIMEOUT: Duration = Duration::from_secs(5);
const ACCEPT_RETRY: Duration = Duration::from_millis(100);

fn effective_uid() -> u32 {
    // SAFETY: geteuid has no preconditions and touches no memory.
    unsafe { libc::geteuid() }
}

/// What one mint produced.
pub(crate) enum Minted {
    Url(String),
    /// The runtime is draining and mints nothing more.
    Stopping,
}

/// Mints one URL. An error is a failure other than the drain.
pub(crate) type Mint = Arc<dyn Fn() -> Result<Minted> + Send + Sync>;

/// The listening socket. Dropping it stops answering and removes the socket,
/// on every exit path including an early return or a panic.
pub(crate) struct Listener {
    path: PathBuf,
    owner: u32,
    task: tokio::task::JoinHandle<()>,
}

impl Drop for Listener {
    fn drop(&mut self) {
        self.task.abort();
        let _ = remove_own_socket(&self.path, self.owner);
    }
}

/// Unlink `path` when it is a socket `owner` owns. Nothing there is fine; any
/// other entry is left alone and refused.
fn remove_own_socket(path: &Path, owner: u32) -> Result<()> {
    match std::fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_socket() && metadata.uid() == owner => {
            std::fs::remove_file(path).context("Remove the sign-in socket")
        }
        Ok(_) => bail!("{} exists and is not this account's socket", path.display()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error).context("Inspect the sign-in socket path"),
    }
}

/// Start the listener for a Rust-only runtime. The Node-backed runtime keeps
/// its data directory as it was, so it gets none. Without the listener the
/// server still serves; only headless sign-in is unavailable.
pub(crate) fn start(runtime: &pp_core::CoreRuntime, data_dir: &Path) -> Option<Listener> {
    if !runtime.rust_only() {
        return None;
    }
    let handle = runtime.handle();
    let mint: Mint = Arc::new(move || match handle.browser_launch_target() {
        Ok(target) => Ok(Minted::Url(target.into_url())),
        // pp-gateway's BootstrapMinter refuses with this once the drain begins.
        Err(error)
            if error
                .chain()
                .any(|cause| cause.to_string() == "Runtime is stopping") =>
        {
            Ok(Minted::Stopping)
        }
        Err(error) => Err(error),
    });
    listen(data_dir, mint)
        .inspect_err(|error| eprintln!("pp-server bootstrap-url is unavailable: {error:#}"))
        .ok()
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
    remove_own_socket(&path, owner)?;
    let listener = tokio::net::UnixListener::bind(&path).context("Bind the sign-in socket")?;
    // A socket without group or other write permission is already closed to
    // other accounts while this narrows it.
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))
        .context("Restrict the sign-in socket")?;
    let task = tokio::spawn(async move {
        loop {
            match listener.accept().await {
                Ok((stream, _)) => {
                    let mint = mint.clone();
                    tokio::spawn(async move {
                        let _ = tokio::time::timeout(TIMEOUT, answer(stream, owner, mint)).await;
                    });
                }
                // A failed accept, such as running out of descriptors, is
                // transient: keep the socket and try again shortly.
                Err(_) => tokio::time::sleep(ACCEPT_RETRY).await,
            }
        }
    });
    Ok(Listener { path, owner, task })
}

async fn answer(mut stream: tokio::net::UnixStream, owner: u32, mint: Mint) -> std::io::Result<()> {
    // Read to the end of the request, which the client marks by shutting
    // down its write half, before answering. A client that never ends it is
    // cut off by the caller's timeout and gets no answer.
    let mut request = Vec::with_capacity(REQUEST.len());
    (&mut stream)
        .take(MAX_REQUEST)
        .read_to_end(&mut request)
        .await?;
    if stream.peer_cred()?.uid() != owner || request != REQUEST {
        return stream.write_all(REFUSED).await;
    }
    match mint() {
        Ok(Minted::Url(url)) => {
            stream
                .write_all(format!("{URL_PREFIX}{url}\n").as_bytes())
                .await
        }
        Ok(Minted::Stopping) => stream.write_all(UNAVAILABLE).await,
        Err(error) => {
            // A failed mint produced no URL, so its error holds none.
            eprintln!("pp-server bootstrap-url: minting a sign-in URL failed: {error:#}");
            stream.write_all(FAILED).await
        }
    }
}

/// The process on the other end must run as `uid`, as pp-server does.
fn check_peer(stream: &std::os::unix::net::UnixStream, uid: u32) -> Result<()> {
    ensure!(
        peer_uid(stream).context("Cannot identify the process on the socket")? == uid,
        "The socket belongs to a process of another account; refusing to use it"
    );
    Ok(())
}

#[cfg(any(target_os = "linux", target_os = "android"))]
fn peer_uid(stream: &std::os::unix::net::UnixStream) -> std::io::Result<u32> {
    use std::os::fd::AsRawFd;
    let mut credentials = libc::ucred {
        pid: 0,
        uid: 0,
        gid: 0,
    };
    let mut length = std::mem::size_of::<libc::ucred>() as libc::socklen_t;
    // SAFETY: `credentials` and `length` are valid for writes of the sizes
    // passed, and the descriptor stays open for the call.
    let result = unsafe {
        libc::getsockopt(
            stream.as_raw_fd(),
            libc::SOL_SOCKET,
            libc::SO_PEERCRED,
            (&raw mut credentials).cast(),
            &raw mut length,
        )
    };
    if result != 0 {
        return Err(std::io::Error::last_os_error());
    }
    Ok(credentials.uid)
}

#[cfg(not(any(target_os = "linux", target_os = "android")))]
fn peer_uid(stream: &std::os::unix::net::UnixStream) -> std::io::Result<u32> {
    use std::os::fd::AsRawFd;
    let (mut uid, mut gid) = (0, 0);
    // SAFETY: `uid` and `gid` are valid for writes, and the descriptor stays
    // open for the call.
    if unsafe { libc::getpeereid(stream.as_raw_fd(), &raw mut uid, &raw mut gid) } != 0 {
        return Err(std::io::Error::last_os_error());
    }
    Ok(uid)
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
    // pp-server's storage lease refuses such a directory, so no server of
    // this account listens there.
    ensure!(
        metadata.mode() & 0o022 == 0,
        "{} is writable by other accounts; refusing to use it",
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
        // NotFound: the socket went away after the check above.
        Err(error)
            if matches!(
                error.kind(),
                std::io::ErrorKind::ConnectionRefused | std::io::ErrorKind::NotFound
            ) =>
        {
            bail!(not_running())
        }
        Err(error) => return Err(error).context("Cannot reach pp-server"),
    };
    check_peer(&stream, uid)?;
    stream.set_read_timeout(Some(TIMEOUT))?;
    stream.set_write_timeout(Some(TIMEOUT))?;
    stream.write_all(REQUEST)?;
    stream.shutdown(std::net::Shutdown::Write)?;
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
        FAILED => bail!("pp-server could not create a sign-in URL; its stderr says why"),
        _ => bail!("pp-server sent an unexpected answer"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::DirBuilderExt;

    fn directory() -> tempfile::TempDir {
        // Under /tmp: a socket path must fit in about 100 bytes, which macOS's
        // per-user temporary directory leaves no room for.
        let directory = tempfile::Builder::new()
            .prefix("pp-bootstrap-")
            .tempdir_in("/tmp")
            .unwrap();
        std::fs::set_permissions(directory.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        directory
    }

    fn counting_mint() -> (Mint, Arc<std::sync::atomic::AtomicUsize>) {
        let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let seen = calls.clone();
        let mint: Mint = Arc::new(move || {
            let call = seen.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            Ok(Minted::Url(format!(
                "http://127.0.0.1:1/__desktop/bootstrap?token=t{call}"
            )))
        });
        (mint, calls)
    }

    /// A directory with a listener whose mint counts its calls.
    fn serving() -> (
        tempfile::TempDir,
        Listener,
        Arc<std::sync::atomic::AtomicUsize>,
    ) {
        let directory = directory();
        let (mint, calls) = counting_mint();
        let listener = listen(directory.path(), mint).unwrap();
        (directory, listener, calls)
    }

    async fn request_off_thread(data: &Path, uid: u32) -> Result<String> {
        let data = data.to_owned();
        tokio::task::spawn_blocking(move || request_as(&data, uid))
            .await
            .unwrap()
    }

    #[tokio::test]
    async fn the_owner_gets_a_fresh_url_per_request_from_an_owner_only_socket() {
        let (directory, listener, calls) = serving();
        let socket = std::fs::symlink_metadata(directory.path().join(SOCKET)).unwrap();
        assert_eq!(socket.mode() & 0o777, 0o600);
        let uid = effective_uid();
        let first = request_off_thread(directory.path(), uid).await.unwrap();
        let second = request_off_thread(directory.path(), uid).await.unwrap();
        assert_eq!(first, "http://127.0.0.1:1/__desktop/bootstrap?token=t0");
        assert_eq!(second, "http://127.0.0.1:1/__desktop/bootstrap?token=t1");
        assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 2);
        drop(listener);
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
        drop(listener);
    }

    /// Send `request` on a raw connection, end it, and return the answer.
    async fn exchange(path: PathBuf, request: &'static [u8]) -> String {
        tokio::task::spawn_blocking(move || {
            let mut stream = std::os::unix::net::UnixStream::connect(path).unwrap();
            stream.write_all(request).unwrap();
            stream.shutdown(std::net::Shutdown::Write).unwrap();
            let mut answer = String::new();
            stream.read_to_string(&mut answer).unwrap();
            answer
        })
        .await
        .unwrap()
    }

    #[tokio::test]
    async fn only_the_exact_request_gets_a_url() {
        let (directory, listener, calls) = serving();
        let path = directory.path().join(SOCKET);
        for request in [
            &b"bootstrap-urX\n"[..],
            b"bootstrap-url\nANYTHING",
            b"bootstrap-url",
            b"",
        ] {
            let answer = exchange(path.clone(), request).await;
            assert_eq!(answer, "refused\n", "{request:?}");
        }
        assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 0);
        let answer = exchange(path, REQUEST).await;
        assert_eq!(
            answer,
            "url http://127.0.0.1:1/__desktop/bootstrap?token=t0\n"
        );
        drop(listener);
    }

    #[tokio::test]
    async fn a_silent_client_is_cut_off_and_others_are_still_answered() {
        let (directory, listener, _) = serving();
        let path = directory.path().join(SOCKET);
        let silent = tokio::task::spawn_blocking(move || {
            let mut stream = std::os::unix::net::UnixStream::connect(path).unwrap();
            let started = std::time::Instant::now();
            let mut answer = Vec::new();
            let _ = stream.read_to_end(&mut answer);
            (answer, started.elapsed())
        });
        // While that connection waits, another client gets its URL.
        let url = request_off_thread(directory.path(), effective_uid())
            .await
            .unwrap();
        assert!(url.ends_with("token=t0"), "{url}");
        let (answer, waited) = silent.await.unwrap();
        assert!(answer.is_empty(), "{answer:?}");
        assert!(waited >= TIMEOUT - Duration::from_millis(500), "{waited:?}");
        drop(listener);
    }

    #[tokio::test]
    async fn a_stopping_runtime_answers_unavailable() {
        let directory = directory();
        let mint: Mint = Arc::new(|| Ok(Minted::Stopping));
        let listener = listen(directory.path(), mint).unwrap();
        let error = request_off_thread(directory.path(), effective_uid())
            .await
            .unwrap_err();
        assert_eq!(
            error.to_string(),
            "pp-server is stopping; start it again and retry"
        );
        drop(listener);
    }

    #[tokio::test]
    async fn a_failed_mint_answers_failed() {
        let directory = directory();
        let mint: Mint = Arc::new(|| bail!("Session mutex poisoned"));
        let listener = listen(directory.path(), mint).unwrap();
        let error = request_off_thread(directory.path(), effective_uid())
            .await
            .unwrap_err();
        assert_eq!(
            error.to_string(),
            "pp-server could not create a sign-in URL; its stderr says why"
        );
        drop(listener);
    }

    #[tokio::test]
    async fn a_panic_unwinding_past_the_listener_removes_the_socket() {
        let directory = directory();
        let (mint, _) = counting_mint();
        let path = directory.path().join(SOCKET);
        let unwound = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _listener = listen(directory.path(), mint).unwrap();
            assert!(path.exists());
            panic!("serve() failed after the listener started");
        }));
        assert!(unwound.is_err());
        assert!(!path.exists());
    }

    #[tokio::test]
    async fn a_symlink_at_the_socket_name_is_left_alone() {
        let directory = directory();
        let path = directory.path().join(SOCKET);
        let other_socket = directory.path().join("other.sock");
        let _other = std::os::unix::net::UnixListener::bind(&other_socket).unwrap();
        let file = directory.path().join("file");
        std::fs::write(&file, b"kept").unwrap();
        let (mint, _) = counting_mint();
        for target in [&other_socket, &file] {
            std::os::unix::fs::symlink(target, &path).unwrap();
            let error = listen(directory.path(), mint.clone()).err().unwrap();
            assert!(error.to_string().contains("is not this account's socket"));
            assert_eq!(std::fs::read_link(&path).unwrap(), *target);
            std::fs::remove_file(&path).unwrap();
        }
        assert!(
            std::fs::symlink_metadata(&other_socket)
                .unwrap()
                .file_type()
                .is_socket()
        );
        assert_eq!(std::fs::read(&file).unwrap(), b"kept");
    }

    #[tokio::test]
    async fn a_leftover_socket_is_replaced_but_another_entry_is_kept() {
        let directory = directory();
        let path = directory.path().join(SOCKET);
        drop(std::os::unix::net::UnixListener::bind(&path).unwrap());
        let (mint, _) = counting_mint();
        drop(listen(directory.path(), mint.clone()).unwrap());
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
        let (directory, listener, calls) = serving();
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
        drop(listener);
    }

    #[test]
    fn a_peer_of_another_account_is_not_trusted() {
        let (ours, _theirs) = std::os::unix::net::UnixStream::pair().unwrap();
        check_peer(&ours, effective_uid()).unwrap();
        let error = check_peer(&ours, effective_uid() + 1).unwrap_err();
        assert!(error.to_string().contains("another account"), "{error}");
    }

    #[test]
    fn a_directory_others_can_write_is_refused() {
        let directory = directory();
        std::fs::set_permissions(directory.path(), std::fs::Permissions::from_mode(0o770)).unwrap();
        let error = request_as(directory.path(), effective_uid()).unwrap_err();
        assert!(
            error
                .to_string()
                .ends_with("is writable by other accounts; refusing to use it"),
            "{error}"
        );
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
