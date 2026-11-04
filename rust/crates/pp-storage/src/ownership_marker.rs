//! The durable Rust ownership marker, `<data dir>/.pp-rust-owned.json`.
//!
//! The marker records that Rust owns a data directory. It is separate from the
//! `.desktop-owner.json` process lease record. Its bytes are one compact JSON
//! object followed by a newline:
//!
//! ```json
//! {"version":1,"owner":"rust","created_at":"2026-11-03T00:00:00Z"}
//! ```
//!
//! [`inspect`] reads it without writing. In a `desktop_rebuild` build the
//! storage writer calls [`admit_writer`] while it holds the directory lease and
//! before any schema write:
//!
//! | Gate | Marker | Directory | Result |
//! | --- | --- | --- | --- |
//! | switch on | valid | any | open |
//! | switch on | absent | empty | write the marker atomically, then open |
//! | switch on | absent | not empty | refuse, change nothing |
//! | switch on | refused (unreadable, truncated, unknown version, ...) | any | refuse |
//! | switch off | any | any | refuse before any write |
//!
//! [`refuse_marked`] is the Node-backed runtime's check: any marker entry,
//! valid or not, refuses it.

use anyhow::Result;
use pp_os::Dir;
use std::{
    fmt,
    io::{Read, Write},
    path::Path,
};

#[cfg(unix)]
mod unix;
#[cfg(unix)]
use unix as platform;

#[cfg(windows)]
mod windows;
#[cfg(windows)]
use windows as platform;

/// File name of the durable ownership marker.
pub const MARKER_FILE: &str = ".pp-rust-owned.json";
/// Prefix of the marker writer's temporary file. A crash between create and
/// rename leaves one behind; it still counts as empty.
pub const MARKER_TEMP_PREFIX: &str = ".pp-rust-owned.json.tmp-";
/// The only marker format version.
pub const MARKER_VERSION: u64 = 1;
/// The only accepted `owner` value.
pub const MARKER_OWNER: &str = "rust";
/// The process lease record. A live one makes a directory non-empty.
pub const OWNER_RECORD: &str = ".desktop-owner.json";
/// A valid marker is 65 bytes. Anything over this is refused unread.
const MARKER_MAX_BYTES: u64 = 1024;

/// A parsed, valid marker.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Marker {
    /// UTC, `YYYY-MM-DDTHH:MM:SSZ`.
    pub created_at: String,
}

impl Marker {
    /// A marker created now.
    pub fn now() -> Self {
        let now = time::OffsetDateTime::now_utc();
        Self {
            created_at: format!(
                "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}Z",
                now.year(),
                u8::from(now.month()),
                now.day(),
                now.hour(),
                now.minute(),
                now.second()
            ),
        }
    }

    /// The exact bytes a marker file holds.
    pub fn to_bytes(&self) -> Vec<u8> {
        format!(
            "{{\"version\":{MARKER_VERSION},\"owner\":\"{MARKER_OWNER}\",\"created_at\":{}}}\n",
            serde_json::Value::from(self.created_at.as_str())
        )
        .into_bytes()
    }

    /// Strict reader: exactly `version`, `owner` and `created_at`, with
    /// `version` 1, `owner` `rust`, and a valid UTC timestamp.
    pub fn parse(bytes: &[u8]) -> Result<Self, MarkerRefusal> {
        let value: serde_json::Value =
            serde_json::from_slice(bytes).map_err(|_| MarkerRefusal::NotJson)?;
        let object = value.as_object().ok_or(MarkerRefusal::NotObject)?;
        if let Some(key) = object
            .keys()
            .find(|key| !matches!(key.as_str(), "version" | "owner" | "created_at"))
        {
            return Err(MarkerRefusal::UnknownField(key.clone()));
        }
        let field = |name: &'static str| object.get(name).ok_or(MarkerRefusal::MissingField(name));
        if field("version")?.as_u64() != Some(MARKER_VERSION) {
            return Err(MarkerRefusal::UnsupportedVersion);
        }
        if field("owner")?.as_str() != Some(MARKER_OWNER) {
            return Err(MarkerRefusal::WrongOwner);
        }
        let created_at = field("created_at")?
            .as_str()
            .filter(|value| is_utc_timestamp(value))
            .ok_or(MarkerRefusal::BadCreatedAt)?;
        Ok(Self {
            created_at: created_at.to_owned(),
        })
    }
}

