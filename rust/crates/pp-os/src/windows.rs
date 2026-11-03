use crate::{Identity, Kind, Status};
use std::{
    ffi::{OsStr, OsString},
    fs::{File, OpenOptions},
    io,
    os::windows::{
        ffi::{OsStrExt, OsStringExt},
        fs::OpenOptionsExt,
        io::AsRawHandle,
    },
    path::{Path, PathBuf},
};
use windows_sys::Win32::Storage::FileSystem::{
    BY_HANDLE_FILE_INFORMATION, FILE_ATTRIBUTE_DIRECTORY, FILE_ATTRIBUTE_REPARSE_POINT,
    FILE_FLAG_BACKUP_SEMANTICS, FILE_FLAG_OPEN_REPARSE_POINT, FILE_NAME_NORMALIZED,
    FILE_READ_ATTRIBUTES, FILE_SHARE_DELETE, FILE_SHARE_READ, FILE_SHARE_WRITE,
    GetFileInformationByHandle, GetFinalPathNameByHandleW, MOVE_FILE_FLAGS,
    MOVEFILE_REPLACE_EXISTING, MOVEFILE_WRITE_THROUGH, MoveFileExW, VOLUME_NAME_DOS,
};

/// Windows has no POSIX owner; [`crate::PosixAccess`] is never constructed.
pub(crate) fn effective_uid() -> u32 {
    u32::MAX
}

fn information(file: &File) -> io::Result<BY_HANDLE_FILE_INFORMATION> {
    let mut information = std::mem::MaybeUninit::<BY_HANDLE_FILE_INFORMATION>::uninit();
    // SAFETY: the handle is open for the lifetime of `file`, and the call
    // fills the whole structure when it succeeds.
    if unsafe { GetFileInformationByHandle(file.as_raw_handle(), information.as_mut_ptr()) } == 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(unsafe { information.assume_init() })
}

pub(crate) fn status_of(file: &File) -> io::Result<Status> {
    let information = information(file)?;
    let attributes = information.dwFileAttributes;
    let kind = if attributes & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
        Kind::Link
    } else if attributes & FILE_ATTRIBUTE_DIRECTORY != 0 {
        Kind::Directory
    } else {
        Kind::File
    };
    Ok(Status {
        kind,
        identity: Identity {
            device: u64::from(information.dwVolumeSerialNumber),
            index: (u64::from(information.nFileIndexHigh) << 32)
                | u64::from(information.nFileIndexLow),
        },
        links: u64::from(information.nNumberOfLinks),
        access: None,
    })
}

/// Open only to read attributes. Attribute-only access takes no part in
/// sharing checks, so this works on locked and pinned entries.
fn open_attributes(path: &Path, follow: bool) -> io::Result<File> {
    let mut flags = FILE_FLAG_BACKUP_SEMANTICS;
    if !follow {
        flags |= FILE_FLAG_OPEN_REPARSE_POINT;
    }
    OpenOptions::new()
        .access_mode(FILE_READ_ATTRIBUTES)
        .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE)
        .custom_flags(flags)
        .open(path)
}

pub(crate) fn status_follow(path: &Path) -> io::Result<Status> {
    status_of(&open_attributes(path, true)?)
}

pub(crate) fn status_nofollow(path: &Path) -> io::Result<Status> {
    status_of(&open_attributes(path, false)?)
}

/// `FILE_FLAG_OPEN_REPARSE_POINT` opens a link itself instead of its target.
/// Refuse it, as `O_NOFOLLOW` does on Unix.
fn refuse_link(file: File) -> io::Result<File> {
    if status_of(&file)?.kind == Kind::Link {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "refusing to follow a symbolic link or reparse point",
        ));
    }
    Ok(file)
}

pub(crate) fn open_read_nofollow(path: &Path) -> io::Result<File> {
    refuse_link(
        OpenOptions::new()
            .read(true)
            .custom_flags(FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT)
            .open(path)?,
    )
}

pub(crate) fn not_regular_file(found_directory: bool) -> io::Error {
    if found_directory {
        io::Error::from(io::ErrorKind::IsADirectory)
    } else {
        io::Error::new(io::ErrorKind::InvalidInput, "not a regular file")
    }
}

/// New directories inherit the parent's ACL. The data directory's default
/// parent, the user's profile, is private to that user.
pub(crate) fn create_private_dir(path: &Path, recursive: bool) -> io::Result<()> {
    std::fs::DirBuilder::new().recursive(recursive).create(path)
}

fn wide(path: &Path) -> Vec<u16> {
    OsStr::new(path).encode_wide().chain(Some(0)).collect()
}

