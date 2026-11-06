//! Windows log files. The admitted directory is held open without
//! `FILE_SHARE_DELETE`, so it cannot be renamed, replaced or removed while the
//! logs are open, and entry paths under it keep resolving inside it. Every
//! open refuses a reparse point (symbolic link or junction) as the final
//! component. Access control is a protected DACL that grants only the current
//! user, the equivalent of modes 0600 and 0700.

use crate::windows_security::{CurrentUser, PrivateDescriptor, SE_FILE_OBJECT, SavedDacl};
use std::{
    fs::{File, Metadata},
    os::windows::{
        ffi::OsStrExt,
        io::{AsRawHandle, FromRawHandle},
    },
    path::{Path, PathBuf},
};
use windows_sys::{
    Wdk::{
        Foundation::OBJECT_ATTRIBUTES,
        Storage::FileSystem::{FILE_NON_DIRECTORY_FILE, FILE_OPEN_REPARSE_POINT, NtOpenFile},
    },
    Win32::{
        Foundation::{
            GENERIC_READ, HANDLE, INVALID_HANDLE_VALUE, RtlNtStatusToDosError, UNICODE_STRING,
        },
        Security::SECURITY_ATTRIBUTES,
        Storage::FileSystem::{
            BY_HANDLE_FILE_INFORMATION, CREATE_NEW, CreateFileW, FILE_APPEND_DATA,
            FILE_ATTRIBUTE_DIRECTORY, FILE_ATTRIBUTE_REPARSE_POINT, FILE_FLAG_BACKUP_SEMANTICS,
            FILE_FLAG_OPEN_REPARSE_POINT, FILE_LIST_DIRECTORY, FILE_READ_ATTRIBUTES,
            FILE_SHARE_DELETE, FILE_SHARE_READ, FILE_SHARE_WRITE, FILE_STANDARD_INFO,
            FileStandardInfo, GetFileInformationByHandle, GetFileInformationByHandleEx,
            MOVEFILE_REPLACE_EXISTING, MOVEFILE_WRITE_THROUGH, MoveFileExW, OPEN_EXISTING,
            READ_CONTROL, ReOpenFile, SYNCHRONIZE, WRITE_DAC,
        },
        System::IO::IO_STATUS_BLOCK,
    },
};

pub(super) struct Directory {
    handle: File,
    path: PathBuf,
}

fn wide(path: &Path) -> Vec<u16> {
    path.as_os_str().encode_wide().chain([0]).collect()
}

fn invalid() -> std::io::Error {
    std::io::Error::from(std::io::ErrorKind::InvalidInput)
}

fn open(
    path: &Path,
    access: u32,
    share: u32,
    disposition: u32,
    flags: u32,
) -> std::io::Result<File> {
    open_with(path, access, share, disposition, flags, None)
}

/// `descriptor` is the security of a file this call creates.
fn open_with(
    path: &Path,
    access: u32,
    share: u32,
    disposition: u32,
    flags: u32,
    descriptor: Option<&PrivateDescriptor>,
) -> std::io::Result<File> {
    let attributes = descriptor.map(|descriptor| SECURITY_ATTRIBUTES {
        nLength: size_of::<SECURITY_ATTRIBUTES>() as u32,
        lpSecurityDescriptor: descriptor.as_ptr(),
        bInheritHandle: 0,
    });
    let handle = unsafe {
        CreateFileW(
            wide(path).as_ptr(),
            access,
            share,
            attributes
                .as_ref()
                .map_or(std::ptr::null(), |attributes| attributes as *const _),
            disposition,
            FILE_FLAG_OPEN_REPARSE_POINT | flags,
            std::ptr::null_mut(),
        )
    };
    if handle == INVALID_HANDLE_VALUE {
        return Err(std::io::Error::last_os_error());
    }
    Ok(unsafe { File::from_raw_handle(handle as _) })
}

