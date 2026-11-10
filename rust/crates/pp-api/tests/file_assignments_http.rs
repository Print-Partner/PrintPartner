#![cfg(feature = "desktop_rebuild")]
//! GRE-1220 through the mounted Rust routes: print-file preview and
//! assignment on a disposable copy of a real SQLite fixture, redeeming a
//! files/content snapshot exactly once, and Node's refusals; GRE-1255's
//! upload route and the `upload_token` it issues; GRE-1256's read of a print
//! file off its printer and the drift it records.
use axum::{Router, body::Body, http::StatusCode};
use checkoff_http::{Fixture, post, post_as};
use pp_api::{
    file_assignments::file_assignments_router,
    printer_host::{Answer, Capabilities, HostAnswer, HostFile, HostStream, PrinterHostPort},
};
use pp_storage::{
    SettingsClient,
    auth::Secret,
    checkoff_progress::print_file::{
        CAPTURE_UNAVAILABLE, MAX_CLASSIFIABLE_BYTES, MAX_UPLOAD_BYTES, Scope, SnapshotStore,
        UploadRefusal, UploadStore, file_source,
    },
    checkoff_progress::{AssignedLink, Assignment, Request, attribution::Unit},
    read_model::Credential,
};
use serde_json::{Value, json};
use std::{
    collections::HashMap,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};

#[path = "support/checkoff_http.rs"]
mod checkoff_http;
#[path = "support/multipart.rs"]
mod multipart;
use multipart::multipart;
#[path = "../../pp-storage/tests/common/js_json.rs"]
mod js_json;

const EXPIRED: &str =
    "This file preview expired or its source changed. Cancel and reopen the file.";
const PATH: &str = "gcodes/gear.gcode";
const PREVIEW: &str = "/printer-checkoff/file-assignments/preview";
const ASSIGN: &str = "/printer-checkoff/file-assignments";

fn integrations(base_url: &str) -> String {
    json!([
        { "id": "sim-moonraker", "type": "moonraker", "config": { "enabled": true, "base_url": base_url } },
        { "id": "sim-bambu", "type": "bambu", "config": { "enabled": true } },
        { "id": "sim-off", "type": "moonraker", "config": { "enabled": false } },
    ])
    .to_string()
}

fn setting(fixture: &Fixture, key: &str, value: &str) {
    fixture
        .sql()
        .execute(
            "INSERT INTO app_settings (tenant_id, key, value) VALUES ('default', ?1, ?2)
             ON CONFLICT (tenant_id, key) DO UPDATE SET value=excluded.value",
            [key, value],
        )
        .unwrap();
}

/// The shared fixture with a fleet of three printers on loopback-only hosts.
fn fixture() -> Fixture {
    let fixture = Fixture::new();
    let printer = |id: &str, integration: &str| json!({ "id": id, "name": format!("Printer {id}"), "bed_height_mm": 250, "integration_id": integration });
    let fleet = json!([
        printer("sim-1", " sim-moonraker "),
        printer("sim-2", "sim-bambu"),
        printer("sim-3", "sim-off"),
    ]);
    setting(&fixture, "printer.fleet", &fleet.to_string());
    setting(
        &fixture,
        "integrations",
        &integrations("http://127.0.0.1:9"),
    );
    fixture
}

/// A printer host serving the files in its map, by storage path, with a
/// fixed modification time; it lists them as Node's `StorageListing` does.
/// While `broken` is set its listing fails, and so does every read, as a
/// stored host's read fails when its listing does (the gate lives in
/// `pp_gateway::printer_host::HostReads::open_file`; this mock mirrors it).
/// While `busy` is set every answer is `Busy`. A `declared` length replaces
/// the real one on every read.
#[derive(Default)]
struct Host {
    files: Mutex<HashMap<String, Vec<u8>>>,
    broken: AtomicBool,
    busy: AtomicBool,
    declared: Mutex<Option<u64>>,
}

const MODIFIED: &str = "2026-11-01T00:00:00.000Z";

impl Host {
    fn serve(&self, path: &str, bytes: &[u8]) {
        self.files
            .lock()
            .unwrap()
            .insert(path.into(), bytes.to_vec());
    }
}

impl PrinterHostPort for Host {
    fn capabilities(&self, _: String) -> Answer<Capabilities> {
        Box::pin(async { HostAnswer::Unsupported })
    }

    fn files(&self, _: String, path: String) -> Answer<Value> {
        if self.busy.load(Ordering::Relaxed) {
            return Box::pin(async { HostAnswer::Busy });
        }
        if self.broken.load(Ordering::Relaxed) {
            return Box::pin(async { HostAnswer::Failed("listing failed".into()) });
        }
        let entries: Vec<Value> = self
            .files
            .lock()
            .unwrap()
            .iter()
            .filter(|(file, _)| file.rsplit_once('/').map_or("", |(directory, _)| directory) == path)
            .map(|(file, bytes)| json!({ "kind": "file", "path": file, "name": file, "size_bytes": bytes.len(), "modified_at": MODIFIED }))
            .collect();
        Box::pin(async move { HostAnswer::Ok(json!({ "path": path, "entries": entries })) })
    }

