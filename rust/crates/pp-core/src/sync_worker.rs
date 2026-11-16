//! Syncs claimed GitHub Sources through the import engine (GRE-1724).
//!
//! The worker claims `JobKind::Sync`, asks [`plan_github_sync`] which commit
//! and files to keep, downloads those files from the GitHub transport, and
//! publishes them with the same snapshot store a supplied import uses.
//! Activation records the commit on the Source. A later sync of the same
//! commit reuses that revision. Tests point the transport at a local stand-in
//! and never call GitHub.

use anyhow::{Context, Result, anyhow};
use pp_api::sources_github::GithubConnector;
use pp_services::sync::{self, SnapshotPlan, parse_github_url, plan_github_sync};
use pp_source::{
    ArtifactBudget, ContentFile, FileKind, MAX_CONTENT_BYTES, SnapshotRequest, TenantRepos,
};
use pp_storage::jobs::{ResultArtifact, SourceScanResult};
use pp_storage::{
    WriterOwner,
    auth::AuthPolicy,
    catalog::SourceBusy,
    jobs::{AttemptLease, ClaimedAttempt, JobKind, Payload, ServerWorkerClient, WorkerOperation},
    source_sync::{
        GithubSyncTarget, NextSyncTarget, SourceSyncFinishOutcome, SourceSyncHalt, SyncSettlement,
        SyncTargetLease,
    },
    uploads::{File, GithubSnapshot},
};
use sha2::{Digest, Sha256};
use std::{
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};
use tokio::sync::Notify;

const IDLE: Duration = Duration::from_millis(250);

pub struct SyncWorker {
    worker: ServerWorkerClient,
    transport: crate::github::GithubTransport,
    repos: PathBuf,
    wake: Arc<Notify>,
}

impl SyncWorker {
    pub fn new(
        owner: &WriterOwner,
        policy: AuthPolicy,
        transport: crate::github::GithubTransport,
    ) -> Result<Self> {
        Ok(Self {
            worker: owner.job_worker_with_policy(policy, crate::uploads::worker_admission())?,
            transport,
            repos: owner.import_repos_root()?,
            wake: Arc::new(Notify::new()),
        })
    }

    pub fn waker(&self) -> Arc<Notify> {
        Arc::clone(&self.wake)
    }

    /// Claims and runs sync jobs until `cancelled` is set. An idle worker
    /// rechecks shortly, because enqueue lives in the HTTP router and does
    /// not hold this wake.
    pub fn spawn(self, cancelled: Arc<AtomicBool>) -> tokio::task::JoinHandle<()> {
        tokio::spawn(async move {
            while !cancelled.load(Ordering::Acquire) {
                match self.claim_and_run(&cancelled).await {
                    Ok(true) => {}
                    Ok(false) => {
                        let _ = tokio::time::timeout(IDLE, self.wake.notified()).await;
                    }
                    Err(error) => {
                        tracing::warn!(error = %error, "GitHub sync stopped");
                        tokio::time::sleep(Duration::from_millis(250)).await;
                    }
                }
            }
        })
    }

    /// Claims one waiting sync and runs it. `Ok(false)` means the queue was empty.
    pub async fn run_next(&self) -> Result<bool> {
        self.claim_and_run(&Arc::new(AtomicBool::new(false))).await
    }

    async fn claim_and_run(&self, cancelled: &Arc<AtomicBool>) -> Result<bool> {
        let worker = self.worker.clone();
        let flag = Arc::clone(cancelled);
        let claimed = tokio::task::spawn_blocking(move || {
            worker
                .claim_pending_kind(JobKind::Sync, &flag, Duration::from_secs(1))?
                .receive()
        })
        .await
        .context("Sync worker stopped")??;
        match claimed {
            Some(claimed) => {
                self.run(claimed, cancelled).await?;
                Ok(true)
            }
            None => Ok(false),
        }
    }

