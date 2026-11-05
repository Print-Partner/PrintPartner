#![cfg(feature = "desktop_rebuild")]
//! GRE-1090 through the mounted Rust route: the P/F contract request from
//! `web/packages/contracts/src/checkoff-burst.test.ts`, replay across a
//! restart, undo and its conflict, on a disposable copy of a real SQLite fixture.
use axum::{
    Router,
    body::Body,
    extract::ConnectInfo,
    http::{Request, StatusCode},
};
use pp_api::{accepted::AcceptedHttpConfig, checkoff::checkoff_router};
use pp_storage::{
    Limits, WriterOwner,
    checkoff_progress::wire::{
        UndoBatchReceipt, UndoBatchRequest, VerifyBatchReceipt, VerifyBatchRequest,
    },
};
use serde_json::{Value, json};
use std::{net::SocketAddr, path::PathBuf};
use tower::ServiceExt;

const ORIGIN: &str = "http://127.0.0.1:4100";
#[path = "../../pp-storage/tests/common/js_json.rs"]
mod js_json;

const NODE: &str = include_str!("../../../../fixtures/parity/printer-checkoff-verify/node.json");
const DATABASE: &[u8] =
    include_bytes!("../../pp-storage/tests/fixtures/required-units/unchanged.db");

struct Fixture(PathBuf);

impl Fixture {
    fn new() -> Self {
        let path = tempfile::tempdir().unwrap().keep();
        std::fs::create_dir_all(path.join("repos/1/revisions/fixture")).unwrap();
        std::fs::write(path.join("print-partner.db"), DATABASE).unwrap();
        let sql = rusqlite::Connection::open(path.join("print-partner.db")).unwrap();
        let node = js_json::parse(NODE);
        for statement in node["setup"].as_array().unwrap() {
            sql.execute_batch(statement.as_str().unwrap()).unwrap();
        }
        Self(path)
    }

    fn open(&self, flag: &str) -> (WriterOwner, Router) {
        let owner = WriterOwner::open(&self.0, Limits::default()).unwrap().0;
        let config = AcceptedHttpConfig::from_startup(ORIGIN, "default", Some(flag)).unwrap();
        let router = checkoff_router(config, owner.checkoff_progress().unwrap());
        (owner, router)
    }