fn move_file(from: &Path, to: &Path, flags: MOVE_FILE_FLAGS) -> io::Result<()> {
    let (from, to) = (wide(from), wide(to));
    // SAFETY: both buffers are NUL-terminated UTF-16 and outlive the call.
    if unsafe { MoveFileExW(from.as_ptr(), to.as_ptr(), flags) } == 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

pub(crate) fn publish_rename(from: &Path, to: &Path) -> io::Result<()> {
    move_file(from, to, MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH)
}

/// Without `MOVEFILE_REPLACE_EXISTING` the move fails when `to` exists.
pub(crate) fn publish_new(from: &Path, to: &Path) -> io::Result<()> {
    move_file(from, to, MOVEFILE_WRITE_THROUGH)
}

/// `FlushFileBuffers` needs write access; a read-only handle gets access
/// denied.
pub(crate) fn sync_file(path: &Path) -> io::Result<()> {
    OpenOptions::new().write(true).open(path)?.sync_all()
}

/// Windows cannot flush a directory through a read handle. Renames made with
/// [`publish_rename`] and [`publish_new`] are written through before they
/// return, so this only checks that the directory still opens.
pub(crate) fn sync_directory(path: &Path) -> io::Result<()> {
    let status = status_follow(path)?;
    if status.kind != Kind::Directory {
        return Err(io::Error::from(io::ErrorKind::NotADirectory));
    }
    Ok(())
}

/// The path of the object `file` is open on, with every junction and symbolic
/// link resolved, in `\\?\` form.
fn final_path(file: &File) -> io::Result<PathBuf> {
    let mut buffer = vec![0u16; 512];
    loop {
        let capacity = u32::try_from(buffer.len()).unwrap_or(u32::MAX);
        // SAFETY: the handle is open for the lifetime of `file`, and the call
        // writes at most `capacity` UTF-16 units into `buffer`.
        let length = unsafe {
            GetFinalPathNameByHandleW(
                file.as_raw_handle(),
                buffer.as_mut_ptr(),
                capacity,
                FILE_NAME_NORMALIZED | VOLUME_NAME_DOS,
            )
        } as usize;
        if length == 0 {
            return Err(io::Error::last_os_error());
        }
        // On success the length excludes the NUL; when the buffer is too small
        // it is the size needed, including the NUL.
        if length < buffer.len() {
            buffer.truncate(length);
            return Ok(PathBuf::from(OsString::from_wide(&buffer)));
        }
        buffer.resize(length, 0);
    }
}

pub(crate) struct Dir {
    directory: File,
    path: PathBuf,
}

impl Dir {
    /// Deny delete sharing: while this handle is open the directory cannot be
    /// renamed or removed. `path` is the handle's final path, not the input
    /// spelling, so replacing a junction or symbolic link used to reach the
    /// directory does not redirect entry operations.
    pub(crate) fn open(path: &Path) -> io::Result<Self> {
        let directory = OpenOptions::new()
            .read(true)
            .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE)
            .custom_flags(FILE_FLAG_BACKUP_SEMANTICS)
            .open(path)?;
        let path = final_path(&directory)?;
        Ok(Self { directory, path })
    }

    pub(crate) fn try_clone(&self) -> io::Result<Self> {
        Ok(Self {
            directory: self.directory.try_clone()?,
            path: self.path.clone(),
        })
    }

    pub(crate) fn status(&self) -> io::Result<Status> {
        status_of(&self.directory)
    }

    pub(crate) fn status_nofollow(&self, name: &str) -> io::Result<Status> {
        status_nofollow(&self.path.join(name))
    }

    pub(crate) fn open_read_nofollow(&self, name: &str) -> io::Result<File> {
        open_read_nofollow(&self.path.join(name))
    }

    /// `CREATE_NEW` fails on any existing entry, including a dangling link.
    /// For a directory or directory link it reports access denied instead of
    /// "file exists"; when an entry is present, report `AlreadyExists` as Unix
    /// does.
    pub(crate) fn create_new_nofollow(&self, name: &str) -> io::Result<File> {
        let path = self.path.join(name);
        OpenOptions::new()
            .read(true)
            .write(true)
            .create_new(true)
            .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT)
            .open(&path)
            .map_err(|error| {
                if error.kind() == io::ErrorKind::PermissionDenied && status_nofollow(&path).is_ok()
                {
                    io::Error::new(io::ErrorKind::AlreadyExists, error)
                } else {
                    error
                }
            })
    }

    pub(crate) fn open_write_nofollow(&self, name: &str) -> io::Result<File> {
        refuse_link(
            OpenOptions::new()
                .read(true)
                .write(true)
                .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT)
                .open(self.path.join(name))?,
        )
    }

    pub(crate) fn remove_file(&self, name: &str) -> io::Result<()> {
        std::fs::remove_file(self.path.join(name))
    }
}
