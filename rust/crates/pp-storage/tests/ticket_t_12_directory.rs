use pp_storage::{Limits, WriterOwner, lease::StorageLease};
use std::os::unix::fs::DirBuilderExt;
use std::{
    os::unix::fs::{MetadataExt, PermissionsExt, symlink},
    path::{Path, PathBuf},
};

#[path = "common/process.rs"]
mod child_process;

struct Copy(PathBuf);

impl Copy {
    fn new() -> Self {
        Self(std::env::temp_dir().join(format!(
            "pp-t12-directory-{}",
            hex::encode(rand::random::<[u8; 16]>())
        )))
    }
}

impl Drop for Copy {
    fn drop(&mut self) {
        if self.0.exists() {
            mode(&self.0, 0o700);
            std::fs::remove_dir_all(&self.0).unwrap();
        }
    }
}

fn mode(path: &Path, mode: u32) {
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode)).unwrap();
}

#[test]
#[cfg(target_os = "linux")]
fn ticket_t_12_lock_acl_write_elevation_is_refused() {
    use std::os::unix::ffi::OsStrExt;
    let copy = Copy::new();
    std::fs::DirBuilder::new()
        .mode(0o700)
        .create(&copy.0)
        .unwrap();
    let lock = copy.0.join("printpartner.lock");
    let database = copy.0.join("print-partner.db");
    std::fs::write(&lock, b"existing lock").unwrap();
    mode(&lock, 0o600);
    std::fs::write(&database, b"must remain unopened").unwrap();
    // Linux POSIX ACL xattr: grant a foreign named user write through the mask.
    let mut acl = 2u32.to_le_bytes().to_vec();
    let foreign_uid = if unsafe { libc::geteuid() } == 65534 {
        65533
    } else {
        65534
    };
    for (tag, permissions, uid) in [
        (1u16, 6u16, u32::MAX),
        (2, 2, foreign_uid),
        (4, 0, u32::MAX),
        (16, 2, u32::MAX),
        (32, 0, u32::MAX),
    ] {
        acl.extend(tag.to_le_bytes());
        acl.extend(permissions.to_le_bytes());
        acl.extend(uid.to_le_bytes());
    }
    let path = std::ffi::CString::new(lock.as_os_str().as_bytes()).unwrap();
    assert_eq!(
        unsafe {
            libc::setxattr(
                path.as_ptr(),
                c"system.posix_acl_access".as_ptr(),
                acl.as_ptr().cast(),
                acl.len(),
                0,
            )
        },
        0,
        "{}",
        std::io::Error::last_os_error()
    );
    assert_eq!(std::fs::metadata(&lock).unwrap().mode() & 0o777, 0o620);
    for error in [
        StorageLease::acquire(&copy.0).err().unwrap(),
        WriterOwner::open(&copy.0, Limits::default()).err().unwrap(),
    ] {
        assert_eq!(error.to_string(), "Unsafe data directory lock");
    }
    assert_eq!(std::fs::read(&lock).unwrap(), b"existing lock");
    assert_eq!(std::fs::read(&database).unwrap(), b"must remain unopened");
    assert_eq!(std::fs::read_dir(&copy.0).unwrap().count(), 2);
    assert_eq!(
        unsafe { libc::removexattr(path.as_ptr(), c"system.posix_acl_access".as_ptr()) },
        0
    );
    mode(&lock, 0o600);
    assert!(
        StorageLease::acquire(&copy.0)
            .unwrap()
            .release()
            .lock_released
    );
}

