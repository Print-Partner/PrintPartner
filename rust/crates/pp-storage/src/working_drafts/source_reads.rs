//! The production [`DraftReads`]: Source checkouts and revision workspaces
//! under `<data>/repos`, read as Node's `scanRepo`, `findSourceManifestPath`,
//! `loadCommunityManifest` and `loadPathHintRules` read them.
use super::observation::{
    ArtifactRead, CatalogPurpose, CheckoutManifestUse, DocumentRead, DraftReads, InventoryPath,
    OwnedCatalogReadRequest, OwnedCheckoutReadRequest, OwnedStlReadRequest, PreparationBudget,
    ReadFailure, ReadResult, StlInventory, StlPurpose, StlRootRead,
};
use crate::auth::{AuthPolicy, SessionTenantPolicy};
use sha2::{Digest, Sha256};
use std::{
    fs,
    io::{ErrorKind, Read},
    path::{Component, Path, PathBuf},
};

const MANIFEST: &str = "print-partner.manifest.yaml";
/// Node's build copies `src/data` to `dist/current/data`, the first file
/// `loadShippedPathHintRules` tries. Its second candidate is the same file in
/// the source tree, so `ShippedHintsSecond` reads as missing.
const SHIPPED_HINTS: &[u8] =
    // ast-grep-ignore: no-writer-family-literal -- Embeds the shipped hints Node reads; Rust writes no path-hints file.
    include_bytes!("../../../../../web/apps/server/src/data/path-hints.yaml");
const CHUNK: usize = 64 * 1024;

/// Node's `SourceFilesystemPolicy` for a stored Source `localPath`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SourceFilesystemPolicy {
    /// The path must stay in the Source's workspace `repos/<id>`.
    Isolated,
    /// Any absolute folder the user stored, as Node's trusted
    /// `resolveSourceFilesystemRoot` returns `resolve(storedPath)`.
    TrustedSingleUser,
}
impl SourceFilesystemPolicy {
    /// Node's `createPorts` trusts a self-host server that is not multi-user,
    /// which is the desktop compat child. Rust single-account sessions are
    /// that case; account tenants are multi-user and stay isolated.
    pub fn for_auth(policy: &AuthPolicy) -> Self {
        match policy.session_tenant {
            SessionTenantPolicy::SingleAccountDefault => Self::TrustedSingleUser,
            SessionTenantPolicy::AccountTenant => Self::Isolated,
        }
    }
}

