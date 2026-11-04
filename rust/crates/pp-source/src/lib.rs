mod acl;
pub mod archive;
mod directory;
#[cfg(feature = "desktop_rebuild")]
pub mod local_selection;
pub mod media;
#[cfg(feature = "desktop_rebuild")]
pub mod observation;
mod path_policy;
#[cfg(feature = "desktop_rebuild")]
pub mod retained_capture;
mod unicode16;

#[cfg(all(test, target_os = "macos"))]
#[path = "../tests/support/process.rs"]
mod test_process;

use directory::Directory;
use path_policy::PathCollisions;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    borrow::Borrow,
    collections::{BTreeMap, BTreeSet},
    fmt,
    io::{self, Read, Write},
    path::Path,
    sync::{Arc, Mutex, PoisonError},
};
use unicode_normalization::UnicodeNormalization;
use unicode16::nfc16;

pub const MANIFEST: &str = ".printpartner-source-snapshot.json";
pub const MAX_CONTENT_BYTES: u64 = 1024 * 1024 * 1024;
pub const MAX_ENTRIES: usize = 10_000;
const MAX_MANIFEST_BYTES: u64 = 8 * 1024 * 1024;
const CANDIDATE: &str = ".pp-source-candidate";
const MAX_SAFE_INTEGER: u64 = 9_007_199_254_740_991;
/// `PATH_MAX` counts the terminating NUL, so a consumer that opens a stored
/// file by absolute path gets ENAMETOOLONG at this many bytes.
#[cfg(target_os = "macos")]
const PATH_MAX: usize = libc::PATH_MAX as usize;
#[cfg(not(any(target_os = "macos", windows)))]
const PATH_MAX: usize = 4096;
/// Windows readers open long absolute paths through the `\\?\` form, which
/// `std` adds itself, up to 32,767 UTF-16 units. A unit takes at least one
/// WTF-8 byte, so counting bytes against it is conservative.
#[cfg(windows)]
const PATH_MAX: usize = 32_767;

type Result<T> = std::result::Result<T, Error>;

