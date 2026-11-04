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

/// Stable problem kinds. The serialized value is the problem `type` URN.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "codegen", derive(schemars::JsonSchema))]
pub enum ProblemType {
    /// The request could not be read: unparsable JSON, a wrong media type, an
    /// oversized body or target, a bad path or query, or a body that timed out.
    #[serde(rename = "urn:print-partner:problem:malformed-request")]
    MalformedRequest,
    /// The request was readable but a value is invalid.
    #[serde(rename = "urn:print-partner:problem:validation")]
    Validation,
    /// No valid session or API key.
    #[serde(rename = "urn:print-partner:problem:unauthenticated")]
    Unauthenticated,
    /// The caller is known but not allowed, or the transport is refused.
    #[serde(rename = "urn:print-partner:problem:forbidden")]
    Forbidden,
    /// The target does not exist or belongs to another tenant.
    #[serde(rename = "urn:print-partner:problem:not-found")]
    NotFound,
    /// The route exists but not for this method.
    #[serde(rename = "urn:print-partner:problem:method-not-allowed")]
    MethodNotAllowed,
    /// The target's state does not allow the operation.
    #[serde(rename = "urn:print-partner:problem:conflict")]
    Conflict,
    /// The version or digest the request expected is no longer current.
    #[serde(rename = "urn:print-partner:problem:precondition-failed")]
    PreconditionFailed,
    /// Admission or the writer queue is full. Retry later.
    #[serde(rename = "urn:print-partner:problem:overloaded")]
    Overloaded,
    /// The request or an upstream call ran out of time.
    #[serde(rename = "urn:print-partner:problem:timeout")]
    Timeout,
    /// The service is stopping, cancelled the request or is otherwise down.
    #[serde(rename = "urn:print-partner:problem:unavailable")]
    Unavailable,
    /// A write may or may not have committed. Reload and reconcile; do not
    /// blindly retry.
    #[serde(rename = "urn:print-partner:problem:outcome-unknown")]
    OutcomeUnknown,
    /// An upstream provider failed or answered badly.
    #[serde(rename = "urn:print-partner:problem:upstream")]
    Upstream,
    /// The feature is not configured on this server.
    #[serde(rename = "urn:print-partner:problem:not-implemented")]
    NotImplemented,
    /// An unexpected server failure. The cause is logged, not sent.
    #[serde(rename = "urn:print-partner:problem:internal")]
    Internal,
}

impl ProblemType {
    #[rustfmt::skip]
    pub const ALL: [Self; 15] = [
        Self::MalformedRequest, Self::Validation, Self::Unauthenticated, Self::Forbidden,
        Self::NotFound, Self::MethodNotAllowed, Self::Conflict, Self::PreconditionFailed,
        Self::Overloaded, Self::Timeout, Self::Unavailable, Self::OutcomeUnknown, Self::Upstream,
        Self::NotImplemented, Self::Internal,
    ];

    /// The fixed `title` for this type.
    pub fn title(self) -> &'static str {
        match self {
            Self::MalformedRequest => "Malformed Request",
            Self::Validation => "Validation Failed",
            Self::Unauthenticated => "Authentication Required",
            Self::Forbidden => "Forbidden",
            Self::NotFound => "Not Found",
            Self::MethodNotAllowed => "Method Not Allowed",
            Self::Conflict => "Conflict",
            Self::PreconditionFailed => "Precondition Failed",
            Self::Overloaded => "Service Overloaded",
            Self::Timeout => "Timed Out",
            Self::Unavailable => "Service Unavailable",
            Self::OutcomeUnknown => "Outcome Unknown",
            Self::Upstream => "Upstream Failure",
            Self::NotImplemented => "Not Implemented",
            Self::Internal => "Internal Error",
        }
    }

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
            pointer: ancestor(pointer, MAX_POINTER_BYTES),
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

    pub fn with_code(mut self, code: &str) -> Self {
        self.code = Some(cut(code, MAX_CODE_BYTES));
        self
    }

    /// Adds field errors, keeping at most [`MAX_FIELD_ERRORS`] in total.
    pub fn with_errors(mut self, errors: impl IntoIterator<Item = FieldError>) -> Self {
        let room = MAX_FIELD_ERRORS.saturating_sub(self.errors.len());
        self.errors.extend(errors.into_iter().take(room));
        self
    }
}

/// The longest whole-token prefix of a JSON Pointer that fits `limit`, so an
/// RFC 6901 escape or a UTF-8 character is never split. `""` (the whole
/// document) when even the first token is too long.
fn ancestor(pointer: &str, limit: usize) -> String {
    if pointer.len() <= limit {
        return pointer.to_owned();
    }
    // `/` is ASCII, so its byte offset is always a character boundary.
    let bytes = &pointer.as_bytes()[..=limit];
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
        for kind in ProblemType::ALL {
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
