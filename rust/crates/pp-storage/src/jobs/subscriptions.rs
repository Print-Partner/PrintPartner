use super::{Credential, JobAccessFailure, JobRecord, JobSnapshot};
use crate::auth::{self, AuthPolicy, StreamAuthority};
use anyhow::{Result, ensure};
use rusqlite::{Connection, TransactionBehavior};
use std::{
    cell::RefCell,
    collections::{HashMap, VecDeque},
    sync::{Arc, Condvar, Mutex, PoisonError, Weak},
    time::{Duration, Instant},
};

const MAX_SUBSCRIPTIONS: usize = 128;
const MAILBOX_CAPACITY: usize = 16;

thread_local! {
    static COLLECTOR: RefCell<Option<Collector>> = const { RefCell::new(None) };
}

struct Collector {
    changes: Vec<CommittedJobView>,
    committed: bool,
}

pub(crate) fn begin_collection() {
    COLLECTOR.with(|collector| {
        *collector.borrow_mut() = Some(Collector {
            changes: Vec::new(),
            committed: false,
        });
    });
}

pub(crate) fn mark_committed() {
    COLLECTOR.with(|collector| {
        if let Some(collector) = collector.borrow_mut().as_mut() {
            collector.committed = true;
        }
    });
}

pub(crate) fn record(job: &JobRecord) {
    COLLECTOR.with(|collector| {
        if let Some(collector) = collector.borrow_mut().as_mut()
            && !job.internal_only()
        {
            collector.changes.push(CommittedJobView {
                tenant: job.tenant.clone(),
                snapshot: Arc::new(job.snapshot()),
            });
        }
    });
}

pub(crate) fn finish_collection() -> Vec<CommittedJobView> {
    COLLECTOR.with(|collector| {
        let Some(collector) = collector.borrow_mut().take() else {
            return Vec::new();
        };
        if !collector.committed {
            return Vec::new();
        }
        let changes = collector.changes;
        let mut final_by_job = HashMap::new();
        let mut order = Vec::new();
        for change in changes {
            if !final_by_job.contains_key(&change.snapshot.job_id) {
                order.push(change.snapshot.job_id.clone());
            }
            final_by_job.insert(change.snapshot.job_id.clone(), change);
        }
        order
            .into_iter()
            .filter_map(|id| final_by_job.remove(&id))
            .collect()
    })
}

pub(crate) struct CommittedJobView {
    tenant: String,
    snapshot: Arc<JobSnapshot>,
}

#[derive(Clone)]
pub struct JobSubscription {
    cell: Arc<SubscriptionCell>,
}

pub struct PreparedFrame<T> {
    cell: Arc<SubscriptionCell>,
    snapshot: Arc<JobSnapshot>,
    payload: T,
}

