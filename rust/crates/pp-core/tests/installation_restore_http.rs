#![cfg(feature = "desktop_rebuild")]
//! `POST /api/v2/installation/restore/stage` and `POST /api/v2/installation/restore`
//! (GRE-1161) on a disposable, Rust-owned installation: a backup made and
//! downloaded through the mounted routes is staged and restored, and the
//! database and files come back with their IDs, drafts and history.

use pp_api::admin::AdminHttpConfig;
use pp_core::{
    DesktopRebuild,
    rebuild_api::{desktop_policy, product_router},
};
use pp_storage::{Limits, WriterOwner, ownership_marker::WriterGate, restore::CONFIRMATION};
use pp_testkit::{Installation, MarkerState, Spec};
use reqwest::Method;
use rusqlite::{Connection, OpenFlags};
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    time::Duration,
};

const STAGE: &str = "/api/v2/installation/restore/stage";
const RESTORE: &str = "/api/v2/installation/restore";

/// Builds 7 and 29, a Plan snapshot and an assistant decision (History), and
/// an open draft, written before the owner opens.
const SEED: &str = "INSERT INTO build_profiles(id, name) VALUES (7, 'Seven'), (29, 'Twenty-nine');
INSERT INTO plan_snapshots(id, profile_id, name, created_at, payload_json)
  VALUES (3, 29, 'before', '2026-10-01T00:00:00.000Z', '{\"layers\":2}');
INSERT INTO plan_drafts(id, profile_id, base_plan_version, state, digest_format,
  snapshot_digest, created_by, idempotency_key, created_at)
  VALUES (41, 29, 0, 'open', 'v1', 'aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa',
  'user:1', 'seed', '2026-10-01T00:00:00.000Z');";

/// The tables that hold Build IDs, drafts and History.
const KEPT_TABLES: [&str; 6] = [
    "build_profiles",
    "plan_drafts",
    "plan_snapshots",
    "plan_revisions",
    "plan_decisions",
    "plan_draft_parts",
];

struct Server {
    origin: String,
    client: reqwest::Client,
    cookie: String,
    serving: Option<pp_api::serving::Serving>,
    owner: Arc<Mutex<WriterOwner>>,
    root: PathBuf,
    _installation: Installation,
}

impl Server {
    async fn start() -> Self {
        let installation = Installation::create(Spec {
            node_database: true,
            marker: MarkerState::Valid,
            ..Spec::default()
        })
        .unwrap();
        Connection::open(installation.database_path())
            .unwrap()
            .execute_batch(SEED)
            .unwrap();
        let root = installation.path().canonicalize().unwrap();
        for (path, bytes) in [
            ("repos/source-a/part.stl", &b"solid a\nendsolid a\n"[..]),
            ("exports/plate.3mf", b"plate"),
        ] {
            std::fs::create_dir_all(root.join(path).parent().unwrap()).unwrap();
            std::fs::write(root.join(path), bytes).unwrap();
        }
        let owner = WriterOwner::open_with_gate(&root, Limits::default(), WriterGate::SwitchOn)
            .unwrap()
            .0;
        let owner = Arc::new(Mutex::new(owner));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let origin = format!("http://{}", listener.local_addr().unwrap());
        let router = product_router(
            DesktopRebuild::from_value(Some("1")),
            Arc::clone(&owner),
            &origin,
            None,
        )
        .unwrap()
        .expect("flag on mounts the product routes");
        let mut server = Self {
            client: reqwest::Client::builder().no_proxy().build().unwrap(),
            cookie: String::new(),
            serving: Some(pp_api::serving::Serving::start(listener, router)),
            origin,
            owner,
            root,
            _installation: installation,
        };
        let account = json!({"email": "owner@example.test", "password": "password-1234"});
        let signed_in = server
            .request(Method::POST, "/auth/register")
            .json(&account)
            .send()
            .await
            .unwrap();
        assert_eq!(signed_in.status(), 200);
        let set_cookie = signed_in.headers()["set-cookie"].to_str().unwrap();
        server.cookie = set_cookie.split(';').next().unwrap().to_owned();
        server
    }

    fn request(&self, method: Method, path: &str) -> reqwest::RequestBuilder {
        let request = self
            .client
            .request(method, format!("{}{path}", self.origin))
            .header("Origin", &self.origin);
        if self.cookie.is_empty() {
            request
        } else {
            request.header("Cookie", &self.cookie)
        }
    }

    async fn json(&self, request: reqwest::RequestBuilder) -> (u16, Value) {
        let response = request.send().await.unwrap();
        let status = response.status().as_u16();
        (status, response.json().await.unwrap_or(Value::Null))
    }

    /// [`Self::json`] for an installation route, which never lets an answer
    /// be cached.
    async fn installation(&self, request: reqwest::RequestBuilder) -> (u16, Value) {
        let response = request.send().await.unwrap();
        let cache = response.headers().get("cache-control").cloned();
        assert_eq!(cache.unwrap(), "private, no-store");
        let status = response.status().as_u16();
        (status, response.json().await.unwrap_or(Value::Null))
    }

