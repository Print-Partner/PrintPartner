//! One lock per integration for its certificate pin (GRE-1245, GRE-1296).
//!
//! A Bambu Test connection holds it from reading the pin, through the network
//! test, until the pin write has settled; a reset takes it too. Without it,
//! two first-use tests could both read "no pin", and the second could send the
//! access code to a different certificate before the first one's pin is
//! stored.
//!
//! Once a pin write or reset is queued, only the writer's answer (applied or
//! refused) or a stopped writer releases the lock: never elapsed time and
//! never a dropped request. The storage writer runs a queued command even
//! after its caller stopped waiting, so releasing earlier would let a later
//! first-use test run while an earlier pin can still land.
use anyhow::{Result, anyhow};
use pp_storage::WriteReply;
use std::{
    collections::HashMap,
    sync::{Arc, LazyLock, Mutex, MutexGuard, PoisonError, mpsc::TryRecvError},
    time::Duration,
};

/// How long a request waits for the lock before it answers Busy. Longer than
/// one Test connection: the driver's 10-second status timeout, the reads and
/// a normal pin write.
const LOCK_WAIT: Duration = Duration::from_secs(30);
/// How long a request waits for a queued write's answer before it answers
/// Busy. The lock waits on for the writer.
const ANSWER_WAIT: Duration = Duration::from_millis(10_500);
/// How often the lock's holder checks for the writer's answer.
const SETTLE_POLL: Duration = Duration::from_millis(20);

/// Every [`super::StoredPrinterHosts`] in the process shares these.
pub(super) static PIN_LOCKS: LazyLock<PinLocks> = LazyLock::new(PinLocks::default);

/// An integration's lock and how many [`PinGuard`]s hold or wait on it. The
/// count is explicit, so a waiter cancelled at any point still removes the
/// entry when it is the last one.
struct Slot {
    lock: Arc<tokio::sync::Mutex<()>>,
    users: usize,
}

#[derive(Default)]
pub(super) struct PinLocks(Mutex<HashMap<String, Slot>>);

impl PinLocks {
    fn slots(&self) -> MutexGuard<'_, HashMap<String, Slot>> {
        self.0.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// The integration's lock, or `None` after [`LOCK_WAIT`].
    pub(super) async fn lock(&'static self, integration: &str) -> Option<PinGuard> {
        // Counted before the first await, so cancelling this future drops a
        // guard that removes the entry.
        let mut pin = PinGuard::join(self, integration);
        let lock = pin.lock.clone();
        pin.held = Some(
            tokio::time::timeout(LOCK_WAIT, lock.lock_owned())
                .await
                .ok()?,
        );
        Some(pin)
    }
}

/// A holder of, or waiter on, an integration's lock. Dropping it releases the
/// lock if held and removes the entry once no guard is left.
pub(super) struct PinGuard {
    locks: &'static PinLocks,
    integration: String,
    lock: Arc<tokio::sync::Mutex<()>>,
    held: Option<tokio::sync::OwnedMutexGuard<()>>,
}

impl PinGuard {
    fn join(locks: &'static PinLocks, integration: &str) -> Self {
        let lock = {
            let mut slots = locks.slots();
            let slot = slots.entry(integration.to_owned()).or_insert_with(|| Slot {
                lock: Arc::default(),
                users: 0,
            });
            slot.users += 1;
            slot.lock.clone()
        };
        Self {
            locks,
            integration: integration.to_owned(),
            lock,
            held: None,
        }
    }
}

impl Drop for PinGuard {
    fn drop(&mut self) {
        let mut slots = self.locks.slots();
        drop(self.held.take());
        if let Some(slot) = slots.get_mut(&self.integration) {
            slot.users -= 1;
            if slot.users == 0 {
                slots.remove(&self.integration);
            }
        }
    }
}

/// Runs `submit` on the blocking pool and, when it queued a write, waits for
/// the writer's answer and then runs `after`, all while `guard` holds the
/// lock. A detached task owns the guard, so the request may answer Busy
/// [`ANSWER_WAIT`] after this starts (`None`) or be dropped without freeing
/// the lock. A writer that stops answering while the write is still queued
/// keeps the lock held: later Test connections and resets for that
/// integration answer Busy rather than trust a certificate.
pub(super) async fn under_lock<R: Send + 'static>(
    guard: PinGuard,
    submit: impl FnOnce() -> Result<Option<WriteReply>> + Send + 'static,
    after: impl FnOnce(Option<bool>) -> Result<R> + Send + 'static,
) -> Option<Result<R>> {
    let (answer, answered) = tokio::sync::oneshot::channel();
    tokio::spawn(async move {
        let _held = guard;
        let result = async {
            let written = match tokio::task::spawn_blocking(submit).await?? {
                Some(reply) => Some(settle(reply).await?),
                None => None,
            };
            tokio::task::spawn_blocking(move || after(written)).await?
        }
        .await;
        let _ = answer.send(result);
    });
    tokio::time::timeout(ANSWER_WAIT, answered).await.ok()?.ok()
}

