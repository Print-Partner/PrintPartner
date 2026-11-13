#![cfg(feature = "desktop_rebuild")]
//! Replays the Node job runner golden in `fixtures/parity/jobs-engine`.
//! Regenerate it with `node fixtures/parity/jobs-engine/generate.mjs`.
include!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/src/claimed_attempt.rs"
));
use pp_storage::jobs::ClaimedAttempt;
use pp_storage::{
    Limits, WriterOwner,
    auth::{self, AuthPolicy, FirstUserTenant, RegistrationPolicy, Secret, SessionTenantPolicy},
    jobs::{
        CompletedResult, Credential, FrameAdmission, JobAccessFailure, JobKind, JobSubscription,
        LegacyListFilter, LegacyProfileFilter, LegacyStatusFilter, NonblockingSend, Outcome,
        Payload, PreparedEvent, StreamClose, UserOperation, WorkerAdmission, WorkerOperation,
    },
};
use std::{
    path::Path,
    sync::{Arc, atomic::AtomicBool},
    time::Duration,
};

#[path = "common/scratch.rs"]
mod scratch;

fn policy() -> AuthPolicy {
    AuthPolicy {
        registration: RegistrationPolicy::Open,
        session_tenant: SessionTenantPolicy::AccountTenant,
        first_user: FirstUserTenant::NewUser,
    }
}

