//! T-30: a second owner of the same data directory is refused, and a different
//! data directory runs independently. The cross-process cases (two processes,
//! SIGKILL release, Rust runtime against standalone Node) are in
//! pp-storage/tests/gre_636_advisory_lock.rs.

use pp_storage::lease::StorageLease;

const REFUSAL: &str = "Data directory already owned; stop the other Print Partner process before starting another instance";

fn data_dir() -> tempfile::TempDir {
    tempfile::Builder::new()
        .prefix("pp-t30-")
        .tempdir()
        .unwrap()
}

fn refused(result: anyhow::Result<StorageLease>) -> bool {
    let error = result.err().expect("second owner must be refused");
    error.chain().any(|cause| cause.to_string() == REFUSAL)
}

#[test]
fn ticket_t_30_same_data_directory_admits_one_owner() {
    let data = data_dir();
    let owner = StorageLease::acquire(data.path()).unwrap();
    assert!(refused(StorageLease::acquire(data.path())));
    drop(owner);
    StorageLease::acquire(data.path()).expect("lock released with its owner");
}

#[test]
fn ticket_t_30_different_data_directories_run_independently() {
    let (first, second) = (data_dir(), data_dir());
    let a = StorageLease::acquire(first.path()).unwrap();
    let b = StorageLease::acquire(second.path()).unwrap();
    assert_ne!(a.data_dir(), b.data_dir());
    assert!(refused(StorageLease::acquire(first.path())));
    assert!(refused(StorageLease::acquire(second.path())));
}
