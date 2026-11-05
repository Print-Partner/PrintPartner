use super::super::*;
use crate::auth::{AuthFailure, AuthInputFailure, Secret};
use sha2::{Digest, Sha256};

const ACCOUNT: &str = "publication-owner";
const SESSION: &str = "publication-policy-session";

fn policy(session_tenant: auth::SessionTenantPolicy) -> auth::AuthPolicy {
    auth::AuthPolicy {
        registration: auth::RegistrationPolicy::FirstAccountOnly,
        session_tenant,
        first_user: auth::FirstUserTenant::NewUser,
    }
}

fn with_owner(tenant: &str, test: impl FnOnce(&WriterOwner, &Connection, &Value)) {
    let data = super::golden();
    let mut case = data["transactions"][0].clone();
    for (table, rows) in case["before"].as_object_mut().unwrap() {
        if table == "app_settings" {
            continue;
        }
        for row in rows.as_array_mut().unwrap() {
            if let Some(value) = row.get_mut("tenant_id") {
                *value = json!(tenant);
            }
        }
    }
    let c = super::transaction::database(&json!({"definitions": data["transactionSchema"]}), &case);
    c.execute_batch("DROP TABLE plan_apply_admissions; DROP TABLE IF EXISTS domain_events; UPDATE app_settings SET value='35' WHERE key='schema_version' AND tenant_id='default'").unwrap();
    c.execute("INSERT INTO users(id,display_name,is_admin,created_at) VALUES(?1,'Publication owner',1,'2026-09-06T00:00:00.000Z')", [ACCOUNT]).unwrap();
    c.execute("INSERT INTO sessions(id,user_id,expires_at,provider) VALUES(?1,?2,'2099-01-01T00:00:00.000Z','email')", params![hex::encode(Sha256::digest(SESSION)), ACCOUNT]).unwrap();
    let dir = crate::tests::directory("publication-policy");
    c.backup("main", dir.join("print-partner.db"), None)
        .unwrap();
    drop(c);
    let (owner, _) = WriterOwner::open(&dir, crate::Limits::default()).unwrap();
    let c = Connection::open(dir.join("print-partner.db")).unwrap();
    test(&owner, &c, &data["clients"][0]);
    owner.shutdown().unwrap();
    drop(c);
    std::fs::remove_dir_all(dir).unwrap();
}

fn apply(client: &PublicationClient, case: &Value) -> Outcome {
    client
        .apply(
            PublicationCommand::new(
                serde_json::from_value(case["profile"].clone()).unwrap(),
                serde_json::from_value(case["draft"].clone()).unwrap(),
                serde_json::from_value(case["request"].clone()).unwrap(),
                "policy-key".into(),
                Credential::Session(Secret::new(SESSION.into())),
            )
            .unwrap(),
            &AtomicBool::new(false),
            Duration::from_secs(2),
        )
        .unwrap()
}

fn assert_publication(session_tenant: auth::SessionTenantPolicy, tenant: &str) {
    with_owner(tenant, |owner, c, case| {
        let client = owner
            .publication_with_policy(policy(session_tenant))
            .unwrap();
        assert!(matches!(apply(&client, case), Outcome::Applied { .. }));
        let stored: (String, String, String) = c
            .query_row(
                "SELECT tenant_id,actor_id,idempotency_key FROM plan_apply_requests",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .unwrap();
        assert_eq!(stored, (tenant.into(), ACCOUNT.into(), "policy-key".into()));
        let revision: (String, String, String) = c
            .query_row(
                "SELECT tenant_id,created_by,accepted_by FROM plan_revisions",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .unwrap();
        assert_eq!(revision, (tenant.into(), ACCOUNT.into(), ACCOUNT.into()));
    });
}

#[test]
fn owner_policy_publishes_to_default_for_single_account_with_account_actor() {
    assert_publication(auth::SessionTenantPolicy::SingleAccountDefault, "default");
}

#[test]
fn owner_policy_publishes_to_account_tenant_with_account_actor() {
    assert_publication(
        auth::SessionTenantPolicy::AccountTenant,
        "publication-owner",
    );
}

#[test]
fn owner_rejects_invalid_policy_before_writes() {
    with_owner("default", |owner, c, case| {
        let version = || {
            c.query_row("PRAGMA data_version", [], |r| r.get::<_, i64>(0))
                .unwrap()
        };
        let before = version();
        let mut invalid = policy(auth::SessionTenantPolicy::SingleAccountDefault);
        invalid.first_user = auth::FirstUserTenant::ClaimDefault;
        let error = owner.publication_with_policy(invalid).err().unwrap();
        assert!(matches!(
            error.downcast_ref::<AuthFailure>(),
            Some(AuthFailure::InvalidInput(AuthInputFailure::Policy))
        ));
        assert_eq!(error.to_string(), "Invalid authentication policy");
        assert_eq!(version(), before);
        assert_eq!(
            c.query_row("SELECT count(*) FROM plan_apply_requests", [], |r| r
                .get::<_, i64>(0))
                .unwrap(),
            0
        );
        let client = owner
            .publication_with_policy(policy(auth::SessionTenantPolicy::SingleAccountDefault))
            .unwrap();
        assert!(matches!(apply(&client, case), Outcome::Applied { .. }));
    });
}
