use std::{fs::File, io, path::Path};

/// A new file. Windows has no mode bits: it inherits the private ACL of the
/// directory it is created in.
pub(crate) fn create_private_file(path: &Path) -> io::Result<File> {
    File::create_new(path)
}

/// Ctrl+C, Ctrl+Break, the console closing, the user logging off, or the
/// system shutting down. For the last three Windows ends the process when its
/// grace period runs out, whether or not the shutdown finished.
pub(crate) fn stop_signal() -> io::Result<impl Future<Output = ()>> {
    use tokio::signal::windows::{ctrl_break, ctrl_c, ctrl_close, ctrl_logoff, ctrl_shutdown};
    let mut interrupt = ctrl_c()?;
    let mut brk = ctrl_break()?;
    let mut close = ctrl_close()?;
    let mut logoff = ctrl_logoff()?;
    let mut shutdown = ctrl_shutdown()?;
    Ok(async move {
        tokio::select! {
            _ = interrupt.recv() => {}
            _ = brk.recv() => {}
            _ = close.recv() => {}
            _ = logoff.recv() => {}
            _ = shutdown.recv() => {}
        }
    })
}
