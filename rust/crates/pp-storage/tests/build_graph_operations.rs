#![cfg(feature = "desktop_rebuild")]
use pp_domain::PositiveId;
use pp_storage::{
    Limits, WriterOwner,
    auth::{self, Secret},
    build_graph::{
        AcceptedProgress, AcceptedProgressUnavailable, BuildCommand, BuildGraphClient,
        BuildOutcome, Failure, PlanFreshness, PlanStaleReason, ProfileSummary,
    },
    read_model::Credential,
};
use rusqlite::Connection;
use std::{
    os::unix::fs::DirBuilderExt,
    path::PathBuf,
    sync::{Arc, atomic::AtomicBool},
    time::Duration,
};
fn id(value: u64) -> PositiveId {
    PositiveId::new(value).unwrap()
}
fn credential() -> Credential {
    Credential::Session(Secret::new("read-fixture-secret".into()))
}
fn policy() -> auth::AuthPolicy {
    auth::AuthPolicy {
        registration: auth::RegistrationPolicy::Open,
        first_user: auth::FirstUserTenant::NewUser,
        session_tenant: auth::SessionTenantPolicy::AccountTenant,
    }
}
struct Fixture {
    root: PathBuf,
    owner: Option<WriterOwner>,
    client: BuildGraphClient,
}
impl Fixture {
    fn new() -> Self {
        Self::from_bytes(include_bytes!("fixtures/accepted-plan-node.db"))
    }
    fn from_bytes(database: &[u8]) -> Self {
        let root = std::env::temp_dir().join(format!(
            "pp-plan-read-{}-{}",
            std::process::id(),
            rand::random::<u64>()
        ));
        std::fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(root.join("repos/1/revisions/accepted"))
            .unwrap();
        std::fs::write(root.join("print-partner.db"), database).unwrap();
        let (owner, _) = WriterOwner::open(
            &root,
            Limits {
                queued_writes: 2,
                readers: 1,
            },
        )
        .unwrap();
        let client = owner.build_graph_with_policy(policy()).unwrap();
        Self {
            root,
            owner: Some(owner),
            client,
        }
    }
    fn read(&self) -> ProfileSummary {
        match self.run(BuildCommand::Read { build: id(1) }).unwrap() {
            BuildOutcome::Read { profile } => *profile,
            outcome => panic!("{outcome:?}"),
        }
    }
    fn run(&self, command: BuildCommand) -> anyhow::Result<BuildOutcome> {
        self.client.execute(
            credential(),
            command,
            Arc::new(AtomicBool::new(false)),
            Duration::from_secs(2),
        )
    }
    fn raw(&self) -> Connection {
        let c = Connection::open(self.root.join("print-partner.db")).unwrap();
        c.busy_timeout(Duration::from_secs(3)).unwrap();
        c
    }
    fn mutate(&self, sql: &str) {
        let c = self.raw();
        c.pragma_update(None, "foreign_keys", false).unwrap();
        let triggers = c
            .prepare("SELECT name FROM sqlite_master WHERE type='trigger'")
            .unwrap()
            .query_map([], |r| r.get::<_, String>(0))
            .unwrap()
            .collect::<rusqlite::Result<Vec<_>>>()
            .unwrap();
        for t in triggers {
            c.execute_batch(&format!("DROP TRIGGER \"{t}\";")).unwrap();
        }
        c.execute_batch(sql)
            .unwrap_or_else(|e| panic!("{sql}: {e}"));
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        if let Some(owner) = self.owner.take() {
            owner.shutdown().unwrap();
        }
        std::fs::remove_dir_all(&self.root).unwrap();
    }
}

