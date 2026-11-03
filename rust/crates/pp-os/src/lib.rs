//! Filesystem mechanics shared by Print Partner crates ([ADR 0015]).
//!
//! This crate opens directories as handles, opens and inspects their entries
//! without following links, compares file identities, takes advisory locks and
//! publishes files durably. It holds no business policy: callers decide which
//! names to open, which owners and modes to accept and when to publish.
//!
//! Each operation has one Unix and one Windows adapter with the same
//! signature. The guarantees differ where the platforms differ:
//!
//! | Guarantee | Unix | Windows |
//! | --- | --- | --- |
//! | Entry access is relative to the opened directory | `openat`, `fstatat` and `unlinkat` on the directory descriptor | The directory handle denies delete sharing, so the directory cannot be renamed or removed while a [`Dir`] is open; entries are opened as `<final path>\<name>`, where the final path is read from the handle with junctions and links resolved |
//! | No-follow | `O_NOFOLLOW` and `AT_SYMLINK_NOFOLLOW` | `FILE_FLAG_OPEN_REPARSE_POINT`, then refusal of any reparse point (symbolic links and junctions) |
//! | Identity | device and inode | volume serial number and 64-bit file index |
//! | Owner and mode | [`PosixAccess`] | `None`: access is governed by ACLs, which this crate does not inspect yet |
//! | Durable rename | `rename` ([`publish_rename`]) or `link` and `unlink` ([`publish_new`]), then [`sync_directory`] | `MoveFileExW` with `MOVEFILE_WRITE_THROUGH`; [`sync_directory`] only checks that the directory opens |
//! | File flush | `fsync` on a read-only descriptor | `FlushFileBuffers` on a handle with write access |
//! | Lock | `flock` through `fs4` | `LockFileEx` through `fs4` |
//!
//! [ADR 0015]: ../../../docs/adr/0015-shared-filesystem-portability.md

use std::{
    ffi::OsStr,
    fs::File,
    io,
    path::{Component, Path},
};

#[cfg(unix)]
mod unix;
#[cfg(unix)]
use unix as platform;

#[cfg(windows)]
mod windows;
#[cfg(windows)]
use windows as platform;

/// Identity of a filesystem object, stable while the object exists.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Identity {
    device: u64,
    index: u64,
}

/// The type an entry had when it was inspected.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    File,
    Directory,
    /// A symbolic link, or on Windows any reparse point such as a junction.
    Link,
    Other,
}

/// POSIX owner and permission bits of an entry.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PosixAccess {
    uid: u32,
    mode: u32,
}

impl PosixAccess {
    #[cfg(test)]
    pub fn uid(self) -> u32 {
        self.uid
    }

    /// The full `st_mode`, including the file type bits.
    pub fn mode(self) -> u32 {
        self.mode
    }

    pub fn owned_by_current_user(self) -> bool {
        self.uid == platform::effective_uid()
    }

    pub fn owned_by_root(self) -> bool {
        self.uid == 0
    }
}

/// What one inspection of an entry saw.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Status {
    kind: Kind,
    identity: Identity,
    links: u64,
    access: Option<PosixAccess>,
}

impl Status {
    #[cfg(test)]
    pub fn kind(&self) -> Kind {
        self.kind
    }

    pub fn is_file(&self) -> bool {
        self.kind == Kind::File
    }

    pub fn is_dir(&self) -> bool {
        self.kind == Kind::Directory
    }

    pub fn identity(&self) -> Identity {
        self.identity
    }

    /// The number of hard links to the entry.
    pub fn links(&self) -> u64 {
        self.links
    }

    /// The owner and mode bits, or `None` where access is governed by ACLs.
    pub fn posix_access(&self) -> Option<PosixAccess> {
        self.access
    }
}

/// Inspect an open file or directory.
pub fn status_of(file: &File) -> io::Result<Status> {
    platform::status_of(file)
}

/// Inspect `path`, following a final link.
pub fn status_follow(path: &Path) -> io::Result<Status> {
    platform::status_follow(path)
}

/// Inspect `path` without following a final link.
pub fn status_nofollow(path: &Path) -> io::Result<Status> {
    platform::status_nofollow(path)
}

/// Open `path` for reading without following a final link. The result may be
/// a directory; callers that need a regular file check [`status_of`].
pub fn open_read_nofollow(path: &Path) -> io::Result<File> {
    platform::open_read_nofollow(path)
}

