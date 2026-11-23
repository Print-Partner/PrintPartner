//! GRE-1300's acceptance calls through a running Rust-only `pp-server`, the
//! start the Docker image makes: `propose_set_build_source_roles` and
//! `propose_import_build_inputs` on `/api/v1/mcp` and `/mcp`, with an API key
//! and no cookie. State is read back through the REST routes the UI reads.
#![cfg(all(unix, feature = "desktop_rebuild"))]

#[path = "../../pp-storage/tests/common/process.rs"]
mod child_process;
#[path = "../../pp-api/tests/support/mcp_reply.rs"]
mod mcp_reply;
#[path = "../../pp-core/tests/support/release.rs"]
mod release;
#[path = "support/rust_only.rs"]
mod rust_only;

use reqwest::Method;
use serde_json::{Value, json};
use std::os::unix::fs::PermissionsExt;

const ROLES: &str = "propose_set_build_source_roles";
const IMPORT: &str = "propose_import_build_inputs";
/// A selected draft written into a brief; each confirm must clear it.
const DRAFT: i64 = 7;
const UNKNOWN_KEY: &str = "ppk_0000000000000000000000000000000000000000";

/// A signed-in browser session, used only to set up and read back state, and
/// an API key, the only credential the MCP calls carry.
struct Gateway {
    server: rust_only::Server,
    http: reqwest::Client,
    origin: String,
    cookies: String,
    key: String,
}

/// One MCP session on `path`.
struct Mcp<'a> {
    gateway: &'a Gateway,
    path: &'static str,
    session: String,
    id: u64,
}

fn first_cookie(response: &reqwest::Response) -> String {
    let header = response.headers().get("set-cookie").expect("a cookie");
    let value = header.to_str().unwrap();
    value.split(';').next().unwrap().to_owned()
}

impl Gateway {
    /// Signs in with `pp-server bootstrap-url`, lowers the import reserve to
    /// 1 MiB because the data directory sits on /tmp, which may have less
    /// than the 5 GiB default free, turns the Orders Module on so an
    /// assistant may propose a Build, and creates an API key.
    async fn open(server: rust_only::Server) -> Self {
        let origin = server.origin().to_owned();
        let http = reqwest::Client::builder()
            .no_proxy()
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .unwrap();
        let output = child_process::command(rust_only::server_binary())
            .args(["bootstrap-url", "--data"])
            .arg(&server.data)
            .output()
            .unwrap();
        assert!(output.status.success(), "{output:?}");
        let url = String::from_utf8(output.stdout).unwrap();
        let bootstrap = http.get(url.trim()).send().await.unwrap();
        assert_eq!(bootstrap.status(), 303);
        let mut gateway = Self {
            cookies: first_cookie(&bootstrap),
            server,
            http,
            origin,
            key: String::new(),
        };
        let register = json!({ "email": "gateway@example.test", "password": "password-1234" });
        let response = gateway
            .send(Method::POST, "/auth/register", Some(register))
            .await;
        assert_eq!(response.status(), 200);
        gateway.cookies = format!("{}; {}", gateway.cookies, first_cookie(&response));
        let reserve = json!({ "reserve_bytes": 1_048_576 });
        let (status, body) = gateway
            .rest(
                Method::PUT,
                "/api/v1/settings/import-free-space-reserve",
                Some(reserve),
            )
            .await;
        assert_eq!(status, 200, "{body}");
        assert_eq!(body["reserve_bytes"].as_u64(), Some(1_048_576));
        let on = json!({ "on": true });
        let (status, body) = gateway
            .rest(Method::PUT, "/api/v2/modules/orders", Some(on))
            .await;
        assert_eq!(status, 200, "{body}");
        let (status, body) = gateway
            .rest(Method::POST, "/settings/api-keys", Some(json!({})))
            .await;
        assert_eq!(status, 201, "{body}");
        gateway.key = body["key"].as_str().unwrap().to_owned();
        gateway
    }

    async fn send(&self, method: Method, path: &str, body: Option<Value>) -> reqwest::Response {
        let mut request = self
            .http
            .request(method, format!("{}{path}", self.origin))
            .header("origin", &self.origin)
            .header("cookie", &self.cookies);
        if let Some(body) = body {
            request = request
                .header("content-type", "application/json")
                .body(body.to_string());
        }
        request.send().await.unwrap()
    }

