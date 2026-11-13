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
    SEED, brief, evidence, resolved, roles, two_bases, write_sources,
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
        (Some(brief(roles(), resolved())), true),
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
            _ => matches!(error, RebuildError::SourceChoicesUnsupported),
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
