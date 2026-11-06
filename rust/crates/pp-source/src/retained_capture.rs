use crate::{
    Directory, Error, LocalFiles, MAX_ENTRIES, PathCollisions, Result, SourcePath,
    local_selection::InputFile,
};
use pp_os::Identity;
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    fs::File,
    io::{Read, Write},
    path::Path,
    sync::atomic::{AtomicBool, Ordering},
};

#[cfg(test)]
#[derive(Default)]
struct TraceState {
    events: Vec<&'static str>,
    fail_at: Option<usize>,
    second_fail_at: Option<usize>,
    add_slot_sibling_at_unlink: bool,
    replace_deleting_at_unlink: bool,
    replace_slot_at_unlink: Option<std::path::PathBuf>,
}

#[derive(Clone, Default)]
struct Trace {
    #[cfg(test)]
    state: Option<std::sync::Arc<std::sync::Mutex<TraceState>>>,
}

impl Trace {
    fn before(&self, _event: &'static str) -> Result<()> {
        #[cfg(test)]
        if let Some(state) = &self.state {
            let mut state = state.lock().map_err(|_| Error::CorruptSnapshot)?;
            let index = state.events.len();
            state.events.push(_event);
            if state.fail_at == Some(index) || state.second_fail_at == Some(index) {
                return Err(Error::Io(std::io::Error::other("injected I/O failure")));
            }
        }
        Ok(())
    }

    fn before_slot_remove(&self, slot: &Directory) -> Result<()> {
        self.before("slot_remove")?;
        let _ = slot;
        #[cfg(test)]
        if let Some(state) = &self.state {
            let add_sibling = state
                .lock()
                .map_err(|_| Error::CorruptSnapshot)?
                .add_slot_sibling_at_unlink;
            if add_sibling {
                let mut evidence = slot.file_exclusive("late-evidence")?;
                evidence.write_all(b"preserve")?;
                evidence.sync_all()?;
                slot.sync()?;
            }
            let replacement = state
                .lock()
                .map_err(|_| Error::CorruptSnapshot)?
                .replace_slot_at_unlink
                .take();
            if let Some(path) = replacement {
                std::fs::rename(&path, path.with_extension("original"))?;
                std::fs::create_dir(&path)?;
            }
        }
        Ok(())
    }

    fn before_deleting_remove(&self, slot: &Directory) -> Result<()> {
        let _ = slot;
        #[cfg(test)]
        if let Some(state) = &self.state {
            let replace = state
                .lock()
                .map_err(|_| Error::CorruptSnapshot)?
                .replace_deleting_at_unlink;
            if replace {
                slot.rename_no_replace(DELETING, "original-deleting")?;
                slot.create_child_exclusive(DELETING)?;
                slot.sync()?;
            }
        }
        Ok(())
    }
}

const VAULT: &str = "source-captures";
const CANDIDATE: &str = "candidate";
const FROZEN: &str = "frozen";
const DELETING: &str = "deleting";
const INPUT: &str = "input";
const MANIFEST: &str = "capture-manifest.json";
const CAPTURE_ID_BYTES: usize = 32;
const MAX_ALLOCATION_ATTEMPTS: usize = 16;
const MAX_MANIFEST_BYTES: usize = 8 * 1024 * 1024;

#[derive(Clone, PartialEq, Eq)]
pub struct RetainedCaptureId(String);

impl std::fmt::Debug for RetainedCaptureId {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("RetainedCaptureId([redacted])")
    }
}