    async fn run(&self, claimed: ClaimedAttempt, cancelled: &AtomicBool) -> Result<()> {
        let ClaimedAttempt { job, mut lease, .. } = claimed;
        let Payload::Sync { .. } = &job.payload else {
            return Err(anyhow!("Sync worker claimed another job kind"));
        };
        let tenant = job.tenant.clone();
        self.with_lease(&mut lease, |worker, attempt| {
            worker.authorize(&attempt)?;
            worker.open_source_sync(&attempt)?;
            Ok(((), attempt))
        })
        .await?;
        loop {
            if cancelled.load(Ordering::Acquire) {
                return Ok(());
            }
            let next = match self
                .with_lease(&mut lease, |worker, mut attempt| {
                    let next = worker.claim_next_source_sync_target(&mut attempt)?;
                    Ok((next, attempt))
                })
                .await
            {
                Ok(next) => next,
                Err(error) if error.downcast_ref::<SourceBusy>().is_some() => {
                    self.touch(&mut lease).await?;
                    tokio::time::sleep(Duration::from_millis(50)).await;
                    continue;
                }
                Err(error) => return Err(error),
            };
            match next {
                NextSyncTarget::Complete => {
                    let outcome = self
                        .with_lease(&mut lease, |worker, attempt| {
                            Ok((worker.finish_source_sync(&attempt)?, attempt))
                        })
                        .await?;
                    return match outcome {
                        SourceSyncFinishOutcome::Completed(_) => Ok(()),
                        SourceSyncFinishOutcome::NeedsReconciliation => {
                            Err(anyhow!("Sync needs reconciliation"))
                        }
                    };
                }
                NextSyncTarget::Halted => return Ok(()),
                NextSyncTarget::OrdinaryFailure(_) => {}
                NextSyncTarget::RetainedFailure(_) => {
                    self.with_lease(&mut lease, |worker, attempt| {
                        worker.halt_source_sync(
                            &attempt,
                            SourceSyncHalt::Fatal,
                            "A sync target still holds its reservation".into(),
                        )?;
                        Ok(((), attempt))
                    })
                    .await?;
                    return Ok(());
                }
                NextSyncTarget::Local(target) => {
                    self.fail_lease(
                        &mut lease,
                        target,
                        "Local directory sync is not this worker",
                    )
                    .await?;
                }
                NextSyncTarget::Github(target) => {
                    self.sync_github(&mut lease, target, &tenant, cancelled)
                        .await?;
                }
            }
        }
    }

    async fn sync_github(
        &self,
        lease: &mut AttemptLease,
        mut target: GithubSyncTarget,
        tenant: &str,
        cancelled: &AtomicBool,
    ) -> Result<()> {
        if cancelled.load(Ordering::Acquire) {
            return Ok(());
        }
        self.touch(lease).await?;
        match self.prepare(&target, tenant, cancelled).await {
            Ok((snapshot, settlement)) => {
                self.with_lease(lease, move |worker, attempt| {
                    worker.activate_github_source(&attempt, target.lease(), snapshot)?;
                    worker.settle_source_sync_target(&attempt, target.lease_mut(), settlement)?;
                    Ok(((), attempt))
                })
                .await
            }
            Err(Stop::Refused(detail)) => self.fail_github(lease, target, &detail).await,
            Err(Stop::Broken(error)) => {
                let detail = clip(&error.to_string());
                self.fail_github(lease, target, &detail).await?;
                Err(error)
            }
        }
    }

