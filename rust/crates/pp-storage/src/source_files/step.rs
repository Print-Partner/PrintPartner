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
//!
//! Publication is fenced against shutdown. The worker writes the cache file
//! inside a reader checkout. `WriterOwner::close` refuses new checkouts and
//! waits for the active ones before it releases the data-folder lease, so no
//! worker publishes after shutdown starts, and a publish already running ends
//! before the next owner can start. After writing, the worker reads the file
//! back, so a tree the cache cannot return is a failure, never a loop.

use super::{
    SourceFilesClient, SourceFilesError,
    thumbnail::{read_cached, revision_file, store},
};
use crate::{SettingsClient, read_model::Credential};
use pp_domain::step_assembly::{self, PartNode, PartTree, StepError};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    collections::{HashMap, VecDeque},
    fs,
    io::{self, Read},
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
/// How long a publish waits for a reader checkout.
const PUBLISH_WAIT: Duration = Duration::from_secs(5);

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
    /// Fences each publish against owner shutdown.
    readers: SettingsClient,
    state: Mutex<State>,
    wake: Condvar,
    /// Set when the last client is dropped; also cancels a running read.
    stopping: AtomicBool,
    converted: AtomicU64,
    /// Test only: the worker waits on this before it publishes.
    #[cfg(test)]
    publish_gate: Mutex<Option<std::sync::mpsc::Receiver<()>>>,
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
    pub(super) fn new(data: PathBuf, readers: SettingsClient) -> Self {
        Self {
            shared: Arc::new(Shared {
                data,
                readers,
                state: Mutex::default(),
                wake: Condvar::new(),
                stopping: AtomicBool::new(false),
                converted: AtomicU64::new(0),
                #[cfg(test)]
                publish_gate: Mutex::default(),
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

    /// Test only: the worker's next publish waits until the sender sends or
    /// is dropped.
    #[cfg(test)]
    pub(super) fn hold_step_publish(&self) -> std::sync::mpsc::Sender<()> {
        let (send, receive) = std::sync::mpsc::channel();
        let gate = self.steps.shared.publish_gate.lock();
        *gate.unwrap_or_else(PoisonError::into_inner) = Some(receive);
        send
    }

    /// Test only: true when no STEP file is queued or converting.
    #[cfg(test)]
    pub(super) fn step_jobs_idle(&self) -> bool {
        let state = self.steps.shared.lock();
        let busy = |job: &Job| matches!(job, Job::Queued | Job::Converting);
        state.queue.is_empty() && !state.jobs.values().any(busy)
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
    let sha256 = hash(file, &shared.stopping).map_err(|_| StepFailure::Unreadable)?;
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
    #[cfg(test)]
    {
        let mut gate = shared.publish_gate.lock();
        let gate = gate
            .as_mut()
            .unwrap_or_else(|poisoned| poisoned.get_mut())
            .take();
        if let Some(gate) = gate {
            let _ = gate.recv();
        }
    }
    let name = cache_name(&sha256);
    let published = shared.readers.read(
        "source_files.step_publish",
        &shared.stopping,
        PUBLISH_WAIT,
        |_| store(&shared.data, STEP_FOLDER, &name, &bytes),
    );
    match published {
        Ok(Ok(_)) => {}
        // The owner is closing, or the client is gone: publish nothing.
        Err(_) => return Err(StepFailure::Unreadable),
        Ok(Err(error)) => {
            tracing::warn!(error = %error, "step_part_tree_cache_write_failed");
            return Err(StepFailure::CacheUnwritable);
        }
    }
    match read_tree(&shared.data, &sha256) {
        Some(_) => Ok(sha256),
        None => Err(StepFailure::CacheUnwritable),
    }
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

/// The file's SHA-256, checking `stopping` between 1 MiB reads.
fn hash(file: &Path, stopping: &AtomicBool) -> io::Result<String> {
    let mut hasher = Sha256::new();
    let mut opened = fs::File::open(file)?;
    let mut buffer = vec![0; 1 << 20];
    loop {
        if stopping.load(Ordering::Relaxed) {
            return Err(io::ErrorKind::Interrupted.into());
        }
        match opened.read(&mut buffer)? {
            0 => return Ok(hex::encode(hasher.finalize())),
            read => hasher.update(&buffer[..read]),
        }
    }
}
