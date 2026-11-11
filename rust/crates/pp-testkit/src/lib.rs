//! Disposable Print Partner installations for tests.
//!
//! Every [`Installation`] is a fresh temporary directory that this crate
//! creates and deletes on drop. Writes go only inside that directory, so a test
//! never touches a real data directory. The durable ownership marker
//! ([`MARKER_FILE`]) is written only by [`Installation`] methods.
//!
//! This crate is only ever a `[dev-dependencies]` entry.
//! `rust/scripts/check-boundaries.py` refuses it as a normal or build
//! dependency, so it is never compiled into a release binary.
//!
//! ```
//! use pp_testkit::{Fixture, Installation, Marker};
//!
//! let install = Installation::create(Fixture::RustMarked).unwrap();
//! let bytes = std::fs::read(install.marker_path()).unwrap();
//! assert_eq!(Marker::parse(&bytes).unwrap(), Marker::default());
//! ```

pub mod bambu;
pub mod mcp_planning;
pub mod moonraker;
pub mod node_path;
pub mod node_schema;

use std::{
    collections::{BTreeMap, BTreeSet},
    fmt,
    fs::{self, File, OpenOptions},
    io::{self, Write},
    path::{Path, PathBuf},
};

/// A recursive listing of a directory tree: every regular file with its bytes,
/// every directory as `None`, keyed by path relative to the root.
pub type Snapshot = BTreeMap<PathBuf, Option<Vec<u8>>>;

/// Lists `root` recursively. Compare two snapshots to prove that an operation
/// left a directory byte-for-byte unchanged. Only real directories are
/// descended into (`symlink_metadata`); every other entry is read with
/// `fs::read`, which follows symlinks, so a file symlink records its target's
/// bytes and a directory symlink fails the snapshot.
pub fn snapshot(root: &Path) -> io::Result<Snapshot> {
    let mut entries = BTreeMap::new();
    let mut pending = vec![root.to_path_buf()];
    while let Some(directory) = pending.pop() {
        for entry in fs::read_dir(&directory)? {
            let path = entry?.path();
            let relative = path
                .strip_prefix(root)
                .map_err(|_| io::Error::other("entry outside the snapshot root"))?
                .to_path_buf();
            if fs::symlink_metadata(&path)?.is_dir() {
                entries.insert(relative, None);
                pending.push(path);
            } else {
                entries.insert(relative, Some(fs::read(&path)?));
            }
        }
    }
    Ok(entries)
}

/// File name of the durable Rust ownership marker in a data directory.
pub const MARKER_FILE: &str = ".pp-rust-owned.json";
/// The only marker format version.
pub const MARKER_VERSION: u64 = 1;
/// The only accepted `owner` value.
pub const MARKER_OWNER: &str = "rust";
/// `created_at` that fixtures write, so fixture bytes are deterministic.
pub const FIXTURE_CREATED_AT: &str = "2026-11-03T00:00:00Z";
/// Database file name Node and Rust use inside the data directory.
pub const DATABASE_FILE: &str = "print-partner.db";

/// Instance suffix used for the Node lease leftovers this crate writes.
const LEFTOVER_INSTANCE: &str = "abababababababababababababababab";
/// Earlier, unrelated lease attempts. Their leftovers must not share the live
/// lease's instance, or Node's stale-lease recovery collides with them.
const PRIOR_INSTANCE: &str = "cdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcd";
/// A PID that no process can have (above Linux's `pid_max` ceiling), so the
/// leftover owner records describe a dead owner.
const DEAD_PID: u32 = 2_147_483_647;

/// The durable ownership marker.
///
/// On disk it is one compact JSON object with exactly these keys, in this
/// order, followed by a newline:
///
/// ```json
/// {"version":1,"owner":"rust","created_at":"2026-11-03T00:00:00Z"}
/// ```
///
/// `created_at` is UTC in RFC 3339 form `YYYY-MM-DDTHH:MM:SSZ`. A reader
/// refuses any other key, version, owner, or timestamp shape.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Marker {
    pub version: u64,
    pub owner: String,
    pub created_at: String,
}

impl Default for Marker {
    fn default() -> Self {
        Self::new(FIXTURE_CREATED_AT)
    }
}

/// Why [`Marker::parse`] refused some bytes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum MarkerError {
    NotJson,
    NotObject,
    MissingField(&'static str),
    UnknownField(String),
    UnsupportedVersion,
    WrongOwner,
    BadCreatedAt,
}

