use crate::{accepted_plate, loopback};
use accepted_plate::{data_dir, initialize_fixture};
use axum::{
    Router,
    body::to_bytes,
    http::{Request, StatusCode, header},
    response::Response,
};
use pp_api::{
    NetworkAdmission,
    accepted::AcceptedHttpConfig,
    exports::{exports_router, exports_router_with_slicer},
    plates::plates_router,
};
use pp_services::native_slicers::{Artifact, Delivery, Handoff, HandoffFailure, Instance, Picker};
use pp_storage::{
    Limits, WriterOwner, auth,
    read_model::{
        Credential,
        planning::{FilamentInventoryKind, RoleFilamentAssignment},
    },
};
use serde_json::{Value, json};
use std::{
    io::{Cursor, Read},
    path::Path,
    sync::{Arc, Mutex, atomic::AtomicBool},
    time::Duration,
};
use zip::ZipArchive;

const EXPORT: &str = "/api/v2/plans/1/exports/managed-3mf";
const DIRECT: &str = "/api/v2/plans/1/exports/direct-3mf";
const STL: &str = "repos/1/revisions/accepted/included.stl";
const TOKEN: &str = "ppu_574c8d5f92624f029e21705a8ebb8a1b";
const PLATE: &str = "plate_e7bb4d93583fcd4cfcebb2dfc6f8e503";
const COOKIE: Option<&str> = Some("pp_session=read-fixture-secret");

#[path = "managed_spoolman_3mf.rs"]
mod managed_spoolman;

fn open(root: &Path) -> (WriterOwner, Router) {
    let owner = WriterOwner::open(root, Limits::default()).unwrap().0;
    let config = AcceptedHttpConfig::from_startup(loopback::ORIGIN, "default", Some("1")).unwrap();
    let router =
        exports_router(config.clone(), owner.accepted_reads().unwrap()).merge(plates_router(
            config,
            owner
                .accepted_plates(pp_storage::auth::AuthPolicy {
                    registration: pp_storage::auth::RegistrationPolicy::Open,
                    session_tenant: pp_storage::auth::SessionTenantPolicy::AccountTenant,
                    first_user: pp_storage::auth::FirstUserTenant::NewUser,
                })
                .unwrap(),
        ));
    (owner, router)
}

