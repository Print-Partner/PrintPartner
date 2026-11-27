#![cfg(feature = "desktop_rebuild")]

#[path = "../../pp-api/tests/support/accepted_plate.rs"]
mod accepted_plate;
#[path = "support/release.rs"]
mod release;

use pp_core::CoreRuntime;
use reqwest::{
    Client, Method, Response, StatusCode,
    header::{self, HeaderValue},
};
use serde_json::{Value, json};
use std::{
    fs,
    io::{Cursor, Read},
    path::Path,
    time::Duration,
};
use zip::ZipArchive;

const DIRECT: &str = "/api/v2/plans/1/exports/direct-3mf";
const SOURCE_FILE: &str = "repos/1/revisions/accepted/included.stl";
const INCLUDED: &str = "ppu_574c8d5f92624f029e21705a8ebb8a1b";
const EXCLUDED: &str = "ppu_827108f7baf3a2f3461d20c24e85822e";
const LEGACY: [&str; 2] = ["/jobs/export-direct-3mf", "/api/v1/jobs/export-direct-3mf"];
const WAIT: Duration = Duration::from_secs(15);

#[tokio::test]
async fn ticket_t_28_recovery_direct_export_uses_one_launcher_owner() {
    let data = accepted_plate::data_dir();
    let mut child = tokio::process::Command::new(std::env::current_exe().unwrap());
    child
        .args(["--ignored", "--exact", "direct_export_in_child"])
        .env("PP_DIRECT_EXPORT_DATA", data.path())
        .env("PP_DESKTOP_REBUILD", "1")
        .kill_on_drop(true);
    let result = tokio::time::timeout(Duration::from_secs(120), child.output())
        .await
        .expect("direct export proof timed out")
        .unwrap();
    let stdout = String::from_utf8_lossy(&result.stdout);
    assert!(
        result.status.success(),
        "{stdout}\n{}",
        String::from_utf8_lossy(&result.stderr)
    );
    assert!(
        stdout.contains("1 passed"),
        "the child did not run: {stdout}"
    );
}

#[tokio::test]
#[ignore = "runs as a subprocess with the Rust launcher gate enabled"]
async fn direct_export_in_child() {
    let data = std::path::PathBuf::from(std::env::var_os("PP_DIRECT_EXPORT_DATA").unwrap());
    accepted_plate::initialize_fixture(&data);
    let source = data.join(SOURCE_FILE);
    let original = fs::read(&source).unwrap();

    let (runtime, api) = Api::start(&data, Runtime::Desktop).await;
    api.refuse_without_account().await;
    api.retire_legacy_jobs().await;
    assert_no_export_effects(&data, &api).await;
    api.refuse_invalid_selection().await;
    let archive = api.download().await;
    verify_one_required_unit(&archive);
    assert_no_export_effects(&data, &api).await;
    stop(runtime).await;

    let (runtime, api) = Api::start(&data, Runtime::Desktop).await;
    assert_no_export_effects(&data, &api).await;
    assert_eq!(api.download().await, archive);
    api.retire_legacy_jobs().await;
    fs::write(
        &source,
        b"mutable source no longer matches the accepted digest",
    )
    .unwrap();
    api.refuse_changed_source().await;
    assert_no_export_effects(&data, &api).await;
    stop(runtime).await;

    fs::write(&source, original).unwrap();
    let (runtime, api) = Api::start(&data, Runtime::Server).await;
    api.refuse_without_account().await;
    api.retire_legacy_jobs().await;
    assert_eq!(api.download().await, archive);
    assert_no_export_effects(&data, &api).await;
    stop(runtime).await;
}

fn verify_one_required_unit(bytes: &[u8]) {
    let mut archive = ZipArchive::new(Cursor::new(bytes)).unwrap();
    let mut names: Vec<_> = archive.file_names().map(Result::unwrap).collect();
    names.sort_unstable();
    assert_eq!(
        names,
        ["3D/3dmodel.model", "[Content_Types].xml", "_rels/.rels"]
    );
    let mut model = String::new();
    archive
        .by_name("3D/3dmodel.model")
        .unwrap()
        .read_to_string(&mut model)
        .unwrap();
    assert_eq!(model.matches("<object ").count(), 1);
    assert_eq!(model.matches("<item ").count(), 1);
    assert!(model.contains(&format!("partnumber=\"{INCLUDED}\"")));
    assert!(!model.contains(EXCLUDED));
    assert_eq!(model.matches("<vertex ").count(), 8);
    assert_eq!(model.matches("<triangle ").count(), 12);
}

async fn assert_no_export_effects(data: &Path, api: &Api) {
    let jobs = api.request(Method::GET, "/api/v1/jobs", true, None).await;
    assert_eq!(
        json_response(jobs, StatusCode::OK).await,
        json!({"jobs": []})
    );
    let storage = rusqlite::Connection::open_with_flags(
        data.join("print-partner.db"),
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
    )
    .unwrap();
    let durable_jobs: i64 = storage
        .query_row("SELECT COUNT(*) FROM durable_jobs", [], |row| row.get(0))
        .unwrap();
    assert_eq!(durable_jobs, 0);
    for name in ["exports", "slicer-handoff"] {
        match fs::read_dir(data.join(name)) {
            Ok(mut entries) => assert!(entries.next().is_none(), "unexpected file in {name}"),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => panic!("could not inspect {name}: {error}"),
        }
    }
}

