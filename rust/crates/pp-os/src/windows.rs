use crate::{FileOpen, Identity, Kind, PosixAccess, Status};
use std::{
    ffi::{OsStr, OsString},
    fs::{File, OpenOptions},
    io,
    mem::offset_of,
    os::windows::{
        ffi::{OsStrExt, OsStringExt},
        fs::OpenOptionsExt,
        io::{AsRawHandle, FromRawHandle},
    },
    path::Path,
    sync::OnceLock,
};
use windows_sys::{
    Wdk::{
        Foundation::OBJECT_ATTRIBUTES,
        Storage::FileSystem::{
            FILE_CREATE, FILE_DIRECTORY_FILE, FILE_DISPOSITION_DELETE,
            FILE_DISPOSITION_IGNORE_READONLY_ATTRIBUTE, FILE_DISPOSITION_INFORMATION,
            FILE_DISPOSITION_INFORMATION_EX, FILE_DISPOSITION_POSIX_SEMANTICS,
            FILE_INFORMATION_CLASS, FILE_NON_DIRECTORY_FILE, FILE_OPEN, FILE_OPEN_REPARSE_POINT,
            FILE_RENAME_INFORMATION, FILE_RENAME_POSIX_SEMANTICS, FILE_SYNCHRONOUS_IO_NONALERT,
            FILE_WRITE_THROUGH, FileDispositionInformation, FileDispositionInformationEx,
            FileRenameInformation, FileRenameInformationEx, NTCREATEFILE_CREATE_DISPOSITION,
            NTCREATEFILE_CREATE_OPTIONS, NtCreateFile, NtSetInformationFile,
        },
    },
    Win32::{
        Foundation::{
            ERROR_NO_MORE_FILES, HANDLE, NTSTATUS, OBJ_CASE_INSENSITIVE, RtlNtStatusToDosError,
            STATUS_INVALID_INFO_CLASS, STATUS_INVALID_PARAMETER, STATUS_NOT_SUPPORTED,
            UNICODE_STRING,
        },
        Storage::FileSystem::{
            BY_HANDLE_FILE_INFORMATION, DELETE, FILE_ACCESS_RIGHTS, FILE_ATTRIBUTE_DIRECTORY,
            FILE_ATTRIBUTE_NORMAL, FILE_ATTRIBUTE_REPARSE_POINT, FILE_FLAG_BACKUP_SEMANTICS,
            FILE_FLAG_OPEN_REPARSE_POINT, FILE_FULL_DIR_INFO, FILE_GENERIC_READ,
            FILE_GENERIC_WRITE, FILE_LIST_DIRECTORY, FILE_READ_ATTRIBUTES, FILE_SHARE_DELETE,
            FILE_SHARE_MODE, FILE_SHARE_READ, FILE_SHARE_WRITE, FileFullDirectoryInfo,
            GetFileInformationByHandle, GetFileInformationByHandleEx, MOVE_FILE_FLAGS,
            MOVEFILE_REPLACE_EXISTING, MOVEFILE_WRITE_THROUGH, MoveFileExW, SYNCHRONIZE,
        },
        System::IO::IO_STATUS_BLOCK,
    },
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

/// Every handle pp-os opens shares read, write and delete, as `std` does by
/// default. Delete sharing is what lets another handle rename or remove the
/// object while this one is open, as on Unix.
const SHARE_ALL: FILE_SHARE_MODE = FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE;

pub(crate) fn nt_result(status: NTSTATUS) -> io::Result<()> {
    if status >= 0 {
        return Ok(());
    }
    // SAFETY: the conversion only reads its argument.
    let code = unsafe { RtlNtStatusToDosError(status) };
    Err(io::Error::from_raw_os_error(code as i32))
}

/// The status codes a file system returns for an information class or flag
/// it does not implement, such as POSIX semantics on FAT.
fn unsupported(status: NTSTATUS) -> bool {
    matches!(
        status,
        STATUS_INVALID_INFO_CLASS | STATUS_INVALID_PARAMETER | STATUS_NOT_SUPPORTED
    )
}

fn set_information(
    file: &File,
    information: *const std::ffi::c_void,
    length: usize,
    class: FILE_INFORMATION_CLASS,
) -> io::Result<NTSTATUS> {
    let length = u32::try_from(length)
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "information is too large"))?;
    let mut status_block = IO_STATUS_BLOCK::default();
    // SAFETY: the handle is open for the lifetime of `file`, and `information`
    // points to `length` readable bytes laid out for `class`.
    Ok(unsafe {
        NtSetInformationFile(
            file.as_raw_handle(),
            &mut status_block,
            information,
            length,
            class,
        )
    })
}

