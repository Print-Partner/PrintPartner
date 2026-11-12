use super::*;
use axum::body::to_bytes;
use pp_storage::{
    WriterFailure,
    auth::AuthFailure,
    printer_setup::{DocumentFault, InvalidDocument},
};
use serde_json::{Value, json};

async fn answered(error: PrinterSetupError) -> (u16, Value) {
    let response = refused(&error);
    assert!(problem::is_problem(&response));
    let status = response.status().as_u16();
    let bytes = to_bytes(response.into_body(), 1 << 20).await.unwrap();
    (status, serde_json::from_slice(&bytes).unwrap())
}

#[tokio::test]
async fn each_error_variant_has_one_status_and_code() {
    let fault = InvalidDocument {
        entry: Some(3),
        reason: DocumentFault::WrongShape,
    };
    let cases = [
        (
            PrinterSetupError::NotFound,
            404,
            json!(code::INTEGRATION_NOT_FOUND),
        ),
        (
            PrinterSetupError::LimitReached,
            409,
            json!(code::INTEGRATION_LIMIT),
        ),
        (
            PrinterSetupError::StoredDocumentInvalid(fault),
            500,
            json!(code::INTEGRATIONS_UNREADABLE),
        ),
        (
            PrinterSetupError::Writer(WriterFailure::Auth(AuthFailure::SessionRequired)),
            401,
            Value::Null,
        ),
        (
            PrinterSetupError::Writer(WriterFailure::QueueFull),
            503,
            Value::Null,
        ),
    ];
    for (error, status, code) in cases {
        let label = format!("{error:?}");
        let (answered_status, body) = answered(error).await;
        assert_eq!((answered_status, &body["code"]), (status, &code), "{label}");
        assert_eq!(body["errors"], Value::Null, "{label}");
    }
}

#[tokio::test]
async fn an_invalid_request_names_every_field_and_no_value() {
    let invalid = |field, reason| InvalidField { field, reason };
    let (status, body) = answered(PrinterSetupError::Invalid(vec![
        invalid(Field::Kind, Reason::NotCreatable),
        invalid(Field::Name, Reason::Empty),
        invalid(Field::BaseUrl, Reason::HasCredentials),
        invalid(Field::Username, Reason::NotUsedByKind),
        invalid(Field::ApiKey, Reason::TooLong),
        invalid(Field::Password, Reason::NotUsedByKind),
    ]))
    .await;
    assert_eq!(status, 400);
    assert_eq!(body["type"], "urn:print-partner:problem:validation");
    assert_eq!(body["code"], code::INVALID_INTEGRATION);
    assert_eq!(
        body["errors"],
        json!([
            {"pointer": "/kind", "detail": "cannot be created here"},
            {"pointer": "/name", "detail": "must not be empty"},
            {"pointer": "/base_url", "detail": "must not contain a username or password"},
            {"pointer": "/username", "detail": "is not used by this kind"},
            {"pointer": "/api_key", "detail": "is too long"},
            {"pointer": "/password", "detail": "is not used by this kind"},
        ])
    );
}

/// A body serde refuses names the field, never its value, and a body that is
/// not JSON is malformed.
#[tokio::test]
async fn an_unreadable_body_is_typed_without_echoing_it() {
    let body = |value: Value| value.to_string().into_bytes();
    let secret = "body-secret-canary";
    let cases = [
        (
            body(
                json!({"kind": "octoprint", "name": "x", "base_url": "http://x.test", "api_key": secret}),
            ),
            "urn:print-partner:problem:validation",
            json!([{"pointer": "/kind", "detail": "is invalid"}]),
        ),
        (
            body(
                json!({"kind": "moonraker", "name": "x", "base_url": "http://x.test", "token": secret}),
            ),
            "urn:print-partner:problem:validation",
            json!([{"pointer": "/token", "detail": "is not allowed"}]),
        ),
        (
            body(json!({"kind": "moonraker", "base_url": "http://x.test", "api_key": secret})),
            "urn:print-partner:problem:validation",
            json!([{"pointer": "/name", "detail": "is required"}]),
        ),
        (
            format!("{{\"api_key\": \"{secret}\"").into_bytes(),
            "urn:print-partner:problem:malformed-request",
            Value::Null,
        ),
    ];
    for (bytes, kind, errors) in cases {
        let Err(found) = problem::parse_body::<CreateIntegrationRequest>(&bytes) else {
            panic!("the body parsed")
        };
        let readable = problem::is_json_object(&bytes) || !found.is_empty();
        let response = invalid(readable, found);
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        let bytes = to_bytes(response.into_body(), 1 << 20).await.unwrap();
        let text = String::from_utf8(bytes.to_vec()).unwrap();
        assert!(!text.contains(secret), "{text}");
        let answered: Value = serde_json::from_str(&text).unwrap();
        assert_eq!(
            (&answered["type"], &answered["errors"]),
            (&json!(kind), &errors)
        );
    }
}
