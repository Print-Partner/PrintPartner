use std::sync::atomic::{AtomicBool, Ordering};

pub(crate) use super::manifest::{
    Glob as BuilderGlob, Group as BuilderGroup, Groups as BuilderGroups,
    Manifest as BuilderManifest, Variant as BuilderVariant,
};

pub(crate) fn parse_builder_manifest(
    bytes: &[u8],
    budget: &mut PreparationBudget<'_>,
) -> ReadResult<BuilderManifest> {
    super::manifest::parse(bytes, budget)
}

pub(crate) use super::manifest::{DimensionOption, Dimensions};

pub(crate) fn parse_variant_dimensions(
    bytes: &[u8],
    budget: &mut PreparationBudget<'_>,
) -> ReadResult<Dimensions> {
    super::manifest::variant_dimensions(bytes, budget)
}

pub(crate) fn merge_builder_groups(target: &mut BuilderGroups, incoming: BuilderGroups) {
    let Ok(()) = super::manifest::merge_with(target, incoming, &mut |_| {
        Ok::<_, std::convert::Infallible>(())
    });
}

pub(crate) type BuilderHint = super::manifest::HintRule;

pub(crate) fn parse_builder_hints(
    bytes: &[u8],
    budget: &mut PreparationBudget<'_>,
) -> ReadResult<Vec<BuilderHint>> {
    super::manifest::hints(bytes, budget)
}

pub(crate) fn infer_builder_siblings(
    paths: &[String],
    budget: &mut PreparationBudget<'_>,
) -> ReadResult<BuilderGroups> {
    super::manifest::infer_siblings(paths, budget)
}

