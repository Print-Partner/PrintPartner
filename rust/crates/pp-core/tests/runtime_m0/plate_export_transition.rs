use super::{
    desktop_cookie, launch, request,
    watcher_proof::{proof_data, run_in_child, shut_down},
};
use axum::http::{HeaderMap, StatusCode, header};
use pp_core::CoreRuntime;
use serde::Deserialize;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    fs,
    io::{Cursor, Read},
    path::{Path, PathBuf},
};
use zip::ZipArchive;

#[path = "../../../pp-api/tests/support/accepted_plate.rs"]
mod accepted_plate;

const PICKER: &str = "/api/v2/slicer-instances";
const HANDOFF: &str = "/api/v2/plans/1/exports/slicer-handoff";
const INSTANCE: &str = "fixture-inbox";
const LEGACY_POSTS: [&str; 4] = [
    "/jobs/export-accepted-plate-3mf",
    "/api/v1/jobs/export-accepted-plate-3mf",
    "/slicer-instances/fixture-inbox/open-accepted-plates",
    "/api/v1/slicer-instances/fixture-inbox/open-accepted-plates",
];

#[tokio::test]
async fn ticket_t_28_recovery_plate_export_uses_one_launcher_owner() {
    let data = accepted_plate::data_dir();
    run_in_child(
        "plate_export_transition::plate_export_in_child",
        data.path(),
    )
    .await;
}

#[tokio::test]
#[ignore = "runs in an isolated subprocess with explicit environment"]
async fn plate_export_in_child() {
    let data = proof_data();
    let input = accepted_plate::initialize_fixture(&data);
    let inbox = configure_inbox(&data);
    let (runtime, api) = RuntimeApi::start(&data, Mode::Desktop).await;
    let revision = api.initialize_plates(input).await;
    let jobs = api.jobs().await;
    api.refuses_legacy_posts(revision).await;
    let plate = api.managed_download(revision).await;
    api.check_picker().await;
    api.refuses_stale_revision(revision).await;
    assert!(inbox_entries(&inbox).is_empty());

    let filename = expected_filename(revision, &plate);
    let published = inbox.join(&filename);
    let pending = inbox.join(format!(".{filename}.pending"));
    fs::write(&pending, b"incomplete fixture").unwrap();
    let delivery = api.deliver(revision).await;
    delivery.check(&filename, false);
    assert_eq!(fs::read(&published).unwrap(), plate);
    assert!(!pending.exists());
    let delivered_entries = inbox_entries(&inbox);
    assert!(delivered_entries.contains(&filename));
    assert_eq!(api.jobs().await, jobs);
    shut_down(runtime).await;

    let (runtime, api) = RuntimeApi::start(&data, Mode::Desktop).await;
    assert_eq!(inbox_entries(&inbox), delivered_entries);
    assert_eq!(api.managed_download(revision).await, plate);
    api.deliver(revision).await.check(&filename, true);
    assert_eq!(fs::read(&published).unwrap(), plate);
    fs::write(&published, b"not the verified artifact").unwrap();
    api.refuses_conflicting_artifact(revision).await;
    assert_eq!(fs::read(&published).unwrap(), b"not the verified artifact");
    assert_eq!(inbox_entries(&inbox), delivered_entries);
    assert_eq!(api.jobs().await, jobs);
    shut_down(runtime).await;

    let (runtime, api) = RuntimeApi::start(&data, Mode::Server).await;
    assert_eq!(api.managed_download(revision).await, plate);
    api.refuses_legacy_posts(revision).await;
    api.has_no_slicer_routes(revision).await;
    assert_eq!(api.jobs().await, jobs);
    assert_eq!(inbox_entries(&inbox), delivered_entries);
    assert_eq!(fs::read(&published).unwrap(), b"not the verified artifact");
    shut_down(runtime).await;
}

fn configure_inbox(data: &Path) -> PathBuf {
    let root = data.join("slicer-handoff");
    let inbox = root.join(INSTANCE);
    fs::create_dir_all(&inbox).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&inbox, fs::Permissions::from_mode(0o700)).unwrap();
    }
    fs::write(
        root.join("instances.json"),
        json!({
            "default": INSTANCE,
            "instances": [{"id": INSTANCE, "name": "Disposable inbox"}]
        })
        .to_string(),
    )
    .unwrap();
    inbox
}

fn inbox_entries(inbox: &Path) -> Vec<String> {
    let mut entries: Vec<_> = fs::read_dir(inbox)
        .unwrap()
        .map(|entry| entry.unwrap().file_name().into_string().unwrap())
        .collect();
    entries.sort_unstable();
    entries
}

fn expected_filename(revision: u64, plate: &[u8]) -> String {
    let digest = hex::encode(Sha256::digest(plate));
    format!("revision-{revision}-plate-1-{digest}.3mf")
}

fn selection(revision: u64) -> Value {
    json!({"plate_revision_id": revision, "plate_ordinal": 1})
}

#[derive(Deserialize)]
struct Delivery {
    instance_id: String,
    filename: String,
    already_delivered: bool,
}

