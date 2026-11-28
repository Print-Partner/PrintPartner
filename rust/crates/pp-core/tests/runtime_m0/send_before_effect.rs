use super::{
    GCODE, Printer, QUEUE, Queue, UPLOAD, create_build, item_id, job_document, proof_data,
    run_in_child,
};
use crate::watcher_proof::{seed_settings, shut_down};
use axum::http::StatusCode;
use pp_core::rebuild_api::desktop_policy;
use pp_storage::{
    Limits, WriterOwner,
    auth::Secret,
    jobs::{JobKind, PersistentState, WorkerAdmission},
    read_model::Credential,
    send_queue::{
        NOT_STARTED_ERROR,
        writes::{Reply, Request, SendQueueClient},
    },
};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{collections::BTreeSet, fs, path::Path, sync::atomic::AtomicBool, time::Duration};

const CLAIMED: &str = "p-claimed";
const QUEUED: &str = "p-unclaimed";
const ACTORS: [&str; 2] = [CLAIMED, QUEUED];
const WAIT: Duration = Duration::from_secs(10);

#[tokio::test]
async fn launcher_recovers_printer_jobs_that_never_admitted_an_effect() {
    let data = tempfile::tempdir().unwrap();
    run_in_child(
        "send_queue::transition::send_shutdown::send_before_effect::recovery_in_child",
        data.path(),
    )
    .await;
}

#[tokio::test]
#[ignore = "run by the pre-effect recovery proof with the rebuild gate set"]
async fn recovery_in_child() {
    let data = proof_data();
    let mut printer = Printer::start().await;
    seed_farm(&data, &printer.url);
    let (runtime, queue) = Queue::open(&data, 1).await;
    let build = create_build(&queue.origin, &queue.cookie).await;
    let mut staged = Vec::new();
    for printer_id in ACTORS {
        let item = queue.add(printer_id, build).await;
        assert_eq!(item["state"], "queued");
        assert_eq!(item["start"], false);
        assert_eq!(item["printer_id"], printer_id);
        assert!(item["upload_job_id"].is_null());
        staged.push(item);
    }
    assert!(queue.job_ids().await.is_empty());
    shut_down(runtime).await;
    assert!(printer.mutations().is_empty());

    let [claimed, queued] = prepare_interruption(&data, &queue.cookie, &staged);
    let jobs = BTreeSet::from([claimed.job.clone(), queued.job.clone()]);
    assert_eq!(jobs.len(), 2);
    for (item, state, attempt) in [(&claimed, "running", 1), (&queued, "queued", 0)] {
        let before = job_document(&data, &item.job);
        assert_eq!(before["state"], state, "{before}");
        assert_eq!(before["attempt"], attempt, "{before}");
        assert_eq!(before["effects"], json!([]), "{before}");
    }
    assert!(printer.mutations().is_empty());
    let files = [claimed.artifact.as_str(), queued.artifact.as_str()];
    for artifact in files {
        assert_eq!(fs::read(artifact).unwrap(), GCODE.as_bytes());
    }

    let (runtime, queue) = Queue::open(&data, 2).await;
    let claimed_failure = claimed.assert_not_started(&queue).await;
    let queued_failure = queued.assert_not_started(&queue).await;
    assert_eq!(queue.job_ids().await, jobs);
    assert_eq!(queue.drain(None).await, json!({"results": []}));
    shut_down(runtime).await;
    assert!(printer.mutations().is_empty());
    let recovered = [&claimed, &queued].map(|item| {
        let saved = job_document(&data, &item.job);
        assert_eq!(saved["state"], "failed", "{saved}");
        assert_eq!(saved["recovery"], NOT_STARTED_ERROR, "{saved}");
        assert_eq!(saved["effects"], json!([]), "{saved}");
        assert_eq!(fs::read(&item.artifact).unwrap(), GCODE.as_bytes());
        saved
    });

    let (runtime, queue) = Queue::open(&data, 2).await;
    assert_eq!(claimed.assert_not_started(&queue).await, claimed_failure);
    assert_eq!(queued.assert_not_started(&queue).await, queued_failure);
    assert!(printer.mutations().is_empty());
    let mut receipts = Vec::new();
    for item in [&claimed, &queued] {
        let job = queue.dispatch_fixture(&item.id).await;
        assert!(!jobs.contains(&job));
        printer.next_upload().await.send(()).unwrap();
        let done = queue.settled(&item.id).await;
        assert_eq!(done["state"], "done", "{done}");
        assert_eq!(done["upload_job_id"], job);
        assert_eq!(done["outcome_unknown"], false);
        let receipt = queue.job(&job).await;
        assert_eq!(receipt["status"], "done");
        assert_eq!(receipt["job_id"], job);
        assert_eq!(receipt["kind"], "printer-upload");
        assert_eq!(receipt["result"]["printer_id"], item.printer);
        assert_eq!(receipt["result"]["started"], false);
        receipts.push((job, receipt, done));
    }
    assert_eq!(queue.job(&claimed.job).await, claimed_failure);
    assert_eq!(queue.job(&queued.job).await, queued_failure);
    let mut expected_jobs = jobs;
    expected_jobs.extend(receipts.iter().map(|(id, _, _)| id.clone()));
    assert_eq!(expected_jobs.len(), 4);
    assert_eq!(queue.job_ids().await, expected_jobs);
    shut_down(runtime).await;
    assert_eq!(printer.mutations(), [UPLOAD, UPLOAD]);
    for (source, (job, _, _)) in [&claimed, &queued].into_iter().zip(&receipts) {
        let persisted = job_document(&data, job);
        let effects = persisted["effects"].as_array().unwrap();
        assert_eq!(effects.len(), 1, "{persisted}");
        let effect = &effects[0];
        assert_eq!(effect["intent"]["operation"], "printer_upload");
        assert_eq!(effect["intent"]["target"], source.printer);
        assert_eq!(
            effect["intent"]["content_hash"],
            hex::encode(Sha256::digest(GCODE))
        );
        assert_eq!(effect["confirmed"], true);
    }
    for artifact in files {
        assert!(!Path::new(artifact).exists(), "{artifact}");
    }

    let (runtime, queue) = Queue::open(&data, 2).await;
    for (job, receipt, item) in receipts {
        assert_eq!(queue.job(&job).await, receipt);
        assert_eq!(queue.item(item_id(&item)).await, item);
    }
    assert_eq!(queue.job(&claimed.job).await, claimed_failure);
    assert_eq!(queue.job(&queued.job).await, queued_failure);
    assert_eq!(queue.drain(None).await, json!({"results": []}));
    assert_eq!(queue.job_ids().await, expected_jobs);
    shut_down(runtime).await;
    assert_eq!(printer.mutations(), [UPLOAD, UPLOAD]);
    for (item, old_job) in [&claimed, &queued].into_iter().zip(recovered) {
        assert_eq!(job_document(&data, &item.job), old_job);
    }
}