/// `YYYY-MM-DDTHH:MM:SSZ`: a valid RFC 3339 time that is exactly 20 bytes
/// (no fraction or offset) with an upper-case `T` and `Z`.
fn is_utc_timestamp(value: &str) -> bool {
    value.len() == 20
        && value.as_bytes()[10] == b'T'
        && value.ends_with('Z')
        && time::OffsetDateTime::parse(value, &time::format_description::well_known::Rfc3339)
            .is_ok()
}

/// Why a present marker was refused.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum MarkerRefusal {
    /// A directory, symlink, socket or other non-regular entry.
    NotRegularFile,
    /// Opening or reading failed.
    Unreadable(std::io::ErrorKind),
    TooLarge,
    NotJson,
    NotObject,
    MissingField(&'static str),
    UnknownField(String),
    UnsupportedVersion,
    WrongOwner,
    BadCreatedAt,
}

impl fmt::Display for MarkerRefusal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NotRegularFile => f.write_str("marker is not a regular file"),
            Self::Unreadable(kind) => write!(f, "marker could not be read ({kind})"),
            Self::TooLarge => write!(f, "marker is larger than {MARKER_MAX_BYTES} bytes"),
            Self::NotJson => f.write_str("marker is not JSON (empty or truncated?)"),
            Self::NotObject => f.write_str("marker is not a JSON object"),
            Self::MissingField(field) => write!(f, "marker has no {field}"),
            Self::UnknownField(field) => write!(f, "marker has unknown field {field:?}"),
            Self::UnsupportedVersion => write!(f, "marker version is not {MARKER_VERSION}"),
            Self::WrongOwner => write!(f, "marker owner is not {MARKER_OWNER}"),
            Self::BadCreatedAt => f.write_str("marker created_at is not a UTC RFC 3339 time"),
        }
    }
}

/// What the data directory's marker entry says.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum MarkerState {
    Absent,
    Valid(Marker),
    /// Present but not a valid marker. Always a refusal.
    Refused(MarkerRefusal),
}

/// Reads the marker without following symlinks and without writing.
pub fn inspect(directory: &Path) -> MarkerState {
    match Dir::open(directory) {
        Ok(dir) => inspect_in(&dir),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => MarkerState::Absent,
        Err(error) => MarkerState::Refused(MarkerRefusal::Unreadable(error.kind())),
    }
}

fn inspect_in(dir: &Dir) -> MarkerState {
    match dir.status_nofollow(MARKER_FILE) {
        Ok(status) if !status.is_file() => {
            return MarkerState::Refused(MarkerRefusal::NotRegularFile);
        }
        Ok(_) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return MarkerState::Absent,
        Err(error) => return MarkerState::Refused(MarkerRefusal::Unreadable(error.kind())),
    }
    match read_regular_file(dir, MARKER_FILE, MARKER_MAX_BYTES) {
        Ok(Some(bytes)) => match Marker::parse(&bytes) {
            Ok(marker) => MarkerState::Valid(marker),
            Err(refusal) => MarkerState::Refused(refusal),
        },
        Ok(None) => MarkerState::Refused(MarkerRefusal::TooLarge),
        Err(error) if error.kind() == std::io::ErrorKind::InvalidData => {
            MarkerState::Refused(MarkerRefusal::NotRegularFile)
        }
        Err(error) => MarkerState::Refused(MarkerRefusal::Unreadable(error.kind())),
    }
}

/// Reads the regular file `name` without following it. `Ok(None)` when it
/// holds more than `limit` bytes; `InvalidData` when it is not a regular file.
fn read_regular_file(dir: &Dir, name: &str, limit: u64) -> std::io::Result<Option<Vec<u8>>> {
    let file = dir.open_read_nofollow(name)?;
    if !pp_os::status_of(&file)?.is_file() {
        return Err(std::io::ErrorKind::InvalidData.into());
    }
    let mut bytes = Vec::new();
    file.take(limit + 1).read_to_end(&mut bytes)?;
    Ok((bytes.len() as u64 <= limit).then_some(bytes))
}

/// A refusal to open a data directory, before anything in it changed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum OwnershipRefusal {
    /// No marker, and the directory holds more than lease files.
    NotEmptyUnmarked { first_entry: String },
    /// A present marker that is not valid.
    Marker(MarkerRefusal),
    /// A `desktop_rebuild` build with `PP_DESKTOP_REBUILD` off.
    SwitchOff,
    /// The Node-backed runtime found a marker entry.
    RustOwned,
}