impl RetainedCaptureId {
    pub fn parse(value: impl Into<String>) -> Result<Self> {
        let value = value.into();
        if value.len() != CAPTURE_ID_BYTES * 2
            || !value
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        {
            return Err(Error::InvalidIdentity);
        }
        Ok(Self(value))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

pub struct RetainedCaptureVault {
    parent: Directory,
    trace: Trace,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RetainedCapturePhase {
    Candidate,
    Frozen,
    Deleting,
    Malformed,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RetainedCaptureEntry {
    id: RetainedCaptureId,
    phase: RetainedCapturePhase,
}

impl RetainedCaptureEntry {
    pub fn capture_id(&self) -> &RetainedCaptureId {
        &self.id
    }

    pub fn phase(&self) -> RetainedCapturePhase {
        self.phase
    }
}

pub struct CaptureCandidate {
    parent: Directory,
    slot: Directory,
    candidate: Directory,
    input: Owned,
    id: RetainedCaptureId,
    paths: Vec<SourcePath>,
    written: Vec<InputFile>,
    collisions: PathCollisions,
    owned_tree: OwnedTree,
    bytes: u64,
    failed: bool,
    trace: Trace,
}

/// An entry this capture created or checked, recorded by identity instead of
/// by an open handle. Windows refuses to rename a directory while any handle
/// below it is open, so a capture holds no handle below its phase directory
/// (`candidate`, `frozen` or `deleting`) and reopens entries by name, checking
/// this identity before it trusts or removes them. NTFS file IDs carry a
/// sequence number, so a freed ID is not handed to a new file. Unix inode
/// numbers can be reused once freed, so on Unix the entry also keeps an open
/// descriptor that pins the inode; Unix renames ignore open descriptors.
struct Owned {
    identity: Identity,
    #[cfg(unix)]
    _pin: File,
}

impl Owned {
    fn file(file: &File) -> Result<Self> {
        Ok(Self {
            identity: pp_os::status_of(file)?.identity(),
            #[cfg(unix)]
            _pin: file.try_clone()?,
        })
    }

    fn directory(directory: &Directory) -> Result<Self> {
        Ok(Self {
            identity: directory.identity()?,
            #[cfg(unix)]
            _pin: {
                use std::os::fd::AsFd;
                File::from(directory.0.as_fd().try_clone_to_owned()?)
            },
        })
    }
}

/// An owned entry named by its `/`-separated path below the phase directory,
/// which keeps naming it after the phase directory is renamed.
struct OwnedEntry {
    path: String,
    owned: Owned,
}

#[derive(Default)]
struct OwnedTree {
    files: Vec<OwnedEntry>,
    directories: Vec<OwnedEntry>,
}

#[derive(Default)]
struct ExpectedDirectory(BTreeMap<String, ExpectedEntry>);

enum ExpectedEntry {
    File,
    Directory(ExpectedDirectory),
}

impl OwnedTree {
    /// Checks that every recorded path below `root` still names the entry
    /// this capture recorded there.
    fn validate(&self, root: &Directory) -> Result<()> {
        for entry in &self.files {
            let (parent, name) = root.parent(&entry.path, false)?;
            parent.require_owned_file(&name, entry.owned.identity)?;
        }
        for entry in &self.directories {
            let (parent, name) = root.parent(&entry.path, false)?;
            parent.require_owned_directory(&name, entry.owned.identity)?;
        }
        Ok(())
    }

    fn remove(self, root: &Directory) -> Result<()> {
        self.validate(root)?;
        for entry in self.files.into_iter().rev() {
            let (parent, name) = root.parent(&entry.path, false)?;
            parent.remove_owned_file(&name, entry.owned.identity)?;
        }
        for entry in self.directories.into_iter().rev() {
            let (parent, name) = root.parent(&entry.path, false)?;
            parent.remove_empty_directory_owned_as(&name, entry.owned.identity)?;
        }
        Ok(())
    }

    /// Records the tree below `input`, which must hold exactly `paths`.
    fn open_exact(input: &Directory, paths: &[SourcePath]) -> Result<Self> {
        let mut expected = ExpectedDirectory::default();
        for path in paths {
            expected.insert(path)?;
        }
        let mut owned = Self::default();
        owned.open_directory(input, &format!("{INPUT}/"), &expected)?;
        Ok(owned)
    }

    fn open_directory(
        &mut self,
        directory: &Directory,
        prefix: &str,
        expected: &ExpectedDirectory,
    ) -> Result<()> {
        let mut actual = directory.entries()?;
        actual.sort();
        let expected_names = expected.0.keys().cloned().collect::<Vec<_>>();
        if actual != expected_names {
            return Err(Error::CorruptSnapshot);
        }
        for (name, entry) in &expected.0 {
            let path = format!("{prefix}{name}");
            match entry {
                ExpectedEntry::File => {
                    let owned = Owned::file(&directory.file(name, false)?)?;
                    self.files.push(OwnedEntry { path, owned });
                }
                ExpectedEntry::Directory(expected) => {
                    let child = directory.child(name, false)?;
                    self.directories.push(OwnedEntry {
                        path: path.clone(),
                        owned: Owned::directory(&child)?,
                    });
                    self.open_directory(&child, &format!("{path}/"), expected)?;
                }
            }
        }
        Ok(())
    }
}

/// Opens the phase directory's `input` only while it is still `owned`.
fn open_input(phase: &Directory, owned: &Owned) -> Result<Directory> {
    let input = phase.child(INPUT, false)?;
    if input.identity()? != owned.identity {
        return Err(Error::CorruptSnapshot);
    }
    Ok(input)
}

impl ExpectedDirectory {
    fn insert(&mut self, path: &SourcePath) -> Result<()> {
        let mut directory = self;
        let mut segments = path.as_str().split('/').peekable();
        while let Some(segment) = segments.next() {
            if segments.peek().is_none() {
                if directory
                    .0
                    .insert(segment.to_owned(), ExpectedEntry::File)
                    .is_some()
                {
                    return Err(Error::DuplicatePath);
                }
                return Ok(());
            }
            let entry = directory
                .0
                .entry(segment.to_owned())
                .or_insert_with(|| ExpectedEntry::Directory(ExpectedDirectory::default()));
            let ExpectedEntry::Directory(next) = entry else {
                return Err(Error::DuplicatePath);
            };
            directory = next;
        }
        Err(Error::UnsafePath)
    }
}

pub struct FrozenCapture {
    parent: Directory,
    slot: Directory,
    frozen: Directory,
    input: Owned,
    id: RetainedCaptureId,
    owned_tree: Option<OwnedTree>,
    trace: Trace,
}

#[derive(Clone, PartialEq, Eq)]
pub struct RetainedCaptureReceipt {
    id: RetainedCaptureId,
    phase: RetainedCapturePhase,
}

impl std::fmt::Debug for RetainedCaptureReceipt {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("RetainedCaptureReceipt")
            .field("id", &self.id)
            .field("phase", &self.phase)
            .finish()
    }
}

impl RetainedCaptureReceipt {
    pub fn capture_id(&self) -> &RetainedCaptureId {
        &self.id
    }

    pub fn phase(&self) -> RetainedCapturePhase {
        self.phase
    }
}

#[derive(Debug)]
pub struct CandidateCleanupError {
    receipt: RetainedCaptureReceipt,
    source: Error,
}

impl CandidateCleanupError {
    pub fn receipt(&self) -> &RetainedCaptureReceipt {
        &self.receipt
    }
}

impl std::fmt::Display for CandidateCleanupError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            formatter,
            "candidate cleanup retained repair evidence: {}",
            self.source
        )
    }
}

impl std::error::Error for CandidateCleanupError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(&self.source)
    }
}

#[derive(Debug)]
pub struct CandidateAllocationError {
    retained: Option<RetainedCaptureReceipt>,
    source: Error,
}

impl CandidateAllocationError {
    pub fn retained(&self) -> Option<&RetainedCaptureReceipt> {
        self.retained.as_ref()
    }
}

