use super::*;
use axum::{
    Json, Router,
    body::to_bytes,
    extract::Query,
    http::Request,
    middleware,
    routing::{get, post},
};
use serde_json::json;
use tower::ServiceExt;

async fn read(response: Response) -> (StatusCode, axum::http::HeaderMap, Value) {
    let status = response.status();
    let headers = response.headers().clone();
    let bytes = to_bytes(response.into_body(), 1 << 20).await.unwrap();
    (status, headers, serde_json::from_slice(&bytes).unwrap())
}

#[tokio::test]
async fn problem_response_builder_sets_media_type_and_keeps_standard_members() {
    let mut extensions = Map::new();
    extensions.insert("workspace".into(), json!({"id": 1}));
    extensions.insert("type".into(), json!("about:blank"));
    extensions.insert("detail".into(), json!("replaced"));
    let problem = problem(
        StatusCode::CONFLICT,
        "Plan draft changed",
        Some("draft_changed"),
    );
    let (status, headers, body) = read(build(problem, extensions)).await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(headers[header::CONTENT_TYPE], PROBLEM_CONTENT_TYPE);
    assert_eq!(headers[header::CACHE_CONTROL], "private, no-store");
    assert_eq!(
        body,
        json!({"type": "urn:print-partner:problem:precondition-failed",
            "title": "Precondition Failed", "status": 409, "detail": "Plan draft changed",
            "code": "draft_changed", "workspace": {"id": 1}})
    );
    let (_, _, body) = read(response(StatusCode::BAD_REQUEST, INVALID_JSON)).await;
    assert_eq!(body["type"], "urn:print-partner:problem:malformed-request");
    let (status, _, body) = read(overloaded("Busy")).await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(body["type"], "urn:print-partner:problem:overloaded");
}

#[derive(Debug, serde::Deserialize)]
struct Limit {
    limit: u8,
}

/// axum's own rejections (query, JSON, 405) carry their parser text; the
/// layer replaces them with a problem for the same status.
#[tokio::test]
async fn problem_response_layer_replaces_extractor_rejections() {
    let router = Router::new()
        .route("/query", get(|Query(_): Query<Limit>| async { "ok" }))
        .route("/json", post(|Json(_): Json<Limit>| async { "ok" }))
        .route_layer(middleware::map_response(layer));
    for (method, uri, content_type, body, status) in [
        ("GET", "/query?limit=canary-7f3a", "", "", 400),
        (
            "POST",
            "/json",
            "application/json",
            r#"{"limit":"canary-7f3a"}"#,
            422,
        ),
        ("POST", "/json", "text/plain", "{}", 415),
        ("DELETE", "/json", "", "", 405),
    ] {
        let kind = if status == 405 {
            "method-not-allowed"
        } else {
            "malformed-request"
        };
        let mut request = Request::builder().method(method).uri(uri);
        if !content_type.is_empty() {
            request = request.header(header::CONTENT_TYPE, content_type);
        }
        let response = router
            .clone()
            .oneshot(request.body(Body::from(body)).unwrap())
            .await
            .unwrap();
        let (got, headers, body) = read(response).await;
        assert_eq!(got.as_u16(), status, "{method} {uri}");
        assert_eq!(headers[header::CONTENT_TYPE], PROBLEM_CONTENT_TYPE);
        assert_eq!(body["type"], format!("urn:print-partner:problem:{kind}"));
        assert!(!body.to_string().contains("canary"), "{body}");
        if status == 405 {
            assert!(headers.contains_key(header::ALLOW));
        }
    }
}

#[derive(Debug, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct Edit {
    name: String,
    items: Vec<Limit>,
}

#[test]
fn problem_response_parse_body_names_the_field_without_its_value() {
    let errors = |body: &str| parse_body::<Edit>(body.as_bytes()).unwrap_err();
    #[rustfmt::skip]
    let cases = [
        ("not json", None),
        (r#"{"name":"a","items":[]} trailing"#, None),
        (r#"{"items":[]}"#, Some(("/name", "is required"))),
        (r#"{"name":"a","items":[{"limit":"canary-7f3a"}]}"#, Some(("/items/0/limit", "is invalid"))),
        (r#"{"name":"a","items":[],"a/b~c":1}"#, Some(("/a~1b~0c", "is not allowed"))),
    ];
    for (body, expected) in cases {
        let expected: Vec<_> = expected
            .map(|(p, d)| FieldError::new(p, d))
            .into_iter()
            .collect();
        assert_eq!(errors(body), expected, "{body}");
    }
    let long = format!(r#"{{"name":"a","items":[],"{}":1}}"#, "k".repeat(4096));
    // No whole token fits in 256 bytes, so no field is named.
    assert_eq!(errors(&long), vec![]);
    let parsed = parse_body::<Edit>(br#"{"name":"a","items":[{"limit":2}]}"#).unwrap();
    assert_eq!((parsed.name.as_str(), parsed.items[0].limit), ("a", 2));
}

#[test]
fn problem_response_without_envelope_requires_a_consistent_envelope() {
    let problem = |title| {
        json!({"type": "urn:print-partner:problem:not-found", "title": title,
        "status": 404, "detail": "x"})
    };
    assert_eq!(
        without_envelope(problem("Not Found")),
        json!({"detail": "x"})
    );
    assert_eq!(without_envelope(problem("Gone")), problem("Gone"));
    assert_eq!(
        without_envelope(json!({"detail": "x"})),
        json!({"detail": "x"})
    );
}