fn seed_farm(data: &Path, url: &str) {
    let integrations = ACTORS.map(|printer| {
        json!({"id": printer, "name": printer, "type": "moonraker", "config": {"base_url": url}})
    });
    let fleet =
        ACTORS.map(|printer| json!({"id": printer, "name": printer, "integration_id": printer}));
    seed_settings(
        data,
        &[
            ("feature_toggles", json!({"farm": true})),
            ("integrations", json!(integrations)),
            ("printer.fleet", json!(fleet)),
        ],
    );
}

struct Unadmitted {
    id: String,
    job: String,
    printer: String,
    artifact: String,
}

impl Unadmitted {
    async fn assert_not_started(&self, owner: &Queue) -> Value {
        let item = owner.item(&self.id).await;
        assert_eq!(item["state"], "error", "{item}");
        assert_eq!(item["error"], NOT_STARTED_ERROR);
        assert_eq!(item["outcome_unknown"], false);
        assert_eq!(item["upload_job_id"], self.job);
        assert_eq!(item["printer_id"], self.printer);

        let path = format!("{QUEUE}/{}/reconcile", self.id);
        let (status, problem) = owner
            .call(
                "POST",
                &path,
                Some(json!({"job_id": self.job, "printed": false})),
            )
            .await;
        assert_eq!(status, StatusCode::CONFLICT, "{problem}");
        assert_eq!(owner.item(&self.id).await, item);
        let job = owner.job(&self.job).await;
        assert_eq!(job["job_id"], self.job);
        assert_eq!(job["status"], "error");
        assert!(!job["finished_at"].is_null(), "{job}");
        assert!(job["result"].is_null(), "{job}");
        job
    }
}

fn prepare_interruption(data: &Path, cookies: &str, staged: &[Value]) -> [Unadmitted; 2] {
    let token = cookies
        .split("; ")
        .find_map(|cookie| cookie.strip_prefix("pp_session="))
        .expect("the disposable launcher issued an account session");
    let policy = desktop_policy();
    let (owner, _) = WriterOwner::open_with_job_feature_admission(data, Limits::default()).unwrap();
    let queue = owner.send_queue(policy).unwrap();
    let worker = owner
        .job_worker_with_policy(
            policy,
            WorkerAdmission {
                kinds: vec![(JobKind::PrinterUpload, 1)],
                total: 1,
                per_resource: 1,
                lease_seconds: 60,
            },
        )
        .unwrap();
    let claimed = dispatch_through_writer(&queue, token, &staged[0]);
    let attempt = worker.claim().unwrap().unwrap();
    assert_eq!(attempt.job.job_id, claimed.job);
    assert_eq!(attempt.job.state, PersistentState::Running);
    assert_eq!(attempt.job.attempt, 1);
    assert!(attempt.job.effects.is_empty());
    let queued = dispatch_through_writer(&queue, token, &staged[1]);
    owner.shutdown().unwrap();
    [claimed, queued]
}

fn dispatch_through_writer(queue: &SendQueueClient, token: &str, staged: &Value) -> Unadmitted {
    let id = item_id(staged).to_owned();
    let printer = staged["printer_id"].as_str().unwrap().to_owned();
    let Reply::Item(stored) = queue_request(queue, token, Request::Get { id: id.clone() }) else {
        panic!("staged queue item was not found");
    };
    let artifact = stored["artifact_path"].as_str().unwrap().to_owned();
    let reply = queue_request(
        queue,
        token,
        Request::Dispatch {
            id: id.clone(),
            host_name: printer.clone(),
        },
    );
    let Reply::Dispatched { item, job_id } = reply else {
        panic!("storage did not dispatch {id}: {reply:?}");
    };
    assert_eq!(item["state"], "sending");
    assert_eq!(item["upload_job_id"], job_id);
    assert_eq!(item["printer_id"], printer);
    Unadmitted {
        id,
        job: job_id,
        printer,
        artifact,
    }
}

fn queue_request(queue: &SendQueueClient, token: &str, request: Request) -> Reply {
    queue
        .apply(
            Credential::Session(Secret::new(token.to_owned())),
            request,
            &AtomicBool::new(false),
            WAIT,
        )
        .unwrap()
}