impl fmt::Display for MarkerError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NotJson => f.write_str("marker is not JSON"),
            Self::NotObject => f.write_str("marker is not a JSON object"),
            Self::MissingField(field) => write!(f, "marker has no {field}"),
            Self::UnknownField(field) => write!(f, "marker has unknown field {field}"),
            Self::UnsupportedVersion => f.write_str("marker version is not 1"),
            Self::WrongOwner => f.write_str("marker owner is not rust"),
            Self::BadCreatedAt => f.write_str("marker created_at is not YYYY-MM-DDTHH:MM:SSZ"),
        }
    }
}

impl std::error::Error for MarkerError {}

impl Marker {
    /// A version 1 Rust marker with the given `created_at`.
    pub fn new(created_at: impl Into<String>) -> Self {
        Self {
            version: MARKER_VERSION,
            owner: MARKER_OWNER.to_owned(),
            created_at: created_at.into(),
        }
    }

    /// The exact bytes a marker file holds.
    pub fn to_bytes(&self) -> Vec<u8> {
        let text = |value: &str| serde_json::Value::from(value).to_string();
        format!(
            "{{\"version\":{},\"owner\":{},\"created_at\":{}}}\n",
            self.version,
            text(&self.owner),
            text(&self.created_at)
        )
        .into_bytes()
    }

    /// Strict test oracle for the marker format. Product code has its own
    /// reader; tests use this to check fixture and product bytes agree.
    pub fn parse(bytes: &[u8]) -> Result<Self, MarkerError> {
        let value: serde_json::Value =
            serde_json::from_slice(bytes).map_err(|_| MarkerError::NotJson)?;
        let object = value.as_object().ok_or(MarkerError::NotObject)?;
        if let Some(field) = object
            .keys()
            .find(|key| !["version", "owner", "created_at"].contains(&key.as_str()))
        {
            return Err(MarkerError::UnknownField(field.clone()));
        }
        let field = |name: &'static str| object.get(name).ok_or(MarkerError::MissingField(name));
        if field("version")?.as_u64() != Some(MARKER_VERSION) {
            return Err(MarkerError::UnsupportedVersion);
        }
        if field("owner")?.as_str() != Some(MARKER_OWNER) {
            return Err(MarkerError::WrongOwner);
        }
        let created_at = field("created_at")?
            .as_str()
            .filter(|value| is_utc_timestamp(value))
            .ok_or(MarkerError::BadCreatedAt)?;
        Ok(Self::new(created_at))
    }
}

fn is_utc_timestamp(value: &str) -> bool {
    let bytes = value.as_bytes();
    bytes.len() == 20
        && bytes.iter().enumerate().all(|(index, byte)| match index {
            4 | 7 => *byte == b'-',
            10 => *byte == b'T',
            13 | 16 => *byte == b':',
            19 => *byte == b'Z',
            _ => byte.is_ascii_digit(),
        })
}

/// Ways [`Installation::write_corrupt_marker`] damages the marker.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Corruption {
    /// A zero-byte file.
    Empty,
    /// The first half of a valid marker.
    Truncated,
    /// Bytes that are not JSON.
    NotJson,
    /// Valid JSON with `owner` set to `node`.
    WrongOwner,
    /// Valid JSON with `version` set to 2.
    UnsupportedVersion,
    /// Valid JSON without `created_at`.
    MissingField,
    /// A directory at the marker path.
    Directory,
}

impl Corruption {
    pub const ALL: [Self; 7] = [
        Self::Empty,
        Self::Truncated,
        Self::NotJson,
        Self::WrongOwner,
        Self::UnsupportedVersion,
        Self::MissingField,
        Self::Directory,
    ];

    /// Bytes for the file variants; `None` for [`Corruption::Directory`].
    pub fn bytes(self) -> Option<Vec<u8>> {
        let valid = Marker::default().to_bytes();
        Some(match self {
            Self::Empty => Vec::new(),
            Self::Truncated => valid[..valid.len() / 2].to_vec(),
            Self::NotJson => b"not json\n".to_vec(),
            Self::WrongOwner => Marker {
                owner: "node".into(),
                ..Marker::default()
            }
            .to_bytes(),
            Self::UnsupportedVersion => Marker {
                version: 2,
                ..Marker::default()
            }
            .to_bytes(),
            Self::MissingField => b"{\"version\":1,\"owner\":\"rust\"}\n".to_vec(),
            Self::Directory => return None,
        })
    }
}

