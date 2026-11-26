//! GRE-453: an MCP session answers only the API key that opened it, on both
//! aliases, and a session does not outlive a restart.
use super::create_build::{confirm, database, json_call, orders_on, pending};
use super::*;

const ALIASES: [&str; 2] = ["/mcp", "/api/v1/mcp"];
const NOT_FOUND: &str = "Not Found: Session not found";

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn mcp_proposal_lifecycle_uses_the_node_tool_names_and_input_schemas() {
    let (server, headers) = orders_on().await;
    let golden: Value = serde_json::from_str(include_str!(
        "../../../../../../web/apps/server/fixtures/mcp/tools-inventory.json"
    ))
    .unwrap();
    let list = json!({ "jsonrpc": "2.0", "id": 2, "method": "tools/list" });
    let (status, body, _) = server.post("/mcp", &pairs(&headers), &list).await;
    assert_eq!(status, 200, "{body}");
    let response = rpc(&body);
    for name in pp_services::mcp_domain::META_TOOLS {
        let find = |tools: &Value| {
            tools
                .as_array()
                .unwrap()
                .iter()
                .find(|tool| tool["name"] == name)
                .unwrap()["inputSchema"]
                .clone()
        };
        assert_eq!(
            find(&response["result"]["tools"]),
            find(&golden["tools"]),
            "{name}"
        );
    }
}

/// Initializes on `path` with `bearer` and sends `notifications/initialized`
/// on the other alias. Returns the headers that continue the session.
async fn open(server: &McpServer, path: &str, bearer: &str) -> Vec<(String, String)> {
    let auth = [("authorization", bearer)];
    let (status, body, session) = server.post(path, &auth, &initialize()).await;
    assert_eq!(status, 200, "{path}: {body}");
    let headers = keyed(bearer, &session.unwrap());
    let other = if path == "/mcp" {
        "/api/v1/mcp"
    } else {
        "/mcp"
    };
    let notified = json!({ "jsonrpc": "2.0", "method": "notifications/initialized" });
    let (status, body, _) = server.post(other, &pairs(&headers), &notified).await;
    assert_eq!(status, 202, "{other}: {body}");
    headers
}

/// Headers that continue `session` with `authorization` set to `bearer`.
fn keyed(bearer: &str, session: &str) -> Vec<(String, String)> {
    let mut headers = vec![
        ("mcp-session-id".to_owned(), session.to_owned()),
        ("mcp-protocol-version".to_owned(), "2025-06-18".to_owned()),
    ];
    if !bearer.is_empty() {
        headers.push(("authorization".to_owned(), bearer.to_owned()));
    }
    headers
}

fn pairs(headers: &[(String, String)]) -> Vec<(&str, &str)> {
    headers
        .iter()
        .map(|(name, value)| (name.as_str(), value.as_str()))
        .collect()
}

fn session_id(headers: &[(String, String)]) -> &str {
    &headers
        .iter()
        .find(|(name, _)| name == "mcp-session-id")
        .unwrap()
        .1
}

/// Status and body of a GET (the notification stream) or DELETE on `path`.
/// Same-origin, so admission passes and only the key or session can refuse.
async fn send(
    server: &McpServer,
    method: Method,
    path: &str,
    headers: &[(String, String)],
) -> (u16, String) {
    let mut request = server
        .client
        .request(method, format!("{}{path}", server.origin))
        .header("accept", "text/event-stream")
        .header("origin", &server.origin);
    for (name, value) in headers {
        request = request.header(name, value);
    }
    let response = request.send().await.unwrap();
    let status = response.status().as_u16();
    // A 2xx GET is an open stream; read the body only of a refusal.
    let body = if status >= 300 {
        response.text().await.unwrap()
    } else {
        String::new()
    };
    (status, body)
}

