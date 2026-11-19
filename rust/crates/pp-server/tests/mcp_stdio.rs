//! `pp-server mcp-stdio` (GRE-1332) serves MCP on stdio and forwards it to
//! the running core's HTTP MCP with an API key. With no core, or a key the
//! core refuses, it exits 1 before serving and writes nothing.

#[path = "../../pp-storage/tests/common/process.rs"]
mod child_process;
#[cfg(all(unix, feature = "desktop_rebuild"))]
#[path = "../../pp-core/tests/support/release.rs"]
mod release;
#[cfg(all(unix, feature = "desktop_rebuild"))]
#[path = "support/rust_only.rs"]
mod rust_only;
#[path = "../../pp-storage/tests/common/scratch.rs"]
mod scratch;

use serde_json::{Value, json};
use std::{
    io::{BufRead, BufReader, Write},
    path::Path,
    process::{Child, ExitStatus, Stdio},
    sync::{
        atomic::{AtomicUsize, Ordering},
        mpsc,
    },
    time::{Duration, Instant},
};

const KEY_VARIABLE: &str = "PRINT_PARTNER_API_KEY";
const UNKNOWN_KEY: &str = "ppk_0000000000000000000000000000000000000000";
const WAIT: Duration = Duration::from_secs(30);
const PROXY_VARIABLES: [&str; 8] = [
    "HTTP_PROXY",
    "http_proxy",
    "HTTPS_PROXY",
    "https_proxy",
    "ALL_PROXY",
    "all_proxy",
    "NO_PROXY",
    "no_proxy",
];

/// A loopback port nothing listens on: bound, then released.
fn released_port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

fn server_binary() -> std::path::PathBuf {
    if std::env::var_os("NEXTEST_BIN_EXE_pp-server").is_some() {
        child_process::bin_exe("pp-server")
    } else {
        env!("CARGO_BIN_EXE_pp-server").into()
    }
}

/// A running `pp-server mcp-stdio`, run from `cwd`. Each stdout line arrives
/// on `lines`, read by `reader`. Dropping it kills and reaps the child.
struct Bridge {
    child: Child,
    lines: mpsc::Receiver<String>,
    reader: Option<std::thread::JoinHandle<()>>,
}

impl Bridge {
    /// Inherited proxy settings are removed first, so only `env` can set one.
    fn start(url: &str, key: Option<&str>, cwd: &Path, env: &[(&str, &str)]) -> Self {
        let mut command = child_process::command(server_binary());
        for variable in PROXY_VARIABLES {
            command.env_remove(variable);
        }
        command.envs(env.iter().copied());
        command
            .args(["mcp-stdio", "--url", url])
            .env_remove(KEY_VARIABLE)
            .current_dir(cwd)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        if let Some(key) = key {
            command.env(KEY_VARIABLE, key);
        }
        let mut child = command.spawn().unwrap();
        let stdout = BufReader::new(child.stdout.take().unwrap());
        let (sender, lines) = mpsc::channel();
        let reader = std::thread::spawn(move || {
            for line in stdout.lines().map_while(Result::ok) {
                if sender.send(line).is_err() {
                    break;
                }
            }
        });
        Self {
            child,
            lines,
            reader: Some(reader),
        }
    }

    fn send(&mut self, message: &Value) {
        let stdin = self.child.stdin.as_mut().unwrap();
        writeln!(stdin, "{message}").unwrap();
        stdin.flush().unwrap();
    }

    /// Waits for exit and for stdout to close, then returns the status, the
    /// rest of stdout and stderr.
    fn finish(mut self) -> (ExitStatus, Vec<String>, String) {
        let deadline = Instant::now() + WAIT;
        let status = loop {
            if let Some(status) = self.child.try_wait().unwrap() {
                break status;
            }
            assert!(Instant::now() < deadline, "mcp-stdio did not exit");
            std::thread::sleep(Duration::from_millis(20));
        };
        let mut stderr = String::new();
        std::io::Read::read_to_string(&mut self.child.stderr.take().unwrap(), &mut stderr).unwrap();
        let _ = self.reader.take().map(std::thread::JoinHandle::join);
        (status, self.lines.try_iter().collect(), stderr)
    }
}