impl std::fmt::Display for CandidateAllocationError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "candidate allocation failed: {}", self.source)
    }
}

impl std::error::Error for CandidateAllocationError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(&self.source)
    }
}

pub enum FreezeResult {
    Frozen(FrozenCapture),
    FrozenWithError {
        capture: FrozenCapture,
        error: Error,
    },
    CandidateError {
        candidate: CaptureCandidate,
        error: Error,
    },
}

pub struct RetainedInputTransfer<'a> {
    pub paths: &'a [SourcePath],
    pub owned_root: &'a Path,
    pub operation: &'a str,
    pub max_bytes: u64,
    pub cancelled: &'a AtomicBool,
}

pub struct RetainedOwnedInput {
    inventory: Vec<InputFile>,
}

impl RetainedOwnedInput {
    pub fn inventory(&self) -> &[InputFile] {
        &self.inventory
    }

    pub fn into_inventory(self) -> Vec<InputFile> {
        self.inventory
    }
}

impl RetainedCaptureVault {
    pub fn open(data_dir: &Path) -> Result<Self> {
        let data = Directory::open_root(data_dir)?;
        Ok(Self {
            parent: data.child(VAULT, true)?,
            trace: Trace::default(),
        })
    }

    pub fn allocate(&self) -> Result<CaptureCandidate> {
        self.allocate_owned().map_err(|error| error.source)
    }

    pub fn allocate_owned(
        &self,
    ) -> std::result::Result<CaptureCandidate, CandidateAllocationError> {
        self.allocate_owned_with(|| {
            let mut bytes = [0u8; CAPTURE_ID_BYTES];
            getrandom::fill(&mut bytes).map_err(|_| Error::Entropy)?;
            RetainedCaptureId::parse(hex::encode(bytes))
        })
    }

    #[cfg(test)]
    fn allocate_with(
        &self,
        mut next_id: impl FnMut() -> Result<RetainedCaptureId>,
    ) -> Result<CaptureCandidate> {
        self.allocate_owned_with(&mut next_id)
            .map_err(|error| error.source)
    }

    fn allocate_owned_with(
        &self,
        mut next_id: impl FnMut() -> Result<RetainedCaptureId>,
    ) -> std::result::Result<CaptureCandidate, CandidateAllocationError> {
        for _ in 0..MAX_ALLOCATION_ATTEMPTS {
            let id = next_id().map_err(|source| CandidateAllocationError {
                retained: None,
                source,
            })?;
            self.trace
                .before("slot_create")
                .map_err(|source| CandidateAllocationError {
                    retained: None,
                    source,
                })?;
            let slot = match self.parent.create_child_exclusive(id.as_str()) {
                Ok(slot) => slot,
                Err(Error::Io(error)) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                    continue;
                }
                Err(source) => {
                    return Err(CandidateAllocationError {
                        retained: None,
                        source,
                    });
                }
            };
            let allocated = (|| -> Result<CaptureCandidate> {
                self.trace.before("parent_sync")?;
                self.parent.sync()?;
                self.trace.before("candidate_create")?;
                let candidate = slot.create_child_exclusive(CANDIDATE)?;
                self.trace.before("slot_sync")?;
                slot.sync()?;
                self.trace.before("input_create")?;
                let input = Owned::directory(&candidate.create_child_exclusive(INPUT)?)?;
                self.trace.before("candidate_sync")?;
                candidate.sync()?;
                Ok(CaptureCandidate {
                    parent: self.parent.try_clone()?,
                    slot: slot.try_clone()?,
                    candidate,
                    input,
                    id: id.clone(),
                    paths: Vec::new(),
                    written: Vec::new(),
                    collisions: PathCollisions::default(),
                    owned_tree: OwnedTree::default(),
                    bytes: 0,
                    failed: false,
                    trace: self.trace.clone(),
                })
            })();
            return match allocated {
                Ok(candidate) => Ok(candidate),
                Err(source) => {
                    let mut phase = Some(RetainedCapturePhase::Malformed);
                    let _ =
                        cleanup_generated_slot(&self.parent, slot, &id, &self.trace, &mut phase);
                    let retained = phase.map(|phase| RetainedCaptureReceipt { id, phase });
                    Err(CandidateAllocationError { retained, source })
                }
            };
        }
        Err(CandidateAllocationError {
            retained: None,
            source: Error::CaptureCollisionLimit,
        })
    }

    pub fn open_frozen(&self, id: &RetainedCaptureId) -> Result<FrozenCapture> {
        let slot = self.parent.child(id.as_str(), false)?;
        require_only_entry(&slot, FROZEN)?;
        let frozen = slot.child(FROZEN, false)?;
        let mut entries = frozen.entries()?;
        entries.sort();
        if entries != [MANIFEST.to_owned(), INPUT.to_owned()] {
            return Err(Error::CorruptSnapshot);
        }
        let input = Owned::directory(&frozen.child(INPUT, false)?)?;
        Ok(FrozenCapture {
            parent: self.parent.try_clone()?,
            slot,
            frozen,
            input,
            id: id.clone(),
            owned_tree: None,
            trace: self.trace.clone(),
        })
    }

    pub fn scan(&self) -> Result<Vec<RetainedCaptureEntry>> {
        let mut result = Vec::new();
        for name in self.parent.entries()? {
            let id = RetainedCaptureId::parse(name)?;
            let slot = self.parent.child(id.as_str(), false)?;
            let entries = slot.entries()?;
            let phase = match entries.as_slice() {
                [name] if name == CANDIDATE => RetainedCapturePhase::Candidate,
                [name] if name == FROZEN => RetainedCapturePhase::Frozen,
                [name] if name == DELETING => RetainedCapturePhase::Deleting,
                _ => RetainedCapturePhase::Malformed,
            };
            result.push(RetainedCaptureEntry { id, phase });
        }
        result.sort_by(|left, right| left.id.as_str().cmp(right.id.as_str()));
        Ok(result)
    }

    pub fn finish_deleting(&self) -> Result<usize> {
        let mut removed = 0;
        for name in self.parent.entries()? {
            let id = match RetainedCaptureId::parse(name) {
                Ok(id) => id,
                Err(_) => return Err(Error::CorruptSnapshot),
            };
            let slot = self.parent.child(id.as_str(), false)?;
            match slot.entries()?.as_slice() {
                [name] if name == DELETING => {
                    let deleting = slot.child(DELETING, false)?;
                    if !deleting.entries()?.is_empty() {
                        return Err(Error::CorruptSnapshot);
                    }
                    self.trace.before("tree_remove")?;
                    self.trace.before_deleting_remove(&slot)?;
                    slot.remove_owned_empty_directory(DELETING, deleting)?;
                    self.trace.before_slot_remove(&slot)?;
                    self.parent
                        .remove_owned_empty_directory(id.as_str(), slot)?;
                    self.trace.before("parent_sync")?;
                    self.parent.sync()?;
                    removed += 1;
                }
                [name] if name == CANDIDATE || name == FROZEN => {}
                _ => return Err(Error::CorruptSnapshot),
            }
        }
        Ok(removed)
    }
}

