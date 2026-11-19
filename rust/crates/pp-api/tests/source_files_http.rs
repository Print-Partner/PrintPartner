#![cfg(feature = "desktop_rebuild")]
//! GRE-430 part 1: `GET /sources/{id}/files` at the root and `/api/v1`, on a
//! disposable copy of the Node-written `accepted-plate-node.db`. Its Source 1
//! has revision 1 at `1/revisions/accepted`; the test writes that snapshot and
//! adds Source 2 without a revision.
//!
//! GRE-1536: the last test mounts the integration and printer routes beside
//! this one and pins the whole body of one refusal from each family that maps
//! its error through `ToProblem`, and of the same requests while storage is
//! busy.
use axum::{Router, http::StatusCode};
use loopback::ORIGIN;
use pp_api::{
    accepted::AcceptedHttpConfig, integrations::integrations_router, printers::printers_router,
    source_files::source_files_router,
};
use pp_storage::{
    Limits, WriterOwner,
    auth::{AuthPolicy, FirstUserTenant, RegistrationPolicy, SessionTenantPolicy},
    printer_setup::MAX_PRINTERS,
};
use serde_json::{Value, json};
use std::{path::Path, time::Duration};

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

const PRINTER: &str = r#"{"name":"Voron","model":"voron-2.4-350","bed_width_mm":350,"bed_depth_mm":350,"bed_height_mm":340}"#;
/// A Source that does not exist, an integration of a kind these routes cannot
/// edit, and one printer more than the limit.
#[rustfmt::skip]
const REFUSED: [(&str, &str, Option<&str>); 3] = [
    ("GET", "/sources/999/files", None),
    ("PATCH", "/api/v2/integrations/bambu-1", Some(r#"{"name":"Renamed"}"#)),
    ("POST", "/api/v2/printers", Some(PRINTER)),
];

#[tokio::test]
async fn each_family_answers_its_refusal_and_a_busy_storage_the_same_way() {
    let root = tempfile::tempdir().unwrap();
    let cookie = installation(root.path());
    let bambu = r#"[{"type":"bambu","id":"bambu-1","config":{"host":"10.0.0.9"},"name":"H2D"}]"#;
    let sql = rusqlite::Connection::open(root.path().join("print-partner.db")).unwrap();
    let seed = "INSERT INTO app_settings(tenant_id,key,value) VALUES('default','integrations',?1)";
    sql.execute(seed, [bambu]).unwrap();
    // One reader, so holding it leaves a read none.
    let limits = Limits {
        readers: 1,
        ..Limits::default()
    };
    let owner = WriterOwner::open(root.path(), limits).unwrap().0;
    let config = AcceptedHttpConfig::from_startup(ORIGIN, "default", Some("1")).unwrap();
    let setup = owner.printer_setup(POLICY).unwrap();
    let router = source_files_router(config.clone(), owner.source_files(POLICY).unwrap())
        .merge(integrations_router(config.clone(), setup.clone()))
        .merge(printers_router(config, setup));
    let send = |(method, path, body)| loopback::send(&router, method, path, body, Some(&cookie));
    for _ in 0..MAX_PRINTERS {
        assert_eq!(send(REFUSED[2]).await.0, StatusCode::OK);
    }

    #[rustfmt::skip]
    let refusals = [
        json!({"type": "urn:print-partner:problem:not-found", "title": "Not Found", "status": 404,
            "detail": "Source not found", "code": "source_not_found"}),
        json!({"type": "urn:print-partner:problem:conflict", "title": "Conflict", "status": 409,
            "detail": "This kind of integration cannot be edited here", "code": "integration_not_editable"}),
        json!({"type": "urn:print-partner:problem:conflict", "title": "Conflict", "status": 409,
            "detail": "The printer limit is reached. Delete one first.", "code": "printer_limit"}),
    ];
    for (request, expected) in REFUSED.into_iter().zip(&refusals) {
        let (status, answered) = send(request).await;
        assert_eq!(json!(status.as_u16()), expected["status"], "{request:?}");
        assert_eq!(&answered, expected, "{request:?}");
    }

    // Another connection holds the write lock, and this test the one reader.
    sql.execute_batch("BEGIN IMMEDIATE").unwrap();
    let reader = owner.client().reader(Duration::from_secs(5)).unwrap();
    let busy = json!({"type": "urn:print-partner:problem:unavailable",
        "title": "Service Unavailable", "status": 503,
        "detail": "Service temporarily unavailable", "code": "transaction_unavailable"});
    for request in REFUSED {
        let (status, answered) = send(request).await;
        assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE, "{request:?}");
        assert_eq!(answered, busy, "{request:?}");
    }
    drop(reader);
    sql.execute_batch("ROLLBACK").unwrap();
}

const NESTED_STEP: &[u8] = include_bytes!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../../fixtures/step/nested-assembly.step"
));

