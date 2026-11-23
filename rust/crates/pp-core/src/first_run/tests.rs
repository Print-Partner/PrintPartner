use super::*;
use pp_testkit::{Fixture, Installation, LeaseForm, Spec, node_installation, snapshot};
use std::path::PathBuf;

fn node_source() -> Installation {
    let node = Installation::create(Fixture::NodeSchema34).unwrap();
    node_installation::populate(node.path()).unwrap();
    node
}

fn import_into_a_new_folder(source: &Path) -> (Installation, PathBuf, Attempt) {
    let parent = Installation::create(Fixture::Empty).unwrap();
    let destination = parent.path().join("new");
    let attempt = import_node(source, &destination);
    (parent, destination, attempt)
}

fn leave_as_a_publish_that_failed_after_moving_every_root(destination: &std::path::Path) {
    std::fs::remove_file(destination.join(REPORT_FILE)).unwrap();
    std::fs::remove_file(destination.join(ownership_marker::MARKER_FILE)).unwrap();
}

fn holding(spec: impl Into<Spec>, files: &[(&str, &[u8])]) -> Installation {
    let installation = Installation::create(spec).unwrap();
    for (name, bytes) in files {
        let path = installation.path().join(name);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, bytes).unwrap();
    }
    installation
}

fn absent(path: &Path) -> bool {
    path.symlink_metadata()
        .is_err_and(|error| error.kind() == std::io::ErrorKind::NotFound)
}

#[test]
fn ticket_t_35_survey_names_each_folder_state_without_writing() {
    let node_leased = Spec {
        node_database: true,
        lease_leftovers: Some(LeaseForm::NodeDirectory),
        ..Spec::default()
    };
    let live_owner = format!("{{\"pid\": {}}}", std::process::id());
    let cases = [
        ("empty", holding(Fixture::Empty, &[]), DataFolder::Empty),
        (
            "lease leftovers only",
            holding(Fixture::EmptyWithLeaseLeftovers, &[]),
            DataFolder::Other,
        ),
        (
            "an entry inside the lease",
            holding(Fixture::Empty, &[(".desktop-lease/extra", b"x")]),
            DataFolder::Other,
        ),
        (
            "a live owner record",
            holding(
                Fixture::Empty,
                &[(".desktop-owner.json", live_owner.as_bytes())],
            ),
            DataFolder::Other,
        ),
        (
            "an unreadable owner record",
            holding(Fixture::Empty, &[(".desktop-owner.json", b"not json")]),
            DataFolder::Other,
        ),
        (
            "node",
            holding(Fixture::NodeSchema34, &[]),
            DataFolder::UnmarkedDatabase,
        ),
        (
            "node leased",
            holding(node_leased, &[]),
            DataFolder::NodeLeased,
        ),
        (
            "stopped while staging",
            holding(
                Fixture::Empty,
                &[(".pp-import/print-partner.db", b"partial")],
            ),
            DataFolder::Unfinished(Unfinished::StagePresent),
        ),
        (
            "stopped before the marker",
            holding(Fixture::NodeSchema34, &[("import-report.json", b"{}")]),
            DataFolder::Unfinished(Unfinished::Unmarked),
        ),
        (
            "marked",
            holding(Fixture::RustMarked, &[]),
            DataFolder::Installed { imported: false },
        ),
        (
            "marked and imported",
            holding(Fixture::RustMarked, &[("import-report.json", b"{}")]),
            DataFolder::Installed { imported: true },
        ),
        (
            "bad marker",
            holding(Fixture::MarkedCorrupt, &[]),
            DataFolder::Other,
        ),
        (
            "other",
            holding(Fixture::Empty, &[("notes.txt", b"x")]),
            DataFolder::Other,
        ),
    ];
    for (case, installation, expected) in cases {
        let before = snapshot(installation.path()).unwrap();
        assert_eq!(survey(installation.path()), expected, "{case}");
        assert_eq!(
            survey(installation.path()) == DataFolder::Empty,
            node_import::inspect_destination(installation.path()) == Destination::Free,
            "{case}: survey and the importer disagree on empty"
        );
        assert_eq!(
            snapshot(installation.path()).unwrap(),
            before,
            "{case}: folder changed"
        );
    }

    let parent = Installation::create(Fixture::Empty).unwrap();
    let missing = parent.path().join("missing");
    assert_eq!(survey(&missing), DataFolder::Empty);
    assert!(absent(&missing), "survey created the folder");

    let file = holding(Fixture::Empty, &[("data", b"x")]);
    assert_eq!(survey(&file.path().join("data")), DataFolder::Other);

    #[cfg(unix)]
    {
        let link_to = |target: &Path, name: &str| {
            let link = parent.path().join(name);
            std::os::unix::fs::symlink(target, &link).unwrap();
            link
        };
        let node = holding(Fixture::NodeSchema34, &[]);
        assert_eq!(
            survey(&link_to(node.path(), "node")),
            DataFolder::UnmarkedDatabase
        );
        let stopped = holding(Fixture::Empty, &[(".pp-import/x", b"")]);
        let linked_stopped = link_to(stopped.path(), "stopped");
        assert_eq!(
            admit_folder(&linked_stopped),
            Err(StartRefusal(Unfinished::StagePresent))
        );
        let empty = holding(Fixture::Empty, &[]);
        assert_eq!(survey(&link_to(empty.path(), "empty")), DataFolder::Empty);
        assert_eq!(
            survey(&link_to(&parent.path().join("nowhere"), "dangling")),
            DataFolder::Other,
            "a dangling link is not absent"
        );

        let marked = holding(Fixture::RustMarked, &[]);
        let linked_marker = holding(Fixture::Empty, &[]);
        std::os::unix::fs::symlink(
            marked.path().join(ownership_marker::MARKER_FILE),
            linked_marker.path().join(ownership_marker::MARKER_FILE),
        )
        .unwrap();
        assert_eq!(
            survey(linked_marker.path()),
            DataFolder::Other,
            "followed a linked marker"
        );
    }
}