    /// A REST call with the browser session; returns status and JSON body.
    async fn rest(&self, method: Method, path: &str, body: Option<Value>) -> (u16, Value) {
        let response = self.send(method, path, body).await;
        let status = response.status().as_u16();
        let text = response.text().await.unwrap();
        (status, serde_json::from_str(&text).unwrap_or(Value::Null))
    }

    async fn get(&self, path: &str) -> Value {
        let (status, body) = self.rest(Method::GET, path, None).await;
        assert_eq!(status, 200, "GET {path}: {body}");
        body
    }

    /// A JSON-RPC message to the MCP endpoint `path`, with `key` and no cookie.
    async fn post_mcp(
        &self,
        path: &str,
        key: Option<&str>,
        session: Option<&str>,
        message: &Value,
    ) -> (u16, String, Option<String>) {
        let mut request = self
            .http
            .post(format!("{}{path}", self.origin))
            .header("origin", &self.origin)
            .header("content-type", "application/json")
            .header("accept", "application/json, text/event-stream")
            .body(message.to_string());
        if let Some(key) = key {
            request = request.header("authorization", format!("Bearer {key}"));
        }
        if let Some(session) = session {
            request = request.header("mcp-session-id", session);
        }
        mcp_reply::send(request).await
    }

    async fn mcp(&self, path: &'static str) -> Mcp<'_> {
        let initialize = json!({ "jsonrpc": "2.0", "id": 0, "method": "initialize", "params": {
            "protocolVersion": "2025-06-18", "capabilities": {},
            "clientInfo": { "name": "gateway-test", "version": "1" } } });
        let (status, body, session) = self
            .post_mcp(path, Some(&self.key), None, &initialize)
            .await;
        assert_eq!(status, 200, "{body}");
        let session = session.expect("an MCP session");
        let initialized = json!({ "jsonrpc": "2.0", "method": "notifications/initialized" });
        let (status, body, _) = self
            .post_mcp(path, Some(&self.key), Some(&session), &initialized)
            .await;
        assert_eq!(status, 202, "{body}");
        Mcp {
            gateway: self,
            path,
            session,
            id: 0,
        }
    }

    /// B's planning as `GET /plans/{id}/build-planning` serves it.
    async fn planning(&self, build: u64) -> Value {
        self.get(&format!("/api/v2/plans/{build}/build-planning"))
            .await
    }

    async fn brief(&self, build: u64) -> Value {
        self.planning(build).await["planning"]["brief"].clone()
    }

    /// The Library as `GET /sources` lists it.
    async fn sources(&self) -> Vec<Value> {
        let listed = self.get("/api/v1/sources").await;
        listed["sources"].as_array().expect("a Source list").clone()
    }

    async fn create_source(&self, name: &str, url: &str, role: &str) -> i64 {
        let source = json!({ "name": name, "url": url, "role": role });
        let (status, body) = self
            .rest(Method::POST, "/api/v1/sources", Some(source))
            .await;
        assert!(status == 200 || status == 201, "{status}: {body}");
        body["id"].as_i64().unwrap()
    }

    /// A local Source with role `mod` and one uploaded STL, imported: it has
    /// a revision, `last_synced_at` and `last_commit_sha`. No network.
    async fn synced_source(&self, name: &str) -> i64 {
        let source = json!({ "name": name, "source_kind": "local", "role": "mod" });
        let (status, body) = self
            .rest(Method::POST, "/api/v1/sources", Some(source))
            .await;
        assert!(status == 200 || status == 201, "{status}: {body}");
        let id = body["id"].as_i64().unwrap();
        let boundary = "pp-gateway-boundary";
        let body = format!(
            "--{boundary}\r\nContent-Disposition: form-data; name=\"file\"; filename=\"kit/part.stl\"\r\n\
             Content-Type: application/octet-stream\r\n\r\nsolid part\nendsolid part\n\r\n--{boundary}--\r\n"
        );
        let response = self
            .http
            .post(format!("{}/api/v1/sources/{id}/upload-files", self.origin))
            .header("origin", &self.origin)
            .header("cookie", &self.cookies)
            .header(
                "content-type",
                format!("multipart/form-data; boundary={boundary}"),
            )
            .body(body)
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), 202);
        let accepted: Value = serde_json::from_str(&response.text().await.unwrap()).unwrap();
        let job = format!("/api/v1/jobs/{}", accepted["job_id"].as_str().unwrap());
        let mut status = Value::Null;
        for _ in 0..600 {
            status = self.get(&job).await["status"].clone();
            if status != "pending" && status != "running" {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        }
        assert_eq!(status, "done", "the upload imported");
        id
    }