impl fmt::Display for OwnershipRefusal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NotEmptyUnmarked { first_entry } => write!(
                f,
                "Data directory has data but no {MARKER_FILE} (found {first_entry:?}); Rust does not take over an existing installation"
            ),
            Self::Marker(refusal) => write!(f, "Data directory {MARKER_FILE} refused: {refusal}"),
            Self::SwitchOff => f.write_str(
                "Rust storage writer refused: PP_DESKTOP_REBUILD is not on, so Rust makes no schema change",
            ),
            Self::RustOwned => write!(
                f,
                "Data directory is owned by the Rust runtime ({MARKER_FILE}); the Node-backed runtime does not open it"
            ),
        }
    }
}

impl std::error::Error for OwnershipRefusal {}

/// Top-level names allowed in a directory that still counts as empty. `*`
/// stands for one or more characters. All but `lost+found`, the filesystem's,
/// are lease names from the Rust and Node lease code. [`allowed_in_empty`]
/// also admits the marker writer's own crash leftover, [`MARKER_TEMP_PREFIX`]
/// followed by one or more characters.
pub const EMPTY_ALLOWLIST: &[&str] = &[
    "printpartner.lock",
    ".desktop-lease",
    ".desktop-lease-candidate-*",
    ".desktop-lease-release-*",
    ".desktop-lease-retired-*",
    OWNER_RECORD,
    ".desktop-owner-release-*.json",
    "lost+found",
];

/// Whether a top-level entry name is in [`EMPTY_ALLOWLIST`]. Inside a
/// `.desktop-lease` directory only [`allowed_in_node_lease`] names may exist,
/// and `.desktop-owner.json` must not name a live process
/// (`first_non_empty_entry_in` checks both).
pub fn allowed_in_empty(name: &str) -> bool {
    let matches = |prefix: &str, suffix: &str| {
        name.strip_prefix(prefix)
            .and_then(|rest| rest.strip_suffix(suffix))
            .is_some_and(|middle| !middle.is_empty())
    };
    matches(MARKER_TEMP_PREFIX, "")
        || EMPTY_ALLOWLIST
            .iter()
            .any(|pattern| match pattern.split_once('*') {
                None => name == *pattern,
                Some((prefix, suffix)) => matches(prefix, suffix),
            })
}

/// Entries a Node `.desktop-lease` directory may hold in an empty directory.
pub fn allowed_in_node_lease(name: &str) -> bool {
    matches!(name, "owner.json" | "previous-marker.json")
}

/// The first entry that makes `directory` (opened as `dir`) non-empty, or
/// `None` when it is empty.
fn first_non_empty_entry_in(dir: &Dir, directory: &Path) -> std::io::Result<Option<String>> {
    let mut names = std::fs::read_dir(directory)?
        .map(|entry| Ok(entry?.file_name().to_string_lossy().into_owned()))
        .collect::<std::io::Result<Vec<_>>>()?;
    names.sort();
    for name in names {
        if !allowed_in_empty(&name) {
            return Ok(Some(name));
        }
        if name == ".desktop-lease" && dir.status_nofollow(&name)?.is_dir() {
            for inner in std::fs::read_dir(directory.join(&name))? {
                let inner = inner?.file_name().to_string_lossy().into_owned();
                if !allowed_in_node_lease(&inner) {
                    return Ok(Some(format!(".desktop-lease/{inner}")));
                }
            }
        }
        if name == OWNER_RECORD && owner_record_is_live(dir) {
            return Ok(Some(name));
        }
    }
    Ok(None)
}

/// A process lease record names a running process when its pid is alive and
/// its recorded identity, if any, still matches. An unreadable record, or an
/// identity that cannot be looked up, counts as live.
fn owner_record_is_live(dir: &Dir) -> bool {
    if !dir
        .status_nofollow(OWNER_RECORD)
        .is_ok_and(|status| status.is_file())
    {
        return true;
    }
    let Ok(record) = read_regular_file(dir, OWNER_RECORD, 64 * 1024)
        .map_err(|_| ())
        .and_then(|bytes| bytes.ok_or(()))
        .and_then(|bytes| serde_json::from_slice::<serde_json::Value>(&bytes).map_err(|_| ()))
    else {
        return true;
    };
    let Some(pid) = record
        .get("pid")
        .and_then(serde_json::Value::as_i64)
        .filter(|pid| (1..=i64::from(i32::MAX)).contains(pid))
    else {
        return true;
    };
    let pid = pid as u32;
    if !platform::process_running(pid) {
        return false;
    }
    // The identity check guards against a reused pid. Where the identity
    // cannot be looked up, the record still counts as live.
    match record.get("process_identity").and_then(|v| v.as_str()) {
        Some(identity) if !identity.is_empty() => {
            platform::process_identity(pid).is_none_or(|actual| actual == identity)
        }
        _ => true,
    }
}

