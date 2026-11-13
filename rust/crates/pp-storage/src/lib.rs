#[cfg(feature = "desktop_rebuild")]
pub mod production;

#[cfg(feature = "desktop_rebuild")]
pub mod admin;
#[cfg(feature = "desktop_rebuild")]
pub mod app_settings;
pub mod filaments;
#[cfg(feature = "desktop_rebuild")]
pub mod import;

pub mod archive;
#[cfg(feature = "desktop_rebuild")]
mod auth_scheduler;
#[cfg(feature = "desktop_rebuild")]
use auth_scheduler::*;
#[cfg(feature = "desktop_rebuild")]
pub mod accepted_artifacts;
pub mod auth;
mod backup_retention;
#[cfg(feature = "desktop_rebuild")]
pub mod build_graph;
pub mod catalog;
#[cfg(feature = "desktop_rebuild")]
pub mod checkoff_progress;
#[cfg(feature = "desktop_rebuild")]
pub mod domain_events;
#[cfg(feature = "desktop_rebuild")]
pub mod features;
pub mod jobs;
pub mod lease;
#[cfg(feature = "desktop_rebuild")]
pub mod manifest_options;
#[cfg(feature = "desktop_rebuild")]
pub mod manifest_text;
#[cfg(feature = "desktop_rebuild")]
pub mod native_secrets;
#[cfg(feature = "desktop_rebuild")]
pub mod order_authority;
pub mod ownership_marker;
#[cfg(feature = "desktop_rebuild")]
pub mod plan_publication;
#[cfg(feature = "desktop_rebuild")]
pub mod plates;
#[cfg(feature = "desktop_rebuild")]
pub use pp_domain::printer_display_state;
#[cfg(feature = "desktop_rebuild")]
pub use pp_domain::slicer_grams;
mod error_macros;
#[cfg(feature = "desktop_rebuild")]
pub mod print_grams;
#[cfg(feature = "desktop_rebuild")]
pub mod printer_hosts;
#[cfg(feature = "desktop_rebuild")]
pub mod printer_setup;
#[cfg(feature = "desktop_rebuild")]
pub mod profiles;
#[cfg(feature = "desktop_rebuild")]
mod read;
pub mod read_model;
#[cfg(feature = "desktop_rebuild")]
pub mod required_units;
#[cfg(feature = "desktop_rebuild")]
pub mod restore;
mod schema;
#[cfg(feature = "desktop_rebuild")]
pub mod send_queue;
#[cfg(feature = "desktop_rebuild")]
pub mod source_files;
#[cfg(feature = "desktop_rebuild")]
pub mod source_scan;
#[cfg(feature = "desktop_rebuild")]
pub mod source_sync;
pub mod sqlite_uri;
#[cfg(feature = "desktop_rebuild")]
pub mod tls_pins;
#[cfg(feature = "desktop_rebuild")]
pub mod uploads;
pub mod versions;
#[cfg(feature = "desktop_rebuild")]
pub mod working_drafts;
#[cfg(feature = "desktop_rebuild")]
mod write;
mod writer_admission;
pub use writer_admission::WriterFailure;

#[cfg(feature = "desktop_rebuild")]
use restore::recover as recover_restore;

#[cfg(not(feature = "desktop_rebuild"))]
fn recover_restore(_directory: &std::path::Path) -> anyhow::Result<()> {
    Ok(())
}

