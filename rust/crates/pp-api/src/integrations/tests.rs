use super::*;
use axum::{body::to_bytes, http::StatusCode};
use pp_storage::{
    WriterFailure,
    auth::AuthFailure,
    printer_setup::{DocumentFault, InvalidDocument},
};
use serde_json::{Value, json};

/// The status and problem body `error` answers with under `wording`.
pub(crate) async fn refusal(error: PrinterSetupError, wording: &Wording) -> (u16, Value) {
    let response = refused(&error, wording);
    assert!(problem::is_problem(&response));
    let status = response.status().as_u16();
    let bytes = to_bytes(response.into_body(), 1 << 20).await.unwrap();
    (status, serde_json::from_slice(&bytes).unwrap())
}

async fn answered(error: PrinterSetupError) -> (u16, Value) {
    refusal(error, &WORDING).await
}

#[tokio::test]
async fn each_error_variant_has_one_status_and_code() {
    let fault = |document| InvalidDocument {
        document,
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
            PrinterSetupError::NotEditable,
            409,
            json!(code::INTEGRATION_NOT_EDITABLE),
        ),
        (
            PrinterSetupError::StoredDocumentInvalid(fault(SetupDocument::Integrations)),
            500,
            json!(code::INTEGRATIONS_UNREADABLE),
        ),
        (
            PrinterSetupError::StoredDocumentInvalid(fault(SetupDocument::Printers)),
            500,
            json!(code::PRINTERS_UNREADABLE),
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
        invalid(Field::Password, Reason::NotResent),
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
            {"pointer": "/password", "detail": "must be sent again when the address changes"},
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
        let response = unreadable(&bytes, found, &WORDING);
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

/// Null clears a credential and keeps any other field; the kind is not a
/// field.
#[test]
fn an_update_body_tells_a_null_credential_from_an_absent_one() {
    let read = |body: Value| {
        problem::parse_body::<UpdateIntegrationRequest>(body.to_string().as_bytes()).ok()
    };
    let kept = read(json!({"name": null, "base_url": null, "enabled": null})).unwrap();
    assert!(kept.name.is_none() && kept.base_url.is_none() && kept.enabled.is_none());
    assert!(kept.username.is_none() && kept.api_key.is_none() && kept.password.is_none());
    let cleared = read(json!({"username": null, "api_key": null, "password": "pw"})).unwrap();
    assert_eq!(
        (cleared.username, cleared.api_key),
        (Some(None), Some(None))
    );
    assert_eq!(cleared.password, Some(Some("pw".into())));
    assert!(read(json!({"kind": "prusalink"})).is_none());
}

/// The whole body of one answer, built from its literal members.
pub(crate) fn body(kind: &str, title: &str, status: u16, detail: &str, code: &str) -> Value {
    json!({"type": format!("urn:print-partner:problem:{kind}"), "title": title,
        "status": status, "detail": detail, "code": code})
}

/// The whole body of every answer, so a change to the mapping cannot move a
/// type, title, detail or code unseen.
#[tokio::test]
async fn each_error_answers_one_whole_body() {
    let fault = |document| InvalidDocument {
        document,
        entry: None,
        reason: DocumentFault::WrongShape,
    };
    let unreadable = |document| PrinterSetupError::StoredDocumentInvalid(fault(document));
    let conflict = |detail, code| body("conflict", "Conflict", 409, detail, code);
    let internal = |detail, code| body("internal", "Internal Error", 500, detail, code);
    let mut invalid = body(
        "validation",
        "Validation Failed",
        400,
        "Request is invalid",
        "invalid_integration",
    );
    invalid["errors"] = json!([{"pointer": "/name", "detail": "must not be empty"}]);
    let empty_name = InvalidField {
        field: Field::Name,
        reason: Reason::Empty,
    };
    #[rustfmt::skip]
    let cases = [
        (PrinterSetupError::NotFound,
            body("not-found", "Not Found", 404, "Integration not found", "integration_not_found")),
        (PrinterSetupError::LimitReached,
            conflict("The integration limit is reached. Delete one first.", "integration_limit")),
        (PrinterSetupError::NotEditable,
            conflict("This kind of integration cannot be edited here", "integration_not_editable")),
        (PrinterSetupError::SlotHoldsFilament,
            conflict("A filament slot this removes still holds filament. Unload it first.", "filament_slot_in_use")),
        (unreadable(SetupDocument::Integrations),
            internal("Stored integrations are unreadable", "integrations_unreadable")),
        (unreadable(SetupDocument::Printers),
            internal("Stored printers are unreadable", "printers_unreadable")),
        (PrinterSetupError::Invalid(vec![empty_name]), invalid),
        (PrinterSetupError::Writer(WriterFailure::Busy),
            body("unavailable", "Service Unavailable", 503, "Service temporarily unavailable", "transaction_unavailable")),
    ];
    for (error, expected) in cases {
        let label = format!("{error:?}");
        let (status, answered) = answered(error).await;
        assert_eq!(json!(status), expected["status"], "{label}");
        assert_eq!(answered, expected, "{label}");
    }
    let response = unreadable_body(b"not json");
    let bytes = to_bytes(response.into_body(), 1 << 20).await.unwrap();
    assert_eq!(
        serde_json::from_slice::<Value>(&bytes).unwrap(),
        body(
            "malformed-request",
            "Malformed Request",
            400,
            "Request is invalid",
            "invalid_integration"
        )
    );
}

fn unreadable_body(bytes: &[u8]) -> Response {
    unreadable(bytes, Vec::new(), &WORDING)
}
