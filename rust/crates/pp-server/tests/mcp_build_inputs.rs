//! GRE-1300's acceptance calls through a running Rust-only `pp-server`, the
//! start the Docker image makes: `propose_set_build_source_roles` and
//! `propose_import_build_inputs` on `/api/v1/mcp` and `/mcp`, with an API key
//! and no cookie. State is read back through the REST routes the UI reads.
#![cfg(all(unix, feature = "desktop_rebuild"))]

#[path = "../../pp-storage/tests/common/process.rs"]
mod child_process;
#[path = "../../pp-core/tests/support/release.rs"]
mod release;
#[path = "support/rust_only.rs"]
mod rust_only;
#[path = "../../pp-storage/tests/common/scratch.rs"]
mod scratch;

use reqwest::Method;
use serde_json::{Value, json};

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

/// The first JSON-RPC message in an SSE or JSON body.
fn rpc_message(body: &str) -> Value {
    let data = body
        .lines()
        .filter_map(|line| line.strip_prefix("data:"))
        .map(str::trim)
        .find(|data| !data.is_empty())
        .unwrap_or(body);
    serde_json::from_str(data).unwrap_or_else(|_| panic!("JSON-RPC body: {body}"))
}

impl Gateway {
    /// Signs in with `pp-server bootstrap-url`, turns the Orders Module on
    /// so an assistant may propose a Build, and creates an API key.
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
        let response = request.send().await.unwrap();
        let status = response.status().as_u16();
        let session = response
            .headers()
            .get("mcp-session-id")
            .map(|value| value.to_str().unwrap().to_owned());
        (status, response.text().await.unwrap(), session)
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
        rpc_message(&body)["result"].clone()
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