/// Status and body of a same-origin `tools/call` POST on `path`.
async fn post_call(
    server: &McpServer,
    path: &str,
    headers: &[(String, String)],
    tool: &str,
    arguments: Value,
) -> (u16, String) {
    let request = json!({ "jsonrpc": "2.0", "id": 7, "method": "tools/call",
                          "params": { "name": tool, "arguments": arguments } });
    let mut headers = pairs(headers);
    headers.push(("origin", SELF));
    let (status, body, _) = server.post(path, &headers, &request).await;
    (status, body)
}

/// Asserts POST, GET and DELETE on both aliases get 404 with `headers`.
async fn assert_session_hidden(server: &McpServer, headers: &[(String, String)], action: &str) {
    for path in ALIASES {
        let confirm = json!({ "action_id": action });
        let (status, body) = post_call(server, path, headers, "confirm_apply", confirm).await;
        assert_eq!((status, body.as_str()), (404, NOT_FOUND), "POST {path}");
        for method in [Method::GET, Method::DELETE] {
            let (status, body) = send(server, method.clone(), path, headers).await;
            assert_eq!((status, body.as_str()), (404, NOT_FOUND), "{method} {path}");
        }
    }
}

fn builds(server: &McpServer) -> i64 {
    database(server)
        .query_row("SELECT COUNT(*) FROM build_profiles", [], |row| row.get(0))
        .unwrap()
}

/// Proposes a Build on the session in `headers` and returns the action id.
async fn propose_build(server: &McpServer, headers: &[(String, String)]) -> String {
    let (proposal, error) = json_call(
        server,
        headers,
        "propose_create_build",
        json!({ "name": "Key-bound Build", "request": "One Voron.", "urls": [] }),
    )
    .await;
    assert!(!error, "{proposal}");
    proposal["action"]["id"].as_str().unwrap().to_owned()
}

/// A second valid key of the same installation cannot reach the first key's
/// session on either alias: POST, GET and DELETE get 404, the pending Build
/// stays pending and no Build is created. The other key can confirm it once
/// in its own session, and the opening key can still close its session.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn mcp_session_key_other_key_cannot_use_or_confirm_a_session() {
    let (server, _) = orders_on().await;
    let owner = format!("Bearer {}", server.create_key().await.1);
    let other = format!("Bearer {}", server.create_key().await.1);
    // Opened on `/mcp`; `json_call` continues it on `/api/v1/mcp`.
    let headers = open(&server, "/mcp", &owner).await;
    let action = propose_build(&server, &headers).await;
    let before = builds(&server);

    let foreign = keyed(&other, session_id(&headers));
    assert_session_hidden(&server, &foreign, &action).await;
    assert_eq!(builds(&server), before);
    let waiting = pending(&server, &headers).await;
    assert_eq!(waiting.len(), 1);
    assert_eq!(waiting[0]["id"], action.as_str());

    // The other key opens its own session to see the tenant's proposal.
    let own = open(&server, "/api/v1/mcp", &other).await;
    let shared = pending(&server, &own).await;
    assert_eq!(waiting, shared);
    assert_eq!(shared[0]["staged_by"], "ticket-t-16");
    assert!(shared[0]["created_at"].as_str() < shared[0]["expires_at"].as_str());
    println!("session A list_pending_actions: {waiting:?}");
    println!("session B list_pending_actions: {shared:?}");

    let (confirmed, error) = confirm(&server, &own, &action).await;
    assert!(!error, "{confirmed}");
    assert_eq!(confirmed["result"]["idempotent_replay"], false);
    assert_eq!(builds(&server), before + 1);
    let (again, error) = confirm(&server, &headers, &action).await;
    assert!(error, "{again}");
    assert_eq!(again["code"], "already_applied");
    assert!(pending(&server, &headers).await.is_empty());
    assert_eq!(builds(&server), before + 1);

    let (status, _) = send(&server, Method::DELETE, "/api/v1/mcp", &headers).await;
    assert_eq!(status, 202);
    let (status, body) = post_call(&server, "/mcp", &headers, "list_plans", json!({})).await;
    assert_eq!((status, body.as_str()), (404, NOT_FOUND));
}

