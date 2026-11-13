#![cfg(feature = "desktop_rebuild")]
//! GRE-1525 through the mounted route, on Node's accepted-plate fixture: the
//! STL pack holds one byte-identical entry per Required unit in scope, refuses
//! the whole pack when a Source file is missing or altered, refuses without a
//! credential, for an unknown Build and for a Build with no accepted Plan,
//! and leaves no file and no job behind.
use accepted_plate::{data_dir, initialize_fixture};
use axum::{
    Router,
    http::{Request, StatusCode, header},
    response::Response,
};
use loopback::ORIGIN;
use pp_api::{accepted::AcceptedHttpConfig, exports::exports_router};
use pp_storage::{Limits, WriterOwner};
use serde_json::{Value, json};
use std::{
    io::{Cursor, Read},
    path::Path,
};

#[path = "support/accepted_plate.rs"]
mod accepted_plate;
#[path = "support/loopback.rs"]
mod loopback;

const PACK: &str = "/api/v2/plans/1/exports/stl-pack";
const STL: &str = "repos/1/revisions/accepted/included.stl";

fn open(root: &Path) -> (WriterOwner, Router) {
    let owner = WriterOwner::open(root, Limits::default()).unwrap().0;
    let config = AcceptedHttpConfig::from_startup(ORIGIN, "default", Some("1")).unwrap();
    let router = exports_router(config, owner.accepted_reads().unwrap());
    (owner, router)
}

async fn get(router: &Router, path: &str, signed_in: bool) -> Response {
    // This binary sends only through `respond`; keep the JSON helpers live.
    let _ = (loopback::send, loopback::send_as);
    let mut request = Request::get(path);
    if signed_in {
        request = request.header("cookie", "pp_session=read-fixture-secret");
    }
    loopback::respond(router, request, Vec::new()).await
}

async fn bytes(response: Response) -> Vec<u8> {
    axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap()
        .to_vec()
}

async fn refused(router: &Router, path: &str, signed_in: bool) -> (StatusCode, Value) {
    let response = get(router, path, signed_in).await;
    let status = response.status();
    (
        status,
        serde_json::from_slice(&bytes(response).await).unwrap(),
    )
}

/// Each entry's name and bytes.
fn entries(archive: Vec<u8>) -> Vec<(String, Vec<u8>)> {
    let mut zip = zip::ZipArchive::new(Cursor::new(archive)).unwrap();
    (0..zip.len())
        .map(|index| {
            let mut entry = zip.by_index(index).unwrap();
            let mut data = Vec::new();
            entry.read_to_end(&mut data).unwrap();
            (entry.name().unwrap().to_string(), data)
        })
        .collect()
}

fn files(dir: &Path) -> usize {
    std::fs::read_dir(dir)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .map(|path| if path.is_dir() { files(&path) } else { 1 })
        .sum()
}

#[tokio::test]
async fn the_pack_holds_each_unit_in_scope_byte_for_byte() {
    let dir = data_dir();
    let root = dir.path();
    initialize_fixture(root);
    let stl = std::fs::read(root.join(STL)).unwrap();
    let (owner, router) = open(root);
    let sql = rusqlite::Connection::open(root.join("print-partner.db")).unwrap();
    let jobs = || -> i64 {
        sql.query_row("SELECT COUNT(*) FROM durable_jobs", [], |r| r.get(0))
            .unwrap()
    };
    let before = (files(root), jobs());

    let response = get(&router, PACK, true).await;
    assert_eq!(response.status(), StatusCode::OK);
    let headers = response.headers().clone();
    assert_eq!(headers[header::CONTENT_TYPE], "application/zip");
    assert_eq!(
        headers[header::CONTENT_DISPOSITION],
        "attachment; filename=\"tracked-operational-build-v1-stl.zip\""
    );
    assert!(!headers.contains_key(header::CONTENT_LENGTH), "chunked");
    let unit = "primary/_root/included_01.stl".to_string();
    assert_eq!(
        entries(bytes(response).await),
        [(unit.clone(), stl.clone())]
    );

    // The fixture's one unit is printed, so nothing remains.
    let (status, problem) = refused(&router, &format!("{PACK}?scope=remaining"), true).await;
    assert_eq!(
        (status, &problem["code"]),
        (StatusCode::CONFLICT, &json!("nothing_to_pack"))
    );
    sql.execute(
        "UPDATE print_progress SET completed=0, assembled=0 WHERE part_id=2",
        [],
    )
    .unwrap();
    let response = get(&router, &format!("{PACK}?scope=remaining"), true).await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(entries(bytes(response).await), [(unit, stl)]);
    let (status, problem) = refused(&router, &format!("{PACK}?scope=some"), true).await;
    assert_eq!(
        (status, &problem["code"]),
        (StatusCode::BAD_REQUEST, &json!("invalid_scope"))
    );

    assert_eq!((files(root), jobs()), before, "no file and no job");
    owner.shutdown().unwrap();
}

#[tokio::test]
async fn a_missing_or_altered_file_refuses_the_whole_pack() {
    for (removed, reason) in [(true, "missing"), (false, "digest_mismatch")] {
        let dir = data_dir();
        let root = dir.path();
        initialize_fixture(root);
        if removed {
            std::fs::remove_file(root.join(STL)).unwrap();
        } else {
            std::fs::write(root.join(STL), "solid altered\nendsolid altered\n").unwrap();
        }
        let (owner, router) = open(root);
        let (status, problem) = refused(&router, PACK, true).await;
        assert_eq!(status, StatusCode::CONFLICT, "{problem}");
        assert_eq!(problem["code"], "source_files_unavailable");
        assert_eq!(
            problem["files"],
            json!([{
                "relative_path": "included.stl",
                "source": "base:Tracked operational source",
                "reason": reason,
            }])
        );
        owner.shutdown().unwrap();
    }
}

#[tokio::test]
async fn the_pack_refuses_credentials_builds_and_plans_it_cannot_use() {
    let dir = data_dir();
    let root = dir.path();
    initialize_fixture(root);
    rusqlite::Connection::open(root.join("print-partner.db"))
        .unwrap()
        .execute_batch(
            "INSERT INTO build_profiles (id, tenant_id, name) VALUES (5, 'default', 'Unaccepted');",
        )
        .unwrap();
    let (owner, router) = open(root);
    assert_eq!(
        refused(&router, PACK, false).await.0,
        StatusCode::UNAUTHORIZED
    );
    let unknown = "/api/v2/plans/999/exports/stl-pack";
    assert_eq!(
        refused(&router, unknown, true).await.0,
        StatusCode::NOT_FOUND
    );
    let (status, problem) = refused(&router, "/api/v2/plans/5/exports/stl-pack", true).await;
    assert_eq!(
        (status, &problem["code"]),
        (StatusCode::CONFLICT, &json!("no_accepted_plan"))
    );
    owner.shutdown().unwrap();
}