    fn cameras(&self, _: String) -> Answer<Value> {
        Box::pin(async { HostAnswer::Unsupported })
    }

    fn file(&self, _: String, path: String) -> Answer<Option<HostFile>> {
        if self.broken.load(Ordering::Relaxed) {
            return Box::pin(async { HostAnswer::Failed("listing failed".into()) });
        }
        let bytes = self.files.lock().unwrap().get(&path).cloned();
        let declared = *self.declared.lock().unwrap();
        Box::pin(async move {
            HostAnswer::Ok(bytes.map(|bytes| HostFile {
                stream: HostStream {
                    status: 200,
                    content_type: None,
                    content_length: Some(declared.unwrap_or(bytes.len() as u64)),
                    body: Body::from(bytes),
                },
                name: path.rsplit('/').next().unwrap_or_default().to_owned(),
                identity: Value::Null,
            }))
        })
    }

    fn camera_view(&self, _: String, _: String) -> Answer<Option<HostStream>> {
        Box::pin(async { HostAnswer::Unsupported })
    }

    fn test(&self, _: String) -> Answer<Value> {
        Box::pin(async { HostAnswer::Unsupported })
    }

    fn tls_pin(&self, _: String) -> Answer<Value> {
        Box::pin(async { HostAnswer::Unsupported })
    }

    fn reset_tls_pin(&self, _: String) -> Answer<Value> {
        Box::pin(async { HostAnswer::Unsupported })
    }

    fn snapshot_source(&self, _: String) -> Answer<Option<String>> {
        Box::pin(async { HostAnswer::Unsupported })
    }

    fn build_exists(&self, _: i64) -> Answer<bool> {
        Box::pin(async { HostAnswer::Unsupported })
    }
}

fn open(
    fixture: &Fixture,
    flag: &str,
    store: &Arc<SnapshotStore>,
) -> (pp_storage::WriterOwner, Router) {
    hosted(fixture, flag, store, Arc::default())
}

fn hosted(
    fixture: &Fixture,
    flag: &str,
    store: &Arc<SnapshotStore>,
    host: Arc<Host>,
) -> (pp_storage::WriterOwner, Router) {
    fixture.open_with(flag, |config, owner| {
        let client = owner.checkoff_progress().unwrap();
        file_assignments_router(config, client, store.clone(), host)
    })
}

fn scope(filename: &str) -> Scope {
    Scope {
        profile_id: 1,
        printer_id: "sim-1".into(),
        remote_path: PATH.into(),
        filename: filename.into(),
    }
}

/// What the files/content route does when a client pulls a file for a Build.
fn capture(store: &Arc<SnapshotStore>, settings: &SettingsClient, bytes: &[u8]) -> String {
    let source = file_source(settings, Duration::from_secs(1), "sim-1").unwrap();
    let mut capture = store
        .begin(
            scope("gear.gcode"),
            json!({ "size_bytes": 1 }),
            source.clone(),
            source.as_deref(),
        )
        .unwrap();
    let token = capture.token().to_owned();
    capture.push(bytes).unwrap();
    capture.finish().unwrap();
    token
}

fn request(extra: Value) -> String {
    let mut body = json!({
        "profile_id": 1,
        "printer_id": " sim-1 ",
        "filename": " gear.gcode ",
        "object_names": ["gear.stl"],
    });
    body.as_object_mut()
        .unwrap()
        .extend(extra.as_object().unwrap().clone());
    body.to_string()
}

fn assignment(extra: Value) -> String {
    let mut body = json!({ "plan_revision_id": 1, "unit_tokens": ["3:0"] });
    body.as_object_mut()
        .unwrap()
        .extend(extra.as_object().unwrap().clone());
    request(body)
}

fn links(fixture: &Fixture) -> String {
    let raw: String = fixture
        .sql()
        .query_row(
            "SELECT COALESCE((SELECT value FROM app_settings WHERE tenant_id='default' AND key='printer.checkoff_links'), '[]')",
            [],
            |r| r.get(0),
        )
        .unwrap();
    raw
}

/// Whether the stored links hold `id`.
fn stored(fixture: &Fixture, id: &Value) -> bool {
    js_json::parse(&links(fixture))
        .as_array()
        .unwrap()
        .iter()
        .any(|link| link["id"] == *id)
}

async fn detail(router: &Router, path: &str, body: &str) -> (StatusCode, Value) {
    let (status, body) = post(router, path, body, true).await;
    (status, body["detail"].clone())
}

