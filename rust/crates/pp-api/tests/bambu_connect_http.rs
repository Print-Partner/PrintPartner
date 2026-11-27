#![cfg(feature = "desktop_rebuild")]

#[path = "support/loopback.rs"]
mod loopback;
#[path = "support/multipart.rs"]
mod multipart;

use axum::{Extension, Router, http::StatusCode};
use pp_api::{
    NetworkAdmission,
    accepted::AcceptedHttpConfig,
    exports::bambu_connect::{PrintFile, Stage, StageFailure, bambu_connect_router},
};
use pp_storage::{Limits, WriterOwner};
use serde_json::Value;
use std::{
    path::PathBuf,
    sync::{Arc, Mutex},
};

const HANDOFF: &str = "/api/v2/plans/1/bambu-connect-handoff";
const COOKIE: &str = "pp_session=required-unit-fixture-secret";
const DATABASE: &[u8] =
    include_bytes!("../../pp-storage/tests/fixtures/required-units/unchanged.db");

struct Files {
    path: PathBuf,
    seen: Mutex<Vec<PrintFile>>,
}

impl Stage for Files {
    fn stage(&self, print: PrintFile) -> Result<PathBuf, StageFailure> {
        self.seen.lock().unwrap().push(print);
        Ok(self.path.clone())
    }
}

struct Fixture {
    _owner: WriterOwner,
    database: rusqlite::Connection,
    router: Router,
    files: Arc<Files>,
    _temp: tempfile::TempDir,
}

impl Fixture {
    fn new(flag: Option<&str>) -> Self {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("print-partner.db");
        std::fs::write(&db, DATABASE).unwrap();
        let owner = WriterOwner::open(dir.path(), Limits::default()).unwrap().0;
        let policy = pp_storage::auth::AuthPolicy {
            registration: pp_storage::auth::RegistrationPolicy::FirstAccountOnly,
            first_user: pp_storage::auth::FirstUserTenant::NewUser,
            session_tenant: pp_storage::auth::SessionTenantPolicy::SingleAccountDefault,
        };
        let files = Arc::new(Files {
            path: dir.path().join("fake path &#.gcode.3mf"),
            seen: Mutex::new(Vec::new()),
        });
        let router = bambu_connect_router(
            AcceptedHttpConfig::from_startup(loopback::ORIGIN, "default", flag).unwrap(),
            owner.accepted_reads_with_policy(policy).unwrap(),
            files.clone(),
        );
        Self {
            _owner: owner,
            database: rusqlite::Connection::open(db).unwrap(),
            router,
            files,
            _temp: dir,
        }
    }

    fn enable_for_build(&self) {
        self.database
            .execute(
                "INSERT OR REPLACE INTO app_settings(tenant_id,key,value)
             VALUES('default','feature_toggles:plan:1','{\"bambu\":true}')",
                [],
            )
            .unwrap();
    }
}

async fn upload(
    router: &Router,
    path: &str,
    cookie: Option<&str>,
    parts: &[(&str, Option<&str>, &[u8])],
    headers: &[(&str, &str)],
) -> (StatusCode, Value) {
    let (content_type, body) = multipart::multipart(parts);
    if headers.is_empty() {
        return loopback::send_as(router, "POST", path, Some(&content_type), body, cookie).await;
    }
    loopback::send_with_headers(
        router,
        "POST",
        path,
        Some(&content_type),
        body,
        cookie,
        headers,
    )
    .await
}

