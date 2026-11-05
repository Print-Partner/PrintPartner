//! Domain event log (GRE-1088): a bounded change feed committed with the
//! state it describes, so the UI can invalidate queries instead of polling.
//!
//! A family appends an event inside its own write transaction with
//! [`append`]. A rolled-back change therefore leaves no event, and commit order
//! is cursor order because one writer thread assigns every sequence number.
//! This is a change feed, not event sourcing: SQLite tables stay the state.
//!
//! Bounds: the log keeps the newest [`RETAINED_EVENTS`] events, each payload is
//! at most [`MAX_PAYLOAD_BYTES`], and one read returns at most [`READ_LIMIT`].
//!
//! Every storage open draws a new random generation, held in memory, and
//! treats the events already stored as expired. A cursor names its
//! generation, so a cursor from before a restart or a restored copy is refused
//! with a resync instead of matching unrelated events. Opening writes nothing.
//! Reads authenticate the caller and return only that tenant's events.

use crate::{
    Envelope, Shared, WriterOwner, auth,
    read_model::{Credential, read_tenant, submit_read},
};
use anyhow::{Result, anyhow, ensure};
use rusqlite::{Connection, OptionalExtension, Transaction, TransactionBehavior, params};
use serde::{Serialize, Serializer};
use serde_json::Value;
use std::{
    fmt,
    sync::{
        Arc, Mutex, PoisonError, Weak,
        atomic::{AtomicBool, AtomicI64, Ordering},
    },
    time::Duration,
};

/// The schema step that installs the log.
pub(crate) const SCHEMA_STEP: u64 = 42;
/// Events kept for replay. Older events are deleted as new ones commit.
pub const RETAINED_EVENTS: i64 = 4096;
pub const MAX_PAYLOAD_BYTES: usize = 2048;
/// Events returned by one read.
pub const READ_LIMIT: usize = 256;
/// Live subscriptions one storage owner accepts.
pub const MAX_SUBSCRIPTIONS: usize = 64;

/// One event type at one payload schema version.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EventKind {
    pub name: &'static str,
    pub version: u32,
}

/// A unit's completed or assembled flag changed, or counts were imported.
pub const PROGRESS_CHANGED: EventKind = EventKind {
    name: "checkoff.progress_changed",
    version: 1,
};
/// A draft was accepted as a new Plan revision.
pub const REVISION_ACCEPTED: EventKind = EventKind {
    name: "plan.revision_accepted",
    version: 1,
};
/// Every kind this build writes and reads. A stored event of any other type
/// or version makes a read answer a resync instead of skipping it.
pub const KNOWN: [EventKind; 2] = [PROGRESS_CHANGED, REVISION_ACCEPTED];

/// A position in the log: `<generation>.<sequence>`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Cursor {
    generation: String,
    seq: i64,
}

impl Cursor {
    /// Accepts 32 lowercase hex digits, a dot, and a decimal sequence without
    /// leading zeros.
    pub fn parse(text: &str) -> Option<Self> {
        let (generation, seq) = text.split_once('.')?;
        let generation_ok = generation.len() == 32
            && generation
                .bytes()
                .all(|byte| matches!(byte, b'0'..=b'9' | b'a'..=b'f'));
        let seq_ok = !seq.is_empty()
            && seq.bytes().all(|byte| byte.is_ascii_digit())
            && (seq == "0" || !seq.starts_with('0'));
        if !(generation_ok && seq_ok) {
            return None;
        }
        Some(Self {
            generation: generation.to_owned(),
            seq: seq.parse().ok()?,
        })
    }
}

impl fmt::Display for Cursor {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}.{}", self.generation, self.seq)
    }
}