impl Delivery {
    fn check(self, filename: &str, already_delivered: bool) {
        assert_eq!(self.instance_id, INSTANCE);
        assert_eq!(self.filename, filename);
        assert_eq!(self.already_delivered, already_delivered);
    }
}

#[derive(Clone, Copy)]
enum Mode {
    Desktop,
    Server,
}

struct RuntimeApi {
    origin: String,
    cookie: String,
}

impl RuntimeApi {
    async fn start(data: &Path, mode: Mode) -> (CoreRuntime, Self) {
        let mut input = launch(data.to_owned());
        input.bundle.node = data.join("no-node-executable");
        let runtime = match mode {
            Mode::Desktop => CoreRuntime::start_for_launcher(input).await,
            Mode::Server => CoreRuntime::start_for_server(input, None).await,
        };
        let mut runtime = super::started(runtime, data);
        assert!(runtime.rust_only());
        assert!(runtime.handle().compat_status().is_none());
        let desktop = desktop_cookie(&mut runtime).await;
        let api = Self {
            origin: runtime.origin().to_owned(),
            cookie: format!("{desktop}; pp_session=read-fixture-secret"),
        };
        (runtime, api)
    }

    async fn send(&self, method: &str, path: &str, body: Option<Value>) -> Reply {
        let (status, headers, bytes) = request(
            &self.origin,
            path,
            method,
            Some(&self.cookie),
            None,
            None,
            body,
        )
        .await;
        Reply {
            status,
            headers,
            bytes,
        }
    }

    async fn initialize_plates(&self, input: Value) -> u64 {
        let body = self
            .send("POST", "/api/v2/plans/1/plates/initialize", Some(input))
            .await
            .ok_json();
        body["plate_revision_id"].as_u64().unwrap()
    }

    async fn jobs(&self) -> Value {
        let body = self.send("GET", "/api/v1/jobs", None).await.ok_json();
        assert!(body["jobs"].is_array());
        body
    }

    async fn managed_download(&self, revision: u64) -> Vec<u8> {
        let path = format!("/api/v2/plans/1/exports/managed-3mf?plate_revision_id={revision}");
        let response = self.send("GET", &path, None).await;
        response.require(StatusCode::OK);
        assert_eq!(response.headers[header::CONTENT_TYPE], "application/zip");
        assert_eq!(response.headers[header::CACHE_CONTROL], "no-store");
        let mut archive = ZipArchive::new(Cursor::new(response.bytes)).unwrap();
        assert_eq!(archive.len(), 2);
        let mut plate = Vec::new();
        archive
            .by_name("plates/0001.3mf")
            .unwrap()
            .read_to_end(&mut plate)
            .unwrap();
        assert!(!ZipArchive::new(Cursor::new(&plate)).unwrap().is_empty());
        plate
    }

    async fn check_picker(&self) {
        let picker = self.send("GET", PICKER, None).await.ok_json();
        assert_eq!(
            picker,
            json!({
                "default_instance_id": INSTANCE,
                "instances": [{"id": INSTANCE, "name": "Disposable inbox", "available": true}]
            })
        );
    }

    async fn deliver(&self, revision: u64) -> Delivery {
        let response = self.send("POST", HANDOFF, Some(selection(revision))).await;
        response.require(StatusCode::OK);
        assert_eq!(response.headers[header::CACHE_CONTROL], "no-store");
        serde_json::from_slice(&response.bytes).unwrap()
    }

    async fn refuses_stale_revision(&self, revision: u64) {
        self.send("POST", HANDOFF, Some(selection(revision - 1)))
            .await
            .require(StatusCode::CONFLICT);
    }

    async fn refuses_conflicting_artifact(&self, revision: u64) {
        let reply = self.send("POST", HANDOFF, Some(selection(revision))).await;
        reply.require(StatusCode::CONFLICT);
        let body: Value = serde_json::from_slice(&reply.bytes).unwrap();
        assert_eq!(body["code"], "slicer_artifact_conflict");
    }

    async fn refuses_legacy_posts(&self, revision: u64) {
        for path in LEGACY_POSTS {
            let reply = self.send("POST", path, Some(selection(revision))).await;
            if path.contains("/jobs/") {
                reply.require(StatusCode::METHOD_NOT_ALLOWED);
            } else {
                reply.require(StatusCode::NOT_FOUND);
            }
        }
    }

    async fn has_no_slicer_routes(&self, revision: u64) {
        self.send("GET", PICKER, None)
            .await
            .require(StatusCode::NOT_FOUND);
        self.send("POST", HANDOFF, Some(selection(revision)))
            .await
            .require(StatusCode::NOT_FOUND);
    }
}

struct Reply {
    status: StatusCode,
    headers: HeaderMap,
    bytes: Vec<u8>,
}

impl Reply {
    fn require(&self, expected: StatusCode) {
        assert_eq!(
            self.status,
            expected,
            "{}",
            String::from_utf8_lossy(&self.bytes)
        );
    }

    fn ok_json(self) -> Value {
        self.require(StatusCode::OK);
        serde_json::from_slice(&self.bytes).unwrap()
    }
}
