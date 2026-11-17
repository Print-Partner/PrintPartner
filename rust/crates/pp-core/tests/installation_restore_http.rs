#![cfg(feature = "desktop_rebuild")]
//! `POST /api/v2/installation/restore/stage` and `POST /api/v2/installation/restore`
//! (GRE-1161) on a disposable, Rust-owned installation: a backup made and
//! downloaded through the mounted routes is staged and restored, and the
//! database and files come back with their IDs, drafts and History. Their
//! absence with the flag off is in `installation_backup_http.rs`.

#[path = "support/installation.rs"]
mod support_installation;

use pp_storage::{Limits, WriterOwner, ownership_marker::WriterGate, restore::CONFIRMATION};
use pp_testkit::{Installation, restore_fixture::table_rows};
use reqwest::Method;
use rusqlite::{Connection, OpenFlags};
use serde_json::{Value, json};
use std::{collections::BTreeMap, path::Path, time::Duration};
use support_installation::Server;

const STAGE: &str = "/api/v2/installation/restore/stage";
const RESTORE: &str = "/api/v2/installation/restore";

/// The tables that hold Build IDs, drafts and History.
const KEPT_TABLES: [&str; 6] = [
    "build_profiles",
    "plan_drafts",
    "plan_draft_parts",
    "plan_snapshots",
    "plan_revisions",
    "plan_decisions",
];

/// The seeded installation, opened as the Rust-only runtime opens it.
async fn start() -> Server {
    let installation = Installation::with_builds_drafts_and_history().unwrap();
    let data = installation.path().canonicalize().unwrap();
    let opened = WriterOwner::open_with_gate(&data, Limits::default(), WriterGate::SwitchOn);
    Server::start(opened.unwrap().0, data, installation, Some("1")).await
}

/// Sends an installation route's request; its answer is never cacheable.
async fn installation_json(request: reqwest::RequestBuilder) -> (u16, Value) {
    let response = request.send().await.unwrap();
    let cache = response.headers().get("cache-control").cloned();
    assert_eq!(cache.unwrap(), "private, no-store");
    let status = response.status().as_u16();
    (status, response.json().await.unwrap_or(Value::Null))
}

async fn stage(server: &Server, archive: Vec<u8>) -> (u16, Value) {
    let request = server.request(Method::POST, STAGE);
    installation_json(
        request
            .header("Content-Type", "application/gzip")
            .body(archive),
    )
    .await
}

async fn restore(server: &Server, body: Value) -> (u16, Value) {
    installation_json(server.request(Method::POST, RESTORE).json(&body)).await
}

/// The rows of [`KEPT_TABLES`] and every file under `repos` and `exports`.
fn kept(data: &Path) -> BTreeMap<String, String> {
    let flags = OpenFlags::SQLITE_OPEN_READ_ONLY;
    let db = Connection::open_with_flags(data.join("print-partner.db"), flags).unwrap();
    db.busy_timeout(Duration::from_secs(5)).unwrap();
    let mut state = BTreeMap::new();
    for table in KEPT_TABLES {
        state.insert(format!("table {table}"), table_rows(&db, table).unwrap());
    }
    for name in ["repos", "exports"] {
        let files = pp_testkit::snapshot(&data.join(name)).unwrap();
        state.insert(format!("root {name}"), format!("{files:?}"));
    }
    state
}

/// The Build list, Build 29's drafts and Build 29's History as served.
async fn served(server: &Server) -> Vec<Value> {
    let mut answers = Vec::new();
    for path in [
        "/api/v2/plans",
        "/api/v2/plans/29/drafts",
        "/api/v2/plans/29/history",
    ] {
        let (status, body) = server.json(Method::GET, path).await;
        assert_eq!(status, 200, "GET {path}: {body}");
        answers.push(body);
    }
    answers
}

fn work_entries(data: &Path) -> Vec<String> {
    let Ok(entries) = std::fs::read_dir(data.join(".pp-restore")) else {
        return Vec::new();
    };
    let mut names: Vec<String> = entries
        .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    names
}

async fn downloaded_backup(server: &Server) -> Vec<u8> {
    let create = server.request(Method::POST, "/api/v2/installation/backup");
    let (status, created) = installation_json(create).await;
    assert_eq!(status, 200, "{created}");
    let (status, archive) = server
        .exchange(Method::GET, "/api/v2/installation/backup/archive")
        .await;
    assert_eq!(status, 200);
    archive.bytes().await.unwrap().to_vec()
}

