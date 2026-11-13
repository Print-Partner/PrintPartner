//! Daily-loop step 3 on a Rust-only runtime: an integration added through
//! `POST /api/v2/integrations` passes its connection test and reports live
//! status with no restart, still does after a restart on the same data
//! directory, is listed by `GET /api/v2/integrations` without its key, and is
//! gone everywhere after `DELETE`. One `PATCH` moves an integration saved at a
//! dead address to the printer. A printer added through
//! `POST /api/v2/printers` shows live in the Farm list the same way, and is
//! read, edited and deleted. The printer is the recorded loopback Moonraker;
//! nothing is uploaded or started.
//!
//! GRE-1523: rows the Node import wrote can hold keys Rust's shapes do not
//! name. No route shows such a key, and an edit stores it back unchanged.
use super::printer_host::recorded;
use super::watcher_proof::{
    proof_data, run_in_child, seed_settings, shut_down, signed_in, start_rust_only,
};
use super::{request, temporary};
use axum::http::StatusCode;
use pp_testkit::moonraker::RoutedPrinter;
use serde_json::{Value, json};
use std::{path::Path, time::Duration};

const SECRET: &str = "created-key-canary";
/// A value under a key Rust's shapes do not name.
const UNREAD: &str = "unread-key-canary";
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

/// An integration saved at a dead address fails its test. A `PATCH` that
/// moves it without its API key is refused. One `PATCH` with the key then
/// moves it to the printer and renames it: with no restart the test passes
/// and it is polled.
async fn moved_to_the_printer(origin: &str, cookie: &str, moonraker: &Value) {
    let mut down = moonraker.clone();
    down["base_url"] = json!("http://127.0.0.1:9");
    let (code, view, _) = call(origin, cookie, "POST", CREATED, Some(down)).await;
    assert_eq!(code, StatusCode::OK, "{view}");
    let id = view["id"].as_str().unwrap();
    let test = format!("/api/v1/integrations/{id}/test");
    let (code, body, _) = call(origin, cookie, "POST", &test, None).await;
    let failed = (code, &body["ok"]);
    assert_eq!(failed, (StatusCode::OK, &json!(false)), "{body}");

    let path = format!("{CREATED}/{id}");
    // A new address without the stored key would send that key to it.
    let refused = json!({"base_url": moonraker["base_url"]});
    let (code, problem, _) = call(origin, cookie, "PATCH", &path, Some(refused)).await;
    let named = (code, &problem["errors"][0]["pointer"]);
    assert_eq!(named, (StatusCode::BAD_REQUEST, &json!("/api_key")));
    let mut patch = moonraker.clone();
    patch["name"] = json!("Moved");
    patch.as_object_mut().unwrap().remove("kind");
    let (code, moved, text) = call(origin, cookie, "PATCH", &path, Some(patch)).await;
    let mut expected = view.clone();
    expected["name"] = json!("Moved");
    expected["base_url"] = moonraker["base_url"].clone();
    assert_eq!((code, &moved), (StatusCode::OK, &expected), "{text}");
    assert!(!text.contains(SECRET), "{text}");
    tested_and_polled(origin, cookie, id).await;
    let (code, _, _) = call(origin, cookie, "DELETE", &path, None).await;
    assert_eq!(code, StatusCode::OK);
}

/// Delete answers what it removed. Then the test and status routes do not
/// know the id, the linked printer shows no integration, and the printer
/// hears nothing more.
async fn deleted(origin: &str, cookie: &str, printer: &RoutedPrinter, view: &Value, kept: &str) {
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
        (&json!(kept), &Value::Null)
    );
    tokio::time::sleep(Duration::from_millis(2500)).await;
    let heard = printer.request_lines().len();
    tokio::time::sleep(Duration::from_millis(3500)).await;
    let later = printer.request_lines().len();
    assert_eq!(later, heard, "a deleted host is polled");
}