/// GET `path` until the conversion leaves queued and converting.
async fn settled(router: &Router, path: &str) -> Value {
    let started = std::time::Instant::now();
    loop {
        let (status, body) = loopback::send(router, "GET", path, None, Some(COOKIE)).await;
        assert_eq!(status, StatusCode::OK, "{path}: {body}");
        if !matches!(body["state"].as_str(), Some("queued" | "converting")) {
            return body;
        }
        assert!(
            started.elapsed() < Duration::from_secs(10),
            "{path} never settled"
        );
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
}

/// GRE-1007: a STEP file is queued on first open, converted once in the
/// background, and every later open at either prefix answers the cached tree.
#[tokio::test]
async fn a_step_file_is_converted_once_and_answered_from_the_cache() {
    let root = tempfile::tempdir().unwrap();
    installation(root.path());
    let snapshot = root.path().join("repos/1/revisions/accepted");
    std::fs::create_dir_all(snapshot.join("cad")).unwrap();
    std::fs::write(snapshot.join("cad/printer.step"), NESTED_STEP).unwrap();
    std::fs::write(snapshot.join("cad/broken.stp"), b"solid not step").unwrap();
    let owner = WriterOwner::open(root.path(), Limits::default()).unwrap().0;
    let config = AcceptedHttpConfig::from_startup(ORIGIN, "default", Some("1")).unwrap();
    let client = owner.source_files(POLICY).unwrap();
    let router = source_files_router(config, client.clone());

    let path = "/api/v1/sources/1/step-conversion?path=cad%2Fprinter.step";
    let (status, first) = loopback::send(&router, "GET", path, None, Some(COOKIE)).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        first,
        json!({"source_id": 1, "path": "cad/printer.step", "state": "queued",
            "sha256": null, "failure": null, "parts": []})
    );
    let done = settled(&router, path).await;
    let part = |name: &str, product_id: &str, instance: Option<&str>, depth: u32| json!({"name": name, "product_id": product_id, "instance": instance, "depth": depth});
    let sha256 = hex::encode(<sha2::Sha256 as sha2::Digest>::digest(NESTED_STEP));
    assert_eq!(
        done,
        json!({
            "source_id": 1,
            "path": "cad/printer.step",
            "state": "done",
            "sha256": sha256,
            "failure": null,
            "parts": [
                part("Printer", "PRN-1", None, 0),
                part("Frame", "FRM-1", Some("Frame:1"), 1),
                part("Gantry", "GNT-1", Some("Gantry:1"), 2),
                part("Carriage", "CAR-1", Some("Carriage:1"), 3),
                part("Hotend", "HOT-1", Some("Hotend:1"), 4),
                part("D\u{fc}se 0.4", "NOZ-1", Some("Nozzle:1"), 5),
                part("SCR-M3", "SCR-M3", Some("Screw:1"), 4),
                part("SCR-M3", "SCR-M3", Some("Screw:2"), 4),
                part("SCR-M3", "SCR-M3", Some("NAUO8"), 2),
                part("Bed; 'heated'", "BED-1", Some("Bed:1"), 1),
            ],
        })
    );
    let (status, again) = loopback::send(
        &router,
        "GET",
        "/sources/1/step-conversion?path=cad/printer.step",
        None,
        Some(COOKIE),
    )
    .await;
    assert_eq!((status, &again), (StatusCode::OK, &done));
    assert_eq!(client.step_files_converted(), 1);
    let cached = root
        .path()
        .join(format!("thumbs/sources/step/{sha256}-v1.json"));
    assert!(cached.is_file(), "{}", cached.display());

    let failed = settled(
        &router,
        "/api/v1/sources/1/step-conversion?path=cad%2Fbroken.stp",
    )
    .await;
    assert_eq!(failed["state"], "failed");
    assert_eq!(failed["failure"], "not_step");
    assert_eq!(failed["parts"], json!([]));
}

#[tokio::test]
async fn step_conversion_refusals_are_problems() {
    let (_root, _owner, router) = with_file("part.stl", PART);
    let cases = [
        (
            "/api/v1/sources/1/step-conversion",
            400,
            "source_file_path_required",
        ),
        (
            "/api/v1/sources/1/step-conversion?path=",
            400,
            "source_file_path_required",
        ),
        (
            "/api/v1/sources/1/step-conversion?path=part.stl",
            404,
            "source_file_not_found",
        ),
        (
            "/api/v1/sources/1/step-conversion?path=missing.step",
            404,
            "source_file_not_found",
        ),
        (
            "/api/v1/sources/2/step-conversion?path=a.step",
            404,
            "source_file_not_found",
        ),
        (
            "/api/v1/sources/9/step-conversion?path=a.step",
            404,
            "source_not_found",
        ),
    ];
    for (path, status, code) in cases {
        let (answered, body) = loopback::send(&router, "GET", path, None, Some(COOKIE)).await;
        assert_eq!(answered.as_u16(), status, "{path}: {body}");
        assert_eq!(body["code"], code, "{path}");
    }
    let (status, _) = loopback::send(
        &router,
        "GET",
        "/api/v1/sources/1/step-conversion?path=a.step",
        None,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
}