/// Credentials, Origin, a broken archive, unknown stages and unknown request
/// members are refused, and none of them leaves an upload behind.
async fn refusals(server: &Server) {
    for path in [STAGE, RESTORE] {
        let anonymous = server.client.post(format!("{}{path}", server.origin));
        let anonymous = anonymous.header("Origin", &server.origin).send().await;
        assert_eq!(anonymous.unwrap().status(), 401, "{path}");
        let foreign = server.request(Method::POST, path);
        let foreign = foreign.header("Origin", "http://evil.example").send().await;
        assert_eq!(foreign.unwrap().status(), 403, "{path}");
    }
    let (status, body) = stage(server, b"not a backup".to_vec()).await;
    assert_eq!(
        (status, &body["detail"]),
        (422, &json!("Invalid backup archive"))
    );
    assert!(work_entries(&server.data).is_empty());
    for restore_id in ["0".repeat(32), "../../etc".into()] {
        let unknown = json!({"restore_id": restore_id, "confirmation": CONFIRMATION});
        let (status, body) = restore(server, unknown).await;
        assert_eq!((status, &body["code"]), (404, &json!("restore_not_found")));
    }
    let extra = json!({"restore_id": "0".repeat(32), "confirmation": CONFIRMATION, "path": "/"});
    let (status, body) = restore(server, extra).await;
    assert_eq!(status, 400, "{body}");
    assert_eq!(body["type"], "urn:print-partner:problem:validation");
}

/// A second stage while the first waits for the owner lock is refused, and
/// the first stages once the lock is released.
async fn one_restore_at_a_time(server: &Server, archive: &[u8]) {
    let held = server.hold_owner();
    let first = stage(server, archive.to_vec());
    let second = stage(server, archive.to_vec());
    tokio::pin!(first, second);
    let (refused, first_refused) = tokio::time::timeout(Duration::from_secs(10), async {
        tokio::select! {
            refused = &mut first => (refused, true),
            refused = &mut second => (refused, false),
        }
    })
    .await
    .expect("one stage was refused while the owner lock was held");
    assert_eq!(refused.0, 503, "{}", refused.1);
    assert_eq!(refused.1["detail"], "A restore is already running");
    held.release();
    let staged = if first_refused {
        second.await
    } else {
        first.await
    };
    assert_eq!(staged.0, 200, "{}", staged.1);
}

/// The seeded Builds, draft and History are what the compared reads hold.
fn assert_seeded(served: &[Value]) {
    let builds = served[0]["profiles"].as_array().unwrap();
    let ids: Vec<_> = builds.iter().map(|build| build["id"].clone()).collect();
    assert!(
        ids.contains(&json!(7)) && ids.contains(&json!(29)),
        "{ids:?}"
    );
    let drafts = served[1]["drafts"].as_array().unwrap();
    assert!(
        drafts.iter().any(|draft| draft["draft_id"] == 41),
        "{drafts:?}"
    );
    assert!(served[2].to_string().contains("before"), "{}", served[2]);
}

#[tokio::test]
async fn a_downloaded_backup_restores_ids_drafts_history_and_files() {
    let server = start().await;
    refusals(&server).await;
    let backed_up = kept(&server.data);
    let served_before = served(&server).await;
    assert_seeded(&served_before);
    let archive = downloaded_backup(&server).await;

    // Change the database and the files after the backup.
    let create = server.request(Method::POST, "/api/v2/plans");
    let later = create
        .json(&json!({"name": "After the backup"}))
        .send()
        .await;
    let later: Value = later.unwrap().json().await.unwrap();
    std::fs::remove_file(server.data.join("repos/source-a/part.stl")).unwrap();
    std::fs::write(server.data.join("exports/later.3mf"), b"later").unwrap();
    assert_ne!(kept(&server.data), backed_up);

    one_restore_at_a_time(&server, &archive).await;
    let (status, preview) = stage(&server, archive).await;
    assert_eq!(status, 200, "{preview}");
    assert_eq!(preview["confirmation"], CONFIRMATION);
    let restore_id = preview["restore_id"].as_str().unwrap().to_owned();
    assert!(
        !preview
            .to_string()
            .contains(&*server.data.to_string_lossy())
    );
    // The newer stage replaced the one staged while the lock was held.
    assert_eq!(work_entries(&server.data), [format!("staged-{restore_id}")]);

    let wrong = json!({"restore_id": restore_id, "confirmation": "yes"});
    let (status, body) = restore(&server, wrong).await;
    assert_eq!(status, 400, "{body}");
    assert_eq!(body["detail"], "Restore confirmation does not match");
    assert_ne!(kept(&server.data), backed_up);

    let confirmed = json!({"restore_id": restore_id, "confirmation": CONFIRMATION});
    let (status, outcome) = restore(&server, confirmed.clone()).await;
    assert_eq!(status, 200, "{outcome}");
    assert_eq!(
        outcome["restored_schema_version"],
        preview["schema_version"]
    );
    assert_eq!(kept(&server.data), backed_up);
    assert_eq!(served(&server).await, served_before);
    let later = format!("/api/v2/plans/{}", later["id"]);
    assert_eq!(server.json(Method::GET, &later).await.0, 404);

    // A stage is used once.
    let (status, body) = restore(&server, confirmed).await;
    assert_eq!((status, &body["code"]), (404, &json!("restore_not_found")));
    server.stop().await;
}
