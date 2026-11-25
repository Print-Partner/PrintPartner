#![cfg(feature = "desktop_rebuild")]
//! GRE-1447 through the mounted Rust routes, on a disposable copy of a real
//! SQLite fixture: the HTML checklist matches `GET /api/v2/plans/{id}/checkoff/units`
//! before and after a Checkoff burst, refuses without a credential, for an
//! unknown Build and for a Build with no accepted Plan, and leaves no file and
//! no job behind.
use axum::{
    Router,
    http::{StatusCode, header},
};
use base64::{Engine, engine::general_purpose::STANDARD};
use checkoff_http::{Fixture, post, send};
use pp_api::{accepted::accepted_router, checkoff::checkoff_router, exports::exports_router};
use pp_storage::{
    WriterOwner,
    production::{geometry::GeometryLimits, thumbnail},
};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{collections::BTreeSet, path::Path};

#[path = "support/accepted_plate.rs"]
mod accepted_plate;
#[path = "support/checkoff_http.rs"]
mod checkoff_http;
#[path = "../../pp-storage/tests/common/js_json.rs"]
mod js_json;

const CHECKLIST: &str = "/api/v2/plans/1/exports/checklist";
const CSP: &str = "default-src 'none'; img-src data:; style-src 'unsafe-inline'";

fn mount(fixture: &Fixture, flag: &str) -> (WriterOwner, Router) {
    fixture.open_with(flag, |config, owner| {
        checkoff_router(config.clone(), owner.checkoff_progress().unwrap())
            .merge(accepted_router(
                config.clone(),
                owner.accepted_reads().unwrap(),
            ))
            .merge(exports_router(config, owner.accepted_reads().unwrap()))
    })
}

/// A loopback GET; the answer's status, headers and body as text.
async fn download(
    router: &Router,
    path: &str,
    cookie: bool,
) -> (StatusCode, header::HeaderMap, String) {
    let credential = cookie.then_some(("cookie", "pp_session=required-unit-fixture-secret"));
    download_as(router, path, credential).await
}

/// [`download`] presenting `credential`, a header name and value.
async fn download_as(
    router: &Router,
    path: &str,
    credential: Option<(&str, &str)>,
) -> (StatusCode, header::HeaderMap, String) {
    let mut request = axum::http::Request::get(path);
    if let Some((name, value)) = credential {
        request = request.header(name, value);
    }
    let response = checkoff_http::loopback::respond(router, request, Vec::new()).await;
    let (parts, body) = response.into_parts();
    let bytes = axum::body::to_bytes(body, 1 << 20).await.unwrap();
    (
        parts.status,
        parts.headers,
        String::from_utf8(bytes.to_vec()).unwrap(),
    )
}

/// Every file under `dir`, relative to `root`, so a download that writes
/// one shows up.
fn files(root: &Path, dir: &Path, found: &mut BTreeSet<String>) {
    for path in std::fs::read_dir(dir)
        .unwrap()
        .map(|entry| entry.unwrap().path())
    {
        match path.is_dir() {
            true => files(root, &path, found),
            false => _ = found.insert(path.strip_prefix(root).unwrap().display().to_string()),
        }
    }
}

/// [`files`] for the whole data directory, without SQLite's own sidecars.
fn data_files(root: &Path) -> BTreeSet<String> {
    let mut found = BTreeSet::new();
    files(root, root, &mut found);
    found.retain(|file| !file.starts_with("print-partner.db-"));
    found
}

fn jobs(fixture: &Fixture) -> i64 {
    fixture
        .sql()
        .query_row("SELECT COUNT(*) FROM durable_jobs", [], |row| row.get(0))
        .unwrap()
}

