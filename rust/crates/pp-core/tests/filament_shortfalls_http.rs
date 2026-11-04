//! Spoolman shortfalls over the mounted Accepted router. Inventory comes from
//! a loopback Spoolman stand-in serving `fixtures/integrations/spoolman`; no
//! test contacts a real Spoolman. Storage is the Node `accepted-plan-node.db`
//! fixture in a scratch directory.
#![cfg(feature = "desktop_rebuild")]

use axum::{
    extract::{Request, State},
    http::{StatusCode, header},
    response::{IntoResponse, Response},
};
use pp_api::accepted::{AcceptedHttpConfig, accepted_router};
use pp_core::spoolman::SpoolmanInventory;
use pp_storage::{Limits, WriterOwner};
use serde_json::{Value, json};
use std::sync::{Arc, Mutex};

#[path = "../../pp-storage/tests/common/scratch.rs"]
mod scratch;

const SESSION: &str = "pp_session=read-fixture-secret";
const FILAMENTS: &str = include_str!("../../../../fixtures/integrations/spoolman/filaments.json");
const SUFFICIENT: &str =
    include_str!("../../../../fixtures/integrations/spoolman/spools-sufficient.json");
const SHORT: &str = include_str!("../../../../fixtures/integrations/spoolman/spools-short.json");
/// A checkoff as Node records it today: a `sent` job with no measured grams.
const NODE_OUTCOMES: &str =
    include_str!("../../../../fixtures/integrations/spoolman/print-outcomes-node.sql");
/// The included Part (ID 2, one remaining unit) prints filament 2 of
/// integration `local`; one completed job measured 42 g for it.
const SETUP: &str = "
    UPDATE parts SET filament_color_id=' spoolman:local:filament:2 ' WHERE id=2;
    INSERT INTO app_settings(tenant_id,key,value)
      VALUES('default','feature_toggles','{\"spoolman\":true}');
    INSERT INTO print_jobs(id,tenant_id,profile_id,at,status,filament_consumed_g)
      VALUES('job-1','default',1,'2026-10-03T00:00:00.000Z','completed',42);
    INSERT INTO print_job_parts(id,tenant_id,job_id,at,profile_id,part_id,result)
      VALUES('job-1-0','default','job-1','2026-10-03T00:00:00.000Z',1,2,'confirmed');";

/// The Spoolman stand-in: fixed bodies, and a `(method, path, authorization)`
/// log of every request.
#[derive(Default)]
struct Stub {
    spools: &'static str,
    fail: bool,
    log: Vec<(String, String, Option<String>)>,
}

async fn spoolman(State(stub): State<Arc<Mutex<Stub>>>, request: Request) -> Response {
    let mut stub = stub.lock().unwrap();
    let auth = request.headers().get(header::AUTHORIZATION);
    let auth = auth.map(|value| value.to_str().unwrap().to_owned());
    let path = request.uri().path().to_owned();
    stub.log
        .push((request.method().to_string(), path.clone(), auth));
    let body = match (stub.fail, request.method().as_str(), path.as_str()) {
        (true, ..) => return StatusCode::INTERNAL_SERVER_ERROR.into_response(),
        (_, "GET", "/api/v1/filament") => FILAMENTS,
        (_, "GET", "/api/v1/spool") => stub.spools,
        _ => return StatusCode::NOT_FOUND.into_response(),
    };
    ([(header::CONTENT_TYPE, "application/json")], body).into_response()
}

/// Serves the router `build` makes for the listener's origin.
async fn serve(build: impl FnOnce(&str) -> axum::Router) -> (String, tokio::task::JoinHandle<()>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let origin = format!("http://{}", listener.local_addr().unwrap());
    let service = build(&origin).into_make_service_with_connect_info::<std::net::SocketAddr>();
    let task = tokio::spawn(async move { axum::serve(listener, service).await.unwrap() });
    (origin, task)
}

struct Harness {
    owner: Option<WriterOwner>,
    origin: String,
    tasks: [tokio::task::JoinHandle<()>; 2],
    stub: Arc<Mutex<Stub>>,
    // Dropped last so storage closes before the directory is removed.
    directory: scratch::Scratch,
}

