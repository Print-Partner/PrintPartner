use super::{Printers, QUEUE, UPLOAD, add, create_build, post_file, sends, settled};
use crate::watcher_proof::{proof_data, run_in_child, shut_down, signed_in};
use crate::{desktop_cookie, launch, request};
use axum::http::StatusCode;
use pp_core::CoreRuntime;
use serde_json::{Value, json};
use std::{collections::BTreeSet, path::Path, time::Duration};
use tokio::time::timeout;

#[path = "send_shutdown.rs"]
mod send_shutdown;

#[path = "send_self_host_exports.rs"]
mod send_self_host_exports;

#[tokio::test]
async fn ticket_t_28_recovery_printer_queue_uses_one_rust_owner() {
    let data = tempfile::tempdir().unwrap();
    run_in_child("send_queue::transition::boundary_in_child", data.path()).await;
}

#[tokio::test]
#[ignore = "run by the printer queue transition proof with the rebuild gate set"]
async fn boundary_in_child() {
    let data = proof_data();
    let printers = Printers::start();
    printers.seed(&data);
    let (runtime, queue) = Queue::open(&data, 1).await;
    let build = create_build(&queue.origin, &queue.cookie).await;
    let cancelled = item_id(&queue.add("p-accepting", build).await).to_owned();
    let accepted = item_id(&queue.add("p-accepting", build).await).to_owned();
    let lost = item_id(&queue.add("p-silent", build).await).to_owned();
    assert!(queue.job_ids().await.is_empty());
    assert_eq!(printers.sends(), [0, 0, 0]);

    queue.legacy_routes_are_absent(&accepted, build).await;
    assert_eq!(printers.sends(), [0, 0, 0]);
    let removed = queue
        .ok("DELETE", &format!("{QUEUE}/{cancelled}"), None)
        .await;
    assert_eq!(removed["item"]["state"], "cancelled");
    assert!(removed["item"]["upload_job_id"].is_null());
    assert!(queue.job_ids().await.is_empty());

    let drained = queue.drain(Some("p-accepting")).await;
    let accepted_job = drained["results"][0]["job_id"].as_str().unwrap();
    assert_eq!(
        drained,
        json!({"results": [{"item_id": accepted, "job_id": accepted_job}]})
    );
    let done = queue.settled(&accepted).await;
    assert_eq!(done["state"], "done", "{done}");
    assert_eq!(done["upload_job_id"], accepted_job, "{done}");
    assert_eq!(printers.sends(), [1, 0, 0]);
    assert_eq!(queue.job(accepted_job).await["status"], "done");
    let (status, _) = queue
        .call("DELETE", &format!("{QUEUE}/{accepted}"), None)
        .await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(queue.settled(&accepted).await, done);

    let dispatched = queue
        .ok("POST", &format!("{QUEUE}/{lost}/dispatch"), None)
        .await;
    let lost_job = dispatched["job_id"].as_str().unwrap();
    assert_eq!(dispatched["item"]["upload_job_id"], lost_job);
    let unknown = queue.settled(&lost).await;
    assert_eq!(unknown["state"], "error", "{unknown}");
    assert_eq!(unknown["outcome_unknown"], true, "{unknown}");
    assert_eq!(unknown["upload_job_id"], lost_job);
    let unknown_job = queue.job(lost_job).await;
    assert_eq!(unknown_job["status"], "error");
    assert_eq!(
        queue.job_ids().await,
        BTreeSet::from([accepted_job.to_owned(), lost_job.to_owned()])
    );
    let upload = format!("POST {UPLOAD} HTTP/1.1");
    let expected = std::slice::from_ref(&upload);
    assert_eq!(sends(printers.accepting.request_lines()), expected);
    assert_eq!(
        sends(printers.silent.lines.lock().unwrap().clone()),
        expected
    );
    shut_down(runtime).await;

    let (runtime, queue) = Queue::open(&data, 2).await;
    assert_eq!(queue.settled(&lost).await, unknown);
    assert_eq!(queue.job(lost_job).await, unknown_job);
    queue.legacy_routes_are_absent(&lost, build).await;
    super::refused_as_unknown(&queue.origin, &queue.cookie, &lost).await;
    assert_eq!(queue.drain(None).await, json!({"results": []}));
    assert_eq!(printers.sends(), [1, 1, 0]);

    let reconcile = format!("{QUEUE}/{lost}/reconcile");
    let (status, stale) = queue
        .call(
            "POST",
            &reconcile,
            Some(json!({"job_id": accepted_job, "printed": true})),
        )
        .await;
    assert_eq!(status, StatusCode::CONFLICT, "{stale}");
    assert_eq!(stale["code"], "send_changed", "{stale}");
    assert_eq!(queue.settled(&lost).await, unknown);
    assert_eq!(queue.job(lost_job).await, unknown_job);
    let answer = json!({"job_id": lost_job, "printed": true});
    let receipt = queue.ok("POST", &reconcile, Some(answer.clone())).await;
    assert_eq!(receipt["item"]["state"], "done");
    assert_eq!(receipt["item"]["upload_job_id"], lost_job);
    assert_eq!(receipt["item"]["outcome_unknown"], false);
    let answered = queue.job(lost_job).await;
    assert_eq!(answered["status"], "error");
    assert!(answered["result"].is_null(), "{answered}");
    assert_ne!(answered, unknown_job);
    assert_eq!(
        queue.call("POST", &reconcile, Some(answer)).await.0,
        StatusCode::CONFLICT
    );
    assert_eq!(queue.drain(None).await, json!({"results": []}));
    shut_down(runtime).await;

    let (runtime, queue) = Queue::open(&data, 3).await;
    assert_eq!(queue.settled(&lost).await, receipt["item"]);
    assert_eq!(queue.job(lost_job).await, answered);
    assert_eq!(queue.drain(None).await, json!({"results": []}));
    assert_eq!(
        queue.job_ids().await,
        BTreeSet::from([accepted_job.to_owned(), lost_job.to_owned()])
    );
    shut_down(runtime).await;
    assert_eq!(printers.sends(), [1, 1, 0], "nothing was started or resent");
}

