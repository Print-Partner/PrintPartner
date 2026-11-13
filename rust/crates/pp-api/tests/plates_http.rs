#![cfg(feature = "desktop_rebuild")]
//! GRE-1189, GRE-1184: the Plate routes through `plates_router` on a disposable
//! copy of the Node-written `accepted-plate-node.db`: admission, the flag,
//! the root and `/api/v2` aliases, and Node's `{ detail, code }` failures.
//! GRE-1196: the Checkoff units read names each unit's head Plate.
//! GRE-419: initialize and arrange, and Node's body limits.
//! GRE-421: the Production setup read and command, at the root only.
use axum::{Router, http::StatusCode};
use loopback::ORIGIN;
use pp_api::{
    accepted::{AcceptedHttpConfig, accepted_router},
    plates::plates_router,
};
use pp_storage::{Limits, WriterOwner};
use serde_json::{Value, json};
use std::path::Path;

#[path = "support/loopback.rs"]
mod loopback;

const TOKEN: &str = "ppu_574c8d5f92624f029e21705a8ebb8a1b";

fn open(root: &Path, flag: &str) -> (WriterOwner, Router) {
    let owner = WriterOwner::open(root, Limits::default()).unwrap().0;
    let policy = pp_storage::auth::AuthPolicy {
        registration: pp_storage::auth::RegistrationPolicy::Open,
        session_tenant: pp_storage::auth::SessionTenantPolicy::AccountTenant,
        first_user: pp_storage::auth::FirstUserTenant::NewUser,
    };
    let config = AcceptedHttpConfig::from_startup(ORIGIN, "default", Some(flag)).unwrap();
    let router = plates_router(config, owner.accepted_plates(policy).unwrap());
    (owner, router)
}

/// A disposable data dir holding a copy of the Node-written fixture.
fn data_dir() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join("repos/1/revisions/accepted")).unwrap();
    std::fs::write(
        dir.path().join("print-partner.db"),
        include_bytes!("../../pp-storage/tests/fixtures/accepted-plate-node.db"),
    )
    .unwrap();
    dir
}

async fn send(
    router: &Router,
    method: &str,
    path: &str,
    body: &Value,
    cookie: bool,
) -> (StatusCode, Value) {
    let cookie = cookie.then_some("pp_session=read-fixture-secret");
    loopback::send(router, method, path, Some(&body.to_string()), cookie).await
}

async fn get(router: &Router, path: &str, cookie: bool) -> (StatusCode, Value) {
    send(router, "GET", path, &Value::Null, cookie).await
}

/// Removes every Plate head, so the Build has no Plate revision.
fn drop_heads(root: &Path) {
    rusqlite::Connection::open(root.join("print-partner.db"))
        .unwrap()
        .execute("DELETE FROM accepted_plate_heads", [])
        .unwrap();
}

/// The fixture unit's entry in `GET /api/v2/plans/1/checkoff/units`, and the
/// head Plate revision the read names.
async fn checkoff_unit(root: &Path) -> (Value, Value) {
    let owner = WriterOwner::open(root, Limits::default()).unwrap().0;
    let config = AcceptedHttpConfig::from_startup(ORIGIN, "default", Some("1")).unwrap();
    let router = accepted_router(config, owner.accepted_reads().unwrap());
    let (status, body) = get(&router, "/api/v2/plans/1/checkoff/units", true).await;
    owner.shutdown().unwrap();
    assert_eq!(status, StatusCode::OK, "{body}");
    let unit = body["parts"]
        .as_array()
        .unwrap()
        .iter()
        .flat_map(|part| part["units"].as_array().unwrap())
        .find(|unit| unit["token"] == TOKEN)
        .unwrap()
        .clone();
    (body["plate_revision_id"].clone(), unit["plate"].clone())
}

