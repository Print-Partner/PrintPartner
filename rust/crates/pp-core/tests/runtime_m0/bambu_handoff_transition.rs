use super::watcher_proof::{production_rows, proof_data, run_in_child, shut_down, signed_in};
use super::{desktop_cookie, launch, started};
use pp_core::{CoreRuntime, rebuild_api::is_bambu_connect_staged_file};
use reqwest::{Client, Method, Response, StatusCode, Url, header};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    fs,
    path::{Path, PathBuf},
    time::Duration,
};

const STAGING: &str = "bambu-connect-handoff";
const LEGACY: &str = "/bambu-connect/handoff";
const LEGACY_ID: &str = "00000000-0000-4000-8000-000000000001";
const PRINT: &[u8] = b"; GRE-2018 synthetic sliced print\n";
const DISPLAY_NAME: &str = "Fixture A & B #1";
const FILE_NAME: &str = "Fixture A & B #1.GCODE";
const DIFFERENT: &[u8] = b"; conflicting bytes\n";

#[tokio::test]
async fn ticket_t_28_recovery_bambu_handoff_uses_only_the_local_rust_owner() {
    let data = tempfile::tempdir().unwrap();
    run_in_child("bambu_handoff_transition::handoff_in_child", data.path()).await;
}

#[tokio::test]
#[ignore = "run inside the Rust-only launcher subprocess"]
async fn handoff_in_child() {
    let data = proof_data().canonicalize().unwrap();
    let (runtime, http) = Handoff::open(&data, Start::FirstDesktop).await;
    let build = http.create_build().await;
    http.assert_session_and_feature_gates(build).await;
    assert!(staged_entries(&data).is_empty());
    http.enable_bambu(build).await;

    let production = production_rows(&data);
    let staged = http.stage(build, PRINT).await;
    let path = check_link(&data, build, &staged);
    http.refuse_legacy_aliases(build).await;
    assert_stage_only(&data, &http, &path).await;
    assert_eq!(production_rows(&data), production);
    shut_down(runtime).await;

    assert!(is_bambu_connect_staged_file(&data, &path));
    assert_eq!(fs::read(&path).unwrap(), PRINT);
    let (runtime, http) = Handoff::open(&data, Start::ReturningDesktop).await;
    let repeated = http.stage(build, PRINT).await;
    assert_eq!(
        repeated, staged,
        "same Build, file type and bytes reuse the link"
    );
    check_link(&data, build, &repeated);
    assert_stage_only(&data, &http, &path).await;

    fs::write(&path, DIFFERENT).unwrap();
    let conflict = json_response(
        http.upload(&handoff_path(build), true, PRINT, None).await,
        StatusCode::CONFLICT,
    )
    .await;
    assert_eq!(
        conflict["detail"],
        "The existing handoff file does not match this upload"
    );
    assert!(conflict["connect_url"].is_null());
    assert_eq!(fs::read(&path).unwrap(), DIFFERENT);
    http.refuse_legacy_aliases(build).await;
    assert_stage_only(&data, &http, &path).await;
    shut_down(runtime).await;

    let (server, http) = Handoff::open(&data, Start::Server).await;
    let sent = http
        .upload(&handoff_path(build), true, b"; fresh headless file\n", None)
        .await;
    assert_unmounted(&sent);
    http.refuse_legacy_aliases(build).await;
    assert_stage_only(&data, &http, &path).await;
    assert_eq!(fs::read(&path).unwrap(), DIFFERENT);
    assert_eq!(production_rows(&data), production);
    shut_down(server).await;
    assert_eq!(staged_entries(&data), [path]);
}

fn handoff_path(build: i64) -> String {
    format!("/api/v2/plans/{build}/bambu-connect-handoff")
}

