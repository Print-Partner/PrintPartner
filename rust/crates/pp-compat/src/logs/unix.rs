use std::{
    ffi::CString,
    fs::{File, Metadata, OpenOptions},
    os::{
        fd::{AsRawFd, FromRawFd},
        unix::fs::{MetadataExt, OpenOptionsExt},
    },
    path::Path,
};

pub(super) type Directory = File;

fn c_name(name: &str) -> std::io::Result<CString> {
    Ok(CString::new(name)?)
}

/// Mode 0700 and this user's ownership, as the Reveal log folder command requires.
pub(super) fn private_folder(_directory: &Path, metadata: &Metadata) -> std::io::Result<bool> {
    Ok(metadata.uid() == unsafe { libc::geteuid() } && metadata.mode() & 0o077 == 0)
}

pub(super) fn open_tail(path: &Path) -> std::io::Result<File> {
    OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(path)
}

pub(super) fn open_directory(directory: &Path) -> std::io::Result<Directory> {
    std::fs::create_dir_all(directory)?;
    OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW)
        .open(directory)
}

fn set_mode(file: &File, mode: libc::mode_t) -> std::io::Result<()> {
    if unsafe { libc::fchmod(file.as_raw_fd(), mode) } != 0 {
        return Err(std::io::Error::last_os_error());
    }
    Ok(())
}

pub(super) fn secure_log(file: &File) -> std::io::Result<()> {
    let metadata = file.metadata()?;
    if !metadata.is_file() || metadata.uid() != unsafe { libc::geteuid() } || metadata.nlink() != 1
    {
        return Err(std::io::Error::from(std::io::ErrorKind::InvalidInput));
    }
    set_mode(file, 0o600)
}

pub(super) fn secure_directory(directory: &Directory) -> std::io::Result<()> {
    let metadata = directory.metadata()?;
    if !metadata.is_dir() || metadata.uid() != unsafe { libc::geteuid() } {
        return Err(std::io::Error::from(std::io::ErrorKind::InvalidInput));
    }
    set_mode(directory, 0o700)
}

pub(super) fn secure_archive(directory: &Directory, name: &str) -> std::io::Result<()> {
    let name = c_name(name)?;
    let fd = unsafe {
        libc::openat(
            directory.as_raw_fd(),
            name.as_ptr(),
            libc::O_RDONLY | libc::O_CLOEXEC | libc::O_NOFOLLOW | libc::O_NONBLOCK,
        )
    };
    if fd < 0 {
        let error = std::io::Error::last_os_error();
        if error.kind() == std::io::ErrorKind::NotFound {
            return Ok(());
        }
        if error.kind() == std::io::ErrorKind::PermissionDenied {
            let mut metadata = std::mem::MaybeUninit::<libc::stat>::uninit();
            if unsafe {
                libc::fstatat(
                    directory.as_raw_fd(),
                    name.as_ptr(),
                    metadata.as_mut_ptr(),
                    libc::AT_SYMLINK_NOFOLLOW,
                )
            } != 0
            {
                return Err(std::io::Error::last_os_error());
            }
            let metadata = unsafe { metadata.assume_init() };
            if metadata.st_mode & libc::S_IFMT == libc::S_IFREG
                && metadata.st_uid == unsafe { libc::geteuid() }
                && metadata.st_nlink == 1
                && metadata.st_mode & 0o7777 & !0o600 == 0
            {
                return Ok(());
            }
        }
        return Err(error);
    }
    let file = unsafe { File::from_raw_fd(fd) };
    secure_log(&file)
}

/// `create` adds O_CREAT | O_EXCL. On macOS, racing dirfd-relative O_CREAT
/// opens without O_EXCL can fail with ENOENT.
pub(super) fn open_append(
    directory: &Directory,
    name: &str,
    create: bool,
) -> std::io::Result<File> {
    let mut flags =
        libc::O_WRONLY | libc::O_APPEND | libc::O_CLOEXEC | libc::O_NOFOLLOW | libc::O_NONBLOCK;
    if create {
        flags |= libc::O_CREAT | libc::O_EXCL;
    }
    let name = c_name(name)?;
    let fd = unsafe {
        libc::openat(
            directory.as_raw_fd(),
            name.as_ptr(),
            flags,
            0o600 as libc::c_uint,
        )
    };
    if fd < 0 {
        return Err(std::io::Error::last_os_error());
    }
    Ok(unsafe { File::from_raw_fd(fd) })
}

/// The open log no longer has a name.
pub(super) fn unlinked(file: &File) -> bool {
    file.metadata().is_ok_and(|metadata| metadata.nlink() == 0)
}

/// Rename within the admitted directory, replacing `to`.
pub(super) fn rename(directory: &Directory, from: &str, to: &str) -> std::io::Result<()> {
    let (from, to) = (c_name(from)?, c_name(to)?);
    let directory = directory.as_raw_fd();
    if unsafe { libc::renameat(directory, from.as_ptr(), directory, to.as_ptr()) } != 0 {
        return Err(std::io::Error::last_os_error());
    }
    Ok(())
}

#[cfg(feature = "desktop_rebuild")]
pub(super) fn file_id(file: &File) -> std::io::Result<String> {
    let metadata = file.metadata()?;
    Ok(format!("{}:{}", metadata.dev(), metadata.ino()))
}

#[cfg(test)]
pub(crate) mod test_support {
    use std::{os::unix::fs::PermissionsExt, path::Path};

    pub(crate) fn identity(file: &std::fs::File) -> (u64, u64) {
        use std::os::unix::fs::MetadataExt;
        let metadata = file.metadata().unwrap();
        (metadata.dev(), metadata.ino())
    }

    pub(crate) fn path_identity(path: &Path) -> (u64, u64) {
        use std::os::unix::fs::MetadataExt;
        let metadata = std::fs::symlink_metadata(path).unwrap();
        (metadata.dev(), metadata.ino())
    }

    fn mode(path: &Path) -> u32 {
        std::fs::metadata(path).unwrap().permissions().mode() & 0o7777
    }

    /// Only this user can open `path`: mode 0600 for a file, 0700 for a directory.
    pub(crate) fn private(path: &Path) -> bool {
        mode(path) == if path.is_dir() { 0o700 } else { 0o600 }
    }

    /// Only this user can open `path`: mode 0600 for a file, 0700 for a directory.
    pub(crate) fn restrict(path: &Path) {
        let mode = if path.is_dir() { 0o700 } else { 0o600 };
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode)).unwrap();
    }

    /// Give the group and others access: 0664 for a file, 0775 for a directory.
    pub(crate) fn widen(path: &Path) {
        let mode = if path.is_dir() { 0o775 } else { 0o664 };
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode)).unwrap();
    }

    /// The permission bits of `path`.
    pub(crate) fn access(path: &Path) -> String {
        format!("{:o}", mode(path))
    }

    /// The access `widen` gave is still in place.
    pub(crate) fn widened(path: &Path) -> bool {
        mode(path) & 0o064 == 0o064
    }

    pub(crate) fn symlink_file(target: &Path, link: &Path) {
        std::os::unix::fs::symlink(target, link).unwrap();
    }

    pub(crate) fn symlink_dir(target: &Path, link: &Path) {
        std::os::unix::fs::symlink(target, link).unwrap();
    }
}
