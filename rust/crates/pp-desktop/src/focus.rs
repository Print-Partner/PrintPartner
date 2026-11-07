//! T-30 single instance. The desktop that owns a data directory listens on a
//! private endpoint of that directory: a socket inside it on Unix, a named
//! pipe named after it on Windows (see the platform modules). A second desktop
//! refused by that directory's lock asks it to show its window and exits. The
//! listener exists only while its owner holds the lock: it starts after core
//! startup admits the directory and stops before core shutdown releases it.
//! The owner answers only a same-user peer naming the device and file number
//! of its own data directory, so it never focuses for another directory.
//! Focus is best effort: any failure keeps the existing startup error.

use crate::platform;
use anyhow::{Context, Result, ensure};
use std::{path::Path, sync::Arc, time::Duration};
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncWrite, AsyncWriteExt};

const REQUEST: &str = "focus v1";
const FOCUSED: &str = "focused";
const MISMATCH: &str = "mismatch";
pub(crate) const LIMIT: u64 = 128;
pub(crate) const TIMEOUT: Duration = Duration::from_secs(2);

/// Refusals from the desktop lock. The standalone-Node refusal is excluded:
/// that owner has no window to focus.
const OWNED: [&str; 2] = [
    "Data directory already owned",
    "Data directory already owned; stop the other Print Partner process before starting another instance",
];

pub(crate) type Focus = Arc<dyn Fn() + Send + Sync>;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Identity {
    pub(crate) dev: u64,
    pub(crate) ino: u64,
}

impl Identity {
    pub(crate) fn of(data: &Path) -> Result<Self> {
        let status = pp_os::status_follow(data).context("Focus data directory unavailable")?;
        ensure!(status.is_dir(), "Focus data directory unavailable");
        let (dev, ino) = status.identity().parts();
        Ok(Self { dev, ino })
    }

    fn line(self) -> String {
        format!("{REQUEST} {} {}\n", self.dev, self.ino)
    }

    fn parse(line: &str) -> Option<Self> {
        let mut parts = line
            .trim_end_matches('\n')
            .strip_prefix(REQUEST)?
            .split(' ');
        let (Some(""), Some(dev), Some(ino), None) =
            (parts.next(), parts.next(), parts.next(), parts.next())
        else {
            return None;
        };
        Some(Self {
            dev: dev.parse().ok()?,
            ino: ino.parse().ok()?,
        })
    }
}

/// True when startup failed because another desktop holds this data directory.
pub fn owned_by_another_desktop(error: &anyhow::Error) -> bool {
    error
        .chain()
        .any(|cause| OWNED.contains(&cause.to_string().as_str()))
}

/// Listener for focus requests. Dropping it stops the listener, so drop it
/// before the lock owner shuts down.
pub struct FocusServer {
    _listener: platform::FocusListener,
}

impl FocusServer {
    /// Listen for focus requests on a data directory whose lock this process
    /// holds. Call inside a tokio runtime.
    pub fn start(data: &Path, focus: impl Fn() + Send + Sync + 'static) -> Result<Self> {
        let identity = Identity::of(data)?;
        Ok(Self {
            _listener: platform::FocusListener::start(data, identity, Arc::new(focus))?,
        })
    }
}

/// Answer one focus request read from `stream`.
pub(crate) async fn answer<S: AsyncRead + AsyncWrite>(
    stream: S,
    identity: Identity,
    focus: &(dyn Fn() + Send + Sync),
) -> Result<()> {
    let (read, mut write) = tokio::io::split(stream);
    let mut line = String::new();
    tokio::io::AsyncReadExt::take(tokio::io::BufReader::new(read), LIMIT)
        .read_line(&mut line)
        .await?;
    let reply = if Identity::parse(&line) == Some(identity) {
        focus();
        FOCUSED
    } else {
        MISMATCH
    };
    write.write_all(format!("{reply}\n").as_bytes()).await?;
    Ok(())
}

/// Ask the desktop that owns `data` to show its window. Returns true only when
/// that owner confirmed the same data directory and focused.
pub fn request(data: &Path) -> Result<bool> {
    let line = Identity::of(data)?.line();
    Ok(platform::exchange(data, &line)?.is_some_and(|reply| reply.trim_end() == FOCUSED))
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    pub(crate) fn data_dir() -> tempfile::TempDir {
        tempfile::Builder::new()
            .prefix("pp-t30-")
            .tempdir()
            .unwrap()
    }

    pub(crate) fn counter() -> (Arc<AtomicUsize>, impl Fn() + Send + Sync + 'static) {
        let count = Arc::new(AtomicUsize::new(0));
        let hits = count.clone();
        (count, move || {
            hits.fetch_add(1, Ordering::SeqCst);
        })
    }

    pub(crate) async fn ask(data: &Path) -> Result<bool> {
        let data = data.to_owned();
        tokio::task::spawn_blocking(move || request(&data)).await?
    }

    /// Send one raw line to the listener of `data` and return its reply.
    async fn send(data: &Path, line: &str) -> String {
        let (data, line) = (data.to_owned(), line.to_owned());
        tokio::task::spawn_blocking(move || platform::exchange(&data, &line))
            .await
            .unwrap()
            .unwrap()
            .unwrap()
    }

    #[tokio::test]
    async fn ticket_t_30_same_data_directory_focuses_the_owner_window() {
        let data = data_dir();
        let (count, focus) = counter();
        let server = FocusServer::start(data.path(), focus).unwrap();
        assert!(ask(data.path()).await.unwrap());
        assert_eq!(count.load(Ordering::SeqCst), 1);
        drop(server);
        assert!(
            !ask(data.path()).await.unwrap_or(false),
            "no owner, no focus"
        );
        assert_eq!(count.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn ticket_t_30_other_data_directory_identity_is_refused() {
        let (owner, other) = (data_dir(), data_dir());
        let (count, focus) = counter();
        let _server = FocusServer::start(owner.path(), focus).unwrap();
        let line = Identity::of(other.path()).unwrap().line();
        assert_eq!(send(owner.path(), &line).await, "mismatch\n");
        assert_eq!(count.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn ticket_t_30_garbled_request_is_refused() {
        let data = data_dir();
        let (count, focus) = counter();
        let _server = FocusServer::start(data.path(), focus).unwrap();
        for line in ["focus v1 1\n", "focus v2 1 2\n", "focus v1 a b\n", "x\n"] {
            assert_eq!(send(data.path(), line).await, "mismatch\n", "{line:?}");
        }
        assert_eq!(count.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn ticket_t_30_only_desktop_lock_refusals_ask_for_focus() {
        let owned = anyhow::anyhow!(OWNED[1]).context("Desktop core startup failed");
        assert!(owned_by_another_desktop(&owned));
        let standalone = anyhow::anyhow!(
            "Data directory already owned by standalone Node; stop it before starting Rust"
        )
        .context("Desktop core startup failed");
        assert!(!owned_by_another_desktop(&standalone));
    }
}