fn item_id(item: &Value) -> &str {
    item["id"].as_str().unwrap()
}

struct Queue {
    origin: String,
    cookie: String,
}

impl Queue {
    async fn open(data: &Path, start: u32) -> (CoreRuntime, Self) {
        let mut input = launch(data.to_owned());
        input.bundle.node = data.join("no-node-executable");
        let mut runtime = CoreRuntime::start_for_launcher(input).await.unwrap();
        assert!(runtime.rust_only());
        assert!(runtime.handle().compat_status().is_none());
        let origin = runtime.origin().to_owned();
        let desktop = desktop_cookie(&mut runtime).await;
        let cookie = signed_in(&origin, &desktop, start).await;
        (runtime, Self { origin, cookie })
    }

    async fn call(&self, method: &str, path: &str, body: Option<Value>) -> (StatusCode, Value) {
        let (status, _, bytes) = timeout(
            Duration::from_secs(10),
            request(
                &self.origin,
                path,
                method,
                Some(&self.cookie),
                None,
                None,
                body,
            ),
        )
        .await
        .unwrap_or_else(|_| panic!("{method} {path} timed out"));
        (
            status,
            serde_json::from_slice(&bytes).unwrap_or(Value::Null),
        )
    }

    async fn ok(&self, method: &str, path: &str, body: Option<Value>) -> Value {
        let (status, value) = self.call(method, path, body).await;
        assert_eq!(status, StatusCode::OK, "{method} {path}: {value}");
        value
    }

    async fn add(&self, printer: &str, build: i64) -> Value {
        add(&self.origin, &self.cookie, printer, build, false).await
    }

    async fn settled(&self, id: &str) -> Value {
        settled(&self.origin, &self.cookie, id).await
    }

    async fn drain(&self, printer: Option<&str>) -> Value {
        let body = printer.map(|id| json!({"printer_id": id}));
        self.ok("POST", &format!("{QUEUE}/drain"), body).await
    }

    async fn job(&self, id: &str) -> Value {
        let job = self.ok("GET", &format!("/api/v1/jobs/{id}"), None).await;
        assert_eq!(job["job_id"], id);
        assert_eq!(job["kind"], "printer-upload");
        job
    }

    async fn job_ids(&self) -> BTreeSet<String> {
        self.ok("GET", "/api/v1/jobs", None).await["jobs"]
            .as_array()
            .unwrap()
            .iter()
            .map(|job| job["job_id"].as_str().unwrap().to_owned())
            .collect()
    }

    async fn legacy_routes_are_absent(&self, id: &str, build: i64) {
        let before = self.ok("GET", QUEUE, None).await;
        let jobs = self.job_ids().await;
        for prefix in ["", "/api/v1"] {
            let queue = format!("{prefix}/printer-send-queue");
            let direct = format!("{prefix}/jobs/printer-upload");
            for path in [&queue, &direct] {
                let response = post_file(
                    &self.origin,
                    &self.cookie,
                    path,
                    "p-accepting",
                    build,
                    false,
                )
                .await;
                assert_unmounted("POST", path, response.status());
            }
            for (method, path) in [
                ("GET", queue.clone()),
                ("POST", format!("{queue}/{id}/dispatch")),
                ("POST", format!("{queue}/drain")),
                ("DELETE", format!("{queue}/{id}")),
            ] {
                let (status, _) = self.call(method, &path, None).await;
                assert_unmounted(method, &path, status);
            }
        }
        assert_eq!(self.ok("GET", QUEUE, None).await, before);
        assert_eq!(self.job_ids().await, jobs);
    }
}

fn assert_unmounted(method: &str, path: &str, status: StatusCode) {
    assert!(
        matches!(
            status,
            StatusCode::NOT_FOUND | StatusCode::METHOD_NOT_ALLOWED
        ),
        "{method} {path}: expected no handler, got {status}",
    );
}
