use super::{BOUNDARY, multipart, reply, revision_files, stl, zipped};
use pp_core::{
    CoreRuntime,
    rebuild_api::desktop_policy,
    source_uploads::IMPORT_QUOTA,
    uploads::{SourceImports, Through},
};
use pp_storage::{
    Limits, WriterOwner,
    jobs::Credential,
    uploads::{Admission, State, Target},
};
use reqwest::{Client, Method, RequestBuilder, redirect::Policy};
use serde::Deserialize;
use serde_json::{Value, json};
use std::{collections::BTreeSet, path::Path, sync::atomic::AtomicBool, time::Duration};
use tokio::time::{sleep, timeout};

const WAIT: Duration = Duration::from_secs(20);
const RESUME_FILE: &str = "resume-only.stl";
const RESUME_KEY: &str = "owned-import-at-launcher-start";

#[tokio::test]
async fn ticket_t_28_recovery_supplied_import_uses_one_launcher_owner() {
    let invocation = tokio::process::Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "transition::supplied_import_in_child",
            "--ignored",
            "--nocapture",
        ])
        .env("PP_DESKTOP_REBUILD", "1")
        .kill_on_drop(true)
        .output();
    let output = timeout(Duration::from_secs(150), invocation)
        .await
        .expect("supplied-import proof timed out")
        .expect("the proof child could not run");
    assert!(output.status.success(), "{output:?}");
    assert!(String::from_utf8_lossy(&output.stdout).contains("1 passed"));
}

#[tokio::test]
#[ignore = "runs in an isolated subprocess with the rebuild gate on"]
async fn supplied_import_in_child() {
    let directory = tempfile::tempdir().unwrap();
    let data = directory.path().canonicalize().unwrap().join("data");
    let (runtime, http) = Launcher::open(&data, "/auth/register").await;
    let source = http.create_source().await;
    assert_eq!(http.revision(source).await, Value::Null);
    assert!(http.job_ids().await.is_empty());
    http.refuse_legacy_jobs().await;

    let bytes = stl("launcher-upload");
    let zip = zipped(&[("kit/zip.stl", &bytes)]);
    let mut receipts = Vec::new();
    let mut revisions = BTreeSet::new();
    for (index, (alias, form, filename, body, published)) in [
        ("", "upload-files", "bare.stl", bytes.as_slice(), "bare.stl"),
        (
            "/api/v1",
            "upload-files",
            "v1.stl",
            bytes.as_slice(),
            "v1.stl",
        ),
        ("", "upload-zip", "kit.zip", zip.as_slice(), "kit/zip.stl"),
        (
            "/api/v1",
            "upload-zip",
            "kit.zip",
            zip.as_slice(),
            "kit/zip.stl",
        ),
    ]
    .into_iter()
    .enumerate()
    {
        let path = format!("{alias}/sources/{source}/{form}");
        if index == 0 {
            let (status, problem) = http.upload(&path, &http.desktop, filename, body).await;
            assert_eq!(status, 401, "{problem}");
            assert!(http.job_ids().await.is_empty());
            assert!(revision_files(&data).is_empty());
        }
        let (status, accepted) = timeout(WAIT, async {
            loop {
                let attempt = http.upload(&path, &http.session, filename, body).await;
                if attempt.0 != 409 {
                    break attempt;
                }
                sleep(Duration::from_millis(25)).await;
            }
        })
        .await
        .expect("the previous import did not return its slot");
        assert_eq!(status, 202, "{path}: {accepted}");
        let id = accepted["job_id"].as_str().unwrap().to_owned();
        let receipt = http.finished(&id).await;
        assert_eq!(receipt["job_id"], id);
        assert_eq!(receipt["kind"], "supplied-source-import");
        let revision = http.revision(source).await.as_i64().unwrap();
        revisions.insert(revision);
        assert!(
            revision_files(&data)
                .iter()
                .any(|path| path.ends_with(&format!("/{published}")))
        );
        receipts.push((id, receipt));
    }
    assert_eq!(
        revisions.len(),
        3,
        "the identical ZIP must reuse its revision"
    );
    let previous: BTreeSet<_> = receipts.iter().map(|(id, _)| id.clone()).collect();
    assert_eq!(previous.len(), 4);
    assert_eq!(http.job_ids().await, previous);
    let before = http.revision(source).await;
    stop(runtime).await;

    // Leave owned input at the same durable checkpoint as an interrupted worker.
    // Removing the original input makes any attempt to recopy it fail.
    let supplied = tempfile::tempdir_in(directory.path()).unwrap();
    let supplied_path = supplied.path().canonicalize().unwrap();
    let resume_bytes = stl("launcher-startup-resume");
    std::fs::write(supplied_path.join(RESUME_FILE), &resume_bytes).unwrap();
    let pending = leave_owned_input(&data, source, &supplied_path);
    supplied.close().unwrap();
    assert!(
        !revision_files(&data)
            .iter()
            .any(|path| path.ends_with(RESUME_FILE))
    );

    let (runtime, http) = Launcher::open(&data, "/auth/login").await;
    let resumed = http.finished(&pending).await;
    assert_eq!(resumed["kind"], "supplied-source-import");
    let current = http.revision(source).await;
    assert!(current.is_i64());
    assert_ne!(current, before);
    let mut expected = previous;
    assert!(expected.insert(pending.clone()));
    assert_eq!(http.job_ids().await, expected);
    for (id, receipt) in &receipts {
        assert_eq!(
            &http.job(id).await,
            receipt,
            "finished upload changed after restart"
        );
    }
    stop(runtime).await;
    let published = settled_input(&data);
    assert_eq!(published.job_id, pending);
    assert!(published.cleanup_settled);
    let receipt = published.receipt.unwrap();
    assert!(receipt.activated);
    assert_eq!(current, receipt.revision_id);
    let artifact = data.join("repos").join(receipt.artifact.locator);
    assert_eq!(
        std::fs::read(artifact.join(RESUME_FILE)).unwrap(),
        resume_bytes
    );

    let (runtime, http) = Launcher::open(&data, "/auth/login").await;
    assert_eq!(http.job_ids().await, expected);
    assert_eq!(http.job(&pending).await, resumed);
    assert_eq!(http.revision(source).await, current);
    stop(runtime).await;
}

