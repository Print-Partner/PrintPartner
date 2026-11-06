#![cfg(feature = "desktop_rebuild")]
//! GRE-365 and GRE-1215 through the mounted Rust routes: Send now, Send
//! ready, Reconcile and Remove on a disposable copy of a real SQLite fixture.
//! The printer checks are a fake, so no request reaches a printer; the send
//! worker's own tests cover the uploads.
use axum::{Router, http::StatusCode};
use loopback::ORIGIN;
use pp_api::{
    accepted::AcceptedHttpConfig,
    send_queue::{Prepared, SendTargets, send_queue_router},
};
use pp_storage::{
    Limits, WriterOwner,
    auth::{AuthPolicy, FirstUserTenant, RegistrationPolicy, SessionTenantPolicy},
    send_queue::{INTERRUPTED_ERROR, writes::NOT_PRINTED},
};
use serde_json::{Value, json};
use std::{
    path::PathBuf,
    sync::{Arc, Mutex},
};

#[path = "support/loopback.rs"]
mod loopback;

const DATABASE: &[u8] =
    include_bytes!("../../pp-storage/tests/fixtures/required-units/unchanged.db");
const POLICY: AuthPolicy = AuthPolicy {
    registration: RegistrationPolicy::FirstAccountOnly,
    first_user: FirstUserTenant::NewUser,
    session_tenant: SessionTenantPolicy::SingleAccountDefault,
};

/// Printer checks that answer from fixed state and record removals.
#[derive(Default)]
struct Targets {
    /// Printers that are not idle.
    busy: Vec<&'static str>,
    /// Items whose staged file is gone.
    missing: Vec<&'static str>,
    discarded: Mutex<Vec<String>>,
}

impl SendTargets for Targets {
    fn prepare(&self, item: &Value, idle: bool) -> Prepared {
        let id = item["id"].as_str().unwrap();
        let printer = item["printer_id"].as_str().unwrap();
        if self.missing.contains(&id) {
            return Prepared::Missing;
        }
        if idle && self.busy.contains(&printer) {
            return Prepared::Refused {
                status: StatusCode::CONFLICT,
                detail: "Printer is printing — wait for Idle or force dispatch".into(),
            };
        }
        Prepared::Ready {
            host_name: format!("{printer} host"),
        }
    }

    fn discard(&self, item: &Value) {
        let id = item["id"].as_str().unwrap().to_owned();
        self.discarded.lock().unwrap().push(id);
    }
}

struct Queue {
    root: PathBuf,
    _owner: WriterOwner,
    router: Router,
    targets: Arc<Targets>,
}

impl Queue {
    /// The fixture with items `(id, printer, state, error)`, mounted with
    /// `flag`.
    fn open(flag: &str, targets: Targets, items: &[(&str, &str, &str, Option<&str>)]) -> Self {
        let root = tempfile::tempdir().unwrap().keep();
        std::fs::write(root.join("print-partner.db"), DATABASE).unwrap();
        let owner = WriterOwner::open(&root, Limits::default()).unwrap().0;
        let sql = rusqlite::Connection::open(root.join("print-partner.db")).unwrap();
        for (position, (id, printer, state, error)) in items.iter().enumerate() {
            sql.execute(
                "INSERT INTO printer_send_queue(tenant_id,position,id,filename,artifact_path,printer_id,profile_id,match,wait_for_idle,start,state,checkoff_units,upload_job_id,error,host_name,created_at,updated_at)
                 VALUES('default',?1,?2,'a.gcode',?3,?4,NULL,'pinned',1,0,?5,NULL,NULL,?6,NULL,'2026-11-06T00:00:00.000Z','2026-11-06T00:00:00.000Z')",
                rusqlite::params![
                    position as i64,
                    id,
                    format!("/data/exports/printer-uploads/{id}/a.gcode"),
                    printer,
                    state,
                    error
                ],
            )
            .unwrap();
        }
        let targets = Arc::new(targets);
        let config = AcceptedHttpConfig::from_startup(ORIGIN, "default", Some(flag)).unwrap();
        let router = send_queue_router(
            config,
            owner.send_queue(POLICY).unwrap(),
            Arc::clone(&targets) as Arc<dyn SendTargets>,
        );
        Self {
            root,
            _owner: owner,
            router,
            targets,
        }
    }

