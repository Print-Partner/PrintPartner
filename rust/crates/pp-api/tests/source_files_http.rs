#![cfg(feature = "desktop_rebuild")]
//! GRE-430 part 1: `GET /sources/{id}/files` at the root and `/api/v1`, on a
//! disposable copy of the Node-written `accepted-plate-node.db`. Its Source 1
//! has revision 1 at `1/revisions/accepted`; the test writes that snapshot and
//! adds Source 2 without a revision.
use axum::{Router, http::StatusCode};
use loopback::ORIGIN;
use pp_api::{accepted::AcceptedHttpConfig, source_files::source_files_router};
use pp_storage::{
    Limits, WriterOwner,
    auth::{AuthPolicy, FirstUserTenant, RegistrationPolicy, SessionTenantPolicy},
};
use serde_json::{Value, json};
use std::path::Path;

#[path = "support/loopback.rs"]
mod loopback;

/// The fixture's session, admitted to its account's installation.
const COOKIE: &str = "pp_session=read-fixture-secret";
const POLICY: AuthPolicy = AuthPolicy {
    registration: RegistrationPolicy::Open,
    session_tenant: SessionTenantPolicy::AccountTenant,
    first_user: FirstUserTenant::NewUser,
};

fn installation(root: &Path) -> String {
    let database = root.join("print-partner.db");
    std::fs::write(
        &database,
        include_bytes!("../../pp-storage/tests/fixtures/accepted-plate-node.db"),
    )
    .unwrap();
    let sql = "INSERT INTO projects(id,tenant_id,name,url,source_kind) VALUES(2,'default','Empty','','local')";
    rusqlite::Connection::open(database)
        .unwrap()
        .execute(sql, [])
        .unwrap();
    let snapshot = root.join("repos/1/revisions/accepted");
    for (path, bytes) in [
        ("stl/body.stl", &b"solid body"[..]),
        ("stl/arms/arm.stl", b"arm"),
        ("README.md", b"# Kit"),
    ] {
        let file = snapshot.join(path);
        std::fs::create_dir_all(file.parent().unwrap()).unwrap();
        std::fs::write(file, bytes).unwrap();
    }
    COOKIE.into()
}

fn router(root: &Path, flag: &str) -> (WriterOwner, Router) {
    let owner = WriterOwner::open(root, Limits::default()).unwrap().0;
    let config = AcceptedHttpConfig::from_startup(ORIGIN, "default", Some(flag)).unwrap();
    let router = source_files_router(config, owner.source_files(POLICY).unwrap());
    (owner, router)
}

#[tokio::test]
async fn lists_the_current_revisions_files_at_both_prefixes() {
    let root = tempfile::tempdir().unwrap();
    let cookie = installation(root.path());
    let (_owner, router) = router(root.path(), "1");
    let expected = json!({
        "source_id": 1,
        "revision_id": 1,
        "files": [
            {"path": "README.md", "kind": "document", "size_bytes": 5},
            {"path": "stl/arms/arm.stl", "kind": "stl", "size_bytes": 3},
            {"path": "stl/body.stl", "kind": "stl", "size_bytes": 10},
        ],
    });
    for path in ["/api/v1/sources/1/files", "/sources/1/files"] {
        let (status, body) = loopback::send(&router, "GET", path, None, Some(&cookie)).await;
        assert_eq!((status, &body), (StatusCode::OK, &expected), "{path}");
    }
}

#[tokio::test]
async fn no_revision_unknown_sources_and_no_session() {
    let root = tempfile::tempdir().unwrap();
    let cookie = installation(root.path());
    let (_owner, router) = router(root.path(), "1");
    let get = |path: &'static str, cookie: Option<String>| {
        let router = router.clone();
        async move { loopback::send(&router, "GET", path, None, cookie.as_deref()).await }
    };

    let (status, body) = get("/api/v1/sources/2/files", Some(cookie.clone())).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        body,
        json!({"source_id": 2, "revision_id": null, "files": []})
    );
    for path in ["/api/v1/sources/3/files", "/api/v1/sources/abc/files"] {
        let (status, body) = get(path, Some(cookie.clone())).await;
        assert_eq!(status, StatusCode::NOT_FOUND, "{path}");
        assert_eq!(body["code"], "source_not_found", "{path}");
    }
    let (status, _) = get("/api/v1/sources/1/files", None).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn the_route_is_absent_without_the_rebuild_flag() {
    let root = tempfile::tempdir().unwrap();
    let cookie = installation(root.path());
    let (_owner, router) = router(root.path(), "0");
    let (status, _): (StatusCode, Value) = loopback::send(
        &router,
        "GET",
        "/api/v1/sources/1/files",
        None,
        Some(&cookie),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}
