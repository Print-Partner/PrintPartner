use super::super::{GCODE, refused_as_unknown, seed_settings, send_now};
use super::{QUEUE, Queue, UPLOAD, create_build, item_id, proof_data, run_in_child};
use axum::{
    Router,
    body::Bytes,
    extract::State,
    http::{Method, StatusCode, Uri},
    response::{IntoResponse, Response},
};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeSet,
    path::Path,
    sync::{Arc, Mutex},
    time::Duration,
};
use tokio::{
    sync::{mpsc, oneshot},
    task::JoinHandle,
    time::timeout,
};

const PRINTER: &str = "p-shutdown";
const DEADLINE: Duration = Duration::from_secs(15);

#[path = "send_before_effect.rs"]
mod send_before_effect;

#[tokio::test]
async fn ticket_t_28_recovery_launcher_shutdown_does_not_replay_an_admitted_upload() {
    let data = tempfile::tempdir().unwrap();
    run_in_child(
        "send_queue::transition::send_shutdown::shutdown_in_child",
        data.path(),
    )
    .await;
}

#[tokio::test]
#[ignore = "run by the Rust-only printer shutdown proof"]
async fn shutdown_in_child() {
    let data = proof_data();
    let mut printer = Printer::start().await;
    printer.seed(&data);

    let (runtime, queue) = Queue::open(&data, 1).await;
    let build = create_build(&queue.origin, &queue.cookie).await;
    let completed = queue.add_fixture(build).await;
    let completed_job = queue.dispatch_fixture(&completed).await;
    let acknowledge = printer.next_upload().await;
    assert_admitted(&data, &completed_job);

    let handle = runtime.handle();
    let stopping = tokio::spawn(runtime.shutdown());
    timeout(DEADLINE, async {
        while handle.track_shutdown_work().is_ok() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("launcher never closed admission");
    acknowledge.send(()).unwrap();
    stopped(stopping).await;
    let acknowledged = job_document(&data, &completed_job);
    assert_eq!(acknowledged["state"], "succeeded");
    assert_eq!(acknowledged["effects"][0]["confirmed"], true);

    let (runtime, queue) = Queue::open(&data, 2).await;
    assert_eq!(queue.item(&completed).await["state"], "done");
    let interrupted = queue.add_fixture(build).await;
    let waiting = queue.add_fixture(build).await;
    assert_eq!(queue.item(&waiting).await["state"], "queued");
    let interrupted_job = queue.dispatch_fixture(&interrupted).await;
    let never_replied = printer.next_upload().await;
    let admitted = assert_admitted(&data, &interrupted_job);
    assert_eq!(printer.mutations(), [UPLOAD, UPLOAD]);

    stopped(tokio::spawn(runtime.shutdown())).await;
    assert_eq!(
        job_document(&data, &interrupted_job),
        admitted,
        "shutdown must leave the admitted effect to startup recovery"
    );
    drop(never_replied);

    let (runtime, queue) = Queue::open(&data, 3).await;
    let unknown = queue.item(&interrupted).await;
    assert_interrupted(&unknown, &interrupted_job);
    let recovered = job_document(&data, &interrupted_job);
    assert_eq!(recovered["state"], "reconciliation_required");
    assert_eq!(recovered["effects"], admitted["effects"]);
    assert!(recovered["public_result"].is_null());
    assert_eq!(job_document(&data, &completed_job), acknowledged);
    let original_jobs = BTreeSet::from([completed_job, interrupted_job.clone()]);
    assert_eq!(queue.job_ids().await, original_jobs);

    refused_as_unknown(&queue.origin, &queue.cookie, &interrupted).await;
    let waiting_item = queue.item(&waiting).await;
    assert_eq!(waiting_item["state"], "queued");
    assert!(waiting_item["upload_job_id"].is_null());
    assert_eq!(printer.mutations(), [UPLOAD, UPLOAD]);
    stopped(tokio::spawn(runtime.shutdown())).await;

    let (runtime, queue) = Queue::open(&data, 4).await;
    assert_eq!(queue.item(&interrupted).await, unknown);
    assert_eq!(queue.item(&waiting).await, waiting_item);
    assert_eq!(job_document(&data, &interrupted_job), recovered);
    queue
        .reconcile_printed(&interrupted, &interrupted_job)
        .await;
    let waiting_job = queue.dispatch_fixture(&waiting).await;
    let acknowledge = printer.next_upload().await;
    assert_admitted(&data, &waiting_job);
    acknowledge.send(()).unwrap();
    assert_eq!(queue.settled(&waiting).await["state"], "done");
    let mut expected = original_jobs;
    assert!(expected.insert(waiting_job));
    assert_eq!(queue.job_ids().await, expected);
    assert_eq!(queue.item(&interrupted).await["state"], "done");
    stopped(tokio::spawn(runtime.shutdown())).await;
    assert_eq!(
        printer.mutations(),
        [UPLOAD, UPLOAD, UPLOAD],
        "only the three explicitly dispatched uploads; no retry or print start"
    );
}

async fn stopped(task: JoinHandle<pp_core::ShutdownReceipt>) {
    let receipt = timeout(DEADLINE, task)
        .await
        .expect("shutdown exceeded its bound")
        .unwrap();
    assert!(receipt.complete(), "{receipt:?}");
    assert!(receipt.storage_released, "{receipt:?}");
}

fn job_document(data: &Path, job_id: &str) -> Value {
    let db = rusqlite::Connection::open_with_flags(
        data.join("print-partner.db"),
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
    )
    .unwrap();
    let saved: String = db
        .query_row(
            "SELECT document FROM durable_jobs WHERE id=?1",
            [job_id],
            |row| row.get(0),
        )
        .unwrap();
    serde_json::from_str(&saved).unwrap()
}

fn assert_admitted(data: &Path, job_id: &str) -> Value {
    let job = job_document(data, job_id);
    assert_eq!(job["state"], "effect_admitted");
    let effects = job["effects"].as_array().unwrap();
    assert_eq!(effects.len(), 1);
    let effect = &effects[0];
    assert_eq!(effect["intent"]["operation"], "printer_upload");
    assert_eq!(effect["intent"]["target"], PRINTER);
    assert_eq!(
        effect["intent"]["content_hash"],
        hex::encode(Sha256::digest(GCODE))
    );
    assert_eq!(effect["confirmed"], false);
    assert!(effect["receipt"].is_null());
    job
}

fn assert_interrupted(item: &Value, job_id: &str) {
    assert_eq!(item["state"], "error", "{item}");
    assert_eq!(item["upload_job_id"], job_id, "{item}");
    assert_eq!(item["outcome_unknown"], true, "{item}");
    assert_eq!(item["error"], pp_storage::send_queue::INTERRUPTED_ERROR);
}

impl Queue {
    async fn add_fixture(&self, build: i64) -> String {
        item_id(&self.add(PRINTER, build).await).to_owned()
    }

    async fn dispatch_fixture(&self, item: &str) -> String {
        send_now(&self.origin, &self.cookie, item).await
    }

    async fn item(&self, id: &str) -> Value {
        self.ok("GET", QUEUE, None).await["items"]
            .as_array()
            .unwrap()
            .iter()
            .find(|item| item["id"] == id)
            .unwrap_or_else(|| panic!("missing queue item {id}"))
            .clone()
    }

    async fn reconcile_printed(&self, item: &str, job: &str) {
        let answer = json!({"job_id": job, "printed": true});
        let reply = self
            .ok("POST", &format!("{QUEUE}/{item}/reconcile"), Some(answer))
            .await;
        assert_eq!(reply["item"]["state"], "done");
        assert_eq!(reply["item"]["outcome_unknown"], false);
    }
}

#[derive(Clone)]
struct PrinterState {
    requests: Arc<Mutex<Vec<String>>>,
    uploads: mpsc::UnboundedSender<(Bytes, oneshot::Sender<()>)>,
}

struct Printer {
    url: String,
    uploads: mpsc::UnboundedReceiver<(Bytes, oneshot::Sender<()>)>,
    requests: Arc<Mutex<Vec<String>>>,
    task: JoinHandle<std::io::Result<()>>,
}

impl Printer {
    async fn start() -> Self {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let (incoming, uploads) = mpsc::unbounded_channel();
        let requests = Arc::new(Mutex::new(Vec::new()));
        let state = PrinterState {
            requests: Arc::clone(&requests),
            uploads: incoming,
        };
        let router = Router::new().fallback(serve_printer).with_state(state);
        let task = tokio::spawn(async move { axum::serve(listener, router).await });
        Self {
            url,
            uploads,
            requests,
            task,
        }
    }

    fn seed(&self, dir: &Path) {
        seed_settings(
            dir,
            &[
                ("feature_toggles", json!({"farm": true})),
                (
                    "integrations",
                    json!([{"id": "shutdown", "name": "Shutdown",
                "type": "moonraker", "config": {"base_url": self.url}}]),
                ),
                (
                    "printer.fleet",
                    json!([{"id": PRINTER, "name": "Shutdown",
                "integration_id": "shutdown"}]),
                ),
            ],
        );
    }

    async fn next_upload(&mut self) -> oneshot::Sender<()> {
        let (body, acknowledge) = timeout(DEADLINE, self.uploads.recv())
            .await
            .expect("printer received no upload")
            .unwrap();
        assert!(
            body.windows(GCODE.len())
                .any(|window| window == GCODE.as_bytes())
        );
        acknowledge
    }

    fn mutations(&self) -> Vec<String> {
        self.requests.lock().unwrap().clone()
    }
}

impl Drop for Printer {
    fn drop(&mut self) {
        self.task.abort();
    }
}

async fn serve_printer(
    State(state): State<PrinterState>,
    method: Method,
    uri: Uri,
    body: Bytes,
) -> Response {
    if method == Method::GET {
        return axum::Json(json!({})).into_response();
    }
    state.requests.lock().unwrap().push(uri.path().to_owned());
    if method != Method::POST || uri.path() != UPLOAD {
        return StatusCode::METHOD_NOT_ALLOWED.into_response();
    }
    let (acknowledge, release) = oneshot::channel();
    if state.uploads.send((body, acknowledge)).is_err() || release.await.is_err() {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    }
    (StatusCode::CREATED, axum::Json(json!({}))).into_response()
}
