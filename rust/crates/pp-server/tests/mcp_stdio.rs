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
    sync::mpsc,
    time::{Duration, Instant},
};

const KEY_VARIABLE: &str = "PRINT_PARTNER_API_KEY";
const UNKNOWN_KEY: &str = "ppk_0000000000000000000000000000000000000000";
const WAIT: Duration = Duration::from_secs(30);
const PROXY_VARIABLES: [&str; 6] = [
    "HTTP_PROXY",
    "http_proxy",
    "ALL_PROXY",
    "all_proxy",
    "NO_PROXY",
    "no_proxy",
];

fn server_binary() -> std::path::PathBuf {
    if std::env::var_os("NEXTEST_BIN_EXE_pp-server").is_some() {
        child_process::bin_exe("pp-server")
    } else {
        env!("CARGO_BIN_EXE_pp-server").into()
    }
}

/// A running `pp-server mcp-stdio`, run from `cwd`. Each stdout line arrives
/// on `lines`. Dropping it kills and reaps the child.
struct Bridge {
    child: Child,
    lines: mpsc::Receiver<String>,
}

impl Bridge {
    fn start(url: &str, key: Option<&str>, cwd: &Path) -> Self {
        Self::start_with(url, key, cwd, &[])
    }

    /// `start` with extra environment. Inherited proxy settings are removed
    /// first, so only `env` can set one.
    fn start_with(url: &str, key: Option<&str>, cwd: &Path, env: &[(&str, &str)]) -> Self {
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
        std::thread::spawn(move || {
            for line in stdout.lines().map_while(Result::ok) {
                if sender.send(line).is_err() {
                    break;
                }
            }
        });
        Self { child, lines }
    }

    fn send(&mut self, message: &Value) {
        let stdin = self.child.stdin.as_mut().unwrap();
        writeln!(stdin, "{message}").unwrap();
        stdin.flush().unwrap();
    }

    /// Waits for exit, then returns the status, the rest of stdout and stderr.
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

fn assert_refused_with(url: &str, key: Option<&str>, env: &[(&str, &str)], reason: &str) {
    let cwd = scratch::Scratch::new("pp-mcp-stdio-cwd-");
    let mut bridge = Bridge::start_with(url, key, &cwd, env);
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
}

#[test]
fn mcp_stdio_refuses_with_no_core_running() {
    // A port nothing listens on: bound, then released.
    let port = std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port();
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
    let port = std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port();
    let url = format!("http://127.0.0.1:{port}/mcp");
    let env = [
        ("HTTP_PROXY", proxy.as_str()),
        ("http_proxy", proxy.as_str()),
        ("ALL_PROXY", proxy.as_str()),
    ];
    // Direct, the closed port refuses at once; through the proxy the request
    // and its key would reach the proxy instead.
    assert_refused_with(&url, Some(UNKNOWN_KEY), &env, "error sending request");
    let received = received.lock().unwrap();
    assert!(
        received.is_empty(),
        "the proxy got {}",
        String::from_utf8_lossy(&received)
    );
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
            let mut bridge = Bridge::start(&format!("{origin}{path}"), Some(&key), &cwd);
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
        let port = std::net::TcpListener::bind("127.0.0.1:0")
            .unwrap()
            .local_addr()
            .unwrap()
            .port();
        let address = format!("127.0.0.1:{port}");
        let root = scratch::Scratch::new("pp-mcp-stdio-restart-");
        let server = rust_only::Server::start_with_listener(&root, Some(&address), None);
        let origin = server.origin().to_owned();
        let key = api_key(&origin, &server.data);
        let cwd = scratch::Scratch::new("pp-mcp-stdio-cwd-");
        let mut bridge = Bridge::start(&format!("{origin}/mcp"), Some(&key), &cwd);
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
