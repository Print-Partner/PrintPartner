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
fn policy() -> AuthPolicy {
    AuthPolicy {
        registration: RegistrationPolicy::Open,
        session_tenant: SessionTenantPolicy::AccountTenant,
        first_user: FirstUserTenant::NewUser,
    }
}
fn run(owner: &WriterOwner, tool: &str, params: Value) -> Value {
    let action = json!({"type":tool,"params":params,"label":"Fixture","summary":"Confirmed"});
    let outcome = owner
        .build_graph_with_policy(policy())
        .unwrap()
        .execute(
            Credential::Session(Secret::new("required-unit-fixture-secret".into())),
            BuildCommand::Mcp(Request::new(1, tool.into(), params, Some(action))),
            Arc::new(AtomicBool::new(false)),
            Duration::from_secs(2),
        )
        .unwrap();
    let BuildOutcome::Mcp(result) = outcome else {
        panic!("MCP outcome")
    };
    result
}
#[test]
fn mcp_planning_confirmations_survive_restart_without_changing_accepted_history() {
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
    let addon = run(
        &owner,
        "add_addon",
        json!({"source_name":"Addon","suggested_excludes":["stock.stl"]}),
    );
    assert_eq!(addon["ok"], true);
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
    assert_eq!(read("kit_manifest_1")["exclude"], json!(["stock.stl"]));
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