    async fn prepare(
        &self,
        target: &GithubSyncTarget,
        tenant: &str,
        cancelled: &AtomicBool,
    ) -> Result<(GithubSnapshot, SyncSettlement), Stop> {
        let found = parse_github_url(target.url())
            .ok_or_else(|| Stop::Refused("Source URL is not a GitHub repository".into()))?;
        let session = self.transport.port(target.token().map(str::to_owned));
        let plan = plan_github_sync(
            &session,
            target.url(),
            target.branch(),
            target.tag(),
            sync::SyncLimits::default(),
        )
        .await
        .map_err(|error| Stop::Refused(clip(&error.to_string())))?;
        if plan.files.is_empty() {
            return Err(Stop::Refused("GitHub Source has no snapshot files".into()));
        }
        let source_id = u64::try_from(target.lease().observation().source_id())
            .map_err(|_| Stop::Refused("Invalid Source id".into()))?;
        let request = snapshot_request(&plan)?;
        let staging = self
            .repos
            .join(".pp-github-sync")
            .join(&plan.commit_sha)
            .join(source_id.to_string());
        let _cleanup = RemoveOnDrop(staging.clone());
        if staging.exists() {
            tokio::fs::remove_dir_all(&staging)
                .await
                .map_err(|error| Stop::Broken(error.into()))?;
        }
        tokio::fs::create_dir_all(&staging)
            .await
            .map_err(|error| Stop::Broken(error.into()))?;
        for file in &plan.files {
            if cancelled.load(Ordering::Acquire) {
                return Err(Stop::Broken(anyhow!("Sync worker is stopping")));
            }
            if !safe_path(&file.path) {
                return Err(Stop::Refused("Invalid GitHub path".into()));
            }
            let destination = staging.join(&file.path);
            if let Some(parent) = destination.parent() {
                tokio::fs::create_dir_all(parent)
                    .await
                    .map_err(|error| Stop::Broken(error.into()))?;
            }
            let mut out = tokio::fs::File::create(&destination)
                .await
                .map_err(|error| Stop::Broken(error.into()))?;
            let cap = file.size_hint_bytes.unwrap_or(MAX_CONTENT_BYTES);
            session
                .download(
                    &found.owner,
                    &found.repo,
                    &plan.commit_sha,
                    &file.path,
                    cap,
                    &mut out,
                )
                .await
                .map_err(|error| Stop::Refused(clip(&error.message)))?;
        }
        let repos = self.repos.clone();
        let tenant = tenant.to_owned();
        let published = tokio::task::spawn_blocking(move || {
            let budget = ArtifactBudget::new(MAX_CONTENT_BYTES, MAX_CONTENT_BYTES)?;
            TenantRepos::open(tenant, &repos)?
                .source(source_id)?
                .materialize_from_directory(request, &staging, budget)
        })
        .await
        .map_err(|error| Stop::Broken(anyhow!("Sync worker stopped: {error}")))?
        .map_err(|error| match error {
            pp_source::Error::Limit => {
                Stop::Refused("The import is over its size or file-count limit".into())
            }
            other => Stop::Broken(other.into()),
        })?;
        if !published
            .upstream_revision_key
            .starts_with(plan.commit_sha.as_str())
            || !digest_text(&published.manifest_digest)
        {
            return Err(Stop::Broken(anyhow!(
                "Published snapshot does not match the commit"
            )));
        }
        let (stl_count, doc_count) = counts(&published.files);
        let source_id = published.source_id;
        let snapshot = GithubSnapshot {
            commit_sha: plan.commit_sha,
            revision_key: published.upstream_revision_key.clone(),
            manifest_digest: published.manifest_digest.clone(),
            locator: published.snapshot_locator,
            stored_bytes: published.stored_bytes,
            files: published
                .files
                .iter()
                .map(|file| File {
                    path: file.path.as_str().into(),
                    size: file.size_bytes,
                    sha256: file.sha256.clone(),
                    kind: stored_kind(&file.kind).into(),
                })
                .collect(),
        };
        let settlement = SyncSettlement {
            result: SourceScanResult {
                project_id: Some(source_id),
                stl_count,
                downloaded: stl_count,
                doc_count,
                docs_downloaded: doc_count,
                pdf_extract_job_id: None,
                postprocess_warning: None,
            },
            receipt: ResultArtifact {
                // Unique per settlement. The same commit reuses the revision
                // but still records a new sync receipt.
                receipt_id: format!(
                    "local-scan-{}",
                    &hex::encode(Sha256::digest(
                        format!(
                            "{}:{source_id}:{}",
                            published.manifest_digest,
                            hex::encode(rand::random::<[u8; 16]>())
                        )
                        .as_bytes()
                    ))[..32]
                ),
                content_hash: published.manifest_digest,
                target: format!("source:{source_id}"),
            },
        };
        Ok((snapshot, settlement))
    }