#[tokio::test]
async fn checkoff_units_http_names_each_units_head_plate() {
    let dir = data_dir();
    assert_eq!(
        checkoff_unit(dir.path()).await,
        (
            json!(1),
            json!({ "plate_id": "fixture-plate", "ordinal": 1, "printer_name": "Fixture Printer" })
        )
    );
    drop_heads(dir.path());
    assert_eq!(checkoff_unit(dir.path()).await, (Value::Null, Value::Null));
}

#[tokio::test]
async fn plates_http_read_answers_node_failures_on_both_aliases() {
    let dir = data_dir();
    let root = dir.path();

    let (owner, router) = open(root, "1");
    assert_eq!(
        get(&router, "/plans/1/plates", false).await.0,
        StatusCode::UNAUTHORIZED
    );
    let infinite = format!("/plans/{}/plates", "9".repeat(320));
    for (path, status, code, detail) in [
        (
            "/plans/x/plates",
            StatusCode::BAD_REQUEST,
            "invalid_request",
            "Request is invalid",
        ),
        (
            infinite.as_str(),
            StatusCode::BAD_REQUEST,
            "invalid_request",
            "Request is invalid",
        ),
        (
            "/api/v2/plans/2/plates",
            StatusCode::NOT_FOUND,
            "profile_not_found",
            "Plan not found",
        ),
        // Node's contract parser refuses the fixture's legacy Plate id.
        (
            "/api/v2/plans/1/plates",
            StatusCode::INTERNAL_SERVER_ERROR,
            "internal_error",
            "Accepted Plate data is inconsistent",
        ),
    ] {
        let (actual, body) = get(&router, path, true).await;
        assert_eq!(
            (actual, &body["code"], &body["detail"]),
            (status, &json!(code), &json!(detail)),
            "{path}"
        );
    }
    owner.shutdown().unwrap();

    drop_heads(root);
    let (owner, router) = open(root, "true");
    let (status, setup) = get(&router, "/plans/1/plates", true).await;
    assert_eq!(
        (status, &setup["kind"]),
        (StatusCode::OK, &json!("setup")),
        "{setup}"
    );
    owner.shutdown().unwrap();

    let (owner, router) = open(root, "0");
    assert_eq!(
        get(&router, "/plans/1/plates", true).await.0,
        StatusCode::NOT_FOUND
    );
    owner.shutdown().unwrap();
}

#[tokio::test]
async fn plates_http_moves_a_unit_once_and_answers_node_failures() {
    let dir = data_dir();
    let root = dir.path().to_path_buf();
    let basis: Value = rusqlite::Connection::open(root.join("print-partner.db"))
        .unwrap()
        .query_row(
            "SELECT plan_version, plan_revision_id, plan_revision_digest, required_unit_mapping_digest FROM accepted_plate_revisions WHERE id=1",
            [],
            |row| {
                Ok(json!({
                    "profile_id": 1,
                    "plan_version": row.get::<_, i64>(0)?,
                    "plan_revision_id": row.get::<_, i64>(1)?,
                    "plan_revision_digest": row.get::<_, String>(2)?,
                    "required_unit_mapping_digest": row.get::<_, String>(3)?,
                }))
            },
        )
        .unwrap();
    let body = |revision: i64| json!({"expected": basis, "expected_plate_revision_id": revision, "x_um": 6000, "y_um": 6000});
    let path = format!("/plans/1/plates/fixture-plate/units/{TOKEN}");

    let (owner, router) = open(&root, "1");
    assert_eq!(
        send(&router, "PATCH", &path, &body(1), false).await.0,
        StatusCode::UNAUTHORIZED
    );
    let (status, refused) = send(&router, "PATCH", &path, &body(2), true).await;
    assert_eq!(status, StatusCode::CONFLICT, "{refused}");
    assert_eq!(
        (&refused["detail"], &refused["code"]),
        (
            &json!("Plate revision changed"),
            &json!("plate_revision_changed")
        )
    );
    let (status, invalid) = send(
        &router,
        "PATCH",
        "/plans/x/plates/fixture-plate/units/x",
        &body(1),
        true,
    )
    .await;
    assert_eq!(
        (status, &invalid["code"]),
        (StatusCode::BAD_REQUEST, &json!("invalid_request"))
    );
    let (status, moved) = send(&router, "PATCH", &format!("/api/v2{path}"), &body(1), true).await;
    assert_eq!(
        (status, moved),
        (
            StatusCode::OK,
            json!({"plate_revision_id": 2, "plate_revision_number": 2})
        )
    );
    let (status, read) = send(&router, "GET", "/plans/1/plates", &Value::Null, true).await;
    assert_eq!(
        (status, &read["code"]),
        (StatusCode::INTERNAL_SERVER_ERROR, &json!("internal_error")),
        "Node's contract parser refuses the fixture's legacy Plate id"
    );
    owner.shutdown().unwrap();

    let (owner, router) = open(&root, "true");
    let (status, again) = send(&router, "PATCH", &path, &body(2), true).await;
    assert_eq!(
        (status, again),
        (
            StatusCode::OK,
            json!({"plate_revision_id": 2, "plate_revision_number": 2})
        ),
        "the same move after a restart changes nothing"
    );
    owner.shutdown().unwrap();

    let (owner, router) = open(&root, "0");
    assert_eq!(
        send(&router, "PATCH", &path, &body(2), true).await.0,
        StatusCode::NOT_FOUND
    );
    owner.shutdown().unwrap();
}