use anyhow::{Context, Result, anyhow, ensure};
use rusqlite::{Connection, OpenFlags, OptionalExtension, params};
pub use schema::SchemaReady;
use serde::{Deserialize, Serialize};
use std::{
    collections::VecDeque,
    path::{Path, PathBuf},
    sync::{
        Arc, Condvar, Mutex,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};
use writer_admission::AdmissionError;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum SettingSnapshot {
    Missing,
    Stored { value: String },
}
#[derive(Clone, Debug)]
pub struct Setting {
    pub tenant: String,
    pub key: String,
    pub value: String,
}
#[derive(Clone, Debug)]
pub enum SettingCommand {
    Set(Setting),
    CompareAndSet {
        setting: Setting,
        expected: SettingSnapshot,
    },
}
#[derive(Clone, Copy)]
pub struct Limits {
    pub queued_writes: usize,
    pub readers: usize,
}
impl Default for Limits {
    fn default() -> Self {
        Self {
            queued_writes: 32,
            readers: 4,
        }
    }
}
pub type WriteReply = mpsc::Receiver<Result<bool>>;
const READER_DRAIN: Duration = Duration::from_secs(5);
enum Work {
    Setting(SettingCommand),
    Backup(PathBuf),
    Archive(Box<archive::Job>),
    #[cfg(feature = "desktop_rebuild")]
    TlsPin(tls_pins::Command),
}
/// One queued `Shared::write`. It opens its own transaction and sends its
/// own typed reply, so a storage family needs no envelope of its own.
type WriteJob = Box<dyn FnOnce(&mut Connection, writer_admission::Deadline) + Send>;
impl From<WriteJob> for Envelope {
    fn from(job: WriteJob) -> Self {
        Self::Write(job)
    }
}
enum WriterControl {
    PageCount,
    AdmitDiskQuota(u32),
}
enum Envelope {
    #[cfg(feature = "desktop_rebuild")]
    WorkingDraft {
        command: working_drafts::Command,
        reply: mpsc::Sender<Result<working_drafts::Outcome>>,
    },
    #[cfg(feature = "desktop_rebuild")]
    WorkingDraftWrite {
        command: working_drafts::Write,
    },
    #[cfg(feature = "desktop_rebuild")]
    BuildGraph {
        command: build_graph::Command,
        reply: build_graph::Reply,
    },
    #[cfg(feature = "desktop_rebuild")]
    NativeSecrets {
        command: native_secrets::Command,
        reply: native_secrets::Reply,
    },
    #[cfg(feature = "desktop_rebuild")]
    Features {
        command: features::Command,
        reply: mpsc::Sender<Result<features::Reply>>,
    },
    Filaments(filaments::Message),
    #[cfg(feature = "desktop_rebuild")]
    AppSettings {
        command: app_settings::Command,
        reply: app_settings::Reply,
    },
    #[cfg(feature = "desktop_rebuild")]
    Publication {
        command: plan_publication::Command,
        reply: mpsc::Sender<Result<plan_publication::Outcome>>,
    },
    #[cfg(feature = "desktop_rebuild")]
    RequiredUnits {
        command: required_units::Command,
        reply: mpsc::Sender<Result<required_units::Outcome>>,
    },
    #[cfg(feature = "desktop_rebuild")]
    Uploads {
        command: uploads::Command,
        reply: mpsc::Sender<Result<uploads::UploadsOutcome>>,
    },
    #[cfg(feature = "desktop_rebuild")]
    Checkoff {
        command: checkoff_progress::Command,
        reply: mpsc::Sender<Result<checkoff_progress::Response>>,
    },
    #[cfg(feature = "desktop_rebuild")]
    Plates {
        command: plates::Command,
        reply: mpsc::Sender<Result<plates::Response>>,
    },
    #[cfg(feature = "desktop_rebuild")]
    Workflow {
        command: read_model::build_workflow::Command,
        reply: mpsc::Sender<Result<read_model::build_workflow::Read>>,
    },
    #[cfg(feature = "desktop_rebuild")]
    SendQueue {
        command: send_queue::writes::Command,
        reply: mpsc::Sender<Result<send_queue::writes::Reply>>,
    },
    #[cfg(feature = "desktop_rebuild")]
    PrintGrams {
        command: print_grams::Command,
        reply: mpsc::Sender<Result<Option<print_grams::Measurement>>>,
    },
    #[cfg(feature = "desktop_rebuild")]
    Farm {
        command: read_model::farm::Command,
        reply: mpsc::Sender<Result<read_model::farm::Farm>>,
    },
    #[cfg(feature = "desktop_rebuild")]
    History {
        command: read_model::history::Command,
        reply: mpsc::Sender<Result<read_model::history::Outcome>>,
    },
    #[cfg(feature = "desktop_rebuild")]
    DomainEvents {
        command: domain_events::Command,
        reply: mpsc::Sender<Result<domain_events::Read>>,
    },
    #[cfg(feature = "desktop_rebuild")]
    Planning {
        command: read_model::planning::Command,
        reply: mpsc::Sender<Result<read_model::planning::Facts>>,
    },
    Read {
        command: read_model::Command,
        reply: mpsc::Sender<Result<read_model::Batch>>,
    },
    Catalog {
        command: catalog::Command,
        reply: mpsc::Sender<Result<catalog::Reply>>,
    },
    CatalogRequest {
        command: catalog::Command,
        reply: mpsc::Sender<Result<catalog::Outcome>>,
    },
    Jobs {
        command: jobs::Command,
        reply: mpsc::Sender<Result<jobs::Outcome>>,
    },
    Setting {
        work: Work,
        reply: mpsc::Sender<Result<bool>>,
    },
    Write(WriteJob),
    Auth {
        command: auth::Command,
        policy: auth::AuthPolicy,
        reply: mpsc::Sender<Result<auth::Reply>>,
    },
    WriterControl {
        control: WriterControl,
        reply: mpsc::Sender<Result<u32>>,
    },
}
#[cfg(feature = "desktop_rebuild")]
fn stop_poisoned_queue(
    error: std::sync::PoisonError<std::sync::MutexGuard<'_, Queue>>,
) -> std::sync::MutexGuard<'_, Queue> {
    let mut queue = error.into_inner();
    queue.closed = true;
    queue.pending.clear();
    for work in queue.auth_ready.drain(..) {
        auth::rebuild::finish_stopped(work);
    }
    queue
}

struct Queue {
    #[cfg(feature = "desktop_rebuild")]
    auth_ready: VecDeque<auth::rebuild::WriterWork>,
    #[cfg(feature = "desktop_rebuild")]
    next: QueueClass,
    closed: bool,
    pending: VecDeque<Envelope>,
}
struct Shared {
    queue: Mutex<Queue>,
    changed: Condvar,
    capacity: usize,
    job_admission: Mutex<Option<Arc<jobs::WorkerAdmission>>>,
    #[cfg(feature = "desktop_rebuild")]
    orphaned_source_leases: Mutex<Vec<u64>>,
    #[cfg(feature = "desktop_rebuild")]
    job_subscriptions: jobs::JobSubscriptions,
    #[cfg(feature = "desktop_rebuild")]
    import_epoch: std::sync::atomic::AtomicU64,
    #[cfg(feature = "desktop_rebuild")]
    import_quota: Mutex<Option<u64>>,
    #[cfg(feature = "desktop_rebuild")]
    domain_events: domain_events::Feed,
    /// Print-file snapshots, one store per open database as Node keeps one
    /// per repository.
    #[cfg(feature = "desktop_rebuild")]
    print_file_snapshots: Arc<checkoff_progress::print_file::SnapshotStore>,
}
#[derive(Clone)]
pub struct SettingsClient {
    shared: Arc<Shared>,
    readers: Arc<ReaderPool>,
}
struct ReaderState {
    closed: bool,
    idle: Vec<Connection>,
    active: usize,
}
struct ReaderPool {
    state: Mutex<ReaderState>,
    changed: Condvar,
}
pub struct ReaderCheckout {
    connection: Option<Connection>,
    pool: Arc<ReaderPool>,
}
/// Why no reader was checked out. Converting it into `anyhow::Error` gives
/// the messages `reader` has always reported.
enum ReaderFailure {
    Stopped,
    /// Every reader stayed checked out for the caller's whole wait.
    Full,
    Poisoned,
    /// The connection could not be made read-only.
    Unusable(anyhow::Error),
}
impl From<ReaderFailure> for anyhow::Error {
    fn from(failure: ReaderFailure) -> Self {
        match failure {
            ReaderFailure::Stopped => anyhow!("Storage stopped"),
            ReaderFailure::Full => anyhow!("Reader pool full"),
            ReaderFailure::Poisoned => anyhow!("Reader pool poisoned"),
            ReaderFailure::Unusable(error) => error,
        }
    }
}
pub struct WriterOwner {
    client: SettingsClient,
    join: Option<JoinHandle<()>>,
    lease: Option<lease::StorageLease>,
    #[cfg(feature = "desktop_rebuild")]
    restore: Arc<restore::Activity>,
    /// How long shutdown waits, in total, for reader checkouts and restore
    /// work after it closes admission.
    drain: Duration,
}

