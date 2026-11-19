//! GRE-1347: `.pp-restore` swapped for a link to a directory outside the
//! data directory after `Work` opened it, the check every later step relies
//! on. Each test swaps through the `after_work_check` hook and shows that
//! nothing outside the data directory is written or removed.

use super::*;

/// Where the swap moves the real `.pp-restore`, inside the data directory.
const MOVED: &str = ".pp-restore-moved";

/// The next `Work` open for `root` is followed by a swap: `.pp-restore` moves
/// to [`MOVED`] and a link to `outside` takes its name, as a concurrent local
/// process could do.
fn swap_after_check(root: &Path, outside: &Path) {
    let (data, target) = (root.to_owned(), outside.to_owned());
    AFTER_WORK_CHECK.lock().unwrap().push((
        root.to_owned(),
        Box::new(move || {
            std::fs::rename(data.join(WORK_DIR), data.join(MOVED)).unwrap();
            std::os::unix::fs::symlink(target, data.join(WORK_DIR)).unwrap();
        }),
    ));
}

/// Removes the link and moves the real directory back.
fn undo_swap(root: &Path) {
    std::fs::remove_file(root.join(WORK_DIR)).unwrap();
    std::fs::rename(root.join(MOVED), root.join(WORK_DIR)).unwrap();
}

/// A directory outside every data directory holding `keep` and the names a
/// path-following restore would touch: an upload, a stage and a journal.
fn decoy(id: &str) -> (tempfile::TempDir, BTreeMap<PathBuf, Option<Vec<u8>>>) {
    let outside = tempfile::tempdir().unwrap();
    let path = outside.path();
    std::fs::write(path.join("keep"), b"keep").unwrap();
    std::fs::write(
        path.join(format!("{INCOMING_PREFIX}{id}{INCOMING_SUFFIX}")),
        b"keep",
    )
    .unwrap();
    std::fs::create_dir(path.join(format!("{STAGED_PREFIX}{id}"))).unwrap();
    std::fs::write(path.join(format!("{STAGED_PREFIX}{id}/keep")), b"keep").unwrap();
    std::fs::write(path.join("journal.json"), b"keep").unwrap();
    let before = pp_testkit::snapshot(path).unwrap();
    (outside, before)
}

fn names(path: &Path) -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(path)
        .unwrap()
        .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    names
}

/// An upload is written into, and removed from, the directory its open
/// checked. A stage is extracted there too, and SQLite, which needs a
/// pathname, is refused once the path no longer names that directory.
#[test]
fn gre_1347_staging_after_a_swap_for_a_link_writes_and_removes_only_inside_the_data_directory() {
    let (_installation, root) = installation();
    let owner = open(&root).unwrap();
    let client = owner.installation().unwrap();
    let scratch = tempfile::tempdir().unwrap();
    let (backup, _) = backup_then_change(&owner, &root, scratch.path());
    let bytes = std::fs::read(&backup).unwrap();
    let (outside, before) = decoy(&random_id());

    swap_after_check(&root, outside.path());
    let mut upload = client.begin_upload(1 << 30).unwrap();
    upload.write(&bytes).unwrap();
    let moved = root.join(MOVED);
    assert_eq!(std::fs::read(moved.join(&upload.name)).unwrap(), bytes);
    assert_eq!(pp_testkit::snapshot(outside.path()).unwrap(), before);
    drop(upload);
    assert!(
        names(&moved).is_empty(),
        "the upload is removed where it was written"
    );
    assert_eq!(pp_testkit::snapshot(outside.path()).unwrap(), before);
    undo_swap(&root);

    let mut upload = client.begin_upload(1 << 30).unwrap();
    upload.write(&bytes).unwrap();
    swap_after_check(&root, outside.path());
    let error = client.stage(upload).unwrap_err();
    assert!(
        matches!(refusal(&error), Some(Refusal::InvalidArchive(detail))
            if detail.contains("no longer names the directory")),
        "{error:#}"
    );
    assert!(
        names(&moved).is_empty(),
        "the refused stage and its upload are removed through their handles"
    );
    assert_eq!(pp_testkit::snapshot(outside.path()).unwrap(), before);
    undo_swap(&root);
    owner.shutdown().unwrap();
}

/// Open clears leftovers through the handle it checked, and refuses to
/// remove `.pp-restore` once a link has its name.
#[test]
fn gre_1347_recover_after_a_swap_for_a_link_removes_nothing_outside_the_data_directory() {
    let (_installation, root) = installation();
    open(&root).unwrap().shutdown().unwrap();
    let live = state(&root);
    let id = random_id();
    let (outside, before) = decoy(&id);
    let work = root.join(WORK_DIR);
    std::fs::create_dir(&work).unwrap();
    std::fs::write(
        work.join(format!("{INCOMING_PREFIX}{id}{INCOMING_SUFFIX}")),
        b"x",
    )
    .unwrap();
    std::fs::create_dir(work.join(format!("{STAGED_PREFIX}{id}"))).unwrap();
    std::fs::write(work.join(format!("{STAGED_PREFIX}{id}/file")), b"x").unwrap();

    swap_after_check(&root, outside.path());
    let error = open(&root).err().unwrap();
    assert!(
        error.to_string().contains("could not be cleared"),
        "{error:#}"
    );
    assert_eq!(pp_testkit::snapshot(outside.path()).unwrap(), before);
    assert!(
        names(&root.join(MOVED)).is_empty(),
        "leftovers are removed through the handle"
    );
    assert_eq!(state(&root), live);
    undo_swap(&root);
    open(&root).unwrap().shutdown().unwrap();
    assert!(!work.exists());
}

/// A crash mid-activation leaves a journal. Swapped after the check, its
/// replay is refused before anything moves, because the prior database's
/// schema check finds the path no longer names the handle. Undone, the
/// replay restores the installation.
#[test]
fn gre_1347_journal_replay_after_a_swap_for_a_link_touches_nothing_outside() {
    let (_installation, root) = installation();
    let owner = open(&root).unwrap();
    let client = owner.installation().unwrap();
    let scratch = tempfile::tempdir().unwrap();
    let (backup, _) = backup_then_change(&owner, &root, scratch.path());
    let live = state(&root);
    let staged = stage(&client, &backup).unwrap();
    // Crash after the journal is published and the database moved aside.
    STEPS.lock().unwrap().push((root.clone(), 3, true));
    let error = client
        .activate(&staged.restore_id, CONFIRMATION)
        .unwrap_err();
    assert!(matches!(error, RestoreError::RestartRequired), "{error:#}");
    owner.shutdown().unwrap();
    assert!(!root.join(archive::DATABASE_FILE).exists());
    let (outside, before) = decoy(&staged.restore_id);
    let journal = std::fs::read(work_path(&root).join("journal.json")).unwrap();

    swap_after_check(&root, outside.path());
    let error = open(&root).err().unwrap();
    assert!(
        format!("{error:#}").contains("no longer names the directory"),
        "{error:#}"
    );
    assert_eq!(pp_testkit::snapshot(outside.path()).unwrap(), before);
    assert_eq!(
        std::fs::read(root.join(MOVED).join("journal.json")).unwrap(),
        journal
    );
    undo_swap(&root);
    open(&root).unwrap().shutdown().unwrap();
    assert_eq!(state(&root), live);
    assert!(!work_path(&root).exists());
}

fn work_path(root: &Path) -> PathBuf {
    root.join(WORK_DIR)
}
