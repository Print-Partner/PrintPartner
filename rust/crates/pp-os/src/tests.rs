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
            ] {
                assert_eq!(error.raw_os_error(), Some(libc::ELOOP), "{name}");
            }
            assert_eq!(
                dir.create_new_nofollow(name).unwrap_err().kind(),
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
            dir.inner.remove_dir("junction").unwrap_err().kind(),
            io::ErrorKind::NotADirectory
        );
        dir.inner.rename("junction", "moved", false).unwrap();
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
        let inner = &dir.inner;

        inner.create_dir("child").unwrap();
        assert_eq!(
            inner.create_dir("child").unwrap_err().kind(),
            io::ErrorKind::AlreadyExists
        );
        assert!(retired.join("child").is_dir());
        assert!(!path.join("child").exists());

        let child = Dir::open(&retired.join("child")).unwrap();
        inner.rename("child", "renamed", false).unwrap();
        drop(child.create_new_nofollow("entry").unwrap());
        assert!(retired.join("renamed").join("entry").is_file());
        assert_eq!(
            inner.remove_dir("renamed").unwrap_err().kind(),
            io::ErrorKind::DirectoryNotEmpty
        );
        child.remove_file("entry").unwrap();
        inner.remove_dir("renamed").unwrap();
        assert!(!retired.join("renamed").exists());
        assert!(child.create_new_nofollow("late").is_err());
        drop(child);

        drop(dir.create_new_nofollow("first").unwrap());
        drop(dir.create_new_nofollow("second").unwrap());
        let first = dir.status_nofollow("first").unwrap().identity();
        assert_eq!(
            inner.rename("first", "second", false).unwrap_err().kind(),
            io::ErrorKind::AlreadyExists
        );
        let held = dir.open_read_nofollow("second").unwrap();
        inner.rename("first", "second", true).unwrap();
        assert_eq!(dir.status_nofollow("second").unwrap().identity(), first);
        assert_ne!(status_of(&held).unwrap().identity(), first);
        assert_eq!(
            dir.status_nofollow("first").unwrap_err().kind(),
            io::ErrorKind::NotFound
        );
        inner.create_dir("read-only").unwrap();
        let mut permissions = std::fs::metadata(retired.join("read-only"))
            .unwrap()
            .permissions();
        permissions.set_readonly(true);
        std::fs::set_permissions(retired.join("read-only"), permissions).unwrap();
        inner.remove_dir("read-only").unwrap();
        assert!(!retired.join("read-only").exists());
        assert!(inner.remove_dir("second").is_err());
        assert!(retired.join("second").is_file());
        assert!(!path.join("second").exists());
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
}
