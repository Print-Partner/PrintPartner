//! T-3.
use super::{assert_clean, bootstrap, launch, pid, temporary};
use axum::http::{HeaderMap, StatusCode};
use http_body_util::BodyExt;
use pp_core::CoreRuntime;
use pp_gateway::ownership::cutover::CUTOVER_FILE;
use serde_json::json;
use sha2::{Digest, Sha256};
use std::{
    fs,
    path::{Path, PathBuf},
    time::Duration,
};

const LICENSE: &[u8] = include_bytes!("../../../../../web/apps/server/src/data/legal/LICENSE");
const SUMMARY: &[u8] =
    include_bytes!("../../../../../web/apps/server/src/data/legal/LICENSE-SUMMARY.md");
const FOREIGN: &str = "http://foreign.invalid";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Coding {
    Identity,
    Gzip,
    Br,
}

impl Coding {
    fn token(self) -> Option<&'static str> {
        match self {
            Coding::Identity => None,
            Coding::Gzip => Some("gzip"),
            Coding::Br => Some("br"),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Document {
    License,
    Summary,
}

impl Document {
    fn aliases(self) -> [&'static str; 2] {
        match self {
            Document::License => ["/legal/license", "/api/v1/legal/license"],
            Document::Summary => ["/legal/summary", "/api/v1/legal/summary"],
        }
    }

    fn bytes(self) -> &'static [u8] {
        match self {
            Document::License => LICENSE,
            Document::Summary => SUMMARY,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Framing {
    Length(usize),
    Chunked,
}

#[derive(Clone, Copy, Debug)]
enum Sender {
    Own,
    Absent,
    Foreign,
}

struct Case {
    method: &'static str,
    document: Document,
    accept_encoding: &'static [&'static str],
    coding: Coding,
    vary: &'static str,
    framing: Framing,
}

const fn case(
    method: &'static str,
    document: Document,
    accept_encoding: &'static [&'static str],
    coding: Coding,
    vary: &'static str,
    framing: Framing,
) -> Case {
    Case {
        method,
        document,
        accept_encoding,
        coding,
        vary,
        framing,
    }
}

use Coding::{Br, Gzip, Identity};
use Document::{License, Summary};
use Framing::{Chunked, Length};

const PLAIN: &str = "Origin";
const COMPRESSED: &str = "Origin, accept-encoding";

const CASES: [Case; 22] = [
    case("GET", License, &["gzip"], Gzip, COMPRESSED, Chunked),
    case("GET", License, &["br"], Br, COMPRESSED, Chunked),
    case("GET", License, &[], Identity, PLAIN, Length(19951)),
    case(
        "GET",
        License,
        &["identity"],
        Identity,
        PLAIN,
        Length(19951),
    ),
    case("GET", License, &["deflate"], Identity, PLAIN, Length(19951)),
    case(
        "GET",
        License,
        &["gzip;q=0, br;q=0"],
        Identity,
        PLAIN,
        Length(19951),
    ),
    case("GET", License, &["br, gzip"], Br, COMPRESSED, Chunked),
    case("GET", License, &["gzip, br"], Br, COMPRESSED, Chunked),
    case(
        "GET",
        License,
        &["gzip;q=0.8, br;q=0.2"],
        Gzip,
        COMPRESSED,
        Chunked,
    ),
    case(
        "GET",
        License,
        &["gzip;q=0.5, br;q=0.9"],
        Br,
        COMPRESSED,
        Chunked,
    ),
    case("GET", License, &["br;q=0, gzip"], Gzip, COMPRESSED, Chunked),
    case("GET", License, &["gzip;q=0, br"], Br, COMPRESSED, Chunked),
    case("GET", License, &["br;q=0, *"], Gzip, COMPRESSED, Chunked),
    case("GET", License, &["*"], Gzip, COMPRESSED, Chunked),
    case(
        "GET",
        License,
        &["gzip;q=0.1", "br"],
        Br,
        COMPRESSED,
        Chunked,
    ),
    case(
        "GET",
        License,
        &["br;q=0", "gzip"],
        Gzip,
        COMPRESSED,
        Chunked,
    ),
    // Fastify's HEAD onSend handler runs before @fastify/compress and nulls the
    // payload, so HEAD reports identity metadata whatever the client accepts.
    case("HEAD", License, &["gzip"], Identity, PLAIN, Length(19951)),
    case("HEAD", License, &["br"], Identity, PLAIN, Length(19951)),
    case("HEAD", License, &[], Identity, PLAIN, Length(19951)),
    case("GET", Summary, &["gzip"], Identity, PLAIN, Length(868)),
    case("GET", Summary, &["br"], Identity, PLAIN, Length(868)),
    case("HEAD", Summary, &["br, gzip"], Identity, PLAIN, Length(868)),
];

const DECODER: &str = r#"
const zlib = require("node:zlib");
const chunks = [];
process.stdin.on("data", (chunk) => chunks.push(chunk));
process.stdin.on("end", () => {
  const input = Buffer.concat(chunks);
  const limit = { maxOutputLength: 1 << 20 };
  process.stdout.write(process.env.PP_CODEC === "br"
    ? zlib.brotliDecompressSync(input, limit)
    : zlib.gunzipSync(input, limit));
});
"#;

async fn isolated(name: &str, rebuild: bool) -> Option<PathBuf> {
    if std::env::var("PP_DRAIN_CASE").ok().as_deref() == Some(name) {
        return Some(PathBuf::from(std::env::var_os("PP_DRAIN_DATA").unwrap()));
    }
    let data = temporary("t3-drain-compression");
    let test = format!("drain::compression::{name}");
    let mut child = tokio::process::Command::new(std::env::current_exe().unwrap());
    child
        .args(["--exact", &test, "--nocapture"])
        .env("PP_DRAIN_CASE", name)
        .env("PP_DRAIN_DATA", &data)
        .env("PP_DESKTOP_REBUILD", if rebuild { "1" } else { "0" })
        .kill_on_drop(true);
    let output = tokio::time::timeout(Duration::from_secs(240), child.output())
        .await
        .expect("isolated compression case exceeded four minutes")
        .unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    println!("{stdout}");
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        stdout.contains("test result: ok. 1 passed;"),
        "{test} ran no tests"
    );
    assert!(!data.join(CUTOVER_FILE).exists());
    println!(
        "{}",
        json!({"child": test, "PP_DESKTOP_REBUILD": rebuild, "passed": 1, "cutover_file": false})
    );
    fs::remove_dir_all(data).unwrap();
    None
}

async fn send(
    origin: &str,
    cookie: &str,
    method: &str,
    path: &str,
    sender: Sender,
    accept_encoding: &[&str],
) -> (StatusCode, HeaderMap, Vec<u8>) {
    let authority = origin.strip_prefix("http://").unwrap();
    let exchange = async {
        let socket = tokio::net::TcpStream::connect(authority).await.unwrap();
        let (mut client, connection) =
            hyper::client::conn::http1::handshake(hyper_util::rt::TokioIo::new(socket))
                .await
                .unwrap();
        tokio::spawn(async move {
            let _ = connection.await;
        });
        let mut request = axum::http::Request::builder()
            .method(method)
            .uri(path)
            .header("host", authority)
            .header("cookie", cookie);
        match sender {
            Sender::Own => request = request.header("origin", origin),
            Sender::Foreign => request = request.header("origin", FOREIGN),
            Sender::Absent => {}
        }
        for field in accept_encoding {
            request = request.header("accept-encoding", *field);
        }
        let response = client
            .send_request(request.body(axum::body::Body::empty()).unwrap())
            .await
            .unwrap();
        let (head, body) = response.into_parts();
        let bytes = body.collect().await.unwrap().to_bytes().to_vec();
        (head.status, head.headers, bytes)
    };
    tokio::time::timeout(Duration::from_secs(10), exchange)
        .await
        .unwrap_or_else(|_| panic!("{method} {path} exceeded 10 seconds"))
}

async fn decode(node: &Path, coding: Coding, raw: &[u8]) -> Result<Vec<u8>, String> {
    use tokio::io::AsyncWriteExt;
    let mut child = tokio::process::Command::new(node)
        .args(["-e", DECODER])
        .env_clear()
        .envs(std::env::var_os("SystemRoot").map(|root| ("SystemRoot", root)))
        .env("PP_CODEC", coding.token().unwrap())
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .unwrap();
    let exchange = async move {
        let mut stdin = child.stdin.take().unwrap();
        stdin.write_all(raw).await.unwrap();
        drop(stdin);
        child.wait_with_output().await
    };
    let output = tokio::time::timeout(Duration::from_secs(10), exchange)
        .await
        .expect("codec exceeded 10 seconds")
        .unwrap();
    if output.status.success() {
        Ok(output.stdout)
    } else {
        Err(String::from_utf8_lossy(&output.stderr).into_owned())
    }
}

fn header<'a>(headers: &'a HeaderMap, name: &str) -> Option<&'a str> {
    headers.get(name).map(|value| value.to_str().unwrap())
}

