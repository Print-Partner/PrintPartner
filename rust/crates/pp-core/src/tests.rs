//! Platform fixtures shared by the unit tests: links, special entries, file
//! access, self-termination and socket identity.

#[cfg(unix)]
pub(crate) mod unix;
#[cfg(unix)]
pub(crate) use unix as platform;

#[cfg(windows)]
pub(crate) mod windows;
#[cfg(windows)]
pub(crate) use windows as platform;

/// `path` made absolute on this host: Windows needs a drive.
pub(crate) fn abs(path: &str) -> String {
    if cfg!(windows) {
        format!("C:{path}")
    } else {
        path.to_owned()
    }
}
