//! PrusaLink storage browse, cameras and `object_list` (T-22). Replays
//! `fixtures/drivers/prusalink/driver/routes.json` through a simulator that
//! checks every Digest response, and compares results and the request log with
//! `node-oracle-reads.json` from the Node adapter. GRE-1044 adds the
//! streamed print-file download and camera snapshot, 8 MiB listings and the
//! `object_list` prefix through the same simulator. Nothing here talks to a
//! real printer.
use md5::{Digest, Md5};
use pp_drivers::prusalink::Driver;
use pp_net::outbound::{HttpGuard, MAX_BODY, MAX_LISTING_BODY, SystemResolver};
use serde::Deserialize;
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, HashMap},
    path::Path,
    sync::{Arc, Mutex},
    time::Duration,
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpListener,
};

const FIXTURES: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../../fixtures/drivers/prusalink/driver"
);
const USER: &str = "fixture-user";
const PASSWORD: &str = "synthetic-fixture-password";

fn read(name: &str) -> Vec<u8> {
    std::fs::read(Path::new(FIXTURES).join(name)).unwrap()
}

#[derive(Deserialize)]
struct Routes {
    routes: HashMap<String, String>,
    browse: Vec<String>,
    configs: HashMap<String, BTreeMap<String, String>>,
}

fn md5(value: &str) -> String {
    hex::encode(Md5::digest(value.as_bytes()))
}

/// Whether `header` is a valid `qop=auth` MD5 answer for `fixture-nonce`,
/// checked without the driver's code.
fn valid(header: &str, method: &str, path: &str) -> bool {
    let Some(fields) = header.strip_prefix("Digest ") else {
        return false;
    };
    let fields: HashMap<_, _> = fields
        .split(", ")
        .filter_map(|part| part.split_once('='))
        .map(|(k, v)| (k, v.trim_matches('"')))
        .collect();
    let field = |k: &str| fields.get(k).copied().unwrap_or("");
    let ha1 = md5(&format!(
        "{}:{}:{PASSWORD}",
        field("username"),
        field("realm")
    ));
    let ha2 = md5(&format!("{method}:{path}"));
    let expected = md5(&format!(
        "{ha1}:{}:{}:{}:auth:{ha2}",
        field("nonce"),
        field("nc"),
        field("cnonce")
    ));
    field("nonce") == "fixture-nonce" && field("uri") == path && field("response") == expected
}

