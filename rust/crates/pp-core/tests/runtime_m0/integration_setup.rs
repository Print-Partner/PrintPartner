//! Daily-loop step 3 on a Rust-only runtime: an integration added through
//! `POST /api/v2/integrations` passes its connection test and reports live
//! status with no restart, still does after a restart on the same data
//! directory, and is gone everywhere after `DELETE`. The printer is the
//! recorded loopback Moonraker; nothing is uploaded or started.
use super::printer_host::recorded;
use super::watcher_proof::{proof_data, run_in_child, shut_down, signed_in, start_rust_only};
use super::{request, temporary};
use axum::http::StatusCode;
use pp_testkit::moonraker::RoutedPrinter;
use serde_json::{Value, json};
use std::{path::Path, time::Duration};

const SECRET: &str = "created-key-canary";
const CREATED: &str = "/api/v2/integrations";
const DEFAULT: &str = "/api/v1/settings/spoolman-default";

#[tokio::test]
async fn integration_setup_rust_only_runtime_creates_polls_and_deletes() {
    let data = temporary("integration-setup");
    run_in_child("integration_setup::integration_setup_in_child", &data).await;
    let _ = std::fs::remove_dir_all(data);
}

/// One request, with its status, parsed body and the body's text.
async fn call(
    origin: &str,
    cookie: &str,
    method: &str,
    path: &str,
    body: Option<Value>,
) -> (StatusCode, Value, String) {
    let (code, _, bytes) = request(origin, path, method, Some(cookie), None, None, body).await;
    let text = String::from_utf8_lossy(&bytes).into_owned();
    (code, serde_json::from_str(&text).unwrap_or_default(), text)
}

