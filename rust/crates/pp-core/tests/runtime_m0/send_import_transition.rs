use super::{QUEUE, UPLOAD, call, sends, settled};
use crate::watcher_proof::{proof_data, run_in_child, shut_down, signed_in};
use crate::{desktop_cookie, launch, request, started};
use axum::http::StatusCode;
use pp_core::{CoreRuntime, node_import};
use pp_storage::send_queue::{INTERRUPTED_ERROR, NOT_STARTED_ERROR};
use pp_testkit::{Fixture, Installation, moonraker::RoutedPrinter, settings::seed_setting};
use rusqlite::{Connection, OpenFlags};
use serde_json::{Value, json};
use std::{fs, path::Path};

const PRINTER: &str = "import-printer";
const QUEUED: &str = "node-queued";
const INTERRUPTED: &str = "node-interrupted";
const MISSING_JOB: &str = "node-upload-missing";
const QUEUED_FILE: &str = "exports/printer-uploads/queued.gcode";
const INTERRUPTED_FILE: &str = "exports/printer-uploads/interrupted.gcode";
const QUEUED_BYTES: &[u8] = b"; disposable queued file\n";
const INTERRUPTED_BYTES: &[u8] = b"; disposable interrupted file\n";

#[tokio::test]
async fn imported_queue_preserves_intent_without_replaying_a_send() {
    let sandbox = tempfile::tempdir().unwrap();
    run_in_child(
        "send_queue::import_transition::proof_in_child",
        sandbox.path(),
    )
    .await;
}

#[tokio::test]
#[ignore = "the parent runs this with the rebuild gate set"]
async fn proof_in_child() {
    let printer = RoutedPrinter::start(vec![(UPLOAD.into(), 201, b"{}".to_vec())]).unwrap();
    let source = node_queue(printer.url());
    let destination = proof_data()
        .canonicalize()
        .unwrap()
        .join("rust-installation");
    let original_database = fs::read(source.database_path()).unwrap();
    let report = node_import::import(source.path(), &destination).unwrap();
    assert!(report.complete);
    assert_eq!(report.source_schema, 34);
    assert_imported_files(&destination);
    assert_eq!(queue_jobs(&destination), Vec::<String>::new());
    assert_eq!(fs::read(source.database_path()).unwrap(), original_database);
    assert!(sends(printer.request_lines()).is_empty());

    let (runtime, queue) = Session::open(&destination, Account::First).await;
    let initial = queue.items().await;
    assert_eq!(initial.len(), 2);
    let (queued, interrupted) = (&initial[0], &initial[1]);
    assert_eq!(queued["id"], QUEUED);
    assert_eq!(queued["state"], "error");
    assert_eq!(queued["error"], NOT_STARTED_ERROR);
    assert_eq!(queued["outcome_unknown"], false);
    assert!(queued["upload_job_id"].is_null());
    assert_eq!(interrupted["id"], INTERRUPTED);
    assert_eq!(interrupted["state"], "error");
    assert_eq!(interrupted["error"], INTERRUPTED_ERROR);
    assert_eq!(interrupted["outcome_unknown"], true);
    assert_eq!(interrupted["upload_job_id"], MISSING_JOB);
    for item in &initial {
        assert!(item.get("artifact_path").is_none(), "{item}");
        assert_eq!(item["printer_id"], PRINTER);
    }
    queue.assert_no_drain().await;
    queue.assert_unknown_and_reserved().await;
    assert_eq!(queue.items().await, initial);
    shut_down(runtime).await;
    assert!(queue_jobs(&destination).is_empty());
    assert!(sends(printer.request_lines()).is_empty());

    let (runtime, queue) = Session::open(&destination, Account::Returning).await;
    assert_eq!(queue.items().await, initial);
    queue.assert_unknown_and_reserved().await;

    let (status, stale) = queue.reconcile("old-unrelated-upload").await;
    assert_eq!(status, StatusCode::CONFLICT, "{stale}");
    assert_eq!(stale["code"], "send_changed");
    assert_eq!(queue.items().await, initial);

    let (status, receipt) = queue.reconcile(MISSING_JOB).await;
    assert_eq!(status, StatusCode::OK, "{receipt}");
    assert_eq!(receipt["item"]["id"], INTERRUPTED);
    assert_eq!(receipt["item"]["state"], "done");
    assert_eq!(receipt["item"]["outcome_unknown"], false);
    assert_eq!(receipt["item"]["upload_job_id"], MISSING_JOB);
    queue.assert_no_drain().await;
    assert!(queue_jobs(&destination).is_empty());
    assert!(sends(printer.request_lines()).is_empty());
    assert_imported_files(&destination);

    let (status, dispatched) = queue.dispatch(QUEUED).await;
    assert_eq!(status, StatusCode::OK, "{dispatched}");
    let job = dispatched["job_id"].as_str().unwrap();
    assert_ne!(job, MISSING_JOB);
    assert_eq!(dispatched["item"]["upload_job_id"], job);
    let finished = settled(&queue.origin, &queue.cookie, QUEUED).await;
    assert_eq!(finished["state"], "done", "{finished}");
    assert_eq!(finished["upload_job_id"], job);
    let after = queue.items().await;
    assert_eq!(after, [finished, receipt["item"].clone()]);
    assert_eq!(queue_jobs(&destination), [job]);
    assert_eq!(
        sends(printer.request_lines()),
        [format!("POST {UPLOAD} HTTP/1.1")]
    );
    shut_down(runtime).await;

    let (runtime, queue) = Session::open(&destination, Account::Returning).await;
    assert_eq!(queue.items().await, after);
    queue.assert_no_drain().await;
    assert_eq!(queue.dispatch(QUEUED).await.0, StatusCode::CONFLICT);
    assert_eq!(queue.dispatch(INTERRUPTED).await.0, StatusCode::CONFLICT);
    assert_eq!(queue_jobs(&destination), [job]);
    shut_down(runtime).await;
    assert_eq!(
        sends(printer.request_lines()),
        [format!("POST {UPLOAD} HTTP/1.1")]
    );
    assert_eq!(fs::read(source.database_path()).unwrap(), original_database);
    assert_eq!(
        fs::read(source.path().join(QUEUED_FILE)).unwrap(),
        QUEUED_BYTES
    );
    assert_eq!(
        fs::read(source.path().join(INTERRUPTED_FILE)).unwrap(),
        INTERRUPTED_BYTES
    );
}

