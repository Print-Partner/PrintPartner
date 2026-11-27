use super::{Fake, Fixture, Gate, Mode, SESSION, assert_one_put, confirm, prepared};
use pp_core::CoreRuntime;
use reqwest::{
    Client, Method, StatusCode,
    header::{COOKIE, HeaderMap, ORIGIN},
};
use serde_json::{Value, json};
use std::{path::Path, time::Duration};
use tokio::time::{sleep, timeout};

const BATCH: &str = "/printer-checkoff/verify-batch";
const V2_BATCH: &str = "/api/v2/printer-checkoff/verify-batch";
const DEDUCTIONS: &str = "/api/v2/spoolman/deductions";
const WAIT: Duration = Duration::from_secs(12);

#[tokio::test]
async fn ticket_t_28_recovery_spoolman_routes_reach_the_launched_worker() {
    let mut child = tokio::process::Command::new(std::env::current_exe().unwrap());
    child
        .args([
            "--exact",
            "transition::boundary_in_child",
            "--ignored",
            "--nocapture",
        ])
        .env("PP_DESKTOP_REBUILD", "1")
        .kill_on_drop(true);
    let output = timeout(Duration::from_secs(120), child.output())
        .await
        .expect("Spoolman launcher proof timed out")
        .expect("could not run the Spoolman launcher proof");
    assert!(
        output.status.success(),
        "Spoolman launcher proof failed: {output:?}"
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("1 passed"),
        "child did not exercise the boundary: {stdout}"
    );
}

#[tokio::test]
#[ignore = "runs with the rebuild gate set in an isolated subprocess"]
async fn boundary_in_child() {
    lost_reply_and_aliases().await;
    explicit_retry().await;
}

async fn lost_reply_and_aliases() {
    let fake = Fake::start(Mode::Drop).await;
    let fixture = seed(&fake);
    let (runtime, http) = Launcher::open(&fixture.path).await;
    assert_eq!(http.deductions().await, json!([]));
    let (put_seen, apply_put) = Gate::hold(&fake.gates.before_put);
    let body = burst("transition-first");
    let verified = http.post(V2_BATCH, body.clone()).await;
    assert_eq!(verified["units_confirmed"], 2);
    let undo_token = verified["undo_token"].as_str().unwrap();
    timeout(WAIT, put_seen)
        .await
        .expect("the launched worker did not send")
        .unwrap();
    let intent = http.wait_for("sending").await;
    assert_eq!(intent["link_id"], "link-verify");
    assert_eq!(intent["integration_id"], "sm-1");
    assert_eq!(intent["spool_id"], 7);
    assert_eq!(intent["units"], 2);
    assert_eq!(intent["grams"], 20);
    assert_eq!(fake.remaining(7), 512.4);
    assert_eq!(http.post(BATCH, body).await["undo_token"], undo_token);
    let status = http.burst_status(BATCH, "transition-first").await;
    assert_eq!(status["receipt"], verified);
    assert_eq!(
        http.burst_status(V2_BATCH, "transition-first").await,
        status
    );
    assert!(apply_put.send(()).is_ok());

    let unknown = http.wait_for("unknown").await;
    assert_eq!(unknown["id"], intent["id"]);
    assert_eq!(unknown["failure"], "reply_lost");
    let id = unknown["id"].as_i64().unwrap();
    assert_one_put(&fake, 20.0);
    assert!((fake.remaining(7) - 492.4).abs() < 1e-6);
    stop(runtime).await;

    let (runtime, http) = Launcher::open(&fixture.path).await;
    assert_eq!(http.deductions().await, json!([unknown]));
    assert_eq!(
        http.burst_status(V2_BATCH, "transition-first").await,
        status
    );
    let detail = http.detail(id).await;
    assert_eq!(detail["deduction"], unknown);
    assert_eq!(
        detail["grams_before"],
        json!({"kind": "known", "grams": 512})
    );
    assert_eq!(detail["grams_now"], json!({"kind": "known", "grams": 492}));
    http.refuse_legacy_paths().await;
    assert_eq!(http.deductions().await, json!([unknown]));
    assert_one_put(&fake, 20.0);
    let applied = http.reconcile(id, "deducted").await;
    assert_eq!(applied["id"], id);
    assert_eq!(applied["state"], "applied");

    let undone = http
        .post(&format!("{BATCH}/undo"), json!({"undo_token": undo_token}))
        .await;
    assert!(undone["undone_at"].is_string());
    assert_eq!(
        http.burst_status(V2_BATCH, "transition-first").await["state"],
        "undone"
    );
    let reverified = http.post(BATCH, burst("transition-reverified")).await;
    assert_eq!(reverified["units_confirmed"], 2);
    let undone_again = http
        .post(
            &format!("{V2_BATCH}/undo"),
            json!({"undo_token": reverified["undo_token"]}),
        )
        .await;
    assert!(undone_again["undone_at"].is_string());
    assert_eq!(
        http.burst_status(BATCH, "transition-reverified").await["state"],
        "undone"
    );
    assert_eq!(http.deductions().await, json!([applied]));
    stop(runtime).await;

    let (runtime, http) = Launcher::open(&fixture.path).await;
    assert_eq!(http.deductions().await, json!([applied]));
    let (status, refused) = http
        .call(
            Method::POST,
            &reconcile_path(id),
            Some(json!({"answer": "retry"})),
        )
        .await;
    assert_eq!(status, StatusCode::CONFLICT, "{refused}");
    assert_eq!(refused["code"], "deduction_state");
    stop(runtime).await;
    assert_one_put(&fake, 20.0);
    assert!((fake.remaining(7) - 492.4).abs() < 1e-6);
}

