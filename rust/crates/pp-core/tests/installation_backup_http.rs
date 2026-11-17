#![cfg(feature = "desktop_rebuild")]
//! `POST /api/v2/installation/backup` and `GET /api/v2/installation/backup/archive`
//! (GRE-1371) on a disposable installation, and the absence of these and the
//! GRE-1161 restore routes with the flag off.

#[path = "support/installation.rs"]
mod support_installation;

use pp_core::{DesktopRebuild, rebuild_api::product_router};
use pp_storage::{Limits, WriterOwner, archive};
use reqwest::Method;
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::{
    io::Read,
    path::Path,
    sync::{Arc, Mutex},
    time::Duration,
};
use support_installation::Server;

const SOURCE: &[u8] = b"solid a\nendsolid a\n";

/// A disposable data directory with one Source file, served with `flag`.
async fn start(flag: Option<&str>) -> Server {
    let root = tempfile::tempdir().unwrap();
    let data = root.path().join("data");
    let owner = WriterOwner::open(&data, Limits::default()).unwrap().0;
    std::fs::create_dir_all(data.join("repos/source-a")).unwrap();
    std::fs::write(data.join("repos/source-a/part.stl"), SOURCE).unwrap();
    Server::start(owner, data, root, flag).await
}

fn export_dirs(data: &Path) -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(data.join("backups"))
        .map(|entries| {
            entries
                .flatten()
                .map(|entry| entry.file_name().to_string_lossy().into_owned())
                .filter(|name| name.starts_with("export-"))
                .collect()
        })
        .unwrap_or_default();
    names.sort();
    names
}

fn entry_bytes(archive: &[u8], name: &str) -> Vec<u8> {
    let decoder = flate2::read::GzDecoder::new(std::io::Cursor::new(archive));
    let mut archive = tar::Archive::new(decoder);
    let mut entries = archive.entries().unwrap();
    let mut entry = entries
        .find(|entry| entry.as_ref().unwrap().path().unwrap().to_string_lossy() == name)
        .unwrap_or_else(|| panic!("archive has no {name}"))
        .unwrap();
    let mut bytes = Vec::new();
    entry.read_to_end(&mut bytes).unwrap();
    bytes
}

#[tokio::test]
async fn routes_are_absent_when_the_rebuild_flag_is_off() {
    let root = tempfile::tempdir().unwrap();
    let owner = WriterOwner::open(root.path(), Limits::default()).unwrap().0;
    let owner = Arc::new(Mutex::new(owner));
    for flag in [None, Some("0"), Some("false"), Some("TRUE")] {
        assert!(
            product_router(
                DesktopRebuild::from_value(flag),
                Arc::clone(&owner),
                "http://127.0.0.1:9",
                None,
            )
            .unwrap()
            .is_none(),
            "{flag:?}"
        );
    }
    let server = start(Some("0")).await;
    for (method, path) in [
        (Method::POST, "/api/v2/installation/backup"),
        (Method::GET, "/api/v2/installation/backup/archive"),
        (Method::POST, "/api/v2/installation/restore/stage"),
        (Method::POST, "/api/v2/installation/restore"),
    ] {
        let (status, response) = server.exchange(method.clone(), path).await;
        assert_eq!(status, 404, "{method} {path}");
        assert_eq!(response.text().await.unwrap(), "");
    }
    assert!(export_dirs(&server.data).is_empty());
    assert!(!server.data.join(".pp-restore").exists());
    server.stop().await;
}

async fn refused_without_a_session(server: &Server, path: &str, method: Method) {
    let response = server
        .client
        .request(method, format!("{}{path}", server.origin))
        .header("Origin", &server.origin)
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 401);
    assert_eq!(response.headers()["cache-control"], "private, no-store");
}

async fn refused_from_another_origin(server: &Server, path: &str, method: Method) {
    let response = server
        .client
        .request(method, format!("{}{path}", server.origin))
        .header("Origin", "http://evil.example")
        .header("Cookie", &server.cookie)
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 403);
}

