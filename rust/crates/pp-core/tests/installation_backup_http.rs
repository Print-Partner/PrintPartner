//! Whole-installation backup and confirmed restore through the composed
//! product router, on a disposable pp-testkit installation with a copied
//! Node schema 34 database and a valid ownership marker.
#![cfg(feature = "desktop_rebuild")]

use pp_core::{DesktopRebuild, rebuild_api::product_router};
use pp_storage::{Limits, WriterOwner, ownership_marker::WriterGate, restore::CONFIRMATION};
use pp_testkit::{Installation, MarkerState, Spec};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::sync::{Arc, Mutex};

const SEED: &str = "INSERT INTO build_profiles(id, name) VALUES (7, 'Seven'), (29, 'Twenty-nine');
INSERT INTO plan_drafts(id, profile_id, base_plan_version, state, digest_format,
  snapshot_digest, created_by, idempotency_key, created_at)
  VALUES (41, 29, 0, 'open', 'v1', 'aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa',
  'user:1', 'seed', '2026-10-01T00:00:00.000Z');";

const STAGE: &str = "/api/v2/installation/restore/stage";
const RESTORE: &str = "/api/v2/installation/restore";

struct Server {
    origin: String,
    client: reqwest::Client,
    cookie: String,
}

impl Server {
    /// Sends `body` as JSON, or as gzip bytes, and returns the status and body.
    async fn send(&self, method: &str, path: &str, body: Option<Value>) -> (u16, Vec<u8>) {
        let mut request = self
            .client
            .request(method.parse().unwrap(), format!("{}{path}", self.origin))
            .header("Origin", &self.origin)
            .header("Cookie", &self.cookie);
        request = match body {
            Some(Value::String(bytes)) => request
                .header("Content-Type", "application/gzip")
                .body(hex::decode(bytes).unwrap()),
            Some(body) => request.json(&body),
            None => request,
        };
        let response = request.send().await.unwrap();
        let status = response.status().as_u16();
        (status, response.bytes().await.unwrap().to_vec())
    }

    async fn json(&self, method: &str, path: &str, body: Option<Value>) -> (u16, Value) {
        let (status, bytes) = self.send(method, path, body).await;
        (
            status,
            serde_json::from_slice(&bytes).unwrap_or(Value::Null),
        )
    }

    async fn build_names(&self) -> Vec<String> {
        let (_, body) = self.json("GET", "/api/v2/plans", None).await;
        let mut names: Vec<String> = (body["profiles"].as_array().unwrap().iter())
            .map(|build| build["name"].as_str().unwrap().to_owned())
            .collect();
        names.sort();
        names
    }
}

type Served = (
    tokio::sync::oneshot::Sender<()>,
    tokio::task::JoinHandle<()>,
    Arc<Mutex<WriterOwner>>,
);

/// Serves `product_router` over a writer opened on `root` with the marker
/// decision on.
async fn serve(root: &std::path::Path) -> (Server, Served) {
    let owner = WriterOwner::open_with_gate(root, Limits::default(), WriterGate::SwitchOn);
    let owner = Arc::new(Mutex::new(owner.unwrap().0));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let origin = format!("http://{}", listener.local_addr().unwrap());
    let flag = DesktopRebuild::from_value(Some("1"));
    let router = product_router(flag, Arc::clone(&owner), &origin, None).unwrap();
    let service = router
        .unwrap()
        .into_make_service_with_connect_info::<std::net::SocketAddr>();
    let (stop, stopped) = tokio::sync::oneshot::channel::<()>();
    let served = tokio::spawn(async move {
        let stopped = async { drop(stopped.await) };
        axum::serve(listener, service)
            .with_graceful_shutdown(stopped)
            .await
            .unwrap();
    });
    let server = Server {
        origin,
        client: reqwest::Client::builder().no_proxy().build().unwrap(),
        cookie: String::new(),
    };
    (server, (stop, served, owner))
}