#[test]
fn ticket_t_12_hostile_lock_entries_refuse_before_preflight_or_cleanup() {
    for (name, kind) in ["printpartner.lock"].into_iter().flat_map(|name| {
        [
            "symlink",
            "dangling",
            "hardlink",
            "directory",
            "fifo",
            "group-write",
        ]
        .map(|kind| (name, kind))
    }) {
        let copy = Copy::new();
        std::fs::DirBuilder::new()
            .mode(0o700)
            .create(&copy.0)
            .unwrap();
        let database = copy.0.join("print-partner.db");
        let config = copy.0.join("desktop.toml");
        let marker = copy.0.join(".desktop-owner.json");
        let target = copy.0.join("target");
        let lock = copy.0.join(name);
        std::fs::write(&database, b"invalid database: preflight must not run").unwrap();
        std::fs::write(&config, b"port = 43123\n").unwrap();
        std::fs::write(&marker, br#"{"pid":2147483647,"kind":"standalone"}"#).unwrap();
        std::fs::write(&target, b"external lock target").unwrap();
        mode(&target, 0o600);
        match kind {
            "symlink" => symlink(&target, &lock).unwrap(),
            "dangling" => symlink(copy.0.join("absent"), &lock).unwrap(),
            "hardlink" => std::fs::hard_link(&target, &lock).unwrap(),
            "directory" => std::fs::create_dir(&lock).unwrap(),
            "fifo" => {
                use std::os::unix::ffi::OsStrExt;
                let name = std::ffi::CString::new(lock.as_os_str().as_bytes()).unwrap();
                assert_eq!(unsafe { libc::mkfifo(name.as_ptr(), 0o600) }, 0);
            }
            "group-write" => {
                std::fs::write(&lock, b"hostile lock").unwrap();
                mode(&lock, 0o660);
            }
            _ => unreachable!(),
        }
        let before =
            [&database, &config, &marker, &target].map(|path| std::fs::read(path).unwrap());
        let lock_before = std::fs::symlink_metadata(&lock).unwrap();
        for error in [
            StorageLease::acquire(&copy.0).err().expect("lease refused"),
            WriterOwner::open(&copy.0, Limits::default())
                .err()
                .expect("writer refused"),
        ] {
            assert_eq!(
                error.to_string(),
                "Unsafe data directory lock",
                "{kind}: {error:#}"
            );
        }
        assert_eq!(
            [&database, &config, &marker, &target].map(|path| std::fs::read(path).unwrap()),
            before
        );
        let lock_after = std::fs::symlink_metadata(&lock).unwrap();
        assert_eq!(
            (lock_before.dev(), lock_before.ino(), lock_before.mode()),
            (lock_after.dev(), lock_after.ino(), lock_after.mode())
        );
        assert!(!copy.0.join(".desktop-lease").exists());
        assert_eq!(std::fs::read_dir(&copy.0).unwrap().count(), 5);
        std::fs::remove_file(&marker).unwrap();
        if kind == "directory" {
            std::fs::remove_dir(&lock).unwrap();
        } else {
            std::fs::remove_file(&lock).unwrap();
        }
        let lease = StorageLease::acquire(&copy.0).expect("repaired lock admitted");
        assert!(lease.release().lock_released);
    }
}

#[test]
fn ticket_t_12_foreign_owned_directory_is_refused() {
    let root = Path::new("/");
    if std::fs::metadata(root).unwrap().uid() == unsafe { libc::geteuid() } {
        return;
    }
    let error = pp_storage::lease::prepare_directory(root).unwrap_err();
    assert_eq!(error.to_string(), "Unsafe data directory permissions");
}

#[test]
fn ticket_t_12_new_store_is_private() {
    let status = child_process::command(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "ticket_t_12_private_store_subprocess",
            "--nocapture",
        ])
        .env("PP_T12_PRIVATE_STORE_PROBE", "1")
        .status()
        .unwrap();
    assert!(status.success());
}

#[test]
fn ticket_t_12_private_store_subprocess() {
    if std::env::var_os("PP_T12_PRIVATE_STORE_PROBE").is_none() {
        return;
    }
    unsafe { libc::umask(0) };
    let copy = Copy::new();
    let nested = copy.0.join("portable/data");
    let owner = StorageLease::acquire(&nested).unwrap();
    for path in [&copy.0, &copy.0.join("portable"), &nested] {
        assert_eq!(
            std::fs::metadata(path).unwrap().permissions().mode() & 0o777,
            0o700
        );
    }
    assert!(owner.release().lock_released);
}

