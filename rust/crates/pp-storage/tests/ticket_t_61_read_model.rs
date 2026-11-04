use pp_storage::{
    Limits, WriterOwner,
    auth::Secret,
    read_model::{
        AcceptedRead, Credential, ReadClient,
        views::{self, CatalogOnly, ReviewObservations},
    },
};
use rusqlite::{Connection, params};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::os::unix::fs::DirBuilderExt;
use std::{
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};

struct Fixture {
    root: PathBuf,
    owner: Option<WriterOwner>,
    client: ReadClient,
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
        let client = owner.accepted_reads().unwrap();
        Self {
            root,
            owner: Some(owner),
            client,
        }
    }
    fn read(&self) -> pp_storage::read_model::Batch {
        self.client
            .read(
                Credential::Session(Secret::new("read-fixture-secret".into())),
                &[1],
                &AtomicBool::new(false),
                Duration::from_secs(1),
            )
            .unwrap()
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
    fn dump(&self) -> String {
        let c = self.raw();
        let mut s = String::new();
        let tables = c
            .prepare("SELECT name FROM sqlite_master WHERE type='table' ORDER BY name")
            .unwrap()
            .query_map([], |r| r.get::<_, String>(0))
            .unwrap()
            .collect::<rusqlite::Result<Vec<_>>>()
            .unwrap();
        for t in tables {
            let mut stmt = c
                .prepare(&format!("SELECT * FROM \"{t}\" ORDER BY rowid"))
                .unwrap();
            let count = stmt.column_count();
            let mut rows = stmt.query([]).unwrap();
            while let Some(r) = rows.next().unwrap() {
                for i in 0..count {
                    s.push_str(&format!("{:?}|", r.get_ref(i).unwrap()));
                }
            }
        }
        s
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        if let Some(o) = self.owner.take() {
            o.shutdown().unwrap();
        }
        std::fs::remove_dir_all(&self.root).unwrap();
    }
}
fn failure(sql: &str, code: &str) {
    let f = Fixture::new();
    f.mutate(sql);
    let before = f.dump();
    let read = f.read();
    assert!(
        matches!(&read.builds[0].accepted,AcceptedRead::IntegrityFailure{code:actual,..} if actual==code),
        "{:#?}",
        read.builds[0].accepted
    );
    assert_eq!(before, f.dump());
}

