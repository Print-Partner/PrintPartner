//! Mode pinning shared by pp-source's unit and integration tests.

/// Pins a Unix mode so root ancestor checks ignore the umask. Windows has none.
pub fn pin_mode(path: impl AsRef<std::path::Path>, mode: u32) {
    #[cfg(unix)]
    std::fs::set_permissions(path, std::os::unix::fs::PermissionsExt::from_mode(mode)).unwrap();
    #[cfg(windows)]
    let _ = (path, mode);
}