/// The checklist states each Part's quantity and printed count as the
/// Checkoff unit read does, and their totals.
fn assert_matches_units(html: &str, units: &Value) {
    let (mut printed, mut total) = (0, 0);
    for part in units["parts"].as_array().unwrap() {
        let (quantity, done) = (
            part["quantity"].as_u64().unwrap(),
            part["printed_count"].as_u64().unwrap(),
        );
        let cells = format!(
            "<td>{}</td><td>{}</td>",
            part["filename"].as_str().unwrap(),
            part["role"].as_str().unwrap()
        );
        let counts =
            format!("<td class=\"n\">{quantity}</td><td class=\"n\">{done} / {quantity}</td>");
        let row = html
            .lines()
            .find(|line| line.contains(&cells))
            .unwrap_or_else(|| panic!("{cells} missing"));
        assert!(row.contains(&counts), "{row} lacks {counts}");
        (printed, total) = (printed + done, total + quantity);
    }
    assert_eq!(
        html.matches("<tr><td>").count(),
        units["parts"].as_array().unwrap().len()
    );
    assert!(html.contains(&format!("<dd>{printed} of {total} units</dd>")));
}

fn accepted_fixture() -> Fixture {
    let dir = accepted_plate::data_dir();
    let root = dir.keep();
    accepted_plate::initialize_fixture(&root);
    Fixture(root)
}

fn make_current_revision(fixture: &Fixture, stl: &[u8]) {
    let root = &fixture.0;
    let snapshot = root.join("repos/1/revisions/current");
    std::fs::create_dir_all(&snapshot).unwrap();
    std::fs::write(snapshot.join("included.stl"), stl).unwrap();

    let sql = fixture.sql();
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

async fn checklist_download(fixture: &Fixture) -> (WriterOwner, (BTreeSet<String>, i64), String) {
    let (owner, router) = mount(fixture, "1");
    let before = (data_files(&fixture.0), jobs(fixture));
    let (status, headers, html) = download_as(
        &router,
        CHECKLIST,
        Some(("cookie", "pp_session=read-fixture-secret")),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{html}");
    assert_eq!(headers[header::CONTENT_SECURITY_POLICY], CSP);
    (owner, before, html)
}

async fn assert_thumbnail_omitted(fixture: &Fixture) {
    let (owner, before, html) = checklist_download(fixture).await;
    assert!(html.contains("included.stl"));
    assert!(html.contains("<dd>1 of 1 units</dd>"));
    assert!(html.contains("<td class=\"n\">1 / 1</td>"));
    assert!(!html.contains("<img"));
    assert_eq!((data_files(&fixture.0), jobs(fixture)), before);
    owner.shutdown().unwrap();
}

#[tokio::test]
async fn checklist_matches_checkoff_units_and_follows_a_burst() {
    let fixture = Fixture::new();
    fixture
        .sql()
        .execute_batch(
            "INSERT INTO build_profiles (id, tenant_id, name) VALUES (5, 'default', 'Unaccepted');",
        )
        .unwrap();
    let (owner, router) = mount(&fixture, "1");
    let (before_files, before_jobs) = (data_files(&fixture.0), jobs(&fixture));

    let (status, units) = send(&router, "GET", "/api/v2/plans/1/checkoff/units", "", true).await;
    assert_eq!(status, StatusCode::OK, "{units}");
    let version = units["basis"]["plan_version"].as_i64().unwrap();
    let (status, headers, html) = download(&router, CHECKLIST, true).await;
    assert_eq!(status, StatusCode::OK, "{html}");
    assert_eq!(headers[header::CONTENT_TYPE], "text/html; charset=utf-8");
    assert_eq!(
        headers[header::CONTENT_DISPOSITION],
        format!("attachment; filename=\"fixture-build-v{version}-checklist.html\"").as_str()
    );
    assert_eq!(headers[header::CACHE_CONTROL], "no-store");
    assert_eq!(
        headers[header::CONTENT_SECURITY_POLICY],
        "default-src 'none'; img-src data:; style-src 'unsafe-inline'"
    );
    assert!(html.contains(&format!("<dt>Plan version</dt><dd>{version}</dd>")));
    assert!(html.contains("default-src 'none'"));
    assert!(
        !html.contains("excluded.stl"),
        "excluded Parts are left out"
    );
    assert_matches_units(&html, &units);

    let body = json!({
        "idempotency_key": "checklist-burst",
        "expected": units["basis"],
        "units": [{ "token": units["parts"][0]["units"][2]["token"], "printed": true }],
    })
    .to_string();
    let (status, receipt) = post(&router, "/api/v2/plans/1/checkoff/bursts", &body, true).await;
    assert_eq!(status, StatusCode::OK, "{receipt}");
    let (_, after) = send(&router, "GET", "/api/v2/plans/1/checkoff/units", "", true).await;
    assert_ne!(
        after["parts"], units["parts"],
        "the burst changed the counts"
    );
    let (status, _, html) = download(&router, CHECKLIST, true).await;
    assert_eq!(status, StatusCode::OK);
    assert_matches_units(&html, &after);

    let (status, _, body) = download(&router, CHECKLIST, false).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED, "{body}");
    for refused in [
        ("cookie", "pp_session=not-a-session"),
        ("authorization", "Bearer ppk_not_a_key"),
        ("x-print-partner-api-key", "ppk_not_a_key"),
    ] {
        let (status, _, body) = download_as(&router, CHECKLIST, Some(refused)).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED, "{refused:?}: {body}");
    }
    let (status, _, body) = download(&router, "/api/v2/plans/999/exports/checklist", true).await;
    assert_eq!(status, StatusCode::NOT_FOUND, "{body}");
    let (status, _, body) = download(&router, "/api/v2/plans/2/exports/checklist", true).await;
    assert_eq!(
        status,
        StatusCode::NOT_FOUND,
        "another tenant's Build: {body}"
    );
    let (status, _, body) = download(&router, "/api/v2/plans/x/exports/checklist", true).await;
    assert_eq!(status, StatusCode::NOT_FOUND, "{body}");
    let (status, _, body) = download(&router, "/api/v2/plans/5/exports/checklist", true).await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(
        serde_json::from_str::<Value>(&body).unwrap()["code"],
        "no_accepted_plan"
    );

    // Compared while storage is open: shutdown removes its lease file.
    assert_eq!(
        data_files(&fixture.0),
        before_files,
        "a download stores no file"
    );
    assert_eq!(jobs(&fixture), before_jobs, "a download starts no job");
    owner.shutdown().unwrap();
}