#[test]
fn ticket_t_12_permission_repair_reopens_the_copied_database() {
    let source = Copy::new();
    let (owner, _) = WriterOwner::open(&source.0, Limits::default()).unwrap();
    owner.shutdown().unwrap();
    let copy = Copy::new();
    std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(&copy.0)
        .unwrap();
    let database = copy.0.join("print-partner.db");
    std::fs::copy(source.0.join("print-partner.db"), &database).unwrap();
    let before = std::fs::read(&database).unwrap();
    mode(&copy.0, 0o775);
    let error = WriterOwner::open(&copy.0, Limits::default()).err().unwrap();
    assert_eq!(error.to_string(), "Unsafe data directory permissions");
    assert_eq!(std::fs::read(&database).unwrap(), before);
    assert!(!copy.0.join("printpartner.lock").exists());
    mode(&copy.0, 0o700);
    let (owner, _) = WriterOwner::open(&copy.0, Limits::default()).unwrap();
    owner.shutdown().unwrap();
}

#[test]
fn ticket_t_12_unsafe_permissions_refuse_before_lock_or_database_changes() {
    for permissions in [0o777, 0o775, 0o707, 0o555, 0o600, 0o300] {
        let copy = Copy::new();
        std::fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(&copy.0)
            .unwrap();
        let database = copy.0.join("print-partner.db");
        let config = copy.0.join("desktop.toml");
        std::fs::write(&database, b"copied database, never open this").unwrap();
        std::fs::write(&config, b"port = 43123\n").unwrap();
        mode(&copy.0, permissions);

        let error = StorageLease::acquire(&copy.0)
            .err()
            .expect("unsafe store refused");
        assert!(
            error
                .to_string()
                .contains("Unsafe data directory permissions"),
            "{error:#}"
        );
        let error = WriterOwner::open(&copy.0, Limits::default())
            .err()
            .expect("writer refused before preflight");
        assert!(
            error
                .to_string()
                .contains("Unsafe data directory permissions"),
            "{error:#}"
        );

        mode(&copy.0, 0o700);
        assert_eq!(
            std::fs::read(&database).unwrap(),
            b"copied database, never open this"
        );
        assert_eq!(std::fs::read(&config).unwrap(), b"port = 43123\n");
        let mut entries = std::fs::read_dir(&copy.0)
            .unwrap()
            .map(|entry| entry.unwrap().file_name())
            .collect::<Vec<_>>();
        entries.sort();
        assert_eq!(entries, ["desktop.toml", "print-partner.db"]);
        let owner = StorageLease::acquire(&copy.0).expect("same copy admitted after repair");
        assert!(owner.release().lock_released);
    }
}

#[test]
fn ticket_t_12_legacy_permissions_and_data_alias_are_preserved() {
    let copy = Copy::new();
    std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(&copy.0)
        .unwrap();
    let data = copy.0.join("data");
    std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(&data)
        .unwrap();
    let alias = copy.0.join("alias");
    symlink(&data, &alias).unwrap();
    for permissions in [0o700, 0o750, 0o755] {
        mode(&data, permissions);
        let owner = StorageLease::acquire(&alias).unwrap();
        assert_eq!(owner.data_dir(), data.canonicalize().unwrap());
        assert_eq!(
            std::fs::metadata(&data).unwrap().permissions().mode() & 0o777,
            permissions
        );
        assert!(StorageLease::acquire(&data).is_err());
        assert!(owner.release().lock_released);
    }
}

fn entries(path: &Path) -> Vec<std::ffi::OsString> {
    let mut names = std::fs::read_dir(path)
        .unwrap()
        .map(|entry| entry.unwrap().file_name())
        .collect::<Vec<_>>();
    names.sort();
    names
}

fn files(path: &Path) -> Vec<(std::ffi::OsString, Vec<u8>)> {
    entries(path)
        .into_iter()
        .filter_map(|name| {
            let file = path.join(&name);
            let metadata = std::fs::symlink_metadata(&file).unwrap();
            metadata
                .is_file()
                .then(|| (name, std::fs::read(file).unwrap()))
        })
        .collect()
}

fn copied_store(data: &Path) {
    let source = Copy::new();
    let (owner, _) = WriterOwner::open(&source.0, Limits::default()).unwrap();
    owner.shutdown().unwrap();
    std::fs::copy(
        source.0.join("print-partner.db"),
        data.join("print-partner.db"),
    )
    .unwrap();
    std::fs::write(data.join("desktop.toml"), b"port = 43123\n").unwrap();
    std::fs::write(
        data.join(".desktop-owner.json"),
        br#"{"pid":2147483647,"kind":"standalone"}"#,
    )
    .unwrap();
}

