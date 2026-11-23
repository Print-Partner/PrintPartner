use super::*;
use crate::node_import::Destination;
use pp_testkit::{Fixture, Installation, LeaseForm, Spec, node_installation, snapshot};
use std::path::PathBuf;

/// A schema 34 Node installation with Builds, Sources and printers.
fn node_source() -> Installation {
    let node = Installation::create(Fixture::NodeSchema34).unwrap();
    node_installation::populate(node.path()).unwrap();
    node
}

/// An installation with `files` (path, bytes) written into it.
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
    let cases = [
        ("empty", holding(Fixture::Empty, &[]), DataFolder::Empty),
        (
            "lease leftovers only",
            holding(Fixture::EmptyWithLeaseLeftovers, &[]),
            DataFolder::Empty,
        ),
        (
            "node",
            holding(Fixture::NodeSchema34, &[]),
            DataFolder::Node,
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
            DataFolder::Unfinished(Unfinished::Staging),
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
        let node = holding(Fixture::NodeSchema34, &[]);
        let link = parent.path().join("link");
        std::os::unix::fs::symlink(node.path(), &link).unwrap();
        assert_eq!(survey(&link), DataFolder::Other, "followed a linked folder");

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
    assert_eq!(survey(node.path()), DataFolder::Node);
    assert_eq!(admit_folder(node.path()), Ok(()));
    assert_eq!(snapshot(node.path()).unwrap(), before);
}

#[test]
fn ticket_t_35_import_node_imports_and_reports_an_installed_destination() {
    let node = node_source();
    let before = snapshot(node.path()).unwrap();
    let parent = Installation::create(Fixture::Empty).unwrap();
    let destination = parent.path().join("new");

    let attempt = import_node(node.path(), &destination);

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
        DataFolder::Installed { imported: true }
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
    let parent = Installation::create(Fixture::Empty).unwrap();
    let destination = parent.path().join("new");

    let attempt = import_node(source.path(), &destination);

    assert!(
        matches!(
            attempt.result,
            AttemptResult::Refused(ImportError::NoDatabase)
        ),
        "{:?}",
        attempt.result
    );
    assert_eq!(attempt.destination, DataFolder::Empty);
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
        Err(StartRefusal(Unfinished::Staging))
    );
    assert_eq!(
        StartRefusal(Unfinished::Staging).to_string(),
        "This data folder holds an import that stopped before it finished. Print Partner does \
         not open it, and the Node folder it came from is unchanged. Import again into a new, \
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
        DataFolder::Unfinished(Unfinished::Staging)
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

#[test]
fn ticket_t_35_stage_name_matches_the_importer() {
    let destination = holding(Fixture::Empty, &[(".pp-import/x", b"")]);
    assert_eq!(
        node_import::inspect_destination(destination.path()),
        Destination::NotEmpty(".pp-import".into())
    );
    assert_eq!(
        survey(destination.path()),
        DataFolder::Unfinished(Unfinished::Staging)
    );
}