/// The `no_head` phase of Node's initialize golden on a fresh data dir: its
/// STL files, re-pinned digests and fleet, and its `creates_head` body.
fn initialize_fixture(root: &Path) -> Value {
    let golden: Value = serde_json::from_str(include_str!(
        "../../../../fixtures/parity/plates-initialize/node.json"
    ))
    .unwrap();
    let phase = golden["phases"]
        .as_array()
        .unwrap()
        .iter()
        .find(|phase| phase["name"] == "no_head")
        .unwrap();
    for file in phase["files"].as_array().unwrap() {
        std::fs::write(
            root.join(file["path"].as_str().unwrap()),
            file["text"].as_str().unwrap(),
        )
        .unwrap();
    }
    let sql = rusqlite::Connection::open(root.join("print-partner.db")).unwrap();
    let triggers: Vec<String> = sql
        .prepare("SELECT name FROM sqlite_schema WHERE type='trigger'")
        .unwrap()
        .query_map([], |row| row.get(0))
        .unwrap()
        .map(Result::unwrap)
        .collect();
    for trigger in triggers {
        sql.execute_batch(&format!("DROP TRIGGER \"{trigger}\""))
            .unwrap();
    }
    sql.execute_batch(&format!(
        "PRAGMA foreign_keys=OFF; {}",
        phase["seed"].as_str().unwrap()
    ))
    .unwrap();
    phase["cases"]
        .as_array()
        .unwrap()
        .iter()
        .find(|case| case["id"] == "creates_head")
        .unwrap()["body"]
        .clone()
}

