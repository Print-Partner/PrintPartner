//! Current-user access control on Windows: the equivalent of mode 0600 and
//! 0700 for log files and directories, and the DACL of the private pipe.

use std::{ffi::c_void, io, os::windows::io::AsRawHandle, ptr};
use windows_sys::Win32::{
    Foundation::{CloseHandle, HANDLE, HLOCAL, LocalFree},
    Security::{
        ACCESS_ALLOWED_ACE, ACE_HEADER, ACL, ACL_SIZE_INFORMATION, AclSizeInformation,
        Authorization::{
            ConvertSidToStringSidW, ConvertStringSecurityDescriptorToSecurityDescriptorW,
            GetSecurityInfo, SDDL_REVISION_1, SE_OBJECT_TYPE,
        },
        CreateWellKnownSid, DACL_SECURITY_INFORMATION, EqualSid, GetAce, GetAclInformation,
        GetTokenInformation, INHERIT_ONLY_ACE, OWNER_SECURITY_INFORMATION, PSECURITY_DESCRIPTOR,
        PSID, SECURITY_MAX_SID_SIZE, SetKernelObjectSecurity, TOKEN_OWNER, TOKEN_QUERY, TOKEN_USER,
        TokenOwner, TokenUser, WinBuiltinAdministratorsSid, WinLocalSystemSid,
    },
    System::{
        SystemServices::{ACCESS_ALLOWED_ACE_TYPE, ACCESS_DENIED_ACE_TYPE},
        Threading::{GetCurrentProcess, OpenProcessToken},
    },
};

pub(crate) use windows_sys::Win32::Security::Authorization::SE_FILE_OBJECT;

fn check(ok: i32) -> io::Result<()> {
    if ok == 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(())
    }
}

/// Memory the system allocated with `LocalAlloc`.
struct Local(*mut c_void);

impl Drop for Local {
    fn drop(&mut self) {
        if !self.0.is_null() {
            unsafe { LocalFree(self.0 as HLOCAL) };
        }
    }
}

/// One `TOKEN_USER` or `TOKEN_OWNER` record of this process's token.
struct TokenSid {
    buffer: Vec<u64>,
}

impl TokenSid {
    fn query(class: i32) -> io::Result<Self> {
        let mut token: HANDLE = ptr::null_mut();
        check(unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) })?;
        let mut length = 0;
        unsafe { GetTokenInformation(token, class, ptr::null_mut(), 0, &mut length) };
        let mut buffer = vec![0u64; (length as usize).div_ceil(8)];
        let queried = check(unsafe {
            GetTokenInformation(
                token,
                class,
                buffer.as_mut_ptr().cast(),
                length,
                &mut length,
            )
        });
        unsafe { CloseHandle(token) };
        queried.map(|()| Self { buffer })
    }

    /// Both records start with one `SID_AND_ATTRIBUTES` or `PSID` field.
    fn sid(&self, class: i32) -> PSID {
        let base = self.buffer.as_ptr();
        if class == TokenUser {
            unsafe { (*base.cast::<TOKEN_USER>()).User.Sid }
        } else {
            unsafe { (*base.cast::<TOKEN_OWNER>()).Owner }
        }
    }
}

/// The user this process runs as, and the owner its new objects get. An
/// elevated administrator's new objects are owned by `BUILTIN\Administrators`.
pub(crate) struct CurrentUser {
    user: TokenSid,
    owner: TokenSid,
    /// SYSTEM and Administrators: the accounts that, like root on Unix, can
    /// take any file, so access for them does not make a file less private.
    root: [Vec<u64>; 2],
}

fn well_known(kind: i32) -> io::Result<Vec<u64>> {
    let mut buffer = vec![0u64; (SECURITY_MAX_SID_SIZE as usize).div_ceil(8)];
    let mut size = SECURITY_MAX_SID_SIZE;
    check(unsafe {
        CreateWellKnownSid(kind, ptr::null_mut(), buffer.as_mut_ptr().cast(), &mut size)
    })?;
    Ok(buffer)
}

impl CurrentUser {
    pub(crate) fn query() -> io::Result<Self> {
        Ok(Self {
            user: TokenSid::query(TokenUser)?,
            owner: TokenSid::query(TokenOwner)?,
            root: [
                well_known(WinLocalSystemSid)?,
                well_known(WinBuiltinAdministratorsSid)?,
            ],
        })
    }

    fn user(&self) -> PSID {
        self.user.sid(TokenUser)
    }

    fn may_own(&self, sid: PSID) -> bool {
        unsafe { EqualSid(sid, self.user()) != 0 || EqualSid(sid, self.owner.sid(TokenOwner)) != 0 }
    }

    /// A protected DACL with one ACE: full access for this user. `inherit`
    /// makes the ACE apply to files and directories created inside.
    pub(crate) fn private_descriptor(&self, inherit: bool) -> io::Result<PrivateDescriptor> {
        let mut text = ptr::null_mut();
        check(unsafe { ConvertSidToStringSidW(self.user(), &mut text) })?;
        let text = Local(text.cast());
        let sid = unsafe { wide_to_string(text.0.cast()) };
        let flags = if inherit { "OICI" } else { "" };
        let sddl: Vec<u16> = format!("D:P(A;{flags};FA;;;{sid})")
            .encode_utf16()
            .chain([0])
            .collect();
        let mut descriptor: PSECURITY_DESCRIPTOR = ptr::null_mut();
        check(unsafe {
            ConvertStringSecurityDescriptorToSecurityDescriptorW(
                sddl.as_ptr(),
                SDDL_REVISION_1,
                &mut descriptor,
                ptr::null_mut(),
            )
        })?;
        Ok(PrivateDescriptor(Local(descriptor)))
    }