    /// What must not change: the Library, Build `build`'s layers and its
    /// Accepted Plan.
    async fn untouched(&self, build: u64) -> Value {
        let detail = self.get(&format!("/api/v2/plans/{build}")).await;
        json!({
            "sources": self.sources().await,
            "layers": self.get(&format!("/api/v2/plans/{build}/layers")).await,
            "accepted": detail["accepted_progress"].clone(),
        })
    }

    /// Everything a propose or a refused confirm must leave alone: `build`'s
    /// planning, the Library, its layers and its Accepted Plan.
    async fn observed(&self, build: u64) -> (Value, Value) {
        (self.planning(build).await, self.untouched(build).await)
    }

    fn database(&self) -> std::path::PathBuf {
        self.server.data.join("print-partner.db")
    }

    /// Selects draft D in `build`'s stored brief. No route selects a draft
    /// without Sources to prepare one from, so this writes the row.
    fn select_draft(&self, build: u64) {
        let key = format!("build_planning.v1.{build}");
        let mut brief = pp_testkit::mcp_planning::setting(&self.database(), &key).unwrap();
        brief["draft_id"] = json!(DRAFT);
        pp_testkit::mcp_planning::save(&self.database(), &key, &brief).unwrap();
    }
}

impl Mcp<'_> {
    async fn request(&mut self, method: &str, params: Value) -> Value {
        self.id += 1;
        let message =
            json!({ "jsonrpc": "2.0", "id": self.id, "method": method, "params": params });
        let key = Some(self.gateway.key.as_str());
        let (status, body, _) = self
            .gateway
            .post_mcp(self.path, key, Some(&self.session), &message)
            .await;
        assert_eq!(status, 200, "{body}");
        mcp_reply::rpc(&body)["result"].clone()
    }

    async fn tools(&mut self) -> Vec<String> {
        let listed = self.request("tools/list", json!({})).await;
        let tools = listed["tools"].as_array().unwrap();
        tools
            .iter()
            .map(|tool| tool["name"].as_str().unwrap().to_owned())
            .collect()
    }

    /// Calls `name`; returns its JSON answer and whether it is a tool error.
    async fn call(&mut self, name: &str, arguments: Value) -> (Value, bool) {
        let params = json!({ "name": name, "arguments": arguments });
        let result = self.request("tools/call", params).await;
        let text = result["content"][0]["text"].as_str().unwrap();
        (
            serde_json::from_str(text).unwrap(),
            result["isError"] == true,
        )
    }

    async fn pending(&mut self) -> Vec<Value> {
        let (listed, _) = self.call("list_pending_actions", json!({})).await;
        listed["actions"].as_array().unwrap().clone()
    }

    /// Proposes; checks that propose changed nothing `observed` reads, and
    /// returns the action id.
    async fn proposed(&mut self, build: u64, name: &str, arguments: Value) -> String {
        let before = self.gateway.observed(build).await;
        let (proposal, error) = self.call(name, arguments).await;
        assert!(!error, "{proposal}");
        assert_eq!(proposal["status"], "proposed", "{proposal}");
        let after = self.gateway.observed(build).await;
        assert_eq!(after, before, "propose wrote nothing");
        proposal["action"]["id"].as_str().unwrap().to_owned()
    }

    async fn confirm(&mut self, action: &str) -> (Value, bool) {
        self.call("confirm_apply", json!({ "action_id": action }))
            .await
    }

    /// Proposes and confirms; returns the confirmed result.
    async fn applied(&mut self, build: u64, name: &str, arguments: Value) -> Value {
        let action = self.proposed(build, name, arguments).await;
        let (applied, error) = self.confirm(&action).await;
        assert!(!error && applied["ok"] == true, "{applied}");
        applied["result"].clone()
    }

    /// `name` is refused at propose with `code`, proposes nothing, and
    /// changes nothing `observed` reads for `build`, its brief included.
    async fn refused_at_propose(&mut self, build: u64, name: &str, arguments: Value, code: &str) {
        let pending = self.pending().await.len();
        let before = self.gateway.observed(build).await;
        let (refused, error) = self.call(name, arguments.clone()).await;
        assert!(error, "{arguments}: {refused}");
        assert_eq!(refused["code"], code, "{arguments}: {refused}");
        assert_eq!(self.pending().await.len(), pending, "{arguments}");
        assert_eq!(self.gateway.observed(build).await, before, "{arguments}");
    }

    /// Confirm is refused with `code`, and nothing `observed` reads changes.
    async fn refused_at_confirm(&mut self, build: u64, action: &str, code: &str) -> Value {
        let before = self.gateway.observed(build).await;
        let (refused, error) = self.confirm(action).await;
        assert!(error, "{refused}");
        assert_eq!(
            (&refused["ok"], &refused["code"]),
            (&json!(false), &json!(code))
        );
        let after = self.gateway.observed(build).await;
        assert_eq!(after, before, "{code}: nothing written");
        refused
    }

    /// A Build made by `propose_create_build`, which gives it a brief.
    async fn build(&mut self, name: &str) -> u64 {
        let args = json!({ "name": name, "request": "Voron 2.4, black ABS" });
        let (proposal, error) = self.call("propose_create_build", args).await;
        assert!(!error, "{proposal}");
        let action = proposal["action"]["id"].as_str().unwrap().to_owned();
        let (created, error) = self.confirm(&action).await;
        assert!(!error, "{created}");
        created["result"]["plan_id"].as_u64().unwrap()
    }
}

