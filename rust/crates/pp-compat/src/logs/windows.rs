//! Windows log files. The admitted directory is held open without
//! `FILE_SHARE_DELETE`, so it cannot be renamed, replaced or removed while the
//! logs are open, and entry paths under it keep resolving inside it. Every
//! open refuses a reparse point (symbolic link or junction) as the final
//! component. Access control is a protected DACL that grants only the current
//! user, the equivalent of modes 0600 and 0700.

use crate::windows_security::{CurrentUser, SE_FILE_OBJECT};
use std::{
    fs::{File, Metadata},
    os::windows::{
        ffi::OsStrExt,
        io::{AsRawHandle, FromRawHandle},
    },
    path::{Path, PathBuf},
};
use windows_sys::Win32::{
    Foundation::{GENERIC_READ, HANDLE, INVALID_HANDLE_VALUE},
    Storage::FileSystem::{
        BY_HANDLE_FILE_INFORMATION, CREATE_NEW, CreateFileW, FILE_APPEND_DATA,
        FILE_ATTRIBUTE_DIRECTORY, FILE_ATTRIBUTE_REPARSE_POINT, FILE_FLAG_BACKUP_SEMANTICS,
        FILE_FLAG_OPEN_REPARSE_POINT, FILE_LIST_DIRECTORY, FILE_READ_ATTRIBUTES, FILE_SHARE_DELETE,
        FILE_SHARE_READ, FILE_SHARE_WRITE, FILE_STANDARD_INFO, FileStandardInfo,
        GetFileInformationByHandle, GetFileInformationByHandleEx, MOVEFILE_REPLACE_EXISTING,
        MOVEFILE_WRITE_THROUGH, MoveFileExW, OPEN_EXISTING, READ_CONTROL, SYNCHRONIZE, WRITE_DAC,
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
    let handle = unsafe {
        CreateFileW(
            wide(path).as_ptr(),
            access,
            share,
            std::ptr::null(),
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
    user.restrict(file, SE_FILE_OBJECT, false)
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
    user.restrict(&directory.handle, SE_FILE_OBJECT, true)
}

pub(super) fn secure_archive(directory: &Directory, name: &str) -> std::io::Result<()> {
    match open(
        &directory.path.join(name),
        READ_CONTROL | WRITE_DAC | FILE_READ_ATTRIBUTES,
        FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
        OPEN_EXISTING,
        0,
    ) {
        Ok(file) => secure_log(&file),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
    }
}

/// `create` fails with `AlreadyExists` when any entry has the name,
/// including a dangling link. The handle shares delete so rotation can rename
/// the open active log.
pub(super) fn open_append(
    directory: &Directory,
    name: &str,
    create: bool,
) -> std::io::Result<File> {
    open(
        &directory.path.join(name),
        FILE_APPEND_DATA | READ_CONTROL | WRITE_DAC | FILE_READ_ATTRIBUTES | SYNCHRONIZE,
        FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
        if create { CREATE_NEW } else { OPEN_EXISTING },
        0,
    )
}

/// The open log no longer has a name: deleted with POSIX semantics, or
/// waiting for its last handle under the classic delete.
pub(super) fn unlinked(file: &File) -> bool {
    let mut information = FILE_STANDARD_INFO::default();
    let read = unsafe {
        GetFileInformationByHandleEx(
            file.as_raw_handle() as HANDLE,
            FileStandardInfo,
            (&mut information as *mut FILE_STANDARD_INFO).cast(),
            size_of::<FILE_STANDARD_INFO>() as u32,
        )
    };
    read != 0 && (information.DeletePending || information.NumberOfLinks == 0)
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
    use std::{fs::File, path::Path};
    use windows_sys::Win32::{
        Security::{
            Authorization::{
                ConvertStringSecurityDescriptorToSecurityDescriptorW, SDDL_REVISION_1,
                SetSecurityInfo,
            },
            DACL_SECURITY_INFORMATION, GetSecurityDescriptorDacl,
            PROTECTED_DACL_SECURITY_INFORMATION, UNPROTECTED_DACL_SECURITY_INFORMATION,
        },
        Storage::FileSystem::{
            FILE_FLAG_BACKUP_SEMANTICS, FILE_READ_ATTRIBUTES, FILE_SHARE_DELETE, FILE_SHARE_READ,
            FILE_SHARE_WRITE, OPEN_EXISTING, READ_CONTROL, WRITE_DAC,
        },
    };

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
            windows_sys::Win32::Foundation::LocalFree(descriptor as _);
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
            .restrict(&handle(path), SE_FILE_OBJECT, path.is_dir())
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
