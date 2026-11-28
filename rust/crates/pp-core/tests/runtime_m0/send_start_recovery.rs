use super::{QUEUE, Queue, UPLOAD, add, create_build};
use crate::send_queue::GCODE;
use crate::watcher_proof::{proof_data, run_in_child, seed_settings, shut_down};
use axum::http::StatusCode;
use pp_testkit::moonraker::fault::{FaultPrinter, Reply};
use rusqlite::{Connection, OpenFlags};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeSet,
    fs,
    path::{Path, PathBuf},
};

const PRINTER: &str = "start-reply-printer";
const START: &str = "/printer/print/start?filename=plate.gcode";
const UNKNOWN: &str = "reconciliation_required";

#[tokio::test]
async fn ticket_t_28_recovery_launcher_never_replays_an_unanswered_print_start() {
    let data = tempfile::tempdir().unwrap();
    run_in_child(
        "send_queue::transition::send_start_recovery::lost_start_in_child",
        data.path(),
    )
    .await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "run by the launcher print-start recovery proof with the rebuild gate set"]
async fn lost_start_in_child() {
    let data = proof_data();
    let printer = FaultPrinter::start_with_read_probes().unwrap();
    seed_printer(&data, &printer);

    let (runtime, queue) = Queue::open(&data, 1).await;
    let build = create_build(&queue.origin, &queue.cookie).await;
    let start = Send::stage(&queue, build, true).await;
    let waiting = Send::stage(&queue, build, false).await;
    assert!(queue.job_ids().await.is_empty());
    assert_effect_count(&printer, 0);

    let job = start.dispatch(&queue).await;
    let admitted = receive_upload(&printer, &data, &job, true);
    printer.accept(Reply::Accepted).unwrap();
    let request = printer.request().unwrap();
    assert_eq!(request.line, format!("POST {START} HTTP/1.1"));
    assert!(request.body.is_empty());
    assert_eq!(printer.received(), 2);
    assert_eq!(printer.accepted(), 1);
    assert_eq!(document(&data, &job), admitted);

    printer.accept(Reply::Lost).unwrap();
    let unknown = queue.settled(&start.id).await;
    let recovered = UnknownStart::record(&data, &job, &admitted, unknown);
    recovered
        .assert_awaiting_decision(&queue, &data, &printer, &start, &waiting)
        .await;
    shut_down(runtime).await;

    let (runtime, queue) = Queue::open(&data, 2).await;
    recovered
        .assert_awaiting_decision(&queue, &data, &printer, &start, &waiting)
        .await;
    let reconcile = start.action("reconcile");
    let (status, stale) = queue
        .call(
            "POST",
            &reconcile,
            Some(json!({"job_id": "not-this-send", "printed": true})),
        )
        .await;
    assert_eq!(status, StatusCode::CONFLICT, "{stale}");
    assert_eq!(stale["code"], "send_changed", "{stale}");
    assert_eq!(document(&data, &job), recovered.document);
    assert_eq!(queue.settled(&start.id).await, recovered.item);

    let answer = json!({"job_id": job, "printed": true});
    let reconciled = queue.ok("POST", &reconcile, Some(answer.clone())).await;
    assert_eq!(reconciled["item"]["state"], "done");
    assert_eq!(reconciled["item"]["upload_job_id"], job);
    assert_eq!(reconciled["item"]["outcome_unknown"], false);
    assert_eq!(
        queue.call("POST", &reconcile, Some(answer)).await.0,
        StatusCode::CONFLICT
    );
    let answered = document(&data, &job);
    assert_eq!(answered["state"], "failed");
    assert_eq!(answered["effects"], admitted["effects"]);
    assert!(answered["public_result"].is_null());
    assert_eq!(decision(&data, &job), "UserPrinted");
    assert_eq!(queue.job_ids().await, BTreeSet::from([job.clone()]));
    assert_effect_count(&printer, 2);
    shut_down(runtime).await;

    let (runtime, queue) = Queue::open(&data, 3).await;
    assert_eq!(queue.settled(&start.id).await, reconciled["item"]);
    assert_eq!(document(&data, &job), answered);
    assert_eq!(decision(&data, &job), "UserPrinted");
    waiting.assert_queued(&queue).await;
    assert_effect_count(&printer, 2);

    let next = waiting.dispatch(&queue).await;
    assert_ne!(next, job);
    let follow_up = receive_upload(&printer, &data, &next, false);
    assert_eq!(printer.accepted(), 2);
    printer.accept(Reply::Accepted).unwrap();
    assert_eq!(queue.settled(&waiting.id).await["state"], "done");
    let finished = document(&data, &next);
    assert_eq!(finished["state"], "succeeded");
    assert_eq!(
        finished["effects"][0]["intent"],
        follow_up["effects"][0]["intent"]
    );
    assert_eq!(finished["effects"][0]["confirmed"], true);
    assert_eq!(queue.job_ids().await, BTreeSet::from([job.clone(), next]));
    assert_eq!(document(&data, &job), answered);
    shut_down(runtime).await;
    assert_effect_count(&printer, 3);
}

