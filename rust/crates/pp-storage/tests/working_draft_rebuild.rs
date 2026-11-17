#![cfg(feature = "desktop_rebuild")]
//! `WorkingDraftClient::rebuild_from_brief`: one transaction sets the layers
//! from the brief's Source roles, prepares and stores a draft, and selects it
//! in the brief. Build 3 has no layers, no draft and no Accepted Plan;
//! Sources 4 (`Base`) and 5 (`Addon`) are synced local Sources.
use pp_storage::{
    Limits, WriterOwner,
    auth::{AuthPolicy, FirstUserTenant, RegistrationPolicy, Secret, SessionTenantPolicy},
    read_model::{Credential, planning::SourceRole},
    working_drafts::{
        PositiveId,
        observation::PreparationLimits,
        rebuild::{RebuildError, RebuildRequest, RebuildSource, Rebuilt},
    },
};
use pp_testkit::rebuild_fixture::{
    SEED, brief, evidence, roles, scoped_choices, two_bases, unresolved_choices, write_sources,
    write_stl,
};
use rusqlite::{Connection, OptionalExtension};
use serde_json::{Value, json};
use std::{
    path::Path,
    sync::{Arc, atomic::AtomicBool},
    time::Duration,
};

/// Build 3's layers and drafts as `kind:id` lists, and its stored brief text.
type State = (String, String, String);

struct Fixture {
    root: tempfile::TempDir,
    owner: WriterOwner,
}

impl Fixture {
    /// Seeds Build 3 with `brief` (none when `None`) and, when `files`, gives
    /// both Sources printable files.
    fn new(brief: Option<Value>, files: bool) -> Self {
        let root = tempfile::tempdir().unwrap();
        let database = root.path().join("print-partner.db");
        let fixture = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/required-units/unchanged.db");
        std::fs::copy(fixture, &database).unwrap();
        write_sources(root.path(), files).unwrap();
        let db = Connection::open(&database).unwrap();
        db.execute_batch(&SEED.replace("<data-dir>", root.path().to_str().unwrap()))
            .unwrap();
        if let Some(brief) = brief {
            db.execute(
                "INSERT INTO app_settings(tenant_id,key,value) VALUES('default','build_planning.v1.3',?1)",
                [brief.to_string()],
            )
            .unwrap();
        }
        drop(db);
        let (owner, _) = WriterOwner::open(root.path(), Limits::default()).unwrap();
        Self { root, owner }
    }

    fn rebuild(&self, key: &str) -> Result<Rebuilt, RebuildError> {
        let policy = AuthPolicy {
            registration: RegistrationPolicy::Open,
            session_tenant: SessionTenantPolicy::AccountTenant,
            first_user: FirstUserTenant::NewUser,
        };
        let reads = Arc::new(self.owner.source_reads().unwrap());
        self.owner
            .working_drafts_with_observations(policy, reads, PreparationLimits::default())
            .unwrap()
            .rebuild_from_brief(
                Credential::Session(Secret::new("required-unit-fixture-secret".into())),
                PositiveId::new(3).unwrap(),
                RebuildRequest::new(key).unwrap(),
                &AtomicBool::new(false),
                Duration::from_secs(30),
            )
    }

    fn state(&self) -> State {
        let db = Connection::open(self.root.path().join("print-partner.db")).unwrap();
        let text = |sql: &str| -> String {
            db.query_row(sql, [], |r| r.get::<_, Option<String>>(0))
                .optional()
                .unwrap()
                .flatten()
                .unwrap_or_default()
        };
        (
            text(
                "SELECT group_concat(layer_type||':'||project_id, ',') FROM (SELECT * FROM profile_layers WHERE profile_id=3 ORDER BY layer_order,id)",
            ),
            text(
                "SELECT group_concat(id||':'||state, ',') FROM (SELECT * FROM plan_drafts WHERE profile_id=3 ORDER BY id)",
            ),
            text("SELECT value FROM app_settings WHERE key='build_planning.v1.3'"),
        )
    }
}

#[test]
fn rebuild_sets_layers_from_roles_and_selects_a_new_draft() {
    let f = Fixture::new(Some(brief(roles(), json!({}))), true);
    let rebuilt = f.rebuild("k").unwrap();
    assert!(rebuilt.created);
    assert_eq!(rebuilt.snapshot_digest.len(), 64);
    assert_eq!(
        rebuilt.sources,
        [
            RebuildSource {
                source_id: 4,
                role: SourceRole::StructuralBase,
                revision: Some("rev4".into())
            },
            RebuildSource {
                source_id: 5,
                role: SourceRole::Addon,
                revision: Some("rev5".into())
            },
        ]
    );
    let (layers, drafts, brief) = f.state();
    assert_eq!(layers, "base:4,addon:5");
    let id = rebuilt.draft_id.get();
    assert_eq!(drafts, format!("{id}:open"));
    let brief: Value = serde_json::from_str(&brief).unwrap();
    assert_eq!(brief["draft_id"], id);
    assert_eq!(brief["managed_source_ids"], json!([4, 5]));
    assert_eq!(
        brief["draft_source_revisions"],
        json!({"4":"rev4","5":"rev5"})
    );

    // The same key and inputs answer the draft the first rebuild made.
    let again = f.rebuild("k").unwrap();
    assert!(!again.created);
    assert_eq!(again.draft_id, rebuilt.draft_id);
    assert_eq!(f.state().1, format!("{id}:open"));

    // Another rebuild abandons that draft; replaying the first key must not
    // select the abandoned draft again.
    let later = f.rebuild("k2").unwrap();
    assert!(later.created);
    let before = f.state();
    assert!(matches!(
        f.rebuild("k"),
        Err(RebuildError::IdempotencyConflict)
    ));
    assert_eq!(f.state(), before);
    let brief: Value = serde_json::from_str(&before.2).unwrap();
    assert_eq!(brief["draft_id"], later.draft_id.get());
}