    fn printed(&self) -> i64 {
        rusqlite::Connection::open(self.0.join("print-partner.db"))
            .unwrap()
            .query_row(
                "SELECT COUNT(*) FROM print_progress WHERE completed=1",
                [],
                |r| r.get(0),
            )
            .unwrap()
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

async fn post(router: &Router, path: &str, body: &str, cookie: bool) -> (StatusCode, Value) {
    let mut request = Request::post(path)
        .header("host", "127.0.0.1:4100")
        .header("origin", ORIGIN)
        .header("content-type", "application/json");
    if cookie {
        request = request.header("cookie", "pp_session=required-unit-fixture-secret");
    }
    let mut request = request.body(Body::from(body.to_owned())).unwrap();
    request
        .extensions_mut()
        .insert(ConnectInfo(SocketAddr::from(([127, 0, 0, 1], 50_000))));
    let response = router.clone().oneshot(request).await.unwrap();
    let status = response.status();
    let bytes = axum::body::to_bytes(response.into_body(), 1 << 22)
        .await
        .unwrap();
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or(Value::Null),
    )
}

fn confirmed(part: i64, unit: i64) -> Value {
    json!({ "part_id": part, "unit_index": unit, "result": "confirmed" })
}

/// `verifyBatchRequest("contract-burst", [plateBed(P), ...partFillBeds(links, 2)])`.
fn contract_request() -> String {
    json!({
        "idempotency_key": "contract-burst",
        "beds": [
            { "link_id": "link-verify", "decisions": [confirmed(1, 1), confirmed(1, 2), confirmed(3, 0), confirmed(3, 1)] },
            { "link_id": "link-burst", "decisions": [confirmed(2, 0)] },
        ],
    })
    .to_string()
}

#[tokio::test]
async fn verify_batch_http_replays_across_restart_and_undoes_once() {
    let fixture = Fixture::new();
    let printed = fixture.printed();
    let (owner, router) = fixture.open("1");
    let (status, first) = post(
        &router,
        "/printer-checkoff/verify-batch",
        &contract_request(),
        true,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{first}");
    assert_eq!(first["units_confirmed"], 5);
    let request: VerifyBatchRequest = serde_json::from_str(&contract_request()).unwrap();
    assert_eq!(
        request.beds.len(),
        2,
        "the generated client's request type takes the P/F body"
    );
    let receipt: VerifyBatchReceipt = serde_json::from_value(first.clone()).unwrap();
    assert_eq!(
        (
            receipt.units_confirmed,
            receipt.beds.len(),
            receipt.undo_token.as_str()
        ),
        (5, 2, first["undo_token"].as_str().unwrap()),
        "the generated client's receipt type reads the served receipt"
    );
    assert_eq!(fixture.printed(), printed + 5);
    owner.shutdown().unwrap();

    let (owner, router) = fixture.open("true");
    let (status, replay) = post(
        &router,
        "/api/v2/printer-checkoff/verify-batch",
        &contract_request(),
        true,
    )
    .await;
    assert_eq!(
        (status, &replay),
        (StatusCode::OK, &first),
        "same key and body after a restart"
    );
    assert_eq!(fixture.printed(), printed + 5);
    let token = json!({ "undo_token": first["undo_token"] }).to_string();
    let (status, undone) = post(&router, "/printer-checkoff/verify-batch/undo", &token, true).await;
    assert_eq!(status, StatusCode::OK, "{undone}");
    assert_eq!(fixture.printed(), printed);
    let _: UndoBatchRequest = serde_json::from_str(&token).unwrap();
    let undone: UndoBatchReceipt = serde_json::from_value(undone).unwrap();
    assert_eq!(undone.undo_token, receipt.undo_token);
    let (status, again) = post(
        &router,
        "/api/v2/printer-checkoff/verify-batch/undo",
        &token,
        true,
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(again["detail"], "This Checkoff burst was already undone");
    owner.shutdown().unwrap();
}

#[tokio::test]
async fn verify_batch_http_records_a_rejection_with_its_reason() {
    let fixture = Fixture::new();
    let printed = fixture.printed();
    let (owner, router) = fixture.open("1");
    let body = json!({
        "idempotency_key": "reject-burst",
        "beds": [{ "link_id": "link-burst", "decisions": [
            { "part_id": 2, "unit_index": 0, "result": "rejected", "reason": "warping", "note": "lifted corner" },
        ]}],
    })
    .to_string();
    let request: VerifyBatchRequest = serde_json::from_str(&body).unwrap();
    assert_eq!(
        serde_json::to_value(&request).unwrap(),
        serde_json::from_str::<Value>(&body).unwrap(),
        "the generated client's request type carries the reason and note"
    );
    let (status, receipt) = post(&router, "/printer-checkoff/verify-batch", &body, true).await;
    assert_eq!(status, StatusCode::OK, "{receipt}");
    assert_eq!(
        (
            receipt["units_rejected"].as_u64(),
            receipt["units_confirmed"].as_u64()
        ),
        (Some(1), Some(0))
    );
    assert_eq!(fixture.printed(), printed, "a rejection marks no Progress");
    let missing = body.replace(r#","reason":"warping""#, "");
    let (status, refused) = post(
        &router,
        "/printer-checkoff/verify-batch",
        &missing.replace("reject-burst", "reject-again"),
        true,
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{refused}");
    owner.shutdown().unwrap();
}

#[tokio::test]
async fn verify_batch_http_refuses_before_storage_writes() {
    let fixture = Fixture::new();
    let printed = fixture.printed();
    let (owner, router) = fixture.open("1");
    let (status, _) = post(
        &router,
        "/printer-checkoff/verify-batch",
        &contract_request(),
        false,
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    let (status, body) = post(&router, "/printer-checkoff/verify-batch", "not json", true).await;
    assert_eq!(
        (status, body["detail"].as_str()),
        (
            StatusCode::BAD_REQUEST,
            Some("Request body must be a JSON object")
        )
    );
    let changed = contract_request().replace("\"unit_index\":2", "\"unit_index\":9");
    post(
        &router,
        "/printer-checkoff/verify-batch",
        &contract_request(),
        true,
    )
    .await;
    let (status, body) = post(&router, "/printer-checkoff/verify-batch", &changed, true).await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(fixture.printed(), printed + 5);
    owner.shutdown().unwrap();

    let (owner, router) = fixture.open("0");
    let (status, _) = post(
        &router,
        "/printer-checkoff/verify-batch",
        &contract_request(),
        true,
    )
    .await;
    assert_eq!(
        status,
        StatusCode::NOT_FOUND,
        "the routes are not mounted with the flag off"
    );
    owner.shutdown().unwrap();
}
