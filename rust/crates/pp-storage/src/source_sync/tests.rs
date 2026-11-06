use super::*;
use crate::{
    Limits, WriterOwner,
    auth::{self, AuthPolicy, FirstUserTenant, RegistrationPolicy, SessionTenantPolicy},
    catalog,
    jobs::{
        self, AuthorityRefusalPhase, AuthorityRefusalReason, ClaimedAttempt, Credential,
        ServerWorkerClient, UserOperation, WorkerAdmission, WorkerOperation,
    },
    source_scan::LocalScanObservation,
};
use serde_json::Value;
use std::{
    path::PathBuf,
    sync::{Arc, atomic::AtomicBool},
    time::Duration,
};

fn policy() -> AuthPolicy {
    AuthPolicy {
        registration: RegistrationPolicy::Open,
        session_tenant: SessionTenantPolicy::AccountTenant,
        first_user: FirstUserTenant::NewUser,
    }
}

fn admission() -> WorkerAdmission {
    WorkerAdmission {
        kinds: vec![(JobKind::Sync, 1), (JobKind::CheckSourceUpdates, 1)],
        total: 1,
        per_resource: 1,
        lease_seconds: 60,
    }
}

struct Fixture {
    owner: WriterOwner,
    root: PathBuf,
    token: String,
    catalog: catalog::SourceCatalogClient,
    worker: ServerWorkerClient,
}

impl Fixture {
    fn new() -> Self {
        Self::with_admission(admission())
    }

    fn with_admission(admission: WorkerAdmission) -> Self {
        let root = std::env::temp_dir().join(format!("pp-source-sync-{}", rand::random::<u64>()));
        let (owner, _) = WriterOwner::open(&root, Limits::default()).unwrap();
        let auth::Outcome::Session { token, .. } = owner
            .auth(FirstUserTenant::NewUser)
            .submit(
                auth::Request::Register {
                    email: "source-sync@example.com".into(),
                    display_name: "Sync".into(),
                    password: auth::Secret::new("source-sync-test-password".into()),
                },
                Arc::new(AtomicBool::new(false)),
                Duration::from_secs(5),
            )
            .unwrap()
            .recv()
            .unwrap()
            .unwrap()
        else {
            panic!("session expected")
        };
        let token = token.expose().to_owned();
        let catalog = owner
            .source_catalog_with_policy(
                catalog::Credentials::Session(auth::Secret::new(token.clone())),
                policy(),
            )
            .unwrap();
        let worker = owner.job_worker_with_policy(policy(), admission).unwrap();
        Self {
            owner,
            root,
            token,
            catalog,
            worker,
        }
    }

    fn login(&mut self) {
        let auth::Outcome::Session { token, .. } = self
            .owner
            .auth(FirstUserTenant::NewUser)
            .submit(
                auth::Request::Login {
                    email: "source-sync@example.com".into(),
                    password: auth::Secret::new("source-sync-test-password".into()),
                },
                Arc::new(AtomicBool::new(false)),
                Duration::from_secs(5),
            )
            .unwrap()
            .recv()
            .unwrap()
            .unwrap()
        else {
            panic!("session expected")
        };
        self.token = token.expose().to_owned();
    }

    fn source(&self, name: &str) -> i64 {
        let catalog::Outcome::Source(Some(source)) = self
            .catalog
            .execute(catalog::Request::Create {
                source: catalog::CreateSource {
                    name: name.into(),
                    source_kind: Some("local".into()),
                    source_type: Some("local".into()),
                    ..Default::default()
                },
            })
            .unwrap()
        else {
            panic!("Source expected")
        };
        source.id
    }

    fn user(&self, operation: UserOperation) -> JobRecord {
        let jobs::Outcome::Job(job, _) = self
            .owner
            .jobs(policy())
            .unwrap()
            .submit(
                Credential::Session(auth::Secret::new(self.token.clone())),
                operation,
                &AtomicBool::new(false),
                Duration::from_secs(5),
            )
            .unwrap()
            .receive()
            .unwrap()
        else {
            panic!("Job expected")
        };
        job
    }

    fn enqueue(&self, project_ids: Option<Vec<u64>>) -> String {
        self.user(UserOperation::Enqueue {
            key: "sync".into(),
            payload_version: 1,
            payload: Payload::Sync { project_ids },
        })
        .job_id
    }

    fn open(&self) -> AttemptLease {
        let ClaimedAttempt {
            lease, source_work, ..
        } = self.worker.claim_kind(JobKind::Sync).unwrap().unwrap();
        assert!(source_work.is_none());
        self.worker.open_source_sync(&lease).unwrap();
        lease
    }

    fn next(&self, lease: &mut AttemptLease) -> NextSyncTarget {
        self.worker.claim_next_source_sync_target(lease).unwrap()
    }

    fn connection(&self) -> Connection {
        Connection::open(self.root.join("print-partner.db")).unwrap()
    }

    fn job(&self, job_id: &str) -> JobRecord {
        let mut connection = self.connection();
        let tx = connection.transaction().unwrap();
        crate::jobs::load(&tx, job_id).unwrap().unwrap()
    }

    fn targets(&self, job_id: &str) -> Vec<(i64, String, Option<String>)> {
        self.connection()
            .prepare("SELECT requested_source_id,state,failure_code FROM source_sync_targets WHERE job_id=?1 ORDER BY ordinal")
            .unwrap()
            .query_map([job_id], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))
            .unwrap()
            .collect::<rusqlite::Result<_>>()
            .unwrap()
    }

    fn batch(&self, job_id: &str) -> (String, Option<String>) {
        self.connection()
            .query_row(
                "SELECT phase,halt_code FROM source_sync_batches WHERE job_id=?1",
                [job_id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap()
    }

    fn snapshot(&self) -> Value {
        database_snapshot(&self.connection())
    }

    fn begin_work(&self, id: i64) -> Result<catalog::SourceWorkLease> {
        self.catalog
            .begin_work(id, &AtomicBool::new(false), Duration::from_secs(5))
    }
}

fn database_snapshot(connection: &Connection) -> Value {
    let rows = |sql: &str| -> Vec<Vec<String>> {
        let mut statement = connection.prepare(sql).unwrap();
        let width = statement.column_count();
        statement
            .query_map([], |row| {
                (0..width)
                    .map(|index| row.get::<_, rusqlite::types::Value>(index))
                    .map(|value| value.map(|value| format!("{value:?}")))
                    .collect()
            })
            .unwrap()
            .collect::<rusqlite::Result<_>>()
            .unwrap()
    };
    serde_json::json!([
        rows("SELECT * FROM source_sync_batches"),
        rows("SELECT * FROM source_sync_targets ORDER BY ordinal"),
        rows("SELECT document FROM durable_jobs"),
        rows("SELECT count(*) FROM durable_job_history"),
    ])
}

fn local(target: NextSyncTarget) -> SyncTargetLease {
    let NextSyncTarget::Local(target) = target else {
        panic!("Local target expected")
    };
    target
}

#[test]
fn generic_sync_finish_and_effects_preserve_the_claim_and_reservation() {
    let mut sync = Fixture::new();
    let id = sync.source("Owned target");
    sync.enqueue(Some(vec![id as u64]));
    let mut lease = sync.open();
    let target = local(sync.next(&mut lease));
    let proof = target.source_work.proof().unwrap();
    let operations = || {
        let receipt = jobs::ResultArtifact {
            receipt_id: "generic-receipt".into(),
            content_hash: "b".repeat(64),
            target: id.to_string(),
        };
        let mut operations = vec![
            WorkerOperation::Finish(None),
            WorkerOperation::Finish(Some(receipt.clone())),
            WorkerOperation::FinishPublic {
                artifact: None,
                result: Box::new(jobs::CompletedResult::SourceSync(jobs::SourceSyncResult {
                    synced: 1,
                    failed: 0,
                    results: vec![],
                    failures: vec![],
                })),
            },
            WorkerOperation::ConfirmEffect(receipt),
        ];
        operations.extend(
            [
                jobs::EffectOperation::SourceRefresh,
                jobs::EffectOperation::LocalArtifact,
                jobs::EffectOperation::PrinterUpload,
                jobs::EffectOperation::PrinterUploadAndStart,
                jobs::EffectOperation::PrinterStart,
                jobs::EffectOperation::SpoolmanDeduction,
            ]
            .map(|operation| {
                WorkerOperation::BeginEffect(jobs::EffectIntent {
                    operation,
                    basis_hash: "a".repeat(64),
                    content_hash: "b".repeat(64),
                    target: id.to_string(),
                })
            }),
        );
        operations
    };
    let before = sync.snapshot();
    let version = lease.version();
    for revoke in [false, true] {
        if revoke {
            sync.connection()
                .execute("DELETE FROM sessions", [])
                .unwrap();
            sync.login();
            sync.catalog = sync
                .owner
                .source_catalog_with_policy(
                    catalog::Credentials::Session(auth::Secret::new(sync.token.clone())),
                    policy(),
                )
                .unwrap();
        }
        for operation in operations() {
            let error = sync.worker.update(&mut lease, operation).unwrap_err();
            assert_eq!(
                error.to_string(),
                "Sync operation belongs to its journal owner"
            );
            assert_eq!(sync.snapshot(), before);
            assert_eq!(lease.version(), version);
            let retained = target.source_work.proof().unwrap();
            assert_eq!(retained.incarnation, proof.incarnation);
            assert_eq!(retained.token, proof.token);
            assert!(
                sync.begin_work(id)
                    .err()
                    .unwrap()
                    .downcast_ref::<SourceBusy>()
                    .is_some()
            );
        }
    }
    drop(target);
    sync.begin_work(id).unwrap().release().unwrap();
    let Fixture { owner, root, .. } = sync;
    owner.shutdown().unwrap();
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn non_sync_generic_effects_and_finish_remain_available() {
    let sync = Fixture::new();
    sync.user(UserOperation::Enqueue {
        key: "updates".into(),
        payload_version: 1,
        payload: Payload::CheckSourceUpdates {},
    });
    let worker = &sync.worker;
    let mut lease = worker
        .claim_kind(JobKind::CheckSourceUpdates)
        .unwrap()
        .unwrap()
        .lease;
    for operation in [
        jobs::EffectOperation::SourceRefresh,
        jobs::EffectOperation::LocalArtifact,
    ] {
        let target = format!("{operation:?}");
        worker
            .update(
                &mut lease,
                WorkerOperation::BeginEffect(jobs::EffectIntent {
                    operation,
                    basis_hash: "a".repeat(64),
                    content_hash: "b".repeat(64),
                    target: target.clone(),
                }),
            )
            .unwrap();
        worker
            .update(
                &mut lease,
                WorkerOperation::ConfirmEffect(jobs::ResultArtifact {
                    receipt_id: target.clone(),
                    content_hash: "b".repeat(64),
                    target,
                }),
            )
            .unwrap();
    }
    let done = worker
        .update(&mut lease, WorkerOperation::Finish(None))
        .unwrap();
    assert_eq!(done.state, PersistentState::Succeeded);
    assert_eq!(done.progress, Some(100));
    assert_eq!(done.effects.len(), 2);
    assert!(done.effects.iter().all(|effect| effect.confirmed));
    let Fixture { owner, root, .. } = sync;
    owner.shutdown().unwrap();
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn targets_follow_the_payload_order_and_unavailable_sources_fail() {
    let golden: Value = serde_json::from_str(include_str!(
        "../../../../../fixtures/parity/source-sync/source-sync.json"
    ))
    .unwrap();
    let sync = Fixture::new();
    let local_id = sync.source("Local");
    let remote = sync.source("Remote");
    sync.connection()
        .execute(
            "UPDATE projects SET source_type='github' WHERE id=?1",
            [remote],
        )
        .unwrap();
    // Node attempts the payload in order: the missing id, then the Local Source.
    let missing = golden["project_ids"][0].as_i64().unwrap();
    assert_eq!(golden["attempts"].as_array().unwrap().len(), 2);
    let job_id = sync.enqueue(Some(vec![missing as u64, remote as u64, local_id as u64]));
    let mut lease = sync.open();
    let version = lease.version();
    let NextSyncTarget::OrdinaryFailure(failure) = sync.next(&mut lease) else {
        panic!("ordinary failure expected")
    };
    assert_eq!(
        serde_json::to_value(&failure).unwrap(),
        golden["result"]["failures"][0]
    );
    assert_eq!(lease.version(), version + 1);
    assert_eq!(sync.job(&job_id).progress, Some(33));
    let NextSyncTarget::OrdinaryFailure(failure) = sync.next(&mut lease) else {
        panic!("ordinary failure expected")
    };
    assert_eq!(
        failure,
        SourceSyncFailure {
            project_id: Some(remote as u64),
            name: Some("Remote".into()),
            error: "Source kind is not supported by Local Sync".into(),
        }
    );
    assert_eq!(sync.job(&job_id).progress, Some(66));
    let target = local(sync.next(&mut lease));
    assert_eq!(target.key().ordinal, 2);
    assert_eq!(target.observation().source_id(), local_id);
    assert_eq!(
        sync.targets(&job_id),
        [
            (missing, "failed".into(), Some("unavailable".into())),
            (remote, "failed".into(), Some("unsupported_kind".into())),
            (local_id, "claimed".into(), None)
        ]
    );
    sync.begin_work(remote).unwrap().release().unwrap();
}

#[test]
fn all_selection_seals_the_least_recently_synced_sources_first() {
    let sync = Fixture::new();
    let [first, second, third] = ["First", "Second", "Third"].map(|name| sync.source(name));
    sync.connection()
        .execute_batch(&format!(
            "UPDATE projects SET last_synced_at='2' WHERE id={first};
             UPDATE projects SET last_synced_at='1' WHERE id={second};"
        ))
        .unwrap();
    let job_id = sync.enqueue(None);
    let lease = sync.open();
    let sealed = vec![
        (third, "pending".into(), None),
        (second, "pending".into(), None),
        (first, "pending".into(), None),
    ];
    assert_eq!(sync.targets(&job_id), sealed);
    let late = sync.source("Late");
    sync.worker.open_source_sync(&lease).unwrap();
    assert_eq!(sync.targets(&job_id), sealed);
    sync.connection()
        .execute(
            "UPDATE source_sync_targets SET requested_source_id=?1 WHERE job_id=?2 AND ordinal=0",
            params![late, job_id],
        )
        .unwrap();
    let before = sync.snapshot();
    let error = sync.worker.open_source_sync(&lease).unwrap_err();
    assert_eq!(error.to_string(), "Sync target basis mismatch");
    assert_eq!(sync.snapshot(), before);
}

#[test]
fn recovery_returns_a_claim_to_pending_and_keeps_its_counter() {
    let sync = Fixture::new();
    let id = sync.source("Recovered");
    let job_id = sync.enqueue(Some(vec![id as u64]));
    let mut lease = sync.open();
    drop(local(sync.next(&mut lease)));
    let Fixture { owner, root, .. } = sync;
    owner.shutdown().unwrap();
    let (owner, _) = WriterOwner::open(&root, Limits::default()).unwrap();
    let worker = owner.job_worker_with_policy(policy(), admission()).unwrap();
    let ClaimedAttempt { mut lease, .. } = worker.claim_kind(JobKind::Sync).unwrap().unwrap();
    worker.open_source_sync(&lease).unwrap();
    let reset: (String, Option<i64>, Option<String>, i64) = Connection::open(root.join("print-partner.db"))
        .unwrap()
        .query_row(
            "SELECT state,parent_generation,reservation_token,claim_generation FROM source_sync_targets WHERE job_id=?1",
            [&job_id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )
        .unwrap();
    assert_eq!(reset, ("pending".into(), None, None, 1));
    let target = local(worker.claim_next_source_sync_target(&mut lease).unwrap());
    assert_eq!(target.claim_generation, 2);
}

#[test]
fn claimed_target_records_its_attempt_and_holds_the_source() {
    let sync = Fixture::new();
    let id = sync.source("Held");
    let job_id = sync.enqueue(Some(vec![id as u64]));
    let ClaimedAttempt { mut lease, .. } = sync.worker.claim_kind(JobKind::Sync).unwrap().unwrap();
    let bridge = sync
        .worker
        .begin_source_work(
            &lease,
            Some(id),
            &AtomicBool::new(false),
            Duration::from_secs(5),
        )
        .err()
        .unwrap();
    assert_eq!(bridge.to_string(), "Attempt is not Source work");
    sync.worker.open_source_sync(&lease).unwrap();
    let version = lease.version();
    let target = local(sync.next(&mut lease));
    assert_eq!(lease.version(), version);
    let proof = target.source_work.proof().unwrap();
    let observation = &target.observation;
    type Captured = (
        String,
        i64,
        i64,
        String,
        String,
        String,
        String,
        i64,
        String,
    );
    let captured: Captured = sync
        .connection()
        .query_row(
            "SELECT state,claim_generation,parent_generation,attempt_worker,attempt_fence_digest,reservation_incarnation,reservation_token,configuration_version,authority_actor FROM source_sync_targets WHERE job_id=?1",
            [&job_id],
            |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                    row.get(5)?,
                    row.get(6)?,
                    row.get(7)?,
                    row.get(8)?,
                ))
            },
        )
        .unwrap();
    assert_eq!(
        captured,
        (
            "claimed".into(),
            1,
            lease.generation(),
            lease.worker().into(),
            digest(lease.fence()),
            proof.incarnation.clone(),
            format!("{:016x}", proof.token),
            observation.configuration_version,
            observation.authority_actor.clone(),
        )
    );
    assert_eq!(target.claim_generation, 1);
    let busy = sync.begin_work(id).err().unwrap();
    assert!(busy.downcast_ref::<SourceBusy>().is_some(), "{busy:#}");
    let again = sync
        .worker
        .claim_next_source_sync_target(&mut lease)
        .err()
        .unwrap();
    assert_eq!(again.to_string(), "Prior Sync target is not pending");
    drop(target);
    sync.begin_work(id).unwrap().release().unwrap();
}

#[test]
fn busy_source_leaves_the_target_pending() {
    let sync = Fixture::new();
    let id = sync.source("Busy");
    let job_id = sync.enqueue(Some(vec![id as u64]));
    let mut lease = sync.open();
    let mut held = sync.begin_work(id).unwrap();
    let before = sync.snapshot();
    let version = lease.version();
    let error = sync
        .worker
        .claim_next_source_sync_target(&mut lease)
        .err()
        .unwrap();
    assert!(error.downcast_ref::<SourceBusy>().is_some(), "{error:#}");
    assert_eq!(sync.snapshot(), before);
    assert_eq!(lease.version(), version);
    held.release().unwrap();
    assert_eq!(local(sync.next(&mut lease)).key().ordinal, 0);
    assert_eq!(sync.targets(&job_id)[0].1, "claimed");
}

fn reopen(sync: Fixture) -> Fixture {
    reopen_with_admission(sync, admission())
}

fn reopen_with_admission(sync: Fixture, admission: WorkerAdmission) -> Fixture {
    let Fixture {
        owner, root, token, ..
    } = sync;
    owner.shutdown().unwrap();
    let (owner, _) = WriterOwner::open(&root, Limits::default()).unwrap();
    let catalog = owner
        .source_catalog_with_policy(
            catalog::Credentials::Session(auth::Secret::new(token.clone())),
            policy(),
        )
        .unwrap();
    let worker = owner.job_worker_with_policy(policy(), admission).unwrap();
    Fixture {
        owner,
        root,
        token,
        catalog,
        worker,
    }
}

fn change_job(sync: &Fixture, job_id: &str, change: impl FnOnce(&mut JobRecord)) {
    let mut connection = sync.connection();
    let tx = connection.transaction().unwrap();
    let mut job = jobs::load(&tx, job_id).unwrap().unwrap();
    change(&mut job);
    jobs::save(&tx, &mut job, "test_recovery_setup").unwrap();
    tx.commit().unwrap();
}

fn enqueue_later(sync: &Fixture, source_id: i64) -> String {
    sync.user(UserOperation::Enqueue {
        key: "later-sync".into(),
        payload_version: 1,
        payload: Payload::Sync {
            project_ids: Some(vec![source_id as u64]),
        },
    })
    .job_id
}

fn durable_claim_blocks_a_second_sync(restart: bool) {
    let admission = WorkerAdmission {
        kinds: vec![(JobKind::Sync, 2)],
        total: 2,
        per_resource: 2,
        ..admission()
    };
    let mut sync = Fixture::with_admission(admission.clone());
    let id = sync.source("Durably claimed");
    let first_job = sync.enqueue(Some(vec![id as u64]));
    let mut first = sync.open();
    drop(local(sync.next(&mut first)));
    if restart {
        sync = reopen_with_admission(sync, admission);
    } else {
        change_job(&sync, &first_job, |job| job.lease_until = Some(now() - 1));
    }
    let ClaimedAttempt {
        lease: resumed_first,
        ..
    } = sync.worker.claim_kind(JobKind::Sync).unwrap().unwrap();
    assert_eq!(resumed_first.job_id(), first_job);
    let second_job = enqueue_later(&sync, id);
    let worker = &sync.worker;
    let ClaimedAttempt {
        lease: mut second, ..
    } = worker.claim_kind(JobKind::Sync).unwrap().unwrap();
    assert_eq!(second.job_id(), second_job);
    worker.open_source_sync(&second).unwrap();
    let before = sync.snapshot();
    let version = second.version();
    let error = worker
        .claim_next_source_sync_target(&mut second)
        .err()
        .unwrap();
    assert!(error.downcast_ref::<SourceBusy>().is_some(), "{error:#}");
    assert_eq!(sync.snapshot(), before);
    assert_eq!(second.version(), version);
    assert_eq!(sync.targets(&second_job), [(id, "pending".into(), None)]);
    sync.worker.open_source_sync(&resumed_first).unwrap();
    let target = local(worker.claim_next_source_sync_target(&mut second).unwrap());
    assert_eq!(target.claim_generation, 1);
    assert_eq!(sync.targets(&second_job), [(id, "claimed".into(), None)]);
    drop(target);
    let Fixture { owner, root, .. } = sync;
    owner.shutdown().unwrap();
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn durable_claim_returns_source_busy_after_restart() {
    durable_claim_blocks_a_second_sync(true);
}

#[test]
fn durable_claim_returns_source_busy_after_lease_expiry() {
    durable_claim_blocks_a_second_sync(false);
}

#[test]
fn unacknowledged_failure_blocks_claim_after_restart() {
    let admission = WorkerAdmission {
        kinds: vec![(JobKind::Sync, 2)],
        total: 2,
        per_resource: 2,
        ..admission()
    };
    let mut sync = Fixture::with_admission(admission.clone());
    let id = sync.source("Unacknowledged failure");
    let first_job = sync.enqueue(Some(vec![id as u64]));
    let mut first = sync.open();
    let target = local(sync.next(&mut first));
    sync.worker
        .fail_source_sync_target(&mut first, &target, "disk read failed")
        .unwrap();
    drop(target);
    let failed = failed_hold(&sync, &first_job);
    assert_eq!(
        (
            failed.0.as_str(),
            failed.1.as_str(),
            failed.2.as_str(),
            failed.3,
        ),
        ("failed", "local_read", "disk read failed", 0)
    );
    sync = reopen_with_admission(sync, admission);
    let ClaimedAttempt {
        lease: resumed_first,
        ..
    } = sync.worker.claim_kind(JobKind::Sync).unwrap().unwrap();
    assert_eq!(resumed_first.job_id(), first_job);
    let second_job = enqueue_later(&sync, id);
    let ClaimedAttempt {
        lease: mut second, ..
    } = sync.worker.claim_kind(JobKind::Sync).unwrap().unwrap();
    assert_eq!(second.job_id(), second_job);
    sync.worker.open_source_sync(&second).unwrap();
    let before = sync.snapshot();
    let version = second.version();
    let error = sync
        .worker
        .claim_next_source_sync_target(&mut second)
        .err()
        .unwrap();
    assert!(error.downcast_ref::<SourceBusy>().is_some(), "{error:#}");
    assert_eq!(sync.snapshot(), before);
    assert_eq!(second.version(), version);
    assert_eq!(sync.targets(&second_job), [(id, "pending".into(), None)]);
    assert_eq!(failed_hold(&sync, &first_job), failed);
    let Fixture { owner, root, .. } = sync;
    owner.shutdown().unwrap();
    std::fs::remove_dir_all(root).unwrap();
}

fn failed_hold(sync: &Fixture, job_id: &str) -> (String, String, String, i64, String, i64) {
    sync.connection()
        .query_row(
            "SELECT state,failure_code,failure_detail,reservation_acknowledged,reservation_token,claim_generation FROM source_sync_targets WHERE job_id=?1 AND ordinal=0",
            [job_id],
            |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                    row.get(5)?,
                ))
            },
        )
        .unwrap()
}

