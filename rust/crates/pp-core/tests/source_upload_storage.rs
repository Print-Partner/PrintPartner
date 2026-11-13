#![cfg(feature = "desktop_rebuild")]
//! Storage failures on the real upload path keep their type through preflight
//! and the upload client, and answer as every storage write does (GRE-428).
//! Each case drives `SourceUploadService::upload`; nothing is hand-built.

use axum::http::StatusCode;
use pp_api::{
    catalog::{SourceUploads, UploadFailure, UploadParts},
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
use std::{
    io::{self, Read},
    path::PathBuf,
    sync::{Arc, atomic::AtomicBool},
    time::Duration,
};

struct Fixture {
    _root: tempfile::TempDir,
    data: PathBuf,
    owner: WriterOwner,
    service: SourceUploadService,
}

impl Fixture {
    fn open(limits: Limits) -> Self {
        let root = tempfile::tempdir().unwrap();
        let data = std::fs::canonicalize(root.path()).unwrap().join("data");
        let (owner, _) = WriterOwner::open(&data, limits).unwrap();
        let imports = Arc::new(
            SourceAcquisitions::new(&owner, desktop_policy(), IMPORT_QUOTA, IMPORT_SLOTS).unwrap(),
        );
        assert_eq!(imports.recover_before_accepting().unwrap(), 0);
        let service = SourceUploadService::new(imports, data.clone(), 0);
        Self {
            _root: root,
            data,
            owner,
            service,
        }
    }

    fn session(&self) -> Credentials {
        let registered = self
            .owner
            .auth_with_policy(desktop_policy())
            .unwrap()
            .submit(
                AuthRequest::Register {
                    email: "owner@example.test".into(),
                    display_name: "Owner".into(),
                    password: Secret::new("ordinary-password".into()),
                },
                Arc::new(AtomicBool::new(false)),
                Duration::from_secs(5),
            )
            .unwrap()
            .recv()
            .unwrap()
            .unwrap();
        let AuthOutcome::Session { token, .. } = registered else {
            panic!("session expected")
        };
        Credentials::Session(token)
    }

    /// The upload's storage failure, and the status the route answers.
    fn refused(&self, credentials: Credentials) -> (WriterFailure, StatusCode) {
        let part: Box<dyn Read + Send> = Box::new(&b"solid a\nendsolid a\n"[..]);
        let files: UploadParts =
            Box::new(std::iter::once(Ok::<_, io::Error>(("a.stl".into(), part))));
        match self.service.upload(credentials, 9_999, files) {
            Err(UploadFailure::Storage(failure)) => {
                let status = writer_failure(&failure).status();
                (failure, status)
            }
            other => panic!("expected a storage failure, got {other:?}"),
        }
    }

    /// Holds SQLite's write lock from another connection until dropped.
    fn lock_database(&self) -> rusqlite::Connection {
        let connection = rusqlite::Connection::open(self.data.join("print-partner.db")).unwrap();
        connection.execute_batch("BEGIN EXCLUSIVE").unwrap();
        connection
    }
}

#[test]
fn a_refused_credential_answers_like_the_auth_seam() {
    let fixture = Fixture::open(Limits::default());
    let bogus = Credentials::Session(Secret::new("x".repeat(64)));
    let (failure, status) = fixture.refused(bogus);
    assert!(matches!(failure, WriterFailure::Auth(_)), "{failure:?}");
    assert_eq!(status, StatusCode::UNAUTHORIZED);
}

#[test]
fn a_storage_fault_is_a_server_error() {
    let fixture = Fixture::open(Limits::default());
    // A Source that does not exist fails inside the admission transaction.
    let (failure, status) = fixture.refused(fixture.session());
    assert!(matches!(failure, WriterFailure::Internal(_)), "{failure:?}");
    assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
}

#[test]
fn a_stopped_writer_is_retryable() {
    let Fixture {
        _root,
        data,
        owner,
        service,
    } = Fixture::open(Limits::default());
    owner.shutdown().unwrap();
    let fixture = (data, service);
    let part: Box<dyn Read + Send> = Box::new(&b"solid a\nendsolid a\n"[..]);
    let files: UploadParts = Box::new(std::iter::once(Ok::<_, io::Error>(("a.stl".into(), part))));
    let refused = fixture
        .1
        .upload(Credentials::Session(Secret::new("x".repeat(64))), 1, files);
    let Err(UploadFailure::Storage(failure)) = refused else {
        panic!("expected a storage failure, got {refused:?}")
    };
    assert!(matches!(failure, WriterFailure::Stopped), "{failure:?}");
    assert_eq!(
        writer_failure(&failure).status(),
        StatusCode::SERVICE_UNAVAILABLE
    );
}

#[test]
fn a_busy_database_is_retryable() {
    let fixture = Fixture::open(Limits::default());
    let session = fixture.session();
    // The writer cannot begin its transaction before its 5 s deadline.
    let _lock = fixture.lock_database();
    let (failure, status) = fixture.refused(session);
    assert!(matches!(failure, WriterFailure::Busy), "{failure:?}");
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
}
