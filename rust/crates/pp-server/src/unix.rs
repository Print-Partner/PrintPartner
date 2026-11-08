use std::{fs::File, io, os::unix::fs::OpenOptionsExt, path::Path};

/// A new file with mode 0600.
pub(crate) fn create_private_file(path: &Path) -> io::Result<File> {
    File::options()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)
}

/// SIGINT or SIGTERM.
pub(crate) fn stop_signal() -> io::Result<impl Future<Output = ()>> {
    use tokio::signal::unix::{SignalKind, signal};
    let mut interrupt = signal(SignalKind::interrupt())?;
    let mut terminate = signal(SignalKind::terminate())?;
    Ok(async move {
        tokio::select! {
            _ = interrupt.recv() => {}
            _ = terminate.recv() => {}
        }
    })
}