impl Harness {
    /// [`SETUP`], the integration pointing at the stand-in, then `sql`.
    async fn new(spools: &'static str, sql: &str) -> Self {
        let stub = Arc::new(Mutex::new(Stub {
            spools,
            ..Stub::default()
        }));
        let state = stub.clone();
        let (stub_origin, stub_task) =
            serve(|_| axum::Router::new().fallback(spoolman).with_state(state)).await;
        let directory = scratch::Scratch::new("pp-shortfalls-");
        std::fs::create_dir_all(directory.join("repos/1/revisions/accepted")).unwrap();
        let database = directory.join("print-partner.db");
        let fixture = include_bytes!("../../pp-storage/tests/fixtures/accepted-plan-node.db");
        std::fs::write(&database, fixture).unwrap();
        let db = rusqlite::Connection::open(&database).unwrap();
        db.execute_batch(SETUP).unwrap();
        let integrations = json!([{"id": "local", "type": "spoolman", "name": "Workshop",
            "config": {"base_url": stub_origin, "api_key": "disposable-key"}}]);
        db.execute(
            "INSERT INTO app_settings(tenant_id,key,value) VALUES('default','integrations',?1)",
            [integrations.to_string()],
        )
        .unwrap();
        db.execute_batch(sql).unwrap();
        drop(db);
        let owner = WriterOwner::open(&directory, Limits::default()).unwrap().0;
        let reads = owner.accepted_reads().unwrap();
        let (origin, task) = serve(|origin| {
            let config = AcceptedHttpConfig::from_startup(origin, "default", Some("1")).unwrap();
            let inventory = Arc::new(SpoolmanInventory::new().unwrap());
            accepted_router(config.with_stock(inventory), reads)
        })
        .await;
        Self {
            owner: Some(owner),
            origin,
            tasks: [stub_task, task],
            stub,
            directory,
        }
    }

    async fn get(&self, path: &str, headers: &[(&str, &str)]) -> (u16, Value) {
        let client = reqwest::Client::builder().no_proxy().build().unwrap();
        let mut request = client.get(format!("{}{path}", self.origin));
        for (name, value) in headers {
            request = request.header(*name, *value);
        }
        let response = request.send().await.unwrap();
        let status = response.status().as_u16();
        let body = serde_json::from_slice(&response.bytes().await.unwrap()).unwrap();
        (status, pp_api::problem::without_envelope(body))
    }

    async fn shortfalls(&self) -> Value {
        let path = "/api/v2/plans/1/filament-shortfalls";
        let (status, body) = self.get(path, &[("Cookie", SESSION)]).await;
        assert_eq!(status, 200, "{body}");
        body
    }

    fn requests(&self) -> Vec<(String, String, Option<String>)> {
        self.stub.lock().unwrap().log.clone()
    }

    /// Every row a stock read could touch.
    fn dump(&self) -> Vec<String> {
        let db = rusqlite::Connection::open(self.directory.join("print-partner.db")).unwrap();
        let mut rows = Vec::new();
        for table in ["print_jobs", "print_job_parts", "app_settings", "parts"] {
            let mut statement = db.prepare(&format!("SELECT * FROM {table}")).unwrap();
            let width = statement.column_count();
            let mut query = statement.query([]).unwrap();
            while let Some(row) = query.next().unwrap() {
                let cells = (0..width).map(|i| format!("{:?}", row.get_ref(i).unwrap()));
                rows.push(format!("{table}: {}", cells.collect::<Vec<_>>().join("|")));
            }
        }
        rows
    }

    fn stop(mut self) {
        self.tasks.iter().for_each(|task| task.abort());
        self.owner.take().unwrap().shutdown().unwrap();
    }
}

/// The expected single filament row inside a Ready body for Plan 1.
fn ready(body: &Value, available: Value, status: Value, needed: Value) {
    let label = match available["reason"] == "unreachable" {
        true => "",
        false => "Prusament PLA · Galaxy Black",
    };
    let part = json!({"part_id": 2, "filename": "included.stl", "remaining_units": 1,
        "grams_needed": needed});
    let row = json!({"filament_color_id": "spoolman:local:filament:2", "label": label,
        "grams_needed": needed, "grams_available": available, "status": status, "parts": [part]});
    let version = body["plan_version"].as_i64().unwrap();
    assert!(version >= 1);
    let expected = json!({"kind": "ready", "profile_id": 1, "revision_id": 1,
        "plan_version": version, "filaments": [row]});
    assert_eq!(body, &expected);
}

fn measured() -> Value {
    json!({"kind": "measured", "grams": 42, "job_count": 1})
}

fn not_recorded() -> Value {
    json!({"kind": "unknown", "reason": "grams_not_recorded"})
}

#[tokio::test]
async fn sufficient_and_short_report_exact_filament_and_grams() {
    let sufficient = Harness::new(SUFFICIENT, "").await;
    let stock = json!({"kind": "spoolman", "grams": 752, "spool_ids": [7, 8]});
    let verdict = json!({"kind": "sufficient"});
    ready(&sufficient.shortfalls().await, stock, verdict, measured());
    sufficient.stop();

    let short = Harness::new(SHORT, "").await;
    let stock = json!({"kind": "spoolman", "grams": 18, "spool_ids": [7]});
    let verdict = json!({"kind": "short", "short_grams": 24});
    ready(&short.shortfalls().await, stock, verdict, measured());
    short.stop();
}