fn node_queue(fixture_url: &str) -> Installation {
    let installation = Installation::create(Fixture::NodeSchema34).unwrap();
    let root = installation.path().canonicalize().unwrap();
    fs::create_dir_all(root.join("exports/printer-uploads")).unwrap();
    for (file, bytes) in [
        (QUEUED_FILE, QUEUED_BYTES),
        (INTERRUPTED_FILE, INTERRUPTED_BYTES),
    ] {
        fs::write(root.join(file), bytes).unwrap();
    }
    let queued = legacy_item(&root, QUEUED, QUEUED_FILE, "queued");
    let mut interrupted = legacy_item(&root, INTERRUPTED, INTERRUPTED_FILE, "sending");
    interrupted["upload_job_id"] = json!(MISSING_JOB);
    interrupted["start"] = json!(true);
    for (key, value) in [
        ("feature_toggles", json!({"farm": true})),
        (
            "integrations",
            json!([{
                "id": "import-host",
                "type": "moonraker",
                "name": "Import loopback",
                "created_at": "2026-11-20T00:00:00.000Z",
                "updated_at": "2026-11-20T00:00:00.000Z",
                "config": {"base_url": fixture_url, "enabled": true}
            }]),
        ),
        (
            "printer.fleet",
            json!([{"id": PRINTER, "name": "Fixture printer", "integration_id": "import-host"}]),
        ),
        ("printer.send_queue", json!([queued, interrupted])),
    ] {
        seed_setting(&root, key, &value.to_string()).unwrap();
    }
    installation
}

