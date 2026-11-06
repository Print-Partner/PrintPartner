#![cfg(feature = "desktop_rebuild")]
//! GRE-1184: the Plate routes through `plates_router` on a disposable copy of
//! the Node-written `accepted-plate-node.db`: admission, the flag, the root
//! and `/api/v2` aliases, and Node's `{ detail, code }` failures.
use axum::{Router, http::StatusCode};
use loopback::ORIGIN;
use pp_api::{accepted::AcceptedHttpConfig, plates::plates_router};
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

#[tokio::test]
async fn plates_http_moves_a_unit_once_and_answers_node_failures() {
    let root = tempfile::tempdir().unwrap();
    let root = root.path().to_path_buf();
    std::fs::create_dir_all(root.join("repos/1/revisions/accepted")).unwrap();
    std::fs::write(
        root.join("print-partner.db"),
        include_bytes!("../../pp-storage/tests/fixtures/accepted-plate-node.db"),
    )
    .unwrap();
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