fn evidence_for(brief: &Value, source: i64) -> Vec<Value> {
    let items = brief["evidence"].as_array().unwrap();
    let mine = items.iter().filter(|item| item["source_id"] == source);
    mine.cloned().collect()
}

/// Source `id` in a `GET /sources` list, checked synced: it has a
/// revision, `last_synced_at` and `last_commit_sha`.
fn synced(listed: &[Value], id: i64) -> Value {
    let found = source(listed, id);
    let fields = [
        "current_source_revision_id",
        "last_synced_at",
        "last_commit_sha",
    ];
    for field in fields {
        assert!(!found[field].is_null(), "{field}: {found}");
    }
    found
}

/// Source `id` in a `GET /sources` list.
fn source(listed: &[Value], id: i64) -> Value {
    let found = listed.iter().find(|source| source["id"] == id);
    found
        .unwrap_or_else(|| panic!("Source {id} in {listed:?}"))
        .clone()
}

fn runtime() -> tokio::runtime::Runtime {
    tokio::runtime::Runtime::new().unwrap()
}

/// Build B and its Library: Sources S1 (`mod`), S2 (`overlay`) and S3
/// (`mod`, synced from an upload), a base layer of B on S1, and B's brief
/// with evidence E1 (S1) and E2 (S2), neither with a role.
struct Seeded {
    build: u64,
    sources: [i64; 3],
    evidence: [String; 2],
    /// The Library, B's layers and B's Accepted Plan (none) once seeded.
    invariant: Value,
}

async fn seed(gateway: &Gateway) -> Seeded {
    let mut mcp = gateway.mcp("/api/v1/mcp").await;
    let build = mcp.build("Voron for Sam").await;
    let mut sources = [0; 3];
    for (index, (name, role)) in [("S1", "mod"), ("S2", "overlay")].into_iter().enumerate() {
        let url = format!("https://github.com/example/{}", name.to_lowercase());
        sources[index] = gateway.create_source(name, &url, role).await;
    }
    sources[2] = gateway.synced_source("S3").await;
    let base = json!({ "project_id": sources[0] });
    let path = format!("/api/v2/plans/{build}/layers/base");
    let (status, body) = gateway.rest(Method::PUT, &path, Some(base)).await;
    assert_eq!(status, 200, "{body}");
    let inputs = json!([
        { "source_id": sources[0], "kind": "canonical_design" },
        { "source_id": sources[1], "kind": "canonical_design" },
    ]);
    let args = json!({ "plan_id": build, "inputs": inputs });
    let imported = mcp.applied(build, IMPORT, args).await;
    let id = |index: usize| {
        imported["evidence"][index]["id"]
            .as_str()
            .unwrap()
            .to_owned()
    };
    let evidence = [id(0), id(1)];
    let brief = gateway.brief(build).await;
    for item in brief["evidence"].as_array().unwrap() {
        assert!(item["source_role"].is_null(), "no role yet: {item}");
    }
    let invariant = gateway.untouched(build).await;
    assert_eq!(
        invariant["accepted"]["kind"], "empty",
        "no Accepted Plan: {invariant}"
    );
    Seeded {
        build,
        sources,
        evidence,
        invariant,
    }
}