    async fn touch(&self, lease: &mut AttemptLease) -> Result<()> {
        self.with_lease(lease, |worker, mut attempt| {
            worker.update(&mut attempt, WorkerOperation::Heartbeat)?;
            Ok(((), attempt))
        })
        .await
    }

    async fn fail_github(
        &self,
        lease: &mut AttemptLease,
        target: GithubSyncTarget,
        detail: &str,
    ) -> Result<()> {
        let observation = target.lease().observation().clone();
        let detail = clip(detail);
        self.record_failure(lease, move |worker, attempt| {
            worker.fail_source_sync_target(attempt, target.lease(), &detail)?;
            worker.acknowledge_source_sync_target(attempt, target.lease(), &observation)?;
            Ok(())
        })
        .await
    }

    async fn fail_lease(
        &self,
        lease: &mut AttemptLease,
        target: SyncTargetLease,
        detail: &str,
    ) -> Result<()> {
        let observation = target.observation().clone();
        let detail = clip(detail);
        self.record_failure(lease, move |worker, attempt| {
            worker.fail_source_sync_target(attempt, &target, &detail)?;
            worker.acknowledge_source_sync_target(attempt, &target, &observation)?;
            Ok(())
        })
        .await
    }

    /// Runs `fail` on a blocking thread and keeps the lease version it wrote,
    /// including when acknowledge refuses after fail has already advanced it.
    async fn record_failure(
        &self,
        lease: &mut AttemptLease,
        fail: impl FnOnce(&ServerWorkerClient, &mut AttemptLease) -> Result<()> + Send + 'static,
    ) -> Result<()> {
        let worker = self.worker.clone();
        let mut attempt = lease.clone();
        let (result, attempt) = tokio::task::spawn_blocking(move || {
            let result = fail(&worker, &mut attempt);
            (result, attempt)
        })
        .await
        .context("Sync worker stopped")?;
        *lease = attempt;
        result
    }

    async fn with_lease<T: Send + 'static>(
        &self,
        lease: &mut AttemptLease,
        work: impl FnOnce(ServerWorkerClient, AttemptLease) -> Result<(T, AttemptLease)>
        + Send
        + 'static,
    ) -> Result<T> {
        let worker = self.worker.clone();
        let attempt = lease.clone();
        let (value, attempt) = tokio::task::spawn_blocking(move || work(worker, attempt))
            .await
            .context("Sync worker stopped")??;
        *lease = attempt;
        Ok(value)
    }
}

enum Stop {
    Refused(String),
    Broken(anyhow::Error),
}

fn snapshot_request(plan: &SnapshotPlan) -> Result<SnapshotRequest, Stop> {
    let value = serde_json::to_value(plan).map_err(|error| Stop::Broken(error.into()))?;
    Ok(SnapshotRequest {
        upstream_revision_key: plan.upstream_revision_key.clone(),
        files: serde_json::from_value(value["files"].clone())
            .map_err(|error| Stop::Broken(error.into()))?,
        selection: serde_json::from_value(value["selection"].clone())
            .map_err(|error| Stop::Broken(error.into()))?,
    })
}

fn counts(files: &[ContentFile]) -> (u64, u64) {
    let stl = files
        .iter()
        .filter(|file| file.kind == FileKind::Stl)
        .count() as u64;
    let docs = files
        .iter()
        .filter(|file| matches!(file.kind, FileKind::Readme | FileKind::Md | FileKind::Pdf))
        .count() as u64;
    (stl, docs)
}

fn stored_kind(kind: &FileKind) -> &'static str {
    match kind {
        FileKind::Stl => "stl",
        FileKind::Artifact => "artifact",
        FileKind::Readme => "readme",
        FileKind::Md => "md",
        FileKind::Pdf => "pdf",
    }
}

fn digest_text(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
}