#[test]
fn ticket_t_61_read_model_node_graph_no_writes() {
    let f = Fixture::new();
    let before = f.dump();
    let b = f.read();
    let AcceptedRead::Ready { snapshot } = &b.builds[0].accepted else {
        panic!("{b:?}")
    };
    assert_eq!(snapshot.parts.len(), 2);
    assert!(snapshot.parts.iter().all(|p| p.units.len() == 1));
    assert!(
        b.builds[0].context_error.is_none(),
        "{:?}",
        b.builds[0].context_error
    );
    assert_eq!(before, f.dump());
    let part = &snapshot.parts[0];
    assert_eq!(
        views::assembled(part.projection_part_id, &b.builds[0].accepted)["body"]["assembled_units"],
        json!([false])
    );
}
#[test]
fn ticket_t_61_read_model_foreign_and_missing_are_indistinguishable() {
    let f = Fixture::new();
    f.mutate("UPDATE build_profiles SET tenant_id='foreign' WHERE id=1");
    let b = f
        .client
        .read(
            Credential::Session(Secret::new("read-fixture-secret".into())),
            &[1, 999],
            &AtomicBool::new(false),
            Duration::ZERO,
        )
        .unwrap();
    assert!(
        b.builds
            .iter()
            .all(|b| matches!(b.accepted, AcceptedRead::Missing) && b.context.is_none())
    );
}
#[test]
fn ticket_t_61_read_model_revocation_and_restart() {
    let mut f = Fixture::new();
    f.owner.take().unwrap().shutdown().unwrap();
    assert!(
        f.client
            .read(
                Credential::Session(Secret::new("read-fixture-secret".into())),
                &[1],
                &AtomicBool::new(false),
                Duration::ZERO
            )
            .is_err()
    );
    let (o, _) = WriterOwner::open(&f.root, Limits::default()).unwrap();
    f.client = o.accepted_reads().unwrap();
    f.owner = Some(o);
    assert!(matches!(
        f.read().builds[0].accepted,
        AcceptedRead::Ready { .. }
    ));
    f.raw().execute("DELETE FROM sessions", []).unwrap();
    assert!(
        f.client
            .read(
                Credential::Session(Secret::new("read-fixture-secret".into())),
                &[1],
                &AtomicBool::new(false),
                Duration::ZERO
            )
            .is_err()
    );
}
#[test]
fn ticket_t_61_read_model_admission_bounds() {
    let f = Fixture::new();
    for ids in [
        vec![0],
        vec![-1],
        vec![9_007_199_254_740_992],
        (1..=65).collect(),
    ] {
        assert!(
            f.client
                .read(
                    Credential::Session(Secret::new("read-fixture-secret".into())),
                    &ids,
                    &AtomicBool::new(false),
                    Duration::ZERO
                )
                .is_err()
        );
    }
    assert!(
        f.client
            .read(
                Credential::Session(Secret::new("read-fixture-secret".into())),
                &[1],
                &AtomicBool::new(true),
                Duration::ZERO
            )
            .is_err()
    );
    assert_eq!(
        f.client
            .read(
                Credential::Session(Secret::new("read-fixture-secret".into())),
                &[1, 1],
                &AtomicBool::new(false),
                Duration::ZERO
            )
            .unwrap()
            .builds
            .len(),
        1
    );
}
#[test]
fn ticket_t_61_read_model_digest() {
    failure(
        "UPDATE plan_revisions SET snapshot_digest=replace(snapshot_digest,'a','b')",
        "revision_digest",
    );
}
#[test]
fn ticket_t_61_read_model_pointer() {
    failure(
        "UPDATE build_profiles SET accepted_plan_version=0",
        "pointer",
    );
}
#[test]
fn ticket_t_61_read_model_revision_tenant() {
    failure("UPDATE plan_revisions SET tenant_id='other'", "revision");
}
#[test]
fn ticket_t_61_read_model_input_tenant() {
    failure(
        "UPDATE plan_revision_inputs SET tenant_id='other'",
        "accepted_inputs",
    );
}
#[test]
fn ticket_t_61_read_model_source_digest() {
    failure(
        "UPDATE source_revisions SET manifest_digest=replace(manifest_digest,'a','b')",
        "source_revision",
    );
}
#[test]
fn ticket_t_61_read_model_projection() {
    failure("UPDATE parts SET filename='wrong.stl'", "projection");
}
#[test]
fn ticket_t_61_read_model_mapping_digest() {
    failure(
        "UPDATE plan_revision_required_unit_sets SET mapping_digest='bad'",
        "required_unit_map",
    );
}
#[test]
fn ticket_t_61_read_model_missing_unit() {
    failure(
        "DELETE FROM required_units WHERE token=(SELECT token FROM required_units LIMIT 1)",
        "required_unit_map",
    );
}
#[test]
fn ticket_t_61_read_model_partial_unit_set() {
    failure(
        "DELETE FROM plan_revision_required_unit_sets",
        "required_unit_map",
    );
}
#[test]
fn ticket_t_61_read_model_progress() {
    failure(
        "UPDATE print_progress SET completed=0,assembled=1",
        "progress",
    );
}
#[test]
fn ticket_t_61_read_model_foreign_progress() {
    failure("UPDATE print_progress SET tenant_id='other'", "progress");
}
#[test]
fn ticket_t_61_read_model_text_budget() {
    failure(
        "UPDATE plan_revision_parts SET notes=printf('%070000d',1)",
        "revision",
    );
}
#[test]
fn ticket_t_61_read_model_unsafe_snapshot() {
    failure(
        "UPDATE source_revisions SET snapshot_locator='../outside'",
        "source_revision",
    );
}
#[test]
fn ticket_t_61_read_model_missing_snapshot() {
    let f = Fixture::new();
    std::fs::remove_dir_all(f.root.join("repos/1")).unwrap();
    assert!(
        matches!(&f.read().builds[0].accepted,AcceptedRead::IntegrityFailure{code,..} if code=="source_revision")
    );
}
#[test]
fn ticket_t_61_read_model_missing_observations_are_errors() {
    let f = Fixture::new();
    let b = f.read();
    let obs = ReviewObservations {
        available_input_roots: Default::default(),
        media_by_part_id: Default::default(),
    };
    #[cfg(feature = "desktop_rebuild")]
    assert!(views::review_json(&b.builds[0].accepted, false, &obs, &CatalogOnly).is_err());
    #[cfg(not(feature = "desktop_rebuild"))]
    assert!(views::review(&b.builds[0].accepted, false, &obs, &CatalogOnly).is_err());
}
#[test]
fn ticket_t_61_read_model_key_route_is_authorized_by_secret_only() {
    let f = Fixture::new();
    let key=json!([{"id":"key_legacy","keyHash":"bGVnYWN5LXNlY3JldA==","createdAt":"2026-01-01T00:00:00.000Z","lastUsedAt":null,"expiresAt":null,"isActive":true}]).to_string();
    for tenant in ["default", "other"] {
        f.raw()
            .execute(
                "INSERT INTO app_settings(tenant_id,key,value) VALUES(?1,'api_keys_v1',?2)",
                params![tenant, key],
            )
            .unwrap();
    }
    let before = f.dump();
    for tenant in ["default", "other"] {
        let b = f
            .client
            .read(
                Credential::ApiKey {
                    routed_tenant: tenant.into(),
                    secret: Secret::new("legacy-secret".into()),
                },
                &[1],
                &AtomicBool::new(false),
                Duration::ZERO,
            )
            .unwrap();
        assert_eq!(
            matches!(b.builds[0].accepted, AcceptedRead::Ready { .. }),
            tenant == "default"
        );
    }
    for (tenant, key) in [("default", "bad"), ("missing", "legacy-secret")] {
        assert!(
            f.client
                .read(
                    Credential::ApiKey {
                        routed_tenant: tenant.into(),
                        secret: Secret::new(key.into())
                    },
                    &[1],
                    &AtomicBool::new(false),
                    Duration::ZERO
                )
                .is_err()
        );
    }
    assert_eq!(before, f.dump());
}
#[test]
fn ticket_t_61_read_model_entire_batch_one_wal_snapshot() {
    let f = Fixture::new();
    let c = f.raw();
    c.execute("INSERT INTO build_profiles(id,tenant_id,name,accepted_plan_version) VALUES(2,'default','0:2',0)",[]).unwrap();
    c.execute("UPDATE build_profiles SET name='0:1' WHERE id=1", [])
        .unwrap();
    let part: i64 = c
        .query_row("SELECT id FROM parts WHERE included=1 LIMIT 1", [], |r| {
            r.get(0)
        })
        .unwrap();
    c.execute(
        "UPDATE print_progress SET completed=0,assembled=0 WHERE part_id=?1",
        [part],
    )
    .unwrap();
    drop(c);
    let done = Arc::new(AtomicBool::new(false));
    let flag = done.clone();
    let path = f.root.join("print-partner.db");
    let writer = std::thread::spawn(move || {
        let mut c = Connection::open(path).unwrap();
        c.busy_timeout(Duration::from_secs(3)).unwrap();
        let mut n = 0;
        while !flag.load(Ordering::Acquire) {
            n += 1;
            let tx = c.transaction().unwrap();
            tx.execute(
                "UPDATE build_profiles SET name=?1 || ':' || id WHERE id IN (1,2)",
                [n.to_string()],
            )
            .unwrap();
            tx.execute("UPDATE print_progress SET completed=?1", [n % 2])
                .unwrap();
            tx.commit().unwrap();
        }
        n
    });
    for _ in 0..40 {
        let b = f
            .client
            .read(
                Credential::Session(Secret::new("read-fixture-secret".into())),
                &[1, 2],
                &AtomicBool::new(false),
                Duration::from_secs(2),
            )
            .unwrap();
        let AcceptedRead::Ready { snapshot } = &b.builds[0].accepted else {
            panic!("{b:?}")
        };
        let AcceptedRead::Empty { profile } = &b.builds[1].accepted else {
            panic!("{b:?}")
        };
        assert_eq!(
            snapshot.profile.name.split(':').next(),
            profile.name.split(':').next()
        );
        let n: i64 = snapshot
            .profile
            .name
            .split(':')
            .next()
            .unwrap()
            .parse()
            .unwrap();
        let p = snapshot.parts.iter().find(|p| p.included).unwrap();
        assert_eq!(p.units[0].completed, n % 2 == 1);
        assert_eq!(
            b.builds[0].context.as_ref().unwrap().profile_summary["summary"]["header"]["name"],
            snapshot.profile.name
        );
    }
    done.store(true, Ordering::Release);
    assert!(writer.join().unwrap() > 0);
}
#[test]
fn ticket_t_61_read_model_collation_matches_measured_node() {
    let mut f = vec![
        "a", "A", "a2", "a10", "á", "Ä", "z", "Z", "é", "e", "😀", "\u{e000}",
    ];
    f.sort_by(|a, b| views::folder_compare(a, b));
    assert_eq!(
        f,
        vec![
            "😀", "a", "A", "á", "Ä", "a10", "a2", "e", "é", "z", "Z", "\u{e000}"
        ]
    );
}
#[test]
fn ticket_t_61_read_model_fixture_has_recorded_hash() {
    let hash = hex::encode(Sha256::digest(include_bytes!(
        "fixtures/accepted-plan-node.db"
    )));
    assert_eq!(hash.len(), 64);
    let v: Value = serde_json::from_str(include_str!("fixtures/accepted-plan-node.json")).unwrap();
    assert_eq!(v["databaseSha256"], hash);
}

