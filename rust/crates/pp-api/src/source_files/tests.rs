use super::*;
use axum::body::to_bytes;
use pp_storage::WriterFailure;
use serde_json::Value;

async fn answered(error: SourceFilesError) -> (u16, Value) {
    let response = refused(&error);
    let status = response.status().as_u16();
    let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    (status, serde_json::from_slice(&body).unwrap())
}

#[tokio::test]
async fn each_storage_failure_has_one_status_and_code() {
    let cases = [
        (
            SourceFilesError::NotFound,
            404,
            Some(code::SOURCE_NOT_FOUND),
        ),
        (
            SourceFilesError::SnapshotUnavailable,
            500,
            Some(code::SNAPSHOT_UNAVAILABLE),
        ),
        (
            SourceFilesError::TooManyFiles,
            422,
            Some(code::TOO_MANY_FILES),
        ),
        (SourceFilesError::Writer(WriterFailure::Stopped), 503, None),
    ];
    for (error, status, code) in cases {
        let label = error.to_string();
        let (answered, body) = answered(error).await;
        assert_eq!(answered, status, "{label}");
        assert_eq!(body["code"].as_str(), code, "{label}");
    }
}

/// The whole body of every answer, so a change to the mapping cannot move a
/// type, title, detail or code unseen.
#[tokio::test]
async fn each_storage_failure_answers_one_whole_body() {
    let body = |kind: &str, title: &str, status: u16, detail: &str, code: &str| {
        serde_json::json!({"type": format!("urn:print-partner:problem:{kind}"), "title": title,
            "status": status, "detail": detail, "code": code})
    };
    #[rustfmt::skip]
    let cases = [
        (SourceFilesError::NotFound,
            body("not-found", "Not Found", 404, "Source not found", "source_not_found")),
        (SourceFilesError::SnapshotUnavailable,
            body("internal", "Internal Error", 500, "The Source's current revision cannot be read", "source_snapshot_unavailable")),
        (SourceFilesError::TooManyFiles,
            body("validation", "Validation Failed", 422, "The revision holds more than 20000 files", "source_too_many_files")),
        (SourceFilesError::Writer(WriterFailure::Busy),
            body("unavailable", "Service Unavailable", 503, "Service temporarily unavailable", "transaction_unavailable")),
    ];
    for (error, expected) in cases {
        let label = error.to_string();
        let (status, answered) = answered(error).await;
        assert_eq!(serde_json::json!(status), expected["status"], "{label}");
        assert_eq!(answered, expected, "{label}");
    }
}
