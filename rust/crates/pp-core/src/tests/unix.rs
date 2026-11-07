use std::{
    ffi::CString,
    os::unix::{
        ffi::OsStrExt,
        fs::{FileTypeExt, PermissionsExt},
        process::ExitStatusExt,
    },
    path::Path,
    process::ExitStatus,
};

pub(crate) use std::os::unix::fs::symlink as symlink_file;
#[cfg(feature = "desktop_rebuild")]
pub(crate) use std::os::unix::fs::symlink as symlink_dir;

/// A FIFO: an entry that is neither a file, a directory nor a link.
pub(crate) fn special_entry(path: &Path) {
    let path = CString::new(path.as_os_str().as_bytes()).unwrap();
    assert_eq!(unsafe { libc::mkfifo(path.as_ptr(), 0o600) }, 0);
}

pub(crate) fn assert_special_entry(path: &Path) {
    assert!(
        std::fs::symlink_metadata(path)
            .unwrap()
            .file_type()
            .is_fifo()
    );
}

/// New state files get mode 0600.
pub(crate) fn assert_private_file(path: &Path) {
    let mode = std::fs::metadata(path).unwrap().permissions().mode();
    assert_eq!(mode & 0o777, 0o600);
}

pub(crate) fn deny_read(path: &Path) {
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o000)).unwrap();
}

pub(crate) fn allow_read(path: &Path) {
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600)).unwrap();
}

/// SIGKILLs this process without running any more of its code. `kill` may
/// return before delivery (seen on macOS), and unwinding would drop the lease
/// and clean up state a real crash leaves behind.
pub(crate) fn kill_self() -> ! {
    assert_eq!(unsafe { libc::kill(libc::getpid(), libc::SIGKILL) }, 0);
    loop {
        std::thread::park();
    }
}

/// The child ended through [`kill_self`].
pub(crate) fn killed(status: &ExitStatus) -> bool {
    status.signal() == Some(libc::SIGKILL)
}

pub(crate) fn raw_socket(listener: &std::net::TcpListener) -> std::os::fd::RawFd {
    std::os::fd::AsRawFd::as_raw_fd(listener)
}

/// The type and local address of the socket `fd` names, or `None` when `fd`
/// is not an open socket.
pub(crate) fn socket_name(fd: std::os::fd::RawFd) -> Option<(i32, Vec<u8>)> {
    let mut kind: libc::c_int = 0;
    let mut kind_len = std::mem::size_of_val(&kind) as libc::socklen_t;
    let kind_status = unsafe {
        libc::getsockopt(
            fd,
            libc::SOL_SOCKET,
            libc::SO_TYPE,
            (&raw mut kind).cast(),
            &mut kind_len,
        )
    };
    let mut storage: libc::sockaddr_storage = unsafe { std::mem::zeroed() };
    let mut len = std::mem::size_of_val(&storage) as libc::socklen_t;
    let status = unsafe { libc::getsockname(fd, (&raw mut storage).cast(), &mut len) };
    (kind_status == 0 && status == 0).then(|| {
        let bytes =
            unsafe { std::slice::from_raw_parts((&raw const storage).cast::<u8>(), len as usize) };
        (kind, bytes.to_vec())
    })
}
