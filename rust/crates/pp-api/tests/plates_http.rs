#![cfg(feature = "desktop_rebuild")]
//! GRE-1189: `GET /plans/{id}/plates` through `plates_router` on a disposable
//! copy of the Node-written `accepted-plate-node.db`: admission, the flag,
//! the root and `/api/v2` aliases, and Node's `{ detail, code }` failures.
use axum::{Router, http::StatusCode};
use loopback::ORIGIN;
use pp_api::{accepted::AcceptedHttpConfig, plates::plates_router};
use pp_storage::{Limits, WriterOwner};
use serde_json::{Value, json};
use std::path::Path;

#[path = "support/loopback.rs"]
mod loopback;

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

async fn get(router: &Router, path: &str, cookie: bool) -> (StatusCode, Value) {
    let cookie = cookie.then_some("pp_session=read-fixture-secret");
    loopback::send(router, "GET", path, "", cookie).await
}

#[tokio::test]
async fn plates_http_read_answers_node_failures_on_both_aliases() {
    let root = tempfile::tempdir().unwrap();
    let root = root.path();
    std::fs::create_dir_all(root.join("repos/1/revisions/accepted")).unwrap();
    std::fs::write(
        root.join("print-partner.db"),
        include_bytes!("../../pp-storage/tests/fixtures/accepted-plate-node.db"),
    )
    .unwrap();

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

    rusqlite::Connection::open(root.join("print-partner.db"))
        .unwrap()
        .execute("DELETE FROM accepted_plate_heads", [])
        .unwrap();
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
