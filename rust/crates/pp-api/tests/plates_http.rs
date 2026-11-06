#![cfg(feature = "desktop_rebuild")]
//! GRE-1189, GRE-1184: the Plate routes through `plates_router` on a disposable
//! copy of the Node-written `accepted-plate-node.db`: admission, the flag,
//! the root and `/api/v2` aliases, and Node's `{ detail, code }` failures.
//! GRE-1196: the Checkoff units read names each unit's head Plate.
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
    loopback::send(router, method, path, &body.to_string(), cookie).await
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
