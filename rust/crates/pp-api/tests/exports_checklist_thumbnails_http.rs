#![cfg(feature = "desktop_rebuild")]
use axum::{
    Router,
    http::{Request, StatusCode, header},
    response::Response,
};
use base64::{Engine, engine::general_purpose::STANDARD};
use loopback::ORIGIN;
use pp_api::{accepted::AcceptedHttpConfig, exports::exports_router};
use pp_storage::{
    Limits, WriterOwner,
    production::{geometry::GeometryLimits, thumbnail},
};
use sha2::{Digest, Sha256};
use std::path::Path;

#[path = "support/accepted_plate.rs"]
mod accepted_plate;
#[path = "support/loopback.rs"]
mod loopback;

const CHECKLIST: &str = "/api/v2/plans/1/exports/checklist";
const ACCEPTED_STL: &str = "repos/1/revisions/accepted/included.stl";
const CSP: &str = "default-src 'none'; img-src data:; style-src 'unsafe-inline'";

fn open(root: &Path) -> (WriterOwner, Router) {
    let owner = WriterOwner::open(root, Limits::default()).unwrap().0;
    let config = AcceptedHttpConfig::from_startup(ORIGIN, "default", Some("1")).unwrap();
    let router = exports_router(config, owner.accepted_reads().unwrap());
    (owner, router)
}

async fn get(router: &Router) -> Response {
    let _ = (
        loopback::send,
        loopback::send_as,
        loopback::send_with_headers,
    );
    let request = Request::get(CHECKLIST).header("cookie", "pp_session=read-fixture-secret");
    loopback::respond(router, request, Vec::new()).await
}

async fn body(response: Response) -> Vec<u8> {
    axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap()
        .to_vec()
}

fn files(dir: &Path) -> usize {
    std::fs::read_dir(dir)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .map(|path| if path.is_dir() { files(&path) } else { 1 })
        .sum()
}

fn jobs(root: &Path) -> i64 {
    rusqlite::Connection::open(root.join("print-partner.db"))
        .unwrap()
        .query_row("SELECT COUNT(*) FROM durable_jobs", [], |row| row.get(0))
        .unwrap()
}

fn make_current_revision(root: &Path, stl: &[u8]) {
    let snapshot = root.join("repos/1/revisions/current");
    std::fs::create_dir_all(&snapshot).unwrap();
    std::fs::write(snapshot.join("included.stl"), stl).unwrap();

    let sql = rusqlite::Connection::open(root.join("print-partner.db")).unwrap();
    let revision_id: i64 = sql
        .query_row(
            "SELECT COALESCE(MAX(id), 0) + 1 FROM source_revisions",
            [],
            |row| row.get(0),
        )
        .unwrap();
    let manifest_digest = hex::encode(Sha256::digest(stl));
    sql.execute(
        "INSERT INTO source_revisions(id,tenant_id,project_id,upstream_revision_key,manifest_digest,snapshot_locator,synced_at) \
         VALUES (?1,'default',1,'thumbnail-current-b',?2,'1/revisions/current','2026-11-13T00:00:00.000Z')",
        rusqlite::params![revision_id, manifest_digest],
    )
    .unwrap();
    sql.execute(
        "UPDATE projects SET current_source_revision_id=?1 WHERE id=1",
        [revision_id],
    )
    .unwrap();
    assert_eq!(
        sql.query_row(
            "SELECT current_source_revision_id FROM projects WHERE id=1",
            [],
            |row| row.get::<_, i64>(0),
        )
        .unwrap(),
        revision_id
    );
}

#[tokio::test]
async fn embeds_the_accepted_revision_thumbnail_when_a_newer_source_is_current() {
    let dir = accepted_plate::data_dir();
    let root = dir.path();
    accepted_plate::initialize_fixture(root);
    let accepted_stl = std::fs::read(root.join(ACCEPTED_STL)).unwrap();
    let current_stl = String::from_utf8(accepted_stl.clone())
        .unwrap()
        .replace("20", "40")
        .into_bytes();
    assert_ne!(accepted_stl, current_stl);
    let accepted_png = thumbnail::render(&accepted_stl, GeometryLimits::default()).unwrap();
    let current_png = thumbnail::render(&current_stl, GeometryLimits::default()).unwrap();
    assert_ne!(
        accepted_png, current_png,
        "fixtures must distinguish revisions"
    );
    make_current_revision(root, &current_stl);

    let (owner, router) = open(root);
    let before = (files(root), jobs(root));
    let response = get(&router).await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(response.headers()[header::CONTENT_SECURITY_POLICY], CSP);
    let html = String::from_utf8(body(response).await).unwrap();
    assert!(html.contains(&format!("content=\"{CSP}\"")));
    assert!(html.contains("<dd>1 of 1 units</dd>"));
    assert!(html.contains("<td class=\"n\">1 / 1</td>"));
    assert!(html.contains("included.stl"));
    assert!(!html.contains("<script"));
    assert!(!html.contains("href="));
    assert!(!html.contains("src=\"http"));
    assert!(!html.contains("src=\"file:"));

    let encoded = html
        .split_once("src=\"data:image/png;base64,")
        .expect("accepted image data URL")
        .1
        .split_once('"')
        .unwrap()
        .0;
    let embedded_png = STANDARD.decode(encoded).unwrap();
    assert_eq!(embedded_png, accepted_png);
    assert_ne!(
        embedded_png, current_png,
        "must not use the current Source revision"
    );
    assert_eq!(
        (files(root), jobs(root)),
        before,
        "no files or jobs created"
    );
    owner.shutdown().unwrap();
}

#[tokio::test]
async fn omits_a_missing_accepted_thumbnail_without_failing_the_checklist() {
    let dir = accepted_plate::data_dir();
    let root = dir.path();
    accepted_plate::initialize_fixture(root);
    std::fs::remove_file(root.join(ACCEPTED_STL)).unwrap();

    let (owner, router) = open(root);
    let response = get(&router).await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(response.headers()[header::CONTENT_SECURITY_POLICY], CSP);
    let html = String::from_utf8(body(response).await).unwrap();
    assert!(html.contains("included.stl"));
    assert!(html.contains("<dd>1 of 1 units</dd>"));
    assert!(!html.contains("<img"));
    owner.shutdown().unwrap();
}

#[tokio::test]
async fn omits_an_oversized_accepted_thumbnail_without_failing_the_checklist() {
    let dir = accepted_plate::data_dir();
    let root = dir.path();
    accepted_plate::initialize_fixture(root);
    std::fs::OpenOptions::new()
        .write(true)
        .open(root.join(ACCEPTED_STL))
        .unwrap()
        .set_len(GeometryLimits::default().max_bytes as u64 + 1)
        .unwrap();

    let (owner, router) = open(root);
    let before = (files(root), jobs(root));
    let response = get(&router).await;
    assert_eq!(response.status(), StatusCode::OK);
    let html = String::from_utf8(body(response).await).unwrap();
    assert!(html.contains("included.stl"));
    assert!(html.contains("<td class=\"n\">1 / 1</td>"));
    assert!(!html.contains("<img"));
    assert_eq!((files(root), jobs(root)), before);
    owner.shutdown().unwrap();
}