impl Seeded {
    /// B's layers and Accepted Plan are as seeded.
    async fn build_kept(&self, gateway: &Gateway) {
        let now = gateway.untouched(self.build).await;
        assert_eq!(now["layers"], self.invariant["layers"], "B's layers");
        assert_eq!(now["accepted"], self.invariant["accepted"], "Accepted Plan");
    }

    fn roles(&self, first: &str, second: &str) -> Value {
        let [e1, e2] = &self.evidence;
        json!({ "plan_id": self.build, "roles": [
            { "evidence_id": e1, "role": first },
            { "evidence_id": e2, "role": second },
        ] })
    }

    fn inputs(&self, inputs: Value) -> Value {
        json!({ "plan_id": self.build, "inputs": inputs })
    }
}

/// A disposable directory under /tmp, so the server's bootstrap socket path
/// fits `sun_path` however long `TMPDIR` is. Mode 0700, which the client
/// requires of a data directory.
fn short_root() -> tempfile::TempDir {
    tempfile::Builder::new()
        .prefix("ppg-")
        .permissions(std::fs::Permissions::from_mode(0o700))
        .tempdir_in("/tmp")
        .unwrap()
}

/// Starts the server on a disposable directory, signs in and seeds B.
fn open(root: &std::path::Path) -> (tokio::runtime::Runtime, Gateway, Seeded) {
    let server = rust_only::Server::start(root);
    let runtime = runtime();
    let (gateway, seeded) = runtime.block_on(async {
        let gateway = Gateway::open(server).await;
        let seeded = seed(&gateway).await;
        (gateway, seeded)
    });
    (runtime, gateway, seeded)
}

impl Gateway {
    /// SIGTERM; the server stops cleanly and never logged the API key.
    fn stop(self) {
        let (exit, printed, logged) = self.server.stop();
        assert!(exit.success(), "{logged}");
        assert!(
            !printed.contains(&self.key) && !logged.contains(&self.key),
            "key in output"
        );
    }
}

/// No key or an unknown key gets 401 on both paths and proposes nothing;
/// `/mcp` lists both tools; a Build without a brief is refused at propose.
async fn roles_refused_without_a_key_or_a_brief(gateway: &Gateway, seeded: &Seeded) {
    let mut mcp = gateway.mcp("/api/v1/mcp").await;
    let call = json!({ "jsonrpc": "2.0", "id": 99, "method": "tools/call",
        "params": { "name": ROLES, "arguments": seeded.roles("structural_base", "addon") } });
    let before = gateway.observed(seeded.build).await;
    for path in ["/api/v1/mcp", "/mcp"] {
        for key in [None, Some(UNKNOWN_KEY)] {
            let (status, body, _) = gateway.post_mcp(path, key, None, &call).await;
            assert_eq!(status, 401, "{path} {key:?}: {body}");
        }
    }
    let session = Some(mcp.session.as_str());
    let (status, body, _) = gateway.post_mcp("/api/v1/mcp", None, session, &call).await;
    assert_eq!(status, 401, "keyless call on a keyed session: {body}");
    assert!(mcp.pending().await.is_empty());
    assert_eq!(gateway.observed(seeded.build).await, before, "a 401 wrote");

    let mut root_mcp = gateway.mcp("/mcp").await;
    let tools = root_mcp.tools().await;
    for tool in [ROLES, IMPORT] {
        assert!(tools.iter().any(|name| name == tool), "{tool} in {tools:?}");
    }

    let plain = Some(json!({ "name": "No brief" }));
    let (status, plain) = gateway.rest(Method::POST, "/api/v2/plans", plain).await;
    assert!(status == 200 || status == 201, "{status}: {plain}");
    let role = json!([{ "evidence_id": seeded.evidence[0], "role": "addon" }]);
    let plain = plain["id"].as_u64().unwrap();
    let no_brief = json!({ "plan_id": plain, "roles": role });
    mcp.refused_at_propose(plain, ROLES, no_brief.clone(), "no_planning_brief")
        .await;
    root_mcp
        .refused_at_propose(plain, ROLES, no_brief, "no_planning_brief")
        .await;
    assert_eq!(gateway.untouched(seeded.build).await, seeded.invariant);
}