/// Run the POSIX-semantics request, and only when the file system does not
/// implement it, the classic one.
pub(crate) fn with_fallback(
    posix: impl FnOnce() -> io::Result<NTSTATUS>,
    classic: impl FnOnce() -> io::Result<NTSTATUS>,
) -> io::Result<()> {
    let status = posix()?;
    if !unsupported(status) {
        return nt_result(status);
    }
    nt_result(classic()?)
}

/// Delete the object `file` is open on. POSIX semantics remove the name at
/// once, even while other handles stay open, as `unlink` does. Without them
/// the name stays until the last handle closes and cannot be opened again.
/// The read-only attribute does not block the delete, as it does not block
/// `unlink`; `std::fs::remove_file` uses the same flags. File systems without
/// the extended class get the classic delete, which a read-only entry refuses,
/// as `DeleteFileW` does there.
fn delete_open(file: &File) -> io::Result<()> {
    with_fallback(|| delete_posix(file), || delete_classic(file))
}

fn delete_posix(file: &File) -> io::Result<NTSTATUS> {
    let posix = FILE_DISPOSITION_INFORMATION_EX {
        Flags: FILE_DISPOSITION_DELETE
            | FILE_DISPOSITION_POSIX_SEMANTICS
            | FILE_DISPOSITION_IGNORE_READONLY_ATTRIBUTE,
    };
    set_information(
        file,
        (&raw const posix).cast(),
        size_of_val(&posix),
        FileDispositionInformationEx,
    )
}

pub(crate) fn delete_classic(file: &File) -> io::Result<NTSTATUS> {
    let classic = FILE_DISPOSITION_INFORMATION { DeleteFile: true };
    set_information(
        file,
        (&raw const classic).cast(),
        size_of_val(&classic),
        FileDispositionInformation,
    )
}

/// A `FILE_RENAME_INFORMATION` naming `to` relative to `root`, without
/// replace, and its length in bytes. The zeroed union is the classic
/// `ReplaceIfExists = false`; `posix` sets `FILE_RENAME_POSIX_SEMANTICS` for
/// the extended class.
fn rename_information(root: HANDLE, to: &str, posix: bool) -> io::Result<(Vec<u64>, usize)> {
    let name: Vec<u16> = OsStr::new(to).encode_wide().collect();
    let name_bytes = name.len() * 2;
    let name_length = u32::try_from(name_bytes)
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "entry name is too long"))?;
    let length = (offset_of!(FILE_RENAME_INFORMATION, FileName) + name_bytes)
        .max(size_of::<FILE_RENAME_INFORMATION>());
    // u64 storage gives the structure its handle alignment.
    let mut buffer = vec![0u64; length.div_ceil(8)];
    let information = buffer.as_mut_ptr().cast::<FILE_RENAME_INFORMATION>();
    // SAFETY: `buffer` holds at least `length` zeroed, suitably aligned bytes:
    // the fixed fields followed by room for every unit of `name`.
    unsafe {
        if posix {
            (*information).Anonymous.Flags = FILE_RENAME_POSIX_SEMANTICS;
        }
        (*information).RootDirectory = root;
        (*information).FileNameLength = name_length;
        std::ptr::copy_nonoverlapping(
            name.as_ptr(),
            (&raw mut (*information).FileName).cast::<u16>(),
            name.len(),
        );
    }
    Ok((buffer, length))
}

fn rename_posix(source: &File, root: HANDLE, to: &str) -> io::Result<NTSTATUS> {
    let (buffer, length) = rename_information(root, to, true)?;
    set_information(
        source,
        buffer.as_ptr().cast(),
        length,
        FileRenameInformationEx,
    )
}

pub(crate) fn rename_classic(source: &File, root: HANDLE, to: &str) -> io::Result<NTSTATUS> {
    let (buffer, length) = rename_information(root, to, false)?;
    set_information(
        source,
        buffer.as_ptr().cast(),
        length,
        FileRenameInformation,
    )
}

pub(crate) struct Dir {
    pub(crate) directory: File,
    /// The lock file [`Dir::try_lock`] and [`Dir::lock`] opened, kept open
    /// until this `Dir` drops, as a Unix `flock` lasts until the descriptor
    /// closes.
    lock: OnceLock<File>,
}

impl Dir {
    /// Share delete as well as read and write, so the directory can be renamed
    /// or removed while this handle is open, as on Unix. Entry operations
    /// never use a path: they open relative to this handle, so after a rename
    /// they reach the moved directory, and replacing a junction or symbolic
    /// link used to reach it does not redirect them.
    pub(crate) fn open(path: &Path) -> io::Result<Self> {
        let directory = OpenOptions::new()
            .read(true)
            .share_mode(SHARE_ALL)
            .custom_flags(FILE_FLAG_BACKUP_SEMANTICS)
            .open(path)?;
        Ok(Self::from_file(directory))
    }

