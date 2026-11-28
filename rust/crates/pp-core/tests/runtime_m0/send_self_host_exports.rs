use super::{QUEUE, Queue, proof_data, run_in_child, shut_down};
use axum::http::StatusCode;
use pp_core::node_import;
use pp_storage::send_queue::{NOT_STARTED_ERROR, writes::MISSING_ARTIFACT};
use pp_testkit::{Fixture, Installation, prusalink::UploadPrinter, settings::seed_setting};
use serde_json::{Value, json};
use std::{collections::BTreeSet, fs, path::Path};

const PRINTER: &str = "self-host-printer";
const FILENAME: &str = "plate.gcode";
const UPLOAD: &str = "/api/v1/files/usb/plate.gcode";
const STAMP: &str = "2026-11-20T00:00:00.000Z";

struct Staged {
    id: &'static str,
    path: &'static str,
    bytes: Option<&'static [u8]>,
}

const LEGACY: Staged = Staged {
    id: "legacy-collision",
    path: "exports/printer-uploads/shared/plate.gcode",
    bytes: Some(b"; flat collision fixture\n"),
};
const MIGRATED: Staged = Staged {
    id: "tenant-collision",
    path: "exports/tenant-default/printer-uploads/shared/plate.gcode",
    bytes: Some(b"; tenant collision fixture\n"),
};
const MISSING: Staged = Staged {
    id: "missing-original",
    path: "exports/printer-uploads/missing/plate.gcode",
    bytes: None,
};
const DECOY: Staged = Staged {
    id: "not-in-queue",
    path: "exports/tenant-default/printer-uploads/missing/plate.gcode",
    bytes: Some(b"; tenant file is not the missing original\n"),
};
const UNMOVED: Staged = Staged {
    id: "not-migrated",
    path: "exports/printer-uploads/not-migrated/plate.gcode",
    bytes: Some(b"; flat file with no tenant collision\n"),
};
const UNMOVED_TENANT: Staged = Staged {
    id: "not-synthesized",
    path: "exports/tenant-default/printer-uploads/not-migrated/plate.gcode",
    bytes: None,
};
const QUEUED: [&Staged; 3] = [&LEGACY, &MIGRATED, &MISSING];
const ON_DISK: [&Staged; 6] = [
    &LEGACY,
    &MIGRATED,
    &MISSING,
    &DECOY,
    &UNMOVED,
    &UNMOVED_TENANT,
];

#[tokio::test]
async fn imported_self_host_exports_never_substitute_staged_bytes() {
    let sandbox = tempfile::tempdir().unwrap();
    run_in_child(
        "send_queue::transition::send_self_host_exports::exports_in_child",
        sandbox.path(),
    )
    .await;
}

#[tokio::test]
#[ignore = "run by the self-host export transition proof with the rebuild gate set"]
async fn exports_in_child() {
    let printer = UploadPrinter::start(UPLOAD, 201, b"{}").unwrap();
    let source = node_installation(printer.url());
    let destination = proof_data().canonicalize().unwrap().join("rust-copy");
    let source_database = fs::read(source.database_path()).unwrap();
    assert_export_layout(source.path(), &[]);

    let report = node_import::import(source.path(), &destination).unwrap();
    assert!(report.complete);
    assert_eq!(report.source_schema, 34);
    assert_eq!(
        report
            .settings
            .notes
            .iter()
            .find(|note| note.name == "rebased printer.send_queue artifact_path")
            .unwrap()
            .count,
        QUEUED.len() as u64,
    );
    assert_export_layout(&destination, &[]);
    assert!(printer.sends().is_empty());

    let (runtime, queue) = Queue::open(&destination, 1).await;
    let initial = queue.ok("GET", QUEUE, None).await;
    let items = initial["items"].as_array().unwrap();
    assert_eq!(items.len(), QUEUED.len());
    for (item, fixture) in items.iter().zip(QUEUED) {
        assert_eq!(item["id"], fixture.id);
        assert_eq!(item["filename"], FILENAME);
        assert_eq!(item["state"], "error");
        assert_eq!(item["error"], NOT_STARTED_ERROR);
        assert_eq!(item["outcome_unknown"], false);
        assert_eq!(item["start"], false);
        assert!(item["upload_job_id"].is_null(), "{item}");
        assert!(item.get("artifact_path").is_none(), "{item}");
    }
    assert!(queue.job_ids().await.is_empty());
    assert_eq!(queue.drain(None).await, json!({"results": []}));
    assert!(printer.sends().is_empty());
    assert_export_layout(&destination, &[]);

    let missing = refused_missing(&queue).await;
    assert!(queue.job_ids().await.is_empty());
    assert!(printer.sends().is_empty());

    let completed = [LEGACY.id, MIGRATED.id];
    let legacy = upload(&queue, &LEGACY).await;
    assert_uploads(&printer, &[&LEGACY]);
    let after_legacy = queue.ok("GET", QUEUE, None).await;
    assert_eq!(
        after_legacy["items"],
        json!([legacy.item, items[1], missing]),
    );
    let migrated = upload(&queue, &MIGRATED).await;
    let jobs = BTreeSet::from([legacy.id.to_owned(), migrated.id.to_owned()]);
    assert_eq!(jobs.len(), 2);
    assert_eq!(queue.job_ids().await, jobs);
    assert_uploads(&printer, &[&LEGACY, &MIGRATED]);
    shut_down(runtime).await;
    assert_export_layout(&destination, &completed);

    let (runtime, queue) = Queue::open(&destination, 2).await;
    let restarted = queue.ok("GET", QUEUE, None).await;
    assert_eq!(
        restarted["items"],
        json!([legacy.item, migrated.item, missing]),
    );
    for upload in [&legacy, &migrated] {
        assert_eq!(queue.job(&upload.id).await, upload.job);
    }
    assert_eq!(queue.drain(None).await, json!({"results": []}));
    let missing_after_retry = refused_missing(&queue).await;
    assert_eq!(missing_after_retry["id"], missing["id"]);
    for (fixture, receipt) in [(&LEGACY, &legacy), (&MIGRATED, &migrated)] {
        let (status, refusal) = queue.call("POST", &dispatch_path(fixture), None).await;
        assert_eq!(status, StatusCode::CONFLICT, "{refusal}");
        assert_eq!(queue.settled(fixture.id).await, receipt.item);
    }
    assert_eq!(queue.job_ids().await, jobs);
    shut_down(runtime).await;

    assert_uploads(&printer, &[&LEGACY, &MIGRATED]);
    assert_export_layout(&destination, &completed);
    assert_export_layout(source.path(), &[]);
    assert_eq!(fs::read(source.database_path()).unwrap(), source_database);
}