impl Serialize for Cursor {
    fn serialize<S: Serializer>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error> {
        serializer.collect_str(self)
    }
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct DomainEvent {
    pub cursor: Cursor,
    #[serde(rename = "type")]
    pub event_type: String,
    pub schema_version: u32,
    pub aggregate_type: String,
    pub aggregate_id: String,
    pub aggregate_version: Option<i64>,
    pub operation_id: String,
    /// Commit time in Unix milliseconds.
    pub occurred_at: i64,
    pub payload: Value,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ResyncReason {
    /// The cursor is from another storage open or another database copy.
    GenerationChanged,
    /// Events after the cursor were already deleted.
    Expired,
    /// The cursor is past the newest event.
    AheadOfLog,
    /// An event after the cursor has a type or version this build does not know.
    UnknownVersion,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Read {
    /// The caller's events after the requested cursor, in commit order.
    /// `position` is where the next read starts. Without a requested cursor
    /// this is empty and `position` is the newest event.
    Events {
        events: Vec<DomainEvent>,
        position: Cursor,
    },
    /// The caller must reload its state, then continue from `position`.
    Resync {
        reason: ResyncReason,
        position: Cursor,
    },
}

/// What a family records about one committed change.
pub(crate) struct Change {
    pub kind: EventKind,
    pub aggregate_type: &'static str,
    pub aggregate_id: String,
    pub aggregate_version: Option<i64>,
    pub payload: Value,
}

/// Appends `change` inside `tx`, the transaction that makes the change, and
/// deletes events past the retention bound. Each call is one operation with a
/// fresh operation ID.
pub(crate) fn append(tx: &Transaction<'_>, tenant: &str, change: Change) -> Result<()> {
    ensure!(KNOWN.contains(&change.kind), "Unknown domain event kind");
    let payload = serde_json::to_string(&change.payload)?;
    ensure!(
        payload.len() <= MAX_PAYLOAD_BYTES,
        "Domain event payload exceeds {MAX_PAYLOAD_BYTES} bytes"
    );
    let mut operation = [0u8; 16];
    getrandom::fill(&mut operation).map_err(|_| anyhow!("Entropy unavailable"))?;
    let occurred_at = time::OffsetDateTime::now_utc().unix_timestamp_nanos() / 1_000_000;
    tx.execute(
        "INSERT INTO domain_events(tenant,event_type,schema_version,aggregate_type,aggregate_id,aggregate_version,operation_id,payload_json,occurred_at) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9)",
        params![
            tenant,
            change.kind.name,
            change.kind.version,
            change.aggregate_type,
            change.aggregate_id,
            change.aggregate_version,
            hex::encode(operation),
            payload,
            i64::try_from(occurred_at)?,
        ],
    )?;
    tx.execute(
        "DELETE FROM domain_events WHERE seq<=?1",
        [tx.last_insert_rowid() - RETAINED_EVENTS],
    )?;
    Ok(())
}

fn head(connection: &Connection) -> Result<i64> {
    let seq: Option<i64> = connection
        .query_row(
            "SELECT seq FROM sqlite_sequence WHERE name='domain_events'",
            [],
            |row| row.get(0),
        )
        .optional()?;
    Ok(seq.unwrap_or(0))
}

fn read(tx: &Transaction<'_>, feed: &Feed, tenant: &str, after: Option<&Cursor>) -> Result<Read> {
    let head = head(tx)?;
    let pruned = feed.opened_at.max(head - RETAINED_EVENTS);
    let at = |seq| Cursor {
        generation: feed.generation.clone(),
        seq,
    };
    let Some(after) = after else {
        return Ok(Read::Events {
            events: Vec::new(),
            position: at(head),
        });
    };
    let refused = if after.generation != feed.generation {
        Some(ResyncReason::GenerationChanged)
    } else if after.seq > head {
        Some(ResyncReason::AheadOfLog)
    } else if after.seq < pruned {
        Some(ResyncReason::Expired)
    } else {
        None
    };
    if let Some(reason) = refused {
        return Ok(Read::Resync {
            reason,
            position: at(head),
        });
    }
    let mut statement = tx.prepare(
        "SELECT seq,event_type,schema_version,aggregate_type,aggregate_id,aggregate_version,operation_id,occurred_at,payload_json FROM domain_events WHERE tenant=?1 AND seq>?2 ORDER BY seq LIMIT ?3",
    )?;
    let mut rows = statement.query(params![tenant, after.seq, READ_LIMIT as i64])?;
    let mut events = Vec::new();
    while let Some(row) = rows.next()? {
        let event_type: String = row.get(1)?;
        let schema_version: u32 = row.get(2)?;
        if !KNOWN
            .iter()
            .any(|kind| kind.name == event_type && kind.version == schema_version)
        {
            return Ok(Read::Resync {
                reason: ResyncReason::UnknownVersion,
                position: at(head),
            });
        }
        events.push(DomainEvent {
            cursor: at(row.get(0)?),
            event_type,
            schema_version,
            aggregate_type: row.get(3)?,
            aggregate_id: row.get(4)?,
            aggregate_version: row.get(5)?,
            operation_id: row.get(6)?,
            occurred_at: row.get(7)?,
            payload: serde_json::from_str(&row.get::<_, String>(8)?)?,
        });
    }
    let position = match events.last() {
        Some(last) if events.len() == READ_LIMIT => last.cursor.clone(),
        _ => at(head),
    };
    Ok(Read::Events { events, position })
}

pub(crate) struct Command {
    credential: Credential,
    policy: auth::AuthPolicy,
    after: Option<Cursor>,
}

pub(crate) fn execute(connection: &mut Connection, feed: &Feed, command: Command) -> Result<Read> {
    let tx = connection.transaction_with_behavior(TransactionBehavior::Deferred)?;
    let tenant = read_tenant(&tx, &command.credential, command.policy)?;
    let result = read(&tx, feed, &tenant, command.after.as_ref())?;
    tx.commit()?;
    Ok(result)
}

/// A callback run on the writer thread after events commit or an auth change
/// commits. It must only signal a task and return: it must not block or call
/// storage, because the writer waits for it. A waker that panics is
/// unsubscribed and the writer continues.
pub type Waker = Arc<dyn Fn() + Send + Sync>;

/// One storage open's view of the log: its generation, the newest sequence
/// at open (events up to it are expired), and live wake-ups for readers.
/// Wakers are held weakly: dropping the caller's [`Waker`] ends its
/// subscription.
pub(crate) struct Feed {
    generation: String,
    opened_at: i64,
    head: AtomicI64,
    wakers: Mutex<Vec<Weak<dyn Fn() + Send + Sync>>>,
}

impl Feed {
    pub(crate) fn new(head: i64) -> Self {
        Self {
            generation: hex::encode(rand::random::<[u8; 16]>()),
            opened_at: head,
            head: AtomicI64::new(head),
            wakers: Mutex::new(Vec::new()),
        }
    }

    pub(crate) fn open(connection: &Connection) -> Result<Self> {
        Ok(Self::new(head(connection)?))
    }

    fn subscribe(&self, waker: &Waker) -> Result<()> {
        let mut wakers = self.wakers.lock().unwrap_or_else(PoisonError::into_inner);
        wakers.retain(|waker| waker.strong_count() > 0);
        ensure!(
            wakers.len() < MAX_SUBSCRIPTIONS,
            "Domain event subscriptions are full"
        );
        wakers.push(Arc::downgrade(waker));
        Ok(())
    }

    fn unsubscribe(&self, waker: &Waker) {
        let gone = Arc::downgrade(waker);
        self.wakers
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .retain(|waker| !Weak::ptr_eq(waker, &gone));
    }

    pub(crate) fn wake(&self) {
        let live: Vec<Waker> = {
            let mut wakers = self.wakers.lock().unwrap_or_else(PoisonError::into_inner);
            wakers.retain(|waker| waker.strong_count() > 0);
            wakers.iter().filter_map(Weak::upgrade).collect()
        };
        for waker in live {
            if std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| waker())).is_err() {
                self.unsubscribe(&waker);
            }
        }
    }

    /// Runs after each writer envelope. Wakes readers when the newest
    /// sequence moved or `authority_changed`, so revoked sessions are
    /// rechecked at once.
    pub(crate) fn after_write(&self, connection: &Connection, authority_changed: bool) {
        let moved = match head(connection) {
            Ok(head) => self.head.swap(head, Ordering::AcqRel) != head,
            Err(_) => true,
        };
        if moved || authority_changed {
            self.wake();
        }
    }
}

#[derive(Clone)]
pub struct DomainEventsClient {
    shared: Arc<Shared>,
    policy: auth::AuthPolicy,
}

impl WriterOwner {
    pub fn domain_events(&self) -> Result<DomainEventsClient> {
        self.domain_events_with_policy(auth::AuthPolicy {
            registration: auth::RegistrationPolicy::Open,
            session_tenant: auth::SessionTenantPolicy::AccountTenant,
            first_user: auth::FirstUserTenant::NewUser,
        })
    }

