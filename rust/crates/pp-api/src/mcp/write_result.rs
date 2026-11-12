//! The refusal codes MCP write tools answer, and the one mapping from a
//! storage [`WriterFailure`] to an MCP result: the MCP counterpart of
//! `problem::writer_failure`. [`Code`] is the closed list `docs/mcp.md`
//! documents; a new write tool adds its codes here.
//!
//! A refused propose answers `{"error": detail, "code": code}` with
//! `isError`. A refused confirm answers `{"ok": false, "code": code,
//! "detail": detail}` and keeps the proposal pending, except for
//! `outcome_unknown`, which also carries `"outcome_unknown": true` so the
//! session drops the proposal instead of risking a second apply.

use pp_storage::{WriterFailure, auth::AuthFailure};
use serde_json::{Value, json};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Code {
    /// Storage did not accept the request's API key.
    Unauthorized,
    /// Storage is busy, stopping or its queue is full. Nothing was written.
    Unavailable,
    /// The write may or may not have committed.
    OutcomeUnknown,
    /// A storage failure no caller can act on. Its cause is never sent.
    Internal,
    // propose_create_build
    BlankName,
    NameTooLong,
    BlankRequest,
    UrlsNotSupported,
    InvalidIdempotencyKey,
    NameTaken,
    OrdersModuleOff,
    ModuleSettingsInvalid,
    IdempotencyRecordInvalid,
}

impl Code {
    pub(super) fn as_str(self) -> &'static str {
        match self {
            Self::Unauthorized => "unauthorized",
            Self::Unavailable => "unavailable",
            Self::OutcomeUnknown => "outcome_unknown",
            Self::Internal => "internal",
            Self::BlankName => "blank_name",
            Self::NameTooLong => "name_too_long",
            Self::BlankRequest => "blank_request",
            Self::UrlsNotSupported => "urls_not_supported",
            Self::InvalidIdempotencyKey => "invalid_idempotency_key",
            Self::NameTaken => "name_taken",
            Self::OrdersModuleOff => "orders_module_off",
            Self::ModuleSettingsInvalid => "module_settings_invalid",
            Self::IdempotencyRecordInvalid => "idempotency_record_invalid",
        }
    }
}

/// A refusal: its code and a plain message for the assistant.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct Refusal {
    pub(super) code: Code,
    pub(super) detail: &'static str,
}

impl Refusal {
    pub(super) const fn new(code: Code, detail: &'static str) -> Self {
        Self { code, detail }
    }

    /// The text of a refused propose.
    pub(super) fn proposal(self) -> String {
        json!({ "error": self.detail, "code": self.code.as_str() }).to_string()
    }

    /// The result of a refused confirm.
    pub(super) fn confirmation(self) -> Value {
        let mut result = json!({ "ok": false, "code": self.code.as_str(), "detail": self.detail });
        if self.code == Code::OutcomeUnknown {
            result["outcome_unknown"] = Value::Bool(true);
        }
        result
    }
}

/// The refusal for a failure every write through the write seam shares.
pub(super) fn writer_failure(failure: &WriterFailure) -> Refusal {
    use AuthFailure::{
        CommitUnknown, CredentialChanged, InvalidCredentials, OwnerMappingRequired, QueueFull,
        SessionRequired, Stopped, Storage,
    };
    match failure {
        WriterFailure::Auth(
            SessionRequired | InvalidCredentials | CredentialChanged | OwnerMappingRequired,
        ) => Refusal::new(Code::Unauthorized, "The API key was not accepted."),
        WriterFailure::QueueFull
        | WriterFailure::Busy
        | WriterFailure::Stopped
        | WriterFailure::Cancelled
        | WriterFailure::Auth(QueueFull | Stopped | Storage) => Refusal::new(
            Code::Unavailable,
            "Storage is busy. Nothing was written; try again.",
        ),
        WriterFailure::Auth(CommitUnknown) => Refusal::new(
            Code::OutcomeUnknown,
            "The result is unknown. Check the Builds before proposing again.",
        ),
        WriterFailure::Auth(_) | WriterFailure::Internal(_) => {
            Refusal::new(Code::Internal, "Storage failed. Nothing was written.")
        }
    }
}