#[test]
fn ticket_t_61_read_model_js_canonical_timestamp() {
    for value in [
        "0000-02-29T12:00:00.000Z",
        "+010000-01-01T00:00:00.001Z",
        "-000001-01-01T00:00:00.000Z",
        "+275760-09-13T00:00:00.000Z",
    ] {
        let f = Fixture::new();
        f.mutate(&format!("UPDATE source_revisions SET synced_at='{value}'"));
        assert!(
            matches!(f.read().builds[0].accepted, AcceptedRead::Ready { .. }),
            "{value}"
        );
    }
    for value in [
        "2026-02-29T00:00:00.000Z",
        "2026-01-01t00:00:00.000Z",
        "+000001-01-01T00:00:00.000Z",
        "2026-01-01T00:00:00Z",
        "+275760-09-13T00:00:00.001Z",
    ] {
        failure(
            &format!("UPDATE source_revisions SET synced_at='{value}'"),
            "source_revision",
        );
    }
}

#[test]
fn ticket_t_61_read_model_plate_and_history_corruption_is_explicit() {
    for sql in [
        "UPDATE accepted_plate_revisions SET layout_digest=printf('%064d',0)",
        "UPDATE accepted_plates SET tenant_id='foreign'",
        "UPDATE accepted_plate_units SET width_um=2147483647",
        "UPDATE accepted_plate_units SET required_unit_token='ppu_00000000000000000000000000000000'",
        "UPDATE plan_revisions SET parent_revision_id=id",
        "UPDATE plan_drafts SET base_revision_id=999999 WHERE state='open'",
    ] {
        let f = Fixture::from_bytes(include_bytes!("fixtures/accepted-plate-node.db"));
        f.mutate(sql);
        let before = f.dump();
        let read = f.read();
        assert!(read.builds[0].context.is_none(), "{sql}");
        assert!(read.builds[0].context_error.is_some(), "{sql}");
        assert_eq!(before, f.dump(), "{sql}");
    }
}