/// Status, content type and body served for one path.
type Served = HashMap<String, (u16, &'static str, Vec<u8>)>;

/// A printer serving `routes` to valid Digest answers, or `forced` for every
/// request. Returns its origin and request log, formatted as the Node oracle
/// records it.
async fn printer(routes: Served, forced: Option<u16>) -> (String, Arc<Mutex<Vec<String>>>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let origin = format!("http://{}", listener.local_addr().unwrap());
    let log = Arc::new(Mutex::new(Vec::new()));
    let shared = log.clone();
    let routes = Arc::new(routes);
    tokio::spawn(async move {
        loop {
            let (mut socket, _) = listener.accept().await.unwrap();
            let (log, routes) = (shared.clone(), routes.clone());
            tokio::spawn(async move {
                let mut bytes = Vec::new();
                while !bytes.windows(4).any(|w| w == b"\r\n\r\n") {
                    let mut chunk = [0; 4096];
                    let n = socket.read(&mut chunk).await.unwrap_or(0);
                    if n == 0 {
                        return;
                    }
                    bytes.extend_from_slice(&chunk[..n]);
                }
                let head = String::from_utf8(bytes).unwrap();
                let mut lines = head.split("\r\n");
                let line: Vec<_> = lines.next().unwrap().split(' ').collect();
                let (method, path) = (line[0], line[1]);
                let headers: HashMap<String, String> = lines
                    .filter_map(|l| l.split_once(": "))
                    .map(|(k, v)| (k.to_ascii_lowercase(), v.to_owned()))
                    .collect();
                let authorization = headers.get("authorization");
                let range = headers
                    .get("range")
                    .map_or(String::new(), |r| format!(" range={r}"));
                let auth = if authorization.is_some() {
                    " +auth"
                } else {
                    ""
                };
                log.lock()
                    .unwrap()
                    .push(format!("{method} {path}{auth}{range}"));
                let json = "application/json";
                let (status, extra, kind, body) = match forced {
                    Some(status) => (status, String::new(), json, b"{}".to_vec()),
                    None if !authorization.is_some_and(|a| valid(a, method, path)) => {
                        let challenge: Value =
                            serde_json::from_slice(&read("../digest-401.json")).unwrap();
                        let header = challenge["headers"]["www-authenticate"]
                            .as_str()
                            .unwrap()
                            .to_owned();
                        let extra = format!("www-authenticate: {header}\r\n");
                        (401, extra, json, Vec::new())
                    }
                    None => match routes.get(path) {
                        Some((status, kind, body)) => (*status, String::new(), *kind, body.clone()),
                        None => (404, String::new(), json, b"{}".to_vec()),
                    },
                };
                let reply = format!(
                    "HTTP/1.1 {status} X\r\ncontent-type: {kind}\r\ncontent-length: {}\r\n{extra}\r\n",
                    body.len()
                );
                if socket.write_all(reply.as_bytes()).await.is_ok() {
                    let _ = socket.write_all(&body).await;
                }
            });
        }
    });
    (origin, log)
}

fn settle<T: serde::Serialize>(result: anyhow::Result<T>) -> Value {
    match result {
        Ok(value) => json!({ "ok": value }),
        Err(error) => json!({ "error": error.to_string() }),
    }
}

#[tokio::test]
async fn ticket_t_22_reads_match_node_oracle() {
    let routes: Routes = serde_json::from_slice(&read("routes.json")).unwrap();
    let oracle: Value = serde_json::from_slice(&read("node-oracle-reads.json")).unwrap();
    let scenarios = [
        ("recorded", "auto", None),
        ("pinned", "pinned", None),
        ("no-password", "auto", None),
        ("http-500", "auto", Some(500)),
        ("http-404", "auto", Some(404)),
    ];
    for (name, config, forced) in scenarios {
        let served = routes
            .routes
            .iter()
            .map(|(path, name)| (path.clone(), (200, "application/json", read(name))))
            .collect();
        let (origin, log) = printer(served, forced).await;
        let guard = HttpGuard::new(
            Arc::new(SystemResolver),
            Duration::from_secs(5),
            2 * 1024 * 1024,
        )
        .unwrap();
        let password = if name == "no-password" { "" } else { PASSWORD };
        let storage = routes.configs[config].get("storage").map(String::as_str);
        let driver = Driver::new(guard, &format!("{origin}/"), USER, Some(password), storage);
        let mut browse = serde_json::Map::new();
        let cameras = settle(driver.cameras().await);
        let object_list = json!(driver.object_list().await);
        for path in &routes.browse {
            browse.insert(path.clone(), settle(driver.browse(path).await));
        }
        let requests = log.lock().unwrap().clone();
        let actual = json!({
            "cameras": cameras,
            "object_list": object_list,
            "browse": browse,
            "requests": requests,
        });
        assert_eq!(actual, oracle[name], "{name}");
    }
}

fn driver(origin: &str) -> Driver {
    let guard = HttpGuard::new(Arc::new(SystemResolver), Duration::from_secs(5), MAX_BODY).unwrap();
    Driver::new(guard, origin, USER, Some(PASSWORD), Some("usb"))
}

fn json_route(status: u16, body: Value) -> (u16, &'static str, Vec<u8>) {
    (status, "application/json", body.to_string().into_bytes())
}

/// The authorized requests in `log`, without the bodyless challenge probes.
fn authorized(log: &Arc<Mutex<Vec<String>>>) -> Vec<String> {
    let log = log.lock().unwrap();
    log.iter()
        .filter(|l| l.ends_with("+auth"))
        .cloned()
        .collect()
}

#[tokio::test]
async fn ticket_t_22_open_file_streams_the_advertised_download() {
    let file: Vec<u8> = (0..3 * MAX_BODY).map(|i| (i % 251) as u8).collect();
    let routes = Served::from([
        (
            "/api/v1/files/usb/plates/BIG.BGC".into(),
            json_route(
                200,
                json!({"name": "BIG.BGC", "refs": {"download": " /api/v1/files/usb/plates/BIG.BGC/raw "}}),
            ),
        ),
        (
            "/api/v1/files/usb/plates/BIG.BGC/raw".into(),
            (200, "application/octet-stream", file.clone()),
        ),
    ]);
    let (origin, log) = printer(routes, None).await;
    let mut reply = driver(&origin).open_file("/plates/BIG.BGC/").await.unwrap();
    assert_eq!(reply.status, 200);
    assert_eq!(reply.content_length, Some(file.len() as u64));
    assert_eq!(
        reply.content_type.as_deref(),
        Some("application/octet-stream")
    );
    let mut read = Vec::new();
    while let Some(chunk) = reply.body.chunk().await.unwrap() {
        read.extend_from_slice(&chunk);
    }
    assert!(
        read == file,
        "the download streams every byte past the 2 MiB buffer"
    );
    assert_eq!(
        authorized(&log),
        [
            "GET /api/v1/files/usb/plates/BIG.BGC +auth",
            "GET /api/v1/files/usb/plates/BIG.BGC/raw +auth"
        ]
    );
    assert!(log.lock().unwrap().iter().all(|l| l.starts_with("GET ")));
}

#[tokio::test]
async fn ticket_t_22_open_file_refuses_what_node_refuses() {
    let routes = Served::from([
        (
            "/api/v1/files/usb/CROSS.BGC".into(),
            json_route(200, json!({"refs": {"download": "http://192.0.2.10/raw"}})),
        ),
        (
            "/api/v1/files/usb/NOREF.BGC".into(),
            json_route(200, json!({"refs": {"download": "  "}})),
        ),
    ]);
    let (origin, log) = printer(routes, None).await;
    let driver = driver(&origin);
    let error = |result: anyhow::Result<pp_net::outbound::StreamReply>| match result {
        Ok(reply) => format!("status {}", reply.status),
        Err(error) => error.to_string(),
    };
    assert_eq!(
        error(driver.open_file("CROSS.BGC").await),
        "PrusaLink advertised a cross-origin download URL"
    );
    assert_eq!(
        error(driver.open_file("NOREF.BGC").await),
        "PrusaLink did not advertise a download URL"
    );
    for bad in ["", "../x", "a\\b", "a//b"] {
        assert_eq!(
            error(driver.open_file(bad).await),
            "Invalid PrusaLink print-file path",
            "{bad:?}"
        );
    }
    // A metadata reply that is not 2xx comes back for the route to map.
    assert_eq!(error(driver.open_file("MISSING.BGC").await), "status 404");
    assert!(
        log.lock()
            .unwrap()
            .iter()
            .all(|l| !l.contains("192.0.2.10") && !l.ends_with("/raw +auth")),
        "nothing was downloaded"
    );
}

#[tokio::test]
async fn ticket_t_22_open_camera_streams_a_listed_snapshot() {
    let cameras = read("cameras.json");
    let jpeg = b"\xff\xd8\xff\xe0 snapshot".to_vec();
    let routes = Served::from([
        ("/api/v1/cameras".into(), (200, "application/json", cameras)),
        (
            "/api/v1/cameras/cam1/snap".into(),
            (200, "image/jpeg", jpeg.clone()),
        ),
    ]);
    let (origin, log) = printer(routes, None).await;
    let driver = driver(&origin);
    let mut reply = driver.open_camera("cam1").await.unwrap();
    assert_eq!(
        (reply.status, reply.content_type.as_deref()),
        (200, Some("image/jpeg"))
    );
    assert_eq!(reply.body.chunk().await.unwrap().unwrap().to_vec(), jpeg);
    // cam3 is disconnected, "cam2" is listed only as " cam2 ", and Node
    // matches the stored id exactly.
    for id in ["cam3", "cam2", "missing"] {
        let error = driver.open_camera(id).await.err().unwrap();
        assert_eq!(error.to_string(), "Camera not found", "{id}");
    }
    let snaps = authorized(&log)
        .into_iter()
        .filter(|l| l.contains("/snap"))
        .count();
    assert_eq!(snaps, 1, "only the listed camera was opened");
}

#[tokio::test]
async fn ticket_t_22_listings_allow_eight_mib() {
    let children = |count: usize| {
        let rows: Vec<_> = (0..count)
            .map(|i| json!({"name": format!("PLATE{i:07}.BGC"), "type": "PRINT_FILE", "size": i}))
            .collect();
        json!({"name": "usb", "type": "FOLDER", "children": rows})
    };
    let row = children(1).to_string().len();
    let between = (3 * MAX_BODY) / row;
    let over = (MAX_LISTING_BODY / row) * 2;
    let routes = Served::from([
        (
            "/api/v1/files/usb/BIG".into(),
            json_route(200, children(between)),
        ),
        (
            "/api/v1/files/usb/HUGE".into(),
            json_route(200, children(over)),
        ),
    ]);
    let (origin, _) = printer(routes, None).await;
    let driver = driver(&origin);
    let listing = driver.browse("BIG").await.unwrap();
    assert_eq!(listing.entries.len(), between);
    let error = driver.browse("HUGE").await.err().unwrap();
    assert!(error.to_string().contains("oversized"), "{error:#}");
}

#[tokio::test]
async fn ticket_t_22_object_list_reads_only_the_prefix_when_range_is_ignored() {
    let mut file = read("print-file.gcode");
    file.resize(3 * MAX_BODY, b';');
    let job = read("job-objects.json");
    let routes = Served::from([
        ("/api/v1/job".into(), (200, "application/json", job)),
        (
            "/api/v1/files/usb/BENCHY~1.BGC/raw".into(),
            (200, "application/octet-stream", file),
        ),
    ]);
    let (origin, _) = printer(routes, None).await;
    assert_eq!(
        driver(&origin).object_list().await,
        ["Benchy", "Café € sign", "Bracket {x}"],
        "a print file larger than 2 MiB still yields its objects"
    );
}

#[tokio::test]
async fn gre_1165_finished_filament_needs_a_finished_job() {
    let job = |state: &str| json!({"state": state, "file": {"display_name": "A.GCODE"}, "consumed_material": 812.5});
    for (state, expected) in [
        ("FINISHED", Some(("A.GCODE".to_owned(), 812.5))),
        ("PRINTING", None),
    ] {
        let routes = Served::from([("/api/v1/job".into(), json_route(200, job(state)))]);
        let (origin, log) = printer(routes, None).await;
        assert_eq!(
            driver(&origin).finished_filament().await,
            expected,
            "{state}"
        );
        assert_eq!(authorized(&log), ["GET /api/v1/job +auth"], "one read");
    }
}
