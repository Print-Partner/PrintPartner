//! T-53 (GRE-388): CPU ticks, package size and UTC timestamps for the perf harness.
use std::fs;
use std::path::Path;
use std::time::{Duration, UNIX_EPOCH};

use pp_perf::{PssError, package_bytes, tree_cpu_ticks_at, utc_timestamp};

/// A procfs `stat` line with the given parent and CPU ticks in fields 14-15.
fn fake_stat(proc_root: &Path, pid: u32, comm: &str, parent: u32, utime: u64, stime: u64) {
    let dir = proc_root.join(pid.to_string());
    fs::create_dir_all(&dir).unwrap();
    fs::write(
        dir.join("stat"),
        format!("{pid} ({comm}) S {parent} {pid} {pid} 0 -1 4194560 10 0 0 0 {utime} {stime} 0 0 20 0 1 0\n"),
    )
    .unwrap();
}

#[test]
fn ticket_t_53_cpu_ticks_sum_the_root_and_its_descendants_only() {
    let proc_root = tempfile::tempdir().unwrap();
    let root = proc_root.path();
    fake_stat(root, 1, "systemd", 0, 9_000, 9_000);
    fake_stat(root, 100, "pp-server", 1, 40, 7);
    fake_stat(root, 101, "node) S 1 (", 100, 300, 25);
    fake_stat(root, 102, "pp worker", 101, 3, 2);
    fake_stat(root, 200, "unrelated", 1, 5_000, 5_000);
    fs::create_dir_all(root.join("self")).unwrap();

    assert_eq!(tree_cpu_ticks_at(root, 100), Ok(40 + 7 + 300 + 25 + 3 + 2));
    assert_eq!(tree_cpu_ticks_at(root, 102), Ok(5));
}

#[test]
fn ticket_t_53_cpu_ticks_report_a_missing_root_and_malformed_stat() {
    let proc_root = tempfile::tempdir().unwrap();
    let root = proc_root.path();
    assert_eq!(tree_cpu_ticks_at(root, 7), Err(PssError::RootMissing(7)));

    let dir = root.join("7");
    fs::create_dir_all(&dir).unwrap();
    fs::write(dir.join("stat"), "7 (short) S 1 7 7 0\n").unwrap();
    assert!(
        matches!(tree_cpu_ticks_at(root, 7), Err(PssError::Malformed { detail, .. }) if detail == "no utime")
    );
    fs::write(dir.join("stat"), "7 (bad) S 1 7 7 0 -1 0 0 0 0 0 lots 1\n").unwrap();
    assert!(
        matches!(tree_cpu_ticks_at(root, 7), Err(PssError::Malformed { detail, .. }) if detail == "utime \"lots\" is not a number")
    );
}

#[test]
fn ticket_t_53_package_bytes_sums_files_without_following_symlinks() {
    let dir = tempfile::tempdir().unwrap();
    let package = dir.path().join("Print Partner.app");
    fs::create_dir_all(package.join("Contents/MacOS")).unwrap();
    fs::write(package.join("Contents/Info.plist"), vec![0u8; 300]).unwrap();
    fs::write(package.join("Contents/MacOS/pp-desktop"), vec![0u8; 4_000]).unwrap();
    let outside = dir.path().join("outside.bin");
    fs::write(&outside, vec![0u8; 1_000_000]).unwrap();
    #[cfg(unix)]
    std::os::unix::fs::symlink(&outside, package.join("Contents/link")).unwrap();

    let link_bytes = if cfg!(unix) {
        fs::symlink_metadata(package.join("Contents/link"))
            .unwrap()
            .len()
    } else {
        0
    };
    assert_eq!(package_bytes(&package).unwrap(), 4_300 + link_bytes);
    assert_eq!(
        package_bytes(&package.join("Contents/MacOS/pp-desktop")).unwrap(),
        4_000
    );
    assert!(package_bytes(&dir.path().join("missing")).is_err());
}

#[test]
fn ticket_t_53_timestamps_are_utc_iso_8601() {
    let at = |seconds| utc_timestamp(UNIX_EPOCH + Duration::from_secs(seconds));
    assert_eq!(at(0), "1970-01-01T00:00:00Z");
    assert_eq!(at(951_782_400), "2000-02-29T00:00:00Z");
    assert_eq!(at(1_793_271_241), "2026-10-29T10:54:01Z");
    assert_eq!(at(4_102_444_799), "2099-12-31T23:59:59Z");
}