fn require_only_entry(directory: &Directory, expected: &str) -> Result<()> {
    let entries = directory.entries()?;
    if entries.len() != 1 || entries[0] != expected {
        return Err(Error::CorruptSnapshot);
    }
    Ok(())
}

fn cleanup_generated_slot(
    parent: &Directory,
    slot: Directory,
    id: &RetainedCaptureId,
    trace: &Trace,
    phase: &mut Option<RetainedCapturePhase>,
) -> Result<()> {
    match slot.entries()?.as_slice() {
        [] => {}
        [name] if name == CANDIDATE => {
            *phase = Some(RetainedCapturePhase::Candidate);
            let candidate = slot.child(CANDIDATE, false)?;
            match candidate.entries()?.as_slice() {
                [] => {}
                [name] if name == INPUT => {
                    let input = candidate.child(INPUT, false)?;
                    candidate.remove_owned_empty_directory(INPUT, input)?;
                }
                _ => return Err(Error::CorruptSnapshot),
            }
            trace.before("deleting_rename")?;
            slot.rename_no_replace(CANDIDATE, DELETING)?;
            *phase = Some(RetainedCapturePhase::Deleting);
            trace.before("slot_sync")?;
            slot.sync()?;
            require_only_entry(&slot, DELETING)?;
            trace.before("tree_remove")?;
            slot.remove_owned_empty_directory(DELETING, candidate)?;
            *phase = Some(RetainedCapturePhase::Malformed);
        }
        _ => return Err(Error::CorruptSnapshot),
    }
    trace.before_slot_remove(&slot)?;
    parent.remove_owned_empty_directory(id.as_str(), slot)?;
    *phase = None;
    trace.before("parent_sync")?;
    parent.sync()
}

impl CaptureCandidate {
    pub fn capture_id(&self) -> &RetainedCaptureId {
        &self.id
    }

    pub fn write_file(
        &mut self,
        path: SourcePath,
        bytes: &[u8],
        max_bytes: u64,
    ) -> Result<InputFile> {
        self.write_file_from(path, bytes, max_bytes)
    }

    pub fn write_file_from(
        &mut self,
        path: SourcePath,
        input: impl Read,
        max_bytes: u64,
    ) -> Result<InputFile> {
        self.write_file_from_cancelled(path, input, max_bytes, &AtomicBool::new(false))
    }

    pub fn write_file_from_cancelled(
        &mut self,
        path: SourcePath,
        input: impl Read,
        max_bytes: u64,
        cancelled: &AtomicBool,
    ) -> Result<InputFile> {
        if self.failed {
            return Err(Error::CorruptSnapshot);
        }
        let result = self.write_file_inner(path, input, max_bytes, cancelled);
        if result.is_err() {
            self.failed = true;
        }
        result
    }

    fn write_file_inner(
        &mut self,
        path: SourcePath,
        mut input: impl Read,
        max_bytes: u64,
        cancelled: &AtomicBool,
    ) -> Result<InputFile> {
        if self.paths.len() >= MAX_ENTRIES {
            return Err(Error::Limit);
        }
        self.collisions.insert(&path, false)?;

        let mut parent = open_input(&self.candidate, &self.input)?;
        let mut relative = INPUT.to_owned();
        let mut segments = path.as_str().split('/').peekable();
        let file_name = loop {
            let segment = segments.next().ok_or(Error::UnsafePath)?;
            relative.push('/');
            relative.push_str(segment);
            if segments.peek().is_none() {
                break segment;
            }
            let (next, created) = parent.create_child_unsynced(segment)?;
            if created {
                self.owned_tree.directories.push(OwnedEntry {
                    path: relative.clone(),
                    owned: Owned::directory(&next)?,
                });
            }
            parent = next;
        };

        let mut file = parent.file_exclusive(file_name)?;
        self.owned_tree.files.push(OwnedEntry {
            path: relative,
            owned: Owned::file(&file)?,
        });
        let mut hash = Sha256::new();
        let mut size = 0u64;
        let mut buffer = [0u8; 64 * 1024];
        loop {
            if cancelled.load(Ordering::Acquire) {
                return Err(Error::Limit);
            }
            let read = input.read(&mut buffer)?;
            if cancelled.load(Ordering::Acquire) {
                return Err(Error::Limit);
            }
            if read == 0 {
                break;
            }
            size = size.checked_add(read as u64).ok_or(Error::Limit)?;
            let total = self.bytes.checked_add(size).ok_or(Error::Limit)?;
            if total > max_bytes {
                return Err(Error::Limit);
            }
            file.write_all(&buffer[..read])?;
            hash.update(&buffer[..read]);
        }
        self.trace.before("file_sync")?;
        file.sync_all()?;
        let result = InputFile {
            path: path.clone(),
            size,
            sha256: hex::encode(hash.finalize()),
        };
        self.paths.push(path);
        self.written.push(result.clone());
        self.bytes = self.bytes.checked_add(size).ok_or(Error::Limit)?;
        Ok(result)
    }

