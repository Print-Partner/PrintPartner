use super::journal::Journal;
use super::*;
use crate::{READER_DRAIN, ReaderPool, Shared, WriterState, jobs};
use rusqlite::{Connection, OpenFlags};
use std::sync::{Arc, atomic::Ordering};

pub(crate) struct Context {
    pub snapshots: Arc<crate::checkoff_progress::print_file::SnapshotStore>,
    pub activity: Arc<Activity>,
    pub readers: Arc<ReaderPool>,
    pub directory: PathBuf,
    pub seed: String,
}

impl Context {
    #[cfg(test)]
    pub(crate) fn for_test() -> Self {
        Self {
            activity: Arc::default(),
            snapshots: Arc::default(),
            readers: Arc::new(ReaderPool {
                state: std::sync::Mutex::new(crate::ReaderState {
                    closed: false,
                    idle: Vec::new(),
                    active: 0,
                }),
                changed: std::sync::Condvar::new(),
            }),
            directory: PathBuf::new(),
            seed: String::new(),
        }
    }
}

pub(crate) struct Command {
    pub staged: PathBuf,
    pub reply: mpsc::Sender<Result<Activated, RestoreError>>,
}

pub(crate) fn execute(
    connection: &mut Connection,
    shared: &Shared,
    state: &mut WriterState,
    command: Command,
) {
    let context = &shared.restoration;
    let result = activate(
        shared,
        &context.readers,
        &context.directory,
        &context.seed,
        connection,
        &command.staged,
        |connection| {
            state.catalog.after_restore();
            state.tokens = crate::plan_publication::random_tokens();
            shared.import_epoch.fetch_add(1, Ordering::AcqRel);
            shared.domain_events.reset(connection)?;
            shared.restoration.snapshots.invalidate();
            Ok(())
        },
    );
    let _ = command.reply.send(result);
}

/// The text a restore request repeats to confirm it.
pub const CONFIRMATION: &str = "Restore replaces this installation's database and files with the backup. It cannot reverse prints already sent to printers or filament usage already recorded in Spoolman.";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Activated {
    /// Schema version of the database as restored.
    pub previous_schema_version: u64,
    /// Schema version after the restored database was opened and migrated.
    pub schema_version: u64,
}

/// Runs on the writer thread for [`Envelope::Control`].
fn activate(
    shared: &Shared,
    readers: &ReaderPool,
    data_dir: &Path,
    seed: &str,
    connection: &mut Connection,
    staged: &Path,
    mut refresh: impl FnMut(&mut Connection) -> Result<()>,
) -> Result<Activated, RestoreError> {
    pause_admission(shared)?;
    let database = data_dir.join(archive::DATABASE_FILE);
    let reader_count = match drain_readers(readers) {
        Ok(count) => count,
        Err(count) => {
            // Nothing changed yet: queued work and job streams carry on.
            open_readers(shared, readers, &database, count).map_err(WriterFailure::from)?;
            resume_admission(shared, false);
            return Err(Refusal::Busy.into());
        }
    };
    shared.job_subscriptions.stop();
    reject_queued(&mut queue(shared));
    let forward = (|| -> Result<(Connection, schema::SchemaReady, Vec<Connection>)> {
        connection.query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |_| Ok(()))?;
        std::mem::replace(connection, Connection::open_in_memory()?)
            .close()
            .map_err(|(_, error)| error)?;
        hooks::during_activation(data_dir);
        let mut journal = Journal::begin(data_dir, staged)?;
        journal.swap(data_dir)?;
        hooks::step(data_dir)?;
        let (mut restored, ready) = open_database(data_dir, seed)?;
        hooks::step(data_dir)?;
        refresh(&mut restored)?;
        hooks::step(data_dir)?;
        let prepared = prepare_readers(&database, reader_count)?;
        hooks::step(data_dir)?;
        journal.commit(data_dir)?;
        hooks::step(data_dir)?;
        journal.finish(data_dir);
        Ok((restored, ready, prepared))
    })();
    let (activated, prepared) = match forward {
        Ok((restored, ready, prepared)) => {
            *connection = restored;
            (
                Activated {
                    previous_schema_version: ready.previous_version,
                    schema_version: ready.version,
                },
                prepared,
            )
        }
        Err(error) => {
            let rollback = (|| -> Result<()> {
                ensure!(!hooks::crashed(&error), "simulated crash");
                if let Some(mut journal) = Journal::read(data_dir)? {
                    journal.rollback(data_dir)?;
                } else {
                    hooks::after_rollback(data_dir);
                }
                *connection = open_database(data_dir, seed)?.0;
                refresh(connection)?;
                open_readers(shared, readers, &database, reader_count)
            })();
            let Err(rollback) = rollback else {
                tracing::error!(error = %format!("{error:#}"), "restore failed and rolled back");
                resume_admission(shared, true);
                return Err(RestoreError::RolledBack);
            };
            // Stay stopped: the next start replays the journal.
            tracing::error!(
                error = %format!("{error:#}"),
                rollback = %format!("{rollback:#}"),
                "restore failed and could not roll back"
            );
            let mut queue = queue(shared);
            queue.closed = true;
            reject_queued(&mut queue);
            shared.changed.notify_all();
            return Err(RestoreError::RestartRequired);
        }
    };
    install_readers(shared, readers, prepared).map_err(WriterFailure::from)?;
    resume_admission(shared, true);
    Ok(activated)
}