impl Drop for Bridge {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn initialize() -> Value {
    json!({
        "protocolVersion": "2025-06-18",
        "capabilities": {},
        "clientInfo": { "name": "gre-1332", "version": "0" },
    })
}

/// Starts a bridge that must refuse: it exits 1, writes no MCP message, says
/// why on stderr, and leaves `cwd` empty.
fn assert_refused(url: &str, key: Option<&str>, reason: &str) {
    assert_refused_with(url, key, &[], reason);
}

/// Returns stderr.
fn assert_refused_with(url: &str, key: Option<&str>, env: &[(&str, &str)], reason: &str) -> String {
    let cwd = scratch::Scratch::new("pp-mcp-stdio-cwd-");
    let mut bridge = Bridge::start(url, key, &cwd, env);
    // A client writes `initialize` at once; the bridge must not answer it.
    bridge.send(
        &json!({ "jsonrpc": "2.0", "id": 1, "method": "initialize", "params": initialize() }),
    );
    let (status, stdout, stderr) = bridge.finish();
    assert_eq!(status.code(), Some(1), "{stderr}");
    assert_eq!(stdout, Vec::<String>::new(), "{stderr}");
    assert!(stderr.starts_with("mcp-stdio: "), "{stderr}");
    assert!(stderr.contains(reason), "{reason} in {stderr}");
    if let Some(key) = key {
        assert!(!stderr.contains(key), "key on stderr");
    }
    assert_eq!(std::fs::read_dir(&*cwd).unwrap().count(), 0, "wrote to cwd");
    stderr
}

#[test]
fn mcp_stdio_refuses_with_no_core_running() {
    let port = released_port();
    let url = format!("http://127.0.0.1:{port}/mcp");
    assert_refused(
        &url,
        Some(UNKNOWN_KEY),
        &format!("No Print Partner core accepted the connection at {url}"),
    );
    assert_refused(
        &url,
        None,
        "PRINT_PARTNER_API_KEY is not set; MCP always needs an API key",
    );
    assert_refused(
        "http://192.0.2.1:8080/mcp",
        Some(UNKNOWN_KEY),
        "--url must use https unless the host is a loopback address",
    );
}

/// A loopback listener that accepts connections, records what each sends
/// and never answers. Returns its port and the bytes received so far.
fn silent_listener() -> (u16, std::sync::Arc<std::sync::Mutex<Vec<u8>>>) {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let received = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let sink = received.clone();
    std::thread::spawn(move || {
        for stream in listener.incoming().map_while(Result::ok) {
            let sink = sink.clone();
            std::thread::spawn(move || {
                let mut stream = stream;
                let mut buffer = [0; 4096];
                while let Ok(read @ 1..) = std::io::Read::read(&mut stream, &mut buffer) {
                    sink.lock().unwrap().extend_from_slice(&buffer[..read]);
                }
            });
        }
    });
    (port, received)
}

#[test]
fn mcp_stdio_gives_up_on_a_listener_that_never_answers() {
    let (port, received) = silent_listener();
    let url = format!("http://127.0.0.1:{port}/mcp");
    let started = Instant::now();
    assert_refused(&url, Some(UNKNOWN_KEY), "no answer within 10 seconds");
    assert!(started.elapsed() < WAIT, "{:?}", started.elapsed());
    // It did reach the listener: the deadline, not a refused connect, ended it.
    assert!(!received.lock().unwrap().is_empty());
}

#[test]
fn mcp_stdio_sends_loopback_http_direct_never_through_a_proxy() {
    let (proxy, received) = silent_listener();
    let proxy = format!("http://127.0.0.1:{proxy}");
    let port = released_port();
    let url = format!("http://127.0.0.1:{port}/mcp");
    let env = [
        ("HTTP_PROXY", proxy.as_str()),
        ("http_proxy", proxy.as_str()),
        ("ALL_PROXY", proxy.as_str()),
    ];
    // Direct, the closed port refuses at once; through the proxy the request
    // and its key would reach the proxy instead.
    assert_refused_with(&url, Some(UNKNOWN_KEY), &env, "it could not connect there");
    let received = received.lock().unwrap();
    assert!(
        received.is_empty(),
        "the proxy got {}",
        String::from_utf8_lossy(&received)
    );
}

/// A loopback listener that answers every request with a 307 redirect to
/// `location`, which keeps the method, body and session header.
fn redirecting_listener(location: String) -> u16 {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    std::thread::spawn(move || {
        for mut stream in listener.incoming().map_while(Result::ok) {
            let mut request = Vec::new();
            let mut buffer = [0; 4096];
            while !request.windows(4).any(|window| window == b"\r\n\r\n") {
                match std::io::Read::read(&mut stream, &mut buffer) {
                    Ok(read @ 1..) => request.extend_from_slice(&buffer[..read]),
                    _ => break,
                }
            }
            let _ = write!(
                stream,
                "HTTP/1.1 307 Temporary Redirect\r\nLocation: {location}\r\n\
                 Content-Length: 0\r\nConnection: close\r\n\r\n"
            );
        }
    });
    port
}

/// Binds `address` (host:port, port 0 allowed), reads one request from each
/// connection, records it, and answers 400 so the bridge does not sit out
/// the handshake deadline.
#[cfg(target_os = "linux")]
fn answering_listener(address: &str) -> (u16, std::sync::Arc<std::sync::Mutex<Vec<u8>>>) {
    let listener = std::net::TcpListener::bind(address).unwrap();
    let port = listener.local_addr().unwrap().port();
    let received = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let sink = received.clone();
    std::thread::spawn(move || {
        for mut stream in listener.incoming().map_while(Result::ok) {
            let sink = sink.clone();
            std::thread::spawn(move || {
                let mut request = Vec::new();
                let mut buffer = [0; 4096];
                while !request.windows(4).any(|window| window == b"\r\n\r\n") {
                    match std::io::Read::read(&mut stream, &mut buffer) {
                        Ok(read @ 1..) => request.extend_from_slice(&buffer[..read]),
                        _ => break,
                    }
                    if request.len() > 65_536 {
                        break;
                    }
                }
                sink.lock().unwrap().extend_from_slice(&request);
                let _ = write!(
                    stream,
                    "HTTP/1.1 400 Bad Request\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
                );
            });
        }
    });
    (port, received)
}

/// Nextest runs each test in its own process, so each compiles its own copy.
#[cfg(target_os = "linux")]
fn dns_interposer() -> String {
    static COMPILED: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    COMPILED
        .get_or_init(|| {
            let library = std::path::PathBuf::from(env!("CARGO_TARGET_TMPDIR"))
                .join(format!("libmcp_stdio_dns-{}.so", std::process::id()));
            let source =
                std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/mcp_stdio_dns.c");
            let status = std::process::Command::new("cc")
                .args(["-shared", "-fPIC", "-o"])
                .arg(&library)
                .arg(&source)
                .status()
                .expect("cc compiles the DNS interposer");
            assert!(status.success(), "cc failed to build {source:?}");
            library.into_os_string().into_string().unwrap()
        })
        .clone()
}

/// `localhost` for plain http is pinned to loopback. A resolver that answers
/// with a different address must receive nothing, and the request that does
/// go out keeps the URL's Host. Fails on 4854265c9: that head dials 127.0.0.2.
#[cfg(target_os = "linux")]
#[test]
fn mcp_stdio_pins_localhost_and_ignores_the_resolver() {
    let library = dns_interposer();
    let (port, pinned) = answering_listener("127.0.0.1:0");
    let (_, diverted) = answering_listener(&format!("127.0.0.2:{port}"));
    let url = format!("http://localhost:{port}/mcp");
    assert_refused_with(
        &url,
        Some(UNKNOWN_KEY),
        &[
            ("LD_PRELOAD", library.as_str()),
            ("PP_MCP_STDIO_DNS", "offbox"),
        ],
        "No Print Partner core accepted",
    );
    let diverted = diverted.lock().unwrap();
    assert!(
        diverted.is_empty(),
        "the resolver's address got {}",
        String::from_utf8_lossy(&diverted)
    );
    let pinned = String::from_utf8_lossy(&pinned.lock().unwrap()).to_ascii_lowercase();
    assert!(
        pinned.contains("host: localhost"),
        "loopback missed the URL host: {pinned}"
    );
    assert!(
        pinned.contains(&UNKNOWN_KEY.to_ascii_lowercase()),
        "loopback missed the key: {pinned}"
    );
}

/// Startup failure must print the refusal and exit while a name lookup is
/// still running. The interposer holds `slow.test` for 20s. Fails on
/// 4854265c9: dropping the runtime waits for that lookup, so the exit lands
/// after the 10s handshake deadline.
#[cfg(target_os = "linux")]
#[test]
fn mcp_stdio_exits_on_startup_failure_while_a_lookup_is_still_running() {
    let library = dns_interposer();
    let started = Instant::now();
    assert_refused_with(
        "https://slow.test/mcp",
        Some(UNKNOWN_KEY),
        &[
            ("LD_PRELOAD", library.as_str()),
            ("PP_MCP_STDIO_DNS", "slow"),
        ],
        "no answer within 10 seconds",
    );
    let elapsed = started.elapsed();
    assert!(
        elapsed < Duration::from_secs(15),
        "startup failure took {elapsed:?}; a name lookup held the exit past the deadline"
    );
}

#[test]
fn mcp_stdio_never_follows_a_redirect_away_from_the_url() {
    // `--url` is the only destination checked; a followed redirect could
    // carry the request body and session header anywhere, plain http included.
    let (target, received) = silent_listener();
    let port = redirecting_listener(format!("http://127.0.0.1:{target}/elsewhere"));
    let url = format!("http://127.0.0.1:{port}/mcp");
    assert_refused(&url, Some(UNKNOWN_KEY), "No Print Partner core accepted");
    let received = received.lock().unwrap();
    assert!(
        received.is_empty(),
        "the redirect target got {}",
        String::from_utf8_lossy(&received)
    );
}

/// A fake key that the echoing core repeats in every failure it can shape.
const ECHOED_KEY: &str = "ppk_echoedfakekey_gre1332_never_print";

/// Failures that repeat the request's `Authorization` header, as a reverse
/// proxy's debug page might, as `name|status|header|body`. `{a}` is that
/// header in every form of [`key_forms`], `{e}` in the forms JSON does not
/// decode back to the key, and `{id}` the JSON-RPC id.
const ECHOES: [&str; 14] = [
    "401 challenge|401 Unauthorized|WWW-Authenticate: Bearer error_description=\"{a}\"|{a}",
    "403 challenge|403 Forbidden|WWW-Authenticate: Bearer error=\"insufficient_scope\", scope=\"{a}\"|{a}",
    "401 body|401 Unauthorized|Content-Type: text/plain|{a}",
    "500 body|500 Internal Server Error|Content-Type: text/plain|failed for {a}",
    r#"500 JSON-RPC error|500 Internal Server Error|Content-Type: application/json|{"jsonrpc":"2.0","id":{id},"error":{"code":-32000,"message":"{a}","data":"{a}"}}"#,
    r#"200 JSON-RPC error|200 OK|Content-Type: application/json|{"jsonrpc":"2.0","id":{id},"error":{"code":-32602,"message":"bad {e}"}}"#,
    r#"isError with a code|200 OK|Content-Type: application/json|{"jsonrpc":"2.0","id":{id},"result":{"isError":true,"content":[{"type":"text","text":"{\"error\":\"{a}\",\"code\":\"not_found\"}"}]}}"#,
    r#"isError with the key as code|200 OK|Content-Type: application/json|{"jsonrpc":"2.0","id":{id},"result":{"isError":true,"content":[{"type":"text","text":"{\"code\":\"{a}\"}"}],"structuredContent":{"echo":"{a}"}}}"#,
    r#"400 JSON|400 Bad Request|Content-Type: application/json|{"echo":"{a}"}"#,
    r#"malformed JSON|200 OK|Content-Type: application/json|{"jsonrpc":"2.0","id":{id},"result":"{a}"#,
    "malformed event|200 OK|Content-Type: text/event-stream|data: {a}\n\n",
    "content type|200 OK|Content-Type: text/html; echo=\"{a}\"|{a}",
    "redirect|307 Temporary Redirect|Location: http://127.0.0.1:9/?auth={a}|",
    "429 Retry-After|429 Too Many Requests|Retry-After: {a}|{a}",
];

/// The `Authorization` header as sent, JSON-escaped, percent-encoded, in
/// base64, and with a mixed-case scheme and an upper-case key.
fn key_forms(header: &str) -> [String; 5] {
    const BASE64: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let base64 = header.as_bytes().chunks(3).flat_map(|chunk| {
        let n = (chunk.iter().enumerate()).fold(0, |n, (i, b)| n | u32::from(*b) << (16 - 8 * i));
        (0..4).map(move |i| match i <= chunk.len() {
            true => char::from(BASE64[(n >> (18 - 6 * i) & 63) as usize]),
            false => '=',
        })
    });
    [
        header.to_owned(),
        header
            .chars()
            .map(|c| format!("\\u{:04x}", u32::from(c)))
            .collect(),
        header.bytes().map(|b| format!("%{b:02X}")).collect(),
        base64.collect(),
        header.replacen("Bearer ", "bEaReR  ", 1).to_uppercase(),
    ]
}

/// Whether `output`, or any string inside it read as JSON, holds the key in
/// any of its forms.
fn leaks(output: &str) -> bool {
    fn strings(text: &str, found: &mut Vec<String>) {
        let mut pending: Vec<Value> = serde_json::from_str(text).into_iter().collect();
        while let Some(value) = pending.pop() {
            match value {
                Value::String(text) => strings(&text, found),
                Value::Array(items) => pending.extend(items),
                Value::Object(fields) => pending.extend(fields.into_iter().map(|(_, v)| v)),
                _ => {}
            }
        }
        found.push(text.to_owned());
    }
    let mut found = Vec::new();
    strings(output, &mut found);
    let forms = key_forms(&format!("Bearer {ECHOED_KEY}"));
    found.iter().any(|text| {
        text.to_uppercase().contains(&ECHOED_KEY.to_uppercase())
            || forms.iter().any(|form| text.contains(form.as_str()))
    })
}

/// One HTTP request: its method, its `Authorization` value and its JSON body.
fn read_request(stream: &mut std::net::TcpStream) -> Option<(String, String, Value)> {
    let mut request = Vec::new();
    let mut buffer = [0; 4096];
    let mut read = |request: &mut Vec<u8>| match std::io::Read::read(stream, &mut buffer) {
        Ok(read @ 1..) => {
            request.extend_from_slice(&buffer[..read]);
            Some(())
        }
        _ => None,
    };
    let end = loop {
        match request.windows(4).position(|window| window == b"\r\n\r\n") {
            Some(end) => break end + 4,
            None => read(&mut request)?,
        }
    };
    let head = String::from_utf8_lossy(&request[..end]).into_owned();
    let header = |name: &str| {
        head.lines().find_map(|line| {
            let (key, value) = line.split_once(':')?;
            key.eq_ignore_ascii_case(name)
                .then(|| value.trim().to_owned())
        })
    };
    let length: usize = header("content-length").map_or(0, |value| value.parse().unwrap());
    while request.len() < end + length {
        read(&mut request)?;
    }
    let body = serde_json::from_slice(&request[end..]).unwrap_or(Value::Null);
    let method = head.split(' ').next().unwrap_or_default().to_owned();
    Some((method, header("authorization").unwrap_or_default(), body))
}

/// A fake core on loopback that answers `initialize` with `echo` when
/// `at_handshake`, or else completes the handshake and answers each later
/// request with `echo`. Returns its port and how many echoes carried the key.
fn echoing_core(echo: &'static str, at_handshake: bool) -> (u16, std::sync::Arc<AtomicUsize>) {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let echoed = std::sync::Arc::new(AtomicUsize::new(0));
    let count = echoed.clone();
    let [_, failure, header, body]: [&str; 4] =
        echo.split('|').collect::<Vec<_>>().try_into().unwrap();
    std::thread::spawn(move || {
        for mut stream in listener.incoming().map_while(Result::ok) {
            let Some((method, authorization, request)) = read_request(&mut stream) else {
                continue;
            };
            let rpc = request["method"].as_str().unwrap_or_default();
            let (status, headers, text) = if method != "POST" {
                ("405 Method Not Allowed", String::new(), String::new())
            } else if rpc.starts_with("notifications/") {
                ("202 Accepted", String::new(), String::new())
            } else if rpc == "initialize" && !at_handshake {
                let result = json!({ "jsonrpc": "2.0", "id": request["id"], "result": {
                    "protocolVersion": "2025-06-18", "capabilities": { "tools": {} },
                    "serverInfo": { "name": "echoing-core", "version": "0" } } });
                let headers = "Content-Type: application/json\r\nMcp-Session-Id: echo\r\n";
                ("200 OK", headers.to_owned(), result.to_string())
            } else {
                if authorization.contains(ECHOED_KEY) {
                    count.fetch_add(1, Ordering::SeqCst);
                }
                let forms = key_forms(&authorization);
                let (raw, encoded) = (forms.join(" "), forms[2..].join(" "));
                let id = request["id"].to_string();
                let fill = |text: &str| {
                    let text = text.replace("{a}", &raw).replace("{e}", &encoded);
                    text.replace("{id}", &id)
                };
                (failure, fill(header) + "\r\n", fill(body))
            };
            let _ = write!(
                stream,
                "HTTP/1.1 {status}\r\n{headers}Content-Length: {}\r\nConnection: close\r\n\r\n{text}",
                text.len()
            );
        }
    });
    (port, echoed)
}

/// `RUST_LOG` asks for every log line; none may carry the key either.
const LOG_EVERYTHING: [(&str, &str); 1] = [("RUST_LOG", "trace")];

/// The core, or something in front of it, repeats the key in a failed
/// handshake. The refusal on stderr still names only a fixed reason.
#[test]
fn mcp_stdio_never_prints_the_key_from_a_failed_handshake() {
    let closed = released_port();
    // A TLS client meets a listener that answers in plain text with the key.
    let plain = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let plain_port = plain.local_addr().unwrap().port();
    std::thread::spawn(move || {
        for mut stream in plain.incoming().map_while(Result::ok) {
            let _ = write!(
                stream,
                "HTTP/1.1 400 Bad Request\r\n\r\nBearer {ECHOED_KEY}"
            );
        }
    });
    let mut cases = vec![
        (
            "connect refused",
            format!("http://127.0.0.1:{closed}/mcp"),
            None,
        ),
        ("TLS", format!("https://127.0.0.1:{plain_port}/mcp"), None),
    ];
    for echo in ECHOES {
        let (port, echoed) = echoing_core(echo, true);
        let name = echo.split('|').next().unwrap();
        cases.push((name, format!("http://127.0.0.1:{port}/mcp"), Some(echoed)));
    }
    let mut leaked = Vec::new();
    for (name, url, echoed) in cases {
        let cwd = scratch::Scratch::new("pp-mcp-stdio-cwd-");
        let bridge = Bridge::start(&url, Some(ECHOED_KEY), &cwd, &LOG_EVERYTHING);
        let (status, stdout, stderr) = bridge.finish();
        println!("{name}: {}", stderr.trim_end());
        assert_eq!(status.code(), Some(1), "{name}: {stderr}");
        assert!(
            stderr.starts_with("mcp-stdio: No Print Partner core accepted"),
            "{name}: {stderr}"
        );
        if let Some(echoed) = echoed {
            assert!(
                echoed.load(Ordering::SeqCst) > 0,
                "{name} never echoed the key"
            );
        }
        if leaks(&stderr) || stdout.iter().any(|line| leaks(line)) {
            leaked.push(name);
        }
    }
    assert_eq!(leaked, Vec::<&str>::new(), "these printed the key");
}

/// The handshake succeeds, then the answer to a tool call repeats the key,
/// as a JSON-RPC error or as a tool result with `isError`. The client gets
/// the failure with a fixed message, and a known refusal code survives.
#[test]
fn mcp_stdio_never_forwards_the_key_from_a_failed_call() {
    let mut leaked = Vec::new();
    for echo in ECHOES {
        let name = echo.split('|').next().unwrap();
        let (port, echoed) = echoing_core(echo, false);
        let cwd = scratch::Scratch::new("pp-mcp-stdio-cwd-");
        let url = format!("http://127.0.0.1:{port}/mcp");
        let mut bridge = Bridge::start(&url, Some(ECHOED_KEY), &cwd, &LOG_EVERYTHING);
        bridge.send(
            &json!({ "jsonrpc": "2.0", "id": 1, "method": "initialize", "params": initialize() }),
        );
        bridge
            .lines
            .recv_timeout(WAIT)
            .expect("initialize answered");
        bridge.send(&json!({ "jsonrpc": "2.0", "method": "notifications/initialized" }));
        bridge.send(&json!({ "jsonrpc": "2.0", "id": 2, "method": "tools/call",
            "params": { "name": "list_plans", "arguments": {} } }));
        let line = bridge
            .lines
            .recv_timeout(WAIT)
            .expect("tools/call answered");
        drop(bridge.child.stdin.take());
        let (_, rest, stderr) = bridge.finish();
        println!("{name}: {line}");
        assert!(
            echoed.load(Ordering::SeqCst) > 0,
            "{name} never echoed the key"
        );
        let reply: Value = serde_json::from_str(&line).unwrap();
        assert_eq!(reply["id"], 2, "{name}: {reply}");
        if [line, stderr]
            .iter()
            .chain(&rest)
            .any(|output| leaks(output))
        {
            leaked.push(name);
            continue;
        }
        let text = reply["result"]["content"][0]["text"]
            .as_str()
            .unwrap_or_default();
        match name {
            "isError with a code" => assert!(text.contains(r#""code":"not_found""#), "{reply}"),
            "isError with the key as code" => assert!(text.contains(r#""code":null"#), "{reply}"),
            _ => assert!(reply["error"]["message"].is_string(), "{name}: {reply}"),
        }
    }
    assert_eq!(leaked, Vec::<&str>::new(), "these forwarded the key");
}

#[cfg(all(unix, feature = "desktop_rebuild"))]
mod with_a_core {
    use super::*;
    use std::{io::Read, net::TcpStream};

    /// Only a bridge with a core behind it answers.
    impl Bridge {
        /// The next JSON-RPC message the bridge writes.
        fn receive(&self) -> Value {
            let line = self.lines.recv_timeout(WAIT).expect("bridge answered");
            serde_json::from_str(&line).unwrap_or_else(|_| panic!("not JSON-RPC: {line}"))
        }

        /// Sends a request and returns its `result`.
        fn request(&mut self, id: u64, method: &str, params: Value) -> Value {
            self.send(&json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params }));
            let reply = self.receive();
            assert_eq!(reply["id"], id, "{reply}");
            reply["result"].clone()
        }

        /// Calls a tool and returns its text content parsed as JSON.
        fn tool(&mut self, id: u64, name: &str, arguments: Value) -> Value {
            let result = self.request(
                id,
                "tools/call",
                json!({ "name": name, "arguments": arguments }),
            );
            assert_ne!(result["isError"], true, "{name}: {result}");
            serde_json::from_str(result["content"][0]["text"].as_str().unwrap()).unwrap()
        }
    }

    /// One HTTP/1.1 exchange with the core; returns status, cookie and body.
    fn exchange(origin: &str, method: &str, path: &str, cookie: &str) -> (u16, String, String) {
        let host = origin.trim_start_matches("http://");
        let mut stream = TcpStream::connect(host).unwrap();
        stream.set_read_timeout(Some(WAIT)).unwrap();
        let body = match path {
            "/auth/register" => r#"{"email":"bridge@example.test","password":"password-1234"}"#,
            "/api/v2/modules/orders" => r#"{"on":true}"#,
            _ => "{}",
        };
        write!(
            stream,
            "{method} {path} HTTP/1.1\r\nHost: {host}\r\nOrigin: {origin}\r\nCookie: {cookie}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        )
        .unwrap();
        let mut response = String::new();
        stream.read_to_string(&mut response).unwrap();
        let status = response[9..12].parse().unwrap();
        let cookie = response
            .lines()
            .find_map(|line| line.strip_prefix("set-cookie: "))
            .map(|value| value.split(';').next().unwrap().to_owned())
            .unwrap_or_default();
        let body = response
            .split_once("\r\n\r\n")
            .map(|(_, body)| body.to_owned())
            .unwrap_or_default();
        (status, cookie, body)
    }

    /// Signs in with a URL from `pp-server bootstrap-url`, the headless
    /// first sign-in, turns the Orders Module on so an assistant may propose
    /// a Build, and creates an API key.
    fn api_key(origin: &str, data: &Path) -> String {
        let output = child_process::command(server_binary())
            .args(["bootstrap-url", "--data"])
            .arg(data)
            .output()
            .unwrap();
        assert!(output.status.success(), "{output:?}");
        let bootstrap = String::from_utf8(output.stdout).unwrap();
        let path = bootstrap.trim().strip_prefix(origin).unwrap();
        let (status, desktop, _) = exchange(origin, "GET", path, "");
        assert_eq!(status, 303);
        let (status, session, body) = exchange(origin, "POST", "/auth/register", &desktop);
        assert_eq!(status, 200, "{body}");
        let cookies = format!("{desktop}; {session}");
        let (status, _, body) = exchange(origin, "PUT", "/api/v2/modules/orders", &cookies);
        assert_eq!(status, 200, "{body}");
        let (status, _, body) = exchange(origin, "POST", "/settings/api-keys", &cookies);
        assert_eq!(status, 201, "{body}");
        let created: Value = serde_json::from_str(&body).unwrap();
        created["key"].as_str().unwrap().to_owned()
    }

    #[test]
    fn mcp_stdio_forwards_to_the_running_core() {
        forwards_to_the_running_core(None);
    }

    /// The Docker image's explicit wildcard listener, which admits only a
    /// literal-IP Host: the bridge run with `docker exec` reaches it this way.
    #[test]
    fn mcp_stdio_forwards_to_an_explicit_listener() {
        forwards_to_the_running_core(Some("0.0.0.0:0"));
    }

    fn forwards_to_the_running_core(listen: Option<&str>) {
        let root = scratch::Scratch::new("pp-mcp-stdio-core-");
        let server = match listen {
            None => rust_only::Server::start(&root),
            Some(_) => rust_only::Server::start_with_listener(&root, listen, None),
        };
        let origin = server.origin().to_owned();
        let key = api_key(&origin, &server.data);
        let cwd = scratch::Scratch::new("pp-mcp-stdio-cwd-");

        for path in ["/mcp", "/api/v1/mcp"] {
            let mut bridge = Bridge::start(&format!("{origin}{path}"), Some(&key), &cwd, &[]);
            let result = bridge.request(1, "initialize", initialize());
            assert_eq!(
                result["serverInfo"]["name"], "print-partner-assistant",
                "{result}"
            );
            assert!(result["capabilities"]["tools"].is_object(), "{result}");
            bridge.send(&json!({ "jsonrpc": "2.0", "method": "notifications/initialized" }));

            let tools = bridge.request(2, "tools/list", json!({}));
            let names: Vec<&str> = tools["tools"]
                .as_array()
                .unwrap()
                .iter()
                .filter_map(|tool| tool["name"].as_str())
                .collect();
            for expected in ["confirm_apply", "list_pending_actions", "list_plans"] {
                assert!(names.contains(&expected), "{expected} in {names:?}");
            }

            // A proposal, its listing and its confirmation share the bridge's
            // one session on the core, and only the confirmation writes.
            let name = format!("Bridge build {}", path.len());
            let proposal = bridge.tool(
                3,
                "propose_create_build",
                json!({ "name": name, "request": "Through stdio." }),
            );
            assert_eq!(proposal["status"], "proposed", "{proposal}");
            let action = proposal["action"]["id"].as_str().unwrap().to_owned();
            let pending = bridge.tool(4, "list_pending_actions", json!({}));
            assert_eq!(pending["actions"][0]["id"], action.as_str(), "{pending}");
            let plans = bridge.tool(5, "list_plans", json!({}));
            assert!(!plans.to_string().contains(&name), "{plans}");
            let applied = bridge.tool(6, "confirm_apply", json!({ "action_id": action }));
            assert_eq!(applied["result"]["name"], name.as_str(), "{applied}");
            let plans = bridge.tool(7, "list_plans", json!({}));
            assert!(plans.to_string().contains(&name), "{plans}");

            // A tool error from the core reaches the client as a tool error.
            let result = bridge.request(
                8,
                "tools/call",
                json!({ "name": "confirm_apply", "arguments": { "action_id": "missing" } }),
            );
            assert_eq!(result["isError"], true, "{result}");

            // Closing stdin ends the bridge cleanly.
            drop(bridge.child.stdin.take());
            let (status, rest, stderr) = bridge.finish();
            assert!(status.success(), "{stderr}");
            assert_eq!(rest, Vec::<String>::new());
            assert!(!stderr.contains(&key), "key on stderr");
        }
        assert_eq!(std::fs::read_dir(&*cwd).unwrap().count(), 0, "wrote to cwd");

        // The core refuses an unknown key, so the bridge refuses to serve.
        let url = format!("{origin}/mcp");
        assert_refused(&url, Some(UNKNOWN_KEY), "No Print Partner core accepted");

        // Once the core stops, the bridge refuses instead of opening its data.
        let data = server.data.clone();
        let (exit, _, _) = server.stop();
        assert!(exit.success());
        let before = listing(&data);
        assert_refused(&url, Some(&key), "No Print Partner core accepted");
        assert_eq!(
            listing(&data),
            before,
            "the bridge touched the data directory"
        );
    }

    /// A core restart forgets the bridge's MCP session and its proposal. The
    /// bridge reports the lost session instead of quietly opening a new one
    /// in which the proposal is gone.
    #[test]
    fn mcp_stdio_reports_a_lost_session_instead_of_replacing_it() {
        let port = released_port();
        let address = format!("127.0.0.1:{port}");
        let root = scratch::Scratch::new("pp-mcp-stdio-restart-");
        let server = rust_only::Server::start_with_listener(&root, Some(&address), None);
        let origin = server.origin().to_owned();
        let key = api_key(&origin, &server.data);
        let cwd = scratch::Scratch::new("pp-mcp-stdio-cwd-");
        let mut bridge = Bridge::start(&format!("{origin}/mcp"), Some(&key), &cwd, &[]);
        bridge.request(1, "initialize", initialize());
        bridge.send(&json!({ "jsonrpc": "2.0", "method": "notifications/initialized" }));
        let proposal = bridge.tool(
            2,
            "propose_create_build",
            json!({ "name": "Before restart", "request": "Lost with the session." }),
        );
        assert_eq!(proposal["status"], "proposed", "{proposal}");

        let (exit, _, _) = server.stop();
        assert!(exit.success());
        let server = rust_only::Server::start_with_listener(&root, Some(&address), None);
        assert_eq!(server.origin(), origin);

        bridge.send(&json!({ "jsonrpc": "2.0", "id": 3, "method": "tools/call",
            "params": { "name": "list_pending_actions", "arguments": {} } }));
        let reply = bridge.receive();
        assert_eq!(reply["id"], 3, "{reply}");
        assert!(reply["result"].is_null(), "a new session answered: {reply}");
        assert!(reply["error"]["message"].is_string(), "{reply}");
        drop(server);
    }

    /// Every entry under `dir` with its length and modification time.
    fn listing(dir: &Path) -> Vec<(std::path::PathBuf, u64, std::time::SystemTime)> {
        let mut entries = Vec::new();
        let mut pending = vec![dir.to_path_buf()];
        while let Some(dir) = pending.pop() {
            for entry in std::fs::read_dir(dir).unwrap() {
                let entry = entry.unwrap();
                let metadata = entry.metadata().unwrap();
                if metadata.is_dir() {
                    pending.push(entry.path());
                }
                entries.push((entry.path(), metadata.len(), metadata.modified().unwrap()));
            }
        }
        entries.sort();
        entries
    }
}
