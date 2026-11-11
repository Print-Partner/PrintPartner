//! GRE-410 (T-28-streams): durable job states reach clients only as the
//! contract's five statuses, and an interrupted attempt reports an explicit
//! `error` outcome after restart. With `desktop_rebuild`, the same outcome
//! reaches a fresh subscription, a lagged subscriber's polling fallback and
//! never another tenant, with a bounded registry.
include!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/src/claimed_attempt.rs"
));
use pp_storage::{
    Limits, WriterOwner,
    auth::{AuthPolicy, FirstUserTenant, RegistrationPolicy, SessionTenantPolicy},
    jobs::*,
};
use std::{path::PathBuf, sync::atomic::AtomicBool, time::Duration};

const WAIT: Duration = Duration::from_secs(5);
/// `JobStatus` in web/packages/contracts/src/index.ts.
const CONTRACT_STATUSES: [&str; 5] = ["pending", "running", "done", "error", "cancelled"];

fn directory() -> PathBuf {
    let path = std::env::temp_dir().join(format!(
        "pp-t28-streams-{}",
        hex::encode(rand::random::<[u8; 12]>())
    ));
    std::fs::create_dir_all(&path).unwrap();
    path
}

fn policy() -> AuthPolicy {
    AuthPolicy {
        registration: RegistrationPolicy::Open,
        session_tenant: SessionTenantPolicy::AccountTenant,
        first_user: FirstUserTenant::NewUser,
    }
}

fn admission() -> WorkerAdmission {
    WorkerAdmission {
        kinds: JobKind::ALL.into_iter().map(|kind| (kind, 1)).collect(),
        total: 4,
        per_resource: 1,
        lease_seconds: 60,
    }
}

fn owner_call(owner: &WriterOwner, operation: UserOperation) -> JobRecord {
    let outcome = owner
        .jobs(policy())
        .unwrap()
        .submit(
            Credential::PhysicalOwner(owner.job_physical_owner()),
            operation,
            &AtomicBool::new(false),
            WAIT,
        )
        .unwrap()
        .receive()
        .unwrap();
    let Outcome::Job(job, _) = outcome else {
        panic!("job expected")
    };
    job
}

fn printer_upload() -> Payload {
    Payload::PrinterUpload {
        printer_id: "printer-a".into(),
        artifact_path: "exports/candidate.gcode".into(),
        filename: "candidate.gcode".into(),
        start: true,
        profile_id: None,
        host_name: None,
        checkoff_units: vec![],
        unlabeled_names: vec![],
    }
}

fn intent(operation: EffectOperation, target: &str) -> EffectIntent {
    EffectIntent {
        operation,
        basis_hash: "a".repeat(64),
        content_hash: "b".repeat(64),
        target: target.into(),
    }
}

/// A restart recovers every attempt that was in flight. One without effects
/// goes back to `pending`; one with an unconfirmed printer effect becomes an
/// explicit `error` that names what to check. No other status string appears.
#[test]
fn ticket_t_28_streams_restart_maps_interrupted_attempts_to_contract_statuses() {
    let path = directory();
    let (owner, _) = WriterOwner::open(&path, Limits::default()).unwrap();
    let worker = owner.job_worker(admission()).unwrap();
    let enqueue = |key: &str, payload| {
        owner_call(
            &owner,
            UserOperation::Enqueue {
                key: key.into(),
                payload_version: 1,
                payload,
            },
        )
        .job_id
    };
    let idle = enqueue("idle", Payload::CheckSourceUpdates {});
    let claimed_attempt!(claimed, _idle_lease) = worker.claim().unwrap().unwrap();
    assert_eq!(claimed.job_id, idle);
    let effect = enqueue("effect", printer_upload());
    let claimed_attempt!(claimed, mut lease) = worker.claim().unwrap().unwrap();
    assert_eq!(claimed.job_id, effect);
    worker
        .update(
            &mut lease,
            WorkerOperation::BeginEffect(intent(EffectOperation::PrinterUpload, "printer-a")),
        )
        .unwrap();
    let get = |owner: &WriterOwner, id: &str| {
        owner_call(owner, UserOperation::Get { job_id: id.into() }).snapshot()
    };
    let before = get(&owner, &effect);
    assert_eq!(
        (before.status, before.message),
        ("running", "Operation in progress")
    );
    owner.shutdown().unwrap();

    let (owner, _) = WriterOwner::open(&path, Limits::default()).unwrap();
    let requeued = get(&owner, &idle);
    assert_eq!(
        (
            requeued.status,
            requeued.error,
            requeued.finished_at.as_deref()
        ),
        ("pending", None, None)
    );
    let interrupted = get(&owner, &effect);
    assert_eq!(
        (interrupted.status, interrupted.message, interrupted.error),
        (
            "error",
            "Check the printer or Spoolman outcome before retrying",
            Some("Check the printer or Spoolman outcome before retrying"),
        )
    );
    for snapshot in [requeued, interrupted] {
        assert!(
            CONTRACT_STATUSES.contains(&snapshot.status),
            "{}",
            snapshot.status
        );
    }
    owner.shutdown().unwrap();
    std::fs::remove_dir_all(path).unwrap();
}

