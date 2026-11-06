use super::*;
use std::io::{Read, Write};

fn scratch() -> tempfile::TempDir {
    tempfile::tempdir().unwrap()
}

#[test]
fn entry_names_must_be_one_component() {
    let root = scratch();
    let dir = Dir::open(root.path()).unwrap();
    for name in ["", ".", "..", "a/b", "a/", "/a"] {
        let error = dir.status_nofollow(name).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::InvalidInput, "{name:?}");
        assert_eq!(
            dir.create_new_nofollow(name).unwrap_err().kind(),
            io::ErrorKind::InvalidInput,
            "{name:?}"
        );
    }
    #[cfg(windows)]
    for name in ["a\\b", "C:", "C:a"] {
        assert_eq!(
            dir.open_read_nofollow(name).unwrap_err().kind(),
            io::ErrorKind::InvalidInput,
            "{name:?}"
        );
    }
}

#[test]
fn create_new_then_open_existing_share_one_identity() {
    let root = scratch();
    let dir = Dir::open(root.path()).unwrap();
    let mut created = dir.create_new_nofollow("entry").unwrap();
    created.write_all(b"bytes").unwrap();
    assert_eq!(
        dir.create_new_nofollow("entry").unwrap_err().kind(),
        io::ErrorKind::AlreadyExists
    );
    let opened = dir.open_write_nofollow("entry").unwrap();
    let mut read = String::new();
    dir.open_read_nofollow("entry")
        .unwrap()
        .read_to_string(&mut read)
        .unwrap();
    assert_eq!(read, "bytes");
    let named = dir.status_nofollow("entry").unwrap();
    assert!(named.is_file());
    assert_eq!(named.links(), 1);
    assert_eq!(status_of(&created).unwrap().identity(), named.identity());
    assert_eq!(status_of(&opened).unwrap().identity(), named.identity());
    assert_eq!(
        status_nofollow(&root.path().join("entry"))
            .unwrap()
            .identity(),
        named.identity()
    );
    assert_eq!(
        dir.open_write_nofollow("missing").unwrap_err().kind(),
        io::ErrorKind::NotFound
    );
}

#[test]
fn hard_links_share_identity_and_count_links() {
    let root = scratch();
    let first = root.path().join("first");
    std::fs::write(&first, b"x").unwrap();
    std::fs::hard_link(&first, root.path().join("second")).unwrap();
    let dir = Dir::open(root.path()).unwrap();
    let a = dir.status_nofollow("first").unwrap();
    let b = dir.status_nofollow("second").unwrap();
    assert_eq!(a.identity(), b.identity());
    assert_eq!(a.links(), 2);
    std::fs::write(root.path().join("third"), b"x").unwrap();
    assert_ne!(
        dir.status_nofollow("third").unwrap().identity(),
        a.identity()
    );
}

#[test]
fn directory_status_matches_its_path_and_kinds_are_reported() {
    let root = scratch();
    let dir = Dir::open(root.path()).unwrap();
    let opened = dir.status().unwrap();
    assert!(opened.is_dir());
    assert_eq!(
        status_nofollow(root.path()).unwrap().identity(),
        opened.identity()
    );
    assert_eq!(
        status_follow(root.path()).unwrap().identity(),
        opened.identity()
    );
    assert_eq!(
        dir.try_clone().unwrap().status().unwrap().identity(),
        opened.identity()
    );
    std::fs::create_dir(root.path().join("child")).unwrap();
    assert_eq!(
        dir.status_nofollow("child").unwrap().kind(),
        Kind::Directory
    );
    assert!(dir.remove_file("child").is_err());
    let read = dir.open_read_nofollow("child").unwrap();
    assert!(status_of(&read).unwrap().is_dir());
    assert_eq!(not_regular_file(true).kind(), io::ErrorKind::IsADirectory);
}

#[test]
fn remove_file_removes_only_the_named_entry() {
    let root = scratch();
    let dir = Dir::open(root.path()).unwrap();
    drop(dir.create_new_nofollow("gone").unwrap());
    drop(dir.create_new_nofollow("kept").unwrap());
    dir.remove_file("gone").unwrap();
    assert_eq!(
        dir.status_nofollow("gone").unwrap_err().kind(),
        io::ErrorKind::NotFound
    );
    assert_eq!(
        dir.remove_file("gone").unwrap_err().kind(),
        io::ErrorKind::NotFound
    );
    assert!(dir.status_nofollow("kept").unwrap().is_file());
}

#[test]
fn locks_exclude_a_second_open_file_until_unlocked() {
    let root = scratch();
    let path = root.path().join("lock");
    let first = File::create(&path).unwrap();
    let second = File::options().write(true).open(&path).unwrap();
    try_lock(&first).unwrap();
    assert!(matches!(try_lock(&second), Err(TryLockError::WouldBlock)));
    unlock(&first).unwrap();
    try_lock(&second).unwrap();
    drop(second);
    try_lock(&first).unwrap();
}

