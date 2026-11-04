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

    #[test]
    fn open_directory_cannot_be_renamed_or_removed() {
        let root = scratch();
        let path = root.path().join("store");
        std::fs::create_dir(&path).unwrap();
        let dir = Dir::open(&path).unwrap();
        let clone = dir.try_clone().unwrap();
        assert!(std::fs::rename(&path, root.path().join("retired")).is_err());
        assert!(std::fs::remove_dir(&path).is_err());
        drop(dir.create_new_nofollow("entry").unwrap());
        assert!(path.join("entry").is_file());
        dir.remove_file("entry").unwrap();
        drop(dir);
        assert!(std::fs::rename(&path, root.path().join("retired")).is_err());
        drop(clone);
        std::fs::rename(&path, root.path().join("retired")).unwrap();
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
