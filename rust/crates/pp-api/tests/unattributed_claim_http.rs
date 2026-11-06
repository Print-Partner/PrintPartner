#![cfg(feature = "desktop_rebuild")]
//! GRE-1182 through the mounted Rust routes: claim and dismiss an unattributed
//! print on a disposable copy of a real SQLite fixture, across a restart, with
//! the refusals that write nothing and the flag off mounting nothing.
use axum::http::StatusCode;
use checkoff_http::{Fixture, post};
use pp_storage::checkoff_progress::wire::{ClaimUnattributedRequest, DismissUnattributedRequest};
use serde_json::{Value, json};

#[path = "support/checkoff_http.rs"]
mod checkoff_http;
#[path = "../../pp-storage/tests/common/js_json.rs"]
mod js_json;

fn print(id: &str, filename: &str) -> Value {
    json!({
        "id": id,
        "integration_id": "sim-moonraker",
        "printer_id": "sim-1",
        "host_name": "Simulated Voron",
        "filename": filename,
        "completed_at": "2026-11-05T20:00:00.000Z",
        "gcode_objects": ["gear.stl", "bracket.stl"],
        "candidates": [],
    })
}

impl Fixture {
    /// The shared fixture with two open unattributed prints.
    fn with_prints() -> Self {
        let fixture = Self::new();
        fixture
            .sql()
            .execute(
                "INSERT INTO app_settings (tenant_id, key, value) VALUES ('default', 'printer.unattributed_prints', ?1)",
                [json!([print("p1", "mixed.gcode"), print("p2", "other.gcode")]).to_string()],
            )
            .unwrap();
        fixture
    }

    /// The rows a claim or dismiss writes.
    fn written(&self) -> (String, String, String, i64) {
        let sql = self.sql();
        let setting = |key: &str| {
            sql.query_row(
                "SELECT COALESCE((SELECT value FROM app_settings WHERE tenant_id='default' AND key=?1), '')",
                [key],
                |r| r.get(0),
            )
            .unwrap()
        };
        (
            setting("printer.unattributed_prints"),
            setting("printer.checkoff_links"),
            setting("printer.plan_bindings"),
            sql.query_row("SELECT COUNT(*) FROM domain_events", [], |r| r.get(0))
                .unwrap(),
        )
    }

    fn print(&self, id: &str) -> Value {
        js_json::parse(&self.written().0)
            .as_array()
            .unwrap()
            .iter()
            .find(|p| p["id"] == id)
            .unwrap()
            .clone()
    }
}

#[tokio::test]
async fn claim_and_dismiss_http_write_once_and_hold_across_restart() {
    let fixture = Fixture::with_prints();
    let (owner, router) = fixture.open("1");
    let body = serde_json::to_string(&ClaimUnattributedRequest {
        profile_id: 1,
        selected_stl_basenames: Some(vec!["gear.stl".into()]),
    })
    .unwrap();
    let (status, claimed) = post(
        &router,
        "/api/v2/printer-checkoff/unattributed/p1/claim",
        &body,
        true,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{claimed}");
    assert_eq!(claimed["ok"], true);
    let link = &claimed["link"];
    assert_eq!(
        (
            &link["profile_id"],
            &link["printer_id"],
            &link["filename"],
            &link["state"]
        ),
        (
            &json!(1),
            &json!("sim-1"),
            &json!("mixed.gcode"),
            &json!("awaiting_verify")
        )
    );
    assert_eq!(link["units"], json!([{ "part_id": 3, "unit_index": 0 }]));
    assert_eq!(fixture.print("p1")["claimed_profile_id"], 1);
    let after_claim = fixture.written();
    assert_eq!(after_claim.3, 1, "one domain event");
    owner.shutdown().unwrap();

    let (owner, router) = fixture.open("true");
    let (status, again) = post(
        &router,
        "/printer-checkoff/unattributed/p1/claim",
        &body,
        true,
    )
    .await;
    assert_eq!(
        (status, again["detail"].as_str()),
        (StatusCode::CONFLICT, Some("Print already claimed"))
    );
    assert_eq!(
        fixture.written(),
        after_claim,
        "a refused claim writes nothing"
    );

    let empty = serde_json::to_string(&DismissUnattributedRequest {}).unwrap();
    let (status, dismissed) = post(
        &router,
        "/api/v2/printer-checkoff/unattributed/p2/dismiss",
        &empty,
        true,
    )
    .await;
    assert_eq!((status, dismissed), (StatusCode::OK, json!({ "ok": true })));
    assert_eq!(fixture.print("p2")["dismissed"], true);
    assert_eq!(fixture.written().3, 2);
    let (status, missing) = post(
        &router,
        "/printer-checkoff/unattributed/nope/dismiss",
        &empty,
        true,
    )
    .await;
    assert_eq!(
        (status, missing["detail"].as_str()),
        (StatusCode::NOT_FOUND, Some("Unattributed print not found"))
    );
    owner.shutdown().unwrap();
}

#[tokio::test]
async fn claim_http_refuses_before_storage_writes() {
    let fixture = Fixture::with_prints();
    let (owner, router) = fixture.open("1");
    let before = fixture.written();
    let claim = "/printer-checkoff/unattributed/p1/claim";
    let (status, _) = post(&router, claim, r#"{"profile_id":1}"#, false).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    let (status, body) = post(&router, claim, "not json", true).await;
    assert_eq!(
        (status, body["detail"].as_str()),
        (
            StatusCode::BAD_REQUEST,
            Some("Request body must be a JSON object")
        )
    );
    let refusals = [
        (r#"{}"#, StatusCode::BAD_REQUEST, "profile_id is required"),
        (
            r#"{"profile_id":1,"selected_stl_basenames":[]}"#,
            StatusCode::BAD_REQUEST,
            "Select at least one plate file",
        ),
        (
            r#"{"profile_id":1,"selected_stl_basenames":["hinge.stl"]}"#,
            StatusCode::BAD_REQUEST,
            "Selected files are not on this plate",
        ),
        (
            r#"{"profile_id":99}"#,
            StatusCode::INTERNAL_SERVER_ERROR,
            "Internal Server Error",
        ),
    ];
    for (body, expected, detail) in refusals {
        let (status, refused) = post(&router, claim, body, true).await;
        assert_eq!(
            (status, refused["detail"].as_str()),
            (expected, Some(detail)),
            "{body}"
        );
    }
    let (status, unknown) = post(
        &router,
        "/printer-checkoff/unattributed/nope/claim",
        r#"{"profile_id":1}"#,
        true,
    )
    .await;
    assert_eq!(
        (status, unknown["detail"].as_str()),
        (StatusCode::NOT_FOUND, Some("Unattributed print not found"))
    );
    assert_eq!(fixture.written(), before);
    owner.shutdown().unwrap();

    let (owner, router) = fixture.open("0");
    let (status, _) = post(&router, claim, r#"{"profile_id":1}"#, true).await;
    assert_eq!(status, StatusCode::NOT_FOUND, "the flag off mounts nothing");
    assert_eq!(fixture.written(), before);
    owner.shutdown().unwrap();
}