/// The writer's answer to one queued command, however long it takes.
async fn settle(reply: WriteReply) -> Result<bool> {
    loop {
        match reply.try_recv() {
            Ok(answer) => return answer,
            Err(TryRecvError::Disconnected) => return Err(anyhow!("Storage stopped")),
            Err(TryRecvError::Empty) => tokio::time::sleep(SETTLE_POLL).await,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc;

    const SHORT: Duration = Duration::from_millis(200);

    fn private() -> &'static PinLocks {
        Box::leak(Box::default())
    }

    /// GRE-1296: a queued pin write keeps the lock past a dropped request
    /// until the writer answers, whether it applies, refuses or stops.
    #[tokio::test]
    async fn ticket_t_23_a_queued_write_keeps_the_lock_until_the_writer_answers() {
        let locks = private();
        for answer in [Some(Ok(true)), Some(Err(anyhow!("refused"))), None] {
            let (writer, reply) = mpsc::channel::<Result<bool>>();
            let guard = locks.lock("a").await.unwrap();
            let request = under_lock(guard, move || Ok(Some(reply)), Ok);
            assert!(
                tokio::time::timeout(SHORT, request).await.is_err(),
                "the request is dropped while the write is queued"
            );
            assert!(
                tokio::time::timeout(SHORT * 3, locks.lock("a"))
                    .await
                    .is_err(),
                "the lock was freed while a pin write could still land"
            );
            match answer {
                Some(answer) => writer.send(answer).unwrap(),
                None => drop(writer),
            }
            let next = tokio::time::timeout(Duration::from_secs(2), locks.lock("a")).await;
            assert!(matches!(next, Ok(Some(_))), "the writer answered");
        }
        assert!(locks.slots().is_empty());
    }

    /// No elapsed time frees a queued write's lock: an hour later it is
    /// still held, and the writer's answer frees it.
    #[tokio::test(start_paused = true)]
    async fn ticket_t_23_no_elapsed_time_frees_a_queued_write() {
        let locks = private();
        let (writer, reply) = mpsc::channel::<Result<bool>>();
        let guard = locks.lock("a").await.unwrap();
        let request = under_lock(guard, move || Ok(Some(reply)), Ok);
        assert!(tokio::time::timeout(SHORT, request).await.is_err());
        tokio::time::advance(Duration::from_secs(3600)).await;
        assert!(
            tokio::time::timeout(SHORT, locks.lock("a")).await.is_err(),
            "an hour freed the lock while a pin write could still land"
        );
        writer.send(Ok(true)).unwrap();
        let next = tokio::time::timeout(Duration::from_secs(1), locks.lock("a")).await;
        assert!(matches!(next, Ok(Some(_))), "the writer answered");
    }

    /// A command that queued nothing releases the lock as soon as `after`
    /// returns.
    #[tokio::test]
    async fn ticket_t_23_nothing_queued_releases_at_once() {
        let locks = private();
        let guard = locks.lock("a").await.unwrap();
        let answer = under_lock(guard, || Ok(None), |written| Ok(written.is_none())).await;
        assert!(matches!(answer, Some(Ok(true))));
        assert!(locks.slots().is_empty());
    }

    /// GRE-1296 P3: a waiter cancelled before or after it was woken leaves no
    /// entry behind.
    #[tokio::test]
    async fn ticket_t_23_cancelled_waiters_leave_no_entry() {
        let locks = private();
        let held = locks.lock("a").await.unwrap();
        assert!(tokio::time::timeout(SHORT, locks.lock("a")).await.is_err());
        assert_eq!(locks.slots()["a"].users, 1, "the timed-out waiter left");

        let mut woken = Box::pin(locks.lock("a"));
        assert!(tokio::time::timeout(SHORT, &mut woken).await.is_err());
        assert_eq!(locks.slots()["a"].users, 2);
        drop(held);
        drop(woken);
        assert!(locks.slots().is_empty(), "a woken, cancelled waiter left");
    }
}