async fn stop(runtime: CoreRuntime) {
    let outcome = tokio::time::timeout(WAIT, runtime.shutdown())
        .await
        .expect("direct export runtime did not stop");
    assert!(outcome.complete(), "{outcome:?}");
}

enum Runtime {
    Desktop,
    Server,
}

struct Api {
    origin: String,
    client: Client,
    desktop_cookie: String,
}

impl Api {
    async fn start(data: &Path, mode: Runtime) -> (CoreRuntime, Self) {
        let mut input = release::launch(data.to_owned());
        input.bundle.node = data.join("not-a-node-executable");
        let mut runtime = match mode {
            Runtime::Desktop => CoreRuntime::start_for_launcher(input).await,
            Runtime::Server => CoreRuntime::start_for_server(input, None).await,
        }
        .expect("start on disposable Accepted Plan data without Node");
        assert!(runtime.rust_only());
        assert!(runtime.handle().compat_status().is_none());
        let client = Client::builder()
            .no_proxy()
            .redirect(reqwest::redirect::Policy::none())
            .timeout(WAIT)
            .build()
            .unwrap();
        let bootstrap = client
            .get(runtime.take_launch_target().unwrap().into_url())
            .send()
            .await
            .unwrap();
        assert_eq!(bootstrap.status(), StatusCode::SEE_OTHER);
        let desktop_cookie =
            pp_testkit::session_cookie(bootstrap.headers()[header::SET_COOKIE].to_str().unwrap());
        let api = Self {
            origin: runtime.origin().to_owned(),
            client,
            desktop_cookie,
        };
        (runtime, api)
    }

    async fn request(
        &self,
        method: Method,
        path: &str,
        account: bool,
        body: Option<Value>,
    ) -> Response {
        let cookie = if account {
            format!("{}; pp_session=read-fixture-secret", self.desktop_cookie)
        } else {
            self.desktop_cookie.clone()
        };
        let mut request = self
            .client
            .request(method, format!("{}{path}", self.origin))
            .header(header::ORIGIN, &self.origin)
            .header(header::COOKIE, HeaderValue::from_str(&cookie).unwrap());
        if let Some(body) = body {
            request = request.json(&body);
        }
        request.send().await.unwrap()
    }

    async fn export(&self, tokens: &str, account: bool) -> Response {
        self.request(
            Method::GET,
            &format!("{DIRECT}?tokens={tokens}"),
            account,
            None,
        )
        .await
    }

    async fn download(&self) -> Vec<u8> {
        let response = self.export(INCLUDED, true).await;
        for (name, value) in [
            (header::CONTENT_TYPE, "model/3mf"),
            (header::CACHE_CONTROL, "no-store"),
            (
                header::CONTENT_DISPOSITION,
                "attachment; filename=\"tracked-operational-build-v1-direct.3mf\"",
            ),
        ] {
            assert_eq!(response.headers()[&name], value, "{name}");
        }
        assert_eq!(response.status(), StatusCode::OK);
        response.bytes().await.unwrap().to_vec()
    }

    async fn retire_legacy_jobs(&self) {
        for path in LEGACY {
            let job = json!({"profile_id": 1, "tokens": [INCLUDED]});
            let response = self.request(Method::POST, path, true, Some(job)).await;
            assert_eq!(response.headers()[header::CACHE_CONTROL], "no-store");
            let problem = json_response(response, StatusCode::GONE).await;
            assert_eq!(problem["code"], "direct_3mf_job_retired", "{path}");
        }
    }

    async fn refuse_without_account(&self) {
        let response = self.export(INCLUDED, false).await;
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    }

    async fn refuse_invalid_selection(&self) {
        let duplicate = self.export(&format!("{INCLUDED},{INCLUDED}"), true).await;
        let problem = json_response(duplicate, StatusCode::BAD_REQUEST).await;
        assert_eq!(problem["code"], "invalid_direct_selection");
        let excluded = self.export(&format!("{INCLUDED},{EXCLUDED}"), true).await;
        let problem = json_response(excluded, StatusCode::CONFLICT).await;
        assert_eq!(problem["code"], "invalid_direct_selection");
        assert_eq!(problem["token"], EXCLUDED);
    }

    async fn refuse_changed_source(&self) {
        let response = self.export(INCLUDED, true).await;
        assert!(!response.headers().contains_key(header::CONTENT_DISPOSITION));
        let problem = json_response(response, StatusCode::CONFLICT).await;
        assert_eq!(problem["code"], "source_files_unavailable");
        assert_eq!(problem["reason"], "digest_mismatch");
        assert_eq!(problem["token"], INCLUDED);
    }
}

async fn json_response(response: Response, expected: StatusCode) -> Value {
    let status = response.status();
    let path = response.url().path().to_owned();
    assert_eq!(status, expected, "{path}");
    response.json().await.unwrap()
}
