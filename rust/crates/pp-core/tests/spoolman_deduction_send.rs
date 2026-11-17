//! GRE-1738: the deduction worker sends each pending row once, against a
//! loopback Spoolman. GRE-1749: reconcile answers an unknown or failed row,
//! and a read reports the grams from before the send and now. GRE-1750: an
//! integration change during the spool GET sends nothing to the old config.
//! Nothing here contacts a real Spoolman or a printer.
#![cfg(feature = "desktop_rebuild")]

use pp_api::accepted::{AcceptedHttpConfig, accepted_router};
use pp_core::{
    deduction_worker::DeductionWorker, rebuild_api::desktop_policy, spoolman::SpoolmanInventory,
};
use pp_storage::{
    Limits, WriterOwner,
    auth::{self, Secret},
    checkoff_progress::Request,
    read_model::Credential,
    spoolman_deductions::{Deduction, Failure, Filter, State},
};
use serde_json::{Value, json};
use std::{
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
    time::Duration,
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpListener,
    sync::oneshot,
};

#[path = "../../pp-storage/tests/common/checkoff_apply.rs"]
mod checkoff_apply;
#[path = "../../pp-storage/tests/common/checkoff_fixture.rs"]
mod checkoff_fixture;
#[path = "../../pp-storage/tests/common/js_json.rs"]
mod js_json;
#[path = "../../pp-storage/tests/common/scratch.rs"]
mod scratch;

use checkoff_apply::apply;
use checkoff_fixture::Fixture;

const FILAMENTS: &str = include_str!("../../../../fixtures/integrations/spoolman/filaments.json");
const SUFFICIENT: &str =
    include_str!("../../../../fixtures/integrations/spoolman/spools-sufficient.json");
const SESSION: &str = "required-unit-fixture-secret";

#[derive(Clone, Copy)]
enum Mode {
    /// 200, and subtract `use_weight` from the spool.
    Apply,
    /// 422, and leave stock as it is.
    Refuse,
    /// Subtract `use_weight`, then close with no reply.
    Drop,
    /// The first PUT is 422. Later PUTs apply.
    RefuseThenApply,
}

struct Hit {
    method: String,
    path: String,
    body: Option<Value>,
}

/// Holds the next single-spool GET: `arrived` fires when it comes in, and
/// the reply waits for `release`.
struct Gate {
    arrived: oneshot::Sender<()>,
    release: oneshot::Receiver<()>,
}

struct Fake {
    origin: String,
    stock: Arc<Mutex<Vec<Value>>>,
    log: Arc<Mutex<Vec<Hit>>>,
    gate: Arc<Mutex<Option<Gate>>>,
    task: tokio::task::JoinHandle<()>,
}

impl Fake {
    async fn start(mode: Mode) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let origin = format!("http://{}", listener.local_addr().unwrap());
        let stock = Arc::new(Mutex::new(
            serde_json::from_str::<Vec<Value>>(SUFFICIENT).unwrap(),
        ));
        let log = Arc::new(Mutex::new(Vec::new()));
        let gate = Arc::new(Mutex::new(None));
        let stock_task = Arc::clone(&stock);
        let log_task = Arc::clone(&log);
        let gate_task = Arc::clone(&gate);
        let task = tokio::spawn(async move {
            loop {
                let Ok((socket, _)) = listener.accept().await else {
                    break;
                };
                let stock = Arc::clone(&stock_task);
                let log = Arc::clone(&log_task);
                let gate = Arc::clone(&gate_task);
                tokio::spawn(async move {
                    let _ = serve_one(socket, mode, stock, log, gate).await;
                });
            }
        });
        Self {
            origin,
            stock,
            log,
            gate,
            task,
        }
    }

    /// Holds the next `GET /api/v1/spool/{id}`. Returns the receiver that
    /// fires when it arrives and the sender that lets it reply.
    fn hold_next_spool_get(&self) -> (oneshot::Receiver<()>, oneshot::Sender<()>) {
        let (arrived, on_arrival) = oneshot::channel();
        let (release, on_release) = oneshot::channel();
        *self.gate.lock().unwrap() = Some(Gate {
            arrived,
            release: on_release,
        });
        (on_arrival, release)
    }

    fn puts(&self) -> Vec<Hit> {
        self.log
            .lock()
            .unwrap()
            .iter()
            .filter(|hit| hit.method == "PUT")
            .map(|hit| Hit {
                method: hit.method.clone(),
                path: hit.path.clone(),
                body: hit.body.clone(),
            })
            .collect()
    }

    fn remaining(&self, spool: i64) -> f64 {
        self.stock
            .lock()
            .unwrap()
            .iter()
            .find(|row| row["id"] == spool)
            .unwrap()["remaining_weight"]
            .as_f64()
            .unwrap()
    }
}