#[tokio::test]
async fn plates_http_initializes_then_arranges_within_node_body_limits() {
    let dir = data_dir();
    let root = dir.path();
    let mut body = initialize_fixture(root);
    let (owner, router) = open(root, "1");

    let initialize = "/plans/1/plates/initialize";
    assert_eq!(
        send(&router, "POST", initialize, &body, false).await.0,
        StatusCode::UNAUTHORIZED
    );
    // Node's 8 MiB initialize limit admits a body past axum's 2 MiB default.
    body["padding"] = json!("x".repeat(3 * 1024 * 1024));
    let (status, ready) = send(
        &router,
        "POST",
        &format!("/api/v2{initialize}"),
        &body,
        true,
    )
    .await;
    assert_eq!(
        (status, &ready["kind"], &ready["plate_revision_id"]),
        (StatusCode::OK, &json!("ready"), &json!(2)),
        "{ready}"
    );
    let (status, replay) = send(&router, "POST", initialize, &body, true).await;
    assert_eq!(
        (status, &replay),
        (StatusCode::OK, &ready),
        "a replay writes nothing"
    );

    let arrange = json!({
        "expected": body["expected"],
        "expected_plate_revision_id": 2,
        "mode": "all",
    });
    let (status, arranged) = send(&router, "POST", "/plans/1/plates/arrange", &arrange, true).await;
    assert_eq!(
        (status, &arranged["plate_revision_id"]),
        (StatusCode::OK, &json!(2)),
        "the packed layout is already the head: {arranged}"
    );
    let mut large = arrange.clone();
    large["padding"] = json!("x".repeat(1024 * 1024));
    let (status, refused) = send(
        &router,
        "POST",
        "/api/v2/plans/1/plates/arrange",
        &large,
        true,
    )
    .await;
    assert_eq!(
        (status, &refused["detail"]),
        (
            StatusCode::PAYLOAD_TOO_LARGE,
            &json!("Request body too large")
        )
    );
    let (status, stale) = send(
        &router,
        "POST",
        "/plans/1/plates/arrange",
        &json!({"expected": body["expected"], "expected_plate_revision_id": 1, "mode": "unplaced"}),
        true,
    )
    .await;
    assert_eq!(
        (status, &stale["code"]),
        (StatusCode::CONFLICT, &json!("plate_revision_changed"))
    );
    owner.shutdown().unwrap();

    let (owner, router) = open(root, "0");
    assert_eq!(
        send(&router, "POST", initialize, &body, true).await.0,
        StatusCode::NOT_FOUND
    );
    owner.shutdown().unwrap();
}

/// Initialize's status and body on a fresh copy of the `no_head` fixture,
/// with its STL removed first when `removed`.
async fn initialize_once(removed: bool) -> (StatusCode, Value) {
    let dir = data_dir();
    let root = dir.path();
    let body = initialize_fixture(root);
    if removed {
        std::fs::remove_file(root.join("repos/1/revisions/accepted/included.stl")).unwrap();
    }
    let (owner, router) = open(root, "1");
    let answer = send(
        &router,
        "POST",
        "/api/v2/plans/1/plates/initialize",
        &body,
        true,
    )
    .await;
    owner.shutdown().unwrap();
    answer
}

/// GRE-1519, GRE-1532: initialize through the public, typed verified open.
/// With the accepted STL in place it answers 200 `ready`: a new Plate
/// revision of Build 1 whose plates hold the Build's unit. With the STL
/// removed it refuses with a 409 `conflict` problem, code `missing`, naming
/// that unit's token. The fixture is only the input; no recorded answer is
/// compared.
#[tokio::test]
async fn plates_http_initialize_is_ready_with_intact_stls_and_refuses_a_missing_one() {
    let (status, ready) = initialize_once(false).await;
    assert_eq!(status, StatusCode::OK, "{ready}");
    assert_eq!(
        (
            &ready["kind"],
            &ready["basis"]["profile_id"],
            &ready["arrange_undo_revision_id"]
        ),
        (&json!("ready"), &json!(1), &Value::Null),
        "{ready}"
    );
    for field in ["plate_revision_id", "plate_revision_number"] {
        let id = ready[field].as_i64();
        assert!(id.is_some_and(|id| id > 0), "{field}: {ready}");
    }
    let plates = ready["plates"].as_array().expect("plates");
    let units: Vec<&Value> = plates
        .iter()
        .flat_map(|plate| plate["units"].as_array().expect("units"))
        .collect();
    let unit = units
        .iter()
        .find(|unit| unit["filename"] == "included.stl")
        .expect("the Build's unit is on a plate");
    assert_eq!(unit["part_id"], 2, "{unit}");
    let token = unit["token"].as_str().expect("token");
    assert!(token.starts_with("ppu_"), "{token}");

    let (status, refused) = initialize_once(true).await;
    assert_eq!(status, StatusCode::CONFLICT, "{refused}");
    assert_eq!(
        (
            &refused["type"],
            &refused["status"],
            &refused["code"],
            &refused["token"]
        ),
        (
            &json!("urn:print-partner:problem:conflict"),
            &json!(409),
            &json!("missing"),
            &json!(token)
        ),
        "{refused}"
    );
}