#[test]
fn identity_reuse_follows_the_file_system() {
    let root = scratch();
    let reused = !Dir::open(root.path())
        .unwrap()
        .identities_are_never_reused()
        .unwrap();
    // Hosted Windows runners use NTFS.
    assert_eq!(reused, cfg!(unix));
}

#[test]
fn a_held_lock_lets_other_handles_read_the_file() {
    let root = scratch();
    let path = root.path().join("lease");
    std::fs::write(&path, b"owner").unwrap();
    let held = File::options().read(true).write(true).open(&path).unwrap();
    try_lock(&held).unwrap();
    assert_eq!(std::fs::read(&path).unwrap(), b"owner");
    let copy = root.path().join("copy");
    std::fs::copy(&path, &copy).unwrap();
    assert_eq!(std::fs::read(&copy).unwrap(), b"owner");
    let other = File::options().write(true).open(&path).unwrap();
    assert!(matches!(try_lock(&other), Err(TryLockError::WouldBlock)));
    unlock(&held).unwrap();
}

#[test]
fn private_directories_are_created_once() {
    let root = scratch();
    let nested = root.path().join("a").join("b");
    assert!(create_private_dir(&nested).is_err());
    create_private_dir_all(&nested).unwrap();
    create_private_dir_all(&nested).unwrap();
    assert_eq!(
        create_private_dir(&nested).unwrap_err().kind(),
        io::ErrorKind::AlreadyExists
    );
    let status = status_follow(&nested).unwrap();
    assert!(status.is_dir());
    #[cfg(unix)]
    assert_eq!(status.posix_access().unwrap().mode() & 0o777, 0o700);
    #[cfg(windows)]
    assert_eq!(status.posix_access(), None);
}

#[test]
fn publish_rename_replaces_the_destination_durably() {
    let root = scratch();
    let temporary = root.path().join(".temporary");
    let destination = root.path().join("published");
    std::fs::write(&temporary, b"new").unwrap();
    std::fs::write(&destination, b"old").unwrap();
    let identity = status_nofollow(&temporary).unwrap().identity();
    publish_rename(&temporary, &destination).unwrap();
    sync_directory(root.path()).unwrap();
    assert_eq!(std::fs::read(&destination).unwrap(), b"new");
    assert_eq!(status_nofollow(&destination).unwrap().identity(), identity);
    assert!(!temporary.exists());
    assert!(sync_directory(&root.path().join("missing")).is_err());
}

#[test]
fn create_new_over_a_directory_reports_already_exists() {
    let root = scratch();
    std::fs::create_dir(root.path().join("taken")).unwrap();
    let dir = Dir::open(root.path()).unwrap();
    assert_eq!(
        dir.create_new_nofollow("taken").unwrap_err().kind(),
        io::ErrorKind::AlreadyExists
    );
    assert!(dir.status_nofollow("taken").unwrap().is_dir());
}

#[test]
fn publish_new_refuses_an_existing_destination() {
    let root = scratch();
    let temporary = root.path().join(".temporary");
    let destination = root.path().join("published");
    std::fs::write(&temporary, b"new").unwrap();
    sync_file(&temporary).unwrap();
    let identity = status_nofollow(&temporary).unwrap().identity();
    publish_new(&temporary, &destination).unwrap();
    sync_directory(root.path()).unwrap();
    assert_eq!(std::fs::read(&destination).unwrap(), b"new");
    assert_eq!(status_nofollow(&destination).unwrap().identity(), identity);
    assert!(!temporary.exists());

    std::fs::write(&temporary, b"newer").unwrap();
    assert_eq!(
        publish_new(&temporary, &destination).unwrap_err().kind(),
        io::ErrorKind::AlreadyExists
    );
    assert_eq!(std::fs::read(&destination).unwrap(), b"new");
    assert_eq!(std::fs::read(&temporary).unwrap(), b"newer");
    assert!(sync_file(&root.path().join("missing")).is_err());
}

#[test]
fn entry_operations_follow_the_handle_after_the_directory_is_renamed() {
    let root = scratch();
    let path = root.path().join("store");
    let retired = root.path().join("retired");
    std::fs::create_dir(&path).unwrap();
    let dir = Dir::open(&path).unwrap();
    let clone = dir.try_clone().unwrap();
    let opened = dir.status().unwrap().identity();
    drop(dir.create_new_nofollow("before").unwrap());
    std::fs::rename(&path, &retired).unwrap();
    std::fs::create_dir(&path).unwrap();
    assert_eq!(status_nofollow(&retired).unwrap().identity(), opened);
    assert_ne!(status_nofollow(&path).unwrap().identity(), opened);
    assert_eq!(dir.status().unwrap().identity(), opened);

    let mut created = clone.create_new_nofollow("after").unwrap();
    created.write_all(b"moved").unwrap();
    drop(created);
    assert!(retired.join("after").is_file());
    assert!(!path.join("after").exists());
    assert!(dir.status_nofollow("before").unwrap().is_file());
    let mut read = String::new();
    dir.open_read_nofollow("after")
        .unwrap()
        .read_to_string(&mut read)
        .unwrap();
    assert_eq!(read, "moved");
    drop(dir.open_write_nofollow("after").unwrap());

    std::fs::write(path.join("before"), b"other").unwrap();
    dir.remove_file("before").unwrap();
    assert!(!retired.join("before").exists());
    assert!(path.join("before").is_file());
    assert_eq!(
        dir.status_nofollow("before").unwrap_err().kind(),
        io::ErrorKind::NotFound
    );
}