/// Writes `marker` atomically: a temporary file in the same directory, fsync,
/// rename onto [`MARKER_FILE`], then fsync the directory.
fn write_marker(dir: &Dir, directory: &Path, marker: &Marker) -> std::io::Result<()> {
    let temp = format!(
        "{MARKER_TEMP_PREFIX}{}",
        hex::encode(rand::random::<[u8; 8]>())
    );
    let written = (|| {
        let mut file = dir.create_new_nofollow(&temp)?;
        file.write_all(&marker.to_bytes())?;
        file.sync_all()?;
        #[cfg(test)]
        tests::fail_after_temp_sync()?;
        pp_os::publish_rename(&directory.join(&temp), &directory.join(MARKER_FILE))
    })();
    if written.is_err() {
        let _ = dir.remove_file(&temp);
        return written;
    }
    pp_os::sync_directory(directory)?;
    // Earlier crash leftovers are ours; the marker now supersedes them.
    for entry in std::fs::read_dir(directory)?.flatten() {
        if let Some(name) = entry
            .file_name()
            .to_str()
            .filter(|name| name.starts_with(MARKER_TEMP_PREFIX))
        {
            let _ = dir.remove_file(name);
        }
    }
    Ok(())
}

// The opt-in turns the marker decision off, so it must never reach an
// optimized build, whatever `--features` or `--all-features` asks for.
#[cfg(all(feature = "test-unmarked-writer", not(debug_assertions)))]
compile_error!("test-unmarked-writer must never be built in a release profile");

/// How the storage writer treats ownership. Only `desktop_rebuild` builds
/// enforce it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WriterGate {
    /// `PP_DESKTOP_REBUILD` is on: apply the marker decision.
    SwitchOn,
    /// `PP_DESKTOP_REBUILD` is off: refuse before any write.
    SwitchOff,
    /// Test-only: skip the marker decision, as before markers existed. Absent
    /// from release builds; see the `test-unmarked-writer` feature.
    #[cfg(any(test, feature = "test-unmarked-writer"))]
    Unchecked,
}

impl WriterGate {
    /// The gate [`crate::WriterOwner::open`] uses. In a build with the test
    /// opt-in this is always [`WriterGate::Unchecked`].
    pub fn from_env() -> Self {
        let on = switch_on(std::env::var("PP_DESKTOP_REBUILD").ok().as_deref());
        #[cfg(any(test, feature = "test-unmarked-writer"))]
        {
            let _ = on;
            Self::Unchecked
        }
        #[cfg(not(any(test, feature = "test-unmarked-writer")))]
        if on { Self::SwitchOn } else { Self::SwitchOff }
    }
}

/// The runtime switch: exactly `1` or `true`.
pub fn switch_on(value: Option<&str>) -> bool {
    matches!(value, Some("1" | "true"))
}

/// The marker decision for the storage writer. Takes the locked directory, so
/// it runs only while the lease is held, and runs before any schema write.
pub(crate) fn admit_writer(locked: &crate::lease::LockedDirectory, gate: WriterGate) -> Result<()> {
    if !cfg!(feature = "desktop_rebuild") {
        return Ok(());
    }
    let (dir, directory) = (locked.dir(), locked.path());
    match gate {
        #[cfg(any(test, feature = "test-unmarked-writer"))]
        WriterGate::Unchecked => Ok(()),
        WriterGate::SwitchOff => Err(OwnershipRefusal::SwitchOff.into()),
        WriterGate::SwitchOn => match inspect_in(dir) {
            MarkerState::Valid(_) => Ok(()),
            MarkerState::Refused(refusal) => Err(OwnershipRefusal::Marker(refusal).into()),
            MarkerState::Absent => {
                if let Some(first_entry) = first_non_empty_entry_in(dir, directory)? {
                    return Err(OwnershipRefusal::NotEmptyUnmarked { first_entry }.into());
                }
                write_marker(dir, directory, &Marker::now())?;
                match inspect_in(dir) {
                    MarkerState::Valid(_) => Ok(()),
                    _ => anyhow::bail!("Ownership marker did not read back after writing"),
                }
            }
        },
    }
}