    /// `name` is refused at propose with `code`, and proposes nothing.
    async fn refused_at_propose(&mut self, name: &str, arguments: Value, code: &str) {
        let pending = self.pending().await.len();
        let (refused, error) = self.call(name, arguments.clone()).await;
        assert!(error, "{arguments}: {refused}");
        assert_eq!(refused["code"], code, "{arguments}: {refused}");
        assert_eq!(self.pending().await.len(), pending, "{arguments}");
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

fn runtime() -> tokio::runtime::Runtime {
    tokio::runtime::Runtime::new().unwrap()
}

/// Build B and its Library: Sources S1 (`mod`), S2 (`overlay`) and S3
/// (`mod`), a base layer of B on S1, and B's brief with evidence E1 (S1) and
/// E2 (S2), neither with a role.
struct Seeded {
    build: u64,
    sources: [i64; 3],
    evidence: [String; 2],
}

async fn seed(gateway: &Gateway) -> Seeded {
    let mut mcp = gateway.mcp("/api/v1/mcp").await;
    let build = mcp.build("Voron for Sam").await;
    let sources = [
        gateway
            .create_source("S1", "https://github.com/example/s1", "mod")
            .await,
        gateway
            .create_source("S2", "https://github.com/example/s2", "overlay")
            .await,
        gateway
            .create_source("S3", "https://github.com/example/s3", "mod")
            .await,
    ];
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
    Seeded {
        build,
        sources,
        evidence,
    }
}

#[test]
fn source_roles_through_the_rust_only_gateway() {
    let root = scratch::Scratch::new("pp-mcp-gateway-roles-");
    let server = rust_only::Server::start(&root);
    runtime().block_on(async {
        let gateway = Gateway::open(server).await;
        let seeded = seed(&gateway).await;
        let (build, [e1, e2]) = (seeded.build, seeded.evidence.clone());
        let invariant = gateway.untouched(build).await;
        assert_eq!(
            invariant["accepted"]["kind"], "empty",
            "no Accepted Plan: {invariant}"
        );
        let roles = |first: &str, second: &str| {
            json!({ "plan_id": build, "roles": [
                { "evidence_id": e1, "role": first },
                { "evidence_id": e2, "role": second },
            ] })
        };

        // No key, an unknown key: 401 on both paths, and no proposal.
        let mut mcp = gateway.mcp("/api/v1/mcp").await;
        let call = json!({ "jsonrpc": "2.0", "id": 99, "method": "tools/call",
            "params": { "name": ROLES, "arguments": roles("structural_base", "addon") } });
        for path in ["/api/v1/mcp", "/mcp"] {
            for key in [None, Some(UNKNOWN_KEY)] {
                let (status, body, _) = gateway.post_mcp(path, key, None, &call).await;
                assert_eq!(status, 401, "{path} {key:?}: {body}");
            }
        }
        let (status, body, _) = gateway
            .post_mcp("/api/v1/mcp", None, Some(&mcp.session), &call)
            .await;
        assert_eq!(status, 401, "keyless call on a keyed session: {body}");
        assert!(mcp.pending().await.is_empty());

        // `/mcp` reaches both tools.
        let mut root_mcp = gateway.mcp("/mcp").await;
        let tools = root_mcp.tools().await;
        for tool in [ROLES, IMPORT] {
            assert!(tools.iter().any(|name| name == tool), "{tool} in {tools:?}");
        }

        // A Build with no brief is refused at propose, on both paths.
        let (status, plain) = gateway
            .rest(
                Method::POST,
                "/api/v2/plans",
                Some(json!({ "name": "No brief" })),
            )
            .await;
        assert!(status == 200 || status == 201, "{status}: {plain}");
        let plain = plain["id"].as_u64().unwrap();
        let no_brief =
            json!({ "plan_id": plain, "roles": [{ "evidence_id": e1, "role": "addon" }] });
        mcp.refused_at_propose(ROLES, no_brief.clone(), "no_planning_brief")
            .await;
        root_mcp
            .refused_at_propose(ROLES, no_brief, "no_planning_brief")
            .await;

        // Propose writes nothing; confirm sets both roles and clears draft D.
        gateway.select_draft(build);
        assert_eq!(gateway.brief(build).await["draft_id"], DRAFT);
        let action = mcp
            .proposed(build, ROLES, roles("structural_base", "addon"))
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
        assert_eq!(
            set,
            [
                (json!(e1), json!("structural_base")),
                (json!(e2), json!("addon"))
            ]
        );
        assert!(
            brief["draft_id"].is_null(),
            "selected draft cleared: {brief}"
        );
        assert_eq!(
            planning["planning"]["planning_phase"],
            json!({ "kind": "preparing" }),
            "{planning}"
        );
        assert_eq!(
            gateway.untouched(build).await,
            invariant,
            "Library, layers, Accepted Plan"
        );

        // Refused at confirm with a code; the brief stays as it is.
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
        // Each refused proposal stays pending.
        assert_eq!(mcp.pending().await.len(), 3);
        assert_eq!(
            gateway.untouched(build).await,
            invariant,
            "Library, layers, Accepted Plan"
        );
    });
}

#[test]
fn import_inputs_through_the_rust_only_gateway() {
    let root = scratch::Scratch::new("pp-mcp-gateway-import-");
    let server = rust_only::Server::start(&root);
    runtime().block_on(async {
        let gateway = Gateway::open(server).await;
        let seeded = seed(&gateway).await;
        let (build, s3) = (seeded.build, seeded.sources[2]);
        let invariant = gateway.untouched(build).await;
        assert_eq!(
            invariant["accepted"]["kind"], "empty",
            "no Accepted Plan: {invariant}"
        );
        let library = gateway.sources().await;
        let args = |inputs: Value| json!({ "plan_id": build, "inputs": inputs });
        let mut mcp = gateway.mcp("/api/v1/mcp").await;

        // Without a key: 401 and no proposal.
        let call = json!({ "jsonrpc": "2.0", "id": 99, "method": "tools/call",
            "params": { "name": IMPORT, "arguments": args(json!([{ "source_id": s3 }])) } });
        let (status, body, _) = gateway
            .post_mcp("/api/v1/mcp", None, Some(&mcp.session), &call)
            .await;
        assert_eq!(status, 401, "{body}");
        assert!(mcp.pending().await.is_empty());

        // Refused at propose with a code; nothing proposed or written.
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
            mcp.refused_at_propose(IMPORT, args(inputs), code).await;
        }
        assert_eq!(gateway.untouched(build).await, invariant);

        // A Library Source: propose writes nothing, confirm attaches it and
        // clears draft D, and no Source is created or changed.
        gateway.select_draft(build);
        let action = mcp
            .proposed(
                build,
                IMPORT,
                args(json!([{ "source_id": s3, "kind": "mod" }])),
            )
            .await;
        let (applied, error) = mcp.confirm(&action).await;
        assert!(!error, "{applied}");
        let attached = &applied["result"]["evidence"][0];
        let s3_evidence = attached["id"].as_str().unwrap().to_owned();
        assert_eq!(
            applied,
            json!({ "ok": true, "result": {
                "evidence": [{ "id": s3_evidence, "kind": "mod", "source_id": s3,
                    "url": format!("printpartner:source:{s3}"), "upload_required": false }],
                "created_source_ids": [],
            } })
        );
        let brief = gateway.brief(build).await;
        let items = evidence_for(&brief, s3);
        assert_eq!(items.len(), 1, "{brief}");
        assert_eq!(
            (&items[0]["id"], &items[0]["kind"]),
            (&json!(s3_evidence), &json!("mod"))
        );
        assert!(
            brief["draft_id"].is_null(),
            "selected draft cleared: {brief}"
        );
        assert_eq!(
            gateway.sources().await,
            library,
            "no Source created or changed"
        );

        // Importing S3 again leaves one item for it.
        mcp.applied(
            build,
            IMPORT,
            args(json!([{ "source_id": s3, "title": "Kit" }])),
        )
        .await;
        let items = evidence_for(&gateway.brief(build).await, s3);
        assert_eq!(items.len(), 1, "{items:?}");
        assert_eq!(items[0]["title"], "Kit");
        assert_eq!(gateway.untouched(build).await, invariant);

        // A GitHub URL creates one unsynced git Source, listed by GET
        // /sources; the same URL on another Build reuses it.
        let kit = "https://github.com/owner/kit";
        let imported = mcp
            .applied(build, IMPORT, args(json!([{ "url": kit }])))
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
        let library = listed;

        // Model pages need an upload and create no Source; an extract is
        // stored with its SHA-256 and retrieval time.
        let imported = mcp
            .applied(
                build,
                IMPORT,
                args(json!([
                    { "url": "https://www.printables.com/model/12345-kit" },
                    { "url": "https://makerworld.com/en/models/678" },
                    { "url": "https://example.com/guide", "extract": " Print the frame first. " },
                ])),
            )
            .await;
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
        let guide = brief["evidence"]
            .as_array()
            .unwrap()
            .iter()
            .find(|item| item["normalized_url"] == "https://example.com/guide")
            .unwrap_or_else(|| panic!("{brief}"))
            .clone();
        assert_eq!(guide["extract"], "Print the frame first.");
        // sha256("Print the frame first.")
        assert_eq!(
            guide["content_hash"],
            "8a59b5bfc58ba8baf8e26fece6b3fd9bbe768c98773b1cdf772da15e06bdb1a4"
        );
        assert!(guide["retrieved_at"].is_string(), "{guide}");

        // One confirm attaches all of its inputs or none: a new repository
        // and a Source deleted after propose attach neither, and no Source
        // is created.
        let gone = gateway
            .create_source("Gone", "https://github.com/example/gone", "mod")
            .await;
        let library = gateway.sources().await;
        let inputs = json!([{ "url": "https://github.com/owner/other" }, { "source_id": gone }]);
        let action = mcp.proposed(build, IMPORT, args(inputs)).await;
        let (status, body) = gateway
            .rest(Method::DELETE, &format!("/api/v1/sources/{gone}"), None)
            .await;
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
        // B's layers and Accepted Plan never changed; the Library changed
        // only by the Sources created above.
        let after = gateway.untouched(build).await;
        assert_eq!(after["layers"], invariant["layers"]);
        assert_eq!(after["accepted"], invariant["accepted"]);
    });
}
