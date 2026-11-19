//! STEP part trees for Library (GRE-1007).
//!
//! Opening a `.step` or `.stp` file of a Source's current revision queues it
//! for one background worker thread and answers at once with its state:
//! queued, converting, done (with the tree) or failed. The worker hashes the
//! file and reads its part tree with [`pp_domain::step_assembly`], which
//! rebuilds the full nesting from the STEP product structure (the workaround
//! for occt-import-js bug #60).
//!
//! The tree is cached as `thumbs/sources/step/<SHA-256 of the file>-v<format>.json`,
//! so the same bytes are read once whatever Source, revision or path holds
//! them: a later open, a copy elsewhere, or an open after a restart finds the
//! cached file and does not read the STEP again. Each answer comes from that
//! cached file. The cache sits inside the thumbnail cache, which a full
//! backup leaves out (`archive::SOURCE_THUMBNAIL_CACHE`) and whose pruning
//! touches only revision-id folders. A file that cannot be read is failed and
//! nothing is cached; the failure is kept until the next start.
//!
//! The worker starts on the first open and stops when the last clone of the
//! client is dropped; a conversion in progress then stops at its next check.

use super::{
    SourceFilesClient, SourceFilesError,
    thumbnail::{read_cached, revision_file, store},
};
use crate::read_model::Credential;
use pp_domain::step_assembly::{self, PartNode, PartTree, StepError};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    collections::{HashMap, VecDeque},
    fs, io,
    path::{Path, PathBuf},
    sync::{
        Arc, Condvar, Mutex, MutexGuard, OnceLock, PoisonError,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    time::Duration,
};

/// The cached file's format. A change makes every cached tree stale.
pub const PART_TREE_FORMAT: u32 = 1;
/// The folder under `thumbs/sources`; not a revision id, so pruning skips it.
const STEP_FOLDER: &str = "step";
const STEP_EXTENSIONS: &[&str] = &[".step", ".stp"];

/// What an open answers.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum StepConversion {
    Queued,
    Converting,
    Done { sha256: String, tree: PartTree },
    Failed(StepFailure),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StepFailure {
    /// The file does not start as an ISO 10303-21 file.
    NotStep,
    /// The file holds no product structure.
    NoProducts,
    /// Over the reader's part or depth cap.
    TooLarge,
    /// An assembly contains itself.
    Cycle,
    /// The file could not be read.
    Unreadable,
    /// The tree was read but could not be cached.
    CacheUnwritable,
}

/// One file of one revision. A revision never changes, so neither does the
/// file's hash once known.
type FileKey = (i64, String);

#[derive(Clone, Debug)]
enum Job {
    Queued,
    Converting,
    Done(String),
    Failed(StepFailure),
}

#[derive(Default)]
struct State {
    jobs: HashMap<FileKey, Job>,
    queue: VecDeque<(FileKey, PathBuf)>,
}

struct Shared {
    data: PathBuf,
    state: Mutex<State>,
    wake: Condvar,
    /// Set when the last client is dropped; also cancels a running read.
    stopping: AtomicBool,
    converted: AtomicU64,
}