fn sha256(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}

async fn compression_baseline(data: PathBuf, enabled: bool) {
    let node = launch(data.clone()).bundle.node;
    let mut runtime = CoreRuntime::start(launch(data.clone())).await.unwrap();
    assert_eq!(runtime.desktop_rebuild().enabled(), enabled);
    let child = pid(&runtime);
    let cookie = bootstrap(&mut runtime).await;
    let origin = runtime.origin().to_owned();

    let mut refused = 0;
    for method in ["GET", "HEAD"] {
        for path in License.aliases() {
            for sender in [Sender::Absent, Sender::Foreign] {
                let (status, headers, _) =
                    send(&origin, &cookie, method, path, sender, &["gzip"]).await;
                assert_eq!(status, StatusCode::FORBIDDEN, "{method} {path} {sender:?}");
                assert!(!headers.contains_key("access-control-allow-origin"));
                refused += 1;
            }
        }
    }

    let mut observed = Vec::new();
    for case in &CASES {
        for path in case.document.aliases() {
            let (status, headers, raw) = send(
                &origin,
                &cookie,
                case.method,
                path,
                Sender::Own,
                case.accept_encoding,
            )
            .await;
            let label = match header(&headers, "content-encoding") {
                Some("gzip") => Some(Gzip),
                Some("br") => Some(Br),
                _ => None,
            };
            let decoded = match label {
                Some(coding) if !raw.is_empty() => Some(decode(&node, coding, &raw).await),
                _ => None,
            };
            println!(
                "{}",
                json!({
                    "method": case.method,
                    "path": path,
                    "accept_encoding": case.accept_encoding,
                    "status": status.as_u16(),
                    "content_encoding": header(&headers, "content-encoding"),
                    "vary": headers.get_all("vary").iter().map(|v| v.to_str().unwrap()).collect::<Vec<_>>(),
                    "content_type": header(&headers, "content-type"),
                    "cache_control": header(&headers, "cache-control"),
                    "content_length": header(&headers, "content-length"),
                    "transfer_encoding": header(&headers, "transfer-encoding"),
                    "raw_length": raw.len(),
                    "raw_sha256": sha256(&raw),
                    "decoded_sha256": decoded.as_ref().map(|d| d.as_ref().map(|b| sha256(b)).map_err(Clone::clone)),
                })
            );
            observed.push((case, path, status, headers, raw, decoded));
        }
    }

    let mut bodies = Vec::new();
    for (case, path, status, headers, raw, decoded) in &observed {
        let at = format!("{} {path} {:?}", case.method, case.accept_encoding);
        assert_eq!(*status, StatusCode::OK, "{at}");
        assert_eq!(
            header(headers, "content-type"),
            Some("text/plain; charset=utf-8"),
            "{at}"
        );
        assert_eq!(header(headers, "cache-control"), Some("no-store"), "{at}");
        assert_eq!(
            header(headers, "access-control-allow-origin"),
            Some(origin.as_str()),
            "{at}"
        );
        assert_eq!(
            header(headers, "access-control-allow-credentials"),
            Some("true"),
            "{at}"
        );
        assert_eq!(
            header(headers, "content-encoding"),
            case.coding.token(),
            "{at}"
        );
        let vary: Vec<_> = headers
            .get_all("vary")
            .iter()
            .map(|v| v.to_str().unwrap())
            .collect();
        assert_eq!(vary.join(", "), case.vary, "{at}");
        match case.framing {
            Length(length) => {
                assert_eq!(
                    header(headers, "content-length"),
                    Some(length.to_string().as_str()),
                    "{at}"
                );
                assert_eq!(header(headers, "transfer-encoding"), None, "{at}");
            }
            Chunked => {
                assert_eq!(header(headers, "content-length"), None, "{at}");
                let expected = (case.method == "GET").then_some("chunked");
                assert_eq!(header(headers, "transfer-encoding"), expected, "{at}");
            }
        }
        let expected = case.document.bytes();
        if case.method == "HEAD" {
            assert!(raw.is_empty(), "{at}");
            assert!(decoded.is_none(), "{at}");
        } else if case.coding == Identity {
            assert_eq!(raw.as_slice(), expected, "{at}");
            assert!(decoded.is_none(), "{at}");
        } else {
            assert_ne!(raw.as_slice(), expected, "{at}");
            assert_eq!(decoded.as_ref().unwrap().as_deref(), Ok(expected), "{at}");
            if let Length(length) = case.framing {
                assert_eq!(raw.len(), length, "{at}");
            }
            if *path == "/legal/license" && case.accept_encoding.len() == 1 {
                bodies.push((case.coding, raw.clone()));
            }
        }
    }

    let gzip = &bodies.iter().find(|(coding, _)| *coding == Gzip).unwrap().1;
    let br = &bodies.iter().find(|(coding, _)| *coding == Br).unwrap().1;
    assert!(decode(&node, Br, gzip).await.is_err(), "gzip labelled br");
    assert!(decode(&node, Gzip, br).await.is_err(), "br labelled gzip");
    for (coding, raw) in [(Gzip, gzip), (Br, br)] {
        let mut corrupt = raw.clone();
        corrupt[raw.len() / 2] ^= 0x55;
        assert_ne!(
            decode(&node, coding, &corrupt).await.as_deref(),
            Ok(LICENSE),
            "corrupt {coding:?} body"
        );
    }

    assert_eq!(refused, 8);
    assert_eq!(observed.len(), 44);
    assert_eq!(observed.iter().filter(|o| o.5.is_some()).count(), 24);
    assert!(!data.join(CUTOVER_FILE).exists());
    let receipt = runtime.shutdown().await;
    assert_clean(&receipt, child);
    println!(
        "{}",
        json!({
            "compiled_desktop_rebuild": cfg!(feature = "desktop_rebuild"),
            "desktop_rebuild": enabled,
            "license_sha256": sha256(LICENSE),
            "summary_sha256": sha256(SUMMARY),
            "refused": refused,
            "representations": observed.len(),
            "decoded": observed.iter().filter(|o| o.5.is_some()).count(),
            "negative_controls": 4,
            "shutdown": receipt,
            "compat_process_group_absent": true,
        })
    );
}

#[tokio::test]
async fn ticket_t_3_drain_compression_runtime_off() {
    let Some(data) = isolated("ticket_t_3_drain_compression_runtime_off", false).await else {
        return;
    };
    compression_baseline(data, false).await;
}

#[cfg(feature = "desktop_rebuild")]
#[tokio::test]
async fn ticket_t_3_drain_compression_runtime_on() {
    let Some(data) = isolated("ticket_t_3_drain_compression_runtime_on", true).await else {
        return;
    };
    compression_baseline(data, true).await;
}