/// The Node-backed runtime's lease: [`crate::lease::StorageLease::acquire`],
/// refusing a directory with any marker entry while the lock is held. A refusal
/// leaves the lease files as it found them.
pub fn lease_unmarked(directory: &Path) -> Result<crate::lease::StorageLease> {
    let locked = crate::lease::admit_directory(directory)?.lock()?;
    if let Err(refusal) = refuse_marked(locked.path()) {
        locked.abandon();
        return Err(refusal.into());
    }
    crate::lease::StorageLease::acquire_locked(locked)
}

/// The Node-backed runtime's check: any marker entry refuses the directory.
/// [`lease_unmarked`] calls it while the lock is held.
pub fn refuse_marked(directory: &Path) -> Result<(), OwnershipRefusal> {
    match inspect(directory) {
        MarkerState::Absent => Ok(()),
        MarkerState::Valid(_) | MarkerState::Refused(_) => Err(OwnershipRefusal::RustOwned),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    thread_local! {
        static FAIL_AFTER_TEMP_SYNC: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    }

    pub(super) fn fail_after_temp_sync() -> std::io::Result<()> {
        if FAIL_AFTER_TEMP_SYNC.get() {
            return Err(std::io::Error::other("injected failure before rename"));
        }
        Ok(())
    }

    #[test]
    fn product_bytes_match_the_testkit_oracle() {
        let marker = Marker {
            created_at: pp_testkit::FIXTURE_CREATED_AT.into(),
        };
        assert_eq!(marker.to_bytes(), pp_testkit::Marker::default().to_bytes());
        let now = Marker::now();
        assert_eq!(
            pp_testkit::Marker::parse(&now.to_bytes())
                .unwrap()
                .created_at,
            now.created_at
        );
        assert_eq!(MARKER_FILE, pp_testkit::MARKER_FILE);
    }

    #[test]
    fn calendar_invalid_timestamps_are_refused() {
        for created_at in [
            "2026-11-03t00:00:00Z",
            "2026-11-03T00:00:00z",
            "2026-11-03T00:00:0.Z",
            "2026-11-03 00:00:00Z",
            "2026-13-01T00:00:00Z",
            "2026-02-30T00:00:00Z",
            "2026-11-03T24:00:00Z",
        ] {
            let bytes = Marker {
                created_at: created_at.into(),
            }
            .to_bytes();
            assert_eq!(Marker::parse(&bytes), Err(MarkerRefusal::BadCreatedAt));
        }
    }

    #[test]
    fn failed_marker_write_leaves_no_marker_and_no_temp_file() {
        let directory = tempfile::tempdir().unwrap();
        FAIL_AFTER_TEMP_SYNC.set(true);
        let dir = Dir::open(directory.path()).unwrap();
        let result = write_marker(&dir, directory.path(), &Marker::now());
        FAIL_AFTER_TEMP_SYNC.set(false);
        assert!(result.is_err());
        assert_eq!(std::fs::read_dir(directory.path()).unwrap().count(), 0);
    }

    #[cfg(feature = "desktop_rebuild")]
    #[test]
    fn marker_write_failure_stops_before_any_schema_write() {
        let directory = tempfile::tempdir().unwrap();
        FAIL_AFTER_TEMP_SYNC.set(true);
        let result = crate::WriterOwner::open_with_gate(
            directory.path(),
            crate::Limits::default(),
            WriterGate::SwitchOn,
        );
        FAIL_AFTER_TEMP_SYNC.set(false);
        assert!(result.is_err());
        let names: Vec<_> = std::fs::read_dir(directory.path())
            .unwrap()
            .map(|entry| entry.unwrap().file_name())
            .collect();
        assert!(names.is_empty(), "nothing left behind: {names:?}");
    }

    #[test]
    fn marker_temp_leftover_counts_as_empty_and_is_removed_after_writing() {
        let directory = tempfile::tempdir().unwrap();
        let leftover = directory
            .path()
            .join(format!("{MARKER_TEMP_PREFIX}0011223344556677"));
        std::fs::write(&leftover, b"{\"version\":1").unwrap();
        let dir = Dir::open(directory.path()).unwrap();
        assert_eq!(
            first_non_empty_entry_in(&dir, directory.path()).unwrap(),
            None
        );
        write_marker(&dir, directory.path(), &Marker::now()).unwrap();
        assert!(!leftover.exists());
        assert!(matches!(inspect(directory.path()), MarkerState::Valid(_)));
    }
}
