#![cfg(feature = "desktop_rebuild")]

#[path = "support/desktop_http.rs"]
mod desktop_http;
#[path = "support/release.rs"]
mod release;

use pp_core::CoreRuntime;
use reqwest::{Client, StatusCode, header};
use serde_json::{Value, json};
use std::{
    fs,
    path::{Path, PathBuf},
    time::Duration,
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpStream,
    time::timeout,
};

const PROFILE: &str = "GRE-2037 synthetic Orca process";
const STREAM: &str = "/ws/profile-sync";
const LIBRARY: &str = "/profile-library";
const WAIT: Duration = Duration::from_secs(20);

#[tokio::test]
async fn profile_sync_stays_with_node_across_launcher_modes() {
    let root = tempfile::tempdir().unwrap();
    for (mode, gate) in [("node", "0"), ("rust", "1")] {
        let sandbox = root.path().join(mode);
        fs::create_dir(&sandbox).unwrap();
        let child = tokio::process::Command::new(std::env::current_exe().unwrap())
            .args(["--ignored", "--exact", "profile_sync_in_child"])
            .env("PP_PROFILE_SYNC_PROOF", &sandbox)
            .env("PP_DESKTOP_REBUILD", gate)
            .kill_on_drop(true)
            .output();
        let output = timeout(Duration::from_secs(180), child)
            .await
            .unwrap()
            .unwrap();
        let stdout = String::from_utf8_lossy(&output.stdout);
        assert!(
            output.status.success() && stdout.contains("1 passed"),
            "{mode}: {stdout}\n{}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
}

#[tokio::test]
#[ignore = "run by the launcher profile-sync proof in each runtime mode"]
async fn profile_sync_in_child() {
    let fixture = Fixture::create(PathBuf::from(
        std::env::var_os("PP_PROFILE_SYNC_PROOF").unwrap(),
    ));
    if fixture.root.ends_with("node") {
        node_owns_stream_and_worker(&fixture).await;
    } else {
        rust_refuses_legacy_stream(&fixture).await;
    }
}

async fn node_owns_stream_and_worker(fixture: &Fixture) {
    let first = image("2.3.2.60", "0.24");
    fixture.save(&first);
    let (runtime, api) = start(&fixture.data, false).await;
    let original = api.profile_for_version("2.3.2.60").await;
    let mut socket = api.subscribe().await;

    let changed = image("2.3.2.61", "0.28");
    fixture.save(&changed);
    assert_event(&mut socket, "2.3.2.61").await;
    let updated = api.profile_for_version("2.3.2.61").await;
    assert_eq!(updated["id"], original["id"]);
    assert_eq!(updated["importedAt"], original["importedAt"]);
    drop(socket);
    stop(runtime).await;
    fixture.assert_row(&updated, "0.28");

    fixture.remove_input();
    let (runtime, api) = start(&fixture.data, false).await;
    assert_eq!(api.profile_for_version("2.3.2.61").await, updated);
    let mut socket = api.subscribe().await;
    let after_restart = image("2.3.2.62", "0.32");
    fixture.save(&after_restart);
    assert_event(&mut socket, "2.3.2.62").await;
    let refreshed = api.profile_for_version("2.3.2.62").await;
    assert_eq!(refreshed["id"], original["id"]);
    assert_eq!(refreshed["importedAt"], original["importedAt"]);
    drop(socket);
    stop(runtime).await;
    fixture.assert_row(&refreshed, "0.32");
    fixture.assert_source_and_ownership(&after_restart);
}

async fn rust_refuses_legacy_stream(fixture: &Fixture) {
    let input = image("2.3.2.60", "0.24");
    fixture.save(&input);
    for auth in ["/auth/register", "/auth/login"] {
        let (runtime, mut api) = start(&fixture.data, true).await;
        api.sign_in(auth).await;
        let (status, _) = api.upgrade().await;
        assert_eq!(status, StatusCode::NOT_FOUND);
        assert_eq!(api.get(LIBRARY).await.status(), StatusCode::NOT_FOUND);
        stop(runtime).await;
        fixture.assert_no_profile();
        fixture.assert_source_and_ownership(&input);
    }
}

struct Fixture {
    root: PathBuf,
    data: PathBuf,
    profile: PathBuf,
}

impl Fixture {
    fn create(root: PathBuf) -> Self {
        let data = pp_storage::lease::prepare_directory(&root.join("data")).unwrap();
        let orca = root.join("synthetic-orca");
        let config = orca.join(".config/OrcaSlicer/user/default");
        for kind in ["machine", "process", "filament"] {
            fs::create_dir_all(config.join(kind)).unwrap();
        }
        let settings = json!({
            "version": 1,
            "settings": {
                "slicer_orca_dir": orca,
                "slicer_prusa_dir": root.join("no-prusa"),
                "slicer_bambu_dir": root.join("no-bambu"),
            },
        });
        fs::write(data.join("desktop-settings.json"), settings.to_string()).unwrap();
        Self {
            root,
            data,
            profile: config.join("process/synthetic.json"),
        }
    }

    fn save(&self, input: &Value) {
        fs::write(&self.profile, input.to_string()).unwrap();
    }

    fn remove_input(&self) {
        fs::remove_file(&self.profile).unwrap();
    }

    fn assert_source_and_ownership(&self, expected: &Value) {
        let actual: Value = serde_json::from_slice(&fs::read(&self.profile).unwrap()).unwrap();
        assert_eq!(actual, *expected);
        assert!(
            !self
                .data
                .join(pp_gateway::ownership::cutover::CUTOVER_FILE)
                .exists()
        );
    }

    fn database(&self) -> rusqlite::Connection {
        rusqlite::Connection::open_with_flags(
            self.data.join("print-partner.db"),
            rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
        )
        .unwrap()
    }

    fn assert_row(&self, profile: &Value, layer_height: &str) {
        let db = self.database();
        let mut query = db.prepare(
            "SELECT id, source_path, resolved_flat_config FROM process_profiles WHERE name = ?1",
        ).unwrap();
        let mut rows = query.query([PROFILE]).unwrap();
        let row = rows.next().unwrap().unwrap();
        assert_eq!(
            row.get::<_, i64>(0).unwrap(),
            profile["id"].as_i64().unwrap()
        );
        assert_eq!(Path::new(&row.get::<_, String>(1).unwrap()), self.profile);
        let flat: Value = serde_json::from_str(&row.get::<_, String>(2).unwrap()).unwrap();
        assert_eq!(flat["layer_height"], layer_height);
        assert!(
            rows.next().unwrap().is_none(),
            "profile sync wrote a duplicate"
        );
    }

    fn assert_no_profile(&self) {
        let count: i64 = self
            .database()
            .query_row(
                "SELECT count(*) FROM process_profiles WHERE name = ?1",
                [PROFILE],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(count, 0);
    }
}

fn image(version: &str, layer_height: &str) -> Value {
    json!({
        "from": "User",
        "name": PROFILE,
        "print_settings_id": PROFILE,
        "version": version,
        "layer_height": layer_height,
    })
}

async fn start(data: &Path, rust_only: bool) -> (CoreRuntime, Api) {
    let mut input = release::launch(data.to_owned());
    if rust_only {
        input.bundle.node = data.join("not-a-node-executable");
    }
    let mut runtime = CoreRuntime::start_for_launcher(input).await.unwrap();
    assert_eq!(runtime.rust_only(), rust_only);
    assert_eq!(runtime.handle().compat_status().is_none(), rust_only);
    let (client, cookie) = desktop_http::connect(&mut runtime, WAIT).await;
    let origin = runtime.origin().to_owned();
    let api = Api {
        client,
        origin,
        cookie,
    };
    (runtime, api)
}

async fn stop(runtime: CoreRuntime) {
    let result = timeout(WAIT, runtime.shutdown()).await.unwrap();
    assert!(result.complete(), "{result:?}");
}

struct Api {
    client: Client,
    origin: String,
    cookie: String,
}

impl Api {
    async fn sign_in(&mut self, path: &str) {
        let response = self.client.post(format!("{}{path}", self.origin))
            .header(header::ORIGIN, &self.origin)
            .header(header::COOKIE, &self.cookie)
            .json(&json!({"email": "profile-proof@example.test", "password": "profile-proof-password"}))
            .send().await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let account =
            pp_testkit::session_cookie(response.headers()[header::SET_COOKIE].to_str().unwrap());
        self.cookie.push_str("; ");
        self.cookie.push_str(&account);
    }

    async fn get(&self, path: &str) -> reqwest::Response {
        self.client
            .get(format!("{}{path}", self.origin))
            .header(header::ORIGIN, &self.origin)
            .header(header::COOKIE, &self.cookie)
            .send()
            .await
            .unwrap()
    }

    async fn profile_for_version(&self, version: &str) -> Value {
        timeout(WAIT, async {
            loop {
                let response = self.get(LIBRARY).await;
                assert_eq!(response.status(), StatusCode::OK);
                let library: Value = response.json().await.unwrap();
                let mut matching = library["profiles"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .filter(|row| row["name"] == PROFILE);
                if let Some(row) = matching.next() {
                    assert!(matching.next().is_none());
                    assert_eq!(row["kind"], "process");
                    assert_eq!(row["slicerFormat"], "orca");
                    assert!(row["lastSyncedAt"].is_string());
                    if row["syncedFromSlicerVersion"] == version {
                        return row.clone();
                    }
                }
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
        })
        .await
        .unwrap_or_else(|_| panic!("profile library never showed version {version}"))
    }

    async fn subscribe(&self) -> TcpStream {
        let (status, socket) = self.upgrade().await;
        assert_eq!(status, StatusCode::SWITCHING_PROTOCOLS);
        socket
    }

    async fn upgrade(&self) -> (StatusCode, TcpStream) {
        let host = self.origin.strip_prefix("http://").unwrap();
        timeout(WAIT, async {
            let mut socket = TcpStream::connect(host).await.unwrap();
            let request = format!(
                "GET {STREAM} HTTP/1.1\r\nHost: {host}\r\nOrigin: {}\r\nCookie: {}\r\nConnection: Upgrade\r\nUpgrade: websocket\r\nSec-WebSocket-Version: 13\r\nSec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\n\r\n",
                self.origin, self.cookie
            );
            socket.write_all(request.as_bytes()).await.unwrap();
            let mut headers = Vec::new();
            while !headers.ends_with(b"\r\n\r\n") {
                assert!(headers.len() < 8192);
                headers.push(socket.read_u8().await.unwrap());
            }
            let headers = std::str::from_utf8(&headers).unwrap();
            let code = headers.split_ascii_whitespace().nth(1).unwrap();
            (StatusCode::from_bytes(code.as_bytes()).unwrap(), socket)
        }).await.expect("profile-sync upgrade timed out")
    }
}

async fn assert_event(socket: &mut TcpStream, version: &str) {
    let event: Value = timeout(WAIT, async {
        assert_eq!(
            socket.read_u8().await.unwrap(),
            0x81,
            "expected a complete text frame"
        );
        let size = socket.read_u8().await.unwrap();
        assert!(size <= 125, "expected a short unmasked profile event");
        let mut payload = vec![0; usize::from(size)];
        socket.read_exact(&mut payload).await.unwrap();
        serde_json::from_slice(&payload).unwrap()
    })
    .await
    .expect("profile-sync event timed out");
    assert_eq!(
        event,
        json!({
            "kind": "process",
            "slicer": "orca",
            "name": PROFILE,
            "version": version,
        })
    );
}
