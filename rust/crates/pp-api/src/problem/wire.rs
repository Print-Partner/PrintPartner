//! The problem document DTO: a stable [`ProblemType`], a fixed title per type,
//! and bounded text and field errors. See `docs/problem-responses.md`.
use serde::{Deserialize, Serialize};

/// Media type of every Rust API error body.
pub const PROBLEM_CONTENT_TYPE: &str = "application/problem+json";
/// Longest `detail`, in bytes. Longer text is cut at a character boundary.
pub const MAX_DETAIL_BYTES: usize = 512;
/// Longest `code`, in bytes.
pub const MAX_CODE_BYTES: usize = 64;
/// Most entries in `errors`. Further field errors are dropped.
pub const MAX_FIELD_ERRORS: usize = 16;
/// Longest field error `pointer`, in bytes.
pub const MAX_POINTER_BYTES: usize = 256;
/// Longest field error `detail`, in bytes.
pub const MAX_FIELD_DETAIL_BYTES: usize = 128;

/// Declares [`ProblemType`] with each variant's `type` URN and fixed
/// title, and [`ProblemType::ALL`] from the same list.
macro_rules! problem_types {
    ($($(#[$doc:meta])* $variant:ident = $urn:literal, $title:literal;)+) => {
        /// Stable problem kinds. The serialized value is the problem `type` URN.
        #[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
        #[cfg_attr(feature = "codegen", derive(schemars::JsonSchema))]
        pub enum ProblemType {
            $(
                $(#[$doc])*
                #[serde(rename = $urn)]
                $variant,
            )+
        }

        impl ProblemType {
            /// Every variant, in declaration order.
            pub const ALL: &'static [Self] = &[$(Self::$variant,)+];

            /// The fixed `title` for this type.
            pub fn title(self) -> &'static str {
                match self {
                    $(Self::$variant => $title,)+
                }
            }
        }
    };
}

problem_types! {
    /// The request could not be read: unparsable JSON, a wrong media type, an
    /// oversized body or target, a bad path or query, or a body that timed out.
    MalformedRequest = "urn:print-partner:problem:malformed-request", "Malformed Request";
    /// The request was readable but a value is invalid.
    Validation = "urn:print-partner:problem:validation", "Validation Failed";
    /// No valid session or API key.
    Unauthenticated = "urn:print-partner:problem:unauthenticated", "Authentication Required";
    /// The caller is known but not allowed, or the transport is refused.
    Forbidden = "urn:print-partner:problem:forbidden", "Forbidden";
    /// The target does not exist or belongs to another tenant.
    NotFound = "urn:print-partner:problem:not-found", "Not Found";
    /// The route exists but not for this method.
    MethodNotAllowed = "urn:print-partner:problem:method-not-allowed", "Method Not Allowed";
    /// The target's state does not allow the operation.
    Conflict = "urn:print-partner:problem:conflict", "Conflict";
    /// The version or digest the request expected is no longer current.
    PreconditionFailed = "urn:print-partner:problem:precondition-failed", "Precondition Failed";
    /// Admission or the writer queue is full. Retry later.
    Overloaded = "urn:print-partner:problem:overloaded", "Service Overloaded";
    /// The request or an upstream call ran out of time.
    Timeout = "urn:print-partner:problem:timeout", "Timed Out";
    /// The service is stopping, cancelled the request or is otherwise down.
    Unavailable = "urn:print-partner:problem:unavailable", "Service Unavailable";
    /// A write may or may not have committed. Reload and reconcile; do not
    /// blindly retry.
    OutcomeUnknown = "urn:print-partner:problem:outcome-unknown", "Outcome Unknown";
    /// An upstream provider failed or answered badly.
    Upstream = "urn:print-partner:problem:upstream", "Upstream Failure";
    /// The feature is not configured on this server.
    NotImplemented = "urn:print-partner:problem:not-implemented", "Not Implemented";
    /// An unexpected server failure. The cause is logged, not sent.
    Internal = "urn:print-partner:problem:internal", "Internal Error";
}

/// Declares [`ProblemCode`] with each variant's wire value, and
/// [`ProblemCode::ALL`] and [`ProblemCode::as_str`] from the same list.
macro_rules! problem_codes {
    ($($variant:ident = $wire:literal;)+) => {
        /// The closed list of `code` values that have a typed producer in
        /// `pp-api` or that the replacement UI branches on. The serialized
        /// value is the `code` string. Older families still send codes
        /// outside this list, so [`Problem::code`] stays a string; a family's
        /// codes join when it is rewritten. See `docs/problem-responses.md`.
        #[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize)]
        #[cfg_attr(feature = "codegen", derive(schemars::JsonSchema))]
        pub enum ProblemCode {
            $(
                #[serde(rename = $wire)]
                $variant,
            )+
        }

        impl ProblemCode {
            /// Every variant, in declaration order.
            pub const ALL: &'static [Self] = &[$(Self::$variant,)+];

            /// The `code` string on the wire.
            pub const fn as_str(self) -> &'static str {
                match self {
                    $(Self::$variant => $wire,)+
                }
            }
        }
    };
}

problem_codes! {
    // The draft and History routes; the replacement UI branches on these.
    DraftChanged = "draft_changed";
    BaseChanged = "base_changed";
    HeaderChanged = "header_changed";
    DuplicateBuildName = "duplicate_build_name";
    DuplicateOrder = "duplicate_order";
    BuildArchived = "build_archived";
    DraftNotFound = "draft_not_found";
    IdempotencyConflict = "idempotency_conflict";
    OutcomeUnknown = "outcome_unknown";
    AlreadyApplied = "already_applied";
    ReconciliationRequired = "reconciliation_required";
    SnapshotNotRestorable = "snapshot_not_restorable";
    SnapshotNotFound = "snapshot_not_found";
    InputsChanged = "inputs_changed";
    PrintedUnitsAffected = "printed_units_affected";
    AcceptedBaselineRequired = "accepted_baseline_required";
    RequiredUnitSetUnavailable = "required_unit_set_unavailable";
    InvalidHeader = "invalid_header";
    // Every write through the write seam, with `OutcomeUnknown`.
    TransactionUnavailable = "transaction_unavailable";
    // The integration and printer routes.
    InvalidIntegration = "invalid_integration";
    IntegrationNotFound = "integration_not_found";
    IntegrationLimit = "integration_limit";
    IntegrationNotEditable = "integration_not_editable";
    IntegrationsUnreadable = "integrations_unreadable";
    InvalidPrinter = "invalid_printer";
    PrinterNotFound = "printer_not_found";
    PrinterLimit = "printer_limit";
    PrintersUnreadable = "printers_unreadable";
    FilamentSlotInUse = "filament_slot_in_use";
    // `GET /sources/{id}/files`.
    SourceNotFound = "source_not_found";
    SourceSnapshotUnavailable = "source_snapshot_unavailable";
    SourceTooManyFiles = "source_too_many_files";
}

impl ProblemType {
    /// The type for an error `status` and Node-compatible `code`. A code that
    /// names a precise cause wins over the status.
    pub fn classify(status: u16, code: Option<&str>) -> Self {
        match code {
            Some("writer_queue_full" | "admission_full") => return Self::Overloaded,
            Some("outcome_unknown") => return Self::OutcomeUnknown,
            Some("draft_changed" | "base_changed") => return Self::PreconditionFailed,
            Some("request_cancelled" | "writer_stopped" | "transaction_unavailable") => {
                return Self::Unavailable;
            }
            _ => {}
        }
        match status {
            400 | 422 => Self::Validation,
            401 => Self::Unauthenticated,
            403 => Self::Forbidden,
            404 => Self::NotFound,
            405 => Self::MethodNotAllowed,
            408 | 504 => Self::Timeout,
            409 => Self::Conflict,
            411 | 413 | 414 | 415 | 431 => Self::MalformedRequest,
            412 | 428 => Self::PreconditionFailed,
            429 => Self::Overloaded,
            501 => Self::NotImplemented,
            502 => Self::Upstream,
            503 => Self::Unavailable,
            _ if (400..500).contains(&status) => Self::Validation,
            _ => Self::Internal,
        }
    }
}

/// One invalid field. `pointer` is a JSON Pointer (RFC 6901) into the request
/// body; `detail` says what is wrong without echoing the value.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "codegen", derive(schemars::JsonSchema))]
pub struct FieldError {
    pub pointer: String,
    pub detail: String,
}

impl FieldError {
    pub fn new(pointer: &str, detail: &str) -> Self {
        Self {
            pointer: ancestor(pointer),
            detail: cut(detail, MAX_FIELD_DETAIL_BYTES),
        }
    }
}

/// An RFC 9457 problem document. Routes may add extension members (for
/// example a draft conflict's `workspace`) next to these fields.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "codegen", derive(schemars::JsonSchema))]
pub struct Problem {
    #[serde(rename = "type")]
    pub problem_type: ProblemType,
    pub title: String,
    pub status: u16,
    /// Absent where Node sent none (draft conflicts and domain errors).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "codegen", schemars(with = "String"))]
    pub detail: Option<String>,
    /// The Node-compatible machine code, when the route has one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "codegen", schemars(with = "String"))]
    pub code: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    #[cfg_attr(feature = "codegen", schemars(length(max = "MAX_FIELD_ERRORS")))]
    pub errors: Vec<FieldError>,
}

impl Problem {
    pub fn new(problem_type: ProblemType, status: u16, detail: &str) -> Self {
        Self {
            problem_type,
            title: problem_type.title().to_owned(),
            status,
            detail: Some(cut(detail, MAX_DETAIL_BYTES)),
            code: None,
            errors: Vec::new(),
        }
    }

    /// Drops `detail`, for routes whose Node body had none.
    pub fn without_detail(mut self) -> Self {
        self.detail = None;
        self
    }

    /// Sets a code outside the closed list, for the older families.
    pub fn with_code(mut self, code: &str) -> Self {
        self.code = Some(cut(code, MAX_CODE_BYTES));
        self
    }

    /// Sets a code from the closed list. New families use this.
    pub fn with_problem_code(mut self, code: ProblemCode) -> Self {
        self.code = Some(code.as_str().to_owned());
        self
    }

    /// Adds field errors, keeping at most [`MAX_FIELD_ERRORS`] in total.
    pub fn with_errors(mut self, errors: impl IntoIterator<Item = FieldError>) -> Self {
        let room = MAX_FIELD_ERRORS.saturating_sub(self.errors.len());
        self.errors.extend(errors.into_iter().take(room));
        self
    }
}

/// The longest whole-token prefix of a JSON Pointer that fits
/// [`MAX_POINTER_BYTES`], so an
/// RFC 6901 escape or a UTF-8 character is never split. `""` (the whole
/// document) when even the first token is too long.
fn ancestor(pointer: &str) -> String {
    if pointer.len() <= MAX_POINTER_BYTES {
        return pointer.to_owned();
    }
    // `/` is ASCII, so its byte offset is always a character boundary.
    let bytes = &pointer.as_bytes()[..=MAX_POINTER_BYTES];
    let end = bytes.iter().rposition(|byte| *byte == b'/').unwrap_or(0);
    pointer[..end].to_owned()
}

fn cut(text: &str, limit: usize) -> String {
    if text.len() <= limit {
        return text.to_owned();
    }
    let mut end = limit;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    text[..end].to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn problem_response_types_are_stable_urns_with_fixed_titles() {
        for &kind in ProblemType::ALL {
            let value = serde_json::to_value(kind).unwrap();
            let urn = value.as_str().unwrap();
            assert!(urn.starts_with("urn:print-partner:problem:"), "{urn}");
            assert_eq!(
                serde_json::from_value::<ProblemType>(value.clone()).unwrap(),
                kind
            );
            assert!(!kind.title().is_empty());
        }
        let problem = Problem::new(ProblemType::NotFound, 404, "Source not found").with_code("x");
        assert_eq!(
            serde_json::to_value(&problem).unwrap(),
            serde_json::json!({"type": "urn:print-partner:problem:not-found", "title": "Not Found",
                "status": 404, "detail": "Source not found", "code": "x"})
        );
    }

    #[test]
    fn problem_response_codes_are_unique_and_hold_every_family_constant() {
        use crate::{integrations::code as setup, source_files::code as files};
        let wire: Vec<&str> = ProblemCode::ALL.iter().map(|code| code.as_str()).collect();
        for (index, &code) in ProblemCode::ALL.iter().enumerate() {
            assert!(!wire[..index].contains(&wire[index]), "{code:?} repeats");
            assert!(wire[index].len() <= MAX_CODE_BYTES, "{code:?}");
            assert_eq!(serde_json::json!(code), wire[index]);
            let problem = Problem::new(ProblemType::Conflict, 409, "").with_problem_code(code);
            assert_eq!(problem.code.as_deref(), Some(wire[index]));
        }
        #[rustfmt::skip]
        let constants = [
            setup::INVALID_INTEGRATION, setup::INTEGRATION_NOT_FOUND, setup::INTEGRATION_LIMIT,
            setup::INTEGRATION_NOT_EDITABLE, setup::INTEGRATIONS_UNREADABLE, setup::INVALID_PRINTER,
            setup::PRINTER_NOT_FOUND, setup::PRINTER_LIMIT, setup::PRINTERS_UNREADABLE,
            setup::FILAMENT_SLOT_IN_USE, files::SOURCE_NOT_FOUND, files::SNAPSHOT_UNAVAILABLE,
            files::TOO_MANY_FILES, crate::problem::TRANSACTION_UNAVAILABLE,
            crate::problem::OUTCOME_UNKNOWN,
        ];
        for constant in constants {
            assert!(wire.contains(&constant), "{constant} is not a ProblemCode");
        }
    }

    #[test]
    fn problem_response_classifies_each_error_class() {
        use ProblemType::*;
        #[rustfmt::skip]
        let cases = [
            (400, None, Validation), (422, Some("domain_error"), Validation),
            (413, None, MalformedRequest), (415, None, MalformedRequest),
            (401, None, Unauthenticated), (403, None, Forbidden), (404, None, NotFound),
            (405, None, MethodNotAllowed), (409, Some("not_open"), Conflict),
            (409, Some("draft_changed"), PreconditionFailed), (412, None, PreconditionFailed),
            (503, Some("writer_queue_full"), Overloaded), (429, None, Overloaded),
            (408, None, Timeout), (504, None, Timeout), (503, Some("writer_stopped"), Unavailable),
            (503, Some("outcome_unknown"), OutcomeUnknown), (502, None, Upstream),
            (501, None, NotImplemented), (500, None, Internal),
        ];
        for (status, code, expected) in cases {
            assert_eq!(
                ProblemType::classify(status, code),
                expected,
                "{status} {code:?}"
            );
        }
    }

    #[test]
    fn problem_response_bounds_field_errors_in_count_and_size() {
        let long = "é".repeat(400);
        let problem = Problem::new(ProblemType::Validation, 400, &long)
            .with_code(&long)
            .with_errors((0..40).map(|_| FieldError::new(&long, &long)));
        assert_eq!(problem.errors.len(), MAX_FIELD_ERRORS);
        assert!(problem.detail.as_ref().unwrap().len() <= MAX_DETAIL_BYTES);
        assert!(problem.code.as_ref().unwrap().len() <= MAX_CODE_BYTES);
        for error in &problem.errors {
            assert!(error.pointer.len() <= MAX_POINTER_BYTES);
            assert!(error.detail.len() <= MAX_FIELD_DETAIL_BYTES);
            assert!(error.detail.chars().all(|c| c == 'é'));
        }
        let more = problem.with_errors([FieldError::new("/x", "y")]);
        assert_eq!(more.errors.len(), MAX_FIELD_ERRORS);
    }

    #[test]
    fn problem_response_pointer_truncation_keeps_whole_tokens() {
        // 254 `a` plus the `~1` escape for `/` is 257 bytes: never cut to a bare `~`.
        let escaped = format!("/{}~1", "a".repeat(254));
        assert_eq!(FieldError::new(&escaped, "x").pointer, "");
        assert_eq!(
            FieldError::new(&format!("/items{escaped}"), "x").pointer,
            "/items"
        );
        let wide = format!("/decision/{}", "é".repeat(200));
        assert_eq!(FieldError::new(&wide, "x").pointer, "/decision");
        let fits = format!("/{}~0", "a".repeat(253));
        assert_eq!(FieldError::new(&fits, "x").pointer, fits);
    }
}