fn seed_printer(data: &Path, printer: &FaultPrinter) {
    seed_settings(
        data,
        &[
            ("feature_toggles", json!({"farm": true})),
            (
                "integrations",
                json!([{
                    "id": "start-reply-loopback",
                    "name": "Start reply loopback",
                    "type": "moonraker",
                    "config": {"base_url": printer.url()}
                }]),
            ),
            (
                "printer.fleet",
                json!([{
                    "id": PRINTER,
                    "name": "Start reply replay check",
                    "integration_id": "start-reply-loopback"
                }]),
            ),
        ],
    );
}

struct Send {
    id: String,
    starts_print: bool,
}

impl Send {
    async fn stage(queue: &Queue, build: i64, starts_print: bool) -> Self {
        let item = add(&queue.origin, &queue.cookie, PRINTER, build, starts_print).await;
        assert_eq!(item["start"], starts_print);
        assert_eq!(item["state"], "queued");
        assert!(item["upload_job_id"].is_null());
        Self {
            id: item["id"].as_str().unwrap().to_owned(),
            starts_print,
        }
    }

    fn action(&self, action: &str) -> String {
        format!("{QUEUE}/{}/{action}", self.id)
    }

    async fn dispatch(&self, queue: &Queue) -> String {
        let dispatched = queue.ok("POST", &self.action("dispatch"), None).await;
        let item = &dispatched["item"];
        assert_eq!(item["id"], self.id);
        assert_eq!(item["state"], "sending");
        assert_eq!(item["start"], self.starts_print);
        assert_eq!(item["upload_job_id"], dispatched["job_id"]);
        dispatched["job_id"].as_str().unwrap().to_owned()
    }

    async fn assert_queued(&self, queue: &Queue) {
        let item = queue.settled(&self.id).await;
        assert_eq!(item["state"], "queued");
        assert_eq!(item["start"], self.starts_print);
        assert!(item["upload_job_id"].is_null());
    }
}

struct UnknownStart {
    job: String,
    item: Value,
    document: Value,
    staged: PathBuf,
}

impl UnknownStart {
    fn record(data: &Path, job: &str, admitted: &Value, item: Value) -> Self {
        let saved = document(data, job);
        assert_eq!(saved["state"], UNKNOWN);
        assert_eq!(saved["effects"], admitted["effects"]);
        assert!(saved["public_result"].is_null());
        assert_eq!(item["upload_job_id"], job);
        assert_eq!(item["state"], "error");
        assert_eq!(item["outcome_unknown"], true);
        assert_eq!(item["start"], true);
        Self {
            job: job.into(),
            item,
            staged: staged_path(&saved).to_owned(),
            document: saved,
        }
    }

