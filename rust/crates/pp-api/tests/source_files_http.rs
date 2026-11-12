#![cfg(feature = "desktop_rebuild")]
//! GRE-430 part 1: `GET /sources/{id}/files` at the root and `/api/v1`, on a
//! disposable installation whose Source 1 has a revision snapshot on disk.
use axum::{Router, http::StatusCode};
use loopback::ORIGIN;
use pp_api::{accepted::AcceptedHttpConfig, source_files::source_files_router};
use pp_storage::{
    Limits, WriterOwner,
    auth::{
        AuthPolicy, FirstUserTenant, Outcome, RegistrationPolicy, Request, Secret,
        SessionTenantPolicy,
    },
};
use serde_json::{Value, json};
use std::{path::Path, time::Duration};

#[path = "support/loopback.rs"]
mod loopback;

const POLICY: AuthPolicy = AuthPolicy {
    registration: RegistrationPolicy::FirstAccountOnly,
    first_user: FirstUserTenant::NewUser,
    session_tenant: SessionTenantPolicy::SingleAccountDefault,
};

/// Registers the one account and answers its session cookie, then adds
/// Source 1 with revision 10 at `1/revisions/r1` and Source 2 with none.
fn installation(root: &Path) -> String {
    let owner = WriterOwner::open(root, Limits::default()).unwrap().0;
    let register = Request::Register {
        email: "owner@example.com".into(),
        display_name: "Owner".into(),
        password: Secret::new("correct-horse".into()),
    };
    let auth = owner.auth(FirstUserTenant::NewUser);
    let reply = auth
        .submit(register, Default::default(), Duration::from_secs(5))
        .unwrap();
    let Outcome::Session { token, .. } = reply.recv().unwrap().unwrap() else {
        panic!("registration returns a session");
    };
    owner.shutdown().unwrap();
    rusqlite::Connection::open(root.join("print-partner.db"))
        .unwrap()
        .execute_batch(
            "INSERT INTO projects(id,tenant_id,name,url,source_kind) VALUES(1,'default','Kit','','local'),(2,'default','Empty','','local');
             INSERT INTO source_revisions(id,tenant_id,project_id,upstream_revision_key,manifest_digest,snapshot_locator,synced_at) VALUES(10,'default',1,'r1','d1','1/revisions/r1','2026-11-12T00:00:00.000Z');
             UPDATE projects SET current_source_revision_id=10 WHERE id=1;",
        )
        .unwrap();
    let snapshot = root.join("repos/1/revisions/r1");
    std::fs::create_dir_all(snapshot.join("stl/arms")).unwrap();
    std::fs::write(snapshot.join("stl/body.stl"), b"solid body").unwrap();
    std::fs::write(snapshot.join("stl/arms/arm.stl"), b"arm").unwrap();
    std::fs::write(snapshot.join("README.md"), b"# Kit").unwrap();
    format!("pp_session={}", token.expose())
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
        "revision_id": 10,
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