#[test]
fn accepted_summary_counts_units_and_preserves_freshness() {
    let f = Fixture::new();
    let p = f.read();
    assert_eq!(p.id, 1);
    assert_eq!(p.part_count, 2);
    assert!(matches!(
        p.accepted_progress,
        AcceptedProgress::Ready {
            total_units: 1,
            remaining_units: 1
        }
    ));
    assert!(matches!(p.freshness, PlanFreshness::Current { .. }));
    assert!(!p.build_stale);
    f.mutate("UPDATE build_profiles SET config_modified_at='2099-01-01T00:00:00.000Z' WHERE id=1;");
    let p = f.read();
    assert!(p.build_stale);
    assert!(
        matches!(p.freshness, PlanFreshness::Stale { reasons, .. } if reasons.iter().any(|r| matches!(r, PlanStaleReason::PlanConfigurationChanged)))
    );
}
#[test]
fn tenant_isolation_missing_and_invalid_credentials() {
    let f = Fixture::new();
    f.mutate("INSERT INTO build_profiles(id,tenant_id,name) VALUES(50,'other','Hidden');");
    for build in [50, 999] {
        assert!(matches!(
            f.run(BuildCommand::Read { build: id(build) }).unwrap(),
            BuildOutcome::MissingBuild
        ));
        assert!(matches!(
            f.run(BuildCommand::ListLayers { build: id(build) })
                .unwrap(),
            BuildOutcome::MissingBuild
        ));
    }
    assert!(
        f.client
            .execute(
                Credential::Session(Secret::new("invalid".into())),
                BuildCommand::List,
                Arc::new(AtomicBool::new(false)),
                Duration::ZERO
            )
            .is_err()
    );
}
#[test]
fn list_and_layers_have_stable_order_and_trim_requests() {
    let f = Fixture::new();
    f.mutate("UPDATE build_profiles SET name='Zulu'; INSERT INTO build_profiles(id,tenant_id,name,special_request) SELECT 20,tenant_id,'Alpha','  please  ' FROM build_profiles WHERE id=1; INSERT INTO build_profiles(id,tenant_id,name) SELECT 21,tenant_id,'Beta' FROM build_profiles WHERE id=1;");
    let BuildOutcome::Listed { profiles } = f.run(BuildCommand::List).unwrap() else {
        panic!("list")
    };
    assert_eq!(
        profiles
            .iter()
            .map(|p| (p.id, p.name.as_str()))
            .collect::<Vec<_>>(),
        vec![(20, "Alpha"), (21, "Beta"), (1, "Zulu")]
    );
    assert_eq!(profiles[0].special_request.as_deref(), Some("please"));
    assert!(matches!(
        profiles[0].accepted_progress,
        AcceptedProgress::Empty
    ));
    f.mutate("INSERT INTO profile_layers(id,tenant_id,profile_id,layer_order,layer_type,project_id) VALUES(10,'default',1,2,'addon',NULL),(9,'default',1,1,'addon',NULL),(8,'default',1,1,'addon',NULL);");
    let BuildOutcome::Layers { profile_id, layers } =
        f.run(BuildCommand::ListLayers { build: id(1) }).unwrap()
    else {
        panic!("layers")
    };
    assert_eq!(profile_id, 1);
    assert_eq!(
        layers.iter().map(|l| l.id).collect::<Vec<_>>(),
        vec![1, 8, 9, 10]
    );
    assert_eq!(
        layers
            .iter()
            .map(|l| (l.layer_order, l.project_id))
            .collect::<Vec<_>>(),
        vec![(0, Some(1)), (1, None), (1, None), (2, None)]
    );
}
#[test]
fn unavailable_states_are_honest() {
    for (sql, expected) in [
        (
            "UPDATE build_profiles SET accepted_plan_revision_id=NULL;",
            AcceptedProgressUnavailable::CompatibilityDirty,
        ),
        (
            "DELETE FROM plan_revision_required_unit_sets; DELETE FROM plan_revision_required_units; DELETE FROM required_units;",
            AcceptedProgressUnavailable::Uninitialized,
        ),
        (
            "UPDATE plan_revisions SET snapshot_digest='broken';",
            AcceptedProgressUnavailable::Integrity,
        ),
    ] {
        let f = Fixture::new();
        f.mutate(sql);
        let p = f.read();
        assert!(
            matches!(p.accepted_progress, AcceptedProgress::Unavailable(reason) if std::mem::discriminant(&reason) == std::mem::discriminant(&expected))
        );
    }
}
#[test]
fn cancelled_and_stopped_requests_are_typed_failures() {
    let mut f = Fixture::new();
    let err = f
        .client
        .execute(
            credential(),
            BuildCommand::List,
            Arc::new(AtomicBool::new(true)),
            Duration::ZERO,
        )
        .unwrap_err();
    assert!(matches!(err.downcast_ref(), Some(Failure::Cancelled)));
    f.owner.take().unwrap().shutdown().unwrap();
    let err = f.run(BuildCommand::List).unwrap_err();
    assert!(matches!(err.downcast_ref(), Some(Failure::Stopped)));
}