impl Drop for Fake {
    fn drop(&mut self) {
        self.task.abort();
    }
}

async fn serve_one(
    mut socket: tokio::net::TcpStream,
    mode: Mode,
    stock: Arc<Mutex<Vec<Value>>>,
    log: Arc<Mutex<Vec<Hit>>>,
    gate: Arc<Mutex<Option<Gate>>>,
) -> std::io::Result<()> {
    let request = read_request(&mut socket).await?;
    let (method, path, body) = match parse_request(&request) {
        Some(parsed) => parsed,
        None => return Ok(()),
    };
    log.lock().unwrap().push(Hit {
        method: method.clone(),
        path: path.clone(),
        body: body.clone(),
    });
    if method == "PUT" {
        match mode {
            Mode::Apply => {
                apply_use(&stock, &path, &body);
                let reply = stock_body(&stock, &path);
                return write_response(&mut socket, 200, "OK", &reply).await;
            }
            Mode::Refuse => {
                return write_response(&mut socket, 422, "Unprocessable Entity", "{}").await;
            }
            Mode::Drop => {
                apply_use(&stock, &path, &body);
                return Ok(());
            }
            Mode::RefuseThenApply => {
                let first = log
                    .lock()
                    .unwrap()
                    .iter()
                    .filter(|hit| hit.method == "PUT")
                    .count()
                    == 1;
                if first {
                    return write_response(&mut socket, 422, "Unprocessable Entity", "{}").await;
                }
                apply_use(&stock, &path, &body);
                let reply = stock_body(&stock, &path);
                return write_response(&mut socket, 200, "OK", &reply).await;
            }
        }
    }
    if path.starts_with("/api/v1/spool/") {
        let held = gate.lock().unwrap().take();
        if let Some(held) = held {
            let _ = held.arrived.send(());
            let _ = held.release.await;
        }
    }
    let reply = match path.as_str() {
        "/api/v1/filament" => Some(FILAMENTS.to_owned()),
        "/api/v1/spool" => Some(serde_json::to_string(&*stock.lock().unwrap()).unwrap()),
        other if other.starts_with("/api/v1/spool/") && !other.ends_with("/use") => {
            Some(stock_body(&stock, other))
        }
        _ => None,
    };
    match reply {
        Some(body) => write_response(&mut socket, 200, "OK", &body).await,
        None => write_response(&mut socket, 404, "Not Found", "{}").await,
    }
}

fn apply_use(stock: &Mutex<Vec<Value>>, path: &str, body: &Option<Value>) {
    let Some(body) = body else { return };
    let Some(grams) = body.get("use_weight").and_then(Value::as_f64) else {
        return;
    };
    let Some(id) = spool_id(path) else { return };
    let mut stock = stock.lock().unwrap();
    if let Some(row) = stock.iter_mut().find(|row| row["id"].as_i64() == Some(id)) {
        let remaining = row["remaining_weight"].as_f64().unwrap_or(0.0);
        row["remaining_weight"] = json!(remaining - grams);
    }
}

fn spool_id(path: &str) -> Option<i64> {
    path.trim_start_matches("/api/v1/spool/")
        .trim_end_matches("/use")
        .parse()
        .ok()
}

fn stock_body(stock: &Mutex<Vec<Value>>, path: &str) -> String {
    let stock = stock.lock().unwrap();
    stock
        .iter()
        .find(|row| row["id"].as_i64() == spool_id(path))
        .map(|row| row.to_string())
        .unwrap_or_else(|| "{}".into())
}

async fn read_request(socket: &mut tokio::net::TcpStream) -> std::io::Result<Vec<u8>> {
    let mut buf = Vec::new();
    let mut tmp = [0u8; 2048];
    loop {
        let read = socket.read(&mut tmp).await?;
        if read == 0 {
            break;
        }
        buf.extend_from_slice(&tmp[..read]);
        if let Some(end) = buf.windows(4).position(|mark| mark == b"\r\n\r\n") {
            let length = content_length(&buf[..end]);
            if buf.len() >= end + 4 + length {
                break;
            }
        }
        if buf.len() > 64 * 1024 {
            break;
        }
    }
    Ok(buf)
}

fn content_length(headers: &[u8]) -> usize {
    let text = String::from_utf8_lossy(headers);
    text.lines()
        .find_map(|line| {
            let (name, value) = line.split_once(':')?;
            name.eq_ignore_ascii_case("content-length")
                .then(|| value.trim().parse().ok())
                .flatten()
        })
        .unwrap_or(0)
}