fn leave_owned_input(data: &Path, source_id: i64, supplied: &Path) -> String {
    offline_imports(data, |imports, credential| {
        let cancelled = AtomicBool::new(false);
        let op = imports
            .admit(
                credential,
                Admission {
                    key: RESUME_KEY.into(),
                    target: Target::Existing { source_id },
                    input: SourceImports::directory_input(supplied).unwrap(),
                    reserved_bytes: 32 * 1024 * 1024,
                    max_input_bytes: 1024,
                    max_prepared_bytes: 1024 * 1024,
                },
                supplied,
                &cancelled,
            )
            .unwrap();
        assert_eq!(op.state, State::Admitted);
        let owned = imports
            .work_operation(&op.job_id, Some(supplied), Through::OwnedInput, &cancelled)
            .unwrap()
            .unwrap();
        assert_eq!(owned.state, State::OwnedInputReady);
        assert_eq!(owned.job_id, op.job_id);
        assert!(owned.receipt.is_none());
        owned.job_id
    })
}

fn offline_imports<T>(data: &Path, inspect: impl FnOnce(&SourceImports, Credential) -> T) -> T {
    let (owner, _) = WriterOwner::open(data, Limits::default()).unwrap();
    let imports = SourceImports::new(&owner, desktop_policy(), IMPORT_QUOTA).unwrap();
    let result = inspect(
        &imports,
        Credential::PhysicalOwner(owner.job_physical_owner()),
    );
    drop(imports);
    owner.shutdown().unwrap();
    result
}

fn settled_input(data: &Path) -> pp_storage::uploads::Operation {
    offline_imports(data, |imports, credential| {
        let settled = imports.get(credential, RESUME_KEY.into()).unwrap();
        assert_eq!(settled.state, State::Activated);
        settled
    })
}

async fn stop(runtime: CoreRuntime) {
    let result = timeout(WAIT, runtime.shutdown())
        .await
        .expect("launcher did not stop");
    assert!(result.complete(), "{result:?}");
}

struct Launcher {
    origin: String,
    client: Client,
    desktop: String,
    session: String,
}