#[tokio::test]
async fn plates_http_serves_production_setup_at_the_root_only() {
    let dir = data_dir();
    let root = dir.path();
    let (owner, router) = open(root, "1");
    let setup = "/plans/1/production-setup";
    assert_eq!(get(&router, setup, false).await.0, StatusCode::UNAUTHORIZED);
    let (status, body) = get(&router, setup, true).await;
    assert_eq!(
        (status, &body["format"], &body["route"]),
        (StatusCode::OK, &json!("production-setup-v1"), &Value::Null),
        "{body}"
    );
    assert_eq!(
        get(&router, &format!("/api/v2{setup}"), true).await.0,
        StatusCode::NOT_FOUND,
        "Node registers no /api/v2 alias"
    );
    let (status, body) = get(&router, "/plans/abc/production-setup", true).await;
    assert_eq!(
        (status, &body["detail"], &body["code"]),
        (
            StatusCode::BAD_REQUEST,
            &json!("invalid Build id"),
            &Value::Null
        )
    );
    let (status, body) = send(&router, "PATCH", setup, &json!({"kind": "nope"}), true).await;
    assert_eq!(
        (status, &body["detail"], &body["issues"][0]["path"]),
        (
            StatusCode::BAD_REQUEST,
            &json!("Invalid production setup"),
            &json!(["kind"])
        )
    );
    let cookie = Some("pp_session=read-fixture-secret");
    let (status, body) = loopback::send(&router, "PATCH", setup, Some("{"), cookie).await;
    assert_eq!(
        (status, &body["detail"]),
        (StatusCode::BAD_REQUEST, &json!("Invalid JSON body"))
    );
    // Node's 8 MiB limit admits a body past axum's 2 MiB default.
    let assignments: Vec<Value> = (0..20_000)
        .map(|index| json!({"token": format!("t{index:0>150}"), "printer_id": "p"}))
        .collect();
    let command =
        json!({"kind": "replace_printer_assignments", "printer_assignments": assignments});
    assert!(command.to_string().len() > 3 * 1024 * 1024);
    let (status, body) = send(&router, "PATCH", setup, &command, true).await;
    assert_eq!(
        (status, body["printer_assignments"].as_array().map(Vec::len)),
        (StatusCode::OK, Some(20_000))
    );
    let oversized =
        json!({"kind": "set_route", "route": "plates", "pad": "x".repeat(8 * 1024 * 1024)});
    let (status, body) = send(&router, "PATCH", setup, &oversized, true).await;
    assert_eq!(
        (status, &body["detail"]),
        (
            StatusCode::PAYLOAD_TOO_LARGE,
            &json!("Request body too large")
        )
    );
    let route = json!({"kind": "set_route", "route": "plates"});
    assert_eq!(
        send(&router, "PATCH", setup, &route, true).await.0,
        StatusCode::OK
    );
    owner.shutdown().unwrap();

    let (owner, router) = open(root, "1");
    let (status, body) = get(&router, setup, true).await;
    assert_eq!(
        (
            status,
            &body["route"],
            body["printer_assignments"].as_array().map(Vec::len)
        ),
        (StatusCode::OK, &json!("plates"), Some(20_000)),
        "the setup survives a restart"
    );
    owner.shutdown().unwrap();

    let (owner, router) = open(root, "0");
    assert_eq!(get(&router, setup, true).await.0, StatusCode::NOT_FOUND);
    owner.shutdown().unwrap();
}