fn assert_ancestor_refused(input: &Path, unchanged: &[&Path]) {
    let before = unchanged
        .iter()
        .map(|path| (entries(path), files(path)))
        .collect::<Vec<_>>();
    for writer in [false, true] {
        let error = if writer {
            WriterOwner::open(input, Limits::default()).err()
        } else {
            StorageLease::acquire(input).err()
        }
        .expect("unsafe ancestor refused");
        assert_eq!(error.to_string(), "Unsafe data directory ancestor");
        assert_eq!(
            unchanged
                .iter()
                .map(|path| (entries(path), files(path)))
                .collect::<Vec<_>>(),
            before
        );
    }
}

fn ancestor_fixture() -> (Copy, PathBuf, PathBuf) {
    let copy = Copy::new();
    let parent = copy.0.join("parent");
    let data = parent.join("data");
    std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(&data)
        .unwrap();
    (copy, parent, data)
}

#[test]
fn ticket_t_12_writable_ancestor_refuses_copied_store_before_preflight() {
    let source = Copy::new();
    let (owner, _) = WriterOwner::open(&source.0, Limits::default()).unwrap();
    owner.shutdown().unwrap();
    for permissions in [0o777, 0o775, 0o707] {
        let (_copy, parent, data) = ancestor_fixture();
        let database = data.join("print-partner.db");
        let config = data.join("desktop.toml");
        let marker = data.join(".desktop-owner.json");
        std::fs::copy(source.0.join("print-partner.db"), &database).unwrap();
        std::fs::write(&config, b"port = 43123\n").unwrap();
        std::fs::write(&marker, br#"{"pid":2147483647,"kind":"standalone"}"#).unwrap();
        let before = [&database, &config, &marker].map(|path| std::fs::read(path).unwrap());
        mode(&parent, permissions);
        assert_ancestor_refused(&data, &[&parent, &data]);
        assert_eq!(
            [&database, &config, &marker].map(|path| std::fs::read(path).unwrap()),
            before
        );
    }
}

#[test]
fn ticket_t_12_unsafe_lexical_alias_parent_is_refused() {
    let (copy, parent, data) = ancestor_fixture();
    let safe = copy.0.join("safe");
    std::fs::rename(&data, &safe).unwrap();
    symlink(&safe, &data).unwrap();
    copied_store(&safe);
    mode(&parent, 0o777);
    assert_ancestor_refused(&data, &[&copy.0, &parent, &safe]);
}

#[test]
fn ticket_t_12_unsafe_resolved_alias_ancestor_is_refused() {
    let (copy, parent, data) = ancestor_fixture();
    let alias = copy.0.join("alias");
    symlink(&data, &alias).unwrap();
    copied_store(&data);
    mode(&parent, 0o777);
    assert_ancestor_refused(&alias, &[&copy.0, &parent, &data]);
}

#[test]
fn ticket_t_12_missing_store_below_unsafe_resolved_alias_is_not_created() {
    let (copy, parent, data) = ancestor_fixture();
    let alias = copy.0.join("alias");
    symlink(&data, &alias).unwrap();
    copied_store(&data);
    mode(&parent, 0o777);
    assert_ancestor_refused(&alias.join("missing/store"), &[&copy.0, &parent, &data]);
}

#[test]
fn ticket_t_12_missing_store_below_writable_ancestor_is_not_created() {
    let (copy, parent, data) = ancestor_fixture();
    mode(&parent, 0o777);
    assert_ancestor_refused(&data.join("missing/store"), &[&copy.0, &parent, &data]);
}

#[test]
fn ticket_t_12_safe_and_sticky_ancestors_admit_private_stores() {
    for permissions in [0o700, 0o750, 0o755, 0o1777] {
        let (copy, parent, data) = ancestor_fixture();
        mode(&parent, permissions);
        let alias = copy.0.join("alias");
        symlink(&data, &alias).unwrap();
        let lease = StorageLease::acquire(&alias.join("missing/store")).unwrap();
        assert_eq!(
            lease.data_dir(),
            data.join("missing/store").canonicalize().unwrap()
        );
        assert!(lease.release().lock_released);
        assert_eq!(
            std::fs::metadata(&parent).unwrap().mode() & 0o7777,
            permissions
        );
        mode(&data, 0o1777);
        assert_eq!(
            StorageLease::acquire(&data).err().unwrap().to_string(),
            "Unsafe data directory permissions"
        );
    }
}

#[test]
fn ticket_t_12_relative_store_checks_full_working_directory_ancestry() {
    let (_copy, parent, data) = ancestor_fixture();
    mode(&parent, 0o777);
    let status = child_process::command(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "ticket_t_12_relative_ancestor_subprocess",
            "--nocapture",
        ])
        .env("PP_T12_RELATIVE_ANCESTOR_PROBE", "1")
        .current_dir(&data)
        .status()
        .unwrap();
    assert!(status.success());
    assert_eq!(entries(&data), Vec::<std::ffi::OsString>::new());
}

