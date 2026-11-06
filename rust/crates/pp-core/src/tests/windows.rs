use std::{os::windows::process::ExitStatusExt, path::Path, process::ExitStatus};
use windows_sys::Win32::{
    Networking::WinSock::{SO_TYPE, SOCKADDR_STORAGE, SOCKET, SOL_SOCKET, getsockname, getsockopt},
    System::Threading::{GetCurrentProcess, TerminateProcess},
};

#[cfg(feature = "desktop_rebuild")]
pub(crate) use std::os::windows::fs::symlink_dir;
pub(crate) use std::os::windows::fs::symlink_file;

pub(crate) type RawSocket = std::os::windows::io::RawSocket;

/// The exit code [`kill_self`] ends the process with.
const KILLED: u32 = 0xDEAD;

/// A junction to the parent directory: Windows keeps no FIFOs in
/// directories, and its entries that are neither files, directories nor
/// symbolic links are reparse points such as this one.
pub(crate) fn special_entry(path: &Path) {
    let status = std::process::Command::new("cmd")
        .args(["/C", "mklink", "/J"])
        .arg(path)
        .arg(path.parent().unwrap())
        .stdout(std::process::Stdio::null())
        .status()
        .unwrap();
    assert!(status.success(), "mklink /J failed: {status}");
}

pub(crate) fn assert_special_entry(path: &Path) {
    assert!(
        std::fs::symlink_metadata(path)
            .unwrap()
            .file_type()
            .is_symlink()
    );
}

/// Windows has no mode bits: a new state file inherits its directory's ACL.
/// Every ACE that grants access names this user, SYSTEM or Administrators,
/// and one names this user, as the profile folder's ACL gives.
pub(crate) fn assert_private_file(path: &Path) {
    let output = |program: &str, args: &[&std::ffi::OsStr]| {
        let output = std::process::Command::new(program)
            .args(args)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{program} failed: {}",
            output.status
        );
        String::from_utf8(output.stdout).unwrap()
    };
    let user = output(
        "whoami",
        &[
            "/user".as_ref(),
            "/fo".as_ref(),
            "csv".as_ref(),
            "/nh".as_ref(),
        ],
    );
    let user = user
        .trim()
        .rsplit(',')
        .next()
        .unwrap()
        .trim_matches('"')
        .to_owned();
    let mut script = std::ffi::OsString::from("(Get-Acl -LiteralPath '");
    script.push(path.as_os_str());
    script.push("').Sddl");
    let sddl = output(
        "powershell",
        &["-NoProfile".as_ref(), "-Command".as_ref(), &script],
    );
    let dacl = sddl
        .trim()
        .split("D:")
        .nth(1)
        .unwrap()
        .split("S:")
        .next()
        .unwrap();
    let granted: Vec<&str> = dacl
        .split('(')
        .skip(1)
        .filter_map(|ace| {
            let fields: Vec<&str> = ace.trim_end_matches(')').split(';').collect();
            (fields[0] == "A" && !fields[1].contains("IO")).then_some(fields[5])
        })
        .collect();
    assert!(
        granted
            .iter()
            .all(|sid| ["SY", "BA", user.as_str()].contains(sid)),
        "{} grants {granted:?}",
        path.display()
    );
    assert!(
        granted.contains(&user.as_str()),
        "{}: {sddl}",
        path.display()
    );
}

fn icacls(path: &Path, change: &[&str]) {
    let status = std::process::Command::new("icacls")
        .arg(path)
        .args(change)
        .stdout(std::process::Stdio::null())
        .status()
        .unwrap();
    assert!(status.success(), "icacls {change:?} failed: {status}");
}

/// Deny Everyone, administrators included, reading the file's data.
pub(crate) fn deny_read(path: &Path) {
    icacls(path, &["/deny", "*S-1-1-0:(RD)"]);
}

pub(crate) fn allow_read(path: &Path) {
    icacls(path, &["/remove:d", "*S-1-1-0"]);
}

/// Terminates this process without running any more of its code, as SIGKILL
/// does on Unix. `TerminateProcess` on the current process may return before
/// the process ends.
pub(crate) fn kill_self() -> ! {
    assert_ne!(unsafe { TerminateProcess(GetCurrentProcess(), KILLED) }, 0);
    loop {
        std::thread::park();
    }
}

/// The child ended through [`kill_self`].
pub(crate) fn killed(status: &ExitStatus) -> bool {
    *status == ExitStatus::from_raw(KILLED)
}

pub(crate) fn raw_socket(listener: &std::net::TcpListener) -> RawSocket {
    std::os::windows::io::AsRawSocket::as_raw_socket(listener)
}

/// The type and local address of the socket `raw` names, or `None` when
/// `raw` is not an open socket.
pub(crate) fn socket_name(raw: RawSocket) -> Option<(i32, Vec<u8>)> {
    let socket = raw as SOCKET;
    let mut kind: i32 = 0;
    let mut kind_len = std::mem::size_of_val(&kind) as i32;
    let kind_status = unsafe {
        getsockopt(
            socket,
            SOL_SOCKET,
            SO_TYPE,
            (&raw mut kind).cast(),
            &mut kind_len,
        )
    };
    let mut storage: SOCKADDR_STORAGE = unsafe { std::mem::zeroed() };
    let mut len = std::mem::size_of_val(&storage) as i32;
    let status = unsafe { getsockname(socket, (&raw mut storage).cast(), &mut len) };
    (kind_status == 0 && status == 0).then(|| {
        let bytes =
            unsafe { std::slice::from_raw_parts((&raw const storage).cast::<u8>(), len as usize) };
        (kind, bytes.to_vec())
    })
}
