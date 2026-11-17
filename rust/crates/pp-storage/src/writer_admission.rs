use crate::{ReaderFailure, auth::AuthFailure, schema::BUSY_TIMEOUT};
use anyhow::Result;
use rusqlite::{Connection, Error, ErrorCode, Transaction, TransactionBehavior};
use std::{
    thread,
    time::{Duration, Instant},
};

const RETRY_PAUSE: Duration = Duration::from_millis(10);

/// Why the writer queue did not take a request. The text is the message
/// existing callers and `pp-api` compare, so `?` into `anyhow::Error` gives
/// them the same error as before.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub(super) enum AdmissionError {
    #[error("Storage stopped")]
    Stopped,
    #[error("Cancelled before admission")]
    Cancelled,
    #[error("Writer queue full")]
    QueueFull,
    #[error("Writer admission poisoned")]
    Poisoned,
}

/// A failure every storage operation shares, whatever its family: a write
/// through `Shared::write` or a read through `SettingsClient::read`. A
/// family's error enum carries it in one variant, and `pp-api` maps it in one
/// place (`pp_api::problem::writer_failure`). Nothing was written in any of
/// these cases.
#[derive(Debug, thiserror::Error)]
pub enum WriterFailure {
    /// The credential did not prove a tenant.
    #[error("credential refused: {0}")]
    Auth(#[from] AuthFailure),
    /// The writer queue stayed full for the caller's whole wait, so the
    /// write was never queued. Retry later.
    #[error("writer queue full")]
    QueueFull,
    /// Storage is shutting down or its writer has stopped.
    #[error("storage stopped")]
    Stopped,
    /// The caller gave up before the write was queued.
    #[error("cancelled before the write was queued")]
    Cancelled,
    /// The write was queued and reached the writer thread, but its
    /// transaction could not start before the deadline: another process
    /// holds the database, where `QueueFull` means this process's own queue
    /// is backed up. For a read, every reader stayed checked out for the
    /// caller's whole wait. Retry later.
    #[error("writer busy past the deadline")]
    Busy,
    /// SQLite or another failure no caller can act on. Never holds a secret.
    #[error("storage failure")]
    Internal(#[source] anyhow::Error),
}

impl From<AdmissionError> for WriterFailure {
    fn from(error: AdmissionError) -> Self {
        match error {
            AdmissionError::Stopped | AdmissionError::Poisoned => Self::Stopped,
            AdmissionError::Cancelled => Self::Cancelled,
            AdmissionError::QueueFull => Self::QueueFull,
        }
    }
}

impl From<ReaderFailure> for WriterFailure {
    fn from(failure: ReaderFailure) -> Self {
        match failure {
            ReaderFailure::Stopped | ReaderFailure::Poisoned => Self::Stopped,
            ReaderFailure::Full => Self::Busy,
            ReaderFailure::Unusable(error) => Self::Internal(error),
        }
    }
}

impl From<anyhow::Error> for WriterFailure {
    /// A `WriterFailure` an older helper wrapped in `anyhow` is returned as
    /// it was, an auth refusal keeps its [`AuthFailure`], and SQLite's busy
    /// code is [`WriterFailure::Busy`]; anything else is `Internal`.
    fn from(error: anyhow::Error) -> Self {
        let error = match error.downcast::<WriterFailure>() {
            Ok(failure) => return failure,
            Err(error) => error,
        };
        let busy = error
            .downcast_ref::<Error>()
            .is_some_and(|error| error.sqlite_error_code() == Some(ErrorCode::DatabaseBusy));
        if busy {
            return Self::Busy;
        }
        match error.downcast::<AuthFailure>() {
            Ok(failure) => Self::Auth(failure),
            Err(error) => Self::Internal(error),
        }
    }
}

impl From<Error> for WriterFailure {
    fn from(error: Error) -> Self {
        anyhow::Error::from(error).into()
    }
}

#[derive(Clone, Copy)]
pub(super) struct Deadline(Instant);

impl Deadline {
    #[cfg(all(test, feature = "desktop_rebuild"))]
    pub(super) fn expired() -> Self {
        Self(Instant::now())
    }

    pub(super) fn at_dequeue() -> Self {
        Self(Instant::now() + BUSY_TIMEOUT)
    }
}

pub(super) fn begin_immediate(
    connection: &mut Connection,
    deadline: Deadline,
) -> Result<Transaction<'_>> {
    begin_with_clock(connection, deadline, Instant::now, thread::sleep)
}

fn busy_error() -> Error {
    Error::SqliteFailure(
        rusqlite::ffi::Error::new(rusqlite::ffi::SQLITE_BUSY),
        Some("Writer transaction admission deadline expired".into()),
    )
}

fn begin_with_clock(
    connection: &mut Connection,
    deadline: Deadline,
    mut now: impl FnMut() -> Instant,
    mut sleep: impl FnMut(Duration),
) -> Result<Transaction<'_>> {
    if now() >= deadline.0 {
        return Err(busy_error().into());
    }
    let connection = &*connection;
    connection.busy_timeout(Duration::ZERO)?;
    let mut last_busy = None;
    let acquired = loop {
        if now() >= deadline.0 {
            break Err(last_busy.take().unwrap_or_else(busy_error));
        }
        // The mutable constructor's returned borrow spans loop retries.
        match Transaction::new_unchecked(connection, TransactionBehavior::Immediate) {
            Ok(tx) => break Ok((tx, now())),
            Err(error) if error.sqlite_error_code() == Some(ErrorCode::DatabaseBusy) => {
                let observed = now();
                if observed >= deadline.0 {
                    break Err(error);
                }
                last_busy = Some(error);
                sleep(RETRY_PAUSE.min(deadline.0 - observed));
            }
            Err(error) => break Err(error),
        }
    };
    connection.busy_timeout(BUSY_TIMEOUT)?;
    let (tx, acquired_at) = acquired?;
    if acquired_at >= deadline.0 || now() >= deadline.0 {
        tx.rollback()?;
        return Err(last_busy.unwrap_or_else(busy_error).into());
    }
    Ok(tx)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;

