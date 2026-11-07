#![cfg(feature = "desktop_rebuild")]
//! GRE-1220 through the mounted Rust routes: print-file preview and
//! assignment on a disposable copy of a real SQLite fixture, redeeming a
//! files/content snapshot exactly once, and Node's refusals.
use axum::{Router, http::StatusCode};
use checkoff_http::{Fixture, post};
use pp_api::file_assignments::file_assignments_router;
use pp_storage::{
    SettingsClient,
    auth::Secret,
    checkoff_progress::print_file::{CAPTURE_UNAVAILABLE, Scope, SnapshotStore, file_source},
    checkoff_progress::{AssignedLink, Assignment, Request, attribution::Unit},
    read_model::Credential,
};
use serde_json::{Value, json};
use std::{
    sync::{Arc, atomic::AtomicBool},
    time::Duration,
};

#[path = "support/checkoff_http.rs"]
mod checkoff_http;
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

fn open(
    fixture: &Fixture,
    flag: &str,
    store: &Arc<SnapshotStore>,
) -> (pp_storage::WriterOwner, Router) {
    fixture.open_with(flag, |config, owner| {
        file_assignments_router(config, owner.checkoff_progress().unwrap(), store.clone())
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
        (
            json!({ "remote_path": PATH }),
            501,
            "Reading a print file off a printer is not available in this runtime yet; reopen the file from its preview",
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