#[test]
fn ticket_t_61_read_model_workflow_rejects_unsafe_numbers() {
    use pp_storage::read_model::workflow::{
        self, Accepted, Build, Checkoff, Facts, PlateState, Production, Sources, Working,
    };
    let mut facts = Facts {
        build: Build {
            id: 1,
            name: "Build".into(),
        },
        sources: Sources::Empty,
        accepted_plan: Accepted::Ready {
            revision_id: 1,
            plan_version: 1,
            total_units: 1,
            remaining_units: 2,
        },
        working_plan: Working::None,
        production: Production {
            plate_state: PlateState::NotStarted,
            queued_jobs: 0,
            sending_jobs: 0,
            printing_jobs: 0,
            failed_jobs: 0,
        },
        checkoff: Checkoff {
            awaiting_verification: 0,
            failed_verifications: 0,
        },
    };
    assert!(workflow::resolve(&facts).is_err());
    facts.accepted_plan = Accepted::None;
    facts.production.queued_jobs = 9_007_199_254_740_991;
    facts.production.sending_jobs = 1;
    assert!(workflow::resolve(&facts).is_err());
}

#[test]
fn read_model_folder_rule_without_functional_class_matches_catalog_digest() {
    use pp_storage::catalog::{FolderRule, NamingCommand, NamingProfile, Outcome, Request, RoleId};
    let f = Fixture::new();
    let catalog = f.owner.as_ref().unwrap().local_source_catalog();
    let mut profile = NamingProfile::default();
    profile.folder_rules.push(FolderRule {
        path_contains: "frame".into(),
        role_id: RoleId::Primary,
        functional_class: None,
    });
    catalog
        .execute(Request::SaveGlobalNaming { profile })
        .unwrap();
    catalog
        .execute(Request::SaveNaming {
            id: 1,
            settings: NamingCommand::UseDefaults,
        })
        .unwrap();
    let db = f.raw();
    let set: i64 = db
        .query_row(
            "SELECT input_set_id FROM plan_accepted_input_sets WHERE profile_id=1 AND tenant_id='default'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    let sources = db
        .prepare(
            "SELECT source_id FROM plan_revision_inputs WHERE input_set_id=?1 ORDER BY source_id",
        )
        .unwrap()
        .query_map([set], |r| r.get::<_, i64>(0))
        .unwrap()
        .collect::<rusqlite::Result<Vec<_>>>()
        .unwrap();
    drop(db);
    let naming = sources
        .into_iter()
        .map(|id| {
            let Outcome::Data(value) = catalog.execute(Request::GetNaming { id }).unwrap() else {
                panic!("naming")
            };
            if id == 1 {
                assert!(
                    value["effective"]["folder_rules"][0]
                        .get("functional_class")
                        .is_none()
                );
                // Node digestEffectiveNaming writes `"functional_class":null`.
                assert_eq!(
                    value["effective_digest"],
                    "93dd019de4c92f363ed93faee9025d4fcc9cbf94463444ed33bf26cc70faef1f"
                );
            }
            (id, value["effective_digest"].as_str().unwrap().to_owned())
        })
        .collect::<Vec<_>>();
    let mut db = f.raw();
    let tx = db.transaction().unwrap();
    for (source, digest) in naming {
        assert_eq!(
            tx.execute(
                "UPDATE plan_revision_inputs SET effective_naming_digest=?1 WHERE input_set_id=?2 AND source_id=?3",
                params![digest, set, source],
            )
            .unwrap(),
            1
        );
    }
    let inputs = tx
        .prepare("SELECT source_id,source_layer,layer_order,tracking_kind,source_revision_id,manifest_digest,effective_naming_digest FROM plan_revision_inputs WHERE input_set_id=?1 ORDER BY source_id")
        .unwrap()
        .query_map([set], |r| {
            Ok(json!({
                "source_id": r.get::<_, i64>(0)?,
                "source_layer": r.get::<_, String>(1)?,
                "layer_order": r.get::<_, i64>(2)?,
                "tracking_kind": r.get::<_, String>(3)?,
                "source_revision_id": r.get::<_, Option<i64>>(4)?,
                "manifest_digest": r.get::<_, Option<String>>(5)?,
                "effective_naming_digest": r.get::<_, String>(6)?,
            }))
        })
        .unwrap()
        .collect::<rusqlite::Result<Vec<_>>>()
        .unwrap();
    let canonical = json!({"version": 2, "inputs": inputs}).to_string();
    tx.execute(
        "UPDATE plan_revision_input_sets SET input_set_digest=?1 WHERE id=?2",
        params![hex::encode(Sha256::digest(canonical.as_bytes())), set],
    )
    .unwrap();
    tx.commit().unwrap();
    drop(db);
    let before = f.dump();
    let read = f.read();
    assert!(matches!(
        read.builds[0].accepted,
        AcceptedRead::Ready { .. }
    ));
    let context = read.builds[0].context.as_ref().unwrap();
    assert_eq!(
        context.profile_summary["summary"]["header"]["freshness"]["status"],
        "current"
    );
    assert_eq!(before, f.dump());
}

#[test]
fn read_model_primary_only_naming_override_stays_current() {
    use pp_storage::catalog::{NamingCommand, NamingProfile, Outcome, Request, RoleId};
    let f = Fixture::new();
    let catalog = f.owner.as_ref().unwrap().local_source_catalog();
    let mut profile = NamingProfile::default();
    profile.roles.retain(|role| role.id == RoleId::Primary);
    catalog
        .execute(Request::SaveNaming {
            id: 1,
            settings: NamingCommand::Override { profile },
        })
        .unwrap();
    let db = f.raw();
    let set: i64 = db
        .query_row(
            "SELECT input_set_id FROM plan_accepted_input_sets WHERE profile_id=1 AND tenant_id='default'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    let sources = db
        .prepare(
            "SELECT source_id FROM plan_revision_inputs WHERE input_set_id=?1 ORDER BY source_id",
        )
        .unwrap()
        .query_map([set], |r| r.get::<_, i64>(0))
        .unwrap()
        .collect::<rusqlite::Result<Vec<_>>>()
        .unwrap();
    drop(db);
    let naming = sources
        .into_iter()
        .map(|id| {
            let Outcome::Data(value) = catalog.execute(Request::GetNaming { id }).unwrap() else {
                panic!("naming")
            };
            if id == 1 {
                assert_eq!(value["effective"]["roles"].as_array().unwrap().len(), 1);
            }
            (id, value["effective_digest"].as_str().unwrap().to_owned())
        })
        .collect::<Vec<_>>();
    let mut db = f.raw();
    let tx = db.transaction().unwrap();
    for (source, digest) in naming {
        assert_eq!(
            tx.execute(
                "UPDATE plan_revision_inputs SET effective_naming_digest=?1 WHERE input_set_id=?2 AND source_id=?3",
                params![digest, set, source],
            )
            .unwrap(),
            1
        );
    }
    let inputs = tx
        .prepare("SELECT source_id,source_layer,layer_order,tracking_kind,source_revision_id,manifest_digest,effective_naming_digest FROM plan_revision_inputs WHERE input_set_id=?1 ORDER BY source_id")
        .unwrap()
        .query_map([set], |r| {
            Ok(json!({
                "source_id": r.get::<_, i64>(0)?,
                "source_layer": r.get::<_, String>(1)?,
                "layer_order": r.get::<_, i64>(2)?,
                "tracking_kind": r.get::<_, String>(3)?,
                "source_revision_id": r.get::<_, Option<i64>>(4)?,
                "manifest_digest": r.get::<_, Option<String>>(5)?,
                "effective_naming_digest": r.get::<_, String>(6)?,
            }))
        })
        .unwrap()
        .collect::<rusqlite::Result<Vec<_>>>()
        .unwrap();
    let canonical = json!({"version": 2, "inputs": inputs}).to_string();
    tx.execute(
        "UPDATE plan_revision_input_sets SET input_set_digest=?1 WHERE id=?2",
        params![hex::encode(Sha256::digest(canonical.as_bytes())), set],
    )
    .unwrap();
    tx.commit().unwrap();
    drop(db);
    let read = f.read();
    let context = read.builds[0].context.as_ref().unwrap();
    assert_eq!(
        context.profile_summary["summary"]["header"]["freshness"]["status"],
        "current"
    );
}

#[test]
fn read_model_stale_freshness_marks_sources_stale() {
    use pp_storage::catalog::Request;
    let f = Fixture::new();
    f.owner
        .as_ref()
        .unwrap()
        .local_source_catalog()
        .execute(Request::SaveImportRules {
            id: 1,
            rules: vec!["STLs".into()],
        })
        .unwrap();
    let read = f.read();
    assert!(matches!(
        read.builds[0].accepted,
        AcceptedRead::Ready { .. }
    ));
    let context = read.builds[0].context.as_ref().unwrap();
    let freshness = &context.profile_summary["summary"]["header"]["freshness"];
    assert_eq!(freshness["status"], "stale");
    assert!(
        freshness["reasons"]
            .as_array()
            .unwrap()
            .iter()
            .any(|r| r["kind"] == "plan_configuration_changed")
    );
    let workspace = &context.workflow["workspace"];
    assert_eq!(workspace["sources"]["kind"], "stale");
    assert!(workspace["sources"]["attached_count"].as_u64().unwrap() > 0);
    assert!(workspace["sources"]["issue_count"].as_u64().unwrap() >= 1);
    assert_eq!(workspace["stages"][0]["status"]["kind"], "stale");
}
#[test]
fn ticket_t_61_read_model_part_resolves_its_plan_in_one_read() {
    let f = Fixture::new();
    let before = f.dump();
    let read = |part: i64| {
        f.client
            .read_part(
                Credential::Session(Secret::new("read-fixture-secret".into())),
                part,
                &AtomicBool::new(false),
                Duration::from_secs(1),
            )
            .unwrap()
    };
    let batch = read(2);
    assert_eq!(batch.builds.len(), 1);
    assert_eq!(batch.builds[0].profile_id, 1);
    assert!(matches!(
        batch.builds[0].accepted,
        AcceptedRead::Ready { .. }
    ));
    assert!(read(999).builds.is_empty());
    for bad in [0, -1, 9_007_199_254_740_992] {
        assert!(
            f.client
                .read_part(
                    Credential::Session(Secret::new("read-fixture-secret".into())),
                    bad,
                    &AtomicBool::new(false),
                    Duration::ZERO,
                )
                .is_err(),
            "{bad}"
        );
    }
    assert_eq!(before, f.dump());
    f.mutate("UPDATE parts SET tenant_id='foreign'");
    assert!(
        read(2).builds.is_empty(),
        "another tenant's Part is missing"
    );
}