/// Which `.desktop-lease` a lease-leftover installation carries.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LeaseForm {
    /// The empty regular file Rust leaves after a crash.
    RustFile,
    /// The directory standalone Node publishes, holding `owner.json` and
    /// `previous-marker.json`.
    NodeDirectory,
}

/// Marker state of an installation.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum MarkerState {
    #[default]
    Absent,
    Valid,
    Corrupt(Corruption),
}

/// What an installation contains. Use a [`Fixture`] for the named cases.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Spec {
    /// Write every lease leftover that still counts as empty.
    pub lease_leftovers: Option<LeaseForm>,
    /// Write a Node schema 34 database.
    pub node_database: bool,
    pub marker: MarkerState,
}

/// The named fixtures.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Fixture {
    /// No entries at all.
    Empty,
    /// Only lease leftovers, with a Node `.desktop-lease` directory.
    EmptyWithLeaseLeftovers,
    /// A schema 34 database from the pinned Node schema. No marker.
    NodeSchema34,
    /// A valid marker and nothing else.
    RustMarked,
    /// A truncated marker and nothing else.
    MarkedCorrupt,
}

impl Fixture {
    pub const ALL: [Self; 5] = [
        Self::Empty,
        Self::EmptyWithLeaseLeftovers,
        Self::NodeSchema34,
        Self::RustMarked,
        Self::MarkedCorrupt,
    ];

    pub fn spec(self) -> Spec {
        let spec = Spec::default();
        match self {
            Self::Empty => spec,
            Self::EmptyWithLeaseLeftovers => Spec {
                lease_leftovers: Some(LeaseForm::NodeDirectory),
                ..spec
            },
            Self::NodeSchema34 => Spec {
                node_database: true,
                ..spec
            },
            Self::RustMarked => Spec {
                marker: MarkerState::Valid,
                ..spec
            },
            Self::MarkedCorrupt => Spec {
                marker: MarkerState::Corrupt(Corruption::Truncated),
                ..spec
            },
        }
    }
}

impl From<Fixture> for Spec {
    fn from(fixture: Fixture) -> Self {
        fixture.spec()
    }
}

/// Whether a top-level entry name may exist in a directory that still counts
/// as empty: `printpartner.lock`, `.desktop-lease`, `.desktop-owner.json`,
/// `lost+found`, `.desktop-lease-candidate-*`, `.desktop-lease-release-*`,
/// `.desktop-lease-retired-*`, and `.desktop-owner-release-*.json`.
///
/// Inside a Node `.desktop-lease` directory only `owner.json` and
/// `previous-marker.json` are allowed ([`allowed_in_node_lease`]).
pub fn allowed_in_empty(name: &str) -> bool {
    let suffixed = |prefix: &str, suffix: &str| {
        name.strip_prefix(prefix)
            .and_then(|rest| rest.strip_suffix(suffix))
            .is_some_and(|middle| !middle.is_empty())
    };
    matches!(
        name,
        "printpartner.lock" | ".desktop-lease" | ".desktop-owner.json" | "lost+found"
    ) || suffixed(".desktop-lease-candidate-", "")
        || suffixed(".desktop-lease-release-", "")
        || suffixed(".desktop-lease-retired-", "")
        || suffixed(".desktop-owner-release-", ".json")
}

/// Whether an entry may exist inside a Node `.desktop-lease` directory in an
/// empty installation.
pub fn allowed_in_node_lease(name: &str) -> bool {
    matches!(name, "owner.json" | "previous-marker.json")
}

/// A disposable data directory, deleted when dropped.
pub struct Installation {
    directory: tempfile::TempDir,
    spec: Spec,
}

impl fmt::Debug for Installation {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Installation")
            .field("path", &self.path())
            .field("spec", &self.spec)
            .finish()
    }
}

impl Installation {
    /// Creates the installation under the system temporary directory.
    pub fn create(spec: impl Into<Spec>) -> io::Result<Self> {
        Self::build(
            tempfile::Builder::new().prefix("pp-testkit-").tempdir()?,
            spec.into(),
        )
    }

    /// Creates the installation in a new directory under `parent`, for tests
    /// that need a specific filesystem. Nothing outside the new directory is
    /// written.
    pub fn create_in(parent: &Path, spec: impl Into<Spec>) -> io::Result<Self> {
        Self::build(
            tempfile::Builder::new()
                .prefix("pp-testkit-")
                .tempdir_in(parent)?,
            spec.into(),
        )
    }

