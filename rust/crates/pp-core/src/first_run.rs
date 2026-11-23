//! First-run setup's one view of a data folder, and its one way to run the
//! Node import. Everything here reads, except [`import_node`], which writes
//! only through `node_import::import` into its destination. Nothing here
//! opens a source for writing, and nothing here empties or deletes a folder.

use crate::node_import::{self, Destination, ImportError, ImportReport, REPORT_FILE};
use pp_storage::ownership_marker::{self, MarkerState};
use std::{fmt, path::Path};

#[cfg(test)]
mod tests;

// Hard-coded because pp-storage keeps the importer's `copy::STAGE` private,
// until a sensitive-path follow-up exports it.
const STAGE: &str = ".pp-import";
const DATABASE: &str = "print-partner.db";
const NODE_LEASE: &str = ".desktop-lease";

/// What a data folder holds, as the runtime and the import see it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DataFolder {
    /// Absent, or a folder the importer accepts as its destination.
    Empty,
    /// A valid ownership marker. `imported` is true when the folder holds
    /// the import's report.
    Installed { imported: bool },
    /// No marker, a database, no Node lease. It is a stopped Node
    /// installation, or an import that failed while moving its copy into
    /// place: the importer removes its stage on that error and has not yet
    /// written its report. Nothing on disk tells the two apart.
    UnmarkedDatabase,
    /// No marker, a Node database and a lease: Node runs there or stopped
    /// without releasing it.
    NodeLeased,
    /// No marker, and the import's stage or its report.
    Unfinished(Unfinished),
    /// Anything else, including a folder that could not be read. The writer
    /// accepts or refuses it with its own reason.
    Other,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Unfinished {
    StagePresent,
    /// The import moved its copy into place and has not written the marker.
    Unmarked,
}

/// Reads `folder` without writing and without creating it. Follows a link at
/// `folder` itself, as startup and the importer do, but not a link at its
/// marker or its top-level names. `Empty` is the importer's own rule.
pub fn survey(folder: &Path) -> DataFolder {
    match folder.metadata() {
        Ok(metadata) if metadata.is_dir() => {}
        // A dangling link is not absent: the importer cannot create a folder there.
        Err(error)
            if error.kind() == std::io::ErrorKind::NotFound
                && folder.symlink_metadata().is_err() =>
        {
            return DataFolder::Empty;
        }
        _ => return DataFolder::Other,
    }
    match ownership_marker::inspect(folder) {
        MarkerState::Valid(_) => {
            let imported = folder
                .join(REPORT_FILE)
                .symlink_metadata()
                .is_ok_and(|metadata| metadata.is_file());
            return DataFolder::Installed { imported };
        }
        MarkerState::Refused(_) => return DataFolder::Other,
        MarkerState::Absent => {}
    }
    let Ok(names) = std::fs::read_dir(folder).and_then(|entries| {
        entries
            .map(|entry| Ok(entry?.file_name().to_string_lossy().into_owned()))
            .collect::<std::io::Result<Vec<_>>>()
    }) else {
        return DataFolder::Other;
    };
    let has = |wanted: &str| names.iter().any(|name| name == wanted);
    if has(STAGE) {
        DataFolder::Unfinished(Unfinished::StagePresent)
    } else if has(REPORT_FILE) {
        DataFolder::Unfinished(Unfinished::Unmarked)
    } else if has(DATABASE) && has(NODE_LEASE) {
        DataFolder::NodeLeased
    } else if has(DATABASE) {
        DataFolder::UnmarkedDatabase
    } else if node_import::inspect_destination(folder) == Destination::Free {
        DataFolder::Empty
    } else {
        DataFolder::Other
    }
}

impl fmt::Display for DataFolder {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Empty => "absent or empty; import again into it or into another empty folder",
            Self::Installed { imported: true } => {
                "an imported installation, ready to start with --data"
            }
            Self::Installed { imported: false } => "a Rust installation, not an import destination",
            Self::UnmarkedDatabase => {
                "a database without an ownership marker: a Node installation, or an import \
                 that failed while moving its copy into place; not an import destination"
            }
            Self::NodeLeased => {
                "a Node installation that is running or did not shut down, not an import destination"
            }
            Self::Unfinished(_) => {
                "an import that did not finish or is still running; Rust-only startup refuses \
                 it; import again into a new, empty folder"
            }
            Self::Other => "not an import destination",
        })
    }
}

