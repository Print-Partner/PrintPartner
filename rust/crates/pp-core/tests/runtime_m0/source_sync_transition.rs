use super::watcher_proof::{proof_data, run_in_child, shut_down, signed_in};
use super::{desktop_cookie, launch, request};
use axum::http::StatusCode;
use pp_core::CoreRuntime;
use serde_json::{Value, json};
use std::{collections::BTreeSet, path::Path, time::Duration};
use tokio::time::{sleep, timeout};

const SYNC_ALIASES: [&str; 2] = ["/jobs/sync", "/api/v1/jobs/sync"];
const DEFERRED_JOBS: [&str; 3] = ["check-source-updates", "extract-source-docs", "import-scan"];
const WAIT: Duration = Duration::from_secs(10);

#[tokio::test]
async fn ticket_t_28_recovery_source_sync_aliases_keep_one_rust_owner() {
    let data = tempfile::tempdir().unwrap();
    run_in_child(
        "source_sync_transition::source_sync_boundary_in_child",
        data.path(),
    )
    .await;
}

#[tokio::test]
#[ignore = "run by the source sync transition proof with the rebuild gate set"]
async fn source_sync_boundary_in_child() {
    let data = proof_data();
    let mut receipts = Vec::new();
    let cutover = data.join(pp_gateway::ownership::cutover::CUTOVER_FILE);
    for (start, sources) in [(1, [9_000_001, 9_000_002]), (2, [9_000_003, 9_000_004])] {
        let (runtime, http) = RustOnly::open(&data, start).await;
        let prior = ids_of(&receipts);
        assert_eq!(http.job_ids().await, prior);
        for (id, before) in &receipts {
            assert_eq!(&http.job(id).await, before, "the completed receipt changed");
        }
        http.assert_other_ingress_refused().await;
        assert_eq!(http.job_ids().await, prior);

        for (alias, source) in SYNC_ALIASES.into_iter().zip(sources) {
            receipts.push(http.submit_and_finish(alias, source).await);
        }
        let expected = ids_of(&receipts);
        assert_eq!(
            expected.len(),
            receipts.len(),
            "each enqueue has its own job"
        );
        assert_eq!(http.job_ids().await, expected);
        assert!(!cutover.exists());
        shut_down(runtime).await;
    }
}

fn ids_of(receipts: &[(String, Value)]) -> BTreeSet<String> {
    receipts.iter().map(|(id, _)| id.clone()).collect()
}

struct RustOnly {
    origin: String,
    desktop: String,
    signed_in: String,
}

impl RustOnly {
    async fn open(data: &Path, start: u32) -> (CoreRuntime, Self) {
        let mut input = launch(data.to_owned());
        input.bundle.node = data.join("no-node-executable");
        let mut runtime = CoreRuntime::start_for_launcher(input).await.unwrap();
        assert!(runtime.rust_only());
        assert!(runtime.handle().compat_status().is_none());
        let origin = runtime.origin().to_owned();
        let desktop = desktop_cookie(&mut runtime).await;
        let signed_in = signed_in(&origin, &desktop, start).await;
        let http = Self {
            origin,
            desktop,
            signed_in,
        };
        let state = http.get("/__runtime").await;
        assert_eq!(state["compat"], json!({"state": "not_used"}));
        assert_eq!(state["rust"], json!({"state": "ready"}));
        (runtime, http)
    }

    async fn call(
        &self,
        method: &str,
        path: &str,
        cookie: &str,
        body: Option<Value>,
    ) -> (StatusCode, Vec<u8>) {
        let (status, _, body) = timeout(
            WAIT,
            request(&self.origin, path, method, Some(cookie), None, None, body),
        )
        .await
        .unwrap_or_else(|_| panic!("{method} {path} timed out"));
        (status, body)
    }

    async fn get(&self, path: &str) -> Value {
        let (status, body) = self.call("GET", path, &self.signed_in, None).await;
        assert_eq!(
            status,
            StatusCode::OK,
            "{path}: {}",
            String::from_utf8_lossy(&body)
        );
        serde_json::from_slice(&body).unwrap()
    }

    async fn job_ids(&self) -> BTreeSet<String> {
        self.get("/api/v1/jobs").await["jobs"]
            .as_array()
            .unwrap()
            .iter()
            .map(|job| job["job_id"].as_str().unwrap().to_owned())
            .collect()
    }

    async fn job(&self, id: &str) -> Value {
        self.get(&format!("/api/v1/jobs/{id}")).await
    }

    async fn assert_other_ingress_refused(&self) {
        let body = json!({"project_ids": [9_000_001]});
        for path in SYNC_ALIASES {
            let (status, _) = self
                .call("POST", path, &self.desktop, Some(body.clone()))
                .await;
            assert_eq!(status, StatusCode::UNAUTHORIZED, "{path}");
            let (status, _) = self
                .call(
                    "POST",
                    path,
                    &self.signed_in,
                    Some(json!({"project_ids": []})),
                )
                .await;
            assert_eq!(status, StatusCode::BAD_REQUEST, "{path}");
        }
        for kind in DEFERRED_JOBS {
            for prefix in ["/jobs", "/api/v1/jobs"] {
                let path = format!("{prefix}/{kind}");
                let (status, _) = self
                    .call("POST", &path, &self.signed_in, Some(body.clone()))
                    .await;
                assert_eq!(status, StatusCode::METHOD_NOT_ALLOWED, "{path}");
            }
        }
    }

    async fn submit_and_finish(&self, alias: &str, source: u64) -> (String, Value) {
        let (status, body) = self
            .call(
                "POST",
                alias,
                &self.signed_in,
                Some(json!({"project_ids": [source]})),
            )
            .await;
        assert_eq!(
            status,
            StatusCode::ACCEPTED,
            "{alias}: {}",
            String::from_utf8_lossy(&body)
        );
        let accepted: Value = serde_json::from_slice(&body).unwrap();
        let id = accepted["job_id"].as_str().unwrap().to_owned();
        let terminal = timeout(WAIT, async {
            loop {
                let job = self.job(&id).await;
                match job["status"].as_str().unwrap() {
                    "pending" | "running" => sleep(Duration::from_millis(25)).await,
                    _ => break job,
                }
            }
        })
        .await
        .unwrap_or_else(|_| panic!("the launched sync worker did not finish {alias}'s job"));
        assert_eq!(terminal["job_id"], id);
        assert_eq!(terminal["kind"], "sync", "{terminal}");
        assert_eq!(terminal["status"], "error", "{terminal}");
        assert!(terminal["result"].is_null(), "{terminal}");
        assert_eq!(
            terminal["error"],
            format!("{source}: Source not found"),
            "{terminal}",
        );
        assert_eq!(self.get(&format!("/jobs/{id}")).await, terminal);
        (id, terminal)
    }
}