/// Printers are refused, created, read, edited and deleted through the
/// routes. Answers the id of the one printer left, which links `host`.
async fn printer_routes(origin: &str, cookie: &str, host: &str, spoolman: &str) -> String {
    let printers = "/api/v2/printers";
    let (code, farm, _) = call(origin, cookie, "GET", printers, None).await;
    assert_eq!((code, &farm["printers"]), (StatusCode::OK, &json!([])));
    let voron = json!({"name": "Voron", "model": "voron-2.4-350", "bed_width_mm": 350,
        "bed_depth_mm": 350, "bed_height_mm": 340, "integration_id": host});
    for (field, value) in [
        ("bed_width_mm", json!(0)),
        ("margin_mm", json!(175)),
        ("max_filament_slots", json!(5)),
        ("integration_id", json!(spoolman)),
    ] {
        let mut body = voron.clone();
        body[field] = value;
        let (code, problem, _) = call(origin, cookie, "POST", printers, Some(body)).await;
        assert_eq!(code, StatusCode::BAD_REQUEST, "{problem}");
        let pointer = format!("/{field}");
        assert_eq!(problem["errors"][0]["pointer"], pointer, "{problem}");
    }
    let (code, kept, _) = call(origin, cookie, "POST", printers, Some(voron.clone())).await;
    assert_eq!(code, StatusCode::OK, "{kept}");
    let mut expected = voron.clone();
    expected["id"] = kept["id"].clone();
    expected["margin_mm"] = json!(4);
    expected["max_filament_slots"] = json!(1);
    assert_eq!(kept, expected);

    // A second printer is read, edited and deleted; its integration stays.
    let (_, spare, _) = call(origin, cookie, "POST", printers, Some(voron)).await;
    let path = format!("{printers}/{}", spare["id"].as_str().unwrap());
    let (code, read, _) = call(origin, cookie, "GET", &path, None).await;
    assert_eq!((code, &read), (StatusCode::OK, &spare));
    let edit = json!({"name": "Spare", "integration_id": null});
    let (code, edited, _) = call(origin, cookie, "PATCH", &path, Some(edit)).await;
    let mut expected = spare.clone();
    expected["name"] = json!("Spare");
    expected["integration_id"] = Value::Null;
    assert_eq!((code, &edited), (StatusCode::OK, &expected));
    let (code, removed, _) = call(origin, cookie, "DELETE", &path, None).await;
    assert_eq!((code, &removed), (StatusCode::OK, &expected));
    let (code, problem, _) = call(origin, cookie, "GET", &path, None).await;
    let gone = (code, &problem["code"]);
    assert_eq!(gone, (StatusCode::NOT_FOUND, &json!("printer_not_found")));
    kept["id"].as_str().unwrap().to_owned()
}