fn node_installation(url: &str) -> Installation {
    let source = Installation::create(Fixture::NodeSchema34).unwrap();
    let root = source.path().canonicalize().unwrap();
    for artifact in ON_DISK {
        if let Some(bytes) = artifact.bytes {
            let target = root.join(artifact.path);
            fs::create_dir_all(target.parent().unwrap()).unwrap();
            fs::write(target, bytes).unwrap();
        }
    }
    let queue = QUEUED.map(|artifact| {
        json!({
            "id": artifact.id,
            "filename": FILENAME,
            "artifact_path": root.join(artifact.path),
            "printer_id": PRINTER,
            "start": false,
            "wait_for_idle": false,
            "state": "queued",
            "created_at": STAMP,
            "updated_at": STAMP
        })
    });
    let host = json!({
        "id": "self-host-fixture",
        "type": "prusalink",
        "name": "Loopback fixture",
        "created_at": STAMP,
        "updated_at": STAMP,
        "config": {"base_url": url, "username": "fixture", "password": "fixture-only"}
    });
    let printer = json!({
        "id": PRINTER,
        "integration_id": host["id"],
        "name": "Fixture printer"
    });
    for (key, value) in [
        ("printer.send_queue", json!(queue)),
        ("feature_toggles", json!({"farm": true})),
        ("integrations", json!([host])),
        ("printer.fleet", json!([printer])),
    ] {
        seed_setting(&root, key, &value.to_string()).unwrap();
    }
    source
}

fn assert_export_layout(root: &Path, completed: &[&str]) {
    let exports = root.join("exports");
    for artifact in ON_DISK {
        let path = root.join(artifact.path);
        if completed.contains(&artifact.id) {
            assert!(!path.parent().unwrap().exists(), "{path:?}");
        } else if let Some(expected) = artifact.bytes {
            let bytes = fs::read(&path).unwrap_or_else(|error| panic!("{path:?}: {error}"));
            assert_eq!(bytes, expected, "{path:?}");
        } else {
            assert!(!path.exists(), "{path:?}");
        }
    }
    assert!(
        !exports
            .join("tenant-default/tenant-default/printer-uploads")
            .exists(),
        "the tenant directory must not be nested after restart",
    );
}

fn dispatch_path(artifact: &Staged) -> String {
    format!("{QUEUE}/{}/dispatch", artifact.id)
}

async fn refused_missing(queue: &Queue) -> Value {
    let (status, refused) = queue.call("POST", &dispatch_path(&MISSING), None).await;
    assert_eq!(status, StatusCode::CONFLICT, "{refused}");
    assert_eq!(refused["detail"], MISSING_ARTIFACT);
    let item = queue.settled(MISSING.id).await;
    assert_eq!(item["state"], "error", "{item}");
    assert_eq!(item["error"], MISSING_ARTIFACT);
    assert_eq!(item["outcome_unknown"], false);
    assert!(item["upload_job_id"].is_null());
    item
}

struct Receipt {
    id: String,
    item: Value,
    job: Value,
}

async fn upload(queue: &Queue, fixture: &Staged) -> Receipt {
    let dispatched = queue.ok("POST", &dispatch_path(fixture), None).await;
    let id = dispatched["job_id"].as_str().unwrap().to_owned();
    assert_eq!(dispatched["item"]["id"], fixture.id);
    assert_eq!(dispatched["item"]["upload_job_id"], id);

    let item = queue.settled(fixture.id).await;
    assert_eq!(item["state"], "done", "{item}");
    assert_eq!(item["upload_job_id"], id);
    let job = queue.job(&id).await;
    assert_eq!(job["status"], "done", "{job}");
    assert_eq!(job["result"]["started"], false, "{job}");
    assert_eq!(job["result"]["printer_id"], PRINTER, "{job}");
    assert_eq!(job["result"]["filename"], FILENAME, "{job}");
    Receipt { id, item, job }
}

fn assert_uploads(printer: &UploadPrinter, artifacts: &[&Staged]) {
    let sends = printer.sends();
    assert_eq!(sends.len(), artifacts.len());
    for (send, artifact) in sends.iter().zip(artifacts) {
        assert_eq!(send.line(), format!("PUT {UPLOAD} HTTP/1.1"));
        assert!(send.complete, "{}", artifact.id);
        assert_eq!(
            Some(send.body.as_slice()),
            artifact.bytes,
            "{}",
            artifact.id
        );
        assert_eq!(send.header("print-after-upload"), Some("?0"));
    }
}
