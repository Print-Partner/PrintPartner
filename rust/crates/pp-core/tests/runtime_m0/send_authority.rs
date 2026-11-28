use super::super::{GCODE, QUEUE, UPLOAD, create_build, send_now};
use super::send_shutdown::job_document;
use super::{Queue, item_id, queue_runtime};
use crate::watcher_proof::{proof_data, run_in_child, seed_settings, shut_down, signed_in};
use axum::http::StatusCode;
use pp_core::CoreRuntime;
use pp_testkit::moonraker::fault::{FaultPrinter, Reply};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{fs, path::Path};

const PRINTER: &str = "original-authority-printer";
const UNKNOWN: &str = "reconciliation_required";

#[tokio::test]
async fn runtime_preserves_observed_upload_after_dispatch_session_is_revoked() {
    let data = tempfile::tempdir().unwrap();
    run_in_child(
        "send_queue::transition::send_authority::revoked_session_in_child",
        data.path(),
    )
    .await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "run by the original-authority proof with the rebuild gate set"]
async fn revoked_session_in_child() {
    let data = proof_data();
    let printer = FaultPrinter::start_with_read_probes().unwrap();
    seed_printer(&data, &printer);
    let (runtime, dispatch, observer) = open_server_with_two_sessions(&data).await;
    let build = create_build(&observer.origin, &observer.cookie).await;
    let item = observer.add(PRINTER, build).await;
    let waiting = observer.add(PRINTER, build).await;
    let item = item_id(&item);
    let waiting = item_id(&waiting);
    assert!(observer.job_ids().await.is_empty());
    assert_eq!(printer.received(), 0);
    assert_eq!(printer.accepted(), 0);

    let job = send_now(&dispatch.origin, &dispatch.cookie, item).await;
    let request = printer.request().unwrap();
    assert_eq!(request.line, format!("POST {UPLOAD} HTTP/1.1"));
    assert!(
        request
            .body
            .windows(GCODE.len())
            .any(|bytes| bytes == GCODE.as_bytes())
    );
    let admitted = Admitted::inspect(&data, &job);
    assert_eq!(printer.accepted(), 0, "inspection precedes the fake effect");
    printer.accept(Reply::Hold).unwrap();
    assert_eq!(printer.accepted(), 1);

    assert_eq!(
        dispatch.ok("POST", "/auth/logout", None).await,
        json!({"ok": true})
    );
    assert_eq!(
        dispatch.call("GET", QUEUE, None).await.0,
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(observer.job_ids().await, [job.clone()].into());
    assert_eq!(job_document(&data, &job)["state"], "effect_admitted");
    assert!(job_document(&data, &job)["effects"][0]["receipt"].is_null());
    printer.accept(Reply::Accepted).unwrap();
    let unknown_item = observer.settled(item).await;
    assert_eq!(unknown_item["state"], "error");
    assert_eq!(unknown_item["upload_job_id"], job);
    assert_eq!(unknown_item["outcome_unknown"], true);
    let unknown_job = observer.job(&job).await;
    assert_eq!(unknown_job["status"], "error");
    assert!(unknown_job["result"].is_null());
    let saved = admitted.observed(&data, &job);
    let (status, _) = dispatch
        .call(
            "POST",
            &format!("{QUEUE}/{item}/reconcile"),
            Some(json!({"job_id": job, "printed": true})),
        )
        .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert_eq!(printer.received(), 1);
    assert_eq!(printer.accepted(), 1);
    shut_down(runtime).await;

    let (runtime, queue) = Queue::open(&data, 2).await;
    assert_eq!(queue.settled(item).await, unknown_item);
    assert_eq!(queue.job(&job).await, unknown_job);
    assert_eq!(admitted.observed(&data, &job), saved);
    assert_blocked(&queue, item, waiting).await;
    assert_eq!(printer.received(), 1);

    let reconcile = format!("{QUEUE}/{item}/reconcile");
    let (status, problem) = queue
        .call(
            "POST",
            &reconcile,
            Some(json!({"job_id": "different-job", "printed": true})),
        )
        .await;
    assert_eq!(status, StatusCode::CONFLICT, "{problem}");
    assert_eq!(problem["code"], "send_changed");
    assert_eq!(admitted.observed(&data, &job), saved);
    assert_eq!(queue.settled(item).await, unknown_item);

    let answer = json!({"job_id": job, "printed": true});
    let settled = queue.ok("POST", &reconcile, Some(answer.clone())).await;
    assert_eq!(settled["item"]["state"], "done");
    assert_eq!(settled["item"]["upload_job_id"], job);
    assert_eq!(settled["item"]["outcome_unknown"], false);
    let done = queue.job(&job).await;
    assert_eq!(done["status"], "done");
    assert_eq!(queue.job_ids().await, [job.clone()].into());
    assert_eq!(
        queue.call("POST", &reconcile, Some(answer)).await.0,
        StatusCode::CONFLICT
    );
    shut_down(runtime).await;
    assert_reconciled(&data, &job, &saved);

    let (runtime, queue) = Queue::open(&data, 2).await;
    assert_eq!(queue.settled(item).await, settled["item"]);
    assert_eq!(queue.job(&job).await, done);
    assert_eq!(queue.job_ids().await, [job.clone()].into());
    let waiting_item = queue.settled(waiting).await;
    assert_eq!(waiting_item["state"], "queued");
    assert!(waiting_item["upload_job_id"].is_null());
    shut_down(runtime).await;
    assert_reconciled(&data, &job, &saved);
    assert!(
        !data
            .join(pp_gateway::ownership::cutover::CUTOVER_FILE)
            .exists()
    );
    assert_eq!(printer.received(), 1, "never replay or start a print");
    assert_eq!(printer.accepted(), 1);
}

async fn open_server_with_two_sessions(data: &Path) -> (CoreRuntime, Queue, Queue) {
    let (runtime, desktop) = queue_runtime(data, true).await;
    let origin = runtime.origin().to_owned();
    let original = signed_in(&origin, &desktop, 1).await;
    let other = signed_in(&origin, &desktop, 2).await;
    assert!(
        original != other,
        "the account must have two distinct sessions"
    );
    let dispatch = Queue {
        origin: origin.clone(),
        cookie: original,
    };
    let observer = Queue {
        origin,
        cookie: other,
    };
    (runtime, dispatch, observer)
}

fn seed_printer(data: &Path, printer: &FaultPrinter) {
    seed_settings(
        data,
        &[
            ("feature_toggles", json!({"farm": true})),
            (
                "integrations",
                json!([{
                    "id": "authority-host",
                    "name": "Authority Host",
                    "type": "moonraker",
                    "config": {"base_url": printer.url()},
                }]),
            ),
            (
                "printer.fleet",
                json!([{"id": PRINTER, "name": PRINTER, "integration_id": "authority-host"}]),
            ),
        ],
    );
}

async fn assert_blocked(queue: &Queue, item: &str, waiting: &str) {
    for id in [item, waiting] {
        let (status, problem) = queue
            .call("POST", &format!("{QUEUE}/{id}/dispatch"), None)
            .await;
        assert_eq!(status, StatusCode::CONFLICT, "{problem}");
    }
    assert_eq!(queue.drain(None).await, json!({"results": []}));
}

struct Admitted {
    intent: Value,
    generation: i64,
}

impl Admitted {
    fn inspect(data: &Path, id: &str) -> Self {
        let job = job_document(data, id);
        assert_eq!(job["job_id"], id);
        assert_eq!(job["state"], "effect_admitted");
        let payload = &job["payload"]["payload"];
        assert_eq!(payload["start"], false);
        assert_staged_bytes(&job);
        assert!(job["_authority_refusal"].is_null());
        let effect = only_effect(&job);
        assert_eq!(effect["confirmed"], false);
        assert!(effect["receipt"].is_null());
        let intent = effect["intent"].clone();
        assert_eq!(intent["operation"], "printer_upload");
        assert_eq!(intent["target"], PRINTER);
        assert_eq!(intent["content_hash"], hex::encode(Sha256::digest(GCODE)));
        Self {
            intent,
            generation: job["generation"].as_i64().unwrap(),
        }
    }

    fn observed(&self, data: &Path, id: &str) -> Value {
        let job = job_document(data, id);
        assert_eq!(job["job_id"], id);
        assert_eq!(job["state"], UNKNOWN);
        assert_staged_bytes(&job);
        assert_eq!(job["generation"], self.generation + 1);
        assert!(job["lease_until"].is_null());
        let refusal = &job["_authority_refusal"];
        assert_eq!(refusal["reason"], "credential_invalid");
        assert_eq!(refusal["phase"], "worker_advance");
        assert_eq!(refusal["generation"], self.generation);
        assert!(job["result"].is_null());
        let effect = only_effect(&job);
        assert_eq!(effect["intent"], self.intent);
        assert_eq!(effect["confirmed"], false);
        assert_eq!(
            effect["receipt"],
            json!({
                "receipt_id": "plate.gcode",
                "content_hash": self.intent["content_hash"],
                "target": PRINTER,
            })
        );
        effect["receipt"].clone()
    }
}

fn assert_staged_bytes(job: &Value) {
    let path = job["payload"]["payload"]["artifact_path"].as_str().unwrap();
    assert_eq!(fs::read(path).unwrap(), GCODE.as_bytes());
}

fn only_effect(job: &Value) -> &Value {
    let effects = job["effects"].as_array().unwrap();
    assert_eq!(effects.len(), 1);
    &effects[0]
}

fn assert_reconciled(data: &Path, id: &str, saved: &Value) {
    let job = job_document(data, id);
    assert_eq!(job["job_id"], id);
    assert_eq!(job["state"], "succeeded");
    assert_eq!(job["result"], *saved);
    let effect = only_effect(&job);
    assert_eq!(effect["confirmed"], true);
    assert_eq!(effect["receipt"], *saved);
    let db = rusqlite::Connection::open_with_flags(
        data.join("print-partner.db"),
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
    )
    .unwrap();
    let (count, decision, receipt): (u32, String, String) = db
        .query_row(
            "SELECT count(*), decision, receipt FROM durable_job_reconciliations WHERE job_id=?1",
            [id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .unwrap();
    assert_eq!(count, 1);
    assert_eq!(decision, "UserPrinted");
    assert_eq!(serde_json::from_str::<Value>(&receipt).unwrap(), *saved);
}