/// The error for an entry that should be a regular file but is not.
pub fn not_regular_file(found_directory: bool) -> io::Error {
    platform::not_regular_file(found_directory)
}

/// Create one directory that only its owner can enter (mode 0700 on Unix).
pub fn create_private_dir(path: &Path) -> io::Result<()> {
    platform::create_private_dir(path, false)
}

/// Create a directory and missing parents, each private as in
/// [`create_private_dir`]. An existing directory is left unchanged.
pub fn create_private_dir_all(path: &Path) -> io::Result<()> {
    platform::create_private_dir(path, true)
}

/// Rename `from` over `to`, replacing a file at `to`. Follow with
/// [`sync_directory`] on the parent to make the new name durable.
pub fn publish_rename(from: &Path, to: &Path) -> io::Result<()> {
    platform::publish_rename(from, to)
}

/// Move `from` to the new name `to`, failing with `AlreadyExists` when `to`
/// exists. Follow with [`sync_directory`] on the parent to make the new name
/// durable.
pub fn publish_new(from: &Path, to: &Path) -> io::Result<()> {
    platform::publish_new(from, to)
}

/// Flush a file's contents to disk, given its path.
pub fn sync_file(path: &Path) -> io::Result<()> {
    platform::sync_file(path)
}

/// Flush a directory's entries, so renames and links in it survive a crash.
pub fn sync_directory(path: &Path) -> io::Result<()> {
    platform::sync_directory(path)
}

/// Why [`try_lock`] did not take the lock.
#[derive(Debug)]
pub enum TryLockError {
    /// Another open file holds the lock.
    WouldBlock,
    Io(io::Error),
}

/// Take an exclusive advisory lock on `file` without waiting.
pub fn try_lock(file: &File) -> Result<(), TryLockError> {
    match fs4::FileExt::try_lock(file) {
        Ok(()) => Ok(()),
        Err(fs4::TryLockError::WouldBlock) => Err(TryLockError::WouldBlock),
        Err(fs4::TryLockError::Error(error)) => Err(TryLockError::Io(error)),
    }
}

/// Release a lock taken with [`try_lock`].
pub fn unlock(file: &File) -> io::Result<()> {
    fs4::FileExt::unlock(file)
}

/// An open directory. Entry operations take one plain name, never a path.
pub struct Dir {
    inner: platform::Dir,
}

impl Dir {
    /// Open the directory at `path`, following links in the path.
    pub fn open(path: &Path) -> io::Result<Self> {
        Ok(Self {
            inner: platform::Dir::open(path)?,
        })
    }

    pub fn try_clone(&self) -> io::Result<Self> {
        Ok(Self {
            inner: self.inner.try_clone()?,
        })
    }

    /// Inspect the opened directory itself.
    pub fn status(&self) -> io::Result<Status> {
        self.inner.status()
    }

    /// Inspect the entry `name` without following it.
    pub fn status_nofollow(&self, name: &str) -> io::Result<Status> {
        self.inner.status_nofollow(checked_name(name)?)
    }

    /// Open the entry `name` for reading without following it. The result may
    /// be a directory or another non-regular file.
    pub fn open_read_nofollow(&self, name: &str) -> io::Result<File> {
        self.inner.open_read_nofollow(checked_name(name)?)
    }

    /// Create the new regular file `name` for reading and writing, with mode
    /// 0600 on Unix. Fails with `AlreadyExists` when any entry has the name.
    pub fn create_new_nofollow(&self, name: &str) -> io::Result<File> {
        self.inner.create_new_nofollow(checked_name(name)?)
    }

    /// Open the existing entry `name` for reading and writing without
    /// following it.
    pub fn open_write_nofollow(&self, name: &str) -> io::Result<File> {
        self.inner.open_write_nofollow(checked_name(name)?)
    }

    /// Remove the entry `name`, which must not be a directory.
    pub fn remove_file(&self, name: &str) -> io::Result<()> {
        self.inner.remove_file(checked_name(name)?)
    }
}

/// Refuse anything but one normal path component, so an entry operation
/// cannot leave its directory.
fn checked_name(name: &str) -> io::Result<&str> {
    let mut components = Path::new(name).components();
    match (components.next(), components.next()) {
        (Some(Component::Normal(part)), None) if part == OsStr::new(name) => Ok(name),
        _ => Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("not a single directory entry name: {name:?}"),
        )),
    }
}

#[cfg(test)]
mod tests;