fn terminal_recovery_closes_sync_batch(reason: &str, effect_confirmed: bool) {
    for restart in [false, true] {
        for claimed in [false, true] {
            let mut sync = Fixture::new();
            let [first, second] = ["First", "Second"].map(|name| sync.source(name));
            let job_id = sync.enqueue(Some(vec![first as u64, second as u64]));
            if reason == "attempt_limit" {
                change_job(&sync, &job_id, |job| {
                    job.attempt = 99;
                    job.generation = 99;
                });
            }
            let mut lease = sync.open();
            if claimed {
                drop(local(sync.next(&mut lease)));
            }
            let before = sync.job(&job_id);
            change_job(&sync, &job_id, |job| {
                job.lease_until = Some(now() - 1);
                if reason == "cancelled" {
                    job.cancel_requested = true;
                } else if reason == "effect" {
                    job.state = PersistentState::EffectAdmitted;
                    job.effects.push(jobs::EffectReceipt {
                        intent: jobs::EffectIntent {
                            operation: jobs::EffectOperation::SourceRefresh,
                            basis_hash: "a".repeat(64),
                            content_hash: "b".repeat(64),
                            target: first.to_string(),
                        },
                        attempt: job.attempt,
                        generation: job.generation,
                        confirmed: effect_confirmed,
                        no_effect: false,
                        receipt: effect_confirmed.then(|| jobs::ResultArtifact {
                            receipt_id: "sync-receipt".into(),
                            content_hash: "b".repeat(64),
                            target: first.to_string(),
                        }),
                        checklist_completion: None,
                    });
                }
            });
            let effects = sync.job(&job_id).effects;
            if restart {
                sync = reopen(sync);
            }
            assert!(sync.worker.claim_kind(JobKind::Sync).unwrap().is_none());
            let recovered = sync.job(&job_id);
            let unresolved = claimed || reason == "effect";
            assert_eq!(
                recovered.state,
                if unresolved {
                    PersistentState::ReconciliationRequired
                } else if reason == "cancelled" {
                    PersistentState::Cancelled
                } else {
                    PersistentState::Failed
                }
            );
            assert_eq!(recovered.generation, before.generation + 1);
            assert!(recovered.fence.is_none());
            assert!(recovered.worker.is_none());
            assert!(recovered.lease_until.is_none());
            assert_eq!(recovered.effects, effects);
            let detail = match reason {
                "attempt_limit" => "Attempt limit reached",
                "cancelled" => "Sync was cancelled",
                "effect" if effect_confirmed => {
                    "Resume from confirmed receipts requires handler reconciliation"
                }
                "effect" => "Inspect effect receipts and reconcile explicitly",
                _ => unreachable!(),
            };
            assert_eq!(recovered.recovery.as_deref(), Some(detail));
            assert_eq!(
                sync.targets(&job_id),
                [
                    if claimed {
                        (
                            first,
                            "reconciliation_required".into(),
                            Some("local_settlement".into()),
                        )
                    } else {
                        (first, "not_run".into(), Some("batch_halt".into()))
                    },
                    (second, "not_run".into(), Some("batch_halt".into())),
                ]
            );
            assert_eq!(
                sync.batch(&job_id),
                if unresolved {
                    ("reconciliation_required".into(), None)
                } else {
                    (
                        "halted".into(),
                        Some(
                            if reason == "cancelled" {
                                "cancelled"
                            } else {
                                "fatal"
                            }
                            .into(),
                        ),
                    )
                }
            );
            let snapshot = sync.snapshot();
            sync = reopen(sync);
            assert_eq!(sync.snapshot(), snapshot);
            let later_source = sync.source("Later work");
            let later = enqueue_later(&sync, later_source);
            let mut lease = sync.open();
            assert_eq!(lease.job_id(), later);
            assert_eq!(local(sync.next(&mut lease)).claim_generation, 1);
            assert_eq!(
                sync.targets(&later),
                [(later_source, "claimed".into(), None)]
            );
            let Fixture { owner, root, .. } = sync;
            owner.shutdown().unwrap();
            std::fs::remove_dir_all(root).unwrap();
        }
    }
}

#[test]
fn attempt_limit_recovery_closes_sync_batch() {
    terminal_recovery_closes_sync_batch("attempt_limit", false);
}

#[test]
fn unconfirmed_effect_recovery_closes_sync_batch() {
    terminal_recovery_closes_sync_batch("effect", false);
}

#[test]
fn confirmed_effect_recovery_closes_sync_batch() {
    terminal_recovery_closes_sync_batch("effect", true);
}

#[test]
fn cancellation_recovery_closes_sync_batch() {
    terminal_recovery_closes_sync_batch("cancelled", false);
}

fn pad_running_sync_document_over_limit(sync: &Fixture, job_id: &str, lease_until: i64) -> String {
    let connection = sync.connection();
    let original: String = connection
        .query_row(
            "SELECT document FROM durable_jobs WHERE id=?1",
            [job_id],
            |row| row.get(0),
        )
        .unwrap();
    let mut value: Value = serde_json::from_str(&original).unwrap();
    value["state"] = "running".into();
    value["state_version"] = 9.into();
    value["generation"] = 9.into();
    value["attempt"] = 1.into();
    value["lease_until"] = lease_until.into();
    value["_attempt_fence"] = "".into();
    value["_attempt_worker"] = "".into();
    value["recovery"] = "".into();
    // One byte past the limit so recover_in decode hits JobDocTooLarge and
    // quarantines before halt_job or re-queue can rewrite the document.
    let width = jobs::JOB_DOC_SIZE_LIMIT + 1 - serde_json::to_string(&value).unwrap().len();
    value["recovery"] = "x".repeat(width).into();
    let document = serde_json::to_string(&value).unwrap();
    assert!(document.len() > jobs::JOB_DOC_SIZE_LIMIT);
    connection
        .execute(
            "UPDATE durable_jobs SET document=?2,state='running',version=9,generation=9,lease_until=?3 WHERE id=?1",
            rusqlite::params![job_id, document, lease_until],
        )
        .unwrap();
    document
}

/// Pads with confirmed effects (kept by `halt_job`) so decode succeeds, then
/// `halt_job`'s recovery / state / finished_at rewrite tips `save` over the limit.
fn pad_running_sync_just_under_limit_for_halt(
    sync: &Fixture,
    job_id: &str,
    lease_until: i64,
) -> String {
    let connection = sync.connection();
    let original: String = connection
        .query_row(
            "SELECT document FROM durable_jobs WHERE id=?1",
            [job_id],
            |row| row.get(0),
        )
        .unwrap();
    let mut value: Value = serde_json::from_str(&original).unwrap();
    value["state"] = "running".into();
    value["state_version"] = 9.into();
    value["generation"] = 9.into();
    value["attempt"] = 100.into();
    value["lease_until"] = lease_until.into();
    value["_attempt_fence"] = "".into();
    value["_attempt_worker"] = "".into();
    value["recovery"] = Value::Null;
    value["progress"] = Value::Null;
    value["finished_at"] = Value::Null;
    value["result"] = Value::Null;
    value["effects"] = Value::Array(vec![]);
    // Leave headroom smaller than halt_job's recovery + state growth so save fails.
    const HEADROOM: usize = 48;
    let effect = |target: &str| {
        serde_json::json!({
            "intent": {
                "operation": "source_refresh",
                "basis_hash": "a".repeat(64),
                "content_hash": "b".repeat(64),
                "target": target,
            },
            "attempt": 100,
            "generation": 9,
            "confirmed": true,
            "receipt": {
                "receipt_id": "halt-pad",
                "content_hash": "b".repeat(64),
                "target": target,
            },
        })
    };
    loop {
        let mut next = value.clone();
        let mut effects = next["effects"].as_array().unwrap().clone();
        effects.push(effect(&"x".repeat(1024)));
        next["effects"] = Value::Array(effects);
        if serde_json::to_string(&next).unwrap().len() > jobs::JOB_DOC_SIZE_LIMIT - HEADROOM {
            break;
        }
        value = next;
    }
    // Grow the last effect target (capped at 1024) until the document sits in
    // [LIMIT - HEADROOM, LIMIT]. Targets appear twice (intent + receipt).
    let budget = jobs::JOB_DOC_SIZE_LIMIT - HEADROOM;
    let mut lo = 1usize;
    let mut hi = 1024usize;
    let mut best = value.clone();
    while lo <= hi {
        let mid = (lo + hi) / 2;
        let mut effects = value["effects"].as_array().unwrap().clone();
        effects.push(effect(&"y".repeat(mid)));
        let mut candidate = value.clone();
        candidate["effects"] = Value::Array(effects);
        let len = serde_json::to_string(&candidate).unwrap().len();
        if len <= budget {
            best = candidate;
            lo = mid + 1;
        } else {
            hi = mid - 1;
        }
    }
    value = best;
    let document = serde_json::to_string(&value).unwrap();
    assert!(
        document.len() <= jobs::JOB_DOC_SIZE_LIMIT,
        "decode must succeed: {}",
        document.len()
    );
    // Targets are duplicated in intent+receipt, so length steps by 2.
    assert!(
        document.len() + 1 >= budget,
        "must sit near the limit: {} (budget {budget})",
        document.len()
    );
    connection
        .execute(
            "UPDATE durable_jobs SET document=?2,state='running',version=9,generation=9,lease_until=?3 WHERE id=?1",
            rusqlite::params![job_id, document, lease_until],
        )
        .unwrap();
    document
}

fn assert_quarantined_sync(
    sync: &Fixture,
    job_id: &str,
    document: &str,
    resource_before: &str,
    version: i64,
    reason_needle: &str,
) {
    let jobs::Outcome::Quarantined(rows) = sync
        .owner
        .jobs(policy())
        .unwrap()
        .submit(
            Credential::Session(auth::Secret::new(sync.token.clone())),
            UserOperation::Quarantined {
                before: None,
                limit: 200,
            },
            &AtomicBool::new(false),
            Duration::from_secs(5),
        )
        .unwrap()
        .receive()
        .unwrap()
    else {
        panic!("Expected quarantine list");
    };
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].job_id, job_id);
    assert!(rows[0].reason.contains(reason_needle), "{}", rows[0].reason);
    assert_eq!(rows[0].uncertain_resource.as_deref(), Some(resource_before));
    let stored: (String, String, String, i64, Option<i64>) = sync
        .connection()
        .query_row(
            "SELECT document,state,resource,version,lease_until FROM durable_jobs WHERE id=?1",
            [job_id],
            |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                ))
            },
        )
        .unwrap();
    assert_eq!(stored.0, document);
    assert_eq!(stored.1, "quarantined");
    assert_eq!(stored.2, resource_before);
    assert_eq!(stored.3, version);
    assert_eq!(stored.4, None);
    let event: String = sync
        .connection()
        .query_row(
            "SELECT event FROM durable_job_history WHERE job_id=?1 ORDER BY version DESC LIMIT 1",
            [job_id],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(event, "document_quarantined_resource_uncertain");
    assert!(
        sync.owner
            .jobs(policy())
            .unwrap()
            .submit(
                Credential::Session(auth::Secret::new(sync.token.clone())),
                UserOperation::Get {
                    job_id: job_id.to_owned()
                },
                &AtomicBool::new(false),
                Duration::from_secs(5),
            )
            .unwrap()
            .receive()
            .is_err(),
        "quarantined Sync Job must not load through Get"
    );
}