/// Reads Sources for working-draft preparation and manifest observation.
/// Under [`SourceFilesystemPolicy::Isolated`] (the default), a stored
/// `localPath` must stay in the Source's workspace `repos/<id>`, lexically
/// and after resolving symlinks, as Node's isolated `resolveSourceFilesystemRoot`
/// requires. Trusted mode accepts any absolute stored folder. A
/// `snapshotLocator` is always resolved under the workspace. Relative paths
/// and `.` or `..` segments are refused in both modes, which is stricter than
/// Node's `resolve`.
pub struct SourceReads {
    repos: PathBuf,
    custom_hints: PathBuf,
    community: Option<PathBuf>,
    filesystem: SourceFilesystemPolicy,
}
impl SourceReads {
    /// Sources under `<data_dir>/repos`; custom hints in
    /// `<data_dir>/path-hints.yaml`. Community manifests read as missing until
    /// [`Self::with_community_manifests`] names Node's `data/manifests`.
    pub fn new(data_dir: &Path) -> Self {
        Self {
            repos: data_dir.join("repos"),
            // ast-grep-ignore: no-writer-family-literal -- Node's custom hints file, read only; Rust never writes it.
            custom_hints: data_dir.join("path-hints.yaml"),
            community: None,
            filesystem: SourceFilesystemPolicy::Isolated,
        }
    }
    pub fn with_community_manifests(mut self, root: impl Into<PathBuf>) -> Self {
        self.community = Some(root.into());
        self
    }
    pub fn with_filesystem_policy(mut self, policy: SourceFilesystemPolicy) -> Self {
        self.filesystem = policy;
        self
    }
    pub fn community_manifests(&self) -> Option<&Path> {
        self.community.as_deref()
    }
    pub fn filesystem_policy(&self) -> SourceFilesystemPolicy {
        self.filesystem
    }
    /// Node's `resolveSourceFilesystemRoot`, which Build planning hydration
    /// uses for a stored `localPath`. Unlike `locate`, the trusted policy returns
    /// the resolved path whether or not it exists, and `.` and `..` segments are
    /// resolved as Node's `path.resolve` resolves them.
    pub(crate) fn node_root(&self, source: i64, stored: Option<&str>) -> Option<PathBuf> {
        // Stored Node paths can mix separators with a Windows verbatim prefix.
        // Drop that prefix before resolving, so Windows treats both separators alike.
        let candidate = node_resolve(&pp_os::display_path(Path::new(
            stored.filter(|path| !path.is_empty())?,
        )));
        if self.filesystem == SourceFilesystemPolicy::TrustedSingleUser {
            return Some(candidate);
        }
        let workspace = node_resolve(&self.repos).join(source.to_string());
        let meta = fs::symlink_metadata(&workspace).ok()?;
        if meta.file_type().is_symlink() || !meta.is_dir() {
            return None;
        }
        let canonical_workspace = fs::canonicalize(&workspace).ok()?;
        let canonical = fs::canonicalize(&candidate).ok()?;
        (canonical.starts_with(&canonical_workspace) && canonical.is_dir()).then_some(canonical)
    }
    /// The Source workspace for an untracked checkout path.
    fn locate(
        &self,
        source: i64,
        stored: &str,
        b: &mut PreparationBudget<'_>,
    ) -> ReadResult<Located> {
        b.check()?;
        b.comparison(stored.len())?;
        let path = Path::new(stored);
        let workspace = self.repos.join(source.to_string());
        let lexical = path.is_absolute()
            && !path
                .components()
                .any(|c| matches!(c, Component::CurDir | Component::ParentDir));
        if lexical && self.filesystem == SourceFilesystemPolicy::TrustedSingleUser {
            return Ok(match fs::canonicalize(path) {
                Ok(canonical) => Located::Present(canonical),
                Err(_) => Located::Absent,
            });
        }
        if !lexical
            || !(pp_os::display_path(path).starts_with(pp_os::display_path(&workspace))
                || fs::canonicalize(&self.repos).is_ok_and(|repos| {
                    pp_os::display_path(path)
                        .starts_with(pp_os::display_path(&repos.join(source.to_string())))
                }))
        {
            return Ok(Located::Refused);
        }
        match fs::symlink_metadata(&workspace) {
            Ok(meta) if meta.is_dir() => {}
            Err(e) if e.kind() == ErrorKind::NotFound => return Ok(Located::Absent),
            _ => return Ok(Located::Refused),
        }
        let Ok(workspace) = fs::canonicalize(&workspace) else {
            return Ok(Located::Refused);
        };
        Ok(match fs::canonicalize(path) {
            Ok(canonical) if canonical.starts_with(&workspace) => Located::Present(canonical),
            Ok(_) => Located::Refused,
            Err(_) => Located::Absent,
        })
    }
    /// Node's `resolveSourceSnapshotPath` under the isolated policy, which
    /// throws for an unsafe snapshot locator.
    fn revision(
        &self,
        source: i64,
        locator: Option<&str>,
        b: &mut PreparationBudget<'_>,
    ) -> ReadResult<PathBuf> {
        b.check()?;
        let locator = locator.ok_or(ReadFailure::UnsafeLocator)?;
        b.comparison(locator.len())?;
        if !crate::read_model::graph::safe_path(locator)
            || !locator.starts_with(&format!("{source}/"))
        {
            return Err(ReadFailure::UnsafeLocator);
        }
        let workspace = self.repos.join(source.to_string());
        if !fs::symlink_metadata(&workspace).is_ok_and(|meta| meta.is_dir()) {
            return Err(ReadFailure::UnsafeLocator);
        }
        match (
            fs::canonicalize(&workspace),
            fs::canonicalize(self.repos.join(locator)),
        ) {
            (Ok(workspace), Ok(root)) if root.starts_with(&workspace) && root.is_dir() => Ok(root),
            _ => Err(ReadFailure::UnsafeLocator),
        }
    }
}
/// Node's `path.resolve` for one path: absolute against the working directory,
/// with `.` and `..` segments removed lexically.
fn node_resolve(path: &Path) -> PathBuf {
    let absolute =
        std::env::current_dir().map_or_else(|_| path.to_path_buf(), |cwd| cwd.join(path));
    let mut resolved = PathBuf::new();
    for component in absolute.components() {
        match component {
            Component::ParentDir => {
                resolved.pop();
            }
            Component::CurDir => {}
            other => resolved.push(other),
        }
    }
    resolved
}
enum Located {
    /// The stored path leaves the Source workspace; nothing is read.
    Refused,
    /// Inside the workspace but not on disk; Node scans no files.
    Absent,
    Present(PathBuf),
}
impl DraftReads for SourceReads {
    fn resolve_stl_root(
        &self,
        request: &OwnedStlReadRequest,
        b: &mut PreparationBudget<'_>,
    ) -> ReadResult<Box<dyn StlRootRead>> {
        b.check()?;
        let (logical, root) = if request.purpose() == StlPurpose::Revision {
            let root = self.revision(request.source_id(), request.stored_path(), b)?;
            let logical = pp_os::display_path(&root).to_string_lossy().into_owned();
            (Some(logical), Some(root))
        } else {
            match request.stored_path().filter(|path| !path.is_empty()) {
                None => (None, None),
                Some(stored) => match self.locate(request.source_id(), stored, b)? {
                    Located::Refused => (None, None),
                    Located::Absent => (Some(stored.to_owned()), None),
                    // Node lstat()s the stored root and scans nothing unless
                    // it is a directory itself.
                    Located::Present(root) => (
                        Some(stored.to_owned()),
                        fs::symlink_metadata(stored)
                            .is_ok_and(|meta| meta.is_dir())
                            .then_some(root),
                    ),
                },
            }
        };
        Ok(Box::new(Root { logical, root }))
    }
    fn read_checkout_manifest(
        &self,
        request: &OwnedCheckoutReadRequest,
        _: CheckoutManifestUse,
        b: &mut PreparationBudget<'_>,
    ) -> ReadResult<DocumentRead> {
        match self.locate(request.source_id(), request.stored_path(), b)? {
            Located::Refused => Ok(DocumentRead::Skipped(ErrorKind::PermissionDenied)),
            Located::Absent => Ok(DocumentRead::Missing),
            Located::Present(root) => file_under_root(&root, &root.join(MANIFEST), b),
        }
    }
    fn read_catalog_document(
        &self,
        request: &OwnedCatalogReadRequest,
        b: &mut PreparationBudget<'_>,
    ) -> ReadResult<DocumentRead> {
        b.check()?;
        match request.purpose() {
            CatalogPurpose::ShippedHintsFirst => {
                b.document(SHIPPED_HINTS.len(), SHIPPED_HINTS.len())?;
                Ok(DocumentRead::Bytes(SHIPPED_HINTS.to_vec()))
            }
            CatalogPurpose::ShippedHintsSecond => Ok(DocumentRead::Missing),
            // Node reads `path-hints.yaml` whenever `existsSync` finds it.
            CatalogPurpose::CustomHints => read_document(&self.custom_hints, b),
            CatalogPurpose::Community(slug) => {
                let Some(root) = self
                    .community
                    .as_deref()
                    .and_then(|r| r.canonicalize().ok())
                else {
                    return Ok(DocumentRead::Missing);
                };
                let file = root.join(format!("{}.yaml", slug.as_str()));
                file_under_root(&root, &file, b)
            }
        }
    }
}
/// Node's `resolvedFileUnderRoot`: `file` must resolve under canonical `root`.
fn file_under_root(
    root: &Path,
    file: &Path,
    b: &mut PreparationBudget<'_>,
) -> ReadResult<DocumentRead> {
    b.check()?;
    match fs::canonicalize(file) {
        Err(e) if e.kind() == ErrorKind::NotFound => Ok(DocumentRead::Missing),
        Err(e) => Ok(DocumentRead::Skipped(e.kind())),
        Ok(file) if !file.starts_with(root) => {
            Ok(DocumentRead::Skipped(ErrorKind::PermissionDenied))
        }
        Ok(file) => read_document(&file, b),
    }
}
fn read_document(path: &Path, b: &mut PreparationBudget<'_>) -> ReadResult<DocumentRead> {
    b.check()?;
    let meta = match fs::metadata(path) {
        Ok(meta) => meta,
        Err(e) if e.kind() == ErrorKind::NotFound => return Ok(DocumentRead::Missing),
        Err(e) => return Ok(DocumentRead::Skipped(e.kind())),
    };
    if meta.is_dir() {
        return Ok(DocumentRead::Skipped(ErrorKind::IsADirectory));
    }
    if !meta.is_file() {
        return Ok(DocumentRead::Skipped(ErrorKind::InvalidInput));
    }
    let size = usize::try_from(meta.len()).unwrap_or(usize::MAX);
    b.document(size, 0)?;
    let mut file = match fs::File::open(path) {
        Ok(file) => file,
        Err(e) => return Ok(DocumentRead::Skipped(e.kind())),
    };
    let mut bytes = Vec::new();
    let mut chunk = vec![0; CHUNK];
    loop {
        let read = match file.read(&mut chunk) {
            Ok(0) => break,
            Ok(read) => read,
            Err(e) if e.kind() == ErrorKind::Interrupted => continue,
            Err(e) => return Ok(DocumentRead::Skipped(e.kind())),
        };
        b.document(size.max(bytes.len() + read), read)?;
        bytes.extend_from_slice(&chunk[..read]);
    }
    Ok(DocumentRead::Bytes(bytes))
}
struct Root {
    logical: Option<String>,
    /// The canonical directory to scan; `None` scans no files.
    root: Option<PathBuf>,
}
impl StlRootRead for Root {
    fn logical_resolved_path(&self) -> Option<&str> {
        self.logical.as_deref()
    }
    fn scan_stls(&mut self, b: &mut PreparationBudget<'_>) -> ReadResult<Box<dyn StlInventory>> {
        b.check()?;
        if self.logical.is_none() {
            return Err(ReadFailure::Unavailable);
        }
        let mut entries = Vec::new();
        if let Some(root) = &self.root {
            walk(root, "", 0, &mut entries, b)?;
        }
        Ok(Box::new(Inventory {
            root: self.root.clone(),
            entries,
        }))
    }
}
/// Node's `walkStlFiles`: libuv lists names in byte order, `lstat` skips
/// symlinks, and unreadable entries are skipped.
fn walk(
    dir: &Path,
    prefix: &str,
    depth: usize,
    out: &mut Vec<InventoryPath>,
    b: &mut PreparationBudget<'_>,
) -> ReadResult<()> {
    b.visit(depth)?;
    let Ok(listing) = fs::read_dir(dir) else {
        return Ok(());
    };
    let mut names = Vec::new();
    for entry in listing {
        b.visit(depth)?;
        if let Some(name) = entry.ok().and_then(|e| e.file_name().into_string().ok()) {
            names.push(name);
        }
    }
    names.sort_unstable();
    for name in names {
        let path = dir.join(&name);
        let relative = if prefix.is_empty() {
            name
        } else {
            format!("{prefix}/{name}")
        };
        let Ok(meta) = fs::symlink_metadata(&path) else {
            continue;
        };
        if meta.is_dir() {
            walk(&path, &relative, depth + 1, out, b)?;
        } else if meta.is_file() && relative.to_lowercase().ends_with(".stl") {
            b.entry(depth, relative.len())?;
            out.push(InventoryPath {
                physical_relative_path: relative,
                traversal_ordinal: out.len(),
            });
        }
    }
    Ok(())
}
struct Inventory {
    root: Option<PathBuf>,
    entries: Vec<InventoryPath>,
}
impl StlInventory for Inventory {
    fn entries(&self) -> &[InventoryPath] {
        &self.entries
    }
    /// Node's `sha256File` over the scanned file, re-checked to be a regular
    /// file under the scanned root.
    fn hash_tracked_winner(
        &mut self,
        index: usize,
        b: &mut PreparationBudget<'_>,
    ) -> ReadResult<ArtifactRead> {
        b.check()?;
        let (Some(root), Some(entry)) = (&self.root, self.entries.get(index)) else {
            return Err(ReadFailure::Unavailable);
        };
        let io = |e: std::io::Error| ReadFailure::Io(e.kind());
        let path = root.join(&entry.physical_relative_path);
        let canonical = fs::canonicalize(&path).map_err(io)?;
        if !canonical.starts_with(root) || !fs::symlink_metadata(&path).map_err(io)?.is_file() {
            return Err(ReadFailure::UnsafeLocator);
        }
        let mut file = fs::File::open(&canonical).map_err(io)?;
        let size = file.metadata().map_err(io)?.len();
        b.artifact(size, 0)?;
        let (mut hash, mut count, mut chunk) = (Sha256::new(), 0u64, vec![0; CHUNK]);
        loop {
            let read = match file.read(&mut chunk) {
                Ok(0) => break,
                Ok(read) => read,
                Err(e) if e.kind() == ErrorKind::Interrupted => continue,
                Err(e) => return Err(io(e)),
            };
            count += read as u64;
            b.artifact(size.max(count), read)?;
            hash.update(&chunk[..read]);
        }
        Ok(ArtifactRead {
            byte_count: count,
            byte_sha256: hex::encode(hash.finalize()),
        })
    }
}

mod mcp_documents;
pub(crate) use mcp_documents::load as mcp_documents;