    fn connections() -> (std::path::PathBuf, Connection, Connection) {
        let path = crate::tests::directory("admission");
        let db = path.join("admission.db");
        let writer = Connection::open(&db).unwrap();
        crate::schema::configure(&writer, false).unwrap();
        writer
            .execute_batch("CREATE TABLE writes(value TEXT);")
            .unwrap();
        let holder = Connection::open(db).unwrap();
        holder.busy_timeout(Duration::ZERO).unwrap();
        (path, writer, holder)
    }

    fn assert_restored(connection: &Connection) {
        assert!(connection.is_autocommit());
        assert_eq!(crate::tests::pragma_value(connection, "busy_timeout"), 5000);
    }

    fn assert_busy(error: anyhow::Error) {
        assert_eq!(
            error.downcast_ref::<Error>().unwrap().sqlite_error_code(),
            Some(ErrorCode::DatabaseBusy),
            "{error:#}"
        );
    }

    #[test]
    fn ticket_t_7_writer_oversleep_refuses_a_lock_released_after_the_deadline() {
        let (path, mut writer, mut holder) = connections();
        let mut held = Some(
            holder
                .transaction_with_behavior(TransactionBehavior::Immediate)
                .unwrap(),
        );
        let started = Instant::now();
        let elapsed = Cell::new(Duration::ZERO);
        let mut sleeps = Vec::new();
        let error = begin_with_clock(
            &mut writer,
            Deadline(started + BUSY_TIMEOUT),
            || started + elapsed.get(),
            |pause| {
                sleeps.push(pause);
                elapsed.set(Duration::from_secs(6));
                held.take().unwrap().rollback().unwrap();
            },
        )
        .unwrap_err();
        assert_busy(error);
        assert_eq!(sleeps, [Duration::from_millis(10)]);
        assert_eq!(elapsed.get(), Duration::from_secs(6));
        assert_restored(&writer);

        let tx = begin_immediate(&mut writer, Deadline::at_dequeue()).unwrap();
        assert_eq!(crate::tests::pragma_value(&tx, "busy_timeout"), 5000);
        tx.execute("INSERT INTO writes VALUES('next')", []).unwrap();
        tx.commit().unwrap();
        assert_restored(&writer);
        drop(held);
        let value: String = holder
            .query_row("SELECT value FROM writes", [], |r| r.get(0))
            .unwrap();
        assert_eq!(value, "next");
        eprintln!(
            "oversleep advanced 6s, released the real lock, refused admission, restored 5000, and committed the next transaction"
        );
        drop(holder);
        drop(writer);
        std::fs::remove_dir_all(path).unwrap();
    }

    #[test]
    fn ticket_t_7_writer_late_success_is_rolled_back_before_and_after_restoration() {
        for expired_read in [3, 4] {
            let (path, mut writer, mut holder) = connections();
            let started = Instant::now();
            let mut reads = 0;
            let error = begin_with_clock(
                &mut writer,
                Deadline(started + BUSY_TIMEOUT),
                || {
                    reads += 1;
                    started
                        + if reads >= expired_read {
                            BUSY_TIMEOUT
                        } else {
                            Duration::ZERO
                        }
                },
                |_| panic!("uncontended acquisition must not sleep"),
            )
            .unwrap_err();
            assert_busy(error);
            assert_restored(&writer);
            holder
                .transaction_with_behavior(TransactionBehavior::Immediate)
                .unwrap()
                .rollback()
                .unwrap();
            let tx = begin_immediate(&mut writer, Deadline::at_dequeue()).unwrap();
            assert_eq!(crate::tests::pragma_value(&tx, "busy_timeout"), 5000);
            tx.commit().unwrap();
            assert_restored(&writer);
            drop(holder);
            drop(writer);
            std::fs::remove_dir_all(path).unwrap();
        }
    }