fn snapshot(connection: &Connection, tenant: &str, key: &str) -> Result<SettingSnapshot> {
    let value: Option<String> = connection
        .query_row(
            "SELECT value FROM app_settings WHERE tenant_id=?1 AND key=?2",
            params![tenant, key],
            |r| r.get(0),
        )
        .optional()?;
    Ok(match value {
        None => SettingSnapshot::Missing,
        Some(value) => SettingSnapshot::Stored { value },
    })
}
fn writer_control(connection: &Connection, control: WriterControl) -> Result<u32> {
    match control {
        WriterControl::PageCount => {
            let pages: i64 = connection.query_row("PRAGMA page_count", [], |row| row.get(0))?;
            u32::try_from(pages).map_err(|_| anyhow!("Database page count exceeds u32"))
        }
        WriterControl::AdmitDiskQuota(max_pages) => {
            // SQLite clamps max_page_count: a value below the current page
            // count becomes that count, and a value above 0xfffffffe becomes
            // 0xfffffffe. Reject both before the PRAGMA so a refusal leaves
            // the previous limit unchanged.
            const SQLITE_MAX_PAGE_COUNT: u32 = 0xffff_fffe;
            ensure!(max_pages > 0, "Disk quota must be positive");
            ensure!(
                max_pages <= SQLITE_MAX_PAGE_COUNT,
                "Disk quota {max_pages} exceeds the SQLite page limit"
            );
            let pages: i64 = connection.query_row("PRAGMA page_count", [], |row| row.get(0))?;
            ensure!(
                i64::from(max_pages) >= pages,
                "Disk quota {max_pages} is below the current database size"
            );
            connection.pragma_update(None, "max_page_count", i64::from(max_pages))?;
            let applied: i64 =
                connection.query_row("PRAGMA max_page_count", [], |row| row.get(0))?;
            ensure!(
                applied == i64::from(max_pages),
                "Disk quota {max_pages} is below the current database size"
            );
            Ok(max_pages)
        }
    }
}
fn execute(
    connection: &mut Connection,
    work: Work,
    admission: writer_admission::Deadline,
) -> Result<bool> {
    let command = match work {
        Work::Setting(command) => command,
        Work::Backup(path) => {
            schema::backup(connection, &path, false)?;
            return Ok(true);
        }
        Work::Archive(job) => {
            // Holding the write lock freezes every SQLite writer, in this
            // process or another, until the last root entry is written.
            let tx = writer_admission::begin_immediate(connection, admission)?;
            archive::write(&tx, &job)?;
            tx.rollback()?;
            return Ok(true);
        }
        #[cfg(feature = "desktop_rebuild")]
        Work::TlsPin(command) => {
            let tx = writer_admission::begin_immediate(connection, admission)?;
            let changed = tls_pins::execute(&tx, command)?;
            tx.commit()?;
            return Ok(changed);
        }
    };
    #[cfg(test)]
    tests::observe_setting(|attempts| attempts.executions += 1);
    let tx = writer_admission::begin_immediate(connection, admission)?;
    let setting = match command {
        SettingCommand::Set(setting) => setting,
        SettingCommand::CompareAndSet { setting, expected } => {
            if snapshot(&tx, &setting.tenant, &setting.key)? != expected {
                #[cfg(test)]
                tests::observe_setting(|attempts| attempts.commits += 1);
                tx.commit()?;
                return Ok(false);
            }
            setting
        }
    };
    #[cfg(test)]
    tests::observe_setting(|attempts| attempts.inserts += 1);
    tx.execute("INSERT INTO app_settings(tenant_id,key,value) VALUES(?1,?2,?3) ON CONFLICT(tenant_id,key) DO UPDATE SET value=excluded.value",params![setting.tenant,setting.key,setting.value])?;
    #[cfg(test)]
    tests::observe_setting(|attempts| attempts.commits += 1);
    tx.commit()?;
    Ok(true)
}
impl Shared {
    /// Waits up to `wait` for room in the writer queue, then queues `envelope`.
    fn admit(
        &self,
        envelope: Envelope,
        cancelled: &AtomicBool,
        wait: Duration,
    ) -> Result<(), AdmissionError> {
        let deadline = Instant::now() + wait;
        let mut queue = self.queue.lock().map_err(|_| AdmissionError::Poisoned)?;
        loop {
            if queue.closed {
                return Err(AdmissionError::Stopped);
            }
            if cancelled.load(Ordering::Acquire) {
                return Err(AdmissionError::Cancelled);
            }
            if queue.pending.len() < self.capacity {
                queue.pending.push_back(envelope);
                self.changed.notify_all();
                return Ok(());
            }
            if Instant::now() >= deadline {
                return Err(AdmissionError::QueueFull);
            }
            queue = self
                .changed
                .wait_timeout(queue, Duration::from_millis(5))
                .map_err(|_| AdmissionError::Poisoned)?
                .0;
        }
    }
}
impl SettingsClient {
    #[cfg(feature = "desktop_rebuild")]
    pub(crate) fn enqueue_auth(&self, work: auth::rebuild::WriterWork) {
        let mut queue = self.shared.queue.lock().unwrap_or_else(stop_poisoned_queue);
        if queue.closed {
            self.shared.changed.notify_all();
            drop(queue);
            auth::rebuild::finish_stopped(work);
            return;
        }
        queue.auth_ready.push_back(work);
        self.shared.changed.notify_all();
    }