fn information(file: &File) -> std::io::Result<BY_HANDLE_FILE_INFORMATION> {
    let mut information = BY_HANDLE_FILE_INFORMATION::default();
    if unsafe { GetFileInformationByHandle(file.as_raw_handle() as HANDLE, &mut information) } == 0
    {
        return Err(std::io::Error::last_os_error());
    }
    Ok(information)
}

fn is_reparse(information: &BY_HANDLE_FILE_INFORMATION) -> bool {
    information.dwFileAttributes & FILE_ATTRIBUTE_REPARSE_POINT != 0
}

fn is_directory(information: &BY_HANDLE_FILE_INFORMATION) -> bool {
    information.dwFileAttributes & FILE_ATTRIBUTE_DIRECTORY != 0
}

/// Owned by this user with a DACL that grants only this user, as the Reveal
/// log folder command requires.
pub(super) fn private_folder(directory: &Path, _metadata: &Metadata) -> std::io::Result<bool> {
    let handle = open(
        directory,
        READ_CONTROL | FILE_READ_ATTRIBUTES,
        FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
        OPEN_EXISTING,
        FILE_FLAG_BACKUP_SEMANTICS,
    )?;
    let information = information(&handle)?;
    if is_reparse(&information) || !is_directory(&information) {
        return Ok(false);
    }
    CurrentUser::query()?.owns_privately(&handle, SE_FILE_OBJECT)
}

pub(super) fn open_tail(path: &Path) -> std::io::Result<File> {
    let file = open(
        path,
        GENERIC_READ,
        FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
        OPEN_EXISTING,
        0,
    )?;
    if is_reparse(&information(&file)?) {
        return Err(invalid());
    }
    Ok(file)
}

pub(super) fn open_directory(directory: &Path) -> std::io::Result<Directory> {
    std::fs::create_dir_all(directory)?;
    let handle = open(
        directory,
        READ_CONTROL | WRITE_DAC | FILE_READ_ATTRIBUTES | FILE_LIST_DIRECTORY | SYNCHRONIZE,
        FILE_SHARE_READ | FILE_SHARE_WRITE,
        OPEN_EXISTING,
        FILE_FLAG_BACKUP_SEMANTICS,
    )?;
    let information = information(&handle)?;
    if is_reparse(&information) || !is_directory(&information) {
        return Err(invalid());
    }
    Ok(Directory {
        handle,
        path: directory.to_path_buf(),
    })
}

pub(super) fn secure_log(file: &File) -> std::io::Result<()> {
    let information = information(file)?;
    if is_reparse(&information)
        || is_directory(&information)
        || information.nNumberOfLinks != 1
        || unlinked(file)
    {
        return Err(invalid());
    }
    let user = CurrentUser::query()?;
    if !user.owns(file, SE_FILE_OBJECT)? {
        return Err(invalid());
    }
    user.restrict(file, false)
}

pub(super) fn secure_directory(directory: &Directory) -> std::io::Result<()> {
    let information = information(&directory.handle)?;
    if is_reparse(&information) || !is_directory(&information) {
        return Err(invalid());
    }
    let user = CurrentUser::query()?;
    if !user.owns(&directory.handle, SE_FILE_OBJECT)? {
        return Err(invalid());
    }
    user.restrict(&directory.handle, true)
}

pub(super) fn secure_archive(directory: &Directory, name: &str) -> std::io::Result<()> {
    let path = directory.path.join(name);
    let share = FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE;
    match open(
        &path,
        READ_CONTROL | WRITE_DAC | FILE_READ_ATTRIBUTES,
        share,
        OPEN_EXISTING,
        0,
    ) {
        Ok(file) => secure_log(&file),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::PermissionDenied => {
            repair_archive(directory, name)
        }
        Err(error) => Err(error),
    }
}

