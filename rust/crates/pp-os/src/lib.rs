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
//! | Entry access is relative to the opened directory | `openat`, `fstatat` and `unlinkat` on the directory descriptor | `NtCreateFile` with the directory handle as `RootDirectory`, then `NtSetInformationFile` on the entry's own handle to delete it. No entry operation re-resolves a path |
//! | The opened directory can be renamed or removed | Yes; entry operations keep reaching the moved directory, and creating entries in a removed one fails | The same: the directory handle shares read, write and delete |
//! | No-follow | `O_NOFOLLOW` and `AT_SYMLINK_NOFOLLOW` | `FILE_FLAG_OPEN_REPARSE_POINT`, then refusal of any reparse point (symbolic links and junctions) |
//! | Identity | device and inode | volume serial number and 64-bit file index |
//! | Owner and mode | [`PosixAccess`] | `None`: access is governed by ACLs, which this crate does not inspect yet |
//! | Durable rename | `rename` ([`publish_rename`]) or `link` and `unlink` ([`publish_new`]), then [`sync_directory`] | `MoveFileExW` with `MOVEFILE_WRITE_THROUGH`; [`sync_directory`] only checks that the directory opens |
//! | Durable entry changes in a [`Dir`] | [`Dir::create_dir`] or [`Dir::rename_no_replace`], then [`Dir::sync`] (`fsync` on the directory) | Both open the entry with `FILE_WRITE_THROUGH`, the option `MoveFileExW` uses for `MOVEFILE_WRITE_THROUGH`; [`Dir::sync`] does nothing. See [Durability on Windows](#durability-on-windows) |
//! | File flush | `fsync` on a read-only descriptor | `FlushFileBuffers` on a handle with write access |
//! | Delete | `unlinkat`: the name goes at once, open handles stay valid | POSIX-semantics delete (`FILE_DISPOSITION_POSIX_SEMANTICS`), the same as Unix. File systems without it, such as FAT, get the classic delete: the name stays until the last handle closes and cannot be opened meanwhile |
//! | Lock | `flock` through `fs4`, on a file or a directory | `LockFileEx` through `fs4`, on a regular file only; see [Directory locks](#directory-locks) |
//!
//! [ADR 0015]: ../../../docs/adr/0015-shared-filesystem-portability.md
//!
//! # Directory locks
//!
//! `LockFileEx` locks byte ranges of a file's data, and NTFS refuses it on a
//! directory handle. A caller that locks a directory on Unix uses a lock file
//! on Windows instead: a regular file inside the directory, created with
//! [`Dir::create_new_nofollow`] or opened with [`Dir::open_write_nofollow`],
//! then passed to [`try_lock`]. Every process that wants the directory takes
//! that same file's lock, so the exclusivity matches `flock` on the directory:
//! one holder at a time, released by [`unlock`], by closing the file or by
//! process exit. A Windows lock is mandatory for the locked range, but a lock
//! file holds no data, so that difference does not matter. On both systems a
//! held lock survives its name being deleted or replaced, so callers compare
//! the locked file's [`Identity`] with the named entry after locking, as
//! pp-storage's data-directory lease does with `printpartner.lock`.
//! [`Dir::try_lock`] and [`Dir::lock`] take such a lock on [`DIR_LOCK_FILE`]
//! for the caller on Windows and `flock` the directory itself on Unix.
//!
//! # Windows entry names
//!
//! Entry names are matched without case, as Win32 paths are. `NtCreateFile`
//! does not strip trailing dots and spaces the way Win32 path parsing does, so
//! `name.` and `name` are different entries; callers should not use such
//! names. Every [`Dir`] entry operation, creating, renaming and removing
//! directories included, refuses `.`, `..`, separators and drive prefixes
//! before it reaches the system.
//!
//! # Durability on Windows
//!
//! A read handle cannot flush a directory, so [`Dir::sync`] does nothing on
//! Windows. [`Dir::create_dir`] and [`Dir::rename_no_replace`] open the entry
//! with `FILE_WRITE_THROUGH` instead, the create option `MoveFileExW` applies
//! for `MOVEFILE_WRITE_THROUGH`, so the change is committed before the call
//! returns. Callers that flush each file with `File::sync_all` and then
//! publish with one `rename_no_replace` rely on that, where Unix callers
//! `fsync` the directory after `renameat`. Tests check that both handles are
//! write-through; nothing simulates power loss.
//!
//! # File systems without POSIX semantics
//!
//! Deletes and renames ask for POSIX semantics first. A file system that does
//! not implement them, such as FAT, gets the classic information class: a
//! deleted name stays until its last handle closes, and a rename onto an
//! existing name fails as before. Hosted CI runs on NTFS, so tests drive the
//! classic path directly and check the fallback decision with fake results.