#[test]
fn remove_file_removes_a_read_only_entry() {
    let root = scratch();
    let dir = Dir::open(root.path()).unwrap();
    drop(dir.create_new_nofollow("read-only").unwrap());
    let path = root.path().join("read-only");
    let mut permissions = std::fs::metadata(&path).unwrap().permissions();
    permissions.set_readonly(true);
    std::fs::set_permissions(&path, permissions).unwrap();
    dir.remove_file("read-only").unwrap();
    assert!(!path.exists());
    assert_eq!(
        dir.status_nofollow("read-only").unwrap_err().kind(),
        io::ErrorKind::NotFound
    );
}

#[test]
fn open_directory_can_be_removed() {
    let root = scratch();
    let path = root.path().join("store");
    std::fs::create_dir(&path).unwrap();
    let dir = Dir::open(&path).unwrap();
    let clone = dir.try_clone().unwrap();
    std::fs::remove_dir(&path).unwrap();
    assert!(dir.create_new_nofollow("entry").is_err());
    assert!(clone.create_new_nofollow("entry").is_err());
    drop(dir);
    drop(clone);
    assert!(!path.exists());
}

#[test]
fn directory_operations_refuse_names_outside_the_directory() {
    let root = scratch();
    let dir = Dir::open(root.path()).unwrap();
    dir.create_dir("inside").unwrap();
    let mut names = vec!["", ".", "..", "a/b", "../inside", "/a"];
    if cfg!(windows) {
        names.extend(["a\\b", "..\\inside", "C:", "C:a"]);
    }
    for name in names {
        let refused = |result: io::Result<()>| {
            assert_eq!(
                result.unwrap_err().kind(),
                io::ErrorKind::InvalidInput,
                "{name:?}"
            );
        };
        refused(dir.create_dir(name));
        refused(dir.remove_dir(name));
        refused(dir.rename_no_replace(name, "moved"));
        refused(dir.rename_no_replace("inside", name));
        refused(dir.open_dir_nofollow(name).map(drop));
        refused(dir.open_search_nofollow(name).map(drop));
        refused(dir.open_file(name, FileOpen::Read).map(drop));
    }
    assert!(dir.status_nofollow("inside").unwrap().is_dir());
}

#[test]
fn windows_aliases_are_recognized_and_refused_only_on_windows() {
    for alias in [
        "NUL",
        "nul.stl",
        "Con",
        "con.tar.gz",
        "AUX .txt",
        "COM1",
        "lpt9.md",
        "COM¹",
        "CONOUT$",
        "name.",
        "name ",
        "a.b.",
    ] {
        assert!(is_windows_alias(alias), "{alias:?}");
    }
    for plain in [
        "null.stl",
        "COM10",
        "LPT",
        "console.txt",
        ".hidden",
        "a.b",
        "nu l",
    ] {
        assert!(!is_windows_alias(plain), "{plain:?}");
    }
    let root = scratch();
    let dir = Dir::open(root.path()).unwrap();
    for alias in ["NUL.stl", "con", "trailing.", "trailing "] {
        let created = dir.create_new_nofollow(alias);
        if cfg!(windows) {
            assert_eq!(created.unwrap_err().kind(), io::ErrorKind::InvalidInput);
            assert_eq!(
                dir.create_dir(alias).unwrap_err().kind(),
                io::ErrorKind::InvalidInput
            );
        } else {
            drop(created.unwrap());
            dir.remove_file(alias).unwrap();
        }
    }
}