/// Polls the status route until the recorded print shows.
async fn printing(origin: &str, cookie: &str, id: &str) {
    let path = format!("/api/v1/integrations/{id}/status");
    let seen = tokio::time::timeout(Duration::from_secs(20), async {
        loop {
            let (code, body, _) = call(origin, cookie, "GET", &path, None).await;
            if code == StatusCode::OK && body["state"] == "printing" {
                return;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    });
    seen.await
        .expect("the runtime never polled the created integration");
}

/// A refused request names its field.
async fn refuses_invalid_requests(origin: &str, cookie: &str, moonraker: &Value) {
    for (field, value) in [
        ("base_url", "http://user:pw@printer.invalid"),
        ("kind", "bambu"),
    ] {
        let mut body = moonraker.clone();
        body[field] = json!(value);
        let (code, problem, text) = call(origin, cookie, "POST", CREATED, Some(body)).await;
        assert_eq!(code, StatusCode::BAD_REQUEST, "{problem}");
        assert!(!text.contains(SECRET), "{text}");
        let pointer = format!("/{field}");
        assert_eq!(problem["errors"][0]["pointer"], pointer, "{problem}");
    }
}

/// The connection test passes and the recorded print shows on the status route.
async fn tested_and_polled(origin: &str, cookie: &str, id: &str) {
    let test = format!("/api/v1/integrations/{id}/test");
    let (code, body, _) = call(origin, cookie, "POST", &test, None).await;
    let passed = (code, &body["ok"]);
    assert_eq!(passed, (StatusCode::OK, &json!(true)), "{body}");
    printing(origin, cookie, id).await;
}

/// Delete answers what it removed. Then the test and status routes do not
/// know the id, the linked printer shows no integration, and the printer
/// hears nothing more.
async fn deleted(origin: &str, cookie: &str, printer: &RoutedPrinter, view: &Value) {
    let id = view["id"].as_str().unwrap();
    let remove = format!("/api/v2/integrations/{id}");
    let (code, removed, text) = call(origin, cookie, "DELETE", &remove, None).await;
    assert_eq!((code, &removed), (StatusCode::OK, view), "{text}");
    assert!(!text.contains(SECRET), "{text}");
    let test = format!("/api/v1/integrations/{id}/test");
    let (code, body, _) = call(origin, cookie, "POST", &test, None).await;
    assert_eq!(code, StatusCode::NOT_FOUND, "{body}");
    // The watchers re-read the stored hosts about once a second.
    let status = format!("/api/v1/integrations/{id}/status");
    let dropped = tokio::time::timeout(Duration::from_secs(20), async {
        while call(origin, cookie, "GET", &status, None).await.0 != StatusCode::NOT_FOUND {
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    });
    dropped.await.expect("the deleted host kept its status");
    let (code, problem, _) = call(origin, cookie, "DELETE", &remove, None).await;
    let again = (code, &problem["code"]);
    assert_eq!(
        again,
        (StatusCode::NOT_FOUND, &json!("integration_not_found"))
    );
    let (_, farm, _) = call(origin, cookie, "GET", "/api/v2/printers", None).await;
    let linked = &farm["printers"][0];
    assert_eq!(
        (&linked["id"], &linked["integration"]),
        (&json!("p-1"), &Value::Null)
    );
    tokio::time::sleep(Duration::from_millis(2500)).await;
    let heard = printer.request_lines().len();
    tokio::time::sleep(Duration::from_millis(3500)).await;
    let later = printer.request_lines().len();
    assert_eq!(later, heard, "a deleted host is polled");
}

#[tokio::test]
#[ignore = "runs in an isolated subprocess with explicit environment"]
async fn integration_setup_in_child() {
    let data = proof_data();
    let (printer, _) = recorded();
    let (runtime, origin, desktop) = start_rust_only(&data).await;
    let moonraker = json!({"kind": "moonraker", "name": "Voron",
        "base_url": printer.url(), "api_key": SECRET});
    for (method, path) in [("POST", CREATED), ("DELETE", "/api/v2/integrations/any")] {
        let body = (method == "POST").then(|| moonraker.clone());
        let (code, _, _) = call(&origin, &desktop, method, path, body).await;
        assert_eq!(code, StatusCode::UNAUTHORIZED, "{method} {path}");
    }
    // A session cookie storage does not know passes the gate and is refused
    // by the write itself.
    let stranger = format!("{desktop}; pp_session=not-a-session");
    let body = Some(moonraker.clone());
    let (code, _, _) = call(&origin, &stranger, "POST", CREATED, body).await;
    assert_eq!(code, StatusCode::UNAUTHORIZED);
    let cookie = signed_in(&origin, &desktop, 1).await;
    refuses_invalid_requests(&origin, &cookie, &moonraker).await;

    let (code, view, text) = call(&origin, &cookie, "POST", CREATED, Some(moonraker)).await;
    assert_eq!(code, StatusCode::OK, "{text}");
    assert!(!text.contains(SECRET), "{text}");
    let id = view["id"].as_str().expect("the new id").to_owned();
    assert_eq!(
        view,
        json!({"id": id, "kind": "moonraker", "name": "Voron", "enabled": true,
            "base_url": printer.url(), "username": null,
            "has_api_key": true, "has_password": false})
    );

    // A printer that links the new host shows it. Printer writes are a later
    // change, so the fleet row is stored directly.
    let fleet = json!([{"id": "p-1", "name": "Voron", "integration_id": id}]).to_string();
    rusqlite::Connection::open(data.join("print-partner.db"))
        .unwrap()
        .execute(
            "INSERT INTO app_settings(tenant_id,key,value) VALUES('default','printer.fleet',?1)",
            [&fleet],
        )
        .unwrap();
    let (code, farm, _) = call(&origin, &cookie, "GET", "/api/v2/printers", None).await;
    assert_eq!(code, StatusCode::OK, "{farm}");
    let linked = &farm["printers"][0]["integration"];
    assert_eq!(linked, &json!({"id": id, "kind": "moonraker"}), "{farm}");

    // A Spoolman integration created here is one the default setting takes.
    let spoolman = json!({"kind": "spoolman", "name": "Spools", "base_url": "http://127.0.0.1:9"});
    let (code, spool, _) = call(&origin, &cookie, "POST", CREATED, Some(spoolman)).await;
    assert_eq!(code, StatusCode::OK, "{spool}");
    let chosen = json!({"integration_id": spool["id"]});
    let (code, body, _) = call(&origin, &cookie, "PUT", DEFAULT, Some(chosen.clone())).await;
    assert_eq!((code, &body), (StatusCode::OK, &chosen));

    // With no restart the new host passes its test and is polled; after a
    // restart on the same data directory it still does.
    tested_and_polled(&origin, &cookie, &id).await;
    shut_down(runtime).await;
    let (runtime, origin, desktop) = start_rust_only(&data).await;
    let cookie = signed_in(&origin, &desktop, 2).await;
    tested_and_polled(&origin, &cookie, &id).await;

    deleted(&origin, &cookie, &printer, &view).await;
    // Deleting the Spoolman integration clears the default that named it.
    let spool_path = format!("/api/v2/integrations/{}", spool["id"].as_str().unwrap());
    let (code, _, _) = call(&origin, &cookie, "DELETE", &spool_path, None).await;
    assert_eq!(code, StatusCode::OK);
    let (code, body, _) = call(&origin, &cookie, "GET", DEFAULT, None).await;
    let cleared = json!({"integration_id": null});
    assert_eq!((code, &body), (StatusCode::OK, &cleared));
    shut_down(runtime).await;
    for line in printer.request_lines() {
        assert!(line.starts_with("GET "), "the printer saw {line}");
    }
    key_is_in_no_file_but_the_database(&data);
}

/// The submitted API key is in no log or other file the runtime wrote. The
/// database is where it is stored, so its files are left out.
fn key_is_in_no_file_but_the_database(data: &Path) {
    let mut pending = vec![data.to_owned()];
    let mut read = 0;
    while let Some(directory) = pending.pop() {
        for entry in std::fs::read_dir(directory).unwrap() {
            let path = entry.unwrap().path();
            let name = path.file_name().unwrap().to_string_lossy().into_owned();
            if path.is_dir() {
                pending.push(path);
            } else if !name.starts_with("print-partner.db") {
                let text = std::fs::read(&path).unwrap();
                let text = String::from_utf8_lossy(&text);
                assert!(!text.contains(SECRET), "{name} holds the API key");
                read += 1;
            }
        }
    }
    assert!(read > 0, "the runtime wrote no file to check");
}
