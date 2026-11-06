use std::{fs::File, io, path::Path};

/// A new file. Windows has no mode bits: it inherits the private ACL of the
/// directory it is created in.
pub(crate) fn create_private_file(path: &Path) -> io::Result<File> {
    File::create_new(path)
}

/// Ctrl+C, Ctrl+Break, or the console closing.
pub(crate) fn stop_signal() -> io::Result<impl Future<Output = ()>> {
    use tokio::signal::windows::{ctrl_break, ctrl_c, ctrl_close};
    let mut interrupt = ctrl_c()?;
    let mut brk = ctrl_break()?;
    let mut close = ctrl_close()?;
    Ok(async move {
        tokio::select! {
            _ = interrupt.recv() => {}
            _ = brk.recv() => {}
            _ = close.recv() => {}
        }
    })
}