fn session(owner: &WriterOwner, email: &str) -> Credential {
    let auth::Outcome::Session { token, .. } = owner
        .auth_with_policy(policy())
        .unwrap()
        .submit(
            auth::Request::Register {
                email: email.into(),
                display_name: "Parity".into(),
                password: Secret::new("long-test-password".into()),
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
    Credential::Session(token)
}

fn golden() -> serde_json::Value {
    serde_json::from_str(include_str!(
        "../../../../fixtures/parity/jobs-engine/jobs-engine.json"
    ))
    .unwrap()
}

fn open() -> (scratch::Scratch, WriterOwner) {
    let path = scratch::Scratch::new("pp-jobs-engine-parity-");
    let owner = WriterOwner::open(&path, Limits::default()).unwrap().0;
    (path, owner)
}

fn copy(credential: &Credential) -> Credential {
    match credential {
        Credential::Session(token) => Credential::Session(Secret::new(token.expose().into())),
        _ => unreachable!(),
    }
}

fn call(
    owner: &WriterOwner,
    credential: Credential,
    operation: UserOperation,
) -> anyhow::Result<Outcome> {
    owner
        .jobs(policy())?
        .submit(
            credential,
            operation,
            &AtomicBool::new(false),
            Duration::from_secs(5),
        )?
        .receive()
}

/// Node starts a job and immediately reports it pending at progress 0, then
/// hides it from every other tenant. The durable engine stores the job, so its
/// queued message names worker admission ("Waiting for a compatible worker")
/// instead of Node's "Queued"; the reference model keeps that wording.
#[test]
fn enqueued_jobs_project_like_node_and_stay_tenant_private() {
    let golden = golden();
    let cases = golden["cases"].as_array().unwrap();
    assert_eq!(cases.len(), 2);
    let (_scratch, owner) = open();
    let tenant_a = session(&owner, "parity-a@example.com");
    let tenant_b = session(&owner, "parity-b@example.com");
    for case in cases {
        let payload = match case["kind"].as_str().unwrap() {
            "check-source-updates" => Payload::CheckSourceUpdates {},
            "export-checklist-html" => Payload::ExportChecklistHtml { profile_id: 7 },
            other => panic!("unexpected kind {other}"),
        };
        let Outcome::Job(job, _) = call(
            &owner,
            copy(&tenant_a),
            UserOperation::Enqueue {
                key: case["id"].as_str().unwrap().into(),
                payload_version: 1,
                payload,
            },
        )
        .unwrap() else {
            panic!("job expected")
        };
        let Outcome::Job(read, _) = call(
            &owner,
            copy(&tenant_a),
            UserOperation::Get {
                job_id: job.job_id.clone(),
            },
        )
        .unwrap() else {
            panic!("job expected")
        };
        let snapshot = serde_json::to_value(read.snapshot()).unwrap();
        for field in ["kind", "status", "progress", "result", "error"] {
            assert_eq!(
                snapshot[field], case["queued"][field],
                "{} {field}",
                case["id"]
            );
        }

        assert!(case["other_tenant"].is_null() && case["other_tenant_stream"].is_null());
        let hidden_stream = owner
            .jobs(policy())
            .unwrap()
            .subscribe(
                copy(&tenant_b),
                job.job_id.clone(),
                &AtomicBool::new(false),
                Duration::from_secs(5),
            )
            .err()
            .expect("other tenant subscribed to the job");
        assert_eq!(
            hidden_stream.downcast_ref::<JobAccessFailure>(),
            Some(&JobAccessFailure::NotFound)
        );
        let hidden = call(
            &owner,
            copy(&tenant_b),
            UserOperation::Get { job_id: job.job_id },
        )
        .err()
        .expect("other tenant read the job");
        assert_eq!(
            hidden.downcast_ref::<JobAccessFailure>(),
            Some(&JobAccessFailure::NotFound)
        );
        assert_eq!(hidden.to_string(), "Job not found");
    }
    owner.shutdown().unwrap();
}

fn enqueue(owner: &WriterOwner, credential: &Credential, key: &str, payload: Payload) -> String {
    let Outcome::Job(job, _) = call(
        owner,
        copy(credential),
        UserOperation::Enqueue {
            key: key.into(),
            payload_version: 1,
            payload,
        },
    )
    .unwrap() else {
        panic!("job expected")
    };
    job.job_id
}

fn subscribe(owner: &WriterOwner, credential: &Credential, job_id: String) -> JobSubscription {
    owner
        .jobs(policy())
        .unwrap()
        .subscribe(
            copy(credential),
            job_id,
            &AtomicBool::new(false),
            Duration::from_secs(5),
        )
        .unwrap()
}

/// Node's listener sees only the events after it subscribes, so the durable
/// stream's queued and claimed frames come first. Node's running event carries
/// "Running…" where the durable model says "Running", and Node fails a job at
/// progress 100 with the handler's error text where the durable worker keeps
/// the last progress and reports "Job failed".
fn assert_stream_ends_like_node(
    subscription: &JobSubscription,
    case: &serde_json::Value,
    fields: &[&str],
) {
    let node = case["stream"].as_array().unwrap();
    let frames = drain(subscription);
    assert_eq!(frames[0]["status"], "pending");
    let tail = &frames[frames.len() - node.len()..];
    for (frame, event) in tail.iter().zip(node) {
        for field in fields {
            assert_eq!(frame[field], event[field], "{} {field}", case["id"]);
        }
    }
}

/// Delivers every frame of a stream that must already have reached its end.
fn drain(subscription: &JobSubscription) -> Vec<serde_json::Value> {
    let mut frames = Vec::new();
    loop {
        match subscription
            .prepare(Duration::ZERO, |snapshot| serde_json::to_value(snapshot))
            .unwrap()
        {
            PreparedEvent::Frame(frame) => assert_eq!(
                subscription.admit(frame, |frame| {
                    frames.push(frame);
                    NonblockingSend::Accepted
                }),
                FrameAdmission::Admitted
            ),
            PreparedEvent::Closed(close) => {
                assert_eq!(close, StreamClose::Finished);
                return frames;
            }
            PreparedEvent::Pending => panic!("stream did not finish"),
        }
    }
}

/// Node runs the update check to an empty result and fails the checklist
/// export, which finishes later. The durable worker finishes the first with
/// Node's literal result and fails the second a second later, because stored
/// times have whole-second precision. The public read and every retained list
/// filter then match Node's kinds, statuses and newest-first order.
#[test]
fn finished_and_retained_jobs_read_and_list_like_node() {
    let golden = golden();
    let finished = golden["finished"].as_array().unwrap();
    assert_eq!(finished.len(), 2);
    let (_scratch, owner) = open();
    let tenant_a = session(&owner, "retained-a@example.com");
    let tenant_b = session(&owner, "retained-b@example.com");
    let client = owner.jobs(policy()).unwrap();
    let worker = owner
        .job_worker_with_policy(
            policy(),
            WorkerAdmission {
                kinds: vec![
                    (JobKind::CheckSourceUpdates, 1),
                    (JobKind::ExportChecklistHtml, 1),
                ],
                total: 2,
                per_resource: 1,
                lease_seconds: 60,
            },
        )
        .unwrap();

    let done = &finished[0];
    assert_eq!(done["kind"], "check-source-updates");
    let id = enqueue(
        &owner,
        &tenant_a,
        done["id"].as_str().unwrap(),
        Payload::CheckSourceUpdates {},
    );
    let stream = subscribe(&owner, &tenant_a, id);
    let claimed_attempt!(job, mut lease) = worker.claim().unwrap().unwrap();
    worker
        .update(&mut lease, WorkerOperation::Progress(10))
        .unwrap();
    let result = serde_json::from_value(done["snapshot"]["result"].clone()).unwrap();
    worker
        .update(
            &mut lease,
            WorkerOperation::FinishPublic {
                artifact: None,
                result: Box::new(CompletedResult::SourceUpdates(result)),
            },
        )
        .unwrap();
    let snapshot = client
        .read_public(
            copy(&tenant_a),
            job.job_id.clone(),
            &AtomicBool::new(false),
            Duration::from_secs(5),
        )
        .unwrap();
    let snapshot = serde_json::to_value(snapshot).unwrap();
    for field in ["kind", "status", "message", "progress", "result", "error"] {
        assert_eq!(snapshot[field], done["snapshot"][field], "{field}");
    }
    assert_stream_ends_like_node(
        &stream,
        done,
        &["kind", "status", "progress", "result", "error"],
    );
    let hidden = client
        .read_public(
            copy(&tenant_b),
            job.job_id,
            &AtomicBool::new(false),
            Duration::from_secs(5),
        )
        .unwrap_err();
    assert_eq!(
        hidden.downcast_ref::<JobAccessFailure>(),
        Some(&JobAccessFailure::NotFound)
    );

    std::thread::sleep(Duration::from_millis(1100));
    let failed = &finished[1];
    assert_eq!(failed["kind"], "export-checklist-html");
    let id = enqueue(
        &owner,
        &tenant_a,
        failed["id"].as_str().unwrap(),
        Payload::ExportChecklistHtml { profile_id: 7 },
    );
    let stream = subscribe(&owner, &tenant_a, id);
    let claimed_attempt!(_, mut lease) = worker.claim().unwrap().unwrap();
    worker
        .update(&mut lease, WorkerOperation::Progress(10))
        .unwrap();
    let job = worker.update(&mut lease, WorkerOperation::Fail).unwrap();
    assert_eq!(job.snapshot().status, failed["status"]);
    assert_stream_ends_like_node(&stream, failed, &["kind", "status"]);

    let retained = golden["retained"].as_array().unwrap();
    assert_eq!(retained.len(), 9);
    for case in retained {
        let id = case["id"].as_str().unwrap();
        let (credential, filter) = match id {
            "all" => (&tenant_a, LegacyListFilter::default()),
            "done" => (
                &tenant_a,
                LegacyListFilter {
                    status: LegacyStatusFilter::Exact("done".into()),
                    ..Default::default()
                },
            ),
            "unknown_status" => (
                &tenant_a,
                LegacyListFilter {
                    status: LegacyStatusFilter::Exact("bogus".into()),
                    ..Default::default()
                },
            ),
            "profile_7" | "profile_8" => (
                &tenant_a,
                LegacyListFilter {
                    profile: LegacyProfileFilter::Exact(id[8..].parse().unwrap()),
                    ..Default::default()
                },
            ),
            "non_numeric_profile" => (
                &tenant_a,
                LegacyListFilter {
                    profile: LegacyProfileFilter::NoMatch,
                    ..Default::default()
                },
            ),
            "since_epoch" => (
                &tenant_a,
                LegacyListFilter {
                    since_millis: Some(0),
                    ..Default::default()
                },
            ),
            "since_future" => (
                &tenant_a,
                LegacyListFilter {
                    since_millis: Some(4_102_444_800_000),
                    ..Default::default()
                },
            ),
            "other_tenant" => (&tenant_b, LegacyListFilter::default()),
            other => panic!("unexpected list case {other}"),
        };
        let jobs = client
            .list_retained(
                copy(credential),
                filter,
                &AtomicBool::new(false),
                Duration::from_secs(5),
            )
            .unwrap()
            .iter()
            .map(|job| serde_json::json!({"kind": job.kind, "status": job.status}))
            .collect::<Vec<_>>();
        assert_eq!(serde_json::Value::from(jobs), case["jobs"], "{id}");
    }
    owner.shutdown().unwrap();
}

/// Rewrites a queued job as a supplied import, the internal kind only the
/// upload journal enqueues.
fn make_supplied_import(path: &Path, job_id: &str) {
    let db = rusqlite::Connection::open(path.join("print-partner.db")).unwrap();
    let document: String = db
        .query_row(
            "SELECT document FROM durable_jobs WHERE id=?1",
            [job_id],
            |row| row.get(0),
        )
        .unwrap();
    let mut document: serde_json::Value = serde_json::from_str(&document).unwrap();
    document["kind"] = "supplied-source-import".into();
    document["payload"] = serde_json::to_value(Payload::SuppliedSourceImport {
        project_id: 1,
        operation_key: "upload".into(),
        input_version: 1,
    })
    .unwrap();
    db.execute(
        "UPDATE durable_jobs SET kind='supplied-source-import',resource='source:1',document=?2 WHERE id=?1",
        rusqlite::params![job_id, document.to_string()],
    )
    .unwrap();
}

/// Node has no supplied-import kind: starting one is refused. A durable
/// supplied import belongs to the upload journal, so the public client refuses
/// to enqueue one too. Unlike Node, an existing one is the tenant's upload job:
/// it reads, lists and streams like any other kind (GRE-428).
#[test]
fn supplied_imports_are_journal_owned_and_publicly_visible() {
    let node = &golden()["internal"];
    assert_eq!(
        node["start_error"],
        "Unsupported job kind: supplied-source-import"
    );
    assert!(node["get"].is_null() && node["stream"].is_null());
    assert_eq!(node["list"], serde_json::json!([]));

    let (path, owner) = open();
    let tenant = session(&owner, "parity-internal@example.com");
    let refused = call(
        &owner,
        copy(&tenant),
        UserOperation::Enqueue {
            key: "upload".into(),
            payload_version: 1,
            payload: Payload::SuppliedSourceImport {
                project_id: 1,
                operation_key: "upload".into(),
                input_version: 1,
            },
        },
    )
    .err()
    .expect("public client enqueued a supplied import");
    assert_eq!(
        refused.to_string(),
        "Supplied imports require atomic domain admission"
    );

    let job_id = enqueue(&owner, &tenant, "internal", Payload::CheckSourceUpdates {});
    make_supplied_import(&path, &job_id);
    let Outcome::Job(record, _) = call(
        &owner,
        copy(&tenant),
        UserOperation::Get {
            job_id: job_id.clone(),
        },
    )
    .unwrap() else {
        panic!("job expected")
    };
    assert_eq!(record.kind, JobKind::SuppliedSourceImport);

    let jobs = owner.jobs(policy()).unwrap();
    let read = jobs
        .read_public(
            copy(&tenant),
            job_id.clone(),
            &AtomicBool::new(false),
            Duration::from_secs(5),
        )
        .unwrap();
    assert_eq!(read.kind, JobKind::SuppliedSourceImport);
    assert!(
        jobs.subscribe(
            copy(&tenant),
            job_id.clone(),
            &AtomicBool::new(false),
            Duration::from_secs(5),
        )
        .is_ok()
    );
    let listed = jobs
        .list_retained(
            copy(&tenant),
            LegacyListFilter::default(),
            &AtomicBool::new(false),
            Duration::from_secs(5),
        )
        .unwrap();
    let listed = serde_json::to_value(&*listed).unwrap();
    assert_eq!(listed[0]["job_id"], job_id.as_str(), "{listed}");
    owner.shutdown().unwrap();
}
