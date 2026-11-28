#![cfg(feature = "desktop_rebuild")]

#[path = "../../pp-api/tests/support/accepted_plate.rs"]
mod accepted_plate;
#[path = "on_demand_exports_transition/editable_kit_recipe.rs"]
mod editable_kit_recipe;
#[path = "support/release.rs"]
mod release;

use pp_core::CoreRuntime;
use reqwest::{Client, Method, Response, StatusCode, header};
use serde_json::{Value, json};
use std::{fs, io::Cursor, path::Path, time::Duration};
use zip::ZipArchive;

const CHECKLIST: &str = "/api/v2/plans/1/exports/checklist";
const PACK: &str = "/api/v2/plans/1/exports/stl-pack";
const REMAINING: &str = "/api/v2/plans/1/exports/stl-pack?scope=remaining";
const UNITS: &str = "/api/v2/plans/1/checkoff/units";
const BURSTS: &str = "/api/v2/plans/1/checkoff/bursts";
const SOURCE: &str = "repos/1/revisions/accepted/included.stl";
const LEGACY_JOBS: [&str; 4] = [
    "/jobs/export-checklist-html",
    "/api/v1/jobs/export-checklist-html",
    "/jobs/export-stl-pack",
    "/api/v1/jobs/export-stl-pack",
];
const DEADLINE: Duration = Duration::from_secs(15);

#[tokio::test]
async fn ticket_t_28_recovery_checklist_and_stl_pack_share_the_launcher_owner() {
    let data = accepted_plate::data_dir();
    let mut process = tokio::process::Command::new(std::env::current_exe().unwrap());
    let run = process
        .args(["--ignored", "--exact", "on_demand_exports_child"])
        .env("PP_DESKTOP_REBUILD", "1")
        .env("PP_ON_DEMAND_EXPORTS_DATA", data.path())
        .kill_on_drop(true)
        .output();
    let result = tokio::time::timeout(Duration::from_secs(120), run)
        .await
        .expect("on-demand export proof timed out")
        .unwrap();
    assert!(
        result.status.success() && String::from_utf8_lossy(&result.stdout).contains("1 passed"),
        "child status {}\n{}\n{}",
        result.status,
        String::from_utf8_lossy(&result.stdout),
        String::from_utf8_lossy(&result.stderr)
    );
}

#[tokio::test]
#[ignore = "runs inside the Rust-only launcher subprocess"]
async fn on_demand_exports_child() {
    let data = std::path::PathBuf::from(std::env::var_os("PP_ON_DEMAND_EXPORTS_DATA").unwrap());
    seed_unprinted_plan(&data);
    let source = fs::read(data.join(SOURCE)).unwrap();

    let (runtime, api) = Session::open(&data).await;
    api.refuse_desktop_only_credentials().await;
    api.refuse_legacy_jobs().await;
    assert_no_export_work(&data, &api).await;

    let unprinted = api.checkoff_units(false).await;
    api.checklist_shows(false).await;
    api.pack_has_only(REMAINING, &source).await;
    api.pack_has_only(PACK, &source).await;

    api.print_unit(&unprinted).await;
    api.check_printed_downloads(&source).await;
    assert_no_export_work(&data, &api).await;
    shutdown(runtime).await;

    let (runtime, api) = Session::open(&data).await;
    api.refuse_legacy_jobs().await;
    api.check_printed_downloads(&source).await;
    fs::write(data.join(SOURCE), b"fixture with the wrong digest").unwrap();
    api.refuse_changed_source().await;
    api.checklist_shows(true).await;
    assert_no_export_work(&data, &api).await;
    shutdown(runtime).await;
}

fn seed_unprinted_plan(data: &Path) {
    accepted_plate::initialize_fixture(data);
    let sql = rusqlite::Connection::open(data.join("print-partner.db")).unwrap();
    let changed = sql
        .execute(
            "UPDATE print_progress SET completed=0, assembled=0 WHERE part_id=2",
            [],
        )
        .unwrap();
    assert_eq!(changed, 1);
}

async fn assert_no_export_work(data: &Path, api: &Session) {
    let public_jobs = api.read_json("/api/v1/jobs").await;
    assert_eq!(public_jobs["jobs"], json!([]));
    let reader = rusqlite::Connection::open_with_flags(
        data.join("print-partner.db"),
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
    )
    .unwrap();
    let actual: i64 = reader
        .query_row("SELECT COUNT(*) FROM durable_jobs", [], |row| row.get(0))
        .unwrap();
    assert_eq!(actual, 0, "an export stored durable work");
    let exports = data.join("exports");
    match fs::read_dir(&exports) {
        Ok(mut files) => assert!(files.next().is_none(), "published an export file"),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => panic!("cannot inspect {}: {error}", exports.display()),
    }
}

async fn shutdown(runtime: CoreRuntime) {
    let drained = tokio::time::timeout(DEADLINE, runtime.shutdown())
        .await
        .expect("launcher did not stop");
    assert!(drained.complete(), "{drained:?}");
}

struct Session {
    client: Client,
    origin: String,
    desktop: String,
}

impl Session {
    async fn open(data: &Path) -> (CoreRuntime, Self) {
        let mut launch = release::launch(data.to_owned());
        launch.bundle.node = data.join("no-node-executable");
        let mut runtime = CoreRuntime::start_for_launcher(launch).await.unwrap();
        assert!(runtime.rust_only());
        assert!(runtime.handle().compat_status().is_none());
        let client = Client::builder()
            .no_proxy()
            .redirect(reqwest::redirect::Policy::none())
            .timeout(DEADLINE)
            .build()
            .unwrap();
        let origin = runtime.origin().to_owned();
        let target = runtime.take_launch_target().unwrap();
        let opened = client.get(target.into_url()).send().await.unwrap();
        assert_eq!(opened.status(), StatusCode::SEE_OTHER);
        let set_cookie = opened.headers()[header::SET_COOKIE].to_str().unwrap();
        let desktop = pp_testkit::session_cookie(set_cookie);
        (
            runtime,
            Self {
                client,
                origin,
                desktop,
            },
        )
    }