use std::{
    ffi::{OsStr, OsString},
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

/// The effective user ID, or `None` on Windows, which has no POSIX owner.
pub fn effective_uid() -> Option<u32> {
    cfg!(unix).then(platform::effective_uid)
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
        self.inner.try_clone().map(|inner| Self { inner })
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

    /// Open the directory at `path`, following links in the path, only to
    /// reach its entries and inspect it: `O_PATH` on Linux and `O_SEARCH` on
    /// macOS, so it needs no read permission. Use [`Dir::reopen`] to
    /// enumerate, sync or lock it. On Windows this is [`Dir::open`].
    pub fn open_search(path: &Path) -> io::Result<Self> {
        platform::Dir::open_search(path).map(|inner| Self { inner })
    }

    /// Open the directory `name` as in [`Dir::open_search`], without
    /// following it.
    pub fn open_search_nofollow(&self, name: &str) -> io::Result<Self> {
        self.inner
            .open_search_nofollow(checked_name(name)?)
            .map(|inner| Self { inner })
    }

    /// Open the directory `name` without following it.
    pub fn open_dir_nofollow(&self, name: &str) -> io::Result<Self> {
        self.inner
            .open_dir_nofollow(checked_name(name)?)
            .map(|inner| Self { inner })
    }

    /// Open this directory again as a new, full handle: `openat(".")` on
    /// Unix, a relative open with an empty name on Windows.
    pub fn reopen(&self) -> io::Result<Self> {
        self.inner.reopen().map(|inner| Self { inner })
    }

    /// The owner and mode bits of the opened directory, read with `fstat`, or
    /// `None` on Windows, where access is governed by ACLs.
    pub fn posix_access(&self) -> io::Result<Option<PosixAccess>> {
        self.inner.posix_access()
    }

    /// The names in this directory, without `.` and `..`, in the order the
    /// file system returns them.
    pub fn entries(&self) -> io::Result<Entries> {
        Ok(Entries {
            inner: self.inner.entries()?,
        })
    }

    /// Open the entry `name` as `how` says, without following it. The result
    /// may be a directory or another non-regular file; callers that need a
    /// regular file check [`status_of`].
    pub fn open_file(&self, name: &str, how: FileOpen) -> io::Result<File> {
        self.inner.open_file(checked_name(name)?, how)
    }

    /// Create the directory `name`, with mode 0700 on Unix. Fails with
    /// `AlreadyExists` when any entry has the name. Follow with
    /// [`Dir::sync`] to make the entry durable.
    pub fn create_dir(&self, name: &str) -> io::Result<()> {
        self.inner.create_dir(checked_name(name)?)
    }

    /// Rename the entry `from`, a link itself rather than its target, to `to`
    /// in this directory, failing with `AlreadyExists` when `to` exists.
    /// Follow with [`Dir::sync`] to make the new name durable.
    pub fn rename_no_replace(&self, from: &str, to: &str) -> io::Result<()> {
        self.inner
            .rename_no_replace(checked_name(from)?, checked_name(to)?)
    }

    /// Remove the empty directory `name`. A link is refused.
    pub fn remove_dir(&self, name: &str) -> io::Result<()> {
        self.inner.remove_dir(checked_name(name)?)
    }

    /// Make entry changes in this directory durable: `fsync` on Unix. On
    /// Windows this does nothing; see
    /// [Durability on Windows](#durability-on-windows).
    pub fn sync(&self) -> io::Result<()> {
        self.inner.sync()
    }

    /// Take an exclusive advisory lock on this directory without waiting,
    /// failing with `WouldBlock` while another handle holds it. Unix locks
    /// the directory with `flock`. Windows locks the regular file
    /// [`DIR_LOCK_FILE`] inside the directory, creating it when missing, and
    /// keeps it open until this `Dir` drops; see
    /// [Directory locks](#directory-locks).
    pub fn try_lock(&self) -> io::Result<()> {
        self.inner.try_lock()
    }

    /// Take the lock as in [`Dir::try_lock`], waiting while another handle
    /// holds it.
    pub fn lock(&self) -> io::Result<()> {
        self.inner.lock()
    }

    /// Release a lock taken through this `Dir`. Closing the `Dir` also
    /// releases it. Like `flock(LOCK_UN)`, this succeeds when no lock is
    /// held.
    pub fn unlock(&self) -> io::Result<()> {
        self.inner.unlock()
    }
}

#[cfg(unix)]
impl std::os::fd::AsFd for Dir {
    fn as_fd(&self) -> std::os::fd::BorrowedFd<'_> {
        self.inner.as_fd()
    }
}

/// The file [`Dir::try_lock`] and [`Dir::lock`] lock inside a directory on
/// Windows, which cannot lock a directory. Unix never creates it. Listings of
/// a locked directory on Windows include it.
pub const DIR_LOCK_FILE: &str = ".pp-os-lock";

/// Create the new file `path` for reading and writing, failing with
/// `AlreadyExists` when it exists. Unix gives it mode 0600; on Windows it
/// inherits its directory's ACL.
pub fn create_new_private_file(path: &Path) -> io::Result<File> {
    platform::create_new_private_file(path)
}

/// How [`Dir::open_file`] opens an entry. None follows a final link, and
/// Unix handles are close-on-exec and non-blocking.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FileOpen {
    /// Read an existing entry (`O_RDONLY`).
    Read,
    /// Read and write an existing entry (`O_RDWR`).
    ReadWrite,
    /// Create a new regular file for writing (`O_WRONLY | O_CREAT | O_EXCL`,
    /// mode 0600), failing with `AlreadyExists` when any entry has the name.
    CreateWriteOnly,
}

/// The names in a directory, from [`Dir::entries`].
pub struct Entries {
    inner: platform::Entries,
}

impl Iterator for Entries {
    type Item = io::Result<OsString>;

    fn next(&mut self) -> Option<Self::Item> {
        self.inner.next()
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