#[tokio::test]
async fn stages_only_an_owned_build_file_and_returns_an_import_link_without_writing_the_database() {
    let fixture = Fixture::new(Some("1"));
    let file = [(
        "file",
        Some("../Kit A & B#.GCODE.3MF"),
        b"fake sliced file".as_slice(),
    )];
    assert_eq!(
        upload(&fixture.router, HANDOFF, Some(COOKIE), &file, &[])
            .await
            .0,
        StatusCode::FORBIDDEN
    );
    fixture.enable_for_build();
    let data_version = || {
        fixture
            .database
            .query_row("PRAGMA data_version", [], |row| row.get::<_, i64>(0))
            .unwrap()
    };
    let before = data_version();
    let (status, body) = upload(&fixture.router, HANDOFF, Some(COOKIE), &file, &[]).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["filename"], "Kit A & B#.gcode.3mf");
    let url = reqwest::Url::parse(body["connect_url"].as_str().unwrap()).unwrap();
    assert_eq!(url.scheme(), "bambu-connect");
    assert_eq!(url.host_str(), Some("import-file"));
    assert!(url.path().is_empty());
    let pairs: Vec<_> = url.query_pairs().into_owned().collect();
    assert_eq!(
        pairs,
        [
            ("path".into(), fixture.files.path.to_str().unwrap().into()),
            ("name".into(), "Kit A & B#".into()),
            ("version".into(), "1.0.0".into()),
        ]
    );
    assert_eq!(body.as_object().unwrap().len(), 2);
    let staged = fixture.files.seen.lock().unwrap();
    assert_eq!(staged.len(), 1);
    assert_eq!(staged[0].build().get(), 1);
    assert_eq!(staged[0].extension(), ".gcode.3mf");
    assert_eq!(staged[0].bytes(), file[0].2);
    assert_eq!(data_version(), before);
}

#[tokio::test]
async fn rejects_remote_api_keys_missing_builds_and_malformed_uploads_before_staging() {
    let fixture = Fixture::new(Some("1"));
    fixture.enable_for_build();
    let remote = fixture.router.clone().layer(Extension(NetworkAdmission {
        origin: loopback::ORIGIN.into(),
        host: "127.0.0.1:4100".into(),
    }));
    let disabled = Fixture::new(None);
    assert_eq!(
        loopback::send(&fixture.router, "POST", HANDOFF, Some("{}"), Some(COOKIE))
            .await
            .0,
        StatusCode::UNSUPPORTED_MEDIA_TYPE
    );
    let valid = ("file", Some("file.gco"), b"test".as_slice());
    let file = [valid];
    for (router, path, cookie, headers, expected) in [
        (
            &fixture.router,
            HANDOFF,
            None,
            [].as_slice(),
            StatusCode::UNAUTHORIZED,
        ),
        (
            &fixture.router,
            HANDOFF,
            Some("pp_session=wrong"),
            &[],
            StatusCode::UNAUTHORIZED,
        ),
        (
            &fixture.router,
            HANDOFF,
            None,
            &[("authorization", "Bearer no")],
            StatusCode::FORBIDDEN,
        ),
        (
            &fixture.router,
            "/api/v2/plans/999/bambu-connect-handoff",
            Some(COOKIE),
            &[],
            StatusCode::NOT_FOUND,
        ),
        (&remote, HANDOFF, Some(COOKIE), &[], StatusCode::FORBIDDEN),
        (
            &disabled.router,
            HANDOFF,
            Some(COOKIE),
            &[],
            StatusCode::NOT_FOUND,
        ),
    ] {
        let (status, problem) = upload(router, path, cookie, &file, headers).await;
        assert_eq!(status, expected, "{problem}");
    }
    for parts in [
        [].as_slice(),
        &[valid, valid],
        &[("file", Some("file.stl"), b"test")],
        &[("file", Some("file.gcode"), b"")],
        &[("printer_id", None, b"printer"), valid],
    ] {
        let (status, problem) = upload(&fixture.router, HANDOFF, Some(COOKIE), parts, &[]).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{problem}");
    }
    assert!(fixture.files.seen.lock().unwrap().is_empty());
}

#[tokio::test]
async fn refuses_a_file_over_the_upload_limit_before_staging() {
    use pp_storage::checkoff_progress::print_file::MAX_UPLOAD_BYTES;
    let fixture = Fixture::new(Some("1"));
    fixture.enable_for_build();
    let bytes = vec![b'x'; MAX_UPLOAD_BYTES + 1];
    let file = [("file", Some("file.gcode"), bytes.as_slice())];
    let (status, problem) = upload(&fixture.router, HANDOFF, Some(COOKIE), &file, &[]).await;
    assert_eq!(status, StatusCode::PAYLOAD_TOO_LARGE, "{problem}");
    assert!(fixture.files.seen.lock().unwrap().is_empty());
}
