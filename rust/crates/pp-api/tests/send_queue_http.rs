#![cfg(feature = "desktop_rebuild")]
//! GRE-365, GRE-1215 and GRE-1227 through the mounted Rust routes: Add to
//! queue, Send now, Send ready, Reconcile and Remove on a disposable copy of a real SQLite fixture.
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
#[path = "support/multipart.rs"]
mod multipart;

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
    /// Removed items' ids, and refused additions' staged paths.
    discarded: Mutex<Vec<String>>,
    /// Files staged for added items, by name.
    staged: Mutex<Vec<(String, Vec<u8>)>>,
    /// Wakes sent to the worker.
    dispatches: Mutex<usize>,
    /// Runs once when the next file is staged: after Add read its units,
    /// before its write.
    on_stage: Mutex<Option<Box<dyn FnOnce() + Send>>>,
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

    fn host(&self, printer_id: &str) -> Result<String, (StatusCode, String)> {
        match printer_id {
            "p-bambu" => Err((
                StatusCode::BAD_REQUEST,
                "Queue is not supported for bambu".into(),
            )),
            "p-none" => Err((StatusCode::NOT_FOUND, "Fleet printer not found".into())),
            printer => Ok(format!("{printer} host")),
        }
    }

    fn stage(&self, name: &str, file: &[u8]) -> std::io::Result<String> {
        if let Some(hook) = self.on_stage.lock().unwrap().take() {
            hook();
        }
        let mut staged = self.staged.lock().unwrap();
        staged.push((name.to_owned(), file.to_vec()));
        Ok(format!(
            "/data/exports/printer-uploads/{}/{name}",
            staged.len()
        ))
    }

    fn dispatched(&self) {
        *self.dispatches.lock().unwrap() += 1;
    }

    fn discard(&self, item: &Value) {
        let id = item.get("id").unwrap_or(&item["artifact_path"]);
        self.discarded
            .lock()
            .unwrap()
            .push(id.as_str().unwrap().to_owned());
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
        // The Accepted Plan's Source snapshot, so the Plan reads as ready.
        std::fs::create_dir_all(root.join("repos/1/revisions/fixture")).unwrap();
        let owner = WriterOwner::open(&root, Limits::default()).unwrap().0;
        let sql = rusqlite::Connection::open(root.join("print-partner.db")).unwrap();
        sql.execute(
            "INSERT OR REPLACE INTO app_settings(tenant_id,key,value) VALUES('default','feature_toggles','{\"farm\":true}')",
            [],
        )
        .unwrap();
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
            owner.accepted_reads_with_policy(POLICY).unwrap(),
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
        loopback::send(&self.router, method, path, Some(body), cookie).await
    }

    async fn add(&self, parts: &[Part<'_>]) -> (StatusCode, Value) {
        self.post_form("/api/v2/printer-send-queue", parts).await
    }

    async fn post_form(&self, path: &str, parts: &[Part<'_>]) -> (StatusCode, Value) {
        let (content_type, body) = multipart::multipart(parts);
        let cookie = Some("pp_session=required-unit-fixture-secret");
        loopback::send_as(
            &self.router,
            "POST",
            path,
            Some(&content_type),
            body,
            cookie,
        )
        .await
    }

    /// Replaces the stored `feature_toggles` text.
    #[track_caller]
    fn store_toggles(&self, value: &str) {
        rusqlite::Connection::open(self.root.join("print-partner.db"))
            .unwrap()
            .execute(
                "UPDATE app_settings SET value=?1 WHERE tenant_id='default' AND key='feature_toggles'",
                [value],
            )
            .unwrap();
    }

    /// The number of queue items and of durable jobs.
    fn counts(&self) -> (i64, i64) {
        let sql = rusqlite::Connection::open(self.root.join("print-partner.db")).unwrap();
        let count = |table: &str| {
            sql.query_row(&format!("SELECT count(*) FROM {table}"), [], |row| {
                row.get(0)
            })
            .unwrap()
        };
        (count("printer_send_queue"), count("durable_jobs"))
    }

    /// The item's stored artifact path, host name, match and Checkoff units.
    fn stored(&self, id: &str) -> [String; 4] {
        rusqlite::Connection::open(self.root.join("print-partner.db"))
            .unwrap()
            .query_row(
                "SELECT artifact_path,host_name,match,coalesce(checkoff_units,'') FROM printer_send_queue WHERE id=?1",
                [id],
                |row| Ok([row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?]),
            )
            .unwrap()
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
        Some(""),
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
    assert_eq!(body["item"]["upload_job_id"], body["job_id"]);
    assert_eq!(body["item"]["outcome_unknown"], false);
    assert!(body["job_id"].is_string());
    assert_eq!(queue.stored("q1")[1], "p-a host");
    assert_eq!(
        queue.item("q1"),
        ("sending".into(), None, Some("queued".into()))
    );
    assert_eq!(*queue.targets.dispatches.lock().unwrap(), 1);
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

    for bad in ["", r#"{"printed":"no"}"#, r#"{"printed":false,"job_id":7}"#] {
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
    // An answer about another send of the item is stale.
    let (status, body) = queue
        .send(
            "POST",
            "/api/v2/printer-send-queue/lost/reconcile",
            r#"{"printed":true,"job_id":"job-other"}"#,
        )
        .await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(body["code"], "send_changed", "{body}");
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
    assert_eq!(body["item"]["outcome_unknown"], false);
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

#[tokio::test]
async fn ticket_t_26_a_send_needs_the_farm_feature() {
    let queue = Queue::open("1", Targets::default(), &[("q1", "p-a", "queued", None)]);
    queue.store_toggles(r#"{"farm":false}"#);
    let (status, body) = queue
        .send("POST", "/api/v2/printer-send-queue/q1/dispatch", "")
        .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_eq!(body["code"], "FEATURE_DISABLED", "{body}");
    let (status, body) = queue
        .send("POST", "/api/v2/printer-send-queue/drain", "")
        .await;
    assert_eq!(
        status,
        StatusCode::FORBIDDEN,
        "one problem for the whole drain"
    );
    assert_eq!(body["code"], "FEATURE_DISABLED", "{body}");
    queue.store_toggles("not json");
    let (status, body) = queue
        .send("POST", "/api/v2/printer-send-queue/q1/dispatch", "")
        .await;
    assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
    assert_eq!(body["code"], "FEATURE_TOGGLES_INVALID", "{body}");
    let (status, body) = queue
        .send("POST", "/api/v2/printer-send-queue/drain", "")
        .await;
    assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
    assert_eq!(body["code"], "FEATURE_TOGGLES_INVALID", "{body}");
    assert_eq!(queue.item("q1"), ("queued".into(), None, None));
    assert_eq!(*queue.targets.dispatches.lock().unwrap(), 0);
}

const GCODE: &[u8] = b"G28\nG1 X10\n";
/// A form part: field name, file name, bytes.
type Part<'a> = (&'a str, Option<&'a str>, &'a [u8]);

#[tokio::test]
async fn add_to_queue_stages_the_file_and_queues_an_item_without_sending() {
    let queue = Queue::open("1", Targets::default(), &[("q1", "p-a", "queued", None)]);
    let before = queue.counts();
    let (status, body) = queue
        .add(&[
            ("printer_id", None, b"p-b"),
            ("profile_id", None, b"1"),
            ("start", None, b"true"),
            ("checkoff_units", None, br#"[{"part_id":3,"unit_index":1}]"#),
            ("gcode", Some("plates/Plate 1.gcode"), GCODE),
        ])
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let item = &body["item"];
    assert_eq!(item["state"], "queued");
    assert_eq!(item["printer_id"], "p-b");
    assert_eq!(item["filename"], "Plate 1.gcode");
    assert_eq!(item["profile_id"], 1);
    assert_eq!(item["unit_count"], 1);
    assert_eq!(
        (&item["start"], &item["wait_for_idle"]),
        (&json!(true), &json!(true))
    );
    assert_eq!(
        (
            &item["error"],
            &item["upload_job_id"],
            &item["outcome_unknown"]
        ),
        (&Value::Null, &Value::Null, &json!(false))
    );
    // The staged path and host name stay server-side.
    assert!(item.get("artifact_path").is_none() && item.get("host_name").is_none());
    let id = item["id"].as_str().unwrap();
    assert_eq!(
        queue.stored(id),
        [
            "/data/exports/printer-uploads/1/Plate 1.gcode",
            "p-b host",
            "pinned",
            r#"[{"part_id":3,"unit_index":1}]"#,
        ]
    );
    assert_eq!(
        *queue.targets.staged.lock().unwrap(),
        [("Plate 1.gcode".to_owned(), GCODE.to_vec())]
    );
    assert_eq!(queue.item(id), ("queued".into(), None, None));
    assert_eq!(queue.counts(), (before.0 + 1, before.1), "no upload job");
    assert_eq!(
        *queue.targets.dispatches.lock().unwrap(),
        0,
        "the worker is not woken"
    );

    let large = vec![b';'; 3 * 1024 * 1024];
    let (status, body) = queue
        .add(&[
            ("printer_id", None, b"p-a"),
            ("profile_id", None, b"1"),
            ("file", Some("big.gco"), &large),
        ])
        .await;
    assert_eq!(
        status,
        StatusCode::OK,
        "a file past axum's 2 MB default: {body}"
    );

    let (_, list) = queue
        .send("POST", "/api/v2/printer-send-queue/drain", "")
        .await;
    assert_eq!(
        list["results"].as_array().unwrap().len(),
        2,
        "one item per printer: {list}"
    );
    assert_eq!(
        list["results"][1]["item_id"],
        json!(id),
        "the new item is after the stored one"
    );
}

const PRINTER: Part = ("printer_id", None, b"p-a");
const PLAN: Part = ("profile_id", None, b"1");
const FILE: Part = ("file", Some("a.gcode"), GCODE);

#[tokio::test]
async fn add_to_queue_refuses_a_bad_form_or_printer_before_staging() {
    let queue = Queue::open("1", Targets::default(), &[]);
    let bad = StatusCode::BAD_REQUEST;
    let cases: [(&[Part], StatusCode, &str); 7] = [
        (&[PLAN, FILE], bad, "printer_id is required"),
        (&[PRINTER, PLAN], bad, "G-code file required"),
        (
            &[PRINTER, PLAN, ("file", Some("a.stl"), GCODE)],
            bad,
            "Only .gcode / .bgcode / .gco files can be sent to a printer",
        ),
        (
            &[PRINTER, FILE],
            bad,
            "Pick a plan to bind this send (profile_id required)",
        ),
        (
            &[PRINTER, PLAN, FILE, ("gcode", Some("b.gcode"), GCODE)],
            bad,
            "Only one G-code file is allowed",
        ),
        (
            &[("printer_id", None, b"p-bambu"), PLAN, FILE],
            bad,
            "Queue is not supported for bambu",
        ),
        (
            &[("printer_id", None, b"p-none"), PLAN, FILE],
            StatusCode::NOT_FOUND,
            "Fleet printer not found",
        ),
    ];
    for (parts, expected, text) in cases {
        let (status, body) = queue.add(parts).await;
        assert_eq!((status, detail(&body)), (expected, text));
    }
    assert!(
        queue.targets.staged.lock().unwrap().is_empty(),
        "nothing was staged"
    );
    assert_eq!(queue.counts().0, 0);
}

#[tokio::test]
async fn add_to_queue_deletes_the_staged_file_when_storage_refuses_the_item() {
    let queue = Queue::open("1", Targets::default(), &[]);
    let (status, body) = queue.add(&[PRINTER, ("plan_id", None, b"99"), FILE]).await;
    assert_eq!(
        (status, detail(&body)),
        (StatusCode::NOT_FOUND, "Profile not found")
    );
    queue.store_toggles(r#"{"farm":false}"#);
    let (status, body) = queue.add(&[PRINTER, PLAN, FILE]).await;
    assert_eq!(
        (status, &body["code"]),
        (StatusCode::FORBIDDEN, &json!("FEATURE_DISABLED"))
    );
    assert_eq!(
        *queue.targets.discarded.lock().unwrap(),
        [
            "/data/exports/printer-uploads/1/a.gcode",
            "/data/exports/printer-uploads/2/a.gcode"
        ]
    );
    assert_eq!(queue.counts().0, 0);
}

#[tokio::test]
async fn add_to_queue_needs_a_valid_session_before_it_reads_the_upload() {
    let queue = Queue::open("1", Targets::default(), &[]);
    let (content_type, body) = multipart::multipart(&[
        ("printer_id", None, b"p-a"),
        ("profile_id", None, b"1"),
        ("file", Some("a.gcode"), GCODE),
    ]);
    let path = "/api/v2/printer-send-queue";
    let wrong = Some("pp_session=not-a-session");
    let (status, _) = loopback::send_as(
        &queue.router,
        "POST",
        path,
        Some(&content_type),
        body,
        wrong,
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert!(queue.targets.staged.lock().unwrap().is_empty());
    let (status, _) = queue.send("POST", path, "{}").await;
    assert_eq!(status, StatusCode::UNSUPPORTED_MEDIA_TYPE);
}

#[tokio::test]
async fn ticket_t_27_upload_only_routes_are_guarded_and_require_a_boolean() {
    let path = "/api/v2/printer-send-queue/settings/upload-only";
    let off = Queue::open("false", Targets::default(), &[]);
    assert_eq!(off.send("GET", path, "").await.0, StatusCode::NOT_FOUND);
    let queue = Queue::open(
        "1",
        Targets::default(),
        &[("upload-only", "p-a", "queued", None)],
    );
    for method in ["GET", "PUT"] {
        assert_eq!(
            loopback::send(
                &queue.router,
                method,
                path,
                Some(r#"{"enabled":true}"#),
                None
            )
            .await
            .0,
            StatusCode::UNAUTHORIZED
        );
    }
    assert_eq!(queue.send("GET", path, "").await.1["enabled"], false);
    for body in [r#"{}"#, r#"{"enabled":"true"}"#, r#"{"enabled":1}"#, "[]"] {
        assert_eq!(
            queue.send("PUT", path, body).await.0,
            StatusCode::BAD_REQUEST
        );
    }
    assert_eq!(
        queue.send("PUT", path, r#"{"enabled":true}"#).await.1["enabled"],
        true
    );
    assert_eq!(queue.send("GET", path, "").await.1["enabled"], true);
    let before = queue.counts();
    let (status, added) = queue
        .add(&[PRINTER, PLAN, FILE, ("start", None, b"true")])
        .await;
    assert_eq!(status, StatusCode::OK, "{added}");
    assert_eq!(added["item"]["start"], false);
    assert_eq!(added["item"]["state"], "queued");
    assert_eq!(
        queue.counts(),
        (before.0 + 1, before.1),
        "Add never enqueues an upload"
    );
    assert_eq!(
        *queue.targets.dispatches.lock().unwrap(),
        0,
        "mode changes never dispatch"
    );
    assert_eq!(
        queue
            .send("DELETE", "/api/v2/printer-send-queue/upload-only", "")
            .await
            .0,
        StatusCode::OK,
        "legacy queue IDs must retain their remove route"
    );
}

#[tokio::test]
async fn ticket_t_27_explicit_dispatch_and_drain_override_stale_start_flags() {
    let queue = Queue::open(
        "true",
        Targets::default(),
        &[("q1", "p-a", "queued", None), ("q2", "p-b", "queued", None)],
    );
    rusqlite::Connection::open(queue.root.join("print-partner.db"))
        .unwrap()
        .execute("UPDATE printer_send_queue SET start=1", [])
        .unwrap();
    queue
        .send(
            "PUT",
            "/api/v2/printer-send-queue/settings/upload-only",
            r#"{"enabled":true}"#,
        )
        .await;
    assert_eq!(queue.item("q1").0, "queued");
    assert_eq!(queue.item("q2").0, "queued");
    assert_eq!(*queue.targets.dispatches.lock().unwrap(), 0);
    let (status, result) = queue
        .send("POST", "/api/v2/printer-send-queue/q1/dispatch", "{}")
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(result["item"]["start"], false);
    assert_eq!(
        queue
            .send("POST", "/api/v2/printer-send-queue/drain", "{}")
            .await
            .0,
        StatusCode::OK
    );
    let sql = rusqlite::Connection::open(queue.root.join("print-partner.db")).unwrap();
    let starts: i64 = sql.query_row(
        "SELECT COUNT(*) FROM durable_jobs WHERE json_extract(document,'$.payload.payload.start')=1",
        [], |row| row.get(0)).unwrap();
    assert_eq!(starts, 0);
    assert_eq!(*queue.targets.dispatches.lock().unwrap(), 2);
}

/// Labels for Build 1's Accepted Plan (bracket.stl ×3, gear.stl ×2, and an
/// excluded Part): two bracket copies, one gear, one name for no Part.
const LABELLED: &[u8] = b"EXCLUDE_OBJECT_DEFINE NAME=bracket.stl_id_0_copy_0
EXCLUDE_OBJECT_DEFINE NAME=bracket.stl_id_0_copy_1
EXCLUDE_OBJECT_DEFINE NAME=gear.stl_id_1_copy_0
EXCLUDE_OBJECT_DEFINE NAME=spoon.stl_id_2_copy_0
G28
";

impl Queue {
    /// The preview for `LABELLED` on Build 1, without its binding, and the binding.
    async fn preview(&self) -> (Value, String) {
        let (status, mut body) = self
            .post_form(
                "/api/v2/printer-send-queue/checkoff-preview",
                &[
                    ("profile_id", None, b"1"),
                    ("file", Some("plate.gcode"), LABELLED),
                ],
            )
            .await;
        assert_eq!(status, StatusCode::OK, "{body}");
        let binding = body["binding"].as_str().unwrap().to_owned();
        assert_eq!(binding.len(), 64);
        body.as_object_mut().unwrap().remove("binding");
        (body, binding)
    }

    async fn add_linked(&self, binding: &str) -> (StatusCode, Value) {
        self.add(&[
            ("printer_id", None, b"p-b"),
            ("profile_id", None, b"1"),
            ("link_checkoff", None, b"1"),
            ("checkoff_binding", None, binding.as_bytes()),
            ("file", Some("plate.gcode"), LABELLED),
        ])
        .await
    }
}

#[tokio::test]
async fn gre_1896_preview_and_add_link_the_same_units_from_the_file() {
    let queue = Queue::open("1", Targets::default(), &[]);
    let before = queue.counts();
    let (preview, binding) = queue.preview().await;
    assert_eq!(
        preview,
        json!({
            "file": "read",
            "unit_count": 3,
            "parts": [
                {"part_id": 1, "filename": "bracket.stl", "count": 2},
                {"part_id": 3, "filename": "gear.stl", "count": 1}
            ],
            "unmatched": ["spoon.stl_id_2_copy_0"]
        })
    );
    assert_eq!(queue.counts(), before, "the preview writes nothing");

    let (status, body) = queue.add_linked(&binding).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["item"]["unit_count"], 3);
    let stored: Value =
        serde_json::from_str(&queue.stored(body["item"]["id"].as_str().unwrap())[3]).unwrap();
    assert_eq!(
        stored,
        json!([
            {"part_id": 1, "unit_index": 0, "object_name": "bracket.stl_id_0_copy_0"},
            {"part_id": 1, "unit_index": 1, "object_name": "bracket.stl_id_0_copy_1"},
            {"part_id": 3, "unit_index": 0, "object_name": "gear.stl_id_1_copy_0"}
        ])
    );

    // Without the opt-in, Add links what the form says, as before.
    let (status, body) = queue
        .add(&[
            ("printer_id", None, b"p-b"),
            ("profile_id", None, b"1"),
            ("file", Some("plate.gcode"), LABELLED),
        ])
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["item"]["unit_count"], 0);
}

#[tokio::test]
async fn gre_1896_add_refuses_a_preview_whose_units_changed() {
    let queue = Queue::open("1", Targets::default(), &[]);
    let (_, binding) = queue.preview().await;
    // bracket.stl unit 1 is printed after the preview.
    rusqlite::Connection::open(queue.root.join("print-partner.db"))
        .unwrap()
        .execute(
            "UPDATE print_progress SET completed=1 WHERE part_id=1 AND unit_index=0",
            [],
        )
        .unwrap();
    let before = queue.counts();
    let (status, body) = queue.add_linked(&binding).await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["code"], "send_units_changed");
    assert_eq!(queue.counts(), before, "nothing queued");
    assert!(
        queue.targets.staged.lock().unwrap().is_empty(),
        "nothing staged"
    );

    let (preview, fresh) = queue.preview().await;
    assert_ne!(fresh, binding);
    assert_eq!(
        preview["parts"][0],
        json!({"part_id": 1, "filename": "bracket.stl", "count": 2})
    );
    let (status, body) = queue.add_linked(&fresh).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let stored: Value =
        serde_json::from_str(&queue.stored(body["item"]["id"].as_str().unwrap())[3]).unwrap();
    assert_eq!(stored[0]["unit_index"], 1);
    assert_eq!(stored[1]["unit_index"], 2);

    let (status, body) = queue.add_linked("").await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
}

#[tokio::test]
async fn gre_1896_a_checkoff_during_add_refuses_it_inside_the_write() {
    let queue = Queue::open("1", Targets::default(), &[]);
    let (_, binding) = queue.preview().await;
    let database = queue.root.join("print-partner.db");
    // Add reads the units and checks the binding first; this lands after that.
    *queue.targets.on_stage.lock().unwrap() = Some(Box::new(move || {
        rusqlite::Connection::open(database)
            .unwrap()
            .execute(
                "UPDATE print_progress SET completed=1 WHERE part_id=1 AND unit_index=0",
                [],
            )
            .unwrap();
    }));
    let before = queue.counts();
    let (status, body) = queue.add_linked(&binding).await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["code"], "send_units_changed");
    assert_eq!(queue.counts(), before, "no item and no job");
    let staged = queue.targets.staged.lock().unwrap().len();
    assert_eq!(staged, 1, "the hook ran after staging began");
    assert_eq!(
        *queue.targets.discarded.lock().unwrap(),
        ["/data/exports/printer-uploads/1/plate.gcode"],
        "the staged file is removed again"
    );
}

#[tokio::test]
async fn gre_1896_preview_says_when_the_file_cannot_be_read() {
    let queue = Queue::open("1", Targets::default(), &[]);
    let preview = "/api/v2/printer-send-queue/checkoff-preview";
    // A binary G-code file whose one G-code block is Heatshrink-compressed.
    let mut heatshrink = b"GCDE\x01\0\0\0\0\0".to_vec();
    heatshrink.extend([1, 0, 3, 0, 4, 0, 0, 0, 4, 0, 0, 0, 0, 0]);
    heatshrink.extend(b"G28\n");
    let truncated = &heatshrink[..16];
    for (bytes, read) in [(&heatshrink[..], "unsupported"), (truncated, "unreadable")] {
        let (status, body) = queue
            .post_form(
                preview,
                &[
                    ("profile_id", None, b"1"),
                    ("file", Some("plate.bgcode"), bytes),
                ],
            )
            .await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(
            (body["file"].as_str(), &body["unit_count"]),
            (Some(read), &json!(0))
        );
    }
    for (parts, want) in [
        (
            vec![
                ("profile_id", None, &b"999"[..]),
                ("file", Some("a.gcode"), LABELLED),
            ],
            StatusCode::NOT_FOUND,
        ),
        (
            vec![("profile_id", None, &b"1"[..])],
            StatusCode::BAD_REQUEST,
        ),
        (
            vec![("file", Some("a.gcode"), LABELLED)],
            StatusCode::BAD_REQUEST,
        ),
    ] {
        let (status, body) = queue.post_form(preview, &parts).await;
        assert_eq!(status, want, "{body}");
    }
}

#[tokio::test]
async fn gre_1896_add_refuses_checkoff_units_cut_blank_or_broken() {
    let queue = Queue::open("1", Targets::default(), &[]);
    let unit = r#"{"part_id":1,"unit_index":0,"object_name":"bracket"},"#;
    let cut = format!("[{}", unit.repeat(70 * 1024 / unit.len()));
    // Valid JSON in its first 64 KiB; the field only goes past the limit.
    let padded = format!("[]{}x", " ".repeat(64 * 1024));
    let before = queue.counts();
    for raw in [cut.as_str(), padded.as_str(), "", "  "] {
        let (status, body) = queue
            .add(&[
                ("printer_id", None, b"p-b"),
                ("profile_id", None, b"1"),
                ("checkoff_units", None, raw.as_bytes()),
                ("file", Some("plate.gcode"), GCODE),
            ])
            .await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
        assert_eq!(
            detail(&body),
            "checkoff_units must be a JSON array of at most 64 KiB"
        );
    }
    assert_eq!(queue.counts(), before);
}