async fn explicit_retry() {
    let fake = Fake::start(Mode::RefuseThenApply).await;
    let fixture = seed(&fake);
    let (runtime, http) = Launcher::open(&fixture.path).await;
    http.post(BATCH, burst("transition-refused")).await;
    let failed = http.wait_for("failed").await;
    assert_eq!(failed["failure"], "refused");
    assert_eq!(failed["failure_status"], 422);
    assert_one_put(&fake, 20.0);
    assert_eq!(fake.remaining(7), 512.4);
    stop(runtime).await;

    let (runtime, http) = Launcher::open(&fixture.path).await;
    assert_eq!(http.deductions().await, json!([failed]));
    assert_one_put(&fake, 20.0);
    let id = failed["id"].as_i64().unwrap();
    let pending = http.reconcile(id, "retry").await;
    assert_eq!(pending["state"], "pending");
    assert_eq!(pending["id"], id);
    let done = http.wait_for("applied").await;
    assert_eq!(done["id"], id);
    assert_eq!(http.detail(id).await["deduction"], done);
    stop(runtime).await;
    let puts = fake.puts();
    assert_eq!(puts.len(), 2);
    assert_eq!(puts[1].path, puts[0].path);
    assert_eq!(puts[1].body, puts[0].body);
    assert!((fake.remaining(7) - 492.4).abs() < 1e-6);
}

fn burst(key: &str) -> Value {
    json!({"idempotency_key": key,
        "beds": [{"link_id": "link-verify", "decisions": [confirm(1, 1), confirm(1, 2)]}]
    })
}

fn seed(fake: &Fake) -> Fixture {
    let (fixture, writer) = prepared(&fake.origin);
    writer.shutdown().unwrap();
    fixture
}

async fn stop(runtime: CoreRuntime) {
    let receipt = timeout(WAIT, runtime.shutdown()).await.unwrap();
    assert!(receipt.complete(), "{receipt:?}");
}

fn reconcile_path(id: i64) -> String {
    format!("{DEDUCTIONS}/{id}/reconcile")
}

struct Launcher {
    client: Client,
    origin: String,
}