async fn initialize(router: &Router, body: &Value) -> Value {
    let (status, ready) = loopback::send(
        router,
        "POST",
        "/api/v2/plans/1/plates/initialize",
        Some(&body.to_string()),
        COOKIE,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{ready}");
    assert_eq!(ready["plate_revision_id"], 2);
    ready
}

async fn ready_fixture() -> (tempfile::TempDir, WriterOwner, Router, Value) {
    let dir = data_dir();
    let init = initialize_fixture(dir.path());
    let (owner, router) = open(dir.path());
    initialize(&router, &init).await;
    (dir, owner, router, init)
}

async fn export(router: &Router) -> Response {
    let request =
        Request::get(format!("{EXPORT}?plate_revision_id=2")).header("cookie", COOKIE.unwrap());
    loopback::respond(router, request, Vec::new()).await
}

async fn archive_bytes(router: &Router) -> Vec<u8> {
    let response = export(router).await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(response.headers()[header::CONTENT_TYPE], "application/zip");
    assert_eq!(response.headers()[header::CACHE_CONTROL], "no-store");
    to_bytes(response.into_body(), 1024 * 1024)
        .await
        .unwrap()
        .to_vec()
}

fn read_members(bytes: &[u8], names: &[&str]) -> Vec<Vec<u8>> {
    let mut archive = ZipArchive::new(Cursor::new(bytes)).unwrap();
    let mut actual: Vec<_> = archive.file_names().map(Result::unwrap).collect();
    actual.sort_unstable();
    assert_eq!(actual, names);
    names
        .iter()
        .map(|name| {
            let mut bytes = Vec::new();
            archive
                .by_name(name)
                .unwrap()
                .read_to_end(&mut bytes)
                .unwrap();
            bytes
        })
        .collect()
}

#[derive(Default)]
struct FakeSlicer(Mutex<Vec<Artifact>>);

impl Handoff for FakeSlicer {
    fn picker(&self) -> Picker {
        Picker {
            instances: vec![Instance {
                id: "fake".into(),
                name: "Fake".into(),
                available: true,
            }],
            default_instance_id: Some("fake".into()),
        }
    }

    fn deliver(
        &self,
        instance: Option<&str>,
        artifact: Artifact,
    ) -> Result<Delivery, HandoffFailure> {
        assert_eq!(instance, Some("fake"));
        self.0.lock().unwrap().push(artifact);
        Ok(Delivery {
            instance_id: "fake".into(),
            filename: "fixture.3mf".into(),
            already_delivered: false,
        })
    }
}

#[tokio::test]
async fn local_slicer_receives_only_the_requested_verified_3mf() {
    let (dir, owner, exports, _) = ready_fixture().await;
    let fake = Arc::new(FakeSlicer::default());
    let config = AcceptedHttpConfig::from_startup(loopback::ORIGIN, "default", Some("1")).unwrap();
    let routes =
        exports_router_with_slicer(config, owner.accepted_reads().unwrap(), Some(fake.clone()));
    let picker = "/api/v2/slicer-instances";
    let mount = "/api/v2/plans/1/exports/slicer-handoff";
    let (status, body) = loopback::send(&routes, "GET", picker, None, COOKIE).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["default_instance_id"], "fake");
    assert_eq!(body["instances"][0]["available"], true);
    assert_eq!(
        loopback::send(&exports, "GET", picker, None, COOKIE)
            .await
            .0,
        StatusCode::NOT_FOUND
    );
    let network = routes.clone().layer(axum::Extension(NetworkAdmission {
        origin: loopback::ORIGIN.into(),
        host: "127.0.0.1:4100".into(),
    }));
    let request = |revision, ordinal| {
        json!({
            "plate_revision_id": revision, "plate_ordinal": ordinal, "instance_id": "fake"
        })
        .to_string()
    };
    for (router, path, body, cookie, status) in [
        (
            &routes,
            mount,
            request(2, 1),
            None,
            StatusCode::UNAUTHORIZED,
        ),
        (
            &network,
            mount,
            request(2, 1),
            COOKIE,
            StatusCode::FORBIDDEN,
        ),
        (&routes, mount, request(1, 1), COOKIE, StatusCode::CONFLICT),
        (&routes, mount, request(2, 9), COOKIE, StatusCode::NOT_FOUND),
        (
            &routes,
            "/api/v2/plans/999/exports/slicer-handoff",
            request(2, 1),
            COOKIE,
            StatusCode::NOT_FOUND,
        ),
    ] {
        let (actual, response) = loopback::send(router, "POST", path, Some(&body), cookie).await;
        assert_eq!(actual, status, "{response}");
    }
    assert!(fake.0.lock().unwrap().is_empty());
    let archive = archive_bytes(&exports).await;
    let members = read_members(&archive, &["manifest.json", "plates/0001.3mf"]);
    let (status, body) = loopback::send(&routes, "POST", mount, Some(&request(2, 1)), COOKIE).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    std::fs::write(dir.path().join(STL), b"changed after Accept").unwrap();
    assert_eq!(
        loopback::send(&routes, "POST", mount, Some(&request(2, 1)), COOKIE)
            .await
            .0,
        StatusCode::CONFLICT
    );
    let delivered = fake.0.lock().unwrap();
    assert_eq!(delivered.len(), 1);
    assert_eq!(
        (delivered[0].plate_revision_id, delivered[0].plate_ordinal),
        (2, 1)
    );
    assert_eq!(delivered[0].bytes, members[1]);
}