#[test]
fn ticket_t_12_relative_ancestor_subprocess() {
    if std::env::var_os("PP_T12_RELATIVE_ANCESTOR_PROBE").is_none() {
        return;
    }
    assert_ancestor_refused(Path::new("missing/store"), &[Path::new(".")]);
}

#[test]
fn ticket_t_12_empty_path_leaves_working_directory_unchanged() {
    let copy = Copy::new();
    std::fs::DirBuilder::new()
        .mode(0o700)
        .create(&copy.0)
        .unwrap();
    std::fs::write(copy.0.join("existing-file"), b"unchanged").unwrap();
    let status = child_process::command(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "ticket_t_12_empty_path_subprocess",
            "--nocapture",
        ])
        .env("PP_T12_EMPTY_PATH_PROBE", "1")
        .current_dir(&copy.0)
        .status()
        .unwrap();
    assert_eq!(entries(&copy.0), ["existing-file"]);
    assert_eq!(
        std::fs::read(copy.0.join("existing-file")).unwrap(),
        b"unchanged"
    );
    assert!(status.success());
}

#[test]
fn ticket_t_12_empty_path_subprocess() {
    if std::env::var_os("PP_T12_EMPTY_PATH_PROBE").is_none() {
        return;
    }
    let input = Path::new("");
    for error in [
        pp_storage::lease::prepare_directory(input).err(),
        StorageLease::acquire(input).err(),
        WriterOwner::open(input, Limits::default()).err(),
    ] {
        assert_eq!(
            error.expect("empty path refused").to_string(),
            "Data directory path is empty"
        );
        assert_eq!(entries(Path::new(".")), ["existing-file"]);
        assert_eq!(std::fs::read("existing-file").unwrap(), b"unchanged");
    }
    assert_eq!(
        pp_storage::lease::prepare_directory(Path::new(".")).unwrap(),
        std::env::current_dir().unwrap()
    );
    let relative = pp_storage::lease::prepare_directory(Path::new("relative-store")).unwrap();
    assert_eq!(
        relative,
        std::env::current_dir().unwrap().join("relative-store")
    );
    std::fs::remove_dir(relative).unwrap();
}

#[test]
fn ticket_t_12_ancestor_lookup_errors_are_not_treated_as_missing() {
    let (copy, _parent, data) = ancestor_fixture();
    let looping = copy.0.join("loop");
    symlink(&looping, &looping).unwrap();
    let file = data.join("file");
    std::fs::write(&file, b"unchanged").unwrap();
    for (input, errno) in [
        (looping.join("missing/store"), libc::ELOOP),
        (file.join("missing/store"), libc::ENOTDIR),
    ] {
        for writer in [false, true] {
            let error = if writer {
                WriterOwner::open(&input, Limits::default()).err()
            } else {
                StorageLease::acquire(&input).err()
            }
            .unwrap();
            assert_eq!(
                error
                    .downcast_ref::<std::io::Error>()
                    .unwrap()
                    .raw_os_error(),
                Some(errno)
            );
        }
    }
    assert_eq!(std::fs::read(&file).unwrap(), b"unchanged");
    assert_eq!(std::fs::read_link(&looping).unwrap(), looping);
    assert_eq!(entries(&data), ["file"]);
}
