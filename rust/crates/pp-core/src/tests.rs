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