    async fn send(&self, method: &str, path: &str, body: &str) -> (StatusCode, Value) {
        let cookie = Some("pp_session=required-unit-fixture-secret");
        loopback::send(&self.router, method, path, body, cookie).await
    }

    /// The item's state and error, and its upload job's state.
    fn item(&self, id: &str) -> (String, Option<String>, Option<String>) {
        rusqlite::Connection::open(self.root.join("print-partner.db"))
            .unwrap()
            .query_row(
                "SELECT q.state,q.error,j.state FROM printer_send_queue q LEFT JOIN durable_jobs j ON j.id=q.upload_job_id WHERE q.id=?1",
                [id],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .unwrap()
    }
}

impl Drop for Queue {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

fn detail(body: &Value) -> &str {
    body["detail"].as_str().unwrap_or_default()
}

#[tokio::test]
async fn ticket_t_26_send_queue_routes_mount_only_with_the_flag_and_need_a_session() {
    let queue = Queue::open("0", Targets::default(), &[("q1", "p-a", "queued", None)]);
    let (status, _) = queue
        .send("POST", "/api/v2/printer-send-queue/q1/dispatch", "")
        .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(queue.item("q1").0, "queued");

    let queue = Queue::open("1", Targets::default(), &[("q1", "p-a", "queued", None)]);
    let (status, _) = loopback::send(
        &queue.router,
        "POST",
        "/api/v2/printer-send-queue/q1/dispatch",
        "",
        None,
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert_eq!(queue.item("q1"), ("queued".into(), None, None));
}

#[tokio::test]
async fn ticket_t_26_send_now_enqueues_one_upload_and_waits_for_an_idle_printer() {
    let targets = Targets {
        busy: vec!["p-b"],
        missing: vec!["gone"],
        ..Targets::default()
    };
    let queue = Queue::open(
        "1",
        targets,
        &[
            ("q1", "p-a", "queued", None),
            ("busy", "p-b", "queued", None),
            ("gone", "p-c", "queued", None),
        ],
    );
    let (status, body) = queue
        .send("POST", "/api/v2/printer-send-queue/q1/dispatch", "")
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["item"]["state"], "sending");
    assert_eq!(body["item"]["host_name"], "p-a host");
    assert!(body["job_id"].is_string());
    assert_eq!(
        queue.item("q1"),
        ("sending".into(), None, Some("queued".into()))
    );
    let (status, body) = queue
        .send("POST", "/api/v2/printer-send-queue/q1/dispatch", "")
        .await;
    assert_eq!(
        (status, detail(&body)),
        (StatusCode::CONFLICT, "Item is not dispatchable")
    );

    let (status, body) = queue
        .send("POST", "/api/v2/printer-send-queue/busy/dispatch", "{}")
        .await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert!(detail(&body).starts_with("Printer is printing"), "{body}");
    assert_eq!(queue.item("busy"), ("queued".into(), None, None));
    let (status, _) = queue
        .send(
            "POST",
            "/api/v2/printer-send-queue/busy/dispatch",
            r#"{"force":true}"#,
        )
        .await;
    assert_eq!(status, StatusCode::OK);

    let (status, body) = queue
        .send("POST", "/api/v2/printer-send-queue/gone/dispatch", "")
        .await;
    assert_eq!(
        (status, detail(&body)),
        (StatusCode::CONFLICT, "Artifact file missing on disk")
    );
    assert_eq!(
        queue.item("gone"),
        (
            "error".into(),
            Some("Artifact file missing on disk".into()),
            None
        )
    );

    let (status, body) = queue
        .send("POST", "/api/v2/printer-send-queue/nope/dispatch", "")
        .await;
    assert_eq!(
        (status, detail(&body)),
        (StatusCode::NOT_FOUND, "Queue item not found")
    );
    let (status, _) = queue
        .send("POST", "/api/v2/printer-send-queue/q1/dispatch", "[]")
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn ticket_t_26_send_ready_sends_one_item_per_idle_printer() {
    let targets = Targets {
        busy: vec!["p-c"],
        ..Targets::default()
    };
    let queue = Queue::open(
        "1",
        targets,
        &[
            ("a1", "p-a", "queued", None),
            ("a2", "p-a", "queued", None),
            ("b1", "p-b", "queued", None),
            ("c1", "p-c", "queued", None),
            ("e1", "p-d", "error", Some("Upload failed")),
        ],
    );
    let (status, _) = queue
        .send(
            "POST",
            "/api/v2/printer-send-queue/drain",
            r#"{"printer_id":""}"#,
        )
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    let (status, body) = queue
        .send(
            "POST",
            "/api/v2/printer-send-queue/drain",
            r#"{"printer_id":"p-b"}"#,
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    let results = body["results"].as_array().unwrap();
    assert_eq!(results.len(), 1, "{body}");
    assert_eq!(results[0]["item_id"], "b1");

    let (status, body) = queue
        .send("POST", "/api/v2/printer-send-queue/drain", "")
        .await;
    assert_eq!(status, StatusCode::OK);
    let sent: Vec<_> = body["results"]
        .as_array()
        .unwrap()
        .iter()
        .map(|result| (result["item_id"].clone(), result["job_id"].is_string()))
        .collect();
    assert_eq!(sent, [(json!("a1"), true)], "{body}");
    // a2 waits behind a1 on the same printer, c1 for an idle printer, and
    // the error item for its own Send now.
    for id in ["a2", "c1"] {
        assert_eq!(queue.item(id).0, "queued", "{id}");
    }
    assert_eq!(queue.item("e1").0, "error");
}

#[tokio::test]
async fn ticket_t_26_an_unknown_outcome_is_refused_until_the_user_reconciles() {
    let targets = Targets {
        busy: vec!["p-a"],
        ..Targets::default()
    };
    let queue = Queue::open(
        "1",
        targets,
        &[
            ("lost", "p-a", "error", Some(INTERRUPTED_ERROR)),
            ("next", "p-a", "queued", None),
        ],
    );
    // Storage's refusal wins over the printer check.
    let (status, body) = queue
        .send("POST", "/api/v2/printer-send-queue/lost/dispatch", "")
        .await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(body["code"], "send_outcome_unknown", "{body}");
    let (status, body) = queue
        .send(
            "POST",
            "/api/v2/printer-send-queue/next/dispatch",
            r#"{"force":true}"#,
        )
        .await;
    assert_eq!(
        (status, detail(&body)),
        (
            StatusCode::CONFLICT,
            "Another send to this printer is still unresolved"
        )
    );

    for bad in ["", r#"{"printed":"no"}"#] {
        let (status, _) = queue
            .send("POST", "/api/v2/printer-send-queue/lost/reconcile", bad)
            .await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{bad}");
    }
    let (status, _) = queue
        .send(
            "POST",
            "/api/v2/printer-send-queue/next/reconcile",
            r#"{"printed":false}"#,
        )
        .await;
    assert_eq!(status, StatusCode::CONFLICT);
    let (status, body) = queue
        .send(
            "POST",
            "/api/v2/printer-send-queue/lost/reconcile",
            r#"{"printed":false}"#,
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["item"]["state"], "error");
    assert_eq!(body["item"]["error"], NOT_PRINTED);
    // Only a new, explicit Send now sends it again.
    assert_eq!(queue.item("lost").0, "error");
    let (status, body) = queue
        .send(
            "POST",
            "/api/v2/printer-send-queue/lost/dispatch",
            r#"{"force":true}"#,
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
}

#[tokio::test]
async fn ticket_t_26_remove_cancels_a_waiting_item_and_deletes_its_staged_file() {
    let queue = Queue::open(
        "1",
        Targets::default(),
        &[("q1", "p-a", "queued", None), ("q2", "p-b", "queued", None)],
    );
    let (status, body) = queue
        .send("DELETE", "/api/v2/printer-send-queue/q1", "")
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["item"]["state"], "cancelled");
    assert_eq!(*queue.targets.discarded.lock().unwrap(), ["q1"]);
    let (status, body) = queue
        .send("DELETE", "/api/v2/printer-send-queue/q1", "")
        .await;
    assert_eq!(
        (status, detail(&body)),
        (StatusCode::CONFLICT, "Item not cancellable")
    );
    let (status, _) = queue
        .send("DELETE", "/api/v2/printer-send-queue/nope", "")
        .await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    queue
        .send("POST", "/api/v2/printer-send-queue/q2/dispatch", "")
        .await;
    let (status, _) = queue
        .send("DELETE", "/api/v2/printer-send-queue/q2", "")
        .await;
    assert_eq!(status, StatusCode::CONFLICT, "a sending item stays");
    assert_eq!(*queue.targets.discarded.lock().unwrap(), ["q1"]);
}