/// An archive whose DACL denies this user even attribute reads. Its owner
/// still holds `READ_CONTROL` and `WRITE_DAC`, so it gets the private DACL
/// and is checked as every log is, through a handle reopened from the same
/// object rather than by name, so a rename cannot swap in another file. A
/// failed check puts the exact old DACL back. A hard link, which could be a
/// file outside the directory, is refused before anything changes.
///
/// `CreateFileW` always adds `SYNCHRONIZE` and `FILE_READ_ATTRIBUTES`, which
/// the owner does not hold here, so the first open is `NtOpenFile` relative to
/// the admitted directory handle. It opens a reparse point as itself, which
/// `secure_log` then refuses.
fn repair_archive(directory: &Directory, name: &str) -> std::io::Result<()> {
    let mut units: Vec<u16> = name.encode_utf16().collect();
    let bytes = u16::try_from(units.len() * 2).map_err(|_| invalid())?;
    let object_name = UNICODE_STRING {
        Length: bytes,
        MaximumLength: bytes,
        Buffer: units.as_mut_ptr(),
    };
    let attributes = OBJECT_ATTRIBUTES {
        Length: size_of::<OBJECT_ATTRIBUTES>() as u32,
        RootDirectory: directory.handle.as_raw_handle() as HANDLE,
        ObjectName: &object_name,
        ..Default::default()
    };
    let share = FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE;
    let mut handle: HANDLE = std::ptr::null_mut();
    let mut status_block = IO_STATUS_BLOCK::default();
    let status = unsafe {
        NtOpenFile(
            &mut handle,
            READ_CONTROL | WRITE_DAC,
            &attributes,
            &mut status_block,
            share,
            FILE_OPEN_REPARSE_POINT | FILE_NON_DIRECTORY_FILE,
        )
    };
    if status < 0 {
        return Err(std::io::Error::from_raw_os_error(
            unsafe { RtlNtStatusToDosError(status) } as i32,
        ));
    }
    let file = unsafe { File::from_raw_handle(handle as _) };
    let user = CurrentUser::query()?;
    if !user.owns(&file, SE_FILE_OBJECT)? {
        return Err(invalid());
    }
    let links = standard(&file)?;
    if links.NumberOfLinks != 1 || links.DeletePending || links.Directory {
        return Err(invalid());
    }
    let saved = SavedDacl::of(&file, SE_FILE_OBJECT)?;
    user.restrict(&file, false)?;
    let checked = reopen(&file).and_then(|reopened| secure_log(&reopened));
    if checked.is_err() {
        saved.restore(&file)?;
    }
    checked
}

/// A new handle to the object `file` names, access-checked against its DACL now.
fn reopen(file: &File) -> std::io::Result<File> {
    let handle = unsafe {
        ReOpenFile(
            file.as_raw_handle() as HANDLE,
            READ_CONTROL | WRITE_DAC | FILE_READ_ATTRIBUTES,
            FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
            FILE_FLAG_OPEN_REPARSE_POINT,
        )
    };
    if handle == INVALID_HANDLE_VALUE {
        return Err(std::io::Error::last_os_error());
    }
    Ok(unsafe { File::from_raw_handle(handle as _) })
}

fn standard(file: &File) -> std::io::Result<FILE_STANDARD_INFO> {
    let mut information = FILE_STANDARD_INFO::default();
    if unsafe {
        GetFileInformationByHandleEx(
            file.as_raw_handle() as HANDLE,
            FileStandardInfo,
            (&mut information as *mut FILE_STANDARD_INFO).cast(),
            size_of::<FILE_STANDARD_INFO>() as u32,
        )
    } == 0
    {
        return Err(std::io::Error::last_os_error());
    }
    Ok(information)
}

/// `create` fails with `AlreadyExists` when any entry has the name,
/// including a dangling link. The handle shares delete so rotation can rename
/// the open active log.
pub(super) fn open_append(
    directory: &Directory,
    name: &str,
    create: bool,
) -> std::io::Result<File> {
    let descriptor = if create {
        Some(CurrentUser::query()?.private_descriptor(false)?)
    } else {
        None
    };
    open_with(
        &directory.path.join(name),
        FILE_APPEND_DATA | READ_CONTROL | WRITE_DAC | FILE_READ_ATTRIBUTES | SYNCHRONIZE,
        FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
        if create { CREATE_NEW } else { OPEN_EXISTING },
        0,
        descriptor.as_ref(),
    )
}

