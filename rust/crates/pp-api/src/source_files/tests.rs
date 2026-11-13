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