#[tokio::test]
async fn checklist_is_not_mounted_with_the_flag_off() {
    let fixture = Fixture::new();
    let (owner, router) = mount(&fixture, "0");
    let (status, _, _) = download(&router, CHECKLIST, true).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    owner.shutdown().unwrap();
}

#[tokio::test]
async fn embeds_the_accepted_revision_thumbnail_when_a_newer_source_is_current() {
    let fixture = accepted_fixture();
    let root = &fixture.0;
    let accepted_stl = std::fs::read(root.join("repos/1/revisions/accepted/included.stl")).unwrap();
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
    make_current_revision(&fixture, &current_stl);

    let (owner, before, html) = checklist_download(&fixture).await;
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
    assert_eq!((data_files(root), jobs(&fixture)), before);
    owner.shutdown().unwrap();
}

#[tokio::test]
async fn omits_a_missing_accepted_thumbnail_without_failing_the_checklist() {
    let fixture = accepted_fixture();
    std::fs::remove_file(fixture.0.join("repos/1/revisions/accepted/included.stl")).unwrap();
    assert_thumbnail_omitted(&fixture).await;
}

#[tokio::test]
async fn omits_an_oversized_accepted_thumbnail_without_failing_the_checklist() {
    let fixture = accepted_fixture();
    std::fs::OpenOptions::new()
        .write(true)
        .open(fixture.0.join("repos/1/revisions/accepted/included.stl"))
        .unwrap()
        .set_len(GeometryLimits::default().max_bytes as u64 + 1)
        .unwrap();
    assert_thumbnail_omitted(&fixture).await;
}
