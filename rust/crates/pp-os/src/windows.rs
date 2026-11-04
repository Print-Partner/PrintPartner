use crate::{Identity, Kind, Status};
use std::{
    ffi::OsStr,
    fs::{File, OpenOptions},
    io,
    mem::offset_of,
    os::windows::{
        ffi::OsStrExt,
        fs::OpenOptionsExt,
        io::{AsRawHandle, FromRawHandle},
    },
    path::Path,
};
use windows_sys::{
    Wdk::{
        Foundation::OBJECT_ATTRIBUTES,
        Storage::FileSystem::{
            FILE_CREATE, FILE_DIRECTORY_FILE, FILE_DISPOSITION_DELETE,
            FILE_DISPOSITION_INFORMATION, FILE_DISPOSITION_INFORMATION_EX,
            FILE_DISPOSITION_POSIX_SEMANTICS, FILE_INFORMATION_CLASS, FILE_NON_DIRECTORY_FILE,
            FILE_OPEN, FILE_OPEN_REPARSE_POINT, FILE_RENAME_INFORMATION,
            FILE_RENAME_POSIX_SEMANTICS, FILE_RENAME_REPLACE_IF_EXISTS,
            FILE_SYNCHRONOUS_IO_NONALERT, FileDispositionInformation, FileDispositionInformationEx,
            FileRenameInformation, FileRenameInformationEx, NTCREATEFILE_CREATE_DISPOSITION,
            NTCREATEFILE_CREATE_OPTIONS, NtCreateFile, NtSetInformationFile,
        },
    },
    Win32::{
        Foundation::{
            HANDLE, NTSTATUS, OBJ_CASE_INSENSITIVE, RtlNtStatusToDosError,
            STATUS_INVALID_INFO_CLASS, STATUS_INVALID_PARAMETER, STATUS_NOT_SUPPORTED,
            UNICODE_STRING,
        },
        Storage::FileSystem::{
            BY_HANDLE_FILE_INFORMATION, DELETE, FILE_ACCESS_RIGHTS, FILE_ATTRIBUTE_DIRECTORY,
            FILE_ATTRIBUTE_NORMAL, FILE_ATTRIBUTE_REPARSE_POINT, FILE_FLAG_BACKUP_SEMANTICS,
            FILE_FLAG_OPEN_REPARSE_POINT, FILE_GENERIC_READ, FILE_GENERIC_WRITE,
            FILE_LIST_DIRECTORY, FILE_READ_ATTRIBUTES, FILE_SHARE_DELETE, FILE_SHARE_MODE,
            FILE_SHARE_READ, FILE_SHARE_WRITE, GetFileInformationByHandle, MOVE_FILE_FLAGS,
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

fn nt_result(status: NTSTATUS) -> io::Result<()> {
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

/// Delete the object `file` is open on. POSIX semantics remove the name at
/// once, even while other handles stay open, as `unlink` does. Without them
/// the name stays until the last handle closes and cannot be opened again.
fn delete_open(file: &File) -> io::Result<()> {
    let posix = FILE_DISPOSITION_INFORMATION_EX {
        Flags: FILE_DISPOSITION_DELETE | FILE_DISPOSITION_POSIX_SEMANTICS,
    };
    let status = set_information(
        file,
        (&raw const posix).cast(),
        size_of_val(&posix),
        FileDispositionInformationEx,
    )?;
    if !unsupported(status) {
        return nt_result(status);
    }
    let classic = FILE_DISPOSITION_INFORMATION { DeleteFile: true };
    nt_result(set_information(
        file,
        (&raw const classic).cast(),
        size_of_val(&classic),
        FileDispositionInformation,
    )?)
}

pub(crate) struct Dir {
    directory: File,
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

    /// `FILE_CREATE` fails on any existing entry, including a dangling link.
    /// Should a file system report access denied instead of a name collision
    /// for a directory, report `AlreadyExists` as Unix does.
    pub(crate) fn create_new_nofollow(&self, name: &str) -> io::Result<File> {
        self.open_entry(
            name,
            FILE_GENERIC_READ | FILE_GENERIC_WRITE,
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

/// Handle-relative directory creation, rename and directory removal. The
/// public [`crate::Dir`] gains these, with their Unix adapters (`mkdirat`,
/// `renameat` and `unlinkat` with `AT_REMOVEDIR`), when their first consumer
/// moves onto pp-os; until then only the Windows tests call them.
#[cfg_attr(
    not(test),
    expect(dead_code, reason = "no consumer until pp-source moves to pp-os")
)]
impl Dir {
    pub(crate) fn create_dir(&self, name: &str) -> io::Result<()> {
        self.open_entry(
            name,
            FILE_LIST_DIRECTORY | FILE_READ_ATTRIBUTES,
            FILE_CREATE,
            FILE_DIRECTORY_FILE,
        )
        .map(drop)
    }

    /// Rename the entry `from`, a link itself rather than its target, to `to`
    /// in this directory. With `replace` an existing file at `to` is replaced;
    /// without it the rename fails with `AlreadyExists`.
    pub(crate) fn rename(&self, from: &str, to: &str, replace: bool) -> io::Result<()> {
        let source = self.open_entry(
            from,
            DELETE | FILE_READ_ATTRIBUTES,
            FILE_OPEN,
            FILE_OPEN_REPARSE_POINT,
        )?;
        let name: Vec<u16> = OsStr::new(to).encode_wide().collect();
        let name_bytes = name.len() * 2;
        let length = (offset_of!(FILE_RENAME_INFORMATION, FileName) + name_bytes)
            .max(size_of::<FILE_RENAME_INFORMATION>());
        // u64 storage gives the structure its handle alignment.
        let mut buffer = vec![0u64; length.div_ceil(8)];
        let information = buffer.as_mut_ptr().cast::<FILE_RENAME_INFORMATION>();
        let flags = FILE_RENAME_POSIX_SEMANTICS
            | if replace {
                FILE_RENAME_REPLACE_IF_EXISTS
            } else {
                0
            };
        // SAFETY: `buffer` holds at least `length` zeroed, suitably aligned
        // bytes: the fixed fields followed by room for every unit of `name`.
        unsafe {
            (*information).Anonymous.Flags = flags;
            (*information).RootDirectory = self.directory.as_raw_handle();
            (*information).FileNameLength = u32::try_from(name_bytes).map_err(|_| {
                io::Error::new(io::ErrorKind::InvalidInput, "entry name is too long")
            })?;
            std::ptr::copy_nonoverlapping(
                name.as_ptr(),
                (&raw mut (*information).FileName).cast::<u16>(),
                name.len(),
            );
        }
        let status = set_information(
            &source,
            information.cast_const().cast(),
            length,
            FileRenameInformationEx,
        )?;
        if !unsupported(status) {
            return nt_result(status);
        }
        // SAFETY: as above; the union is rewritten as the classic Boolean.
        unsafe {
            (*information).Anonymous.Flags = 0;
            (*information).Anonymous.ReplaceIfExists = replace;
        }
        nt_result(set_information(
            &source,
            information.cast_const().cast(),
            length,
            FileRenameInformation,
        )?)
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
}