#[test]
fn ticket_t_35_survey_and_admit_accept_node_folders() {
    let node = holding(Fixture::NodeSchema34, &[]);
    let before = snapshot(node.path()).unwrap();
    assert_eq!(survey(node.path()), DataFolder::UnmarkedDatabase);
    assert_eq!(admit_folder(node.path()), Ok(()));
    assert_eq!(snapshot(node.path()).unwrap(), before);
}

#[test]
fn ticket_t_35_survey_cannot_tell_a_failed_publish_from_node() {
    let node = node_source();
    let (_parent, destination, attempt) = import_into_a_new_folder(node.path());
    assert!(
        matches!(attempt.result, AttemptResult::Imported(_)),
        "{}",
        attempt.result
    );
    leave_as_a_publish_that_failed_after_moving_every_root(&destination);

    assert!(absent(&destination.join(STAGE)), "the stage is present");
    assert_eq!(survey(&destination), DataFolder::UnmarkedDatabase);
    assert_eq!(
        survey(&destination).to_string(),
        "a database without an ownership marker: a Node installation, or an import that \
         failed while moving its copy into place; not an import destination"
    );
    assert_eq!(
        admit_folder(&destination),
        Ok(()),
        "first-run cannot tell this folder from a stopped Node installation, which test \
         builds open on purpose; release builds refuse it at the writer's marker check"
    );

    let retry = import_node(node.path(), &destination);
    assert_eq!(
        retry.destination,
        LeftBehind::Surveyed(DataFolder::UnmarkedDatabase),
        "the folder held a database before this import"
    );
    assert_eq!(
        retry.destination.to_string(),
        DataFolder::UnmarkedDatabase.to_string()
    );
}

#[test]
fn ticket_t_35_import_names_its_own_partial_copy() {
    let parent = Installation::create(Fixture::Empty).unwrap();
    let destination = parent.path().join("new");

    let attempt = attempt_with(&destination, || {
        std::fs::create_dir(&destination).unwrap();
        std::fs::write(destination.join(DATABASE), b"moved").unwrap();
        Err(ImportError::Destination(Destination::NotEmpty(
            "repos".into(),
        )))
    });

    assert!(
        matches!(attempt.result, AttemptResult::Refused(_)),
        "{:?}",
        attempt.result
    );
    assert_eq!(attempt.destination, LeftBehind::PartialCopy);
    assert_eq!(
        attempt.destination.to_string(),
        "this import's partial copy, without an ownership marker; not an import destination; \
         import again into a new, empty folder"
    );
}

