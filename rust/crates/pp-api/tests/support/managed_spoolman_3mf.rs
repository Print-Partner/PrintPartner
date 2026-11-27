use super::{COOKIE, EXPORT, archive_bytes, data_dir, initialize_fixture, open, read_members};
use crate::loopback;
use axum::{
    body::to_bytes,
    http::{Request, StatusCode},
};
use pp_storage::{
    WriterOwner, auth,
    plan_publication::{Outcome, PublicationCommand},
    read_model::{
        Credential,
        planning::{FilamentInventoryKind, RoleFilamentAssignment},
    },
    required_units,
    working_drafts::{
        PositiveId, RecomputeRequest, RecomputeWorkspaceOutcome, observation::PreparationLimits,
    },
};
use serde_json::{Value, json};
use std::{
    sync::{Arc, atomic::AtomicBool},
    time::Duration,
};

const POLICY: auth::AuthPolicy = auth::AuthPolicy {
    registration: auth::RegistrationPolicy::Open,
    session_tenant: auth::SessionTenantPolicy::AccountTenant,
    first_user: auth::FirstUserTenant::NewUser,
};
const WAIT: Duration = Duration::from_secs(5);
const COLOR: &str = "#12AB34";

fn credential() -> Credential {
    Credential::Session(auth::Secret::new("read-fixture-secret".into()))
}

fn assign(owner: &WriterOwner, hex: &str) {
    let saved = owner
        .build_graph_with_policy(POLICY)
        .unwrap()
        .assign_role_filament(
            credential(),
            PositiveId::new(1).unwrap(),
            RoleFilamentAssignment {
                role: "primary".into(),
                inventory_kind: FilamentInventoryKind::Spoolman,
                inventory_id: Some("spoolman:local:filament:3".into()),
                color_hex: hex.into(),
                requested_brand: None,
                requested_name: None,
                substitution_confirmed: false,
            },
            None,
            &AtomicBool::new(false),
            WAIT,
        )
        .unwrap();
    assert_eq!(saved.updated_parts, 1);
}

#[tokio::test]
async fn ticket_t_58_three_mf_managed_preserves_accepted_spoolman_color() {
    let dir = data_dir();
    let root = dir.path();
    let init = initialize_fixture(root);
    std::fs::write(
        root.join("repos/1/revisions/accepted/excluded.stl"),
        "solid excluded",
    )
    .unwrap();
    let sql = rusqlite::Connection::open(root.join("print-partner.db")).unwrap();
    let brief = json!({
        "version": 1, "build_id": 1, "special_request": "",
        "requirements": [], "evidence": [], "contributions": [],
        "differences": [], "resolutions": {}, "role_filaments": [], "draft_review_blockers": [],
        "created_at": "2026-08-20T12:00:00.000Z", "updated_at": "2026-08-20T12:00:00.000Z",
    });
    sql.execute(
        "INSERT INTO app_settings(tenant_id,key,value) VALUES
            ('default','build_planning.v1.1',?1), ('default','feature_toggles','{\"spoolman\":true}')",
        [brief.to_string()],
    ).unwrap();
    sql.execute(
        "DELETE FROM app_settings WHERE tenant_id='default'
         AND key IN ('printer.checkoff_links', 'printer.send_queue')",
        [],
    )
    .unwrap();
    let (owner, router) = open(root);
    super::initialize(&router, &init).await;
    let before = archive_bytes(&router).await;
    assign(&owner, COLOR);
    assert_eq!(
        archive_bytes(&router).await,
        before,
        "the older revision acquired a live color"
    );

    let profile = PositiveId::new(1).unwrap();
    let drafts = owner
        .working_drafts_with_observations(
            POLICY,
            Arc::new(owner.source_reads().unwrap()),
            PreparationLimits::default(),
        )
        .unwrap();
    let recomputed = drafts
        .recompute_and_select(
            credential(),
            profile,
            RecomputeRequest::new("spoolman-draft", false).unwrap(),
            Arc::new(AtomicBool::new(false)),
            WAIT,
        )
        .unwrap();
    let RecomputeWorkspaceOutcome::Selected(required_units::Outcome::Ready { workspace }) =
        recomputed
    else {
        panic!("{recomputed:?}")
    };
    let draft = workspace.draft;
    let command = PublicationCommand::new(
        serde_json::from_value(json!(profile.get())).unwrap(),
        serde_json::from_value(json!(draft.draft_id)).unwrap(),
        serde_json::from_value(json!({
            "expected_snapshot_digest": draft.snapshot_digest,
            "expected_lifecycle_version": draft.lifecycle_version,
            "expected_base": draft.base,
        }))
        .unwrap(),
        "spoolman-accept".into(),
        credential(),
    )
    .unwrap();
    let outcome = owner
        .publication_with_policy(POLICY)
        .unwrap()
        .apply(command, &AtomicBool::new(false), WAIT)
        .unwrap();
    assert!(matches!(outcome, Outcome::Applied { .. }), "{outcome:?}");
    let plate_revision: i64 = sql
        .query_row(
            "SELECT current_revision_id FROM accepted_plate_heads WHERE profile_id=1",
            [],
            |row| row.get(0),
        )
        .unwrap();
    let get = |router| async move {
        let request = Request::get(format!("{EXPORT}?plate_revision_id={plate_revision}"))
            .header("cookie", COOKIE.unwrap());
        let response = loopback::respond(router, request, Vec::new()).await;
        assert_eq!(response.status(), StatusCode::OK);
        to_bytes(response.into_body(), 1024 * 1024).await.unwrap()
    };
    let accepted = get(&router).await;
    let members = read_members(&accepted, &["manifest.json", "plates/0001.3mf"]);
    let model = read_members(
        &members[1],
        &["3D/3dmodel.model", "[Content_Types].xml", "_rels/.rels"],
    );
    assert!(
        std::str::from_utf8(&model[0])
            .unwrap()
            .contains("displaycolor=\"#12AB34\""),
        "accepted Spoolman color missing from the managed 3MF"
    );
    let manifest: Value = serde_json::from_slice(&members[0]).unwrap();
    assert_eq!(
        manifest["plates"][0]["units"][0]["filament_color_id"],
        "spoolman:local:filament:3"
    );
    assert_eq!(manifest["plates"][0]["units"][0]["filament_hex"], COLOR);
    assign(&owner, "#AB3412");
    assert_eq!(get(&router).await, accepted);
    drop(sql);
    owner.shutdown().unwrap();
    let (owner, restarted) = open(root);
    assert_eq!(get(&restarted).await, accepted);
    owner.shutdown().unwrap();
}