impl Shared {
    fn lock(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

/// The queue and its worker, shared by every clone of one client.
pub(super) struct StepJobs {
    shared: Arc<Shared>,
    /// Whether the worker thread started; set on the first open.
    worker: OnceLock<bool>,
}

impl StepJobs {
    pub(super) fn new(data: PathBuf) -> Self {
        Self {
            shared: Arc::new(Shared {
                data,
                state: Mutex::default(),
                wake: Condvar::new(),
                stopping: AtomicBool::new(false),
                converted: AtomicU64::new(0),
            }),
            worker: OnceLock::new(),
        }
    }

    fn open(&self, key: FileKey, file: PathBuf) -> StepConversion {
        let mut state = self.shared.lock();
        match state.jobs.get(&key).cloned() {
            Some(Job::Queued) => return StepConversion::Queued,
            Some(Job::Converting) => return StepConversion::Converting,
            Some(Job::Failed(failure)) => return StepConversion::Failed(failure),
            Some(Job::Done(sha256)) => {
                drop(state);
                if let Some(tree) = read_tree(&self.shared.data, &sha256) {
                    return StepConversion::Done { sha256, tree };
                }
                // The cached file is gone or damaged: read the STEP again.
                state = self.shared.lock();
            }
            None => {}
        }
        if !self.started() {
            state.jobs.insert(key, Job::Failed(StepFailure::Unreadable));
            return StepConversion::Failed(StepFailure::Unreadable);
        }
        state.jobs.insert(key.clone(), Job::Queued);
        state.queue.push_back((key, file));
        drop(state);
        self.shared.wake.notify_one();
        StepConversion::Queued
    }

    /// Starts the worker on first use; false when the thread cannot start.
    fn started(&self) -> bool {
        *self.worker.get_or_init(|| {
            let shared = Arc::clone(&self.shared);
            let spawned = std::thread::Builder::new()
                .name("pp-step-part-trees".into())
                .spawn(move || work(&shared));
            if let Err(error) = &spawned {
                tracing::warn!(error = %error, "step_worker_start_failed");
            }
            spawned.is_ok()
        })
    }
}

impl Drop for StepJobs {
    fn drop(&mut self) {
        self.shared.stopping.store(true, Ordering::Relaxed);
        // Taking the lock orders the flag before a waiting worker's check.
        drop(self.shared.lock());
        self.shared.wake.notify_all();
    }
}

impl SourceFilesClient {
    /// Opens the STEP file at `path` in Source `source_id`'s current
    /// revision: its part tree when cached, otherwise its place in the queue.
    pub fn step_conversion(
        &self,
        credential: Credential,
        source_id: i64,
        path: &str,
        cancelled: &AtomicBool,
        wait: Duration,
    ) -> Result<StepConversion, SourceFilesError> {
        if !self.requestable(path, STEP_EXTENSIONS) {
            return Err(SourceFilesError::FileNotFound);
        }
        let name = "source_files.step_conversion";
        let Some((revision_id, root)) =
            self.snapshot(name, &credential, source_id, cancelled, wait)?
        else {
            return Err(SourceFilesError::FileNotFound);
        };
        let file = revision_file(&root, path)?;
        Ok(self.steps.open((revision_id, path.to_owned()), file))
    }

    /// How many STEP files this client's worker has read a part tree from.
    /// An open answered from the cache does not count.
    pub fn step_files_converted(&self) -> u64 {
        self.steps.shared.converted.load(Ordering::Relaxed)
    }
}

fn work(shared: &Shared) {
    loop {
        let (key, file) = {
            let mut state = shared.lock();
            loop {
                if shared.stopping.load(Ordering::Relaxed) {
                    return;
                }
                if let Some((key, file)) = state.queue.pop_front() {
                    state.jobs.insert(key.clone(), Job::Converting);
                    break (key, file);
                }
                state = shared
                    .wake
                    .wait(state)
                    .unwrap_or_else(PoisonError::into_inner);
            }
        };
        let job = match convert(shared, &file) {
            Ok(sha256) => Job::Done(sha256),
            Err(failure) => Job::Failed(failure),
        };
        shared.lock().jobs.insert(key, job);
    }
}

/// Hashes `file`, and reads and caches its tree unless that hash is cached.
fn convert(shared: &Shared, file: &Path) -> Result<String, StepFailure> {
    let sha256 = hash(file).map_err(|_| StepFailure::Unreadable)?;
    if read_tree(&shared.data, &sha256).is_some() {
        return Ok(sha256);
    }
    let opened = fs::File::open(file).map_err(|_| StepFailure::Unreadable)?;
    let tree = step_assembly::part_tree(opened, &shared.stopping).map_err(|error| match error {
        StepError::NotStep => StepFailure::NotStep,
        StepError::NoProducts => StepFailure::NoProducts,
        StepError::TooLarge => StepFailure::TooLarge,
        StepError::Cycle => StepFailure::Cycle,
        StepError::Io(_) | StepError::Cancelled => StepFailure::Unreadable,
    })?;
    shared.converted.fetch_add(1, Ordering::Relaxed);
    let cached = CachedTree {
        format: PART_TREE_FORMAT,
        sha256: sha256.clone(),
        roots: tree.roots,
    };
    let bytes = serde_json::to_vec(&cached).map_err(|_| StepFailure::CacheUnwritable)?;
    store(&shared.data, STEP_FOLDER, &cache_name(&sha256), &bytes).map_err(|error| {
        tracing::warn!(error = %error, "step_part_tree_cache_write_failed");
        StepFailure::CacheUnwritable
    })?;
    Ok(sha256)
}

/// The cached file: the part tree, nested, with what it was read from.
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct CachedTree {
    format: u32,
    sha256: String,
    roots: Vec<PartNode>,
}

fn cache_name(sha256: &str) -> String {
    format!("{sha256}-v{PART_TREE_FORMAT}.json")
}

/// The cached tree for `sha256`, or `None` when it is missing, unreadable, or
/// not a whole file of this format for that hash.
fn read_tree(data: &Path, sha256: &str) -> Option<PartTree> {
    let bytes = read_cached(data, STEP_FOLDER, &cache_name(sha256)).ok()?;
    let cached: CachedTree = serde_json::from_slice(&bytes).ok()?;
    (cached.format == PART_TREE_FORMAT && cached.sha256 == sha256).then_some(PartTree {
        roots: cached.roots,
    })
}

fn hash(file: &Path) -> io::Result<String> {
    let mut hasher = Sha256::new();
    io::copy(&mut fs::File::open(file)?, &mut hasher)?;
    Ok(hex::encode(hasher.finalize()))
}