fn legacy_item(root: &Path, id: &str, file: &str, state: &str) -> Value {
    let path = root.join(file);
    json!({
        "id": id,
        "filename": path.file_name().unwrap().to_str().unwrap(),
        "artifact_path": path,
        "printer_id": PRINTER,
        "state": state,
        "wait_for_idle": false,
        "start": false,
        "created_at": "2026-11-20T00:00:00.000Z",
        "updated_at": "2026-11-20T00:00:00.000Z"
    })
}

fn database(root: &Path) -> Connection {
    Connection::open_with_flags(
        root.join("print-partner.db"),
        OpenFlags::SQLITE_OPEN_READ_ONLY,
    )
    .unwrap()
}

fn assert_imported_files(root: &Path) {
    let db = database(root);
    let legacy_setting: i64 = db
        .query_row(
            "SELECT count(*) FROM app_settings WHERE key = 'printer.send_queue'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(legacy_setting, 0);
    for (id, relative, bytes) in [
        (QUEUED, QUEUED_FILE, QUEUED_BYTES),
        (INTERRUPTED, INTERRUPTED_FILE, INTERRUPTED_BYTES),
    ] {
        let path: String = db
            .query_row(
                "SELECT artifact_path FROM printer_send_queue WHERE tenant_id = 'default' AND id = ?1",
                [id],
                |row| row.get(0)
            )
            .unwrap();
        assert_eq!(Path::new(&path), root.join(relative));
        assert_eq!(fs::read(path).unwrap(), bytes);
    }
}

fn queue_jobs(root: &Path) -> Vec<String> {
    database(root)
        .prepare("SELECT id FROM durable_jobs WHERE kind = 'printer-upload' ORDER BY id")
        .unwrap()
        .query_map([], |row| row.get::<_, String>(0))
        .unwrap()
        .collect::<rusqlite::Result<_>>()
        .unwrap()
}

enum Account {
    First,
    Returning,
}

struct Session {
    origin: String,
    cookie: String,
}

impl Session {
    async fn open(root: &Path, account: Account) -> (CoreRuntime, Self) {
        let mut input = launch(root.to_owned());
        input.bundle.node = root.join("node-is-not-installed");
        let mut runtime = started(CoreRuntime::start_for_launcher(input).await, root);
        assert!(
            runtime.rust_only() && runtime.handle().compat_status().is_none(),
            "the copied queue belongs only to the Rust runtime"
        );
        let desktop = desktop_cookie(&mut runtime).await;
        let origin = runtime.origin().to_owned();
        let attempt = match account {
            Account::First => 1,
            Account::Returning => 2,
        };
        let cookie = signed_in(&origin, &desktop, attempt).await;
        (runtime, Self { origin, cookie })
    }

    async fn items(&self) -> Vec<Value> {
        let (status, body) = call(&self.origin, &self.cookie, "GET", QUEUE).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        body["items"].as_array().unwrap().clone()
    }

    async fn dispatch(&self, id: &str) -> (StatusCode, Value) {
        call(
            &self.origin,
            &self.cookie,
            "POST",
            &format!("{QUEUE}/{id}/dispatch"),
        )
        .await
    }

    async fn assert_no_drain(&self) {
        let (status, body) = call(
            &self.origin,
            &self.cookie,
            "POST",
            &format!("{QUEUE}/drain"),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(body, json!({"results": []}));
    }

    async fn assert_unknown_and_reserved(&self) {
        let (status, unknown) = self.dispatch(INTERRUPTED).await;
        assert_eq!(status, StatusCode::CONFLICT, "{unknown}");
        assert_eq!(unknown["code"], "send_outcome_unknown");
        let (status, reserved) = self.dispatch(QUEUED).await;
        assert_eq!(status, StatusCode::CONFLICT, "{reserved}");
        assert_eq!(
            reserved["detail"],
            "Another send to this printer is still unresolved"
        );
    }

    async fn reconcile(&self, job_id: &str) -> (StatusCode, Value) {
        let (status, _, body) = request(
            &self.origin,
            &format!("{QUEUE}/{INTERRUPTED}/reconcile"),
            "POST",
            Some(&self.cookie),
            None,
            None,
            Some(json!({"job_id": job_id, "printed": true})),
        )
        .await;
        (status, serde_json::from_slice(&body).unwrap())
    }
}