    fn from_file(directory: File) -> Self {
        Self {
            directory,
            lock: OnceLock::new(),
        }
    }

    pub(crate) fn try_clone(&self) -> io::Result<Self> {
        Ok(Self::from_file(self.directory.try_clone()?))
    }

    pub(crate) fn status(&self) -> io::Result<Status> {
        status_of(&self.directory)
    }

    /// Open the entry `name` with `NtCreateFile`, using this handle as the
    /// root directory: the Windows counterpart of `openat`. The name is
    /// matched without case, as Win32 paths are. Handles are synchronous and
    /// not inherited, as `std` opens them.
    fn open_entry(
        &self,
        name: &str,
        access: FILE_ACCESS_RIGHTS,
        disposition: NTCREATEFILE_CREATE_DISPOSITION,
        options: NTCREATEFILE_CREATE_OPTIONS,
    ) -> io::Result<File> {
        let mut wide: Vec<u16> = OsStr::new(name).encode_wide().collect();
        let bytes = u16::try_from(wide.len() * 2)
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "entry name is too long"))?;
        let object_name = UNICODE_STRING {
            Length: bytes,
            MaximumLength: bytes,
            Buffer: wide.as_mut_ptr(),
        };
        let attributes = OBJECT_ATTRIBUTES {
            Length: size_of::<OBJECT_ATTRIBUTES>() as u32,
            RootDirectory: self.directory.as_raw_handle(),
            ObjectName: &object_name,
            Attributes: OBJ_CASE_INSENSITIVE,
            SecurityDescriptor: std::ptr::null(),
            SecurityQualityOfService: std::ptr::null(),
        };
        let mut handle: HANDLE = std::ptr::null_mut();
        let mut status_block = IO_STATUS_BLOCK::default();
        // SAFETY: every pointer refers to a live local for the duration of the
        // call; `wide` backs `object_name` and is not moved.
        let status = unsafe {
            NtCreateFile(
                &mut handle,
                access | SYNCHRONIZE,
                &attributes,
                &mut status_block,
                std::ptr::null(),
                FILE_ATTRIBUTE_NORMAL,
                SHARE_ALL,
                disposition,
                options | FILE_SYNCHRONOUS_IO_NONALERT,
                std::ptr::null(),
                0,
            )
        };
        nt_result(status)?;
        // SAFETY: the call succeeded, so `handle` is a new handle this
        // process owns and nothing else will close.
        Ok(unsafe { File::from_raw_handle(handle) })
    }

    /// Attribute-only access takes no part in sharing checks, so this works
    /// on locked and pinned entries.
    pub(crate) fn status_nofollow(&self, name: &str) -> io::Result<Status> {
        status_of(&self.open_entry(
            name,
            FILE_READ_ATTRIBUTES,
            FILE_OPEN,
            FILE_OPEN_REPARSE_POINT,
        )?)
    }

    pub(crate) fn open_read_nofollow(&self, name: &str) -> io::Result<File> {
        refuse_link(self.open_entry(name, FILE_GENERIC_READ, FILE_OPEN, FILE_OPEN_REPARSE_POINT)?)
    }

    pub(crate) fn create_new_nofollow(&self, name: &str) -> io::Result<File> {
        self.create_new(name, FILE_GENERIC_READ | FILE_GENERIC_WRITE)
    }

    /// `FILE_CREATE` fails on any existing entry, including a dangling link.
    /// Should a file system report access denied instead of a name collision
    /// for a directory, report `AlreadyExists` as Unix does.
    fn create_new(&self, name: &str, access: FILE_ACCESS_RIGHTS) -> io::Result<File> {
        self.open_entry(
            name,
            access,
            FILE_CREATE,
            FILE_NON_DIRECTORY_FILE | FILE_OPEN_REPARSE_POINT,
        )
        .map_err(|error| {
            if error.kind() == io::ErrorKind::PermissionDenied && self.status_nofollow(name).is_ok()
            {
                io::Error::new(io::ErrorKind::AlreadyExists, error)
            } else {
                error
            }
        })
    }

    /// `FILE_NON_DIRECTORY_FILE` refuses a directory, which write access would
    /// otherwise open for adding entries.
    pub(crate) fn open_write_nofollow(&self, name: &str) -> io::Result<File> {
        refuse_link(self.open_entry(
            name,
            FILE_GENERIC_READ | FILE_GENERIC_WRITE,
            FILE_OPEN,
            FILE_NON_DIRECTORY_FILE | FILE_OPEN_REPARSE_POINT,
        )?)
    }

    /// Open the entry itself, a link included, and delete it. A directory or
    /// directory link is refused, as `unlinkat` without `AT_REMOVEDIR` refuses
    /// a directory.
    pub(crate) fn remove_file(&self, name: &str) -> io::Result<()> {
        delete_open(&self.open_entry(
            name,
            DELETE | FILE_READ_ATTRIBUTES,
            FILE_OPEN,
            FILE_NON_DIRECTORY_FILE | FILE_OPEN_REPARSE_POINT,
        )?)
    }
}

