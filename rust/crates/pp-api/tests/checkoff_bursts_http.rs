#![cfg(feature = "desktop_rebuild")]
//! GRE-1090 through the mounted Rust route: the P/F contract request from
//! `web/packages/contracts/src/checkoff-burst.test.ts`, replay across a
//! restart, undo and its conflict, on a disposable copy of a real SQLite fixture.
use axum::http::StatusCode;
use checkoff_http::{Fixture, post, send};
use pp_storage::checkoff_progress::wire::{
    UndoBatchReceipt, UndoBatchRequest, VerifyBatchReceipt, VerifyBatchRequest,
};
use serde_json::{Value, json};

#[path = "support/checkoff_http.rs"]
mod checkoff_http;
#[path = "../../pp-storage/tests/common/js_json.rs"]
mod js_json;

impl Fixture {
    fn printed(&self) -> i64 {
        self.sql()
            .query_row(
                "SELECT COUNT(*) FROM print_progress WHERE completed=1",
                [],
                |r| r.get(0),
            )
            .unwrap()
    }
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

/// GRE-1126: the farm log through the mounted routes, across a restart.
#[tokio::test]
async fn farm_log_http_pages_across_restart_and_undoes_by_entry() {
    let fixture = Fixture::new();
    let printed = fixture.printed();
    let (owner, router) = fixture.open("1");
    let (status, _) = send(&router, "GET", "/api/v2/farm-log", "", false).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    let (status, _) = post(
        &router,
        "/printer-checkoff/verify-batch",
        &contract_request(),
        true,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let (status, log) = send(&router, "GET", "/api/v2/farm-log?limit=10", "", true).await;
    assert_eq!(status, StatusCode::OK, "{log}");
    assert_eq!(log["entries"][0]["id"], "checkoff_burst:1");
    assert_eq!(log["entries"][0]["undo"]["undoable"], true);
    assert_eq!(log["next_cursor"], Value::Null);
    owner.shutdown().unwrap();

    let (owner, router) = fixture.open("true");
    let (status, reread) = send(&router, "GET", "/api/v2/farm-log?limit=10", "", true).await;
    assert_eq!(
        (status, &reread),
        (StatusCode::OK, &log),
        "stable after restart"
    );
    let (status, undone) = post(&router, "/api/v2/farm-log/checkoff_burst:1/undo", "", true).await;
    assert_eq!(status, StatusCode::OK, "{undone}");
    assert_eq!(undone["kind"], "checkoff_burst_undo");
    assert_eq!(fixture.printed(), printed);
    let (status, again) = post(&router, "/api/v2/farm-log/checkoff_burst:1/undo", "", true).await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(again["detail"], "This Checkoff burst was already undone");
    let (_, log) = send(&router, "GET", "/api/v2/farm-log", "", true).await;
    assert_eq!(log["entries"].as_array().unwrap().len(), 2);
    let (status, _) = send(&router, "GET", "/api/v2/farm-log?limit=1&limit=2", "", true).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    owner.shutdown().unwrap();

    let (owner, router) = fixture.open("0");
    let (status, _) = send(&router, "GET", "/api/v2/farm-log", "", true).await;
    assert_eq!(status, StatusCode::NOT_FOUND, "the flag off mounts nothing");
    owner.shutdown().unwrap();
}

fn import_body(plan_version: i64, rows: Value) -> String {
    json!({
        "expected": {
            "profile_id": 1,
            "plan_version": plan_version,
            "plan_revision_id": 1,
            "plan_revision_digest": "eb2dd43bcbf42d909a0b5bdcbeb9b0010951a905f7c5cb01d4b4def1b9eca54f",
            "required_unit_mapping_digest": "8c446659d41d31efe3d95931c9f898b0d5442a41104474671bdccab4779e3a41",
        },
        "rows": rows,
    })
    .to_string()
}

#[tokio::test]
async fn ticket_t_61_publication_progress_import_http_sets_counts_and_refuses_without_writes() {
    let fixture = Fixture::new();
    let printed = fixture.printed();
    let (owner, router) = fixture.open("1");
    let rows = json!([{ "part_id": 1, "printed_count": 3 }]);
    let invalid = json!({"detail": "Request is invalid", "code": "invalid_request"});
    for (path, body, status, expected) in [
        (
            "/plans/abc/progress/import",
            import_body(1, rows.clone()),
            400,
            invalid.clone(),
        ),
        (
            "/plans/1/progress/import",
            "[]".to_owned(),
            400,
            Value::Null,
        ),
        (
            "/plans/1/progress/import",
            import_body(1, json!([])),
            400,
            Value::Null,
        ),
        (
            "/plans/2/progress/import",
            import_body(1, rows.clone()),
            404,
            json!({"detail": "Plan or Part not found", "code": "progress_target_not_found"}),
        ),
        (
            "/api/v2/plans/1/progress/import",
            import_body(2, rows.clone()),
            409,
            json!({"detail": "Accepted Plan changed; reload and retry", "code": "stale_accepted_plan"}),
        ),
    ] {
        let (actual, body) = post(&router, path, &body, true).await;
        assert_eq!(actual.as_u16(), status, "{path} {body}");
        if !expected.is_null() {
            assert_eq!(
                (&body["detail"], &body["code"]),
                (&expected["detail"], &expected["code"]),
                "{path}"
            );
        }
    }
    let (status, _) = post(
        &router,
        "/plans/1/progress/import",
        &import_body(1, rows.clone()),
        false,
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert_eq!(fixture.printed(), printed);
    let (status, body) = post(
        &router,
        "/api/v2/plans/1/progress/import",
        &import_body(1, rows),
        true,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body, json!({"updated_parts": 1}));
    assert_eq!(fixture.printed(), printed + 2);
    owner.shutdown().unwrap();
}