/// The open log no longer has a name: deleted with POSIX semantics, or
/// waiting for its last handle under the classic delete.
pub(super) fn unlinked(file: &File) -> bool {
    standard(file)
        .is_ok_and(|information| information.DeletePending || information.NumberOfLinks == 0)
}

/// Rename within the admitted directory, replacing `to`. The source is opened
/// as a link, so a reparse point moves as itself.
pub(super) fn rename(directory: &Directory, from: &str, to: &str) -> std::io::Result<()> {
    let (from, to) = (
        wide(&directory.path.join(from)),
        wide(&directory.path.join(to)),
    );
    if unsafe {
        MoveFileExW(
            from.as_ptr(),
            to.as_ptr(),
            MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH,
        )
    } == 0
    {
        return Err(std::io::Error::last_os_error());
    }
    Ok(())
}

#[cfg(test)]
pub(super) mod test_support {
    use super::{CurrentUser, SE_FILE_OBJECT, information, open};
    use std::{
        fs::File,
        os::windows::{fs::OpenOptionsExt, io::AsRawHandle},
        path::Path,
    };
    use windows_sys::Win32::{
        Foundation::{DELETE, LocalFree},
        Security::{
            Authorization::{
                ConvertSecurityDescriptorToStringSecurityDescriptorW,
                ConvertStringSecurityDescriptorToSecurityDescriptorW, GetSecurityInfo,
                SDDL_REVISION_1, SetSecurityInfo,
            },
            DACL_SECURITY_INFORMATION, GetSecurityDescriptorDacl,
            PROTECTED_DACL_SECURITY_INFORMATION, UNPROTECTED_DACL_SECURITY_INFORMATION,
        },
        Storage::FileSystem::{
            FILE_DISPOSITION_INFO, FILE_FLAG_BACKUP_SEMANTICS, FILE_READ_ATTRIBUTES,
            FILE_SHARE_DELETE, FILE_SHARE_READ, FILE_SHARE_WRITE, FileDispositionInfo,
            OPEN_EXISTING, READ_CONTROL, SetFileInformationByHandle, WRITE_DAC,
        },
    };

    /// The DACL of `path` in SDDL, with its protection and inheritance flags.
    pub(crate) fn access(path: &Path) -> String {
        let file = handle(path);
        let mut descriptor = std::ptr::null_mut();
        let mut text = std::ptr::null_mut();
        let mut length = 0;
        unsafe {
            assert_eq!(
                GetSecurityInfo(
                    file.as_raw_handle() as _,
                    SE_FILE_OBJECT,
                    DACL_SECURITY_INFORMATION,
                    std::ptr::null_mut(),
                    std::ptr::null_mut(),
                    std::ptr::null_mut(),
                    std::ptr::null_mut(),
                    &mut descriptor,
                ),
                0
            );
            assert_ne!(
                ConvertSecurityDescriptorToStringSecurityDescriptorW(
                    descriptor,
                    SDDL_REVISION_1,
                    DACL_SECURITY_INFORMATION,
                    &mut text,
                    &mut length,
                ),
                0
            );
            let sddl = String::from_utf16_lossy(std::slice::from_raw_parts(text, length as usize));
            LocalFree(text as _);
            LocalFree(descriptor as _);
            sddl.trim_end_matches('\0').to_owned()
        }
    }