#[test]
fn directories_are_created_renamed_and_removed_through_the_handle() {
    let root = scratch();
    let dir = Dir::open(root.path()).unwrap();
    dir.create_dir("stage").unwrap();
    dir.sync().unwrap();
    assert_eq!(
        dir.create_dir("stage").unwrap_err().kind(),
        io::ErrorKind::AlreadyExists
    );
    let stage = dir.open_dir_nofollow("stage").unwrap();
    let mut file = stage.open_file("file", FileOpen::CreateWriteOnly).unwrap();
    file.write_all(b"staged").unwrap();
    file.sync_all().unwrap();
    assert!(status_of(&file).unwrap().is_file());
    let again = stage.open_file("file", FileOpen::CreateWriteOnly);
    assert_eq!(again.unwrap_err().kind(), io::ErrorKind::AlreadyExists);
    let mut read = Vec::new();
    let mut reader = stage.open_file("file", FileOpen::Read).unwrap();
    reader.read_to_end(&mut read).unwrap();
    assert_eq!(read, b"staged");
    drop((file, reader));

    dir.create_dir("taken").unwrap();
    assert_eq!(
        dir.rename_no_replace("stage", "taken").unwrap_err().kind(),
        io::ErrorKind::AlreadyExists
    );
    dir.rename_no_replace("stage", "published").unwrap();
    dir.sync().unwrap();
    assert_eq!(
        std::fs::read(root.path().join("published/file")).unwrap(),
        b"staged"
    );
    assert_eq!(
        dir.remove_dir("published").unwrap_err().kind(),
        io::ErrorKind::DirectoryNotEmpty
    );
    stage.remove_file("file").unwrap();
    dir.remove_dir("published").unwrap();
    dir.remove_dir("taken").unwrap();
    assert!(dir.entries().unwrap().next().is_none());
}

#[test]
fn entries_list_every_name_once_without_dot_entries() {
    let root = scratch();
    let dir = Dir::open(root.path()).unwrap();
    // Long names fill several enumeration buffers on Windows.
    let mut expected = (0..300)
        .map(|index| format!("{index:03}-{}", "n".repeat(200)))
        .collect::<Vec<_>>();
    for name in &expected {
        drop(dir.create_new_nofollow(name).unwrap());
    }
    dir.create_dir("child").unwrap();
    expected.push("child".to_owned());
    expected.sort();
    let clone = dir.try_clone().unwrap();
    for listed in [&dir, &clone, &dir] {
        let mut names = listed
            .entries()
            .unwrap()
            .map(|name| name.unwrap().into_string().unwrap())
            .collect::<Vec<_>>();
        names.sort();
        assert_eq!(names, expected);
    }
    let search = Dir::open_search(root.path()).unwrap();
    let child = search
        .open_search_nofollow("child")
        .unwrap()
        .reopen()
        .unwrap();
    assert!(child.entries().unwrap().next().is_none());
    assert_eq!(
        child.status().unwrap().identity(),
        dir.status_nofollow("child").unwrap().identity()
    );
}

#[test]
fn directory_locks_exclude_a_second_handle_until_released() {
    let root = scratch();
    let first = Dir::open(root.path()).unwrap();
    let second = Dir::open(root.path()).unwrap();
    // Unlocking without a lock succeeds, as `flock(LOCK_UN)` does.
    first.unlock().unwrap();
    first.try_lock().unwrap();
    let blocked = |dir: &Dir| {
        assert_eq!(
            dir.try_lock().unwrap_err().kind(),
            io::ErrorKind::WouldBlock
        );
        // A refused lock leaves nothing to release.
        dir.unlock().unwrap();
    };
    blocked(&second);
    first.unlock().unwrap();
    second.try_lock().unwrap();
    blocked(&first);
    drop(second);
    first.lock().unwrap();
    first.unlock().unwrap();
    first.unlock().unwrap();
    // Only Windows needs the lock file.
    let lock_file = root.path().join(DIR_LOCK_FILE);
    assert_eq!(lock_file.is_file(), cfg!(windows));
}

#[cfg(unix)]
mod unix {
    use super::*;

    #[test]
    fn symbolic_links_are_reported_and_never_followed() {
        let root = scratch();
        std::fs::write(root.path().join("target"), b"secret").unwrap();
        std::os::unix::fs::symlink("target", root.path().join("link")).unwrap();
        std::os::unix::fs::symlink("absent", root.path().join("dangling")).unwrap();
        let dir = Dir::open(root.path()).unwrap();
        for name in ["link", "dangling"] {
            assert_eq!(dir.status_nofollow(name).unwrap().kind(), Kind::Link);
            for error in [
                dir.open_read_nofollow(name).unwrap_err(),
                dir.open_write_nofollow(name).unwrap_err(),
                open_read_nofollow(&root.path().join(name)).unwrap_err(),
                dir.open_file(name, FileOpen::Read).unwrap_err(),
            ] {
                assert_eq!(error.raw_os_error(), Some(libc::ELOOP), "{name}");
            }
            // With `O_DIRECTORY`, Linux reports the link as not a directory.
            for error in [
                dir.open_dir_nofollow(name).map(drop).unwrap_err(),
                dir.open_search_nofollow(name).map(drop).unwrap_err(),
            ] {
                let code = error.raw_os_error();
                assert!(matches!(code, Some(libc::ENOTDIR | libc::ELOOP)), "{name}");
            }
            assert_eq!(
                dir.create_new_nofollow(name).unwrap_err().kind(),
                io::ErrorKind::AlreadyExists
            );
            assert_eq!(
                dir.open_file(name, FileOpen::CreateWriteOnly)
                    .unwrap_err()
                    .kind(),
                io::ErrorKind::AlreadyExists
            );
            assert_eq!(
                dir.create_dir(name).unwrap_err().kind(),
                io::ErrorKind::AlreadyExists
            );
        }
        assert_eq!(
            status_nofollow(&root.path().join("link")).unwrap().kind(),
            Kind::Link
        );
        assert!(status_follow(&root.path().join("link")).unwrap().is_file());
        dir.remove_file("link").unwrap();
        assert_eq!(
            std::fs::read(root.path().join("target")).unwrap(),
            b"secret"
        );
    }