    pub fn domain_events_with_policy(
        &self,
        policy: auth::AuthPolicy,
    ) -> Result<DomainEventsClient> {
        auth::validate_policy(policy)?;
        ensure!(self.lease.is_some(), "Storage owner is closed");
        Ok(DomainEventsClient {
            shared: self.client.shared.clone(),
            policy,
        })
    }
}

impl DomainEventsClient {
    /// Authenticates `credential` and reads its tenant's events after
    /// `after`, or the current position when `after` is `None`. Runs on
    /// the writer queue, so an auth failure is an `Err`.
    pub fn read(
        &self,
        credential: Credential,
        after: Option<Cursor>,
        cancelled: &AtomicBool,
        wait: Duration,
    ) -> Result<Read> {
        submit_read(&self.shared, cancelled, wait, |reply| {
            Envelope::DomainEvents {
                command: Command {
                    credential,
                    policy: self.policy,
                    after,
                },
                reply,
            }
        })
    }

    /// Calls `waker` after events commit, after auth changes, and when
    /// storage stops, until the caller drops it. Subscribe before the first
    /// read so no commit between them is missed.
    pub fn subscribe(&self, waker: &Waker) -> Result<()> {
        self.shared.domain_events.subscribe(waker)
    }
}

#[cfg(test)]
mod tests;