    pub fn inventory(&self, max_bytes: u64) -> Result<Vec<InputFile>> {
        LocalFiles {
            root: open_input(&self.candidate, &self.input)?,
        }
        .inventory(&self.paths, max_bytes)
    }

    pub fn abort(self) -> std::result::Result<(), CandidateCleanupError> {
        let CaptureCandidate {
            parent,
            slot,
            candidate,
            input,
            id,
            owned_tree,
            trace,
            ..
        } = self;
        let mut phase = RetainedCapturePhase::Candidate;
        let result = (|| {
            require_only_entry(&slot, CANDIDATE)?;
            trace.before("deleting_rename")?;
            slot.rename_no_replace(CANDIDATE, DELETING)?;
            phase = RetainedCapturePhase::Deleting;
            trace.before("slot_sync")?;
            slot.sync()?;
            require_only_entry(&slot, DELETING)?;
            owned_tree.remove(&candidate)?;
            candidate.remove_empty_directory_owned_as(INPUT, input.identity)?;
            trace.before("tree_remove")?;
            slot.remove_owned_empty_directory(DELETING, candidate)?;
            trace.before_slot_remove(&slot)?;
            parent.remove_owned_empty_directory(id.as_str(), slot)?;
            trace.before("parent_sync")?;
            parent.sync()
        })();
        result.map_err(|source| CandidateCleanupError {
            receipt: RetainedCaptureReceipt { id, phase },
            source,
        })
    }

    pub fn freeze_owned(self, manifest: &[u8]) -> FreezeResult {
        let mut candidate = self;
        let before_rename = (|| -> Result<()> {
            if candidate.failed {
                return Err(Error::CorruptSnapshot);
            }
            if candidate.paths.is_empty()
                || manifest.is_empty()
                || manifest.len() > MAX_MANIFEST_BYTES
            {
                return Err(Error::Limit);
            }
            require_only_entry(&candidate.slot, CANDIDATE)?;
            require_only_entry(&candidate.candidate, INPUT)?;
            let input = open_input(&candidate.candidate, &candidate.input)?;
            OwnedTree::open_exact(&input, &candidate.paths)?;
            candidate.owned_tree.validate(&candidate.candidate)?;
            let actual = LocalFiles {
                root: input.try_clone()?,
            }
            .inventory(&candidate.paths, candidate.bytes)?;
            if actual != candidate.written {
                return Err(Error::CorruptSnapshot);
            }

            for owned in candidate.owned_tree.directories.iter().rev() {
                candidate.trace.before("directory_sync")?;
                let (parent, name) = candidate.candidate.parent(&owned.path, false)?;
                parent.child(&name, false)?.sync()?;
            }
            candidate.trace.before("input_sync")?;
            input.sync()?;
            drop(input);

            candidate.trace.before("manifest_create")?;
            let mut manifest_file = candidate.candidate.file_exclusive(MANIFEST)?;
            candidate.owned_tree.files.push(OwnedEntry {
                path: MANIFEST.to_owned(),
                owned: Owned::file(&manifest_file)?,
            });
            manifest_file.write_all(manifest)?;
            candidate.trace.before("manifest_sync")?;
            manifest_file.sync_all()?;
            drop(manifest_file);
            candidate.trace.before("candidate_sync")?;
            candidate.candidate.sync()?;
            let mut entries = candidate.candidate.entries()?;
            entries.sort();
            if entries != [MANIFEST.to_owned(), INPUT.to_owned()] {
                return Err(Error::CorruptSnapshot);
            }
            candidate.trace.before("freeze_rename")?;
            candidate.slot.rename_no_replace(CANDIDATE, FROZEN)?;
            Ok(())
        })();
        if let Err(error) = before_rename {
            return FreezeResult::CandidateError { candidate, error };
        }
        let frozen = FrozenCapture {
            parent: candidate.parent,
            slot: candidate.slot,
            frozen: candidate.candidate,
            input: candidate.input,
            id: candidate.id,
            owned_tree: Some(candidate.owned_tree),
            trace: candidate.trace,
        };
        if let Err(error) = frozen
            .trace
            .before("slot_sync")
            .and_then(|()| frozen.slot.sync())
        {
            return FreezeResult::FrozenWithError {
                capture: frozen,
                error,
            };
        }
        FreezeResult::Frozen(frozen)
    }
}

impl FrozenCapture {
    pub fn capture_id(&self) -> &RetainedCaptureId {
        &self.id
    }

    pub fn manifest(&self) -> Result<Vec<u8>> {
        use std::io::Read;
        let mut bytes = Vec::new();
        self.frozen
            .file(MANIFEST, false)?
            .take((MAX_MANIFEST_BYTES + 1) as u64)
            .read_to_end(&mut bytes)?;
        if bytes.len() > MAX_MANIFEST_BYTES {
            return Err(Error::Limit);
        }
        Ok(bytes)
    }

    pub fn receipt(&self) -> RetainedCaptureReceipt {
        RetainedCaptureReceipt {
            id: self.id.clone(),
            phase: RetainedCapturePhase::Frozen,
        }
    }