/// Handle-relative directory operations, the counterparts of `mkdirat`,
/// `renameat`, `unlinkat` with `AT_REMOVEDIR`, `fdopendir` and `flock`.
impl Dir {
    /// Directory handles share read, write and delete, as [`Dir::open`] does.
    fn open_directory_entry(&self, name: &str) -> io::Result<Self> {
        Ok(Self::from_file(refuse_link(self.open_entry(
            name,
            FILE_GENERIC_READ,
            FILE_OPEN,
            FILE_DIRECTORY_FILE | FILE_OPEN_REPARSE_POINT,
        )?)?))
    }

    /// Windows has no search-only handle.
    pub(crate) fn open_search(path: &Path) -> io::Result<Self> {
        Self::open(path)
    }

    pub(crate) fn open_search_nofollow(&self, name: &str) -> io::Result<Self> {
        self.open_directory_entry(name)
    }

    pub(crate) fn open_dir_nofollow(&self, name: &str) -> io::Result<Self> {
        self.open_directory_entry(name)
    }

    /// An empty name relative to this handle opens the directory itself, as
    /// `ReOpenFile` does: a new handle with its own enumeration position.
    pub(crate) fn reopen(&self) -> io::Result<Self> {
        self.open_directory_entry("")
    }

    pub(crate) fn posix_access(&self) -> io::Result<Option<PosixAccess>> {
        Ok(None)
    }

    /// Enumerate through a fresh handle, so clones that share this handle do
    /// not share a position.
    pub(crate) fn entries(&self) -> io::Result<Entries> {
        Ok(Entries {
            directory: self.reopen()?.directory,
            buffer: vec![0; ENTRIES_BUFFER / 8],
            next: None,
            done: false,
        })
    }

    pub(crate) fn open_file(&self, name: &str, how: FileOpen) -> io::Result<File> {
        match how {
            FileOpen::Read => refuse_link(self.open_entry(
                name,
                FILE_GENERIC_READ,
                FILE_OPEN,
                FILE_OPEN_REPARSE_POINT,
            )?),
            FileOpen::ReadWrite => self.open_write_nofollow(name),
            // Attribute reads let callers check the type of what they created.
            FileOpen::CreateWriteOnly => {
                self.create_new(name, FILE_GENERIC_WRITE | FILE_READ_ATTRIBUTES)
            }
        }
    }

    /// `FILE_WRITE_THROUGH` makes the creation durable before this returns;
    /// see the crate docs on durability.
    pub(crate) fn create_dir(&self, name: &str) -> io::Result<()> {
        self.create_dir_handle(name).map(drop)
    }

    pub(crate) fn create_dir_handle(&self, name: &str) -> io::Result<File> {
        self.open_entry(
            name,
            FILE_LIST_DIRECTORY | FILE_READ_ATTRIBUTES,
            FILE_CREATE,
            FILE_DIRECTORY_FILE | FILE_WRITE_THROUGH,
        )
    }

    /// Rename the entry `from`, a link itself rather than its target, to `to`
    /// in this directory, failing with `AlreadyExists` when `to` exists. The
    /// source is opened with `FILE_WRITE_THROUGH`, so the new name is durable
    /// before this returns.
    pub(crate) fn rename_no_replace(&self, from: &str, to: &str) -> io::Result<()> {
        let source = self.rename_source(from)?;
        let root = self.directory.as_raw_handle();
        with_fallback(
            || rename_posix(&source, root, to),
            || rename_classic(&source, root, to),
        )
    }

    pub(crate) fn rename_source(&self, from: &str) -> io::Result<File> {
        self.open_entry(
            from,
            DELETE | FILE_READ_ATTRIBUTES,
            FILE_OPEN,
            FILE_OPEN_REPARSE_POINT | FILE_WRITE_THROUGH,
        )
    }

