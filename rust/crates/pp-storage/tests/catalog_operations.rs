use pp_storage::{
    Limits, WriterOwner,
    auth::{FirstUserTenant, Outcome as AuthOutcome, Request as AuthRequest, Secret},
    catalog::{
        CreateSource, Credentials, Deletion, NamingCommand, NamingProfile, Outcome, Request,
        SourceCatalogClient, SourcePatch,
    },
};
use rusqlite::Connection;
use serde_json::{Value, json};
use std::os::unix::fs::DirBuilderExt;
use std::{
    path::{Path, PathBuf},
    sync::{Arc, atomic::AtomicBool},
    time::Duration,
};
struct Fixture(PathBuf);
impl Fixture {
    fn new() -> Self {
        let p = std::env::temp_dir().join(format!(
            "pp-catalog-{}",
            hex::encode(rand::random::<[u8; 16]>())
        ));
        std::fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(&p)
            .unwrap();
        Self(p)
    }
    fn open(&self) -> WriterOwner {
        WriterOwner::open(
            &self.0,
            Limits {
                queued_writes: 2,
                readers: 1,
            },
        )
        .unwrap()
        .0
    }
    fn sql(&self) -> Connection {
        Connection::open(self.0.join("print-partner.db")).unwrap()
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}
fn value(client: &SourceCatalogClient, r: Request) -> Value {
    serde_json::to_value(client.execute(r).unwrap()).unwrap()
}
fn create(client: &SourceCatalogClient, name: &str) -> i64 {
    value(
        client,
        Request::Create {
            source: CreateSource {
                name: name.into(),
                source_kind: Some("local".into()),
                ..Default::default()
            },
        },
    )["id"]
        .as_i64()
        .unwrap()
}
fn actor(owner: &WriterOwner, email: &str) -> (String, String) {
    let auth = owner.auth(FirstUserTenant::NewUser);
    let AuthOutcome::Session { user, token } = auth
        .submit(
            AuthRequest::Register {
                email: email.into(),
                display_name: "User".into(),
                password: Secret::new("strong-password".into()),
            },
            Arc::new(AtomicBool::new(false)),
            Duration::from_secs(5),
        )
        .unwrap()
        .recv()
        .unwrap()
        .unwrap()
    else {
        panic!("session")
    };
    (user.tenant_id, token.expose().into())
}
fn session(owner: &WriterOwner, token: &str) -> SourceCatalogClient {
    owner.source_catalog(Credentials::Session(Secret::new(token.into())))
}
fn rows(path: &Path) -> Value {
    let db = Connection::open_with_flags(
        path.join("print-partner.db"),
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
    )
    .unwrap();
    let names = db
        .prepare("SELECT name FROM sqlite_master WHERE type='table' ORDER BY name")
        .unwrap()
        .query_map([], |r| r.get::<_, String>(0))
        .unwrap()
        .collect::<rusqlite::Result<Vec<_>>>()
        .unwrap();
    let mut result = serde_json::Map::new();
    for name in names {
        let mut stmt = db
            .prepare(&format!("SELECT * FROM \"{}\"", name.replace('"', "\"\"")))
            .unwrap();
        let cols = stmt.column_count();
        let records = stmt
            .query_map([], |r| {
                let mut values = Vec::new();
                for i in 0..cols {
                    let v = match r.get_ref(i)? {
                        rusqlite::types::ValueRef::Null => Value::Null,
                        rusqlite::types::ValueRef::Integer(i) => json!(i),
                        rusqlite::types::ValueRef::Real(v) => json!(v),
                        rusqlite::types::ValueRef::Text(s) => json!(String::from_utf8_lossy(s)),
                        rusqlite::types::ValueRef::Blob(b) => json!(hex::encode(b)),
                    };
                    values.push(v);
                }
                Ok(json!(values))
            })
            .unwrap()
            .collect::<rusqlite::Result<Vec<_>>>()
            .unwrap();
        result.insert(name, json!(records));
    }
    json!(result)
}
#[test]
fn ticket_t_59_catalog_crud_owner_restart_and_cancelled_lost_reply() {
    let fixture = Fixture::new();
    let owner = fixture.open();
    let client = owner.local_source_catalog();
    let id = create(&client, " B ");
    let a = create(&client, "A");
    assert_eq!(value(&client, Request::List {})[0]["id"], a);
    assert!(
        client
            .submit(
                Request::Delete { id },
                &AtomicBool::new(true),
                Duration::ZERO
            )
            .is_err()
    );
    let patch = SourcePatch {metadata:Some(serde_json::from_value(json!({"custom_name":"Friendly","category":" A / B ","sync_error":"bad","remote_checked_at":"then"})).unwrap()),..Default::default()};
    value(&client, Request::Update { id, patch });
    let patch = SourcePatch {
        url: Some("https://example.invalid/new".into()),
        ..Default::default()
    };
    let changed = value(&client, Request::Update { id, patch });
    assert_eq!(changed["metadata"]["custom_name"], "Friendly");
    assert_eq!(changed["metadata"]["category"], "A/B");
    assert_eq!(changed["update_status"], "unknown");
    assert!(changed["metadata"].get("sync_error").is_none());
    assert!(changed["metadata"].get("remote_checked_at").is_none());
    drop(
        client
            .submit(
                Request::Create {
                    source: CreateSource {
                        name: "Lost reply".into(),
                        ..Default::default()
                    },
                },
                &AtomicBool::new(false),
                Duration::from_secs(5),
            )
            .unwrap(),
    );
    owner.shutdown().unwrap();
    let owner = fixture.open();
    let client = owner.local_source_catalog();
    assert_eq!(
        value(&client, Request::List {}).as_array().unwrap().len(),
        3
    );
    assert_eq!(value(&client, Request::Get { id })["name"], "B");
    assert_eq!(value(&client, Request::Delete { id })["kind"], "deleted");
    assert_eq!(value(&client, Request::Get { id }), Value::Null);
    assert_eq!(value(&client, Request::Delete { id })["kind"], "not_found");
    owner.shutdown().unwrap();
}
#[test]
fn ticket_t_59_catalog_session_key_owner_isolation_and_revocation() {
    let fixture = Fixture::new();
    let owner = fixture.open();
    let (tenant_a, token_a) = actor(&owner, "a@example.com");
    let (_, token_b) = actor(&owner, "b@example.com");
    let a = session(&owner, &token_a);
    let b = session(&owner, &token_b);
    let local = owner.local_source_catalog();
    let owner_id = create(&local, "Local owner");
    let id = create(&a, "A source");
    assert_eq!(value(&b, Request::Get { id }), Value::Null);
    assert_eq!(value(&a, Request::Get { id: owner_id }), Value::Null);
    assert_eq!(value(&local, Request::Get { id }), Value::Null);
    let auth = owner.auth(FirstUserTenant::NewUser);
    let AuthOutcome::KeyCreated { info, key } = auth
        .submit(
            AuthRequest::CreateKey {
                session: Secret::new(token_a.clone()),
            },
            Arc::new(AtomicBool::new(false)),
            Duration::from_secs(5),
        )
        .unwrap()
        .recv()
        .unwrap()
        .unwrap()
    else {
        panic!("key")
    };
    let key_client = owner.source_catalog(Credentials::Key {
        tenant_id: tenant_a,
        key: Secret::new(key.expose().into()),
    });
    assert_eq!(value(&key_client, Request::Get { id })["id"], id);
    let wrong = owner.source_catalog(Credentials::Key {
        tenant_id: "default".into(),
        key: Secret::new(key.expose().into()),
    });
    assert!(wrong.execute(Request::List {}).is_err());
    auth.submit(
        AuthRequest::RevokeKey {
            session: Secret::new(token_a.clone()),
            key_id: info.id,
        },
        Arc::new(AtomicBool::new(false)),
        Duration::from_secs(5),
    )
    .unwrap()
    .recv()
    .unwrap()
    .unwrap();
    assert!(key_client.execute(Request::List {}).is_err());
    auth.submit(
        AuthRequest::Logout {
            token: Secret::new(token_a),
        },
        Arc::new(AtomicBool::new(false)),
        Duration::from_secs(5),
    )
    .unwrap()
    .recv()
    .unwrap()
    .unwrap();
    assert!(
        a.execute(Request::Create {
            source: CreateSource {
                name: "revoked".into(),
                ..Default::default()
            }
        })
        .is_err()
    );
    assert_eq!(value(&local, Request::List {}).as_array().unwrap().len(), 1);
    owner.shutdown().unwrap();
}
#[test]
fn ticket_t_59_catalog_work_delete_ordering() {
    let fixture = Fixture::new();
    let owner = fixture.open();
    let client = owner.local_source_catalog();
    let id = create(&client, "Active");
    let mut lease = client
        .begin_work(id, &AtomicBool::new(false), Duration::ZERO)
        .unwrap();
    assert_eq!(
        value(&client, Request::Delete { id })["kind"],
        "active_work"
    );
    lease.release().unwrap();
    assert_eq!(value(&client, Request::Delete { id })["kind"], "deleted");
    assert!(
        client
            .begin_work(id, &AtomicBool::new(false), Duration::ZERO)
            .is_err()
    );
    let id = create(&client, "Dropped lease");
    let lease = client
        .begin_work(id, &AtomicBool::new(false), Duration::ZERO)
        .unwrap();
    drop(lease);
    assert_eq!(value(&client, Request::Delete { id })["kind"], "deleted");
    assert!(
        client
            .begin_work(id, &AtomicBool::new(true), Duration::ZERO)
            .is_err()
    );
    owner.shutdown().unwrap();
}
#[test]
fn gre601_duplicate_direct_source_lease_preserves_typed_catalog_context() {
    let fixture = Fixture::new();
    let owner = fixture.open();
    let client = owner.local_source_catalog();
    let id = create(&client, "Typed busy context");
    let active = client
        .begin_work(id, &AtomicBool::new(false), Duration::from_secs(5))
        .unwrap();
    let duplicate = client.begin_work(id, &AtomicBool::new(false), Duration::from_secs(5));
    #[cfg(feature = "desktop_rebuild")]
    {
        let error = duplicate
            .err()
            .expect("duplicate direct Source lease accepted");
        assert!(
            error
                .downcast_ref::<pp_storage::catalog::SourceBusy>()
                .is_some()
        );
        assert_eq!(error.to_string(), "Source work is already active");
    }
    #[cfg(not(feature = "desktop_rebuild"))]
    {
        assert!(duplicate.is_ok());
    }
    drop(active);
    owner.shutdown().unwrap();
}
#[test]
fn ticket_t_59_catalog_categories_naming_and_import_invalidation() {
    let fixture = Fixture::new();
    let owner = fixture.open();
    let client = owner.local_source_catalog();
    let id = create(&client, "Metadata");
    assert_eq!(
        value(&client, Request::GetImportRules { id }),
        json!({"rules":[],"legacy_import_all":true})
    );
    value(
        &client,
        Request::SaveCategories {
            categories: vec![" Printers / Frame ".into(), "Mods".into()],
            replacements: Default::default(),
        },
    );
    assert_eq!(
        value(&client, Request::GetCategories {}),
        json!(["Printers", "Printers/Frame", "Mods"])
    );
    assert_eq!(
        value(&client, Request::GetCategoryTree {})[0]["children"][0]["path"],
        "Printers/Frame"
    );
    let bulk = value(
        &client,
        Request::BulkCategory {
            source_ids: vec![id, id, 999],
            category: Some(" Printers / Frame ".into()),
        },
    );
    assert_eq!(bulk["succeeded"], 1);
    assert_eq!(bulk["failed"], 1);
    value(
        &client,
        Request::SaveCategories {
            categories: vec!["Machine/Frame".into(), "Mods".into()],
            replacements: std::collections::HashMap::from([(
                "Printers".into(),
                Some("Machine".into()),
            )]),
        },
    );
    assert_eq!(
        value(&client, Request::Get { id })["category"],
        "Machine/Frame"
    );
    let mut profile = NamingProfile::default();
    profile.roles[0].label = "Custom".into();
    profile.quantity.regex = r"(?<=x)([0-9]+)\.stl$".into();
    value(
        &client,
        Request::SaveNaming {
            id,
            settings: NamingCommand::Override { profile },
        },
    );
    let naming = value(&client, Request::GetNaming { id });
    assert_eq!(naming["effective"]["roles"][0]["label"], "Custom");
    assert_eq!(naming["effective_digest"].as_str().unwrap().len(), 64);
    value(
        &client,
        Request::SaveNaming {
            id,
            settings: NamingCommand::UseDefaults,
        },
    );
    assert_eq!(
        value(&client, Request::GetNaming { id })["use_defaults"],
        true
    );
    owner.shutdown().unwrap();
    fixture.sql().execute_batch(&format!("INSERT INTO build_profiles(id,tenant_id,name) VALUES(1,'default','Uses source'),(2,'other','Other tenant'); INSERT INTO profile_layers(tenant_id,profile_id,layer_type,project_id) VALUES('default',1,'base',{id}),('other',2,'addon',{id});")).unwrap();
    let owner = fixture.open();
    let client = owner.local_source_catalog();
    assert_eq!(
        value(
            &client,
            Request::SaveImportRules {
                id,
                rules: vec![" /STLs ".into(), "\\STLs".into(), "a.STL".into(), "".into()]
            }
        ),
        json!({"rules":["STLs/","a.STL"]})
    );
    assert_eq!(
        value(&client, Request::GetImportRules { id })["legacy_import_all"],
        false
    );
    owner.shutdown().unwrap();
    let db = fixture.sql();
    let timestamps = db
        .prepare("SELECT config_modified_at FROM build_profiles ORDER BY id")
        .unwrap()
        .query_map([], |r| r.get::<_, Option<String>>(0))
        .unwrap()
        .collect::<rusqlite::Result<Vec<_>>>()
        .unwrap();
    assert!(timestamps[0].is_some());
    assert!(timestamps[1].is_none());
}
#[test]
fn ticket_t_59_catalog_rejections_preserve_rows_and_import_transaction() {
    let fixture = Fixture::new();
    let owner = fixture.open();
    let client = owner.local_source_catalog();
    let id = create(&client, "Rollback");
    owner.shutdown().unwrap();
    let db = fixture.sql();
    db.execute_batch(&format!("INSERT INTO build_profiles(id,tenant_id,name) VALUES(1,'default','Build'); INSERT INTO profile_layers(tenant_id,profile_id,layer_type,project_id) VALUES('default',1,'base',{id}); CREATE TRIGGER reject_config BEFORE UPDATE ON build_profiles BEGIN SELECT RAISE(ABORT,'fixture rejects invalidation'); END;")).unwrap();
    drop(db);
    let owner = fixture.open();
    let before = rows(&fixture.0);
    let client = owner.local_source_catalog();
    assert!(
        client
            .execute(Request::SaveImportRules {
                id,
                rules: vec!["Models".into()]
            })
            .is_err()
    );
    assert!(
        client
            .execute(Request::SaveCategories {
                categories: vec![],
                replacements: Default::default()
            })
            .is_err()
    );
    assert_eq!(value(&client, Request::Delete { id })["kind"], "referenced");
    assert_eq!(rows(&fixture.0), before);
    owner.shutdown().unwrap();
}
#[test]
fn ticket_t_59_catalog_immutable_and_reference_graphs() {
    let fixture = Fixture::new();
    let owner = fixture.open();
    let client = owner.local_source_catalog();
    for n in [
        "History",
        "Direct",
        "Accepted input",
        "Draft input",
        "Legacy part",
        "Snapshot",
    ] {
        create(&client, n);
    }
    owner.shutdown().unwrap();
    let db = fixture.sql();
    db.execute_batch("PRAGMA foreign_keys=ON; INSERT INTO build_profiles(id,tenant_id,name) VALUES(1,'default','Graph'); INSERT INTO source_revisions(tenant_id,project_id,upstream_revision_key,manifest_digest,snapshot_locator,synced_at) VALUES('default',1,'immutable-key','digest','1/revisions/key','2026-01-01'); INSERT INTO profile_layers(tenant_id,profile_id,layer_type,project_id) VALUES('default',1,'base',2); INSERT INTO plan_revision_input_sets(id,tenant_id,profile_id,input_set_digest,expected_input_count,recorded_at,published_at) VALUES(1,'default',1,'inputs',1,'then','then'); INSERT INTO plan_revision_inputs(tenant_id,input_set_id,source_id,source_layer,tracking_kind) VALUES('default',1,3,'base:Accepted input','untracked'); INSERT INTO plan_drafts(id,tenant_id,profile_id,base_plan_version,state,digest_format,snapshot_digest,created_by,idempotency_key,created_at) VALUES(1,'default',1,0,'open','v1','digest','fixture','key','then'); INSERT INTO plan_draft_inputs(tenant_id,draft_id,source_id,source_layer,layer_order,tracking_kind,effective_naming_digest) VALUES('default',1,4,'base:Draft input',0,'untracked','naming'); INSERT INTO parts(tenant_id,profile_id,match_key,filename,source_layer) VALUES('default',1,'part','file.stl','base:Legacy part'); INSERT INTO plan_snapshots(tenant_id,profile_id,name,created_at,payload_json) VALUES('default',1,'Snapshot','then','{\"layers\":[{\"project_id\":6,\"source_name\":\"Snapshot\"}]}');").unwrap();
    drop(db);
    let owner = fixture.open();
    let before = rows(&fixture.0);
    let client = owner.local_source_catalog();
    for id in 1..=6 {
        let expected = if id == 1 {
            "retained_history"
        } else {
            "referenced"
        };
        assert_eq!(
            value(&client, Request::Delete { id })["kind"],
            expected,
            "source {id}"
        );
    }
    assert_eq!(rows(&fixture.0), before);
    owner.shutdown().unwrap();
}
#[test]
fn ticket_t_59_catalog_no_payload_paths_or_forged_authority() {
    for payload in [
        json!({"operation":"create","source":{"name":"Bad","local_path":"/etc"}}),
        json!({"operation":"update","id":1,"patch":{"localPath":"/etc"}}),
        json!({"operation":"list","owner":true}),
        json!({"operation":"list","tenant":"default"}),
    ] {
        assert!(serde_json::from_value::<Request>(payload).is_err());
    }
}
#[test]
fn ticket_t_59_catalog_work_races_delete_through_shared_writer() {
    let fixture = Fixture::new();
    let owner = fixture.open();
    let client = Arc::new(owner.local_source_catalog());
    let id = create(&client, "Race");
    let a = client.clone();
    let b = client.clone();
    let (work, deletion) = std::thread::scope(|scope| {
        let w =
            scope.spawn(move || a.begin_work(id, &AtomicBool::new(false), Duration::from_secs(5)));
        let d = scope.spawn(move || b.execute(Request::Delete { id }));
        (w.join().unwrap(), d.join().unwrap().unwrap())
    });
    match deletion {
        Outcome::Deletion(Deletion::Deleted { .. }) => assert!(work.is_err()),
        Outcome::Deletion(Deletion::ActiveWork) => work.unwrap().release().unwrap(),
        _ => panic!("unexpected deletion"),
    };
    owner.shutdown().unwrap();
}

#[test]
fn ticket_t_59_catalog_deletion_retains_files_and_counts_docs() {
    let fixture = Fixture::new();
    let owner = fixture.open();
    let client = owner.local_source_catalog();
    let id = create(&client, "Docs");
    let source = value(&client, Request::Get { id });
    let path = PathBuf::from(source["local_path"].as_str().unwrap());
    std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(&path)
        .unwrap();
    std::fs::write(path.join("part.stl"), b"retained").unwrap();
    owner.shutdown().unwrap();
    fixture.sql().execute("INSERT INTO source_docs(tenant_id,project_id,path,kind,updated_at) VALUES('default',?1,'readme.md','markdown','then')",[id]).unwrap();
    let owner = fixture.open();
    let client = owner.local_source_catalog();
    assert_eq!(value(&client, Request::Get { id })["doc_count"], 1);
    assert_eq!(value(&client, Request::Delete { id })["kind"], "deleted");
    assert_eq!(std::fs::read(path.join("part.stl")).unwrap(), b"retained");
    owner.shutdown().unwrap();
    assert_eq!(
        fixture
            .sql()
            .query_row("SELECT count(*) FROM source_docs", [], |r| r
                .get::<_, i64>(0))
            .unwrap(),
        0
    );
}

#[test]
fn ticket_t_59_catalog_metadata_rejection_and_legacy_bytes() {
    let fixture = Fixture::new();
    let owner = fixture.open();
    let client = owner.local_source_catalog();
    let id = create(&client, "Legacy");
    owner.shutdown().unwrap();
    fixture
        .sql()
        .execute(
            "UPDATE projects SET metadata_json='not-json',source_kind='' WHERE id=?1",
            [id],
        )
        .unwrap();
    let owner = fixture.open();
    let client = owner.local_source_catalog();
    value(
        &client,
        Request::Update {
            id,
            patch: SourcePatch {
                name: Some("Changed".into()),
                ..Default::default()
            },
        },
    );
    let mut invalid = NamingProfile::default();
    invalid.quantity.regex = "(bad)(two)".into();
    let before = rows(&fixture.0);
    assert!(
        client
            .execute(Request::SaveNaming {
                id,
                settings: NamingCommand::Override { profile: invalid }
            })
            .is_err()
    );
    assert_eq!(rows(&fixture.0), before);
    owner.shutdown().unwrap();
    let db = fixture.sql();
    assert_eq!(
        db.query_row(
            "SELECT metadata_json,source_kind FROM projects WHERE id=?1",
            [id],
            |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
        )
        .unwrap(),
        ("not-json".into(), "".into())
    );
}

#[test]
fn ticket_t_59_catalog_public_backpressure() {
    let fixture = Fixture::new();
    let owner = fixture.open();
    let client = owner.local_source_catalog();
    let mut admitted = Vec::new();
    let mut rejected = false;
    for i in 0..1000 {
        match client.submit(
            Request::Create {
                source: CreateSource {
                    name: format!("Queued {i}"),
                    ..Default::default()
                },
            },
            &AtomicBool::new(false),
            Duration::ZERO,
        ) {
            Ok(reply) => admitted.push(reply),
            Err(error) => {
                assert_eq!(error.to_string(), "Writer queue full");
                rejected = true;
                break;
            }
        }
    }
    assert!(rejected);
    let count = admitted.len();
    for reply in admitted {
        reply.recv().unwrap().unwrap();
    }
    assert_eq!(
        value(&client, Request::List {}).as_array().unwrap().len(),
        count
    );
    owner.shutdown().unwrap();
}

#[test]
fn ticket_t_59_catalog_build_json_and_scoped_note_references() {
    let fixture = Fixture::new();
    let owner = fixture.open();
    let client = owner.local_source_catalog();
    for name in ["Evidence", "Managed", "Pinned", "Decision", "Scoped note"] {
        create(&client, name);
    }
    owner.shutdown().unwrap();
    fixture.sql().execute_batch("INSERT INTO build_profiles(id,tenant_id,name) VALUES(1,'default','Build'); INSERT INTO app_settings(tenant_id,key,value) VALUES('default','build_planning.v1.1','{\"evidence\":[{\"source_id\":1}],\"managed_source_ids\":[2],\"draft_source_revisions\":{\"3\":\"revision\"}}'); INSERT INTO plan_decisions(tenant_id,profile_id,created_at,kind,params_json) VALUES('default',1,'then','applied_action','{\"inputs\":[{\"source_id\":4}]}'); INSERT INTO source_notes(tenant_id,project_id,profile_id,created_at,updated_at) VALUES('default',5,1,'then','then');").unwrap();
    let owner = fixture.open();
    let before = rows(&fixture.0);
    let client = owner.local_source_catalog();
    for id in 1..=5 {
        assert_eq!(value(&client, Request::Delete { id })["kind"], "referenced");
    }
    assert_eq!(rows(&fixture.0), before);
    owner.shutdown().unwrap();
}

fn database_bytes(path: &Path) -> Vec<Option<Vec<u8>>> {
    ["print-partner.db", "print-partner.db-wal"]
        .map(|name| std::fs::read(path.join(name)).ok())
        .into()
}

#[test]
fn catalog_rename_requires_a_name_and_preserves_unique_index_enforcement() {
    let fixture = Fixture::new();
    let owner = fixture.open();
    let client = owner.local_source_catalog();
    let first = create(&client, "First");
    create(&client, "Second");

    let error = client
        .execute(Request::Update {
            id: first,
            patch: SourcePatch {
                name: Some(" \u{2003} ".into()),
                ..Default::default()
            },
        })
        .unwrap_err();
    assert_eq!(error.to_string(), "Source name is required");

    assert!(
        client
            .execute(Request::Update {
                id: first,
                patch: SourcePatch {
                    name: Some("Second".into()),
                    ..Default::default()
                },
            })
            .is_err()
    );
    assert_eq!(value(&client, Request::Get { id: first })["name"], "First");
    owner.shutdown().unwrap();
}

#[test]
fn ticket_t_59_catalog_legacy_names_cannot_be_renamed_out_of_history() {
    let fixture = Fixture::new();
    let owner = fixture.open();
    let client = owner.local_source_catalog();
    for name in [
        "Legacy",
        "Accepted",
        "Draft",
        "Snapshot",
        "Modern",
        "Empty",
        "Ångström",
    ] {
        create(&client, name);
    }
    owner.shutdown().unwrap();
    fixture.sql().execute_batch("PRAGMA foreign_keys=ON;
        INSERT INTO build_profiles(id,tenant_id,name) VALUES(1,'default','Build');
        INSERT INTO parts(tenant_id,profile_id,match_key,filename,source_layer) VALUES('default',1,'p','a.stl','base:legacy');
        INSERT INTO parts(tenant_id,profile_id,match_key,filename,source_layer) VALUES('default',1,'unicode','b.stl','base:ångström');
        INSERT INTO plan_revisions(id,tenant_id,profile_id,revision_number,provenance_kind,digest_format,snapshot_digest,created_by,accepted_by,created_at,accepted_at) VALUES(1,'default',1,1,'legacy','v1','digest','user','user','then','then');
        INSERT INTO plan_revision_parts(tenant_id,revision_id,part_key,source_layer) VALUES('default',1,'accepted','addon:ACCEPTED');
        INSERT INTO plan_drafts(id,tenant_id,profile_id,base_plan_version,state,digest_format,snapshot_digest,created_by,idempotency_key,created_at) VALUES(1,'default',1,0,'open','v1','digest','fixture','key','then');
        INSERT INTO plan_draft_parts(tenant_id,draft_id,part_key,source_layer) VALUES('default',1,'draft','base:dRaFt');
        INSERT INTO plan_snapshots(tenant_id,profile_id,name,created_at,payload_json) VALUES('default',1,'Historical','then','{\"layers\":[{\"source_name\":\"snapshot\"}]}');
        INSERT INTO plan_revision_input_sets(id,tenant_id,profile_id,input_set_digest,expected_input_count,recorded_at,published_at) VALUES(1,'default',1,'inputs',1,'then','then');
        INSERT INTO plan_revision_inputs(tenant_id,input_set_id,source_id,source_layer,tracking_kind) VALUES('default',1,5,'base:Modern','untracked');
        INSERT INTO plan_revisions(id,tenant_id,profile_id,revision_number,input_set_id,provenance_kind,digest_format,snapshot_digest,created_by,accepted_by,created_at,accepted_at) VALUES(2,'default',1,2,1,'tracked','v1','modern-digest','user','user','then','then');").unwrap();
    let owner = fixture.open();
    let client = owner.local_source_catalog();
    let original = rows(&fixture.0);
    let bytes = database_bytes(&fixture.0);
    for id in [1, 2, 3, 4, 7] {
        let error = client
            .execute(Request::Update {
                id,
                patch: SourcePatch {
                    name: Some("Renamed".into()),
                    metadata: Some(
                        serde_json::from_value(json!({"aliases": ["untrusted"]})).unwrap(),
                    ),
                    ..Default::default()
                },
            })
            .unwrap_err();
        assert!(error.to_string().contains("historical name"));
        assert_eq!(value(&client, Request::Delete { id })["kind"], "referenced");
        assert_eq!(rows(&fixture.0), original);
        assert_eq!(database_bytes(&fixture.0), bytes);
    }
    value(
        &client,
        Request::Update {
            id: 1,
            patch: SourcePatch {
                name: Some(" Legacy ".into()),
                ..Default::default()
            },
        },
    );
    assert_eq!(
        value(
            &client,
            Request::Update {
                id: 5,
                patch: SourcePatch {
                    name: Some("Modern renamed".into()),
                    ..Default::default()
                }
            }
        )["name"],
        "Modern renamed"
    );
    assert_eq!(
        value(&client, Request::Delete { id: 5 })["kind"],
        "referenced"
    );
    value(
        &client,
        Request::Update {
            id: 6,
            patch: SourcePatch {
                name: Some("Empty renamed".into()),
                ..Default::default()
            },
        },
    );
    assert_eq!(value(&client, Request::Delete { id: 6 })["kind"], "deleted");
    owner.shutdown().unwrap();
    let owner = fixture.open();
    assert_eq!(
        value(&owner.local_source_catalog(), Request::Get { id: 1 })["name"],
        "Legacy"
    );
    owner.shutdown().unwrap();
}

#[test]
fn ticket_t_59_catalog_regex_complexity_rejection_preserves_writer_and_settings() {
    let fixture = Fixture::new();
    let owner = fixture.open();
    let client = owner.local_source_catalog();
    let id = create(&client, "Naming");
    let original = rows(&fixture.0);
    let bytes = database_bytes(&fixture.0);
    let nested = format!("{}(x){}", "(?:".repeat(16), ")".repeat(16));
    let groups = format!("(x){}", "(?:x)".repeat(64));
    for pattern in [&nested, &groups, "(x", r"(x)[abc", "(x)\\"] {
        for source in [false, true] {
            let mut profile = NamingProfile::default();
            profile.quantity.regex = pattern.into();
            let request = if source {
                Request::SaveNaming {
                    id,
                    settings: NamingCommand::Override { profile },
                }
            } else {
                Request::SaveGlobalNaming { profile }
            };
            assert!(client.execute(request).is_err(), "{pattern}");
            assert_eq!(value(&client, Request::Get { id })["name"], "Naming");
            assert_eq!(rows(&fixture.0), original);
            assert_eq!(database_bytes(&fixture.0), bytes);
        }
    }
    for pattern in [
        r"(?<=x)([0-9]+)\.stl$",
        r"(?=x)(x)\1",
        r"[(](x)[)]",
        r"\((x)\)",
        r"[\](](x)",
    ] {
        let mut profile = NamingProfile::default();
        profile.quantity.regex = pattern.into();
        value(
            &client,
            Request::SaveNaming {
                id,
                settings: NamingCommand::Override { profile },
            },
        );
        assert_eq!(
            value(&client, Request::GetNaming { id })["effective"]["quantity"]["regex"],
            pattern
        );
    }
    for pattern in [
        format!("{}(x){}", "(?:".repeat(15), ")".repeat(15)),
        format!("(x){}", "(?:x)".repeat(63)),
    ] {
        let mut profile = NamingProfile::default();
        profile.quantity.regex = pattern.clone();
        value(
            &client,
            Request::SaveNaming {
                id,
                settings: NamingCommand::Override { profile },
            },
        );
        assert_eq!(
            value(&client, Request::GetNaming { id })["effective"]["quantity"]["regex"],
            pattern
        );
    }
    owner.shutdown().unwrap();
    let mut stored = serde_json::to_value(NamingProfile::default()).unwrap();
    stored["quantity"]["regex"] = json!(nested);
    let db = fixture.sql();
    db.execute("INSERT INTO app_settings(tenant_id,key,value) VALUES('default','stl_naming_defaults',?1) ON CONFLICT(tenant_id,key) DO UPDATE SET value=excluded.value", [stored.to_string()]).unwrap();
    db.execute(
        "UPDATE projects SET metadata_json=?1 WHERE id=?2",
        rusqlite::params![
            json!({"naming":{"use_defaults":false,"override":stored}}).to_string(),
            id
        ],
    )
    .unwrap();
    drop(db);
    let owner = fixture.open();
    let client = owner.local_source_catalog();
    let original = rows(&fixture.0);
    let bytes = database_bytes(&fixture.0);
    assert!(client.execute(Request::GetNaming { id }).is_err());
    assert_eq!(
        value(&client, Request::GetGlobalNaming {})["quantity"]["regex"],
        NamingProfile::default().quantity.regex
    );
    assert_eq!(value(&client, Request::Get { id })["name"], "Naming");
    assert_eq!(rows(&fixture.0), original);
    assert_eq!(database_bytes(&fixture.0), bytes);
    owner.shutdown().unwrap();
}

#[test]
fn catalog_key_audit_commits_protected_deletion_outcomes_and_rolls_back_errors() {
    for case in [
        "not_found",
        "active_work",
        "retained_history",
        "referenced",
        "invalid_update",
    ] {
        let fixture = Fixture::new();
        let owner = fixture.open();
        let (tenant, token) = actor(&owner, "audit@example.test");
        let client = session(&owner, &token);
        let id = create(&client, "Protected");
        let before = value(&client, Request::Get { id });
        let mut lease = None;
        match case {
            "active_work" => {
                lease = Some(
                    client
                        .begin_work(id, &AtomicBool::new(false), Duration::from_secs(5))
                        .unwrap(),
                );
            }
            "retained_history" => {
                fixture.sql().execute(
                    "INSERT INTO source_revisions(tenant_id,project_id,upstream_revision_key,manifest_digest,snapshot_locator,synced_at) VALUES(?1,?2,'audit-revision',?3,?4,'2026-01-01T00:00:00.000Z')",
                    rusqlite::params![tenant, id, "a".repeat(64), format!("{id}/revisions/audit")],
                ).unwrap();
            }
            "referenced" => {
                let db = fixture.sql();
                db.execute(
                    "INSERT INTO build_profiles(tenant_id,name) VALUES(?1,'References Source')",
                    [&tenant],
                )
                .unwrap();
                let profile = db.last_insert_rowid();
                db.execute(
                    "INSERT INTO profile_layers(tenant_id,profile_id,layer_type,project_id) VALUES(?1,?2,'base',?3)",
                    rusqlite::params![tenant, profile, id],
                )
                .unwrap();
            }
            _ => {}
        }
        let auth = owner.auth(FirstUserTenant::NewUser);
        let AuthOutcome::KeyCreated { info, key } = auth
            .submit(
                AuthRequest::CreateKey {
                    session: Secret::new(token.clone()),
                },
                Arc::new(AtomicBool::new(false)),
                Duration::from_secs(5),
            )
            .unwrap()
            .recv()
            .unwrap()
            .unwrap()
        else {
            panic!("key")
        };
        assert!(info.last_used_at.is_none());
        let keyed = owner.source_catalog(Credentials::Key {
            tenant_id: tenant,
            key,
        });
        if case == "invalid_update" {
            assert!(
                keyed
                    .execute(Request::Update {
                        id,
                        patch: SourcePatch {
                            name: Some(" ".into()),
                            ..Default::default()
                        },
                    })
                    .is_err()
            );
        } else {
            let target = if case == "not_found" { id + 1000 } else { id };
            assert_eq!(value(&keyed, Request::Delete { id: target })["kind"], case);
        }
        assert_eq!(value(&client, Request::Get { id }), before);
        let AuthOutcome::Keys { keys, .. } = auth
            .submit(
                AuthRequest::ListKeys {
                    session: Secret::new(token),
                },
                Arc::new(AtomicBool::new(false)),
                Duration::from_secs(5),
            )
            .unwrap()
            .recv()
            .unwrap()
            .unwrap()
        else {
            panic!("keys")
        };
        let used = keys.into_iter().find(|key| key.id == info.id).unwrap();
        assert_eq!(
            used.last_used_at.is_some(),
            case != "invalid_update",
            "{case}"
        );
        if let Some(lease) = &mut lease {
            lease.release().unwrap();
        }
        owner.shutdown().unwrap();
    }
}

#[cfg(feature = "desktop_rebuild")]
fn rebuild_request(input: Value) -> Request {
    serde_json::from_value(input).unwrap()
}

#[cfg(feature = "desktop_rebuild")]
#[test]
fn gre600_location_normalization_is_explicit_and_rejections_are_atomic() {
    let fixture = Fixture::new();
    let owner = fixture.open();
    let client = owner.local_source_catalog();
    let url = "git@github.com:Owner/Repo.git";
    let legacy = value(
        &client,
        Request::Create {
            source: CreateSource {
                name: "Legacy location".into(),
                url: Some(url.into()),
                ..Default::default()
            },
        },
    );
    assert_eq!(legacy["url"], url);
    let source = value(
        &client,
        rebuild_request(json!({"operation":"create_catalog_source", "source":{
            "name":"Normalized", "url":url
        }})),
    );
    assert_eq!(source["url"], "https://github.com/Owner/Repo");
    assert_eq!(source["branch"], "main");
    let id = source["id"].as_i64().unwrap();
    let source = value(
        &client,
        rebuild_request(
            json!({"operation":"update_catalog_source", "id":id, "patch":{
                "url":"https://github.com/Owner/Repo/tree/feature/topic/models", "branch":"feature/topic"
            }}),
        ),
    );
    assert_eq!(source["url"], "https://github.com/Owner/Repo");
    assert_eq!(source["branch"], "feature/topic");
    assert_eq!(source["metadata"]["sync_required"], true);
    let before = rows(&fixture.0);
    for kind in ["printables", "makerworld", "thangs"] {
        assert!(
            client
                .execute(rebuild_request(
                    json!({"operation":"create_catalog_source", "source":{
                        "name":"Missing URL", "source_kind":kind
                    }})
                ))
                .unwrap_err()
                .to_string()
                .contains("model URL is required")
        );
        assert_eq!(rows(&fixture.0), before);
    }
    owner.shutdown().unwrap();
    let owner = fixture.open();
    assert_eq!(
        value(&owner.local_source_catalog(), Request::Get { id })["branch"],
        "feature/topic"
    );
    owner.shutdown().unwrap();
}

#[cfg(feature = "desktop_rebuild")]
#[test]
fn gre600_categories_return_tree_and_preserve_numeric_selector_results() {
    let fixture = Fixture::new();
    let owner = fixture.open();
    let client = owner.local_source_catalog();
    let id = create(&client, "Selected");
    let saved = value(
        &client,
        rebuild_request(json!({"operation":"save_categories_with_tree",
            "categories":["Parts / Mechanical", "Parts/Electrical"], "replacements":{}
        })),
    );
    assert_eq!(
        saved["categories"],
        json!(["Parts", "Parts/Mechanical", "Parts/Electrical"])
    );
    assert_eq!(saved["tree"][0]["children"][0]["parent"], "Parts");
    assert_eq!(
        value(
            &client,
            rebuild_request(json!({"operation":"get_categories_with_tree"}))
        ),
        saved
    );
    let result = value(
        &client,
        rebuild_request(json!({"operation":"apply_numeric_category", "command":{
            "selectors":[id,id,1.5,9223372036854775808.0], "category":"Parts / Mechanical"
        }})),
    );
    assert_eq!(result["succeeded"], 1);
    assert_eq!(result["failed"], 2);
    assert_eq!(result["updated"][0]["category"], "Parts/Mechanical");
    assert_eq!(
        result["results"][1],
        json!({"source_id":1.5,"ok":false,"detail":"Source not found"})
    );
    let before = rows(&fixture.0);
    assert!(
        client
            .execute(rebuild_request(
                json!({"operation":"save_categories_with_tree",
                    "categories":["Other"], "replacements":{"Parts":"Not saved"}
                })
            ))
            .is_err()
    );
    assert_eq!(rows(&fixture.0), before);
    owner.shutdown().unwrap();
}

#[cfg(feature = "desktop_rebuild")]
#[test]
fn gre600_activity_is_tenant_scoped_bounded_and_tolerates_bad_payloads() {
    let fixture = Fixture::new();
    let owner = fixture.open();
    let (tenant, token) = actor(&owner, "activity@example.test");
    let db = fixture.sql();
    for (scope, kind, payload) in [
        (
            tenant.as_str(),
            "source.updated",
            r#"{"source_id":42,"source_name":"Mine"}"#,
        ),
        (tenant.as_str(), "source.sync_failed", "bad-json"),
        (tenant.as_str(), "unrelated", "{}"),
        ("foreign", "source.updated", r#"{"source_name":"Foreign"}"#),
    ] {
        db.execute(
            "INSERT INTO app_events(tenant_id,at,kind,payload_json) VALUES(?1,'then',?2,?3)",
            rusqlite::params![scope, kind, payload],
        )
        .unwrap();
    }
    drop(db);
    let client = session(&owner, &token);
    let events = value(
        &client,
        rebuild_request(json!({"operation":"source_activity","limit":100})),
    );
    assert_eq!(events.as_array().unwrap().len(), 2);
    assert_eq!(events[0]["source_name"], "Source");
    assert_eq!(events[0]["source_id"], Value::Null);
    assert_eq!(events[1]["source_name"], "Mine");
    assert_eq!(events[1]["source_id"].as_f64(), Some(42.0));
    assert_eq!(
        value(
            &client,
            rebuild_request(json!({"operation":"source_activity","limit":0}))
        )
        .as_array()
        .unwrap()
        .len(),
        1
    );
    owner.shutdown().unwrap();
}

#[cfg(feature = "desktop_rebuild")]
#[test]
fn gre600_import_rule_budget_accepts_large_rules_and_rejects_over_limit() {
    let fixture = Fixture::new();
    let owner = fixture.open();
    let client = owner.local_source_catalog();
    let id = create(&client, "Large rules");
    let rule = format!("{}.stl", "a".repeat(1024 * 1024));
    let result = value(
        &client,
        Request::SaveImportRules {
            id,
            rules: vec![rule.clone()],
        },
    );
    assert_eq!(result["rules"][0], rule);
    let before = rows(&fixture.0);
    assert!(
        client
            .execute(Request::SaveImportRules {
                id,
                rules: vec!["a".repeat(8 * 1024 * 1024)]
            })
            .unwrap_err()
            .to_string()
            .contains("too large")
    );
    assert_eq!(rows(&fixture.0), before);
    owner.shutdown().unwrap();
}

#[cfg(not(feature = "desktop_rebuild"))]
#[test]
fn gre600_default_catalog_rejects_rebuild_operations() {
    for operation in [
        "create_catalog_source",
        "update_catalog_source",
        "source_activity",
        "get_categories_with_tree",
        "save_categories_with_tree",
        "apply_numeric_category",
    ] {
        let error = serde_json::from_value::<Request>(json!({"operation":operation}))
            .err()
            .unwrap();
        assert!(error.to_string().contains("unknown variant"));
    }
}

#[cfg(not(feature = "desktop_rebuild"))]
#[test]
fn catalog_metadata_token_is_ignored_without_desktop_rebuild() {
    let fixture = Fixture::new();
    let owner = fixture.open();
    let client = owner.local_source_catalog();
    let id = create(&client, "Tokenless");
    let updated = value(
        &client,
        Request::Update {
            id,
            patch: SourcePatch {
                metadata: Some(
                    serde_json::from_value(json!({"custom_name": "Flag off rename"})).unwrap(),
                ),
                metadata_version: Some(4),
                ..Default::default()
            },
        },
    );
    assert_eq!(updated["metadata"]["custom_name"], "Flag off rename");
    assert!(updated.get("catalog_metadata_version").is_none());
    let raw: String = fixture
        .sql()
        .query_row(
            "SELECT metadata_json FROM projects WHERE id=?1",
            [id],
            |row| row.get(0),
        )
        .unwrap();
    let stored: Value = serde_json::from_str(&raw).unwrap();
    assert_eq!(stored["custom_name"], "Flag off rename");
    assert!(stored.get("catalog_metadata_version").is_none());
    owner.shutdown().unwrap();
}

#[cfg(feature = "desktop_rebuild")]
#[test]
fn gre600_policy_access_and_numeric_mutations_reject_foreign_sources() {
    use pp_storage::auth::{AuthPolicy, RegistrationPolicy, SessionTenantPolicy};
    use pp_storage::catalog::{CatalogFailure, NumericCategoryCommand};
    let fixture = Fixture::new();
    let owner = fixture.open();
    let (tenant, token) = actor(&owner, "access@example.test");
    let policy = AuthPolicy {
        registration: RegistrationPolicy::Open,
        session_tenant: SessionTenantPolicy::AccountTenant,
        first_user: FirstUserTenant::NewUser,
    };
    let client = owner
        .catalog_access(policy)
        .unwrap()
        .session(Secret::new(token.clone()));
    let foreign_id = create(&owner.local_source_catalog(), "Foreign");
    let id = value(
        &client,
        rebuild_request(json!({"operation":"create_catalog_source", "source":{
            "name":"Owned", "source_kind":"local"
        }})),
    )["id"]
        .as_i64()
        .unwrap();
    let error = client
        .execute(rebuild_request(
            json!({"operation":"update_catalog_source", "id":foreign_id,
                "patch":{"name":"Stolen"}
            }),
        ))
        .unwrap_err();
    assert!(matches!(
        error.downcast_ref::<CatalogFailure>(),
        Some(CatalogFailure::NotFound)
    ));
    let duplicate = client
        .execute(rebuild_request(
            json!({"operation":"create_catalog_source", "source":{
                "name":"Owned", "source_kind":"local"
            }}),
        ))
        .unwrap_err();
    assert!(matches!(
        duplicate.downcast_ref::<CatalogFailure>(),
        Some(CatalogFailure::DuplicateName(_))
    ));
    let result = value(
        &client,
        Request::ApplyNumericCategory {
            command: NumericCategoryCommand::from_finite_numbers(
                [id as f64, foreign_id as f64],
                Some("Mine".into()),
            )
            .unwrap(),
        },
    );
    assert_eq!(result["succeeded"], 1);
    assert_eq!(result["failed"], 1);
    assert_ne!(
        value(
            &owner.local_source_catalog(),
            Request::Get { id: foreign_id }
        )["category"],
        "Mine"
    );
    for number in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
        assert!(NumericCategoryCommand::from_finite_numbers([number], None).is_err());
    }
    assert!(owner.catalog_key_access(policy, "x".repeat(512)).is_ok());
    for routed in [String::new(), "x".repeat(513), "x".repeat(4097)] {
        assert!(owner.catalog_key_access(policy, routed).is_err());
    }
    let auth = owner.auth(FirstUserTenant::NewUser);
    let AuthOutcome::KeyCreated { key, .. } = auth
        .submit(
            AuthRequest::CreateKey {
                session: Secret::new(token),
            },
            Arc::new(AtomicBool::new(false)),
            Duration::from_secs(5),
        )
        .unwrap()
        .recv()
        .unwrap()
        .unwrap()
    else {
        panic!("key")
    };
    let keyed = owner.catalog_key_access(policy, tenant).unwrap().key(key);
    assert_eq!(value(&keyed, Request::List {}).as_array().unwrap().len(), 1);
    owner.shutdown().unwrap();
}

#[cfg(feature = "desktop_rebuild")]
#[test]
fn gre600_r1_unsafe_locations_are_typed_and_atomic() {
    use pp_storage::catalog::CatalogFailure;
    let fixture = Fixture::new();
    let owner = fixture.open();
    let client = owner.local_source_catalog();
    let id = value(
        &client,
        Request::Create {
            source: CreateSource {
                name: "Unchanged".into(),
                ..Default::default()
            },
        },
    )["id"]
        .as_i64()
        .unwrap();
    let before = rows(&fixture.0);
    for url in [
        "https://github.com/Owner/Repo%23evil",
        "https://github.com/Owner/Repo%3Fevil",
        "https://github.com/foo%2Fbar/baz",
        "https://github.com/%2E%2E/evil",
        "https://github.com/Owner/%2E",
        "git@github.com:../evil",
        "git@github.com:Owner/Repo?evil",
        "git@github.com:Owner/Repo#evil",
        "git@github.com:Owner/..git",
    ] {
        for request in [
            json!({"operation":"create_catalog_source","source":{"name":"Unsafe","url":url}}),
            json!({"operation":"update_catalog_source","id":id,"patch":{"url":url}}),
        ] {
            let error = client.execute(rebuild_request(request)).unwrap_err();
            assert!(
                matches!(
                    error.downcast_ref::<CatalogFailure>(),
                    Some(CatalogFailure::Input(_))
                ),
                "{url}: {error}"
            );
            assert_eq!(rows(&fixture.0), before);
        }
    }
    let valid = value(
        &client,
        rebuild_request(
            json!({"operation":"update_catalog_source","id":id,"patch":{"url":"https://github.com/%4Fwner/Repo.git/tree/topic"}}),
        ),
    );
    assert_eq!(valid["url"], "https://github.com/Owner/Repo");
    assert_eq!(valid["branch"], "topic");
    owner.shutdown().unwrap();
}

#[cfg(feature = "desktop_rebuild")]
#[test]
fn gre600_r1_numeric_ids_and_activity_remain_exact() {
    use pp_storage::catalog::NumericCategoryCommand;
    let fixture = Fixture::new();
    let owner = fixture.open();
    let client = owner.local_source_catalog();
    let low = 9_007_199_254_740_992_i64;
    let high = low + 1;
    let adjacent = create(&client, "Adjacent");
    let exact = create(&client, "Exact");
    let db = fixture.sql();
    for (old, id) in [(adjacent, low), (exact, high)] {
        db.execute(
            "UPDATE projects SET id=?1 WHERE id=?2",
            rusqlite::params![id, old],
        )
        .unwrap();
    }
    db.execute("INSERT INTO app_events(tenant_id,at,kind,payload_json) VALUES('default','then','source.updated',?1)", [format!("{{\"source_id\":{high}}}")]).unwrap();
    drop(db);
    let result = value(
        &client,
        rebuild_request(
            json!({"operation":"apply_numeric_category","command":{"selectors":[high,1.5,high as f64],"category":"Exact"}}),
        ),
    );
    assert_eq!(result["succeeded"], 1);
    assert_eq!(result["failed"], 2);
    assert_eq!(result["results"][0]["source_id"].as_i64(), Some(high));
    assert_eq!(result["updated"][0]["id"].as_i64(), Some(high));
    assert_ne!(
        value(&client, Request::Get { id: low })["category"],
        "Exact"
    );
    let before = rows(&fixture.0);
    if let Ok(command) =
        NumericCategoryCommand::from_finite_numbers([high as f64], Some("Wrong".into()))
    {
        assert_eq!(
            value(&client, Request::ApplyNumericCategory { command })["succeeded"],
            0
        );
    }
    assert_eq!(rows(&fixture.0), before);
    let events = value(
        &client,
        rebuild_request(json!({"operation":"source_activity","limit":10})),
    );
    assert_eq!(events[0]["source_id"].as_i64(), Some(high));
    let float = value(
        &client,
        Request::ApplyNumericCategory {
            command: NumericCategoryCommand::from_finite_numbers([1.5], None).unwrap(),
        },
    );
    assert_eq!(float["results"][0]["source_id"], json!(1.5));
    owner.shutdown().unwrap();
}

#[cfg(feature = "desktop_rebuild")]
#[test]
fn gre600_r1_category_validation_is_typed_and_rolls_back() {
    use pp_storage::catalog::CatalogFailure;
    let fixture = Fixture::new();
    let owner = fixture.open();
    let client = owner.local_source_catalog();
    create(&client, "Preserved");
    let before = rows(&fixture.0);
    let settings = value(&client, Request::GetCategories {});
    for (categories, replacements) in [
        (json!([""]), json!({})),
        (json!([]), json!({})),
        (json!(["Other"]), json!({"Unknown":null})),
        (json!(["Other"]), json!({"Mods":"Missing"})),
        (json!(["Mods/Child"]), json!({"Mods":"Mods/Child"})),
        (json!(["Mods"]), json!({"Mods":"Mods"})),
    ] {
        for operation in ["save_categories", "save_categories_with_tree"] {
            let error = client.execute(rebuild_request(json!({"operation":operation,"categories":categories,"replacements":replacements}))).unwrap_err();
            assert!(
                matches!(
                    error.downcast_ref::<CatalogFailure>(),
                    Some(CatalogFailure::Input(_))
                ),
                "{error}"
            );
            assert_eq!(rows(&fixture.0), before);
            assert_eq!(value(&client, Request::GetCategories {}), settings);
        }
    }
    fixture.sql().execute_batch("CREATE TRIGGER reject_categories BEFORE INSERT ON app_settings WHEN NEW.key='source_categories' BEGIN SELECT RAISE(ABORT,'category storage failure'); END;").unwrap();
    let error = client
        .execute(rebuild_request(
            json!({"operation":"save_categories_with_tree","categories":["New"],"replacements":{}}),
        ))
        .unwrap_err();
    assert!(error.downcast_ref::<rusqlite::Error>().is_some());
    assert!(error.downcast_ref::<CatalogFailure>().is_none());
    assert_eq!(rows(&fixture.0), before);
    assert_eq!(value(&client, Request::GetCategories {}), settings);
    owner.shutdown().unwrap();
}

#[cfg(not(feature = "desktop_rebuild"))]
#[test]
fn gre601_preview_requires_rebuild_feature() {
    let error = serde_json::from_value::<Request>(json!({
        "operation":"preview_naming", "relative_path":"gear.stl"
    }))
    .err()
    .unwrap();
    assert!(
        error
            .to_string()
            .contains("unknown variant `preview_naming`")
    );
}

#[cfg(feature = "desktop_rebuild")]
fn preview(client: &SourceCatalogClient, path: &str, profile: Value) -> Value {
    value(
        client,
        serde_json::from_value::<Request>(json!({
            "operation":"preview_naming", "relative_path":path, "profile":profile
        }))
        .unwrap(),
    )
}

#[cfg(feature = "desktop_rebuild")]
#[test]
fn gre601_preview_default_behavior_is_read_only() {
    let fixture = Fixture::new();
    let owner = fixture.open();
    let client = owner.local_source_catalog();
    let before = rows(&fixture.0);
    let bytes = database_bytes(&fixture.0);
    for (path, expected) in [
        (
            "folder\\[a]gear x3.STL",
            json!({"role":"accent", "quantity":3, "part_slug":"gear", "filename":"[a]gear x3.STL"}),
        ),
        (
            "[c]/[o]gear.stl",
            json!({"role":"clear", "quantity":1, "part_slug":"gear", "filename":"[o]gear.stl"}),
        ),
        (
            "plain.obj",
            json!({"role":"primary", "quantity":1, "part_slug":"plain.obj", "filename":"plain.obj"}),
        ),
        (
            "[a].stl",
            json!({"role":"accent", "quantity":1, "part_slug":"[a]", "filename":"[a].stl"}),
        ),
        (
            "folder/",
            json!({"role":"primary", "quantity":1, "part_slug":"", "filename":""}),
        ),
    ] {
        assert_eq!(preview(&client, path, Value::Null), expected, "{path}");
    }
    assert_eq!(rows(&fixture.0), before);
    assert_eq!(database_bytes(&fixture.0), bytes);
    owner.shutdown().unwrap();
    let owner = fixture.open();
    assert_eq!(rows(&fixture.0), before);
    assert_eq!(
        preview(&owner.local_source_catalog(), "gear.stl", json!({})),
        json!({
            "role":"primary", "quantity":1, "part_slug":"gear", "filename":"gear.stl"
        })
    );
    assert_eq!(rows(&fixture.0), before);
    owner.shutdown().unwrap();
}

#[cfg(feature = "desktop_rebuild")]
#[test]
fn gre601_preview_merges_roles_and_preserves_reference_ordering() {
    let fixture = Fixture::new();
    let owner = fixture.open();
    let client = owner.local_source_catalog();
    let before = rows(&fixture.0);
    for (path, profile, role, slug) in [
        (
            "[a]gear.stl",
            json!({"folder_rules":[{"path_contains":" GEAR ","role_id":"opaque","functional_class":null},{"path_contains":"gear","role_id":"clear"}]}),
            "opaque",
            "gear",
        ),
        (
            "AA.stl",
            json!({"roles":[{"id":"accent","markers":["A"]},{"id":"clear","markers":["AA"]}]}),
            "clear",
            "A",
        ),
        (
            "tag.stl",
            json!({"roles":[{"id":"primary","markers":["tag"]},{"id":"accent","markers":["tag"]}]}),
            "primary",
            "tag",
        ),
        (
            "😀ab.stl",
            json!({"roles":[{"id":"accent","markers":["😀"]},{"id":"clear","markers":["ab"]}]}),
            "accent",
            "😀ab",
        ),
        (
            "ab😀x.stl",
            json!({"roles":[{"id":"accent","markers":["ab"]},{"id":"clear","markers":["😀x"]}]} ),
            "clear",
            "ab😀x",
        ),
        (
            "[gear.stl",
            json!({"roles":[{"id":"accent","markers":["["," ",""]}]}),
            "accent",
            "gear",
        ),
        (
            "😀gear.stl",
            json!({"roles":[{"id":"accent","markers":["😀"]}]}),
            "accent",
            "gear",
        ),
        (
            "ab😀gear.stl",
            json!({"roles":[{"id":"accent","markers":["ab"]}]}),
            "accent",
            "😀gear",
        ),
        ("[o]/[a]gear.stl", json!({}), "opaque", "gear"),
        ("[a][a]gear.stl", json!({}), "accent", "[a]gear"),
        ("[a][c]gear.stl", json!({}), "accent", "gear"),
        ("[c][a]gear.stl", json!({}), "accent", "[a]gear"),
        (
            "[A]gear.stl",
            json!({"roles":[{"id":"accent","label":" Renamed "}]}),
            "accent",
            "gear",
        ),
        (
            "[a]gear x2.stl",
            json!({"slug":{"strip_markers":false,"strip_quantity":false}}),
            "accent",
            "[a]gear x2",
        ),
        (
            "[a]gear x2.stl",
            json!({"slug":{"strip_markers":false}}),
            "accent",
            "[a]gear",
        ),
        (
            "[a]gear x2.stl",
            json!({"roles":[{"id":"accent","markers":[]}],"slug":{"strip_quantity":false}}),
            "primary",
            "[a]gear x2",
        ),
        (
            "gear.stl",
            json!({"folder_rules":[],"export_role_order":["opaque","clear","accent","primary"]}),
            "primary",
            "gear",
        ),
    ] {
        let filename = path.rsplit('/').next().unwrap();
        let quantity = if path.contains(" x2") { 2 } else { 1 };
        assert_eq!(
            preview(&client, path, profile),
            json!({
                "role":role,"quantity":quantity,"part_slug":slug,"filename":filename
            }),
            "{path}"
        );
    }
    assert_eq!(rows(&fixture.0), before);
    owner.shutdown().unwrap();
}

#[cfg(feature = "desktop_rebuild")]
#[test]
fn gre601_preview_quantity_strips_only_literal_stl_suffixes() {
    let fixture = Fixture::new();
    let owner = fixture.open();
    let client = owner.local_source_catalog();
    for (path, regex, slug) in [
        ("gear x2_stl", r"x([0-9]+)_stl", "gear "),
        ("gear x2_stl", r"x([0-9]+)_stl$", "gear "),
        ("gear x2.stl", r"x([0-9]+)\.stl", "gear x2"),
        ("gear x2.STL", r"x([0-9]+)\.stl$", "gear "),
        ("gear x2.astl", r"x([0-9]+)\.astl$", "gear "),
        ("gear x2_stl-tail", r"x([0-9]+)_stl", "gear -tail"),
    ] {
        assert_eq!(
            preview(&client, path, json!({"quantity":{"regex":regex}})),
            json!({"role":"primary","quantity":2,"part_slug":slug,"filename":path}),
            "{path} with {regex}"
        );
    }
    owner.shutdown().unwrap();
}

#[cfg(feature = "desktop_rebuild")]
#[test]
fn gre601_preview_case_matching_preserves_original_marker_length_order() {
    let fixture = Fixture::new();
    let owner = fixture.open();
    let client = owner.local_source_catalog();
    for (path, profile, role, slug, filename) in [
        (
            "İXYgear.stl",
            json!({"roles":[{"id":"accent","markers":["İ"]},{"id":"clear","markers":["XY"]}],"slug":{"strip_markers":false}}),
            "clear",
            "İXYgear",
            "İXYgear.stl",
        ),
        (
            "İxgear.stl",
            json!({"roles":[{"id":"accent","markers":["x"]},{"id":"clear","markers":["İ"]}],"slug":{"strip_markers":false}}),
            "accent",
            "İxgear",
            "İxgear.stl",
        ),
        (
            "MiXeD/gear.stl",
            json!({"folder_rules":[{"path_contains":"mIxEd","role_id":"opaque"}]}),
            "opaque",
            "gear",
            "gear.stl",
        ),
        (
            "TaG/gear.stl",
            json!({"roles":[{"id":"accent","markers":["tAg"]}]}),
            "accent",
            "gear",
            "gear.stl",
        ),
    ] {
        assert_eq!(
            preview(&client, path, profile),
            json!({"role":role,"quantity":1,"part_slug":slug,"filename":filename}),
            "{path}"
        );
    }
    let mut markers = (0..512)
        .map(|i| format!("missing-marker-{i}"))
        .collect::<Vec<_>>();
    markers.push("FiNaL".into());
    let path = format!("{}/final/gear.stl", "z".repeat(400_000));
    assert_eq!(
        preview(
            &client,
            &path,
            json!({"roles":[{"id":"accent","markers":markers}]})
        ),
        json!({"role":"accent","quantity":1,"part_slug":"gear","filename":"gear.stl"})
    );
    owner.shutdown().unwrap();
}

#[cfg(feature = "desktop_rebuild")]
#[test]
fn gre601_preview_quantity_uses_numeric_prefix_clamping_and_null() {
    let fixture = Fixture::new();
    let owner = fixture.open();
    let client = owner.local_source_catalog();
    for (path, regex, default, quantity, slug) in [
        (
            "gear x0.stl".to_owned(),
            r"[ _]x([0-9]+)\.stl$",
            1,
            json!(1),
            "gear",
        ),
        (
            "gear x-9.stl".to_owned(),
            r"x(-?[0-9]+)\.stl$",
            1,
            json!(1),
            "gear x-9",
        ),
        (
            "gear x+12tail.stl".to_owned(),
            r"x(.+)\.stl$",
            1,
            json!(12),
            "gear x+12tail",
        ),
        (
            "gear x  7tail.stl".to_owned(),
            r"x(.+)\.stl$",
            1,
            json!(7),
            "gear x  7tail",
        ),
        (
            "gear xabc.stl".to_owned(),
            r"x(.+)\.stl$",
            1,
            Value::Null,
            "gear xabc",
        ),
        (
            "gear x.stl".to_owned(),
            r"x([0-9]*)\.stl$",
            6,
            json!(6),
            "gear x",
        ),
        (
            "gear.stl".to_owned(),
            r"x([0-9]+)\.stl$",
            9,
            json!(9),
            "gear",
        ),
        (
            "gear x2.STL".to_owned(),
            r"x([0-9]+)\.stl$",
            1,
            json!(2),
            "gear ",
        ),
        (
            format!("gear x{}.stl", "9".repeat(400)),
            r"x([0-9]+)\.stl$",
            1,
            Value::Null,
            "gear ",
        ),
        (
            "gear x9007199254740993.stl".to_owned(),
            r"x([0-9]+)\.stl$",
            1,
            json!(9_007_199_254_740_992_i64),
            "gear ",
        ),
    ] {
        assert_eq!(
            preview(
                &client,
                &path,
                json!({"quantity":{"regex":regex,"default":default}})
            ),
            json!({
                "role":"primary","quantity":quantity,"part_slug":slug,"filename":path
            }),
            "{path}"
        );
    }
    owner.shutdown().unwrap();
}

#[cfg(feature = "desktop_rebuild")]
#[test]
fn gre601_preview_rejects_invalid_typed_overrides_without_writes() {
    use pp_storage::catalog::CatalogFailure;
    let fixture = Fixture::new();
    let owner = fixture.open();
    let client = owner.local_source_catalog();
    let before = rows(&fixture.0);
    let bytes = database_bytes(&fixture.0);
    for profile in [
        json!({"roles":null}),
        json!({"quantity":null}),
        json!({"slug":null}),
        json!({"folder_rules":null}),
        json!({"export_role_order":null}),
        json!({"roles":[{"id":"accent","label":null}]}),
        json!({"roles":[{"id":"accent","markers":null}]}),
        json!({"quantity":{"regex":null}}),
        json!({"quantity":{"default":null}}),
        json!({"slug":{"strip_markers":null}}),
        json!({"slug":{"strip_quantity":null}}),
        json!({"extra":true}),
        json!({"quantity":{"extra":true}}),
        json!({"slug":{"extra":true}}),
        json!({"roles":[{"id":"accent","extra":true}]}),
        json!({"roles":[{"id":"invalid"}]}),
        json!({"quantity":{"default":-1}}),
        json!({"quantity":{"default":1.5}}),
        json!({"folder_rules":[{"path_contains":"x","role_id":"accent","extra":true}]}),
    ] {
        assert!(
            serde_json::from_value::<Request>(json!({
                "operation":"preview_naming","relative_path":"gear.stl","profile":profile
            }))
            .is_err(),
            "{profile}"
        );
    }
    for (profile, message) in [
        (json!({"roles":[]}), "Empty roles override"),
        (
            json!({"roles":[{"id":"accent"},{"id":"accent"}]}),
            "Duplicate role override",
        ),
        (
            json!({"roles":[{"id":"primary","label":" "}]}),
            "Blank naming label",
        ),
        (
            json!({"quantity":{"default":0}}),
            "Invalid quantity default",
        ),
        (
            json!({"quantity":{"default":9007199254740992_u64}}),
            "Invalid quantity default",
        ),
        (
            json!({"quantity":{"regex":"["}}),
            "Invalid quantity regex structure",
        ),
        (
            json!({"quantity":{"regex":"(x)[z-a]"}}),
            "Invalid quantity regex",
        ),
        (
            json!({"quantity":{"regex":"x"}}),
            "Quantity regex must have one capture group",
        ),
        (
            json!({"quantity":{"regex":"(x)(y)"}}),
            "Quantity regex must have one capture group",
        ),
        (
            json!({"folder_rules":[{"path_contains":" ","role_id":"clear"}]}),
            "Blank folder rule",
        ),
        (json!({"export_role_order":[]}), "Invalid export role order"),
        (
            json!({"export_role_order":["primary","accent","clear","clear"]}),
            "Duplicate export role",
        ),
    ] {
        let error = client
            .execute(rebuild_request(json!({
                "operation":"preview_naming","relative_path":"gear.stl","profile":profile
            })))
            .unwrap_err();
        assert!(
            matches!(error.downcast_ref::<CatalogFailure>(), Some(CatalogFailure::Input(detail)) if detail == message),
            "{profile}: {error:?}"
        );
    }
    assert_eq!(
        preview(&client, "gear.stl", json!({"quantity":{},"slug":{}})),
        json!({
            "role":"primary","quantity":1,"part_slug":"gear","filename":"gear.stl"
        })
    );
    assert_eq!(rows(&fixture.0), before);
    assert_eq!(database_bytes(&fixture.0), bytes);
    owner.shutdown().unwrap();
}

#[cfg(feature = "desktop_rebuild")]
#[test]
fn gre601_preview_uses_persisted_tenant_defaults_and_existing_authority() {
    use pp_storage::auth::{AuthPolicy, RegistrationPolicy, SessionTenantPolicy};
    let fixture = Fixture::new();
    let owner = fixture.open();
    let (tenant, token) = actor(&owner, "preview@example.test");
    let policy = AuthPolicy {
        registration: RegistrationPolicy::Open,
        session_tenant: SessionTenantPolicy::AccountTenant,
        first_user: FirstUserTenant::NewUser,
    };
    let client = owner
        .catalog_access(policy)
        .unwrap()
        .session(Secret::new(token.clone()));
    let local = owner.local_source_catalog();
    let id = create(&client, "Naming source");
    let mut profile = NamingProfile::default();
    profile.quantity.default = 7;
    profile.roles[1].markers = vec!["tag".into()];
    value(
        &client,
        Request::SaveGlobalNaming {
            profile: profile.clone(),
        },
    );
    profile.quantity.default = 19;
    value(
        &local,
        Request::SaveGlobalNaming {
            profile: profile.clone(),
        },
    );
    profile.quantity.default = 99;
    value(
        &client,
        Request::SaveNaming {
            id,
            settings: NamingCommand::Override { profile },
        },
    );
    let before = rows(&fixture.0);
    let bytes = database_bytes(&fixture.0);
    let tenant_expected =
        json!({"role":"accent","quantity":7,"part_slug":"gear","filename":"taggear.stl"});
    assert_eq!(
        preview(&client, "taggear.stl", Value::Null),
        tenant_expected
    );
    assert_eq!(
        value(
            &client,
            rebuild_request(json!({
                "operation":"preview_naming","relative_path":"taggear.stl"
            }))
        ),
        tenant_expected
    );
    assert_eq!(
        preview(&client, "taggear.stl", json!({"quantity":{"default":3}})),
        json!({
            "role":"accent","quantity":3,"part_slug":"gear","filename":"taggear.stl"
        })
    );
    assert_eq!(
        preview(&local, "taggear.stl", json!({})),
        json!({
            "role":"accent","quantity":19,"part_slug":"gear","filename":"taggear.stl"
        })
    );
    let denied = owner
        .catalog_access(policy)
        .unwrap()
        .session(Secret::new("invalid-session".into()));
    let error = denied
        .execute(rebuild_request(json!({
            "operation":"preview_naming","relative_path":"gear.stl"
        })))
        .unwrap_err();
    assert_eq!(error.to_string(), "Authentication required");
    assert_eq!(rows(&fixture.0), before);
    assert_eq!(database_bytes(&fixture.0), bytes);
    let auth = owner.auth(FirstUserTenant::NewUser);
    let AuthOutcome::KeyCreated { key, .. } = auth
        .submit(
            AuthRequest::CreateKey {
                session: Secret::new(token.clone()),
            },
            Arc::new(AtomicBool::new(false)),
            Duration::from_secs(5),
        )
        .unwrap()
        .recv()
        .unwrap()
        .unwrap()
    else {
        panic!("key")
    };
    let foreign = owner
        .catalog_key_access(policy, "default".into())
        .unwrap()
        .key(key);
    let before = rows(&fixture.0);
    let bytes = database_bytes(&fixture.0);
    assert_eq!(
        foreign
            .execute(rebuild_request(json!({
                "operation":"preview_naming","relative_path":"gear.stl"
            })))
            .unwrap_err()
            .to_string(),
        "Authentication required"
    );
    assert_eq!(rows(&fixture.0), before);
    assert_eq!(database_bytes(&fixture.0), bytes);
    owner.shutdown().unwrap();
    let owner = fixture.open();
    let client = owner
        .catalog_access(policy)
        .unwrap()
        .session(Secret::new(token));
    assert_eq!(preview(&client, "taggear.stl", json!({})), tenant_expected);
    assert_eq!(rows(&fixture.0), before);
    assert_eq!(
        value(&client, Request::GetNaming { id })["effective"]["quantity"]["default"],
        99
    );
    let db = fixture.sql();
    assert_eq!(
        db.query_row(
            "SELECT value FROM app_settings WHERE tenant_id=?1 AND key='stl_naming_defaults'",
            [&tenant],
            |row| row.get::<_, String>(0)
        )
        .unwrap()
        .parse::<Value>()
        .unwrap()["quantity"]["default"],
        7
    );
    drop(db);
    owner.shutdown().unwrap();
}

#[cfg(feature = "desktop_rebuild")]
#[test]
fn gre601_preview_adds_missing_roles_and_falls_back_from_invalid_stored_defaults() {
    let fixture = Fixture::new();
    let owner = fixture.open();
    let client = owner.local_source_catalog();
    let mut profile = NamingProfile::default();
    profile.roles.truncate(1);
    profile.quantity.default = 4;
    value(&client, Request::SaveGlobalNaming { profile });
    owner.shutdown().unwrap();
    let owner = fixture.open();
    let client = owner.local_source_catalog();
    let before = rows(&fixture.0);
    let bytes = database_bytes(&fixture.0);
    assert_eq!(
        preview(
            &client,
            "taggear.stl",
            json!({"roles":[{"id":"accent","markers":["tag"]}]})
        ),
        json!({
            "role":"accent","quantity":4,"part_slug":"gear","filename":"taggear.stl"
        })
    );
    assert_eq!(
        preview(&client, "taggear.stl", json!({})),
        json!({
            "role":"primary","quantity":4,"part_slug":"taggear","filename":"taggear.stl"
        })
    );
    assert_eq!(rows(&fixture.0), before);
    assert_eq!(database_bytes(&fixture.0), bytes);
    owner.shutdown().unwrap();
    let db = fixture.sql();
    db.execute("UPDATE app_settings SET value='{}' WHERE tenant_id='default' AND key='stl_naming_defaults'", []).unwrap();
    drop(db);
    let owner = fixture.open();
    let before = rows(&fixture.0);
    let bytes = database_bytes(&fixture.0);
    assert_eq!(
        preview(&owner.local_source_catalog(), "[a]gear x2.stl", json!({})),
        json!({
            "role":"accent","quantity":2,"part_slug":"gear","filename":"[a]gear x2.stl"
        })
    );
    assert_eq!(rows(&fixture.0), before);
    assert_eq!(database_bytes(&fixture.0), bytes);
    owner.shutdown().unwrap();
}

#[test]
fn gre601_naming_save_validation_is_classified_and_atomic() {
    let fixture = Fixture::new();
    let owner = fixture.open();
    let client = owner.local_source_catalog();
    let id = create(&client, "Naming validation");
    let baseline = rows(&fixture.0);
    let defaults = serde_json::to_value(NamingProfile::default()).unwrap();
    let cases = [
        ("roles", json!([]), "Naming requires primary role"),
        (
            "roles",
            json!([{"id":"accent","label":"Accent","markers":[]}]),
            "Naming requires primary role",
        ),
        (
            "roles",
            json!([
                {"id":"primary","label":"First","markers":[]},
                {"id":"primary","label":"Second","markers":[]}
            ]),
            "Duplicate naming role",
        ),
        (
            "roles",
            json!([{"id":"primary","label":" \t\n","markers":[]}]),
            "Blank naming label",
        ),
        ("export_role_order", json!([]), "Invalid export role order"),
        (
            "export_role_order",
            json!(["primary", "accent", "clear", "clear"]),
            "Duplicate export role",
        ),
        (
            "quantity",
            json!({"regex":"(x)","default":0}),
            "Invalid quantity default",
        ),
        (
            "quantity",
            json!({"regex":"(x)","default":9007199254740992_u64}),
            "Invalid quantity default",
        ),
        (
            "quantity",
            json!({"regex":"[","default":1}),
            "Invalid quantity regex structure",
        ),
        (
            "quantity",
            json!({"regex":"(x)[z-a]","default":1}),
            "Invalid quantity regex",
        ),
        (
            "quantity",
            json!({"regex":"x","default":1}),
            "Quantity regex must have one capture group",
        ),
        (
            "quantity",
            json!({"regex":"(x)(y)","default":1}),
            "Quantity regex must have one capture group",
        ),
        (
            "quantity",
            json!({"regex":format!("({})", "x".repeat(4096)),"default":1}),
            "Quantity regex too long",
        ),
        (
            "quantity",
            json!({"regex":format!("{}x{}", "(".repeat(17), ")".repeat(17)),"default":1}),
            "Quantity regex exceeds group complexity limits",
        ),
        (
            "folder_rules",
            json!([{"path_contains":" \n","role_id":"accent"}]),
            "Blank folder rule",
        ),
    ];
    for (field, invalid, message) in cases {
        let mut profile = defaults.clone();
        profile[field] = invalid;
        for source in [false, true] {
            let profile = serde_json::from_value(profile.clone()).unwrap();
            let request = if source {
                Request::SaveNaming {
                    id,
                    settings: NamingCommand::Override { profile },
                }
            } else {
                Request::SaveGlobalNaming { profile }
            };
            let error = client.execute(request).unwrap_err();
            assert_eq!(error.to_string(), message, "{field}, source={source}");
            #[cfg(feature = "desktop_rebuild")]
            assert!(
                matches!(
                    error.downcast_ref::<pp_storage::catalog::CatalogFailure>(),
                    Some(pp_storage::catalog::CatalogFailure::Input(detail)) if detail == message
                ),
                "{field}, source={source}: {error:?}"
            );
            assert_eq!(rows(&fixture.0), baseline, "{field}, source={source}");
        }
    }
    drop(client);
    owner.shutdown().unwrap();
    let reopened = fixture.open();
    assert_eq!(rows(&fixture.0), baseline);
    reopened.shutdown().unwrap();
}

#[test]
fn gre601_naming_missing_source_has_stable_classification() {
    let fixture = Fixture::new();
    let owner = fixture.open();
    let client = owner.local_source_catalog();
    let baseline = rows(&fixture.0);
    for id in [0, -1, i64::MAX] {
        for request in [
            Request::GetNaming { id },
            Request::SaveNaming {
                id,
                settings: NamingCommand::UseDefaults,
            },
            Request::SaveNaming {
                id,
                settings: NamingCommand::Override {
                    profile: NamingProfile::default(),
                },
            },
        ] {
            let error = client.execute(request).unwrap_err();
            assert_eq!(error.to_string(), "Source not found");
            #[cfg(feature = "desktop_rebuild")]
            assert!(matches!(
                error.downcast_ref::<pp_storage::catalog::CatalogFailure>(),
                Some(pp_storage::catalog::CatalogFailure::NotFound)
            ));
            assert_eq!(rows(&fixture.0), baseline);
        }
    }
    owner.shutdown().unwrap();
}

#[test]
fn gre601_naming_stored_override_errors_are_distinct_and_read_only() {
    let fixture = Fixture::new();
    let owner = fixture.open();
    let client = owner.local_source_catalog();
    let id = create(&client, "Stored naming");
    let db = fixture.sql();
    for invalid in [
        json!(false),
        json!({"unknown":true}),
        json!({"roles":{}}),
        json!({"roles":[]}),
        json!({"roles":[{}]}),
        json!({"roles":[{"id":"invalid"}]}),
        json!({"roles":[{"id":"accent"},{"id":"accent"}]}),
        json!({"roles":[{"id":"primary","label":" "}]}),
        json!({"roles":[{"id":"primary","extra":true}]}),
        json!({"quantity":null}),
        json!({"quantity":{"default":0}}),
        json!({"quantity":{"regex":"["}}),
        json!({"slug":{"strip_markers":"true"}}),
        json!({"folder_rules":[{"path_contains":" ","role_id":"clear"}]}),
        json!({"export_role_order":[]}),
    ] {
        let metadata = json!({
            "unrelated":{"keep":[1,2]},
            "naming":{"use_defaults":false,"override":invalid}
        });
        db.execute(
            "UPDATE projects SET metadata_json=?1 WHERE id=?2",
            rusqlite::params![metadata.to_string(), id],
        )
        .unwrap();
        let before = rows(&fixture.0);
        let error = client.execute(Request::GetNaming { id }).unwrap_err();
        #[cfg(feature = "desktop_rebuild")]
        {
            assert_eq!(
                error.to_string(),
                "Stored Source naming settings are invalid"
            );
            assert!(matches!(
                error.downcast_ref::<pp_storage::catalog::CatalogFailure>(),
                Some(pp_storage::catalog::CatalogFailure::InvalidStoredNaming)
            ));
            assert!(error.chain().count() > 1, "original cause is retained");
        }
        #[cfg(not(feature = "desktop_rebuild"))]
        assert_ne!(
            error.to_string(),
            "Stored Source naming settings are invalid"
        );
        assert_eq!(rows(&fixture.0), before, "{invalid}");
        let repaired = value(
            &client,
            Request::SaveNaming {
                id,
                settings: NamingCommand::UseDefaults,
            },
        );
        assert_eq!(repaired["use_defaults"], true);
        assert_eq!(repaired["override"], json!({}));
        assert_eq!(
            value(&client, Request::Get { id })["metadata"]["unrelated"],
            json!({"keep":[1,2]})
        );
    }
    drop(db);
    owner.shutdown().unwrap();
}

#[test]
fn gre601_naming_storage_aborts_keep_sql_errors_and_roll_back() {
    let fixture = Fixture::new();
    let owner = fixture.open();
    let client = owner.local_source_catalog();
    let id = create(&client, "Naming abort");
    let db = fixture.sql();
    db.execute_batch(
        "CREATE TRIGGER reject_global_naming BEFORE INSERT ON app_settings
         WHEN NEW.key='stl_naming_defaults'
         BEGIN SELECT RAISE(ABORT,'global naming storage failure'); END;
         CREATE TRIGGER reject_source_naming BEFORE UPDATE OF metadata_json ON projects
         BEGIN SELECT RAISE(ABORT,'source naming storage failure'); END;",
    )
    .unwrap();
    let baseline = rows(&fixture.0);
    for (request, expected) in [
        (
            Request::SaveGlobalNaming {
                profile: NamingProfile::default(),
            },
            "global naming storage failure",
        ),
        (
            Request::SaveNaming {
                id,
                settings: NamingCommand::Override {
                    profile: NamingProfile::default(),
                },
            },
            "source naming storage failure",
        ),
        (
            Request::SaveNaming {
                id,
                settings: NamingCommand::UseDefaults,
            },
            "source naming storage failure",
        ),
    ] {
        let error = client.execute(request).unwrap_err();
        assert_eq!(error.to_string(), expected);
        assert!(error.downcast_ref::<rusqlite::Error>().is_some());
        #[cfg(feature = "desktop_rebuild")]
        assert!(
            error
                .downcast_ref::<pp_storage::catalog::CatalogFailure>()
                .is_none()
        );
        assert_eq!(rows(&fixture.0), baseline);
    }
    db.execute_batch("DROP TRIGGER reject_global_naming; DROP TRIGGER reject_source_naming;")
        .unwrap();
    let saved = value(
        &client,
        Request::SaveNaming {
            id,
            settings: NamingCommand::Override {
                profile: NamingProfile::default(),
            },
        },
    );
    assert_eq!(saved["use_defaults"], false);
    drop(db);
    owner.shutdown().unwrap();
    let reopened = fixture.open();
    assert_eq!(
        value(&reopened.local_source_catalog(), Request::GetNaming { id }),
        saved
    );
    reopened.shutdown().unwrap();
}

#[test]
fn gre601_naming_projection_normalizes_partial_overrides_and_digest() {
    let fixture = Fixture::new();
    let owner = fixture.open();
    let client = owner.local_source_catalog();
    let id = create(&client, "Partial naming");
    let mut global = NamingProfile::default();
    global.quantity.default = 7;
    global.roles[0].label = "  Main \u{feff}".into();
    value(&client, Request::SaveGlobalNaming { profile: global });
    let db = fixture.sql();
    let partial = json!({
        "roles":[{"id":"accent","label":" \u{a0}Highlight\u{feff}"}],
        "quantity":{"regex":"  [ _]x([0-9]+)\\.stl$  "},
        "slug":{"strip_markers":false},
        "folder_rules":[{"path_contains":"  accents/ ","role_id":"accent"}]
    });
    db.execute(
        "UPDATE projects SET metadata_json=?1 WHERE id=?2",
        rusqlite::params![
            json!({"naming":{"use_defaults":false,"override":partial}}).to_string(),
            id
        ],
    )
    .unwrap();
    let before = rows(&fixture.0);
    let projected = value(&client, Request::GetNaming { id });
    assert_eq!(projected["use_defaults"], false);
    assert_eq!(projected["effective"]["roles"][0]["label"], "Main");
    assert_eq!(projected["effective"]["roles"][1]["label"], "Highlight");
    assert_eq!(
        projected["effective"]["roles"][1]["markers"],
        json!(["[a]"])
    );
    assert_eq!(projected["effective"]["quantity"]["default"], 7);
    assert_eq!(projected["effective"]["slug"]["strip_markers"], false);
    assert_eq!(projected["effective"]["slug"]["strip_quantity"], true);
    assert_eq!(
        projected["override"],
        json!({
            "roles":[{"id":"accent","label":"Highlight"}],
            "quantity":{"regex":"[ _]x([0-9]+)\\.stl$"},
            "slug":{"strip_markers":false},
            "folder_rules":[{"path_contains":"accents/","role_id":"accent"}]
        })
    );
    assert_eq!(rows(&fixture.0), before);
    let full = serde_json::from_value(projected["effective"].clone()).unwrap();
    let saved = value(
        &client,
        Request::SaveNaming {
            id,
            settings: NamingCommand::Override { profile: full },
        },
    );
    assert_eq!(saved["effective"], projected["effective"]);
    assert_eq!(saved["effective_digest"], projected["effective_digest"]);
    assert_eq!(saved["override"], saved["effective"]);
    let mut global = NamingProfile::default();
    global.quantity.default = 19;
    value(&client, Request::SaveGlobalNaming { profile: global });
    assert_eq!(value(&client, Request::GetNaming { id }), saved);
    let reset = value(
        &client,
        Request::SaveNaming {
            id,
            settings: NamingCommand::UseDefaults,
        },
    );
    assert_eq!(reset["use_defaults"], true);
    assert_eq!(reset["effective"]["quantity"]["default"], 19);
    assert_ne!(reset["effective_digest"], saved["effective_digest"]);
    drop(db);
    owner.shutdown().unwrap();
    let reopened = fixture.open();
    assert_eq!(
        value(&reopened.local_source_catalog(), Request::GetNaming { id }),
        reset
    );
    reopened.shutdown().unwrap();
}

#[test]
fn gre601_naming_global_corruption_falls_back_without_repairing_storage() {
    let fixture = Fixture::new();
    let owner = fixture.open();
    let client = owner.local_source_catalog();
    let id = create(&client, "Default fallback");
    let expected = value(&client, Request::GetNaming { id });
    let db = fixture.sql();
    for invalid in ["not json".to_owned(), "null".to_owned(), "{}".to_owned(), {
        let mut invalid = serde_json::to_value(NamingProfile::default()).unwrap();
        invalid["quantity"]["default"] = json!(0);
        invalid.to_string()
    }] {
        db.execute(
            "INSERT INTO app_settings(tenant_id,key,value) VALUES('default','stl_naming_defaults',?1)
             ON CONFLICT(tenant_id,key) DO UPDATE SET value=excluded.value",
            [invalid],
        )
        .unwrap();
        let before = rows(&fixture.0);
        assert_eq!(value(&client, Request::GetNaming { id }), expected);
        assert_eq!(
            value(&client, Request::GetGlobalNaming {}),
            serde_json::to_value(NamingProfile::default()).unwrap()
        );
        assert_eq!(rows(&fixture.0), before);
    }
    drop(db);
    owner.shutdown().unwrap();
}

#[cfg(feature = "desktop_rebuild")]
#[test]
fn gre601_naming_auth_precedes_input_and_foreign_sources_are_not_found() {
    use pp_storage::catalog::CatalogFailure;
    let fixture = Fixture::new();
    let owner = fixture.open();
    let local = owner.local_source_catalog();
    let foreign_id = create(&local, "Private naming");
    let (_, token) = actor(&owner, "naming-tenant@example.com");
    let client = session(&owner, &token);
    let id = create(&client, "Tenant naming");
    let denied = session(&owner, "invalid-session");
    let before = rows(&fixture.0);
    for request in [
        Request::GetNaming { id },
        Request::SaveNaming {
            id,
            settings: NamingCommand::UseDefaults,
        },
        Request::SaveGlobalNaming {
            profile: NamingProfile {
                roles: vec![],
                ..NamingProfile::default()
            },
        },
    ] {
        let error = denied.execute(request).unwrap_err();
        assert_eq!(error.to_string(), "Authentication required");
        assert!(error.downcast_ref::<CatalogFailure>().is_none());
    }
    for request in [
        Request::GetNaming { id: foreign_id },
        Request::SaveNaming {
            id: foreign_id,
            settings: NamingCommand::Override {
                profile: NamingProfile {
                    roles: vec![],
                    ..NamingProfile::default()
                },
            },
        },
    ] {
        let error = client.execute(request).unwrap_err();
        assert!(matches!(
            error.downcast_ref::<CatalogFailure>(),
            Some(CatalogFailure::NotFound)
        ));
    }
    assert_eq!(rows(&fixture.0), before);
    owner.shutdown().unwrap();
}

#[test]
fn gre601_naming_global_read_failure_is_not_stored_override_corruption() {
    let fixture = Fixture::new();
    let owner = fixture.open();
    let client = owner.local_source_catalog();
    let id = create(&client, "Naming read failure");
    let db = fixture.sql();
    db.execute_batch("ALTER TABLE app_settings RENAME TO unavailable_settings;")
        .unwrap();
    let before = rows(&fixture.0);
    for request in [Request::GetNaming { id }, Request::GetGlobalNaming {}] {
        let error = client.execute(request).unwrap_err();
        assert!(error.downcast_ref::<rusqlite::Error>().is_some());
        #[cfg(feature = "desktop_rebuild")]
        assert!(
            error
                .downcast_ref::<pp_storage::catalog::CatalogFailure>()
                .is_none()
        );
        assert_eq!(rows(&fixture.0), before);
    }
    db.execute_batch("ALTER TABLE unavailable_settings RENAME TO app_settings;")
        .unwrap();
    assert_eq!(
        value(&client, Request::GetNaming { id })["use_defaults"],
        true
    );
    drop(db);
    owner.shutdown().unwrap();
}