fn check_link(data: &Path, build: i64, receipt: &Value) -> PathBuf {
    assert_eq!(receipt["filename"], format!("{DISPLAY_NAME}.gcode"));
    let link = Url::parse(receipt["connect_url"].as_str().unwrap()).unwrap();
    assert_eq!(link.scheme(), "bambu-connect");
    assert_eq!(link.host_str(), Some("import-file"));
    assert!(link.path().is_empty());
    assert!(link.fragment().is_none());
    let mut params = link.query_pairs();
    let Some((key, path)) = params.next() else {
        panic!("handoff link has no file");
    };
    assert_eq!(key, "path");
    assert_eq!(params.next(), Some(("name".into(), DISPLAY_NAME.into())));
    assert_eq!(params.next(), Some(("version".into(), "1.0.0".into())));
    assert!(params.next().is_none());
    let staged = PathBuf::from(path.into_owned());
    let digest = hex::encode(Sha256::digest(PRINT));
    assert_eq!(
        staged,
        data.join(STAGING).join(format!("{build}-{digest}.gcode"))
    );
    assert_eq!(fs::read(&staged).unwrap(), PRINT);
    assert!(is_bambu_connect_staged_file(data, &staged));
    assert!(!is_bambu_connect_staged_file(
        data,
        &staged.with_extension("3mf")
    ));
    let copy = data.join(staged.file_name().unwrap());
    fs::write(&copy, PRINT).unwrap();
    assert!(!is_bambu_connect_staged_file(data, &copy));
    fs::remove_file(copy).unwrap();
    staged
}

async fn assert_stage_only(data: &Path, http: &Handoff, file: &Path) {
    assert_eq!(staged_entries(data), [file.to_path_buf()]);
    let jobs = http
        .request(Method::GET, "/api/v1/jobs", true)
        .send()
        .await
        .unwrap();
    assert_eq!(json_response(jobs, StatusCode::OK).await["jobs"], json!([]));
    let db = rusqlite::Connection::open_with_flags(
        data.join("print-partner.db"),
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
    )
    .unwrap();
    let durable_jobs: i64 = db
        .query_row("SELECT count(*) FROM durable_jobs", [], |row| row.get(0))
        .unwrap();
    assert_eq!(durable_jobs, 0);
    assert!(
        files_in(data.join("exports")).is_empty(),
        "staging must not publish an export"
    );
}

fn staged_entries(data: &Path) -> Vec<PathBuf> {
    let path = data.join(STAGING);
    match fs::read_dir(&path) {
        Ok(entries) => entries.map(|entry| entry.unwrap().path()).collect(),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Vec::new(),
        Err(error) => panic!("cannot list {}: {error}", path.display()),
    }
}

fn files_in(directory: PathBuf) -> Vec<PathBuf> {
    if !directory.exists() {
        return Vec::new();
    }
    fs::read_dir(directory)
        .unwrap()
        .flat_map(|entry| {
            let entry = entry.unwrap();
            if entry.file_type().unwrap().is_dir() {
                files_in(entry.path())
            } else {
                vec![entry.path()]
            }
        })
        .collect()
}

#[derive(Clone, Copy)]
enum Start {
    FirstDesktop,
    ReturningDesktop,
    Server,
}

struct Handoff {
    client: Client,
    origin: String,
    desktop: String,
    session: String,
}

impl Handoff {
    async fn open(data: &Path, start: Start) -> (CoreRuntime, Self) {
        let mut input = launch(data.to_owned());
        input.bundle.node = data.join("not-installed-node");
        let startup = if matches!(start, Start::Server) {
            CoreRuntime::start_for_server(input, None).await
        } else {
            CoreRuntime::start_for_launcher(input).await
        };
        let mut runtime = started(startup, data);
        assert!(
            runtime.rust_only() && runtime.handle().compat_status().is_none(),
            "Bambu Connect staging must not start Node compatibility"
        );
        let origin = runtime.origin().to_owned();
        let desktop = desktop_cookie(&mut runtime).await;
        let session = signed_in(
            &origin,
            &desktop,
            if matches!(start, Start::FirstDesktop) {
                1
            } else {
                2
            },
        )
        .await;
        let client = Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .timeout(Duration::from_secs(15))
            .no_proxy()
            .build()
            .unwrap();
        (
            runtime,
            Self {
                client,
                origin,
                desktop,
                session,
            },
        )
    }

    fn request(&self, method: Method, path: &str, signed_in: bool) -> reqwest::RequestBuilder {
        let cookie = if signed_in {
            &self.session
        } else {
            &self.desktop
        };
        self.client
            .request(method, format!("{}{path}", self.origin))
            .header(header::ORIGIN, &self.origin)
            .header(header::ACCEPT, "application/json")
            .header(header::COOKIE, cookie)
    }