#[test]
fn ppjs_metadata_is_lossless_and_projection_mismatch_is_integrity() {
    let f = Fixture::new();
    f.mutate("UPDATE parts SET requirement=x'50504a530100000001d800',option_group_id=x'50504a530100000001dc00' WHERE id=2; UPDATE plan_revision_parts SET requirement=x'50504a530100000001d800',option_group_id=x'50504a530100000001dc00' WHERE id=2; UPDATE plan_revisions SET snapshot_digest='dce022016e6d53ef863ddd38b2f132add059758f0f53f109a65352c0bc3e9eab';");
    let profile = f.read();
    assert!(
        matches!(
            profile.accepted_progress,
            AcceptedProgress::Ready {
                total_units: 1,
                remaining_units: 1
            }
        ),
        "{profile:?}"
    );
    let batch = f
        .owner
        .as_ref()
        .unwrap()
        .accepted_reads_with_policy(policy())
        .unwrap()
        .read(
            credential(),
            &[1],
            &AtomicBool::new(false),
            Duration::from_secs(2),
        )
        .unwrap();
    assert!(matches!(
        &batch.builds[0].accepted,
        pp_storage::read_model::AcceptedRead::Ready { .. }
    ));
    let body = String::from_utf8(batch.into_json_body().unwrap().into_bytes()).unwrap();
    assert!(body.contains(r#""requirement":"\ud800""#));
    assert!(body.contains(r#""optionGroupId":"\udc00""#));
    f.mutate("UPDATE parts SET requirement=x'50504a530100000001d801' WHERE id=2;");
    assert!(matches!(
        f.read().accepted_progress,
        AcceptedProgress::Unavailable(AcceptedProgressUnavailable::Integrity)
    ));
}

#[test]
fn review_malformed_manifest_is_typed_and_list_keeps_healthy_build() {
    for field in ["requirement", "option_group_id"] {
        let f = Fixture::new();
        f.mutate(&format!("UPDATE plan_revision_parts SET {field}=x'50504a5301' WHERE id=2; INSERT INTO build_profiles(id,tenant_id,name) VALUES(20,'default','Healthy');"));
        assert!(matches!(
            f.read().accepted_progress,
            AcceptedProgress::Unavailable(AcceptedProgressUnavailable::Integrity)
        ));
        let BuildOutcome::Listed { profiles } = f.run(BuildCommand::List).unwrap() else {
            panic!("list")
        };
        assert_eq!(profiles.len(), 2);
        assert!(matches!(
            profiles
                .iter()
                .find(|p| p.id == 20)
                .unwrap()
                .accepted_progress,
            AcceptedProgress::Empty
        ));
    }
}

#[test]
fn review_list_caps_builds_before_loading_graphs() {
    let f = Fixture::new();
    f.mutate("WITH RECURSIVE n(id) AS (SELECT 2 UNION ALL SELECT id+1 FROM n WHERE id<64) INSERT INTO build_profiles(id,tenant_id,name) SELECT id,'default','Empty '||id FROM n;");
    let BuildOutcome::Listed { profiles } = f.run(BuildCommand::List).unwrap() else {
        panic!("list")
    };
    assert_eq!(profiles.len(), 64);
    f.mutate("INSERT INTO build_profiles(id,tenant_id,name) VALUES(65,'default','Extra'); DROP TABLE plan_revision_parts;");
    let error = f.run(BuildCommand::List).unwrap_err();
    assert!(matches!(error.downcast_ref(), Some(Failure::TooManyBuilds)));
}

#[test]
fn review_working_counts_non_scalar_metadata_edits() {
    for field in ["requirement", "option_group_id"] {
        let f = Fixture::new();
        let c = f.raw();
        let columns = c
            .prepare("SELECT * FROM plan_revision_parts")
            .unwrap()
            .column_names()
            .iter()
            .filter(|s| !matches!(**s, "id" | "revision_id" | "projection_part_id"))
            .map(|s| s.to_string())
            .collect::<Vec<_>>()
            .join(",");
        drop(c);
        f.mutate(&format!("DELETE FROM plan_draft_parts; DELETE FROM plan_drafts; INSERT INTO plan_drafts(id,tenant_id,profile_id,base_revision_id,base_plan_version,state,digest_format,snapshot_digest,created_by,idempotency_key,created_at) SELECT 1,tenant_id,profile_id,id,1,'open','v1','digest','fixture','key','then' FROM plan_revisions LIMIT 1; INSERT INTO plan_draft_parts(draft_id,base_revision_part_id,{columns}) SELECT 1,id,{columns} FROM plan_revision_parts; UPDATE plan_revision_parts SET {field}=x'50504a530100000001d800' WHERE id=2; UPDATE plan_draft_parts SET {field}=x'50504a530100000001d801' WHERE base_revision_part_id=2;"));
        let batch = f
            .owner
            .as_ref()
            .unwrap()
            .accepted_reads_with_policy(policy())
            .unwrap()
            .read(
                credential(),
                &[1],
                &AtomicBool::new(false),
                Duration::from_secs(2),
            )
            .unwrap();
        let context = serde_json::to_value(batch.builds[0].context.as_ref().unwrap()).unwrap();
        assert_eq!(
            context["workflow"]["workspace"]["working_plan"]["change_count"],
            serde_json::json!(1),
            "{context}"
        );
    }
}

#[test]
fn review_missing_accepted_input_retains_healthy_build() {
    let f = Fixture::new();
    f.mutate("UPDATE plan_accepted_input_sets SET input_set_id=999999; INSERT INTO build_profiles(id,tenant_id,name) VALUES(20,'default','Healthy');");
    let profile = f.read();
    assert!(matches!(
        profile.accepted_progress,
        AcceptedProgress::Unavailable(AcceptedProgressUnavailable::Integrity)
    ));
    assert!(
        matches!(profile.freshness, PlanFreshness::Unavailable { code } if code == "accepted_inputs")
    );
    let BuildOutcome::Listed { profiles } = f.run(BuildCommand::List).unwrap() else {
        panic!("list")
    };
    assert_eq!(profiles.len(), 2);
    assert!(
        profiles.iter().any(|profile| profile.id == 20
            && matches!(profile.accepted_progress, AcceptedProgress::Empty))
    );
}

#[test]
fn review_layers_allow_1024_and_reject_1025() {
    let f = Fixture::new();
    f.mutate("DELETE FROM profile_layers; WITH RECURSIVE ids(n) AS (SELECT 1 UNION ALL SELECT n+1 FROM ids WHERE n<1024) INSERT INTO profile_layers(id,tenant_id,profile_id,layer_order,layer_type) SELECT n,'default',1,n,'addon' FROM ids;");
    let BuildOutcome::Layers { layers, .. } =
        f.run(BuildCommand::ListLayers { build: id(1) }).unwrap()
    else {
        panic!("layers")
    };
    assert_eq!(layers.len(), 1024);
    f.mutate("INSERT INTO profile_layers(id,tenant_id,profile_id,layer_order,layer_type) VALUES(1025,'default',1,1025,'addon');");
    let error = f
        .run(BuildCommand::ListLayers { build: id(1) })
        .unwrap_err();
    assert!(matches!(error.downcast_ref(), Some(Failure::TooManyLayers)));
}

fn name(value: &str) -> pp_storage::build_graph::BuildName {
    pp_storage::build_graph::BuildName::parse(value.into()).unwrap()
}
fn create(
    f: &Fixture,
    value: &str,
    source: Option<PositiveId>,
) -> (PositiveId, Vec<pp_storage::build_graph::ProfileLayer>) {
    let BuildOutcome::Created { profile, layers } = f
        .run(BuildCommand::Create {
            name: name(value),
            base_source: source,
        })
        .unwrap()
    else {
        panic!("create")
    };
    (id(profile.id as u64), layers)
}
fn read_build(f: &Fixture, build: PositiveId) -> ProfileSummary {
    let BuildOutcome::Read { profile } = f.run(BuildCommand::Read { build }).unwrap() else {
        panic!("read")
    };
    *profile
}
fn layer_result(f: &Fixture, command: BuildCommand) -> Vec<pp_storage::build_graph::ProfileLayer> {
    let BuildOutcome::Layers { layers, .. } = f.run(command).unwrap() else {
        panic!("layers")
    };
    layers
}
fn sources(f: &Fixture) {
    f.raw().execute_batch("INSERT INTO projects(id,tenant_id,name,url) VALUES(2,'default','Addon','addon'),(3,'default','Replacement','replacement'),(50,'other','Foreign','foreign');").unwrap();
}

#[test]
fn mutation_create_touch_attachments_and_delete() {
    let f = Fixture::new();
    sources(&f);
    let (build, initial) = create(&f, "\u{feff} New build \u{a0}", Some(id(1)));
    assert_eq!(read_build(&f, build).name, "New build");
    assert_eq!(initial.len(), 1);
    assert_eq!(
        (
            initial[0].layer_order,
            initial[0].layer_type.as_str(),
            initial[0].project_id
        ),
        (0, "base", Some(1))
    );
    let layers = layer_result(
        &f,
        BuildCommand::AttachAddon {
            build,
            source: id(2),
        },
    );
    assert_eq!(
        layers
            .iter()
            .map(|l| (l.layer_order, l.project_id))
            .collect::<Vec<_>>(),
        vec![(0, Some(1)), (1, Some(2))]
    );
    let addon = id(layers[1].id as u64);
    let layers = layer_result(
        &f,
        BuildCommand::ReplaceAttachment {
            build,
            layer: addon,
            source: id(3),
        },
    );
    assert_eq!(layers[1].project_id, Some(3));
    assert_eq!(layers[1].layer_type, "addon");
    let layers = layer_result(
        &f,
        BuildCommand::SetBase {
            build,
            source: id(2),
        },
    );
    assert_eq!(layers[0].id, initial[0].id);
    assert_eq!(layers[0].project_id, Some(2));
    f.raw()
        .execute(
            "UPDATE build_profiles SET config_modified_at='old',last_used_at='old' WHERE id=?1",
            [build.get() as i64],
        )
        .unwrap();
    let BuildOutcome::Touched { profile } = f.run(BuildCommand::Touch { build }).unwrap() else {
        panic!("touch")
    };
    assert_ne!(profile.last_used_at.as_deref(), Some("old"));
    let config: String = f
        .raw()
        .query_row(
            "SELECT config_modified_at FROM build_profiles WHERE id=?1",
            [build.get() as i64],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(config, "old");
    assert!(matches!(
        f.run(BuildCommand::Detach {
            build,
            layer: addon
        })
        .unwrap(),
        BuildOutcome::Deleted
    ));
    let config: String = f
        .raw()
        .query_row(
            "SELECT config_modified_at FROM build_profiles WHERE id=?1",
            [build.get() as i64],
            |r| r.get(0),
        )
        .unwrap();
    assert_ne!(config, "old");
    assert_eq!(
        layer_result(&f, BuildCommand::ListLayers { build }).len(),
        1
    );
    assert!(matches!(
        f.run(BuildCommand::Delete { build }).unwrap(),
        BuildOutcome::Deleted
    ));
    assert!(matches!(
        f.run(BuildCommand::Read { build }).unwrap(),
        BuildOutcome::MissingBuild
    ));
    let (empty, layers) = create(&f, "Empty", None);
    assert_eq!(layers.len(), 0);
    assert_eq!(
        layer_result(
            &f,
            BuildCommand::SetBase {
                build: empty,
                source: id(1)
            }
        )[0]
        .layer_type,
        "base"
    );
    assert_eq!(read_build(&f, id(1)).id, 1);
}

#[test]
fn mutation_duplicate_missing_membership_and_tenant_cases() {
    let f = Fixture::new();
    sources(&f);
    let (build, initial) = create(&f, "Owned", Some(id(1)));
    let layer = id(initial[0].id as u64);
    assert!(
        matches!(f.run(BuildCommand::Create { name: name("Owned"), base_source: None }).unwrap(), BuildOutcome::DuplicateName { name } if name == "Owned")
    );
    for source in [50, 999] {
        assert!(matches!(
            f.run(BuildCommand::Create {
                name: name("Invalid"),
                base_source: Some(id(source))
            })
            .unwrap(),
            BuildOutcome::InvalidBaseSource
        ));
        let invalid_rows: i64 = f
            .raw()
            .query_row(
                "SELECT count(*) FROM build_profiles WHERE tenant_id='default' AND name='Invalid'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(invalid_rows, 0);
        for command in [
            BuildCommand::SetBase {
                build,
                source: id(source),
            },
            BuildCommand::AttachAddon {
                build,
                source: id(source),
            },
            BuildCommand::ReplaceAttachment {
                build,
                layer,
                source: id(source),
            },
        ] {
            assert!(matches!(
                f.run(command).unwrap(),
                BuildOutcome::MissingSource
            ));
        }
    }
    for command in [
        BuildCommand::AttachAddon {
            build,
            source: id(1),
        },
        BuildCommand::ReplaceAttachment {
            build,
            layer: id(1),
            source: id(2),
        },
        BuildCommand::Detach {
            build,
            layer: id(1),
        },
    ] {
        match command {
            BuildCommand::AttachAddon { .. } => assert!(matches!(
                f.run(command).unwrap(),
                BuildOutcome::DuplicateAttachment { .. }
            )),
            _ => assert!(matches!(
                f.run(command).unwrap(),
                BuildOutcome::MissingLayer
            )),
        }
    }
    layer_result(
        &f,
        BuildCommand::AttachAddon {
            build,
            source: id(2),
        },
    );
    for command in [
        BuildCommand::SetBase {
            build,
            source: id(2),
        },
        BuildCommand::ReplaceAttachment {
            build,
            layer,
            source: id(2),
        },
    ] {
        assert!(
            matches!(f.run(command).unwrap(), BuildOutcome::DuplicateAttachment { source_name } if source_name == "Addon")
        );
    }
    assert_eq!(
        layer_result(
            &f,
            BuildCommand::ReplaceAttachment {
                build,
                layer,
                source: id(1)
            }
        )[0]
        .project_id,
        Some(1)
    );
    f.raw().execute_batch("INSERT INTO build_profiles(id,tenant_id,name) VALUES(50,'other','Owned'); INSERT INTO profile_layers(id,tenant_id,profile_id,layer_type,project_id) VALUES(50,'other',50,'base',50);").unwrap();
    for build in [id(50), id(999)] {
        for command in [
            BuildCommand::Touch { build },
            BuildCommand::SetBase {
                build,
                source: id(1),
            },
            BuildCommand::AttachAddon {
                build,
                source: id(1),
            },
            BuildCommand::ReplaceAttachment {
                build,
                layer: id(50),
                source: id(1),
            },
            BuildCommand::Detach {
                build,
                layer: id(50),
            },
            BuildCommand::Delete { build },
        ] {
            assert!(matches!(
                f.run(command).unwrap(),
                BuildOutcome::MissingBuild
            ));
        }
    }
    for command in [
        BuildCommand::ReplaceAttachment {
            build,
            layer: id(50),
            source: id(1),
        },
        BuildCommand::Detach {
            build,
            layer: id(50),
        },
    ] {
        assert!(matches!(
            f.run(command).unwrap(),
            BuildOutcome::MissingLayer
        ));
    }
    let foreign: String = f
        .raw()
        .query_row("SELECT name FROM build_profiles WHERE id=50", [], |r| {
            r.get(0)
        })
        .unwrap();
    assert_eq!(foreign, "Owned");
    assert_eq!(
        layer_result(&f, BuildCommand::ListLayers { build })
            .iter()
            .map(|l| l.project_id)
            .collect::<Vec<_>>(),
        vec![Some(1), Some(2)]
    );
}

#[test]
fn mutation_delete_cascades_accepted_graph_and_scoped_settings() {
    let f = Fixture::new();
    f.raw().execute_batch("INSERT INTO app_settings(tenant_id,key,value) VALUES('default','production_setup:1','{}'),('default','role_filaments_1','{}'),('default','feature_toggles:plan:1','{}'),('other','production_setup:1','keep'),('other','role_filaments_1','keep'),('other','feature_toggles:plan:1','keep'),('default','production_setup:2','keep'),('default','role_filaments_2','keep'),('default','feature_toggles:plan:2','keep'),('default','feature_toggles','keep');").unwrap();
    assert!(matches!(
        f.run(BuildCommand::Delete { build: id(1) }).unwrap(),
        BuildOutcome::Deleted
    ));
    let c = f.raw();
    for table in [
        "build_profiles",
        "profile_layers",
        "parts",
        "print_progress",
        "plan_revisions",
        "plan_revision_parts",
        "required_units",
        "plan_revision_required_units",
        "plan_revision_input_sets",
        "plan_accepted_input_sets",
    ] {
        let count: i64 = c
            .query_row(&format!("SELECT count(*) FROM {table}"), [], |r| r.get(0))
            .unwrap();
        assert_eq!(count, 0, "{table}");
    }
    let settings: Vec<(String, String)> = c
        .prepare("SELECT tenant_id,key FROM app_settings WHERE value='keep' ORDER BY tenant_id,key")
        .unwrap()
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
        .unwrap()
        .collect::<rusqlite::Result<_>>()
        .unwrap();
    assert_eq!(
        settings,
        vec![
            ("default".into(), "feature_toggles".into()),
            ("default".into(), "feature_toggles:plan:2".into()),
            ("default".into(), "production_setup:2".into()),
            ("default".into(), "role_filaments_2".into()),
            ("other".into(), "feature_toggles:plan:1".into()),
            ("other".into(), "production_setup:1".into()),
            ("other".into(), "role_filaments_1".into())
        ]
    );
    let removed: i64 = c.query_row("SELECT count(*) FROM app_settings WHERE tenant_id='default' AND key IN ('production_setup:1','role_filaments_1','feature_toggles:plan:1')", [], |r| r.get(0)).unwrap();
    assert_eq!(removed, 0);
    assert!(
        c.prepare("PRAGMA foreign_key_check")
            .unwrap()
            .query([])
            .unwrap()
            .next()
            .unwrap()
            .is_none()
    );
}

#[test]
fn mutation_rolls_back_when_returning_layers_or_freshness_update_fails() {
    let f = Fixture::new();
    sources(&f);
    f.raw().execute_batch("CREATE TRIGGER reject_config BEFORE UPDATE OF config_modified_at ON build_profiles BEGIN SELECT RAISE(ABORT,'reject config'); END;").unwrap();
    assert!(
        f.run(BuildCommand::AttachAddon {
            build: id(1),
            source: id(2)
        })
        .is_err()
    );
    assert_eq!(
        layer_result(&f, BuildCommand::ListLayers { build: id(1) }).len(),
        1
    );
    f.raw()
        .execute_batch("DROP TRIGGER reject_config;")
        .unwrap();
    f.mutate("WITH RECURSIVE n(id) AS (SELECT 2 UNION ALL SELECT id+1 FROM n WHERE id<1024) INSERT INTO profile_layers(id,tenant_id,profile_id,layer_order,layer_type) SELECT id,'default',1,id,'addon' FROM n;");
    let before: Option<String> = f
        .raw()
        .query_row(
            "SELECT config_modified_at FROM build_profiles WHERE id=1",
            [],
            |r| r.get(0),
        )
        .unwrap();
    let error = f
        .run(BuildCommand::AttachAddon {
            build: id(1),
            source: id(2),
        })
        .unwrap_err();
    assert!(matches!(error.downcast_ref(), Some(Failure::TooManyLayers)));
    assert_eq!(
        layer_result(&f, BuildCommand::ListLayers { build: id(1) }).len(),
        1024
    );
    let after: Option<String> = f
        .raw()
        .query_row(
            "SELECT config_modified_at FROM build_profiles WHERE id=1",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(after, before);
}

#[test]
fn mutation_graph_budget_is_distinct_and_touch_rolls_back() {
    use pp_storage::build_graph::BudgetResource;
    for (sql, resource) in [
        (
            "WITH RECURSIVE n(id) AS (SELECT 2 UNION ALL SELECT id+1 FROM n WHERE id<200001) INSERT INTO profile_layers(id,tenant_id,profile_id,layer_type) SELECT id,'default',1,'addon' FROM n;",
            BudgetResource::GraphRows,
        ),
        (
            "WITH RECURSIVE n(id) AS (SELECT 2 UNION ALL SELECT id+1 FROM n WHERE id<1100) INSERT INTO profile_layers(id,tenant_id,profile_id,layer_type) SELECT id,'default',1,replace(hex(zeroblob(32500)),'0','a') FROM n;",
            BudgetResource::GraphBytes,
        ),
    ] {
        let f = Fixture::new();
        f.mutate(sql);
        f.raw()
            .execute_batch("UPDATE build_profiles SET last_used_at='before';")
            .unwrap();
        for command in [
            BuildCommand::Read { build: id(1) },
            BuildCommand::List,
            BuildCommand::Touch { build: id(1) },
        ] {
            let error = f.run(command).unwrap_err();
            assert!(
                matches!(error.downcast_ref(), Some(Failure::BudgetExhausted { resource: actual }) if *actual == resource),
                "{error:?}"
            );
        }
        let error = f
            .owner
            .as_ref()
            .unwrap()
            .accepted_reads_with_policy(policy())
            .unwrap()
            .read(
                credential(),
                &[1],
                &AtomicBool::new(false),
                Duration::from_secs(2),
            )
            .unwrap_err();
        assert!(
            matches!(error.downcast_ref(), Some(Failure::BudgetExhausted { resource: actual }) if *actual == resource),
            "{error:?}"
        );
        let last: String = f
            .raw()
            .query_row(
                "SELECT last_used_at FROM build_profiles WHERE id=1",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(last, "before");
    }
    let f = Fixture::new();
    f.mutate("UPDATE plan_revisions SET snapshot_digest='broken';");
    assert!(matches!(
        f.read().accepted_progress,
        AcceptedProgress::Unavailable(AcceptedProgressUnavailable::Integrity)
    ));
}

#[test]
fn mutation_name_validation_matches_reference() {
    use pp_storage::build_graph::BuildName;
    let name: pp_domain::build_identity::BuildName =
        BuildName::parse("\u{feff} Build \u{2029}".into()).unwrap();
    assert_eq!(name.as_str(), "Build");
    assert_eq!(
        BuildName::parse(" \u{a0}".into()).unwrap_err(),
        "Profile name is required"
    );
    assert!(BuildName::parse("a".repeat(4096)).is_ok());
    assert_eq!(
        BuildName::parse("a".repeat(4097)).unwrap_err(),
        "Profile name is too long"
    );
}

#[test]
fn mutation_cancellation_after_transaction_starts_rolls_back_touch() {
    use std::sync::atomic::Ordering;
    let f = Fixture::new();
    f.raw().execute_batch("UPDATE build_profiles SET last_used_at='before'; CREATE TRIGGER slow_touch AFTER UPDATE OF last_used_at ON build_profiles BEGIN SELECT sum(n) FROM (WITH RECURSIVE n(n) AS (SELECT 1 UNION ALL SELECT n+1 FROM n WHERE n<1000000) SELECT n FROM n); END;").unwrap();
    let client = f.client.clone();
    let cancelled = Arc::new(AtomicBool::new(false));
    let worker_cancelled = cancelled.clone();
    let worker = std::thread::spawn(move || {
        client.execute(
            credential(),
            BuildCommand::Touch { build: id(1) },
            worker_cancelled,
            Duration::from_secs(2),
        )
    });
    let probe = f.raw();
    probe.busy_timeout(Duration::ZERO).unwrap();
    let deadline = std::time::Instant::now() + Duration::from_secs(2);
    loop {
        match probe.execute_batch("BEGIN IMMEDIATE") {
            Ok(()) => probe.execute_batch("ROLLBACK").unwrap(),
            Err(rusqlite::Error::SqliteFailure(error, _))
                if error.code == rusqlite::ErrorCode::DatabaseBusy =>
            {
                break;
            }
            Err(error) => panic!("{error}"),
        }
        assert!(
            std::time::Instant::now() < deadline,
            "mutation never started"
        );
        std::thread::sleep(Duration::from_millis(1));
    }
    cancelled.store(true, Ordering::Release);
    let error = worker.join().unwrap().unwrap_err();
    assert!(
        matches!(error.downcast_ref(), Some(Failure::Cancelled)),
        "{error:?}"
    );
    let last: String = f
        .raw()
        .query_row(
            "SELECT last_used_at FROM build_profiles WHERE id=1",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(last, "before");
}

#[test]
fn mutation_create_and_delete_rollback_include_dependent_writes() {
    let f = Fixture::new();
    f.raw().execute_batch("CREATE TRIGGER reject_layer BEFORE INSERT ON profile_layers BEGIN SELECT RAISE(ABORT,'reject layer'); END;").unwrap();
    assert!(
        f.run(BuildCommand::Create {
            name: name("Rolled back"),
            base_source: Some(id(1))
        })
        .is_err()
    );
    let count: i64 = f
        .raw()
        .query_row(
            "SELECT count(*) FROM build_profiles WHERE name='Rolled back'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(count, 0);
    f.raw().execute_batch("DROP TRIGGER reject_layer; INSERT INTO app_settings(tenant_id,key,value) VALUES('default','production_setup:1','keep'),('default','role_filaments_1','keep'),('default','feature_toggles:plan:1','keep'); CREATE TRIGGER reject_delete BEFORE DELETE ON build_profiles BEGIN SELECT RAISE(ABORT,'reject delete'); END;").unwrap();
    assert!(f.run(BuildCommand::Delete { build: id(1) }).is_err());
    assert_eq!(f.read().id, 1);
    let settings = || {
        f.raw()
            .prepare("SELECT key,value FROM app_settings WHERE tenant_id='default' AND key IN ('production_setup:1','role_filaments_1','feature_toggles:plan:1') ORDER BY key")
            .unwrap()
            .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))
            .unwrap()
            .collect::<rusqlite::Result<Vec<_>>>()
            .unwrap()
    };
    let expected = vec![
        ("feature_toggles:plan:1".into(), "keep".into()),
        ("production_setup:1".into(), "keep".into()),
        ("role_filaments_1".into(), "keep".into()),
    ];
    assert_eq!(settings(), expected);
    f.raw().execute_batch("DROP TRIGGER reject_delete; CREATE TRIGGER reject_plan_delete BEFORE DELETE ON app_settings WHEN OLD.tenant_id='default' AND OLD.key='feature_toggles:plan:1' BEGIN SELECT RAISE(ABORT,'reject plan delete'); END;").unwrap();
    let error = f.run(BuildCommand::Delete { build: id(1) }).unwrap_err();
    assert!(
        error.to_string().contains("reject plan delete"),
        "{error:#}"
    );
    assert_eq!(f.read().id, 1);
    assert_eq!(settings(), expected);
    assert!(matches!(
        f.read().accepted_progress,
        AcceptedProgress::Ready {
            total_units: 1,
            remaining_units: 1
        }
    ));
    assert_eq!(
        layer_result(&f, BuildCommand::ListLayers { build: id(1) }).len(),
        1
    );
}

#[test]
fn mutation_requires_valid_credentials_before_writing() {
    let f = Fixture::new();
    for command in [
        BuildCommand::Create {
            name: name("Unauthorized"),
            base_source: None,
        },
        BuildCommand::Delete { build: id(1) },
    ] {
        assert!(
            f.client
                .execute(
                    Credential::Session(Secret::new("invalid".into())),
                    command,
                    Arc::new(AtomicBool::new(false)),
                    Duration::from_secs(2)
                )
                .is_err()
        );
    }
    assert_eq!(f.read().id, 1);
    let count: i64 = f
        .raw()
        .query_row("SELECT count(*) FROM build_profiles", [], |r| r.get(0))
        .unwrap();
    assert_eq!(count, 1);
}

#[test]
fn mutation_list_budget_is_shared_between_build_summaries() {
    use pp_storage::build_graph::BudgetResource;
    let f = Fixture::new();
    f.mutate("INSERT INTO build_profiles(id,tenant_id,name) VALUES(2,'default','Second'); WITH RECURSIVE n(id) AS (SELECT 2 UNION ALL SELECT id+1 FROM n WHERE id<200000) INSERT INTO profile_layers(id,tenant_id,profile_id,layer_type) SELECT id,'default',CASE WHEN id<=100000 THEN 1 ELSE 2 END,'addon' FROM n;");
    for build in [id(1), id(2)] {
        assert_eq!(read_build(&f, build).id, build.get() as i64);
    }
    let error = f.run(BuildCommand::List).unwrap_err();
    assert!(
        matches!(
            error.downcast_ref(),
            Some(Failure::BudgetExhausted {
                resource: BudgetResource::GraphRows
            })
        ),
        "{error:?}"
    );
}

#[test]
fn audit_delete_cascades_public_accepted_plate_graph_across_reopen() {
    let mut f = Fixture::from_bytes(include_bytes!("fixtures/accepted-plate-node.db"));
    let tables = [
        "profile_layers",
        "parts",
        "print_progress",
        "plan_drafts",
        "plan_draft_inputs",
        "plan_draft_parts",
        "plan_draft_required_unit_reconciliations",
        "plan_draft_required_unit_assignments",
        "plan_revisions",
        "plan_revision_parts",
        "plan_revision_input_sets",
        "plan_revision_inputs",
        "plan_accepted_input_sets",
        "plan_revision_required_unit_sets",
        "plan_revision_required_units",
        "plan_apply_requests",
        "required_units",
        "accepted_plate_revisions",
        "accepted_plate_heads",
        "accepted_plates",
        "accepted_plate_units",
    ];
    for table in tables {
        let count: i64 = f
            .raw()
            .query_row(&format!("SELECT count(*) FROM {table}"), [], |r| r.get(0))
            .unwrap();
        assert!(count > 0, "fixture must exercise {table}");
    }
    let sources: i64 = f
        .raw()
        .query_row("SELECT count(*) FROM projects", [], |r| r.get(0))
        .unwrap();
    assert!(sources > 0);
    assert!(matches!(
        f.run(BuildCommand::Delete { build: id(1) }).unwrap(),
        BuildOutcome::Deleted
    ));
    f.owner.take().unwrap().shutdown().unwrap();
    for _ in 0..2 {
        let (owner, _) = WriterOwner::open(&f.root, Limits::default()).unwrap();
        let client = owner.build_graph_with_policy(policy()).unwrap();
        assert!(matches!(
            client
                .execute(
                    credential(),
                    BuildCommand::Read { build: id(1) },
                    Arc::new(AtomicBool::new(false)),
                    Duration::from_secs(2)
                )
                .unwrap(),
            BuildOutcome::MissingBuild
        ));
        owner.shutdown().unwrap();
        let c = f.raw();
        for table in ["build_profiles"].into_iter().chain(tables) {
            let count: i64 = c
                .query_row(&format!("SELECT count(*) FROM {table}"), [], |r| r.get(0))
                .unwrap();
            assert_eq!(count, 0, "delete must cascade {table}");
        }
        assert_eq!(
            c.query_row("SELECT count(*) FROM projects", [], |r| r.get::<_, i64>(0))
                .unwrap(),
            sources
        );
        assert!(
            c.prepare("PRAGMA foreign_key_check")
                .unwrap()
                .query([])
                .unwrap()
                .next()
                .unwrap()
                .is_none()
        );
    }
}

#[test]
fn audit_delete_cascades_terminal_jobs_and_preserves_controls_across_reopen() {
    for status in ["completed", "sent"] {
        let mut f = Fixture::new();
        let (control, _) = create(&f, "Same tenant control", None);
        f.raw().execute_batch("INSERT INTO build_profiles(id,tenant_id,name) VALUES(999,'other','Foreign control')").unwrap();
        let sources: i64 = f
            .raw()
            .query_row("SELECT count(*) FROM projects", [], |r| r.get(0))
            .unwrap();
        assert!(sources > 0);
        for (job, tenant, build) in [
            ("target-job", "default", 1),
            ("control-job", "default", control.get() as i64),
            ("foreign-job", "other", 999),
        ] {
            f.raw().execute("INSERT INTO print_jobs(id,tenant_id,profile_id,at,status) VALUES(?1,?2,?3,'2026-01-01T00:00:00.000Z',?4)",
                rusqlite::params![job, tenant, build, status]).unwrap();
        }
        assert!(matches!(
            f.run(BuildCommand::Delete { build: id(1) }).unwrap(),
            BuildOutcome::Deleted
        ));
        f.owner.take().unwrap().shutdown().unwrap();
        for _ in 0..2 {
            let (owner, _) = WriterOwner::open(&f.root, Limits::default()).unwrap();
            let client = owner.build_graph_with_policy(policy()).unwrap();
            assert!(matches!(
                client
                    .execute(
                        credential(),
                        BuildCommand::Read { build: id(1) },
                        Arc::new(AtomicBool::new(false)),
                        Duration::from_secs(2)
                    )
                    .unwrap(),
                BuildOutcome::MissingBuild
            ));
            assert!(
                matches!(client.execute(credential(), BuildCommand::Read { build: control },
                Arc::new(AtomicBool::new(false)), Duration::from_secs(2)).unwrap(), BuildOutcome::Read { profile } if profile.name == "Same tenant control")
            );
            owner.shutdown().unwrap();
            let c = f.raw();
            let jobs: Vec<(String, String)> = c
                .prepare("SELECT id,status FROM print_jobs ORDER BY id")
                .unwrap()
                .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
                .unwrap()
                .collect::<rusqlite::Result<_>>()
                .unwrap();
            assert_eq!(
                jobs,
                vec![
                    ("control-job".into(), status.into()),
                    ("foreign-job".into(), status.into())
                ]
            );
            assert_eq!(
                c.query_row(
                    "SELECT count(*) FROM build_profiles WHERE id IN (?1,999)",
                    [control.get() as i64],
                    |r| r.get::<_, i64>(0)
                )
                .unwrap(),
                2
            );
            assert_eq!(
                c.query_row("SELECT count(*) FROM projects", [], |r| r.get::<_, i64>(0))
                    .unwrap(),
                sources
            );
            assert!(
                c.prepare("PRAGMA foreign_key_check")
                    .unwrap()
                    .query([])
                    .unwrap()
                    .next()
                    .unwrap()
                    .is_none()
            );
        }
    }
}

#[test]
fn audit_created_build_and_attachments_survive_owner_reopen() {
    let mut f = Fixture::new();
    sources(&f);
    let (build, _) = create(&f, "Persisted build", Some(id(1)));
    let expected = layer_result(
        &f,
        BuildCommand::AttachAddon {
            build,
            source: id(2),
        },
    );
    assert_eq!(
        expected
            .iter()
            .map(|layer| (
                layer.layer_order,
                layer.layer_type.as_str(),
                layer.project_id
            ))
            .collect::<Vec<_>>(),
        vec![(0, "base", Some(1)), (1, "addon", Some(2))]
    );
    f.owner.take().unwrap().shutdown().unwrap();
    let (owner, _) = WriterOwner::open(&f.root, Limits::default()).unwrap();
    let client = owner.build_graph_with_policy(policy()).unwrap();
    let outcome = client
        .execute(
            credential(),
            BuildCommand::ListLayers { build },
            Arc::new(AtomicBool::new(false)),
            Duration::from_secs(2),
        )
        .unwrap();
    assert!(
        matches!(outcome, BuildOutcome::Layers { profile_id, layers }
        if profile_id == build.get() as i64 && layers == expected)
    );
    let outcome = client
        .execute(
            credential(),
            BuildCommand::Read { build },
            Arc::new(AtomicBool::new(false)),
            Duration::from_secs(2),
        )
        .unwrap();
    assert!(matches!(outcome, BuildOutcome::Read { profile } if profile.name == "Persisted build"));
    owner.shutdown().unwrap();
}
