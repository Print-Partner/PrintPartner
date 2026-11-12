//! GRE-1353: what a real gateway answers on `/mcp` and `/api/v1/mcp` once the
//! Rust product routes are mounted. Shared by the Node-backed start
//! (`gateway_mount.rs`) and the Rust-only launcher (`runtime_m0/mcp_gateway.rs`).
use serde_json::{Value, json};
use std::time::Duration;

/// Creates an API key with a Rust session and returns it as a bearer value.
pub async fn bearer(client: &reqwest::Client, origin: &str, cookies: &str) -> String {
    let created = client
        .post(format!("{origin}/settings/api-keys"))
        .header("origin", origin)
        .header("cookie", cookies)
        .json(&json!({}))
        .send()
        .await
        .unwrap();
    assert_eq!(created.status(), 201);
    let key: Value = created.json().await.unwrap();
    format!("Bearer {}", key["key"].as_str().unwrap())
}

/// One MCP `initialize` POST with only the given headers: no cookie unless a
/// header carries it. Returns the status and the body up to the `initialize`
/// result or the end, whichever comes first.
async fn initialize(
    client: &reqwest::Client,
    origin: &str,
    path: &str,
    headers: &[(&str, &str)],
) -> (u16, String) {
    let mut request = client
        .post(format!("{origin}{path}"))
        .header("accept", "application/json, text/event-stream")
        .json(&json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "initialize",
            "params": {
                "protocolVersion": "2025-06-18",
                "capabilities": {},
                "clientInfo": {"name": "gateway-mcp", "version": "0"},
            },
        }));
    for (name, value) in headers {
        request = request.header(*name, *value);
    }
    let mut response = request.send().await.unwrap();
    let status = response.status().as_u16();
    let mut body = Vec::new();
    // rmcp opens the SSE stream with an empty priming event and may keep it
    // open after the result, so stop once the result has arrived.
    while let Ok(Ok(Some(chunk))) =
        tokio::time::timeout(Duration::from_secs(5), response.chunk()).await
    {
        body.extend_from_slice(&chunk);
        if body.windows(10).any(|window| window == b"serverInfo") {
            break;
        }
    }
    (status, String::from_utf8(body).unwrap())
}

/// MCP answers on its API key alone, without the desktop cookie. Neither the
/// desktop cookie nor the Rust session is a key, Host and a present Origin
/// stay exact, and every other path still needs the desktop cookie.
/// `desktop` is the desktop cookie; `cookies` adds the Rust session to it.
pub async fn assert_key_only(
    client: &reqwest::Client,
    origin: &str,
    desktop: &str,
    cookies: &str,
    bearer: &str,
) {
    let localhost = origin
        .strip_prefix("http://")
        .unwrap()
        .replace("127.0.0.1", "localhost");
    for path in ["/mcp", "/api/v1/mcp"] {
        // A key, no cookie, no Origin: how an MCP client connects.
        for headers in [
            vec![("authorization", bearer)],
            vec![("authorization", bearer), ("origin", origin)],
        ] {
            let (status, body) = initialize(client, origin, path, &headers).await;
            assert_eq!(status, 200, "{path} {headers:?}: {body}");
            assert!(body.contains("serverInfo"), "{path}: {body}");
        }
        for (label, headers, expected, detail) in [
            // The MCP guard refuses a keyless request.
            ("no key", vec![], 403, "Invalid request origin"),
            (
                "no key, Origin",
                vec![("origin", origin)],
                401,
                "Valid API key required",
            ),
            // Neither cookie is a key, alone or next to one.
            (
                "desktop cookie",
                vec![("origin", origin), ("cookie", desktop)],
                401,
                "Valid API key required",
            ),
            (
                "both cookies",
                vec![("origin", origin), ("cookie", cookies)],
                401,
                "Valid API key required",
            ),
            (
                "both cookies and a key",
                vec![
                    ("origin", origin),
                    ("cookie", cookies),
                    ("authorization", bearer),
                ],
                401,
                "Valid API key required",
            ),
            // The gateway keeps Host and a present Origin exact.
            (
                "foreign Host",
                vec![("authorization", bearer), ("host", "evil.example")],
                400,
                "Invalid Host",
            ),
            (
                "Host by name",
                vec![("authorization", bearer), ("host", localhost.as_str())],
                400,
                "Invalid Host",
            ),
            (
                "foreign Origin",
                vec![("authorization", bearer), ("origin", "http://evil.example")],
                403,
                "Invalid Origin",
            ),
        ] {
            let (status, body) = initialize(client, origin, path, &headers).await;
            assert_eq!(status, expected, "{path} {label}: {body}");
            assert!(body.contains(detail), "{path} {label}: {body}");
        }
    }
    // A key opens no other path: the cookie gate answers first.
    for path in ["/api/v1/plans", "/mcp/", "/api/v1/mcp/sessions"] {
        let response = client
            .post(format!("{origin}{path}"))
            .header("origin", origin)
            .header("authorization", bearer)
            .json(&json!({}))
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), 401, "{path}");
        assert!(
            response
                .text()
                .await
                .unwrap()
                .contains("Desktop session required"),
            "{path}"
        );
    }
}