    fn enqueue(&self, work: Work, cancelled: &AtomicBool, wait: Duration) -> Result<WriteReply> {
        let (reply, receiver) = mpsc::channel();
        self.shared
            .admit(Envelope::Setting { work, reply }, cancelled, wait)?;
        Ok(receiver)
    }
    pub fn submit(
        &self,
        command: SettingCommand,
        cancelled: &AtomicBool,
        wait: Duration,
    ) -> Result<WriteReply> {
        let (SettingCommand::Set(setting) | SettingCommand::CompareAndSet { setting, .. }) =
            &command;
        ensure!(
            !auth::OWNED_SETTINGS.contains(&setting.key.as_str()),
            "Setting {} is owned by auth and changes only through auth requests",
            setting.key
        );
        self.enqueue(Work::Setting(command), cancelled, wait)
    }
    /// The print-file snapshot store the files/content route captures into
    /// and the assignment route redeems from.
    #[cfg(feature = "desktop_rebuild")]
    pub fn print_file_snapshots(&self) -> Arc<checkoff_progress::print_file::SnapshotStore> {
        Arc::clone(&self.shared.print_file_snapshots)
    }
    pub fn reader(&self, wait: Duration) -> Result<ReaderCheckout> {
        Ok(self.checkout(wait)?)
    }
    /// [`SettingsClient::reader`] with the refusal typed.
    fn checkout(&self, wait: Duration) -> Result<ReaderCheckout, ReaderFailure> {
        let deadline = Instant::now() + wait;
        let poisoned = |_| ReaderFailure::Poisoned;
        let mut state = self.readers.state.lock().map_err(poisoned)?;
        loop {
            if state.closed {
                return Err(ReaderFailure::Stopped);
            }
            if let Some(connection) = state.idle.pop() {
                if let Err(error) = schema::configure(&connection, true) {
                    state.idle.push(connection);
                    return Err(ReaderFailure::Unusable(error));
                }
                state.active += 1;
                return Ok(ReaderCheckout {
                    connection: Some(connection),
                    pool: self.readers.clone(),
                });
            }
            if Instant::now() >= deadline {
                return Err(ReaderFailure::Full);
            }
            let left = deadline.saturating_duration_since(Instant::now());
            let waited = self.readers.changed.wait_timeout(state, left);
            state = waited.map_err(|_| ReaderFailure::Poisoned)?.0;
        }
    }
}
impl ReaderCheckout {
    pub(crate) fn connection(&self) -> &Connection {
        self.connection.as_ref().expect("checked out connection")
    }
    pub fn snapshot(&self, tenant: &str, key: &str) -> Result<SettingSnapshot> {
        snapshot(self.connection(), tenant, key)
    }
    pub fn get_setting(
        &self,
        tenant: &str,
        key: &str,
        default: Option<&str>,
    ) -> Result<Option<String>> {
        Ok(match self.snapshot(tenant, key)? {
            SettingSnapshot::Stored { value } if !value.is_empty() => Some(value),
            _ => default.map(str::to_owned),
        })
    }
}
impl Drop for ReaderCheckout {
    fn drop(&mut self) {
        let connection = self.connection.take().expect("checked out connection");
        let mut state = self.pool.state.lock().expect("Reader pool poisoned");
        if state.closed {
            drop(connection);
        } else {
            state.idle.push(connection);
        }
        state.active -= 1;
        self.pool.changed.notify_all();
    }
}
impl WriterOwner {
    /// The owned data directory's `repos` directory, where accepted Source
    /// snapshots live.
    #[cfg(feature = "desktop_rebuild")]
    pub(crate) fn repos_dir(&self) -> Result<PathBuf> {
        Ok(self
            .lease
            .as_ref()
            .ok_or_else(|| anyhow!("Storage owner is closed"))?
            .data_dir()
            .join("repos"))
    }

    /// In a `desktop_rebuild` build, applies the ownership marker decision
    /// ([`ownership_marker`]) with the gate from `PP_DESKTOP_REBUILD` before any
    /// schema write.
    pub fn open(directory: &Path, limits: Limits) -> Result<(Self, SchemaReady)> {
        Self::open_with_effect_policy(
            directory,
            limits,
            jobs::EffectAdmissionPolicy::Legacy,
            ownership_marker::WriterGate::from_env(),
        )
    }

    /// [`WriterOwner::open`] with an explicit gate, for tests of the marker
    /// decision. Absent from release builds.
    #[cfg(any(test, feature = "test-unmarked-writer"))]
    pub fn open_with_gate(
        directory: &Path,
        limits: Limits,
        gate: ownership_marker::WriterGate,
    ) -> Result<(Self, SchemaReady)> {
        Self::open_with_effect_policy(directory, limits, jobs::EffectAdmissionPolicy::Legacy, gate)
    }