#[tokio::test]
async fn ticket_t_58_three_mf_managed_archive_golden_survives_restart() {
    let (dir, owner, router, init) = ready_fixture().await;
    let root = dir.path();
    let before = archive_bytes(&router).await;
    let members = read_members(&before, &["manifest.json", "plates/0001.3mf"]);
    let manifest: Value = serde_json::from_slice(&members[0]).unwrap();
    assert_eq!(manifest["format"], "accepted-plate-3mf-manifest-v1");
    for (key, value) in init["expected"].as_object().unwrap() {
        assert_eq!(&manifest["basis"][key], value);
    }
    assert_eq!(manifest["basis"]["plate_revision_id"], 2);
    assert_eq!(manifest["basis"]["plate_revision_number"], 2);
    let sql = rusqlite::Connection::open(root.join("print-partner.db")).unwrap();
    let layout: String = sql
        .query_row(
            "SELECT layout_digest FROM accepted_plate_revisions WHERE id=2",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(manifest["basis"]["layout_digest"], layout);
    assert_eq!(
        manifest["plates"],
        json!([{
            "ordinal": 1, "plate_id": PLATE, "entry": "plates/0001.3mf",
            "printer": { "id": "p-a", "name": "Printer A", "model": "Model A" },
            "units": [{
                "token": TOKEN, "object_name": format!("included__{TOKEN}"), "object_id": 1,
                "filament_color_id": null, "filament_hex": null,
                "source_id": 1, "source_revision_id": 1, "relative_path": "included.stl",
                "artifact_sha256": "f4a9ecb98d89be6e025764de8ea6d0b9c9784a670907513d7f5d9e96753ce171"
            }]
        }])
    );
    let model = read_members(
        &members[1],
        &["3D/3dmodel.model", "[Content_Types].xml", "_rels/.rels"],
    );
    assert_eq!(model[0], include_bytes!("managed_plate_3mf.model"));
    assert!(
        std::str::from_utf8(&model[1])
            .unwrap()
            .contains("application/vnd.ms-package.3dmanufacturing-3dmodel+xml")
    );
    assert!(
        std::str::from_utf8(&model[2])
            .unwrap()
            .contains("Target=\"/3D/3dmodel.model\"")
    );
    let jobs: i64 = sql
        .query_row("SELECT COUNT(*) FROM durable_jobs", [], |r| r.get(0))
        .unwrap();
    assert_eq!(jobs, 0);
    assert_eq!(
        sql.execute(
            "UPDATE parts SET filament_custom_hex='#12AB34' WHERE profile_id=1 AND relative_path='included.stl'",
            [],
        )
        .unwrap(),
        1
    );
    drop(sql);
    assert!(
        archive_bytes(&router).await == before,
        "live filament changed the archived revision"
    );

    drop(router);
    owner.shutdown().unwrap();
    let (owner, router) = open(root);
    assert_eq!(archive_bytes(&router).await, before);
    owner.shutdown().unwrap();
}

async fn refuse(router: &Router, path: &str, code: &str, expected: StatusCode) -> Value {
    let (status, body) = loopback::send(router, "GET", path, None, COOKIE).await;
    assert_eq!((status, &body["code"]), (expected, &json!(code)), "{body}");
    body
}

async fn direct_bytes(router: &Router) -> Vec<u8> {
    let request =
        Request::get(format!("{DIRECT}?tokens={TOKEN}")).header("cookie", COOKIE.unwrap());
    let response = loopback::respond(router, request, Vec::new()).await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(response.headers()[header::CONTENT_TYPE], "model/3mf");
    assert_eq!(response.headers()[header::CACHE_CONTROL], "no-store");
    assert!(
        response.headers()[header::CONTENT_DISPOSITION]
            .to_str()
            .unwrap()
            .ends_with("-v1-direct.3mf\"")
    );
    to_bytes(response.into_body(), 1024 * 1024)
        .await
        .unwrap()
        .to_vec()
}

#[tokio::test]
async fn ticket_t_58_three_mf_direct_golden_survives_restart() {
    let dir = data_dir();
    let root = dir.path();
    initialize_fixture(root);
    let sql = rusqlite::Connection::open(root.join("print-partner.db")).unwrap();
    assert_eq!(
        sql.execute(
            "UPDATE parts SET filament_custom_hex='#12AB34' WHERE id=2",
            []
        )
        .unwrap(),
        1
    );
    let (owner, router) = open(root);
    let before = direct_bytes(&router).await;
    let model = read_members(
        &before,
        &["3D/3dmodel.model", "[Content_Types].xml", "_rels/.rels"],
    );
    assert_eq!(model[0], include_bytes!("direct_3mf.model"));
    let jobs: i64 = sql
        .query_row("SELECT COUNT(*) FROM durable_jobs", [], |r| r.get(0))
        .unwrap();
    assert_eq!(jobs, 0);
    assert!(root.join("exports").read_dir().unwrap().next().is_none());
    drop(sql);
    drop(router);
    owner.shutdown().unwrap();
    let (owner, router) = open(root);
    assert_eq!(direct_bytes(&router).await, before);
    owner.shutdown().unwrap();
}

#[tokio::test]
async fn ticket_t_58_three_mf_direct_uses_saved_spoolman_color() {
    let dir = data_dir();
    let root = dir.path();
    initialize_fixture(root);
    let sql = rusqlite::Connection::open(root.join("print-partner.db")).unwrap();
    let brief = json!({
        "version": 1, "build_id": 1, "special_request": "",
        "requirements": [], "evidence": [], "contributions": [],
        "differences": [], "resolutions": {}, "role_filaments": [], "draft_review_blockers": [],
        "created_at": "2026-08-20T12:00:00.000Z", "updated_at": "2026-08-20T12:00:00.000Z",
    });
    sql.execute(
        "INSERT INTO app_settings(tenant_id,key,value) VALUES
            ('default','build_planning.v1.1',?1), ('default','feature_toggles','{\"spoolman\":true}')
         ON CONFLICT(tenant_id,key) DO UPDATE SET value=excluded.value",
        [brief.to_string()],
    ).unwrap();
    let role = sql
        .query_row("SELECT role FROM parts WHERE id=2", [], |row| row.get(0))
        .unwrap();
    let (owner, router) = open(root);
    let saved = owner
        .build_graph_with_policy(auth::AuthPolicy {
            registration: auth::RegistrationPolicy::Open,
            session_tenant: auth::SessionTenantPolicy::AccountTenant,
            first_user: auth::FirstUserTenant::NewUser,
        })
        .unwrap()
        .assign_role_filament(
            Credential::Session(auth::Secret::new("read-fixture-secret".into())),
            pp_storage::build_graph::PositiveId::new(1).unwrap(),
            RoleFilamentAssignment {
                role,
                inventory_kind: FilamentInventoryKind::Spoolman,
                inventory_id: Some("spoolman:local:filament:3".into()),
                color_hex: "#12AB34".into(),
                requested_brand: None,
                requested_name: None,
                substitution_confirmed: false,
            },
            None,
            &AtomicBool::new(false),
            Duration::from_secs(2),
        )
        .unwrap();
    assert!(saved.updated_parts > 0);
    let material: (String, Option<String>) = sql
        .query_row(
            "SELECT filament_color_id,filament_custom_hex FROM parts WHERE id=2",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert_eq!(material, ("spoolman:local:filament:3".into(), None));
    let bytes = direct_bytes(&router).await;
    let model = read_members(
        &bytes,
        &["3D/3dmodel.model", "[Content_Types].xml", "_rels/.rels"],
    );
    assert!(
        std::str::from_utf8(&model[0])
            .unwrap()
            .contains("displaycolor=\"#12AB34\""),
        "assigned Spoolman color missing from the direct 3MF"
    );
    owner.shutdown().unwrap();
}

#[tokio::test]
async fn ticket_t_58_three_mf_direct_refuses_invalid_selection_and_sources() {
    let dir = data_dir();
    initialize_fixture(dir.path());
    let (owner, router) = open(dir.path());
    let path = format!("{DIRECT}?tokens={TOKEN}");
    assert_eq!(
        loopback::send(&router, "GET", &path, None, None).await.0,
        StatusCode::UNAUTHORIZED
    );
    for path in [
        DIRECT.into(),
        format!("{path},{TOKEN}"),
        format!("{path}&tokens={TOKEN}"),
    ] {
        refuse(
            &router,
            &path,
            "invalid_direct_selection",
            StatusCode::BAD_REQUEST,
        )
        .await;
    }
    let excluded = "ppu_827108f7baf3a2f3461d20c24e85822e";
    let body = refuse(
        &router,
        &format!("{path},{excluded}"),
        "invalid_direct_selection",
        StatusCode::CONFLICT,
    )
    .await;
    assert_eq!(body["token"], excluded);
    let many = (0..257)
        .map(|i| format!("ppu_{i:032x}"))
        .collect::<Vec<_>>()
        .join(",");
    let body = refuse(
        &router,
        &format!("{DIRECT}?tokens={many}"),
        "three_mf_too_large",
        StatusCode::UNPROCESSABLE_ENTITY,
    )
    .await;
    assert_eq!(body["limit"], "objects");
    std::fs::write(dir.path().join(STL), b"changed").unwrap();
    let (status, body) = loopback::send(&router, "GET", &path, None, COOKIE).await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["code"], "source_files_unavailable");
    assert_eq!(
        (&body["reason"], &body["token"]),
        (&json!("digest_mismatch"), &json!(TOKEN))
    );
    owner.shutdown().unwrap();
}

#[tokio::test]
async fn ticket_t_58_three_mf_requires_auth_and_current_placed_revision() {
    let dir = data_dir();
    let init = initialize_fixture(dir.path());
    let (owner, router) = open(dir.path());
    let path = format!("{EXPORT}?plate_revision_id=2");
    for cookie in [None, Some("pp_session=invalid")] {
        assert_eq!(
            loopback::send(&router, "GET", &path, None, cookie).await.0,
            StatusCode::UNAUTHORIZED
        );
    }
    refuse(
        &router,
        EXPORT,
        "plate_revision_required",
        StatusCode::BAD_REQUEST,
    )
    .await;
    refuse(&router, &path, "plates_not_published", StatusCode::CONFLICT).await;
    initialize(&router, &init).await;
    refuse(
        &router,
        &format!("{EXPORT}?plate_revision_id=1"),
        "plate_revision_changed",
        StatusCode::CONFLICT,
    )
    .await;
    let (status, revised) = loopback::send(
        &router,
        "POST",
        &format!("/api/v2/plans/1/plates/{PLATE}/units/{TOKEN}/unplace"),
        Some(&json!({"expected": init["expected"], "expected_plate_revision_id": 2}).to_string()),
        COOKIE,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{revised}");
    refuse(
        &router,
        &format!("{EXPORT}?plate_revision_id=3"),
        "unplaced_units",
        StatusCode::CONFLICT,
    )
    .await;
    owner.shutdown().unwrap();
}

#[tokio::test]
async fn ticket_t_58_three_mf_refuses_changed_and_oversized_sources_without_an_archive() {
    let (dir, owner, router, _) = ready_fixture().await;
    let source = dir.path().join(STL);
    let path = format!("{EXPORT}?plate_revision_id=2");
    std::fs::write(&source, b"changed").unwrap();
    let body = refuse(
        &router,
        &path,
        "source_files_unavailable",
        StatusCode::CONFLICT,
    )
    .await;
    assert_eq!(body["reason"], "digest_mismatch");
    assert_eq!(body["token"], TOKEN);
    std::fs::File::create(source)
        .unwrap()
        .set_len(64 * 1024 * 1024 + 1)
        .unwrap();
    let body = refuse(
        &router,
        &path,
        "three_mf_too_large",
        StatusCode::UNPROCESSABLE_ENTITY,
    )
    .await;
    assert_eq!(body["limit"], "source_bytes");
    owner.shutdown().unwrap();
}
