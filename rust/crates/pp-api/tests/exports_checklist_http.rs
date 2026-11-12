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
use checkoff_http::{Fixture, post, send};
use pp_api::{accepted::accepted_router, checkoff::checkoff_router, exports::exports_router};
use pp_storage::WriterOwner;
use serde_json::{Value, json};
use std::{collections::BTreeSet, path::Path};

#[path = "support/checkoff_http.rs"]
mod checkoff_http;
#[path = "../../pp-storage/tests/common/js_json.rs"]
mod js_json;

const CHECKLIST: &str = "/api/v2/plans/1/exports/checklist";

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
    let headers: Vec<(&str, &str)> = credential.into_iter().collect();
    let response =
        checkoff_http::loopback::respond(router, "GET", path, None, Vec::new(), None, &headers)
            .await;
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
    let jobs = |fixture: &Fixture| -> i64 {
        fixture
            .sql()
            .query_row("SELECT COUNT(*) FROM durable_jobs", [], |r| r.get(0))
            .unwrap()
    };
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
        "default-src 'none'; style-src 'unsafe-inline'"
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