/// One import attempt and the state it left its destination in.
#[derive(Debug)]
pub struct Attempt {
    pub result: AttemptResult,
    pub destination: LeftBehind,
}

/// What an import attempt left at its destination.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LeftBehind {
    /// What [`survey`] reads there now.
    Surveyed(DataFolder),
    /// Empty before this attempt and an unmarked database after it: the
    /// importer failed after moving the database in. [`survey`] alone cannot
    /// tell this folder from a Node installation.
    PartialCopy,
}

impl fmt::Display for LeftBehind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Surveyed(folder) => write!(f, "{folder}"),
            Self::PartialCopy => f.write_str(
                "this import's partial copy, without an ownership marker; not an import \
                 destination; import again into a new, empty folder",
            ),
        }
    }
}

#[derive(Debug)]
pub enum AttemptResult {
    Imported(ImportReport),
    /// The source or destination was refused as it is.
    Refused(ImportError),
    /// Reading or writing failed partway. The source is unchanged.
    Failed(ImportError),
}

impl From<ImportError> for AttemptResult {
    fn from(error: ImportError) -> Self {
        match error {
            ImportError::Io { .. }
            | ImportError::Sqlite(_)
            | ImportError::CopyMismatch(_)
            | ImportError::Flush { .. } => Self::Failed(error),
            _ => Self::Refused(error),
        }
    }
}

impl fmt::Display for AttemptResult {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Imported(report) => write!(f, "{report}"),
            Self::Refused(error) => write!(f, "import refused: {error}"),
            Self::Failed(error) => write!(f, "import failed: {error}"),
        }
    }
}

/// Runs `node_import::import` once and says what it left at `destination`.
/// The importer keeps every refusal and limit it has. Synchronous and long
/// (minutes per 10 GB): call it off the async runtime.
pub fn import_node(source: &Path, destination: &Path) -> Attempt {
    attempt_with(destination, || node_import::import(source, destination))
}

fn attempt_with(
    destination: &Path,
    run: impl FnOnce() -> Result<ImportReport, ImportError>,
) -> Attempt {
    let before = survey(destination);
    let result = run().map_or_else(AttemptResult::from, AttemptResult::Imported);
    let destination = match (before, survey(destination)) {
        (DataFolder::Empty, DataFolder::UnmarkedDatabase) => LeftBehind::PartialCopy,
        (_, after) => LeftBehind::Surveyed(after),
    };
    Attempt {
        result,
        destination,
    }
}

/// Why startup did not open a data folder: it holds an import that did not
/// finish or is still running.
/// Holds no path, so a dialog can show it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct StartRefusal(pub Unfinished);

impl fmt::Display for StartRefusal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(
            "This data folder holds an import that did not finish or is still running. Print \
             Partner does not open it, and the Node folder it came from is unchanged. Import again \
             into a new, empty folder.",
        )
    }
}

impl std::error::Error for StartRefusal {}

/// Refuses a folder that holds the import's stage or its report, before
/// startup touches the folder. Only Rust-only startup (`desktop_rebuild` and
/// `PP_DESKTOP_REBUILD=1`) calls this; a Node-backed start does not. Every
/// other folder goes on to the writer's marker decision. An import that
/// failed while moving its copy into place is a
/// [`DataFolder::UnmarkedDatabase`] and goes on too: release builds refuse it
/// at the marker check, and builds with `test-unmarked-writer` open it, as
/// they open Node folders on purpose.
pub fn admit_folder(folder: &Path) -> Result<(), StartRefusal> {
    match survey(folder) {
        DataFolder::Unfinished(stage) => Err(StartRefusal(stage)),
        DataFolder::Empty
        | DataFolder::Installed { .. }
        | DataFolder::UnmarkedDatabase
        | DataFolder::NodeLeased
        | DataFolder::Other => Ok(()),
    }
}