/// The Farm list shows exactly `printer`, linked to `host` and printing.
async fn listed_live(origin: &str, cookie: &str, printer: &str, host: &str) {
    let link = json!({"id": host, "kind": "moonraker"});
    let live = tokio::time::timeout(Duration::from_secs(20), async {
        loop {
            let (code, farm, _) = call(origin, cookie, "GET", "/api/v2/printers", None).await;
            assert_eq!(code, StatusCode::OK, "{farm}");
            let listed = farm["printers"].as_array().unwrap();
            let one = listed.len() == 1 && listed[0]["id"] == printer;
            if one
                && listed[0]["integration"] == link
                && listed[0]["state"]["execution"] == "printing"
            {
                return;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    });
    live.await
        .expect("the created printer never showed live in the Farm list");
}

#[tokio::test]
#[ignore = "runs in an isolated subprocess with explicit environment"]
async fn integration_setup_in_child() {
    let data = proof_data();
    let (printer, _) = recorded();
    let (runtime, origin, desktop) = start_rust_only(&data).await;
    let moonraker = json!({"kind": "moonraker", "name": "Voron",
        "base_url": printer.url(), "api_key": SECRET});
    let any = "/api/v2/integrations/any";
    let anonymous = [
        ("GET", CREATED),
        ("POST", CREATED),
        ("PATCH", any),
        ("DELETE", any),
    ];
    for (method, path) in anonymous {
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
    moved_to_the_printer(&origin, &cookie, &moonraker).await;

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

    // A Spoolman integration created here is one the default setting takes.
    let spoolman = json!({"kind": "spoolman", "name": "Spools", "base_url": "http://127.0.0.1:9"});
    let (code, spool, _) = call(&origin, &cookie, "POST", CREATED, Some(spoolman)).await;
    assert_eq!(code, StatusCode::OK, "{spool}");
    let chosen = json!({"integration_id": spool["id"]});
    let (code, body, _) = call(&origin, &cookie, "PUT", DEFAULT, Some(chosen.clone())).await;
    assert_eq!((code, &body), (StatusCode::OK, &chosen));
    // The list shows both, in stored order, and never the key.
    let (code, list, text) = call(&origin, &cookie, "GET", CREATED, None).await;
    assert_eq!(code, StatusCode::OK, "{text}");
    assert!(!text.contains(SECRET), "{text}");
    assert_eq!(list, json!({"integrations": [view, spool]}));

    // With no restart the new host passes its test and is polled, and the
    // printer that links it shows live; after a restart on the same data
    // directory both still hold.
    let spool_id = spool["id"].as_str().unwrap();
    let kept = printer_routes(&origin, &cookie, &id, spool_id).await;
    tested_and_polled(&origin, &cookie, &id).await;
    listed_live(&origin, &cookie, &kept, &id).await;
    shut_down(runtime).await;
    let (runtime, origin, desktop) = start_rust_only(&data).await;
    let cookie = signed_in(&origin, &desktop, 2).await;
    tested_and_polled(&origin, &cookie, &id).await;
    listed_live(&origin, &cookie, &kept, &id).await;

    deleted(&origin, &cookie, &printer, &view, &kept).await;
    // Deleting the Spoolman integration clears the default that named it.
    let spool_path = format!("/api/v2/integrations/{}", spool["id"].as_str().unwrap());
    let (code, _, _) = call(&origin, &cookie, "DELETE", &spool_path, None).await;
    assert_eq!(code, StatusCode::OK);
    let (code, body, _) = call(&origin, &cookie, "GET", DEFAULT, None).await;
    let cleared = json!({"integration_id": null});
    assert_eq!((code, &body), (StatusCode::OK, &cleared));
    let (code, list, _) = call(&origin, &cookie, "GET", CREATED, None).await;
    assert_eq!((code, list), (StatusCode::OK, json!({"integrations": []})));
    shut_down(runtime).await;
    for line in printer.request_lines() {
        assert!(line.starts_with("GET "), "the printer saw {line}");
    }
    key_is_in_no_file_but_the_database(&data);
}

#[tokio::test]
async fn imported_rows_keep_their_unread_keys_and_no_route_shows_them() {
    let data = temporary("unread-keys");
    run_in_child("integration_setup::unread_keys_in_child", &data).await;
    let _ = std::fs::remove_dir_all(data);
}

#[tokio::test]
#[ignore = "runs in an isolated subprocess with explicit environment"]
async fn unread_keys_in_child() {
    let data = proof_data();
    // Rows as the import writes them: Rust's fields, then the keys it does
    // not name, on the printer, on the integration and in its config.
    let stamp = "2026-11-12T03:04:05.678Z";
    let fleet = json!([{"id": "p-1", "name": "Voron", "model": "v", "bed_width_mm": 350,
        "bed_depth_mm": 350, "bed_height_mm": 340, "margin_mm": 4, "max_filament_slots": 1,
        "loaded_filaments": [], "integration_id": "moon-1",
        "nozzle_mm": 0.4, "notes": {"token": UNREAD}}]);
    let hosts = json!([{"id": "moon-1", "type": "moonraker", "name": "Host",
        "config": {"base_url": "http://127.0.0.1:9", "enabled": false,
                   "poll_seconds": 5, "legacy": {"token": UNREAD}},
        "created_at": stamp, "updated_at": stamp, "capabilities": {"note": UNREAD}}]);
    seed_settings(
        &data,
        &[("integrations", hosts), ("printer.fleet", fleet.clone())],
    );
    let (runtime, origin, desktop) = start_rust_only(&data).await;
    let cookie = signed_in(&origin, &desktop, 1).await;
    let printer = "/api/v2/printers/p-1";
    let host = "/api/v2/integrations/moon-1";
    let renamed = Some(json!({"name": "Renamed"}));
    for (method, path, body) in [
        ("GET", printer, None),
        ("GET", "/api/v2/printers", None),
        ("GET", CREATED, None),
        ("PATCH", printer, renamed.clone()),
        ("PATCH", host, renamed),
        ("GET", printer, None),
    ] {
        let (code, _, text) = call(&origin, &cookie, method, path, body).await;
        assert_eq!(code, StatusCode::OK, "{method} {path}: {text}");
        for trace in [
            UNREAD,
            "nozzle",
            "notes",
            "poll_seconds",
            "legacy",
            "capabilities",
        ] {
            assert!(
                !text.contains(trace),
                "{method} {path} shows {trace}: {text}"
            );
        }
    }
    shut_down(runtime).await;
    // The stored rows hold what the edits set and every unread key as it was.
    let stored = super::farm_reads::settings_rows(&data).unwrap();
    let mut edited = fleet;
    edited[0]["name"] = json!("Renamed");
    assert!(stored.contains(&edited.to_string()), "{stored}");
    for kept in [
        format!(r#""enabled":false,"poll_seconds":5,"legacy":{{"token":"{UNREAD}"}}}}"#),
        format!(r#"","capabilities":{{"note":"{UNREAD}"}}}}]"#),
        r#""name":"Renamed","config":{"base_url":"http://127.0.0.1:9""#.to_owned(),
    ] {
        assert!(stored.contains(&kept), "{kept} is not in {stored}");
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
                assert!(!text.contains(UNREAD), "{name} holds an unread key's value");
                read += 1;
            }
        }
    }
    assert!(read > 0, "the runtime wrote no file to check");
}