    /// Delete `path` with the classic disposition: its name stays taken until
    /// the last open handle closes. `DeleteFileW` uses POSIX semantics on
    /// current NTFS, which frees the name at once.
    pub(crate) fn delete_pending(path: &Path) {
        let file = std::fs::OpenOptions::new()
            .access_mode(DELETE)
            .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE)
            .custom_flags(windows_sys::Win32::Storage::FileSystem::FILE_FLAG_OPEN_REPARSE_POINT)
            .open(path)
            .unwrap();
        let disposition = FILE_DISPOSITION_INFO { DeleteFile: true };
        assert_ne!(
            unsafe {
                SetFileInformationByHandle(
                    file.as_raw_handle() as _,
                    FileDispositionInfo,
                    (&disposition as *const FILE_DISPOSITION_INFO).cast(),
                    size_of::<FILE_DISPOSITION_INFO>() as u32,
                )
            },
            0
        );
    }

    fn handle(path: &Path) -> File {
        open(
            path,
            READ_CONTROL | WRITE_DAC | FILE_READ_ATTRIBUTES,
            FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
            OPEN_EXISTING,
            FILE_FLAG_BACKUP_SEMANTICS,
        )
        .unwrap()
    }

    pub(crate) fn identity(file: &File) -> (u64, u64) {
        let information = information(file).unwrap();
        (
            information.dwVolumeSerialNumber.into(),
            (u64::from(information.nFileIndexHigh) << 32) | u64::from(information.nFileIndexLow),
        )
    }

    pub(crate) fn path_identity(path: &Path) -> (u64, u64) {
        identity(&handle(path))
    }

    /// Only this user can open `path`.
    pub(crate) fn private(path: &Path) -> bool {
        CurrentUser::query()
            .unwrap()
            .owns_privately(&handle(path), SE_FILE_OBJECT)
            .unwrap()
    }

    fn set_dacl(path: &Path, sddl: &str, protected: bool) {
        let sddl: Vec<u16> = sddl.encode_utf16().chain([0]).collect();
        let mut descriptor = std::ptr::null_mut();
        let (mut present, mut defaulted, mut dacl) = (0, 0, std::ptr::null_mut());
        unsafe {
            assert_ne!(
                ConvertStringSecurityDescriptorToSecurityDescriptorW(
                    sddl.as_ptr(),
                    SDDL_REVISION_1,
                    &mut descriptor,
                    std::ptr::null_mut(),
                ),
                0
            );
            assert_ne!(
                GetSecurityDescriptorDacl(descriptor, &mut present, &mut dacl, &mut defaulted),
                0
            );
            let flags = DACL_SECURITY_INFORMATION
                | if protected {
                    PROTECTED_DACL_SECURITY_INFORMATION
                } else {
                    UNPROTECTED_DACL_SECURITY_INFORMATION
                };
            let file = handle(path);
            assert_eq!(
                SetSecurityInfo(
                    std::os::windows::io::AsRawHandle::as_raw_handle(&file) as _,
                    SE_FILE_OBJECT,
                    flags,
                    std::ptr::null_mut(),
                    std::ptr::null_mut(),
                    dacl,
                    std::ptr::null(),
                ),
                0
            );
            LocalFree(descriptor as _);
        }
    }

    /// Give every account read access, besides what `path` inherits.
    pub(crate) fn widen(path: &Path) {
        set_dacl(path, "D:(A;;FR;;;WD)", false);
    }

    /// Grant no account any access. The owner keeps its implicit
    /// `READ_CONTROL` and `WRITE_DAC`.
    pub(crate) fn deny_all(path: &Path) {
        set_dacl(path, "D:P", true);
    }

    /// Only this user can open `path`.
    pub(crate) fn restrict(path: &Path) {
        CurrentUser::query()
            .unwrap()
            .restrict(&handle(path), path.is_dir())
            .unwrap();
    }

    /// The access `widen` gave is still in place.
    pub(crate) fn widened(path: &Path) -> bool {
        !private(path)
    }

    pub(crate) fn symlink_file(target: &Path, link: &Path) {
        std::os::windows::fs::symlink_file(target, link).unwrap();
    }

    pub(crate) fn symlink_dir(target: &Path, link: &Path) {
        std::os::windows::fs::symlink_dir(target, link).unwrap();
    }
}