    async fn get(&self, path: &str) -> Value {
        let (status, body) = self.json(self.request(Method::GET, path)).await;
        assert_eq!(status, 200, "GET {path}: {body}");
        body
    }

    async fn stage(&self, archive: Vec<u8>) -> (u16, Value) {
        let request = self
            .request(Method::POST, STAGE)
            .header("Content-Type", "application/gzip")
            .body(archive);
        self.installation(request).await
    }

    async fn restore(&self, body: Value) -> (u16, Value) {
        self.installation(self.request(Method::POST, RESTORE).json(&body))
            .await
    }

    async fn stop(mut self) {
        if let Some(mut serving) = self.serving.take() {
            serving.stop().await.unwrap();
        }
        let owner = Arc::into_inner(self.owner).expect("the router still holds the owner");
        owner.into_inner().unwrap().shutdown().unwrap();
    }
}

/// The rows of [`KEPT_TABLES`], sorted, and every file under `repos` and
/// `exports`.
fn kept(root: &Path) -> BTreeMap<String, String> {
    let db = Connection::open_with_flags(
        root.join("print-partner.db"),
        OpenFlags::SQLITE_OPEN_READ_ONLY,
    )
    .unwrap();
    db.busy_timeout(Duration::from_secs(5)).unwrap();
    let mut state = BTreeMap::new();
    for table in KEPT_TABLES {
        let mut select = db.prepare(&format!("SELECT * FROM \"{table}\"")).unwrap();
        let width = select.column_count();
        let mut rows = select.query([]).unwrap();
        let mut found = Vec::new();
        while let Some(row) = rows.next().unwrap() {
            let values: Vec<_> = (0..width).map(|i| row.get_ref(i).unwrap()).collect();
            found.push(format!("{values:?}"));
        }
        found.sort();
        state.insert(format!("table {table}"), found.join("\n"));
    }
    for name in ["repos", "exports"] {
        let files = pp_testkit::snapshot(&root.join(name)).unwrap();
        state.insert(format!("root {name}"), format!("{files:?}"));
    }
    state
}

/// The Build list, Build 29's drafts and Build 29's History as the routes
/// serve them.
async fn served(server: &Server) -> Vec<Value> {
    vec![
        server.get("/api/v2/plans").await,
        server.get("/api/v2/plans/29/drafts").await,
        server.get("/api/v2/plans/29/history").await,
    ]
}

fn work_entries(root: &Path) -> Vec<String> {
    let Ok(entries) = std::fs::read_dir(root.join(".pp-restore")) else {
        return Vec::new();
    };
    let mut names: Vec<String> = entries
        .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    names
}

async fn backup_bytes(server: &Server) -> Vec<u8> {
    let (status, created) = server
        .installation(server.request(Method::POST, "/api/v2/installation/backup"))
        .await;
    assert_eq!(status, 200, "{created}");
    let response = server
        .request(Method::GET, "/api/v2/installation/backup/archive")
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 200);
    response.bytes().await.unwrap().to_vec()
}

/// Credentials, Origin, a broken archive, a wrong confirmation and an
/// unknown stage are refused, and none of them leaves an upload behind.
async fn refusals(server: &Server) {
    for path in [STAGE, RESTORE] {
        let anonymous = server
            .client
            .post(format!("{}{path}", server.origin))
            .header("Origin", &server.origin)
            .send()
            .await
            .unwrap();
        assert_eq!(anonymous.status(), 401, "{path}");
        let foreign = server
            .request(Method::POST, path)
            .header("Origin", "http://evil.example")
            .send()
            .await
            .unwrap();
        assert_eq!(foreign.status(), 403, "{path}");
    }
    let (status, body) = server.stage(b"not a backup".to_vec()).await;
    assert_eq!(status, 422, "{body}");
    assert_eq!(body["detail"], "Invalid backup archive");
    assert!(work_entries(&server.root).is_empty());
    let unknown = json!({"restore_id": "0".repeat(32), "confirmation": CONFIRMATION});
    let (status, body) = server.restore(unknown).await;
    assert_eq!(status, 404, "{body}");
    assert_eq!(body["code"], "restore_not_found");
    let (status, body) = server
        .restore(json!({"restore_id": "../../etc", "confirmation": CONFIRMATION}))
        .await;
    assert_eq!(
        (status, body["code"].clone()),
        (404, json!("restore_not_found"))
    );
    let extra = json!({"restore_id": "0".repeat(32), "confirmation": CONFIRMATION, "path": "/"});
    let (status, body) = server.restore(extra).await;
    assert_eq!(status, 400, "{body}");
    assert_eq!(body["type"], "urn:print-partner:problem:validation");
}