fn parse_request(bytes: &[u8]) -> Option<(String, String, Option<Value>)> {
    let text = std::str::from_utf8(bytes).ok()?;
    let (head, body) = text.split_once("\r\n\r\n")?;
    let mut lines = head.lines();
    let mut request = lines.next()?.split_whitespace();
    let method = request.next()?.to_owned();
    let path = request.next()?.to_owned();
    let body = if body.is_empty() {
        None
    } else {
        serde_json::from_str(body).ok()
    };
    Some((method, path, body))
}

async fn write_response(
    socket: &mut tokio::net::TcpStream,
    status: u16,
    reason: &str,
    body: &str,
) -> std::io::Result<()> {
    let response = format!(
        "HTTP/1.1 {status} {reason}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    socket.write_all(response.as_bytes()).await?;
    socket.shutdown().await
}

fn integration(origin: &str, id: &str) -> String {
    integration_with(origin, id, true)
}

fn integration_with(origin: &str, id: &str, enabled: bool) -> String {
    json!([{
        "id": id,
        "type": "spoolman",
        "name": "Spoolman",
        "config": {"base_url": origin, "api_key": "disposable-key", "enabled": enabled},
        "created_at": "2026-11-01T00:00:00.000Z",
        "updated_at": "2026-11-01T00:00:00.000Z"
    }])
    .to_string()
}

fn put(fixture: &Fixture, key: &str, value: &str) {
    fixture
        .sql()
        .execute(
            "REPLACE INTO app_settings (tenant_id, key, value) VALUES ('default', ?1, ?2)",
            [key, value],
        )
        .unwrap();
}

/// Module on, `sm-1` at `origin`, Part 1 pinned to spool 7, 40 g over 4 units.
fn prepared(origin: &str) -> (Fixture, WriterOwner) {
    let fixture = Fixture::new();
    let owner = fixture.open();
    put(&fixture, "feature_toggles", r#"{"spoolman":true}"#);
    put(&fixture, "integrations", &integration(origin, "sm-1"));
    fixture
        .sql()
        .execute_batch(
            "UPDATE parts SET spoolman_spool_id = 'spoolman:sm-1:spool:7' WHERE id = 1;
             INSERT INTO print_link_grams (tenant_id, link_id, grams, units, basis, updated_at)
             VALUES ('default', 'link-verify', 40, 4, 'estimated', '2026-11-12T00:00:00.000Z');",
        )
        .unwrap();
    (fixture, owner)
}

fn confirm(part: i64, unit: i64) -> Value {
    json!({ "part_id": part, "unit_index": unit, "result": "confirmed" })
}

fn burst(owner: &WriterOwner, key: &str) -> Value {
    let body = json!({
        "idempotency_key": key,
        "beds": [{ "link_id": "link-verify", "decisions": [confirm(1, 1), confirm(1, 2)] }]
    });
    let response = apply(owner, Request::VerifyBatch { body });
    assert_eq!(response["status"], 200, "{response}");
    response
}

fn rows(owner: &WriterOwner) -> Vec<Deduction> {
    let policy = auth::AuthPolicy {
        registration: auth::RegistrationPolicy::Open,
        first_user: auth::FirstUserTenant::NewUser,
        session_tenant: auth::SessionTenantPolicy::AccountTenant,
    };
    owner
        .spoolman_deductions(policy)
        .unwrap()
        .list(
            Credential::Session(Secret::new(SESSION.into())),
            Filter::default(),
            &AtomicBool::new(false),
            Duration::from_secs(5),
        )
        .unwrap()
}

async fn send_once(owner: &WriterOwner) -> bool {
    let worker =
        DeductionWorker::new(owner, desktop_policy(), SpoolmanInventory::new().unwrap()).unwrap();
    worker.run_next().await.unwrap()
}

fn assert_one_put(fake: &Fake, grams: f64) {
    let puts = fake.puts();
    assert_eq!(puts.len(), 1, "puts {}", puts.len());
    assert_eq!(puts[0].path, "/api/v1/spool/7/use");
    let sent = puts[0].body.as_ref().unwrap()["use_weight"]
        .as_f64()
        .unwrap();
    assert!((sent - grams).abs() < 1e-6, "{sent}");
}

#[test]
fn a_committed_verify_burst_wakes_the_worker_and_undo_does_not() {
    let (_fixture, owner) = prepared("http://127.0.0.1:9");
    let hits = Arc::new(AtomicUsize::new(0));
    let counted = Arc::clone(&hits);
    owner
        .spoolman_deductions(desktop_policy())
        .unwrap()
        .on_burst(Arc::new(move || {
            counted.fetch_add(1, Ordering::SeqCst);
        }));
    let first = burst(&owner, "k1");
    assert_eq!(hits.load(Ordering::SeqCst), 1);
    let undone = apply(
        &owner,
        Request::UndoBatch {
            body: json!({ "undo_token": first["body"]["undo_token"] }),
        },
    );
    assert_eq!(undone["status"], 200, "{undone}");
    assert_eq!(hits.load(Ordering::SeqCst), 1);
    owner.shutdown().unwrap();
}

#[tokio::test]
async fn a_burst_wakes_the_worker_and_one_put_applies_twenty_grams() {
    let fake = Fake::start(Mode::Apply).await;
    let (_fixture, owner) = prepared(&fake.origin);
    let worker =
        DeductionWorker::new(&owner, desktop_policy(), SpoolmanInventory::new().unwrap()).unwrap();
    let stop = Arc::new(AtomicBool::new(false));
    let wake = worker.waker();
    let task = worker.spawn(Arc::clone(&stop));
    tokio::time::sleep(Duration::from_millis(200)).await;
    burst(&owner, "k1");
    let start = std::time::Instant::now();
    loop {
        let applied = rows(&owner).iter().any(|row| row.state == State::Applied);
        if applied || start.elapsed() > Duration::from_secs(3) {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    stop.store(true, Ordering::Release);
    wake.notify_one();
    task.await.unwrap();
    let found = rows(&owner);
    assert_eq!(found.len(), 1);
    assert_eq!(found[0].state, State::Applied);
    assert_eq!(found[0].target.as_ref().unwrap().grams, 20.0);
    assert_eq!(found[0].spool_grams_before, Some(512.4));
    assert_one_put(&fake, 20.0);
    assert!((fake.remaining(7) - 492.4).abs() < 1e-6);
    owner.shutdown().unwrap();
}

#[tokio::test]
async fn a_dropped_reply_is_unknown_and_a_restart_does_not_send_again() {
    let fake = Fake::start(Mode::Drop).await;
    let (fixture, owner) = prepared(&fake.origin);
    burst(&owner, "k1");
    assert!(send_once(&owner).await);
    let found = rows(&owner);
    assert_eq!(found[0].state, State::Unknown);
    assert_eq!(found[0].failure, Some(Failure::ReplyLost));
    assert_one_put(&fake, 20.0);
    owner.shutdown().unwrap();

    let owner = fixture.open();
    assert!(!send_once(&owner).await);
    assert_eq!(rows(&owner)[0].state, State::Unknown);
    assert_eq!(fake.puts().len(), 1);
    owner.shutdown().unwrap();
}

#[tokio::test]
async fn a_refused_put_and_a_closed_port_fail_without_changing_stock() {
    let fake = Fake::start(Mode::Refuse).await;
    let (_fixture, owner) = prepared(&fake.origin);
    burst(&owner, "k1");
    assert!(send_once(&owner).await);
    let found = rows(&owner);
    assert_eq!(found[0].state, State::Failed);
    assert_eq!(found[0].failure, Some(Failure::Refused));
    assert_eq!(found[0].failure_status, Some(422));
    assert_one_put(&fake, 20.0);
    assert!((fake.remaining(7) - 512.4).abs() < 1e-6);
    owner.shutdown().unwrap();

    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    drop(listener);
    let watching = Fake::start(Mode::Apply).await;
    let (_fixture, owner) = prepared(&format!("http://127.0.0.1:{port}"));
    burst(&owner, "k1");
    assert!(send_once(&owner).await);
    let found = rows(&owner);
    assert_eq!(found[0].state, State::Failed);
    assert_eq!(found[0].failure, Some(Failure::Unreachable));
    assert_eq!(found[0].failure_status, None);
    assert!(watching.puts().is_empty());
    assert!((watching.remaining(7) - 512.4).abs() < 1e-6);
    owner.shutdown().unwrap();
}

#[tokio::test]
async fn a_restart_while_sending_is_unknown_and_sends_nothing() {
    let fake = Fake::start(Mode::Apply).await;
    let (fixture, owner) = prepared(&fake.origin);
    fixture
        .sql()
        .execute(
            "INSERT INTO spoolman_deductions (
                tenant_id, burst, link_id, profile_id, state, units,
                integration_id, spool_id, grams, grams_basis, created_at, updated_at
             ) VALUES (
                'default', 'burst-sending', 'link-verify', 1, 'sending', 2,
                'sm-1', 7, 20, 'estimated', '2026-11-17T00:00:00.000Z', '2026-11-17T00:00:00.000Z'
             )",
            [],
        )
        .unwrap();
    owner.shutdown().unwrap();

    let owner = fixture.open();
    let found = rows(&owner);
    assert_eq!(found[0].state, State::Unknown);
    assert_eq!(found[0].failure, Some(Failure::Interrupted));
    assert!(!send_once(&owner).await);
    assert!(fake.puts().is_empty());
    assert_eq!(fake.log.lock().unwrap().len(), 0);
    owner.shutdown().unwrap();
}

#[tokio::test]
async fn undo_after_applied_then_confirming_again_adds_no_row() {
    let fake = Fake::start(Mode::Apply).await;
    let (_fixture, owner) = prepared(&fake.origin);
    let first = burst(&owner, "k1");
    assert!(send_once(&owner).await);
    assert_eq!(rows(&owner)[0].state, State::Applied);
    let undone = apply(
        &owner,
        Request::UndoBatch {
            body: json!({ "undo_token": first["body"]["undo_token"] }),
        },
    );
    assert_eq!(undone["status"], 200, "{undone}");
    burst(&owner, "k2");
    let found = rows(&owner);
    assert_eq!(found.len(), 1);
    assert_eq!(found[0].state, State::Applied);
    assert_eq!(fake.puts().len(), 1);
    owner.shutdown().unwrap();
}

#[tokio::test]
async fn module_off_leaves_the_row_pending_until_it_is_on() {
    let fake = Fake::start(Mode::Apply).await;
    let (fixture, owner) = prepared(&fake.origin);
    burst(&owner, "k1");
    put(&fixture, "feature_toggles", r#"{"spoolman":false}"#);
    assert!(!send_once(&owner).await);
    assert_eq!(rows(&owner)[0].state, State::Pending);
    assert!(fake.puts().is_empty());
    assert_eq!(fake.log.lock().unwrap().len(), 0);

    put(&fixture, "feature_toggles", r#"{"spoolman":true}"#);
    assert!(send_once(&owner).await);
    assert_eq!(rows(&owner)[0].state, State::Applied);
    assert_one_put(&fake, 20.0);
    owner.shutdown().unwrap();
}

/// Runs one worker turn, and commits `change` to the integrations setting
/// while the worker's spool GET to `fake` is held open.
async fn send_once_changing_integrations_during_get(
    fixture: &Fixture,
    owner: &WriterOwner,
    fake: &Fake,
    change: &str,
) -> bool {
    let (arrived, release) = fake.hold_next_spool_get();
    let worker =
        DeductionWorker::new(owner, desktop_policy(), SpoolmanInventory::new().unwrap()).unwrap();
    let turn = tokio::spawn(async move { worker.run_next().await.unwrap() });
    arrived.await.unwrap();
    put(fixture, "integrations", change);
    release.send(()).unwrap();
    turn.await.unwrap()
}

#[tokio::test]
async fn disabling_the_integration_during_the_spool_get_sends_nothing() {
    let fake = Fake::start(Mode::Apply).await;
    let (fixture, owner) = prepared(&fake.origin);
    burst(&owner, "k1");
    let disabled = integration_with(&fake.origin, "sm-1", false);
    assert!(send_once_changing_integrations_during_get(&fixture, &owner, &fake, &disabled).await);
    let found = rows(&owner);
    assert_eq!(found[0].state, State::Pending);
    assert_eq!(found[0].spool_grams_before, None);
    assert!(fake.puts().is_empty());

    assert!(send_once(&owner).await);
    let found = rows(&owner);
    assert_eq!(found[0].state, State::Failed);
    assert_eq!(found[0].failure, Some(Failure::IntegrationDisabled));
    assert!(fake.puts().is_empty());
    assert!((fake.remaining(7) - 512.4).abs() < 1e-6);
    owner.shutdown().unwrap();
}

#[tokio::test]
async fn moving_the_integration_during_the_spool_get_sends_only_to_the_new_endpoint() {
    let old = Fake::start(Mode::Apply).await;
    let new = Fake::start(Mode::Apply).await;
    let (fixture, owner) = prepared(&old.origin);
    burst(&owner, "k1");
    let moved = integration(&new.origin, "sm-1");
    assert!(send_once_changing_integrations_during_get(&fixture, &owner, &old, &moved).await);
    assert_eq!(rows(&owner)[0].state, State::Pending);
    assert!(old.puts().is_empty());
    assert!(new.log.lock().unwrap().is_empty());

    assert!(send_once(&owner).await);
    let found = rows(&owner);
    assert_eq!(found[0].state, State::Applied);
    assert_eq!(found[0].spool_grams_before, Some(512.4));
    assert!(old.puts().is_empty());
    assert!((old.remaining(7) - 512.4).abs() < 1e-6);
    assert_one_put(&new, 20.0);
    owner.shutdown().unwrap();
}

#[tokio::test]
async fn the_next_shortfalls_read_shows_the_lower_grams() {
    let fake = Fake::start(Mode::Apply).await;
    let directory = scratch::Scratch::new("pp-deduction-shortfalls-");
    std::fs::create_dir_all(directory.join("repos/1/revisions/accepted")).unwrap();
    let database = directory.join("print-partner.db");
    std::fs::write(
        &database,
        include_bytes!("../../pp-storage/tests/fixtures/accepted-plan-node.db"),
    )
    .unwrap();
    let db = rusqlite::Connection::open(&database).unwrap();
    db.execute_batch(
        "UPDATE parts SET filament_color_id=' spoolman:local:filament:2 ' WHERE id=2;
         INSERT INTO app_settings(tenant_id,key,value)
           VALUES('default','feature_toggles','{\"spoolman\":true}');
         INSERT INTO print_jobs(id,tenant_id,profile_id,at,status,filament_consumed_g,link_id)
           VALUES('job-1','default',1,'2026-10-03T00:00:00.000Z','sent',NULL,'link-a');
         INSERT INTO print_job_parts(id,tenant_id,job_id,at,profile_id,part_id,result,link_id)
           VALUES('job-1-0','default','job-1','2026-10-03T00:00:00.000Z',1,2,'confirmed','link-a');",
    )
    .unwrap();
    db.execute(
        "INSERT INTO app_settings(tenant_id,key,value) VALUES('default','integrations',?1)",
        [integration(&fake.origin, "local")],
    )
    .unwrap();
    drop(db);
    let owner = WriterOwner::open(&directory, Limits::default()).unwrap().0;
    let db = rusqlite::Connection::open(&database).unwrap();
    db.execute_batch(
        "INSERT INTO print_link_grams(tenant_id,link_id,grams,units,basis,updated_at)
           VALUES('default','link-a',42,1,'measured','2026-10-03T01:00:00.000Z');
         INSERT INTO spoolman_deductions (
           tenant_id, burst, link_id, profile_id, state, units,
           integration_id, spool_id, grams, grams_basis, created_at, updated_at
         ) VALUES (
           'default', 'burst-short', 'link-a', 1, 'pending', 1,
           'local', 7, 20, 'estimated', '2026-11-17T00:00:00.000Z', '2026-11-17T00:00:00.000Z'
         );",
    )
    .unwrap();
    drop(db);

    let reads = owner.accepted_reads().unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let origin = format!("http://{}", listener.local_addr().unwrap());
    let router = {
        let config = AcceptedHttpConfig::from_startup(&origin, "default", Some("1")).unwrap();
        let inventory = Arc::new(SpoolmanInventory::new().unwrap());
        accepted_router(config.with_stock(inventory), reads)
    };
    let task = tokio::spawn(async move {
        axum::serve(
            listener,
            router.into_make_service_with_connect_info::<std::net::SocketAddr>(),
        )
        .await
        .unwrap()
    });
    let before = shortfalls(&origin).await;
    assert_eq!(before["grams"], 752);
    assert!(send_once(&owner).await);
    let after = shortfalls(&origin).await;
    assert_eq!(after["grams"], 732);
    assert_eq!(after["spool_ids"], json!([7, 8]));
    assert_one_put(&fake, 20.0);
    task.abort();
    owner.shutdown().unwrap();
}

async fn shortfalls(origin: &str) -> Value {
    let client = reqwest::Client::builder().no_proxy().build().unwrap();
    let response = client
        .get(format!("{origin}/api/v2/plans/1/filament-shortfalls"))
        .header("Cookie", "pp_session=read-fixture-secret")
        .send()
        .await
        .unwrap();
    let status = response.status();
    let body: Value = response.json().await.unwrap();
    assert_eq!(status, 200, "{body}");
    body["filaments"][0]["grams_available"].clone()
}

fn requests(fake: &Fake) -> usize {
    fake.log.lock().unwrap().len()
}

struct Api {
    origin: String,
    task: tokio::task::JoinHandle<()>,
}

impl Api {
    async fn start(owner: &WriterOwner) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let origin = format!("http://{}", listener.local_addr().unwrap());
        let config = AcceptedHttpConfig::from_startup(&origin, "default", Some("1")).unwrap();
        let router = pp_api::spoolman_deductions::spoolman_deductions_router(
            config,
            owner.spoolman_deductions(desktop_policy()).unwrap(),
            Arc::new(SpoolmanInventory::new().unwrap()),
        );
        let task = tokio::spawn(async move {
            axum::serve(
                listener,
                router.into_make_service_with_connect_info::<std::net::SocketAddr>(),
            )
            .await
            .unwrap()
        });
        Self { origin, task }
    }
}

impl Drop for Api {
    fn drop(&mut self) {
        self.task.abort();
    }
}

fn http_client() -> reqwest::Client {
    reqwest::Client::builder().no_proxy().build().unwrap()
}

async fn get_deduction(origin: &str, id: i64) -> (u16, Value) {
    let response = http_client()
        .get(format!("{origin}/api/v2/spoolman/deductions/{id}"))
        .header("Cookie", format!("pp_session={SESSION}"))
        .send()
        .await
        .unwrap();
    let status = response.status().as_u16();
    let body = response.json().await.unwrap_or(Value::Null);
    (status, body)
}

async fn post_answer(origin: &str, id: i64, body: Value) -> (u16, Value) {
    let response = http_client()
        .post(format!(
            "{origin}/api/v2/spoolman/deductions/{id}/reconcile"
        ))
        .header("Cookie", format!("pp_session={SESSION}"))
        .header("Origin", origin)
        .json(&body)
        .send()
        .await
        .unwrap();
    let status = response.status().as_u16();
    let body = response.json().await.unwrap_or(Value::Null);
    (status, body)
}

async fn post_raw(origin: &str, id: i64, body: &str) -> (u16, Value) {
    let response = http_client()
        .post(format!(
            "{origin}/api/v2/spoolman/deductions/{id}/reconcile"
        ))
        .header("Cookie", format!("pp_session={SESSION}"))
        .header("Origin", origin)
        .header("Content-Type", "application/json")
        .body(body.to_owned())
        .send()
        .await
        .unwrap();
    let status = response.status().as_u16();
    let body = response.json().await.unwrap_or(Value::Null);
    (status, body)
}

async fn wait_state(owner: &WriterOwner, state: State) -> i64 {
    let start = std::time::Instant::now();
    loop {
        if let Some(row) = rows(owner).iter().find(|row| row.state == state) {
            return row.id;
        }
        assert!(
            start.elapsed() < Duration::from_secs(5),
            "timed out waiting for {state:?}"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

#[tokio::test]
async fn deducted_applies_unknown_without_a_request_and_get_reports_grams() {
    let fake = Fake::start(Mode::Drop).await;
    let (fixture, owner) = prepared(&fake.origin);
    burst(&owner, "k1");
    assert!(send_once(&owner).await);
    let id = rows(&owner)[0].id;
    assert_eq!(rows(&owner)[0].state, State::Unknown);
    let before_requests = requests(&fake);
    assert_eq!(fake.puts().len(), 1);

    let api = Api::start(&owner).await;
    let (status, body) = post_answer(&api.origin, id, json!({"answer": "deducted"})).await;
    assert_eq!(status, 200, "{body}");
    assert_eq!(body["state"], "applied");
    assert!(body["failure"].is_null());
    assert_eq!(requests(&fake), before_requests);
    assert_eq!(fake.puts().len(), 1);

    let (status, detail) = get_deduction(&api.origin, id).await;
    assert_eq!(status, 200, "{detail}");
    assert_eq!(detail["deduction"]["state"], "applied");
    assert_eq!(
        detail["grams_before"],
        json!({"kind": "known", "grams": 512})
    );
    assert_eq!(detail["grams_now"], json!({"kind": "known", "grams": 492}));
    assert_eq!(requests(&fake), before_requests + 1);

    put(&fixture, "feature_toggles", r#"{"spoolman":false}"#);
    let (status, detail) = get_deduction(&api.origin, id).await;
    assert_eq!(status, 200, "{detail}");
    assert_eq!(detail["grams_now"], json!({"kind": "known", "grams": 492}));
    put(&fixture, "feature_toggles", r#"{"spoolman":true}"#);

    let seen = requests(&fake);
    put(
        &fixture,
        "integrations",
        &integration(&fake.origin, "sm-1").replace("\"enabled\":true", "\"enabled\":false"),
    );
    let (status, detail) = get_deduction(&api.origin, id).await;
    assert_eq!(status, 200, "{detail}");
    assert_eq!(
        detail["grams_before"],
        json!({"kind": "known", "grams": 512})
    );
    assert_eq!(detail["grams_now"], json!({"kind": "unknown"}));
    assert_eq!(requests(&fake), seen);
    put(&fixture, "integrations", &integration(&fake.origin, "sm-1"));

    drop(fake);
    let (status, detail) = get_deduction(&api.origin, id).await;
    assert_eq!(status, 200, "{detail}");
    assert_eq!(
        detail["grams_before"],
        json!({"kind": "known", "grams": 512})
    );
    assert_eq!(detail["grams_now"], json!({"kind": "unknown"}));

    let (status, body) = post_answer(&api.origin, id, json!({"answer": "retry"})).await;
    assert_eq!(status, 409, "{body}");
    assert_eq!(body["code"], "deduction_state");
    let (status, body) = post_answer(&api.origin, id, json!({"answer": "nope"})).await;
    assert_eq!(status, 400, "{body}");
    let (status, body) = post_raw(&api.origin, id, "{").await;
    assert_eq!(status, 400, "{body}");
    let (status, body) = get_deduction(&api.origin, 0).await;
    assert_eq!(status, 404, "{body}");
    assert_eq!(body["code"], "deduction_not_found");
    owner.shutdown().unwrap();
}

#[tokio::test]
async fn dismiss_keeps_the_failure_and_a_pending_row_conflicts() {
    let fake = Fake::start(Mode::Refuse).await;
    let (_fixture, owner) = prepared(&fake.origin);
    burst(&owner, "k1");
    assert!(send_once(&owner).await);
    let id = rows(&owner)[0].id;
    let seen = requests(&fake);
    assert_eq!(fake.puts().len(), 1);

    let api = Api::start(&owner).await;
    let (status, body) = post_answer(&api.origin, id, json!({"answer": "dismiss"})).await;
    assert_eq!(status, 200, "{body}");
    assert_eq!(body["state"], "dismissed");
    assert_eq!(body["failure"], "refused");
    assert_eq!(body["failure_status"], 422);
    assert_eq!(requests(&fake), seen);
    assert_eq!(fake.puts().len(), 1);
    let (status, body) = post_answer(&api.origin, id, json!({"answer": "deducted"})).await;
    assert_eq!(status, 409, "{body}");
    assert_eq!(body["code"], "deduction_state");
    assert_eq!(requests(&fake), seen);

    let (status, detail) = get_deduction(&api.origin, id).await;
    assert_eq!(status, 200, "{detail}");
    assert_eq!(
        detail["grams_before"],
        json!({"kind": "known", "grams": 512})
    );
    assert_eq!(detail["grams_now"], json!({"kind": "known", "grams": 512}));
    owner.shutdown().unwrap();

    let fake = Fake::start(Mode::Apply).await;
    let (_fixture, owner) = prepared(&fake.origin);
    burst(&owner, "k1");
    let id = rows(&owner)[0].id;
    assert_eq!(rows(&owner)[0].state, State::Pending);
    let api = Api::start(&owner).await;
    let (status, body) = post_answer(&api.origin, id, json!({"answer": "deducted"})).await;
    assert_eq!(status, 409, "{body}");
    assert_eq!(body["code"], "deduction_state");
    assert_eq!(rows(&owner)[0].state, State::Pending);
    assert!(fake.puts().is_empty());
    assert_eq!(requests(&fake), 0);
    let (status, detail) = get_deduction(&api.origin, id).await;
    assert_eq!(status, 200, "{detail}");
    assert_eq!(detail["grams_before"], json!({"kind": "unknown"}));
    assert_eq!(detail["grams_now"], json!({"kind": "known", "grams": 512}));
    assert!(fake.puts().is_empty());
    owner.shutdown().unwrap();
}

#[tokio::test]
async fn retry_wakes_the_worker_for_exactly_one_more_put() {
    let fake = Fake::start(Mode::RefuseThenApply).await;
    let (_fixture, owner) = prepared(&fake.origin);
    let worker =
        DeductionWorker::new(&owner, desktop_policy(), SpoolmanInventory::new().unwrap()).unwrap();
    let stop = Arc::new(AtomicBool::new(false));
    let wake = worker.waker();
    let task = worker.spawn(Arc::clone(&stop));
    tokio::time::sleep(Duration::from_millis(200)).await;
    let api = Api::start(&owner).await;
    burst(&owner, "k1");
    let id = wait_state(&owner, State::Failed).await;
    assert_eq!(fake.puts().len(), 1);

    let (status, body) = post_answer(&api.origin, id, json!({"answer": "retry"})).await;
    assert_eq!(status, 200, "{body}");
    assert_eq!(body["state"], "pending");
    assert!(body["failure"].is_null());
    let id = wait_state(&owner, State::Applied).await;
    assert_eq!(fake.puts().len(), 2);
    assert_eq!(fake.puts()[1].path, "/api/v1/spool/7/use");
    let (status, detail) = get_deduction(&api.origin, id).await;
    assert_eq!(status, 200, "{detail}");
    assert_eq!(detail["deduction"]["state"], "applied");
    assert_eq!(
        detail["grams_before"],
        json!({"kind": "known", "grams": 512})
    );
    assert_eq!(detail["grams_now"], json!({"kind": "known", "grams": 492}));

    stop.store(true, Ordering::Release);
    wake.notify_one();
    task.await.unwrap();
    owner.shutdown().unwrap();
}