impl Launcher {
    async fn open(data: &Path, auth: &str) -> (CoreRuntime, Self) {
        let mut input = super::release::launch(data.to_path_buf());
        input.bundle.node = data.join("missing-node-for-supplied-import-proof");
        let mut runtime = CoreRuntime::start_for_launcher(input)
            .await
            .expect("Rust-only launcher startup");
        assert!(runtime.rust_only(), "the launcher must select Rust");
        assert!(
            runtime.handle().compat_status().is_none(),
            "Node must not start"
        );
        let origin = runtime.origin().to_owned();
        let client = Client::builder()
            .no_proxy()
            .timeout(WAIT)
            .redirect(Policy::none())
            .build()
            .unwrap();
        let bootstrap = client
            .get(runtime.take_launch_target().unwrap().into_url())
            .send()
            .await
            .unwrap();
        assert_eq!(bootstrap.status(), 303);
        let desktop = cookie(&bootstrap);
        let auth_response = client
            .post(format!("{origin}{auth}"))
            .header("Origin", &origin)
            .header("Cookie", &desktop)
            .json(&json!({"email": "import@example.test", "password": "password-1234"}))
            .send()
            .await
            .unwrap();
        assert_eq!(auth_response.status(), 200);
        let session = format!("{desktop}; {}", cookie(&auth_response));
        (
            runtime,
            Self {
                origin,
                client,
                desktop,
                session,
            },
        )
    }

    fn request(&self, method: Method, path: &str, credentials: &str) -> RequestBuilder {
        self.client
            .request(method, format!("{}{path}", self.origin))
            .header("Origin", self.origin.as_str())
            .header("Cookie", credentials)
    }

    async fn get(&self, path: &str) -> Value {
        let (status, body) = reply(self.request(Method::GET, path, &self.session)).await;
        assert_eq!(status, 200, "{path}: {body}");
        body
    }

    async fn create_source(&self) -> i64 {
        let (status, created) = reply(
            self.request(Method::POST, "/api/v1/sources", &self.session)
                .json(&json!({"name": "Local supplied files", "source_kind": "local"})),
        )
        .await;
        assert!(matches!(status, 200 | 201), "{created}");
        created["id"].as_i64().unwrap()
    }

    async fn upload(
        &self,
        path: &str,
        credentials: &str,
        filename: &str,
        bytes: &[u8],
    ) -> (u16, Value) {
        reply(
            self.request(Method::POST, path, credentials)
                .header(
                    "Content-Type",
                    format!("multipart/form-data; boundary={BOUNDARY}"),
                )
                .body(multipart(&[(filename, bytes)])),
        )
        .await
    }

    async fn revision(&self, id: i64) -> Value {
        self.get(&format!("/api/v1/sources/{id}")).await["current_source_revision_id"].clone()
    }

    async fn job_ids(&self) -> BTreeSet<String> {
        #[derive(Deserialize)]
        struct JobId {
            job_id: String,
        }
        let mut list = self.get("/api/v1/jobs").await;
        let jobs: Vec<JobId> = serde_json::from_value(list["jobs"].take()).unwrap();
        jobs.into_iter().map(|job| job.job_id).collect()
    }

    async fn job(&self, id: &str) -> Value {
        self.get(&format!("/api/v1/jobs/{id}")).await
    }

    async fn finished(&self, id: &str) -> Value {
        timeout(WAIT, async {
            loop {
                let job = self.job(id).await;
                match job["status"].as_str() {
                    Some("done") => break job,
                    Some("pending" | "running") => sleep(Duration::from_millis(25)).await,
                    _ => panic!("supplied import did not complete: {job}"),
                }
            }
        })
        .await
        .unwrap_or_else(|_| panic!("job {id} did not finish"))
    }

    async fn refuse_legacy_jobs(&self) {
        for prefix in ["", "/api/v1"] {
            for job in ["import-scan", "check-source-updates", "extract-source-docs"] {
                let path = format!("{prefix}/jobs/{job}");
                let (status, body) = reply(
                    self.request(Method::POST, &path, &self.session)
                        .json(&json!({})),
                )
                .await;
                assert!(matches!(status, 404 | 405), "{path}: {status} {body}");
            }
        }
        assert!(self.job_ids().await.is_empty());
    }
}

fn cookie(response: &reqwest::Response) -> String {
    response.headers()["set-cookie"]
        .to_str()
        .unwrap()
        .split(';')
        .next()
        .unwrap()
        .to_owned()
}