#[tokio::test]
async fn snapshot_redeems_once_for_its_exact_scope() {
    let fixture = fixture();
    let store = Arc::new(SnapshotStore::default());
    let (owner, router) = open(&fixture, "1", &store);
    let before = links(&fixture);
    let token = capture(&store, &owner.client(), b"G1 X1\n");
    let snapshot = json!({ "remote_path": PATH, "snapshot_token": format!(" {token} ") });

    let (status, preview) = post(&router, PREVIEW, &request(snapshot.clone()), true).await;
    assert_eq!(status, StatusCode::OK, "{preview}");
    assert_eq!(
        (
            &preview["inspected"],
            &preview["classification"],
            &preview["print_ready"],
            &preview["next_action"]
        ),
        (
            &json!(true),
            &json!({ "format": "gcode" }),
            &json!(true),
            &json!("")
        )
    );
    assert_eq!(preview["suggestion_basis"], "object_names");
    assert_eq!(preview["suggested_units"][0]["part_id"], 3);
    assert_eq!(preview["plan_revision_id"], 1);
    assert_eq!(links(&fixture), before, "a preview writes nothing");

    let renamed =
        json!({ "filename": "gear2.gcode", "remote_path": PATH, "snapshot_token": token });
    assert_eq!(
        detail(
            &router,
            "/api/v1/printer-checkoff/file-assignments",
            &assignment(renamed)
        )
        .await,
        (StatusCode::CONFLICT, json!(EXPIRED)),
        "bound to the file name it was captured for"
    );

    let (status, assigned) = post(
        &router,
        "/api/v1/printer-checkoff/file-assignments",
        &assignment(snapshot.clone()),
        true,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{assigned}");
    let link = &assigned["link"];
    assert_eq!(
        (
            &link["printer_id"],
            &link["integration_id"],
            &link["host_name"],
            &link["remote_path"],
            &link["state"]
        ),
        (
            &json!("sim-1"),
            &json!("sim-moonraker"),
            &json!("Printer sim-1"),
            &json!(PATH),
            &json!("watching")
        )
    );
    assert_eq!(link["classification"], json!({ "format": "gcode" }));
    assert_eq!(link["remote_identity"]["size_bytes"], 6);
    assert!(stored(&fixture, &link["id"]));

    assert_eq!(
        detail(&router, PREVIEW, &request(snapshot)).await,
        (StatusCode::CONFLICT, json!(EXPIRED)),
        "spent by the assignment"
    );
    owner.shutdown().unwrap();
}

#[tokio::test]
async fn a_changed_host_invalidates_a_held_snapshot_and_the_flag_off_mounts_nothing() {
    let fixture = fixture();
    let store = Arc::new(SnapshotStore::default());
    let (owner, router) = open(&fixture, "1", &store);
    let token = capture(&store, &owner.client(), b"G1 X2\n");
    setting(
        &fixture,
        "integrations",
        &integrations("http://127.0.0.1:10"),
    );
    let before = links(&fixture);
    let moved = json!({ "remote_path": PATH, "snapshot_token": token });
    assert_eq!(
        detail(&router, ASSIGN, &assignment(moved)).await,
        (StatusCode::CONFLICT, json!(EXPIRED))
    );
    assert_eq!(links(&fixture), before, "a refusal writes nothing");
    owner.shutdown().unwrap();

    let (owner, off) = open(&fixture, "0", &store);
    let (status, _) = post(&off, ASSIGN, &assignment(json!({})), true).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    owner.shutdown().unwrap();
}

/// Refusals both routes share, in Node's order.
fn shared_refusals() -> Vec<(Value, u16, &'static str)> {
    vec![
        (json!({ "profile_id": 0 }), 400, "profile_id is required"),
        (
            json!({ "filename": "gear.stl" }),
            400,
            "Choose a .gcode, .gco, .bgcode, or .3mf print file",
        ),
        (
            json!({ "remote_path": PATH, "upload_token": "u" }),
            400,
            "Send remote_path or upload_token, not both",
        ),
        (
            json!({ "snapshot_token": "t" }),
            400,
            "A snapshot token requires its original printer file path",
        ),
        (
            json!({ "remote_path": PATH, "snapshot_token": null }),
            400,
            "A snapshot token requires its original printer file path",
        ),
        (json!({ "printer_id": "nope" }), 404, "Printer not found"),
        (
            json!({ "printer_id": "", "tracking": "host" }),
            400,
            "PrintPartner cannot watch a printer it does not manage. Name the printer that made this print, or record the print as already made.",
        ),
        (
            json!({ "printer_id": "", "remote_path": PATH }),
            400,
            "A file held on a printer needs the printer it sits on. Name that printer, or upload the file instead.",
        ),
        (
            json!({ "printer_id": "sim-3" }),
            409,
            "Use manual tracking because this printer has no available host",
        ),
        (
            json!({ "object_names": [1] }),
            400,
            "Import supports at most 500 object names of up to 200 characters; no objects were imported",
        ),
        (
            json!({ "upload_token": "gone" }),
            409,
            "That upload is no longer available; upload the print file again",
        ),
    ]
}

#[tokio::test]
async fn both_routes_refuse_in_nodes_order_and_write_nothing() {
    let fixture = fixture();
    let store = Arc::new(SnapshotStore::default());
    let (owner, router) = open(&fixture, "true", &store);
    let before = links(&fixture);
    for (extra, status, expected) in shared_refusals() {
        for path in [PREVIEW, ASSIGN] {
            assert_eq!(
                detail(&router, path, &assignment(extra.clone())).await,
                (StatusCode::from_u16(status).unwrap(), json!(expected)),
                "{path} {extra}"
            );
        }
    }
    let assign_only = [
        (
            json!({ "plan_revision_id": "x" }),
            "plan_revision_id is required",
        ),
        (
            json!({ "unit_tokens": null }),
            "unit_tokens is required; confirm the Required units this file produces",
        ),
        (
            json!({ "unit_tokens": ["3"] }),
            "unit_tokens holds an unreadable unit",
        ),
        (
            json!({ "object_mappings": {} }),
            "object_mappings must be a list of at most 500 choices",
        ),
        (
            json!({ "object_mappings": [{ "object_index": 0, "part_id": "3", "unit_index": 0 }] }),
            "object_mappings contains an invalid choice",
        ),
    ];
    for (extra, expected) in assign_only {
        assert_eq!(
            detail(&router, ASSIGN, &assignment(extra.clone())).await,
            (StatusCode::BAD_REQUEST, json!(expected)),
            "{extra}"
        );
    }
    assert_eq!(
        detail(
            &router,
            ASSIGN,
            &assignment(json!({ "filename": "gear.3mf" }))
        )
        .await,
        (
            StatusCode::CONFLICT,
            json!(
                "PrintPartner has to read a 3MF to tell whether it holds printer instructions; upload the file so PrintPartner can read it"
            )
        )
    );
    assert_eq!(links(&fixture), before, "no refusal writes");
    owner.shutdown().unwrap();
}

#[tokio::test]
async fn a_file_nothing_can_read_is_recorded_unread() {
    let fixture = fixture();
    let store = Arc::new(SnapshotStore::default());
    let (owner, router) = open(&fixture, "true", &store);
    let (status, unread) = post(
        &router,
        PREVIEW,
        &request(json!({ "printer_id": "sim-2", "remote_path": PATH })),
        true,
    )
    .await;
    assert_eq!(
        (status, &unread["inspected"]),
        (StatusCode::OK, &json!(false))
    );
    assert!(unread.get("classification").is_none());
    let (status, manual) = post(
        &router,
        ASSIGN,
        &assignment(json!({ "tracking": "manual", "remote_path": PATH, "completed": true })),
        true,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{manual}");
    assert_eq!(
        (
            &manual["link"]["integration_id"],
            &manual["link"]["state"],
            manual["link"].get("classification")
        ),
        (&json!("manual:sim-1"), &json!("awaiting_verify"), None)
    );
    let (status, unmanaged) = post(
        &router,
        ASSIGN,
        &assignment(json!({ "printer_id": null, "filename": "other.gcode" })),
        true,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{unmanaged}");
    assert_eq!(
        (
            &unmanaged["link"]["printer_id"],
            &unmanaged["link"]["integration_id"]
        ),
        (&json!("unmanaged"), &json!("manual:unmanaged"))
    );
    owner.shutdown().unwrap();
}

#[test]
fn one_capture_at_a_time_and_a_dropped_capture_releases_the_store() {
    let store = Arc::new(SnapshotStore::default());
    let source = Some("s".to_owned());
    assert_eq!(
        store.begin(scope("a.gcode"), json!({}), None, None).err(),
        Some(CAPTURE_UNAVAILABLE),
        "no linked host, no capture"
    );
    assert_eq!(
        store
            .begin(scope("a.gcode"), json!({}), source.clone(), Some("t"))
            .err(),
        Some(CAPTURE_UNAVAILABLE),
        "the host changed while the file was requested"
    );
    let first = store
        .begin(scope("a.gcode"), json!({}), source.clone(), Some("s"))
        .unwrap();
    assert_eq!(
        store
            .begin(scope("b.gcode"), json!({}), source.clone(), Some("s"))
            .err(),
        Some(CAPTURE_UNAVAILABLE)
    );
    let token = first.token().to_owned();
    drop(first);
    assert!(
        store.read(&token, &scope("a.gcode"), Some("s")).is_none(),
        "never finished"
    );
    let mut second = store
        .begin(scope("b.gcode"), json!({}), source.clone(), Some("s"))
        .unwrap();
    second.push(b"not a print file").unwrap();
    assert_eq!(second.finish(), Err("Printer file could not be inspected"));
    let mut third = store
        .begin(scope("c.gcode"), json!({ "mtime": 1 }), source, Some("s"))
        .unwrap();
    let token = third.token().to_owned();
    third.push(b"G28\n").unwrap();
    third.finish().unwrap();
    let held = store.read(&token, &scope("c.gcode"), Some("s")).unwrap();
    assert_eq!(held.identity["mtime"], 1);
    assert_eq!(held.identity["size_bytes"], 4);
    assert!(store.read(&token, &scope("c.gcode"), None).is_none());
    let reserved = store.reserve(&token, &scope("c.gcode"), Some("s")).unwrap();
    assert!(
        store
            .reserve(&token, &scope("c.gcode"), Some("s"))
            .is_none(),
        "one assignment at a time holds a token"
    );
    assert!(store.read(&token, &scope("c.gcode"), Some("s")).is_none());
    drop(reserved);
    let reserved = store.reserve(&token, &scope("c.gcode"), Some("s")).unwrap();
    assert_eq!(reserved.inspected().unwrap().identity["size_bytes"], 4);
    reserved.spend();
    assert!(store.read(&token, &scope("c.gcode"), Some("s")).is_none());
}

#[test]
fn a_reserved_snapshot_still_counts_toward_the_128_token_limit() {
    let store = Arc::new(SnapshotStore::default());
    let source = || Some("s".to_owned());
    let tokens: Vec<String> = (0..128)
        .map(|index| {
            let mut capture = store
                .begin(
                    scope(&format!("{index}.gcode")),
                    json!({}),
                    source(),
                    Some("s"),
                )
                .unwrap();
            capture.push(b"G28\n").unwrap();
            let token = capture.token().to_owned();
            capture.finish().unwrap();
            token
        })
        .collect();
    let full = || {
        store
            .begin(scope("x.gcode"), json!({}), source(), Some("s"))
            .err()
    };
    assert_eq!(full(), Some(CAPTURE_UNAVAILABLE));
    let held = store
        .reserve(&tokens[0], &scope("0.gcode"), Some("s"))
        .unwrap();
    assert_eq!(
        full(),
        Some(CAPTURE_UNAVAILABLE),
        "a reservation keeps its slot"
    );
    drop(held);
    assert_eq!(
        full(),
        Some(CAPTURE_UNAVAILABLE),
        "a failed assignment puts it back"
    );
    store
        .reserve(&tokens[0], &scope("0.gcode"), Some("s"))
        .unwrap()
        .spend();
    assert_eq!(full(), None, "a spent token frees its slot");
}

/// The assignment transaction itself rechecks the host a snapshot was
/// redeemed against, so a settings write between the route's read and the
/// write cannot record stale evidence.
#[tokio::test]
async fn the_assignment_transaction_refuses_a_host_changed_since_redemption() {
    let fixture = fixture();
    let store = Arc::new(SnapshotStore::default());
    let (owner, _router) = open(&fixture, "1", &store);
    let stale = file_source(&owner.client(), Duration::from_secs(1), "sim-1").unwrap();
    setting(
        &fixture,
        "integrations",
        &integrations("http://127.0.0.1:10"),
    );
    let current = file_source(&owner.client(), Duration::from_secs(1), "sim-1").unwrap();
    assert_ne!(stale, current);
    let apply = |source: Option<String>| {
        let assignment = Assignment {
            profile_id: 1,
            plan_revision_id: 1,
            object_names: vec!["gear.stl".into()],
            units: vec![Unit::at(3, 0)],
            mappings: None,
            retain_unmatched: false,
            completed: false,
            link: AssignedLink {
                integration_id: "sim-moonraker".into(),
                printer_id: "sim-1".into(),
                host_name: "Printer sim-1".into(),
                filename: "gear.gcode".into(),
                remote_path: Some(PATH.into()),
                remote_identity: None,
                classification: None,
            },
            snapshot_source: source,
        };
        owner
            .checkoff_progress()
            .unwrap()
            .apply(
                Credential::Session(Secret::new("required-unit-fixture-secret".into())),
                Request::AssignFile(Box::new(assignment)),
                &AtomicBool::new(false),
                Duration::from_secs(5),
            )
            .unwrap()
    };
    let before = links(&fixture);
    let refused = serde_json::to_value(apply(stale)).unwrap();
    assert_eq!(
        (&refused["status"], &refused["body"]["detail"]),
        (&json!(409), &json!(EXPIRED))
    );
    assert_eq!(links(&fixture), before);
    assert_eq!(apply(current).status, 200);
    owner.shutdown().unwrap();
}

const UPLOAD: &str = "/printer-checkoff/file-assignments/upload";
const GONE: &str = "That upload is no longer available; upload the print file again";

async fn upload(router: &Router, parts: &[(&str, Option<&str>, &[u8])]) -> (StatusCode, Value) {
    let (content_type, body) = multipart(parts);
    post_as(router, UPLOAD, &content_type, body).await
}

type Parts<'a> = Vec<(&'a str, Option<&'a str>, &'a [u8])>;

/// A fixture with the routes mounted, for the upload tests.
fn mounted(flag: &str) -> (Fixture, pp_storage::WriterOwner, Router) {
    let fixture = fixture();
    let (owner, router) = open(&fixture, flag, &Arc::new(SnapshotStore::default()));
    (fixture, owner, router)
}

fn upload_gcode(bytes: &[u8]) -> Parts<'_> {
    vec![
        ("profile_id", None, b"1"),
        ("object_names", None, br#"[" gear.stl ", 7]"#),
        ("file", Some(r"C:\prints\ gear.gcode "), bytes),
    ]
}

#[tokio::test]
async fn an_upload_is_held_for_its_build_and_spent_by_one_assignment() {
    let (fixture, owner, router) = mounted("1");
    let before = links(&fixture);

    let (status, held) = upload(&router, &upload_gcode(b"G1 X1\n")).await;
    assert_eq!(status, StatusCode::OK, "{held}");
    assert_eq!(
        (
            &held["inspected"],
            &held["classification"],
            &held["print_ready"],
            &held["next_action"],
            &held["suggestion_basis"],
            &held["suggested_units"][0]["part_id"],
            &held["plan_revision_id"],
        ),
        (
            &json!(true),
            &json!({ "format": "gcode" }),
            &json!(true),
            &json!(""),
            &json!("object_names"),
            &json!(3),
            &json!(1)
        )
    );
    assert_eq!(links(&fixture), before, "an upload writes nothing");
    let token = held["upload_token"].as_str().unwrap().to_owned();
    let unmanaged = |extra: Value| {
        let mut body = json!({ "printer_id": null, "upload_token": format!(" {token} ") });
        body.as_object_mut()
            .unwrap()
            .extend(extra.as_object().unwrap().clone());
        assignment(body)
    };

    let (status, preview) = post(&router, PREVIEW, &unmanaged(json!({})), true).await;
    assert_eq!(status, StatusCode::OK, "{preview}");
    assert_eq!(preview["classification"], json!({ "format": "gcode" }));

    let (status, failed) = post(
        &router,
        ASSIGN,
        &unmanaged(json!({ "plan_revision_id": 2 })),
        true,
    )
    .await;
    assert!(status.is_client_error(), "{status} {failed}");
    assert_eq!(
        links(&fixture),
        before,
        "a failed assignment writes nothing"
    );

    let (status, assigned) = post(&router, ASSIGN, &unmanaged(json!({})), true).await;
    assert_eq!(
        status,
        StatusCode::OK,
        "a failed assignment keeps the upload: {assigned}"
    );
    assert_eq!(
        assigned["link"]["classification"],
        json!({ "format": "gcode" })
    );
    assert!(stored(&fixture, &assigned["link"]["id"]));

    assert_eq!(
        detail(
            &router,
            ASSIGN,
            &unmanaged(json!({ "filename": "again.gcode" }))
        )
        .await,
        (StatusCode::CONFLICT, json!(GONE)),
        "a recorded upload is spent"
    );
    owner.shutdown().unwrap();
}

/// Upload refusals in Node's order, each with its status and detail.
fn upload_refusals() -> Vec<(Parts<'static>, u16, &'static str)> {
    let gcode: &[u8] = b"G28\n";
    vec![
        (
            vec![("file", Some("a.gcode"), gcode)],
            400,
            "profile_id is required",
        ),
        (
            vec![
                ("profile_id", None, b"1.5"),
                ("file", Some("a.gcode"), gcode),
            ],
            400,
            "profile_id is required",
        ),
        (
            vec![
                ("profile_id", None, b"1"),
                ("other", Some("a.gcode"), gcode),
            ],
            400,
            "Attach the print file as the file part",
        ),
        (
            vec![("profile_id", None, b"1"), ("file", Some("dir/ "), gcode)],
            400,
            "filename is required",
        ),
        (
            vec![
                ("profile_id", None, b"1"),
                ("file", Some("dir/a.gcode/"), gcode),
            ],
            400,
            "filename is required",
        ),
        (
            vec![("profile_id", None, b"1"), ("file", Some("dir/.."), gcode)],
            400,
            "filename is required",
        ),
        (
            vec![("constructor", None, b"x"), ("profile_id", None, b"1")],
            400,
            "That upload could not be read",
        ),
        (
            vec![
                ("toString", None, b"x"),
                ("file", Some("a.gcode"), gcode),
                ("file", Some("b.gcode"), gcode),
            ],
            400,
            "Upload one print file at a time",
        ),
        (
            vec![("profile_id", None, b"1"), ("file", Some("a.stl"), gcode)],
            400,
            "Choose a .gcode, .gco, .bgcode, or .3mf print file",
        ),
        (
            vec![
                ("profile_id", None, b"1"),
                ("file", Some("a.gcode"), gcode),
                ("file", Some("b.gcode"), gcode),
            ],
            400,
            "Upload one print file at a time",
        ),
        (
            vec![
                ("a", None, b""),
                ("b", None, b""),
                ("c", None, b""),
                ("d", None, b""),
                ("profile_id", None, b"1"),
            ],
            400,
            "That upload could not be read",
        ),
        (
            vec![
                ("profile_id", None, b"999"),
                ("file", Some("a.gcode"), gcode),
            ],
            500,
            "Internal Server Error",
        ),
    ]
}

#[tokio::test]
async fn the_upload_route_refuses_in_nodes_order() {
    let (fixture, owner, router) = mounted("true");
    let before = links(&fixture);
    let cases = upload_refusals();
    // Both mounts, so the cases stay inside each one's 10-a-minute window.
    for (index, (parts, status, expected)) in cases.into_iter().enumerate() {
        let (content_type, body) = multipart(&parts);
        let path = ["", "/api/v1"][index % 2];
        let (got, body) = post_as(&router, &format!("{path}{UPLOAD}"), &content_type, body).await;
        assert_eq!(
            (got, &body["detail"]),
            (StatusCode::from_u16(status).unwrap(), &json!(expected)),
            "{expected}"
        );
    }
    let (status, rejected) = upload(
        &router,
        &[
            ("profile_id", None, b"1"),
            ("file", Some("a.gcode"), b"\0\0binary"),
        ],
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "{rejected}");
    let octets = concat!(
        "--pp\r\nContent-Disposition: form-data; name=\"profile_id\"\r\n\r\n1\r\n",
        "--pp\r\nContent-Disposition: form-data; name=\"file\"\r\n",
        "Content-Type: Application/Octet-Stream; charset=binary\r\n\r\nG28\n\r\n--pp--\r\n",
    );
    let (status, unnamed) = post_as(
        &router,
        UPLOAD,
        "multipart/form-data; boundary=pp",
        octets.as_bytes().to_vec(),
    )
    .await;
    assert_eq!(
        (status, &unnamed["detail"]),
        (StatusCode::BAD_REQUEST, &json!("filename is required")),
        "an octet-stream part is a file without a name"
    );
    let (status, json) = post(&router, UPLOAD, "{}", true).await;
    assert_eq!(
        (status, &json["detail"]),
        (
            StatusCode::UNSUPPORTED_MEDIA_TYPE,
            &json!("Send the print file as multipart/form-data")
        )
    );
    assert_eq!(links(&fixture), before, "no refusal writes");
    owner.shutdown().unwrap();
}

#[tokio::test]
async fn uploads_up_to_64_mb_are_held_to_two_and_the_route_takes_ten_a_minute() {
    let (_fixture, owner, router) = mounted("1");
    let full = b"G28\n".repeat(MAX_UPLOAD_BYTES / 4);
    for _ in 0..2 {
        let (status, body) = upload(&router, &upload_gcode(&full)).await;
        assert_eq!(status, StatusCode::OK, "{body}");
    }
    let (status, body) = upload(&router, &upload_gcode(b"G28\n")).await;
    assert_eq!(
        (status, &body["detail"]),
        (
            StatusCode::SERVICE_UNAVAILABLE,
            &json!(
                "PrintPartner is holding as many uploaded print files as it will; record one of them, then try again"
            )
        )
    );
    let mut big = full;
    big.push(b'\n');
    let (status, body) = upload(
        &router,
        &[
            ("profile_id", None, b"1"),
            ("file", Some("big.gcode"), &big),
        ],
    )
    .await;
    assert_eq!(
        (status, &body["detail"]),
        (
            StatusCode::PAYLOAD_TOO_LARGE,
            &json!("That print file is larger than the 64 MB PrintPartner will take as an upload")
        )
    );
    let (status, body) = upload(
        &router,
        &[
            ("profile_id", None, b"1"),
            ("valueOf", Some("big.gcode"), &big),
        ],
    )
    .await;
    assert_eq!(
        (status, &body["detail"]),
        (
            StatusCode::BAD_REQUEST,
            &json!("That upload could not be read")
        ),
        "Node drains a prototype-named part without its size limit"
    );
    for _ in 0..5 {
        let (status, _) = post(&router, UPLOAD, "{}", true).await;
        assert_eq!(status, StatusCode::UNSUPPORTED_MEDIA_TYPE);
    }
    let (status, _) = post(&router, UPLOAD, "{}", true).await;
    assert_eq!(status, StatusCode::TOO_MANY_REQUESTS);
    let junk = Some("pp_session=junk");
    let (status, _) =
        checkoff_http::loopback::send(&router, "POST", UPLOAD, Some("{}"), junk).await;
    assert_eq!(
        status,
        StatusCode::UNAUTHORIZED,
        "a bad credential is refused before the window and the body"
    );
    let (status, _) = post(&router, &format!("/api/v1{UPLOAD}"), "{}", true).await;
    assert_eq!(
        status,
        StatusCode::UNSUPPORTED_MEDIA_TYPE,
        "each mount counts alone"
    );
    owner.shutdown().unwrap();
}

#[test]
fn held_uploads_are_bound_to_their_build_and_capped_at_two_files() {
    let uploads = Arc::new(UploadStore::default());
    let (token, inspected) = uploads.hold(1, b"G28\n").unwrap();
    assert_eq!(inspected.identity["size_bytes"], 4);
    assert!(uploads.read(&token, 2).is_none(), "another Build's token");
    assert!(uploads.reserve(&token, 2).is_none());
    let held = uploads.reserve(&token, 1).unwrap();
    assert!(
        uploads.reserve(&token, 1).is_none(),
        "one assignment at a time"
    );
    assert!(
        uploads.read(&token, 1).is_some(),
        "a preview still reads a held upload"
    );
    drop(held);
    assert!(
        uploads.read(&token, 1).is_some(),
        "a failed assignment puts it back"
    );
    uploads.reserve(&token, 1).unwrap().spend();
    assert!(uploads.read(&token, 1).is_none(), "spent");

    let mut big = b"G28\n".repeat(MAX_UPLOAD_BYTES / 4);
    big.truncate(MAX_UPLOAD_BYTES);
    let (first, _) = uploads.hold(1, &big).unwrap();
    let held = uploads.reserve(&first, 1).unwrap();
    uploads.hold(1, &big).unwrap();
    assert_eq!(
        uploads.hold(1, b"G28\n").err(),
        Some(UploadRefusal::Full),
        "a reservation still counts"
    );
    held.spend();
    uploads.hold(1, b"G28\n").unwrap();
}

/// The stored link with `id`.
fn link(fixture: &Fixture, id: &Value) -> Value {
    js_json::parse(&links(fixture))
        .as_array()
        .unwrap()
        .iter()
        .find(|link| link["id"] == *id)
        .cloned()
        .unwrap()
}

#[tokio::test]
async fn a_printer_file_is_read_classified_and_recorded_with_its_identity() {
    let fixture = fixture();
    let host = Arc::new(Host::default());
    host.serve(PATH, b"G1 X1\n");
    host.serve("gcodes/bad.gcode", b"\x00\x01binary");
    let (owner, router) = hosted(&fixture, "1", &Arc::default(), host.clone());
    let remote = json!({ "remote_path": PATH });

    let (status, preview) = post(&router, PREVIEW, &request(remote.clone()), true).await;
    assert_eq!(status, StatusCode::OK, "{preview}");
    assert_eq!(
        (&preview["inspected"], &preview["classification"]),
        (&json!(true), &json!({ "format": "gcode" }))
    );
    let missing = json!({ "remote_path": "gcodes/missing.gcode" });
    let (status, unread) = post(&router, PREVIEW, &request(missing), true).await;
    assert_eq!(
        (status, &unread["inspected"]),
        (StatusCode::OK, &json!(false)),
        "a file the host does not serve is unread"
    );
    host.broken.store(true, Ordering::Relaxed);
    let (status, unlisted) = post(&router, PREVIEW, &request(remote.clone()), true).await;
    assert_eq!(
        (status, &unlisted["inspected"]),
        (StatusCode::OK, &json!(false)),
        "the route reads a failed host read as unread"
    );
    host.broken.store(false, Ordering::Relaxed);
    host.busy.store(true, Ordering::Relaxed);
    let (status, _) = post(&router, PREVIEW, &request(remote.clone()), true).await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE, "busy settings");
    host.busy.store(false, Ordering::Relaxed);
    *host.declared.lock().unwrap() = Some(MAX_CLASSIFIABLE_BYTES as u64 + 1);
    assert_eq!(
        detail(&router, PREVIEW, &request(remote.clone())).await,
        (
            StatusCode::CONFLICT,
            json!("That print file is too large to inspect")
        ),
        "a declared length past the limit is refused before the body is read"
    );
    *host.declared.lock().unwrap() = None;
    let bad = json!({ "remote_path": "gcodes/bad.gcode", "filename": "bad.gcode" });
    assert_eq!(
        detail(&router, ASSIGN, &assignment(bad)).await,
        (
            StatusCode::CONFLICT,
            json!("Those bytes are not G-code, binary G-code, or a 3MF package")
        )
    );

    let (status, assigned) = post(&router, ASSIGN, &assignment(remote), true).await;
    assert_eq!(status, StatusCode::OK, "{assigned}");
    let identity = &link(&fixture, &assigned["link"]["id"])["remote_identity"];
    assert_eq!(
        (&identity["size_bytes"], &identity["modified_at"]),
        (&json!(6), &json!(MODIFIED))
    );
    assert_eq!(identity["sha256"].as_str().map(str::len), Some(64));
    owner.shutdown().unwrap();
}

#[tokio::test]
async fn reading_a_changed_printer_file_records_drift_on_the_link_that_watches_it() {
    let fixture = fixture();
    let host = Arc::new(Host::default());
    host.serve(PATH, b"G1 X1\n");
    let (owner, router) = hosted(&fixture, "1", &Arc::default(), host.clone());
    let remote = assignment(json!({ "remote_path": PATH }));
    let (status, assigned) = post(&router, ASSIGN, &remote, true).await;
    assert_eq!(status, StatusCode::OK, "{assigned}");
    let id = &assigned["link"]["id"];
    let taken = (
        StatusCode::CONFLICT,
        json!("That print file is already assigned"),
    );

    host.serve(PATH, b"G1 X2\n");
    assert_eq!(detail(&router, ASSIGN, &remote).await, taken);
    assert_eq!(
        link(&fixture, id)["remote_drift"]["reason"],
        "content",
        "the observation stands though the assignment was refused"
    );

    host.serve(PATH, b"G1 X1\n");
    assert_eq!(detail(&router, ASSIGN, &remote).await, taken);
    assert!(
        link(&fixture, id).get("remote_drift").is_none(),
        "a restored file clears the drift"
    );
    owner.shutdown().unwrap();
}