/// Propose writes nothing; confirm sets both roles and clears draft D, and
/// the Library, B's layers and the Accepted Plan stay as they are.
async fn roles_set_and_clear_the_selected_draft(gateway: &Gateway, seeded: &Seeded) {
    let (build, [e1, e2]) = (seeded.build, &seeded.evidence);
    let mut mcp = gateway.mcp("/api/v1/mcp").await;
    gateway.select_draft(build);
    assert_eq!(gateway.brief(build).await["draft_id"], DRAFT);
    let action = mcp
        .proposed(build, ROLES, seeded.roles("structural_base", "addon"))
        .await;
    let (applied, error) = mcp.confirm(&action).await;
    assert!(!error, "{applied}");
    assert_eq!(
        applied,
        json!({ "ok": true, "result": { "updated": 2, "evidence": [
            { "id": e1, "source_id": seeded.sources[0], "source_role": "structural_base" },
            { "id": e2, "source_id": seeded.sources[1], "source_role": "addon" },
        ] } })
    );
    let planning = gateway.planning(build).await;
    let brief = &planning["planning"]["brief"];
    let set: Vec<_> = brief["evidence"]
        .as_array()
        .unwrap()
        .iter()
        .map(|item| (item["id"].clone(), item["source_role"].clone()))
        .collect();
    let expected = [
        (json!(e1), json!("structural_base")),
        (json!(e2), json!("addon")),
    ];
    assert_eq!(set, expected);
    assert!(
        brief["draft_id"].is_null(),
        "selected draft cleared: {brief}"
    );
    let phase = &planning["planning"]["planning_phase"];
    assert_eq!(phase, &json!({ "kind": "preparing" }), "{planning}");
    assert_eq!(gateway.untouched(build).await, seeded.invariant);
}

/// Confirm refuses an unknown evidence id, a role outside the four and a
/// second structural base with a code, and the brief stays as it is. These
/// proposals and confirms run on `/mcp`.
async fn roles_refused_at_confirm(gateway: &Gateway, seeded: &Seeded) {
    let (build, e2) = (seeded.build, &seeded.evidence[1]);
    let mut mcp = gateway.mcp("/mcp").await;
    for (roles, code) in [
        (
            json!([{ "evidence_id": "E9", "role": "addon" }]),
            "evidence_not_found",
        ),
        (
            json!([{ "evidence_id": e2, "role": "base" }]),
            "invalid_source_role",
        ),
        (
            json!([{ "evidence_id": e2, "role": "structural_base" }]),
            "multiple_structural_bases",
        ),
    ] {
        let args = json!({ "plan_id": build, "roles": roles });
        let action = mcp.proposed(build, ROLES, args).await;
        let refused = mcp.refused_at_confirm(build, &action, code).await;
        assert!(refused["detail"].is_string(), "{refused}");
    }
    assert_eq!(
        mcp.pending().await.len(),
        3,
        "refused proposals stay pending"
    );
    assert_eq!(gateway.untouched(build).await, seeded.invariant);
}

#[test]
fn source_roles_through_the_rust_only_gateway() {
    let root = short_root();
    let (runtime, gateway, seeded) = open(root.path());
    runtime.block_on(async {
        roles_refused_without_a_key_or_a_brief(&gateway, &seeded).await;
        roles_set_and_clear_the_selected_draft(&gateway, &seeded).await;
        roles_refused_at_confirm(&gateway, &seeded).await;
    });
    gateway.stop();
}

/// No key gets 401; bad inputs are refused at propose with a code.
async fn import_refused_without_a_key_or_at_propose(gateway: &Gateway, seeded: &Seeded) {
    let s3 = seeded.sources[2];
    let mut mcp = gateway.mcp("/api/v1/mcp").await;
    let call = json!({ "jsonrpc": "2.0", "id": 99, "method": "tools/call",
        "params": { "name": IMPORT, "arguments": seeded.inputs(json!([{ "source_id": s3 }])) } });
    let session = Some(mcp.session.as_str());
    let before = gateway.observed(seeded.build).await;
    let (status, body, _) = gateway.post_mcp("/api/v1/mcp", None, session, &call).await;
    assert_eq!(status, 401, "{body}");
    assert!(mcp.pending().await.is_empty());
    assert_eq!(gateway.observed(seeded.build).await, before, "a 401 wrote");
    for (inputs, code) in [
        (json!([{ "source_id": 999_999 }]), "source_not_found"),
        (
            json!([{ "source_id": s3, "kind": "part" }]),
            "invalid_evidence_kind",
        ),
        (
            json!([{ "source_id": s3, "derived_from_evidence_id": "E9" }]),
            "derived_evidence_not_found",
        ),
        (
            json!([{ "url": "ftp://example.com/kit.zip" }]),
            "invalid_url",
        ),
    ] {
        mcp.refused_at_propose(seeded.build, IMPORT, seeded.inputs(inputs), code)
            .await;
    }
    assert_eq!(gateway.untouched(seeded.build).await, seeded.invariant);
}