    /// Replace the DACL of `object` with [`Self::private_descriptor`]. The
    /// handle needs `WRITE_DAC`. The kernel call changes this object only:
    /// unlike `SetSecurityInfo`, it never rewrites the inherited ACEs of
    /// existing children, which may be hard links to files elsewhere.
    pub(crate) fn restrict(&self, object: &impl AsRawHandle, inherit: bool) -> io::Result<()> {
        let descriptor = self.private_descriptor(inherit)?;
        check(unsafe {
            SetKernelObjectSecurity(
                object.as_raw_handle() as HANDLE,
                DACL_SECURITY_INFORMATION,
                descriptor.as_ptr(),
            )
        })
    }

    fn security(
        object: &impl AsRawHandle,
        kind: SE_OBJECT_TYPE,
    ) -> io::Result<(PSID, *mut ACL, Local)> {
        let mut owner: PSID = ptr::null_mut();
        let mut dacl: *mut ACL = ptr::null_mut();
        let mut descriptor: PSECURITY_DESCRIPTOR = ptr::null_mut();
        win32(unsafe {
            GetSecurityInfo(
                object.as_raw_handle() as HANDLE,
                kind,
                OWNER_SECURITY_INFORMATION | DACL_SECURITY_INFORMATION,
                &mut owner,
                ptr::null_mut(),
                &mut dacl,
                ptr::null_mut(),
                &mut descriptor,
            )
        })?;
        Ok((owner, dacl, Local(descriptor)))
    }

    /// True when this user or its default owner owns `object`. The handle
    /// needs `READ_CONTROL`.
    pub(crate) fn owns(&self, object: &impl AsRawHandle, kind: SE_OBJECT_TYPE) -> io::Result<bool> {
        let (owner, _, _descriptor) = Self::security(object, kind)?;
        Ok(!owner.is_null() && self.may_own(owner))
    }

    /// True when [`Self::owns`] holds and every ACE that grants access names
    /// this user, SYSTEM or Administrators: no other account can open `object`.
    pub(crate) fn owns_privately(
        &self,
        object: &impl AsRawHandle,
        kind: SE_OBJECT_TYPE,
    ) -> io::Result<bool> {
        let (owner, dacl, _descriptor) = Self::security(object, kind)?;
        if owner.is_null() || !self.may_own(owner) || dacl.is_null() {
            return Ok(false);
        }
        let mut size = ACL_SIZE_INFORMATION::default();
        check(unsafe {
            GetAclInformation(
                dacl,
                (&mut size as *mut ACL_SIZE_INFORMATION).cast(),
                size_of::<ACL_SIZE_INFORMATION>() as u32,
                AclSizeInformation,
            )
        })?;
        for index in 0..size.AceCount {
            let mut ace: *mut c_void = ptr::null_mut();
            check(unsafe { GetAce(dacl, index, &mut ace) })?;
            let header = unsafe { &*ace.cast::<ACE_HEADER>() };
            if u32::from(header.AceFlags) & INHERIT_ONLY_ACE != 0
                || u32::from(header.AceType) == ACCESS_DENIED_ACE_TYPE
            {
                continue;
            }
            if u32::from(header.AceType) != ACCESS_ALLOWED_ACE_TYPE {
                return Ok(false);
            }
            let allowed = ace.cast::<ACCESS_ALLOWED_ACE>();
            let sid = unsafe { ptr::addr_of_mut!((*allowed).SidStart) }.cast::<c_void>();
            let root = |known: &Vec<u64>| unsafe { EqualSid(sid, known.as_ptr() as PSID) } != 0;
            if unsafe { EqualSid(sid, self.user()) } == 0 && !self.root.iter().any(root) {
                return Ok(false);
            }
        }
        Ok(true)
    }
}

/// The DACL an object had, with its protection and inheritance flags, to put
/// back exactly after a change that turned out wrong.
pub(crate) struct SavedDacl(Local);

impl SavedDacl {
    /// The handle needs `READ_CONTROL`.
    pub(crate) fn of(object: &impl AsRawHandle, kind: SE_OBJECT_TYPE) -> io::Result<Self> {
        let mut descriptor: PSECURITY_DESCRIPTOR = ptr::null_mut();
        win32(unsafe {
            GetSecurityInfo(
                object.as_raw_handle() as HANDLE,
                kind,
                DACL_SECURITY_INFORMATION,
                ptr::null_mut(),
                ptr::null_mut(),
                ptr::null_mut(),
                ptr::null_mut(),
                &mut descriptor,
            )
        })?;
        Ok(Self(Local(descriptor)))
    }

    /// The handle needs `WRITE_DAC`.
    pub(crate) fn restore(&self, object: &impl AsRawHandle) -> io::Result<()> {
        check(unsafe {
            SetKernelObjectSecurity(
                object.as_raw_handle() as HANDLE,
                DACL_SECURITY_INFORMATION,
                self.0.0,
            )
        })
    }
}

/// A self-relative security descriptor from [`CurrentUser::private_descriptor`].
pub(crate) struct PrivateDescriptor(Local);

// The descriptor is immutable once built and only read by the system.
unsafe impl Send for PrivateDescriptor {}
unsafe impl Sync for PrivateDescriptor {}

impl PrivateDescriptor {
    pub(crate) fn as_ptr(&self) -> PSECURITY_DESCRIPTOR {
        self.0.0
    }
}

fn win32(status: u32) -> io::Result<()> {
    if status == 0 {
        Ok(())
    } else {
        Err(io::Error::from_raw_os_error(status as i32))
    }
}

unsafe fn wide_to_string(text: *const u16) -> String {
    let mut length = 0;
    while unsafe { *text.add(length) } != 0 {
        length += 1;
    }
    String::from_utf16_lossy(unsafe { std::slice::from_raw_parts(text, length) })
}