    fn request(&self, method: Method, path: &str, signed_in: bool) -> reqwest::RequestBuilder {
        let mut cookie = self.desktop.clone();
        if signed_in {
            cookie.push_str("; pp_session=read-fixture-secret");
        }
        self.client
            .request(method, format!("{}{path}", self.origin))
            .header(header::ORIGIN, &self.origin)
            .header(header::COOKIE, cookie)
    }

    async fn read(&self, path: &str) -> Response {
        self.request(Method::GET, path, true).send().await.unwrap()
    }

    async fn read_json(&self, path: &str) -> Value {
        checked_json(self.read(path).await, StatusCode::OK).await
    }

    async fn refuse_desktop_only_credentials(&self) {
        for path in [CHECKLIST, PACK] {
            let response = self.request(Method::GET, path, false).send().await.unwrap();
            assert_eq!(response.status(), StatusCode::UNAUTHORIZED, "{path}");
        }
    }

    async fn refuse_legacy_jobs(&self) {
        for path in LEGACY_JOBS {
            let response = self
                .request(Method::POST, path, true)
                .json(&json!({"profile_id": 1}))
                .send()
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::METHOD_NOT_ALLOWED, "{path}");
        }
    }

    async fn checkoff_units(&self, printed: bool) -> Value {
        let snapshot = self.read_json(UNITS).await;
        let parts = snapshot["parts"].as_array().unwrap();
        assert_eq!(parts.len(), 1);
        let part = &parts[0];
        assert_eq!(part["filename"], "included.stl");
        assert_eq!(part["quantity"], 1);
        assert_eq!(part["printed_count"], u8::from(printed));
        let units = part["units"].as_array().unwrap();
        assert_eq!(units.len(), 1);
        assert_eq!(units[0]["printed"], printed);
        snapshot
    }

    async fn print_unit(&self, snapshot: &Value) {
        let change = json!({
            "idempotency_key": "on-demand-export-proof",
            "expected": snapshot["basis"],
            "units": [{"token": snapshot["parts"][0]["units"][0]["token"], "printed": true}]
        });
        let receipt = self
            .request(Method::POST, BURSTS, true)
            .json(&change)
            .send()
            .await
            .unwrap();
        checked_json(receipt, StatusCode::OK).await;
    }

    async fn checklist_shows(&self, printed: bool) {
        let response = self.read(CHECKLIST).await;
        check_attachment(&response, "text/html; charset=utf-8", "checklist.html");
        assert_eq!(
            response.headers()[header::CONTENT_SECURITY_POLICY],
            "default-src 'none'; img-src data:; style-src 'unsafe-inline'"
        );
        let html = response.text().await.unwrap();
        let count = u8::from(printed);
        assert!(html.contains("<h1>Tracked operational Build</h1>"));
        assert!(html.contains(&format!("<dd>{count} of 1 units</dd>")));
        assert!(html.contains(&format!("<td class=\"n\">{count} / 1</td>")));
        assert!(html.contains("included.stl"));
        assert!(!html.contains("excluded.stl"));
    }

    async fn pack_has_only(&self, path: &str, source: &[u8]) {
        let response = self.read(path).await;
        check_attachment(&response, "application/zip", "stl.zip");
        let body = response.bytes().await.unwrap();
        let mut archive = ZipArchive::new(Cursor::new(body)).unwrap();
        assert_eq!(archive.len(), 1);
        let mut entry = archive.by_name("primary/_root/included_01.stl").unwrap();
        assert_eq!(entry.compression(), zip::CompressionMethod::Stored);
        let mut extracted = Vec::new();
        std::io::copy(&mut entry, &mut extracted).unwrap();
        assert_eq!(extracted, source);
    }

    async fn check_printed_downloads(&self, source: &[u8]) {
        self.checkoff_units(true).await;
        self.checklist_shows(true).await;
        let empty = checked_json(self.read(REMAINING).await, StatusCode::CONFLICT).await;
        assert_eq!(empty["code"], "nothing_to_pack");
        self.pack_has_only(PACK, source).await;
    }

    async fn refuse_changed_source(&self) {
        let response = self.read(PACK).await;
        assert!(!response.headers().contains_key(header::CONTENT_DISPOSITION));
        let problem = checked_json(response, StatusCode::CONFLICT).await;
        assert_eq!(problem["code"], "source_files_unavailable");
        assert_eq!(problem["files"][0]["relative_path"], "included.stl");
        assert_eq!(problem["files"][0]["reason"], "digest_mismatch");
    }
}

fn check_attachment(response: &Response, content_type: &str, suffix: &str) {
    let meta = response.headers();
    assert_eq!(
        response.status(),
        StatusCode::OK,
        "{}",
        response.url().path()
    );
    let filename = format!("attachment; filename=\"tracked-operational-build-v1-{suffix}\"");
    for (name, expected) in [
        (header::CONTENT_DISPOSITION, filename.as_str()),
        (header::CONTENT_TYPE, content_type),
        (header::CACHE_CONTROL, "no-store"),
    ] {
        assert_eq!(meta[&name], expected, "{name}");
    }
}

async fn checked_json(response: Response, expected: StatusCode) -> Value {
    let path = response.url().path().to_owned();
    let status = response.status();
    let text = response.text().await.unwrap();
    assert_eq!(status, expected, "{path}: {text}");
    serde_json::from_str(&text).unwrap()
}