    pub fn stabilize(&self) -> Result<()> {
        require_only_entry(&self.slot, FROZEN)?;
        let entries = self.frozen.entries()?;
        if entries.len() != 2
            || !entries.iter().any(|entry| entry == INPUT)
            || !entries.iter().any(|entry| entry == MANIFEST)
        {
            return Err(Error::CorruptSnapshot);
        }
        open_input(&self.frozen, &self.input)?.sync()?;
        self.frozen.sync()?;
        self.slot.sync()
    }

    pub fn inventory(&self, paths: &[SourcePath], max_bytes: u64) -> Result<Vec<InputFile>> {
        LocalFiles {
            root: open_input(&self.frozen, &self.input)?,
        }
        .inventory(paths, max_bytes)
    }

    pub fn transfer_owned_input(
        &self,
        transfer: RetainedInputTransfer<'_>,
    ) -> Result<RetainedOwnedInput> {
        let local = LocalFiles {
            root: open_input(&self.frozen, &self.input)?,
        };
        let (_owned, inventory) = local.capture(
            transfer.paths,
            transfer.owned_root,
            transfer.operation,
            transfer.max_bytes,
            transfer.cancelled,
        )?;
        Ok(RetainedOwnedInput { inventory })
    }

    pub fn remove(self, paths: &[SourcePath]) -> Result<()> {
        require_only_entry(&self.slot, FROZEN)?;
        let mut frozen_entries = self.frozen.entries()?;
        frozen_entries.sort();
        if frozen_entries != [MANIFEST.to_owned(), INPUT.to_owned()] {
            return Err(Error::CorruptSnapshot);
        }
        let mut observed = OwnedTree::open_exact(&open_input(&self.frozen, &self.input)?, paths)?;
        observed.files.push(OwnedEntry {
            path: MANIFEST.to_owned(),
            owned: Owned::file(&self.frozen.file(MANIFEST, false)?)?,
        });
        let owned = self.owned_tree.unwrap_or(observed);
        self.trace.before("deleting_rename")?;
        self.slot.rename_no_replace(FROZEN, DELETING)?;
        self.trace.before("slot_sync")?;
        self.slot.sync()?;
        require_only_entry(&self.slot, DELETING)?;
        owned.remove(&self.frozen)?;
        self.frozen
            .remove_empty_directory_owned_as(INPUT, self.input.identity)?;
        self.trace.before("tree_remove")?;
        self.slot
            .remove_owned_empty_directory(DELETING, self.frozen)?;
        self.trace.before_slot_remove(&self.slot)?;
        self.parent
            .remove_owned_empty_directory(self.id.as_str(), self.slot)?;
        self.trace.before("parent_sync")?;
        self.parent.sync()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scratch::Scratch;
    use std::{
        fs,
        path::PathBuf,
        sync::{Arc, Mutex},
    };

    /// Returns the root first so that, on panic, the vault drops before the
    /// scratch directory is removed.
    fn traced_vault(name: &str) -> (Scratch, RetainedCaptureVault, Arc<Mutex<TraceState>>) {
        let root = Scratch::new(name);
        let state = Arc::new(Mutex::new(TraceState::default()));
        let mut vault = RetainedCaptureVault::open(&root).unwrap();
        vault.trace.state = Some(state.clone());
        (root, vault, state)
    }

    fn freeze(candidate: CaptureCandidate, manifest: &[u8]) -> FrozenCapture {
        match candidate.freeze_owned(manifest) {
            FreezeResult::Frozen(frozen) => frozen,
            FreezeResult::FrozenWithError { error, .. }
            | FreezeResult::CandidateError { error, .. } => panic!("freeze failed: {error}"),
        }
    }

    fn candidate_with(vault: &RetainedCaptureVault, path: &str, bytes: &[u8]) -> CaptureCandidate {
        let mut candidate = vault.allocate().unwrap();
        candidate
            .write_file(SourcePath::try_from(path.to_owned()).unwrap(), bytes, 4)
            .unwrap();
        candidate
    }

    #[test]
    fn retained_capture_collision_retry() {
        let root = Scratch::new("collision");
        let vault = RetainedCaptureVault::open(&root).unwrap();
        let collision = RetainedCaptureId::parse("11".repeat(32)).unwrap();
        vault
            .parent
            .create_child_exclusive(collision.as_str())
            .unwrap();
        let success = RetainedCaptureId::parse("22".repeat(32)).unwrap();
        let mut ids = vec![collision.clone(), collision, success.clone()].into_iter();
        let candidate = vault.allocate_with(|| Ok(ids.next().unwrap())).unwrap();
        assert_eq!(candidate.capture_id(), &success);
    }

    #[test]
    fn retained_capture_freeze_sync_order_through_returned_failures() {
        let expected = [
            "directory_sync",
            "directory_sync",
            "input_sync",
            "manifest_create",
            "manifest_sync",
            "candidate_sync",
            "freeze_rename",
            "slot_sync",
        ];
        for fail_at in 0..=expected.len() {
            let (_root, vault, state) = traced_vault("fault-order");
            let candidate = candidate_with(&vault, "a/b/part.stl", b"mesh");
            {
                let mut state = state.lock().unwrap();
                state.events.clear();
                state.fail_at = (fail_at < expected.len()).then_some(fail_at);
            }
            let result = candidate.freeze_owned(b"manifest");
            let state = state.lock().unwrap();
            let observed = if fail_at < expected.len() {
                if fail_at < 7 {
                    assert!(matches!(
                        result,
                        FreezeResult::CandidateError {
                            error: Error::Io(_),
                            ..
                        }
                    ));
                } else {
                    assert!(matches!(
                        result,
                        FreezeResult::FrozenWithError {
                            error: Error::Io(_),
                            ..
                        }
                    ));
                }
                &expected[..=fail_at]
            } else {
                assert!(matches!(result, FreezeResult::Frozen(_)));
                &expected[..]
            };
            assert_eq!(state.events, observed);
            drop(state);
        }
    }

    #[test]
    fn retained_capture_holds_no_handle_below_its_phase_directory() {
        let (root, vault, _state) = traced_vault("phase-handles");
        let candidate = candidate_with(&vault, "a/part.stl", b"mesh");
        let slot = root.join(VAULT).join(candidate.capture_id().as_str());
        // Windows refuses these renames while any handle below the moved
        // directory is open.
        fs::rename(slot.join(CANDIDATE), slot.join("moved")).unwrap();
        fs::rename(slot.join("moved"), slot.join(CANDIDATE)).unwrap();
        let frozen = freeze(candidate, b"manifest");
        fs::rename(slot.join(FROZEN), slot.join("moved")).unwrap();
        fs::rename(slot.join("moved"), slot.join(FROZEN)).unwrap();
        frozen
            .remove(&[SourcePath::try_from("a/part.stl".to_owned()).unwrap()])
            .unwrap();
        assert!(vault.scan().unwrap().is_empty());
    }

    #[test]
    fn retained_frozen_remove_refuses_replaced_owned_entries() {
        let paths = [SourcePath::try_from("a/part.stl".to_owned()).unwrap()];
        for replaced in ["input/a/part.stl", "input/a", "input", MANIFEST] {
            let (root, vault, _state) = traced_vault("replaced-owned-entry");
            let frozen = freeze(candidate_with(&vault, "a/part.stl", b"mesh"), b"manifest");
            let phase = root
                .join(VAULT)
                .join(frozen.capture_id().as_str())
                .join(FROZEN);
            // Same name and contents, different object: only the recorded
            // identity tells them apart.
            let original = phase.join(replaced);
            let moved = root.join("moved-original");
            fs::rename(&original, &moved).unwrap();
            if replaced == MANIFEST {
                fs::write(&original, b"manifest").unwrap();
            } else {
                let file = phase.join("input/a/part.stl");
                fs::create_dir_all(file.parent().unwrap()).unwrap();
                fs::write(&file, b"mesh").unwrap();
            }
            assert!(
                matches!(frozen.remove(&paths), Err(Error::CorruptSnapshot)),
                "{replaced}"
            );
            // A replaced input stops removal before the deleting rename; a
            // replacement below it stops removal before any unlink.
            let phase = if replaced == "input" {
                phase
            } else {
                phase.with_file_name(DELETING)
            };
            assert_eq!(fs::read(phase.join("input/a/part.stl")).unwrap(), b"mesh");
            assert_eq!(fs::read(phase.join(MANIFEST)).unwrap(), b"manifest");
            assert!(moved.exists());
        }
    }

    #[test]
    fn retained_capture_post_rename_error_returns_frozen_ownership() {
        let (_root, vault, state) = traced_vault("post-rename-ownership");
        let candidate = candidate_with(&vault, "input/part.stl", b"part");
        state.lock().unwrap().events.clear();
        state.lock().unwrap().fail_at = Some(6);
        let FreezeResult::FrozenWithError { capture, .. } = candidate.freeze_owned(b"manifest")
        else {
            panic!("post-rename sync failure did not transfer frozen ownership")
        };
        assert_eq!(capture.receipt().phase(), RetainedCapturePhase::Frozen);
        assert_eq!(
            vault.scan().unwrap()[0].phase(),
            RetainedCapturePhase::Frozen
        );
    }

    #[test]
    fn retained_partial_allocation_failures_remove_generated_slot() {
        use RetainedCapturePhase::{Candidate, Deleting, Malformed};
        for (fail_at, second, phase) in (0..=5).map(|index| (index, None, None)).chain([
            (5, Some(6), Some(Candidate)),
            (5, Some(7), Some(Deleting)),
            (5, Some(8), Some(Deleting)),
            (5, Some(9), Some(Malformed)),
            (5, Some(10), None),
            (1, Some(2), Some(Malformed)),
        ]) {
            let (_root, vault, state) = traced_vault("allocation-cleanup");
            *state.lock().unwrap() = TraceState {
                fail_at: Some(fail_at),
                second_fail_at: second,
                ..TraceState::default()
            };
            let error = vault.allocate_owned().err().unwrap();
            assert_eq!(error.retained().map(RetainedCaptureReceipt::phase), phase);
            let entries = vault.scan().unwrap();
            assert_eq!(entries.len(), usize::from(phase.is_some()));
            if let Some(receipt) = error.retained() {
                assert_eq!(entries[0].capture_id(), receipt.capture_id());
                assert_eq!(entries[0].phase(), receipt.phase());
            }
        }
    }

    #[test]
    fn retained_cleanup_empty_slot_remains_repair_evidence() {
        let (_root, vault, state) = traced_vault("empty-slot");
        let candidate = candidate_with(&vault, "part.stl", b"mesh");
        let frozen = freeze(candidate, b"manifest");
        {
            let mut state = state.lock().unwrap();
            state.events.clear();
            state.fail_at = Some(3);
        }
        assert!(matches!(
            frozen.remove(&[SourcePath::try_from("part.stl".to_owned()).unwrap()]),
            Err(Error::Io(_))
        ));
        let entries = vault.scan().unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].phase(), RetainedCapturePhase::Malformed);
        assert!(matches!(
            vault.finish_deleting(),
            Err(Error::CorruptSnapshot)
        ));
        assert_eq!(vault.scan().unwrap(), entries);
    }

    #[test]
    fn retained_frozen_cleanup_preserves_sibling_created_at_final_unlink() {
        let (root, vault, state) = traced_vault("frozen-final-unlink");
        let candidate = candidate_with(&vault, "part.stl", b"mesh");
        let id = candidate.capture_id().as_str().to_owned();
        let frozen = freeze(candidate, b"manifest");
        state.lock().unwrap().add_slot_sibling_at_unlink = true;

        assert!(matches!(
            frozen.remove(&[SourcePath::try_from("part.stl".to_owned()).unwrap()]),
            Err(Error::Io(_))
        ));
        let slot = root.join(VAULT).join(id);
        assert_eq!(fs::read(slot.join("late-evidence")).unwrap(), b"preserve");
        assert!(!slot.join(DELETING).exists());
    }

    #[test]
    fn retained_deleting_restart_preserves_sibling_created_at_final_unlink() {
        let (root, vault, state) = traced_vault("deleting-final-unlink");
        let candidate = candidate_with(&vault, "part.stl", b"mesh");
        let id = candidate.capture_id().as_str().to_owned();
        let frozen = freeze(candidate, b"manifest");
        let slot = root.join(VAULT).join(&id);
        fs::remove_file(slot.join("frozen/input/part.stl")).unwrap();
        fs::remove_dir(slot.join("frozen/input")).unwrap();
        fs::remove_file(slot.join("frozen/capture-manifest.json")).unwrap();
        fs::rename(slot.join(FROZEN), slot.join(DELETING)).unwrap();
        drop(frozen);
        state.lock().unwrap().add_slot_sibling_at_unlink = true;

        assert!(matches!(vault.finish_deleting(), Err(Error::Io(_))));
        assert_eq!(fs::read(slot.join("late-evidence")).unwrap(), b"preserve");
        assert!(!slot.join(DELETING).exists());
    }

    #[test]
    fn retained_deleting_restart_preserves_replaced_directory_identity() {
        let (root, vault, state) = traced_vault("deleting-replaced-identity");
        state.lock().unwrap().replace_deleting_at_unlink = true;
        let id = RetainedCaptureId::parse("33".repeat(32)).unwrap();
        let slot = vault.parent.create_child_exclusive(id.as_str()).unwrap();
        slot.create_child_exclusive(DELETING).unwrap();
        slot.sync().unwrap();

        assert!(matches!(
            vault.finish_deleting(),
            Err(Error::CorruptSnapshot)
        ));
        assert!(root.join(VAULT).join(id.as_str()).join(DELETING).is_dir());
        assert!(
            root.join(VAULT)
                .join(id.as_str())
                .join("original-deleting")
                .is_dir()
        );
    }

    fn replace_slot_on_cleanup(
        root: &Path,
        id: &RetainedCaptureId,
        state: &Mutex<TraceState>,
    ) -> PathBuf {
        let path = root.join(VAULT).join(id.as_str());
        state.lock().unwrap().replace_slot_at_unlink = Some(path.clone());
        path
    }

    fn assert_replacement_slot_survives(path: &Path) {
        let replacement = pp_os::status_follow(path).expect("replacement slot was deleted");
        let original = pp_os::status_follow(&path.with_extension("original")).unwrap();
        assert!(replacement.is_dir());
        assert!(original.is_dir());
        assert_ne!(replacement.identity(), original.identity());
        assert_eq!(fs::read_dir(path).unwrap().count(), 0);
        assert_eq!(
            fs::read_dir(path.with_extension("original"))
                .unwrap()
                .count(),
            0
        );
    }

    #[test]
    fn retained_candidate_abort_preserves_replaced_slot_identity() {
        let (root, vault, state) = traced_vault("abort-replaced-slot");
        let candidate = candidate_with(&vault, "part.stl", b"mesh");
        let path = replace_slot_on_cleanup(&root, candidate.capture_id(), &state);
        let result = candidate.abort();
        assert_replacement_slot_survives(&path);
        assert!(matches!(result.unwrap_err().source, Error::CorruptSnapshot));
    }

    #[test]
    fn retained_frozen_remove_preserves_replaced_slot_identity() {
        let (root, vault, state) = traced_vault("frozen-replaced-slot");
        let frozen = freeze(candidate_with(&vault, "part.stl", b"mesh"), b"manifest");
        let path = replace_slot_on_cleanup(&root, frozen.capture_id(), &state);
        let result = frozen.remove(&[SourcePath::try_from("part.stl".to_owned()).unwrap()]);
        assert_replacement_slot_survives(&path);
        assert!(matches!(result, Err(Error::CorruptSnapshot)));
    }

    #[test]
    fn retained_deleting_restart_preserves_replaced_slot_identity() {
        let (root, vault, state) = traced_vault("restart-replaced-slot");
        let id = RetainedCaptureId::parse("44".repeat(32)).unwrap();
        let slot = vault.parent.create_child_exclusive(id.as_str()).unwrap();
        slot.create_child_exclusive(DELETING).unwrap();
        let path = replace_slot_on_cleanup(&root, &id, &state);
        let result = vault.finish_deleting();
        assert_replacement_slot_survives(&path);
        assert!(matches!(result, Err(Error::CorruptSnapshot)));
    }

    #[test]
    fn retained_allocation_cleanup_preserves_replaced_slot_identity() {
        let (root, vault, state) = traced_vault("allocation-replaced-slot");
        let id = RetainedCaptureId::parse("55".repeat(32)).unwrap();
        let path = replace_slot_on_cleanup(&root, &id, &state);
        state.lock().unwrap().fail_at = Some(5);
        let result = vault.allocate_owned_with(|| Ok(id.clone()));
        assert_replacement_slot_survives(&path);
        let error = result.err().unwrap();
        assert!(matches!(error.source, Error::Io(_)));
        assert_eq!(error.retained().unwrap().capture_id(), &id);
        assert_eq!(
            error.retained().unwrap().phase(),
            RetainedCapturePhase::Malformed
        );
    }
}