#[cfg(feature = "desktop_rebuild")]
mod rebuild {
    use super::*;
    use pp_storage::auth::{self, Secret, rebuild::AuthLifetimes};
    use std::sync::{Arc, atomic::AtomicBool};

    enum Event {
        Snapshot(Box<JobSnapshot>),
        Closed(StreamClose),
        Pending,
    }

    fn next(subscription: &JobSubscription, wait: Duration) -> Event {
        match subscription
            .prepare(wait, |snapshot| {
                Ok::<_, std::convert::Infallible>(snapshot.clone())
            })
            .unwrap()
        {
            PreparedEvent::Frame(frame) => {
                let mut delivered = None;
                assert_eq!(
                    subscription.admit(frame, |snapshot| {
                        delivered = Some(snapshot);
                        NonblockingSend::Accepted
                    }),
                    FrameAdmission::Admitted
                );
                Event::Snapshot(Box::new(delivered.unwrap()))
            }
            PreparedEvent::Closed(close) => Event::Closed(close),
            PreparedEvent::Pending => Event::Pending,
        }
    }

    /// Drains a stream that must end on its own: every snapshot, then the close.
    fn drain(subscription: &JobSubscription) -> (Vec<JobSnapshot>, StreamClose) {
        let mut snapshots = Vec::new();
        loop {
            match next(subscription, WAIT) {
                Event::Snapshot(snapshot) => snapshots.push(*snapshot),
                Event::Closed(close) => return (snapshots, close),
                Event::Pending => panic!("stream did not close"),
            }
        }
    }

    fn json(snapshot: &JobSnapshot) -> serde_json::Value {
        serde_json::to_value(snapshot).unwrap()
    }

    fn session(token: &str) -> Credential {
        Credential::Session(Secret::new(token.to_owned()))
    }

    fn register(owner: &WriterOwner, email: &str) -> String {
        let outcome = owner
            .rebuild_auth_with_policy(policy(), AuthLifetimes::default())
            .unwrap()
            .submit(
                auth::Request::Register {
                    email: email.into(),
                    display_name: "Streams".into(),
                    password: Secret::new("long-test-password".into()),
                },
                Arc::new(AtomicBool::new(false)),
                WAIT,
            )
            .unwrap()
            .recv()
            .unwrap()
            .unwrap();
        let auth::Outcome::Session { token, .. } = outcome else {
            panic!("session expected")
        };
        token.expose().to_owned()
    }

    fn enqueue(owner: &WriterOwner, token: &str, key: &str) -> String {
        let outcome = owner
            .jobs(policy())
            .unwrap()
            .submit(
                session(token),
                UserOperation::Enqueue {
                    key: key.into(),
                    payload_version: 1,
                    payload: Payload::CheckSourceUpdates {},
                },
                &AtomicBool::new(false),
                WAIT,
            )
            .unwrap()
            .receive()
            .unwrap();
        let Outcome::Job(job, _) = outcome else {
            panic!("job expected")
        };
        job.job_id
    }

    fn subscribe(
        owner: &WriterOwner,
        token: &str,
        id: &str,
    ) -> Result<JobSubscription, JobAccessFailure> {
        owner
            .jobs(policy())
            .unwrap()
            .subscribe(session(token), id.into(), &AtomicBool::new(false), WAIT)
            .map_err(|error| *error.downcast_ref::<JobAccessFailure>().unwrap())
    }

    fn poll(owner: &WriterOwner, token: &str, id: &str) -> Result<JobSnapshot, JobAccessFailure> {
        owner
            .jobs(policy())
            .unwrap()
            .read_public(session(token), id.into(), &AtomicBool::new(false), WAIT)
            .map_err(|error| *error.downcast_ref::<JobAccessFailure>().unwrap())
    }

    fn worker(owner: &WriterOwner) -> ServerWorkerClient {
        owner
            .job_worker_with_policy(
                policy(),
                WorkerAdmission {
                    kinds: vec![(JobKind::CheckSourceUpdates, 1)],
                    total: 1,
                    per_resource: 1,
                    lease_seconds: 60,
                },
            )
            .unwrap()
    }