    #[test]
    fn entry_access_follows_the_handle_after_the_path_is_replaced() {
        let root = scratch();
        let path = root.path().join("store");
        std::fs::create_dir(&path).unwrap();
        let dir = Dir::open(&path).unwrap();
        let opened = dir.status().unwrap().identity();
        std::fs::rename(&path, root.path().join("retired")).unwrap();
        std::fs::create_dir(&path).unwrap();
        drop(dir.create_new_nofollow("entry").unwrap());
        assert!(root.path().join("retired").join("entry").is_file());
        assert!(!path.join("entry").exists());
        assert_ne!(status_nofollow(&path).unwrap().identity(), opened);
    }

    #[test]
    fn posix_access_reports_owner_and_mode() {
        use std::os::unix::fs::PermissionsExt;
        let root = scratch();
        let path = root.path().join("file");
        std::fs::write(&path, b"").unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o640)).unwrap();
        let access = status_nofollow(&path).unwrap().posix_access().unwrap();
        assert_eq!(access.mode() & 0o7777, 0o640);
        assert!(access.owned_by_current_user());
        assert_eq!(access.uid(), unsafe { libc::geteuid() });
        let dir = Dir::open(root.path()).unwrap();
        assert_eq!(
            dir.status_nofollow("file").unwrap().posix_access(),
            Some(access)
        );
        let created = dir.create_new_nofollow("created").unwrap();
        let mode = status_of(&created).unwrap().posix_access().unwrap().mode();
        assert_eq!(mode & 0o077, 0, "{mode:o}");
        assert_eq!(not_regular_file(false).raw_os_error(), Some(libc::EINVAL));
    }
}

#[cfg(windows)]
mod windows {
    use super::*;

    /// Symbolic links need the create-symlink privilege or developer mode.
    /// Hosted runners are administrators; elsewhere the test says why it
    /// skipped instead of passing silently.
    fn symlink(target: &str, link: &Path, directory: bool) -> bool {
        let result = if directory {
            std::os::windows::fs::symlink_dir(target, link)
        } else {
            std::os::windows::fs::symlink_file(target, link)
        };
        match result {
            Ok(()) => true,
            Err(error) if error.raw_os_error() == Some(1314) => {
                eprintln!("SKIP: symbolic links need SeCreateSymbolicLinkPrivilege: {error}");
                false
            }
            Err(error) => panic!("create symbolic link {}: {error}", link.display()),
        }
    }

    #[test]
    fn reparse_points_are_reported_and_never_followed() {
        let root = scratch();
        std::fs::write(root.path().join("target"), b"secret").unwrap();
        std::fs::create_dir(root.path().join("target-dir")).unwrap();
        if !symlink("target", &root.path().join("link"), false)
            || !symlink("absent", &root.path().join("dangling"), false)
            || !symlink("target-dir", &root.path().join("dir-link"), true)
        {
            return;
        }
        let dir = Dir::open(root.path()).unwrap();
        for name in ["link", "dangling", "dir-link"] {
            assert_eq!(
                dir.status_nofollow(name).unwrap().kind(),
                Kind::Link,
                "{name}"
            );
            assert_eq!(
                status_nofollow(&root.path().join(name)).unwrap().kind(),
                Kind::Link
            );
            assert!(dir.open_read_nofollow(name).is_err(), "{name}");
            assert!(
                open_read_nofollow(&root.path().join(name)).is_err(),
                "{name}"
            );
            assert_eq!(
                dir.create_new_nofollow(name).unwrap_err().kind(),
                io::ErrorKind::AlreadyExists,
                "{name}"
            );
        }
        assert!(dir.open_write_nofollow("link").is_err());
        assert!(status_follow(&root.path().join("link")).unwrap().is_file());
        assert!(
            status_follow(&root.path().join("dir-link"))
                .unwrap()
                .is_dir()
        );
        dir.remove_file("link").unwrap();
        assert_eq!(
            std::fs::read(root.path().join("target")).unwrap(),
            b"secret"
        );
    }