impl Launcher {
    async fn open(data: &Path) -> (CoreRuntime, Self) {
        let mut input = super::release::launch(data.to_owned());
        input.bundle.node = data.join("no-node-executable");
        let mut runtime = CoreRuntime::start_for_launcher(input)
            .await
            .expect("Rust launcher did not start");
        assert!(runtime.rust_only(), "the proof requires the Rust owner");
        assert!(runtime.handle().compat_status().is_none());
        let origin = runtime.origin().to_owned();
        let bootstrap = loopback_client()
            .build()
            .unwrap()
            .get(runtime.take_launch_target().unwrap().into_url())
            .send()
            .await
            .unwrap();
        assert_eq!(bootstrap.status(), StatusCode::SEE_OTHER);
        let desktop = bootstrap.headers()["set-cookie"]
            .to_str()
            .unwrap()
            .split(';')
            .next()
            .unwrap();
        let cookie = format!("{desktop}; pp_session={SESSION}");
        let headers = HeaderMap::from_iter([
            (ORIGIN, origin.parse().unwrap()),
            (COOKIE, cookie.parse().unwrap()),
        ]);
        let client = loopback_client().default_headers(headers).build().unwrap();
        (runtime, Self { client, origin })
    }

    async fn call(&self, method: Method, path: &str, body: Option<Value>) -> (StatusCode, Value) {
        let mut request = self
            .client
            .request(method, format!("{}{path}", self.origin));
        if let Some(body) = body {
            request = request.json(&body);
        }
        let reply = request.send().await.unwrap();
        (reply.status(), reply.json().await.unwrap_or(Value::Null))
    }

    async fn ok(&self, method: Method, path: &str, body: Option<Value>) -> Value {
        let (status, reply) = self.call(method, path, body).await;
        assert_eq!(status, StatusCode::OK, "{path}: {reply}");
        reply
    }

    async fn post(&self, path: &str, body: Value) -> Value {
        self.ok(Method::POST, path, Some(body)).await
    }

    async fn reconcile(&self, id: i64, answer: &str) -> Value {
        self.post(&reconcile_path(id), json!({"answer": answer}))
            .await
    }

    async fn deductions(&self) -> Value {
        self.ok(Method::GET, DEDUCTIONS, None).await["deductions"].clone()
    }

    async fn detail(&self, id: i64) -> Value {
        self.ok(Method::GET, &format!("{DEDUCTIONS}/{id}"), None)
            .await
    }

    async fn burst_status(&self, path: &str, key: &str) -> Value {
        self.ok(Method::GET, &format!("{path}?idempotency_key={key}"), None)
            .await
    }

    async fn wait_for(&self, state: &str) -> Value {
        timeout(WAIT, async {
            loop {
                let rows = self.deductions().await;
                let rows = rows.as_array().unwrap();
                assert_eq!(rows.len(), 1, "{rows:?}");
                if rows[0]["state"] == state {
                    break rows[0].clone();
                }
                sleep(Duration::from_millis(25)).await;
            }
        })
        .await
        .unwrap_or_else(|_| panic!("the launched worker did not settle at {state}"))
    }

    async fn refuse_legacy_paths(&self) {
        let single = json!({"link_id": "link-verify", "decisions": [confirm(1, 1)]});
        for prefix in ["", "/api/v1", "/api/v2"] {
            let path = format!("{prefix}/printer-checkoff/verify");
            self.unmounted(&path, single.clone()).await;
        }
        self.unmounted(&format!("/api/v1{BATCH}"), burst("transition-legacy"))
            .await;
        self.unmounted(
            &format!("/api/v1{BATCH}/undo"),
            json!({"undo_token": "unused"}),
        )
        .await;
    }

    async fn unmounted(&self, path: &str, body: Value) {
        let (status, reply) = self.call(Method::POST, path, Some(body)).await;
        assert!(
            matches!(
                status,
                StatusCode::NOT_FOUND | StatusCode::METHOD_NOT_ALLOWED
            ),
            "{path}: expected no handler, got {status}: {reply}"
        );
    }
}

fn loopback_client() -> reqwest::ClientBuilder {
    Client::builder()
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(WAIT)
}