/// Library Source S3: confirm attaches it once and clears draft D, and no
/// Source is created or changed; importing it again keeps one item.
async fn import_attaches_a_library_source(gateway: &Gateway, seeded: &Seeded) {
    let (build, s3) = (seeded.build, seeded.sources[2]);
    let mut mcp = gateway.mcp("/api/v1/mcp").await;
    let synced = synced(&gateway.sources().await, s3);
    gateway.select_draft(build);
    assert_eq!(gateway.brief(build).await["draft_id"], DRAFT);
    let inputs = seeded.inputs(json!([{ "source_id": s3, "kind": "mod" }]));
    let action = mcp.proposed(build, IMPORT, inputs).await;
    let (applied, error) = mcp.confirm(&action).await;
    assert!(!error, "{applied}");
    let id = applied["result"]["evidence"][0]["id"]
        .as_str()
        .unwrap()
        .to_owned();
    assert_eq!(
        applied,
        json!({ "ok": true, "result": {
            "evidence": [{ "id": id, "kind": "mod", "source_id": s3,
                "url": format!("printpartner:source:{s3}"), "upload_required": false }],
            "created_source_ids": [],
        } })
    );
    let brief = gateway.brief(build).await;
    let items = evidence_for(&brief, s3);
    assert_eq!(items.len(), 1, "{brief}");
    assert_eq!(
        (&items[0]["id"], &items[0]["kind"]),
        (&json!(id), &json!("mod"))
    );
    assert!(
        brief["draft_id"].is_null(),
        "selected draft cleared: {brief}"
    );
    assert_eq!(
        gateway.untouched(build).await,
        seeded.invariant,
        "no Source changed"
    );
    assert_eq!(source(&gateway.sources().await, s3), synced, "S3 as it was");

    let again = seeded.inputs(json!([{ "source_id": s3, "title": "Kit" }]));
    mcp.applied(build, IMPORT, again).await;
    let items = evidence_for(&gateway.brief(build).await, s3);
    assert_eq!(items.len(), 1, "{items:?}");
    assert_eq!(
        (&items[0]["id"], &items[0]["title"]),
        (&json!(id), &json!("Kit"))
    );
    assert_eq!(gateway.untouched(build).await, seeded.invariant);
}

/// A GitHub URL creates one unsynced git Source, listed by `GET /sources`;
/// the same URL on another Build reuses it.
async fn import_creates_a_git_source_once(gateway: &Gateway, seeded: &Seeded) {
    let build = seeded.build;
    let mut mcp = gateway.mcp("/api/v1/mcp").await;
    let library = gateway.sources().await;
    let kit = "https://github.com/owner/kit";
    let imported = mcp
        .applied(build, IMPORT, seeded.inputs(json!([{ "url": kit }])))
        .await;
    let created = imported["created_source_ids"].clone();
    let kit_id = created[0].as_i64().unwrap();
    assert_eq!(created, json!([kit_id]), "{imported}");
    assert_eq!(imported["evidence"][0]["source_id"], kit_id);
    let listed = gateway.sources().await;
    assert_eq!(listed.len(), library.len() + 1);
    let source = listed.iter().find(|source| source["id"] == kit_id).unwrap();
    assert_eq!(
        (&source["url"], &source["source_type"]),
        (&json!(kit), &json!("git"))
    );
    assert!(source["last_synced_at"].is_null(), "unsynced: {source}");

    let other = mcp.build("Second kit").await;
    let again = json!({ "plan_id": other, "inputs": [{ "url": kit }] });
    let reused = mcp.applied(other, IMPORT, again).await;
    assert_eq!(reused["created_source_ids"], json!([]), "{reused}");
    assert_eq!(reused["evidence"][0]["source_id"], kit_id);
    assert_eq!(gateway.sources().await, listed, "reused, not created");
    seeded.build_kept(gateway).await;
}