#[test]
fn rebuild_refusals_leave_layers_drafts_and_brief_unchanged() {
    let cases = [
        (None, true),
        (
            Some(brief(json!([evidence("addon", 5, "addon")]), json!({}))),
            true,
        ),
        (Some(brief(two_bases(), json!({}))), true),
        (Some(brief(roles(), json!({}))), false),
        (Some(unresolved_choices()), true),
    ];
    for (index, (brief, files)) in cases.into_iter().enumerate() {
        let f = Fixture::new(brief, files);
        let before = f.state();
        let error = f.rebuild("k").unwrap_err();
        let expected = match index {
            0 => matches!(error, RebuildError::NoBrief),
            1 => matches!(error, RebuildError::NoStructuralBase),
            2 => matches!(error, RebuildError::MultipleStructuralBases),
            3 => matches!(error, RebuildError::NoStls),
            _ => matches!(
                error,
                RebuildError::UnresolvedSourceChoices { ref groups } if groups == &["group-1".to_owned()]
            ),
        };
        assert!(expected, "case {index}: {error:?}");
        assert_eq!(f.state(), before, "case {index} wrote");
    }
}

#[test]
fn rebuild_keys_must_be_non_blank_and_leave_room_for_the_inputs_suffix() {
    assert!(RebuildRequest::new("  k  ").is_ok());
    assert!(matches!(
        RebuildRequest::new(" "),
        Err(RebuildError::InvalidIdempotencyKey)
    ));
    assert!(RebuildRequest::new(&"k".repeat(143)).is_ok());
    assert!(RebuildRequest::new(&"k".repeat(144)).is_err());
}

#[test]
fn rebuild_applies_a_scoped_contribution_and_drops_the_superseded_file() {
    let f = Fixture::new(Some(scoped_choices()), true);
    write_stl(f.root.path(), 4, "superseded.stl").unwrap();
    write_stl(f.root.path(), 5, "mods/a/plate.stl").unwrap();
    write_stl(f.root.path(), 5, "mods/b/other.stl").unwrap();
    let rebuilt = f.rebuild("k").unwrap();
    assert!(rebuilt.created);
    let db = Connection::open(f.root.path().join("print-partner.db")).unwrap();
    let mut query = db
        .prepare(
            "SELECT source_layer, relative_path FROM plan_draft_parts WHERE draft_id=?1 ORDER BY source_layer, relative_path",
        )
        .unwrap();
    let parts: Vec<(String, String)> = query
        .query_map([i64::try_from(rebuilt.draft_id.get()).unwrap()], |row| {
            Ok((row.get(0)?, row.get(1)?))
        })
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    assert_eq!(
        parts,
        [
            ("addon:Addon".into(), "mods/a/plate.stl".into()),
            ("base:Base".into(), "bracket.stl".into()),
            ("base:Base".into(), "gear.stl".into()),
        ]
    );
    let again = f.rebuild("k").unwrap();
    assert!(!again.created);
    assert_eq!(again.draft_id, rebuilt.draft_id);
}

#[test]
fn rebuild_removes_a_managed_layer_whose_role_became_evidence_and_keeps_a_ui_layer() {
    let f = Fixture::new(Some(brief(roles(), json!({}))), true);
    f.rebuild("k").unwrap();
    let root = f.root.path().to_str().unwrap().replace('\'', "''");
    write_stl(f.root.path(), 6, "ui.stl").unwrap();
    let db = Connection::open(f.root.path().join("print-partner.db")).unwrap();
    db.execute_batch(&format!(
        "INSERT INTO projects(id,tenant_id,name,url,source_type,branch,local_path,last_synced_at,last_commit_sha) VALUES(6,'default','Extra','','local','main','{root}/repos/6','2026-11-01T00:00:00.000Z','rev6');
         INSERT INTO profile_layers(tenant_id,profile_id,layer_order,layer_type,project_id) VALUES('default',3,2,'addon',6);"
    ))
    .unwrap();
    let mut stored: Value = serde_json::from_str(&f.state().2).unwrap();
    stored["evidence"]
        .as_array_mut()
        .unwrap()
        .iter_mut()
        .find(|item| item["id"] == "addon")
        .unwrap()["source_role"] = json!("evidence");
    db.execute(
        "UPDATE app_settings SET value=?1 WHERE key='build_planning.v1.3'",
        [stored.to_string()],
    )
    .unwrap();
    drop(db);
    let before_layers = f.state().0;
    assert_eq!(before_layers, "base:4,addon:5,addon:6");
    let rebuilt = f.rebuild("later").unwrap();
    assert!(rebuilt.created);
    assert_eq!(
        rebuilt
            .sources
            .iter()
            .map(|s| s.source_id)
            .collect::<Vec<_>>(),
        [4]
    );
    let (layers, _, brief) = f.state();
    assert_eq!(layers, "base:4,addon:6");
    let brief: Value = serde_json::from_str(&brief).unwrap();
    assert_eq!(brief["managed_source_ids"], json!([4]));
}