/// Sets `key_id`'s expiry in the past, as a key whose lifetime ran out.
fn expire(server: &McpServer, key_id: &str) {
    let connection = database(server);
    let raw: String = connection
        .query_row(
            "SELECT value FROM app_settings WHERE tenant_id='default' AND key='api_keys_v1'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    let mut keys: Value = serde_json::from_str(&raw).unwrap();
    for key in keys.as_array_mut().unwrap() {
        if key["id"] == key_id {
            key["expiresAt"] = json!("2000-01-01T00:00:00.000Z");
        }
    }
    connection
        .execute(
            "UPDATE app_settings SET value=?1 WHERE tenant_id='default' AND key='api_keys_v1'",
            [keys.to_string()],
        )
        .unwrap();
}

/// Missing, invalid, expired and revoked keys still fail the credential gate
/// on an open session, before the session check. A rotated key's replacement
/// opens a fresh session but cannot reach the old one.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn mcp_session_key_credential_gate_and_rotation() {
    let server = McpServer::start(Some("1")).await;
    let (expired_id, expired) = server.create_key().await;
    let (revoked_id, revoked) = server.create_key().await;
    let (rotated_id, rotated) = server.create_key().await;
    let mut sessions = Vec::new();
    for key in [&expired, &revoked, &rotated] {
        sessions.push(open(&server, "/api/v1/mcp", &format!("Bearer {key}")).await);
    }
    expire(&server, &expired_id);
    server.revoke(revoked_id).await;
    let refused = [
        ("missing", String::new(), &sessions[0]),
        (
            "invalid",
            "Bearer ppk_0000000000000000000000000000000000000000".to_owned(),
            &sessions[0],
        ),
        ("expired", format!("Bearer {expired}"), &sessions[0]),
        ("revoked", format!("Bearer {revoked}"), &sessions[1]),
    ];
    for (name, bearer, opened) in &refused {
        let headers = keyed(bearer, session_id(opened));
        for path in ALIASES {
            let (status, body) = post_call(&server, path, &headers, "list_plans", json!({})).await;
            assert_eq!(status, 401, "{name} POST {path}: {body}");
            assert_eq!(rpc(&body), json!({ "detail": KEY_REQUIRED }), "{name}");
            for method in [Method::GET, Method::DELETE] {
                let (status, _) = send(&server, method.clone(), path, &headers).await;
                assert_eq!(status, 401, "{name} {method} {path}");
            }
        }
    }

    let replacement = server
        .auth(auth::Request::RotateKey {
            session: Secret::new(FIXTURE_SESSION.into()),
            key_id: rotated_id,
        })
        .await;
    let auth::Outcome::KeyCreated { key, .. } = replacement else {
        panic!("rotation creates a replacement key");
    };
    let replacement = format!("Bearer {}", key.expose());
    let headers = keyed(&format!("Bearer {rotated}"), session_id(&sessions[2]));
    let (status, _) = post_call(&server, "/mcp", &headers, "list_plans", json!({})).await;
    assert_eq!(status, 401, "the rotated key");
    let foreign = keyed(&replacement, session_id(&sessions[2]));
    assert_session_hidden(&server, &foreign, "none").await;
    let fresh = open(&server, "/mcp", &replacement).await;
    let (plans, error) = json_call(&server, &fresh, "list_plans", json!({})).await;
    assert!(!error, "{plans}");
}