    #[test]
    fn replacing_the_link_used_to_open_a_directory_does_not_redirect_entries() {
        let root = scratch();
        std::fs::create_dir(root.path().join("first")).unwrap();
        std::fs::create_dir(root.path().join("second")).unwrap();
        let link = root.path().join("store");
        if !symlink("first", &link, true) {
            return;
        }
        let dir = Dir::open(&link).unwrap();
        std::fs::remove_dir(&link).unwrap();
        assert!(symlink("second", &link, true));
        drop(dir.create_new_nofollow("entry").unwrap());
        assert!(root.path().join("first").join("entry").is_file());
        assert!(!root.path().join("second").join("entry").exists());
        assert!(dir.status_nofollow("entry").unwrap().is_file());
        dir.remove_file("entry").unwrap();
        assert!(!root.path().join("first").join("entry").exists());
    }

    /// Create a junction (a mount-point reparse point) at `link`. Junctions
    /// need no privilege, so tests that use them run on any host.
    fn junction(target: &Path, link: &Path) {
        use std::os::windows::{ffi::OsStrExt, fs::OpenOptionsExt, io::AsRawHandle};
        use windows_sys::Win32::{
            Storage::FileSystem::{FILE_FLAG_BACKUP_SEMANTICS, FILE_FLAG_OPEN_REPARSE_POINT},
            System::IO::DeviceIoControl,
        };
        const FSCTL_SET_REPARSE_POINT: u32 = 0x0009_00A4;
        const IO_REPARSE_TAG_MOUNT_POINT: u32 = 0xA000_0003;
        let wide =
            |text: &OsStr| -> Vec<u8> { text.encode_wide().flat_map(u16::to_le_bytes).collect() };
        let mut substitute = OsStr::new(r"\??\").to_os_string();
        substitute.push(target.as_os_str());
        let (substitute, print) = (wide(&substitute), wide(target.as_os_str()));
        let paths_length = substitute.len() + 2 + print.len() + 2;
        let mut buffer = Vec::new();
        buffer.extend(IO_REPARSE_TAG_MOUNT_POINT.to_le_bytes());
        for field in [
            8 + paths_length,
            0,
            0,
            substitute.len(),
            substitute.len() + 2,
            print.len(),
        ] {
            buffer.extend(u16::try_from(field).unwrap().to_le_bytes());
        }
        buffer.extend(&substitute);
        buffer.extend([0, 0]);
        buffer.extend(&print);
        buffer.extend([0, 0]);

        std::fs::create_dir(link).unwrap();
        let directory = File::options()
            .write(true)
            .custom_flags(FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT)
            .open(link)
            .unwrap();
        let mut returned = 0;
        // SAFETY: the handle is open, and `buffer` is a complete mount-point
        // reparse buffer of the length passed.
        let set = unsafe {
            DeviceIoControl(
                directory.as_raw_handle(),
                FSCTL_SET_REPARSE_POINT,
                buffer.as_ptr().cast(),
                u32::try_from(buffer.len()).unwrap(),
                std::ptr::null_mut(),
                0,
                &mut returned,
                std::ptr::null_mut(),
            )
        };
        assert_ne!(
            set,
            0,
            "junction {}: {}",
            link.display(),
            io::Error::last_os_error()
        );
    }

    #[test]
    fn junctions_are_reported_and_never_followed() {
        let root = scratch();
        let target = root.path().join("target-dir");
        std::fs::create_dir(&target).unwrap();
        std::fs::write(target.join("inside"), b"secret").unwrap();
        junction(&target, &root.path().join("junction"));
        let dir = Dir::open(root.path()).unwrap();
        assert_eq!(dir.status_nofollow("junction").unwrap().kind(), Kind::Link);
        assert!(dir.open_read_nofollow("junction").is_err());
        assert!(dir.open_write_nofollow("junction").is_err());
        assert_eq!(
            dir.create_new_nofollow("junction").unwrap_err().kind(),
            io::ErrorKind::AlreadyExists
        );
        assert!(dir.remove_file("junction").is_err());
        assert_eq!(
            dir.remove_dir("junction").unwrap_err().kind(),
            io::ErrorKind::NotADirectory
        );
        assert!(dir.open_dir_nofollow("junction").is_err());
        assert!(dir.open_search_nofollow("junction").is_err());
        assert!(dir.open_file("junction", FileOpen::Read).is_err());
        dir.rename_no_replace("junction", "moved").unwrap();
        assert_eq!(dir.status_nofollow("moved").unwrap().kind(), Kind::Link);
        assert_eq!(std::fs::read(target.join("inside")).unwrap(), b"secret");
        assert!(dir.status_nofollow("target-dir").unwrap().is_dir());
    }

    #[test]
    fn entries_are_created_renamed_and_removed_relative_to_the_handle() {
        let root = scratch();
        let path = root.path().join("store");
        let retired = root.path().join("retired");
        std::fs::create_dir(&path).unwrap();
        let dir = Dir::open(&path).unwrap();
        std::fs::rename(&path, &retired).unwrap();
        std::fs::create_dir(&path).unwrap();

        dir.create_dir("child").unwrap();
        assert_eq!(
            dir.create_dir("child").unwrap_err().kind(),
            io::ErrorKind::AlreadyExists
        );
        assert!(retired.join("child").is_dir());
        assert!(!path.join("child").exists());

        let child = Dir::open(&retired.join("child")).unwrap();
        dir.rename_no_replace("child", "renamed").unwrap();
        drop(child.create_new_nofollow("entry").unwrap());
        assert!(retired.join("renamed").join("entry").is_file());
        assert_eq!(
            dir.remove_dir("renamed").unwrap_err().kind(),
            io::ErrorKind::DirectoryNotEmpty
        );
        child.remove_file("entry").unwrap();
        dir.remove_dir("renamed").unwrap();
        assert!(!retired.join("renamed").exists());
        assert!(child.create_new_nofollow("late").is_err());
        drop(child);

        drop(dir.create_new_nofollow("first").unwrap());
        drop(dir.create_new_nofollow("second").unwrap());
        let first = dir.status_nofollow("first").unwrap().identity();
        assert_eq!(
            dir.rename_no_replace("first", "second").unwrap_err().kind(),
            io::ErrorKind::AlreadyExists
        );
        assert_eq!(dir.status_nofollow("first").unwrap().identity(), first);
        dir.remove_file("first").unwrap();
        dir.create_dir("read-only").unwrap();
        let mut permissions = std::fs::metadata(retired.join("read-only"))
            .unwrap()
            .permissions();
        permissions.set_readonly(true);
        std::fs::set_permissions(retired.join("read-only"), permissions).unwrap();
        dir.remove_dir("read-only").unwrap();
        assert!(!retired.join("read-only").exists());
        assert!(dir.remove_dir("second").is_err());
        assert!(retired.join("second").is_file());
        assert!(!path.join("second").exists());
    }

    fn mode(file: &File) -> u32 {
        use windows_sys::Wdk::Storage::FileSystem::{
            FILE_MODE_INFORMATION, FileModeInformation, NtQueryInformationFile,
        };
        use windows_sys::Win32::System::IO::IO_STATUS_BLOCK;
        let mut information = FILE_MODE_INFORMATION::default();
        let mut status_block = IO_STATUS_BLOCK::default();
        // SAFETY: the handle is open, and the buffer is the class's structure.
        let status = unsafe {
            NtQueryInformationFile(
                std::os::windows::io::AsRawHandle::as_raw_handle(file),
                &mut status_block,
                (&raw mut information).cast(),
                size_of::<FILE_MODE_INFORMATION>() as u32,
                FileModeInformation,
            )
        };
        assert!(status >= 0, "NtQueryInformationFile: {status:#x}");
        information.Mode
    }

    #[test]
    fn created_directories_and_renamed_entries_are_written_through() {
        use windows_sys::Wdk::Storage::FileSystem::FILE_WRITE_THROUGH;
        let root = scratch();
        let dir = Dir::open(root.path()).unwrap();
        let created = dir.inner.create_dir_handle("created").unwrap();
        assert_ne!(mode(&created) & FILE_WRITE_THROUGH, 0);
        let source = dir.inner.rename_source("created").unwrap();
        assert_ne!(mode(&source) & FILE_WRITE_THROUGH, 0);
        drop((created, source));
        // Ordinary entry handles are not written through.
        let file = dir.create_new_nofollow("file").unwrap();
        assert_eq!(mode(&file) & FILE_WRITE_THROUGH, 0);
    }

    #[test]
    fn fallback_runs_only_when_posix_semantics_are_unsupported() {
        use crate::windows::with_fallback;
        use std::cell::Cell;
        use windows_sys::Win32::Foundation::{
            STATUS_ACCESS_DENIED, STATUS_INVALID_INFO_CLASS, STATUS_INVALID_PARAMETER,
            STATUS_NOT_SUPPORTED, STATUS_OBJECT_NAME_COLLISION, STATUS_SUCCESS,
        };
        for unsupported in [
            STATUS_INVALID_INFO_CLASS,
            STATUS_INVALID_PARAMETER,
            STATUS_NOT_SUPPORTED,
        ] {
            let classic = Cell::new(false);
            with_fallback(
                || Ok(unsupported),
                || {
                    classic.set(true);
                    Ok(STATUS_SUCCESS)
                },
            )
            .unwrap();
            assert!(classic.get(), "{unsupported:#x}");
            let error =
                with_fallback(|| Ok(unsupported), || Ok(STATUS_OBJECT_NAME_COLLISION)).unwrap_err();
            assert_eq!(error.kind(), io::ErrorKind::AlreadyExists);
        }
        for decided in [
            STATUS_SUCCESS,
            STATUS_ACCESS_DENIED,
            STATUS_OBJECT_NAME_COLLISION,
        ] {
            let classic = Cell::new(false);
            let result = with_fallback(
                || Ok(decided),
                || {
                    classic.set(true);
                    Ok(STATUS_SUCCESS)
                },
            );
            assert!(!classic.get(), "{decided:#x}");
            assert_eq!(result.is_ok(), decided == STATUS_SUCCESS);
        }
    }

    /// NTFS implements the classic classes too, so the fallback requests run
    /// here exactly as a FAT volume would receive them.
    #[test]
    fn classic_delete_and_rename_behave_as_on_fat() {
        use crate::windows::{delete_classic, nt_result, rename_classic};
        use std::os::windows::io::AsRawHandle;
        let root = scratch();
        let dir = Dir::open(root.path()).unwrap();
        drop(dir.create_new_nofollow("first").unwrap());
        drop(dir.create_new_nofollow("second").unwrap());
        let handle = dir.inner.directory.as_raw_handle();

        let source = dir.inner.rename_source("first").unwrap();
        assert_eq!(
            nt_result(rename_classic(&source, handle, "second").unwrap())
                .unwrap_err()
                .kind(),
            io::ErrorKind::AlreadyExists
        );
        nt_result(rename_classic(&source, handle, "renamed").unwrap()).unwrap();
        drop(source);
        assert!(dir.status_nofollow("renamed").unwrap().is_file());
        assert!(!root.path().join("first").exists());

        dir.create_dir("empty").unwrap();
        let source = dir.inner.rename_source("empty").unwrap();
        nt_result(rename_classic(&source, handle, "moved").unwrap()).unwrap();
        drop(source);
        assert!(dir.status_nofollow("moved").unwrap().is_dir());

        // A classic delete keeps the name until the last handle closes.
        let held = dir.open_read_nofollow("renamed").unwrap();
        let victim = dir.inner.rename_source("renamed").unwrap();
        nt_result(delete_classic(&victim).unwrap()).unwrap();
        drop(victim);
        assert!(dir.open_read_nofollow("renamed").is_err());
        drop(held);
        assert_eq!(
            dir.status_nofollow("renamed").unwrap_err().kind(),
            io::ErrorKind::NotFound
        );
    }

    #[test]
    fn a_held_directory_lock_file_cannot_be_removed_or_replaced() {
        let root = scratch();
        let first = Dir::open(root.path()).unwrap();
        let second = Dir::open(root.path()).unwrap();
        first.try_lock().unwrap();
        let path = root.path().join(DIR_LOCK_FILE);
        assert!(second.remove_file(DIR_LOCK_FILE).is_err());
        assert!(std::fs::remove_file(&path).is_err());
        assert!(std::fs::rename(&path, root.path().join("moved")).is_err());
        assert_eq!(
            second.try_lock().unwrap_err().kind(),
            io::ErrorKind::WouldBlock
        );
        first.unlock().unwrap();
        second.try_lock().unwrap();
        drop((first, second));
        std::fs::remove_file(&path).unwrap();
    }

    #[test]
    fn directories_cannot_be_locked_so_callers_lock_a_file_inside() {
        let root = scratch();
        let directory = open_read_nofollow(root.path()).unwrap();
        assert!(status_of(&directory).unwrap().is_dir());
        assert!(matches!(try_lock(&directory), Err(TryLockError::Io(_))));

        let first = Dir::open(root.path()).unwrap();
        let second = Dir::open(root.path()).unwrap();
        let held = first.create_new_nofollow("lock").unwrap();
        let other = second.open_write_nofollow("lock").unwrap();
        try_lock(&held).unwrap();
        assert!(matches!(try_lock(&other), Err(TryLockError::WouldBlock)));
        unlock(&held).unwrap();
        try_lock(&other).unwrap();
    }

    #[test]
    fn locked_and_pinned_entries_can_still_be_inspected() {
        let root = scratch();
        let dir = Dir::open(root.path()).unwrap();
        let lock = dir.create_new_nofollow("lock").unwrap();
        try_lock(&lock).unwrap();
        assert_eq!(
            dir.status_nofollow("lock").unwrap().identity(),
            status_of(&lock).unwrap().identity()
        );
        assert_eq!(
            status_nofollow(root.path()).unwrap().identity(),
            dir.status().unwrap().identity()
        );
        unlock(&lock).unwrap();
    }

    #[test]
    fn whole_file_locks_and_lock_range_locks_exclude_each_other() {
        let root = scratch();
        let path = root.path().join("lock");
        let ours = File::create(&path).unwrap();
        let whole = File::options().write(true).open(&path).unwrap();
        try_lock(&ours).unwrap();
        assert!(matches!(
            whole.try_lock(),
            Err(std::fs::TryLockError::WouldBlock)
        ));
        unlock(&ours).unwrap();
        whole.try_lock().unwrap();
        assert!(matches!(try_lock(&ours), Err(TryLockError::WouldBlock)));
        whole.unlock().unwrap();
        try_lock(&ours).unwrap();
    }
}