/// Model pages need an upload and create no Source; an extract is stored
/// with its SHA-256 and retrieval time. This import runs on `/mcp`.
async fn import_attaches_pages_and_an_extract(gateway: &Gateway, seeded: &Seeded) {
    let build = seeded.build;
    let mut mcp = gateway.mcp("/mcp").await;
    let library = gateway.sources().await;
    let inputs = seeded.inputs(json!([
        { "url": "https://www.printables.com/model/12345-kit" },
        { "url": "https://makerworld.com/en/models/678" },
        { "url": "https://example.com/guide", "extract": " Print the frame first. " },
    ]));
    let imported = mcp.applied(build, IMPORT, inputs).await;
    let fields: Vec<_> = imported["evidence"]
        .as_array()
        .unwrap()
        .iter()
        .map(|item| (item["source_id"].clone(), item["upload_required"].clone()))
        .collect();
    let page = (Value::Null, json!(true));
    assert_eq!(fields, [page.clone(), page, (Value::Null, json!(false))]);
    assert_eq!(imported["created_source_ids"], json!([]));
    assert_eq!(gateway.sources().await, library, "a page creates no Source");
    let brief = gateway.brief(build).await;
    let stored = |url: &str| {
        let items = brief["evidence"].as_array().unwrap();
        let found = items.iter().find(|item| item["normalized_url"] == url);
        found.unwrap_or_else(|| panic!("{url} in {brief}")).clone()
    };
    for page in [
        "https://www.printables.com/model/12345-kit",
        "https://makerworld.com/en/models/678",
    ] {
        let item = stored(page);
        let fields = [
            &item["url"],
            &item["kind"],
            &item["input_kind"],
            &item["upload_required"],
        ];
        let expected = [
            json!(page),
            json!("model_source"),
            json!("model_page"),
            json!(true),
        ];
        assert_eq!(fields, expected.each_ref(), "{item}");
        assert_eq!(item.get("source_id"), Some(&Value::Null), "{item}");
    }
    let guide = stored("https://example.com/guide");
    assert_eq!(guide["extract"], "Print the frame first.");
    // sha256("Print the frame first.")
    let hash = "8a59b5bfc58ba8baf8e26fece6b3fd9bbe768c98773b1cdf772da15e06bdb1a4";
    assert_eq!(guide["content_hash"], hash);
    assert!(guide["retrieved_at"].is_string(), "{guide}");
    seeded.build_kept(gateway).await;
}

/// One confirm attaches all of its inputs or none: a new repository and a
/// Source deleted after propose attach neither, and no Source is created.
async fn import_confirm_is_atomic(gateway: &Gateway, seeded: &Seeded) {
    let build = seeded.build;
    let mut mcp = gateway.mcp("/api/v1/mcp").await;
    let gone = gateway
        .create_source("Gone", "https://github.com/example/gone", "mod")
        .await;
    let library = gateway.sources().await;
    let inputs = json!([{ "url": "https://github.com/owner/other" }, { "source_id": gone }]);
    let action = mcp.proposed(build, IMPORT, seeded.inputs(inputs)).await;
    let path = format!("/api/v1/sources/{gone}");
    let (status, body) = gateway.rest(Method::DELETE, &path, None).await;
    assert!(status == 200 || status == 204, "{status}: {body}");
    let library: Vec<_> = library
        .into_iter()
        .filter(|source| source["id"] != gone)
        .collect();
    assert_eq!(gateway.sources().await, library);
    let refused = mcp
        .refused_at_confirm(build, &action, "source_not_found")
        .await;
    assert_eq!(refused["source_id"], gone, "{refused}");
    assert_eq!(
        mcp.pending().await.len(),
        1,
        "the refused proposal stays pending"
    );
    seeded.build_kept(gateway).await;
}

#[test]
fn import_inputs_through_the_rust_only_gateway() {
    let root = short_root();
    let (runtime, gateway, seeded) = open(root.path());
    runtime.block_on(async {
        import_refused_without_a_key_or_at_propose(&gateway, &seeded).await;
        import_attaches_a_library_source(&gateway, &seeded).await;
        import_creates_a_git_source_once(&gateway, &seeded).await;
        import_attaches_pages_and_an_extract(&gateway, &seeded).await;
        import_confirm_is_atomic(&gateway, &seeded).await;
    });
    gateway.stop();
}
