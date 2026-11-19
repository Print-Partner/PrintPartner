#![cfg(feature = "desktop_rebuild")]
//! `POST /api/v2/printers/probe` through the composed `product_router`, with
//! the gateway's checked driver classifying a recorded loopback Moonraker.
//! The recorded printer only answers; nothing contacts a real printer.

#[allow(dead_code)]
#[path = "support/installation.rs"]
mod support_installation;

use pp_storage::{Limits, WriterOwner};
use pp_testkit::moonraker::RecordedPrinter;
use reqwest::Method;
use serde_json::{Value, json};
use support_installation::Server;

const PATH: &str = "/api/v2/printers/probe";

#[tokio::test]
async fn composed_router_classifies_a_recorded_moonraker_with_one_read() {
    let root = tempfile::tempdir().unwrap();
    let data = root.path().join("data");
    let owner = WriterOwner::open(&data, Limits::default()).unwrap().0;
    let server = Server::start(owner, data, root, Some("1")).await;
    let printer = RecordedPrinter::start(vec![(
        200,
        br#"{"result":{"klippy_state":"ready","components":["file_manager"]}}"#,
    )])
    .unwrap();
    let body = json!({"kind": "moonraker", "address": printer.url()});

    let anonymous = server
        .client
        .post(format!("{}{PATH}", server.origin))
        .header("Origin", &server.origin)
        .json(&body)
        .send()
        .await
        .unwrap();
    assert_eq!(anonymous.status(), 401);
    assert_eq!(printer.calls(), 0, "no read before authentication");

    let response = server
        .request(Method::POST, PATH)
        .json(&body)
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 200);
    let result: Value = response.json().await.unwrap();
    assert_eq!(
        result,
        json!({
            "kind": "moonraker",
            "reachable": true,
            "credentials_needed": false,
            "reason": "Printer responded successfully.",
            "capabilities": ["status", "files"],
        })
    );
    assert!(!result.to_string().contains(printer.url()));
    assert_eq!(printer.calls(), 1);
    let lines = printer.request_lines();
    assert!(lines[0].starts_with("GET /server/info "), "{lines:?}");
    server.stop().await;
}
