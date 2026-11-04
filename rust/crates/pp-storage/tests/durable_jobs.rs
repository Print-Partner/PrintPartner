include!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/src/claimed_attempt.rs"
));
use anyhow::Result;
use pp_storage::{
    Limits, WriterOwner,
    auth::{self, AuthPolicy, FirstUserTenant, RegistrationPolicy, Secret, SessionTenantPolicy},
    jobs::*,
};
use rusqlite::{Connection, ErrorCode, OpenFlags};
use std::os::unix::fs::DirBuilderExt;
use std::{
    path::PathBuf,
    sync::{Arc, Barrier, atomic::AtomicBool},
    thread,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

const CURRENT_SCHEMA: u64 = if cfg!(feature = "desktop_rebuild") {
    41
} else {
    35
};

#[path = "common/process.rs"]
mod child_process;
fn directory() -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "pp-durable-{}",
        hex::encode(rand::random::<[u8; 8]>())
    ));
    std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(&dir)
        .unwrap();
    dir
}
fn policy() -> AuthPolicy {
    AuthPolicy {
        registration: RegistrationPolicy::Open,
        session_tenant: SessionTenantPolicy::AccountTenant,
        first_user: FirstUserTenant::NewUser,
    }
}
fn fixture() -> (PathBuf, WriterOwner) {
    let path = directory();
    let (owner, ready) = WriterOwner::open(&path, Limits::default()).unwrap();
    assert_eq!(ready.version, CURRENT_SCHEMA);
    (path, owner)
}
/// Waits until the store's whole-second clock reaches the claimed lease's
/// expiry. Probing with a heartbeat would renew a lease that is still valid.
fn wait_for_lease_expiry(job: &JobRecord) {
    let expiry = u64::try_from(job.lease_until.unwrap()).unwrap();
    let deadline = Instant::now() + Duration::from_secs(5);
    while SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs()
        < expiry
    {
        assert!(Instant::now() < deadline, "lease did not expire");
        thread::sleep(Duration::from_millis(20));
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
/// A worker for session-enqueued jobs. With `desktop_rebuild` those jobs keep
/// their original credential, and claiming one needs the current policy.
#[cfg(feature = "desktop_rebuild")]
fn session_worker(owner: &WriterOwner) -> ServerWorkerClient {
    owner.job_worker_with_policy(policy(), admission()).unwrap()
}
#[cfg(not(feature = "desktop_rebuild"))]
fn session_worker(owner: &WriterOwner) -> ServerWorkerClient {
    owner.job_worker(admission()).unwrap()
}
fn call(owner: &WriterOwner, operation: UserOperation) -> Result<Outcome> {
    owner
        .jobs(policy())?
        .submit(
            Credential::PhysicalOwner(owner.job_physical_owner()),
            operation,
            &AtomicBool::new(false),
            Duration::from_secs(5),
        )?
        .receive()
}
fn job(outcome: Outcome) -> JobRecord {
    match outcome {
        Outcome::Job(job, _) => job,
        _ => panic!("Expected job"),
    }
}
fn enqueue(owner: &WriterOwner, key: &str, payload: Payload) -> JobRecord {
    job(call(
        owner,
        UserOperation::Enqueue {
            key: key.into(),
            payload_version: 1,
            payload,
        },
    )
    .unwrap())
}
fn get(owner: &WriterOwner, id: &str) -> JobRecord {
    job(call(owner, UserOperation::Get { job_id: id.into() }).unwrap())
}
fn printer(name: &str) -> Payload {
    Payload::PrinterUpload {
        printer_id: name.into(),
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
fn receipt(target: &str) -> ResultArtifact {
    ResultArtifact {
        receipt_id: "receipt-1".into(),
        content_hash: "b".repeat(64),
        target: target.into(),
    }
}
fn distinct_intent(operation: EffectOperation, target: &str, seed: u64) -> EffectIntent {
    EffectIntent {
        operation,
        basis_hash: format!("{:064x}", seed + 1),
        content_hash: format!("{:064x}", seed + 10_000),
        target: target.into(),
    }
}
fn distinct_receipt(intent: &EffectIntent, id: &str) -> ResultArtifact {
    ResultArtifact {
        receipt_id: id.into(),
        content_hash: intent.content_hash.clone(),
        target: intent.target.clone(),
    }
}
fn begin_and_confirm(
    worker: &ServerWorkerClient,
    lease: &mut AttemptLease,
    intent: EffectIntent,
    receipt_id: &str,
) -> ResultArtifact {
    let receipt = distinct_receipt(&intent, receipt_id);
    worker
        .update(lease, WorkerOperation::BeginEffect(intent))
        .unwrap();
    worker
        .update(lease, WorkerOperation::ConfirmEffect(receipt.clone()))
        .unwrap();
    receipt
}
fn reconciliations(owner: &WriterOwner, id: &str) -> Vec<ReconciliationRecord> {
    match call(
        owner,
        UserOperation::Reconciliations {
            job_id: id.into(),
            before_version: None,
            limit: 200,
        },
    )
    .unwrap()
    {
        Outcome::Reconciliations(records) => records,
        _ => panic!("reconciliations"),
    }
}
fn reconcile(
    owner: &WriterOwner,
    record: &JobRecord,
    decision: Decision,
    receipt: Option<ResultArtifact>,
) -> Result<Outcome> {
    call(
        owner,
        UserOperation::Reconcile {
            job_id: record.job_id.clone(),
            expected_version: record.state_version,
            expected_generation: record.generation,
            effect_hash: record.effects.last().unwrap().intent.content_hash.clone(),
            decision,
            receipt,
        },
    )
}
fn enqueue_start(owner: &WriterOwner, key: &str, parent_id: &str) -> Result<Outcome> {
    call(
        owner,
        UserOperation::Enqueue {
            key: key.into(),
            payload_version: 1,
            payload: Payload::PrinterStart(PrinterStartRequest::new(parent_id)),
        },
    )
}
fn uploaded_only_parent(owner: &WriterOwner, key: &str, printer_id: &str) -> JobRecord {
    enqueue(owner, key, printer(printer_id));
    let worker = owner.job_worker(admission()).unwrap();
    let claimed_attempt!(_, mut lease) = worker.claim().unwrap().unwrap();
    worker
        .update(
            &mut lease,
            WorkerOperation::BeginEffect(intent(EffectOperation::PrinterUpload, printer_id)),
        )
        .unwrap();
    let uncertain = worker.update(&mut lease, WorkerOperation::Fail).unwrap();
    job(reconcile(
        owner,
        &uncertain,
        Decision::ConfirmSucceeded,
        Some(receipt(printer_id)),
    )
    .unwrap())
}
fn denied_start_parent(owner: &WriterOwner, key: &str, printer_id: &str, seed: u64) -> JobRecord {
    enqueue(owner, key, printer(printer_id));
    let worker = owner.job_worker(admission()).unwrap();
    let claimed_attempt!(_, mut lease) = worker.claim().unwrap().unwrap();
    let upload = distinct_intent(EffectOperation::PrinterUpload, printer_id, seed);
    begin_and_confirm(
        &worker,
        &mut lease,
        upload,
        &format!("denied-start-upload-{seed}"),
    );
    let start = distinct_intent(EffectOperation::PrinterStart, printer_id, seed + 1);
    worker
        .update(&mut lease, WorkerOperation::BeginEffect(start))
        .unwrap();
    let uncertain = worker.update(&mut lease, WorkerOperation::Fail).unwrap();
    job(reconcile(owner, &uncertain, Decision::ConfirmNoEffect, None).unwrap())
}
fn readonly(path: &std::path::Path) -> Connection {
    Connection::open_with_flags(
        path.join("print-partner.db"),
        OpenFlags::SQLITE_OPEN_READ_ONLY,
    )
    .unwrap()
}
fn assert_invalid_job_skipped_on_restart(path: &std::path::Path, id: &str) {
    let document = || {
        readonly(path)
            .query_row(
                "SELECT document FROM durable_jobs WHERE id=?1",
                [id],
                |row| row.get::<_, String>(0),
            )
            .unwrap()
    };
    let before = document();
    let (owner, _) = WriterOwner::open(path, Limits::default()).unwrap();
    match call(&owner, UserOperation::List(JobListQuery::default())).unwrap() {
        Outcome::List(records) => assert!(records.is_empty()),
        _ => panic!("Expected list"),
    }
    assert!(call(&owner, UserOperation::Get { job_id: id.into() }).is_err());
    assert!(enqueue_start(&owner, "invalid-parent-child", id).is_err());
    owner.shutdown().unwrap();
    assert_eq!(document(), before);
}
fn register(owner: &WriterOwner, email: &str) -> (auth::User, String) {
    let result = owner
        .auth(FirstUserTenant::NewUser)
        .submit(
            auth::Request::Register {
                email: email.into(),
                display_name: "Job test".into(),
                password: Secret::new("long-test-password".into()),
            },
            Arc::new(AtomicBool::new(false)),
            Duration::from_secs(5),
        )
        .unwrap()
        .recv()
        .unwrap()
        .unwrap();
    match result {
        auth::Outcome::Session { user, token } => (user, token.expose().into()),
        _ => panic!("Session expected"),
    }
}
fn session_call(
    owner: &WriterOwner,
    policy: AuthPolicy,
    token: &str,
    operation: UserOperation,
) -> Result<Outcome> {
    owner
        .jobs(policy)?
        .submit(
            Credential::Session(Secret::new(token.into())),
            operation,
            &AtomicBool::new(false),
            Duration::from_secs(5),
        )?
        .receive()
}
#[test]
fn ticket_t_28_claims_duplicate_conflict_and_concurrent_claims() {
    let (path, owner) = fixture();
    let first = enqueue(&owner, "idempotent", Payload::CheckSourceUpdates {});
    let same = enqueue(&owner, "idempotent", Payload::CheckSourceUpdates {});
    assert_eq!(first.job_id, same.job_id);
    assert_eq!(first.state_version, same.state_version);
    assert!(
        call(
            &owner,
            UserOperation::Enqueue {
                key: "idempotent".into(),
                payload_version: 1,
                payload: Payload::ImportScan { project_id: 1 }
            }
        )
        .is_err()
    );
    assert_eq!(get(&owner, &first.job_id).state_version, 1);
    let worker = owner.job_worker(admission()).unwrap();
    let barrier = Arc::new(Barrier::new(8));
    let handles: Vec<_> = (0..8)
        .map(|_| {
            let worker = worker.clone();
            let barrier = barrier.clone();
            thread::spawn(move || {
                barrier.wait();
                worker.claim().unwrap()
            })
        })
        .collect();
    let mut claims: Vec<_> = handles
        .into_iter()
        .filter_map(|h| h.join().unwrap())
        .collect();
    assert_eq!(claims.len(), 1);
    let claimed_attempt!(_, mut lease) = claims.pop().unwrap();
    let mut stale = lease.clone();
    worker
        .update(&mut lease, WorkerOperation::Progress(25))
        .unwrap();
    assert!(
        worker
            .update(&mut stale, WorkerOperation::Heartbeat)
            .is_err()
    );
    assert!(
        worker
            .update(&mut stale, WorkerOperation::Finish(None))
            .is_err()
    );
    assert_eq!(get(&owner, &first.job_id).progress, Some(25));
    let other = owner.job_worker(admission()).unwrap();
    assert!(
        other
            .update(&mut lease, WorkerOperation::Heartbeat)
            .is_err()
    );
    let serialized = serde_json::to_string(&get(&owner, &first.job_id)).unwrap();
    assert!(!serialized.contains("fence"));
    owner.shutdown().unwrap();
    let raw = readonly(&path);
    assert_eq!(
        raw.query_row("SELECT COUNT(*) FROM durable_jobs", [], |r| r
            .get::<_, i64>(0))
            .unwrap(),
        1
    );
}
#[test]
fn ticket_t_28_claims_real_auth_tenant_policy_and_revocation() {
    let (_, owner) = fixture();
    let (a, token_a) = register(&owner, "a@example.com");
    let (_, token_b) = register(&owner, "b@example.com");
    let first = job(session_call(
        &owner,
        policy(),
        &token_a,
        UserOperation::Enqueue {
            key: "tenant-job".into(),
            payload_version: 1,
            payload: Payload::CheckSourceUpdates {},
        },
    )
    .unwrap());
    assert_eq!(first.tenant, a.tenant_id);
    assert!(
        session_call(
            &owner,
            policy(),
            &token_b,
            UserOperation::Get {
                job_id: first.job_id.clone()
            }
        )
        .is_err()
    );
    assert!(
        session_call(
            &owner,
            policy(),
            &token_b,
            UserOperation::Cancel {
                job_id: first.job_id.clone()
            }
        )
        .is_err()
    );
    let mut single = policy();
    single.session_tenant = SessionTenantPolicy::SingleAccountDefault;
    assert!(
        session_call(
            &owner,
            single,
            &token_a,
            UserOperation::List(JobListQuery {
                limit: 10,
                ..Default::default()
            })
        )
        .is_err()
    );
    let keys = owner
        .auth(FirstUserTenant::NewUser)
        .submit(
            auth::Request::CreateKey {
                session: Secret::new(token_a.clone()),
            },
            Arc::new(AtomicBool::new(false)),
            Duration::from_secs(5),
        )
        .unwrap()
        .recv()
        .unwrap()
        .unwrap();
    let key = match keys {
        auth::Outcome::KeyCreated { key, .. } => key.expose().to_owned(),
        _ => panic!("key expected"),
    };
    for (tenant, works) in [(a.tenant_id.as_str(), true), ("default", false)] {
        let result = owner
            .jobs(policy())
            .unwrap()
            .submit(
                Credential::RoutedKey {
                    tenant: tenant.into(),
                    key: Secret::new(key.clone()),
                },
                UserOperation::Get {
                    job_id: first.job_id.clone(),
                },
                &AtomicBool::new(false),
                Duration::from_secs(5),
            )
            .unwrap()
            .receive();
        assert_eq!(result.is_ok(), works);
    }
    owner
        .auth(FirstUserTenant::NewUser)
        .submit(
            auth::Request::Logout {
                token: Secret::new(token_a.clone()),
            },
            Arc::new(AtomicBool::new(false)),
            Duration::from_secs(5),
        )
        .unwrap()
        .recv()
        .unwrap()
        .unwrap();
    assert!(
        session_call(
            &owner,
            policy(),
            &token_a,
            UserOperation::Get {
                job_id: first.job_id
            }
        )
        .is_err()
    );
    owner.shutdown().unwrap();
}
#[test]
fn ticket_t_28_claims_single_account_maps_default_and_physical_owner_is_opaque() {
    let (_, owner) = fixture();
    let (_, token) = register(&owner, "single@example.com");
    let mut single = policy();
    single.session_tenant = SessionTenantPolicy::SingleAccountDefault;
    let record = job(session_call(
        &owner,
        single,
        &token,
        UserOperation::Enqueue {
            key: "single".into(),
            payload_version: 1,
            payload: Payload::CheckSourceUpdates {},
        },
    )
    .unwrap());
    assert_eq!(record.tenant, "default");
    let (_, other) = fixture();
    assert!(
        other
            .jobs(policy())
            .unwrap()
            .submit(
                Credential::PhysicalOwner(owner.job_physical_owner()),
                UserOperation::List(JobListQuery {
                    limit: 10,
                    ..Default::default()
                }),
                &AtomicBool::new(false),
                Duration::from_secs(5)
            )
            .unwrap()
            .receive()
            .is_err()
    );
    other.shutdown().unwrap();
    owner.shutdown().unwrap();
}
#[test]
fn ticket_t_28_claims_cancel_before_admission_running_and_effect() {
    let (_, owner) = fixture();
    let client = owner.jobs(policy()).unwrap();
    assert!(
        client
            .submit(
                Credential::PhysicalOwner(owner.job_physical_owner()),
                UserOperation::Enqueue {
                    key: "pre-cancel".into(),
                    payload_version: 1,
                    payload: Payload::CheckSourceUpdates {}
                },
                &AtomicBool::new(true),
                Duration::ZERO
            )
            .is_err()
    );
    let queued = enqueue(&owner, "queued", Payload::CheckSourceUpdates {});
    let cancelled = job(call(
        &owner,
        UserOperation::Cancel {
            job_id: queued.job_id,
        },
    )
    .unwrap());
    assert_eq!(cancelled.state, PersistentState::Cancelled);
    let worker = owner.job_worker(admission()).unwrap();
    assert!(worker.claim().unwrap().is_none());
    let running = enqueue(&owner, "running", Payload::CheckSourceUpdates {});
    let claimed_attempt!(_, mut lease) = worker.claim().unwrap().unwrap();
    assert_eq!(
        job(call(
            &owner,
            UserOperation::Cancel {
                job_id: running.job_id
            }
        )
        .unwrap())
        .state,
        PersistentState::Cancelled
    );
    assert!(
        worker
            .update(
                &mut lease,
                WorkerOperation::BeginEffect(intent(EffectOperation::SourceRefresh, "source:1"))
            )
            .is_err()
    );
    let effect = enqueue(&owner, "effect", printer("printer-a"));
    let claimed_attempt!(_, mut lease) = worker.claim().unwrap().unwrap();
    worker
        .update(
            &mut lease,
            WorkerOperation::BeginEffect(intent(EffectOperation::PrinterUpload, "printer-a")),
        )
        .unwrap();
    let cancelled = job(call(
        &owner,
        UserOperation::Cancel {
            job_id: effect.job_id,
        },
    )
    .unwrap());
    assert_eq!(cancelled.state, PersistentState::ReconciliationRequired);
    assert_eq!(cancelled.snapshot().status, "error");
    assert!(
        worker
            .update(
                &mut lease,
                WorkerOperation::ConfirmEffect(receipt("printer-a"))
            )
            .is_err()
    );
    owner.shutdown().unwrap();
}
#[test]
fn ticket_t_28_claims_kind_resource_capacity_and_source_intent() {
    let (_, owner) = fixture();
    #[cfg(feature = "desktop_rebuild")]
    assert_eq!(source(&owner.local_source_catalog(), "Fixture source"), 1);
    let mut limits = admission();
    for (_, cap) in &mut limits.kinds {
        *cap = 4;
    }
    let worker = owner.job_worker(limits.clone()).unwrap();
    enqueue(&owner, "pa1", printer("a"));
    enqueue(&owner, "pa2", printer("a"));
    enqueue(&owner, "pb1", printer("b"));
    let mut count = 0;
    while worker.claim().unwrap().is_some() {
        count += 1;
    }
    assert_eq!(count, 2);
    enqueue(&owner, "source1", Payload::ImportScan { project_id: 1 });
    enqueue(
        &owner,
        "source1-docs",
        Payload::ExtractSourceDocs { project_id: 1 },
    );
    assert!(worker.claim().unwrap().is_some());
    assert!(worker.claim().unwrap().is_none());
    limits.per_resource = 2;
    assert!(owner.job_worker(limits).is_err());
    owner.shutdown().unwrap();
}
#[test]
fn ticket_t_28_recovery_orderly_restart_and_subject_bound_reconciliation() {
    let (path, owner) = fixture();
    let record = enqueue(&owner, "uncertain", printer("printer-a"));
    let worker = owner.job_worker(admission()).unwrap();
    let claimed_attempt!(_, mut old) = worker.claim().unwrap().unwrap();
    worker
        .update(
            &mut old,
            WorkerOperation::BeginEffect(intent(
                EffectOperation::PrinterUploadAndStart,
                "printer-a",
            )),
        )
        .unwrap();
    owner.shutdown().unwrap();
    let (owner, _) = WriterOwner::open(&path, Limits::default()).unwrap();
    let recovered = get(&owner, &record.job_id);
    assert_eq!(recovered.state, PersistentState::ReconciliationRequired);
    assert_eq!(recovered.snapshot().status, "error");
    let worker = owner.job_worker(admission()).unwrap();
    assert!(worker.claim().unwrap().is_none());
    assert!(worker.update(&mut old, WorkerOperation::Heartbeat).is_err());
    let reconcile = |version, hash, receipt| UserOperation::Reconcile {
        job_id: record.job_id.clone(),
        expected_version: version,
        expected_generation: recovered.generation,
        effect_hash: hash,
        decision: Decision::ConfirmSucceeded,
        receipt,
    };
    assert!(
        call(
            &owner,
            reconcile(
                recovered.state_version - 1,
                "b".repeat(64),
                Some(receipt("printer-a"))
            )
        )
        .is_err()
    );
    assert!(
        call(
            &owner,
            reconcile(
                recovered.state_version,
                "c".repeat(64),
                Some(receipt("printer-a"))
            )
        )
        .is_err()
    );
    assert!(
        call(
            &owner,
            reconcile(recovered.state_version, "b".repeat(64), None)
        )
        .is_err()
    );
    let done = job(call(
        &owner,
        reconcile(
            recovered.state_version,
            "b".repeat(64),
            Some(receipt("printer-a")),
        ),
    )
    .unwrap());
    assert_eq!(done.state, PersistentState::Succeeded);
    assert!(done.snapshot().finished_at.unwrap().ends_with('Z'));
    let events = match call(
        &owner,
        UserOperation::History {
            job_id: done.job_id,
            before_version: None,
            limit: 200,
        },
    )
    .unwrap()
    {
        Outcome::History(events) => events,
        _ => panic!("history"),
    };
    assert!(
        events
            .iter()
            .any(|entry| entry.event.starts_with("reconciled:ConfirmSucceeded:"))
    );
    owner.shutdown().unwrap();
}
/// Cancellation cannot undo an acknowledged remote effect, for every
/// external-effect kind, before or after the effect is confirmed, and a
/// restart does not re-queue or resend it either.
#[test]
fn ticket_t_28_recovery_cancel_cannot_undo_acknowledged_effects() {
    let external: Vec<_> = JobKind::ALL
        .into_iter()
        .filter(|kind| kind.effect_class() != EffectClass::LocalStaging)
        .collect();
    assert_eq!(
        external,
        [JobKind::CheckSourceUpdates, JobKind::PrinterUpload],
        "a new external-effect kind needs a case below"
    );
    let cases = [
        (
            "printer-admitted",
            printer("printer-a"),
            EffectOperation::PrinterUpload,
            "printer-a",
            false,
        ),
        (
            "printer-confirmed",
            printer("printer-b"),
            EffectOperation::PrinterUploadAndStart,
            "printer-b",
            true,
        ),
        (
            "refresh-admitted",
            Payload::CheckSourceUpdates {},
            EffectOperation::SourceRefresh,
            "source:1",
            false,
        ),
        (
            "refresh-confirmed",
            Payload::CheckSourceUpdates {},
            EffectOperation::SourceRefresh,
            "source:2",
            true,
        ),
    ];
    // Each case gets its own store: a job awaiting reconciliation keeps its
    // kind's capacity, so a second job of the same kind would not be claimed.
    for (key, payload, operation, target, confirm) in cases {
        let (path, owner) = fixture();
        let record = enqueue(&owner, key, payload);
        let worker = owner.job_worker(admission()).unwrap();
        let claimed_attempt!(_, mut lease) = worker.claim().unwrap().unwrap();
        worker
            .update(
                &mut lease,
                WorkerOperation::BeginEffect(intent(operation, target)),
            )
            .unwrap();
        if confirm {
            worker
                .update(&mut lease, WorkerOperation::ConfirmEffect(receipt(target)))
                .unwrap();
        }
        let cancelled = job_after_cancel(&owner, &record.job_id);
        assert_eq!(
            cancelled.state,
            PersistentState::ReconciliationRequired,
            "{key}"
        );
        assert_eq!(
            cancelled.recovery.as_deref(),
            Some("Cancellation cannot undo an admitted effect"),
            "{key}"
        );
        assert_eq!(cancelled.snapshot().status, "error", "{key}");
        assert_eq!(cancelled.effects.len(), 1, "{key}");
        assert_eq!(cancelled.effects[0].intent.target, target, "{key}");
        assert_eq!(cancelled.effects[0].confirmed, confirm, "{key}");
        assert_eq!(
            cancelled.effects[0]
                .receipt
                .as_ref()
                .map(|r| r.target.as_str()),
            confirm.then_some(target),
            "{key}"
        );
        // The cancelled attempt can neither finish nor admit another effect.
        assert!(
            worker
                .update(&mut lease, WorkerOperation::ConfirmEffect(receipt(target)))
                .is_err(),
            "{key}"
        );
        assert!(
            worker
                .update(
                    &mut lease,
                    WorkerOperation::BeginEffect(intent(operation, "other"))
                )
                .is_err(),
            "{key}"
        );
        // A second cancel changes nothing.
        let again = job_after_cancel(&owner, &record.job_id);
        assert_eq!(again.state_version, cancelled.state_version, "{key}");
        owner.shutdown().unwrap();
        // A restart keeps the effect for reconciliation and resends nothing.
        let (owner, _) = WriterOwner::open(&path, Limits::default()).unwrap();
        let after = get(&owner, &record.job_id);
        assert_eq!(
            after.state,
            PersistentState::ReconciliationRequired,
            "{key}"
        );
        assert_eq!(after.attempt, cancelled.attempt, "{key}");
        assert_eq!(after.generation, cancelled.generation, "{key}");
        assert_eq!(after.recovery, cancelled.recovery, "{key}");
        assert_eq!(after.effects, cancelled.effects, "{key}");
        assert!(
            owner
                .job_worker(admission())
                .unwrap()
                .claim()
                .unwrap()
                .is_none(),
            "{key}"
        );
        owner.shutdown().unwrap();
    }
    // Cancelling before admission still ends cancelled with no effect, for
    // both external-effect kinds.
    for (key, payload, operation, target) in [
        (
            "printer-before-admission",
            printer("printer-c"),
            EffectOperation::PrinterUpload,
            "printer-c",
        ),
        (
            "refresh-before-admission",
            Payload::CheckSourceUpdates {},
            EffectOperation::SourceRefresh,
            "source:3",
        ),
    ] {
        let (_, owner) = fixture();
        let before = enqueue(&owner, key, payload);
        let worker = owner.job_worker(admission()).unwrap();
        let claimed_attempt!(_, mut lease) = worker.claim().unwrap().unwrap();
        let cancelled = job_after_cancel(&owner, &before.job_id);
        assert_eq!(cancelled.state, PersistentState::Cancelled, "{key}");
        assert!(
            cancelled.effects.is_empty() && cancelled.recovery.is_none(),
            "{key}"
        );
        assert!(
            worker
                .update(
                    &mut lease,
                    WorkerOperation::BeginEffect(intent(operation, target))
                )
                .is_err(),
            "{key}"
        );
        owner.shutdown().unwrap();
    }
}
fn job_after_cancel(owner: &WriterOwner, job_id: &str) -> JobRecord {
    job(call(
        owner,
        UserOperation::Cancel {
            job_id: job_id.into(),
        },
    )
    .unwrap())
}
#[test]
fn ticket_t_28_recovery_receipts_duplicate_effects_and_finish() {
    let (_, owner) = fixture();
    enqueue(&owner, "effects", printer("printer-a"));
    let worker = owner.job_worker(admission()).unwrap();
    let claimed_attempt!(_, mut lease) = worker.claim().unwrap().unwrap();
    assert!(
        worker
            .update(
                &mut lease,
                WorkerOperation::BeginEffect(intent(EffectOperation::PrinterStart, "printer-a"))
            )
            .is_err()
    );
    assert!(
        worker
            .update(
                &mut lease,
                WorkerOperation::BeginEffect(intent(EffectOperation::PrinterUpload, "foreign"))
            )
            .is_err()
    );
    worker
        .update(
            &mut lease,
            WorkerOperation::BeginEffect(intent(EffectOperation::PrinterUpload, "printer-a")),
        )
        .unwrap();
    assert!(
        worker
            .update(&mut lease, WorkerOperation::Finish(None))
            .is_err()
    );
    assert!(
        worker
            .update(
                &mut lease,
                WorkerOperation::ConfirmEffect(receipt("foreign"))
            )
            .is_err()
    );
    worker
        .update(
            &mut lease,
            WorkerOperation::ConfirmEffect(receipt("printer-a")),
        )
        .unwrap();
    assert!(
        worker
            .update(
                &mut lease,
                WorkerOperation::BeginEffect(intent(EffectOperation::PrinterUpload, "printer-a"))
            )
            .is_err()
    );
    worker
        .update(
            &mut lease,
            WorkerOperation::BeginEffect(intent(EffectOperation::PrinterStart, "printer-a")),
        )
        .unwrap();
    worker
        .update(
            &mut lease,
            WorkerOperation::ConfirmEffect(receipt("printer-a")),
        )
        .unwrap();
    worker
        .update(
            &mut lease,
            WorkerOperation::BeginEffect(intent(EffectOperation::SpoolmanDeduction, "spool:3")),
        )
        .unwrap();
    let uncertain = worker.update(&mut lease, WorkerOperation::Fail).unwrap();
    assert_eq!(uncertain.state, PersistentState::ReconciliationRequired);
    owner.shutdown().unwrap();
}
#[test]
fn ticket_t_28_claims_lost_reply_drain_and_backup_committed_job() {
    let (path, owner) = fixture();
    let pending = owner
        .jobs(policy())
        .unwrap()
        .submit(
            Credential::PhysicalOwner(owner.job_physical_owner()),
            UserOperation::Enqueue {
                key: "lost-reply".into(),
                payload_version: 1,
                payload: Payload::CheckSourceUpdates {},
            },
            &AtomicBool::new(false),
            Duration::from_secs(5),
        )
        .unwrap();
    drop(pending);
    let same = enqueue(&owner, "lost-reply", Payload::CheckSourceUpdates {});
    let backup = path.with_extension("backup.db");
    owner.backup(&backup).unwrap();
    let copy = Connection::open_with_flags(&backup, OpenFlags::SQLITE_OPEN_READ_ONLY).unwrap();
    assert_eq!(
        copy.query_row("SELECT id FROM durable_jobs", [], |r| r.get::<_, String>(0))
            .unwrap(),
        same.job_id
    );
    drop(copy);
    owner.shutdown().unwrap();
    let raw = readonly(&path);
    assert_eq!(
        raw.query_row("SELECT COUNT(*) FROM durable_job_keys", [], |r| r
            .get::<_, i64>(0))
            .unwrap(),
        1
    );
}
#[test]
fn ticket_t_28_recovery_retention_preserves_uncertainty_and_idempotency() {
    let (_, owner) = fixture();
    let worker = owner.job_worker(admission()).unwrap();
    for index in 0..5 {
        enqueue(
            &owner,
            &format!("done-{index}"),
            Payload::CheckSourceUpdates {},
        );
        let claimed_attempt!(_, mut lease) = worker.claim().unwrap().unwrap();
        worker
            .update(&mut lease, WorkerOperation::Finish(None))
            .unwrap();
    }
    let uncertain = enqueue(&owner, "uncertain", printer("p"));
    let claimed_attempt!(_, mut lease) = worker.claim().unwrap().unwrap();
    worker
        .update(
            &mut lease,
            WorkerOperation::BeginEffect(intent(EffectOperation::PrinterUpload, "p")),
        )
        .unwrap();
    worker.update(&mut lease, WorkerOperation::Fail).unwrap();
    assert_eq!(owner.retain_jobs(1, 1).unwrap(), 4);
    assert_eq!(
        get(&owner, &uncertain.job_id).state,
        PersistentState::ReconciliationRequired
    );
    let repeated = enqueue(&owner, "done-0", Payload::CheckSourceUpdates {});
    assert_eq!(repeated.state, PersistentState::Succeeded);
    owner.shutdown().unwrap();
}
#[test]
fn ticket_t_28_claims_invalid_payloads_fail_before_writes() {
    let (path, owner) = fixture();
    for payload in [
        Payload::ImportScan { project_id: 0 },
        Payload::ExportDirect3mf {
            profile_id: 1,
            tokens: vec![],
        },
        Payload::ExportDirect3mf {
            profile_id: 1,
            tokens: vec!["bad-token".into()],
        },
    ] {
        assert!(
            call(
                &owner,
                UserOperation::Enqueue {
                    key: "invalid".into(),
                    payload_version: 1,
                    payload
                }
            )
            .is_err()
        );
    }
    assert!(
        call(
            &owner,
            UserOperation::Enqueue {
                key: "version".into(),
                payload_version: 2,
                payload: Payload::CheckSourceUpdates {}
            }
        )
        .is_err()
    );
    owner.shutdown().unwrap();
    assert_eq!(
        readonly(&path)
            .query_row("SELECT COUNT(*) FROM durable_jobs", [], |r| r
                .get::<_, i64>(0))
            .unwrap(),
        0
    );
}
#[test]
fn ticket_t_28_claims_ordinary_transaction_error_rolls_back() {
    let (path, owner) = fixture();
    owner.shutdown().unwrap();
    let fixture = Connection::open(path.join("print-partner.db")).unwrap();
    fixture.execute_batch("CREATE TRIGGER reject_test_job BEFORE INSERT ON durable_job_keys WHEN NEW.key='reject' BEGIN SELECT RAISE(ABORT,'fixture constraint'); END;").unwrap();
    drop(fixture);
    let (owner, _) = WriterOwner::open(&path, Limits::default()).unwrap();
    assert!(
        call(
            &owner,
            UserOperation::Enqueue {
                key: "reject".into(),
                payload_version: 1,
                payload: Payload::CheckSourceUpdates {}
            }
        )
        .is_err()
    );
    owner.shutdown().unwrap();
    let raw = readonly(&path);
    for table in ["durable_jobs", "durable_job_keys", "durable_job_history"] {
        assert_eq!(
            raw.query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |r| r
                .get::<_, i64>(0))
                .unwrap(),
            0
        );
    }
}

#[test]
fn ticket_t_28_recovery_every_existing_kind_has_incomplete_intent_policy() {
    let token = format!("ppu_{}", "a".repeat(32));
    let payloads = vec![
        Payload::Sync {
            project_ids: Some(vec![1]),
        },
        Payload::ImportScan { project_id: 1 },
        Payload::ExtractSourceDocs { project_id: 1 },
        Payload::CheckSourceUpdates {},
        Payload::ExportStlPack {
            profile_id: 1,
            missing_only: false,
            group_by: GroupBy::ColorDir,
            unit_tokens: vec![token.clone()],
            filename_grouping: None,
        },
        Payload::ExportChecklistHtml { profile_id: 1 },
        Payload::ExportKitBundle {
            profile_id: 1,
            include_print_progress: true,
        },
        Payload::ExportAcceptedPlate3mf {
            profile_id: 1,
            expected_plate_revision_id: 2,
        },
        Payload::ExportDirect3mf {
            profile_id: 1,
            tokens: vec![token],
        },
        printer("test-printer"),
    ];
    // Supplied imports are never enqueued here and advance only through the
    // upload journal's owned phases.
    let kinds = JobKind::ALL
        .into_iter()
        .filter(|kind| kind.name() != "supplied-source-import")
        .collect::<Vec<_>>();
    assert_eq!(payloads.len(), kinds.len());
    for (payload, kind) in payloads.into_iter().zip(kinds) {
        assert_eq!(payload.kind(), kind);
        // Rebuild Sync effects belong to the batch journal, covered by source_sync tests.
        if cfg!(feature = "desktop_rebuild") && kind == JobKind::Sync {
            continue;
        }
        let (path, owner) = fixture();
        #[cfg(feature = "desktop_rebuild")]
        assert_eq!(source(&owner.local_source_catalog(), "Fixture source"), 1);
        let queued = enqueue(&owner, "kind-coverage", payload);
        let worker = owner.job_worker(admission()).unwrap();
        let claimed_attempt!(_, mut lease) = worker.claim().unwrap().unwrap();
        let (operation, target) = match kind {
            JobKind::PrinterUpload => (EffectOperation::PrinterUploadAndStart, "test-printer"),
            JobKind::Sync
            | JobKind::ImportScan
            | JobKind::ExtractSourceDocs
            | JobKind::CheckSourceUpdates => (EffectOperation::SourceRefresh, "source:1"),
            _ => (EffectOperation::LocalArtifact, "exports/candidate"),
        };
        worker
            .update(
                &mut lease,
                WorkerOperation::BeginEffect(intent(operation, target)),
            )
            .unwrap();
        owner.shutdown().unwrap();
        let (owner, _) = WriterOwner::open(&path, Limits::default()).unwrap();
        let current = get(&owner, &queued.job_id);
        assert_eq!(
            current.state,
            PersistentState::ReconciliationRequired,
            "{kind:?}"
        );
        assert_eq!(current.effects.len(), 1);
        assert_eq!(current.effects[0].attempt, 1);
        assert!(!current.effects[0].confirmed);
        assert!(
            owner
                .job_worker(admission())
                .unwrap()
                .claim()
                .unwrap()
                .is_none()
        );
        owner.shutdown().unwrap();
    }
}
#[test]
fn ticket_t_28_recovery_confirmed_local_candidate_requires_exact_receipt() {
    use sha2::{Digest, Sha256};
    let (path, owner) = fixture();
    let bytes = b"documented local artifact fixture";
    let hash = hex::encode(Sha256::digest(bytes));
    let target = "exports/candidate.txt";
    std::fs::write(path.join(target), bytes).unwrap();
    let queued = enqueue(
        &owner,
        "local-artifact",
        Payload::ExportChecklistHtml { profile_id: 1 },
    );
    let worker = owner.job_worker(admission()).unwrap();
    let claimed_attempt!(_, mut lease) = worker.claim().unwrap().unwrap();
    let effect = EffectIntent {
        operation: EffectOperation::LocalArtifact,
        basis_hash: "a".repeat(64),
        content_hash: hash.clone(),
        target: target.into(),
    };
    worker
        .update(&mut lease, WorkerOperation::BeginEffect(effect))
        .unwrap();
    let artifact = ResultArtifact {
        receipt_id: "fixture-receipt".into(),
        content_hash: hash.clone(),
        target: target.into(),
    };
    worker
        .update(&mut lease, WorkerOperation::ConfirmEffect(artifact.clone()))
        .unwrap();
    owner.shutdown().unwrap();
    let (owner, _) = WriterOwner::open(&path, Limits::default()).unwrap();
    let recovered = get(&owner, &queued.job_id);
    assert_eq!(recovered.state, PersistentState::ReconciliationRequired);
    assert!(recovered.effects[0].confirmed);
    assert_eq!(
        hex::encode(Sha256::digest(std::fs::read(path.join(target)).unwrap())),
        artifact.content_hash
    );
    let mut wrong = artifact.clone();
    wrong.content_hash = "c".repeat(64);
    let make = |receipt| UserOperation::Reconcile {
        job_id: queued.job_id.clone(),
        expected_version: recovered.state_version,
        expected_generation: recovered.generation,
        effect_hash: hash.clone(),
        decision: Decision::ConfirmSucceeded,
        receipt: Some(receipt),
    };
    assert!(call(&owner, make(wrong)).is_err());
    assert_eq!(
        job(call(&owner, make(artifact)).unwrap()).state,
        PersistentState::Succeeded
    );
    owner.shutdown().unwrap();
}
#[test]
fn ticket_t_28_recovery_abandon_keeps_uncertain_proof_and_resource_lock() {
    let (_, owner) = fixture();
    let queued = enqueue(&owner, "uncertain-abandon", printer("a"));
    let worker = owner.job_worker(admission()).unwrap();
    let claimed_attempt!(_, mut lease) = worker.claim().unwrap().unwrap();
    worker
        .update(
            &mut lease,
            WorkerOperation::BeginEffect(intent(EffectOperation::PrinterUpload, "a")),
        )
        .unwrap();
    let pending = worker.update(&mut lease, WorkerOperation::Fail).unwrap();
    let result = job(call(
        &owner,
        UserOperation::Reconcile {
            job_id: queued.job_id.clone(),
            expected_version: pending.state_version,
            expected_generation: pending.generation,
            effect_hash: "b".repeat(64),
            decision: Decision::Abandon,
            receipt: None,
        },
    )
    .unwrap());
    assert_eq!(result.state, PersistentState::ReconciliationRequired);
    owner.retain_jobs(1, 1).unwrap();
    assert_eq!(get(&owner, &queued.job_id).effects.len(), 1);
    enqueue(&owner, "another-a", printer("a"));
    assert!(worker.claim().unwrap().is_none());
    owner.shutdown().unwrap();
}
#[test]
fn ticket_t_28_claims_duplicate_concurrent_enqueue_and_no_handler_admission() {
    let (_, owner) = fixture();
    let client = owner.jobs(policy()).unwrap();
    let barrier = Arc::new(Barrier::new(8));
    let handles: Vec<_> = (0..8)
        .map(|_| {
            let client = client.clone();
            let credential = Credential::PhysicalOwner(owner.job_physical_owner());
            let barrier = barrier.clone();
            thread::spawn(move || {
                barrier.wait();
                job(client
                    .submit(
                        credential,
                        UserOperation::Enqueue {
                            key: "concurrent-intent".into(),
                            payload_version: 1,
                            payload: Payload::CheckSourceUpdates {},
                        },
                        &AtomicBool::new(false),
                        Duration::from_secs(5),
                    )
                    .unwrap()
                    .receive()
                    .unwrap())
                .job_id
            })
        })
        .collect();
    let ids: std::collections::HashSet<_> =
        handles.into_iter().map(|h| h.join().unwrap()).collect();
    assert_eq!(ids.len(), 1);
    let mut no_handlers = admission();
    no_handlers.kinds.clear();
    assert!(
        owner
            .job_worker(no_handlers)
            .unwrap()
            .claim()
            .unwrap()
            .is_none()
    );
    owner.shutdown().unwrap();
}
#[test]
fn ticket_t_28_claims_expired_lease_fences_old_worker() {
    let (_, owner) = fixture();
    let queued = enqueue(&owner, "expiry", Payload::CheckSourceUpdates {});
    let mut config = admission();
    config.lease_seconds = 1;
    let worker = owner.job_worker(config).unwrap();
    let claimed_attempt!(claimed, mut old) = worker.claim().unwrap().unwrap();
    wait_for_lease_expiry(&claimed);
    assert!(worker.update(&mut old, WorkerOperation::Heartbeat).is_err());
    let claimed_attempt!(next, mut current) = worker.claim().unwrap().unwrap();
    assert_eq!(next.job_id, queued.job_id);
    assert_eq!(next.attempt, 2);
    assert!(next.generation > 1);
    assert!(worker.update(&mut old, WorkerOperation::Fail).is_err());
    worker
        .update(&mut current, WorkerOperation::Finish(None))
        .unwrap();
    owner.shutdown().unwrap();
}
#[test]
fn ticket_t_28_claims_list_filters_pagination_and_history() {
    let (_, owner) = fixture();
    for id in 1..=3 {
        enqueue(
            &owner,
            &format!("profile-{id}"),
            Payload::ExportChecklistHtml { profile_id: id },
        );
    }
    let list = |query| match call(&owner, UserOperation::List(query)).unwrap() {
        Outcome::List(items) => items,
        _ => panic!("list"),
    };
    let first = list(JobListQuery {
        limit: 2,
        ..Default::default()
    });
    assert_eq!(first.len(), 2);
    let last = first.last().unwrap();
    let second = list(JobListQuery {
        limit: 2,
        before: Some((last.updated_at, last.job_id.clone())),
        ..Default::default()
    });
    assert_eq!(second.len(), 1);
    assert!(!first.iter().any(|job| job.job_id == second[0].job_id));
    let filtered = list(JobListQuery {
        profile_id: Some(2),
        status: Some("pending".into()),
        ..Default::default()
    });
    assert_eq!(filtered.len(), 1);
    let cancelled = job(call(
        &owner,
        UserOperation::Cancel {
            job_id: filtered[0].job_id.clone(),
        },
    )
    .unwrap());
    assert_eq!(
        list(JobListQuery {
            status: Some("cancelled".into()),
            ..Default::default()
        })
        .len(),
        1
    );
    assert!(
        list(JobListQuery {
            since: Some(cancelled.updated_at + 10),
            ..Default::default()
        })
        .is_empty()
    );
    owner.shutdown().unwrap();
}
#[test]
fn ticket_t_28_claims_schema35_corruption_and36_preserve_input_bytes() {
    for corruption in [
        "UPDATE app_settings SET value='36' WHERE tenant_id='default' AND key='schema_version'",
        "ALTER TABLE durable_jobs ADD COLUMN unintended TEXT",
        "UPDATE durable_jobs SET version=version+1",
    ] {
        let (path, owner) = fixture();
        enqueue(&owner, "retained", Payload::CheckSourceUpdates {});
        owner.shutdown().unwrap();
        let raw = Connection::open(path.join("print-partner.db")).unwrap();
        raw.execute_batch(corruption).unwrap();
        drop(raw);
        let db = path.join("print-partner.db");
        let before = std::fs::read(&db).unwrap();
        assert!(WriterOwner::open(&path, Limits::default()).is_err());
        assert_eq!(before, std::fs::read(&db).unwrap());
        assert!(!path.join(".desktop-owner.json").exists());
    }
}
#[test]
fn schema35_auxiliary_indexes_install_and_reopen() {
    let (path, owner) = fixture();
    owner.shutdown().unwrap();
    let raw = Connection::open(path.join("print-partner.db")).unwrap();
    raw.execute_batch(
        "DROP INDEX durable_jobs_printer_start_parent;
         DROP INDEX durable_job_keys_archived_printer_start_parent;",
    )
    .unwrap();
    drop(raw);
    let (owner, ready) = WriterOwner::open(&path, Limits::default()).unwrap();
    assert_eq!(ready.version, CURRENT_SCHEMA);
    assert_eq!(ready.previous_version, CURRENT_SCHEMA);
    owner.shutdown().unwrap();
    let raw = readonly(&path);
    let indexes = raw
        .prepare("SELECT name FROM sqlite_master WHERE type='index' AND name LIKE 'durable_%_printer_start_parent' ORDER BY name")
        .unwrap()
        .query_map([], |row| row.get::<_, String>(0))
        .unwrap()
        .collect::<rusqlite::Result<Vec<_>>>()
        .unwrap();
    assert_eq!(
        indexes,
        [
            "durable_job_keys_archived_printer_start_parent",
            "durable_jobs_printer_start_parent",
        ]
    );
    assert_eq!(
        raw.query_row(
            "SELECT value FROM app_settings WHERE tenant_id='default' AND key='schema_version'",
            [],
            |row| row.get::<_, String>(0),
        )
        .unwrap(),
        CURRENT_SCHEMA.to_string()
    );
    drop(raw);
    let (owner, ready) = WriterOwner::open(&path, Limits::default()).unwrap();
    assert_eq!(ready.version, CURRENT_SCHEMA);
    assert_eq!(ready.previous_version, CURRENT_SCHEMA);
    owner.shutdown().unwrap();
}
#[test]
fn ticket_t_28_claims_wire_job_kinds_match_existing_contract() {
    let names = JobKind::ALL.map(JobKind::name);
    #[cfg(feature = "desktop_rebuild")]
    assert_eq!(names[10..], ["supplied-source-import"]);
    assert_eq!(
        names[..10],
        [
            "sync",
            "import-scan",
            "extract-source-docs",
            "check-source-updates",
            "export-stl-pack",
            "export-checklist-html",
            "export-kit-bundle",
            "export-accepted-plate-3mf",
            "export-direct-3mf",
            "printer-upload"
        ]
    );
    for kind in JobKind::ALL {
        assert_eq!(
            serde_json::from_str::<JobKind>(&serde_json::to_string(&kind).unwrap()).unwrap(),
            kind
        );
    }
    assert!(
        serde_json::from_str::<Payload>(
            r#"{"kind":"import-scan","payload":{"project_id":1,"tenant_id":"foreign"}}"#
        )
        .is_err()
    );
}
#[test]
fn ticket_t_28_recovery_success_requires_effect_receipts() {
    let (_, owner) = fixture();
    enqueue(&owner, "no-fake-printer", printer("p"));
    let worker = owner.job_worker(admission()).unwrap();
    let claimed_attempt!(_, mut lease) = worker.claim().unwrap().unwrap();
    assert!(
        worker
            .update(&mut lease, WorkerOperation::Finish(None))
            .is_err()
    );
    worker
        .update(
            &mut lease,
            WorkerOperation::BeginEffect(intent(EffectOperation::PrinterUpload, "p")),
        )
        .unwrap();
    worker
        .update(&mut lease, WorkerOperation::ConfirmEffect(receipt("p")))
        .unwrap();
    assert!(
        worker
            .update(&mut lease, WorkerOperation::Finish(None))
            .is_err()
    );
    worker
        .update(
            &mut lease,
            WorkerOperation::BeginEffect(intent(EffectOperation::PrinterStart, "p")),
        )
        .unwrap();
    let mut start_receipt = receipt("p");
    start_receipt.receipt_id = "confirmed-start".into();
    worker
        .update(
            &mut lease,
            WorkerOperation::ConfirmEffect(start_receipt.clone()),
        )
        .unwrap();
    let finished = worker
        .update(&mut lease, WorkerOperation::Finish(None))
        .unwrap();
    assert_eq!(finished.state, PersistentState::Succeeded);
    assert_eq!(finished.result, Some(start_receipt.clone()));
    assert_eq!(get(&owner, &finished.job_id).result, Some(start_receipt));
    enqueue(
        &owner,
        "no-fake-export",
        Payload::ExportChecklistHtml { profile_id: 1 },
    );
    let claimed_attempt!(_, mut lease) = worker.claim().unwrap().unwrap();
    assert!(
        worker
            .update(&mut lease, WorkerOperation::Finish(None))
            .is_err()
    );
    worker
        .update(
            &mut lease,
            WorkerOperation::BeginEffect(intent(EffectOperation::LocalArtifact, "artifact")),
        )
        .unwrap();
    worker
        .update(
            &mut lease,
            WorkerOperation::ConfirmEffect(receipt("artifact")),
        )
        .unwrap();
    assert_eq!(
        worker
            .update(
                &mut lease,
                WorkerOperation::Finish(Some(receipt("artifact")))
            )
            .unwrap()
            .state,
        PersistentState::Succeeded
    );
    owner.shutdown().unwrap();
}
#[test]
fn ticket_t_28_claims_schema35_migration_rollback_and_backup_restart() {
    let (path, owner) = fixture();
    owner.shutdown().unwrap();
    let fixture = Connection::open(path.join("print-partner.db")).unwrap();
    #[cfg(feature = "desktop_rebuild")]
    fixture
        .execute_batch("DROP TABLE source_sync_targets; DROP TABLE source_sync_batches; DROP TABLE source_scan_executions; DROP TABLE plan_apply_admissions; DROP TABLE source_preclaim_refusals; DROP TABLE source_revision_observations; DROP TABLE source_revision_artifacts; DROP TABLE source_revision_attempts; DROP TABLE source_import_quota; DROP TABLE source_import_operations; DROP TRIGGER trg_source_revisions_provenance_immutable_update; DROP TRIGGER trg_source_revisions_provenance_immutable_delete")
        .unwrap();
    fixture.execute_batch("DROP TABLE durable_job_reconciliations; DROP TABLE durable_job_history; DROP TABLE durable_job_keys; DROP TABLE durable_jobs; UPDATE app_settings SET value='34' WHERE tenant_id='default' AND key='schema_version'; CREATE TRIGGER reject_schema35 BEFORE UPDATE ON app_settings WHEN NEW.key='schema_version' AND NEW.value='35' BEGIN SELECT RAISE(ABORT,'fixture migration constraint'); END;").unwrap();
    drop(fixture);
    assert!(WriterOwner::open(&path, Limits::default()).is_err());
    let raw = Connection::open(path.join("print-partner.db")).unwrap();
    assert_eq!(
        raw.query_row(
            "SELECT COUNT(*) FROM sqlite_master WHERE name LIKE 'durable_job%'",
            [],
            |r| r.get::<_, i64>(0)
        )
        .unwrap(),
        0
    );
    assert_eq!(
        raw.query_row(
            "SELECT value FROM app_settings WHERE tenant_id='default' AND key='schema_version'",
            [],
            |r| r.get::<_, String>(0)
        )
        .unwrap(),
        "34"
    );
    raw.execute_batch("DROP TRIGGER reject_schema35;").unwrap();
    drop(raw);
    let (owner, ready) = WriterOwner::open(&path, Limits::default()).unwrap();
    assert_eq!(ready.previous_version, 34);
    let backup = ready.backup.unwrap();
    let before = std::fs::read(&backup).unwrap();
    owner.shutdown().unwrap();
    let (owner, ready) = WriterOwner::open(&path, Limits::default()).unwrap();
    assert!(ready.backup.is_none());
    assert_eq!(before, std::fs::read(&backup).unwrap());
    owner.shutdown().unwrap();
}
#[test]
fn ticket_t_28_recovery_lost_claim_reply_is_fenced_on_orderly_restart() {
    let (path, owner) = fixture();
    let queued = enqueue(&owner, "lost-claim", Payload::CheckSourceUpdates {});
    let worker = owner.job_worker(admission()).unwrap();
    drop(
        worker
            .claim_pending(&AtomicBool::new(false), Duration::from_secs(5))
            .unwrap(),
    );
    owner.shutdown().unwrap();
    let (owner, _) = WriterOwner::open(&path, Limits::default()).unwrap();
    let worker = owner.job_worker(admission()).unwrap();
    let claimed_attempt!(recovered, _) = worker.claim().unwrap().unwrap();
    assert_eq!(recovered.job_id, queued.job_id);
    assert_eq!(recovered.attempt, 2);
    assert_eq!(recovered.generation, 3);
    owner.shutdown().unwrap();
}
#[test]
fn ticket_t_28_claims_crash_retry_fences_stale_updates() {
    const CHILD_DIR: &str = "PP_GRE408_CRASH_DIR";
    if let Some(path) = std::env::var_os(CHILD_DIR) {
        let path = PathBuf::from(path);
        let (owner, _) = WriterOwner::open(&path, Limits::default()).unwrap();
        let queued = enqueue(&owner, "crash-retry", Payload::CheckSourceUpdates {});
        let worker = owner.job_worker(admission()).unwrap();
        let claimed_attempt!(claimed, _) = worker.claim().unwrap().unwrap();
        assert_eq!(claimed.job_id, queued.job_id);
        // process::exit skips Drop, so the owner marker and running claim stay behind.
        std::process::exit(28);
    }

    let path = directory();
    let output = child_process::command(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "ticket_t_28_claims_crash_retry_fences_stale_updates",
            "--nocapture",
        ])
        .env(CHILD_DIR, &path)
        .output()
        .unwrap();
    assert_eq!(
        output.status.code(),
        Some(28),
        "stdout: {}\nstderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let raw = readonly(&path);
    let documents = raw
        .prepare("SELECT document FROM durable_jobs")
        .unwrap()
        .query_map([], |row| row.get::<_, String>(0))
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap();
    assert_eq!(documents.len(), 1);
    let crashed: JobRecord = serde_json::from_str(&documents[0]).unwrap();
    assert_eq!(crashed.state, PersistentState::Running);
    assert_eq!(crashed.attempt, 1);
    assert_eq!(crashed.generation, 1);
    let job_id = crashed.job_id;
    drop(raw);

    let (owner, _) = WriterOwner::open(&path, Limits::default()).unwrap();
    let duplicate = enqueue(&owner, "crash-retry", Payload::CheckSourceUpdates {});
    assert_eq!(duplicate.job_id, job_id);
    let worker = owner.job_worker(admission()).unwrap();
    let claimed_attempt!(claimed, mut lease) = worker.claim().unwrap().unwrap();
    assert_eq!(claimed.job_id, job_id);
    assert_eq!(claimed.attempt, 2);
    assert_eq!(claimed.generation, 3);
    assert!(worker.claim().unwrap().is_none());

    let history = |owner: &WriterOwner| match call(
        owner,
        UserOperation::History {
            job_id: job_id.clone(),
            before_version: None,
            limit: 200,
        },
    )
    .unwrap()
    {
        Outcome::History(entries) => entries
            .into_iter()
            .map(|entry| (entry.version, entry.state, entry.event))
            .collect::<Vec<_>>(),
        _ => panic!("history"),
    };
    let mut stale = lease.clone();
    worker
        .update(&mut lease, WorkerOperation::Heartbeat)
        .unwrap();
    let before_record = serde_json::to_value(get(&owner, &job_id)).unwrap();
    let before_history = history(&owner);
    for operation in [WorkerOperation::Heartbeat, WorkerOperation::Finish(None)] {
        let error = worker.update(&mut stale, operation).unwrap_err();
        assert!(error.to_string().contains("Stale attempt fence"), "{error}");
    }
    assert_eq!(
        serde_json::to_value(get(&owner, &job_id)).unwrap(),
        before_record
    );
    assert_eq!(history(&owner), before_history);

    let done = worker
        .update(&mut lease, WorkerOperation::Finish(None))
        .unwrap();
    assert_eq!(done.state, PersistentState::Succeeded);
    assert_eq!(done.attempt, 2);
    assert_eq!(done.generation, 3);
    owner.shutdown().unwrap();

    let (owner, _) = WriterOwner::open(&path, Limits::default()).unwrap();
    let persisted = get(&owner, &job_id);
    assert_eq!(persisted.state, PersistentState::Succeeded);
    assert_eq!(persisted.attempt, 2);
    assert_eq!(persisted.generation, 3);
    assert_eq!(persisted.state_version, done.state_version);
    owner.shutdown().unwrap();
}
#[test]
fn ticket_t_28_recovery_reconciliation_records_authenticated_subject() {
    let (_, owner) = fixture();
    let (identity, token) = register(&owner, "reconcile@example.com");
    let mut payload = printer("owned-printer");
    if let Payload::PrinterUpload { start, .. } = &mut payload {
        *start = false;
    }
    let queued = job(session_call(
        &owner,
        policy(),
        &token,
        UserOperation::Enqueue {
            key: "subject".into(),
            payload_version: 1,
            payload,
        },
    )
    .unwrap());
    let worker = session_worker(&owner);
    let claimed_attempt!(_, mut lease) = worker.claim().unwrap().unwrap();
    worker
        .update(
            &mut lease,
            WorkerOperation::BeginEffect(intent(EffectOperation::PrinterUpload, "owned-printer")),
        )
        .unwrap();
    let uncertain = worker.update(&mut lease, WorkerOperation::Fail).unwrap();
    let resolved = job(session_call(
        &owner,
        policy(),
        &token,
        UserOperation::Reconcile {
            job_id: queued.job_id.clone(),
            expected_version: uncertain.state_version,
            expected_generation: uncertain.generation,
            effect_hash: "b".repeat(64),
            decision: Decision::ConfirmSucceeded,
            receipt: Some(receipt("owned-printer")),
        },
    )
    .unwrap());
    assert_eq!(resolved.state, PersistentState::Succeeded);
    match session_call(
        &owner,
        policy(),
        &token,
        UserOperation::Reconciliations {
            job_id: queued.job_id,
            before_version: None,
            limit: 10,
        },
    )
    .unwrap()
    {
        Outcome::Reconciliations(records) => {
            assert_eq!(records.len(), 1);
            assert_eq!(records[0].subject, format!("user:{}", identity.user_id));
            assert_eq!(records[0].generation, uncertain.generation);
            assert_eq!(records[0].effect_hash, "b".repeat(64));
            assert_eq!(records[0].receipt, Some(receipt("owned-printer")));
        }
        _ => panic!("reconciliation"),
    }
    owner.shutdown().unwrap();
}
#[test]
fn split_printer_upload_reconciles_to_uploaded_only() {
    let (path, owner) = fixture();
    let queued = enqueue(&owner, "uploaded-only", printer("uploaded-only-printer"));
    let worker = owner.job_worker(admission()).unwrap();
    let claimed_attempt!(_, mut lease) = worker.claim().unwrap().unwrap();
    worker
        .update(
            &mut lease,
            WorkerOperation::BeginEffect(intent(
                EffectOperation::PrinterUpload,
                "uploaded-only-printer",
            )),
        )
        .unwrap();
    worker
        .update(
            &mut lease,
            WorkerOperation::ConfirmEffect(receipt("uploaded-only-printer")),
        )
        .unwrap();
    let uncertain = worker.update(&mut lease, WorkerOperation::Fail).unwrap();
    let mut changed_receipt = receipt("uploaded-only-printer");
    changed_receipt.receipt_id = "different-receipt".into();
    assert!(
        call(
            &owner,
            UserOperation::Reconcile {
                job_id: queued.job_id.clone(),
                expected_version: uncertain.state_version,
                expected_generation: uncertain.generation,
                effect_hash: "b".repeat(64),
                decision: Decision::ConfirmSucceeded,
                receipt: Some(changed_receipt),
            },
        )
        .is_err()
    );
    assert!(
        call(
            &owner,
            UserOperation::Reconcile {
                job_id: queued.job_id.clone(),
                expected_version: uncertain.state_version,
                expected_generation: uncertain.generation,
                effect_hash: "b".repeat(64),
                decision: Decision::ConfirmNoEffect,
                receipt: None,
            },
        )
        .is_err()
    );
    let resolved = job(call(
        &owner,
        UserOperation::Reconcile {
            job_id: queued.job_id.clone(),
            expected_version: uncertain.state_version,
            expected_generation: uncertain.generation,
            effect_hash: "b".repeat(64),
            decision: Decision::ConfirmSucceeded,
            receipt: Some(receipt("uploaded-only-printer")),
        },
    )
    .unwrap());
    assert!(resolved.state.terminal());
    assert_eq!(resolved.snapshot().status, "done");
    assert_eq!(
        resolved.snapshot().message,
        "Uploaded only; print not started"
    );
    assert_eq!(resolved.effects.len(), 1);
    assert!(resolved.effects[0].confirmed);
    assert_eq!(resolved.result, Some(receipt("uploaded-only-printer")));
    assert!(
        worker
            .update(&mut lease, WorkerOperation::Heartbeat)
            .is_err()
    );

    owner.shutdown().unwrap();
    let owner = WriterOwner::open(&path, Limits::default()).unwrap().0;
    let recovered = get(&owner, &queued.job_id);
    assert!(recovered.state.terminal());
    assert_eq!(recovered.snapshot().status, "done");
    assert_eq!(recovered.effects, resolved.effects);
    assert_eq!(recovered.result, resolved.result);
    let listed = match call(
        &owner,
        UserOperation::List(JobListQuery {
            status: Some("done".into()),
            ..Default::default()
        }),
    )
    .unwrap()
    {
        Outcome::List(records) => records,
        _ => panic!("job list"),
    };
    assert!(listed.iter().any(|record| record.job_id == queued.job_id));
    let newer = enqueue(&owner, "newer-terminal", Payload::CheckSourceUpdates {});
    let worker = owner.job_worker(admission()).unwrap();
    let claimed_attempt!(claimed, mut lease) = worker.claim().unwrap().unwrap();
    assert_eq!(claimed.job_id, newer.job_id);
    worker
        .update(&mut lease, WorkerOperation::Finish(None))
        .unwrap();
    owner.shutdown().unwrap();
    let raw = Connection::open(path.join("print-partner.db")).unwrap();
    raw.execute(
        "UPDATE durable_jobs SET updated=updated-172800, document=json_set(document,'$.updated_at',updated-172800) WHERE id=?1",
        [&queued.job_id],
    )
    .unwrap();
    drop(raw);
    let owner = WriterOwner::open(&path, Limits::default()).unwrap().0;
    assert_eq!(owner.retain_jobs(1, 1).unwrap(), 1);
    assert!(
        call(
            &owner,
            UserOperation::Get {
                job_id: queued.job_id.clone(),
            },
        )
        .is_err()
    );
    let child = enqueue(
        &owner,
        "start-from-retained-parent",
        Payload::PrinterStart(PrinterStartRequest::new(queued.job_id)),
    );
    let Payload::PrinterStart(request) = child.payload else {
        panic!("printer start payload")
    };
    assert_eq!(request.printer_id(), Some("uploaded-only-printer"));
    owner.shutdown().unwrap();
}
#[test]
fn uploaded_only_parent_authorizes_one_bound_start_without_reupload() {
    let (path, owner) = fixture();
    let queued = enqueue(&owner, "uploaded-parent", printer("deliberate-printer"));
    let worker = owner.job_worker(admission()).unwrap();
    let claimed_attempt!(_, mut lease) = worker.claim().unwrap().unwrap();
    let upload_intent = intent(EffectOperation::PrinterUpload, "deliberate-printer");
    worker
        .update(
            &mut lease,
            WorkerOperation::BeginEffect(upload_intent.clone()),
        )
        .unwrap();
    worker
        .update(
            &mut lease,
            WorkerOperation::ConfirmEffect(receipt("deliberate-printer")),
        )
        .unwrap();
    let uncertain = worker.update(&mut lease, WorkerOperation::Fail).unwrap();
    let parent = job(call(
        &owner,
        UserOperation::Reconcile {
            job_id: queued.job_id.clone(),
            expected_version: uncertain.state_version,
            expected_generation: uncertain.generation,
            effect_hash: upload_intent.content_hash.clone(),
            decision: Decision::ConfirmSucceeded,
            receipt: Some(receipt("deliberate-printer")),
        },
    )
    .unwrap());
    let parent_before = serde_json::to_value(&parent).unwrap();
    let start_payload = || Payload::PrinterStart(PrinterStartRequest::new(&parent.job_id));
    let first = enqueue(&owner, "deliberate-start", start_payload());
    assert_eq!(first.kind, JobKind::PrinterUpload);
    assert_eq!(first.payload.kind(), JobKind::PrinterUpload);
    let Payload::PrinterStart(request) = &first.payload else {
        panic!("printer start payload")
    };
    assert_eq!(request.printer_id(), Some("deliberate-printer"));
    assert_eq!(request.upload_effect(), Some(&parent.effects[0]));
    assert!(
        call(
            &owner,
            UserOperation::Enqueue {
                key: "forged-start-binding".into(),
                payload_version: 1,
                payload: first.payload.clone(),
            },
        )
        .is_err()
    );
    let (_, foreign_token) = register(&owner, "foreign-start@example.com");
    assert!(
        session_call(
            &owner,
            policy(),
            &foreign_token,
            UserOperation::Enqueue {
                key: "foreign-start".into(),
                payload_version: 1,
                payload: start_payload(),
            },
        )
        .is_err()
    );
    let same = enqueue(&owner, "deliberate-start", start_payload());
    assert_eq!(same.job_id, first.job_id);
    assert!(
        call(
            &owner,
            UserOperation::Enqueue {
                key: "duplicate-deliberate-start".into(),
                payload_version: 1,
                payload: start_payload(),
            },
        )
        .is_err()
    );
    assert_eq!(
        serde_json::to_value(get(&owner, &parent.job_id)).unwrap(),
        parent_before
    );
    call(
        &owner,
        UserOperation::Cancel {
            job_id: first.job_id,
        },
    )
    .unwrap();
    let retry = enqueue(&owner, "retried-deliberate-start", start_payload());
    let claimed_attempt!(claimed, mut start_lease) = worker.claim().unwrap().unwrap();
    assert_eq!(claimed.job_id, retry.job_id);
    enqueue(&owner, "same-printer-upload", printer("deliberate-printer"));
    assert!(worker.claim().unwrap().is_none());
    for operation in [
        EffectOperation::PrinterUpload,
        EffectOperation::PrinterUploadAndStart,
        EffectOperation::SpoolmanDeduction,
        EffectOperation::LocalArtifact,
    ] {
        assert!(
            worker
                .update(
                    &mut start_lease,
                    WorkerOperation::BeginEffect(intent(operation, "deliberate-printer")),
                )
                .is_err()
        );
    }
    let mut wrong_content = intent(EffectOperation::PrinterStart, "deliberate-printer");
    wrong_content.content_hash = "c".repeat(64);
    assert!(
        worker
            .update(
                &mut start_lease,
                WorkerOperation::BeginEffect(wrong_content),
            )
            .is_err()
    );
    worker
        .update(
            &mut start_lease,
            WorkerOperation::BeginEffect(intent(
                EffectOperation::PrinterStart,
                "deliberate-printer",
            )),
        )
        .unwrap();
    worker
        .update(
            &mut start_lease,
            WorkerOperation::ConfirmEffect(receipt("deliberate-printer")),
        )
        .unwrap();
    let done = worker
        .update(
            &mut start_lease,
            WorkerOperation::Finish(Some(receipt("deliberate-printer"))),
        )
        .unwrap();
    assert_eq!(done.state, PersistentState::Succeeded);
    assert_eq!(done.effects.len(), 1);
    assert_eq!(
        done.effects[0].intent.operation,
        EffectOperation::PrinterStart
    );
    assert!(
        call(
            &owner,
            UserOperation::Enqueue {
                key: "start-after-success".into(),
                payload_version: 1,
                payload: start_payload(),
            },
        )
        .is_err()
    );
    owner.shutdown().unwrap();
    let owner = WriterOwner::open(&path, Limits::default()).unwrap().0;
    assert_eq!(
        enqueue(&owner, "retried-deliberate-start", start_payload()).job_id,
        retry.job_id
    );
    owner.shutdown().unwrap();
}
#[test]
fn admitted_unconfirmed_upload_reaches_uploaded_only() {
    let (_, owner) = fixture();
    let parent = uploaded_only_parent(&owner, "unconfirmed-upload", "unconfirmed-printer");
    assert_eq!(parent.state, PersistentState::UploadedOnly);
    assert_eq!(parent.effects.len(), 1);
    assert!(parent.effects[0].confirmed);
    assert_eq!(parent.result, Some(receipt("unconfirmed-printer")));
    owner.shutdown().unwrap();
}
#[test]
fn start_child_rejects_wrong_target_and_basis() {
    let (_, owner) = fixture();
    let parent = uploaded_only_parent(&owner, "effect-parent", "effect-printer");
    let child = job(enqueue_start(&owner, "effect-child", &parent.job_id).unwrap());
    let worker = owner.job_worker(admission()).unwrap();
    let claimed_attempt!(claimed, mut lease) = worker.claim().unwrap().unwrap();
    assert_eq!(claimed.job_id, child.job_id);
    assert!(
        worker
            .update(
                &mut lease,
                WorkerOperation::BeginEffect(intent(
                    EffectOperation::PrinterStart,
                    "wrong-printer",
                )),
            )
            .is_err()
    );
    let mut wrong_basis = intent(EffectOperation::PrinterStart, "effect-printer");
    wrong_basis.basis_hash = "e".repeat(64);
    assert!(
        worker
            .update(&mut lease, WorkerOperation::BeginEffect(wrong_basis),)
            .is_err()
    );
    owner.shutdown().unwrap();
}
#[test]
fn start_child_cannot_finish_without_confirmed_start() {
    let (_, owner) = fixture();
    let parent = uploaded_only_parent(&owner, "finish-parent", "finish-printer");
    let child = job(enqueue_start(&owner, "finish-child", &parent.job_id).unwrap());
    let worker = owner.job_worker(admission()).unwrap();
    let claimed_attempt!(claimed, mut lease) = worker.claim().unwrap().unwrap();
    assert_eq!(claimed.job_id, child.job_id);
    assert!(
        worker
            .update(&mut lease, WorkerOperation::Finish(None))
            .is_err()
    );
    assert!(
        worker
            .update(
                &mut lease,
                WorkerOperation::Finish(Some(receipt("finish-printer"))),
            )
            .is_err()
    );
    worker
        .update(
            &mut lease,
            WorkerOperation::BeginEffect(intent(EffectOperation::PrinterStart, "finish-printer")),
        )
        .unwrap();
    assert!(
        worker
            .update(&mut lease, WorkerOperation::Finish(None))
            .is_err()
    );
    worker
        .update(
            &mut lease,
            WorkerOperation::ConfirmEffect(receipt("finish-printer")),
        )
        .unwrap();
    let done = worker
        .update(&mut lease, WorkerOperation::Finish(None))
        .unwrap();
    assert_eq!(done.state, PersistentState::Succeeded);
    assert_eq!(done.result, Some(receipt("finish-printer")));
    owner.shutdown().unwrap();
}
#[test]
fn confirmed_combined_receipt_cannot_be_rewritten() {
    let (_, owner) = fixture();
    let queued = enqueue(&owner, "combined-receipt", printer("combined-printer"));
    let worker = owner.job_worker(admission()).unwrap();
    let claimed_attempt!(_, mut lease) = worker.claim().unwrap().unwrap();
    worker
        .update(
            &mut lease,
            WorkerOperation::BeginEffect(intent(
                EffectOperation::PrinterUploadAndStart,
                "combined-printer",
            )),
        )
        .unwrap();
    worker
        .update(
            &mut lease,
            WorkerOperation::ConfirmEffect(receipt("combined-printer")),
        )
        .unwrap();
    let uncertain = worker.update(&mut lease, WorkerOperation::Fail).unwrap();
    let mut forged = receipt("combined-printer");
    forged.receipt_id = "forged".into();
    assert!(reconcile(&owner, &uncertain, Decision::ConfirmSucceeded, Some(forged),).is_err());
    let resolved = job(reconcile(
        &owner,
        &uncertain,
        Decision::ConfirmSucceeded,
        Some(receipt("combined-printer")),
    )
    .unwrap());
    assert_eq!(resolved.state, PersistentState::Succeeded);
    assert_eq!(resolved.job_id, queued.job_id);
    owner.shutdown().unwrap();
}
#[test]
fn printer_finish_rejects_ancillary_result_then_accepts_primary_receipt() {
    let (_, owner) = fixture();
    let worker = owner.job_worker(admission()).unwrap();
    for (index, start) in [false, true].into_iter().enumerate() {
        let printer_id = format!("finish-primary-printer-{index}");
        let mut payload = printer(&printer_id);
        let Payload::PrinterUpload {
            start: requested_start,
            ..
        } = &mut payload
        else {
            unreachable!()
        };
        *requested_start = start;
        let queued = enqueue(&owner, &format!("finish-primary-{index}"), payload);
        let claimed_attempt!(claimed, mut lease) = worker.claim().unwrap().unwrap();
        assert_eq!(claimed.job_id, queued.job_id);
        assert!(
            worker
                .update(&mut lease, WorkerOperation::Finish(None))
                .is_err()
        );
        let upload = distinct_intent(
            EffectOperation::PrinterUpload,
            &printer_id,
            600 + index as u64,
        );
        let upload_receipt = begin_and_confirm(
            &worker,
            &mut lease,
            upload,
            &format!("finish-upload-{index}"),
        );
        let primary_receipt = if start {
            let start = distinct_intent(
                EffectOperation::PrinterStart,
                &printer_id,
                610 + index as u64,
            );
            begin_and_confirm(&worker, &mut lease, start, &format!("finish-start-{index}"))
        } else {
            upload_receipt
        };
        let spoolman = distinct_intent(
            EffectOperation::SpoolmanDeduction,
            &format!("spoolman:finish:{index}"),
            620 + index as u64,
        );
        let spoolman_receipt = begin_and_confirm(
            &worker,
            &mut lease,
            spoolman,
            &format!("finish-spoolman-{index}"),
        );
        assert!(
            worker
                .update(&mut lease, WorkerOperation::Finish(Some(spoolman_receipt)),)
                .is_err()
        );
        let succeeded = worker
            .update(
                &mut lease,
                WorkerOperation::Finish(Some(primary_receipt.clone())),
            )
            .unwrap();
        assert_eq!(succeeded.state, PersistentState::Succeeded);
        assert_eq!(succeeded.result, Some(primary_receipt));
    }
    owner.shutdown().unwrap();
}
#[test]
fn active_and_failed_parents_cannot_authorize_start() {
    let (_, owner) = fixture();
    let queued = enqueue(&owner, "wrong-state-parent", printer("wrong-state-printer"));
    assert!(enqueue_start(&owner, "from-queued", &queued.job_id).is_err());
    let worker = owner.job_worker(admission()).unwrap();
    let claimed_attempt!(running, mut lease) = worker.claim().unwrap().unwrap();
    assert!(enqueue_start(&owner, "from-running", &running.job_id).is_err());
    let admitted = worker
        .update(
            &mut lease,
            WorkerOperation::BeginEffect(intent(
                EffectOperation::PrinterUpload,
                "wrong-state-printer",
            )),
        )
        .unwrap();
    assert!(enqueue_start(&owner, "from-admitted", &admitted.job_id).is_err());
    let uncertain = worker.update(&mut lease, WorkerOperation::Fail).unwrap();
    assert!(enqueue_start(&owner, "from-uncertain", &uncertain.job_id).is_err());
    let failed = job(reconcile(&owner, &uncertain, Decision::ConfirmNoEffect, None).unwrap());
    assert_eq!(failed.state, PersistentState::Failed);
    assert!(enqueue_start(&owner, "from-failed", &failed.job_id).is_err());
    let cancelled = enqueue(&owner, "cancelled-parent", printer("cancelled-printer"));
    let cancelled = job(call(
        &owner,
        UserOperation::Cancel {
            job_id: cancelled.job_id,
        },
    )
    .unwrap());
    assert!(enqueue_start(&owner, "from-cancelled", &cancelled.job_id).is_err());
    assert!(enqueue_start(&owner, "from-missing", "missing-parent").is_err());
    owner.shutdown().unwrap();
}
#[test]
fn succeeded_and_nonprinter_parents_cannot_authorize_start() {
    let (_, owner) = fixture();
    let worker = owner.job_worker(admission()).unwrap();
    let mut no_start = printer("no-start-printer");
    let Payload::PrinterUpload { start, .. } = &mut no_start else {
        unreachable!()
    };
    *start = false;
    let queued = enqueue(&owner, "no-start-parent", no_start);
    let claimed_attempt!(_, mut lease) = worker.claim().unwrap().unwrap();
    worker
        .update(
            &mut lease,
            WorkerOperation::BeginEffect(intent(
                EffectOperation::PrinterUpload,
                "no-start-printer",
            )),
        )
        .unwrap();
    worker
        .update(
            &mut lease,
            WorkerOperation::ConfirmEffect(receipt("no-start-printer")),
        )
        .unwrap();
    let succeeded = worker
        .update(
            &mut lease,
            WorkerOperation::Finish(Some(receipt("no-start-printer"))),
        )
        .unwrap();
    assert!(enqueue_start(&owner, "from-succeeded", &succeeded.job_id).is_err());
    assert_eq!(succeeded.job_id, queued.job_id);
    let mut reconciled_no_start = printer("reconciled-no-start-printer");
    let Payload::PrinterUpload { start, .. } = &mut reconciled_no_start else {
        unreachable!()
    };
    *start = false;
    enqueue(&owner, "reconciled-no-start-parent", reconciled_no_start);
    let claimed_attempt!(_, mut lease) = worker.claim().unwrap().unwrap();
    worker
        .update(
            &mut lease,
            WorkerOperation::BeginEffect(intent(
                EffectOperation::PrinterUpload,
                "reconciled-no-start-printer",
            )),
        )
        .unwrap();
    let uncertain = worker.update(&mut lease, WorkerOperation::Fail).unwrap();
    let reconciled = job(reconcile(
        &owner,
        &uncertain,
        Decision::ConfirmSucceeded,
        Some(receipt("reconciled-no-start-printer")),
    )
    .unwrap());
    assert_eq!(reconciled.state, PersistentState::Succeeded);
    assert!(enqueue_start(&owner, "from-reconciled", &reconciled.job_id).is_err());
    let parent = uploaded_only_parent(&owner, "child-parent", "child-parent-printer");
    let child = job(enqueue_start(&owner, "child-parent-child", &parent.job_id).unwrap());
    assert!(enqueue_start(&owner, "from-child", &child.job_id).is_err());
    let nonprinter = enqueue(&owner, "nonprinter-parent", Payload::CheckSourceUpdates {});
    assert!(enqueue_start(&owner, "from-nonprinter", &nonprinter.job_id).is_err());
    owner.shutdown().unwrap();
}
#[test]
fn uploaded_only_does_not_consume_active_queue_capacity() {
    let (_, owner) = fixture();
    let parent = uploaded_only_parent(&owner, "cap-parent", "cap-printer");
    assert_eq!(parent.state, PersistentState::UploadedOnly);
    for index in 0..1024 {
        enqueue(
            &owner,
            &format!("active-cap-{index}"),
            Payload::CheckSourceUpdates {},
        );
    }
    assert!(
        call(
            &owner,
            UserOperation::Enqueue {
                key: "active-cap-overflow".into(),
                payload_version: 1,
                payload: Payload::CheckSourceUpdates {},
            },
        )
        .is_err()
    );
    owner.shutdown().unwrap();
}
#[test]
fn archived_enqueue_replay_rejects_mismatched_identity_without_writes() {
    for field in ["tenant", "job_id"] {
        let (path, owner) = fixture();
        let parent = uploaded_only_parent(&owner, "replay-identity", "replay-printer");
        owner.shutdown().unwrap();
        let raw = Connection::open(path.join("print-partner.db")).unwrap();
        raw.execute(
            "UPDATE durable_jobs SET updated=updated-172800,document=json_set(document,'$.updated_at',updated-172800)",
            [],
        ).unwrap();
        drop(raw);
        let owner = WriterOwner::open(&path, Limits::default()).unwrap().0;
        assert_eq!(owner.retain_jobs(1, 1).unwrap(), 1);
        owner.shutdown().unwrap();
        let raw = Connection::open(path.join("print-partner.db")).unwrap();
        raw.execute(
            "UPDATE durable_job_keys SET archived_document=json_set(archived_document,?1,'other-identity') WHERE job_id=?2",
            rusqlite::params![format!("$.{field}"), parent.job_id],
        ).unwrap();
        let snapshot = || {
            raw.query_row(
            "SELECT archived_document,(SELECT COUNT(*) FROM durable_jobs),(SELECT COUNT(*) FROM durable_job_history),(SELECT COUNT(*) FROM durable_job_keys) FROM durable_job_keys WHERE job_id=?1",
            [&parent.job_id],
            |row| Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?, row.get::<_, i64>(2)?, row.get::<_, i64>(3)?)),
        ).unwrap()
        };
        let before = snapshot();
        let owner = WriterOwner::open(&path, Limits::default()).unwrap().0;
        let error = call(
            &owner,
            UserOperation::Enqueue {
                key: "replay-identity".into(),
                payload_version: 1,
                payload: parent.payload.clone(),
            },
        )
        .err()
        .expect("Mismatched archived identity must reject replay");
        assert_eq!(error.to_string(), "Job not found");
        assert_eq!(snapshot(), before);
        owner.shutdown().unwrap();
    }
}
#[test]
fn archived_started_child_blocks_new_key_and_replays_original_key() {
    let (path, owner) = fixture();
    let parent = uploaded_only_parent(&owner, "archive-parent", "archive-printer");
    let child = job(enqueue_start(&owner, "archive-child", &parent.job_id).unwrap());
    let worker = owner.job_worker(admission()).unwrap();
    let claimed_attempt!(_, mut lease) = worker.claim().unwrap().unwrap();
    worker
        .update(
            &mut lease,
            WorkerOperation::BeginEffect(intent(EffectOperation::PrinterStart, "archive-printer")),
        )
        .unwrap();
    worker
        .update(
            &mut lease,
            WorkerOperation::ConfirmEffect(receipt("archive-printer")),
        )
        .unwrap();
    worker
        .update(
            &mut lease,
            WorkerOperation::Finish(Some(receipt("archive-printer"))),
        )
        .unwrap();
    owner.shutdown().unwrap();
    let raw = Connection::open(path.join("print-partner.db")).unwrap();
    raw.execute(
        "UPDATE durable_jobs SET updated=updated-172800, document=json_set(document,'$.updated_at',updated-172800)",
        [],
    )
    .unwrap();
    drop(raw);
    let owner = WriterOwner::open(&path, Limits::default()).unwrap().0;
    assert_eq!(owner.retain_jobs(1, 1).unwrap(), 2);
    assert_eq!(
        readonly(&path)
            .query_row("SELECT COUNT(*) FROM durable_jobs", [], |row| row
                .get::<_, i64>(0))
            .unwrap(),
        0
    );
    assert!(enqueue_start(&owner, "archive-new-key", &parent.job_id).is_err());
    assert_eq!(
        job(enqueue_start(&owner, "archive-child", &parent.job_id).unwrap()).job_id,
        child.job_id
    );
    let raw = readonly(&path);
    for (sql, expected) in [
        (
            "EXPLAIN QUERY PLAN SELECT document FROM durable_jobs WHERE tenant=?1 AND json_extract(document,'$.payload.payload.uploaded_job_id')=?2",
            "durable_jobs_printer_start_parent",
        ),
        (
            "EXPLAIN QUERY PLAN SELECT archived_document FROM durable_job_keys WHERE tenant=?1 AND archived_document IS NOT NULL AND json_extract(archived_document,'$.payload.payload.uploaded_job_id')=?2",
            "durable_job_keys_archived_printer_start_parent",
        ),
    ] {
        let details = raw
            .prepare(sql)
            .unwrap()
            .query_map(rusqlite::params!["default", parent.job_id], |row| {
                row.get::<_, String>(3)
            })
            .unwrap()
            .collect::<rusqlite::Result<Vec<_>>>()
            .unwrap();
        assert!(details.iter().any(|detail| detail.contains(expected)));
    }
    let details = raw
        .prepare("EXPLAIN QUERY PLAN SELECT document FROM durable_jobs WHERE tenant=?1 AND json_extract(document,'$.payload.payload.uploaded_job_id')=?2 UNION ALL SELECT archived_document FROM durable_job_keys WHERE tenant=?1 AND archived_document IS NOT NULL AND json_extract(archived_document,'$.payload.payload.uploaded_job_id')=?2")
        .unwrap()
        .query_map(rusqlite::params!["default", parent.job_id], |row| row.get::<_, String>(3))
        .unwrap()
        .collect::<rusqlite::Result<Vec<_>>>()
        .unwrap();
    assert!(
        details.iter().any(|detail| detail
            .contains("SEARCH durable_jobs USING INDEX durable_jobs_printer_start_parent")),
        "{details:?}"
    );
    assert!(
        details.iter().any(|detail| detail.contains(
            "SEARCH durable_job_keys USING INDEX durable_job_keys_archived_printer_start_parent"
        )),
        "{details:?}"
    );
    assert!(
        !details
            .iter()
            .any(|detail| detail.contains("SCAN durable_")),
        "{details:?}"
    );
    drop(raw);
    owner.shutdown().unwrap();
}
#[test]
fn uploaded_only_parent_is_inert() {
    let (path, owner) = fixture();
    let parent = uploaded_only_parent(&owner, "inert-parent", "inert-printer");
    let cancelled = job(call(
        &owner,
        UserOperation::Cancel {
            job_id: parent.job_id.clone(),
        },
    )
    .unwrap());
    assert_eq!(cancelled.state, PersistentState::UploadedOnly);
    assert_eq!(cancelled.state_version, parent.state_version);
    for decision in [
        Decision::ConfirmSucceeded,
        Decision::ConfirmNoEffect,
        Decision::Abandon,
    ] {
        assert!(reconcile(&owner, &parent, decision, None).is_err());
    }
    let worker = owner.job_worker(admission()).unwrap();
    assert!(worker.claim().unwrap().is_none());
    owner.shutdown().unwrap();
    let owner = WriterOwner::open(&path, Limits::default()).unwrap().0;
    assert_eq!(
        get(&owner, &parent.job_id).state,
        PersistentState::UploadedOnly
    );
    assert!(
        owner
            .job_worker(admission())
            .unwrap()
            .claim()
            .unwrap()
            .is_none()
    );
    owner.shutdown().unwrap();
}
#[test]
fn denied_start_settles_uploaded_only_and_authorizes_bound_child() {
    let (_, owner) = fixture();
    let queued = enqueue(
        &owner,
        "denied-start-parent",
        printer("denied-start-printer"),
    );
    let worker = owner.job_worker(admission()).unwrap();
    let claimed_attempt!(_, mut lease) = worker.claim().unwrap().unwrap();
    let upload_intent = distinct_intent(EffectOperation::PrinterUpload, "denied-start-printer", 1);
    let upload_receipt = distinct_receipt(&upload_intent, "upload-receipt");
    worker
        .update(
            &mut lease,
            WorkerOperation::BeginEffect(upload_intent.clone()),
        )
        .unwrap();
    worker
        .update(
            &mut lease,
            WorkerOperation::ConfirmEffect(upload_receipt.clone()),
        )
        .unwrap();
    let upload_bytes = serde_json::to_vec(&get(&owner, &queued.job_id).effects[0]).unwrap();
    let start_intent = distinct_intent(EffectOperation::PrinterStart, "denied-start-printer", 2);
    worker
        .update(
            &mut lease,
            WorkerOperation::BeginEffect(start_intent.clone()),
        )
        .unwrap();
    let uncertain = worker.update(&mut lease, WorkerOperation::Fail).unwrap();
    let settled = job(reconcile(&owner, &uncertain, Decision::ConfirmNoEffect, None).unwrap());
    assert_eq!(settled.state, PersistentState::UploadedOnly);
    assert_eq!(settled.snapshot().status, "done");
    assert_eq!(settled.result, Some(upload_receipt.clone()));
    assert_eq!(settled.effects.len(), 2);
    assert!(!settled.effects[0].no_effect);
    assert!(settled.effects[1].no_effect);
    assert_eq!(
        serde_json::to_vec(&settled.effects[0]).unwrap(),
        upload_bytes
    );
    let audit = reconciliations(&owner, &settled.job_id);
    assert_eq!(audit[0].decision, "ConfirmNoEffect");
    assert_eq!(audit[0].receipt, None);
    assert_eq!(audit[0].target, start_intent.target);
    let parent_bytes = serde_json::to_vec(&settled).unwrap();
    let child = job(enqueue_start(&owner, "denied-start-child", &settled.job_id).unwrap());
    let Payload::PrinterStart(request) = &child.payload else {
        panic!("printer start payload")
    };
    assert_eq!(request.upload_effect(), Some(&settled.effects[0]));
    assert_eq!(request.upload_effect().unwrap().receipt, settled.result);
    assert_eq!(
        serde_json::to_vec(&get(&owner, &settled.job_id)).unwrap(),
        parent_bytes
    );
    assert!(enqueue_start(&owner, "denied-start-second", &settled.job_id).is_err());
    let claimed_attempt!(claimed, mut child_lease) = worker.claim().unwrap().unwrap();
    assert_eq!(claimed.job_id, child.job_id);
    for operation in [
        EffectOperation::PrinterUpload,
        EffectOperation::PrinterUploadAndStart,
    ] {
        assert!(
            worker
                .update(
                    &mut child_lease,
                    WorkerOperation::BeginEffect(distinct_intent(
                        operation,
                        "denied-start-printer",
                        3,
                    )),
                )
                .is_err()
        );
    }
    let mut child_start = upload_intent;
    child_start.operation = EffectOperation::PrinterStart;
    worker
        .update(&mut child_lease, WorkerOperation::BeginEffect(child_start))
        .unwrap();
    worker
        .update(
            &mut child_lease,
            WorkerOperation::ConfirmEffect(upload_receipt.clone()),
        )
        .unwrap();
    worker
        .update(
            &mut child_lease,
            WorkerOperation::Finish(Some(upload_receipt)),
        )
        .unwrap();
    assert!(enqueue_start(&owner, "denied-start-third", &settled.job_id).is_err());
    owner.shutdown().unwrap();
}
#[test]
fn confirmed_spoolman_settles_to_upload_receipt_and_subject_audit() {
    let (_, owner) = fixture();
    let queued = enqueue(&owner, "spoolman-parent", printer("spoolman-printer"));
    let worker = owner.job_worker(admission()).unwrap();
    let claimed_attempt!(_, mut lease) = worker.claim().unwrap().unwrap();
    let upload_intent = distinct_intent(EffectOperation::PrinterUpload, "spoolman-printer", 10);
    let upload_receipt = distinct_receipt(&upload_intent, "upload-receipt-10");
    worker
        .update(&mut lease, WorkerOperation::BeginEffect(upload_intent))
        .unwrap();
    worker
        .update(
            &mut lease,
            WorkerOperation::ConfirmEffect(upload_receipt.clone()),
        )
        .unwrap();
    let spoolman_intent = distinct_intent(EffectOperation::SpoolmanDeduction, "spoolman:1", 11);
    let spoolman_receipt = distinct_receipt(&spoolman_intent, "spoolman-receipt-1");
    worker
        .update(
            &mut lease,
            WorkerOperation::BeginEffect(spoolman_intent.clone()),
        )
        .unwrap();
    worker
        .update(
            &mut lease,
            WorkerOperation::ConfirmEffect(spoolman_receipt.clone()),
        )
        .unwrap();
    let uncertain = worker.update(&mut lease, WorkerOperation::Fail).unwrap();
    let settled = job(reconcile(
        &owner,
        &uncertain,
        Decision::ConfirmSucceeded,
        Some(spoolman_receipt.clone()),
    )
    .unwrap());
    assert_eq!(settled.state, PersistentState::UploadedOnly);
    assert_eq!(settled.result, Some(upload_receipt));
    assert_eq!(settled.effects[1].receipt, Some(spoolman_receipt.clone()));
    assert!(settled.effects[1].confirmed);
    let audit = reconciliations(&owner, &queued.job_id);
    assert_eq!(audit[0].receipt, Some(spoolman_receipt));
    assert_eq!(audit[0].target, spoolman_intent.target);
    enqueue(&owner, "after-spoolman", printer("spoolman-printer"));
    assert!(worker.claim().unwrap().is_some());
    owner.shutdown().unwrap();
}
#[test]
fn confirmed_spoolman_rejects_changed_or_denied_outcome() {
    let (path, owner) = fixture();
    enqueue(
        &owner,
        "spoolman-reject",
        printer("spoolman-reject-printer"),
    );
    let worker = owner.job_worker(admission()).unwrap();
    let claimed_attempt!(_, mut lease) = worker.claim().unwrap().unwrap();
    let upload = distinct_intent(
        EffectOperation::PrinterUpload,
        "spoolman-reject-printer",
        20,
    );
    worker
        .update(&mut lease, WorkerOperation::BeginEffect(upload.clone()))
        .unwrap();
    worker
        .update(
            &mut lease,
            WorkerOperation::ConfirmEffect(distinct_receipt(&upload, "upload-receipt-20")),
        )
        .unwrap();
    let spoolman = distinct_intent(EffectOperation::SpoolmanDeduction, "spoolman:20", 21);
    let spoolman_receipt = distinct_receipt(&spoolman, "spoolman-receipt-20");
    worker
        .update(&mut lease, WorkerOperation::BeginEffect(spoolman.clone()))
        .unwrap();
    worker
        .update(
            &mut lease,
            WorkerOperation::ConfirmEffect(spoolman_receipt.clone()),
        )
        .unwrap();
    let uncertain = worker.update(&mut lease, WorkerOperation::Fail).unwrap();
    let raw = readonly(&path);
    let before: (String, i64, i64) = raw
        .query_row(
            "SELECT document,(SELECT COUNT(*) FROM durable_job_history WHERE job_id=?1),(SELECT COUNT(*) FROM durable_job_reconciliations WHERE job_id=?1) FROM durable_jobs WHERE id=?1",
            [&uncertain.job_id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .unwrap();
    drop(raw);
    let mut forged = spoolman_receipt.clone();
    forged.receipt_id = "forged-spoolman".into();
    assert!(reconcile(&owner, &uncertain, Decision::ConfirmSucceeded, Some(forged),).is_err());
    assert!(reconcile(&owner, &uncertain, Decision::ConfirmNoEffect, None).is_err());
    let raw = readonly(&path);
    let after: (String, i64, i64) = raw
        .query_row(
            "SELECT document,(SELECT COUNT(*) FROM durable_job_history WHERE job_id=?1),(SELECT COUNT(*) FROM durable_job_reconciliations WHERE job_id=?1) FROM durable_jobs WHERE id=?1",
            [&uncertain.job_id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .unwrap();
    assert_eq!(after, before);
    drop(raw);
    let abandoned = job(reconcile(&owner, &uncertain, Decision::Abandon, None).unwrap());
    assert_eq!(abandoned.state, PersistentState::ReconciliationRequired);
    assert!(abandoned.effects.iter().all(|effect| !effect.no_effect));
    let audit = reconciliations(&owner, &abandoned.job_id);
    assert_eq!(audit[0].decision, "Abandon");
    assert_eq!(audit[0].receipt, None);
    let settled = job(reconcile(
        &owner,
        &abandoned,
        Decision::ConfirmSucceeded,
        Some(spoolman_receipt),
    )
    .unwrap());
    assert_eq!(settled.state, PersistentState::UploadedOnly);
    owner.shutdown().unwrap();
}
#[test]
fn denying_spoolman_deduction_preserves_confirmed_print() {
    let (_, owner) = fixture();
    enqueue(&owner, "started-print", printer("started-printer"));
    let worker = owner.job_worker(admission()).unwrap();
    let claimed_attempt!(_, mut lease) = worker.claim().unwrap().unwrap();
    begin_and_confirm(
        &worker,
        &mut lease,
        intent(EffectOperation::PrinterUpload, "started-printer"),
        "upload-receipt",
    );
    let start_receipt = begin_and_confirm(
        &worker,
        &mut lease,
        intent(EffectOperation::PrinterStart, "started-printer"),
        "start-receipt",
    );
    worker
        .update(
            &mut lease,
            WorkerOperation::BeginEffect(intent(EffectOperation::SpoolmanDeduction, "spool:3")),
        )
        .unwrap();
    let uncertain = worker.update(&mut lease, WorkerOperation::Fail).unwrap();
    let settled = job(reconcile(&owner, &uncertain, Decision::ConfirmNoEffect, None).unwrap());
    assert_eq!(settled.state, PersistentState::Succeeded);
    assert_eq!(settled.result, Some(start_receipt));
    assert!(settled.effects[0].confirmed);
    assert!(settled.effects[1].confirmed);
    assert!(settled.effects[2].no_effect);
    let audit = reconciliations(&owner, &settled.job_id);
    assert_eq!(audit.len(), 1);
    assert_eq!(audit[0].decision, "ConfirmNoEffect");
    assert_eq!(audit[0].receipt, None);
    let stored = get(&owner, &settled.job_id);
    assert_eq!(stored.state, PersistentState::Succeeded);
    assert_eq!(stored.result, settled.result);
    assert!(stored.effects[2].no_effect);
    owner.shutdown().unwrap();
}
#[test]
fn unresolved_spoolman_settles_by_either_explicit_decision() {
    let (_, owner) = fixture();
    let worker = owner.job_worker(admission()).unwrap();
    for (index, (start_confirmed, decision)) in [false, true]
        .into_iter()
        .flat_map(|start_confirmed| {
            [Decision::ConfirmNoEffect, Decision::ConfirmSucceeded]
                .into_iter()
                .map(move |decision| (start_confirmed, decision))
        })
        .enumerate()
    {
        let printer_id = format!("unresolved-spoolman-printer-{index}");
        enqueue(
            &owner,
            &format!("unresolved-spoolman-{index}"),
            printer(&printer_id),
        );
        let claimed_attempt!(_, mut lease) = worker.claim().unwrap().unwrap();
        let upload = distinct_intent(
            EffectOperation::PrinterUpload,
            &printer_id,
            30 + index as u64,
        );
        let upload_receipt = begin_and_confirm(
            &worker,
            &mut lease,
            upload,
            &format!("upload-receipt-3{index}"),
        );
        let primary_receipt = if start_confirmed {
            let start = distinct_intent(
                EffectOperation::PrinterStart,
                &printer_id,
                35 + index as u64,
            );
            begin_and_confirm(
                &worker,
                &mut lease,
                start,
                &format!("start-receipt-3{index}"),
            )
        } else {
            upload_receipt
        };
        let spoolman = distinct_intent(
            EffectOperation::SpoolmanDeduction,
            &format!("spoolman:3{index}"),
            40 + index as u64,
        );
        let spoolman_receipt = distinct_receipt(&spoolman, &format!("spoolman-receipt-3{index}"));
        worker
            .update(&mut lease, WorkerOperation::BeginEffect(spoolman))
            .unwrap();
        let uncertain = worker.update(&mut lease, WorkerOperation::Fail).unwrap();
        let supplied = (decision == Decision::ConfirmSucceeded).then_some(spoolman_receipt.clone());
        let settled = job(reconcile(&owner, &uncertain, decision, supplied).unwrap());
        assert_eq!(
            settled.state,
            if start_confirmed {
                PersistentState::Succeeded
            } else {
                PersistentState::UploadedOnly
            }
        );
        assert_eq!(settled.result, Some(primary_receipt));
        let spoolman_effect = settled.effects.last().unwrap();
        assert_eq!(
            spoolman_effect.no_effect,
            decision == Decision::ConfirmNoEffect
        );
        assert_eq!(
            spoolman_effect.receipt,
            (decision == Decision::ConfirmSucceeded).then_some(spoolman_receipt.clone())
        );
        let audit = reconciliations(&owner, &settled.job_id);
        assert_eq!(audit.len(), 1);
        assert_eq!(audit[0].decision, format!("{decision:?}"));
        assert_eq!(
            audit[0].receipt,
            (decision == Decision::ConfirmSucceeded).then_some(spoolman_receipt)
        );
    }
    owner.shutdown().unwrap();
}
#[test]
fn spoolman_before_upload_settles_without_order_dependence() {
    let (_, owner) = fixture();
    let worker = owner.job_worker(admission()).unwrap();
    for (index, confirm_upload) in [true, false].into_iter().enumerate() {
        let printer_id = format!("spoolman-first-printer-{index}");
        enqueue(
            &owner,
            &format!("spoolman-first-{index}"),
            printer(&printer_id),
        );
        let claimed_attempt!(_, mut lease) = worker.claim().unwrap().unwrap();
        let spoolman = distinct_intent(
            EffectOperation::SpoolmanDeduction,
            &format!("spoolman:first:{index}"),
            50 + index as u64,
        );
        begin_and_confirm(
            &worker,
            &mut lease,
            spoolman,
            &format!("spoolman-first-receipt-{index}"),
        );
        let upload = distinct_intent(
            EffectOperation::PrinterUpload,
            &printer_id,
            60 + index as u64,
        );
        let upload_receipt = distinct_receipt(&upload, &format!("upload-last-receipt-{index}"));
        worker
            .update(&mut lease, WorkerOperation::BeginEffect(upload))
            .unwrap();
        if confirm_upload {
            worker
                .update(
                    &mut lease,
                    WorkerOperation::ConfirmEffect(upload_receipt.clone()),
                )
                .unwrap();
        }
        let uncertain = worker.update(&mut lease, WorkerOperation::Fail).unwrap();
        let (decision, supplied) = if confirm_upload {
            (Decision::ConfirmSucceeded, Some(upload_receipt.clone()))
        } else {
            (Decision::ConfirmNoEffect, None)
        };
        let settled = job(reconcile(&owner, &uncertain, decision, supplied).unwrap());
        if confirm_upload {
            assert_eq!(settled.state, PersistentState::UploadedOnly);
            assert_eq!(settled.result, Some(upload_receipt));
        } else {
            assert_eq!(settled.state, PersistentState::Failed);
            assert!(settled.effects[1].no_effect);
            assert!(settled.effects[0].confirmed);
        }
    }
    owner.shutdown().unwrap();
}
#[test]
fn reconciliation_does_not_infer_start_or_combined_outcomes() {
    let (_, owner) = fixture();
    let worker = owner.job_worker(admission()).unwrap();
    enqueue(
        &owner,
        "combined-denied",
        printer("combined-denied-printer"),
    );
    let claimed_attempt!(_, mut lease) = worker.claim().unwrap().unwrap();
    let combined = distinct_intent(
        EffectOperation::PrinterUploadAndStart,
        "combined-denied-printer",
        70,
    );
    worker
        .update(&mut lease, WorkerOperation::BeginEffect(combined))
        .unwrap();
    let uncertain = worker.update(&mut lease, WorkerOperation::Fail).unwrap();
    let failed = job(reconcile(&owner, &uncertain, Decision::ConfirmNoEffect, None).unwrap());
    assert_eq!(failed.state, PersistentState::Failed);
    assert!(failed.effects[0].no_effect);
    assert!(failed.result.is_none());

    enqueue(&owner, "unknown-start", printer("unknown-start-printer"));
    let claimed_attempt!(_, mut lease) = worker.claim().unwrap().unwrap();
    let upload = distinct_intent(EffectOperation::PrinterUpload, "unknown-start-printer", 71);
    begin_and_confirm(&worker, &mut lease, upload, "unknown-start-upload");
    let start = distinct_intent(EffectOperation::PrinterStart, "unknown-start-printer", 72);
    worker
        .update(&mut lease, WorkerOperation::BeginEffect(start))
        .unwrap();
    let uncertain = worker.update(&mut lease, WorkerOperation::Fail).unwrap();
    let abandoned = job(reconcile(&owner, &uncertain, Decision::Abandon, None).unwrap());
    assert_eq!(abandoned.state, PersistentState::ReconciliationRequired);
    assert!(!abandoned.effects[1].no_effect);
    assert!(!abandoned.effects[1].confirmed);

    let mut no_start = printer("no-start-spoolman-printer");
    let Payload::PrinterUpload { start, .. } = &mut no_start else {
        unreachable!()
    };
    *start = false;
    enqueue(&owner, "no-start-spoolman", no_start);
    let claimed_attempt!(_, mut lease) = worker.claim().unwrap().unwrap();
    let upload = distinct_intent(
        EffectOperation::PrinterUpload,
        "no-start-spoolman-printer",
        73,
    );
    begin_and_confirm(&worker, &mut lease, upload, "no-start-upload");
    let spoolman = distinct_intent(EffectOperation::SpoolmanDeduction, "spoolman:no-start", 74);
    let spoolman_receipt =
        begin_and_confirm(&worker, &mut lease, spoolman, "no-start-spoolman-receipt");
    let uncertain = worker.update(&mut lease, WorkerOperation::Fail).unwrap();
    let succeeded = job(reconcile(
        &owner,
        &uncertain,
        Decision::ConfirmSucceeded,
        Some(spoolman_receipt),
    )
    .unwrap());
    assert_eq!(succeeded.state, PersistentState::Succeeded);
    owner.shutdown().unwrap();
}
#[test]
fn archived_uploaded_only_document_keeps_denied_and_confirmed_effect_facts() {
    let (path, owner) = fixture();
    enqueue(
        &owner,
        "archive-facts-parent",
        printer("archive-facts-printer"),
    );
    let worker = owner.job_worker(admission()).unwrap();
    let claimed_attempt!(_, mut lease) = worker.claim().unwrap().unwrap();
    let upload = distinct_intent(EffectOperation::PrinterUpload, "archive-facts-printer", 80);
    let upload_receipt =
        begin_and_confirm(&worker, &mut lease, upload, "archive-facts-upload-receipt");
    let spoolman = distinct_intent(
        EffectOperation::SpoolmanDeduction,
        "spoolman:archive-facts",
        81,
    );
    begin_and_confirm(
        &worker,
        &mut lease,
        spoolman,
        "archive-facts-spoolman-receipt",
    );
    let start = distinct_intent(EffectOperation::PrinterStart, "archive-facts-printer", 82);
    worker
        .update(&mut lease, WorkerOperation::BeginEffect(start))
        .unwrap();
    let uncertain = worker.update(&mut lease, WorkerOperation::Fail).unwrap();
    let parent = job(reconcile(&owner, &uncertain, Decision::ConfirmNoEffect, None).unwrap());
    assert_eq!(parent.state, PersistentState::UploadedOnly);
    assert_eq!(parent.result, Some(upload_receipt));
    assert!(parent.effects[1].confirmed);
    assert!(parent.effects[2].no_effect);
    let before: String = readonly(&path)
        .query_row(
            "SELECT document FROM durable_jobs WHERE id=?1",
            [&parent.job_id],
            |row| row.get(0),
        )
        .unwrap();
    owner.shutdown().unwrap();
    let owner = WriterOwner::open(&path, Limits::default()).unwrap().0;
    let after: String = readonly(&path)
        .query_row(
            "SELECT document FROM durable_jobs WHERE id=?1",
            [&parent.job_id],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(after, before);
    owner.shutdown().unwrap();
    let raw = Connection::open(path.join("print-partner.db")).unwrap();
    raw.execute(
        "UPDATE durable_jobs SET updated=updated-172800, document=json_set(document,'$.updated_at',updated-172800) WHERE id=?1",
        [&parent.job_id],
    )
    .unwrap();
    drop(raw);
    let owner = WriterOwner::open(&path, Limits::default()).unwrap().0;
    assert_eq!(owner.retain_jobs(1, 1).unwrap(), 1);
    assert_eq!(
        readonly(&path)
            .query_row(
                "SELECT COUNT(*) FROM durable_job_reconciliations WHERE job_id=?1",
                [&parent.job_id],
                |row| row.get::<_, i64>(0),
            )
            .unwrap(),
        0
    );
    let child = job(enqueue_start(&owner, "archive-facts-child", &parent.job_id).unwrap());
    let Payload::PrinterStart(request) = child.payload else {
        panic!("printer start payload")
    };
    assert_eq!(request.upload_effect(), Some(&parent.effects[0]));
    assert_eq!(
        request.upload_effect().unwrap().receipt.as_ref(),
        parent.result.as_ref()
    );
    owner.shutdown().unwrap();
}
#[test]
fn effect_outcome_wire_is_legacy_compatible_and_omits_false() {
    let unresolved = r#"{"intent":{"operation":"printer_upload","basis_hash":"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa","content_hash":"bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb","target":"printer"},"attempt":1,"generation":1,"confirmed":false,"receipt":null}"#;
    let unresolved_effect: EffectReceipt = serde_json::from_str(unresolved).unwrap();
    assert!(!unresolved_effect.no_effect);
    assert_eq!(
        serde_json::to_string(&unresolved_effect).unwrap(),
        unresolved
    );
    let confirmed = r#"{"intent":{"operation":"printer_upload","basis_hash":"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa","content_hash":"bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb","target":"printer"},"attempt":1,"generation":1,"confirmed":true,"receipt":{"receipt_id":"legacy","content_hash":"bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb","target":"printer"}}"#;
    let confirmed_effect: EffectReceipt = serde_json::from_str(confirmed).unwrap();
    assert!(!confirmed_effect.no_effect);
    assert_eq!(serde_json::to_string(&confirmed_effect).unwrap(), confirmed);
    assert!(
        !serde_json::to_string(&receipt("printer"))
            .unwrap()
            .contains("no_effect")
    );

    let (path, owner) = fixture();
    enqueue(&owner, "legacy-reconcile", printer("legacy-printer"));
    let worker = owner.job_worker(admission()).unwrap();
    let claimed_attempt!(_, mut lease) = worker.claim().unwrap().unwrap();
    worker
        .update(
            &mut lease,
            WorkerOperation::BeginEffect(intent(EffectOperation::PrinterUpload, "legacy-printer")),
        )
        .unwrap();
    let uncertain = worker.update(&mut lease, WorkerOperation::Fail).unwrap();
    assert!(
        !serde_json::to_string(&uncertain)
            .unwrap()
            .contains("no_effect")
    );
    owner.shutdown().unwrap();
    let owner = WriterOwner::open(&path, Limits::default()).unwrap().0;
    let recovered = get(&owner, &uncertain.job_id);
    let settled = job(reconcile(
        &owner,
        &recovered,
        Decision::ConfirmSucceeded,
        Some(receipt("legacy-printer")),
    )
    .unwrap());
    assert_eq!(settled.state, PersistentState::UploadedOnly);
    owner.shutdown().unwrap();
}
#[test]
fn invalid_persisted_effect_outcomes_fail_closed() {
    let (path, owner) = fixture();
    let parent = denied_start_parent(&owner, "tamper-unresolved", "tamper-unresolved-printer", 90);
    owner.shutdown().unwrap();
    let raw = Connection::open(path.join("print-partner.db")).unwrap();
    raw.execute(
        "UPDATE durable_jobs SET document=json_remove(document,'$.effects[1].no_effect') WHERE id=?1",
        [&parent.job_id],
    )
    .unwrap();
    drop(raw);
    assert_invalid_job_skipped_on_restart(&path, &parent.job_id);

    let (path, owner) = fixture();
    let parent = denied_start_parent(&owner, "tamper-conflict", "tamper-conflict-printer", 100);
    owner.shutdown().unwrap();
    let raw = Connection::open(path.join("print-partner.db")).unwrap();
    raw.execute(
        "UPDATE durable_jobs SET document=json_set(document,'$.effects[0].no_effect',json('true')) WHERE id=?1",
        [&parent.job_id],
    )
    .unwrap();
    drop(raw);
    assert_invalid_job_skipped_on_restart(&path, &parent.job_id);

    let (path, owner) = fixture();
    let parent = denied_start_parent(&owner, "tamper-running", "tamper-running-printer", 110);
    owner.shutdown().unwrap();
    let raw = Connection::open(path.join("print-partner.db")).unwrap();
    raw.execute(
        "UPDATE durable_jobs SET state='running',document=json_set(document,'$.state','running') WHERE id=?1",
        [&parent.job_id],
    )
    .unwrap();
    drop(raw);
    assert_invalid_job_skipped_on_restart(&path, &parent.job_id);

    for (index, operation) in ["printer_start", "printer_upload_and_start"]
        .into_iter()
        .enumerate()
    {
        let (path, owner) = fixture();
        let parent = denied_start_parent(
            &owner,
            &format!("tamper-confirmed-{index}"),
            &format!("tamper-confirmed-printer-{index}"),
            120 + index as u64,
        );
        owner.shutdown().unwrap();
        let raw = Connection::open(path.join("print-partner.db")).unwrap();
        raw.execute(
            "UPDATE durable_jobs SET document=json_set(json_remove(document,'$.effects[1].no_effect'),'$.effects[1].confirmed',json('true'),'$.effects[1].intent.operation',?2,'$.effects[1].receipt',json_object('receipt_id',?3,'content_hash',json_extract(document,'$.effects[1].intent.content_hash'),'target',json_extract(document,'$.effects[1].intent.target'))) WHERE id=?1",
            rusqlite::params![parent.job_id, operation, format!("tampered-confirmed-{index}")],
        )
        .unwrap();
        drop(raw);
        assert_invalid_job_skipped_on_restart(&path, &parent.job_id);
    }

    let (path, owner) = fixture();
    let parent = denied_start_parent(&owner, "tamper-receipt", "tamper-receipt-printer", 112);
    owner.shutdown().unwrap();
    let raw = Connection::open(path.join("print-partner.db")).unwrap();
    raw.execute(
        "UPDATE durable_jobs SET document=json_set(document,'$.effects[0].receipt.content_hash',?2) WHERE id=?1",
        rusqlite::params![parent.job_id, "f".repeat(64)],
    )
    .unwrap();
    drop(raw);
    assert_invalid_job_skipped_on_restart(&path, &parent.job_id);

    let (path, owner) = fixture();
    let parent = denied_start_parent(&owner, "tamper-operation", "tamper-operation-printer", 115);
    owner.shutdown().unwrap();
    let raw = Connection::open(path.join("print-partner.db")).unwrap();
    raw.execute(
        "UPDATE durable_jobs SET document=json_set(document,'$.effects[1].intent.operation','local_artifact') WHERE id=?1",
        [&parent.job_id],
    )
    .unwrap();
    drop(raw);
    assert_invalid_job_skipped_on_restart(&path, &parent.job_id);
}
#[test]
fn uploaded_only_result_tamper_rejects_decode_and_child_binding() {
    let (path, owner) = fixture();
    enqueue(&owner, "tamper-result", printer("tamper-result-printer"));
    let worker = owner.job_worker(admission()).unwrap();
    let claimed_attempt!(_, mut lease) = worker.claim().unwrap().unwrap();
    let upload = distinct_intent(EffectOperation::PrinterUpload, "tamper-result-printer", 120);
    begin_and_confirm(&worker, &mut lease, upload, "tamper-upload-receipt");
    let spoolman = distinct_intent(
        EffectOperation::SpoolmanDeduction,
        "spoolman:tamper-result",
        121,
    );
    let spoolman_receipt =
        begin_and_confirm(&worker, &mut lease, spoolman, "tamper-spoolman-receipt");
    let uncertain = worker.update(&mut lease, WorkerOperation::Fail).unwrap();
    let parent = job(reconcile(
        &owner,
        &uncertain,
        Decision::ConfirmSucceeded,
        Some(spoolman_receipt),
    )
    .unwrap());
    let raw = Connection::open(path.join("print-partner.db")).unwrap();
    raw.execute(
        "UPDATE durable_jobs SET document=json_set(document,'$.result',json_extract(document,'$.effects[1].receipt')) WHERE id=?1",
        [&parent.job_id],
    )
    .unwrap();
    drop(raw);
    assert!(enqueue_start(&owner, "tampered-result-child", &parent.job_id).is_err());
    owner.shutdown().unwrap();
    assert_invalid_job_skipped_on_restart(&path, &parent.job_id);
}
#[test]
fn retained_parent_rejects_embedded_job_id_mismatch_without_writes() {
    let (path, owner) = fixture();
    let parent = uploaded_only_parent(&owner, "live-id-parent", "live-id-printer");
    let raw = Connection::open(path.join("print-partner.db")).unwrap();
    raw.execute(
        "UPDATE durable_jobs SET document=json_set(document,'$.job_id','embedded-other-id') WHERE id=?1",
        [&parent.job_id],
    )
    .unwrap();
    let before: (String, i64, i64) = raw
        .query_row(
            "SELECT document,(SELECT COUNT(*) FROM durable_job_history),(SELECT COUNT(*) FROM durable_job_keys) FROM durable_jobs WHERE id=?1",
            [&parent.job_id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .unwrap();
    drop(raw);
    assert!(enqueue_start(&owner, "live-id-child", &parent.job_id).is_err());
    let raw = readonly(&path);
    let after: (String, i64, i64) = raw
        .query_row(
            "SELECT document,(SELECT COUNT(*) FROM durable_job_history),(SELECT COUNT(*) FROM durable_job_keys) FROM durable_jobs WHERE id=?1",
            [&parent.job_id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .unwrap();
    assert_eq!(after, before);
    drop(raw);
    owner.shutdown().unwrap();

    let (path, owner) = fixture();
    let parent = uploaded_only_parent(&owner, "archive-id-parent", "archive-id-printer");
    owner.shutdown().unwrap();
    let raw = Connection::open(path.join("print-partner.db")).unwrap();
    raw.execute(
        "UPDATE durable_jobs SET updated=updated-172800,document=json_set(document,'$.updated_at',updated-172800) WHERE id=?1",
        [&parent.job_id],
    )
    .unwrap();
    drop(raw);
    let owner = WriterOwner::open(&path, Limits::default()).unwrap().0;
    assert_eq!(owner.retain_jobs(1, 1).unwrap(), 1);
    let raw = Connection::open(path.join("print-partner.db")).unwrap();
    raw.execute(
        "UPDATE durable_job_keys SET archived_document=json_set(archived_document,'$.job_id','embedded-other-id') WHERE job_id=?1",
        [&parent.job_id],
    )
    .unwrap();
    let before: (String, i64, i64) = raw
        .query_row(
            "SELECT archived_document,(SELECT COUNT(*) FROM durable_jobs),(SELECT COUNT(*) FROM durable_job_history) FROM durable_job_keys WHERE job_id=?1",
            [&parent.job_id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .unwrap();
    drop(raw);
    assert!(enqueue_start(&owner, "archive-id-child", &parent.job_id).is_err());
    let raw = readonly(&path);
    let after: (String, i64, i64) = raw
        .query_row(
            "SELECT archived_document,(SELECT COUNT(*) FROM durable_jobs),(SELECT COUNT(*) FROM durable_job_history) FROM durable_job_keys WHERE job_id=?1",
            [&parent.job_id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .unwrap();
    assert_eq!(after, before);
    drop(raw);
    owner.shutdown().unwrap();
}
#[test]
fn denied_start_child_failure_allows_only_a_fresh_deliberate_key() {
    let (_, owner) = fixture();
    let parent = uploaded_only_parent(&owner, "denied-child-parent", "denied-child-printer");
    let parent_before = serde_json::to_vec(&parent).unwrap();
    let child = job(enqueue_start(&owner, "denied-child", &parent.job_id).unwrap());
    let Payload::PrinterStart(request) = &child.payload else {
        panic!("printer start payload")
    };
    let upload = request.upload_effect().unwrap().clone();
    let mut start = upload.intent.clone();
    start.operation = EffectOperation::PrinterStart;
    let worker = owner.job_worker(admission()).unwrap();
    let claimed_attempt!(claimed, mut lease) = worker.claim().unwrap().unwrap();
    assert_eq!(claimed.job_id, child.job_id);
    worker
        .update(&mut lease, WorkerOperation::BeginEffect(start))
        .unwrap();
    let uncertain = worker.update(&mut lease, WorkerOperation::Fail).unwrap();
    assert!(enqueue_start(&owner, "denied-child-fresh", &parent.job_id).is_err());
    assert_eq!(
        job(enqueue_start(&owner, "denied-child", &parent.job_id).unwrap()).job_id,
        child.job_id
    );
    let failed = job(reconcile(&owner, &uncertain, Decision::ConfirmNoEffect, None).unwrap());
    assert_eq!(failed.state, PersistentState::Failed);
    assert_eq!(failed.effects.len(), 1);
    assert_eq!(
        failed.effects[0].intent.operation,
        EffectOperation::PrinterStart
    );
    assert!(failed.effects[0].no_effect);
    assert!(!failed.effects[0].confirmed);
    assert!(failed.effects[0].receipt.is_none());
    assert_eq!(
        job(enqueue_start(&owner, "denied-child", &parent.job_id).unwrap()).job_id,
        child.job_id
    );
    let retry = job(enqueue_start(&owner, "denied-child-fresh", &parent.job_id).unwrap());
    assert_ne!(retry.job_id, child.job_id);
    assert!(retry.effects.is_empty());
    assert_eq!(
        serde_json::to_vec(&get(&owner, &parent.job_id)).unwrap(),
        parent_before
    );
    owner.shutdown().unwrap();
}
#[test]
fn persisted_effect_outcome_truth_table_accepts_exactly_three_shapes() {
    let (path, owner) = fixture();
    let worker = owner.job_worker(admission()).unwrap();
    let cases = [
        (false, false, false, true),
        (false, false, true, false),
        (false, true, false, true),
        (false, true, true, false),
        (true, false, false, false),
        (true, false, true, true),
        (true, true, false, false),
        (true, true, true, false),
    ];
    for (index, (confirmed, no_effect, has_receipt, valid)) in cases.into_iter().enumerate() {
        let printer_id = format!("outcome-printer-{index}");
        let queued = enqueue(&owner, &format!("outcome-{index}"), printer(&printer_id));
        let claimed_attempt!(claimed, mut lease) = worker.claim().unwrap().unwrap();
        assert_eq!(claimed.job_id, queued.job_id);
        let combined = distinct_intent(
            EffectOperation::PrinterUploadAndStart,
            &printer_id,
            300 + index as u64,
        );
        worker
            .update(&mut lease, WorkerOperation::BeginEffect(combined))
            .unwrap();
        let uncertain = worker.update(&mut lease, WorkerOperation::Fail).unwrap();
        let failed = job(reconcile(&owner, &uncertain, Decision::ConfirmNoEffect, None).unwrap());
        assert_eq!(failed.state, PersistentState::Failed);
        let raw = Connection::open(path.join("print-partner.db")).unwrap();
        let document: String = raw
            .query_row(
                "SELECT document FROM durable_jobs WHERE id=?1",
                [&failed.job_id],
                |row| row.get(0),
            )
            .unwrap();
        let mut document: serde_json::Value = serde_json::from_str(&document).unwrap();
        document["effects"][0]["confirmed"] = confirmed.into();
        document["effects"][0]["no_effect"] = no_effect.into();
        document["effects"][0]["receipt"] = if has_receipt {
            serde_json::json!({
                "receipt_id": format!("outcome-receipt-{index}"),
                "content_hash": document["effects"][0]["intent"]["content_hash"],
                "target": document["effects"][0]["intent"]["target"]
            })
        } else {
            serde_json::Value::Null
        };
        raw.execute(
            "UPDATE durable_jobs SET document=?2 WHERE id=?1",
            rusqlite::params![failed.job_id, document.to_string()],
        )
        .unwrap();
        drop(raw);
        assert_eq!(
            call(
                &owner,
                UserOperation::Get {
                    job_id: failed.job_id,
                },
            )
            .is_ok(),
            valid,
            "outcome case {index}"
        );
    }
    owner.shutdown().unwrap();
}
#[test]
fn duplicate_confirmed_upload_parent_rejects_read_and_bind_but_allows_restart() {
    let (path, owner) = fixture();
    let parent = uploaded_only_parent(&owner, "duplicate-upload-parent", "duplicate-printer");
    let raw = Connection::open(path.join("print-partner.db")).unwrap();
    raw.execute(
        "UPDATE durable_jobs SET document=json_insert(document,'$.effects[#]',json_extract(document,'$.effects[0]')) WHERE id=?1",
        [&parent.job_id],
    )
    .unwrap();
    let before: (String, i64, i64, i64) = raw
        .query_row(
            "SELECT document,(SELECT COUNT(*) FROM durable_jobs),(SELECT COUNT(*) FROM durable_job_history),(SELECT COUNT(*) FROM durable_job_keys) FROM durable_jobs WHERE id=?1",
            [&parent.job_id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )
        .unwrap();
    drop(raw);
    assert!(
        call(
            &owner,
            UserOperation::Get {
                job_id: parent.job_id.clone(),
            },
        )
        .is_err()
    );
    assert!(enqueue_start(&owner, "duplicate-upload-child", &parent.job_id).is_err());
    let after: (String, i64, i64, i64) = readonly(&path)
        .query_row(
            "SELECT document,(SELECT COUNT(*) FROM durable_jobs),(SELECT COUNT(*) FROM durable_job_history),(SELECT COUNT(*) FROM durable_job_keys) FROM durable_jobs WHERE id=?1",
            [&parent.job_id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )
        .unwrap();
    assert_eq!(after, before);
    owner.shutdown().unwrap();
    assert_invalid_job_skipped_on_restart(&path, &parent.job_id);
}
#[test]
fn uploaded_only_persisted_proof_rejects_independent_corruptions() {
    let (path, owner) = fixture();
    let parent = denied_start_parent(
        &owner,
        "proof-corruption-parent",
        "proof-corruption-printer",
        400,
    );
    let raw = Connection::open(path.join("print-partner.db")).unwrap();
    let original: String = raw
        .query_row(
            "SELECT document FROM durable_jobs WHERE id=?1",
            [&parent.job_id],
            |row| row.get(0),
        )
        .unwrap();
    for case in 0..4 {
        let mut document: serde_json::Value = serde_json::from_str(&original).unwrap();
        match case {
            0 => {
                let denied_start = document["effects"][1].clone();
                document["effects"]
                    .as_array_mut()
                    .unwrap()
                    .push(denied_start);
            }
            1 => document["payload"]["payload"]["printer_id"] = "other-printer".into(),
            2 => document["effects"]
                .as_array_mut()
                .unwrap()
                .push(serde_json::json!({
                    "intent": {
                        "operation": "spoolman_deduction",
                        "basis_hash": "c".repeat(64),
                        "content_hash": "d".repeat(64),
                        "target": "spoolman:unresolved"
                    },
                    "attempt": 1,
                    "generation": 1,
                    "confirmed": false,
                    "receipt": null
                })),
            3 => document["payload"]["payload"]["start"] = false.into(),
            _ => unreachable!(),
        }
        raw.execute(
            "UPDATE durable_jobs SET document=?2 WHERE id=?1",
            rusqlite::params![parent.job_id, document.to_string()],
        )
        .unwrap();
        assert!(
            call(
                &owner,
                UserOperation::Get {
                    job_id: parent.job_id.clone(),
                },
            )
            .is_err(),
            "proof corruption {case}"
        );
        assert!(
            enqueue_start(
                &owner,
                &format!("proof-corruption-child-{case}"),
                &parent.job_id,
            )
            .is_err(),
            "proof binding {case}"
        );
    }
    raw.execute(
        "UPDATE durable_jobs SET document=?2 WHERE id=?1",
        rusqlite::params![parent.job_id, original],
    )
    .unwrap();
    drop(raw);
    assert_eq!(
        get(&owner, &parent.job_id).state,
        PersistentState::UploadedOnly
    );
    owner.shutdown().unwrap();
}
#[test]
fn non_uploaded_confirmed_receipt_mismatch_fails_get() {
    let (path, owner) = fixture();
    let worker = owner.job_worker(admission()).unwrap();
    let mut no_start = printer("receipt-mismatch-printer");
    let Payload::PrinterUpload { start, .. } = &mut no_start else {
        unreachable!()
    };
    *start = false;
    let queued = enqueue(&owner, "receipt-mismatch", no_start);
    let claimed_attempt!(claimed, mut lease) = worker.claim().unwrap().unwrap();
    assert_eq!(claimed.job_id, queued.job_id);
    let upload = distinct_intent(
        EffectOperation::PrinterUpload,
        "receipt-mismatch-printer",
        500,
    );
    let upload_receipt = begin_and_confirm(&worker, &mut lease, upload, "receipt-mismatch");
    let succeeded = worker
        .update(&mut lease, WorkerOperation::Finish(Some(upload_receipt)))
        .unwrap();
    assert_eq!(succeeded.state, PersistentState::Succeeded);
    let raw = Connection::open(path.join("print-partner.db")).unwrap();
    raw.execute(
        "UPDATE durable_jobs SET document=json_set(document,'$.effects[0].receipt.content_hash',?2) WHERE id=?1",
        rusqlite::params![succeeded.job_id, "f".repeat(64)],
    )
    .unwrap();
    drop(raw);
    assert!(
        call(
            &owner,
            UserOperation::Get {
                job_id: succeeded.job_id,
            },
        )
        .is_err()
    );
    owner.shutdown().unwrap();
}
#[test]
fn denied_effect_on_running_fails_get() {
    let (path, owner) = fixture();
    let worker = owner.job_worker(admission()).unwrap();

    let denied = enqueue(&owner, "running-denied", printer("running-denied-printer"));
    let claimed_attempt!(claimed, mut lease) = worker.claim().unwrap().unwrap();
    assert_eq!(claimed.job_id, denied.job_id);
    worker
        .update(
            &mut lease,
            WorkerOperation::BeginEffect(distinct_intent(
                EffectOperation::PrinterUploadAndStart,
                "running-denied-printer",
                501,
            )),
        )
        .unwrap();
    let uncertain = worker.update(&mut lease, WorkerOperation::Fail).unwrap();
    let failed = job(reconcile(&owner, &uncertain, Decision::ConfirmNoEffect, None).unwrap());
    let raw = Connection::open(path.join("print-partner.db")).unwrap();
    raw.execute(
        "UPDATE durable_jobs SET document=json_set(document,'$.state','running') WHERE id=?1",
        [&failed.job_id],
    )
    .unwrap();
    drop(raw);
    assert!(
        call(
            &owner,
            UserOperation::Get {
                job_id: failed.job_id,
            },
        )
        .is_err()
    );
    owner.shutdown().unwrap();
}
#[test]
fn live_child_binding_corruption_fails_get() {
    let (path, owner) = fixture();

    let parent = uploaded_only_parent(&owner, "live-binding-parent", "live-binding-printer");
    let child = job(enqueue_start(&owner, "live-binding-child", &parent.job_id).unwrap());
    let raw = Connection::open(path.join("print-partner.db")).unwrap();
    raw.execute(
        "UPDATE durable_jobs SET document=json_set(document,'$.payload.payload.binding.upload_effect.no_effect',json('true')) WHERE id=?1",
        [&child.job_id],
    )
    .unwrap();
    drop(raw);
    assert!(
        call(
            &owner,
            UserOperation::Get {
                job_id: child.job_id,
            },
        )
        .is_err()
    );
    owner.shutdown().unwrap();
}
#[test]
fn archived_parent_tenant_and_child_binding_corruptions_fail_without_writes() {
    let (path, owner) = fixture();
    let parent = uploaded_only_parent(&owner, "archive-tenant-parent", "archive-tenant-printer");
    owner.shutdown().unwrap();
    let raw = Connection::open(path.join("print-partner.db")).unwrap();
    raw.execute(
        "UPDATE durable_jobs SET updated=updated-172800,document=json_set(document,'$.updated_at',updated-172800) WHERE id=?1",
        [&parent.job_id],
    )
    .unwrap();
    drop(raw);
    let owner = WriterOwner::open(&path, Limits::default()).unwrap().0;
    assert_eq!(owner.retain_jobs(1, 1).unwrap(), 1);
    let raw = Connection::open(path.join("print-partner.db")).unwrap();
    raw.execute(
        "UPDATE durable_job_keys SET archived_document=json_set(archived_document,'$.tenant','other-tenant') WHERE job_id=?1",
        [&parent.job_id],
    )
    .unwrap();
    let before: (String, i64, i64, i64) = raw
        .query_row(
            "SELECT archived_document,(SELECT COUNT(*) FROM durable_jobs),(SELECT COUNT(*) FROM durable_job_history),(SELECT COUNT(*) FROM durable_job_keys) FROM durable_job_keys WHERE job_id=?1",
            [&parent.job_id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )
        .unwrap();
    drop(raw);
    assert!(enqueue_start(&owner, "archive-tenant-child", &parent.job_id).is_err());
    let after: (String, i64, i64, i64) = readonly(&path)
        .query_row(
            "SELECT archived_document,(SELECT COUNT(*) FROM durable_jobs),(SELECT COUNT(*) FROM durable_job_history),(SELECT COUNT(*) FROM durable_job_keys) FROM durable_job_keys WHERE job_id=?1",
            [&parent.job_id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )
        .unwrap();
    assert_eq!(after, before);
    owner.shutdown().unwrap();

    let (path, owner) = fixture();
    let parent = uploaded_only_parent(&owner, "archive-binding-parent", "archive-binding-printer");
    let child = job(enqueue_start(&owner, "archive-binding-child", &parent.job_id).unwrap());
    call(
        &owner,
        UserOperation::Cancel {
            job_id: child.job_id.clone(),
        },
    )
    .unwrap();
    owner.shutdown().unwrap();
    let raw = Connection::open(path.join("print-partner.db")).unwrap();
    raw.execute(
        "UPDATE durable_jobs SET updated=updated-172800,document=json_set(document,'$.updated_at',updated-172800)",
        [],
    )
    .unwrap();
    drop(raw);
    let owner = WriterOwner::open(&path, Limits::default()).unwrap().0;
    assert_eq!(owner.retain_jobs(1, 1).unwrap(), 2);
    let raw = Connection::open(path.join("print-partner.db")).unwrap();
    let valid_document: String = raw
        .query_row(
            "SELECT archived_document FROM durable_job_keys WHERE job_id=?1",
            [&child.job_id],
            |row| row.get(0),
        )
        .unwrap();
    drop(raw);
    assert_eq!(
        job(enqueue_start(&owner, "archive-binding-child", &parent.job_id).unwrap()).job_id,
        child.job_id
    );
    let raw = Connection::open(path.join("print-partner.db")).unwrap();
    raw.execute(
        "UPDATE durable_job_keys SET archived_document=json_set(archived_document,'$.payload.payload.binding.upload_effect.no_effect',json('true')) WHERE job_id=?1",
        [&child.job_id],
    )
    .unwrap();
    let before: (String, i64, i64, i64) = raw
        .query_row(
            "SELECT archived_document,(SELECT COUNT(*) FROM durable_jobs),(SELECT COUNT(*) FROM durable_job_history),(SELECT COUNT(*) FROM durable_job_keys) FROM durable_job_keys WHERE job_id=?1",
            [&child.job_id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )
        .unwrap();
    drop(raw);
    assert!(enqueue_start(&owner, "archive-binding-child", &parent.job_id).is_err());
    let after: (String, i64, i64, i64) = readonly(&path)
        .query_row(
            "SELECT archived_document,(SELECT COUNT(*) FROM durable_jobs),(SELECT COUNT(*) FROM durable_job_history),(SELECT COUNT(*) FROM durable_job_keys) FROM durable_job_keys WHERE job_id=?1",
            [&child.job_id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )
        .unwrap();
    assert_eq!(after, before);
    let raw = Connection::open(path.join("print-partner.db")).unwrap();
    raw.execute(
        "UPDATE durable_job_keys SET archived_document=?2 WHERE job_id=?1",
        rusqlite::params![child.job_id, valid_document],
    )
    .unwrap();
    drop(raw);
    assert_eq!(
        job(enqueue_start(&owner, "archive-binding-child", &parent.job_id).unwrap()).job_id,
        child.job_id
    );
    owner.shutdown().unwrap();
}
#[test]
fn reachable_printer_reconciliation_truth_table_has_no_stranded_shape() {
    fn sequences(
        prefix: &mut Vec<EffectOperation>,
        remaining: &mut Vec<EffectOperation>,
        result: &mut Vec<Vec<EffectOperation>>,
    ) {
        if !prefix.is_empty() {
            result.push(prefix.clone());
        }
        for index in 0..remaining.len() {
            let operation = remaining.remove(index);
            prefix.push(operation);
            sequences(prefix, remaining, result);
            prefix.pop();
            remaining.insert(index, operation);
        }
    }
    let (_, owner) = fixture();
    let worker = owner.job_worker(admission()).unwrap();
    let mut operation_sequences = Vec::new();
    sequences(
        &mut Vec::new(),
        &mut vec![
            EffectOperation::SpoolmanDeduction,
            EffectOperation::PrinterUpload,
            EffectOperation::PrinterStart,
            EffectOperation::PrinterUploadAndStart,
        ],
        &mut operation_sequences,
    );
    let mut reachable = 0;
    let mut saw_spoolman_only = [false; 2];
    let mut saw_spoolman_first_upload = false;
    for start in [false, true] {
        for last_confirmed in [false, true] {
            for (case, operations) in operation_sequences.iter().enumerate() {
                let printer_id =
                    format!("truth-printer-{}-{last_confirmed}-{case}", u8::from(start));
                let mut payload = printer(&printer_id);
                let Payload::PrinterUpload {
                    start: payload_start,
                    ..
                } = &mut payload
                else {
                    unreachable!()
                };
                *payload_start = start;
                enqueue(
                    &owner,
                    &format!("truth-{}-{last_confirmed}-{case}", u8::from(start)),
                    payload,
                );
                let claimed_attempt!(_, mut lease) = worker.claim().unwrap().unwrap();
                let mut last_receipt = None;
                let mut scenario_receipts = Vec::new();
                let mut valid = true;
                for (index, operation) in operations.iter().copied().enumerate() {
                    let target = if operation == EffectOperation::SpoolmanDeduction {
                        format!("spoolman:truth:{case}:{index}")
                    } else {
                        printer_id.clone()
                    };
                    let effect_intent = distinct_intent(
                        operation,
                        &target,
                        1_000_000
                            + (u64::from(start) * 100_000)
                            + (u64::from(last_confirmed) * 50_000)
                            + (case as u64 * 100)
                            + index as u64,
                    );
                    let effect_receipt =
                        distinct_receipt(&effect_intent, &format!("truth-receipt-{case}-{index}"));
                    if worker
                        .update(&mut lease, WorkerOperation::BeginEffect(effect_intent))
                        .is_err()
                    {
                        valid = false;
                        break;
                    }
                    if index + 1 < operations.len() || last_confirmed {
                        worker
                            .update(
                                &mut lease,
                                WorkerOperation::ConfirmEffect(effect_receipt.clone()),
                            )
                            .unwrap();
                    }
                    scenario_receipts.push((operation, effect_receipt.clone()));
                    last_receipt = Some(effect_receipt);
                }
                if !valid {
                    worker.update(&mut lease, WorkerOperation::Fail).unwrap();
                    continue;
                }
                reachable += 1;
                let uncertain = worker.update(&mut lease, WorkerOperation::Fail).unwrap();
                let succeeded =
                    reconcile(&owner, &uncertain, Decision::ConfirmSucceeded, last_receipt);
                let terminal = match succeeded {
                    Ok(outcome) => job(outcome),
                    Err(_) => {
                        job(reconcile(&owner, &uncertain, Decision::ConfirmNoEffect, None).unwrap())
                    }
                };
                assert!(terminal.state.terminal());
                let confirmed = |operation| {
                    terminal.effects.iter().any(|effect| {
                        effect.intent.operation == operation
                            && effect.confirmed
                            && !effect.no_effect
                    })
                };
                let unresolved = |operation| {
                    terminal.effects.iter().any(|effect| {
                        effect.intent.operation == operation
                            && !effect.confirmed
                            && !effect.no_effect
                    })
                };
                let confirmed_upload = terminal
                    .effects
                    .iter()
                    .filter(|effect| {
                        effect.intent.operation == EffectOperation::PrinterUpload
                            && effect.confirmed
                            && !effect.no_effect
                    })
                    .count()
                    == 1;
                let start_proven = confirmed(EffectOperation::PrinterStart)
                    || confirmed(EffectOperation::PrinterUploadAndStart);
                let ordinary_success = start_proven || (!start && confirmed_upload);
                let uploaded_only = start
                    && confirmed_upload
                    && !start_proven
                    && !unresolved(EffectOperation::PrinterStart)
                    && !unresolved(EffectOperation::PrinterUploadAndStart)
                    && !terminal.effects.iter().any(|effect| {
                        effect.intent.operation == EffectOperation::PrinterUploadAndStart
                    });
                assert!(!(ordinary_success && uploaded_only));
                assert_eq!(
                    terminal.state,
                    if ordinary_success {
                        PersistentState::Succeeded
                    } else if uploaded_only {
                        PersistentState::UploadedOnly
                    } else {
                        PersistentState::Failed
                    }
                );
                if ordinary_success {
                    let expected_operation =
                        if operations.contains(&EffectOperation::PrinterUploadAndStart) {
                            EffectOperation::PrinterUploadAndStart
                        } else if start {
                            EffectOperation::PrinterStart
                        } else {
                            EffectOperation::PrinterUpload
                        };
                    let expected = scenario_receipts
                        .iter()
                        .find(|(operation, _)| *operation == expected_operation)
                        .map(|(_, receipt)| receipt.clone());
                    assert_eq!(terminal.result, expected);
                }
                if operations == &[EffectOperation::SpoolmanDeduction] {
                    saw_spoolman_only[usize::from(start)] = true;
                    assert_eq!(terminal.state, PersistentState::Failed);
                }
                if operations.first() == Some(&EffectOperation::SpoolmanDeduction)
                    && operations.contains(&EffectOperation::PrinterUpload)
                {
                    saw_spoolman_first_upload = true;
                }
            }
        }
    }
    assert_eq!(reachable, 30);
    assert_eq!(saw_spoolman_only, [true, true]);
    assert!(saw_spoolman_first_upload);
    owner.shutdown().unwrap();
}
#[test]
fn ticket_t_28_recovery_global_and_per_tenant_history_limits() {
    let (_, owner) = fixture();
    let (_, a) = register(&owner, "history-a@example.com");
    let (_, b) = register(&owner, "history-b@example.com");
    let worker = session_worker(&owner);
    for token in [&a, &b] {
        for index in 0..3 {
            session_call(
                &owner,
                policy(),
                token,
                UserOperation::Enqueue {
                    key: format!("history-{index}"),
                    payload_version: 1,
                    payload: Payload::CheckSourceUpdates {},
                },
            )
            .unwrap();
            let claimed_attempt!(_, mut lease) = worker.claim().unwrap().unwrap();
            worker
                .update(&mut lease, WorkerOperation::Finish(None))
                .unwrap();
        }
    }
    assert_eq!(owner.retain_jobs(2, 3).unwrap(), 3);
    let mut total = 0;
    for token in [&a, &b] {
        match session_call(
            &owner,
            policy(),
            token,
            UserOperation::List(JobListQuery::default()),
        )
        .unwrap()
        {
            Outcome::List(records) => {
                assert!(records.len() <= 2);
                total += records.len();
            }
            _ => panic!("history list"),
        }
    }
    assert_eq!(total, 3);
    owner.shutdown().unwrap();
}
#[test]
fn ticket_t_28_recovery_old_completed_history_expires_but_uncertainty_survives() {
    let (path, owner) = fixture();
    let done = enqueue(&owner, "old-done", Payload::CheckSourceUpdates {});
    let worker = owner.job_worker(admission()).unwrap();
    let claimed_attempt!(_, mut lease) = worker.claim().unwrap().unwrap();
    worker
        .update(&mut lease, WorkerOperation::Finish(None))
        .unwrap();
    let pending = enqueue(&owner, "old-uncertain", printer("p"));
    let claimed_attempt!(_, mut lease) = worker.claim().unwrap().unwrap();
    worker
        .update(
            &mut lease,
            WorkerOperation::BeginEffect(intent(EffectOperation::PrinterUpload, "p")),
        )
        .unwrap();
    worker.update(&mut lease, WorkerOperation::Fail).unwrap();
    owner.shutdown().unwrap();
    let raw = Connection::open(path.join("print-partner.db")).unwrap();
    raw.execute_batch("UPDATE durable_jobs SET updated=updated-172800,document=json_set(document,'$.updated_at',updated-172800);").unwrap();
    drop(raw);
    let (owner, _) = WriterOwner::open(&path, Limits::default()).unwrap();
    match call(&owner, UserOperation::List(JobListQuery::default())).unwrap() {
        Outcome::List(records) => {
            assert_eq!(records.len(), 1);
            assert_eq!(records[0].job_id, pending.job_id);
        }
        _ => panic!("list"),
    };
    assert_eq!(
        enqueue(&owner, "old-done", Payload::CheckSourceUpdates {}).job_id,
        done.job_id
    );
    owner.shutdown().unwrap();
}

fn source(catalog: &pp_storage::catalog::SourceCatalogClient, name: &str) -> i64 {
    use pp_storage::catalog::{CreateSource, Outcome, Request};
    match catalog
        .execute(Request::Create {
            source: CreateSource {
                name: name.into(),
                ..Default::default()
            },
        })
        .unwrap()
    {
        Outcome::Source(Some(source)) => source.id,
        _ => panic!("Source required"),
    }
}
fn deletion(
    catalog: &pp_storage::catalog::SourceCatalogClient,
    id: i64,
) -> pp_storage::catalog::Deletion {
    match catalog
        .execute(pp_storage::catalog::Request::Delete { id })
        .unwrap()
    {
        pp_storage::catalog::Outcome::Deletion(value) => value,
        _ => panic!("Deletion required"),
    }
}
fn source_lease(
    worker: &ServerWorkerClient,
    lease: &AttemptLease,
    id: Option<i64>,
) -> Result<pp_storage::catalog::SourceWorkLease> {
    worker.begin_source_work(lease, id, &AtomicBool::new(false), Duration::from_secs(5))
}
#[test]
fn integration_source_id_reservations_and_live_lease_have_distinct_lifetimes() {
    use pp_storage::catalog::Deletion;
    let (_, owner) = fixture();
    let catalog = owner.local_source_catalog();
    let id = source(&catalog, "Individual");
    let other = source(&catalog, "Other");
    let first = enqueue(
        &owner,
        "individual",
        Payload::ImportScan {
            project_id: id as u64,
        },
    );
    let second = enqueue(
        &owner,
        "same-source",
        Payload::ExtractSourceDocs {
            project_id: id as u64,
        },
    );
    assert_eq!(deletion(&catalog, id), Deletion::ActiveWork);
    assert_eq!(
        deletion(&catalog, other),
        Deletion::Deleted { source_id: other }
    );
    let worker = owner.job_worker(admission()).unwrap();
    let claimed_attempt!(claimed, mut lease, source_work) = worker.claim().unwrap().unwrap();
    assert_eq!(claimed.job_id, first.job_id);
    assert!(source_lease(&worker, &lease, Some(id)).is_err());
    #[cfg(feature = "desktop_rebuild")]
    let mut live = source_work.unwrap();
    #[cfg(not(feature = "desktop_rebuild"))]
    let mut live = source_lease(&worker, &lease, None).unwrap();
    #[cfg(feature = "desktop_rebuild")]
    assert!(
        source_lease(&worker, &lease, None)
            .err()
            .unwrap()
            .downcast_ref::<pp_storage::catalog::SourceBusy>()
            .is_some()
    );
    let stale = lease.clone();
    worker
        .update(&mut lease, WorkerOperation::Heartbeat)
        .unwrap();
    assert!(source_lease(&worker, &stale, None).is_err());
    worker
        .update(&mut lease, WorkerOperation::Finish(None))
        .unwrap();
    assert!(source_lease(&worker, &lease, None).is_err());
    call(
        &owner,
        UserOperation::Cancel {
            job_id: second.job_id,
        },
    )
    .unwrap();
    assert_eq!(deletion(&catalog, id), Deletion::ActiveWork);
    live.release().unwrap();
    assert_eq!(deletion(&catalog, id), Deletion::Deleted { source_id: id });
    owner.shutdown().unwrap();
}
#[test]
fn integration_drop_and_orderly_restart_keep_source_reservations() {
    use pp_storage::catalog::Deletion;
    let (path, owner) = fixture();
    let catalog = owner.local_source_catalog();
    let id = source(&catalog, "Restart");
    let queued = enqueue(
        &owner,
        "restart-source",
        Payload::ImportScan {
            project_id: id as u64,
        },
    );
    let worker = owner.job_worker(admission()).unwrap();
    let claimed_attempt!(_, old, source_work) = worker.claim().unwrap().unwrap();
    #[cfg(feature = "desktop_rebuild")]
    drop(source_work.unwrap());
    #[cfg(not(feature = "desktop_rebuild"))]
    drop(source_lease(&worker, &old, None).unwrap());
    assert_eq!(deletion(&catalog, id), Deletion::ActiveWork);
    owner.shutdown().unwrap();
    let owner = WriterOwner::open(&path, Limits::default()).unwrap().0;
    let catalog = owner.local_source_catalog();
    assert_eq!(get(&owner, &queued.job_id).state, PersistentState::Queued);
    assert_eq!(deletion(&catalog, id), Deletion::ActiveWork);
    let worker = owner.job_worker(admission()).unwrap();
    assert!(source_lease(&worker, &old, None).is_err());
    let claimed_attempt!(_, _current, source_work) = worker.claim().unwrap().unwrap();
    #[cfg(feature = "desktop_rebuild")]
    let mut live = source_work.unwrap();
    #[cfg(not(feature = "desktop_rebuild"))]
    let mut live = source_lease(&worker, &_current, None).unwrap();
    live.release().unwrap();
    call(
        &owner,
        UserOperation::Cancel {
            job_id: queued.job_id,
        },
    )
    .unwrap();
    assert_eq!(deletion(&catalog, id), Deletion::Deleted { source_id: id });
    owner.shutdown().unwrap();
}
#[test]
fn integration_wildcard_uncertainty_and_abandon_guard_until_matching_resolution() {
    use pp_storage::catalog::Deletion;
    for payload in [
        Payload::Sync { project_ids: None },
        Payload::CheckSourceUpdates {},
    ] {
        if cfg!(feature = "desktop_rebuild") && matches!(payload, Payload::Sync { .. }) {
            continue;
        }
        let (path, owner) = fixture();
        let catalog = owner.local_source_catalog();
        let id = source(&catalog, "Wildcard");
        let another = source(&catalog, "Wildcard other");
        let sync = matches!(payload, Payload::Sync { .. });
        let queued = enqueue(&owner, "wildcard", payload);
        assert_eq!(deletion(&catalog, id), Deletion::ActiveWork);
        let worker = owner.job_worker(admission()).unwrap();
        let claimed_attempt!(_, mut lease) = worker.claim().unwrap().unwrap();
        assert!(source_lease(&worker, &lease, None).is_err());
        // Sync reserves each target's Source when it claims the target.
        if cfg!(feature = "desktop_rebuild") && sync {
            assert!(source_lease(&worker, &lease, Some(id)).is_err());
        } else {
            source_lease(&worker, &lease, Some(id))
                .unwrap()
                .release()
                .unwrap();
        }
        worker
            .update(
                &mut lease,
                WorkerOperation::BeginEffect(intent(
                    EffectOperation::SourceRefresh,
                    "source-operation",
                )),
            )
            .unwrap();
        assert_eq!(deletion(&catalog, another), Deletion::ActiveWork);
        owner.shutdown().unwrap();
        let owner = WriterOwner::open(&path, Limits::default()).unwrap().0;
        let catalog = owner.local_source_catalog();
        let recovered = get(&owner, &queued.job_id);
        assert_eq!(recovered.state, PersistentState::ReconciliationRequired);
        assert_eq!(deletion(&catalog, id), Deletion::ActiveWork);
        let decide = |record: &JobRecord, hash: &str, decision| UserOperation::Reconcile {
            job_id: record.job_id.clone(),
            expected_version: record.state_version,
            expected_generation: record.generation,
            effect_hash: hash.into(),
            decision,
            receipt: None,
        };
        assert!(
            call(
                &owner,
                decide(&recovered, &"c".repeat(64), Decision::ConfirmNoEffect)
            )
            .is_err()
        );
        let abandoned = job(call(
            &owner,
            decide(&recovered, &"b".repeat(64), Decision::Abandon),
        )
        .unwrap());
        assert_eq!(deletion(&catalog, another), Deletion::ActiveWork);
        let additional = enqueue(
            &owner,
            "second-reservation",
            Payload::ImportScan {
                project_id: id as u64,
            },
        );
        call(
            &owner,
            decide(&abandoned, &"b".repeat(64), Decision::ConfirmNoEffect),
        )
        .unwrap();
        assert_eq!(
            deletion(&catalog, another),
            Deletion::Deleted { source_id: another }
        );
        assert_eq!(deletion(&catalog, id), Deletion::ActiveWork);
        call(
            &owner,
            UserOperation::Cancel {
                job_id: additional.job_id,
            },
        )
        .unwrap();
        assert_eq!(deletion(&catalog, id), Deletion::Deleted { source_id: id });
        owner.shutdown().unwrap();
    }
}
#[test]
fn integration_source_bridge_rejects_foreign_worker_missing_source_and_non_source_attempt() {
    let (_, owner) = fixture();
    let worker = owner.job_worker(admission()).unwrap();
    let foreign = owner.job_worker(admission()).unwrap();
    let missing = enqueue(&owner, "missing", Payload::ImportScan { project_id: 9876 });
    #[cfg(feature = "desktop_rebuild")]
    {
        assert!(worker.claim().unwrap().is_none());
        let failed = get(&owner, &missing.job_id);
        assert_eq!(failed.state, PersistentState::Failed);
        assert_eq!(failed.attempt, 0);
        assert_eq!(failed.generation, 0);
    }
    #[cfg(not(feature = "desktop_rebuild"))]
    let claimed_attempt!(_, lease) = worker.claim().unwrap().unwrap();
    #[cfg(not(feature = "desktop_rebuild"))]
    assert!(source_lease(&worker, &lease, None).is_err());
    #[cfg(not(feature = "desktop_rebuild"))]
    assert!(source_lease(&foreign, &lease, None).is_err());
    call(
        &owner,
        UserOperation::Cancel {
            job_id: missing.job_id,
        },
    )
    .unwrap();
    enqueue(&owner, "printer", printer("ordinary-fixture"));
    let claimed_attempt!(_, lease) = worker.claim().unwrap().unwrap();
    assert!(source_lease(&worker, &lease, Some(1)).is_err());
    assert!(source_lease(&foreign, &lease, Some(1)).is_err());
    owner.shutdown().unwrap();
}
#[test]
fn integration_wildcard_lookup_and_reservations_are_tenant_owned() {
    use pp_storage::catalog::{Credentials, Deletion};
    let (_, owner) = fixture();
    let (_, token) = register(&owner, "source-tenant@example.test");
    let tenant_catalog = owner.source_catalog(Credentials::Session(Secret::new(token.clone())));
    let tenant_id = source(&tenant_catalog, "Tenant source");
    let local_catalog = owner.local_source_catalog();
    let local_id = source(&local_catalog, "Local source");
    let queued = job(session_call(
        &owner,
        policy(),
        &token,
        UserOperation::Enqueue {
            key: "tenant-wildcard".into(),
            payload_version: 1,
            payload: Payload::CheckSourceUpdates {},
        },
    )
    .unwrap());
    let worker = session_worker(&owner);
    let claimed_attempt!(_, lease) = worker.claim().unwrap().unwrap();
    assert!(source_lease(&worker, &lease, Some(local_id)).is_err());
    let mut live = source_lease(&worker, &lease, Some(tenant_id)).unwrap();
    assert_eq!(deletion(&tenant_catalog, tenant_id), Deletion::ActiveWork);
    assert_eq!(
        deletion(&local_catalog, local_id),
        Deletion::Deleted {
            source_id: local_id
        }
    );
    live.release().unwrap();
    session_call(
        &owner,
        policy(),
        &token,
        UserOperation::Cancel {
            job_id: queued.job_id,
        },
    )
    .unwrap();
    assert_eq!(
        deletion(&tenant_catalog, tenant_id),
        Deletion::Deleted {
            source_id: tenant_id
        }
    );
    owner.shutdown().unwrap();
}
#[test]
fn integration_source_bridge_expiry_and_cancelled_admission() {
    let (_, owner) = fixture();
    let id = source(&owner.local_source_catalog(), "Expiry");
    enqueue(
        &owner,
        "expiry",
        Payload::ExtractSourceDocs {
            project_id: id as u64,
        },
    );
    let mut config = admission();
    config.lease_seconds = 1;
    let worker = owner.job_worker(config).unwrap();
    let claimed_attempt!(claimed, lease) = worker.claim().unwrap().unwrap();
    assert!(
        worker
            .begin_source_work(&lease, None, &AtomicBool::new(true), Duration::ZERO)
            .is_err()
    );
    wait_for_lease_expiry(&claimed);
    assert!(source_lease(&worker, &lease, None).is_err());
    assert_eq!(
        deletion(&owner.local_source_catalog(), id),
        pp_storage::catalog::Deletion::ActiveWork
    );
    owner.shutdown().unwrap();
}

#[test]
fn integration_individual_uncertain_reservation_survives_restart_and_matching_success() {
    use pp_storage::catalog::Deletion;
    let (path, owner) = fixture();
    let catalog = owner.local_source_catalog();
    let id = source(&catalog, "Individual uncertain");
    let other = source(&catalog, "Unreserved individual");
    let queued = enqueue(
        &owner,
        "individual-effect",
        Payload::ImportScan {
            project_id: id as u64,
        },
    );
    let worker = owner.job_worker(admission()).unwrap();
    let claimed_attempt!(_, mut lease, source_work) = worker.claim().unwrap().unwrap();
    worker
        .update(
            &mut lease,
            WorkerOperation::BeginEffect(intent(EffectOperation::SourceRefresh, "source-effect")),
        )
        .unwrap();
    #[cfg(feature = "desktop_rebuild")]
    let mut live = source_work.unwrap();
    #[cfg(not(feature = "desktop_rebuild"))]
    let mut live = source_lease(&worker, &lease, None).unwrap();
    live.release().unwrap();
    owner.shutdown().unwrap();
    let owner = WriterOwner::open(&path, Limits::default()).unwrap().0;
    let catalog = owner.local_source_catalog();
    let uncertain = get(&owner, &queued.job_id);
    assert_eq!(uncertain.state, PersistentState::ReconciliationRequired);
    assert_eq!(deletion(&catalog, id), Deletion::ActiveWork);
    assert_eq!(
        deletion(&catalog, other),
        Deletion::Deleted { source_id: other }
    );
    let resolve = |receipt| UserOperation::Reconcile {
        job_id: queued.job_id.clone(),
        expected_version: uncertain.state_version,
        expected_generation: uncertain.generation,
        effect_hash: "b".repeat(64),
        decision: Decision::ConfirmSucceeded,
        receipt: Some(receipt),
    };
    assert!(call(&owner, resolve(receipt("wrong-source-effect"))).is_err());
    assert_eq!(deletion(&catalog, id), Deletion::ActiveWork);
    call(&owner, resolve(receipt("source-effect"))).unwrap();
    assert_eq!(deletion(&catalog, id), Deletion::Deleted { source_id: id });
    owner.shutdown().unwrap();
}

fn filename_payload(grouping: FilenameExport) -> Payload {
    Payload::ExportStlPack {
        profile_id: 1,
        missing_only: false,
        group_by: GroupBy::ColorDir,
        unit_tokens: vec![],
        filename_grouping: Some(grouping),
    }
}

fn filename_definition() -> FilenameExport {
    FilenameExport {
        definition: FilenameGrouping {
            name: "Print settings".into(),
            rules: vec![FilenameRule {
                suffix: "-A".into(),
                group: "Aesthetic".into(),
            }],
            overrides: [("part.stl".into(), "Unassigned".into())].into(),
        },
        arrangement: Arrangement::Group,
        group: Some("Aesthetic".into()),
        role: Some("Structural".into()),
    }
}

#[test]
fn filename_grouping_rejects_padded_reserved_labels_before_admission() {
    let (_path, owner) = fixture();
    for (index, reserved) in [" conflict ", "\tUnassigned\n", "\u{feff}CONFLICT\u{feff}"]
        .into_iter()
        .enumerate()
    {
        let mut grouping = filename_definition();
        grouping.definition.rules[0].group = reserved.into();
        assert!(
            call(
                &owner,
                UserOperation::Enqueue {
                    key: format!("reserved-rule-{index}"),
                    payload_version: 1,
                    payload: filename_payload(grouping),
                },
            )
            .is_err()
        );
    }
    for (index, reserved) in [" conflict ", "\u{feff}Conflict\t"].into_iter().enumerate() {
        let mut grouping = filename_definition();
        grouping
            .definition
            .overrides
            .insert("part.stl".into(), reserved.into());
        assert!(
            call(
                &owner,
                UserOperation::Enqueue {
                    key: format!("reserved-override-{index}"),
                    payload_version: 1,
                    payload: filename_payload(grouping),
                },
            )
            .is_err()
        );
    }
    let mut grouping = filename_definition();
    grouping.definition.rules[0].group = "\u{0085}Aesthetic\u{0085}".into();
    assert!(
        call(
            &owner,
            UserOperation::Enqueue {
                key: "non-js-whitespace".into(),
                payload_version: 1,
                payload: filename_payload(grouping),
            },
        )
        .is_err()
    );
    owner.shutdown().unwrap();
}

#[test]
fn filename_grouping_normalizes_before_hashing_and_persistence() {
    let (path, owner) = fixture();
    let canonical = filename_definition();
    let mut padded = canonical.clone();
    padded.definition.name = "\u{feff} Print settings \t".into();
    padded.definition.rules[0].suffix = "\n -A \r".into();
    padded.definition.rules[0].group = "\u{00a0} Aesthetic \u{feff}".into();
    padded
        .definition
        .overrides
        .insert("part.stl".into(), " Unassigned ".into());
    padded.group = Some(" Aesthetic ".into());
    padded.role = Some("\tStructural\n".into());
    let first = enqueue(&owner, "normalized-filename", filename_payload(padded));
    assert_eq!(first.payload, filename_payload(canonical.clone()));
    let same = enqueue(
        &owner,
        "normalized-filename",
        filename_payload(canonical.clone()),
    );
    assert_eq!(first.job_id, same.job_id);
    assert_eq!(first.state_version, same.state_version);
    let mut bounded = canonical.clone();
    bounded.definition.rules[0].group = format!(" {} ", "A".repeat(80));
    bounded.group = None;
    let bounded = enqueue(&owner, "trim-before-length", filename_payload(bounded));
    let Payload::ExportStlPack {
        filename_grouping: Some(grouping),
        ..
    } = bounded.payload
    else {
        panic!("Expected filename grouping");
    };
    assert_eq!(grouping.definition.rules[0].group, "A".repeat(80));
    owner.shutdown().unwrap();
    let (owner, _) = WriterOwner::open(&path, Limits::default()).unwrap();
    assert_eq!(
        get(&owner, &first.job_id).payload,
        filename_payload(canonical)
    );
    owner.shutdown().unwrap();
}

#[test]
fn gre_526_startup_surfaces_queued_oversized_documents() {
    let (path, owner) = fixture();
    let damaged = enqueue(&owner, "damaged", Payload::CheckSourceUpdates {});
    let healthy = enqueue(&owner, "healthy", Payload::CheckSourceUpdates {});
    owner.shutdown().unwrap();
    let raw = Connection::open(path.join("print-partner.db")).unwrap();
    let document = format!("{{\"padding\":\"{}\"}}", "x".repeat(JOB_DOC_SIZE_LIMIT));
    raw.execute(
        "UPDATE durable_jobs SET document=?2 WHERE id=?1",
        rusqlite::params![damaged.job_id, document],
    )
    .unwrap();
    drop(raw);

    for _ in 0..2 {
        let (owner, _) = WriterOwner::open(&path, Limits::default()).unwrap();
        let Outcome::Quarantined(rows) = call(
            &owner,
            UserOperation::Quarantined {
                before: None,
                limit: 200,
            },
        )
        .unwrap() else {
            panic!("Expected quarantine list");
        };
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].job_id, damaged.job_id);
        assert!(rows[0].reason.contains("131072-byte limit"));
        let Outcome::List(rows) =
            call(&owner, UserOperation::List(JobListQuery::default())).unwrap()
        else {
            panic!("Expected list");
        };
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].job_id, healthy.job_id);
        owner.shutdown().unwrap();
    }
    let stored: (String, String, i64) = readonly(&path)
        .query_row(
            "SELECT document,state,version FROM durable_jobs WHERE id=?1",
            [&damaged.job_id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .unwrap();
    assert_eq!(
        stored,
        (document, "quarantined".into(), damaged.state_version + 1)
    );
}

#[test]
fn gre_526_recovery_growth_does_not_block_startup_or_restart() {
    let (path, owner) = fixture();
    let damaged = enqueue(&owner, "damaged", Payload::CheckSourceUpdates {});
    let healthy = enqueue(&owner, "healthy", Payload::CheckSourceUpdates {});
    owner.shutdown().unwrap();
    let raw = Connection::open(path.join("print-partner.db")).unwrap();
    let original: String = raw
        .query_row(
            "SELECT document FROM durable_jobs WHERE id=?1",
            [&damaged.job_id],
            |row| row.get(0),
        )
        .unwrap();
    let mut value: serde_json::Value = serde_json::from_str(&original).unwrap();
    value["state"] = "running".into();
    value["state_version"] = 9.into();
    value["generation"] = 9.into();
    value["attempt"] = 1.into();
    value["lease_until"] = 0.into();
    value["_attempt_fence"] = "".into();
    value["_attempt_worker"] = "".into();
    value["recovery"] = "".into();
    let width = JOB_DOC_SIZE_LIMIT - serde_json::to_string(&value).unwrap().len();
    value["recovery"] = "x".repeat(width).into();
    let document = serde_json::to_string(&value).unwrap();
    assert_eq!(document.len(), JOB_DOC_SIZE_LIMIT);
    raw.execute("UPDATE durable_jobs SET document=?2,state='running',version=9,generation=9,lease_until=0 WHERE id=?1",
        rusqlite::params![damaged.job_id, document]).unwrap();
    drop(raw);

    for _ in 0..2 {
        let (owner, _) = WriterOwner::open(&path, Limits::default()).unwrap();
        let Outcome::Quarantined(rows) = call(
            &owner,
            UserOperation::Quarantined {
                before: None,
                limit: 200,
            },
        )
        .unwrap() else {
            panic!("Expected quarantine list");
        };
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].job_id, damaged.job_id);
        assert!(
            call(
                &owner,
                UserOperation::Get {
                    job_id: damaged.job_id.clone()
                }
            )
            .is_err()
        );
        let Outcome::List(rows) =
            call(&owner, UserOperation::List(JobListQuery::default())).unwrap()
        else {
            panic!("Expected list");
        };
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].job_id, healthy.job_id);
        owner.shutdown().unwrap();
    }
    let stored: (String, String, i64) = readonly(&path)
        .query_row(
            "SELECT document,state,version FROM durable_jobs WHERE id=?1",
            [&damaged.job_id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .unwrap();
    assert_eq!(stored, (document, "quarantined".into(), 10));
}

#[test]
fn gre_526_startup_and_load_skip_and_log_invalid_saved_rows() {
    use sha2::{Digest, Sha256};
    const CHILD_DATA_DIR: &str = "PP_GRE_526_CHILD_DATA_DIR";
    if let Some(path) = std::env::var_os(CHILD_DATA_DIR) {
        let (owner, _) = WriterOwner::open(&PathBuf::from(path), Limits::default()).unwrap();
        let records = match call(&owner, UserOperation::List(JobListQuery::default())).unwrap() {
            Outcome::List(records) => records,
            _ => panic!("Expected list"),
        };
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].state, PersistentState::Queued);
        assert_eq!(records[0].generation, 2);
        assert_eq!(get(&owner, &records[0].job_id).job_id, records[0].job_id);
        owner.shutdown().unwrap();
        return;
    }

    let (path, owner) = fixture();
    let healthy = enqueue(&owner, "healthy", Payload::CheckSourceUpdates {});
    owner
        .job_worker(admission())
        .unwrap()
        .claim()
        .unwrap()
        .unwrap();
    owner.shutdown().unwrap();
    let raw = Connection::open(path.join("print-partner.db")).unwrap();
    let document: String = raw
        .query_row(
            "SELECT document FROM durable_jobs WHERE id=?1",
            [&healthy.job_id],
            |row| row.get(0),
        )
        .unwrap();
    let mut oversized: serde_json::Value = serde_json::from_str(&document).unwrap();
    oversized["job_id"] = "oversized-row".into();
    oversized["recovery"] = "x".repeat(JOB_DOC_SIZE_LIMIT).into();
    let oversized = serde_json::to_string(&oversized).unwrap();
    assert!(oversized.len() > JOB_DOC_SIZE_LIMIT);
    for (id, document) in [("corrupt-row", "{}"), ("oversized-row", oversized.as_str())] {
        // Valid JSON with invalid job data can be inserted despite the JSON expression index.
        raw.execute(
            "INSERT INTO durable_jobs(id,tenant,kind,state,resource,version,generation,lease_until,created,updated,document)
             SELECT ?1,tenant,kind,state,resource,version,generation,lease_until,created,updated,?2 FROM durable_jobs WHERE id=?3",
            rusqlite::params![id, document, healthy.job_id],
        ).unwrap();
    }
    drop(raw);

    let output = child_process::command(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "gre_526_startup_and_load_skip_and_log_invalid_saved_rows",
            "--nocapture",
        ])
        .env(CHILD_DATA_DIR, &path)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "stdout: {}\nstderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let log = String::from_utf8(output.stderr).unwrap();
    for id in ["corrupt-row", "oversized-row"] {
        let job_ref = hex::encode(Sha256::digest(id.as_bytes()));
        assert!(
            log.contains(&format!("job_ref={}", &job_ref[..16])),
            "{log}"
        );
        assert!(!log.contains(id), "{log}");
    }
    assert!(
        log.contains(&format!("exceeding the {JOB_DOC_SIZE_LIMIT}-byte limit")),
        "{log}"
    );
    let raw = readonly(&path);
    let stored: String = raw
        .query_row(
            "SELECT document FROM durable_jobs WHERE id='oversized-row'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(stored, oversized);
    let corrupt: String = raw
        .query_row(
            "SELECT document FROM durable_jobs WHERE id='corrupt-row'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(corrupt, "{}");
}

#[test]
fn gre_526_startup_diagnostics_are_bounded_and_redacted() {
    const CHILD_DATA_DIR: &str = "PP_GRE_526_REDACTION_CHILD_DATA_DIR";
    if let Some(path) = std::env::var_os(CHILD_DATA_DIR) {
        let (owner, _) = WriterOwner::open(&PathBuf::from(path), Limits::default()).unwrap();
        let Outcome::Quarantined(rows) = call(
            &owner,
            UserOperation::Quarantined {
                before: None,
                limit: 200,
            },
        )
        .unwrap() else {
            panic!("Expected quarantine list");
        };
        assert_eq!(rows.len(), 20);
        for row in rows {
            assert_eq!(
                row.reason,
                "Invalid durable job document; operator inspection required"
            );
        }
        let Outcome::List(rows) =
            call(&owner, UserOperation::List(JobListQuery::default())).unwrap()
        else {
            panic!("Expected list");
        };
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].state, PersistentState::Queued);
        owner.shutdown().unwrap();
        return;
    }

    let (path, owner) = fixture();
    let healthy = enqueue(&owner, "healthy", Payload::CheckSourceUpdates {});
    owner.shutdown().unwrap();
    let raw = Connection::open(path.join("print-partner.db")).unwrap();
    let original: String = raw
        .query_row(
            "SELECT document FROM durable_jobs WHERE id=?1",
            [&healthy.job_id],
            |row| row.get(0),
        )
        .unwrap();
    let mut document: serde_json::Value = serde_json::from_str(&original).unwrap();
    document["state"] = format!("sensitive-document-value{}", "x".repeat(8192)).into();
    let document = serde_json::to_string(&document).unwrap();
    for index in 0..20 {
        let id = format!("sensitive-row-id\n{}-{index}", "y".repeat(8192));
        raw.execute("INSERT INTO durable_jobs(id,tenant,kind,state,resource,version,generation,lease_until,created,updated,document)
            SELECT ?1,tenant,kind,state,resource,version,generation,lease_until,created,updated,?2 FROM durable_jobs WHERE id=?3",
            rusqlite::params![id, document, healthy.job_id]).unwrap();
    }
    drop(raw);
    let output = child_process::command(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "gre_526_startup_diagnostics_are_bounded_and_redacted",
            "--nocapture",
        ])
        .env(CHILD_DATA_DIR, &path)
        .output()
        .unwrap();
    let log = String::from_utf8(output.stderr).unwrap();
    assert!(
        output.status.success(),
        "stdout: {}\nstderr: {log}",
        String::from_utf8_lossy(&output.stdout)
    );
    assert!(!log.contains("sensitive-"), "{log}");
    assert!(log.lines().count() <= 27, "{log}");
    assert!(log.lines().all(|line| line.len() <= 200), "{log}");
    assert!(
        log.contains("Additional invalid durable job rows skipped: 12"),
        "{log}"
    );
}

#[test]
fn invalid_attempts_do_not_consume_worker_capacity_but_keep_printer_reservations() {
    for effect_admitted in [false, true] {
        for expired in [false, true] {
            for total in [1, 4] {
                let (path, owner) = fixture();
                let damaged = enqueue(&owner, "damaged", printer("uncertain-printer"));
                let mut limits = admission();
                limits.total = total;
                let worker = owner.job_worker(limits.clone()).unwrap();
                let claimed_attempt!(_, mut lease) = worker.claim().unwrap().unwrap();
                if effect_admitted {
                    worker
                        .update(
                            &mut lease,
                            WorkerOperation::BeginEffect(intent(
                                EffectOperation::PrinterUpload,
                                "uncertain-printer",
                            )),
                        )
                        .unwrap();
                }
                let blocked = enqueue(&owner, "blocked", printer("uncertain-printer"));
                let healthy = enqueue(&owner, "healthy", printer("healthy-printer"));
                owner.shutdown().unwrap();
                let raw = Connection::open(path.join("print-partner.db")).unwrap();
                raw.execute(
                    "UPDATE durable_jobs SET document='{}',lease_until=?2 WHERE id=?1",
                    rusqlite::params![damaged.job_id, if expired { 0 } else { i64::MAX }],
                )
                .unwrap();
                let before: (String, String, String, i64, i64, i64) = raw.query_row(
                    "SELECT document,state,resource,version,generation,lease_until FROM durable_jobs WHERE id=?1",
                    [&damaged.job_id],
                    |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?, r.get(5)?)),
                ).unwrap();
                drop(raw);

                let (owner, _) = WriterOwner::open(&path, Limits::default()).unwrap();
                let Outcome::Quarantined(records) = call(
                    &owner,
                    UserOperation::Quarantined {
                        before: None,
                        limit: 1,
                    },
                )
                .unwrap() else {
                    panic!("Expected quarantined rows");
                };
                assert_eq!(records.len(), 1);
                assert_eq!(records[0].job_id, damaged.job_id);
                assert!(!records[0].reason.is_empty());
                assert_eq!(
                    records[0].uncertain_resource.as_deref(),
                    Some("printer:uncertain-printer")
                );
                let worker = owner.job_worker(limits).unwrap();
                let claimed_attempt!(claimed, mut lease) = worker.claim().unwrap().unwrap();
                assert_eq!(claimed.job_id, healthy.job_id);
                worker.update(&mut lease, WorkerOperation::Fail).unwrap();
                assert!(worker.claim().unwrap().is_none());
                assert_eq!(get(&owner, &blocked.job_id).state, PersistentState::Queued);
                owner.shutdown().unwrap();
                let after: (String, String, String, i64, i64, Option<i64>) = readonly(&path).query_row(
                    "SELECT document,state,resource,version,generation,lease_until FROM durable_jobs WHERE id=?1",
                    [&damaged.job_id],
                    |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?, r.get(5)?)),
                ).unwrap();
                assert_eq!(after.0, before.0); // Preserve the damaged document exactly.
                assert_eq!(after.1, "quarantined");
                assert_eq!(after.2, before.2); // Preserve the uncertain resource reservation.
                assert_eq!(after.3, before.3 + 1);
                assert_eq!(after.4, before.4);
                assert_eq!(after.5, None);
                let event: String = readonly(&path).query_row(
                    "SELECT event FROM durable_job_history WHERE job_id=?1 ORDER BY version DESC LIMIT 1",
                    [&damaged.job_id], |row| row.get(0),
                ).unwrap();
                assert_eq!(event, "document_quarantined_resource_uncertain");
            }
        }
    }
}

#[test]
fn list_limit_skips_invalid_newest_rows_and_returns_a_usable_cursor() {
    let (path, owner) = fixture();
    let older = enqueue(&owner, "older", Payload::CheckSourceUpdates {});
    let healthy = enqueue(&owner, "healthy", Payload::CheckSourceUpdates {});
    let damaged = enqueue(&owner, "damaged", Payload::CheckSourceUpdates {});
    owner.shutdown().unwrap();
    let raw = Connection::open(path.join("print-partner.db")).unwrap();
    // Give each row a distinct, deterministic ordering in both document and index.
    for (id, updated) in [
        (&older.job_id, 1),
        (&healthy.job_id, 2),
        (&damaged.job_id, 3),
    ] {
        raw.execute(
            "UPDATE durable_jobs SET updated=?2,document=json_set(document,'$.updated_at',?2) WHERE id=?1",
            rusqlite::params![id, updated],
        ).unwrap();
    }
    raw.execute(
        "UPDATE durable_jobs SET document='{}' WHERE id=?1",
        [&damaged.job_id],
    )
    .unwrap();
    drop(raw);
    let (owner, _) = WriterOwner::open(&path, Limits::default()).unwrap();
    let mut query = JobListQuery {
        limit: 1,
        ..JobListQuery::default()
    };
    for expected in [&healthy.job_id, &older.job_id] {
        let Outcome::List(records) = call(&owner, UserOperation::List(query.clone())).unwrap()
        else {
            panic!("Expected list");
        };
        assert_eq!(records.len(), 1);
        assert_eq!(&records[0].job_id, expected);
        query.before = Some((records[0].updated_at, records[0].job_id.clone()));
    }
    let Outcome::List(records) = call(&owner, UserOperation::List(query)).unwrap() else {
        panic!("Expected list");
    };
    assert!(records.is_empty());
    owner.shutdown().unwrap();
}

#[test]
fn bound_printer_start_finish_none_uses_confirmed_start_receipt() {
    let (_, owner) = fixture();
    let parent = uploaded_only_parent(&owner, "finish-child-parent", "finish-child-printer");
    let child = job(enqueue_start(&owner, "finish-child", &parent.job_id).unwrap());
    let Payload::PrinterStart(request) = &child.payload else {
        panic!("start payload")
    };
    let mut start = request.upload_effect().unwrap().intent.clone();
    start.operation = EffectOperation::PrinterStart;
    let worker = owner.job_worker(admission()).unwrap();
    let claimed_attempt!(claimed, mut lease) = worker.claim().unwrap().unwrap();
    assert_eq!(claimed.job_id, child.job_id);
    assert!(
        worker
            .update(&mut lease, WorkerOperation::Finish(None))
            .is_err()
    );
    let receipt = begin_and_confirm(&worker, &mut lease, start, "child-start-result");
    let done = worker
        .update(&mut lease, WorkerOperation::Finish(None))
        .unwrap();
    assert_eq!(done.state, PersistentState::Succeeded);
    assert_eq!(done.progress, Some(100));
    assert_eq!(done.result, Some(receipt));
    assert_eq!(
        serde_json::to_value(get(&owner, &child.job_id)).unwrap(),
        serde_json::to_value(done).unwrap()
    );
    owner.shutdown().unwrap();
}

#[test]
fn scoped_sync_reserves_and_leases_only_listed_sources() {
    use pp_storage::catalog::Deletion;
    let (_, owner) = fixture();
    let catalog = owner.local_source_catalog();
    let first = source(&catalog, "Sync first");
    let second = source(&catalog, "Sync second");
    let outside = source(&catalog, "Outside sync");
    let removable = source(&catalog, "Unrelated deletion");
    let sync = enqueue(
        &owner,
        "scoped-sync",
        Payload::Sync {
            project_ids: Some(vec![first as u64, second as u64]),
        },
    );
    assert_eq!(deletion(&catalog, first), Deletion::ActiveWork);
    assert_eq!(deletion(&catalog, second), Deletion::ActiveWork);
    assert_eq!(
        deletion(&catalog, removable),
        Deletion::Deleted {
            source_id: removable
        }
    );
    let worker = owner.job_worker(admission()).unwrap();
    let claimed_attempt!(claimed, mut lease) = worker.claim().unwrap().unwrap();
    assert_eq!(claimed.job_id, sync.job_id);
    assert!(source_lease(&worker, &lease, None).is_err());
    assert!(source_lease(&worker, &lease, Some(outside)).is_err());
    for id in [first, second] {
        let source_work = source_lease(&worker, &lease, Some(id));
        // Sync reserves each target's Source when it claims the target.
        #[cfg(feature = "desktop_rebuild")]
        assert!(source_work.is_err());
        #[cfg(not(feature = "desktop_rebuild"))]
        source_work.unwrap().release().unwrap();
    }
    let unrelated = enqueue(
        &owner,
        "outside-import",
        Payload::ImportScan {
            project_id: outside as u64,
        },
    );
    enqueue(
        &owner,
        "overlapping-extract",
        Payload::ExtractSourceDocs {
            project_id: second as u64,
        },
    );
    let claimed_attempt!(claimed, mut other_lease) = worker.claim().unwrap().unwrap();
    assert_eq!(claimed.job_id, unrelated.job_id);
    worker
        .update(&mut other_lease, WorkerOperation::Finish(None))
        .unwrap();
    assert!(worker.claim().unwrap().is_none());
    let finish = worker.update(&mut lease, WorkerOperation::Finish(None));
    #[cfg(feature = "desktop_rebuild")]
    {
        assert_eq!(
            finish.unwrap_err().to_string(),
            "Sync operation belongs to its journal owner"
        );
        assert_eq!(get(&owner, &sync.job_id).state, PersistentState::Running);
        assert!(worker.claim().unwrap().is_none());
        call(
            &owner,
            UserOperation::Cancel {
                job_id: sync.job_id,
            },
        )
        .unwrap();
    }
    #[cfg(not(feature = "desktop_rebuild"))]
    finish.unwrap();
    assert!(worker.claim().unwrap().is_some());
    owner.shutdown().unwrap();
}

#[test]
fn legacy_scoped_sync_wildcard_column_reopens_with_payload_scope() {
    use pp_storage::catalog::Deletion;
    let (path, owner) = fixture();
    let catalog = owner.local_source_catalog();
    let id = source(&catalog, "Legacy scoped");
    let other = source(&catalog, "Legacy unrelated");
    let sync = enqueue(
        &owner,
        "legacy-sync",
        Payload::Sync {
            project_ids: Some(vec![id as u64]),
        },
    );
    owner.shutdown().unwrap();
    let raw = Connection::open(path.join("print-partner.db")).unwrap();
    raw.execute(
        "UPDATE durable_jobs SET resource='source:*' WHERE id=?1",
        [&sync.job_id],
    )
    .unwrap();
    drop(raw);
    let owner = WriterOwner::open(&path, Limits::default()).unwrap().0;
    let catalog = owner.local_source_catalog();
    assert_eq!(deletion(&catalog, id), Deletion::ActiveWork);
    assert_eq!(
        deletion(&catalog, other),
        Deletion::Deleted { source_id: other }
    );
    let worker = owner.job_worker(admission()).unwrap();
    let claimed_attempt!(_, lease) = worker.claim().unwrap().unwrap();
    assert!(source_lease(&worker, &lease, Some(other)).is_err());
    let stored: String = readonly(&path)
        .query_row(
            "SELECT resource FROM durable_jobs WHERE id=?1",
            [&sync.job_id],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(stored, format!("source-set:{id}"));
    owner.shutdown().unwrap();
}

#[test]
fn empty_scoped_sync_does_not_reserve_any_source() {
    use pp_storage::catalog::Deletion;
    let (_, owner) = fixture();
    let catalog = owner.local_source_catalog();
    let id = source(&catalog, "Empty sync unrelated");
    enqueue(
        &owner,
        "empty-sync",
        Payload::Sync {
            project_ids: Some(vec![]),
        },
    );
    assert_eq!(deletion(&catalog, id), Deletion::Deleted { source_id: id });
    let worker = owner.job_worker(admission()).unwrap();
    let claimed_attempt!(_, lease) = worker.claim().unwrap().unwrap();
    assert!(source_lease(&worker, &lease, Some(id)).is_err());
    assert!(source_lease(&worker, &lease, None).is_err());
    owner.shutdown().unwrap();
}

fn disk_full(error: &anyhow::Error) -> bool {
    matches!(
        error.downcast_ref::<rusqlite::Error>(),
        Some(rusqlite::Error::SqliteFailure(code, _)) if code.code == ErrorCode::DiskFull
    )
}

#[test]
fn ticket_t_28_claims_writer_disk_quota_keeps_the_active_attempt() {
    let (_path, owner) = fixture();
    let worker = owner
        .job_worker(WorkerAdmission {
            kinds: vec![(JobKind::CheckSourceUpdates, 1), (JobKind::Sync, 1)],
            total: 4,
            per_resource: 4,
            lease_seconds: 60,
        })
        .unwrap();
    let held = enqueue(&owner, "held", Payload::CheckSourceUpdates {});
    let claimed_attempt!(claimed, _lease) = worker.claim().unwrap().unwrap();
    assert_eq!(claimed.job_id, held.job_id);
    assert_eq!(claimed.attempt, 1);
    assert_eq!(claimed.state, PersistentState::Running);
    let victim = enqueue(&owner, "victim", Payload::Sync { project_ids: None });
    assert_eq!(victim.attempt, 0);
    assert_eq!(victim.state, PersistentState::Queued);

    let refused = owner
        .admit_disk_quota(1)
        .expect_err("quota below the database size");
    assert!(refused.to_string().contains("below"), "{refused:#}");
    // SQLite clamps an undersized max_page_count to the current size. A refusal
    // must leave the previous limit in place so later writes can still grow.
    let before = owner.database_pages().unwrap();
    let mut grew = false;
    for index in 0..64 {
        enqueue(
            &owner,
            &format!("grow-{index}"),
            Payload::CheckSourceUpdates {},
        );
        if owner.database_pages().unwrap() > before {
            grew = true;
            break;
        }
    }
    assert!(
        grew,
        "refused disk quota froze the database at {before} pages"
    );

    let pages = owner.database_pages().unwrap();
    assert!(pages > 1, "schema occupies more than one page");
    assert_eq!(owner.admit_disk_quota(pages).unwrap(), pages);
    // SQLite clamps max_page_count above 0xfffffffe to that maximum and
    // returns it. A refusal must keep the quota admitted just above.
    let oversized = owner
        .admit_disk_quota(u32::MAX)
        .expect_err("quota above the sqlite page limit");
    assert!(oversized.to_string().contains("exceeds"), "{oversized:#}");
    let mut grew_after_oversized = false;
    for index in 0..32 {
        match call(
            &owner,
            UserOperation::Enqueue {
                key: format!("oversize-{index}"),
                payload_version: 1,
                payload: Payload::CheckSourceUpdates {},
            },
        ) {
            Ok(_) => {
                if owner.database_pages().unwrap() > pages {
                    grew_after_oversized = true;
                    break;
                }
            }
            Err(error) => {
                assert!(disk_full(&error), "{error:#}");
                break;
            }
        }
    }
    assert!(
        !grew_after_oversized,
        "refused oversized quota ({oversized}) raised the page limit past {pages}"
    );

    let mut failed_key = None;
    for index in 0..256 {
        let key = format!("fill-{index}");
        match call(
            &owner,
            UserOperation::Enqueue {
                key: key.clone(),
                payload_version: 1,
                payload: Payload::CheckSourceUpdates {},
            },
        ) {
            Ok(_) => {}
            Err(error) => {
                assert!(disk_full(&error), "{error:#}");
                failed_key = Some(key);
                break;
            }
        }
    }
    let failed_key = failed_key.expect("admitted disk quota must reject a job write");

    let still = get(&owner, &held.job_id);
    assert_eq!(still.attempt, claimed.attempt);
    assert_eq!(still.generation, claimed.generation);
    assert_eq!(still.state_version, claimed.state_version);
    assert_eq!(still.state, PersistentState::Running);
    let queued = get(&owner, &victim.job_id);
    assert_eq!(queued.attempt, 0);
    assert_eq!(queued.generation, 0);
    assert_eq!(queued.state, PersistentState::Queued);
    match call(
        &owner,
        UserOperation::History {
            job_id: victim.job_id.clone(),
            before_version: None,
            limit: 20,
        },
    )
    .unwrap()
    {
        Outcome::History(entries) => {
            assert!(entries.iter().all(|entry| entry.event != "claimed"));
        }
        _ => panic!("history"),
    }
    let still = get(&owner, &held.job_id);
    assert_eq!(still.attempt, 1);
    assert_eq!(still.generation, claimed.generation);
    assert_eq!(still.state, PersistentState::Running);

    assert_eq!(owner.admit_disk_quota(pages + 64).unwrap(), pages + 64);
    let claimed_attempt!(retried, _) = worker.claim().unwrap().unwrap();
    assert_eq!(retried.job_id, victim.job_id);
    assert_eq!(retried.attempt, 1);
    assert_eq!(retried.state, PersistentState::Running);
    let still = get(&owner, &held.job_id);
    assert_eq!(still.attempt, 1);
    assert_eq!(still.generation, claimed.generation);

    let created = enqueue(&owner, &failed_key, Payload::CheckSourceUpdates {});
    let replayed = enqueue(&owner, &failed_key, Payload::CheckSourceUpdates {});
    assert_eq!(replayed.job_id, created.job_id);
    owner.shutdown().unwrap();
}

#[cfg(feature = "desktop_rebuild")]
#[test]
fn individual_source_claim_skips_busy_head_without_mutating_it() {
    let (_, owner) = fixture();
    let catalog = owner.local_source_catalog();
    let busy_id = source(&catalog, "Busy head");
    let free_id = source(&catalog, "Free tail");
    let first = enqueue(
        &owner,
        "busy-owner",
        Payload::ImportScan {
            project_id: busy_id as u64,
        },
    );
    let mut config = admission();
    for (_, capacity) in &mut config.kinds {
        *capacity = 2;
    }
    let worker = owner.job_worker(config).unwrap();
    let mut active = worker.claim().unwrap().unwrap();
    assert_eq!(active.job.job_id, first.job_id);
    let waiting = enqueue(
        &owner,
        "busy-waiting",
        Payload::ImportScan {
            project_id: busy_id as u64,
        },
    );
    let free = enqueue(
        &owner,
        "free-tail",
        Payload::ImportScan {
            project_id: free_id as u64,
        },
    );
    worker
        .update(&mut active.lease, WorkerOperation::Finish(None))
        .unwrap();
    assert_eq!(get(&owner, &first.job_id).state, PersistentState::Succeeded);
    let before = get(&owner, &waiting.job_id);
    let mut other = worker.claim().unwrap().unwrap();
    assert_eq!(other.job.job_id, free.job_id);
    assert!(worker.claim().unwrap().is_none());
    let after = get(&owner, &waiting.job_id);
    assert_eq!(after.state_version, before.state_version);
    assert_eq!(after.attempt, before.attempt);
    assert_eq!(after.generation, before.generation);
    worker
        .update(&mut other.lease, WorkerOperation::Finish(None))
        .unwrap();
    drop(other.source_work.take());
    drop(active.source_work.take());
    let next = worker.claim().unwrap().unwrap();
    assert_eq!(next.job.job_id, waiting.job_id);
    owner.shutdown().unwrap();
}

#[cfg(feature = "desktop_rebuild")]
#[test]
fn expired_attempt_keeps_source_authority_until_drop() {
    let (_, owner) = fixture();
    let id = source(&owner.local_source_catalog(), "Expired overlap");
    let queued = enqueue(
        &owner,
        "expired-overlap",
        Payload::ExtractSourceDocs {
            project_id: id as u64,
        },
    );
    let mut config = admission();
    config.lease_seconds = 1;
    let worker = owner.job_worker(config).unwrap();
    let mut first = worker.claim().unwrap().unwrap();
    wait_for_lease_expiry(&first.job);
    assert!(worker.claim().unwrap().is_none());
    let recovered = get(&owner, &queued.job_id);
    assert_eq!(recovered.state, PersistentState::Queued);
    assert_eq!(recovered.attempt, 1);
    assert!(worker.claim().unwrap().is_none());
    let unchanged = get(&owner, &queued.job_id);
    assert_eq!(unchanged.state_version, recovered.state_version);
    assert_eq!(unchanged.attempt, recovered.attempt);
    assert_eq!(unchanged.generation, recovered.generation);
    drop(first.source_work.take());
    let second = worker.claim().unwrap().unwrap();
    assert_eq!(second.job.job_id, queued.job_id);
    assert_eq!(second.job.attempt, 2);
    owner.shutdown().unwrap();
}

#[cfg(feature = "desktop_rebuild")]
#[test]
fn duplicate_direct_and_wildcard_source_leases_are_typed_busy() {
    let (_, owner) = fixture();
    let catalog = owner.local_source_catalog();
    let id = source(&catalog, "Typed busy");
    let direct = catalog
        .begin_work(id, &AtomicBool::new(false), Duration::from_secs(5))
        .unwrap();
    let duplicate = match catalog.begin_work(id, &AtomicBool::new(false), Duration::from_secs(5)) {
        Err(error) => error,
        Ok(_) => panic!("duplicate direct Source lease accepted"),
    };
    assert!(
        duplicate
            .downcast_ref::<pp_storage::catalog::SourceBusy>()
            .is_some()
    );
    let queued = enqueue(&owner, "wildcard-busy", Payload::CheckSourceUpdates {});
    let worker = owner.job_worker(admission()).unwrap();
    let wildcard = worker.claim().unwrap().unwrap();
    assert!(wildcard.source_work.is_none());
    let duplicate = match source_lease(&worker, &wildcard.lease, Some(id)) {
        Err(error) => error,
        Ok(_) => panic!("duplicate wildcard Source lease accepted"),
    };
    assert!(
        duplicate
            .downcast_ref::<pp_storage::catalog::SourceBusy>()
            .is_some()
    );
    assert_eq!(get(&owner, &queued.job_id).state, PersistentState::Running);
    drop(direct);
    let mut released = source_lease(&worker, &wildcard.lease, Some(id)).unwrap();
    released.release().unwrap();
    owner.shutdown().unwrap();
}

#[cfg(feature = "desktop_rebuild")]
#[test]
fn abandoned_claim_reply_releases_source_authority() {
    let (_, owner) = fixture();
    let catalog = owner.local_source_catalog();
    let id = source(&catalog, "Abandoned claim");
    let queued = enqueue(
        &owner,
        "abandoned-claim",
        Payload::ImportScan {
            project_id: id as u64,
        },
    );
    let worker = owner.job_worker(admission()).unwrap();
    drop(
        worker
            .claim_pending(&AtomicBool::new(false), Duration::from_secs(5))
            .unwrap(),
    );
    catalog
        .execute(pp_storage::catalog::Request::Get { id })
        .unwrap();
    assert_eq!(get(&owner, &queued.job_id).state, PersistentState::Running);
    let mut lease = catalog
        .begin_work(id, &AtomicBool::new(false), Duration::from_secs(5))
        .unwrap();
    lease.release().unwrap();
    owner.shutdown().unwrap();
}

#[cfg(feature = "desktop_rebuild")]
#[test]
fn missing_source_job_fails_without_starving_healthy_work() {
    let (_, owner) = fixture();
    let valid_id = source(&owner.local_source_catalog(), "Healthy tail");
    let invalid = enqueue(
        &owner,
        "missing-head",
        Payload::ImportScan { project_id: 9876 },
    );
    std::thread::sleep(Duration::from_millis(1100));
    let valid = enqueue(
        &owner,
        "healthy-tail",
        Payload::ImportScan {
            project_id: valid_id as u64,
        },
    );
    let worker = owner.job_worker(admission()).unwrap();
    let claim = worker.claim().unwrap().unwrap();
    assert_eq!(claim.job.job_id, valid.job_id);
    let failed = get(&owner, &invalid.job_id);
    assert_eq!(failed.state, PersistentState::Failed);
    assert_eq!(failed.attempt, 0);
    assert_eq!(failed.generation, 0);
    assert_eq!(
        failed.recovery.as_deref(),
        Some("Source is unavailable for this job")
    );
    let history = match call(
        &owner,
        UserOperation::History {
            job_id: invalid.job_id,
            before_version: None,
            limit: 10,
        },
    )
    .unwrap()
    {
        Outcome::History(history) => history,
        _ => panic!("job history required"),
    };
    assert!(
        history
            .iter()
            .any(|entry| entry.event == "source_unavailable")
    );
    assert!(worker.claim().unwrap().is_none());
    owner.shutdown().unwrap();
}

#[cfg(feature = "desktop_rebuild")]
#[test]
fn foreign_source_job_fails_without_tenant_leak_or_starvation() {
    use pp_storage::catalog::Credentials;
    let (_, owner) = fixture();
    let (_, token) = register(&owner, "foreign-source@example.test");
    let tenant_catalog = owner.source_catalog(Credentials::Session(Secret::new(token)));
    let foreign_id = source(&tenant_catalog, "Foreign Source");
    let local_id = source(&owner.local_source_catalog(), "Local tail");
    let invalid = enqueue(
        &owner,
        "foreign-head",
        Payload::ExtractSourceDocs {
            project_id: foreign_id as u64,
        },
    );
    std::thread::sleep(Duration::from_millis(1100));
    let valid = enqueue(
        &owner,
        "local-tail",
        Payload::ExtractSourceDocs {
            project_id: local_id as u64,
        },
    );
    let worker = owner.job_worker(admission()).unwrap();
    let claim = worker.claim().unwrap().unwrap();
    assert_eq!(claim.job.job_id, valid.job_id);
    let failed = get(&owner, &invalid.job_id);
    assert_eq!(failed.state, PersistentState::Failed);
    assert_eq!(failed.attempt, 0);
    assert_eq!(failed.generation, 0);
    assert_eq!(
        failed.recovery.as_deref(),
        Some("Source is unavailable for this job")
    );
    let mut tenant_work = tenant_catalog
        .begin_work(foreign_id, &AtomicBool::new(false), Duration::from_secs(5))
        .unwrap();
    tenant_work.release().unwrap();
    owner.shutdown().unwrap();
}

#[cfg(feature = "desktop_rebuild")]
#[test]
fn individual_source_claim_prevents_overlap_with_spare_resource_capacity() {
    let (_, owner) = fixture();
    let id = source(&owner.local_source_catalog(), "Shared Source");
    let first = enqueue(
        &owner,
        "first",
        Payload::ImportScan {
            project_id: id as u64,
        },
    );
    let second = enqueue(
        &owner,
        "second",
        Payload::ExtractSourceDocs {
            project_id: id as u64,
        },
    );
    let mut config = admission();
    config.per_resource = 2;
    let worker = owner.job_worker(config).unwrap();
    let mut claimed = worker.claim().unwrap().unwrap();
    assert_eq!(claimed.job.job_id, first.job_id);
    assert!(claimed.source_work.is_some());
    let before = get(&owner, &second.job_id);
    assert!(worker.claim().unwrap().is_none());
    let after = get(&owner, &second.job_id);
    assert_eq!(after.state, PersistentState::Queued);
    assert_eq!(after.state_version, before.state_version);
    assert_eq!(after.attempt, 0);
    assert_eq!(after.generation, 0);
    worker
        .update(&mut claimed.lease, WorkerOperation::Finish(None))
        .unwrap();
    assert!(worker.claim().unwrap().is_none());
    drop(claimed.source_work.take());
    let next = worker.claim().unwrap().unwrap();
    assert_eq!(next.job.job_id, second.job_id);
    assert!(next.source_work.is_some());
    owner.shutdown().unwrap();
}
