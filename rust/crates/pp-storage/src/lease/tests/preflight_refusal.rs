//! Refused preflight keeps lease files, live storage and recovery copies intact.

use super::*;

/// Writes a reused `printpartner.lock` and a Rust `.desktop-lease` leftover with old mtimes.
fn reused_leftovers(path: &Path) -> [std::time::SystemTime; 2] {
    for (name, bytes) in [
        ("printpartner.lock", &b""[..]),
        (".desktop-lease", b"leftover reservation"),
    ] {
        std::fs::write(path.join(name), bytes).unwrap();
        // Lease files must deny group and other write whatever the umask is.
        std::fs::set_permissions(path.join(name), std::fs::Permissions::from_mode(0o600)).unwrap();
    }
    [
        set_old_mtime(&path.join("printpartner.lock")),
        set_old_mtime(&path.join(".desktop-lease")),
    ]
}

/// Opens the writer with a preflight failure and checks that the data directory,
/// recursively and byte for byte, is as it was before, and that no lock is held.
fn assert_refused_open_leaves_listing(
    path: &Path,
    fail_owned_preflight: bool,
    lease_created: bool,
    expected: &str,
) {
    let before = listing(path);
    RUNTIME_DIR.take();
    // A reservation this call created must go while the advisory lock is still held.
    let created = !path.join(".desktop-lease").exists();
    if created {
        CHECK_LOCK_DURING_RESERVATION_RELEASE.set(Some(path.to_path_buf()));
    }
    FAIL_OWNED_PREFLIGHT.set(fail_owned_preflight);
    let result = crate::WriterOwner::open(path, crate::Limits::default());
    FAIL_OWNED_PREFLIGHT.set(false);
    let error = format!("{:#}", result.err().expect("preflight refused the writer"));
    assert!(error.contains(expected), "{error}");
    if created {
        assert!(CHECK_LOCK_DURING_RESERVATION_RELEASE.take().is_none());
    }
    assert_eq!(listing(path), before);
    // Owned preflight and restore recovery run after the lease creates its
    // runtime directory. Abandon must remove that directory on either refusal.
    match RUNTIME_DIR.take() {
        Some(runtime_dir) => {
            assert!(
                lease_created,
                "the first preflight runs before the lease exists"
            );
            assert!(!runtime_dir.exists(), "{}", runtime_dir.display());
        }
        None => assert!(!lease_created, "the lease recorded no runtime directory"),
    }
    for name in ["printpartner.lock", ".desktop-lease"] {
        if let Ok(file) = File::open(path.join(name)) {
            pp_os::try_lock(&file).unwrap();
        }
    }
}

#[cfg(feature = "desktop_rebuild")]
#[test]
fn second_preflight_failure_keeps_restore_upload_and_stage() {
    let installation = pp_testkit::Installation::create(pp_testkit::Fixture::NodeSchema34).unwrap();
    let path = installation.path();
    let work = path.join(".pp-restore");
    let stage = work.join("staged-0123456789abcdef0123456789abcdef");
    std::fs::create_dir_all(&stage).unwrap();
    std::fs::copy(installation.database_path(), stage.join("print-partner.db")).unwrap();
    std::fs::write(
        work.join("incoming-0123456789abcdef0123456789abcdef.tar.gz"),
        b"interrupted upload",
    )
    .unwrap();
    let database = pp_testkit::node_schema::database_files(path);
    let recovery = pp_testkit::snapshot(&work).unwrap();
    assert_refused_open_leaves_listing(path, true, true, "forced owned preflight failure");
    assert_eq!(pp_testkit::node_schema::database_files(path), database);
    assert_eq!(pp_testkit::snapshot(&work).unwrap(), recovery);
}

#[test]
#[cfg(feature = "desktop_rebuild")]
fn refused_restore_recovery_removes_the_lease_files_it_created() {
    let path = directory();
    let work = path.join(".pp-restore");
    std::fs::create_dir(&work).unwrap();
    std::fs::write(work.join("unknown-backup"), b"preserve this backup").unwrap();

    assert_refused_open_leaves_listing(&path, false, true, "holds unexpected entry unknown-backup");

    std::fs::remove_dir_all(path).unwrap();
}

#[test]
fn gre_1159_failed_preflight_removes_the_lease_files_it_created() {
    // The first preflight, under the lock, refuses a database that is a directory.
    let path = directory();
    std::fs::create_dir(path.join("print-partner.db")).unwrap();
    assert_refused_open_leaves_listing(&path, false, false, "Is a directory");
    std::fs::remove_dir_all(&path).unwrap();
    // The second preflight runs after the lease is held.
    let path = directory();
    assert_refused_open_leaves_listing(&path, true, true, "forced owned preflight failure");
    std::fs::remove_dir_all(path).unwrap();
}

#[test]
fn gre_1159_failed_preflight_leaves_reused_lease_files_unchanged() {
    for owned in [false, true] {
        let path = directory();
        if !owned {
            std::fs::create_dir(path.join("print-partner.db")).unwrap();
        }
        let mtimes = reused_leftovers(&path);
        let expected = if owned {
            "forced owned preflight failure"
        } else {
            "Is a directory"
        };
        assert_refused_open_leaves_listing(&path, owned, owned, expected);
        for (name, modified) in ["printpartner.lock", ".desktop-lease"].iter().zip(mtimes) {
            let metadata = std::fs::metadata(path.join(name)).unwrap();
            assert_eq!(
                metadata.modified().unwrap(),
                modified,
                "{name} owned={owned}"
            );
        }
        std::fs::remove_dir_all(path).unwrap();
    }
}
