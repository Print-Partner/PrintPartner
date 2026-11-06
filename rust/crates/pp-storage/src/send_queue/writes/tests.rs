use super::*;
use crate::{
    Limits, WriterOwner,
    auth::{
        AuthPolicy, FirstUserTenant, Outcome as AuthOutcome, RegistrationPolicy,
        Request as AuthRequest, Secret, SessionTenantPolicy,
    },
    jobs::{
        ClaimedAttempt, EffectIntent, EffectOperation, JobKind, ResultArtifact, ServerWorkerClient,
        WorkerAdmission, WorkerOperation,
    },
    send_queue::{INTERRUPTED_ERROR, NOT_STARTED_ERROR, UNKNOWN_OUTCOME_ERROR},
};
use serde_json::json;
use std::path::PathBuf;

const POLICY: AuthPolicy = AuthPolicy {
    registration: RegistrationPolicy::Open,
    session_tenant: SessionTenantPolicy::AccountTenant,
    first_user: FirstUserTenant::NewUser,
};

struct Harness {
    root: PathBuf,
    owner: WriterOwner,
    client: SendQueueClient,
    token: String,
    tenant: String,
}

impl Harness {
    fn open() -> Self {
        let root = std::env::temp_dir().join(format!(
            "pp-send-queue-{}",
            hex::encode(rand::random::<[u8; 16]>())
        ));
        let (owner, _) = WriterOwner::open(&root, Limits::default()).unwrap();
        let AuthOutcome::Session { user, token } = owner
            .auth_with_policy(POLICY)
            .unwrap()
            .submit(
                AuthRequest::Register {
                    email: "queue@example.com".into(),
                    display_name: "Queue".into(),
                    password: Secret::new("ordinary-password".into()),
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
        let client = owner.send_queue(POLICY).unwrap();
        Self {
            root,
            owner,
            client,
            token: token.expose().into(),
            tenant: user.tenant_id,
        }
    }

    fn reopen(self) -> Self {
        let Self {
            root,
            owner,
            token,
            tenant,
            ..
        } = self;
        owner.shutdown().unwrap();
        let (owner, _) = WriterOwner::open(&root, Limits::default()).unwrap();
        let client = owner.send_queue(POLICY).unwrap();
        Self {
            root,
            owner,
            client,
            token,
            tenant,
        }
    }

    fn apply(&self, request: Request) -> Reply {
        self.client
            .apply(
                Credential::Session(Secret::new(self.token.clone())),
                request,
                &AtomicBool::new(false),
                Duration::from_secs(5),
            )
            .unwrap()
    }

    fn database(&self) -> Connection {
        Connection::open(self.root.join("print-partner.db")).unwrap()
    }

    /// Stores items `(id, printer, state, upload_job_id)` in this order.
    fn seed(&self, items: &[(&str, &str, &str, Option<&str>)]) {
        let connection = self.database();
        for (position, (id, printer, state, job)) in items.iter().enumerate() {
            connection
                .execute(
                    "INSERT INTO printer_send_queue(tenant_id,position,id,filename,artifact_path,printer_id,profile_id,match,wait_for_idle,start,state,checkoff_units,upload_job_id,error,host_name,created_at,updated_at)
                     VALUES(?1,?2,?3,?4,?5,?6,NULL,'pinned',1,0,?7,'[{\"part_id\":4,\"unit_index\":1}]',?8,NULL,NULL,'2026-11-05T01:00:00.000Z','2026-11-05T01:00:00.000Z')",
                    params![
                        self.tenant,
                        position as i64,
                        id,
                        format!("{id}.gcode"),
                        format!("/data/exports/printer-uploads/{id}/{id}.gcode"),
                        printer,
                        state,
                        job,
                    ],
                )
                .unwrap();
        }
    }

    fn item(&self, id: &str) -> (String, Option<String>, Option<String>) {
        self.database()
            .query_row(
                "SELECT state,error,upload_job_id FROM printer_send_queue WHERE tenant_id=?1 AND id=?2",
                params![self.tenant, id],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .unwrap()
    }

    fn job(&self, id: &str) -> (String, Value) {
        let (state, document): (String, String) = self
            .database()
            .query_row(
                "SELECT state,document FROM durable_jobs WHERE id=?1",
                [id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        (state, serde_json::from_str(&document).unwrap())
    }

    fn worker(&self) -> ServerWorkerClient {
        self.owner
            .job_worker_with_policy(
                POLICY,
                WorkerAdmission {
                    kinds: vec![(JobKind::PrinterUpload, 3)],
                    total: 3,
                    per_resource: 1,
                    lease_seconds: 60,
                },
            )
            .unwrap()
    }

    fn send(&self, id: &str) -> Reply {
        self.apply(Request::Dispatch {
            id: id.into(),
            host_name: "Micron".into(),
        })
    }

    /// Dispatches the item and leaves its upload with an unknown outcome.
    fn unknown_outcome(&self, worker: &ServerWorkerClient, id: &str, printer: &str) -> String {
        let job = self.dispatch(id);
        let mut attempt = claim(worker);
        worker
            .update(
                &mut attempt.lease,
                WorkerOperation::BeginEffect(intent(printer)),
            )
            .unwrap();
        worker
            .update(&mut attempt.lease, WorkerOperation::Fail)
            .unwrap();
        job
    }

    fn dispatch(&self, id: &str) -> String {
        match self.apply(Request::Dispatch {
            id: id.into(),
            host_name: "Micron".into(),
        }) {
            Reply::Dispatched { job_id, .. } => job_id,
            other => panic!("dispatch refused: {other:?}"),
        }
    }
}

fn refused(status: u16, detail: &'static str) -> Reply {
    Reply::Refused {
        status,
        detail,
        code: None,
    }
}

fn intent(printer: &str) -> EffectIntent {
    EffectIntent {
        operation: EffectOperation::PrinterUpload,
        basis_hash: "a".repeat(64),
        content_hash: "b".repeat(64),
        target: printer.into(),
    }
}

fn claim(worker: &ServerWorkerClient) -> ClaimedAttempt {
    worker.claim().unwrap().expect("a printer upload to claim")
}

#[test]
fn ticket_t_26_dispatch_claims_an_item_and_enqueues_its_upload_in_one_transaction() {
    let queue = Harness::open();
    queue.seed(&[
        ("q1", "p-moon", "queued", None),
        ("q2", "p-moon", "queued", None),
        ("q3", "p-prusa", "done", Some("job-node")),
    ]);
    let Reply::Dispatched { item, job_id } = queue.apply(Request::Dispatch {
        id: "q1".into(),
        host_name: "Micron".into(),
    }) else {
        panic!("dispatch refused")
    };
    assert_eq!(item["state"], "sending");
    assert_eq!(item["upload_job_id"], json!(job_id));
    assert_eq!(item["host_name"], "Micron");
    assert!(item.get("error").is_none());
    let (state, document) = queue.job(&job_id);
    assert_eq!(state, "queued");
    assert_eq!(
        document["payload"],
        json!({"kind": "printer-upload", "payload": {"printer_id": "p-moon", "artifact_path": "/data/exports/printer-uploads/q1/q1.gcode",
               "filename": "q1.gcode", "start": false, "profile_id": null, "host_name": "Micron",
               "checkoff_units": [{"part_id": 4, "unit_index": 1, "object_name": null}], "unlabeled_names": []}})
    );
    assert_eq!(
        queue.apply(Request::Dispatch {
            id: "q2".into(),
            host_name: "Micron".into()
        }),
        refused(409, "Item changed concurrently")
    );
    assert_eq!(queue.item("q2"), ("queued".into(), None, None));
    for (id, reply) in [
        ("q3", refused(409, "Item is not dispatchable")),
        ("q1", refused(409, "Item is not dispatchable")),
        ("nope", refused(404, "Queue item not found")),
    ] {
        assert_eq!(
            queue.apply(Request::Dispatch {
                id: id.into(),
                host_name: "Micron".into()
            }),
            reply
        );
    }
    let jobs: i64 = queue
        .database()
        .query_row("SELECT COUNT(*) FROM durable_jobs", [], |row| row.get(0))
        .unwrap();
    assert_eq!(jobs, 1);
}

#[test]
fn ticket_t_26_upload_outcomes_settle_the_item_in_the_job_transaction() {
    let queue = Harness::open();
    queue.seed(&[
        ("done", "p-a", "queued", None),
        ("failed", "p-b", "queued", None),
        ("unknown", "p-c", "queued", None),
    ]);
    let worker = queue.worker();

    let job = queue.dispatch("done");
    let mut attempt = claim(&worker);
    assert_eq!(attempt.lease.job_id(), job);
    worker
        .update(
            &mut attempt.lease,
            WorkerOperation::BeginEffect(intent("p-a")),
        )
        .unwrap();
    worker
        .update(
            &mut attempt.lease,
            WorkerOperation::ConfirmEffect(ResultArtifact {
                receipt_id: "upload-1".into(),
                content_hash: "b".repeat(64),
                target: "p-a".into(),
            }),
        )
        .unwrap();
    assert_eq!(queue.item("done").0, "sending");
    worker
        .update(&mut attempt.lease, WorkerOperation::Finish(None))
        .unwrap();
    assert_eq!(queue.item("done"), ("done".into(), None, Some(job)));

    let job = queue.dispatch("failed");
    let mut attempt = claim(&worker);
    worker
        .update(&mut attempt.lease, WorkerOperation::Fail)
        .unwrap();
    assert_eq!(
        queue.item("failed"),
        ("error".into(), Some("Upload failed".into()), Some(job))
    );

    let job = queue.dispatch("unknown");
    let mut attempt = claim(&worker);
    worker
        .update(
            &mut attempt.lease,
            WorkerOperation::BeginEffect(intent("p-c")),
        )
        .unwrap();
    worker
        .update(&mut attempt.lease, WorkerOperation::Fail)
        .unwrap();
    assert_eq!(queue.job(&job).0, "reconciliation_required");
    assert_eq!(
        queue.item("unknown"),
        (
            "error".into(),
            Some(UNKNOWN_OUTCOME_ERROR.into()),
            Some(job)
        )
    );
}

#[test]
fn ticket_t_26_send_now_refuses_an_unknown_outcome_until_the_user_reconciles() {
    let queue = Harness::open();
    queue.seed(&[("q1", "p-a", "queued", None), ("q2", "p-a", "queued", None)]);
    let worker = queue.worker();
    let first = queue.unknown_outcome(&worker, "q1", "p-a");
    // Send now never resends it, the printer stays reserved for every other
    // item, and nothing claims a send on its own.
    assert_eq!(queue.send("q1"), OUTCOME_UNKNOWN);
    assert_eq!(
        queue.apply(Request::Missing { id: "q1".into() }),
        OUTCOME_UNKNOWN
    );
    assert_eq!(
        queue.send("q2"),
        refused(409, "Another send to this printer is still unresolved")
    );
    assert!(worker.claim().unwrap().is_none());

    let Reply::Item(item) = queue.apply(Request::Reconcile {
        id: "q1".into(),
        printed: false,
    }) else {
        panic!("reconcile refused")
    };
    assert_eq!(item["state"], "error");
    assert_eq!(item["error"], NOT_PRINTED);
    let (state, document) = queue.job(&first);
    assert_eq!(state, "failed");
    assert_eq!(
        document["recovery"],
        "You reconciled this send: it did not print"
    );
    let decision: String = queue
        .database()
        .query_row(
            "SELECT decision FROM durable_job_reconciliations WHERE job_id=?1",
            [&first],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(decision, "UserDidNotPrint");
    // The answer alone sends nothing; a new Send now does.
    assert!(worker.claim().unwrap().is_none());
    let second = queue.dispatch("q1");
    assert_ne!(first, second);
    assert_eq!(claim(&worker).lease.job_id(), second);
}

#[test]
fn ticket_t_26_it_printed_finishes_the_item_and_node_interrupted_sends_reconcile_too() {
    let queue = Harness::open();
    queue.seed(&[
        ("q3", "p-b", "queued", None),
        ("node", "p-c", "error", Some("job-node")),
    ]);
    queue
        .database()
        .execute(
            "UPDATE printer_send_queue SET error=?1 WHERE id='node'",
            [INTERRUPTED_ERROR],
        )
        .unwrap();
    let worker = queue.worker();
    let printed = queue.unknown_outcome(&worker, "q3", "p-b");
    let Reply::Item(item) = queue.apply(Request::Reconcile {
        id: "q3".into(),
        printed: true,
    }) else {
        panic!("reconcile refused")
    };
    assert_eq!(
        (item["state"].clone(), item.get("error")),
        (json!("done"), None)
    );
    assert_eq!(queue.job(&printed).0, "succeeded");
    assert_eq!(
        queue.apply(Request::Reconcile {
            id: "q3".into(),
            printed: false,
        }),
        refused(409, "This item has no send with an unknown printer outcome")
    );

    assert_eq!(queue.send("node"), OUTCOME_UNKNOWN);
    assert!(matches!(
        queue.apply(Request::Reconcile {
            id: "node".into(),
            printed: false,
        }),
        Reply::Item(_)
    ));
    assert!(matches!(queue.send("node"), Reply::Dispatched { .. }));
}

#[test]
fn ticket_t_26_remove_cancels_only_waiting_items_and_releases_an_unresolved_upload() {
    let queue = Harness::open();
    queue.seed(&[
        ("waiting", "p-a", "queued", None),
        ("busy", "p-b", "queued", None),
        ("unknown", "p-c", "queued", None),
    ]);
    let worker = queue.worker();
    let Reply::Item(item) = queue.apply(Request::Remove {
        id: "waiting".into(),
    }) else {
        panic!("remove refused")
    };
    assert_eq!(item["state"], "cancelled");

    queue.dispatch("busy");
    assert_eq!(
        queue.apply(Request::Remove { id: "busy".into() }),
        refused(409, "Item not cancellable")
    );
    assert_eq!(
        queue.apply(Request::Remove { id: "nope".into() }),
        refused(404, "Queue item not found")
    );
    let mut attempt = claim(&worker);
    worker
        .update(&mut attempt.lease, WorkerOperation::Fail)
        .unwrap();

    let job = queue.dispatch("unknown");
    let mut attempt = claim(&worker);
    worker
        .update(
            &mut attempt.lease,
            WorkerOperation::BeginEffect(intent("p-c")),
        )
        .unwrap();
    worker
        .update(&mut attempt.lease, WorkerOperation::Fail)
        .unwrap();
    assert!(matches!(
        queue.apply(Request::Remove {
            id: "unknown".into()
        }),
        Reply::Item(_)
    ));
    assert_eq!(
        queue.item("unknown"),
        ("cancelled".into(), None, Some(job.clone()))
    );
    let (state, document) = queue.job(&job);
    assert_eq!(state, "failed");
    assert_eq!(document["recovery"], REMOVED);
}

#[test]
fn ticket_t_26_restart_interrupts_a_dispatched_item_and_never_starts_its_upload() {
    let queue = Harness::open();
    queue.seed(&[
        ("waiting", "p-a", "queued", None),
        ("running", "p-b", "queued", None),
        ("admitted", "p-c", "queued", None),
    ]);
    let worker = queue.worker();
    let running = queue.dispatch("running");
    let _running_attempt = claim(&worker);
    let admitted = queue.dispatch("admitted");
    let mut attempt = claim(&worker);
    worker
        .update(
            &mut attempt.lease,
            WorkerOperation::BeginEffect(intent("p-c")),
        )
        .unwrap();
    let waiting = queue.dispatch("waiting");
    drop(worker);

    let queue = queue.reopen();
    for (id, job, job_state, error) in [
        ("waiting", &waiting, "failed", NOT_STARTED_ERROR),
        ("running", &running, "failed", NOT_STARTED_ERROR),
        (
            "admitted",
            &admitted,
            "reconciliation_required",
            INTERRUPTED_ERROR,
        ),
    ] {
        assert_eq!(
            queue.item(id),
            ("error".into(), Some(error.into()), Some(job.clone())),
            "{id}"
        );
        assert_eq!(queue.job(job).0, job_state, "{id}");
    }
    assert!(queue.worker().claim().unwrap().is_none());
}

#[test]
fn ticket_t_26_reads_and_missing_files_follow_node() {
    let queue = Harness::open();
    queue.seed(&[
        ("a", "p-a", "queued", None),
        ("b", "p-b", "error", Some("job-node")),
        ("c", "p-a", "queued", None),
        ("d", "p-a", "done", None),
    ]);
    let ids = |reply: Reply| match reply {
        Reply::Items(items) => items
            .iter()
            .map(|item| item["id"].as_str().unwrap().to_owned())
            .collect::<Vec<_>>(),
        other => panic!("{other:?}"),
    };
    assert_eq!(
        ids(queue.apply(Request::Ready { printer_id: None })),
        ["a", "c"]
    );
    assert_eq!(
        ids(queue.apply(Request::Ready {
            printer_id: Some("p-b".into())
        })),
        Vec::<String>::new()
    );
    let Reply::Item(item) = queue.apply(Request::Get { id: "b".into() }) else {
        panic!("get refused")
    };
    assert_eq!(item["upload_job_id"], "job-node");
    let Reply::Item(item) = queue.apply(Request::Missing { id: "c".into() }) else {
        panic!("missing refused")
    };
    assert_eq!(
        (item["state"].clone(), item["error"].clone()),
        (json!("error"), json!(MISSING_ARTIFACT))
    );
    assert_eq!(
        queue.apply(Request::Missing { id: "d".into() }),
        refused(409, "Item is not dispatchable")
    );
    // A send Node started has no Rust job to release.
    queue.dispatch("b");
}

#[test]
fn ticket_t_26_a_queue_left_in_the_legacy_setting_cannot_change() {
    let queue = Harness::open();
    queue
        .database()
        .execute(
            "INSERT INTO app_settings(tenant_id,key,value) VALUES(?1,'printer.send_queue','not json')",
            [&queue.tenant],
        )
        .unwrap();
    for request in [
        Request::Get { id: "a".into() },
        Request::Ready { printer_id: None },
        Request::Dispatch {
            id: "a".into(),
            host_name: "Micron".into(),
        },
        Request::Remove { id: "a".into() },
        Request::Missing { id: "a".into() },
        Request::Reconcile {
            id: "a".into(),
            printed: true,
        },
    ] {
        assert_eq!(queue.apply(request), LEGACY);
    }
}