/// One create holds the owner lock, so the other create and a download are
/// refused. Returns the backup that finished after the lock was released.
async fn created_while_the_owner_is_held(server: &Server, create: &str, archive: &str) -> Value {
    let held = server.hold_owner();
    let first = server.request(Method::POST, create).send();
    let second = server.request(Method::POST, create).send();
    tokio::pin!(first, second);
    let (label, refused) = tokio::time::timeout(Duration::from_secs(10), async {
        tokio::select! {
            response = &mut first => ("first", response),
            response = &mut second => ("second", response),
        }
    })
    .await
    .expect("one create was refused while the owner lock was held");
    let refused = refused.unwrap();
    assert_eq!(refused.status(), 503);
    let refused: Value = refused.json().await.unwrap();
    assert_eq!(refused["detail"], "A backup is already running");
    let (status, during) = server.json(Method::GET, archive).await;
    assert_eq!(status, 503, "{during}");
    assert_eq!(during["detail"], "A backup is already running");
    held.release();
    let created = match label {
        "first" => second.await.unwrap(),
        "second" => first.await.unwrap(),
        other => panic!("unexpected create: {other}"),
    };
    assert_eq!(created.status(), 200);
    assert_eq!(created.headers()["cache-control"], "private, no-store");
    assert_eq!(created.headers()["content-type"], "application/json");
    created.json().await.unwrap()
}

fn assert_created(server: &Server, created: &Value) -> (String, u64) {
    assert_eq!(created["name"], "print-partner-backup.tar.gz");
    assert!(created.get("path").is_none(), "{created}");
    assert!(
        !created
            .to_string()
            .contains(&server.data.display().to_string())
    );
    let sha256 = created["sha256"].as_str().unwrap().to_owned();
    assert_eq!(sha256.len(), 64);
    assert!(sha256.bytes().all(|byte| byte.is_ascii_hexdigit()));
    let size = created["size_bytes"].as_u64().unwrap();
    assert!(size > 0);
    assert_eq!(export_dirs(&server.data).len(), 1);
    (sha256, size)
}

async fn assert_download(server: &Server, archive: &str, created: &Value, sha256: &str, size: u64) {
    let (status, response) = server.exchange(Method::GET, archive).await;
    assert_eq!(status, 200);
    assert_eq!(response.headers()["content-type"], "application/gzip");
    assert_eq!(response.headers()["cache-control"], "private, no-store");
    assert_eq!(
        response.headers()["content-disposition"],
        "attachment; filename=\"print-partner-backup.tar.gz\""
    );
    let bytes = response.bytes().await.unwrap();
    assert_eq!(bytes.len() as u64, size);
    assert_eq!(hex::encode(Sha256::digest(&bytes)), sha256);
    let downloaded = server.data.join("../downloaded.tar.gz");
    std::fs::write(&downloaded, &bytes).unwrap();
    let metadata = archive::validate(&downloaded, archive::Limits::backup()).unwrap();
    assert_eq!(metadata.created_at, created["created_at"].as_str().unwrap());
    assert_eq!(
        metadata.app_version,
        created["app_version"].as_str().unwrap()
    );
    assert_eq!(metadata.scope, archive::MetadataScope::Full);
    assert!(!entry_bytes(&bytes, "print-partner.db").is_empty());
    assert_eq!(entry_bytes(&bytes, "repos/source-a/part.stl"), SOURCE);
}

#[tokio::test]
async fn an_authenticated_client_creates_and_downloads_one_backup() {
    let server = start(Some("1")).await;
    let create = "/api/v2/installation/backup";
    let archive = "/api/v2/installation/backup/archive";
    refused_without_a_session(&server, create, Method::POST).await;
    refused_without_a_session(&server, archive, Method::GET).await;
    assert!(export_dirs(&server.data).is_empty());
    refused_from_another_origin(&server, create, Method::POST).await;
    refused_from_another_origin(&server, archive, Method::GET).await;
    assert!(export_dirs(&server.data).is_empty());

    let (status, missing) = server.json(Method::GET, archive).await;
    assert_eq!(status, 404, "{missing}");
    assert_eq!(missing["detail"], "No backup has been created");
    assert_eq!(missing["type"], "urn:print-partner:problem:not-found");

    let created = created_while_the_owner_is_held(&server, create, archive).await;
    let (sha256, size) = assert_created(&server, &created);
    assert_download(&server, archive, &created, &sha256, size).await;

    std::fs::write(
        server.data.join("repos/source-a/part.stl"),
        b"solid a\nendsolid a\nmore\n",
    )
    .unwrap();
    let (status, again) = server.json(Method::POST, create).await;
    assert_eq!(status, 200, "{again}");
    assert_ne!(again["sha256"], created["sha256"]);
    assert_eq!(export_dirs(&server.data).len(), 1);
    let (status, response) = server.exchange(Method::GET, archive).await;
    assert_eq!(status, 200);
    let bytes = response.bytes().await.unwrap();
    assert_eq!(
        hex::encode(Sha256::digest(&bytes)),
        again["sha256"].as_str().unwrap()
    );
    assert_eq!(bytes.len() as u64, again["size_bytes"].as_u64().unwrap());
    server.stop().await;
}