    /// Enforces feature choices for new Spoolman deductions on every worker.
    /// Call this constructor again on reopen to retain admission enforcement.
    #[cfg(feature = "desktop_rebuild")]
    pub fn open_with_job_feature_admission(
        directory: &Path,
        limits: Limits,
    ) -> Result<(Self, SchemaReady)> {
        Self::open_with_effect_policy(
            directory,
            limits,
            jobs::EffectAdmissionPolicy::EnforceFeatures,
            ownership_marker::WriterGate::from_env(),
        )
    }

    fn open_with_effect_policy(
        directory: &Path,
        limits: Limits,
        effect_policy: jobs::EffectAdmissionPolicy,
        gate: ownership_marker::WriterGate,
    ) -> Result<(Self, SchemaReady)> {
        let now = time::OffsetDateTime::now_utc();
        let timestamp = format!(
            "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}.{:03}Z",
            now.year(),
            u8::from(now.month()),
            now.day(),
            now.hour(),
            now.minute(),
            now.second(),
            now.millisecond()
        );
        Self::open_at_with_effect_policy(directory, limits, &timestamp, effect_policy, gate)
    }

    pub fn open_at(
        directory: &Path,
        limits: Limits,
        seed_timestamp: &str,
    ) -> Result<(Self, SchemaReady)> {
        Self::open_at_with_effect_policy(
            directory,
            limits,
            seed_timestamp,
            jobs::EffectAdmissionPolicy::Legacy,
            ownership_marker::WriterGate::from_env(),
        )
    }