    /// A live stream closes as the owner stops. After restart, each fresh
    /// subscription gets the interrupted outcome exactly once and closes, and
    /// polling reports the same snapshot.
    #[test]
    fn ticket_t_28_streams_resubscribe_after_restart_reports_interrupted_outcome_once() {
        let path = directory();
        let (owner, _) = WriterOwner::open(&path, Limits::default()).unwrap();
        let token = register(&owner, "restart@example.com");
        let id = enqueue(&owner, &token, "restart");
        let live = subscribe(&owner, &token, &id).unwrap();
        let worker = worker(&owner);
        let claimed_attempt!(_, mut lease) = worker.claim().unwrap().unwrap();
        worker
            .update(
                &mut lease,
                WorkerOperation::BeginEffect(intent(EffectOperation::SourceRefresh, "source:*")),
            )
            .unwrap();
        let seen = (0..3)
            .map(|_| match next(&live, WAIT) {
                Event::Snapshot(snapshot) => (snapshot.status, snapshot.message),
                _ => panic!("snapshot expected"),
            })
            .collect::<Vec<_>>();
        assert_eq!(
            seen,
            [
                ("pending", "Waiting for a compatible worker"),
                ("running", "Running"),
                ("running", "Operation in progress"),
            ]
        );
        owner.shutdown().unwrap();
        assert!(matches!(
            next(&live, Duration::ZERO),
            Event::Closed(StreamClose::OwnerStopped)
        ));

        let (owner, _) = WriterOwner::open(&path, Limits::default()).unwrap();
        let polled = poll(&owner, &token, &id).unwrap();
        assert_eq!(
            (polled.status, polled.error),
            ("error", Some("Check the previous outcome before retrying"))
        );
        for _ in 0..2 {
            let (snapshots, close) = drain(&subscribe(&owner, &token, &id).unwrap());
            assert_eq!(close, StreamClose::Finished);
            assert_eq!(
                snapshots.iter().map(json).collect::<Vec<_>>(),
                [json(&polled)]
            );
        }
        owner.shutdown().unwrap();
        std::fs::remove_dir_all(path).unwrap();
    }

    /// A subscriber that falls 16 commits behind closes as lagged instead of
    /// buffering more. Polling then reports the terminal outcome, and a
    /// resubscription delivers it once.
    #[test]
    fn ticket_t_28_streams_lagged_subscriber_falls_back_to_polling_terminal_outcome() {
        let path = directory();
        let (owner, _) = WriterOwner::open(&path, Limits::default()).unwrap();
        let token = register(&owner, "lag@example.com");
        let id = enqueue(&owner, &token, "lag");
        let slow = subscribe(&owner, &token, &id).unwrap();
        let worker = worker(&owner);
        let claimed_attempt!(_, mut lease) = worker.claim().unwrap().unwrap();
        for progress in 1..=16 {
            worker
                .update(&mut lease, WorkerOperation::Progress(progress))
                .unwrap();
        }
        worker
            .update(&mut lease, WorkerOperation::Finish(None))
            .unwrap();
        assert!(matches!(
            next(&slow, WAIT),
            Event::Closed(StreamClose::Lagged)
        ));
        let polled = poll(&owner, &token, &id).unwrap();
        assert_eq!((polled.status, polled.progress), ("done", Some(100)));
        let (snapshots, close) = drain(&subscribe(&owner, &token, &id).unwrap());
        assert_eq!(close, StreamClose::Finished);
        assert_eq!(
            snapshots.iter().map(json).collect::<Vec<_>>(),
            [json(&polled)]
        );
        owner.shutdown().unwrap();
        std::fs::remove_dir_all(path).unwrap();
    }

    /// Another tenant can neither subscribe to nor poll the job, and a revoked
    /// session's stream closes before any queued snapshot.
    #[test]
    fn ticket_t_28_streams_foreign_tenant_and_revoked_session_get_nothing() {
        let path = directory();
        let (owner, _) = WriterOwner::open(&path, Limits::default()).unwrap();
        let token = register(&owner, "owner@example.com");
        let foreign = register(&owner, "foreign@example.com");
        let id = enqueue(&owner, &token, "tenant");
        assert_eq!(
            subscribe(&owner, &foreign, &id).err(),
            Some(JobAccessFailure::NotFound)
        );
        assert_eq!(
            poll(&owner, &foreign, &id).err(),
            Some(JobAccessFailure::NotFound)
        );

        let stream = subscribe(&owner, &token, &id).unwrap();
        let logout = owner
            .rebuild_auth_with_policy(policy(), AuthLifetimes::default())
            .unwrap()
            .submit(
                auth::Request::Logout {
                    token: Secret::new(token.clone()),
                },
                Arc::new(AtomicBool::new(false)),
                WAIT,
            )
            .unwrap()
            .recv()
            .unwrap()
            .unwrap();
        assert!(matches!(logout, auth::Outcome::Changed(true)));
        assert!(matches!(
            next(&stream, WAIT),
            Event::Closed(StreamClose::Revoked)
        ));
        owner.shutdown().unwrap();
        std::fs::remove_dir_all(path).unwrap();
    }

    /// Live streams are capped at 128. The next one is refused, and a dropped
    /// stream frees its slot.
    #[test]
    fn ticket_t_28_streams_subscription_registry_is_bounded() {
        let path = directory();
        let (owner, _) = WriterOwner::open(&path, Limits::default()).unwrap();
        let token = register(&owner, "bounded@example.com");
        let id = enqueue(&owner, &token, "bounded");
        let mut live = (0..128)
            .map(|_| subscribe(&owner, &token, &id).unwrap())
            .collect::<Vec<_>>();
        assert_eq!(
            subscribe(&owner, &token, &id).err(),
            Some(JobAccessFailure::Capacity)
        );
        live.pop();
        assert!(subscribe(&owner, &token, &id).is_ok());
        owner.shutdown().unwrap();
        std::fs::remove_dir_all(path).unwrap();
    }
}
