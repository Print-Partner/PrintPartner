#![cfg(feature = "desktop_rebuild")]
//! Storage failures on the real upload path keep their type through preflight
//! and the upload client, and answer as every storage write does (GRE-428).
//! Each case drives `SourceUploadService::upload`; nothing is hand-built.

use axum::http::StatusCode;
use pp_api::{
    catalog::{SourceUploads, UploadFailure, UploadKind, UploadParts},
    problem::writer_failure,
};
use pp_core::{
    rebuild_api::desktop_policy,
    source_acquisition::SourceAcquisitions,
    source_uploads::{IMPORT_QUOTA, IMPORT_SLOTS, SourceUploadService},
};
use pp_storage::{
    Limits, WriterFailure, WriterOwner,
    auth::{Outcome as AuthOutcome, Request as AuthRequest, Secret},
    catalog::Credentials,
};
use std::{io, sync::Arc, time::Duration};

/// One storage owner with an upload service over it, opened in `root`.
fn opened(root: &tempfile::TempDir) -> (WriterOwner, SourceUploadService) {
    let data = std::fs::canonicalize(root.path()).unwrap().join("data");
    let owner = WriterOwner::open(&data, Limits::default()).unwrap().0;
    let acquisitions =
        SourceAcquisitions::new(&owner, desktop_policy(), IMPORT_QUOTA, IMPORT_SLOTS).unwrap();
    acquisitions.recover_before_accepting().unwrap();
    let service = SourceUploadService::new(Arc::new(acquisitions), data, 0);
    (owner, service)
}

/// Registers the first account and returns its session.
fn signed_in(owner: &WriterOwner) -> Credentials {
    let request = AuthRequest::Register {
        email: "owner@example.test".into(),
        display_name: "Owner".into(),
        password: Secret::new("ordinary-password".into()),
    };
    let auth = owner.auth_with_policy(desktop_policy()).unwrap();
    let reply = auth.submit(request, Arc::default(), Duration::from_secs(5));
    match reply.unwrap().recv().unwrap().unwrap() {
        AuthOutcome::Session { token, .. } => Credentials::Session(token),
        _ => panic!("session expected"),
    }
}

fn stranger() -> Credentials {
    Credentials::Session(Secret::new("x".repeat(64)))
}

/// Uploads one small STL to a Source that does not exist and returns the
/// storage failure with the status the route answers for it.
fn refused(service: &SourceUploadService, credentials: Credentials) -> (WriterFailure, StatusCode) {
    let file: (String, Box<dyn io::Read + Send>) = (
        "a.stl".into(),
        Box::new(io::Cursor::new(b"solid a\nendsolid a\n")),
    );
    let parts: UploadParts = Box::new([Ok(file)].into_iter());
    let Err(UploadFailure::Storage(failure)) =
        service.upload(credentials, 9_999, UploadKind::Files, parts)
    else {
        panic!("expected a storage failure")
    };
    let status = writer_failure(&failure).status();
    (failure, status)
}

#[test]
fn a_refused_credential_answers_like_the_auth_seam() {
    let root = tempfile::tempdir().unwrap();
    let (_owner, service) = opened(&root);
    let (failure, status) = refused(&service, stranger());
    assert!(matches!(failure, WriterFailure::Auth(_)), "{failure:?}");
    assert_eq!(status, StatusCode::UNAUTHORIZED);
}

#[test]
fn a_storage_fault_is_a_server_error() {
    let root = tempfile::tempdir().unwrap();
    let (owner, service) = opened(&root);
    // A Source that does not exist fails inside the admission transaction.
    let (failure, status) = refused(&service, signed_in(&owner));
    assert!(matches!(failure, WriterFailure::Internal(_)), "{failure:?}");
    assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
}

#[test]
fn a_stopped_writer_is_retryable() {
    let root = tempfile::tempdir().unwrap();
    let (owner, service) = opened(&root);
    owner.shutdown().unwrap();
    let (failure, status) = refused(&service, stranger());
    assert!(matches!(failure, WriterFailure::Stopped), "{failure:?}");
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
}

#[test]
fn a_busy_database_is_retryable() {
    let root = tempfile::tempdir().unwrap();
    let (owner, service) = opened(&root);
    let session = signed_in(&owner);
    // Another connection holds the write lock past the writer's 5 s deadline.
    let lock = rusqlite::Connection::open(root.path().join("data/print-partner.db")).unwrap();
    lock.execute_batch("BEGIN EXCLUSIVE").unwrap();
    let (failure, status) = refused(&service, session);
    assert!(matches!(failure, WriterFailure::Busy), "{failure:?}");
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
}