#[test]
fn ticket_t_35_import_node_imports_and_reports_an_installed_destination() {
    let node = node_source();
    let before = snapshot(node.path()).unwrap();

    let (_parent, destination, attempt) = import_into_a_new_folder(node.path());

    let AttemptResult::Imported(report) = &attempt.result else {
        panic!("not imported: {}", attempt.result);
    };
    assert_eq!(
        (
            report.complete,
            report.source_schema,
            report.builds,
            report.sources,
            report.printers,
            report.unmatched_prints
        ),
        (true, 34, 3, 2, 2, 2)
    );
    assert_eq!(
        attempt.destination,
        LeftBehind::Surveyed(DataFolder::Installed { imported: true })
    );
    assert_eq!(
        attempt.destination.to_string(),
        "an imported installation, ready to start with --data"
    );
    assert_eq!(admit_folder(&destination), Ok(()));
    assert_eq!(snapshot(node.path()).unwrap(), before, "source changed");
}

#[test]
fn ticket_t_35_refused_import_leaves_the_destination_empty_and_the_source_unchanged() {
    let source = holding(Fixture::Empty, &[]);
    let before = snapshot(source.path()).unwrap();

    let (_parent, destination, attempt) = import_into_a_new_folder(source.path());

    assert!(
        matches!(
            attempt.result,
            AttemptResult::Refused(ImportError::NoDatabase)
        ),
        "{:?}",
        attempt.result
    );
    assert_eq!(attempt.destination, LeftBehind::Surveyed(DataFolder::Empty));
    assert_eq!(
        std::fs::read_dir(&destination).unwrap().count(),
        0,
        "a refused import left entries in its new destination"
    );
    assert_eq!(snapshot(source.path()).unwrap(), before, "source changed");
}

#[test]
fn ticket_t_35_stopped_import_is_refused_by_startup_and_by_a_retry() {
    let node = node_source();
    let source_before = snapshot(node.path()).unwrap();
    let stopped = holding(
        Fixture::Empty,
        &[(".pp-import/print-partner.db", b"partial")],
    );
    let stopped_before = snapshot(stopped.path()).unwrap();

    assert_eq!(
        admit_folder(stopped.path()),
        Err(StartRefusal(Unfinished::StagePresent))
    );
    assert_eq!(
        StartRefusal(Unfinished::StagePresent).to_string(),
        "This data folder holds an import that did not finish or is still running. Print \
         Partner does not open it, and the Node folder it came from is unchanged. Import again into a new, \
         empty folder."
    );
    let retry = import_node(node.path(), stopped.path());
    assert!(
        matches!(
            &retry.result,
            AttemptResult::Refused(ImportError::Destination(Destination::NotEmpty(entry)))
                if entry == ".pp-import"
        ),
        "{:?}",
        retry.result
    );
    assert_eq!(
        retry.destination,
        LeftBehind::Surveyed(DataFolder::Unfinished(Unfinished::StagePresent))
    );
    assert_eq!(
        retry.destination.to_string(),
        "an import that did not finish or is still running; Rust-only startup refuses it; \
         import again into a new, empty folder"
    );
    assert_eq!(
        snapshot(stopped.path()).unwrap(),
        stopped_before,
        "stopped import changed"
    );

    let unmarked = holding(Fixture::NodeSchema34, &[("import-report.json", b"{}")]);
    assert_eq!(
        admit_folder(unmarked.path()),
        Err(StartRefusal(Unfinished::Unmarked))
    );

    let parent = Installation::create(Fixture::Empty).unwrap();
    let fresh: PathBuf = parent.path().join("fresh");
    let attempt = import_node(node.path(), &fresh);
    assert!(
        matches!(attempt.result, AttemptResult::Imported(_)),
        "{}",
        attempt.result
    );
    assert_eq!(
        snapshot(node.path()).unwrap(),
        source_before,
        "source changed"
    );
}
