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

const PART: &[u8] = b"solid part\nfacet normal 0 0 1\nouter loop\nvertex 0 0 0\nvertex 20 0 0\nvertex 0 20 10\nendloop\nendfacet\nendsolid part";

/// The fixture installation with one more file in Source 1's revision, and
/// the rebuild router over it.
fn with_file(path: &str, bytes: &[u8]) -> (tempfile::TempDir, WriterOwner, Router) {
    let root = tempfile::tempdir().unwrap();
    installation(root.path());
    let snapshot = root.path().join("repos/1/revisions/accepted");
    std::fs::write(snapshot.join(path), bytes).unwrap();
    let (owner, router) = router(root.path(), "1");
    (root, owner, router)
}

async fn get_png(
    router: &Router,
    path: &str,
    if_none_match: Option<&str>,
) -> axum::response::Response {
    let mut request = axum::http::Request::builder()
        .method("GET")
        .uri(path)
        .header("cookie", COOKIE);
    if let Some(tag) = if_none_match {
        request = request.header("if-none-match", tag);
    }
    loopback::respond(router, request, Vec::new()).await
}

#[tokio::test]
async fn a_thumbnail_is_a_png_with_an_etag_that_revalidates() {
    let (_root, _owner, router) = with_file("stl/part.stl", PART);

    let first = get_png(
        &router,
        "/api/v1/sources/1/thumbnail?path=stl/part.stl",
        None,
    )
    .await;
    assert_eq!(first.status(), StatusCode::OK);
    assert_eq!(first.headers()["content-type"], "image/png");
    assert_eq!(first.headers()["cache-control"], "private, no-cache");
    let etag = first.headers()["etag"].to_str().unwrap().to_owned();
    assert!(etag.starts_with("\"1-v1-"), "{etag}");
    let png = axum::body::to_bytes(first.into_body(), 1 << 20)
        .await
        .unwrap();
    assert_eq!(&png[..8], b"\x89PNG\r\n\x1a\n");
    assert_eq!(&png[16..24], &[0, 0, 1, 0, 0, 0, 1, 0], "256 x 256");

    let again = get_png(
        &router,
        "/sources/1/thumbnail?path=stl%2Fpart.stl",
        Some(&etag),
    )
    .await;
    assert_eq!(again.status(), StatusCode::NOT_MODIFIED);
    assert_eq!(again.headers()["etag"], etag.as_str());
    let other = get_png(
        &router,
        "/sources/1/thumbnail?path=stl%2Fpart.stl",
        Some("\"old\""),
    )
    .await;
    assert_eq!(other.status(), StatusCode::OK);
    let same = axum::body::to_bytes(other.into_body(), 1 << 20)
        .await
        .unwrap();
    assert_eq!(same, png);
}

#[tokio::test]
async fn thumbnail_refusals_are_problems() {
    let (root, _owner, router) = with_file("broken.stl", b"solid broken\nendsolid broken");
    for (path, status, code) in [
        (
            "/api/v1/sources/1/thumbnail",
            400,
            "source_file_path_required",
        ),
        (
            "/api/v1/sources/1/thumbnail?path=README.md",
            404,
            "source_file_not_found",
        ),
        (
            "/api/v1/sources/1/thumbnail?path=..%2Fx.stl",
            404,
            "source_file_not_found",
        ),
        (
            "/api/v1/sources/2/thumbnail?path=x.stl",
            404,
            "source_file_not_found",
        ),
        (
            "/api/v1/sources/9/thumbnail?path=x.stl",
            404,
            "source_not_found",
        ),
        (
            "/api/v1/sources/1/thumbnail?path=broken.stl",
            422,
            "source_file_unrenderable",
        ),
    ] {
        let (answered, body) = loopback::send(&router, "GET", path, None, Some(COOKIE)).await;
        assert_eq!(
            (answered.as_u16(), &body["code"]),
            (status, &json!(code)),
            "{path}"
        );
    }
    assert!(
        std::fs::read_dir(root.path().join("thumbs"))
            .map_or(true, |mut entries| entries.next().is_none()),
        "nothing was cached"
    );
}