/// A restart against the same data directory forgets every session: with a
/// key that is still valid, the old session ID gets 404 on both aliases and
/// a fresh session still sees the proposal and can dismiss it.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn mcp_session_key_old_session_is_gone_after_restart() {
    let (server, _) = orders_on().await;
    let bearer = format!("Bearer {}", server.create_key().await.1);
    let before = open(&server, "/mcp", &bearer).await;
    let action = propose_build(&server, &before).await;
    let builds_before = builds(&server);

    let server = server.restart().await;
    assert_session_hidden(&server, &before, &action).await;
    assert_eq!(builds(&server), builds_before);
    let fresh = open(&server, "/api/v1/mcp", &bearer).await;
    assert_eq!(pending(&server, &fresh).await[0]["id"], action);
    let (dismissed, error) = json_call(
        &server,
        &fresh,
        "dismiss_proposed_action",
        json!({ "action_id": action }),
    )
    .await;
    assert!(!error && dismissed["ok"] == true, "{dismissed}");
    let reconnected = open(&server, "/mcp", &bearer).await;
    assert!(pending(&server, &reconnected).await.is_empty());
    let (refused, error) = confirm(&server, &reconnected, &action).await;
    assert!(error, "{refused}");
    assert_eq!(refused["code"], "already_dismissed");
    assert_eq!(builds(&server), builds_before);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn mcp_pending_action_refuses_expiry_and_names_the_dependency_to_apply_first() {
    use pp_storage::pending_actions::{NewAction, StagedBy};
    use pp_storage::read_model::Credential;

    let (server, headers) = orders_on().await;
    let before = builds(&server);
    let first = propose_build(&server, &headers).await;
    // MCP proposal schemas are unchanged. Stage a dependency through the
    // queue seam, as a future caller that arranges a sequence of actions would.
    let queue = server
        .owner
        .accepted_reads_with_policy(policy())
        .unwrap()
        .pending_actions();
    let dependent = "dependent-fixture";
    tokio::task::spawn_blocking({
        let first = first.clone();
        move || {
            queue
                .stage(
                    Credential::Session(Secret::new(FIXTURE_SESSION.into())),
                    NewAction {
                        id: dependent.into(),
                        action_type: "propose_create_build".into(),
                        plan_id: 0,
                        label: "Build after the first".into(),
                        summary: "Create a Build".into(),
                        params: json!({"name":"Dependent Build","request":"Synthetic request"})
                            .as_object()
                            .unwrap()
                            .clone(),
                        depends_on: vec![first],
                        staged_by: StagedBy::label("Fixture"),
                    },
                    &AtomicBool::new(false),
                    Duration::from_secs(2),
                )
                .unwrap();
        }
    })
    .await
    .unwrap();
    let waiting = pending(&server, &headers).await;
    assert_eq!(waiting[1]["depends_on"], json!([first]));
    let (blocked, error) = confirm(&server, &headers, dependent).await;
    assert!(error, "{blocked}");
    assert_eq!(blocked["code"], "dependency_blocked");
    assert_eq!(blocked["blocked_by"]["id"], first);
    assert_eq!(builds(&server), before);

    let (confirmed, error) = confirm(&server, &headers, &first).await;
    assert!(!error, "{confirmed}");
    let (confirmed, error) = confirm(&server, &headers, dependent).await;
    assert!(!error, "{confirmed}");
    assert_eq!(builds(&server), before + 2);

    let (proposal, error) = json_call(
        &server,
        &headers,
        "propose_create_build",
        json!({ "name": "Expired Build", "request": "Synthetic request" }),
    )
    .await;
    assert!(!error, "{proposal}");
    let expired = proposal["action"]["id"].as_str().unwrap();
    database(&server)
        .execute(
            "UPDATE pending_actions SET expires_at=created_at WHERE id=?1",
            [expired],
        )
        .unwrap();
    let waiting = pending(&server, &headers).await;
    assert_eq!(waiting[0]["state"], "expired");
    let (refused, error) = confirm(&server, &headers, expired).await;
    assert!(error, "{refused}");
    assert_eq!(refused["code"], "expired");
    assert_eq!(refused["expires_at"], waiting[0]["expires_at"]);
    assert_eq!(builds(&server), before + 2);
}
