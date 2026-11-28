use super::watcher_proof::{shut_down, signed_in};
use super::{desktop_cookie, launch, request, started};
use axum::http::StatusCode;
use pp_core::{CoreRuntime, CoreStatus};
use serde_json::{Value, json};
use std::{
    collections::BTreeSet,
    fs,
    io::{self, Cursor},
    path::{Path, PathBuf},
    time::Duration,
};

const ALIASES: [&str; 2] = ["/jobs/export-kit-bundle", "/api/v1/jobs/export-kit-bundle"];
const NAME: &str = "Disposable Kit Bundle";
const WAIT: Duration = Duration::from_secs(15);

#[tokio::test]
async fn kit_bundle_aliases_and_worker_remain_with_node() {
    let data = tempfile::tempdir().unwrap();
    for (case, gate) in [("node", "0"), ("rust", "1")] {
        let directory = data.path().join(case);
        fs::create_dir(&directory).unwrap();
        let output = tokio::time::timeout(
            Duration::from_secs(180),
            tokio::process::Command::new(std::env::current_exe().unwrap())
                .args([
                    "--ignored",
                    "--exact",
                    "kit_bundle_transition::kit_bundle_boundary_in_child",
                    "--nocapture",
                ])
                .env("PP_KIT_PROOF_DATA", &directory)
                .env("PP_DESKTOP_REBUILD", gate)
                .kill_on_drop(true)
                .output(),
        )
        .await
        .unwrap_or_else(|_| panic!("{case} kit-bundle proof timed out"))
        .unwrap();
        let stdout = String::from_utf8_lossy(&output.stdout);
        assert!(
            output.status.success(),
            "{case}: {stdout}\n{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(stdout.contains("1 passed"), "{case}: child ran no tests");
    }
}

#[tokio::test]
#[ignore = "run by the compatibility proof for both launcher modes"]
async fn kit_bundle_boundary_in_child() {
    let data = PathBuf::from(std::env::var_os("PP_KIT_PROOF_DATA").unwrap());
    if std::env::var("PP_DESKTOP_REBUILD").as_deref() == Ok("1") {
        prove_rust_boundary(&data).await;
    } else {
        prove_node_publication(&data).await;
    }
    assert!(
        !data
            .join(pp_gateway::ownership::cutover::CUTOVER_FILE)
            .exists()
    );
}

async fn prove_node_publication(data: &Path) {
    let (runtime, http) = Session::open(data, false, 1).await;
    let profile = http.create_plan("/plans").await;
    assert!(files_under(&data.join("exports")).is_empty());
    let mut published = BTreeSet::new();
    let mut receipts = Vec::new();

    for (route, with_progress) in ALIASES.into_iter().zip([false, true]) {
        let body = submission(profile, with_progress);
        let response = http.json("POST", route, Some(body)).await;
        let job_id = response["job_id"].as_str().unwrap();
        let job = http.terminal_job(job_id).await;
        assert_eq!(job["job_id"], job_id);
        assert_eq!(job["kind"], "export-kit-bundle");
        assert_eq!(job["status"], "done", "{job}");
        assert_eq!(job["result"]["profile_id"], profile);

        let result = &job["result"];
        let path = PathBuf::from(result["path"].as_str().unwrap());
        let canonical = path.canonicalize().unwrap();
        assert!(canonical.starts_with(data.join("exports").canonicalize().unwrap()));
        published.insert(path);
        let download = result["download_url"].as_str().unwrap();
        assert!(download.starts_with("/exports/"), "{download}");
        assert!(download.ends_with(".print-partner-kit.zip"), "{download}");
        let bytes = http.bundle(download).await;
        assert_eq!(fs::read(&canonical).unwrap(), bytes);

        let kit = read_kit_archive(&bytes);
        assert_eq!(kit["format"], "print-partner-kit");
        assert_eq!(kit["version"], 3);
        assert_eq!(kit["profile"]["name"], NAME);
        assert_eq!(kit["parts"], json!([]));
        assert!(kit["sources"].is_array(), "{kit}");
        assert_eq!(files_under(&data.join("exports")), published);
        receipts.push((job_id.to_owned(), download.to_owned(), bytes));
    }
    assert_ne!(receipts[0].0, receipts[1].0);
    shut_down(runtime).await;

    let (runtime, http) = Session::open(data, false, 2).await;
    for (id, download, expected) in receipts {
        assert_eq!(
            http.call("GET", &format!("/jobs/{id}"), None).await.0,
            StatusCode::NOT_FOUND,
            "the old Node process's in-memory job must not be replayed"
        );
        assert_eq!(http.bundle(&download).await, expected);
    }
    shut_down(runtime).await;
    assert_eq!(files_under(&data.join("exports")), published);
}

async fn prove_rust_boundary(data: &Path) {
    let (runtime, http) = Session::open(data, true, 1).await;
    let profile = http.create_plan("/api/v2/plans").await;
    http.assert_no_export_work(data).await;
    http.refuse_legacy_aliases(profile).await;
    http.assert_no_export_work(data).await;
    shut_down(runtime).await;

    let (runtime, http) = Session::open(data, true, 2).await;
    let plans = http.json("GET", "/api/v2/plans", None).await;
    assert!(
        plans["profiles"]
            .as_array()
            .unwrap()
            .iter()
            .any(|plan| plan["id"] == profile && plan["name"] == NAME),
        "the refusal must not be explained by a missing Plan: {plans}"
    );
    http.assert_no_export_work(data).await;
    http.refuse_legacy_aliases(profile).await;
    http.assert_no_export_work(data).await;
    shut_down(runtime).await;
}

fn submission(profile: i64, include_print_progress: bool) -> Value {
    json!({"profile_id": profile, "include_print_progress": include_print_progress})
}

fn files_under(root: &Path) -> BTreeSet<PathBuf> {
    let entries = match fs::read_dir(root) {
        Ok(entries) => entries,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return BTreeSet::new(),
        Err(error) => panic!("{}: {error}", root.display()),
    };
    let mut files = BTreeSet::new();
    for entry in entries {
        let entry = entry.unwrap();
        if entry.file_type().unwrap().is_dir() {
            files.extend(files_under(&entry.path()));
        } else {
            files.insert(entry.path());
        }
    }
    files
}

fn read_kit_archive(bytes: &[u8]) -> Value {
    let mut archive = zip::ZipArchive::new(Cursor::new(bytes)).unwrap();
    assert_eq!(archive.len(), 1);
    serde_json::from_reader(archive.by_name("kit.json").unwrap()).unwrap()
}

struct Session {
    origin: String,
    cookie: String,
}

impl Session {
    async fn open(data: &Path, rust: bool, boot: u32) -> (CoreRuntime, Self) {
        let mut input = launch(data.to_owned());
        if rust {
            input.bundle.node = data.join("no-node-executable");
        }
        let mut runtime = started(CoreRuntime::start_for_launcher(input).await, data);
        assert_eq!(runtime.rust_only(), rust);
        let origin = runtime.origin().to_owned();
        let desktop = desktop_cookie(&mut runtime).await;
        let cookie = if rust {
            assert!(runtime.handle().compat_status().is_none());
            signed_in(&origin, &desktop, boot).await
        } else {
            let compat = runtime.handle().compat_status().unwrap();
            assert!(matches!(*compat.borrow(), CoreStatus::Ready { .. }));
            desktop
        };
        let http = Self { origin, cookie };
        if rust {
            let state = http.json("GET", "/__runtime", None).await;
            assert_eq!(state["compat"], json!({"state": "not_used"}));
        }
        (runtime, http)
    }

    async fn call(
        &self,
        method: &str,
        path: &str,
        body: Option<Value>,
    ) -> (StatusCode, axum::http::HeaderMap, Vec<u8>) {
        tokio::time::timeout(
            WAIT,
            request(
                &self.origin,
                path,
                method,
                Some(&self.cookie),
                None,
                None,
                body,
            ),
        )
        .await
        .unwrap_or_else(|_| panic!("{method} {path} timed out"))
    }

    async fn json(&self, method: &str, path: &str, body: Option<Value>) -> Value {
        let (status, _, bytes) = self.call(method, path, body).await;
        assert_eq!(
            status,
            StatusCode::OK,
            "{method} {path}: {}",
            String::from_utf8_lossy(&bytes)
        );
        serde_json::from_slice(&bytes).unwrap()
    }

    async fn create_plan(&self, route: &str) -> i64 {
        let created = self.json("POST", route, Some(json!({"name": NAME}))).await;
        assert_eq!(created["name"], NAME);
        created["id"].as_i64().unwrap()
    }

    async fn terminal_job(&self, id: &str) -> Value {
        let path = format!("/api/v1/jobs/{id}");
        tokio::time::timeout(WAIT, async {
            loop {
                let job = self.json("GET", &path, None).await;
                match job["status"].as_str().unwrap() {
                    "done" | "error" | "cancelled" => return job,
                    "pending" | "running" => {}
                    other => panic!("unexpected Node job status {other}: {job}"),
                }
                tokio::time::sleep(Duration::from_millis(25)).await;
            }
        })
        .await
        .unwrap_or_else(|_| panic!("Node kit export {id} never finished"))
    }

    async fn bundle(&self, download: &str) -> Vec<u8> {
        let (status, headers, bytes) = self.call("GET", download, None).await;
        assert_eq!(status, StatusCode::OK, "{download}");
        assert_eq!(headers["content-type"], "application/zip");
        bytes
    }

    async fn refuse_legacy_aliases(&self, profile: i64) {
        for path in ALIASES {
            for progress in [false, true] {
                let (status, _, bytes) = self
                    .call("POST", path, Some(submission(profile, progress)))
                    .await;
                assert_eq!(
                    status,
                    StatusCode::METHOD_NOT_ALLOWED,
                    "{path}: {}",
                    String::from_utf8_lossy(&bytes)
                );
            }
        }
    }

    async fn assert_no_export_work(&self, data: &Path) {
        let jobs = self.json("GET", "/api/v1/jobs", None).await;
        assert_eq!(jobs["jobs"], json!([]), "{jobs}");
        assert!(files_under(&data.join("exports")).is_empty());
    }
}
