#![cfg(feature = "desktop_rebuild")]
use pp_storage::{
    Limits, WriterOwner,
    auth::{AuthPolicy, FirstUserTenant, RegistrationPolicy, Secret, SessionTenantPolicy},
    build_graph::{BuildCommand, BuildOutcome, mcp::Request},
    read_model::Credential,
};
use serde_json::{Value, json};
use std::{
    sync::{Arc, atomic::AtomicBool},
    time::Duration,
};
fn run(owner: &WriterOwner, tool: &str, params: Value) -> Value {
    let action = json!({"type":tool,"params":params,"label":"Fixture","summary":"Confirmed"});
    call(owner, tool, params, Some(action))
}
fn propose(owner: &WriterOwner, tool: &str, params: Value) -> Value {
    call(owner, tool, params, None)
}
fn call(owner: &WriterOwner, tool: &str, params: Value, action: Option<Value>) -> Value {
    let outcome = owner
        .build_graph_with_policy(AuthPolicy {
            registration: RegistrationPolicy::Open,
            session_tenant: SessionTenantPolicy::AccountTenant,
            first_user: FirstUserTenant::NewUser,
        })
        .unwrap()
        .execute(
            Credential::Session(Secret::new("required-unit-fixture-secret".into())),
            BuildCommand::Mcp(Request::new(1, tool.into(), params, action)),
            Arc::new(AtomicBool::new(false)),
            Duration::from_secs(2),
        )
        .unwrap();
    let BuildOutcome::Mcp(result) = outcome else {
        panic!("MCP outcome")
    };
    result
}
fn assert_manifest_refusals(owner: &WriterOwner, db: &rusqlite::Connection, kit: &Value) {
    let mut invalid = ["layers", "include", "exclude"]
        .map(|key| {
            let mut value = kit.clone();
            value[key] = json!([7]);
            value.to_string()
        })
        .to_vec();
    invalid.extend([
        "{".into(),
        "[]".into(),
        format!("{{\"future\":{}}}", "[".repeat(65) + "0" + &"]".repeat(65)),
        json!({"future":"x".repeat(8 * 1024 * 1024)}).to_string(),
        json!({"future":vec![0;100_001]}).to_string(),
    ]);
    let store = |raw: &str| {
        db.execute("INSERT OR REPLACE INTO app_settings(tenant_id,key,value) VALUES('default','kit_manifest_1',?1)", [raw]).unwrap();
    };
    for raw in invalid {
        store(&raw);
        let refused = run(
            owner,
            "add_addon",
            json!({"source_name":"Addon","suggested_excludes":["stock.stl"]}),
        );
        assert_eq!(refused["ok"], false);
        assert!(refused["detail"].as_str().unwrap().contains("stored kit"));
        let saved: String = db
            .query_row(
                "SELECT value FROM app_settings WHERE tenant_id='default' AND key='kit_manifest_1'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(saved, raw);
        for (table, expected) in [("profile_layers", 1), ("plan_decisions", 0)] {
            let count: i64 = db
                .query_row(&format!("SELECT count(*) FROM {table}"), [], |r| r.get(0))
                .unwrap();
            assert_eq!(count, expected);
        }
    }
    store(&kit.to_string());
}
#[test]
fn mcp_planning_confirmations_preserve_manifest_and_accepted_history() {
    let root = tempfile::tempdir().unwrap();
    let database = root.path().join("print-partner.db");
    std::fs::write(
        &database,
        include_bytes!("fixtures/required-units/unchanged.db"),
    )
    .unwrap();
    std::fs::create_dir_all(root.path().join("repos/2")).unwrap();
    let seed = include_str!("../../../../fixtures/parity/mcp-proposals/build-planning/seed.sql")
        .replace("<data-dir>", root.path().to_str().unwrap());
    let db = rusqlite::Connection::open(&database).unwrap();
    db.execute_batch(&seed).unwrap();
    let accepted: (i64, i64) = db
        .query_row(
            "SELECT accepted_plan_revision_id,accepted_plan_version FROM build_profiles WHERE id=1",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    drop(db);
    let (owner, _) = WriterOwner::open(root.path(), Limits::default()).unwrap();
    let db = rusqlite::Connection::open(&database).unwrap();
    let kit = json!({"name":"Keep","layers":["Base"],"include":["keep.stl"],"exclude":["old.stl","old.stl"],"selections":{"size":"350"},"replacements":{"stock":"custom"},"choice_tree":[{"id":"tree"}],"category_links":[{"id":"link"}],"future":{"enabled":true}});
    assert_manifest_refusals(&owner, &db, &kit);
    drop(db);
    let addon = run(
        &owner,
        "add_addon",
        json!({"source_name":"Addon","suggested_excludes":["stock.stl","old.stl","stock.stl"]}),
    );
    assert_eq!(addon["ok"], true, "{addon}");
    let brief = run(
        &owner,
        "propose_update_build_brief",
        json!({"requirements":[{"key":1.0,"value":1e20}]}),
    );
    assert_eq!(brief["ok"], true);
    owner.shutdown().unwrap();
    let (owner, _) = WriterOwner::open(root.path(), Limits::default()).unwrap();
    assert_eq!(
        run(&owner, "add_addon", json!({"source_name":"Addon"}))["ok"],
        false
    );
    owner.shutdown().unwrap();
    let db = rusqlite::Connection::open(&database).unwrap();
    let read = |key: &str| {
        let raw: String = db
            .query_row(
                "SELECT value FROM app_settings WHERE tenant_id='default' AND key=?1",
                [key],
                |r| r.get(0),
            )
            .unwrap();
        serde_json::from_str::<Value>(&raw).unwrap()
    };
    let mut expected = kit;
    expected["exclude"] = json!(["old.stl", "stock.stl"]);
    assert_eq!(read("kit_manifest_1"), expected);
    assert_eq!(addon["result"]["exclude"], expected["exclude"]);
    assert_eq!(
        read("build_planning.v1.1")["requirements"],
        json!([{"key":"1","value":"100000000000000000000","status":"unverified"}])
    );
    let after: (i64, i64) = db
        .query_row(
            "SELECT accepted_plan_revision_id,accepted_plan_version FROM build_profiles WHERE id=1",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    assert_eq!(after, accepted);
    let count: i64 = db
        .query_row(
            "SELECT count(*) FROM plan_decisions WHERE tenant_id='default' AND profile_id=1",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(count, 2);
}
/// Copies the fixture database and seeds Sources 2 (`Addon`) and 3 (foreign).
fn seeded() -> (tempfile::TempDir, std::path::PathBuf) {
    let root = tempfile::tempdir().unwrap();
    let database = root.path().join("print-partner.db");
    std::fs::write(
        &database,
        include_bytes!("fixtures/required-units/unchanged.db"),
    )
    .unwrap();
    std::fs::create_dir_all(root.path().join("repos/2")).unwrap();
    let seed = include_str!("../../../../fixtures/parity/mcp-proposals/build-planning/seed.sql")
        .replace("<data-dir>", root.path().to_str().unwrap());
    rusqlite::Connection::open(&database)
        .unwrap()
        .execute_batch(&seed)
        .unwrap();
    (root, database)
}
fn addon_layers(database: &std::path::Path) -> Vec<i64> {
    let db = rusqlite::Connection::open(database).unwrap();
    let mut query = db
        .prepare("SELECT project_id FROM profile_layers WHERE tenant_id='default' AND profile_id=1 AND layer_type='addon' ORDER BY id")
        .unwrap();
    query
        .query_map([], |r| r.get(0))
        .unwrap()
        .collect::<rusqlite::Result<Vec<i64>>>()
        .unwrap()
}
/// GRE-1423: a legacy Source stored with surrounding whitespace proposes under
/// its stored name, and confirming that proposal attaches the same Source.
#[test]
fn mcp_planning_whitespace_source_names_propose_and_confirm() {
    for stored in ["  Addon", "Addon  ", "\u{a0}Addon\t"] {
        for typed in ["Addon", stored] {
            let (root, database) = seeded();
            rusqlite::Connection::open(&database)
                .unwrap()
                .execute("UPDATE projects SET name=?1 WHERE id=2", [stored])
                .unwrap();
            let (owner, _) = WriterOwner::open(root.path(), Limits::default()).unwrap();
            let proposal = propose(&owner, "add_addon", json!({"source_name":typed}));
            assert_eq!(proposal["source_name"], stored, "{stored:?} {typed:?}");
            assert_eq!(proposal.get("error"), None, "{proposal}");
            let confirmed = run(
                &owner,
                "add_addon",
                json!({"source_name":proposal["source_name"]}),
            );
            assert_eq!(confirmed["ok"], true, "{stored:?} {typed:?}: {confirmed}");
            owner.shutdown().unwrap();
            assert_eq!(addon_layers(&database), [2], "{stored:?} {typed:?}");
        }
    }
}
/// GRE-1423: when the approved name no longer exists, confirmation refuses even
/// though a Source whose name differs only by whitespace is still there, and it
/// writes nothing.
#[test]
fn mcp_planning_confirm_refuses_whitespace_lookalike_without_writing() {
    for (approved, lookalike) in [
        (" Addon", "Addon"),
        ("Addon ", "Addon"),
        ("Addon", " Addon"),
        ("Addon", "Addon "),
    ] {
        let (root, database) = seeded();
        std::fs::create_dir_all(root.path().join("repos/4")).unwrap();
        let db = rusqlite::Connection::open(&database).unwrap();
        db.execute("UPDATE projects SET name=?1 WHERE id=2", [approved])
            .unwrap();
        db.execute(
            "INSERT INTO projects(id,tenant_id,name,url,source_type,branch,local_path,last_synced_at) VALUES(4,'default',?1,'','local','main',?2,'2026-11-01T00:00:00.000Z')",
            rusqlite::params![lookalike, root.path().join("repos/4").to_str().unwrap()],
        )
        .unwrap();
        drop(db);
        let (owner, _) = WriterOwner::open(root.path(), Limits::default()).unwrap();
        let proposal = propose(&owner, "add_addon", json!({"source_name":approved}));
        assert_eq!(proposal["source_name"], approved, "{proposal}");
        // The approved Source goes away before the user confirms.
        rusqlite::Connection::open(&database)
            .unwrap()
            .execute("DELETE FROM projects WHERE id=2", [])
            .unwrap();
        let before = pp_testkit::mcp_planning::state(&database).unwrap();
        let params = json!({"source_name":proposal["source_name"],"suggested_excludes":["x.stl"]});
        let refused = run(&owner, "add_addon", params.clone());
        assert_eq!(
            refused,
            json!({"ok":false,"detail":format!("Source not found: {}", approved.trim())}),
            "{approved:?} vs {lookalike:?}"
        );
        assert_eq!(params["source_name"], approved);
        owner.shutdown().unwrap();
        assert_eq!(before, pp_testkit::mcp_planning::state(&database).unwrap());
        assert_eq!(addon_layers(&database), Vec::<i64>::new());
    }
}