fn safe_path(path: &str) -> bool {
    !path.is_empty()
        && !path.starts_with('/')
        && path
            .split('/')
            .all(|part| !part.is_empty() && part != "." && part != "..")
}

fn clip(detail: &str) -> String {
    let mut detail = detail.trim().to_owned();
    if detail.is_empty() {
        detail = "GitHub sync failed".into();
    }
    if detail.len() > 1024 {
        detail.truncate(1024);
        while !detail.is_char_boundary(detail.len()) {
            detail.pop();
        }
    }
    detail
}

struct RemoveOnDrop(PathBuf);

impl Drop for RemoveOnDrop {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{
        Json, Router,
        extract::{Path, State},
        http::StatusCode,
        response::IntoResponse,
        routing::get,
    };
    use pp_api::{
        auth::{AuthHttpConfig, CookieTransport, ProviderClient, ResetMailer, auth_router},
        catalog::{CatalogHttpConfig, admit_catalog_routes},
        jobs::{JobsHttpConfig, jobs_router},
    };
    use pp_storage::{
        Limits,
        auth::{AuthPolicy, FirstUserTenant, RegistrationPolicy, SessionTenantPolicy},
    };
    use reqwest::Method;
    use serde_json::{Value, json};
    use std::{collections::HashMap, sync::Mutex};

    struct World {
        branch_sha: String,
        tag_sha: String,
        files: HashMap<String, Vec<(String, Vec<u8>)>>,
        hits: u32,
        raw_paths: Vec<String>,
    }

    fn sha(label: &str) -> String {
        hex::encode(Sha256::digest(label.as_bytes()))[..40].to_string()
    }

    fn kit(stl: &[u8]) -> Vec<(String, Vec<u8>)> {
        vec![
            ("part.stl".into(), stl.to_vec()),
            ("README.md".into(), b"# Kit\n".to_vec()),
            ("notes/guide.md".into(), b"guide\n".to_vec()),
            ("sheet.pdf".into(), b"%PDF-1.1\n".to_vec()),
            (
                "print-partner.manifest.yaml".into(),
                b"name: kit\n".to_vec(),
            ),
        ]
    }

    fn policy() -> AuthPolicy {
        AuthPolicy {
            registration: RegistrationPolicy::FirstAccountOnly,
            first_user: FirstUserTenant::NewUser,
            session_tenant: SessionTenantPolicy::SingleAccountDefault,
        }
    }