    fn open_at_with_effect_policy(
        directory: &Path,
        limits: Limits,
        seed_timestamp: &str,
        effect_policy: jobs::EffectAdmissionPolicy,
        gate: ownership_marker::WriterGate,
    ) -> Result<(Self, SchemaReady)> {
        ensure!(
            limits.queued_writes > 0 && limits.readers > 0,
            "Storage limits must be positive"
        );
        let locked = lease::admit_directory(directory)?.lock()?;
        if let Err(error) = ownership_marker::admit_writer(&locked, gate) {
            locked.abandon();
            return Err(error);
        }
        if let Err(error) = locked.preflight() {
            locked.abandon();
            return Err(error);
        }
        let lease = lease::StorageLease::acquire_locked(locked)?;
        let path = lease.data_dir().join("print-partner.db");
        let version = match lease.preflight() {
            Ok(version) => version,
            Err(error) => {
                lease.abandon();
                return Err(error);
            }
        };
        // Recovery removes uploads and stages. Admit the live schema first so
        // an older runtime cannot discard a newer installation's recovery copy.
        if let Err(error) = recover_restore(lease.data_dir()) {
            lease.abandon();
            return Err(error);
        }
        // Preserve one literal per line so the covers suppression applies only to that literal.
        #[rustfmt::skip]
        let directories = [
            "repos",
            "sources",
            "exports",
            // ast-grep-ignore: no-writer-family-literal -- Creates the empty directory, not thumbnail files; Node renders accepted thumbnails.
            "thumbs",
            // ast-grep-ignore: no-writer-family-literal -- Creates the empty directory, not cover files; see GRE-675.
            "covers",
        ];
        for name in directories {
            std::fs::create_dir_all(lease.data_dir().join(name))
                .with_context(|| format!("WriterOwner::open: create storage directory {name}"))?;
        }
        let (mut connection, ready) = schema::initialize(&path, version, seed_timestamp)?;
        filaments::cleanup_temporary_files(lease.data_dir());
        jobs::recover(&mut connection)?;
        #[cfg(feature = "desktop_rebuild")]
        send_queue::recover(&mut connection)?;
        #[cfg(feature = "desktop_rebuild")]
        let event_feed = domain_events::Feed::open(&connection)?;
        let mut idle = Vec::new();
        for _ in 0..limits.readers {
            let reader = Connection::open_with_flags(
                &path,
                OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
            )?;
            schema::configure(&reader, true)?;
            idle.push(reader);
        }
        let readers = Arc::new(ReaderPool {
            state: Mutex::new(ReaderState {
                closed: false,
                idle,
                active: 0,
            }),
            changed: Condvar::new(),
        });
        let shared = Arc::new(Shared {
            queue: Mutex::new(Queue {
                #[cfg(feature = "desktop_rebuild")]
                auth_ready: VecDeque::new(),
                #[cfg(feature = "desktop_rebuild")]
                next: QueueClass::Normal,
                closed: false,
                pending: VecDeque::new(),
            }),
            changed: Condvar::new(),
            capacity: limits.queued_writes,
            #[cfg(feature = "desktop_rebuild")]
            domain_events: event_feed,
            #[cfg(feature = "desktop_rebuild")]
            print_file_snapshots: Arc::default(),
            job_admission: Mutex::new(None),
            #[cfg(feature = "desktop_rebuild")]
            orphaned_source_leases: Mutex::new(Vec::new()),
            #[cfg(feature = "desktop_rebuild")]
            job_subscriptions: jobs::JobSubscriptions::new(),
            #[cfg(feature = "desktop_rebuild")]
            import_epoch: std::sync::atomic::AtomicU64::new(0),
            #[cfg(feature = "desktop_rebuild")]
            import_quota: Mutex::new(None),
        });
        let worker = shared.clone();
        let mut catalog_state = catalog::State::new(lease.data_dir().to_owned());
        #[cfg(feature = "desktop_rebuild")]
        let mut publication_tokens = plan_publication::random_tokens();
        let join = thread::spawn(move || {
            loop {
                #[cfg(not(feature = "desktop_rebuild"))]
                let (envelope, admission) = {
                    let mut queue = worker.queue.lock().expect("Writer admission poisoned");
                    while queue.pending.is_empty() && !queue.closed {
                        queue = worker
                            .changed
                            .wait(queue)
                            .expect("Writer admission poisoned");
                    }
                    match queue.pending.pop_front() {
                        Some(envelope) => {
                            let admission = writer_admission::Deadline::at_dequeue();
                            worker.changed.notify_all();
                            (envelope, admission)
                        }
                        None => break,
                    }
                };
                #[cfg(feature = "desktop_rebuild")]
                let scheduled = {
                    let mut queue = worker.queue.lock().unwrap_or_else(stop_poisoned_queue);
                    loop {
                        if let Some(scheduled) = schedule(&mut queue, Instant::now()) {
                            worker.changed.notify_all();
                            break Some((scheduled, writer_admission::Deadline::at_dequeue()));
                        }
                        if queue.closed && queue.pending.is_empty() && queue.auth_ready.is_empty() {
                            break None;
                        }
                        if let Some(retry) = queue
                            .auth_ready
                            .iter()
                            .filter_map(|work| work.retry_at())
                            .min()
                        {
                            let wait = retry.saturating_duration_since(Instant::now());
                            queue = worker
                                .changed
                                .wait_timeout(queue, wait)
                                .map(|(queue, _)| queue)
                                .unwrap_or_else(|error| {
                                    stop_poisoned_queue(std::sync::PoisonError::new(
                                        error.into_inner().0,
                                    ))
                                });
                        } else {
                            queue = worker
                                .changed
                                .wait(queue)
                                .unwrap_or_else(stop_poisoned_queue);
                        }
                    }
                };
                #[cfg(feature = "desktop_rebuild")]
                let Some((scheduled, admission)) = scheduled else {
                    break;
                };
                #[cfg(feature = "desktop_rebuild")]
                let envelope = match scheduled {
                    Scheduled::Normal(envelope) => envelope,
                    Scheduled::Auth(work) => {
                        if let Some(retry) =
                            auth::rebuild::advance(&mut connection, work, admission)
                        {
                            let mut queue = worker.queue.lock().unwrap_or_else(stop_poisoned_queue);
                            queue.auth_ready.push_back(retry);
                            worker.changed.notify_all();
                        }
                        worker.domain_events.after_write(&connection, true);
                        continue;
                    }
                };
                #[cfg(feature = "desktop_rebuild")]
                catalog_state.reap(std::mem::take(
                    &mut *worker
                        .orphaned_source_leases
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner),
                ));
                #[cfg(feature = "desktop_rebuild")]
                let authority_changed = matches!(envelope, Envelope::Auth { .. });
                match envelope {
                    #[cfg(feature = "desktop_rebuild")]
                    Envelope::NativeSecrets { command, reply } => {
                        let _ = reply.send(native_secrets::execute(
                            &mut connection,
                            &worker,
                            command,
                            admission,
                        ));
                    }
                    #[cfg(feature = "desktop_rebuild")]
                    Envelope::Features { command, reply } => {
                        let _ = reply.send(features::execute(&mut connection, command, admission));
                    }
                    Envelope::Filaments(message) => {
                        filaments::dispatch(&mut connection, message, admission)
                    }
                    #[cfg(feature = "desktop_rebuild")]
                    Envelope::AppSettings { command, reply } => {
                        reply.send(app_settings::execute(&mut connection, command, admission));
                    }
                    #[cfg(feature = "desktop_rebuild")]
                    Envelope::BuildGraph { command, reply } => {
                        reply.send(build_graph::execute(&mut connection, command, admission));
                    }
                    #[cfg(feature = "desktop_rebuild")]
                    Envelope::Publication { command, reply } => {
                        let _ = reply.send(plan_publication::execute(
                            &mut connection,
                            command,
                            publication_tokens.as_mut(),
                            admission,
                        ));
                    }
                    #[cfg(feature = "desktop_rebuild")]
                    Envelope::RequiredUnits { command, reply } => {
                        let _ = reply.send(required_units::execute(
                            &mut connection,
                            command,
                            admission,
                        ));
                    }
                    #[cfg(feature = "desktop_rebuild")]
                    Envelope::Uploads { command, reply } => {
                        let changes_accounting = command.changes_accounting();
                        let changes_before = connection.total_changes();
                        let result =
                            uploads::execute(&mut connection, &catalog_state, command, admission);
                        if result.is_ok()
                            && changes_accounting
                            && connection.total_changes() != changes_before
                        {
                            worker.import_epoch.fetch_add(1, Ordering::AcqRel);
                        }
                        let _ = reply.send(result);
                    }
                    #[cfg(feature = "desktop_rebuild")]
                    Envelope::Checkoff { command, reply } => {
                        let _ = reply.send(checkoff_progress::execute(
                            &mut connection,
                            command,
                            admission,
                        ));
                    }
                    #[cfg(feature = "desktop_rebuild")]
                    Envelope::Plates { command, reply } => {
                        let _ = reply.send(plates::execute(&mut connection, command, admission));
                    }
                    #[cfg(feature = "desktop_rebuild")]
                    Envelope::WorkingDraft { command, reply } => {
                        let _ = reply.send(working_drafts::execute(&mut connection, command));
                    }
                    #[cfg(feature = "desktop_rebuild")]
                    Envelope::WorkingDraftWrite { command } => {
                        working_drafts::write(
                            &mut connection,
                            command,
                            admission,
                            publication_tokens.as_mut(),
                        );
                    }
                    #[cfg(feature = "desktop_rebuild")]
                    Envelope::Workflow { command, reply } => {
                        let _ = reply.send(read_model::build_workflow::execute(
                            &mut connection,
                            command,
                        ));
                    }
                    #[cfg(feature = "desktop_rebuild")]
                    Envelope::Farm { command, reply } => {
                        let _ = reply.send(read_model::farm::execute(&mut connection, command));
                    }
                    #[cfg(feature = "desktop_rebuild")]
                    Envelope::SendQueue { command, reply } => {
                        let _ = reply.send(send_queue::writes::execute(
                            &mut connection,
                            command,
                            admission,
                            &worker.job_subscriptions,
                        ));
                    }
                    #[cfg(feature = "desktop_rebuild")]
                    Envelope::PrintGrams { command, reply } => {
                        let _ =
                            reply.send(print_grams::execute(&mut connection, command, admission));
                    }
                    #[cfg(feature = "desktop_rebuild")]
                    Envelope::History { command, reply } => {
                        let _ = reply.send(read_model::history::execute(&mut connection, command));
                    }
                    #[cfg(feature = "desktop_rebuild")]
                    Envelope::DomainEvents { command, reply } => {
                        let _ = reply.send(domain_events::execute(
                            &mut connection,
                            &worker.domain_events,
                            command,
                        ));
                    }
                    #[cfg(feature = "desktop_rebuild")]
                    Envelope::Planning { command, reply } => {
                        let _ = reply.send(read_model::planning::execute(&mut connection, command));
                    }
                    Envelope::Read { command, reply } => {
                        let _ = reply.send(read_model::execute(&mut connection, command));
                    }
                    Envelope::Catalog { command, reply } => {
                        let _ = reply.send(catalog::execute(
                            &mut connection,
                            &mut catalog_state,
                            command,
                            admission,
                        ));
                    }
                    Envelope::CatalogRequest { command, reply } => {
                        let result = catalog::execute(
                            &mut connection,
                            &mut catalog_state,
                            command,
                            admission,
                        )
                        .and_then(|r| match r {
                            catalog::Reply::Outcome(o) => Ok(o),
                            _ => Err(anyhow!("Unexpected catalog reply")),
                        });
                        let _ = reply.send(result);
                    }
                    Envelope::Jobs { command, reply } => {
                        // A cancel can settle an import's reservation.
                        #[cfg(feature = "desktop_rebuild")]
                        let cancels = matches!(
                            &command,
                            jobs::Command::User {
                                operation: jobs::UserOperation::Cancel { .. },
                                ..
                            }
                        );
                        let result = jobs::execute(
                            &mut connection,
                            command,
                            effect_policy,
                            admission,
                            #[cfg(feature = "desktop_rebuild")]
                            &worker.job_subscriptions,
                            #[cfg(feature = "desktop_rebuild")]
                            &mut catalog_state,
                        );
                        #[cfg(feature = "desktop_rebuild")]
                        if cancels && result.is_ok() {
                            worker.import_epoch.fetch_add(1, Ordering::AcqRel);
                        }
                        let _ = reply.send(result);
                    }
                    Envelope::Setting { work, reply } => {
                        let _ = reply.send(execute(&mut connection, work, admission));
                    }
                    Envelope::Write(job) => job(&mut connection, admission),
                    Envelope::Auth {
                        command,
                        policy,
                        reply,
                    } => {
                        #[cfg(not(feature = "desktop_rebuild"))]
                        let result = auth::execute(&mut connection, command, policy, admission);
                        #[cfg(feature = "desktop_rebuild")]
                        let result = jobs::authority_change(
                            &mut connection,
                            &worker.job_subscriptions,
                            |connection| auth::execute(connection, command, policy, admission),
                        );
                        let _ = reply.send(result);
                    }
                    Envelope::WriterControl { control, reply } => {
                        let _ = reply.send(writer_control(&connection, control));
                    }
                }
                #[cfg(feature = "desktop_rebuild")]
                worker
                    .domain_events
                    .after_write(&connection, authority_changed);
            }
            drop(connection);
            #[cfg(feature = "desktop_rebuild")]
            worker.domain_events.wake();
        });
        Ok((
            Self {
                client: SettingsClient { shared, readers },
                join: Some(join),
                lease: Some(lease),
                #[cfg(feature = "desktop_rebuild")]
                restore: Arc::default(),
                drain: READER_DRAIN,
            },
            ready,
        ))
    }
    pub fn client(&self) -> SettingsClient {
        self.client.clone()
    }

    pub fn database_pages(&self) -> Result<u32> {
        self.writer_control(WriterControl::PageCount)
    }

    pub fn admit_disk_quota(&self, max_pages: u32) -> Result<u32> {
        self.writer_control(WriterControl::AdmitDiskQuota(max_pages))
    }

    fn writer_control(&self, control: WriterControl) -> Result<u32> {
        let deadline = Instant::now() + Duration::from_secs(5);
        let receiver = {
            let mut queue = self
                .client
                .shared
                .queue
                .lock()
                .map_err(|_| anyhow!("Writer admission poisoned"))?;
            loop {
                ensure!(!queue.closed, "Storage stopped");
                if queue.pending.len() < self.client.shared.capacity {
                    let (reply, receiver) = mpsc::channel();
                    queue
                        .pending
                        .push_back(Envelope::WriterControl { control, reply });
                    self.client.shared.changed.notify_all();
                    break receiver;
                }
                ensure!(Instant::now() < deadline, "Writer queue full");
                queue = self
                    .client
                    .shared
                    .changed
                    .wait_timeout(queue, Duration::from_millis(5))
                    .map_err(|_| anyhow!("Writer admission poisoned"))?
                    .0;
            }
        };
        receiver
            .recv()
            .map_err(|_| anyhow!("Writer stopped without disk quota result"))?
    }
    fn checked_destination(&self, destination: &Path) -> Result<(PathBuf, PathBuf)> {
        let lease = self
            .lease
            .as_ref()
            .ok_or_else(|| anyhow!("Storage stopped"))?;
        let parent = destination
            .parent()
            .ok_or_else(|| anyhow!("Backup parent missing"))?
            .canonicalize()?;
        let name = destination
            .file_name()
            .ok_or_else(|| anyhow!("Backup file name missing"))?;
        let managed_runtime = match lease.runtime_dir().canonicalize() {
            Ok(runtime) => parent.starts_with(runtime),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => false,
            Err(error) => return Err(error.into()),
        };
        ensure!(
            !parent.starts_with(lease.data_dir()) && !managed_runtime,
            "Backup destination is inside managed storage"
        );
        let destination = parent.join(name);
        ensure!(
            destination
                .symlink_metadata()
                .is_err_and(|error| error.kind() == std::io::ErrorKind::NotFound),
            "Backup destination must be a new file"
        );
        Ok((destination, lease.data_dir().to_owned()))
    }
    fn run_backup(&self, work: Work) -> Result<()> {
        let reply = self
            .client
            .enqueue(work, &AtomicBool::new(false), Duration::from_secs(5))?;
        reply
            .recv()
            .map_err(|_| anyhow!("Writer stopped without backup result"))??;
        Ok(())
    }
    pub fn backup(&self, destination: &Path) -> Result<()> {
        let (destination, _) = self.checked_destination(destination)?;
        self.run_backup(Work::Backup(destination))
    }
    /// Writes a validated app backup archive (Node's format 2) outside
    /// managed storage. The database snapshot and every included root are
    /// read while the writer holds the SQLite write lock.
    pub fn create_archive(
        &self,
        destination: &Path,
        request: archive::Request,
    ) -> Result<archive::Metadata> {
        let (destination, data_dir) = self.checked_destination(destination)?;
        let metadata = archive::Metadata::new(&request.app_version, request.scope);
        self.run_backup(Work::Archive(Box::new(archive::Job {
            destination,
            data_dir,
            metadata: metadata.clone(),
            request,
            free_space: archive::free_space,
        })))?;
        Ok(metadata)
    }
    fn close(&mut self) -> Result<()> {
        if self.join.is_none() && self.lease.is_none() {
            return Ok(());
        }
        #[cfg(feature = "desktop_rebuild")]
        self.client.shared.job_subscriptions.stop_owner();
        // Close restore, writer and reader admission before waiting on any
        // of them, then wait for all of them against one deadline. Restore
        // work runs in `.pp-restore`, which the next owner clears.
        #[cfg(feature = "desktop_rebuild")]
        self.restore.refuse_new();
        {
            let mut queue = self
                .client
                .shared
                .queue
                .lock()
                .map_err(|_| anyhow!("Writer admission poisoned"))?;
            queue.closed = true;
            self.client.shared.changed.notify_all();
        }
        {
            let mut readers = self
                .client
                .readers
                .state
                .lock()
                .map_err(|_| anyhow!("Reader pool poisoned"))?;
            readers.closed = true;
            readers.idle.clear();
            self.client.readers.changed.notify_all();
        }
        let deadline = Instant::now() + self.drain;
        #[cfg(all(test, unix, feature = "desktop_rebuild"))]
        if let Some(lease) = &self.lease {
            restore::admission_closed(lease.data_dir(), deadline);
        }
        let remaining_readers = {
            let mut readers = self
                .client
                .readers
                .state
                .lock()
                .map_err(|_| anyhow!("Reader pool poisoned"))?;
            while readers.active > 0 && Instant::now() < deadline {
                readers = self
                    .client
                    .readers
                    .changed
                    .wait_timeout(readers, deadline.saturating_duration_since(Instant::now()))
                    .map_err(|_| anyhow!("Reader pool poisoned"))?
                    .0;
            }
            readers.active
        };
        #[cfg(feature = "desktop_rebuild")]
        let restore_active = self.restore.wait_until(deadline);
        if let Some(join) = self.join.take() {
            join.join().map_err(|_| anyhow!("Writer join failed"))?;
        }
        ensure!(
            remaining_readers == 0,
            "Reader checkouts still active after {}s: {remaining_readers}; ownership retained until process exit",
            self.drain.as_secs()
        );
        #[cfg(feature = "desktop_rebuild")]
        ensure!(
            restore_active == 0,
            "Restore work still active after {}s: {restore_active}; ownership retained until process exit",
            self.drain.as_secs()
        );
        if let Some(lease) = self.lease.take() {
            let released = lease.release();
            ensure!(
                released.marker_removed && released.runtime_removed && released.lock_released,
                "Storage cleanup failed; ownership retained until explicit retry or process exit"
            );
        }
        Ok(())
    }
    /// The directory lease this owner holds until shutdown, for a Rust-only
    /// runtime's desktop state files next to the database.
    #[cfg(feature = "desktop_rebuild")]
    pub fn lease(&self) -> Option<&lease::StorageLease> {
        self.lease.as_ref()
    }
    /// Shortens the shutdown drain so tests need no real 5 s waits.
    #[cfg(all(test, unix, feature = "desktop_rebuild"))]
    pub(crate) fn set_drain(&mut self, drain: Duration) {
        self.drain = drain;
    }
    pub fn shutdown(mut self) -> Result<()> {
        let result = self.close();
        if result.is_err()
            && let Some(lease) = self.lease.take()
        {
            lease.retain_until_process_exit();
        }
        result
    }
}
impl Drop for WriterOwner {
    fn drop(&mut self) {
        if self.close().is_err()
            && let Some(lease) = self.lease.take()
        {
            lease.retain_until_process_exit();
        }
    }
}

#[cfg(test)]
mod tests;
