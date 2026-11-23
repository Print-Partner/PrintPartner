//! First-run setup's one view of a data folder, and its one way to run the
//! Node import. Everything here reads, except [`import_node`], which writes
//! only through `node_import::import` into its destination. Nothing here
//! opens a source for writing, and nothing here empties or deletes a folder.

use crate::node_import::{self, ImportError, ImportReport, REPORT_FILE};
use pp_storage::ownership_marker::{self, MarkerState};
use std::{fmt, path::Path};

#[cfg(test)]
mod tests;

/// The import's staging folder. pp-storage keeps its constant private;
/// `ticket_t_35_stage_name_matches_the_importer` pins this literal against
/// the importer's own view of a destination.
const STAGE: &str = ".pp-import";
const DATABASE: &str = "print-partner.db";
const NODE_LEASE: &str = ".desktop-lease";

/// What a data folder holds, as the runtime and the import see it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DataFolder {
    /// Absent, or holding only names the marker rule allows in an empty
    /// folder.
    Empty,
    /// A valid ownership marker. `imported` is true when the folder holds
    /// the import's report.
    Installed { imported: bool },
    /// No marker, a Node database, no lease: a stopped Node installation.
    Node,
    /// No marker, a Node database and a lease: Node runs there or stopped
    /// without releasing it.
    NodeLeased,
    /// No marker and the import's own leftovers.
    Unfinished(Unfinished),
    /// Anything else, including a folder that could not be read. The writer
    /// accepts or refuses it with its own reason.
    Other,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Unfinished {
    /// The import stopped while staging: `.pp-import` is present.
    Staging,
    /// The import moved its copy into place and stopped before the marker.
    Unmarked,
}

/// Reads `folder` without writing, without creating it, and without
/// following a link at the folder, its marker or its top-level names.
pub fn survey(folder: &Path) -> DataFolder {
    match folder.symlink_metadata() {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return DataFolder::Empty,
        Ok(metadata) if metadata.is_dir() => {}
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
        DataFolder::Unfinished(Unfinished::Staging)
    } else if has(REPORT_FILE) {
        DataFolder::Unfinished(Unfinished::Unmarked)
    } else if has(DATABASE) && has(NODE_LEASE) {
        DataFolder::NodeLeased
    } else if has(DATABASE) {
        DataFolder::Node
    } else if names
        .iter()
        .all(|name| ownership_marker::allowed_in_empty(name))
    {
        DataFolder::Empty
    } else {
        DataFolder::Other
    }
}

/// The CLI's recovery line: what the folder now holds and what to do next.
impl fmt::Display for DataFolder {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Empty => "absent or empty; import again into it or into another empty folder",
            Self::Installed { imported: true } => {
                "an imported installation, ready to start with --data"
            }
            Self::Installed { imported: false } => "a Rust installation, not an import destination",
            Self::Node => "a Node installation, not an import destination",
            Self::NodeLeased => {
                "a Node installation that is running or did not shut down, not an import destination"
            }
            Self::Unfinished(_) => {
                "a stopped import; no Print Partner runtime opens it; import again into a new, empty folder"
            }
            Self::Other => "not an import destination",
        })
    }
}

/// One import attempt and the state it left its destination in.
#[derive(Debug)]
pub struct Attempt {
    pub result: AttemptResult,
    /// The destination after the attempt: the recovery result.
    pub destination: DataFolder,
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

/// Runs `node_import::import` once, then surveys the destination. The
/// importer keeps every refusal and limit it has. Synchronous and long
/// (minutes per 10 GB): call it off the async runtime.
pub fn import_node(source: &Path, destination: &Path) -> Attempt {
    let result = node_import::import(source, destination)
        .map_or_else(AttemptResult::from, AttemptResult::Imported);
    Attempt {
        result,
        destination: survey(destination),
    }
}

/// Why startup did not open a data folder: it holds a stopped import.
/// Holds no path, so a dialog can show it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct StartRefusal(pub Unfinished);

impl fmt::Display for StartRefusal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(
            "This data folder holds an import that stopped before it finished. Print Partner \
             does not open it, and the Node folder it came from is unchanged. Import again \
             into a new, empty folder.",
        )
    }
}

impl std::error::Error for StartRefusal {}

/// Refuses a stopped import before startup touches the folder. Every other
/// folder goes on to the writer, which keeps its own marker decision. Test
/// builds skip that decision, so this is what keeps them off a stopped
/// import too.
pub fn admit_folder(folder: &Path) -> Result<(), StartRefusal> {
    match survey(folder) {
        DataFolder::Unfinished(stage) => Err(StartRefusal(stage)),
        DataFolder::Empty
        | DataFolder::Installed { .. }
        | DataFolder::Node
        | DataFolder::NodeLeased
        | DataFolder::Other => Ok(()),
    }
}