    async fn assert_awaiting_decision(
        &self,
        queue: &Queue,
        data: &Path,
        printer: &FaultPrinter,
        start: &Send,
        waiting: &Send,
    ) {
        assert_eq!(queue.settled(&start.id).await, self.item);
        assert_eq!(queue.job(&self.job).await["status"], "error");
        super::super::refused_as_unknown(&queue.origin, &queue.cookie, &start.id).await;
        let (status, blocked) = queue.call("POST", &waiting.action("dispatch"), None).await;
        assert_eq!(status, StatusCode::CONFLICT, "{blocked}");
        assert_eq!(
            blocked["detail"],
            "Another send to this printer is still unresolved"
        );
        assert_eq!(queue.drain(Some(PRINTER)).await, json!({"results": []}));
        waiting.assert_queued(queue).await;
        assert_eq!(queue.job_ids().await, BTreeSet::from([self.job.clone()]));
        assert_eq!(document(data, &self.job), self.document);
        assert_eq!(fs::read(&self.staged).unwrap(), GCODE.as_bytes());
        assert_effect_count(printer, 2);
    }
}

fn receive_upload(printer: &FaultPrinter, data: &Path, job: &str, start: bool) -> Value {
    let upload = printer.request().unwrap();
    assert_eq!(upload.line, format!("POST {UPLOAD} HTTP/1.1"));
    assert_eq!(
        upload
            .body
            .windows(GCODE.len())
            .filter(|part| *part == GCODE.as_bytes())
            .count(),
        1
    );
    let saved = document(data, job);
    assert_eq!(saved["job_id"], job);
    assert_eq!(saved["state"], "effect_admitted");
    assert_eq!(saved["payload"]["payload"]["start"], start);
    let [effect] = saved["effects"].as_array().unwrap().as_slice() else {
        panic!("expected one admitted effect: {saved}");
    };
    assert_eq!(
        effect["intent"]["operation"],
        if start {
            "printer_upload_and_start"
        } else {
            "printer_upload"
        }
    );
    let staged = fs::read(staged_path(&saved)).unwrap();
    assert_eq!(staged, GCODE.as_bytes());
    let admitted = &effect["intent"];
    assert_eq!(
        (
            admitted["target"].as_str(),
            admitted["content_hash"].as_str()
        ),
        (
            Some(PRINTER),
            Some(hex::encode(Sha256::digest(staged)).as_str())
        )
    );
    assert_eq!(effect["confirmed"], false);
    assert!(effect["receipt"].is_null());
    saved
}

fn staged_path(job: &Value) -> &Path {
    Path::new(job["payload"]["payload"]["artifact_path"].as_str().unwrap())
}

fn stores(data: &Path) -> Connection {
    Connection::open_with_flags(
        data.join("print-partner.db"),
        OpenFlags::SQLITE_OPEN_READ_ONLY,
    )
    .unwrap()
}

fn document(data: &Path, job: &str) -> Value {
    let text: String = stores(data)
        .query_row(
            "SELECT document FROM durable_jobs WHERE id=?1",
            [job],
            |row| row.get(0),
        )
        .unwrap();
    serde_json::from_str(&text).unwrap()
}

fn decision(data: &Path, job: &str) -> String {
    let decisions = stores(data)
        .prepare("SELECT decision FROM durable_job_reconciliations WHERE job_id=?1")
        .unwrap()
        .query_map([job], |row| row.get::<_, String>(0))
        .unwrap()
        .collect::<rusqlite::Result<Vec<_>>>()
        .unwrap();
    let [only]: [String; 1] = decisions.try_into().unwrap();
    only
}

fn assert_effect_count(printer: &FaultPrinter, effects: usize) {
    assert_eq!(printer.received(), effects, "no extra mutation requests");
    assert_eq!(
        printer.accepted(),
        effects,
        "counting accepted fake effects"
    );
}
