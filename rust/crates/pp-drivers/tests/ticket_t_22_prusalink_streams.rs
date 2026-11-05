//! GRE-1044: PrusaLink print-file download, camera snapshot, 8 MiB listings
//! and the `object_list` prefix, through a loopback simulator that checks
//! every Digest answer. Nothing here talks to a real printer.
use md5::{Digest, Md5};
use pp_drivers::prusalink::Driver;
use pp_net::outbound::{HttpGuard, MAX_BODY, MAX_LISTING_BODY, SystemResolver};
use serde_json::{Value, json};
use std::{
    collections::HashMap,
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
    "/../../../fixtures/drivers/prusalink"
);
const USER: &str = "fixture-user";
const PASSWORD: &str = "synthetic-fixture-password";

fn md5(value: &str) -> String {
    hex::encode(Md5::digest(value.as_bytes()))
}

/// Whether `header` is a valid `qop=auth` MD5 answer for `fixture-nonce`.
fn valid(header: &str, path: &str) -> bool {
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
    let ha2 = md5(&format!("GET:{path}"));
    let expected = md5(&format!(
        "{ha1}:{}:{}:{}:auth:{ha2}",
        field("nonce"),
        field("nc"),
        field("cnonce")
    ));
    field("nonce") == "fixture-nonce" && field("uri") == path && field("response") == expected
}

type Routes = HashMap<String, (u16, &'static str, Vec<u8>)>;

/// A printer answering authorized GETs from `routes` (status, content type,
/// body) and 404 otherwise. Returns its origin and `GET path [+auth]` log.
async fn printer(routes: Routes) -> (String, Arc<Mutex<Vec<String>>>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let origin = format!("http://{}", listener.local_addr().unwrap());
    let log = Arc::new(Mutex::new(Vec::new()));
    let (shared, routes) = (log.clone(), Arc::new(routes));
    let challenge: Value = serde_json::from_slice(
        &std::fs::read(Path::new(FIXTURES).join("digest-401.json")).unwrap(),
    )
    .unwrap();
    let challenge = challenge["headers"]["www-authenticate"]
        .as_str()
        .unwrap()
        .to_owned();
    tokio::spawn(async move {
        loop {
            let (mut socket, _) = listener.accept().await.unwrap();
            let (log, routes, challenge) = (shared.clone(), routes.clone(), challenge.clone());
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
                let head = String::from_utf8_lossy(&bytes).into_owned();
                let mut lines = head.split("\r\n");
                let line: Vec<_> = lines.next().unwrap().split(' ').collect();
                let (method, path) = (line[0].to_owned(), line[1].to_owned());
                let headers: HashMap<String, String> = lines
                    .filter_map(|l| l.split_once(": "))
                    .map(|(k, v)| (k.to_ascii_lowercase(), v.to_owned()))
                    .collect();
                let authorization = headers.get("authorization");
                let auth = if authorization.is_some() {
                    " +auth"
                } else {
                    ""
                };
                log.lock().unwrap().push(format!("{method} {path}{auth}"));
                let (status, extra, kind, body) = if !authorization.is_some_and(|a| valid(a, &path))
                {
                    (
                        401,
                        format!("www-authenticate: {challenge}\r\n"),
                        "application/json",
                        Vec::new(),
                    )
                } else {
                    match routes.get(&path) {
                        Some((status, kind, body)) => (*status, String::new(), *kind, body.clone()),
                        None => (404, String::new(), "application/json", b"{}".to_vec()),
                    }
                };
                let reply = format!(
                    "HTTP/1.1 {status} X\r\ncontent-type: {kind}\r\ncontent-length: {}\r\n{extra}\r\n",
                    body.len()
                );
                if socket.write_all(reply.as_bytes()).await.is_err() {
                    return;
                }
                let _ = socket.write_all(&body).await;
            });
        }
    });
    (origin, log)
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
    let routes = Routes::from([
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
    let (origin, log) = printer(routes).await;
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
    let routes = Routes::from([
        (
            "/api/v1/files/usb/CROSS.BGC".into(),
            json_route(200, json!({"refs": {"download": "http://192.0.2.10/raw"}})),
        ),
        (
            "/api/v1/files/usb/NOREF.BGC".into(),
            json_route(200, json!({"refs": {"download": "  "}})),
        ),
    ]);
    let (origin, log) = printer(routes).await;
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
    let cameras = std::fs::read(Path::new(FIXTURES).join("driver/cameras.json")).unwrap();
    let jpeg = b"\xff\xd8\xff\xe0 snapshot".to_vec();
    let routes = Routes::from([
        ("/api/v1/cameras".into(), (200, "application/json", cameras)),
        (
            "/api/v1/cameras/cam1/snap".into(),
            (200, "image/jpeg", jpeg.clone()),
        ),
    ]);
    let (origin, log) = printer(routes).await;
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
    let routes = Routes::from([
        (
            "/api/v1/files/usb/BIG".into(),
            json_route(200, children(between)),
        ),
        (
            "/api/v1/files/usb/HUGE".into(),
            json_route(200, children(over)),
        ),
    ]);
    let (origin, _) = printer(routes).await;
    let driver = driver(&origin);
    let listing = driver.browse("BIG").await.unwrap();
    assert_eq!(listing.entries.len(), between);
    let error = driver.browse("HUGE").await.err().unwrap();
    assert!(error.to_string().contains("oversized"), "{error:#}");
}

#[tokio::test]
async fn ticket_t_22_object_list_reads_only_the_prefix_when_range_is_ignored() {
    let mut file = std::fs::read(Path::new(FIXTURES).join("driver/print-file.gcode")).unwrap();
    file.resize(3 * MAX_BODY, b';');
    let job = std::fs::read(Path::new(FIXTURES).join("driver/job-objects.json")).unwrap();
    let routes = Routes::from([
        ("/api/v1/job".into(), (200, "application/json", job)),
        (
            "/api/v1/files/usb/BENCHY~1.BGC/raw".into(),
            (200, "application/octet-stream", file),
        ),
    ]);
    let (origin, _) = printer(routes).await;
    assert_eq!(
        driver(&origin).object_list().await,
        ["Benchy", "Café € sign", "Bracket {x}"],
        "a print file larger than 2 MiB still yields its objects"
    );
}