    #[test]
    fn ticket_t_7_writer_expired_entry_leaves_the_handler_unchanged() {
        let (path, mut writer, holder) = connections();
        writer.busy_timeout(Duration::from_millis(1234)).unwrap();
        let expired = Instant::now();
        let error = begin_with_clock(
            &mut writer,
            Deadline(expired),
            || expired,
            |_| panic!("expired admission must not sleep"),
        )
        .unwrap_err();
        assert_busy(error);
        assert!(writer.is_autocommit());
        assert_eq!(crate::tests::pragma_value(&writer, "busy_timeout"), 1234);
        writer.busy_timeout(BUSY_TIMEOUT).unwrap();
        begin_immediate(&mut writer, Deadline::at_dequeue())
            .unwrap()
            .commit()
            .unwrap();
        assert_restored(&writer);
        drop(holder);
        drop(writer);
        std::fs::remove_dir_all(path).unwrap();
    }

    #[test]
    fn ticket_t_7_writer_non_busy_begin_error_is_terminal_and_restores_the_timeout() {
        let (path, mut writer, holder) = connections();
        writer.pragma_update(None, "query_only", true).unwrap();
        let started = Instant::now();
        let mut reads = 0;
        let error = begin_with_clock(
            &mut writer,
            Deadline(started + BUSY_TIMEOUT),
            || {
                reads += 1;
                started
            },
            |_| panic!("non-busy errors must not retry"),
        )
        .unwrap_err();
        assert_eq!(
            error.downcast_ref::<Error>().unwrap().sqlite_error_code(),
            Some(ErrorCode::ReadOnly)
        );
        assert_eq!(reads, 2);
        assert_restored(&writer);
        writer.pragma_update(None, "query_only", false).unwrap();
        begin_immediate(&mut writer, Deadline::at_dequeue())
            .unwrap()
            .commit()
            .unwrap();
        assert_restored(&writer);
        drop(holder);
        drop(writer);
        std::fs::remove_dir_all(path).unwrap();
    }

    #[test]
    fn ticket_t_7_writer_expiry_after_busy_preserves_the_sqlite_error() {
        let (path, mut writer, mut holder) = connections();
        let mut held = Some(
            holder
                .transaction_with_behavior(TransactionBehavior::Immediate)
                .unwrap(),
        );
        let started = Instant::now();
        let mut reads = 0;
        let error = begin_with_clock(
            &mut writer,
            Deadline(started + BUSY_TIMEOUT),
            || {
                reads += 1;
                if reads == 3 {
                    held.take().unwrap().rollback().unwrap();
                    started + BUSY_TIMEOUT
                } else {
                    started
                }
            },
            |_| panic!("busy result observed at expiry must not sleep"),
        )
        .unwrap_err();
        assert_eq!(reads, 3);
        assert!(matches!(error.downcast_ref::<Error>(),
            Some(Error::SqliteFailure(code, Some(message)))
                if code.code == ErrorCode::DatabaseBusy && message == "database is locked"));
        assert_restored(&writer);
        begin_immediate(&mut writer, Deadline::at_dequeue())
            .unwrap()
            .commit()
            .unwrap();
        assert_restored(&writer);
        drop(held);
        drop(holder);
        drop(writer);
        std::fs::remove_dir_all(path).unwrap();
    }

    #[test]
    fn ticket_t_7_writer_release_before_expiry_admits_and_restores_before_business_sql() {
        let (path, mut writer, mut holder) = connections();
        let mut held = Some(
            holder
                .transaction_with_behavior(TransactionBehavior::Immediate)
                .unwrap(),
        );
        let started = Instant::now();
        let elapsed = Cell::new(Duration::from_millis(4999));
        let mut sleeps = Vec::new();
        let tx = begin_with_clock(
            &mut writer,
            Deadline(started + BUSY_TIMEOUT),
            || started + elapsed.get(),
            |pause| {
                sleeps.push(pause);
                elapsed.set(Duration::from_micros(4_999_500));
                held.take().unwrap().commit().unwrap();
            },
        )
        .unwrap();
        assert_eq!(crate::tests::pragma_value(&tx, "busy_timeout"), 5000);
        tx.execute("INSERT INTO writes VALUES('released')", [])
            .unwrap();
        tx.commit().unwrap();
        assert_eq!(sleeps, [Duration::from_millis(1)]);
        assert_restored(&writer);
        drop(held);
        let value: String = holder
            .query_row("SELECT value FROM writes", [], |r| r.get(0))
            .unwrap();
        assert_eq!(value, "released");
        drop(holder);
        drop(writer);
        std::fs::remove_dir_all(path).unwrap();
    }
}
