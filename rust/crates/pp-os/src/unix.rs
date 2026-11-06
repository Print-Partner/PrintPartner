use crate::{FileOpen, Identity, Kind, PosixAccess, Status};
use rustix::fs::{self as rfs, AtFlags, Mode, OFlags, RenameFlags};
use std::{
    ffi::{CString, OsStr, OsString},
    fs::{File, Metadata, OpenOptions},
    io,
    os::{
        fd::{AsFd, AsRawFd, BorrowedFd, FromRawFd},
        unix::{
            ffi::OsStrExt,
            fs::{DirBuilderExt, MetadataExt, OpenOptionsExt},
        },
    },
    path::Path,
};

pub(crate) fn effective_uid() -> u32 {
    unsafe { libc::geteuid() }
}

pub(crate) fn optional_uid() -> Option<u32> {
    Some(effective_uid())
}

/// Unix names have no device aliases.
pub(crate) fn refuses_alias(_name: &str) -> bool {
    false
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

pub(crate) fn publish_new(from: &Path, to: &Path) -> io::Result<()> {
    std::fs::hard_link(from, to)?;
    std::fs::remove_file(from)
}

pub(crate) fn create_new_private_file(path: &Path) -> io::Result<File> {
    OpenOptions::new()
        .read(true)
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)
}

pub(crate) fn sync_file(path: &Path) -> io::Result<()> {
    File::open(path)?.sync_all()
}

pub(crate) fn sync_directory(path: &Path) -> io::Result<()> {
    File::open(path)?.sync_all()
}

pub(crate) struct Dir {
    directory: File,
}

impl Dir {
    pub(crate) fn from_open(directory: File) -> Self {
        Self { directory }
    }

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
    /// `flock(LOCK_EX | LOCK_NB)` on the directory. A held lock reports
    /// `EWOULDBLOCK`, the error `fs2` returned before.
    pub(crate) fn try_lock(&self) -> io::Result<()> {
        crate::try_lock_or(&self.directory, || {
            io::Error::from_raw_os_error(libc::EWOULDBLOCK)
        })
    }

    pub(crate) fn lock(&self) -> io::Result<()> {
        fs4::FileExt::lock(&self.directory)
    }

    pub(crate) fn unlock(&self) -> io::Result<()> {
        crate::unlock(&self.directory)
    }

    pub(crate) fn as_fd(&self) -> BorrowedFd<'_> {
        self.directory.as_fd()
    }
}

/// Directory operations that use `rustix`, with the flags and modes
/// pp-source passed to it before it moved onto pp-os.
impl Dir {
    fn from_fd(fd: std::os::fd::OwnedFd) -> Self {
        Self {
            directory: File::from(fd),
        }
    }

    /// Search-only access: `O_PATH` on Linux, `O_SEARCH` on macOS.
    fn search_flags() -> OFlags {
        #[cfg(target_os = "linux")]
        {
            OFlags::PATH
        }
        #[cfg(target_os = "macos")]
        {
            OFlags::from_bits_retain(libc::O_SEARCH as _)
        }
        #[cfg(not(any(target_os = "linux", target_os = "macos")))]
        {
            OFlags::RDONLY
        }
    }

    pub(crate) fn open_search(path: &Path) -> io::Result<Self> {
        Ok(Self::from_fd(rfs::open(
            path,
            Self::search_flags() | OFlags::DIRECTORY | OFlags::CLOEXEC,
            Mode::empty(),
        )?))
    }

    pub(crate) fn open_search_nofollow(&self, name: &str) -> io::Result<Self> {
        Ok(Self::from_fd(rfs::openat(
            &self.directory,
            name,
            Self::search_flags() | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
            Mode::empty(),
        )?))
    }

    pub(crate) fn open_dir_nofollow(&self, name: &str) -> io::Result<Self> {
        Ok(Self::from_fd(rfs::openat(
            &self.directory,
            name,
            OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
            Mode::empty(),
        )?))
    }

    pub(crate) fn reopen(&self) -> io::Result<Self> {
        self.open_dir_nofollow(".")
    }

    pub(crate) fn posix_access(&self) -> io::Result<Option<PosixAccess>> {
        let stat = rfs::fstat(&self.directory)?;
        Ok(Some(PosixAccess {
            uid: stat.st_uid,
            mode: stat.st_mode as u32,
        }))
    }

    pub(crate) fn entries(&self) -> io::Result<Entries> {
        Ok(Entries(rfs::Dir::read_from(&self.directory)?))
    }

    pub(crate) fn open_file(&self, name: &str, how: FileOpen) -> io::Result<File> {
        let access = match how {
            FileOpen::Read => OFlags::RDONLY,
            FileOpen::ReadWrite => OFlags::RDWR,
            FileOpen::CreateWriteOnly => OFlags::WRONLY | OFlags::CREATE | OFlags::EXCL,
            FileOpen::Append => OFlags::WRONLY | OFlags::APPEND,
            FileOpen::CreateAppend => {
                OFlags::WRONLY | OFlags::APPEND | OFlags::CREATE | OFlags::EXCL
            }
        };
        Ok(File::from(rfs::openat(
            &self.directory,
            name,
            OFlags::NOFOLLOW | OFlags::CLOEXEC | OFlags::NONBLOCK | access,
            Mode::RUSR | Mode::WUSR,
        )?))
    }

    pub(crate) fn create_dir(&self, name: &str) -> io::Result<()> {
        Ok(rfs::mkdirat(
            &self.directory,
            name,
            Mode::RUSR | Mode::WUSR | Mode::XUSR,
        )?)
    }

    pub(crate) fn rename_no_replace(&self, from: &str, to: &str) -> io::Result<()> {
        Ok(rfs::renameat_with(
            &self.directory,
            from,
            &self.directory,
            to,
            RenameFlags::NOREPLACE,
        )?)
    }

    pub(crate) fn rename_replace(&self, from: &str, to: &str) -> io::Result<()> {
        Ok(rfs::renameat(&self.directory, from, &self.directory, to)?)
    }

    pub(crate) fn remove_dir(&self, name: &str) -> io::Result<()> {
        Ok(rfs::unlinkat(&self.directory, name, AtFlags::REMOVEDIR)?)
    }

    pub(crate) fn sync(&self) -> io::Result<()> {
        self.directory.sync_all()
    }
}

pub(crate) struct Entries(rfs::Dir);

impl Iterator for Entries {
    type Item = io::Result<OsString>;

    fn next(&mut self) -> Option<Self::Item> {
        loop {
            let entry = match self.0.next()? {
                Ok(entry) => entry,
                Err(error) => return Some(Err(error.into())),
            };
            let name = OsStr::from_bytes(entry.file_name().to_bytes());
            if name != "." && name != ".." {
                return Some(Ok(name.to_owned()));
            }
        }
    }
}

/// `flock(LOCK_EX | LOCK_NB)` on `file`.
pub(crate) fn try_lock(file: &File) -> Result<(), crate::TryLockError> {
    match fs4::FileExt::try_lock(file) {
        Ok(()) => Ok(()),
        Err(fs4::TryLockError::WouldBlock) => Err(crate::TryLockError::WouldBlock),
        Err(fs4::TryLockError::Error(error)) => Err(crate::TryLockError::Io(error)),
    }
}

pub(crate) fn unlock(file: &File) -> io::Result<()> {
    fs4::FileExt::unlock(file)
}