pub enum PreparedEvent<T> {
    Frame(PreparedFrame<T>),
    Closed(StreamClose),
    Pending,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NonblockingSend {
    Accepted,
    Disconnected,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FrameAdmission {
    Admitted,
    Closed(StreamClose),
    Disconnected,
    Superseded,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StreamClose {
    Finished,
    Revoked,
    Expired,
    Lagged,
    OwnerStopped,
}

struct SubscriptionCell {
    authority_gate: Arc<Mutex<()>>,
    slot: Mutex<StreamSlot>,
    changed: Condvar,
}

struct StreamSlot {
    credential: Credential,
    policy: AuthPolicy,
    identity: auth::StreamAuthorityIdentity,
    tenant: String,
    subject: String,
    expires_at: Option<i64>,
    job_id: String,
    queue: VecDeque<Arc<JobSnapshot>>,
    last_version: i64,
    close: Option<StreamClose>,
}

pub(crate) struct JobSubscriptions {
    authority_gate: Arc<Mutex<()>>,
    registry: Mutex<SubscriptionRegistry>,
}

enum SubscriptionRegistry {
    Running(Vec<Weak<SubscriptionCell>>),
    Stopped,
}

impl SubscriptionRegistry {
    fn accepting_cells(
        &mut self,
    ) -> std::result::Result<&mut Vec<Weak<SubscriptionCell>>, JobAccessFailure> {
        match self {
            Self::Running(cells) => Ok(cells),
            Self::Stopped => Err(JobAccessFailure::Stopped),
        }
    }

    fn take_on_stop(&mut self) -> Vec<Weak<SubscriptionCell>> {
        match std::mem::replace(self, Self::Stopped) {
            Self::Running(cells) => cells,
            Self::Stopped => Vec::new(),
        }
    }
}

impl JobSubscriptions {
    pub(crate) fn new() -> Self {
        Self {
            authority_gate: Arc::new(Mutex::new(())),
            registry: Mutex::new(SubscriptionRegistry::Running(Vec::new())),
        }
    }

    pub(crate) fn authority_change<T>(&self, change: impl FnOnce() -> T) -> T {
        let _gate = self
            .authority_gate
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        change()
    }

    pub(crate) fn register(
        &self,
        credential: Credential,
        authority: StreamAuthority,
        policy: AuthPolicy,
        snapshot: JobSnapshot,
    ) -> Result<JobSubscription> {
        let mut registry = self.registry.lock().unwrap_or_else(PoisonError::into_inner);
        let cells = registry.accepting_cells()?;
        cells.retain(|cell| cell.strong_count() > 0);
        ensure!(cells.len() < MAX_SUBSCRIPTIONS, JobAccessFailure::Capacity);
        let terminal = matches!(snapshot.status, "done" | "error" | "cancelled");
        let version = snapshot.version;
        let cell = Arc::new(SubscriptionCell {
            authority_gate: self.authority_gate.clone(),
            slot: Mutex::new(StreamSlot {
                credential,
                policy,
                identity: authority.identity,
                tenant: authority.tenant,
                subject: authority.subject,
                expires_at: authority.expires_at,
                job_id: snapshot.job_id.clone(),
                queue: VecDeque::from([Arc::new(snapshot)]),
                last_version: version,
                close: terminal.then_some(StreamClose::Finished),
            }),
            changed: Condvar::new(),
        });
        cells.push(Arc::downgrade(&cell));
        Ok(JobSubscription { cell })
    }

    pub(crate) fn publish(&self, changes: Vec<CommittedJobView>) {
        if changes.is_empty() {
            return;
        }
        let mut registry = self.registry.lock().unwrap_or_else(PoisonError::into_inner);
        let SubscriptionRegistry::Running(cells) = &mut *registry else {
            return;
        };
        cells.retain(|cell| cell.strong_count() > 0);
        for weak in cells.iter() {
            let Some(cell) = weak.upgrade() else {
                continue;
            };
            let mut slot = cell.slot.lock().unwrap_or_else(PoisonError::into_inner);
            if slot.close.is_some() && slot.close != Some(StreamClose::Finished) {
                continue;
            }
            for change in &changes {
                if change.tenant != slot.tenant || change.snapshot.job_id != slot.job_id {
                    continue;
                }
                if change.snapshot.version <= slot.last_version {
                    continue;
                }
                if slot.queue.len() >= MAILBOX_CAPACITY {
                    slot.queue.clear();
                    slot.close = Some(StreamClose::Lagged);
                    break;
                }
                slot.last_version = change.snapshot.version;
                slot.queue.push_back(change.snapshot.clone());
                if matches!(change.snapshot.status, "done" | "error" | "cancelled") {
                    slot.close = Some(StreamClose::Finished);
                }
            }
            drop(slot);
            cell.changed.notify_all();
        }
    }

    pub(crate) fn revalidate(&self, connection: &mut Connection) -> Result<()> {
        let mut registry = self.registry.lock().unwrap_or_else(PoisonError::into_inner);
        let SubscriptionRegistry::Running(cells) = &mut *registry else {
            return Ok(());
        };
        cells.retain(|cell| cell.strong_count() > 0);
        if cells.is_empty() {
            return Ok(());
        }
        let tx = connection.transaction_with_behavior(TransactionBehavior::Deferred)?;
        for weak in cells.iter() {
            let Some(cell) = weak.upgrade() else {
                continue;
            };
            let mut slot = cell.slot.lock().unwrap_or_else(PoisonError::into_inner);
            if slot.close.is_some() && slot.close != Some(StreamClose::Finished) {
                continue;
            }
            let current = auth::resolve_stream_authority(&tx, &slot.credential, slot.policy, false);
            let valid = current.is_ok_and(|current| {
                current.identity == slot.identity
                    && current.tenant == slot.tenant
                    && current.subject == slot.subject
            });
            if !valid {
                slot.queue.clear();
                slot.close = Some(StreamClose::Revoked);
                drop(slot);
                cell.changed.notify_all();
            }
        }
        tx.commit()?;
        Ok(())
    }

    pub(crate) fn stop(&self) {
        let _gate = self
            .authority_gate
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        self.stop_under_authority_gate();
    }

    pub(crate) fn stop_under_authority_gate(&self) {
        let cells = {
            let mut registry = self.registry.lock().unwrap_or_else(PoisonError::into_inner);
            let SubscriptionRegistry::Running(cells) = &mut *registry else {
                return;
            };
            cells.retain(|cell| cell.strong_count() > 0);
            cells.iter().filter_map(Weak::upgrade).collect()
        };
        Self::close_cells(cells);
    }

    pub(crate) fn stop_owner(&self) {
        let _gate = self
            .authority_gate
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        let cells = {
            let mut registry = self.registry.lock().unwrap_or_else(PoisonError::into_inner);
            registry
                .take_on_stop()
                .into_iter()
                .filter_map(|cell| cell.upgrade())
                .collect()
        };
        Self::close_cells(cells);
    }

    fn close_cells(cells: Vec<Arc<SubscriptionCell>>) {
        for cell in cells {
            let mut slot = cell.slot.lock().unwrap_or_else(PoisonError::into_inner);
            slot.queue.clear();
            slot.close = Some(StreamClose::OwnerStopped);
            drop(slot);
            cell.changed.notify_all();
        }
    }
}

impl JobSubscription {
    pub fn prepare<T, E>(
        &self,
        wait: Duration,
        encode: impl FnOnce(&JobSnapshot) -> std::result::Result<T, E>,
    ) -> std::result::Result<PreparedEvent<T>, E> {
        self.prepare_with_clock(wait, encode, || {
            time::OffsetDateTime::now_utc().unix_timestamp()
        })
    }

    fn prepare_with_clock<T, E>(
        &self,
        wait: Duration,
        encode: impl FnOnce(&JobSnapshot) -> std::result::Result<T, E>,
        now: impl Fn() -> i64,
    ) -> std::result::Result<PreparedEvent<T>, E> {
        let deadline = Instant::now() + wait;
        let mut slot = self
            .cell
            .slot
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        loop {
            let now = now();
            if slot.expires_at.is_some_and(|expiry| expiry <= now) {
                slot.queue.clear();
                slot.close = Some(StreamClose::Expired);
            }
            if let Some(close) = slot.close.filter(|close| *close != StreamClose::Finished) {
                return Ok(PreparedEvent::Closed(close));
            }
            if let Some(snapshot) = slot.queue.front().cloned() {
                drop(slot);
                let payload = encode(&snapshot)?;
                return Ok(PreparedEvent::Frame(PreparedFrame {
                    cell: self.cell.clone(),
                    snapshot,
                    payload,
                }));
            }
            if let Some(close) = slot.close {
                return Ok(PreparedEvent::Closed(close));
            }
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return Ok(PreparedEvent::Pending);
            }
            let authority_remaining = slot
                .expires_at
                .map(|expiry| Duration::from_secs((expiry - now).max(0) as u64))
                .unwrap_or(remaining);
            slot = self
                .cell
                .changed
                .wait_timeout(slot, remaining.min(authority_remaining))
                .unwrap_or_else(PoisonError::into_inner)
                .0;
        }
    }

    pub fn admit<T>(
        &self,
        frame: PreparedFrame<T>,
        send: impl FnOnce(T) -> NonblockingSend,
    ) -> FrameAdmission {
        self.admit_with_clock(frame, send, || {
            time::OffsetDateTime::now_utc().unix_timestamp()
        })
    }

    fn admit_with_clock<T>(
        &self,
        frame: PreparedFrame<T>,
        send: impl FnOnce(T) -> NonblockingSend,
        now: impl FnOnce() -> i64,
    ) -> FrameAdmission {
        if !Arc::ptr_eq(&self.cell, &frame.cell) {
            return FrameAdmission::Superseded;
        }
        // A poisoned gate means an authority change panicked before revalidation
        // could revoke this stream, so delivery fails closed.
        let (_gate, poisoned) = match self.cell.authority_gate.lock() {
            Ok(gate) => (gate, false),
            Err(poison) => (poison.into_inner(), true),
        };
        let mut slot = self
            .cell
            .slot
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        if poisoned {
            slot.queue.clear();
            slot.close = Some(StreamClose::Revoked);
        }
        let now = now();
        if slot.expires_at.is_some_and(|expiry| expiry <= now) {
            slot.queue.clear();
            slot.close = Some(StreamClose::Expired);
        }
        if let Some(close) = slot.close.filter(|close| *close != StreamClose::Finished) {
            return FrameAdmission::Closed(close);
        }
        let Some(current) = slot.queue.front() else {
            return slot
                .close
                .map(FrameAdmission::Closed)
                .unwrap_or(FrameAdmission::Superseded);
        };
        if !Arc::ptr_eq(current, &frame.snapshot) {
            return FrameAdmission::Superseded;
        }
        match send(frame.payload) {
            NonblockingSend::Accepted => {
                slot.queue.pop_front();
                FrameAdmission::Admitted
            }
            NonblockingSend::Disconnected => FrameAdmission::Disconnected,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        Limits, WriterOwner,
        auth::{
            FirstUserTenant, Outcome, RegistrationPolicy, Request, Secret, SessionTenantPolicy,
        },
        jobs::{JobKind, Payload, PersistentState},
    };
    use std::{convert::Infallible, sync::atomic::AtomicBool};

    const POLICY: AuthPolicy = AuthPolicy {
        registration: RegistrationPolicy::Open,
        session_tenant: SessionTenantPolicy::AccountTenant,
        first_user: FirstUserTenant::NewUser,
    };

    fn job(id: &str, state_version: i64, state: PersistentState) -> JobRecord {
        serde_json::from_value(serde_json::json!({
            "job_id": id, "tenant": "tenant-a", "kind": "check-source-updates",
            "payload_version": 1, "payload": {"kind": "check-source-updates", "payload": {}},
            "state": state, "state_version": state_version, "attempt": 0, "generation": 0,
            "lease_until": null, "created_at": 1, "updated_at": 1, "finished_at": null,
            "cancel_requested": false, "progress": null, "effects": [], "result": null,
            "recovery": null,
        }))
        .unwrap()
    }

    fn subscribe_to(
        subscriptions: &JobSubscriptions,
        job: JobRecord,
        expires_at: Option<i64>,
    ) -> Result<JobSubscription> {
        let authority = StreamAuthority {
            tenant: "tenant-a".into(),
            subject: "user:a".into(),
            identity: auth::StreamAuthorityIdentity::Session("digest".into()),
            expires_at,
        };
        let credential = Credential::Session(Secret::new("token".into()));
        subscriptions.register(credential, authority, POLICY, job.snapshot())
    }

    fn subscribe(subscriptions: &JobSubscriptions) -> Result<JobSubscription> {
        subscribe_to(subscriptions, job("a", 1, PersistentState::Queued), None)
    }

    fn view(id: &str, state_version: i64, state: PersistentState) -> CommittedJobView {
        let job = job(id, state_version, state);
        CommittedJobView {
            tenant: job.tenant.clone(),
            snapshot: Arc::new(job.snapshot()),
        }
    }

    fn drain(subscription: &JobSubscription) -> (Vec<i64>, Option<StreamClose>) {
        let mut versions = Vec::new();
        loop {
            match subscription
                .prepare(Duration::ZERO, |snapshot| {
                    Ok::<_, Infallible>(snapshot.version)
                })
                .unwrap()
            {
                PreparedEvent::Frame(frame) => {
                    versions.push(frame.payload);
                    assert_eq!(
                        subscription.admit(frame, |_| NonblockingSend::Accepted),
                        FrameAdmission::Admitted
                    );
                }
                PreparedEvent::Closed(close) => return (versions, Some(close)),
                PreparedEvent::Pending => return (versions, None),
            }
        }
    }

    #[test]
    fn collection_publishes_the_last_committed_view_per_job_in_first_seen_order() {
        begin_collection();
        record(&job("b", 2, PersistentState::Running));
        record(&job("a", 2, PersistentState::Running));
        record(&job("b", 3, PersistentState::Succeeded));
        mark_committed();
        let changes = finish_collection();
        let seen: Vec<_> = changes
            .iter()
            .map(|change| (change.snapshot.job_id.as_str(), change.snapshot.version))
            .collect();
        assert_eq!(seen, [("b", 3), ("a", 2)]);

        begin_collection();
        record(&job("a", 4, PersistentState::Running));
        assert!(finish_collection().is_empty());
        record(&job("a", 5, PersistentState::Running));
        assert!(finish_collection().is_empty());
    }

    #[test]
    fn supplied_import_saves_never_reach_a_stream() {
        let import = |state_version| -> JobRecord {
            let mut job = job("import", state_version, PersistentState::Running);
            job.kind = JobKind::SuppliedSourceImport;
            job.payload = Payload::SuppliedSourceImport {
                project_id: 1,
                operation_key: "upload".into(),
                input_version: 1,
            };
            job
        };
        let subscriptions = JobSubscriptions::new();
        let subscription = subscribe_to(&subscriptions, import(1), None).unwrap();
        begin_collection();
        record(&import(2));
        mark_committed();
        subscriptions.publish(finish_collection());
        assert_eq!(drain(&subscription), (vec![1], None));
    }

    #[test]
    fn stream_delivers_newer_versions_and_finishes_once_terminal_frames_drain() {
        let subscriptions = JobSubscriptions::new();
        let subscription = subscribe(&subscriptions).unwrap();
        subscriptions.publish(vec![
            view("a", 2, PersistentState::Running),
            view("b", 3, PersistentState::Running),
            view("a", 2, PersistentState::Running),
            view("a", 1, PersistentState::Running),
        ]);
        assert_eq!(drain(&subscription), (vec![1, 2], None));
        subscriptions.publish(vec![view("a", 4, PersistentState::ReconciliationRequired)]);
        subscriptions.publish(vec![view("a", 5, PersistentState::Failed)]);
        assert_eq!(
            drain(&subscription),
            (vec![4, 5], Some(StreamClose::Finished))
        );

        let terminal = subscribe_to(
            &subscriptions,
            job("c", 7, PersistentState::Cancelled),
            None,
        )
        .unwrap();
        assert_eq!(drain(&terminal), (vec![7], Some(StreamClose::Finished)));
    }

    #[test]
    fn full_mailbox_closes_the_stream_as_lagged() {
        let subscriptions = JobSubscriptions::new();
        let subscription = subscribe(&subscriptions).unwrap();
        subscriptions.publish(
            (2..=MAILBOX_CAPACITY as i64)
                .map(|version| view("a", version, PersistentState::Running))
                .collect(),
        );
        subscriptions.publish(vec![view("a", 17, PersistentState::Running)]);
        assert_eq!(drain(&subscription), (vec![], Some(StreamClose::Lagged)));
    }

    #[test]
    fn admission_rejects_stale_frames_and_keeps_undelivered_frames() {
        let subscriptions = JobSubscriptions::new();
        let subscription = subscribe(&subscriptions).unwrap();
        let prepare = || match subscription
            .prepare(Duration::ZERO, |_| Ok::<_, Infallible>(()))
            .unwrap()
        {
            PreparedEvent::Frame(frame) => frame,
            _ => panic!("frame expected"),
        };
        assert_eq!(
            subscription.admit(prepare(), |_| NonblockingSend::Disconnected),
            FrameAdmission::Disconnected
        );
        let stale = prepare();
        assert_eq!(
            subscription.admit(prepare(), |_| NonblockingSend::Accepted),
            FrameAdmission::Admitted
        );
        assert_eq!(
            subscription.admit(stale, |_| NonblockingSend::Accepted),
            FrameAdmission::Superseded
        );
    }

    #[test]
    fn registry_caps_live_streams_and_reclaims_dropped_ones() {
        let subscriptions = JobSubscriptions::new();
        let mut live: Vec<_> = (0..MAX_SUBSCRIPTIONS)
            .map(|_| subscribe(&subscriptions).unwrap())
            .collect();
        let refused = subscribe(&subscriptions).err().unwrap();
        assert_eq!(
            refused.downcast_ref::<JobAccessFailure>(),
            Some(&JobAccessFailure::Capacity)
        );
        live.pop();
        assert!(subscribe(&subscriptions).is_ok());
    }

    #[test]
    fn persisted_key_expiry_blocks_prepared_frame_without_writer_traffic() {
        let root = std::env::temp_dir().join(format!(
            "pp-job-key-expiry-{}",
            hex::encode(rand::random::<[u8; 12]>())
        ));
        let (owner, _) = WriterOwner::open(&root, Limits::default()).unwrap();
        let auth = owner
            .rebuild_auth_with_policy(
                POLICY,
                auth::rebuild::AuthLifetimes::shorter(
                    Duration::from_secs(3600),
                    Some(Duration::from_secs(3600)),
                )
                .unwrap(),
            )
            .unwrap();
        let call = |request| {
            auth.submit(
                request,
                Arc::new(AtomicBool::new(false)),
                Duration::from_secs(5),
            )
            .unwrap()
            .recv()
            .unwrap()
            .unwrap()
        };
        let Outcome::Session { token, user } = call(Request::Register {
            email: "key-expiry@example.com".into(),
            display_name: "Key expiry".into(),
            password: Secret::new("long-test-password".into()),
        }) else {
            panic!("session expected")
        };
        let jobs = owner.jobs(POLICY).unwrap();
        let crate::jobs::Outcome::Job(job, _) = jobs
            .submit(
                Credential::Session(Secret::new(token.expose().to_owned())),
                crate::jobs::UserOperation::Enqueue {
                    key: "key-expiry".into(),
                    payload_version: 1,
                    payload: Payload::CheckSourceUpdates {},
                },
                &AtomicBool::new(false),
                Duration::from_secs(5),
            )
            .unwrap()
            .receive()
            .unwrap()
        else {
            panic!("job expected")
        };
        let Outcome::KeyCreated { key, info } = call(Request::CreateKey { session: token }) else {
            panic!("key expected")
        };
        let expiry = time::OffsetDateTime::parse(
            info.expires_at.as_deref().expect("persisted key expiry"),
            &time::format_description::well_known::Rfc3339,
        )
        .unwrap()
        .unix_timestamp();
        let subscription = jobs
            .subscribe(
                Credential::RoutedKey {
                    tenant: user.tenant_id,
                    key,
                },
                job.job_id,
                &AtomicBool::new(false),
                Duration::from_secs(5),
            )
            .unwrap();
        assert_eq!(
            subscription.cell.slot.lock().unwrap().expires_at,
            Some(expiry)
        );
        let PreparedEvent::Frame(initial) = subscription
            .prepare_with_clock(
                Duration::ZERO,
                |snapshot| Ok::<_, Infallible>(snapshot.clone()),
                || expiry - 1,
            )
            .unwrap()
        else {
            panic!("prepared frame expected")
        };
        assert_eq!(
            subscription.admit_with_clock(
                initial,
                |_| panic!("expired frame must not be sent"),
                || expiry
            ),
            FrameAdmission::Closed(StreamClose::Expired)
        );
        owner.shutdown().unwrap();
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn expiry_closes_the_stream_and_authority_changes_hold_the_admission_gate() {
        let subscriptions = JobSubscriptions::new();
        let expired = subscribe_to(
            &subscriptions,
            job("a", 1, PersistentState::Queued),
            Some(0),
        )
        .unwrap();
        assert_eq!(drain(&expired), (vec![], Some(StreamClose::Expired)));
        let subscription = subscribe(&subscriptions).unwrap();
        let gate_held =
            subscriptions.authority_change(|| subscription.cell.authority_gate.try_lock().is_err());
        assert!(gate_held);
    }

    #[test]
    fn admission_fails_closed_after_an_authority_change_panics() {
        let subscriptions = JobSubscriptions::new();
        let subscription = subscribe(&subscriptions).unwrap();
        let PreparedEvent::Frame(frame) = subscription
            .prepare(Duration::ZERO, |snapshot| {
                Ok::<_, Infallible>(snapshot.version)
            })
            .unwrap()
        else {
            panic!("frame expected")
        };
        let panicked = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            subscriptions.authority_change(|| panic!("logout committed, revalidation lost"))
        }));
        assert!(panicked.is_err());
        assert_eq!(
            subscription.admit(frame, |_| NonblockingSend::Accepted),
            FrameAdmission::Closed(StreamClose::Revoked)
        );
        assert_eq!(drain(&subscription), (vec![], Some(StreamClose::Revoked)));
    }

    #[test]
    fn revalidation_revokes_streams_whose_session_ended() {
        let root = std::env::temp_dir().join(format!(
            "pp-job-subscriptions-{}",
            hex::encode(rand::random::<[u8; 16]>())
        ));
        let (owner, _) = WriterOwner::open(&root, Limits::default()).unwrap();
        let client = owner.auth_with_policy(POLICY).unwrap();
        let run = |request| {
            client
                .submit(
                    request,
                    Arc::new(AtomicBool::new(false)),
                    Duration::from_secs(5),
                )
                .unwrap()
                .recv()
                .unwrap()
                .unwrap()
        };
        let Outcome::Session { token, .. } = run(Request::Register {
            email: "stream@example.com".into(),
            display_name: "Stream".into(),
            password: Secret::new("ordinary-password".into()),
        }) else {
            panic!("session expected")
        };
        let credential = || Credential::Session(Secret::new(token.expose().into()));
        let mut connection = Connection::open(root.join("print-partner.db")).unwrap();
        let tx = connection.transaction().unwrap();
        let current = auth::resolve_stream_authority(&tx, &credential(), POLICY, false).unwrap();
        drop(tx);
        let subscriptions = JobSubscriptions::new();
        let subscription = subscriptions
            .register(
                credential(),
                current,
                POLICY,
                job("a", 1, PersistentState::Queued).snapshot(),
            )
            .unwrap();
        subscriptions.revalidate(&mut connection).unwrap();
        assert_eq!(drain(&subscription), (vec![1], None));

        run(Request::Logout {
            token: Secret::new(token.expose().into()),
        });
        subscriptions.revalidate(&mut connection).unwrap();
        assert_eq!(drain(&subscription), (vec![], Some(StreamClose::Revoked)));
        drop((connection, client, owner));
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn fault_stop_closes_streams_and_owner_stop_also_refuses_registration() {
        let subscriptions = JobSubscriptions::new();
        let subscription = subscribe(&subscriptions).unwrap();
        subscriptions.stop();
        let stopped = (vec![], Some(StreamClose::OwnerStopped));
        assert_eq!(drain(&subscription), stopped);

        let subscription = subscribe(&subscriptions).unwrap();
        subscriptions.stop_owner();
        subscriptions.stop_owner();
        assert_eq!(drain(&subscription), stopped);
        let refused = subscribe(&subscriptions).err().unwrap();
        assert_eq!(
            refused.downcast_ref::<JobAccessFailure>(),
            Some(&JobAccessFailure::Stopped)
        );
    }
    #[test]
    fn stopping_old_job_streams_allows_restored_lower_versions() {
        let subscriptions = JobSubscriptions::new();
        let old = subscribe_to(&subscriptions, job("a", 9, PersistentState::Queued), None).unwrap();
        subscriptions.stop();
        assert_eq!(drain(&old), (vec![], Some(StreamClose::OwnerStopped)));
        let restored =
            subscribe_to(&subscriptions, job("a", 1, PersistentState::Queued), None).unwrap();
        assert_eq!(drain(&restored), (vec![1], None));
    }
}
