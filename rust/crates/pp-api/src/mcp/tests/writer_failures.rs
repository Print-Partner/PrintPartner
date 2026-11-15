//! Every [`WriterFailure`] and its two answers: the HTTP problem, as status
//! and whole body, and the MCP refusal, as code and message. The two differ
//! on purpose: MCP has no 403 and no `transaction_unavailable`.
use crate::mcp::write_result::{Code, writer_failure};
use crate::problem;
use axum::body::to_bytes;
use pp_storage::{
    WriterFailure as W,
    auth::{AuthFailure as A, AuthInputFailure as I},
};
use serde_json::{Value, json};

fn body(kind: &str, title: &str, status: u16, detail: &str, code: Option<&str>) -> Value {
    let mut body = json!({"type": format!("urn:print-partner:problem:{kind}"), "title": title,
        "status": status, "detail": detail});
    if let Some(code) = code {
        body["code"] = json!(code);
    }
    body
}

/// Every `AuthFailure` there is. `pp-api` sorts an unlisted one to `Internal`
/// through `Auth(_)`, so a new variant would get that answer unseen. This
/// match has no wildcard: a new variant fails to compile here. Add it to this
/// match, to its row in the test below, and to the count there.
fn is_auth(failure: &W) -> bool {
    let W::Auth(auth) = failure else {
        return false;
    };
    match auth {
        A::InvalidInput(
            I::TooLong | I::Email | I::ProviderIdentity | I::PasswordTooShort | I::Policy,
        )
        | A::InvalidCredentials
        | A::SessionRequired
        | A::CurrentPasswordIncorrect
        | A::OAuthOnlyAccount
        | A::DuplicateEmail
        | A::RegistrationClosed
        | A::SingleAccountExists
        | A::OwnerMappingRequired
        | A::CredentialChanged
        | A::QueueFull
        | A::Stopped
        | A::CommitUnknown
        | A::Storage => true,
    }
}

#[tokio::test]
async fn each_writer_failure_keeps_its_http_problem_and_its_mcp_refusal() {
    const DOWN: &str = "Service temporarily unavailable";
    const BUSY: &str = "Storage is busy. Nothing was written; try again.";
    const KEY: &str = "The API key was not accepted.";
    let unavailable = |code| body("unavailable", "Service Unavailable", 503, DOWN, code);
    #[rustfmt::skip]
    let rows = [
        (vec![W::Auth(A::SessionRequired), W::Auth(A::InvalidCredentials), W::Auth(A::CredentialChanged)],
            body("unauthenticated", "Authentication Required", 401, "Authentication required", None),
            Code::Unauthorized, KEY),
        (vec![W::Auth(A::OwnerMappingRequired)],
            body("forbidden", "Forbidden", 403, "Explicit account owner mapping is required", None),
            Code::Unauthorized, KEY),
        (vec![W::QueueFull, W::Auth(A::QueueFull)],
            body("overloaded", "Service Overloaded", 503, DOWN, None), Code::Unavailable, BUSY),
        (vec![W::Busy], unavailable(Some("transaction_unavailable")), Code::Unavailable, BUSY),
        (vec![W::Auth(A::CommitUnknown)],
            body("outcome-unknown", "Outcome Unknown", 503, DOWN, Some("outcome_unknown")),
            Code::OutcomeUnknown, "The result is unknown. Check the Builds before proposing again."),
        (vec![W::Stopped, W::Cancelled, W::Auth(A::Stopped), W::Auth(A::Storage)],
            unavailable(None), Code::Unavailable, BUSY),
        (vec![W::Auth(A::InvalidInput(I::TooLong)), W::Auth(A::InvalidInput(I::Email)),
            W::Auth(A::InvalidInput(I::ProviderIdentity)), W::Auth(A::InvalidInput(I::PasswordTooShort)),
            W::Auth(A::InvalidInput(I::Policy)), W::Auth(A::CurrentPasswordIncorrect),
            W::Auth(A::OAuthOnlyAccount), W::Auth(A::DuplicateEmail), W::Auth(A::RegistrationClosed),
            W::Auth(A::SingleAccountExists), W::Internal(anyhow::anyhow!("cause-canary"))],
            body("internal", "Internal Error", 500, "Internal Server Error", None),
            Code::Internal, "Storage failed. Nothing was written."),
    ];
    // 13 unit variants and the 5 kinds of `InvalidInput`, each named once.
    let named: std::collections::BTreeSet<String> = rows
        .iter()
        .flat_map(|row| row.0.iter().filter(|failure| is_auth(failure)))
        .map(|failure| format!("{failure:?}"))
        .collect();
    assert_eq!(named.len(), 18, "{named:?}");
    for (failures, expected, code, message) in rows {
        for failure in failures {
            let response = problem::writer_failure(&failure);
            assert!(problem::is_problem(&response), "{failure:?}");
            let status = response.status().as_u16();
            let bytes = to_bytes(response.into_body(), 1 << 16).await.unwrap();
            let answered: Value = serde_json::from_slice(&bytes).unwrap();
            assert_eq!(json!(status), expected["status"], "{failure:?}");
            assert_eq!(answered, expected, "{failure:?}");
            let refusal = writer_failure(&failure);
            assert_eq!(
                (refusal.code, refusal.detail),
                (code, message),
                "{failure:?}"
            );
        }
    }
}