#[derive(Debug, Clone, Copy)]
pub enum Resource {
    Entries,
    Depth,
    Path,
    PathBytes,
    ArtifactBytes,
    DocumentBytes,
    Nodes,
    Visits,
    Rules,
    ComparisonWork,
}
#[derive(Debug)]
pub enum ReadFailure {
    UnsafeLocator,
    InvalidGrant,
    LimitExceeded(Resource),
    Cancelled,
    Unavailable,
    Io(std::io::ErrorKind),
    InvalidDocument,
}
impl std::fmt::Display for ReadFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{self:?}")
    }
}
impl std::error::Error for ReadFailure {}
pub type ReadResult<T> = std::result::Result<T, ReadFailure>;
#[derive(Clone, Copy)]
pub struct PreparationLimits {
    pub entries: usize,
    pub depth: usize,
    pub path: usize,
    pub path_bytes: usize,
    pub artifact_bytes: u64,
    pub document_bytes: usize,
    pub total_document_bytes: usize,
    pub nodes: usize,
    pub visits: usize,
    pub rules: usize,
    pub comparison_work: usize,
}
impl Default for PreparationLimits {
    fn default() -> Self {
        Self {
            entries: 10000,
            depth: 64,
            path: 4096,
            path_bytes: 8 * 1024 * 1024,
            artifact_bytes: 1024 * 1024 * 1024,
            document_bytes: 1024 * 1024,
            total_document_bytes: 8 * 1024 * 1024,
            nodes: 100000,
            visits: 100000,
            rules: 10000,
            comparison_work: 1 << 28,
        }
    }
}
impl PreparationLimits {
    pub fn validate(&self) -> ReadResult<()> {
        let max = Self::default();
        if self.entries > max.entries
            || self.depth > max.depth
            || self.path > max.path
            || self.path_bytes > max.path_bytes
            || self.artifact_bytes > max.artifact_bytes
            || self.document_bytes > max.document_bytes
            || self.total_document_bytes > max.total_document_bytes
            || self.nodes > max.nodes
            || self.visits > max.visits
            || self.rules > max.rules
            || self.comparison_work > max.comparison_work
        {
            Err(ReadFailure::InvalidGrant)
        } else {
            Ok(())
        }
    }
}
/// Accounting shared by all reads and parsing in one preparation attempt.
/// Callers must propagate failures and discard a budget after a refusal.
/// Raw document bytes and expanded parser bytes have independent total caps.
pub struct PreparationBudget<'a> {
    cancelled: &'a AtomicBool,
    limits: PreparationLimits,
    visits: usize,
    entries: usize,
    paths: usize,
    artifacts: u64,
    documents: usize,
    expanded: usize,
    nodes: usize,
    rules: usize,
    compared: usize,
    #[cfg(test)]
    checks_until_cancel: std::sync::atomic::AtomicUsize,
}
impl<'a> PreparationBudget<'a> {
    pub fn new(cancelled: &'a AtomicBool, limits: PreparationLimits) -> ReadResult<Self> {
        limits.validate()?;
        Ok(Self {
            cancelled,
            limits,
            visits: 0,
            entries: 0,
            paths: 0,
            artifacts: 0,
            documents: 0,
            expanded: 0,
            nodes: 0,
            rules: 0,
            compared: 0,
            #[cfg(test)]
            checks_until_cancel: Default::default(),
        })
    }
    /// Cancels this budget's flag when the next `checks` calls to `check` have passed.
    #[cfg(test)]
    pub(crate) fn cancel_after_checks(&mut self, checks: usize) {
        *self.checks_until_cancel.get_mut() = checks + 1;
    }
    #[cfg(test)]
    pub(crate) fn limit_visits_after(&mut self, visits: usize) {
        self.limits.visits = self.visits.saturating_add(visits);
    }
    #[cfg(test)]
    pub(crate) fn limit_comparisons_after(&mut self, work: usize) {
        self.limits.comparison_work = self.compared.saturating_add(work);
    }
    pub fn check(&self) -> ReadResult<()> {
        #[cfg(test)]
        match self.checks_until_cancel.load(Ordering::Relaxed) {
            0 => {}
            1 => self.cancelled.store(true, Ordering::Release),
            remaining => self
                .checks_until_cancel
                .store(remaining - 1, Ordering::Relaxed),
        }
        if self.cancelled.load(Ordering::Acquire) {
            Err(ReadFailure::Cancelled)
        } else {
            Ok(())
        }
    }
    pub fn entry(&mut self, depth: usize, path_bytes: usize) -> ReadResult<()> {
        self.check()?;
        if depth > self.limits.depth {
            return Err(ReadFailure::LimitExceeded(Resource::Depth));
        }
        if path_bytes > self.limits.path {
            return Err(ReadFailure::LimitExceeded(Resource::Path));
        }
        self.entries = self.entries.saturating_add(1);
        self.paths = self.paths.saturating_add(path_bytes);
        if self.entries > self.limits.entries {
            return Err(ReadFailure::LimitExceeded(Resource::Entries));
        }
        if self.paths > self.limits.path_bytes {
            return Err(ReadFailure::LimitExceeded(Resource::PathBytes));
        }
        Ok(())
    }
    pub fn artifact(&mut self, file_bytes: u64, chunk: usize) -> ReadResult<()> {
        self.check()?;
        self.artifacts = self.artifacts.saturating_add(chunk as u64);
        if file_bytes > self.limits.artifact_bytes || self.artifacts > self.limits.artifact_bytes {
            Err(ReadFailure::LimitExceeded(Resource::ArtifactBytes))
        } else {
            Ok(())
        }
    }
    pub fn document(&mut self, file_bytes: usize, chunk: usize) -> ReadResult<()> {
        self.check()?;
        self.documents = self.documents.saturating_add(chunk);
        if file_bytes > self.limits.document_bytes
            || self.documents > self.limits.total_document_bytes
        {
            Err(ReadFailure::LimitExceeded(Resource::DocumentBytes))
        } else {
            Ok(())
        }
    }
    pub fn expansion(&mut self, bytes: usize) -> ReadResult<()> {
        self.check()?;
        self.expanded = self
            .expanded
            .checked_add(bytes)
            .ok_or(ReadFailure::LimitExceeded(Resource::DocumentBytes))?;
        if self.expanded > self.limits.total_document_bytes {
            Err(ReadFailure::LimitExceeded(Resource::DocumentBytes))
        } else {
            Ok(())
        }
    }
    pub fn node(&mut self, depth: usize) -> ReadResult<()> {
        self.check()?;
        if depth > self.limits.depth {
            return Err(ReadFailure::LimitExceeded(Resource::Depth));
        }
        self.nodes = self.nodes.saturating_add(1);
        if self.nodes > self.limits.nodes {
            Err(ReadFailure::LimitExceeded(Resource::Nodes))
        } else {
            Ok(())
        }
    }
    pub fn visit(&mut self, depth: usize) -> ReadResult<()> {
        self.check()?;
        if depth > self.limits.depth {
            return Err(ReadFailure::LimitExceeded(Resource::Depth));
        }
        self.visits = self.visits.saturating_add(1);
        if self.visits > self.limits.visits {
            Err(ReadFailure::LimitExceeded(Resource::Visits))
        } else {
            Ok(())
        }
    }
    pub fn rule(&mut self) -> ReadResult<()> {
        self.check()?;
        self.rules = self.rules.saturating_add(1);
        if self.rules > self.limits.rules {
            Err(ReadFailure::LimitExceeded(Resource::Rules))
        } else {
            Ok(())
        }
    }
    /// Charges transient path-comparison work, which the byte caps do not cover.
    pub fn comparison(&mut self, work: usize) -> ReadResult<()> {
        self.check()?;
        self.compared = self.compared.saturating_add(work);
        if self.compared > self.limits.comparison_work {
            Err(ReadFailure::LimitExceeded(Resource::ComparisonWork))
        } else {
            Ok(())
        }
    }
}
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum StlPurpose {
    Revision,
    Untracked,
    CheckoutOptions,
}
/// A storage-issued STL read request. Adapters cannot manufacture requests.
///
/// ```compile_fail
/// use pp_storage::working_drafts::observation::{OwnedStlReadRequest, StlPurpose};
/// let request = OwnedStlReadRequest {
///     tenant: "foreign".into(), source_id: 1, path: None, purpose: StlPurpose::Revision,
/// };
/// ```
pub struct OwnedStlReadRequest {
    pub(crate) tenant: String,
    pub(crate) source_id: i64,
    pub(crate) path: Option<String>,
    pub(crate) purpose: StlPurpose,
}
impl OwnedStlReadRequest {
    pub fn tenant(&self) -> &str {
        &self.tenant
    }
    pub fn source_id(&self) -> i64 {
        self.source_id
    }
    pub fn stored_path(&self) -> Option<&str> {
        self.path.as_deref()
    }
    pub fn purpose(&self) -> StlPurpose {
        self.purpose
    }
}
/// A storage-issued checkout read request.
///
/// ```compile_fail
/// use pp_storage::working_drafts::observation::OwnedCheckoutReadRequest;
/// let request = OwnedCheckoutReadRequest {
///     tenant: "foreign".into(), source_id: 1, path: "/etc".into(),
/// };
/// ```
pub struct OwnedCheckoutReadRequest {
    pub(crate) tenant: String,
    pub(crate) source_id: i64,
    pub(crate) path: String,
}
impl OwnedCheckoutReadRequest {
    pub fn tenant(&self) -> &str {
        &self.tenant
    }
    pub fn source_id(&self) -> i64 {
        self.source_id
    }
    pub fn stored_path(&self) -> &str {
        &self.path
    }
}
#[derive(Clone, Copy)]
pub enum CheckoutManifestUse {
    OptionGroups,
    PartRulesAndDefaults,
}
/// The `<slug>` in a community manifest's `<slug>.yaml`. It is always a
/// single path segment, so an adapter can join it to the manifests root.
/// The adapter must still refuse a resolved path that leaves the root
/// through a symlink, as Node's `resolvedFileUnderRoot` does.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CommunitySlug(String);
impl CommunitySlug {
    /// Node's `isCommunityManifestSlug`: `^[a-z0-9][a-z0-9._-]{0,63}$`, minus
    /// the stems of other documents in the manifests directory, so they are
    /// never read as community manifests. `registry-index.yaml` ships there,
    /// and Node's kit catalog loader also looks there for a deployment-provided
    /// `kit-catalog.yaml`.
    pub fn parse(value: &str) -> Option<Self> {
        let bytes = value.as_bytes();
        // ast-grep-ignore: no-writer-family-literal -- Refuses embedded catalog document names as slugs; reads and writes nothing.
        let valid = !matches!(value, "kit-catalog" | "registry-index")
            && matches!(bytes.first(), Some(b'a'..=b'z' | b'0'..=b'9'))
            && bytes.len() <= 64
            && bytes
                .iter()
                .all(|byte| matches!(byte, b'a'..=b'z' | b'0'..=b'9' | b'.' | b'_' | b'-'));
        valid.then(|| Self(value.to_owned()))
    }
    pub fn as_str(&self) -> &str {
        &self.0
    }
}
#[derive(Clone)]
pub enum CatalogPurpose {
    Community(CommunitySlug),
    ShippedHintsFirst,
    ShippedHintsSecond,
    CustomHints,
}
/// A storage-issued catalog read request.
///
/// ```compile_fail
/// use pp_storage::working_drafts::observation::{OwnedCatalogReadRequest, CatalogPurpose};
/// let request = OwnedCatalogReadRequest { purpose: CatalogPurpose::CustomHints };
/// ```
pub struct OwnedCatalogReadRequest {
    pub(crate) purpose: CatalogPurpose,
}
impl OwnedCatalogReadRequest {
    pub fn purpose(&self) -> &CatalogPurpose {
        &self.purpose
    }
}
pub enum DocumentRead {
    Missing,
    Skipped(std::io::ErrorKind),
    Bytes(Vec<u8>),
}
pub struct ArtifactRead {
    pub byte_count: u64,
    pub byte_sha256: String,
}
pub struct InventoryPath {
    pub physical_relative_path: String,
    pub traversal_ordinal: usize,
}
/// Observation adapter for storage-issued requests. Implementations enforce
/// locator safety and charge the supplied budget before performing reads.
pub trait DraftReads: Send + Sync {
    fn resolve_stl_root(
        &self,
        request: &OwnedStlReadRequest,
        budget: &mut PreparationBudget<'_>,
    ) -> ReadResult<Box<dyn StlRootRead>>;
    fn read_checkout_manifest(
        &self,
        request: &OwnedCheckoutReadRequest,
        occurrence: CheckoutManifestUse,
        budget: &mut PreparationBudget<'_>,
    ) -> ReadResult<DocumentRead>;
    fn read_catalog_document(
        &self,
        request: &OwnedCatalogReadRequest,
        budget: &mut PreparationBudget<'_>,
    ) -> ReadResult<DocumentRead>;
}
pub trait StlRootRead: Send {
    fn logical_resolved_path(&self) -> Option<&str>;
    fn scan_stls(
        &mut self,
        budget: &mut PreparationBudget<'_>,
    ) -> ReadResult<Box<dyn StlInventory>>;
}
pub trait StlInventory: Send {
    fn entries(&self) -> &[InventoryPath];
    fn hash_tracked_winner(
        &mut self,
        index: usize,
        budget: &mut PreparationBudget<'_>,
    ) -> ReadResult<ArtifactRead>;
}