    fn build(directory: tempfile::TempDir, spec: Spec) -> io::Result<Self> {
        let installation = Self { directory, spec };
        if let Some(form) = spec.lease_leftovers {
            installation.write_lease_leftovers(form)?;
        }
        if spec.node_database {
            node_schema::write_schema_34_database(&installation.database_path())?;
        }
        match spec.marker {
            MarkerState::Absent => {}
            MarkerState::Valid => installation.write_marker(&Marker::default())?,
            MarkerState::Corrupt(corruption) => installation.write_corrupt_marker(corruption)?,
        }
        Ok(installation)
    }

    /// The data directory.
    pub fn path(&self) -> &Path {
        self.directory.path()
    }

    /// The spec this installation was created from. Later marker writes do
    /// not change it.
    pub fn spec(&self) -> Spec {
        self.spec
    }

    pub fn marker_path(&self) -> PathBuf {
        self.path().join(MARKER_FILE)
    }

    pub fn database_path(&self) -> PathBuf {
        self.path().join(DATABASE_FILE)
    }

    /// Sorted top-level entry names.
    pub fn entries(&self) -> io::Result<BTreeSet<String>> {
        fs::read_dir(self.path())?
            .map(|entry| Ok(entry?.file_name().to_string_lossy().into_owned()))
            .collect()
    }

    /// Replaces any marker with `marker`, written with mode 0600 and synced.
    pub fn write_marker(&self, marker: &Marker) -> io::Result<()> {
        self.write_marker_bytes(&marker.to_bytes())
    }

    /// Replaces any marker with a damaged one.
    pub fn write_corrupt_marker(&self, corruption: Corruption) -> io::Result<()> {
        match corruption.bytes() {
            Some(bytes) => self.write_marker_bytes(&bytes),
            None => {
                self.remove_marker()?;
                fs::create_dir(self.marker_path())
            }
        }
    }

    /// Removes the marker file or directory if present.
    pub fn remove_marker(&self) -> io::Result<()> {
        let path = self.marker_path();
        match path.symlink_metadata() {
            Ok(metadata) if metadata.is_dir() => fs::remove_dir_all(path),
            Ok(_) => fs::remove_file(path),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(error),
        }
    }

    fn write_marker_bytes(&self, bytes: &[u8]) -> io::Result<()> {
        self.remove_marker()?;
        let mut file = create_new(&self.marker_path())?;
        file.write_all(bytes)?;
        file.sync_all()
    }

    fn write_lease_leftovers(&self, form: LeaseForm) -> io::Result<()> {
        let root = self.path();
        let owner = format!(
            "{{\"pid\":{DEAD_PID},\"process_identity\":\"pp-testkit-dead\",\"instance\":\"{LEFTOVER_INSTANCE}\"}}"
        );
        let marker = format!(
            "{{\"pid\":{DEAD_PID},\"kind\":\"standalone\",\"process_identity\":\"pp-testkit-dead\",\"instance\":\"{LEFTOVER_INSTANCE}\"}}"
        );
        create_new(&root.join("printpartner.lock"))?;
        match form {
            LeaseForm::RustFile => {
                create_new(&root.join(".desktop-lease"))?;
            }
            LeaseForm::NodeDirectory => {
                write_owner_directory(&root.join(".desktop-lease"), &owner)?;
                create_new(&root.join(".desktop-lease/previous-marker.json"))?
                    .write_all(marker.as_bytes())?;
            }
        }
        let prior_owner = format!(
            "{{\"pid\":{DEAD_PID},\"process_identity\":\"pp-testkit-dead\",\"instance\":\"{PRIOR_INSTANCE}\"}}"
        );
        for prefix in [
            ".desktop-lease-candidate-",
            ".desktop-lease-release-",
            ".desktop-lease-retired-",
        ] {
            write_owner_directory(
                &root.join(format!("{prefix}{PRIOR_INSTANCE}")),
                &prior_owner,
            )?;
        }
        create_new(&root.join(".desktop-owner.json"))?.write_all(marker.as_bytes())?;
        create_new(&root.join(format!(".desktop-owner-release-{LEFTOVER_INSTANCE}.json")))?
            .write_all(marker.as_bytes())?;
        fs::create_dir(root.join("lost+found"))
    }
}

fn write_owner_directory(path: &Path, owner: &str) -> io::Result<()> {
    fs::create_dir(path)?;
    create_new(&path.join("owner.json"))?.write_all(owner.as_bytes())
}

fn create_new(path: &Path) -> io::Result<File> {
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
    options.open(path)
}

#[cfg(test)]
mod tests;
