#![cfg(feature = "desktop_rebuild")]
//! GRE-1169: manual-complete of a print on a hand-tracked printer through the
//! mounted Rust route, on a disposable copy of a real SQLite fixture.
use axum::http::StatusCode;
use checkoff_http::{Fixture, post};
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

    fn links(&self) -> Value {
        let raw: String = self
            .sql()
            .query_row(
                "SELECT value FROM app_settings WHERE tenant_id='default' AND key='printer.checkoff_links'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        js_json::parse(&raw)
    }

    fn link(&self, id: &str) -> Value {
        self.links()
            .as_array()
            .unwrap()
            .iter()
            .find(|link| link["id"] == id)
            .cloned()
            .unwrap()
    }

    /// Adds a watching link for a hand-tracked printer and one for a printer
    /// a host monitors.
    fn add_watching(&self) {
        let mut links = self.links();
        for (id, integration) in [
            ("link-manual", "manual:shelf"),
            ("link-hosted", "sim-moonraker"),
        ] {
            links.as_array_mut().unwrap().push(json!({
                "id": id, "profile_id": 1, "integration_id": integration, "printer_id": "shelf",
                "host_name": "Shelf", "filename": format!("{id}.gcode"), "state": "watching",
                "created_at": "2020-01-01T00:00:00.000Z", "units": [{ "part_id": 2, "unit_index": 0 }],
            }));
        }
        self.sql()
            .execute(
                "UPDATE app_settings SET value=?1 WHERE tenant_id='default' AND key='printer.checkoff_links'",
                [links.to_string()],
            )
            .unwrap();
    }
}

#[tokio::test]
async fn checkoff_single_http_manual_complete_only_for_hand_tracked_prints() {
    let fixture = Fixture::new();
    fixture.add_watching();
    let printed = fixture.printed();
    let (owner, router) = fixture.open("true");
    let complete = |id: &str| format!("/printer-checkoff/{id}/manual-complete");

    let (status, missing) = post(&router, &complete("no-such-link"), "", true).await;
    assert_eq!(status, StatusCode::NOT_FOUND, "{missing}");
    assert_eq!(missing["detail"], "Tracked print not found");
    let (status, hosted) = post(&router, &complete("link-hosted"), "", true).await;
    assert_eq!(status, StatusCode::CONFLICT, "{hosted}");
    assert_eq!(
        hosted["detail"],
        "This print is monitored by its printer host"
    );
    assert_eq!(fixture.link("link-hosted")["state"], "watching");

    let (status, done) = post(
        &router,
        &format!("/api/v2{}", complete("link-manual")),
        "",
        true,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{done}");
    let link = &done["link"];
    assert_eq!(link["state"], "awaiting_verify");
    assert_eq!(link["host_outcome"], "success");
    assert_eq!(link["last_progress"], 100);
    assert_eq!(link["saw_active"], true);
    assert!(link["completed_at"].is_string());
    assert_eq!(&fixture.link("link-manual"), link);
    assert_eq!(
        fixture.printed(),
        printed,
        "manual complete checks nothing off"
    );

    let (status, twice) = post(
        &router,
        &format!("/api/v1{}", complete("link-manual")),
        "",
        true,
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "{twice}");
    assert_eq!(
        twice["detail"],
        "Tracked print is no longer waiting to finish"
    );
    owner.shutdown().unwrap();
}

#[tokio::test]
async fn checkoff_single_http_routes_are_absent_without_the_flag() {
    let fixture = Fixture::new();
    let (owner, router) = fixture.open("0");
    let (status, _) = post(
        &router,
        "/printer-checkoff/link-verify/manual-complete",
        "",
        true,
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    owner.shutdown().unwrap();
}