    async fn stand_in(world: Arc<Mutex<World>>) -> std::net::SocketAddr {
        let router = Router::new()
            .route("/repos/{owner}/{repo}/commits/{git_ref}", get(commit_sha))
            .route("/repos/{owner}/{repo}/git/trees/{sha}", get(tree))
            .route("/raw/{owner}/{repo}/{commit}/{*path}", get(raw))
            .with_state(world);
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(listener, router).await.unwrap();
        });
        address
    }

    async fn commit_sha(
        Path((owner, repo, git_ref)): Path<(String, String, String)>,
        State(world): State<Arc<Mutex<World>>>,
    ) -> impl IntoResponse {
        let mut world = world.lock().unwrap();
        world.hits += 1;
        if owner != "maker" || repo != "kit" {
            return StatusCode::NOT_FOUND.into_response();
        }
        let sha = if git_ref == "main" {
            world.branch_sha.clone()
        } else if git_ref == "v1" {
            world.tag_sha.clone()
        } else if world.files.contains_key(&git_ref) {
            git_ref
        } else {
            return StatusCode::NOT_FOUND.into_response();
        };
        sha.into_response()
    }

    async fn tree(
        Path((owner, repo, commit)): Path<(String, String, String)>,
        State(world): State<Arc<Mutex<World>>>,
    ) -> impl IntoResponse {
        let mut world = world.lock().unwrap();
        world.hits += 1;
        if owner != "maker" || repo != "kit" {
            return StatusCode::NOT_FOUND.into_response();
        }
        let Some(files) = world.files.get(&commit).cloned() else {
            return StatusCode::NOT_FOUND.into_response();
        };
        let tree = files
            .into_iter()
            .map(|(path, bytes)| {
                json!({
                    "path": path,
                    "type": "blob",
                    "mode": "100644",
                    "size": bytes.len(),
                    "sha": sha(&format!("blob:{path}:{}", bytes.len())),
                })
            })
            .collect::<Vec<_>>();
        Json(json!({ "tree": tree, "truncated": false })).into_response()
    }

    async fn raw(
        Path((owner, repo, commit, path)): Path<(String, String, String, String)>,
        State(world): State<Arc<Mutex<World>>>,
    ) -> impl IntoResponse {
        let mut world = world.lock().unwrap();
        world.hits += 1;
        world.raw_paths.push(path.clone());
        if owner != "maker" || repo != "kit" {
            return StatusCode::NOT_FOUND.into_response();
        }
        let body = world.files.get(&commit).and_then(|files| {
            files
                .iter()
                .find(|(name, _)| name == &path)
                .map(|(_, bytes)| bytes.clone())
        });
        match body {
            Some(body) => body.into_response(),
            None => StatusCode::NOT_FOUND.into_response(),
        }
    }

    struct App {
        origin: String,
        client: reqwest::Client,
        cookie: String,
        serving: pp_api::serving::Serving,
    }

    impl App {
        async fn exchange(&self, method: Method, path: &str, body: Option<&Value>) -> (u16, Value) {
            let mut request = self
                .client
                .request(method, format!("{}{path}", self.origin))
                .header("Origin", &self.origin)
                .header("Cookie", &self.cookie);
            if let Some(body) = body {
                request = request.json(body);
            }
            let response = request.send().await.unwrap();
            let status = response.status().as_u16();
            let body = response.text().await.unwrap();
            let parsed = serde_json::from_str(&body).unwrap_or(Value::Null);
            (status, parsed)
        }
    }

    fn revisions(root: &std::path::Path, id: i64) -> Vec<String> {
        let mut names =
            std::fs::read_dir(root.join("repos").join(id.to_string()).join("revisions"))
                .unwrap()
                .map(|entry| entry.unwrap().file_name().into_string().unwrap())
                .collect::<Vec<_>>();
        names.sort();
        names
    }

    #[tokio::test]
    async fn github_sync_publishes_revisions_against_a_local_stand_in() {
        let first = sha("commit-main-1");
        let second = sha("commit-main-2");
        let tag = sha("commit-v1");
        let world = Arc::new(Mutex::new(World {
            branch_sha: first.clone(),
            tag_sha: tag.clone(),
            files: HashMap::from([
                (first.clone(), kit(b"solid first\n")),
                (tag.clone(), kit(b"solid tag\n")),
            ]),
            hits: 0,
            raw_paths: Vec::new(),
        }));
        let address = stand_in(Arc::clone(&world)).await;
        assert!(address.ip().is_loopback());
        let transport = crate::github::GithubTransport::stand_in(
            reqwest::Url::parse(&format!("http://{address}/")).unwrap(),
            reqwest::Url::parse(&format!("http://{address}/raw/")).unwrap(),
            Duration::from_secs(5),
        );

        let directory = tempfile::tempdir().unwrap();
        let owner = Arc::new(std::sync::Mutex::new(
            WriterOwner::open(directory.path(), Limits::default())
                .unwrap()
                .0,
        ));
        let worker = {
            let guard = owner.lock().unwrap();
            SyncWorker::new(&guard, policy(), transport).unwrap()
        };
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let origin = format!("http://{}", listener.local_addr().unwrap());
        let auth = owner.lock().unwrap().auth_with_policy(policy()).unwrap();
        let jobs = owner.lock().unwrap().jobs(policy()).unwrap();
        let router = admit_catalog_routes(
            CatalogHttpConfig::from_startup(&origin, "default", policy(), Some("true")).unwrap(),
            Arc::clone(&owner),
        )
        .merge(jobs_router(
            JobsHttpConfig::from_startup(&origin, "default", Some("true")).unwrap(),
            jobs,
        ))
        .merge(auth_router(
            AuthHttpConfig::new(&origin, CookieTransport::LoopbackHttp, false, None, false)
                .unwrap(),
            auth,
            ProviderClient::new(None, None).unwrap(),
            ResetMailer::disabled(),
        ));
        let serving = pp_api::serving::Serving::start(listener, router);
        let client = reqwest::Client::builder().no_proxy().build().unwrap();
        let registered = client
            .post(format!("{origin}/auth/register"))
            .header("Origin", &origin)
            .json(&json!({"email":"sync@example.test","password":"password-1234"}))
            .send()
            .await
            .unwrap();
        assert_eq!(registered.status(), 200, "{registered:?}");
        let cookie = registered.headers()["set-cookie"]
            .to_str()
            .unwrap()
            .split(';')
            .next()
            .unwrap()
            .to_owned();
        let app = App {
            origin,
            client,
            cookie,
            serving,
        };

        for (path, body) in [
            ("/api/v1/jobs/sync", json!({})),
            ("/api/v1/jobs/sync", json!({"project_ids": null})),
            ("/jobs/sync", json!({"project_ids": []})),
            ("/jobs/sync", json!({"project_ids": "main"})),
        ] {
            let (status, problem) = app.exchange(Method::POST, path, Some(&body)).await;
            assert_eq!(status, 400, "{path} {problem}");
            assert!(
                problem["detail"]
                    .as_str()
                    .unwrap_or("")
                    .contains("project_ids"),
                "{problem}"
            );
        }

        let (status, created) = app
            .exchange(
                Method::POST,
                "/api/v1/sources",
                Some(&json!({
                    "name": "Kit",
                    "url": "https://github.com/maker/kit",
                    "branch": "main",
                    "source_kind": "github"
                })),
            )
            .await;
        assert_eq!(status, 200, "{created}");
        let source_id = created["id"].as_i64().unwrap();
        let (status, job) = app
            .exchange(
                Method::POST,
                "/api/v1/jobs/sync",
                Some(&json!({"project_ids": [source_id]})),
            )
            .await;
        assert_eq!(status, 202, "{job}");
        let job_id = job["job_id"].as_str().unwrap();
        assert_eq!(job_id.len(), 64);
        assert!(worker.run_next().await.unwrap(), "sync job was not claimed");
        let (status, snapshot) = app
            .exchange(Method::GET, &format!("/api/v1/jobs/{job_id}"), None)
            .await;
        assert_eq!(status, 200, "{snapshot}");
        assert_eq!(snapshot["status"], "done", "{snapshot}");
        assert_eq!(snapshot["result"]["synced"], 1, "{snapshot}");
        let source = source_of(&app, source_id).await;
        assert_eq!(source["last_commit_sha"], first);
        assert!(
            source["last_synced_at"]
                .as_str()
                .is_some_and(|at| !at.is_empty())
        );
        let revision_id = source["current_source_revision_id"].as_i64().unwrap();
        let published = revisions(directory.path(), source_id);
        assert_eq!(published, vec![format!("{first}.snapshot-v2")]);
        for name in [
            "part.stl",
            "README.md",
            "notes/guide.md",
            "sheet.pdf",
            "print-partner.manifest.yaml",
        ] {
            let path = directory
                .path()
                .join("repos")
                .join(source_id.to_string())
                .join("revisions")
                .join(&published[0])
                .join(name);
            assert!(path.is_file(), "{}", path.display());
        }
        assert_eq!(
            std::fs::read(
                directory
                    .path()
                    .join("repos")
                    .join(source_id.to_string())
                    .join("revisions")
                    .join(&published[0])
                    .join("part.stl")
            )
            .unwrap(),
            b"solid first\n"
        );
        {
            let seen = world.lock().unwrap();
            assert!(seen.hits >= 7, "stand-in saw {} requests", seen.hits);
            for name in [
                "part.stl",
                "README.md",
                "notes/guide.md",
                "sheet.pdf",
                "print-partner.manifest.yaml",
            ] {
                assert!(seen.raw_paths.iter().any(|path| path == name), "{name}");
            }
        }

        world.lock().unwrap().branch_sha = second.clone();
        world
            .lock()
            .unwrap()
            .files
            .insert(second.clone(), kit(b"solid second\n"));
        sync_again(&app, &worker, source_id).await;
        let moved = source_of(&app, source_id).await;
        let moved_id = moved["current_source_revision_id"].as_i64().unwrap();
        assert_ne!(moved_id, revision_id);
        assert_eq!(moved["last_commit_sha"], second);
        let both = revisions(directory.path(), source_id);
        let mut expected = vec![
            format!("{first}.snapshot-v2"),
            format!("{second}.snapshot-v2"),
        ];
        expected.sort();
        assert_eq!(both, expected);
        assert_eq!(
            std::fs::read(
                directory
                    .path()
                    .join("repos")
                    .join(source_id.to_string())
                    .join("revisions")
                    .join(format!("{first}.snapshot-v2"))
                    .join("part.stl")
            )
            .unwrap(),
            b"solid first\n"
        );
        assert_eq!(
            std::fs::read(
                directory
                    .path()
                    .join("repos")
                    .join(source_id.to_string())
                    .join("revisions")
                    .join(format!("{second}.snapshot-v2"))
                    .join("part.stl")
            )
            .unwrap(),
            b"solid second\n"
        );

        sync_again(&app, &worker, source_id).await;
        let same = source_of(&app, source_id).await;
        assert_eq!(
            same["current_source_revision_id"].as_i64().unwrap(),
            moved_id
        );
        assert_eq!(same["last_commit_sha"], second);
        assert_eq!(revisions(directory.path(), source_id), both);

        let (status, tagged) = app
            .exchange(
                Method::POST,
                "/api/v1/sources",
                Some(&json!({
                    "name": "Tagged",
                    "url": "https://github.com/maker/kit",
                    "branch": "main",
                    "tag": "v1",
                    "source_kind": "github"
                })),
            )
            .await;
        assert_eq!(status, 200, "{tagged}");
        let tagged_id = tagged["id"].as_i64().unwrap();
        sync_again(&app, &worker, tagged_id).await;
        let tagged = source_of(&app, tagged_id).await;
        assert_eq!(tagged["last_commit_sha"], tag);
        assert_ne!(tagged["last_commit_sha"], second);
        assert_eq!(
            revisions(directory.path(), tagged_id),
            vec![format!("{tag}.snapshot-v2")]
        );

        let mut serving = app.serving;
        serving.stop().await.unwrap();
        drop(worker);
        drop(app.client);
        let owner = match Arc::try_unwrap(owner) {
            Ok(owner) => owner,
            Err(_) => panic!("router still holds storage"),
        };
        owner.into_inner().unwrap().shutdown().unwrap();
        // Opening again runs the import journal check, including the GitHub rows.
        let reopened = WriterOwner::open(directory.path(), Limits::default())
            .unwrap()
            .0;
        reopened.shutdown().unwrap();
    }

    async fn source_of(app: &App, id: i64) -> Value {
        let (status, source) = app
            .exchange(Method::GET, &format!("/api/v1/sources/{id}"), None)
            .await;
        assert_eq!(status, 200, "{source}");
        source
    }

    async fn sync_again(app: &App, worker: &SyncWorker, id: i64) {
        let (status, job) = app
            .exchange(
                Method::POST,
                "/jobs/sync",
                Some(&json!({"project_ids": [id]})),
            )
            .await;
        assert_eq!(status, 202, "{job}");
        assert!(worker.run_next().await.unwrap(), "sync job was not claimed");
        let job_id = job["job_id"].as_str().unwrap();
        let (status, snapshot) = app
            .exchange(Method::GET, &format!("/api/v1/jobs/{job_id}"), None)
            .await;
        assert_eq!(status, 200, "{snapshot}");
        assert_eq!(snapshot["status"], "done", "{snapshot}");
        assert_eq!(snapshot["result"]["synced"], 1, "{snapshot}");
    }
}