#[tokio::test(flavor = "multi_thread")]
async fn backup_and_confirmed_restore_round_trip_through_mounted_routes() {
    let spec = Spec {
        node_database: true,
        marker: MarkerState::Valid,
        ..Spec::default()
    };
    let installation = Installation::create(spec).unwrap();
    let db = installation.database_path();
    rusqlite::Connection::open(&db)
        .unwrap()
        .execute_batch(SEED)
        .unwrap();
    let root = installation.path().canonicalize().unwrap();
    let part = root.join("repos/source-a/part.stl");
    std::fs::create_dir_all(part.parent().unwrap()).unwrap();
    std::fs::write(&part, b"solid a\n").unwrap();
    let (mut server, (stop, served, owner)) = serve(&root).await;
    let backup = "/api/v2/installation/backup";
    let (stage, restore) = (STAGE, RESTORE);

    // Every route refuses an anonymous request before any work.
    let archive_path = format!("{backup}/archive");
    let routes = [("POST", backup), ("GET", &archive_path), ("POST", stage)];
    for (method, path) in routes.into_iter().chain([("POST", restore)]) {
        let status = server.send(method, path, None).await.0;
        assert_eq!(status, 401, "{method} {path}");
    }
    assert!(!root.join(".pp-restore").exists());
    let account = json!({"email": "owner@example.com", "password": "password-1234"});
    let registered = (server
        .client
        .post(format!("{}/auth/register", server.origin)))
    .header("Origin", &server.origin)
    .json(&account)
    .send()
    .await
    .unwrap();
    let cookie = registered.headers()["set-cookie"].to_str().unwrap();
    server.cookie = cookie.split(';').next().unwrap().to_owned();
    assert_eq!(server.build_names().await, ["Seven", "Twenty-nine"]);

    let (status, created) = server.json("POST", backup, None).await;
    assert_eq!(status, 200, "{created}");
    let (status, archive) = server.send("GET", &archive_path, None).await;
    assert_eq!(status, 200);
    assert_eq!(
        archive.len() as u64,
        created["size_bytes"].as_u64().unwrap()
    );
    assert_eq!(hex::encode(Sha256::digest(&archive)), created["sha256"]);

    // Change the installation after the backup.
    let plan = json!({"name": "After backup"});
    assert_eq!(
        server.send("POST", "/api/v2/plans", Some(plan)).await.0,
        200
    );
    std::fs::remove_file(&part).unwrap();
    assert_eq!(server.build_names().await.len(), 3);

    // A damaged upload is refused and changes nothing.
    let damaged = Some(Value::String(hex::encode(b"not a backup")));
    assert_eq!(server.send("POST", stage, damaged).await.0, 422);
    let (status, preview) = server
        .json("POST", stage, Some(Value::String(hex::encode(&archive))))
        .await;
    assert_eq!(status, 200, "{preview}");
    assert_eq!(preview["confirmation"], CONFIRMATION);
    assert!(CONFIRMATION.contains("printers") && CONFIRMATION.contains("Spoolman"));
    let id = preview["restore_id"].clone();
    for (body, expected) in [
        (json!({"restore_id": id, "confirmation": "yes"}), 400),
        (
            json!({"restore_id": "0".repeat(32), "confirmation": CONFIRMATION}),
            404,
        ),
        (
            json!({"restore_id": id, "confirmation": CONFIRMATION, "path": "/"}),
            400,
        ),
    ] {
        assert_eq!(
            server.send("POST", restore, Some(body.clone())).await.0,
            expected,
            "{body}"
        );
    }
    assert_eq!(server.build_names().await.len(), 3);

    let confirmed = json!({"restore_id": id, "confirmation": CONFIRMATION});
    let (status, outcome) = server.json("POST", restore, Some(confirmed)).await;
    assert_eq!(status, 200, "{outcome}");
    assert_eq!(
        outcome["restored_schema_version"],
        outcome["schema_version"]
    );

    assert_restored(&server, &root).await;
    stop.send(()).unwrap();
    served.await.unwrap();
    let owner = Arc::into_inner(owner).expect("routers dropped");
    owner.into_inner().unwrap().shutdown().unwrap();
}

/// The session in the backup still holds; IDs, drafts, files and the
/// ownership marker are back.
async fn assert_restored(server: &Server, root: &std::path::Path) {
    assert_eq!(server.build_names().await, ["Seven", "Twenty-nine"]);
    let (status, build) = server.json("GET", "/api/v2/plans/29", None).await;
    assert_eq!((status, build["id"].as_i64()), (200, Some(29)), "{build}");
    let db = rusqlite::Connection::open(root.join("print-partner.db")).unwrap();
    let drafts: String = db
        .query_row("SELECT group_concat(id) FROM plan_drafts", [], |row| {
            row.get(0)
        })
        .unwrap();
    assert_eq!(drafts, "41");
    let part = std::fs::read(root.join("repos/source-a/part.stl")).unwrap();
    assert_eq!(part, b"solid a\n");
    assert!(matches!(
        pp_storage::ownership_marker::inspect(root),
        pp_storage::ownership_marker::MarkerState::Valid(_)
    ));
}