#[tokio::test]
async fn unavailable_inventory_and_unprinted_parts_stay_unknown() {
    let unknown = json!({"kind": "unknown"});
    let unreachable = json!({"kind": "unknown", "reason": "unreachable"});
    let failing = Harness::new(SUFFICIENT, "").await;
    failing.stub.lock().unwrap().fail = true;
    let body = failing.shortfalls().await;
    ready(&body, unreachable.clone(), unknown.clone(), measured());
    failing.stop();

    // Nothing listens on the discard port.
    let down = Harness::new(
        SUFFICIENT,
        "UPDATE app_settings SET value=json_set(value,'$[0].config.base_url','http://127.0.0.1:9')
          WHERE key='integrations'",
    )
    .await;
    let body = down.shortfalls().await;
    ready(&body, unreachable, unknown.clone(), measured());
    down.stop();

    let unprinted = Harness::new(SUFFICIENT, "DELETE FROM print_jobs;").await;
    let stock = json!({"kind": "spoolman", "grams": 752, "spool_ids": [7, 8]});
    ready(
        &unprinted.shortfalls().await,
        stock,
        unknown,
        not_recorded(),
    );
    unprinted.stop();
}

/// No code in Node or Rust records measured grams yet, so a real library
/// reads `grams_not_recorded`. The same Node-shaped rows, once completed
/// with grams, take the measured path.
#[tokio::test]
async fn node_shaped_outcomes_report_grams_not_recorded() {
    let stock = json!({"kind": "spoolman", "grams": 752, "spool_ids": [7, 8]});
    let clear = "DELETE FROM print_job_parts; DELETE FROM print_jobs;";
    let today = Harness::new(SUFFICIENT, &format!("{clear}{NODE_OUTCOMES}")).await;
    let unknown = json!({"kind": "unknown"});
    ready(
        &today.shortfalls().await,
        stock.clone(),
        unknown,
        not_recorded(),
    );
    today.stop();

    let complete = "UPDATE print_jobs SET status='completed', filament_consumed_g=84,
        completed_at='2026-10-03T01:00:00.000Z';";
    let sql = format!("{clear}{NODE_OUTCOMES}{complete}");
    let recorded = Harness::new(SUFFICIENT, &sql).await;
    let sufficient = json!({"kind": "sufficient"});
    ready(&recorded.shortfalls().await, stock, sufficient, measured());
    recorded.stop();
}

#[tokio::test]
async fn module_off_returns_the_core_reason_without_contacting_spoolman() {
    for (value, reason) in [
        (None, "default_off"),
        (Some(r#"{"spoolman":false}"#), "turned_off"),
        (Some("not json"), "setting_unreadable"),
    ] {
        let sql = match value {
            None => "DELETE FROM app_settings WHERE key='feature_toggles'".to_owned(),
            Some(value) => {
                format!("UPDATE app_settings SET value='{value}' WHERE key='feature_toggles'")
            }
        };
        let harness = Harness::new(SUFFICIENT, &sql).await;
        let off = json!({"kind": "module_off", "module": "spoolman", "reason": reason});
        assert_eq!(harness.shortfalls().await, off);
        assert!(harness.requests().is_empty(), "{reason}");
        harness.stop();
    }
}

#[tokio::test]
async fn repeated_reads_only_get_inventory_and_write_nothing() {
    let harness = Harness::new(SUFFICIENT, "").await;
    let before = harness.dump();
    let first = harness.shortfalls().await;
    for alias in ["", "/api/v1", "/api/v2"] {
        let path = format!("{alias}/plans/1/filament-shortfalls");
        let read = harness.get(&path, &[("Cookie", SESSION)]).await;
        assert_eq!(read, (200, first.clone()), "{path}");
    }
    let requests = harness.requests();
    assert_eq!(requests.len(), 8);
    for (index, (method, path, auth)) in requests.iter().enumerate() {
        let resource = ["filament", "spool"][index % 2];
        let expected = (
            "GET",
            format!("/api/v1/{resource}"),
            Some("Bearer disposable-key"),
        );
        assert_eq!((method.as_str(), path.clone(), auth.as_deref()), expected);
    }
    assert_eq!(before, harness.dump(), "shortfall reads write nothing");
    harness.stop();
}

#[tokio::test]
async fn shortfalls_require_a_credential_and_a_visible_plan() {
    let harness = Harness::new(SUFFICIENT, "").await;
    let refused = (401, json!({"detail": "Authentication required"}));
    let wrong: [&[(&str, &str)]; 3] = [
        &[],
        &[("Cookie", "pp_session=wrong")],
        &[("Authorization", "Bearer wrong")],
    ];
    for headers in wrong {
        for path in ["/plans/1", "/api/v2/plans/abc"] {
            let path = format!("{path}/filament-shortfalls");
            assert_eq!(harness.get(&path, headers).await, refused, "{path}");
        }
    }
    let missing = (404, json!({"detail": "Profile not found"}));
    for path in ["/api/v2/plans/999", "/api/v2/plans/abc"] {
        let path = format!("{path}/filament-shortfalls");
        assert_eq!(
            harness.get(&path, &[("Cookie", SESSION)]).await,
            missing,
            "{path}"
        );
    }
    assert!(harness.requests().is_empty());
    harness.stop();
}