    async fn create_build(&self) -> i64 {
        let response = self
            .request(Method::POST, "/api/v2/plans", true)
            .json(&json!({"name": "Bambu handoff proof"}))
            .send()
            .await
            .unwrap();
        json_response(response, StatusCode::OK).await["id"]
            .as_i64()
            .unwrap()
    }

    async fn enable_bambu(&self, build: i64) {
        let changed = self
            .request(Method::PATCH, &format!("/plans/{build}/features"), true)
            .json(&json!({"bambu": true}))
            .send()
            .await
            .unwrap();
        assert_eq!(
            json_response(changed, StatusCode::OK).await["effective"]["bambu"]["enabled"],
            true
        );
    }

    async fn assert_session_and_feature_gates(&self, build: i64) {
        let path = handoff_path(build);
        let anonymous = self.upload(&path, false, PRINT, None).await;
        assert_eq!(anonymous.status(), StatusCode::UNAUTHORIZED);
        let disabled = self.upload(&path, true, PRINT, None).await;
        assert_eq!(
            json_response(disabled, StatusCode::FORBIDDEN).await["detail"],
            "Bambu Connect is disabled for this Build"
        );
        let missing = self
            .upload(&handoff_path(build + 1), true, PRINT, None)
            .await;
        assert_eq!(
            json_response(missing, StatusCode::NOT_FOUND).await["detail"],
            "Build not found"
        );
    }

    async fn stage(&self, build: i64, print: &[u8]) -> Value {
        let staged = self.upload(&handoff_path(build), true, print, None).await;
        assert_eq!(staged.headers()[header::CACHE_CONTROL], "no-store");
        json_response(staged, StatusCode::OK).await
    }

    async fn upload(
        &self,
        path: &str,
        signed_in: bool,
        print: &[u8],
        legacy_build: Option<i64>,
    ) -> Response {
        let boundary = "bambu-connect-transition";
        let mut body = if let Some(build) = legacy_build {
            format!("--{boundary}\r\nContent-Disposition: form-data; name=\"profile_id\"\r\n\r\n{build}\r\n--{boundary}\r\nContent-Disposition: form-data; name=\"launch\"\r\n\r\nfalse\r\n").into_bytes()
        } else {
            Vec::new()
        };
        body.extend_from_slice(format!(
            "--{boundary}\r\nContent-Disposition: form-data; name=\"file\"; filename=\"{FILE_NAME}\"\r\n\r\n"
        ).as_bytes());
        body.extend_from_slice(print);
        body.extend_from_slice(format!("\r\n--{boundary}--\r\n").as_bytes());
        self.request(Method::POST, path, signed_in)
            .header(
                header::CONTENT_TYPE,
                format!("multipart/form-data; boundary={boundary}"),
            )
            .body(body)
            .send()
            .await
            .unwrap()
    }

    async fn refuse_legacy_aliases(&self, build: i64) {
        for prefix in ["", "/api/v1"] {
            let legacy = format!("{prefix}{LEGACY}");
            let staging = self.upload(&legacy, true, PRINT, Some(build)).await;
            assert_unmounted(&staging);
            let download = self
                .request(
                    Method::GET,
                    &format!("{legacy}/{LEGACY_ID}/file?profile_id={build}"),
                    true,
                )
                .send()
                .await
                .unwrap();
            assert_unmounted(&download);
        }
    }
}

fn assert_unmounted(response: &Response) {
    let status = response.status();
    assert!(
        status == StatusCode::NOT_FOUND || status == StatusCode::METHOD_NOT_ALLOWED,
        "{}: {status}",
        response.url().path()
    );
    assert!(!response.headers().contains_key(header::CONTENT_DISPOSITION));
    assert!(!response.headers().contains_key(header::LOCATION));
}

async fn json_response(response: Response, expected: StatusCode) -> Value {
    let path = response.url().path().to_owned();
    let status = response.status();
    let bytes = response.bytes().await.unwrap();
    assert_eq!(
        status,
        expected,
        "{path}: {}",
        String::from_utf8_lossy(&bytes)
    );
    serde_json::from_slice(&bytes).unwrap()
}