#[derive(Debug)]
pub enum Error {
    UnsafePath,
    UnsafeAncestor,
    UnsafeAcl,
    #[cfg(feature = "desktop_rebuild")]
    Entropy,
    #[cfg(feature = "desktop_rebuild")]
    CaptureCollisionLimit,
    InvalidIdentity,
    DuplicatePath,
    Limit,
    /// The file's absolute published path would reach `PATH_MAX`, so readers
    /// that open it by path could not.
    StoredPathTooLong {
        path: String,
        bytes: usize,
    },
    CorruptSnapshot,
    /// An intact revision published under the earlier lowercase path rule
    /// contains names that now collide under full Unicode case folding. It is
    /// permanent, not transient: the revision is retained unchanged and can
    /// never be reused. Publish the corrected file set under a new revision key.
    AmbiguousRevision(String),
    Io(io::Error),
    Json(serde_json::Error),
}
impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(error) => write!(f, "io: {error}"),
            Self::Json(error) => write!(f, "invalid-json: {error}"),
            Self::AmbiguousRevision(key) => write!(
                f,
                "ambiguous-revision: published revision {key} has paths that collide under \
                 Unicode case folding; it is retained but cannot be reused, publish the \
                 corrected files under a new revision key"
            ),
            Self::StoredPathTooLong { path, bytes } => write!(
                f,
                "stored-path-too-long: {path} would be stored at an absolute path of {bytes} \
                 bytes; this system opens paths of at most {} bytes; \
                 use a shorter repository or folder path and retry the import",
                PATH_MAX - 1
            ),
            _ => write!(f, "{}", self.category()),
        }
    }
}
impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io(error) => Some(error),
            Self::Json(error) => Some(error),
            _ => None,
        }
    }
}
impl Error {
    pub fn category(&self) -> &'static str {
        match self {
            Self::UnsafePath => "unsafe-path",
            Self::UnsafeAncestor => "unsafe-ancestor",
            Self::UnsafeAcl => "unsafe-acl",
            #[cfg(feature = "desktop_rebuild")]
            Self::Entropy => "entropy-unavailable",
            #[cfg(feature = "desktop_rebuild")]
            Self::CaptureCollisionLimit => "capture-collision-limit",
            Self::InvalidIdentity => "invalid-identity",
            Self::DuplicatePath => "duplicate-path",
            Self::Limit => "limit",
            Self::StoredPathTooLong { .. } => "stored-path-too-long",
            Self::CorruptSnapshot => "corrupt-snapshot",
            Self::AmbiguousRevision(_) => "ambiguous-revision",
            Self::Io(_) => "io",
            Self::Json(_) => "invalid-json",
        }
    }
}
impl From<io::Error> for Error {
    fn from(e: io::Error) -> Self {
        Self::Io(e)
    }
}
impl From<serde_json::Error> for Error {
    fn from(e: serde_json::Error) -> Self {
        Self::Json(e)
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct SourcePath(String);
fn source_path_structure(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 4096
        && !value.contains(['\\', '\0', ':'])
        && !value.starts_with('/')
        && value.split('/').count() <= 64
        && value.split('/').next() != Some(MANIFEST)
        && value
            .split('/')
            .all(|segment| !segment.is_empty() && segment != "." && segment != "..")
}
impl TryFrom<String> for SourcePath {
    type Error = Error;
    fn try_from(value: String) -> Result<Self> {
        // New requests and new publications stay on Unicode 17 NFC.
        if source_path_structure(&value) && value.nfc().collect::<String>() == value {
            Ok(Self(value))
        } else {
            Err(Error::UnsafePath)
        }
    }
}
impl From<SourcePath> for String {
    fn from(value: SourcePath) -> Self {
        value.0
    }
}
impl SourcePath {
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// On Windows, refuses segments that absolute-path readers reinterpret
    /// ([`pp_os::is_windows_alias`]): renaming a user's path would change its
    /// identity. Unix accepts every name it accepted before. Only new names
    /// are checked.
    pub(crate) fn require_portable(&self) -> Result<()> {
        if self.0.split('/').any(pp_os::reinterpreted_here) {
            return Err(Error::UnsafePath);
        }
        Ok(())
    }

    /// A path stored before the Unicode 17 pin. Unicode 17 NFC is still
    /// accepted. A Unicode 16 NFC spelling is accepted only here, so a new
    /// `SnapshotRequest` cannot persist it.
    fn from_persisted(value: String) -> Result<Self> {
        if source_path_structure(&value)
            && (value.nfc().collect::<String>() == value || nfc16(&value) == value)
        {
            Ok(Self(value))
        } else {
            Err(Error::UnsafePath)
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum FileKind {
    Stl,
    Artifact,
    Readme,
    Md,
    Pdf,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SelectedFile {
    pub path: SourcePath,
    pub kind: FileKind,
    pub size_hint_bytes: Option<u64>,
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum OmissionReason {
    DocumentationByteBudget,
    UnknownDocumentSize,
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct OmittedFile {
    pub path: SourcePath,
    pub kind: FileKind,
    pub size_hint_bytes: Option<u64>,
    pub reason: OmissionReason,
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Selection {
    pub max_stl_files: u64,
    pub max_documentation_bytes: u64,
    pub omitted_files: Vec<OmittedFile>,
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ContentFile {
    pub path: SourcePath,
    pub kind: FileKind,
    pub size_bytes: u64,
    pub sha256: String,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SnapshotRequest {
    pub upstream_revision_key: String,
    pub files: Vec<SelectedFile>,
    pub selection: Selection,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct StoredContentFile {
    path: String,
    kind: FileKind,
    size_bytes: u64,
    sha256: String,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct StoredOmittedFile {
    path: String,
    kind: FileKind,
    size_hint_bytes: Option<u64>,
    reason: OmissionReason,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct StoredSelection {
    max_stl_files: u64,
    max_documentation_bytes: u64,
    omitted_files: Vec<StoredOmittedFile>,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct StoredManifest {
    version: u8,
    upstream_revision_key: String,
    manifest_digest: String,
    selection: StoredSelection,
    files: Vec<StoredContentFile>,
}
impl TryFrom<StoredManifest> for Manifest {
    type Error = Error;
    fn try_from(value: StoredManifest) -> Result<Self> {
        let files = value
            .files
            .into_iter()
            .map(|file| {
                Ok(ContentFile {
                    path: SourcePath::from_persisted(file.path)?,
                    kind: file.kind,
                    size_bytes: file.size_bytes,
                    sha256: file.sha256,
                })
            })
            .collect::<Result<Vec<_>>>()?;
        let omitted_files = value
            .selection
            .omitted_files
            .into_iter()
            .map(|file| {
                Ok(OmittedFile {
                    path: SourcePath::from_persisted(file.path)?,
                    kind: file.kind,
                    size_hint_bytes: file.size_hint_bytes,
                    reason: file.reason,
                })
            })
            .collect::<Result<Vec<_>>>()?;
        Ok(Self {
            version: value.version,
            upstream_revision_key: value.upstream_revision_key,
            manifest_digest: value.manifest_digest,
            selection: Selection {
                max_stl_files: value.selection.max_stl_files,
                max_documentation_bytes: value.selection.max_documentation_bytes,
                omitted_files,
            },
            files,
        })
    }
}
#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", try_from = "StoredManifest")]
struct Manifest {
    version: u8,
    upstream_revision_key: String,
    manifest_digest: String,
    selection: Selection,
    files: Vec<ContentFile>,
}
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PublishedSnapshot {
    pub tenant_id: String,
    pub source_id: u64,
    pub upstream_revision_key: String,
    pub manifest_digest: String,
    pub snapshot_locator: String,
    pub files: Vec<ContentFile>,
    pub selection: Selection,
    pub publication: Publication,
    pub stored_bytes: u64,
}
#[derive(Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Publication {
    Created,
    Reused,
}

pub struct TenantRepos {
    tenant_id: String,
    root: Directory,
    root_bytes: usize,
}
pub struct SourceRoot {
    tenant_id: String,
    source_id: u64,
    revisions: Directory,
    /// Byte length of the absolute `<root>/<source>/revisions` path.
    revisions_bytes: usize,
    lock: Arc<SourceLock>,
}
/// The per-Source advisory lock on this handle's revisions directory. The
/// handle's materializations and its live `ExtractedArchive` share one lock
/// through a holder count, so releasing one holder never unlocks another.
pub(crate) struct SourceLock {
    revisions: Directory,
    holders: Mutex<usize>,
}
pub(crate) struct SourceLockGuard {
    lock: Arc<SourceLock>,
    released: bool,
}
impl SourceLock {
    /// Joins this handle's holders, waiting for peers only when none exist.
    fn acquire(self: &Arc<Self>) -> Result<SourceLockGuard> {
        let mut holders = self.holders.lock().unwrap_or_else(PoisonError::into_inner);
        if *holders == 0 {
            self.revisions.lock()?;
        }
        *holders += 1;
        Ok(SourceLockGuard {
            lock: self.clone(),
            released: false,
        })
    }
    /// Takes the lock only when neither this handle nor a peer holds it.
    pub(crate) fn try_acquire_alone(self: &Arc<Self>) -> io::Result<Option<SourceLockGuard>> {
        let mut holders = self.holders.lock().unwrap_or_else(PoisonError::into_inner);
        if *holders > 0 {
            return Ok(None);
        }
        match self.revisions.try_lock() {
            Ok(()) => {}
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => return Ok(None),
            Err(error) => return Err(error),
        }
        *holders = 1;
        Ok(Some(SourceLockGuard {
            lock: self.clone(),
            released: false,
        }))
    }
}
impl SourceLockGuard {
    fn release(&mut self) -> io::Result<()> {
        if self.released {
            return Ok(());
        }
        self.released = true;
        let mut holders = self
            .lock
            .holders
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        *holders -= 1;
        if *holders == 0 {
            self.lock.revisions.unlock()?;
        }
        Ok(())
    }
}
impl Drop for SourceLockGuard {
    fn drop(&mut self) {
        let _ = self.release();
    }
}
pub struct LocalFiles {
    root: Directory,
}
#[derive(Clone, Copy)]
pub struct ArtifactBudget {
    stored_bytes: u64,
    content_bytes: u64,
}
impl ArtifactBudget {
    pub fn new(reserved_stored_bytes: u64, max_content_bytes: u64) -> Result<Self> {
        if max_content_bytes > MAX_CONTENT_BYTES {
            return Err(Error::Limit);
        }
        Ok(Self {
            stored_bytes: reserved_stored_bytes,
            content_bytes: max_content_bytes,
        })
    }
}
impl TenantRepos {
    pub fn open(tenant_id: String, configured_repos: &Path) -> Result<Self> {
        if tenant_id.trim().is_empty() || tenant_id.len() > 200 {
            return Err(Error::InvalidIdentity);
        }
        let normalized: std::path::PathBuf = configured_repos.components().collect();
        if configured_repos.as_os_str() != normalized.as_os_str() {
            return Err(Error::UnsafePath);
        }
        let root = Directory::open_root(configured_repos)?;
        Ok(Self {
            tenant_id,
            root,
            // `open_root` accepted only `/` and normal components.
            root_bytes: configured_repos
                .components()
                .skip(1)
                .map(|name| 1 + name.as_os_str().len())
                .sum(),
        })
    }
    pub fn source(&self, source_id: u64) -> Result<SourceRoot> {
        if source_id == 0 || source_id > MAX_SAFE_INTEGER {
            return Err(Error::InvalidIdentity);
        }
        let revisions = self
            .root
            .child(&source_id.to_string(), true)?
            .child("revisions", true)?;
        Ok(SourceRoot {
            tenant_id: self.tenant_id.clone(),
            source_id,
            lock: Arc::new(SourceLock {
                revisions: revisions.try_clone()?,
                holders: Mutex::new(0),
            }),
            revisions,
            revisions_bytes: self.root_bytes + format!("/{source_id}/revisions").len(),
        })
    }
}
impl LocalFiles {
    pub fn open(selected_directory: &Path) -> Result<Self> {
        Ok(Self {
            root: Directory::open_root(selected_directory)?,
        })
    }
}

fn validate_revision(key: &str) -> Result<()> {
    if key.is_empty()
        || key.len() > 200
        || !key.as_bytes()[0].is_ascii_alphanumeric()
        || !key
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b))
        || pp_os::reinterpreted_here(key)
    {
        return Err(Error::InvalidIdentity);
    }
    Ok(())
}
fn compare_paths(a: &str, b: &str) -> std::cmp::Ordering {
    a.encode_utf16().cmp(b.encode_utf16())
}
/// True when only the current folding policy rejects these selected paths:
/// the earlier rule compared whole-path `to_lowercase` spellings and
/// rejected files that were also parents of other files.
fn legacy_only_collision(files: &[SelectedFile]) -> bool {
    let mut current = PathCollisions::merging_folder_case();
    if files
        .iter()
        .all(|file| current.insert(&file.path, false).is_ok())
    {
        return false;
    }
    let mut paths = BTreeSet::new();
    for file in files {
        if !paths.insert(file.path.0.to_lowercase()) {
            return false;
        }
    }
    paths.iter().all(|path| {
        let mut parent = path.as_str();
        while let Some((prefix, _)) = parent.rsplit_once('/') {
            if paths.contains(prefix) {
                return false;
            }
            parent = prefix;
        }
        true
    })
}
/// New requests only: their names are about to be published, so they must
/// also be [portable](SourcePath::require_portable). Persisted manifests,
/// legacy ambiguity checks included, keep main's classification.
fn validate_request(request: &mut SnapshotRequest) -> Result<()> {
    validate_request_policy(request, true, false)?;
    request
        .files
        .iter()
        .try_for_each(|file| file.path.require_portable())
}
fn validate_request_with(request: &mut SnapshotRequest, selected_collisions: bool) -> Result<()> {
    validate_request_policy(request, selected_collisions, false)
}
fn validate_persisted_request(request: &mut SnapshotRequest) -> Result<()> {
    validate_request_policy(request, true, true)
}
fn validate_request_policy(
    request: &mut SnapshotRequest,
    selected_collisions: bool,
    persisted_unicode16: bool,
) -> Result<()> {
    validate_revision(&request.upstream_revision_key)?;
    if request.files.len() + request.selection.omitted_files.len() > MAX_ENTRIES
        || request.selection.max_stl_files > MAX_SAFE_INTEGER
        || request.selection.max_documentation_bytes > MAX_SAFE_INTEGER
    {
        return Err(Error::Limit);
    }
    // Snapshot destinations share one spelling per ASCII-lowercased folder,
    // so only folder spellings that canonicalize together may differ.
    let mut paths = if persisted_unicode16 {
        PathCollisions::persisted_unicode16()
    } else {
        PathCollisions::merging_folder_case()
    };
    for file in &request.files {
        if file.size_hint_bytes.is_some_and(|n| n > MAX_SAFE_INTEGER) {
            return Err(Error::Limit);
        }
        if selected_collisions {
            paths.insert(&file.path, false)?;
        }
    }
    let selected_paths: BTreeSet<_> = request
        .files
        .iter()
        .map(|file| file.path.as_str())
        .collect();
    // Omitted paths keep Node parity: they dedupe among themselves by
    // `to_lowercase` and conflict with selected paths only on exact spelling.
    // Selected paths above use full Unicode case folding instead.
    let mut omitted_paths = BTreeSet::new();
    for file in &request.selection.omitted_files {
        if matches!(file.kind, FileKind::Stl | FileKind::Artifact)
            || file.size_hint_bytes.is_some_and(|n| n > MAX_SAFE_INTEGER)
        {
            return Err(Error::CorruptSnapshot);
        }
        if selected_paths.contains(file.path.as_str())
            || !omitted_paths.insert(file.path.0.to_lowercase())
        {
            return Err(Error::DuplicatePath);
        }
    }
    request
        .files
        .sort_by(|a, b| compare_paths(a.path.as_str(), b.path.as_str()));
    request
        .selection
        .omitted_files
        .sort_by(|a, b| compare_paths(a.path.as_str(), b.path.as_str()));
    Ok(())
}
fn digest(files: &[ContentFile]) -> Result<String> {
    Ok(hex::encode(Sha256::digest(serde_json::to_vec(files)?)))
}
fn copy_hash(mut input: impl Read, mut output: impl Write, limit: u64) -> Result<(u64, String)> {
    let mut hash = Sha256::new();
    let mut count = 0u64;
    let mut buffer = [0u8; 64 * 1024];
    loop {
        let n = input.read(&mut buffer)?;
        if n == 0 {
            break;
        }
        count = count.checked_add(n as u64).ok_or(Error::Limit)?;
        if count > limit {
            return Err(Error::Limit);
        }
        output.write_all(&buffer[..n])?;
        hash.update(&buffer[..n]);
    }
    Ok((count, hex::encode(hash.finalize())))
}
#[derive(Clone, Copy)]
enum TreeIdentity {
    /// New publication listings. Unicode 17 NFC, rejected unless that form is a new write.
    Unicode17,
    /// Version-1 listings. Unicode 16 NFC keeps historically distinct names distinct.
    Unicode16,
}

fn listing_key(path: &str, identity: TreeIdentity) -> String {
    match identity {
        TreeIdentity::Unicode17 => path.nfc().collect(),
        TreeIdentity::Unicode16 => nfc16(path),
    }
}

fn tree_paths(
    dir: &Directory,
    prefix: &str,
    result: &mut BTreeSet<String>,
    depth: usize,
    identity: TreeIdentity,
) -> Result<()> {
    if depth > 64 {
        return Err(Error::Limit);
    }
    for name in dir.entries()? {
        if prefix.is_empty() && name == MANIFEST {
            continue;
        }
        if result.len() >= MAX_ENTRIES {
            return Err(Error::Limit);
        }
        // HFS+ may enumerate decomposed names even though the manifest uses
        // NFC. Normalize only the comparison path; open the original entry
        // relative to the pinned directory with NOFOLLOW below.
        let path = format!("{prefix}{}", listing_key(&name, identity));
        match identity {
            TreeIdentity::Unicode17 => {
                SourcePath::try_from(path.clone())?;
            }
            TreeIdentity::Unicode16 => {
                if !source_path_structure(&path) {
                    return Err(Error::UnsafePath);
                }
            }
        }
        match dir.child(&name, false) {
            Ok(child) => {
                if !result.insert(format!("{path}/")) {
                    return Err(Error::CorruptSnapshot);
                }
                tree_paths(&child, &format!("{path}/"), result, depth + 1, identity)?;
                child.sync()?;
            }
            Err(_) => {
                dir.file(&name, false)?;
                if !result.insert(path) {
                    return Err(Error::CorruptSnapshot);
                }
            }
        }
    }
    Ok(())
}
fn expected_paths(files: &[ContentFile], identity: TreeIdentity) -> BTreeSet<String> {
    let mut paths = BTreeSet::new();
    for file in files {
        // Open the stored spelling. Publication compares Unicode 17 NFC.
        // A version-1 load compares Unicode 16 NFC so a pair that Unicode 17
        // collapses stays two files.
        paths.insert(listing_key(file.path.as_str(), identity));
        let mut parent = file.path.as_str();
        while let Some((prefix, _)) = parent.rsplit_once('/') {
            paths.insert(format!("{}/", listing_key(prefix, identity)));
            parent = prefix;
        }
    }
    paths
}

/// Every published file, the manifest included, must stay openable by its
/// absolute path.
fn check_stored_path(revision_bytes: usize, path: &str) -> Result<()> {
    let bytes = revision_bytes + 1 + path.len();
    if bytes >= PATH_MAX {
        return Err(Error::StoredPathTooLong {
            path: path.to_owned(),
            bytes,
        });
    }
    Ok(())
}

fn canonical_destination(
    path: &SourcePath,
    directories: &mut BTreeMap<String, String>,
) -> Result<SourcePath> {
    let mut original = String::new();
    let mut canonical = String::new();
    let mut components = path.as_str().split('/').peekable();
    while let Some(component) = components.next() {
        if !original.is_empty() {
            original.push('/');
            canonical.push('/');
        }
        original.push_str(component);
        canonical.push_str(component);
        if components.peek().is_some() {
            canonical = directories
                .entry(original.to_lowercase())
                .or_insert_with(|| canonical.clone())
                .clone();
        }
    }
    SourcePath::try_from(canonical)
}

impl SourceRoot {
    pub fn materialize(
        &mut self,
        request: SnapshotRequest,
        input: &LocalFiles,
        budget: ArtifactBudget,
    ) -> Result<PublishedSnapshot> {
        self.materialize_lazy(request, || Ok(input), budget)
    }
    /// Open local inputs only if the revision needs a new publication.
    pub fn materialize_from_directory(
        &mut self,
        request: SnapshotRequest,
        input_directory: &Path,
        budget: ArtifactBudget,
    ) -> Result<PublishedSnapshot> {
        self.materialize_lazy(request, || LocalFiles::open(input_directory), budget)
    }
    fn materialize_lazy<I: Borrow<LocalFiles>>(
        &mut self,
        mut request: SnapshotRequest,
        open_input: impl FnOnce() -> Result<I>,
        budget: ArtifactBudget,
    ) -> Result<PublishedSnapshot> {
        validate_request(&mut request)?;
        // Joins a live archive's hold from this handle instead of waiting on it.
        let mut guard = self.lock.acquire()?;
        let result = self.materialize_locked(request, open_input, budget);
        let unlocked = guard.release();
        match result {
            Err(error) => Err(error),
            Ok(value) => {
                unlocked?;
                Ok(value)
            }
        }
    }
    fn materialize_locked<I: Borrow<LocalFiles>>(
        &self,
        request: SnapshotRequest,
        open_input: impl FnOnce() -> Result<I>,
        budget: ArtifactBudget,
    ) -> Result<PublishedSnapshot> {
        self.revisions.remove_tree(CANDIDATE)?;
        match self.revisions.child(&request.upstream_revision_key, false) {
            Ok(existing) => return self.load(&existing, &request.upstream_revision_key),
            Err(Error::Io(error)) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(error),
        }
        let input = open_input()?;
        let candidate = self.revisions.child(CANDIDATE, true)?;
        let outcome = self.write_candidate(&candidate, request, input.borrow(), budget);
        let cleanup = self
            .revisions
            .remove_tree(CANDIDATE)
            .and_then(|()| self.revisions.sync());
        match outcome {
            Err(error) => Err(error),
            Ok(receipt) => {
                cleanup?;
                Ok(receipt)
            }
        }
    }
    fn write_candidate(
        &self,
        candidate: &Directory,
        request: SnapshotRequest,
        input: &LocalFiles,
        budget: ArtifactBudget,
    ) -> Result<PublishedSnapshot> {
        let revision_bytes = self.revisions_bytes + 1 + request.upstream_revision_key.len();
        check_stored_path(revision_bytes, MANIFEST)?;
        let mut bytes = 0;
        let mut docs = 0;
        let mut stls = 0;
        let mut files = Vec::new();
        let mut directories = BTreeMap::new();
        let omitted_paths: BTreeSet<_> = request
            .selection
            .omitted_files
            .iter()
            .map(|file| file.path.as_str())
            .collect();
        for selected in request.files {
            let source = input.root.file(selected.path.as_str(), false)?;
            let length = source.metadata()?.len();
            // Keep the original spelling for reading case-sensitive inputs, but
            // share one deterministic spelling for destination directories.
            let path = canonical_destination(&selected.path, &mut directories)?;
            check_stored_path(revision_bytes, path.as_str())?;
            // Folder canonicalization can introduce an exact omission overlap
            // that was absent from the original case-sensitive input paths.
            if omitted_paths.contains(path.as_str()) {
                return Err(Error::DuplicatePath);
            }
            let mut destination = candidate.file(path.as_str(), true)?;
            let (size_bytes, sha256) = copy_hash(
                source,
                &mut destination,
                budget.content_bytes.min(budget.stored_bytes) - bytes,
            )?;
            if length != size_bytes {
                return Err(Error::CorruptSnapshot);
            }
            destination.sync_all()?;
            bytes += size_bytes;
            if selected.kind == FileKind::Stl {
                stls += 1;
            } else {
                docs += size_bytes;
            }
            if stls > request.selection.max_stl_files
                || docs > request.selection.max_documentation_bytes
            {
                return Err(Error::Limit);
            }
            files.push(ContentFile {
                path,
                kind: selected.kind,
                size_bytes,
                sha256,
            });
        }
        files.sort_by(|a, b| compare_paths(a.path.as_str(), b.path.as_str()));
        let manifest = Manifest {
            version: 1,
            upstream_revision_key: request.upstream_revision_key,
            manifest_digest: digest(&files)?,
            selection: request.selection,
            files,
        };
        let mut encoded = serde_json::to_vec_pretty(&manifest)?;
        encoded.push(b'\n');
        if encoded.len() as u64 > MAX_MANIFEST_BYTES
            || bytes + encoded.len() as u64 > budget.stored_bytes
        {
            return Err(Error::Limit);
        }
        let mut file = candidate.file(MANIFEST, true)?;
        file.write_all(&encoded)?;
        file.sync_all()?;
        let mut paths = BTreeSet::new();
        tree_paths(candidate, "", &mut paths, 0, TreeIdentity::Unicode17)?;
        if paths != expected_paths(&manifest.files, TreeIdentity::Unicode17) {
            return Err(Error::CorruptSnapshot);
        }
        candidate.sync()?;
        self.revisions
            .publish(CANDIDATE, &manifest.upstream_revision_key)?;
        Ok(self.receipt(manifest, Publication::Created, bytes + encoded.len() as u64))
    }
    fn load(&self, existing: &Directory, key: &str) -> Result<PublishedSnapshot> {
        let mut raw = Vec::new();
        existing
            .file(MANIFEST, false)?
            .take(MAX_MANIFEST_BYTES + 1)
            .read_to_end(&mut raw)?;
        if raw.len() as u64 > MAX_MANIFEST_BYTES {
            return Err(Error::Limit);
        }
        let manifest: Manifest = serde_json::from_slice(&raw)?;
        let mut request = SnapshotRequest {
            upstream_revision_key: manifest.upstream_revision_key.clone(),
            files: manifest
                .files
                .iter()
                .map(|f| SelectedFile {
                    path: f.path.clone(),
                    kind: f.kind.clone(),
                    size_hint_bytes: Some(f.size_bytes),
                })
                .collect(),
            selection: manifest.selection.clone(),
        };
        let ambiguous = match validate_persisted_request(&mut request) {
            Err(Error::DuplicatePath) if legacy_only_collision(&request.files) => {
                // Every other request constraint must still hold.
                validate_request_with(&mut request, false)?;
                true
            }
            other => {
                other?;
                false
            }
        };
        if manifest.version != 1
            || manifest.upstream_revision_key != key
            || digest(&manifest.files)? != manifest.manifest_digest
            || !manifest
                .files
                .windows(2)
                .all(|p| compare_paths(p[0].path.as_str(), p[1].path.as_str()).is_lt())
        {
            return Err(Error::CorruptSnapshot);
        }
        let mut actual = BTreeSet::new();
        tree_paths(existing, "", &mut actual, 0, TreeIdentity::Unicode16)?;
        if actual != expected_paths(&manifest.files, TreeIdentity::Unicode16) {
            return Err(Error::CorruptSnapshot);
        }
        let mut bytes = 0;
        for file in &manifest.files {
            let (size, sha) = copy_hash(
                existing.file(file.path.as_str(), false)?,
                io::sink(),
                MAX_CONTENT_BYTES - bytes,
            )?;
            if size != file.size_bytes || sha != file.sha256 {
                return Err(Error::CorruptSnapshot);
            }
            bytes += size;
        }
        // Report the policy break only for an otherwise intact publication.
        if ambiguous {
            return Err(Error::AmbiguousRevision(key.to_owned()));
        }
        Ok(self.receipt(manifest, Publication::Reused, bytes + raw.len() as u64))
    }
    fn receipt(
        &self,
        manifest: Manifest,
        publication: Publication,
        stored_bytes: u64,
    ) -> PublishedSnapshot {
        PublishedSnapshot {
            tenant_id: self.tenant_id.clone(),
            source_id: self.source_id,
            snapshot_locator: format!(
                "{}/revisions/{}",
                self.source_id, manifest.upstream_revision_key
            ),
            upstream_revision_key: manifest.upstream_revision_key,
            manifest_digest: manifest.manifest_digest,
            files: manifest.files,
            selection: manifest.selection,
            publication,
            stored_bytes,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scratch::Scratch;
    use std::fs;

    use crate::modes::pin_mode;

    /// Removed when the scratch drops, including while a test unwinds from a
    /// panic.
    struct Tree(Scratch);
    impl Tree {
        fn new() -> Self {
            let path = Scratch::new("pp-source-tree-");
            // Pin the root mode so the ancestor check does not depend on umask.
            pin_mode(&path, 0o755);
            Self(path)
        }
        fn paths(&self) -> Result<BTreeSet<String>> {
            let mut paths = BTreeSet::new();
            tree_paths(
                &Directory::open_root(&self.0)?,
                "",
                &mut paths,
                0,
                TreeIdentity::Unicode17,
            )?;
            Ok(paths)
        }
    }

    #[test]
    fn handle_holders_share_one_flock_until_the_last_releases() {
        let tree = Tree::new();
        let source = TenantRepos::open("tenant".into(), &tree.0)
            .unwrap()
            .source(1)
            .unwrap();
        let peer = Directory(pp_os::Dir::open(&tree.0.join("1/revisions")).unwrap());
        // Archive readiness, then a materialization joining from this handle.
        let archive = source.lock.try_acquire_alone().unwrap().unwrap();
        assert!(source.lock.try_acquire_alone().unwrap().is_none());
        let materialize = source.lock.acquire().unwrap();
        // Discarding the archive must not unlock the running materialization.
        drop(archive);
        assert!(peer.try_lock().is_err());
        assert!(source.lock.try_acquire_alone().unwrap().is_none());
        drop(materialize);
        peer.try_lock().unwrap();
        assert!(source.lock.try_acquire_alone().unwrap().is_none());
        peer.unlock().unwrap();
        assert!(source.lock.try_acquire_alone().unwrap().is_some());
    }

    #[test]
    fn decomposed_listing_names_compare_as_nfc_with_original_names_for_io() {
        let tree = Tree::new();
        // Real NFD names exercise HFS+ listings on Linux too. Opening their
        // NFC replacements would fail on Linux, proving raw-name I/O here.
        fs::create_dir(tree.0.join("pie\u{300}ces")).unwrap();
        fs::write(tree.0.join("pie\u{300}ces/cafe\u{301}.stl"), b"solid").unwrap();
        fs::write(tree.0.join("re\u{301}sume\u{301}.md"), b"notes").unwrap();
        assert_eq!(
            tree.paths().unwrap(),
            BTreeSet::from([
                "pièces/".into(),
                "pièces/café.stl".into(),
                "résumé.md".into(),
            ])
        );
    }

    #[cfg(unix)]
    #[test]
    fn normalized_listing_names_do_not_hide_symlinks() {
        use std::os::unix::fs::symlink;

        let tree = Tree::new();
        fs::write(tree.0.join("target.stl"), b"solid").unwrap();
        symlink("target.stl", tree.0.join("cafe\u{301}.stl")).unwrap();
        assert!(tree.paths().is_err());
        fs::remove_file(tree.0.join("cafe\u{301}.stl")).unwrap();
        symlink("..", tree.0.join("pie\u{300}ces")).unwrap();
        assert!(tree.paths().is_err());
    }

    #[test]
    fn normalization_collisions_cannot_hide_extra_entries() {
        use std::io::ErrorKind;

        let tree = Tree::new();
        fs::write(tree.0.join("café.stl"), b"first").unwrap();
        // On normalization-insensitive volumes the second spelling cannot be
        // created separately; the NFD regression above still runs there.
        match fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(tree.0.join("cafe\u{301}.stl"))
        {
            Ok(_) => assert!(matches!(tree.paths(), Err(Error::CorruptSnapshot))),
            Err(error) if error.kind() == ErrorKind::AlreadyExists => {}
            Err(error) => panic!("{error}"),
        }
    }

    fn entry_names(dir: &std::path::Path) -> BTreeSet<String> {
        fs::read_dir(dir)
            .unwrap()
            .map(|entry| entry.unwrap().file_name().into_string().unwrap())
            .collect()
    }

    #[test]
    fn unicode16_manifest_stays_readable_while_new_writes_reject_it() {
        use sha2::Digest;

        let legacy = "a\u{1ADD}\u{0300}.stl".to_owned();
        let omitted = "notes\u{1ADD}\u{0300}.md".to_owned();
        assert!(SourcePath::try_from(legacy.clone()).is_err());
        assert!(SourcePath::try_from("\u{00E0}\u{1ADD}.stl".to_owned()).is_ok());
        assert!(SourcePath::from_persisted("cafe\u{301}.stl".to_owned()).is_err());

        let content = b"solid";
        let files = vec![ContentFile {
            path: SourcePath::from_persisted(legacy.clone()).unwrap(),
            kind: FileKind::Stl,
            size_bytes: content.len() as u64,
            sha256: hex::encode(sha2::Sha256::digest(content)),
        }];
        let manifest = Manifest {
            version: 1,
            upstream_revision_key: "legacy-rev".into(),
            manifest_digest: hex::encode(sha2::Sha256::digest(serde_json::to_vec(&files).unwrap())),
            selection: Selection {
                max_stl_files: 10,
                max_documentation_bytes: 1000,
                omitted_files: vec![OmittedFile {
                    path: SourcePath::from_persisted(omitted.clone()).unwrap(),
                    kind: FileKind::Md,
                    size_hint_bytes: None,
                    reason: OmissionReason::DocumentationByteBudget,
                }],
            },
            files,
        };
        let mut encoded = serde_json::to_vec_pretty(&manifest).unwrap();
        encoded.push(b'\n');
        let decoded: Manifest = serde_json::from_slice(&encoded).unwrap();
        assert_eq!(decoded.files[0].path.as_str(), legacy);
        assert_eq!(decoded.selection.omitted_files[0].path.as_str(), omitted);

        let root = Scratch::new("pp-source-unicode16-");
        for dir in ["repos", "input"] {
            let path = root.join(dir);
            fs::create_dir(&path).unwrap();
            pin_mode(&path, 0o755);
        }
        pin_mode(&root, 0o755);
        let revision = root.join("repos/42/revisions/legacy-rev");
        fs::create_dir_all(&revision).unwrap();
        fs::write(revision.join(&legacy), content).unwrap();
        fs::write(revision.join(MANIFEST), &encoded).unwrap();
        let decomposed = root.join("repos/42/revisions/decomposed-rev");
        fs::create_dir_all(&decomposed).unwrap();
        fs::write(decomposed.join("cafe\u{301}.stl"), b"x").unwrap();
        fs::write(
            decomposed.join(MANIFEST),
            br#"{"version":1,"upstreamRevisionKey":"decomposed-rev","manifestDigest":"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa","selection":{"maxStlFiles":1,"maxDocumentationBytes":1,"omittedFiles":[]},"files":[{"path":"cafe\u0301.stl","kind":"stl","sizeBytes":1,"sha256":"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"}]}"#,
        )
        .unwrap();

        let input = LocalFiles::open(&root.join("input")).unwrap();
        let mut source = TenantRepos::open("tenant".into(), &root.join("repos"))
            .unwrap()
            .source(42)
            .unwrap();
        let request = |key: &str| SnapshotRequest {
            upstream_revision_key: key.into(),
            files: vec![SelectedFile {
                path: SourcePath::try_from("part.stl".to_owned()).unwrap(),
                kind: FileKind::Stl,
                size_hint_bytes: None,
            }],
            selection: Selection {
                max_stl_files: 10,
                max_documentation_bytes: 1000,
                omitted_files: vec![],
            },
        };
        let published = source
            .materialize(
                request("legacy-rev"),
                &input,
                ArtifactBudget::new(1024 * 1024, 1024 * 1024).unwrap(),
            )
            .unwrap();
        assert_eq!(published.publication, Publication::Reused);
        assert_eq!(published.files[0].path.as_str(), legacy);
        assert_eq!(published.selection.omitted_files[0].path.as_str(), omitted);
        assert_eq!(fs::read(revision.join(&legacy)).unwrap(), content);
        // Case-sensitive APFS still resolves the Unicode 17 spelling to this
        // file, so Path::exists cannot prove the stored name. The directory
        // entry itself must stay the Unicode 16 bytes.
        let stored = entry_names(&revision);
        assert!(stored.contains(&legacy));
        assert!(!stored.contains("\u{00E0}\u{1ADD}.stl"));
        assert!(
            source
                .materialize(
                    request("decomposed-rev"),
                    &input,
                    ArtifactBudget::new(1024 * 1024, 1024 * 1024).unwrap(),
                )
                .is_err()
        );
    }

    #[test]
    fn unicode16_distinct_pair_stays_byte_exact_while_new_writes_still_collide() {
        use sha2::Digest;

        let legacy = "a\u{1ADD}\u{0300}.stl".to_owned();
        let composed = "\u{00E0}\u{1ADD}.stl".to_owned();
        assert!(SourcePath::try_from(legacy.clone()).is_err());
        assert!(SourcePath::try_from(composed.clone()).is_ok());
        let legacy_path = SourcePath::from_persisted(legacy.clone()).unwrap();
        let composed_path = SourcePath::from_persisted(composed.clone()).unwrap();
        let mut publication = PathCollisions::merging_folder_case();
        publication.insert(&composed_path, false).unwrap();
        assert!(matches!(
            publication.insert(&legacy_path, false),
            Err(Error::DuplicatePath)
        ));
        let mut persisted = PathCollisions::persisted_unicode16();
        persisted.insert(&composed_path, false).unwrap();
        persisted.insert(&legacy_path, false).unwrap();
        assert!(serde_json::from_str::<SnapshotRequest>(&format!(
            r#"{{"upstreamRevisionKey":"new-rev","files":[{{"path":{},"kind":"stl"}}],"selection":{{"maxStlFiles":1,"maxDocumentationBytes":1,"omittedFiles":[]}}}}"#,
            serde_json::to_string(&legacy).unwrap()
        ))
        .is_err());

        let legacy_bytes = b"legacy-solid";
        let composed_bytes = b"composed-solid";
        let mut files = vec![
            ContentFile {
                path: legacy_path,
                kind: FileKind::Stl,
                size_bytes: legacy_bytes.len() as u64,
                sha256: hex::encode(sha2::Sha256::digest(legacy_bytes)),
            },
            ContentFile {
                path: composed_path,
                kind: FileKind::Stl,
                size_bytes: composed_bytes.len() as u64,
                sha256: hex::encode(sha2::Sha256::digest(composed_bytes)),
            },
        ];
        files.sort_by(|a, b| compare_paths(a.path.as_str(), b.path.as_str()));
        let manifest = Manifest {
            version: 1,
            upstream_revision_key: "pair-rev".into(),
            manifest_digest: hex::encode(sha2::Sha256::digest(serde_json::to_vec(&files).unwrap())),
            selection: Selection {
                max_stl_files: 10,
                max_documentation_bytes: 1000,
                omitted_files: vec![],
            },
            files,
        };
        let mut encoded = serde_json::to_vec_pretty(&manifest).unwrap();
        encoded.push(b'\n');

        let root = Scratch::new("pp-source-unicode16-pair-");
        for dir in ["repos", "input"] {
            let path = root.join(dir);
            fs::create_dir(&path).unwrap();
            pin_mode(&path, 0o755);
        }
        pin_mode(&root, 0o755);
        let revision = root.join("repos/42/revisions/pair-rev");
        fs::create_dir_all(&revision).unwrap();
        fs::write(revision.join(&legacy), legacy_bytes).unwrap();
        fs::write(revision.join(&composed), composed_bytes).unwrap();
        fs::write(revision.join(MANIFEST), &encoded).unwrap();
        // macOS 26 case-sensitive APFS is still normalization-insensitive, and
        // its normalizer equates this Unicode 17 pair. The second write then
        // replaces the first file. Load must report the snapshot corrupt
        // instead of returning one file under both names.
        let stored = entry_names(&revision);
        let distinct = stored.contains(&legacy) && stored.contains(&composed);

        let input = LocalFiles::open(&root.join("input")).unwrap();
        let mut source = TenantRepos::open("tenant".into(), &root.join("repos"))
            .unwrap()
            .source(42)
            .unwrap();
        let published = source.materialize(
            SnapshotRequest {
                upstream_revision_key: "pair-rev".into(),
                files: vec![SelectedFile {
                    path: SourcePath::try_from("part.stl".to_owned()).unwrap(),
                    kind: FileKind::Stl,
                    size_hint_bytes: None,
                }],
                selection: Selection {
                    max_stl_files: 10,
                    max_documentation_bytes: 1000,
                    omitted_files: vec![],
                },
            },
            &input,
            ArtifactBudget::new(1024 * 1024, 1024 * 1024).unwrap(),
        );
        if !distinct {
            assert!(matches!(published, Err(Error::CorruptSnapshot)));
            assert_eq!(
                stored.iter().filter(|name| name.ends_with(".stl")).count(),
                1
            );
            return;
        }
        let published = published.unwrap();
        assert_eq!(published.publication, Publication::Reused);
        let mut names: Vec<_> = published
            .files
            .iter()
            .map(|file| file.path.as_str().to_owned())
            .collect();
        names.sort();
        let mut expected = vec![legacy.clone(), composed.clone()];
        expected.sort();
        assert_eq!(names, expected);
        assert_eq!(fs::read(revision.join(&legacy)).unwrap(), legacy_bytes);
        assert_eq!(fs::read(revision.join(&composed)).unwrap(), composed_bytes);
        assert_eq!(fs::read(revision.join(MANIFEST)).unwrap(), encoded);
        assert_eq!(fs::read_dir(&revision).unwrap().count(), 3);
        // One directory entry for the pair is the case-sensitive APFS result.
        // Reload must stay corrupt after that entry disappears.
        fs::remove_file(revision.join(&composed)).unwrap();
        assert!(matches!(
            source.materialize(
                SnapshotRequest {
                    upstream_revision_key: "pair-rev".into(),
                    files: vec![SelectedFile {
                        path: SourcePath::try_from("part.stl".to_owned()).unwrap(),
                        kind: FileKind::Stl,
                        size_hint_bytes: None,
                    }],
                    selection: Selection {
                        max_stl_files: 10,
                        max_documentation_bytes: 1000,
                        omitted_files: vec![],
                    },
                },
                &input,
                ArtifactBudget::new(1024 * 1024, 1024 * 1024).unwrap(),
            ),
            Err(Error::CorruptSnapshot)
        ));
    }
}

#[cfg(test)]
#[path = "../../pp-storage/tests/common/cleanup.rs"]
mod cleanup;
#[cfg(test)]
#[path = "../tests/support/modes.rs"]
mod modes;
/// Auto-removing test scratch, shared with pp-storage's tests.
#[cfg(test)]
#[path = "../../pp-storage/tests/common/scratch.rs"]
mod scratch;

#[cfg(test)]
#[test]
fn scratch_root_is_empty_after_lib_tests() {
    cleanup::assert_test_run_clean(&["--skip", "scratch_root_is_empty_after_lib_tests"]);
}