    /// Remove the empty directory `name`. A junction or directory symbolic
    /// link is refused, as `unlinkat` with `AT_REMOVEDIR` refuses a link.
    pub(crate) fn remove_dir(&self, name: &str) -> io::Result<()> {
        let directory = self.open_entry(
            name,
            DELETE | FILE_READ_ATTRIBUTES,
            FILE_OPEN,
            FILE_DIRECTORY_FILE | FILE_OPEN_REPARSE_POINT,
        )?;
        if status_of(&directory)?.kind != Kind::Directory {
            return Err(io::Error::from(io::ErrorKind::NotADirectory));
        }
        delete_open(&directory)
    }

    /// A read handle cannot flush a directory. [`Dir::create_dir`] and
    /// [`Dir::rename_no_replace`] write through instead.
    pub(crate) fn sync(&self) -> io::Result<()> {
        Ok(())
    }

    /// Open or create the lock file once and keep it for this `Dir`.
    fn lock_file(&self, name: &str) -> io::Result<&File> {
        if let Some(file) = self.lock.get() {
            return Ok(file);
        }
        let file = match self.open_write_nofollow(name) {
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                match self.create_new_nofollow(name) {
                    Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
                        self.open_write_nofollow(name)
                    }
                    created => created,
                }
            }
            opened => opened,
        }?;
        Ok(self.lock.get_or_init(|| file))
    }

    pub(crate) fn try_lock(&self, lock_file: &str) -> io::Result<()> {
        match crate::try_lock(self.lock_file(lock_file)?) {
            Ok(()) => Ok(()),
            Err(crate::TryLockError::WouldBlock) => Err(io::Error::new(
                io::ErrorKind::WouldBlock,
                "another handle holds the directory lock",
            )),
            Err(crate::TryLockError::Io(error)) => Err(error),
        }
    }

    pub(crate) fn lock(&self, lock_file: &str) -> io::Result<()> {
        fs4::FileExt::lock(self.lock_file(lock_file)?)
    }

    /// Unix `flock(LOCK_UN)` succeeds without a lock; so does this.
    pub(crate) fn unlock(&self) -> io::Result<()> {
        match self.lock.get() {
            Some(file) => crate::unlock(file),
            None => Ok(()),
        }
    }
}

/// Room for many entries per call; one entry needs at most about 600 bytes.
const ENTRIES_BUFFER: usize = 64 * 1024;

/// Reads `FILE_FULL_DIR_INFO` records with `GetFileInformationByHandleEx`,
/// one buffer at a time.
pub(crate) struct Entries {
    directory: File,
    buffer: Vec<u64>,
    /// Byte offset of the next record in `buffer`, if any remain.
    next: Option<usize>,
    done: bool,
}

impl Entries {
    fn fill(&mut self) -> io::Result<bool> {
        let bytes = u32::try_from(self.buffer.len() * 8).expect("buffer fits u32");
        // SAFETY: the handle is open and synchronous, and `buffer` has
        // `bytes` writable bytes with the alignment the records need.
        let filled = unsafe {
            GetFileInformationByHandleEx(
                self.directory.as_raw_handle(),
                FileFullDirectoryInfo,
                self.buffer.as_mut_ptr().cast(),
                bytes,
            )
        };
        if filled != 0 {
            self.next = Some(0);
            return Ok(true);
        }
        let error = io::Error::last_os_error();
        if error.raw_os_error() == Some(ERROR_NO_MORE_FILES as i32) {
            return Ok(false);
        }
        Err(error)
    }

    fn record(&mut self, offset: usize) -> OsString {
        let base = self.buffer.as_ptr().cast::<u8>();
        // SAFETY: the system wrote a record at `offset` inside the buffer,
        // 8-byte aligned, followed by `FileNameLength` bytes of UTF-16.
        unsafe {
            let record = base.add(offset).cast::<FILE_FULL_DIR_INFO>();
            let next = (*record).NextEntryOffset as usize;
            self.next = (next != 0).then_some(offset + next);
            let units = (*record).FileNameLength as usize / 2;
            let name = (&raw const (*record).FileName).cast::<u16>();
            OsString::from_wide(std::slice::from_raw_parts(name, units))
        }
    }
}

impl Iterator for Entries {
    type Item = io::Result<OsString>;

    fn next(&mut self) -> Option<Self::Item> {
        loop {
            if let Some(offset) = self.next {
                let name = self.record(offset);
                if name != "." && name != ".." {
                    return Some(Ok(name));
                }
                continue;
            }
            if self.done {
                return None;
            }
            match self.fill() {
                Ok(true) => {}
                Ok(false) => {
                    self.done = true;
                    return None;
                }
                Err(error) => {
                    self.done = true;
                    return Some(Err(error));
                }
            }
        }
    }
}