fn oversized_sync_recovery_quarantines(expired_lease: bool, claimed: bool) {
    // per_resource: 1 so a quarantined Sync blocks another Job on the same Sources.
    let admission = WorkerAdmission {
        kinds: vec![(JobKind::Sync, 2)],
        total: 2,
        per_resource: 1,
        ..admission()
    };
    let mut sync = Fixture::with_admission(admission.clone());
    let [first, second] = ["First", "Second"].map(|name| sync.source(name));
    let job_id = sync.enqueue(Some(vec![first as u64, second as u64]));
    let mut lease = sync.open();
    if claimed {
        drop(local(sync.next(&mut lease)));
    }
    drop(lease);
    let targets_before = sync.targets(&job_id);
    let batch_before = sync.batch(&job_id);
    let resource_before: String = sync
        .connection()
        .query_row(
            "SELECT resource FROM durable_jobs WHERE id=?1",
            [&job_id],
            |row| row.get(0),
        )
        .unwrap();
    assert!(!resource_before.is_empty(), "{resource_before}");
    // Live lease: 2_000_000_000 = 2033-05-18 UTC. Expired: 0.
    // Startup cells reopen → recover(all=true). Expired-lease cells pad on the
    // live owner and claim → recover_in(tx, false); they must not reopen.
    let lease_until = if expired_lease { 0 } else { 2_000_000_000 };
    let document = pad_running_sync_document_over_limit(&sync, &job_id, lease_until);
    if expired_lease {
        assert!(
            sync.worker.claim_kind(JobKind::Sync).unwrap().is_none(),
            "expired-lease cell must hit claim→recover_in(false)"
        );
    } else {
        sync = reopen_with_admission(sync, admission);
    }
    assert_quarantined_sync(
        &sync,
        &job_id,
        &document,
        &resource_before,
        10,
        "131072-byte limit",
    );
    assert_eq!(sync.targets(&job_id), targets_before);
    assert_eq!(sync.batch(&job_id), batch_before);
    let same_source = sync
        .user(UserOperation::Enqueue {
            key: format!("blocked-overlap-{expired_lease}-{claimed}"),
            payload_version: 1,
            payload: Payload::Sync {
                project_ids: Some(vec![first as u64]),
            },
        })
        .job_id;
    assert!(
        sync.worker.claim_kind(JobKind::Sync).unwrap().is_none(),
        "quarantined Sources must block admission of overlapping Sync work"
    );
    assert_eq!(sync.job(&same_source).state, PersistentState::Queued);
    let later_source = sync.source("Later work");
    let later = enqueue_later(&sync, later_source);
    let mut later_lease = sync.open();
    assert_eq!(later_lease.job_id(), later);
    assert_eq!(local(sync.next(&mut later_lease)).claim_generation, 1);
    assert_eq!(
        sync.targets(&later),
        [(later_source, "claimed".into(), None)]
    );
    let Fixture { owner, root, .. } = sync;
    owner.shutdown().unwrap();
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn oversized_sync_recovery_quarantines_on_startup_and_lease_expiry() {
    for expired_lease in [false, true] {
        for claimed in [false, true] {
            oversized_sync_recovery_quarantines(expired_lease, claimed);
        }
    }
}

#[test]
fn halt_job_doc_too_large_quarantines_with_batch_and_targets() {
    // Decode succeeds; halt_job writes batch/targets then save hits JobDocTooLarge;
    // recover_in quarantines in the same transaction with the original document.
    // Drive claim→recover_in(false) on the live owner: the pad uses more than 16
    // effects (schema validate on reopen would refuse before recover).
    let admission = WorkerAdmission {
        kinds: vec![(JobKind::Sync, 2)],
        total: 2,
        per_resource: 1,
        ..admission()
    };
    let sync = Fixture::with_admission(admission);
    let [first, second] = ["First", "Second"].map(|name| sync.source(name));
    let job_id = sync.enqueue(Some(vec![first as u64, second as u64]));
    let mut lease = sync.open();
    drop(local(sync.next(&mut lease)));
    drop(lease);
    let resource_before: String = sync
        .connection()
        .query_row(
            "SELECT resource FROM durable_jobs WHERE id=?1",
            [&job_id],
            |row| row.get(0),
        )
        .unwrap();
    assert!(!resource_before.is_empty(), "{resource_before}");
    let document = pad_running_sync_just_under_limit_for_halt(&sync, &job_id, 0);
    assert!(
        sync.worker.claim_kind(JobKind::Sync).unwrap().is_none(),
        "claim must run recover_in(false) through halt_job→JobDocTooLarge"
    );
    // Stored document still decodes; list reason is the generic quarantine text.
    // stderr from recover_in records the halted encode's JobDocTooLarge size.
    assert_quarantined_sync(
        &sync,
        &job_id,
        &document,
        &resource_before,
        10,
        "Document quarantined; operator inspection required",
    );
    assert_eq!(
        sync.targets(&job_id),
        [
            (
                first,
                "reconciliation_required".into(),
                Some("local_settlement".into()),
            ),
            (second, "not_run".into(), Some("batch_halt".into())),
        ]
    );
    assert_eq!(
        sync.batch(&job_id),
        ("reconciliation_required".into(), None)
    );
    let blocked = sync
        .user(UserOperation::Enqueue {
            key: "halt-blocked-overlap".into(),
            payload_version: 1,
            payload: Payload::Sync {
                project_ids: Some(vec![first as u64]),
            },
        })
        .job_id;
    assert!(sync.worker.claim_kind(JobKind::Sync).unwrap().is_none());
    assert_eq!(sync.job(&blocked).state, PersistentState::Queued);
    let later_source = sync.source("Later work");
    let later = enqueue_later(&sync, later_source);
    let mut later_lease = sync.open();
    assert_eq!(later_lease.job_id(), later);
    assert_eq!(local(sync.next(&mut later_lease)).claim_generation, 1);
    let Fixture { owner, root, .. } = sync;
    owner.shutdown().unwrap();
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn cancel_and_revoked_authority_close_the_batch() {
    for (revoke, claimed) in [(false, false), (false, true), (true, false), (true, true)] {
        let sync = Fixture::new();
        let [first, second] = ["First", "Second"].map(|name| sync.source(name));
        let job_id = sync.enqueue(Some(vec![first as u64, second as u64]));
        let mut lease = sync.open();
        let target = claimed.then(|| local(sync.next(&mut lease)));
        let job = if revoke {
            sync.connection()
                .execute("DELETE FROM sessions", [])
                .unwrap();
            assert!(matches!(sync.next(&mut lease), NextSyncTarget::Halted));
            let job = sync.job(&job_id);
            let refusal = job.authority_refusal.as_ref().unwrap();
            assert_eq!(
                (refusal.reason, refusal.phase, refusal.generation),
                (
                    AuthorityRefusalReason::CredentialInvalid,
                    AuthorityRefusalPhase::SourceWriter,
                    job.generation - 1
                )
            );
            assert_eq!(lease.version(), job.state_version);
            job
        } else {
            let job = sync.user(UserOperation::Cancel {
                job_id: job_id.clone(),
            });
            assert!(matches!(sync.next(&mut lease), NextSyncTarget::Halted));
            job
        };
        let (code, recovery) = if revoke {
            (
                "authority_refused",
                "Original credential is no longer valid",
            )
        } else {
            ("cancelled", "Sync was cancelled")
        };
        assert_eq!(job.recovery.as_deref(), Some(recovery));
        assert!(job.worker.is_none() && job.fence.is_none());
        let not_run = |id| (id, "not_run".into(), Some("batch_halt".into()));
        if claimed {
            let reason = if revoke { code } else { "local_settlement" };
            assert_eq!(job.state, PersistentState::ReconciliationRequired);
            assert_eq!(
                sync.targets(&job_id),
                [
                    (first, "reconciliation_required".into(), Some(reason.into())),
                    not_run(second)
                ]
            );
            assert_eq!(
                sync.batch(&job_id),
                ("reconciliation_required".into(), None)
            );
        } else {
            let state = if revoke {
                PersistentState::Failed
            } else {
                PersistentState::Cancelled
            };
            assert_eq!(job.state, state);
            assert_eq!(sync.targets(&job_id), [not_run(first), not_run(second)]);
            assert_eq!(sync.batch(&job_id), ("halted".into(), Some(code.into())));
        }
        drop(target);
    }
}

#[test]
fn open_authority_refusal_prunes_terminal_jobs() {
    let sync = Fixture::new();
    let id = sync.source("Pending");
    let job_id = sync.enqueue(Some(vec![id as u64]));
    let ClaimedAttempt { lease, .. } = sync.worker.claim_kind(JobKind::Sync).unwrap().unwrap();
    sync.worker.open_source_sync(&lease).unwrap();
    let mut connection = sync.connection();
    let tx = connection.transaction().unwrap();
    let mut older = jobs::load(&tx, &job_id).unwrap().unwrap();
    older.job_id = "older-terminal".into();
    older.state = PersistentState::Failed;
    older.worker = None;
    older.fence = None;
    older.lease_until = None;
    jobs::save(&tx, &mut older, "test_terminal").unwrap();
    tx.execute(
        "UPDATE durable_jobs SET updated=?1 WHERE id=?2",
        params![now() - 86401, older.job_id],
    )
    .unwrap();
    tx.execute("DELETE FROM sessions", []).unwrap();
    tx.commit().unwrap();
    let error = sync.worker.open_source_sync(&lease).unwrap_err();
    assert_eq!(
        error.downcast_ref::<auth::AuthorityFailure>(),
        Some(&auth::AuthorityFailure::CredentialInvalid)
    );
    assert_sync_refusal(&sync, &job_id, &lease, false, true);
    let tx = connection.transaction().unwrap();
    assert!(jobs::load(&tx, "older-terminal").unwrap().is_none());
    assert_eq!(
        tx.query_row("SELECT count(*) FROM durable_jobs", [], |row| row
            .get::<_, i64>(0))
            .unwrap(),
        1
    );
    drop(tx);
    // Windows cannot remove the data directory while this connection is open.
    drop(connection);
    assert_refusal_survives_reopen(sync, &job_id);
}

#[test]
fn open_and_advance_authority_refusal_halts_without_claimed_targets() {
    for (opened, advance) in [(false, false), (true, false), (true, true)] {
        let sync = Fixture::new();
        let id = sync.source("Pending");
        let job_id = sync.enqueue(Some(vec![id as u64]));
        let ClaimedAttempt { mut lease, .. } =
            sync.worker.claim_kind(JobKind::Sync).unwrap().unwrap();
        if opened {
            sync.worker.open_source_sync(&lease).unwrap();
        }
        sync.connection()
            .execute("DELETE FROM sessions", [])
            .unwrap();
        let error = if advance {
            sync.worker
                .update(&mut lease, WorkerOperation::Heartbeat)
                .unwrap_err()
        } else {
            sync.worker.open_source_sync(&lease).unwrap_err()
        };
        assert_eq!(
            error.downcast_ref::<auth::AuthorityFailure>(),
            Some(&auth::AuthorityFailure::CredentialInvalid)
        );
        let refused = sync.job(&job_id);
        assert_eq!(refused.state, PersistentState::Failed);
        assert_sync_refusal(&sync, &job_id, &lease, false, opened);
        assert_refusal_survives_reopen(sync, &job_id);
    }
}

#[test]
fn open_and_advance_authority_refusals_preserve_claimed_target_evidence() {
    for at_open in [false, true] {
        let mut sync = Fixture::new();
        let first = sync.source("Claimed");
        let second = sync.source("Pending");
        let job_id = sync.enqueue(Some(vec![first as u64, second as u64]));
        let mut lease = sync.open();
        let mut target = Some(local(sync.next(&mut lease)));
        let evidence = sync.connection().query_row(
            "SELECT parent_generation,attempt_worker,attempt_fence_digest,reservation_incarnation,reservation_token,configuration_version,authority_actor,authority_basis_digest,activation_observation_digest,path_observation_digest FROM source_sync_targets WHERE job_id=?1 AND ordinal=0",
            [&job_id],
            |row| (0..10).map(|index| row.get::<_, rusqlite::types::Value>(index)).collect::<rusqlite::Result<Vec<_>>>(),
        ).unwrap();
        if at_open {
            drop(target.take());
            let Fixture {
                owner, root, token, ..
            } = sync;
            owner.shutdown().unwrap();
            let (owner, _) = WriterOwner::open(&root, Limits::default()).unwrap();
            let catalog = owner
                .source_catalog_with_policy(
                    catalog::Credentials::Session(auth::Secret::new(token.clone())),
                    policy(),
                )
                .unwrap();
            let worker = owner.job_worker_with_policy(policy(), admission()).unwrap();
            lease = worker.claim_kind(JobKind::Sync).unwrap().unwrap().lease;
            sync = Fixture {
                owner,
                root,
                token,
                catalog,
                worker,
            };
        }
        sync.connection()
            .execute("DELETE FROM sessions", [])
            .unwrap();
        let error = if at_open {
            sync.worker.open_source_sync(&lease).unwrap_err()
        } else {
            sync.worker
                .update(&mut lease, WorkerOperation::Progress(10))
                .unwrap_err()
        };
        assert_eq!(
            error.downcast_ref::<auth::AuthorityFailure>(),
            Some(&auth::AuthorityFailure::CredentialInvalid)
        );
        assert_sync_refusal(&sync, &job_id, &lease, true, true);
        let retained = sync.connection().query_row(
            "SELECT parent_generation,attempt_worker,attempt_fence_digest,reservation_incarnation,reservation_token,configuration_version,authority_actor,authority_basis_digest,activation_observation_digest,path_observation_digest FROM source_sync_targets WHERE job_id=?1 AND ordinal=0",
            [&job_id],
            |row| (0..10).map(|index| row.get::<_, rusqlite::types::Value>(index)).collect::<rusqlite::Result<Vec<_>>>(),
        ).unwrap();
        assert_eq!(retained, evidence);
        assert_eq!(
            sync.targets(&job_id),
            [
                (
                    first,
                    "reconciliation_required".into(),
                    Some("authority_refused".into())
                ),
                (second, "not_run".into(), Some("batch_halt".into())),
            ]
        );
        if !at_open {
            sync.login();
            sync.catalog = sync
                .owner
                .source_catalog_with_policy(
                    catalog::Credentials::Session(auth::Secret::new(sync.token.clone())),
                    policy(),
                )
                .unwrap();
            let busy = sync.begin_work(first).err().unwrap();
            assert!(busy.downcast_ref::<SourceBusy>().is_some());
        }
        drop(target);
        assert_refusal_survives_reopen(sync, &job_id);
    }
}

#[test]
fn unrecognized_open_and_advance_failures_leave_the_batch_unchanged() {
    let sync = Fixture::new();
    let id = sync.source("Claimed");
    let job_id = sync.enqueue(Some(vec![id as u64]));
    let mut lease = sync.open();
    let target = local(sync.next(&mut lease));
    sync.connection()
        .execute("ALTER TABLE sessions RENAME TO unavailable_sessions", [])
        .unwrap();
    let before = sync.snapshot();
    let open_error = sync.worker.open_source_sync(&lease).unwrap_err();
    assert!(
        open_error.downcast_ref::<rusqlite::Error>().is_some(),
        "{open_error:#}"
    );
    assert_eq!(sync.snapshot(), before);
    let advance_error = sync
        .worker
        .update(&mut lease, WorkerOperation::Heartbeat)
        .unwrap_err();
    assert!(
        advance_error.downcast_ref::<rusqlite::Error>().is_some(),
        "{advance_error:#}"
    );
    assert_eq!(sync.snapshot(), before);
    sync.connection()
        .execute("ALTER TABLE unavailable_sessions RENAME TO sessions", [])
        .unwrap();
    assert_eq!(sync.job(&job_id).state, PersistentState::Running);
    drop(target);
    let Fixture { owner, root, .. } = sync;
    owner.shutdown().unwrap();
    std::fs::remove_dir_all(root).unwrap();
}

fn assert_sync_refusal(
    sync: &Fixture,
    job_id: &str,
    lease: &AttemptLease,
    claimed: bool,
    opened: bool,
) {
    let job = sync.job(job_id);
    let refusal = job.authority_refusal.as_ref().unwrap();
    assert_eq!(
        (refusal.reason, refusal.phase, refusal.generation),
        (
            AuthorityRefusalReason::CredentialInvalid,
            AuthorityRefusalPhase::SourceWriter,
            lease.generation(),
        )
    );
    assert_eq!(job.generation, lease.generation() + 1);
    assert!(job.worker.is_none() && job.fence.is_none() && job.lease_until.is_none());
    assert_eq!(
        job.recovery.as_deref(),
        Some("Original credential is no longer valid")
    );
    assert_eq!(
        job.state,
        if claimed {
            PersistentState::ReconciliationRequired
        } else {
            PersistentState::Failed
        }
    );
    if opened {
        assert_eq!(
            sync.batch(job_id),
            if claimed {
                ("reconciliation_required".into(), None)
            } else {
                ("halted".into(), Some("authority_refused".into()))
            }
        );
        if !claimed {
            assert_eq!(sync.targets(job_id)[0].1, "not_run");
        }
    } else {
        assert_eq!(
            sync.connection()
                .query_row("SELECT count(*) FROM source_sync_batches", [], |row| row
                    .get::<_, i64>(0))
                .unwrap(),
            0
        );
        assert!(sync.targets(job_id).is_empty());
    }
    let before = sync.snapshot();
    assert!(sync.worker.open_source_sync(lease).is_err());
    let mut stale = lease.clone();
    assert!(
        sync.worker
            .update(&mut stale, WorkerOperation::Heartbeat)
            .is_err()
    );
    assert_eq!(sync.snapshot(), before);
}

fn assert_refusal_survives_reopen(sync: Fixture, job_id: &str) {
    let before = sync.snapshot();
    let refused = sync.job(job_id);
    let Fixture { owner, root, .. } = sync;
    owner.shutdown().unwrap();
    let (owner, _) = WriterOwner::open(&root, Limits::default()).unwrap();
    let mut connection = Connection::open(root.join("print-partner.db")).unwrap();
    let tx = connection.transaction().unwrap();
    let reopened = jobs::load(&tx, job_id).unwrap().unwrap();
    assert_eq!(reopened.authority_refusal, refused.authority_refusal);
    assert_eq!(reopened.recovery, refused.recovery);
    assert_eq!(reopened.generation, refused.generation);
    assert_eq!(reopened.state, refused.state);
    drop(tx);
    assert_eq!(database_snapshot(&connection), before);
    drop(connection);
    owner.shutdown().unwrap();
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn cancel_preserves_a_batch_halted_by_worker_advance_refusal() {
    let mut sync = Fixture::new();
    let id = sync.source("Pending");
    let job_id = sync.enqueue(Some(vec![id as u64]));
    let mut lease = sync.open();
    // Retained jobs can contain generic effects admitted before GRE-854.
    let mut legacy = sync.job(&job_id);
    legacy.effects.push(jobs::EffectReceipt {
        intent: jobs::EffectIntent {
            operation: jobs::EffectOperation::SourceRefresh,
            basis_hash: "a".repeat(64),
            content_hash: "b".repeat(64),
            target: id.to_string(),
        },
        attempt: legacy.attempt,
        generation: legacy.generation,
        confirmed: false,
        no_effect: false,
        receipt: None,
        checklist_completion: None,
    });
    sync.connection()
        .execute(
            "UPDATE durable_jobs SET state='effect_admitted',document=json_set(document,'$.state','effect_admitted','$.effects',json(?1)) WHERE id=?2",
            params![serde_json::to_string(&legacy.effects).unwrap(), job_id],
        )
        .unwrap();
    sync.connection()
        .execute("DELETE FROM sessions", [])
        .unwrap();
    assert!(
        sync.worker
            .update(&mut lease, WorkerOperation::Heartbeat)
            .is_err()
    );
    let refused = sync.job(&job_id);
    assert_eq!(refused.state, PersistentState::ReconciliationRequired);
    assert_eq!(
        refused.authority_refusal.as_ref().unwrap().phase,
        AuthorityRefusalPhase::SourceWriter
    );
    assert_eq!(
        sync.batch(&job_id),
        ("reconciliation_required".into(), None)
    );
    sync.login();
    let cancelled = sync.user(UserOperation::Cancel {
        job_id: job_id.clone(),
    });
    assert!(cancelled.cancel_requested);
    assert_eq!(cancelled.state, PersistentState::ReconciliationRequired);
    assert_eq!(cancelled.generation, refused.generation);
    assert_eq!(cancelled.authority_refusal, refused.authority_refusal);
    assert_eq!(
        sync.batch(&job_id),
        ("reconciliation_required".into(), None)
    );
    assert_eq!(
        sync.targets(&job_id),
        [(id, "not_run".into(), Some("batch_halt".into()))]
    );
    let Fixture { owner, root, .. } = sync;
    owner.shutdown().unwrap();
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn repeated_authority_refusal_halt_preserves_evidence() {
    for claimed in [false, true] {
        let sync = Fixture::new();
        let id = sync.source("Refused");
        let job_id = sync.enqueue(Some(vec![id as u64]));
        let mut lease = sync.open();
        let target = claimed.then(|| local(sync.next(&mut lease)));
        sync.connection()
            .execute("DELETE FROM sessions", [])
            .unwrap();
        assert!(matches!(sync.next(&mut lease), NextSyncTarget::Halted));
        let refused = sync.job(&job_id);
        let before = sync.snapshot();
        let mut connection = sync.connection();
        let tx = connection.transaction().unwrap();
        let repeated = halt_job(
            &tx,
            refused.clone(),
            "authority_refused",
            refused.recovery.as_deref().unwrap(),
        )
        .unwrap();
        tx.commit().unwrap();
        assert_eq!(repeated.generation, refused.generation);
        assert_eq!(repeated.authority_refusal, refused.authority_refusal);
        assert_eq!(repeated.recovery, refused.recovery);
        assert_eq!(sync.snapshot(), before);
        drop(connection);
        drop(target);
        let Fixture { owner, root, .. } = sync;
        owner.shutdown().unwrap();
        std::fs::remove_dir_all(root).unwrap();
    }
}

#[test]
fn authority_refusal_survives_cancel_twice_and_reopen() {
    let mut sync = Fixture::new();
    let id = sync.source("Refused");
    let job_id = sync.enqueue(Some(vec![id as u64]));
    let mut lease = sync.open();
    let target = local(sync.next(&mut lease));
    sync.connection()
        .execute("DELETE FROM sessions", [])
        .unwrap();
    assert!(matches!(sync.next(&mut lease), NextSyncTarget::Halted));
    let refused = sync.job(&job_id);
    assert_eq!(refused.state, PersistentState::ReconciliationRequired);
    assert_eq!(
        refused.recovery.as_deref(),
        Some("Original credential is no longer valid")
    );
    assert_eq!(
        sync.targets(&job_id)[0].2.as_deref(),
        Some("authority_refused")
    );
    sync.login();
    let before = sync.snapshot();
    let cancelled = sync.user(UserOperation::Cancel {
        job_id: job_id.clone(),
    });
    assert!(cancelled.cancel_requested);
    assert_eq!(cancelled.recovery, refused.recovery);
    assert_eq!(cancelled.authority_refusal, refused.authority_refusal);
    assert_eq!(cancelled.generation, refused.generation);
    let after = sync.snapshot();
    assert_eq!(after[0], before[0]);
    assert_eq!(after[1], before[1]);
    let repeated = sync.user(UserOperation::Cancel {
        job_id: job_id.clone(),
    });
    assert_eq!(repeated.authority_refusal, refused.authority_refusal);
    assert_eq!(repeated.recovery, refused.recovery);
    assert_eq!(repeated.generation, refused.generation);
    assert_eq!(sync.snapshot(), after);
    drop(target);
    let Fixture { owner, root, .. } = sync;
    owner.shutdown().unwrap();
    let (owner, _) = WriterOwner::open(&root, Limits::default()).unwrap();
    let mut connection = Connection::open(root.join("print-partner.db")).unwrap();
    let tx = connection.transaction().unwrap();
    let reopened = jobs::load(&tx, &job_id).unwrap().unwrap();
    assert_eq!(reopened.authority_refusal, refused.authority_refusal);
    assert_eq!(reopened.recovery, refused.recovery);
    assert_eq!(reopened.generation, refused.generation);
    assert!(reopened.cancel_requested);
    assert_eq!(reopened.state, PersistentState::ReconciliationRequired);
    let failure: (String, String) = tx.query_row(
        "SELECT failure_code,failure_detail FROM source_sync_targets WHERE job_id=?1 AND ordinal=0",
        [&job_id],
        |row| Ok((row.get(0)?, row.get(1)?)),
    ).unwrap();
    assert_eq!(
        failure,
        (
            "authority_refused".into(),
            "Original credential is no longer valid".into()
        )
    );
    drop(tx);
    drop(connection);
    owner.shutdown().unwrap();
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn worker_failure_halts_an_open_batch() {
    let sync = Fixture::new();
    let id = sync.source("Failed");
    let job_id = sync.enqueue(Some(vec![id as u64]));
    let mut lease = sync.open();
    let job = sync
        .worker
        .update(&mut lease, WorkerOperation::Fail)
        .unwrap();
    assert_eq!(job.state, PersistentState::Failed);
    assert_eq!(job.recovery.as_deref(), Some("Source Sync worker failed"));
    assert_eq!(sync.batch(&job_id), ("halted".into(), Some("fatal".into())));
}

#[test]
fn claims_require_an_open_batch_and_the_live_attempt() {
    let sync = Fixture::new();
    let id = sync.source("Unopened");
    sync.enqueue(Some(vec![id as u64]));
    let ClaimedAttempt { mut lease, .. } = sync.worker.claim_kind(JobKind::Sync).unwrap().unwrap();
    let before = sync.snapshot();
    let unopened = sync
        .worker
        .claim_next_source_sync_target(&mut lease)
        .err()
        .unwrap();
    assert_eq!(unopened.to_string(), "Sync batch is not open");
    assert_eq!(sync.snapshot(), before);
    sync.worker.open_source_sync(&lease).unwrap();
    let mut stale = lease.clone();
    sync.worker
        .update(&mut lease, WorkerOperation::Progress(1))
        .unwrap();
    let before = sync.snapshot();
    let error = sync
        .worker
        .claim_next_source_sync_target(&mut stale)
        .err()
        .unwrap();
    assert_eq!(error.to_string(), "Stale attempt fence");
    assert_eq!(sync.snapshot(), before);
    assert_eq!(local(sync.next(&mut lease)).key().ordinal, 0);
}

fn refuse_bind(sync: &Fixture, lease: &AttemptLease, target: &SyncTargetLease) -> String {
    let before = sync.snapshot();
    let error = sync
        .worker
        .bind_source_sync_target(lease, target)
        .err()
        .unwrap();
    assert_eq!(sync.snapshot(), before);
    error.to_string()
}

fn tamper(sync: &Fixture, sql: &str, job_id: &str) {
    assert_eq!(sync.connection().execute(sql, [job_id]).unwrap(), 1);
}

#[test]
fn sync_candidate_requires_claim_and_current_source_proof() {
    let sync = Fixture::new();
    let first = sync.source("First");
    let second = sync.source("Second");
    let other = sync.source("Other");
    let job_id = sync.enqueue(Some(vec![first as u64, second as u64]));
    let mut lease = sync.open();
    let mut target = local(sync.next(&mut lease));
    let bound = BoundSyncTarget {
        key: SyncTargetKey {
            job_id: job_id.clone(),
            ordinal: 0,
        },
        source_id: first,
        claim_generation: 1,
    };
    let before = sync.snapshot();
    assert_eq!(
        sync.worker
            .bind_source_sync_target(&lease, &target)
            .unwrap(),
        bound
    );
    assert_eq!(sync.snapshot(), before);

    target.key.job_id = "other-job".into();
    assert_eq!(
        refuse_bind(&sync, &lease, &target),
        "Sync target Job mismatch"
    );
    target.key.job_id = job_id.clone();
    target.key.ordinal = 1;
    assert_eq!(
        refuse_bind(&sync, &lease, &target),
        "Sync target is not claimed"
    );
    target.key.ordinal = 0;
    target.claim_generation = 2;
    assert_eq!(
        refuse_bind(&sync, &lease, &target),
        "Sync target claim generation mismatch"
    );
    target.claim_generation = 1;
    let claim = sync.snapshot();
    let fence = digest(lease.fence());
    tamper(
        &sync,
        &format!(
            "UPDATE source_sync_targets SET attempt_fence_digest='{}' WHERE job_id=?1 AND ordinal=0",
            "b".repeat(64)
        ),
        &job_id,
    );
    assert_eq!(
        refuse_bind(&sync, &lease, &target),
        "Sync target attempt mismatch"
    );
    tamper(
        &sync,
        &format!(
            "UPDATE source_sync_targets SET attempt_fence_digest='{fence}' WHERE job_id=?1 AND ordinal=0"
        ),
        &job_id,
    );
    assert_eq!(sync.snapshot(), claim);

    let stale = lease.clone();
    sync.worker
        .update(&mut lease, WorkerOperation::Progress(1))
        .unwrap();
    assert_eq!(refuse_bind(&sync, &stale, &target), "Stale attempt fence");

    let claimed = std::mem::replace(&mut target.source_work, sync.begin_work(other).unwrap());
    assert_eq!(
        refuse_bind(&sync, &lease, &target),
        "Sync target reservation mismatch"
    );
    let held = sync.snapshot();
    let set_token = |token: u64| {
        tamper(
            &sync,
            &format!(
                "UPDATE source_sync_targets SET reservation_token='{token:016x}' WHERE job_id=?1 AND ordinal=0"
            ),
            &job_id,
        )
    };
    set_token(target.source_work.proof().unwrap().token);
    assert_eq!(
        refuse_bind(&sync, &lease, &target),
        "Wrong Source reservation"
    );
    std::mem::replace(&mut target.source_work, claimed)
        .release()
        .unwrap();
    set_token(target.source_work.proof().unwrap().token);
    assert_eq!(sync.snapshot(), held);
    assert_eq!(
        sync.worker
            .bind_source_sync_target(&lease, &target)
            .unwrap(),
        bound
    );
    target.source_work.release().unwrap();
    assert_eq!(
        refuse_bind(&sync, &lease, &target),
        "Lease already released"
    );
}

#[test]
fn sync_candidate_rejects_changed_basis() {
    let sync = Fixture::new();
    let first = sync.source("First");
    let second = sync.source("Second");
    let other = sync.source("Other");
    let job_id = sync.enqueue(Some(vec![first as u64, second as u64]));
    let mut lease = sync.open();
    let mut target = local(sync.next(&mut lease));
    let sealed = sync.snapshot();
    let digest: String = sync
        .connection()
        .query_row(
            "SELECT target_basis_digest FROM source_sync_batches WHERE job_id=?1",
            [&job_id],
            |row| row.get(0),
        )
        .unwrap();
    let batch = "UPDATE source_sync_batches SET";
    let second_target = "WHERE job_id=?1 AND ordinal=1";
    for (change, restore, expected) in [
        (
            format!(
                "{batch} target_basis_digest='{}' WHERE job_id=?1",
                "a".repeat(64)
            ),
            format!("{batch} target_basis_digest='{digest}' WHERE job_id=?1"),
            "Sync target basis mismatch",
        ),
        (
            format!("UPDATE source_sync_targets SET requested_source_id={other} {second_target}"),
            format!("UPDATE source_sync_targets SET requested_source_id={second} {second_target}"),
            "Sync selected Sources mismatch",
        ),
        (
            format!("{batch} target_count=target_count+1 WHERE job_id=?1"),
            format!("{batch} target_count=target_count-1 WHERE job_id=?1"),
            "Sync target count mismatch",
        ),
    ] {
        tamper(&sync, &change, &job_id);
        assert_eq!(refuse_bind(&sync, &lease, &target), expected);
        tamper(&sync, &restore, &job_id);
        assert_eq!(sync.snapshot(), sealed);
    }

    tamper(
        &sync,
        "UPDATE source_sync_batches SET phase='reconciliation_required' WHERE job_id=?1",
        &job_id,
    );
    assert_eq!(
        refuse_bind(&sync, &lease, &target),
        "Sync batch is not open"
    );
    tamper(
        &sync,
        "UPDATE source_sync_batches SET phase='open' WHERE job_id=?1",
        &job_id,
    );
    assert_eq!(sync.snapshot(), sealed);

    target.key.ordinal = 2;
    assert_eq!(
        refuse_bind(&sync, &lease, &target),
        "Sync target is outside the sealed basis"
    );
    target.key.ordinal = 0;
    let busy = sync.begin_work(first).err().unwrap();
    assert!(busy.downcast_ref::<SourceBusy>().is_some(), "{busy:#}");
    assert_eq!(
        sync.worker
            .bind_source_sync_target(&lease, &target)
            .unwrap()
            .source_id,
        first
    );
    assert_eq!(sync.snapshot(), sealed);
}

fn source_row(sync: &Fixture, id: i64) -> (i64, Option<String>, Option<i64>) {
    sync.connection()
        .query_row(
            "SELECT source_configuration_version,local_path,current_source_revision_id FROM projects WHERE id=?1",
            [id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .unwrap()
}

fn refuse_unchanged(
    sync: &Fixture,
    lease: &AttemptLease,
    target: &SyncTargetLease,
    source_id: i64,
) -> String {
    let rows = sync.snapshot();
    let source = source_row(sync, source_id);
    let error = refuse_bind(sync, lease, target);
    assert_eq!(sync.snapshot(), rows);
    assert_eq!(source_row(sync, source_id), source);
    error
}

#[test]
fn sync_candidate_content_requires_matching_revision() {
    let sync = Fixture::new();
    let first = sync.source("First");
    let other = sync.source("Other");
    let job_id = sync.enqueue(Some(vec![first as u64]));
    let mut lease = sync.open();
    let target = local(sync.next(&mut lease));
    let bound = BoundSyncTarget {
        key: SyncTargetKey {
            job_id: job_id.clone(),
            ordinal: 0,
        },
        source_id: first,
        claim_generation: 1,
    };
    let sealed = sync.snapshot();
    assert_eq!(source_row(&sync, first).2, None);
    assert_eq!(
        sync.worker
            .bind_source_sync_target(&lease, &target)
            .unwrap(),
        bound
    );
    assert_eq!(sync.snapshot(), sealed);

    assert_eq!(
        sync.connection()
            .execute(
                "INSERT INTO source_revisions(tenant_id,project_id,upstream_revision_key,manifest_digest,snapshot_locator,synced_at,completeness) SELECT tenant_id,id,'candidate-revision','digest','snap','2026-10-21T00:00:00Z','complete' FROM projects WHERE id=?1",
                [first],
            )
            .unwrap(),
        1
    );
    tamper(
        &sync,
        "UPDATE projects SET current_source_revision_id=(SELECT id FROM source_revisions WHERE project_id=?1 AND upstream_revision_key='candidate-revision') WHERE id=?1",
        &first.to_string(),
    );
    assert_eq!(
        refuse_unchanged(&sync, &lease, &target, first),
        "Sync revision identity mismatch"
    );
    tamper(
        &sync,
        "UPDATE projects SET current_source_revision_id=NULL WHERE id=?1",
        &first.to_string(),
    );
    assert_eq!(sync.snapshot(), sealed);
    assert_eq!(
        sync.worker
            .bind_source_sync_target(&lease, &target)
            .unwrap(),
        bound
    );

    let result = jobs::SourceScanResult {
        project_id: Some(other as u64),
        stl_count: 0,
        downloaded: 0,
        doc_count: 0,
        docs_downloaded: 0,
        pdf_extract_job_id: None,
        postprocess_warning: None,
    };
    let result_json = serde_json::to_string(&result).unwrap();
    let result_digest = digest(result_json.as_bytes());
    let receipt_id = format!("local-scan-{}", "a".repeat(32));
    let receipt_hash = "b".repeat(64);
    assert_eq!(
        sync.connection()
            .execute(
                "UPDATE source_sync_targets SET result_json=?2 WHERE job_id=?1 AND ordinal=0",
                (&job_id, &result_json),
            )
            .unwrap(),
        1
    );
    assert_eq!(
        refuse_unchanged(&sync, &lease, &target, first),
        "Sync result capture incomplete"
    );
    assert_eq!(
        sync.connection()
            .execute(
                "UPDATE source_sync_targets SET result_digest=?2,receipt_id=?3,receipt_hash=?4,receipt_target=?5 WHERE job_id=?1 AND ordinal=0",
                (
                    &job_id,
                    &result_digest,
                    &receipt_id,
                    &receipt_hash,
                    format!("source:{other}"),
                ),
            )
            .unwrap(),
        1
    );
    assert_eq!(
        refuse_unchanged(&sync, &lease, &target, first),
        "Sync result Source mismatch"
    );
    tamper(
        &sync,
        "UPDATE source_sync_targets SET result_json=NULL,result_digest=NULL,receipt_id=NULL,receipt_hash=NULL,receipt_target=NULL WHERE job_id=?1 AND ordinal=0",
        &job_id,
    );
    assert_eq!(sync.snapshot(), sealed);
    assert_eq!(
        sync.worker
            .bind_source_sync_target(&lease, &target)
            .unwrap(),
        bound
    );
}

#[test]
fn sync_candidate_rejects_changed_configuration_or_observation() {
    let sync = Fixture::new();
    let first = sync.source("First");
    let job_id = sync.enqueue(Some(vec![first as u64]));
    let mut lease = sync.open();
    let target = local(sync.next(&mut lease));
    let sealed = sync.snapshot();
    let (version, path, _) = source_row(&sync, first);
    let bound = BoundSyncTarget {
        key: SyncTargetKey {
            job_id: job_id.clone(),
            ordinal: 0,
        },
        source_id: first,
        claim_generation: 1,
    };
    let set_version = |next: i64| {
        assert_eq!(
            sync.connection()
                .execute(
                    "UPDATE projects SET source_configuration_version=?2 WHERE id=?1",
                    (first, next),
                )
                .unwrap(),
            1
        );
    };
    let set_path = |next: Option<&str>| {
        assert_eq!(
            sync.connection()
                .execute(
                    "UPDATE projects SET local_path=?2 WHERE id=?1",
                    (first, next)
                )
                .unwrap(),
            1
        );
    };
    set_version(version + 1);
    assert_eq!(
        refuse_unchanged(&sync, &lease, &target, first),
        "Sync Source configuration mismatch"
    );
    set_version(version);
    assert_eq!(sync.snapshot(), sealed);
    set_path(Some("moved"));
    assert_eq!(
        refuse_unchanged(&sync, &lease, &target, first),
        "Sync Source observation mismatch"
    );
    set_path(path.as_deref());
    assert_eq!(sync.snapshot(), sealed);
    assert_eq!(
        sync.worker
            .bind_source_sync_target(&lease, &target)
            .unwrap(),
        bound
    );
    assert_eq!(sync.snapshot(), sealed);
}

fn settlement(source_id: i64) -> SyncSettlement {
    SyncSettlement {
        result: jobs::SourceScanResult {
            project_id: Some(u64::try_from(source_id).unwrap()),
            stl_count: 2,
            downloaded: 1,
            doc_count: 3,
            docs_downloaded: 0,
            pdf_extract_job_id: None,
            postprocess_warning: None,
        },
        receipt: jobs::ResultArtifact {
            receipt_id: format!(
                "local-scan-{:032x}",
                u64::try_from(source_id).expect("Source ID is positive")
            ),
            content_hash: "cd".repeat(32),
            target: format!("source:{source_id}"),
        },
    }
}

fn stored_success(
    sync: &Fixture,
    job_id: &str,
) -> (
    String,
    i64,
    String,
    String,
    String,
    String,
    String,
    i64,
    String,
) {
    sync.connection()
        .query_row(
            "SELECT state,claim_generation,result_json,result_digest,receipt_id,receipt_hash,receipt_target,reservation_acknowledged,reservation_token FROM source_sync_targets WHERE job_id=?1 AND ordinal=0",
            [job_id],
            |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                    row.get(5)?,
                    row.get(6)?,
                    row.get(7)?,
                    row.get(8)?,
                ))
            },
        )
        .unwrap()
}

fn refuse_settle(
    sync: &Fixture,
    lease: &AttemptLease,
    target: &mut SyncTargetLease,
    settlement: SyncSettlement,
    source_id: i64,
) -> String {
    let rows = sync.snapshot();
    let source = source_row(sync, source_id);
    let error = sync
        .worker
        .settle_source_sync_target(lease, target, settlement)
        .unwrap_err();
    assert_eq!(sync.snapshot(), rows);
    assert_eq!(source_row(sync, source_id), source);
    let busy = sync.begin_work(source_id).err().unwrap();
    assert!(busy.downcast_ref::<SourceBusy>().is_some(), "{busy:#}");
    error.to_string()
}

#[test]
fn sync_settle_persists_one_result() {
    let sync = Fixture::new();
    let first = sync.source("First");
    let second = sync.source("Second");
    let other = sync.source("Other");
    let job_id = sync.enqueue(Some(vec![first as u64, second as u64]));
    let mut lease = sync.open();
    let mut target = local(sync.next(&mut lease));
    let token = target.source_work.proof().unwrap().token;
    let mut other_lease = sync.begin_work(other).unwrap();
    let before = sync.snapshot();
    let source = source_row(&sync, first);
    let busy = sync.begin_work(first).err().unwrap();
    assert!(busy.downcast_ref::<SourceBusy>().is_some(), "{busy:#}");
    let settlement = settlement(first);
    let settled = sync
        .worker
        .settle_source_sync_target(&lease, &mut target, settlement.clone())
        .unwrap();
    let result_json = serde_json::to_string(&settlement.result).unwrap();
    assert_eq!(
        settled,
        SettledSyncTarget {
            key: SyncTargetKey {
                job_id: job_id.clone(),
                ordinal: 0,
            },
            source_id: first,
            result: settlement.result.clone(),
            receipt: settlement.receipt.clone(),
        }
    );
    assert_eq!(
        stored_success(&sync, &job_id),
        (
            "succeeded".into(),
            1,
            result_json.clone(),
            digest(result_json.as_bytes()),
            settlement.receipt.receipt_id,
            settlement.receipt.content_hash,
            settlement.receipt.target,
            1,
            format!("{token:016x}"),
        )
    );
    let after = sync.snapshot();
    assert_eq!(after[0], before[0]);
    assert_eq!(after[1][1], before[1][1]);
    assert_eq!(after[2], before[2]);
    assert_eq!(after[3], before[3]);
    assert_eq!(source_row(&sync, first), source);
    assert_eq!(sync.batch(&job_id), ("open".into(), None));
    assert_eq!(
        sync.targets(&job_id),
        vec![
            (first, "succeeded".into(), None),
            (second, "pending".into(), None),
        ]
    );
    let still_held = sync.begin_work(other).err().unwrap();
    assert!(
        still_held.downcast_ref::<SourceBusy>().is_some(),
        "{still_held:#}"
    );
    other_lease.release().unwrap();
    sync.begin_work(first).unwrap().release().unwrap();
    assert_eq!(local(sync.next(&mut lease)).key().ordinal, 1);
}

#[test]
fn sync_stale_settlement_preserves_target() {
    let sync = Fixture::new();
    let first = sync.source("First");
    let other = sync.source("Other");
    let job_id = sync.enqueue(Some(vec![first as u64]));
    let mut lease = sync.open();
    let mut target = local(sync.next(&mut lease));
    let settlement = settlement(first);
    let stale = lease.clone();
    sync.worker
        .update(&mut lease, WorkerOperation::Progress(1))
        .unwrap();
    let before = sync.snapshot();
    assert_eq!(
        refuse_settle(&sync, &stale, &mut target, settlement.clone(), first),
        "Stale attempt fence"
    );
    let held = std::mem::replace(&mut target.source_work, sync.begin_work(other).unwrap());
    assert_eq!(
        refuse_settle(&sync, &lease, &mut target, settlement.clone(), first),
        "Sync target reservation mismatch"
    );
    std::mem::replace(&mut target.source_work, held)
        .release()
        .unwrap();
    let mut unnamed = settlement.clone();
    unnamed.result.project_id = None;
    assert_eq!(
        refuse_settle(&sync, &lease, &mut target, unnamed, first),
        "Sync result Source mismatch"
    );
    assert_eq!(sync.snapshot(), before);
    assert_eq!(sync.targets(&job_id), vec![(first, "claimed".into(), None)]);
    assert_eq!(
        sync.worker
            .bind_source_sync_target(&lease, &target)
            .unwrap()
            .source_id,
        first
    );
    assert_eq!(sync.snapshot(), before);
}

fn metadata(sync: &Fixture, id: i64) -> Option<String> {
    sync.connection()
        .query_row(
            "SELECT metadata_json FROM projects WHERE id=?1",
            [id],
            |row| row.get(0),
        )
        .unwrap()
}

fn refuse_fail(
    sync: &Fixture,
    lease: &mut AttemptLease,
    target: &SyncTargetLease,
    source_id: i64,
    detail: &str,
) -> String {
    let rows = sync.snapshot();
    let source = metadata(sync, source_id);
    let error = sync
        .worker
        .fail_source_sync_target(lease, target, detail)
        .unwrap_err();
    assert_eq!(sync.snapshot(), rows);
    assert_eq!(metadata(sync, source_id), source);
    let busy = sync.begin_work(source_id).err().unwrap();
    assert!(busy.downcast_ref::<SourceBusy>().is_some(), "{busy:#}");
    error.to_string()
}

fn retained_failure(sync: &Fixture, lease: &mut AttemptLease) -> SourceSyncFailure {
    let NextSyncTarget::RetainedFailure(failure) = sync.next(lease) else {
        panic!("retained failure expected")
    };
    failure
}

#[test]
fn sync_target_failure_records_source_observation() {
    let sync = Fixture::new();
    let first = sync.source("First");
    let second = sync.source("Second");
    assert_eq!(
        sync.connection()
            .execute(
                "UPDATE projects SET metadata_json=?1 WHERE id=?2",
                (r#"{"keep":7}"#, first),
            )
            .unwrap(),
        1
    );
    let job_id = sync.enqueue(Some(vec![first as u64, second as u64]));
    let mut lease = sync.open();
    let target = local(sync.next(&mut lease));
    let version = lease.version();
    let capture: (String, String, String) = sync.connection().query_row(
        "SELECT activation_observation_digest,path_observation_digest,reservation_token FROM source_sync_targets WHERE job_id=?1 AND ordinal=0",
        [&job_id],
        |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
    ).unwrap();
    assert_eq!(
        refuse_fail(&sync, &mut lease, &target, first, ""),
        "Invalid Local Sync failure"
    );
    assert_eq!(
        refuse_fail(&sync, &mut lease, &target, first, &"x".repeat(1025)),
        "Invalid Local Sync failure"
    );
    let source = source_row(&sync, first);
    assert_eq!(
        sync.connection()
            .execute(
                "UPDATE projects SET local_path='moved' WHERE id=?1",
                [first]
            )
            .unwrap(),
        1
    );
    assert_eq!(
        refuse_fail(&sync, &mut lease, &target, first, "disk read failed"),
        "Sync Source observation mismatch"
    );
    assert_eq!(
        sync.connection()
            .execute(
                "UPDATE projects SET local_path=?2 WHERE id=?1",
                (first, source.1.clone()),
            )
            .unwrap(),
        1
    );
    assert_eq!(source_row(&sync, first), source);
    assert_eq!(metadata(&sync, first).as_deref(), Some(r#"{"keep":7}"#));
    assert_eq!(lease.version(), version);
    assert!(sync.job(&job_id).progress.is_none());
    let failed = sync
        .worker
        .fail_source_sync_target(&mut lease, &target, "disk read failed")
        .unwrap();
    assert_eq!(
        failed,
        FailedSyncTarget {
            key: SyncTargetKey {
                job_id: job_id.clone(),
                ordinal: 0
            },
            failure: SourceSyncFailure {
                project_id: Some(first as u64),
                name: Some("First".into()),
                error: "disk read failed".into(),
            },
            progress: 0,
            version: version + 1,
        }
    );
    assert_eq!(
        metadata(&sync, first).as_deref(),
        Some(r#"{"keep":7,"sync_error":"disk read failed","sync_required":true}"#)
    );
    let still: (String, String, String) = sync.connection().query_row(
        "SELECT activation_observation_digest,path_observation_digest,reservation_token FROM source_sync_targets WHERE job_id=?1 AND ordinal=0",
        [&job_id],
        |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
    ).unwrap();
    assert_eq!(still, capture);
    assert_eq!(
        sync.targets(&job_id),
        vec![
            (first, "failed".into(), Some("local_read".into())),
            (second, "pending".into(), None),
        ]
    );
    assert_eq!(sync.batch(&job_id), ("open".into(), None));
    assert_eq!(retained_failure(&sync, &mut lease), failed.failure);
    assert_eq!(lease.version(), version + 1);
}

#[test]
fn sync_failure_does_not_replace_settled_result() {
    let sync = Fixture::new();
    let first = sync.source("First");
    let second = sync.source("Second");
    let job_id = sync.enqueue(Some(vec![first as u64, second as u64]));
    let mut lease = sync.open();
    let mut first_target = local(sync.next(&mut lease));
    sync.worker
        .settle_source_sync_target(&lease, &mut first_target, settlement(first))
        .unwrap();
    let stored = stored_success(&sync, &job_id);
    let first_metadata = metadata(&sync, first);
    assert!(sync.job(&job_id).progress.is_none());
    let second_target = local(sync.next(&mut lease));
    let mut planted = settlement(second);
    planted.receipt.receipt_id = format!("local-scan-{}", "ef".repeat(16));
    let result_json = serde_json::to_string(&planted.result).unwrap();
    let result_digest = digest(result_json.as_bytes());
    assert_eq!(
        sync.connection().execute(
            "UPDATE source_sync_targets SET result_json=?2,result_digest=?3,receipt_id=?4,receipt_hash=?5,receipt_target=?6 WHERE job_id=?1 AND ordinal=1",
            (&job_id, &result_json, &result_digest, &planted.receipt.receipt_id, &planted.receipt.content_hash, &planted.receipt.target),
        ).unwrap(),
        1
    );
    assert_eq!(
        refuse_fail(
            &sync,
            &mut lease,
            &second_target,
            second,
            "disk read failed"
        ),
        "Sync target already stores a result"
    );
    assert_eq!(stored_success(&sync, &job_id), stored);
    assert_eq!(metadata(&sync, second), None);
    let planted_row: (String, String, String) = sync.connection().query_row(
        "SELECT result_json,receipt_target,state FROM source_sync_targets WHERE job_id=?1 AND ordinal=1",
        [&job_id],
        |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
    ).unwrap();
    assert_eq!(
        planted_row,
        (
            result_json,
            planted.receipt.target.clone(),
            "claimed".into()
        )
    );
    assert_eq!(
        sync.connection().execute(
            "UPDATE source_sync_targets SET result_json=NULL,result_digest=NULL,receipt_id=NULL,receipt_hash=NULL,receipt_target=NULL WHERE job_id=?1 AND ordinal=1",
            [&job_id],
        ).unwrap(),
        1
    );
    let failed = sync
        .worker
        .fail_source_sync_target(&mut lease, &second_target, "disk read failed")
        .unwrap();
    assert_eq!(stored_success(&sync, &job_id), stored);
    assert_eq!(metadata(&sync, first), first_metadata);
    assert_eq!(
        metadata(&sync, second).as_deref(),
        Some(r#"{"sync_error":"disk read failed","sync_required":true}"#)
    );
    assert_eq!(
        (failed.progress, sync.job(&job_id).progress),
        (50, Some(50))
    );
    assert_eq!(failed.failure.project_id, Some(second as u64));
    assert_eq!(
        sync.targets(&job_id),
        vec![
            (first, "succeeded".into(), None),
            (second, "failed".into(), Some("local_read".into())),
        ]
    );
    let acknowledged: (i64, Option<String>) = sync.connection().query_row(
        "SELECT reservation_acknowledged,result_json FROM source_sync_targets WHERE job_id=?1 AND ordinal=1",
        [&job_id],
        |row| Ok((row.get(0)?, row.get(1)?)),
    ).unwrap();
    assert_eq!(acknowledged, (0, None));
    assert_eq!(retained_failure(&sync, &mut lease), failed.failure);
    sync.begin_work(first).unwrap().release().unwrap();
    let busy = sync.begin_work(second).err().unwrap();
    assert!(busy.downcast_ref::<SourceBusy>().is_some(), "{busy:#}");
}

fn refuse_acknowledge(
    sync: &Fixture,
    lease: &mut AttemptLease,
    target: &SyncTargetLease,
    observation: &LocalScanObservation,
    source_id: i64,
) -> String {
    let rows = sync.snapshot();
    let version = lease.version();
    let error = sync
        .worker
        .acknowledge_source_sync_target(lease, target, observation)
        .unwrap_err();
    assert_eq!(lease.version(), version);
    assert_eq!(sync.snapshot(), rows);
    let busy = sync.begin_work(source_id).err().unwrap();
    assert!(busy.downcast_ref::<SourceBusy>().is_some(), "{busy:#}");
    error.to_string()
}

fn retained_row(
    sync: &Fixture,
    job_id: &str,
    ordinal: i64,
) -> (String, String, i64, Option<String>) {
    sync.connection()
        .query_row(
            "SELECT failure_code,failure_detail,reservation_acknowledged,result_json FROM source_sync_targets WHERE job_id=?1 AND ordinal=?2",
            (job_id, ordinal),
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )
        .unwrap()
}

#[test]
fn sync_acknowledgement_requires_matching_observation() {
    let sync = Fixture::new();
    let id = sync.source("Retained");
    let other = sync.source("Other");
    let job_id = sync.enqueue(Some(vec![id as u64]));
    let mut lease = sync.open();
    let mut target = local(sync.next(&mut lease));
    let failed = sync
        .worker
        .fail_source_sync_target(&mut lease, &target, "disk read failed")
        .unwrap();
    let observation = target.observation().clone();
    let hold = retained_row(&sync, &job_id, 0);
    assert_eq!(
        hold,
        ("local_read".into(), "disk read failed".into(), 0, None)
    );
    let mut wrong_path = observation.clone();
    wrong_path.path_observation_digest = "ab".repeat(32);
    assert_eq!(
        refuse_acknowledge(&sync, &mut lease, &target, &wrong_path, id),
        "Sync acknowledgement observation mismatch"
    );
    let mut wrong_version = observation.clone();
    wrong_version.configuration_version += 1;
    assert_eq!(
        refuse_acknowledge(&sync, &mut lease, &target, &wrong_version, id),
        "Sync acknowledgement observation mismatch"
    );
    let generation = target.claim_generation;
    target.claim_generation = generation + 1;
    assert_eq!(
        refuse_acknowledge(&sync, &mut lease, &target, &observation, id),
        "Sync target claim generation mismatch"
    );
    target.claim_generation = generation;
    let held = std::mem::replace(&mut target.source_work, sync.begin_work(other).unwrap());
    assert_eq!(
        refuse_acknowledge(&sync, &mut lease, &target, &observation, id),
        "Sync target acknowledgement mismatch"
    );
    std::mem::replace(&mut target.source_work, held)
        .release()
        .unwrap();
    let mut stale = lease.clone();
    sync.worker
        .update(&mut lease, WorkerOperation::Progress(1))
        .unwrap();
    assert_eq!(
        refuse_acknowledge(&sync, &mut stale, &target, &observation, id),
        "Stale attempt fence"
    );
    assert_eq!(
        sync.connection()
            .execute("UPDATE projects SET local_path='moved' WHERE id=?1", [id])
            .unwrap(),
        1
    );
    assert_eq!(
        refuse_acknowledge(&sync, &mut lease, &target, &wrong_path, id),
        "Sync acknowledgement observation mismatch"
    );
    assert_eq!(retained_row(&sync, &job_id, 0), hold);
    assert_eq!(retained_failure(&sync, &mut lease), failed.failure);
    assert_eq!(
        metadata(&sync, id).as_deref(),
        Some(r#"{"sync_error":"disk read failed","sync_required":true}"#)
    );
    let version_before = lease.version();
    let acknowledged = sync
        .worker
        .acknowledge_source_sync_target(&mut lease, &target, &observation)
        .unwrap();
    assert_eq!(
        acknowledged,
        AcknowledgedSyncTarget {
            key: SyncTargetKey {
                job_id: job_id.clone(),
                ordinal: 0,
            },
            failure: failed.failure,
            progress: 100,
            version: version_before + 1,
        }
    );
    assert_eq!(
        retained_row(&sync, &job_id, 0),
        ("local_read".into(), "disk read failed".into(), 1, None)
    );
    assert_eq!(sync.job(&job_id).progress, Some(100));
    assert_eq!(sync.batch(&job_id), ("open".into(), None));
    assert_eq!(source_row(&sync, id).1.as_deref(), Some("moved"));
    assert!(matches!(sync.next(&mut lease), NextSyncTarget::Complete));
    sync.begin_work(id).unwrap().release().unwrap();
}

#[test]
fn sync_acknowledgement_replay_preserves_result() {
    let sync = Fixture::new();
    let first = sync.source("First");
    let second = sync.source("Second");
    let job_id = sync.enqueue(Some(vec![first as u64, second as u64]));
    let mut lease = sync.open();
    let mut first_target = local(sync.next(&mut lease));
    sync.worker
        .settle_source_sync_target(&lease, &mut first_target, settlement(first))
        .unwrap();
    let stored = stored_success(&sync, &job_id);
    let second_target = local(sync.next(&mut lease));
    let failed = sync
        .worker
        .fail_source_sync_target(&mut lease, &second_target, "disk read failed")
        .unwrap();
    assert_eq!(failed.progress, 50);
    assert_eq!(stored_success(&sync, &job_id), stored);
    assert_eq!(retained_failure(&sync, &mut lease), failed.failure);
    let observation = second_target.observation().clone();
    let acknowledged = sync
        .worker
        .acknowledge_source_sync_target(&mut lease, &second_target, &observation)
        .unwrap();
    assert_eq!(acknowledged.failure, failed.failure);
    assert_eq!(acknowledged.progress, 100);
    assert_eq!(acknowledged.key.ordinal, 1);
    assert_eq!(stored_success(&sync, &job_id), stored);
    assert_eq!(
        retained_row(&sync, &job_id, 1),
        ("local_read".into(), "disk read failed".into(), 1, None)
    );
    let after = sync.snapshot();
    let mut wrong = observation.clone();
    wrong.path_observation_digest = "cd".repeat(32);
    let version = lease.version();
    let error = sync
        .worker
        .acknowledge_source_sync_target(&mut lease, &second_target, &wrong)
        .unwrap_err();
    assert_eq!(
        error.to_string(),
        "Sync acknowledgement observation mismatch"
    );
    assert_eq!(lease.version(), version);
    assert_eq!(sync.snapshot(), after);
    let again = sync
        .worker
        .acknowledge_source_sync_target(&mut lease, &second_target, &observation)
        .unwrap();
    assert_eq!(again, acknowledged);
    assert_eq!(lease.version(), version);
    assert_eq!(sync.snapshot(), after);
    assert_eq!(stored_success(&sync, &job_id), stored);
    assert_eq!(sync.job(&job_id).progress, Some(100));
    assert!(matches!(sync.next(&mut lease), NextSyncTarget::Complete));
    sync.begin_work(first).unwrap().release().unwrap();
    sync.begin_work(second).unwrap().release().unwrap();
}

fn refuse_adopt(
    sync: &Fixture,
    lease: &AttemptLease,
    key: SyncTargetKey,
    source_id: i64,
) -> String {
    let rows = sync.snapshot();
    let source = source_row(sync, source_id);
    let error = sync
        .worker
        .adopt_source_sync_target(lease, key)
        .unwrap_err();
    assert_eq!(sync.snapshot(), rows);
    assert_eq!(source_row(sync, source_id), source);
    error.to_string()
}

#[test]
fn sync_adopts_matching_retained_result() {
    let sync = Fixture::new();
    let first = sync.source("First");
    let second = sync.source("Second");
    let job_id = sync.enqueue(Some(vec![first as u64, second as u64]));
    let mut lease = sync.open();
    let mut target = local(sync.next(&mut lease));
    let key = target.key().clone();
    let settlement = settlement(first);
    let settled = sync
        .worker
        .settle_source_sync_target(&lease, &mut target, settlement.clone())
        .unwrap();
    let stored = stored_success(&sync, &job_id);
    let after = sync.snapshot();
    let adopted = sync
        .worker
        .adopt_source_sync_target(&lease, key.clone())
        .unwrap();
    assert_eq!(adopted, settled);
    assert_eq!(
        adopted,
        SettledSyncTarget {
            key: SyncTargetKey {
                job_id: job_id.clone(),
                ordinal: 0,
            },
            source_id: first,
            result: settlement.result.clone(),
            receipt: settlement.receipt.clone(),
        }
    );
    assert_eq!(sync.snapshot(), after);
    assert_eq!(stored_success(&sync, &job_id), stored);
    let again = sync.worker.adopt_source_sync_target(&lease, key).unwrap();
    assert_eq!(again, settled);
    assert_eq!(sync.snapshot(), after);
    assert_eq!(local(sync.next(&mut lease)).key().ordinal, 1);
    sync.begin_work(first).unwrap().release().unwrap();
}

#[test]
fn sync_adopt_rejects_changed_provenance() {
    let sync = Fixture::new();
    let id = sync.source("Adopted");
    let job_id = sync.enqueue(Some(vec![id as u64]));
    let mut lease = sync.open();
    let mut target = local(sync.next(&mut lease));
    let key = target.key().clone();
    let settled = sync
        .worker
        .settle_source_sync_target(&lease, &mut target, settlement(id))
        .unwrap();
    let stored = stored_success(&sync, &job_id);
    let original = source_row(&sync, id);
    assert_eq!(
        sync.connection()
            .execute("UPDATE projects SET local_path='moved' WHERE id=?1", [id])
            .unwrap(),
        1
    );
    let after_move = sync.snapshot();
    assert_eq!(
        refuse_adopt(&sync, &lease, key.clone(), id),
        "Sync adoption provenance mismatch"
    );
    assert_eq!(sync.snapshot(), after_move);
    assert_eq!(stored_success(&sync, &job_id), stored);
    assert_eq!(
        sync.connection()
            .execute(
                "UPDATE projects SET local_path=?1 WHERE id=?2",
                (original.1.as_deref(), id),
            )
            .unwrap(),
        1
    );
    assert_eq!(source_row(&sync, id), original);
    let stale = lease.clone();
    sync.worker
        .update(&mut lease, WorkerOperation::Progress(1))
        .unwrap();
    assert_eq!(
        refuse_adopt(&sync, &stale, key.clone(), id),
        "Stale attempt fence"
    );
    assert_eq!(
        sync.worker.adopt_source_sync_target(&lease, key).unwrap(),
        settled
    );
    assert_eq!(stored_success(&sync, &job_id), stored);
    sync.begin_work(id).unwrap().release().unwrap();
}

#[test]
fn sync_adopt_rejects_reclaimed_attempt() {
    let sync = Fixture::new();
    let id = sync.source("Reclaimed");
    let job_id = sync.enqueue(Some(vec![id as u64]));
    let mut lease = sync.open();
    let mut target = local(sync.next(&mut lease));
    let key = target.key().clone();
    let settled = sync
        .worker
        .settle_source_sync_target(&lease, &mut target, settlement(id))
        .unwrap();
    let stored = stored_success(&sync, &job_id);
    assert_eq!(
        sync.worker
            .adopt_source_sync_target(&lease, key.clone())
            .unwrap(),
        settled
    );
    change_job(&sync, &job_id, |job| job.lease_until = Some(now() - 1));
    let ClaimedAttempt {
        lease: reclaimed, ..
    } = sync.worker.claim_kind(JobKind::Sync).unwrap().unwrap();
    assert_eq!(reclaimed.job_id(), job_id);
    assert_ne!(reclaimed.generation(), lease.generation());
    let before = sync.snapshot();
    assert_eq!(
        refuse_adopt(&sync, &reclaimed, key, id),
        "Sync target attempt mismatch"
    );
    assert_eq!(sync.snapshot(), before);
    assert_eq!(sync.snapshot(), before);
    assert_eq!(stored_success(&sync, &job_id), stored);
    sync.begin_work(id).unwrap().release().unwrap();
}

#[test]
fn sync_adopt_rejects_failed_target() {
    let sync = Fixture::new();
    let id = sync.source("Failed");
    sync.enqueue(Some(vec![id as u64]));
    let mut lease = sync.open();
    let target = local(sync.next(&mut lease));
    let key = target.key().clone();
    sync.worker
        .fail_source_sync_target(&mut lease, &target, "disk read failed")
        .unwrap();
    assert_eq!(
        refuse_adopt(&sync, &lease, key, id),
        "Sync target is not an acknowledged success"
    );
}

#[test]
fn sync_halt_after_error_retains_primary_failure() {
    let sync = Fixture::new();
    let first = sync.source("Primary");
    let second = sync.source("Sibling");
    let job_id = sync.enqueue(Some(vec![first as u64, second as u64]));
    let mut lease = sync.open();
    let target = local(sync.next(&mut lease));
    sync.worker
        .fail_source_sync_target(&mut lease, &target, "disk read failed")
        .unwrap();
    drop(target);
    let retained = failed_hold(&sync, &job_id);
    assert_eq!(
        (
            retained.0.as_str(),
            retained.1.as_str(),
            retained.2.as_str(),
            retained.3,
        ),
        ("failed", "local_read", "disk read failed", 0)
    );
    let stale = lease.clone();
    sync.worker
        .update(&mut lease, WorkerOperation::Progress(1))
        .unwrap();
    let before = sync.snapshot();
    let refused = sync
        .worker
        .halt_source_sync(
            &stale,
            SourceSyncHalt::Fatal,
            "primary source operation failed".into(),
        )
        .err()
        .unwrap();
    assert_eq!(refused.to_string(), "Stale attempt fence");
    assert_eq!(sync.snapshot(), before);
    assert_eq!(failed_hold(&sync, &job_id), retained);
    let detail = "primary source operation failed";
    let halted = sync
        .worker
        .halt_source_sync(&lease, SourceSyncHalt::Fatal, detail.into())
        .unwrap();
    assert_eq!(halted.state, PersistentState::ReconciliationRequired);
    assert_eq!(halted.recovery.as_deref(), Some(detail));
    assert_eq!(
        sync.batch(&job_id),
        ("reconciliation_required".into(), None)
    );
    assert_eq!(
        sync.targets(&job_id),
        [
            (first, "failed".into(), Some("local_read".into())),
            (second, "not_run".into(), Some("batch_halt".into())),
        ]
    );
    assert_eq!(failed_hold(&sync, &job_id), retained);
    let reopened = sync.job(&job_id);
    assert_eq!(reopened.state, PersistentState::ReconciliationRequired);
    assert_eq!(reopened.recovery.as_deref(), Some(detail));
    assert_eq!(failed_hold(&sync, &job_id), retained);
    let Fixture { owner, root, .. } = sync;
    owner.shutdown().unwrap();
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn sync_halt_after_error_keeps_target_for_reconciliation() {
    let sync = Fixture::new();
    let first = sync.source("Claimed");
    let second = sync.source("Pending");
    let job_id = sync.enqueue(Some(vec![first as u64, second as u64]));
    let mut lease = sync.open();
    let target = local(sync.next(&mut lease));
    let claim = sync
        .connection()
        .query_row(
            "SELECT state,claim_generation,reservation_token FROM source_sync_targets WHERE job_id=?1 AND ordinal=0",
            [&job_id],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, i64>(1)?,
                    row.get::<_, String>(2)?,
                ))
            },
        )
        .unwrap();
    assert_eq!(claim.0, "claimed");
    let detail = "settlement reply unresolved";
    let halted = sync
        .worker
        .halt_source_sync(&lease, SourceSyncHalt::UncertainTarget, detail.into())
        .unwrap();
    assert_eq!(halted.state, PersistentState::ReconciliationRequired);
    assert_eq!(halted.recovery.as_deref(), Some(detail));
    assert_eq!(
        sync.batch(&job_id),
        ("reconciliation_required".into(), None)
    );
    assert_eq!(
        sync.targets(&job_id),
        [
            (
                first,
                "reconciliation_required".into(),
                Some("local_settlement".into()),
            ),
            (second, "not_run".into(), Some("batch_halt".into())),
        ]
    );
    let kept: (String, String, i64, String) = sync
        .connection()
        .query_row(
            "SELECT state,failure_code,claim_generation,reservation_token FROM source_sync_targets WHERE job_id=?1 AND ordinal=0",
            [&job_id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )
        .unwrap();
    assert_eq!(kept.0, "reconciliation_required");
    assert_eq!(kept.1, "local_settlement");
    assert_eq!(kept.2, claim.1);
    assert_eq!(kept.3, claim.2);
    drop(target);
    let Fixture { owner, root, .. } = sync;
    owner.shutdown().unwrap();
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn sync_inspect_returns_authorized_target() {
    let sync = Fixture::new();
    let first = sync.source("Inspected");
    let second = sync.source("Sibling");
    let job_id = sync.enqueue(Some(vec![first as u64, second as u64]));
    let mut lease = sync.open();
    let mut target = local(sync.next(&mut lease));
    let key = target.key().clone();
    let before_claim = sync.snapshot();
    assert_eq!(
        sync.worker
            .inspect_source_sync_target(&lease, key.clone())
            .unwrap(),
        SyncTargetInspection::Pending
    );
    assert_eq!(sync.snapshot(), before_claim);
    let settlement = settlement(first);
    let settled = sync
        .worker
        .settle_source_sync_target(&lease, &mut target, settlement.clone())
        .unwrap();
    let after_settle = sync.snapshot();
    assert_eq!(
        sync.worker
            .inspect_source_sync_target(&lease, key.clone())
            .unwrap(),
        SyncTargetInspection::Succeeded(settlement.result.clone())
    );
    assert_eq!(sync.snapshot(), after_settle);
    assert_eq!(
        sync.worker.inspect_source_sync_target(&lease, key).unwrap(),
        SyncTargetInspection::Succeeded(settled.result)
    );
    assert_eq!(sync.snapshot(), after_settle);
    let sibling = local(sync.next(&mut lease));
    assert_eq!(
        sync.worker
            .inspect_source_sync_target(&lease, sibling.key().clone())
            .unwrap(),
        SyncTargetInspection::Pending
    );
    assert_eq!(sync.targets(&job_id)[0].1, "succeeded");
    drop(sibling);
    let Fixture { owner, root, .. } = sync;
    owner.shutdown().unwrap();
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn sync_inspect_is_read_only_and_tenant_scoped() {
    let sync = Fixture::new();
    let id = sync.source("Scoped");
    let job_id = sync.enqueue(Some(vec![id as u64]));
    let mut lease = sync.open();
    let target = local(sync.next(&mut lease));
    let key = target.key().clone();
    let before = sync.snapshot();
    assert_eq!(
        sync.worker
            .inspect_source_sync_target(&lease, key.clone())
            .unwrap(),
        SyncTargetInspection::Pending
    );
    assert_eq!(sync.snapshot(), before);
    let foreign = SyncTargetKey {
        job_id: "foreign-job".into(),
        ordinal: 0,
    };
    assert_eq!(
        sync.worker
            .inspect_source_sync_target(&lease, foreign)
            .err()
            .unwrap()
            .to_string(),
        "Sync target Job mismatch"
    );
    assert_eq!(sync.snapshot(), before);
    let outside = SyncTargetKey {
        job_id: job_id.clone(),
        ordinal: 1,
    };
    assert_eq!(
        sync.worker
            .inspect_source_sync_target(&lease, outside)
            .err()
            .unwrap()
            .to_string(),
        "Sync target is outside the sealed basis"
    );
    assert_eq!(sync.snapshot(), before);
    assert_eq!(
        sync.connection()
            .execute(
                "UPDATE source_sync_targets SET tenant='other-tenant' WHERE job_id=?1 AND ordinal=0",
                [&job_id],
            )
            .unwrap(),
        1
    );
    let after_tamper = sync.snapshot();
    assert_eq!(
        sync.worker
            .inspect_source_sync_target(&lease, key.clone())
            .err()
            .unwrap()
            .to_string(),
        "Sync target tenant mismatch"
    );
    assert_eq!(sync.snapshot(), after_tamper);
    assert_eq!(
        sync.connection()
            .execute(
                "UPDATE source_sync_targets SET tenant=?1 WHERE job_id=?2 AND ordinal=0",
                rusqlite::params![sync.job(&job_id).tenant, job_id],
            )
            .unwrap(),
        1
    );
    assert_eq!(
        sync.worker.inspect_source_sync_target(&lease, key).unwrap(),
        SyncTargetInspection::Pending
    );
    drop(target);
    let Fixture { owner, root, .. } = sync;
    owner.shutdown().unwrap();
    std::fs::remove_dir_all(root).unwrap();
}

fn recovery(sync: &Fixture, job_id: &str, version: i64) -> Result<SyncRecoveryInspection> {
    sync.owner
        .jobs(policy())
        .unwrap()
        .inspect_source_sync_recovery(
            Credential::Session(auth::Secret::new(sync.token.clone())),
            job_id.into(),
            version,
            &AtomicBool::new(false),
            Duration::from_secs(5),
        )
}

#[test]
fn sync_retained_inspection_reports_disposition() {
    let sync = Fixture::new();
    let first = sync.source("Retained Success");
    let second = sync.source("Retained Pending");
    let job_id = sync.enqueue(Some(vec![first as u64, second as u64]));
    let mut lease = sync.open();
    let mut target = local(sync.next(&mut lease));
    let settled = sync
        .worker
        .settle_source_sync_target(&lease, &mut target, settlement(first))
        .unwrap();
    drop(target);
    let cancelled = sync.user(UserOperation::Cancel {
        job_id: job_id.clone(),
    });
    assert_eq!(cancelled.state, PersistentState::Cancelled);
    let before = sync.snapshot();
    let inspection = recovery(&sync, &job_id, cancelled.state_version).unwrap();
    assert_eq!(inspection.state(), SyncRecoveryState::Cancelled);
    assert_eq!(
        inspection.original_authority(),
        OriginalAuthorityStatus::Valid
    );
    assert_eq!(
        inspection.targets()[0].disposition(),
        SyncRecoveryDisposition::Succeeded
    );
    assert!(inspection.targets()[0].acknowledged());
    assert_eq!(inspection.targets()[0].result(), Some(&settled.result));
    assert_eq!(
        inspection.targets()[1].disposition(),
        SyncRecoveryDisposition::NotRun
    );
    assert_eq!(
        inspection.targets()[1].failure(),
        Some("Sync was cancelled")
    );
    assert_eq!(sync.snapshot(), before);
    assert_eq!(
        recovery(&sync, &job_id, cancelled.state_version).unwrap(),
        inspection
    );
    assert_eq!(
        recovery(&sync, &job_id, cancelled.state_version - 1)
            .unwrap_err()
            .to_string(),
        "Stale Sync recovery version"
    );
    assert_eq!(sync.snapshot(), before);
    let Fixture { owner, root, .. } = sync;
    owner.shutdown().unwrap();
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn sync_retained_inspection_rejects_corrupt_binding() {
    let sync = Fixture::new();
    let first = sync.source("Bound Success");
    let second = sync.source("Bound Sibling");
    let job_id = sync.enqueue(Some(vec![first as u64, second as u64]));
    let mut lease = sync.open();
    let mut target = local(sync.next(&mut lease));
    sync.worker
        .settle_source_sync_target(&lease, &mut target, settlement(first))
        .unwrap();
    drop(target);
    let version = sync
        .user(UserOperation::Cancel {
            job_id: job_id.clone(),
        })
        .state_version;
    let before = sync.snapshot();
    let actor: String = sync
        .connection()
        .query_row(
            "SELECT authority_actor FROM source_sync_targets WHERE job_id=?1 AND ordinal=0",
            [&job_id],
            |row| row.get(0),
        )
        .unwrap();
    let digest: String = sync
        .connection()
        .query_row(
            "SELECT result_digest FROM source_sync_targets WHERE job_id=?1 AND ordinal=0",
            [&job_id],
            |row| row.get(0),
        )
        .unwrap();
    let path_digest: String = sync
        .connection()
        .query_row(
            "SELECT path_observation_digest FROM source_sync_targets WHERE job_id=?1 AND ordinal=0",
            [&job_id],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(sync.connection().execute(
        "UPDATE source_sync_targets SET authority_actor='other-actor' WHERE job_id=?1 AND ordinal=0",
        [&job_id]).unwrap(), 1);
    let after = sync.snapshot();
    assert_eq!(
        recovery(&sync, &job_id, version).unwrap_err().to_string(),
        "Sync recovery authority capture mismatch"
    );
    assert_eq!(sync.snapshot(), after);
    assert_eq!(
        sync.connection()
            .execute(
                "UPDATE source_sync_targets SET authority_actor=?1 WHERE job_id=?2 AND ordinal=0",
                rusqlite::params![actor, job_id]
            )
            .unwrap(),
        1
    );
    assert_eq!(sync.snapshot(), before);
    assert_eq!(
        sync.connection()
            .execute(
                "UPDATE source_sync_targets SET result_digest=?1 WHERE job_id=?2 AND ordinal=0",
                rusqlite::params!["cd".repeat(32), job_id]
            )
            .unwrap(),
        1
    );
    let after = sync.snapshot();
    assert_eq!(
        recovery(&sync, &job_id, version).unwrap_err().to_string(),
        "Sync result digest mismatch"
    );
    assert_eq!(sync.snapshot(), after);
    assert_eq!(
        sync.connection()
            .execute(
                "UPDATE source_sync_targets SET result_digest=?1 WHERE job_id=?2 AND ordinal=0",
                rusqlite::params![digest, job_id]
            )
            .unwrap(),
        1
    );
    assert_eq!(sync.snapshot(), before);
    assert_eq!(
        sync.connection()
            .execute(
                "UPDATE source_sync_targets SET path_observation_digest=NULL WHERE job_id=?1 AND ordinal=0",
                [&job_id],
            )
            .unwrap(),
        1
    );
    let after = sync.snapshot();
    assert_eq!(
        recovery(&sync, &job_id, version).unwrap_err().to_string(),
        "Sync producer capture incomplete"
    );
    assert_eq!(sync.snapshot(), after);
    assert_eq!(
        sync.connection()
            .execute(
                "UPDATE source_sync_targets SET path_observation_digest=?1 WHERE job_id=?2 AND ordinal=0",
                rusqlite::params![path_digest, job_id],
            )
            .unwrap(),
        1
    );
    assert_eq!(sync.snapshot(), before);
    assert_eq!(
        sync.connection()
            .execute(
                "UPDATE source_sync_targets SET path_observation_digest=?1 WHERE job_id=?2 AND ordinal=0",
                rusqlite::params!["AB".repeat(32), job_id],
            )
            .unwrap(),
        1
    );
    let after = sync.snapshot();
    assert_eq!(
        recovery(&sync, &job_id, version).unwrap_err().to_string(),
        "Invalid Sync path digest"
    );
    assert_eq!(sync.snapshot(), after);
    assert_eq!(
        sync.connection()
            .execute(
                "UPDATE source_sync_targets SET path_observation_digest=?1 WHERE job_id=?2 AND ordinal=0",
                rusqlite::params![path_digest, job_id],
            )
            .unwrap(),
        1
    );
    assert_eq!(sync.snapshot(), before);
    assert_eq!(
        sync.connection()
            .execute(
                "UPDATE source_sync_targets SET requested_source_id=?1 WHERE job_id=?2 AND ordinal=0",
                rusqlite::params![second, job_id],
            )
            .unwrap(),
        1
    );
    let after = sync.snapshot();
    assert_eq!(
        recovery(&sync, &job_id, version).unwrap_err().to_string(),
        "Sync selected Sources mismatch"
    );
    assert_eq!(sync.snapshot(), after);
    assert_eq!(
        sync.connection()
            .execute(
                "UPDATE source_sync_targets SET requested_source_id=?1 WHERE job_id=?2 AND ordinal=0",
                rusqlite::params![first, job_id],
            )
            .unwrap(),
        1
    );
    assert_eq!(sync.snapshot(), before);
    assert_eq!(
        sync.connection()
            .execute(
                "UPDATE source_sync_targets SET state='pending',failure_code=NULL,failure_detail=NULL WHERE job_id=?1 AND ordinal=1",
                [&job_id],
            )
            .unwrap(),
        1
    );
    // Clean pending shape, then refuse via the pending rule (ack=1).
    assert_eq!(
        sync.connection()
            .execute(
                "UPDATE source_sync_targets SET reservation_acknowledged=1 WHERE job_id=?1 AND ordinal=1",
                [&job_id],
            )
            .unwrap(),
        1
    );
    let after = sync.snapshot();
    assert_eq!(
        recovery(&sync, &job_id, version).unwrap_err().to_string(),
        "Invalid pending Sync target"
    );
    assert_eq!(sync.snapshot(), after);
    assert_eq!(
        sync.connection()
            .execute(
                "UPDATE source_sync_targets SET state='not_run',failure_code='batch_halt',failure_detail='Sync was cancelled',reservation_acknowledged=0 WHERE job_id=?1 AND ordinal=1",
                [&job_id],
            )
            .unwrap(),
        1
    );
    assert_eq!(sync.snapshot(), before);
    assert_eq!(sync.connection().execute(
        "UPDATE source_sync_batches SET phase='reconciliation_required',halt_code=NULL WHERE job_id=?1",
        [&job_id]).unwrap(), 1);
    let after = sync.snapshot();
    assert_eq!(
        recovery(&sync, &job_id, version).unwrap_err().to_string(),
        "Sync recovery batch phase mismatch"
    );
    assert_eq!(sync.snapshot(), after);
    let Fixture { owner, root, .. } = sync;
    owner.shutdown().unwrap();
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn sync_retained_inspection_reports_failed_and_reconciliation() {
    let sync = Fixture::new();
    let first = sync.source("Failed Target");
    let second = sync.source("Sibling Pending");
    let job_id = sync.enqueue(Some(vec![first as u64, second as u64]));
    let mut lease = sync.open();
    let target = local(sync.next(&mut lease));
    sync.worker
        .fail_source_sync_target(&mut lease, &target, "disk read failed")
        .unwrap();
    drop(target);
    // Unacked failed evidence keeps Cancel on the reconciliation path.
    let halted = sync.user(UserOperation::Cancel {
        job_id: job_id.clone(),
    });
    assert_eq!(halted.state, PersistentState::ReconciliationRequired);
    let before = sync.snapshot();
    let inspection = recovery(&sync, &job_id, halted.state_version).unwrap();
    assert_eq!(
        inspection.state(),
        SyncRecoveryState::ReconciliationRequired
    );
    assert_eq!(
        inspection.targets()[0].disposition(),
        SyncRecoveryDisposition::Failed
    );
    assert_eq!(inspection.targets()[0].failure(), Some("disk read failed"));
    assert!(!inspection.targets()[0].acknowledged());
    assert_eq!(
        inspection.targets()[1].disposition(),
        SyncRecoveryDisposition::NotRun
    );
    assert_eq!(sync.snapshot(), before);
    // batch_halt is DB-legal but not a failed-state code → unknown-code refusal.
    assert_eq!(
        sync.connection()
            .execute(
                "UPDATE source_sync_targets SET failure_code='batch_halt' WHERE job_id=?1 AND ordinal=0",
                [&job_id],
            )
            .unwrap(),
        1
    );
    let after = sync.snapshot();
    assert_eq!(
        recovery(&sync, &job_id, halted.state_version)
            .unwrap_err()
            .to_string(),
        "Invalid Sync failure code"
    );
    assert_eq!(sync.snapshot(), after);
    assert_eq!(
        sync.connection()
            .execute(
                "UPDATE source_sync_targets SET failure_code='local_read' WHERE job_id=?1 AND ordinal=0",
                [&job_id],
            )
            .unwrap(),
        1
    );
    assert_eq!(sync.snapshot(), before);
    assert_eq!(
        sync.connection()
            .execute(
                "UPDATE source_sync_targets SET reservation_acknowledged=1 WHERE job_id=?1 AND ordinal=1",
                [&job_id],
            )
            .unwrap(),
        1
    );
    let after = sync.snapshot();
    assert_eq!(
        recovery(&sync, &job_id, halted.state_version)
            .unwrap_err()
            .to_string(),
        "Invalid not-run Sync target"
    );
    assert_eq!(sync.snapshot(), after);
    assert_eq!(
        sync.connection()
            .execute(
                "UPDATE source_sync_targets SET reservation_acknowledged=0 WHERE job_id=?1 AND ordinal=1",
                [&job_id],
            )
            .unwrap(),
        1
    );
    assert_eq!(sync.snapshot(), before);
    let Fixture { owner, root, .. } = sync;
    owner.shutdown().unwrap();
    std::fs::remove_dir_all(root).unwrap();

    // Claimed-then-cancelled becomes reconciliation_required.
    let sync = Fixture::new();
    let first = sync.source("Claimed");
    let second = sync.source("Pending");
    let job_id = sync.enqueue(Some(vec![first as u64, second as u64]));
    let mut lease = sync.open();
    let target = local(sync.next(&mut lease));
    let job = sync.user(UserOperation::Cancel {
        job_id: job_id.clone(),
    });
    assert_eq!(job.state, PersistentState::ReconciliationRequired);
    drop(target);
    let before = sync.snapshot();
    let inspection = recovery(&sync, &job_id, job.state_version).unwrap();
    assert_eq!(
        inspection.state(),
        SyncRecoveryState::ReconciliationRequired
    );
    assert_eq!(
        inspection.targets()[0].disposition(),
        SyncRecoveryDisposition::ReconciliationRequired
    );
    assert_eq!(
        inspection.targets()[0].failure(),
        Some("Sync was cancelled")
    );
    assert_eq!(
        inspection.targets()[1].disposition(),
        SyncRecoveryDisposition::NotRun
    );
    assert_eq!(sync.snapshot(), before);

    // Pending disposition after restoring a not_run row to a clean pending shape.
    assert_eq!(
        sync.connection()
            .execute(
                "UPDATE source_sync_targets SET state='pending',failure_code=NULL,failure_detail=NULL WHERE job_id=?1 AND ordinal=1",
                [&job_id],
            )
            .unwrap(),
        1
    );
    let after = sync.snapshot();
    let inspection = recovery(&sync, &job_id, job.state_version).unwrap();
    assert_eq!(
        inspection.targets()[1].disposition(),
        SyncRecoveryDisposition::Pending
    );
    assert_eq!(sync.snapshot(), after);
    // Claimed with leftover failure_code is refused by the claimed rule.
    assert_eq!(
        sync.connection()
            .execute(
                "UPDATE source_sync_targets SET state='claimed',failure_code='batch_halt',failure_detail=NULL WHERE job_id=?1 AND ordinal=0",
                [&job_id],
            )
            .unwrap(),
        1
    );
    let after = sync.snapshot();
    assert_eq!(
        recovery(&sync, &job_id, job.state_version)
            .unwrap_err()
            .to_string(),
        "Invalid claimed Sync target"
    );
    assert_eq!(sync.snapshot(), after);
    assert_eq!(
        sync.connection()
            .execute(
                "UPDATE source_sync_targets SET state='claimed',failure_code=NULL,failure_detail=NULL WHERE job_id=?1 AND ordinal=0",
                [&job_id],
            )
            .unwrap(),
        1
    );
    let after = sync.snapshot();
    let inspection = recovery(&sync, &job_id, job.state_version).unwrap();
    assert_eq!(
        inspection.targets()[0].disposition(),
        SyncRecoveryDisposition::Claimed
    );
    assert_eq!(sync.snapshot(), after);
    let Fixture { owner, root, .. } = sync;
    owner.shutdown().unwrap();
    std::fs::remove_dir_all(root).unwrap();

    // OriginalAuthorityStatus::Unavailable when the job's original authority is missing.
    let sync = Fixture::new();
    let id = sync.source("Authority");
    let job_id = sync.enqueue(Some(vec![id as u64]));
    let mut lease = sync.open();
    let mut target = local(sync.next(&mut lease));
    sync.worker
        .settle_source_sync_target(&lease, &mut target, settlement(id))
        .unwrap();
    drop(target);
    let _cancelled = sync.user(UserOperation::Cancel {
        job_id: job_id.clone(),
    });
    let mut connection = sync.connection();
    let tx = connection.transaction().unwrap();
    let mut job = jobs::load(&tx, &job_id).unwrap().unwrap();
    job.authority = None;
    job.authority_disposition = jobs::AuthorityDisposition::LegacyMissing;
    jobs::save(&tx, &mut job, "test_clear_authority").unwrap();
    let version = job.state_version;
    tx.commit().unwrap();
    drop(connection);
    let before = sync.snapshot();
    let inspection = recovery(&sync, &job_id, version).unwrap();
    assert_eq!(
        inspection.original_authority(),
        OriginalAuthorityStatus::Unavailable
    );
    assert_eq!(
        inspection.targets()[0].disposition(),
        SyncRecoveryDisposition::Succeeded
    );
    assert_eq!(sync.snapshot(), before);
    let Fixture { owner, root, .. } = sync;
    owner.shutdown().unwrap();
    std::fs::remove_dir_all(root).unwrap();

    // Refused: delete sessions so the credential is invalid, then re-login for a
    // new session that is not the original authority subject/credential.
    let mut sync = Fixture::new();
    let id = sync.source("Refused");
    let job_id = sync.enqueue(Some(vec![id as u64]));
    let mut lease = sync.open();
    let mut target = local(sync.next(&mut lease));
    sync.worker
        .settle_source_sync_target(&lease, &mut target, settlement(id))
        .unwrap();
    drop(target);
    let cancelled = sync.user(UserOperation::Cancel {
        job_id: job_id.clone(),
    });
    let version = cancelled.state_version;
    sync.connection()
        .execute("DELETE FROM sessions", [])
        .unwrap();
    sync.login();
    let before = sync.snapshot();
    let inspection = recovery(&sync, &job_id, version).unwrap();
    assert!(matches!(
        inspection.original_authority(),
        OriginalAuthorityStatus::Refused(_)
    ));
    assert_eq!(sync.snapshot(), before);
    let Fixture { owner, root, .. } = sync;
    owner.shutdown().unwrap();
    std::fs::remove_dir_all(root).unwrap();
}

fn refuse_finish(sync: &Fixture, lease: &AttemptLease) -> String {
    let rows = sync.snapshot();
    let error = sync.worker.finish_source_sync(lease).unwrap_err();
    assert_eq!(sync.snapshot(), rows);
    error.to_string()
}

fn completed_finish(sync: &Fixture, lease: &AttemptLease) -> Box<CompletedSourceSync> {
    match sync.worker.finish_source_sync(lease).unwrap() {
        SourceSyncFinishOutcome::Completed(completed) => completed,
        SourceSyncFinishOutcome::NeedsReconciliation => {
            panic!("ordinary Sync completion required reconciliation")
        }
    }
}

#[test]
fn sync_finish_requires_all_targets_settled() {
    let sync = Fixture::new();
    let first = sync.source("Finish First");
    let second = sync.source("Finish Second");
    let job_id = sync.enqueue(Some(vec![first as u64, second as u64]));
    let mut lease = sync.open();
    let mut first_target = local(sync.next(&mut lease));
    let first_settlement = settlement(first);
    let settled = sync
        .worker
        .settle_source_sync_target(&lease, &mut first_target, first_settlement.clone())
        .unwrap();
    drop(first_target);
    assert_eq!(
        refuse_finish(&sync, &lease),
        "Sync target is not acknowledged"
    );
    assert_eq!(sync.batch(&job_id), ("open".into(), None));
    assert_eq!(
        sync.targets(&job_id),
        vec![
            (first, "succeeded".into(), None),
            (second, "pending".into(), None),
        ]
    );
    let mut second_target = local(sync.next(&mut lease));
    let second_settlement = settlement(second);
    sync.worker
        .settle_source_sync_target(&lease, &mut second_target, second_settlement.clone())
        .unwrap();
    drop(second_target);
    assert!(matches!(sync.next(&mut lease), NextSyncTarget::Complete));
    let completed = completed_finish(&sync, &lease);
    assert_eq!(completed.job.job_id, job_id);
    assert_eq!(completed.job.state, PersistentState::Succeeded);
    assert_eq!(completed.job.progress, Some(100));
    assert_eq!(
        completed.aggregate,
        jobs::SourceSyncResult {
            synced: 2,
            failed: 0,
            results: vec![
                first_settlement.result.clone(),
                second_settlement.result.clone()
            ],
            failures: vec![],
        }
    );
    assert_eq!(settled.result, first_settlement.result);
    assert_eq!(
        completed.job.public_result,
        Some(jobs::CompletedResult::SourceSync(
            completed.aggregate.clone()
        ))
    );
    assert_eq!(sync.batch(&job_id), ("completed".into(), None));
    let (phase, aggregate_json, aggregate_digest): (String, String, String) = sync
        .connection()
        .query_row(
            "SELECT phase,aggregate_json,aggregate_digest FROM source_sync_batches WHERE job_id=?1",
            [&job_id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .unwrap();
    assert_eq!(phase, "completed");
    let stored: jobs::SourceSyncResult = serde_json::from_str(&aggregate_json).unwrap();
    assert_eq!(stored, completed.aggregate);
    assert_eq!(
        digest(serde_json::to_string(&stored).unwrap().as_bytes()),
        aggregate_digest
    );
    let Fixture { owner, root, .. } = sync;
    owner.shutdown().unwrap();
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn sync_finish_replay_returns_same_aggregate() {
    let sync = Fixture::new();
    let first = sync.source("Replay First");
    let second = sync.source("Replay Second");
    let job_id = sync.enqueue(Some(vec![first as u64, second as u64]));
    let mut lease = sync.open();
    let mut first_target = local(sync.next(&mut lease));
    sync.worker
        .settle_source_sync_target(&lease, &mut first_target, settlement(first))
        .unwrap();
    drop(first_target);
    let second_target = local(sync.next(&mut lease));
    sync.worker
        .fail_source_sync_target(&mut lease, &second_target, "disk read failed")
        .unwrap();
    let observation = second_target.observation().clone();
    sync.worker
        .acknowledge_source_sync_target(&mut lease, &second_target, &observation)
        .unwrap();
    drop(second_target);
    let finished = completed_finish(&sync, &lease);
    assert_eq!(finished.job.state, PersistentState::Succeeded);
    assert_eq!(finished.aggregate.synced, 1);
    assert_eq!(finished.aggregate.failed, 1);
    assert_eq!(
        finished.aggregate.failures,
        vec![jobs::SourceSyncFailure {
            project_id: Some(second as u64),
            name: Some("Replay Second".into()),
            error: "disk read failed".into(),
        }]
    );
    assert_eq!(sync.batch(&job_id), ("completed".into(), None));
    let after = sync.snapshot();
    let version = finished.job.state_version;
    let replay = completed_finish(&sync, &lease);
    assert_eq!(replay.job.state_version, version);
    assert_eq!(replay.job.state, finished.job.state);
    assert_eq!(replay.aggregate, finished.aggregate);
    assert_eq!(replay.job.public_result, finished.job.public_result);
    assert_eq!(sync.snapshot(), after);
    assert_eq!(sync.batch(&job_id), ("completed".into(), None));
    let Fixture { owner, root, .. } = sync;
    owner.shutdown().unwrap();
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn sync_finish_all_failed_marks_the_job_failed_with_joined_recovery() {
    let sync = Fixture::new();
    let missing = 9_999_i64;
    let remote = sync.source("All Failed Remote");
    sync.connection()
        .execute(
            "UPDATE projects SET source_type='github' WHERE id=?1",
            [remote],
        )
        .unwrap();
    let local_id = sync.source("All Failed Local");
    let job_id = sync.enqueue(Some(vec![missing as u64, remote as u64, local_id as u64]));
    let mut lease = sync.open();
    assert!(matches!(
        sync.next(&mut lease),
        NextSyncTarget::OrdinaryFailure(_)
    ));
    assert!(matches!(
        sync.next(&mut lease),
        NextSyncTarget::OrdinaryFailure(_)
    ));
    let target = local(sync.next(&mut lease));
    sync.worker
        .fail_source_sync_target(&mut lease, &target, "disk read failed")
        .unwrap();
    let observation = target.observation().clone();
    sync.worker
        .acknowledge_source_sync_target(&mut lease, &target, &observation)
        .unwrap();
    drop(target);
    assert!(matches!(sync.next(&mut lease), NextSyncTarget::Complete));
    let finished = completed_finish(&sync, &lease);
    let expected = jobs::SourceSyncResult {
        synced: 0,
        failed: 3,
        results: vec![],
        failures: vec![
            SourceSyncFailure {
                project_id: Some(missing as u64),
                name: None,
                error: "Source not found".into(),
            },
            SourceSyncFailure {
                project_id: Some(remote as u64),
                name: Some("All Failed Remote".into()),
                error: "Source kind is not supported by Local Sync".into(),
            },
            SourceSyncFailure {
                project_id: Some(local_id as u64),
                name: Some("All Failed Local".into()),
                error: "disk read failed".into(),
            },
        ],
    };
    assert_eq!(finished.aggregate, expected);
    assert_eq!(finished.job.job_id, job_id);
    assert_eq!(finished.job.state, PersistentState::Failed);
    assert_eq!(finished.job.public_result, None);
    assert_eq!(finished.job.progress, Some(100));
    let recovery = format!(
        "{missing}: Source not found; {remote}: Source kind is not supported by Local Sync; {local_id}: disk read failed"
    );
    assert_eq!(finished.job.recovery.as_deref(), Some(recovery.as_str()));
    let stored = sync.job(&job_id);
    assert_eq!(stored.state, PersistentState::Failed);
    assert_eq!(stored.public_result, None);
    assert_eq!(stored.recovery.as_deref(), Some(recovery.as_str()));
    assert_eq!(sync.batch(&job_id), ("failed".into(), None));
    let aggregate_json: String = sync
        .connection()
        .query_row(
            "SELECT aggregate_json FROM source_sync_batches WHERE job_id=?1",
            [&job_id],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(
        serde_json::from_str::<jobs::SourceSyncResult>(&aggregate_json).unwrap(),
        expected
    );
    // Exact retry takes the terminal branch: same aggregate, no write.
    let after = sync.snapshot();
    let replay = completed_finish(&sync, &lease);
    assert_eq!(replay.aggregate, expected);
    assert_eq!(replay.job.state, PersistentState::Failed);
    assert_eq!(replay.job.public_result, None);
    assert_eq!(replay.job.state_version, finished.job.state_version);
    assert_eq!(sync.snapshot(), after);
    let Fixture { owner, root, .. } = sync;
    owner.shutdown().unwrap();
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn sync_finish_refuses_unsettled_claims_unacknowledged_failures_and_reconciliation_codes() {
    let sync = Fixture::new();
    let id = sync.source("Refused Finish");
    let job_id = sync.enqueue(Some(vec![id as u64]));
    let mut lease = sync.open();
    let target = local(sync.next(&mut lease));
    assert_eq!(
        refuse_finish(&sync, &lease),
        "Sync target is not acknowledged"
    );
    assert_eq!(sync.targets(&job_id), vec![(id, "claimed".into(), None)]);
    sync.worker
        .fail_source_sync_target(&mut lease, &target, "disk read failed")
        .unwrap();
    assert_eq!(
        refuse_finish(&sync, &lease),
        "Sync target is not acknowledged"
    );
    assert_eq!(
        sync.targets(&job_id),
        vec![(id, "failed".into(), Some("local_read".into()))]
    );
    let observation = target.observation().clone();
    sync.worker
        .acknowledge_source_sync_target(&mut lease, &target, &observation)
        .unwrap();
    drop(target);
    // Reserved codes are valid failed rows, but only a reconciler may close them.
    tamper(
        &sync,
        "UPDATE source_sync_targets SET failure_code='configuration_changed' WHERE job_id=?1 AND failure_code='local_read'",
        &job_id,
    );
    assert_eq!(
        refuse_finish(&sync, &lease),
        "Sync terminal failure requires reconciliation"
    );
    assert_eq!(sync.batch(&job_id), ("open".into(), None));
    tamper(
        &sync,
        "UPDATE source_sync_targets SET failure_code='local_read' WHERE job_id=?1 AND failure_code='configuration_changed'",
        &job_id,
    );
    let finished = completed_finish(&sync, &lease);
    assert_eq!(finished.job.state, PersistentState::Failed);
    assert_eq!(sync.batch(&job_id), ("failed".into(), None));
    let Fixture { owner, root, .. } = sync;
    owner.shutdown().unwrap();
    std::fs::remove_dir_all(root).unwrap();
}

fn job_rows(sync: &Fixture, job_id: &str) -> Value {
    let connection = sync.connection();
    let rows = |sql: &str| -> Vec<Vec<String>> {
        let mut statement = connection.prepare(sql).unwrap();
        let width = statement.column_count();
        statement
            .query_map([job_id], |row| {
                (0..width)
                    .map(|index| row.get::<_, rusqlite::types::Value>(index))
                    .map(|value| value.map(|value| format!("{value:?}")))
                    .collect()
            })
            .unwrap()
            .collect::<rusqlite::Result<_>>()
            .unwrap()
    };
    serde_json::json!([
        rows("SELECT * FROM source_sync_batches WHERE job_id=?1"),
        rows("SELECT * FROM source_sync_targets WHERE job_id=?1 ORDER BY ordinal"),
        rows("SELECT * FROM durable_jobs WHERE id=?1"),
        rows("SELECT * FROM durable_job_history WHERE job_id=?1 ORDER BY version"),
        rows("SELECT * FROM durable_job_keys WHERE job_id=?1"),
    ])
}

#[test]
fn sync_prune_refuses_retained_claim() {
    for shape in ["claimed", "failed", "reconciliation_required"] {
        let sync = Fixture::new();
        let finished_source = sync.source("Prune Finished");
        let held_source = sync.source("Prune Held");
        let finished_id = sync.enqueue(Some(vec![finished_source as u64]));
        let mut lease = sync.open();
        let mut target = local(sync.next(&mut lease));
        sync.worker
            .settle_source_sync_target(&lease, &mut target, settlement(finished_source))
            .unwrap();
        drop(target);
        let finished = completed_finish(&sync, &lease);
        let held_id = enqueue_later(&sync, held_source);
        let mut lease = sync.open();
        let target = local(sync.next(&mut lease));
        match shape {
            "claimed" => {}
            "failed" => {
                sync.worker
                    .fail_source_sync_target(&mut lease, &target, "disk read failed")
                    .unwrap();
            }
            _ => {
                sync.worker
                    .update(&mut lease, WorkerOperation::Fail)
                    .unwrap();
            }
        }
        drop(target);
        // Force a terminal Job that still holds Sync evidence, so this test
        // checks the prune guard on its own.
        change_job(&sync, &held_id, |job| {
            job.state = PersistentState::Failed;
            job.worker = None;
            job.fence = None;
            job.lease_until = None;
        });
        let mut connection = sync.connection();
        let tx = connection.transaction().unwrap();
        for id in [&finished_id, &held_id] {
            tx.execute(
                "UPDATE durable_jobs SET updated=?1 WHERE id=?2",
                params![now() - 86401, id],
            )
            .unwrap();
        }
        tx.commit().unwrap();
        let held_state = match shape {
            "claimed" => (held_source, "claimed".into(), None),
            "failed" => (held_source, "failed".into(), Some("local_read".into())),
            _ => (
                held_source,
                "reconciliation_required".into(),
                Some("local_settlement".into()),
            ),
        };
        assert_eq!(sync.targets(&held_id), std::slice::from_ref(&held_state));
        let held_rows = job_rows(&sync, &held_id);
        assert_eq!(sync.owner.retain_jobs(1, 1).unwrap(), 1, "{shape}");
        assert_eq!(job_rows(&sync, &held_id), held_rows, "{shape}");
        assert_eq!(sync.targets(&held_id), [held_state]);
        // Permitted cleanup drops the settled batch and archives the finish result.
        let rows = job_rows(&sync, &finished_id);
        assert_eq!(rows[0], serde_json::json!([]));
        assert_eq!(rows[1], serde_json::json!([]));
        assert_eq!(rows[2], serde_json::json!([]));
        let archived: String = connection
            .query_row(
                "SELECT archived_document FROM durable_job_keys WHERE job_id=?1",
                [&finished_id],
                |row| row.get(0),
            )
            .unwrap();
        let archived = jobs::decode(&archived).unwrap();
        assert_eq!(archived.state, PersistentState::Succeeded);
        assert_eq!(archived.state_version, finished.job.state_version);
        assert_eq!(archived.public_result, finished.job.public_result);
        assert_eq!(
            archived.public_result,
            Some(jobs::CompletedResult::SourceSync(jobs::SourceSyncResult {
                synced: 1,
                failed: 0,
                results: vec![settlement(finished_source).result],
                failures: vec![],
            }))
        );
        let replay = sync.user(UserOperation::Enqueue {
            key: "sync".into(),
            payload_version: 1,
            payload: Payload::Sync {
                project_ids: Some(vec![finished_source as u64]),
            },
        });
        assert_eq!(replay.job_id, finished_id);
        assert_eq!(replay.public_result, finished.job.public_result);
        let after = sync.snapshot();
        assert_eq!(sync.owner.retain_jobs(1, 1).unwrap(), 0, "{shape}");
        assert_eq!(sync.snapshot(), after);
        assert_eq!(job_rows(&sync, &held_id), held_rows);
        drop(connection);
        let Fixture { owner, root, .. } = sync;
        owner.shutdown().unwrap();
        std::fs::remove_dir_all(root).unwrap();
    }
}

#[test]
fn sync_reconciliation_uses_batch_owner() {
    let sync = Fixture::new();
    let first = sync.source("Owner First");
    let second = sync.source("Owner Second");
    let job_id = sync.enqueue(Some(vec![first as u64, second as u64]));
    let mut lease = sync.open();
    drop(local(sync.next(&mut lease)));
    let effect = jobs::EffectIntent {
        operation: jobs::EffectOperation::SourceRefresh,
        basis_hash: "a".repeat(64),
        content_hash: "b".repeat(64),
        target: first.to_string(),
    };
    change_job(&sync, &job_id, |job| {
        job.lease_until = Some(now() - 1);
        job.state = PersistentState::EffectAdmitted;
        job.effects.push(jobs::EffectReceipt {
            intent: effect,
            attempt: job.attempt,
            generation: job.generation,
            confirmed: false,
            no_effect: false,
            receipt: None,
            checklist_completion: None,
        });
    });
    assert!(sync.worker.claim_kind(JobKind::Sync).unwrap().is_none());
    let held = sync.job(&job_id);
    assert_eq!(held.state, PersistentState::ReconciliationRequired);
    assert_eq!(
        sync.batch(&job_id),
        ("reconciliation_required".into(), None)
    );
    let before = sync.snapshot();
    let receipt = jobs::ResultArtifact {
        receipt_id: "sync-receipt".into(),
        content_hash: "b".repeat(64),
        target: first.to_string(),
    };
    for (decision, receipt) in [
        (jobs::Decision::ConfirmSucceeded, Some(receipt)),
        (jobs::Decision::ConfirmNoEffect, None),
        (jobs::Decision::Abandon, None),
    ] {
        let error = sync
            .owner
            .jobs(policy())
            .unwrap()
            .submit(
                Credential::Session(auth::Secret::new(sync.token.clone())),
                UserOperation::Reconcile {
                    job_id: job_id.clone(),
                    expected_version: held.state_version,
                    expected_generation: held.generation,
                    effect_hash: "b".repeat(64),
                    decision,
                    receipt,
                },
                &AtomicBool::new(false),
                Duration::from_secs(5),
            )
            .unwrap()
            .receive()
            .err()
            .unwrap();
        assert_eq!(
            error.to_string(),
            "Sync cannot use generic effect reconciliation"
        );
        assert_eq!(sync.snapshot(), before);
    }
    let reconciliations: i64 = sync
        .connection()
        .query_row(
            "SELECT count(*) FROM durable_job_reconciliations",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(reconciliations, 0);
    let inspection = recovery(&sync, &job_id, held.state_version).unwrap();
    assert_eq!(
        inspection.state(),
        SyncRecoveryState::ReconciliationRequired
    );
    assert_eq!(
        inspection
            .targets()
            .iter()
            .map(|target| (
                target.requested_source_id(),
                target.disposition(),
                target.acknowledged()
            ))
            .collect::<Vec<_>>(),
        [
            (
                first as u64,
                SyncRecoveryDisposition::ReconciliationRequired,
                false
            ),
            (second as u64, SyncRecoveryDisposition::NotRun, false),
        ]
    );
    assert_eq!(sync.owner.retain_jobs(1, 1).unwrap(), 0);
    assert_eq!(sync.snapshot(), before);
    let Fixture { owner, root, .. } = sync;
    owner.shutdown().unwrap();
    std::fs::remove_dir_all(root).unwrap();
}
