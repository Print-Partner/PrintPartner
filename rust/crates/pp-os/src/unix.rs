use crate::{Identity, Kind, PosixAccess, Status};
use std::{
    ffi::CString,
    fs::{File, Metadata, OpenOptions},
    io,
    os::{
        fd::{AsRawFd, FromRawFd},
        unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt},
    },
    path::Path,
};

pub(crate) fn effective_uid() -> u32 {
    unsafe { libc::geteuid() }
}

fn from_metadata(metadata: &Metadata) -> Status {
    let file_type = metadata.file_type();
    let kind = if file_type.is_symlink() {
        Kind::Link
    } else if file_type.is_dir() {
        Kind::Directory
    } else if file_type.is_file() {
        Kind::File
    } else {
        Kind::Other
    };
    Status {
        kind,
        identity: Identity {
            device: metadata.dev(),
            index: metadata.ino(),
        },
        links: metadata.nlink(),
        access: Some(PosixAccess {
            uid: metadata.uid(),
            mode: metadata.mode(),
        }),
    }
}

/// Widen a `stat` field the way `std::os::unix::fs::MetadataExt` does (`as
/// u64`). Field types differ between Linux and macOS.
fn widen<T: Into<i128>>(value: T) -> u64 {
    value.into() as u64
}

fn from_stat(stat: &libc::stat) -> Status {
    let kind = match stat.st_mode & libc::S_IFMT {
        libc::S_IFLNK => Kind::Link,
        libc::S_IFDIR => Kind::Directory,
        libc::S_IFREG => Kind::File,
        _ => Kind::Other,
    };
    Status {
        kind,
        identity: Identity {
            device: widen(stat.st_dev),
            index: widen(stat.st_ino),
        },
        links: widen(stat.st_nlink),
        access: Some(PosixAccess {
            uid: stat.st_uid,
            mode: widen(stat.st_mode) as u32,
        }),
    }
}

pub(crate) fn status_of(file: &File) -> io::Result<Status> {
    Ok(from_metadata(&file.metadata()?))
}

pub(crate) fn status_follow(path: &Path) -> io::Result<Status> {
    Ok(from_metadata(&std::fs::metadata(path)?))
}

pub(crate) fn status_nofollow(path: &Path) -> io::Result<Status> {
    Ok(from_metadata(&std::fs::symlink_metadata(path)?))
}

pub(crate) fn open_read_nofollow(path: &Path) -> io::Result<File> {
    OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW)
        .open(path)
}

pub(crate) fn not_regular_file(found_directory: bool) -> io::Error {
    io::Error::from_raw_os_error(if found_directory {
        libc::EISDIR
    } else {
        libc::EINVAL
    })
}

pub(crate) fn create_private_dir(path: &Path, recursive: bool) -> io::Result<()> {
    std::fs::DirBuilder::new()
        .recursive(recursive)
        .mode(0o700)
        .create(path)
}

pub(crate) fn publish_rename(from: &Path, to: &Path) -> io::Result<()> {
    std::fs::rename(from, to)
}

pub(crate) fn sync_directory(path: &Path) -> io::Result<()> {
    File::open(path)?.sync_all()
}

pub(crate) struct Dir {
    directory: File,
}

impl Dir {
    pub(crate) fn open(path: &Path) -> io::Result<Self> {
        let directory = OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_DIRECTORY | libc::O_NONBLOCK)
            .open(path)?;
        Ok(Self { directory })
    }

    pub(crate) fn try_clone(&self) -> io::Result<Self> {
        Ok(Self {
            directory: self.directory.try_clone()?,
        })
    }

    pub(crate) fn status(&self) -> io::Result<Status> {
        status_of(&self.directory)
    }

    pub(crate) fn status_nofollow(&self, name: &str) -> io::Result<Status> {
        let name = CString::new(name)?;
        let mut stat = std::mem::MaybeUninit::<libc::stat>::uninit();
        let inspected = unsafe {
            libc::fstatat(
                self.directory.as_raw_fd(),
                name.as_ptr(),
                stat.as_mut_ptr(),
                libc::AT_SYMLINK_NOFOLLOW,
            )
        };
        if inspected < 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(from_stat(&unsafe { stat.assume_init() }))
    }

    pub(crate) fn open_read_nofollow(&self, name: &str) -> io::Result<File> {
        self.openat(
            name,
            libc::O_RDONLY | libc::O_CLOEXEC | libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_NOCTTY,
        )
    }

    /// Create with O_EXCL and open an existing entry without O_CREAT, as two
    /// calls. On macOS, racing dirfd-relative O_CREAT opens without O_EXCL can
    /// fail with ENOENT.
    pub(crate) fn create_new_nofollow(&self, name: &str) -> io::Result<File> {
        self.openat(name, Self::WRITE | libc::O_CREAT | libc::O_EXCL)
    }

    pub(crate) fn open_write_nofollow(&self, name: &str) -> io::Result<File> {
        self.openat(name, Self::WRITE)
    }

    const WRITE: libc::c_int = libc::O_RDWR | libc::O_CLOEXEC | libc::O_NOFOLLOW | libc::O_NONBLOCK;

    fn openat(&self, name: &str, flags: libc::c_int) -> io::Result<File> {
        let name = CString::new(name)?;
        let mode: libc::c_uint = 0o600;
        let fd = unsafe { libc::openat(self.directory.as_raw_fd(), name.as_ptr(), flags, mode) };
        if fd < 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(unsafe { File::from_raw_fd(fd) })
    }

    pub(crate) fn remove_file(&self, name: &str) -> io::Result<()> {
        let name = CString::new(name)?;
        if unsafe { libc::unlinkat(self.directory.as_raw_fd(), name.as_ptr(), 0) } < 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(())
    }
}