/// A second stage while the first waits for the owner lock is refused.
async fn one_restore_at_a_time(server: &Server, archive: &[u8]) {
    let (release_tx, release_rx) = std::sync::mpsc::channel();
    let (held_tx, held_rx) = std::sync::mpsc::channel();
    let owner = Arc::clone(&server.owner);
    let holder = std::thread::spawn(move || {
        let guard = owner.lock().unwrap();
        held_tx.send(()).unwrap();
        let _ = release_rx.recv();
        drop(guard);
    });
    held_rx.recv().unwrap();
    let first = server.stage(archive.to_vec());
    let second = server.stage(archive.to_vec());
    tokio::pin!(first, second);
    let refused = tokio::time::timeout(Duration::from_secs(10), async {
        tokio::select! {
            refused = &mut first => (refused, true),
            refused = &mut second => (refused, false),
        }
    })
    .await
    .expect("one stage was refused while the owner lock was held");
    let ((status, body), first_refused) = refused;
    assert_eq!(status, 503, "{body}");
    assert_eq!(body["detail"], "A restore is already running");
    release_tx.send(()).unwrap();
    holder.join().unwrap();
    let (status, body) = if first_refused {
        second.await
    } else {
        first.await
    };
    assert_eq!(status, 200, "{body}");
}

#[tokio::test]
async fn routes_are_absent_when_the_rebuild_flag_is_off() {
    let root = tempfile::tempdir().unwrap();
    let owner = WriterOwner::open(root.path(), Limits::default()).unwrap().0;
    let reads = owner.accepted_reads_with_policy(desktop_policy()).unwrap();
    let owner = Arc::new(Mutex::new(owner));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let origin = format!("http://{}", listener.local_addr().unwrap());
    let router = pp_api::restore::restore_router(
        AdminHttpConfig::from_startup(&origin, "default", Some("0")).unwrap(),
        Arc::clone(&owner),
        reads,
    );
    let mut serving = pp_api::serving::Serving::start(listener, router);
    let client = reqwest::Client::builder().no_proxy().build().unwrap();
    for path in [STAGE, RESTORE] {
        let response = client
            .post(format!("{origin}{path}"))
            .header("Origin", &origin)
            .body("x")
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), 404, "{path}");
        assert_eq!(response.text().await.unwrap(), "");
    }
    serving.stop().await.unwrap();
    assert!(!root.path().join(".pp-restore").exists());
    let owner = Arc::into_inner(owner).unwrap().into_inner().unwrap();
    owner.shutdown().unwrap();
}

#[tokio::test]
async fn a_downloaded_backup_restores_ids_drafts_history_and_files() {
    let server = Server::start().await;
    refusals(&server).await;
    let backed_up = kept(&server.root);
    let served_before = served(&server).await;
    let ids: Vec<_> = (served_before[0]["profiles"].as_array().unwrap().iter())
        .map(|build| build["id"].clone())
        .collect();
    assert!(
        ids.contains(&json!(7)) && ids.contains(&json!(29)),
        "{ids:?}"
    );
    let drafts = served_before[1]["drafts"].as_array().unwrap();
    assert!(
        drafts.iter().any(|draft| draft["draft_id"] == 41),
        "{drafts:?}"
    );
    assert!(
        served_before[2].to_string().contains("before"),
        "{}",
        served_before[2]
    );
    let archive = backup_bytes(&server).await;

    // Change the database and the files after the backup.
    let (status, later) = server
        .json(
            server
                .request(Method::POST, "/api/v2/plans")
                .json(&json!({"name": "After the backup"})),
        )
        .await;
    assert_eq!(status, 200, "{later}");
    std::fs::remove_file(server.root.join("repos/source-a/part.stl")).unwrap();
    std::fs::write(server.root.join("exports/later.3mf"), b"later").unwrap();
    assert_ne!(kept(&server.root), backed_up);

    one_restore_at_a_time(&server, &archive).await;
    let (status, preview) = server.stage(archive).await;
    assert_eq!(status, 200, "{preview}");
    assert_eq!(preview["confirmation"], CONFIRMATION);
    assert!(preview["schema_version"].as_u64().unwrap() > 0);
    let restore_id = preview["restore_id"].as_str().unwrap().to_owned();
    assert!(
        !preview
            .to_string()
            .contains(&server.root.display().to_string())
    );
    // The newer stage replaced the one staged while the lock was held.
    assert_eq!(work_entries(&server.root), [format!("staged-{restore_id}")]);

    let (status, body) = server
        .restore(json!({"restore_id": restore_id, "confirmation": "yes"}))
        .await;
    assert_eq!(status, 400, "{body}");
    assert_eq!(body["detail"], "Restore confirmation does not match");
    assert_ne!(kept(&server.root), backed_up);

    let confirmed = json!({"restore_id": restore_id, "confirmation": CONFIRMATION});
    let (status, outcome) = server.restore(confirmed.clone()).await;
    assert_eq!(status, 200, "{outcome}");
    assert!(
        outcome["schema_version"].as_u64().unwrap() >= preview["schema_version"].as_u64().unwrap()
    );
    assert_eq!(kept(&server.root), backed_up);
    assert_eq!(served(&server).await, served_before);
    let later_path = format!("/api/v2/plans/{}", later["id"]);
    let (status, _) = server.json(server.request(Method::GET, &later_path)).await;
    assert_eq!(status, 404);

    // A stage is used once.
    let (status, body) = server.restore(confirmed).await;
    assert_eq!(
        (status, body["code"].clone()),
        (404, json!("restore_not_found"))
    );
    server.stop().await;
}
