#![cfg(feature = "desktop_rebuild")]
//! GRE-1191 through the mounted Rust routes: the Checkoff tab reads units and
//! the basis from `GET /api/v2/plans/{id}/checkoff/units`, sends one manual
//! burst, replays it across a restart, and undoes it once, on a disposable copy
//! of a real SQLite fixture. With the flag off nothing is mounted.
use axum::{Router, http::StatusCode};
use checkoff_http::{Fixture, post, send};
use pp_api::accepted::{AcceptedHttpConfig, accepted_router};
use pp_storage::{
    WriterOwner,
    checkoff_progress::wire::{ManualBurstReceipt, ManualBurstRequest, UndoBatchReceipt},
};
use serde_json::{Value, json};

#[path = "support/checkoff_http.rs"]
mod checkoff_http;
#[path = "../../pp-storage/tests/common/js_json.rs"]
mod js_json;

fn mount(fixture: &Fixture, flag: &str) -> (WriterOwner, Router) {
    let (owner, checkoff) = fixture.open(flag);
    let config =
        AcceptedHttpConfig::from_startup("http://127.0.0.1:4100", "default", Some(flag)).unwrap();
    let reads = accepted_router(config, owner.accepted_reads().unwrap());
    (owner, checkoff.merge(reads))
}

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

#[tokio::test]
async fn manual_burst_http_reads_units_records_replays_and_undoes_once() {
    let fixture = Fixture::new();
    let printed = fixture.printed();
    let (owner, router) = mount(&fixture, "1");
    let (status, units) = send(&router, "GET", "/api/v2/plans/1/checkoff/units", "", true).await;
    assert_eq!(status, StatusCode::OK, "{units}");
    assert_eq!(units["profile_id"], 1);
    assert_eq!(units["basis"]["profile_id"], 1);
    assert_eq!(
        units["plate_revision_id"],
        Value::Null,
        "the fixture has no Plates"
    );
    let filenames: Vec<&str> = units["parts"]
        .as_array()
        .unwrap()
        .iter()
        .map(|p| p["filename"].as_str().unwrap())
        .collect();
    assert_eq!(
        filenames,
        ["bracket.stl", "gear.stl"],
        "excluded Parts are left out"
    );
    assert_eq!(
        units["parts"][0]["units"][0],
        json!({ "token": "ppu_00000000000000000000000000000001", "unit_index": 0,
            "object_name": "bracket__ppu_00000000000000000000000000000001", "printed": true, "assembled": true, "plate": null })
    );
    let (status, _) = send(&router, "GET", "/api/v2/plans/1/checkoff/units", "", false).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    let (status, _) = send(&router, "GET", "/api/v2/plans/999/checkoff/units", "", true).await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    let body = json!({
        "idempotency_key": "tab-burst",
        "expected": units["basis"],
        "units": [{ "token": units["parts"][0]["units"][2]["token"], "printed": true }],
    })
    .to_string();
    let _: ManualBurstRequest = serde_json::from_str(&body).unwrap();
    let (status, first) = post(&router, "/api/v2/plans/1/checkoff/bursts", &body, true).await;
    assert_eq!(status, StatusCode::OK, "{first}");
    let receipt: ManualBurstReceipt = serde_json::from_value(first.clone()).unwrap();
    assert_eq!(receipt.printed_added, 2, "unit 3 fills unit 2 too");
    assert_eq!(fixture.printed(), printed + 2);
    let (status, refused) = post(&router, "/api/v2/plans/3/checkoff/bursts", &body, true).await;
    assert_eq!(
        (status, &refused["detail"]),
        (StatusCode::NOT_FOUND, &Value::from("Plan not found")),
        "a body for another Build"
    );
    let (status, _) = post(&router, "/api/v2/plans/1/checkoff/bursts", &body, false).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    owner.shutdown().unwrap();

    let (owner, router) = mount(&fixture, "true");
    let (status, replay) = post(&router, "/plans/1/checkoff/bursts", &body, true).await;
    assert_eq!(
        (status, &replay),
        (StatusCode::OK, &first),
        "same key after a restart"
    );
    assert_eq!(
        fixture.printed(),
        printed + 2,
        "a replay counts nothing twice"
    );
    let token = json!({ "undo_token": receipt.undo_token }).to_string();
    let (status, undone) = post(
        &router,
        "/api/v2/plans/1/checkoff/bursts/undo",
        &token,
        true,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{undone}");
    let _: UndoBatchReceipt = serde_json::from_value(undone).unwrap();
    assert_eq!(fixture.printed(), printed);
    let (status, again) = post(
        &router,
        "/api/v2/plans/1/checkoff/bursts/undo",
        &token,
        true,
    )
    .await;
    assert_eq!(
        (status, &again["detail"]),
        (
            StatusCode::CONFLICT,
            &Value::from("This Checkoff burst was already undone")
        )
    );
    owner.shutdown().unwrap();
}

#[tokio::test]
async fn manual_burst_http_is_not_mounted_with_the_flag_off() {
    let fixture = Fixture::new();
    let (owner, router) = mount(&fixture, "0");
    for (method, path) in [
        ("GET", "/api/v2/plans/1/checkoff/units"),
        ("POST", "/api/v2/plans/1/checkoff/bursts"),
        ("POST", "/api/v2/plans/1/checkoff/bursts/undo"),
    ] {
        let (status, _) = send(&router, method, path, "{}", true).await;
        assert_eq!(status, StatusCode::NOT_FOUND, "{method} {path}");
    }
    owner.shutdown().unwrap();
}
