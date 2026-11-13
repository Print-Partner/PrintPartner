//! The refusal codes MCP tools answer, and the one mapping from a
//! storage [`WriterFailure`] to an MCP result: the MCP counterpart of
//! `problem::writer_failure`. [`Code`] is the closed list `docs/mcp.md`
//! documents; a new write tool adds its codes here.
//!
//! A refused propose answers `{"error": detail, "code": code}` with
//! `isError`. A refused confirm answers `{"ok": false, "code": code,
//! "detail": detail}` and keeps the proposal pending, except for
//! `outcome_unknown`, which also carries `"outcome_unknown": true` so the
//! session drops the proposal instead of risking a second apply.

use pp_services::mcp_domain::ToolInvocation;
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
    // propose_edit_plan_draft_parts
    ArgumentsRequired,
    NotFound,
    NotOpen,
    PartsRequired,
    InvalidEdit,
    PartNotFound,
    EditEmpty,
    InvalidDigest,
    DraftChanged,
    BaseChanged,
    AcceptedBaselineRequired,
    EditRefused,
    // get_build_planning_state, get_plan_draft, propose_set_build_source_roles,
    // propose_import_build_inputs
    InvalidPlanId,
    PlanNotFound,
    NoPlanningBrief,
    // propose_set_build_source_roles
    InvalidRoles,
    PlanningBriefInvalid,
    InvalidRoleAssignment,
    InvalidSourceRole,
    EvidenceNotFound,
    MultipleStructuralBases,
    // farm tools
    FarmModuleOff,
    // propose_import_build_inputs
    InvalidInputs,
    InvalidInput,
    InvalidRepositoryUrl,
    SourceNameTaken,
    InvalidEvidenceKind,
    InvalidUrl,
    SourceNotFound,
    DerivedEvidenceNotFound,
    // get_print_stats
    InvalidHours,
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
            Self::ArgumentsRequired => "arguments_required",
            Self::NotFound => "not_found",
            Self::NotOpen => "not_open",
            Self::PartsRequired => "parts_required",
            Self::InvalidEdit => "invalid_edit",
            Self::PartNotFound => "part_not_found",
            Self::EditEmpty => "edit_empty",
            Self::InvalidDigest => "invalid_digest",
            Self::DraftChanged => "draft_changed",
            Self::BaseChanged => "base_changed",
            Self::AcceptedBaselineRequired => "accepted_baseline_required",
            Self::EditRefused => "edit_refused",
            Self::InvalidPlanId => "invalid_plan_id",
            Self::PlanNotFound => "plan_not_found",
            Self::NoPlanningBrief => "no_planning_brief",
            Self::InvalidRoles => "invalid_roles",
            Self::PlanningBriefInvalid => "planning_brief_invalid",
            Self::InvalidRoleAssignment => "invalid_role_assignment",
            Self::InvalidSourceRole => "invalid_source_role",
            Self::EvidenceNotFound => "evidence_not_found",
            Self::MultipleStructuralBases => "multiple_structural_bases",
            Self::FarmModuleOff => "farm_module_off",
            Self::InvalidInputs => "invalid_inputs",
            Self::InvalidInput => "invalid_input",
            Self::InvalidRepositoryUrl => "invalid_repository_url",
            Self::SourceNameTaken => "source_name_taken",
            Self::InvalidEvidenceKind => "invalid_evidence_kind",
            Self::InvalidUrl => "invalid_url",
            Self::SourceNotFound => "source_not_found",
            Self::DerivedEvidenceNotFound => "derived_evidence_not_found",
            Self::InvalidHours => "invalid_hours",
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

    /// A refused propose, as the tool's invocation.
    pub(super) fn invocation(self) -> ToolInvocation {
        ToolInvocation {
            content: self.proposal(),
            proposed_action: None,
        }
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

/// The refusal for a failed read through a client that reports a full or
/// stopped writer queue as a plain message: `problem::writer_admission`
/// classifies it, as the HTTP routes do, so it answers `unavailable`.
pub(super) fn read_failure(error: anyhow::Error) -> Refusal {
    use crate::problem::{Admission, writer_admission};
    writer_failure(&match writer_admission(&error) {
        Some(Admission::Full) => WriterFailure::QueueFull,
        Some(Admission::Stopped) => WriterFailure::Stopped,
        None => WriterFailure::from(error),
    })
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