fn queue(shared: &Shared) -> MutexGuard<'_, crate::Queue> {
    shared
        .queue
        .lock()
        .unwrap_or_else(crate::stop_poisoned_queue)
}

/// Refuses new writer, authentication and restore work until
/// [`resume_admission`]. Families that queue work themselves check only
/// `closed`; [`reject_queued`] fails what they add meanwhile.
fn pause_admission(shared: &Shared) -> Result<(), WriterFailure> {
    let mut queue = queue(shared);
    if queue.closed {
        return Err(WriterFailure::Stopped);
    }
    queue.restoring = true;
    shared.restoration.activity.lock().paused = true;
    Ok(())
}

/// Fails queued work unrun: dropping an envelope drops its reply sender.
fn reject_queued(queue: &mut crate::Queue) {
    queue.pending.clear();
    for work in queue.auth_ready.drain(..) {
        crate::auth::rebuild::finish_stopped(work);
    }
}

/// Reopens admission. After the database was replaced or rolled back,
/// `changed` fails the work queued while that ran, under the same lock.
fn resume_admission(shared: &Shared, changed: bool) {
    let mut queue = queue(shared);
    if changed {
        reject_queued(&mut queue);
    }
    queue.restoring = false;
    shared.restoration.activity.lock().paused = false;
    shared.changed.notify_all();
}

/// Closes idle readers and waits for checkouts to return. `Err` carries the
/// pool size when a checkout is still out after [`READER_DRAIN`].
fn drain_readers(readers: &ReaderPool) -> Result<usize, usize> {
    let mut state = readers
        .state
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let count = state.idle.len() + state.active;
    state.closed = true;
    state.idle.clear();
    readers.changed.notify_all();
    let deadline = Instant::now() + READER_DRAIN;
    while state.active > 0 && Instant::now() < deadline {
        state = readers
            .changed
            .wait_timeout(state, deadline.saturating_duration_since(Instant::now()))
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .0;
    }
    if state.active > 0 {
        Err(count)
    } else {
        Ok(count)
    }
}

fn open_readers(
    shared: &Shared,
    readers: &ReaderPool,
    database: &Path,
    count: usize,
) -> Result<()> {
    let queue = shared
        .queue
        .lock()
        .map_err(|_| anyhow!("Writer admission poisoned"))?;
    if queue.closed {
        return Ok(());
    }
    let mut state = readers
        .state
        .lock()
        .map_err(|_| anyhow!("Reader pool poisoned"))?;
    let count = count.saturating_sub(state.active);
    let idle = prepare_readers(database, count)?;
    state.idle = idle;
    state.closed = false;
    readers.changed.notify_all();
    Ok(())
}

fn prepare_readers(database: &Path, count: usize) -> Result<Vec<Connection>> {
    let mut idle = Vec::with_capacity(count);
    for _ in 0..count {
        let reader = Connection::open_with_flags(
            database,
            OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )?;
        schema::configure(&reader, true)?;
        idle.push(reader);
    }
    Ok(idle)
}

fn install_readers(shared: &Shared, readers: &ReaderPool, idle: Vec<Connection>) -> Result<()> {
    let queue = shared
        .queue
        .lock()
        .map_err(|_| anyhow!("Writer admission poisoned"))?;
    if queue.closed {
        return Ok(());
    }
    let mut state = readers
        .state
        .lock()
        .map_err(|_| anyhow!("Reader pool poisoned"))?;
    state.idle = idle;
    state.closed = false;
    readers.changed.notify_all();
    Ok(())
}

/// Opens the live database as `WriterOwner::open` does: schema admission,
/// migrations and job recovery.
fn open_database(data_dir: &Path, seed: &str) -> Result<(Connection, schema::SchemaReady)> {
    let version = admit_schema(data_dir)?;
    let (mut connection, ready) =
        schema::initialize(&data_dir.join(archive::DATABASE_FILE), version, seed)?;
    jobs::recover(&mut connection)?;
    crate::send_queue::recover(&mut connection)?;
    Ok((connection, ready))
}

#[cfg(all(test, unix))]
pub(super) use super::tests as hooks;
#[cfg(not(all(test, unix)))]
pub(super) mod hooks {
    pub(crate) fn step(_: &std::path::Path) -> anyhow::Result<()> {
        Ok(())
    }
    pub(super) fn during_activation(_: &std::path::Path) {}
    pub(crate) fn after_rollback(_: &std::path::Path) {}
    pub(super) fn crashed(_: &anyhow::Error) -> bool {
        false
    }
}
