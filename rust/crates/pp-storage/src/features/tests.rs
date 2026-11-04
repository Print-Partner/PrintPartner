use super::*;
use crate::{
    Limits,
    auth::{FirstUserTenant, Outcome, Request, Secret},
};
use pp_domain::features::Source;
use std::{path::PathBuf, sync::atomic::AtomicBool};

static ACTIVE: AtomicBool = AtomicBool::new(false);

const WAIT: Duration = Duration::from_secs(5);
const DEFAULTS: [(&str, bool, Source); 4] = [
    ("farm", false, Source::Default),
    ("orders", false, Source::Default),
    ("bambu", false, Source::Default),
    ("spoolman", false, Source::Default),
];

fn account_policy() -> auth::AuthPolicy {
    auth::AuthPolicy {
        registration: auth::RegistrationPolicy::Open,
        session_tenant: auth::SessionTenantPolicy::AccountTenant,
        first_user: FirstUserTenant::NewUser,
    }
}

struct Account {
    tenant: String,
    token: String,
}
impl Account {
    fn session(&self) -> Credential {
        Credential::Session(Secret::new(self.token.clone()))
    }
}

struct Fixture {
    root: PathBuf,
    owner: Option<WriterOwner>,
    client: FeatureClient,
}
impl Fixture {
    fn new() -> Self {
        let root = std::env::temp_dir().join(format!(
            "pp-features-{}",
            hex::encode(rand::random::<[u8; 16]>())
        ));
        let (owner, _) = WriterOwner::open(&root, Limits::default()).unwrap();
        let client = owner.features_with_policy(account_policy()).unwrap();
        Self {
            root,
            owner: Some(owner),
            client,
        }
    }
    fn auth(&self, request: Request) -> Outcome {
        self.owner
            .as_ref()
            .unwrap()
            .auth(FirstUserTenant::NewUser)
            .submit(request, Arc::new(AtomicBool::new(false)), WAIT)
            .unwrap()
            .recv()
            .unwrap()
            .unwrap()
    }
    fn account(&self, name: &str) -> Account {
        let Outcome::Session { user, token } = self.auth(Request::Register {
            email: format!("{name}@example.com"),
            display_name: name.into(),
            password: Secret::new("correct-horse".into()),
        }) else {
            panic!("registration returns a session")
        };
        Account {
            tenant: user.tenant_id,
            token: token.expose().to_owned(),
        }
    }
    fn api_key(&self, account: &Account, routed_tenant: &str) -> Credential {
        let Outcome::KeyCreated { key, .. } = self.auth(Request::CreateKey {
            session: Secret::new(account.token.clone()),
        }) else {
            panic!("key creation returns the key")
        };
        Credential::ApiKey {
            routed_tenant: routed_tenant.into(),
            secret: key,
        }
    }
    fn raw(&self) -> Connection {
        let connection = Connection::open(self.root.join("print-partner.db")).unwrap();
        connection.busy_timeout(WAIT).unwrap();
        connection
    }
    fn plan(&self, account: &Account, name: &str) -> PositiveId {
        let raw = self.raw();
        raw.execute(
            "INSERT INTO build_profiles (tenant_id, name) VALUES (?1, ?2)",
            params![account.tenant, name],
        )
        .unwrap();
        PositiveId::new(u64::try_from(raw.last_insert_rowid()).unwrap()).unwrap()
    }
    fn rows(&self, account: &Account) -> Vec<(String, String)> {
        self.raw()
            .prepare(
                "SELECT key, value FROM app_settings
                 WHERE tenant_id=?1 AND key GLOB 'feature_toggles*' ORDER BY key",
            )
            .unwrap()
            .query_map([&account.tenant], |row| Ok((row.get(0)?, row.get(1)?)))
            .unwrap()
            .collect::<rusqlite::Result<_>>()
            .unwrap()
    }
    fn count(&self) -> i64 {
        self.raw()
            .query_row(
                "SELECT count(*) FROM app_settings WHERE key GLOB 'feature_toggles*'",
                [],
                |row| row.get(0),
            )
            .unwrap()
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

fn summary(view: &View) -> Vec<(&'static str, bool, Source)> {
    view.effective()
        .iter()
        .map(|resolved| (resolved.feature.key(), resolved.enabled, resolved.source))
        .collect()
}

fn row(key: &str, value: &str) -> (String, String) {
    (key.into(), value.into())
}

#[test]
fn fresh_tenant_reads_registry_defaults_without_writing() {
    let f = Fixture::new();
    let alice = f.account("alice");
    let view = f.client.read(alice.session(), None, &ACTIVE, WAIT).unwrap();
    assert_eq!(view.user(), &Toggles::default());
    assert_eq!(view.plan(), None);
    assert_eq!(summary(&view), DEFAULTS);
    assert_eq!(f.count(), 0);
}

#[test]
fn user_choice_applies_to_its_own_tenant_only() {
    let f = Fixture::new();
    let alice = f.account("alice");
    let bob = f.account("bob");
    let written = f
        .client
        .set(
            alice.session(),
            Scope::User,
            Feature::Farm,
            Some(true),
            &ACTIVE,
            WAIT,
        )
        .unwrap();
    assert_eq!(summary(&written)[0], ("farm", true, Source::User));
    assert_eq!(
        f.client.read(alice.session(), None, &ACTIVE, WAIT).unwrap(),
        written
    );
    assert_eq!(f.rows(&alice), [row("feature_toggles", r#"{"farm":true}"#)]);
    let other = f.client.read(bob.session(), None, &ACTIVE, WAIT).unwrap();
    assert_eq!(summary(&other), DEFAULTS);
}

#[test]
fn plan_override_applies_only_to_that_plan() {
    let f = Fixture::new();
    let alice = f.account("alice");
    let workshop = f.plan(&alice, "Workshop");
    let gifts = f.plan(&alice, "Gifts");
    f.client
        .set(
            alice.session(),
            Scope::User,
            Feature::Farm,
            Some(true),
            &ACTIVE,
            WAIT,
        )
        .unwrap();
    let written = f
        .client
        .set(
            alice.session(),
            Scope::Plan(workshop),
            Feature::Farm,
            Some(false),
            &ACTIVE,
            WAIT,
        )
        .unwrap();
    let mut farm_off = Toggles::default();
    farm_off.set(Feature::Farm, Some(false));
    assert_eq!(written.plan(), Some((workshop, &farm_off)));
    assert_eq!(summary(&written)[0], ("farm", false, Source::Plan));
    assert_eq!(
        f.client
            .read(alice.session(), Some(workshop), &ACTIVE, WAIT)
            .unwrap(),
        written
    );

    let user_view = f.client.read(alice.session(), None, &ACTIVE, WAIT).unwrap();
    assert_eq!(summary(&user_view)[0], ("farm", true, Source::User));
    let other_plan = f
        .client
        .read(alice.session(), Some(gifts), &ACTIVE, WAIT)
        .unwrap();
    assert_eq!(other_plan.plan(), Some((gifts, &Toggles::default())));
    assert_eq!(summary(&other_plan)[0], ("farm", true, Source::User));
    assert_eq!(
        f.rows(&alice),
        [
            row("feature_toggles", r#"{"farm":true}"#),
            row(
                &format!("feature_toggles:plan:{}", workshop.get()),
                r#"{"farm":false}"#
            ),
        ]
    );
}

#[test]
fn clearing_the_last_choice_deletes_the_layer_row() {
    let f = Fixture::new();
    let alice = f.account("alice");
    let plan = f.plan(&alice, "Workshop");
    for _ in 0..2 {
        f.client
            .set(
                alice.session(),
                Scope::User,
                Feature::Bambu,
                Some(true),
                &ACTIVE,
                WAIT,
            )
            .unwrap();
    }
    assert_eq!(f.count(), 1);
    f.client
        .set(
            alice.session(),
            Scope::Plan(plan),
            Feature::Bambu,
            Some(false),
            &ACTIVE,
            WAIT,
        )
        .unwrap();
    assert_eq!(f.count(), 2);

    let cleared = f
        .client
        .set(
            alice.session(),
            Scope::Plan(plan),
            Feature::Bambu,
            None,
            &ACTIVE,
            WAIT,
        )
        .unwrap();
    assert_eq!(cleared.plan(), Some((plan, &Toggles::default())));
    assert_eq!(summary(&cleared)[2], ("bambu", true, Source::User));
    assert_eq!(f.count(), 1);

    for _ in 0..2 {
        let view = f
            .client
            .set(
                alice.session(),
                Scope::User,
                Feature::Bambu,
                None,
                &ACTIVE,
                WAIT,
            )
            .unwrap();
        assert_eq!(summary(&view), DEFAULTS);
    }
    assert_eq!(f.count(), 0);
}

#[test]
fn foreign_and_missing_plans_are_not_found() {
    let f = Fixture::new();
    let alice = f.account("alice");
    let bob = f.account("bob");
    let alices_plan = f.plan(&alice, "Workshop");
    let missing = PositiveId::new(9_999).unwrap();
    for plan in [alices_plan, missing] {
        let read = f.client.read(bob.session(), Some(plan), &ACTIVE, WAIT);
        assert_eq!(read.unwrap_err().to_string(), "Plan not found");
        let set = f.client.set(
            bob.session(),
            Scope::Plan(plan),
            Feature::Farm,
            Some(true),
            &ACTIVE,
            WAIT,
        );
        assert_eq!(set.unwrap_err().to_string(), "Plan not found");
    }
    assert_eq!(f.count(), 0);

    f.client
        .set(
            alice.session(),
            Scope::Plan(alices_plan),
            Feature::Farm,
            Some(true),
            &ACTIVE,
            WAIT,
        )
        .unwrap();
    assert_eq!(f.count(), 1);
}

#[test]
fn credentials_resolve_to_the_owning_tenant() {
    let f = Fixture::new();
    let alice = f.account("alice");
    let bob = f.account("bob");
    let by_session = f
        .client
        .set(
            alice.session(),
            Scope::User,
            Feature::Spoolman,
            Some(true),
            &ACTIVE,
            WAIT,
        )
        .unwrap();
    let by_key = f
        .client
        .read(f.api_key(&alice, &alice.tenant), None, &ACTIVE, WAIT)
        .unwrap();
    assert_eq!(by_key, by_session);
    assert_eq!(summary(&by_key)[3], ("spoolman", true, Source::User));

    let misrouted = f
        .client
        .read(f.api_key(&alice, &bob.tenant), None, &ACTIVE, WAIT);
    assert_eq!(
        misrouted.unwrap_err().to_string(),
        "Authentication required"
    );
    let invalid = Credential::Session(Secret::new("not-a-session".into()));
    let set = f.client.set(
        invalid,
        Scope::User,
        Feature::Farm,
        Some(true),
        &ACTIVE,
        WAIT,
    );
    assert_eq!(set.unwrap_err().to_string(), "Authentication required");
    assert_eq!(
        f.rows(&alice),
        [row("feature_toggles", r#"{"spoolman":true}"#)]
    );
    assert_eq!(f.count(), 1);
}

#[test]
fn unreadable_stored_layer_fails_instead_of_resetting() {
    let f = Fixture::new();
    let alice = f.account("alice");
    f.raw()
        .execute(
            "INSERT INTO app_settings(tenant_id,key,value) VALUES(?1,'feature_toggles',?2)",
            params![alice.tenant, r#"{"farm":"yes"}"#],
        )
        .unwrap();
    let expected = r#"Stored feature_toggles is invalid: invalid type: string "yes", expected a boolean at line 1 column 13"#;
    let read = f.client.read(alice.session(), None, &ACTIVE, WAIT);
    assert_eq!(read.unwrap_err().to_string(), expected);
    let set = f.client.set(
        alice.session(),
        Scope::User,
        Feature::Orders,
        Some(true),
        &ACTIVE,
        WAIT,
    );
    assert_eq!(set.unwrap_err().to_string(), expected);
    assert_eq!(
        f.rows(&alice),
        [row("feature_toggles", r#"{"farm":"yes"}"#)]
    );
}

#[test]
fn stopped_storage_refuses_feature_reads() {
    let mut f = Fixture::new();
    let alice = f.account("alice");
    f.owner.take().unwrap().shutdown().unwrap();
    let read = f.client.read(alice.session(), None, &ACTIVE, WAIT);
    assert_eq!(read.unwrap_err().to_string(), "Storage stopped");
}

#[test]
fn future_values_survive_patch_clear_and_reopen() {
    let mut f = Fixture::new();
    let alice = f.account("alice");
    let plan = f.plan(&alice, "Workshop");
    for (scope, key) in [
        (Scope::User, "feature_toggles".to_owned()),
        (
            Scope::Plan(plan),
            format!("feature_toggles:plan:{}", plan.get()),
        ),
    ] {
        f.raw()
            .execute(
                "INSERT INTO app_settings(tenant_id,key,value) VALUES(?1,?2,?3)",
                params![
                    alice.tenant,
                    key,
                    r#"{"future_thing":{"mode":"x","limits":[1,null,true]}}"#
                ],
            )
            .unwrap();
        let written = f
            .client
            .set(
                alice.session(),
                scope,
                Feature::Orders,
                Some(true),
                &ACTIVE,
                WAIT,
            )
            .unwrap();
        assert!(written.effective().enabled(Feature::Orders));
        let value: String = f
            .raw()
            .query_row(
                "SELECT value FROM app_settings WHERE tenant_id=?1 AND key=?2",
                params![alice.tenant, key],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(
            value,
            r#"{"future_thing":{"mode":"x","limits":[1,null,true]},"orders":true}"#
        );
        let cleared = f
            .client
            .set(alice.session(), scope, Feature::Orders, None, &ACTIVE, WAIT)
            .unwrap();
        assert_eq!(summary(&cleared), DEFAULTS);
        let value: String = f
            .raw()
            .query_row(
                "SELECT value FROM app_settings WHERE tenant_id=?1 AND key=?2",
                params![alice.tenant, key],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(
            value,
            r#"{"future_thing":{"mode":"x","limits":[1,null,true]}}"#
        );
    }
    assert_eq!(f.count(), 2);
    let written = f
        .client
        .set(
            alice.session(),
            Scope::User,
            Feature::Orders,
            Some(true),
            &ACTIVE,
            WAIT,
        )
        .unwrap();
    let rows = f.rows(&alice);
    f.owner.take().unwrap().shutdown().unwrap();
    let (owner, _) = WriterOwner::open(&f.root, Limits::default()).unwrap();
    f.client = owner.features_with_policy(account_policy()).unwrap();
    f.owner = Some(owner);
    assert_eq!(
        f.client.read(alice.session(), None, &ACTIVE, WAIT).unwrap(),
        written
    );
    assert_eq!(f.rows(&alice), rows);
    assert_eq!(
        summary(
            &f.client
                .read(alice.session(), Some(plan), &ACTIVE, WAIT)
                .unwrap()
        )[1],
        ("orders", true, Source::User)
    );
}

#[test]
fn concurrent_distinct_patches_preserve_every_choice() {
    let f = Fixture::new();
    let alice = f.account("alice");
    let barrier = std::sync::Barrier::new(4);
    std::thread::scope(|threads| {
        for feature in Feature::ALL {
            let barrier = &barrier;
            let client = &f.client;
            let alice = &alice;
            threads.spawn(move || {
                barrier.wait();
                let view = client
                    .set(
                        alice.session(),
                        Scope::User,
                        feature,
                        Some(true),
                        &ACTIVE,
                        WAIT,
                    )
                    .unwrap();
                assert!(view.effective().enabled(feature));
            });
        }
    });
    let view = f.client.read(alice.session(), None, &ACTIVE, WAIT).unwrap();
    assert_eq!(
        summary(&view),
        [
            ("farm", true, Source::User),
            ("orders", true, Source::User),
            ("bambu", true, Source::User),
            ("spoolman", true, Source::User),
        ]
    );
    assert_eq!(
        f.rows(&alice),
        [row(
            "feature_toggles",
            r#"{"bambu":true,"farm":true,"orders":true,"spoolman":true}"#
        )]
    );
    let plan = f.plan(&alice, "Workshop");
    for feature in Feature::ALL {
        f.client
            .set(
                alice.session(),
                Scope::Plan(plan),
                feature,
                Some(false),
                &ACTIVE,
                WAIT,
            )
            .unwrap();
    }
    let view = f
        .client
        .read(alice.session(), Some(plan), &ACTIVE, WAIT)
        .unwrap();
    assert_eq!(
        summary(&view),
        [
            ("farm", false, Source::Plan),
            ("orders", false, Source::Plan),
            ("bambu", false, Source::Plan),
            ("spoolman", false, Source::Plan),
        ]
    );
    for feature in Feature::ALL {
        f.client
            .set(
                alice.session(),
                Scope::Plan(plan),
                feature,
                None,
                &ACTIVE,
                WAIT,
            )
            .unwrap();
    }
    let view = f
        .client
        .read(alice.session(), Some(plan), &ACTIVE, WAIT)
        .unwrap();
    assert_eq!(
        summary(&view),
        [
            ("farm", true, Source::User),
            ("orders", true, Source::User),
            ("bambu", true, Source::User),
            ("spoolman", true, Source::User),
        ]
    );
    assert_eq!(f.count(), 1);
}

#[test]
fn single_account_policy_uses_the_default_tenant() {
    let f = Fixture::new();
    let alice = f.account("alice");
    let policy = auth::AuthPolicy {
        registration: auth::RegistrationPolicy::FirstAccountOnly,
        session_tenant: auth::SessionTenantPolicy::SingleAccountDefault,
        first_user: FirstUserTenant::NewUser,
    };
    let client = f
        .owner
        .as_ref()
        .unwrap()
        .features_with_policy(policy)
        .unwrap();
    let default = Account {
        tenant: "default".into(),
        token: alice.token.clone(),
    };
    let plan = f.plan(&default, "Desktop");
    let view = client
        .set(
            alice.session(),
            Scope::User,
            Feature::Farm,
            Some(true),
            &ACTIVE,
            WAIT,
        )
        .unwrap();
    assert_eq!(summary(&view)[0], ("farm", true, Source::User));
    assert_eq!(
        f.rows(&default),
        [row("feature_toggles", r#"{"farm":true}"#)]
    );
    assert_eq!(f.rows(&alice).len(), 0);
    assert_eq!(
        summary(&f.client.read(alice.session(), None, &ACTIVE, WAIT).unwrap()),
        DEFAULTS
    );
    assert_eq!(
        client
            .read(alice.session(), Some(plan), &ACTIVE, WAIT)
            .unwrap()
            .plan(),
        Some((plan, &Toggles::default()))
    );
    let Outcome::KeyCreated { key, .. } = f
        .owner
        .as_ref()
        .unwrap()
        .auth_with_policy(policy)
        .unwrap()
        .submit(
            Request::CreateKey {
                session: Secret::new(alice.token.clone()),
            },
            Arc::new(AtomicBool::new(false)),
            WAIT,
        )
        .unwrap()
        .recv()
        .unwrap()
        .unwrap()
    else {
        panic!("key creation returns the key")
    };
    assert_eq!(
        client
            .read(
                Credential::ApiKey {
                    routed_tenant: "default".into(),
                    secret: key
                },
                None,
                &ACTIVE,
                WAIT
            )
            .unwrap(),
        view
    );
}

#[test]
fn expired_sessions_and_revoked_keys_cannot_patch() {
    let f = Fixture::new();
    let alice = f.account("alice");
    let key = f.api_key(&alice, &alice.tenant);
    f.raw()
        .execute(
            "UPDATE sessions SET expires_at='2000-01-01T00:00:00.000Z'",
            [],
        )
        .unwrap();
    for read in [
        f.client.read(alice.session(), None, &ACTIVE, WAIT),
        f.client.set(
            alice.session(),
            Scope::User,
            Feature::Farm,
            Some(true),
            &ACTIVE,
            WAIT,
        ),
    ] {
        assert_eq!(read.unwrap_err().to_string(), "Authentication required");
    }
    f.raw().execute(
        "UPDATE app_settings SET value=json_set(value,'$[0].isActive',json('false')) WHERE tenant_id=?1 AND key='api_keys_v1'",
        [&alice.tenant],
    ).unwrap();
    assert_eq!(
        f.client
            .set(key, Scope::User, Feature::Farm, Some(true), &ACTIVE, WAIT)
            .unwrap_err()
            .to_string(),
        "Authentication required"
    );
    assert_eq!(f.count(), 0);
}

#[test]
fn cancelled_before_admission_does_not_write() {
    let f = Fixture::new();
    let alice = f.account("alice");
    let cancelled = AtomicBool::new(true);
    assert_eq!(
        f.client
            .read(alice.session(), None, &cancelled, WAIT)
            .unwrap_err()
            .to_string(),
        "Cancelled before admission"
    );
    assert_eq!(
        f.client
            .set(
                alice.session(),
                Scope::User,
                Feature::Farm,
                Some(true),
                &cancelled,
                WAIT
            )
            .unwrap_err()
            .to_string(),
        "Cancelled before admission"
    );
    assert_eq!(f.count(), 0);
    let view = f
        .client
        .set(
            alice.session(),
            Scope::User,
            Feature::Farm,
            Some(true),
            &ACTIVE,
            WAIT,
        )
        .unwrap();
    assert_eq!(summary(&view)[0], ("farm", true, Source::User));
    assert_eq!(f.count(), 1);
}

#[test]
fn admitted_patch_waits_for_its_result_despite_cancellation_and_zero_wait() {
    let f = Fixture::new();
    let alice = f.account("alice");
    let mut raw = f.raw();
    let transaction = raw
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .unwrap();
    let blocker = f
        .owner
        .as_ref()
        .unwrap()
        .client()
        .submit(
            crate::SettingCommand::Set(crate::Setting {
                tenant: alice.tenant.clone(),
                key: "queue_probe".into(),
                value: "held".into(),
            }),
            &ACTIVE,
            WAIT,
        )
        .unwrap();
    let shared = &f.client.shared;
    let deadline = Instant::now() + WAIT;
    let mut queue = shared.queue.lock().unwrap();
    while !queue.pending.is_empty() {
        assert!(
            Instant::now() < deadline,
            "writer takes the blocking command"
        );
        queue = shared
            .changed
            .wait_timeout(queue, Duration::from_millis(5))
            .unwrap()
            .0;
    }
    drop(queue);
    let cancelled = AtomicBool::new(false);
    std::thread::scope(|threads| {
        let patch = threads.spawn(|| {
            f.client.set(
                alice.session(),
                Scope::User,
                Feature::Farm,
                Some(true),
                &cancelled,
                Duration::ZERO,
            )
        });
        let mut queue = shared.queue.lock().unwrap();
        while queue.pending.is_empty() {
            assert!(Instant::now() < deadline, "feature patch is admitted");
            queue = shared
                .changed
                .wait_timeout(queue, Duration::from_millis(5))
                .unwrap()
                .0;
        }
        cancelled.store(true, Ordering::Release);
        drop(queue);
        transaction.commit().unwrap();
        assert!(blocker.recv_timeout(WAIT).unwrap().unwrap());
        let view = patch.join().unwrap().unwrap();
        assert_eq!(summary(&view)[0], ("farm", true, Source::User));
    });
    assert_eq!(f.rows(&alice), [row("feature_toggles", r#"{"farm":true}"#)]);
}

/// `GET /health` Module state read from the same settings.
mod health_modules {
    use super::{Fixture, WAIT, params};
    use crate::features::{
        USER_KEY,
        modules::{ASSEMBLY_TRACKING_KEY, Module, ModuleOffReason, ModuleStatus},
    };

    const TENANT: &str = "default";

    fn put(f: &Fixture, tenant: &str, key: &str, value: &str) {
        f.raw()
            .execute(
                "INSERT INTO app_settings(tenant_id,key,value) VALUES(?1,?2,?3)
                 ON CONFLICT(tenant_id,key) DO UPDATE SET value=excluded.value",
                params![tenant, key, value],
            )
            .unwrap();
    }
    fn modules(f: &Fixture) -> Vec<ModuleStatus> {
        f.owner
            .as_ref()
            .unwrap()
            .client()
            .modules(TENANT, WAIT)
            .unwrap()
    }
    fn on(module: Module) -> ModuleStatus {
        ModuleStatus::On { module }
    }
    fn off(module: Module, reason: ModuleOffReason) -> ModuleStatus {
        ModuleStatus::Off { module, reason }
    }
    fn all_off(reason: ModuleOffReason) -> Vec<ModuleStatus> {
        Module::ALL.map(|module| off(module, reason)).to_vec()
    }

    #[test]
    fn fresh_install_lists_every_module_off_by_default() {
        let f = Fixture::new();
        assert_eq!(modules(&f), all_off(ModuleOffReason::DefaultOff));
        assert_eq!(
            serde_json::to_value(modules(&f)).unwrap()[0],
            serde_json::json!({"state": "off", "module": "farm", "reason": "default_off"})
        );
    }

    #[test]
    fn each_change_shows_on_the_next_read() {
        let f = Fixture::new();
        put(&f, TENANT, USER_KEY, r#"{"farm":true,"bambu":false}"#);
        put(&f, TENANT, ASSEMBLY_TRACKING_KEY, "1");
        assert_eq!(
            modules(&f),
            [
                on(Module::Farm),
                off(Module::Spoolman, ModuleOffReason::DefaultOff),
                off(Module::Orders, ModuleOffReason::DefaultOff),
                off(Module::Bambu, ModuleOffReason::TurnedOff),
                on(Module::AssemblyTracking),
            ]
        );
        // Same owner, no reopen: the next read sees the new settings.
        put(&f, TENANT, USER_KEY, r#"{"farm":false}"#);
        put(&f, TENANT, ASSEMBLY_TRACKING_KEY, "0");
        let now = modules(&f);
        assert_eq!(now[0], off(Module::Farm, ModuleOffReason::TurnedOff));
        assert_eq!(
            now[4],
            off(Module::AssemblyTracking, ModuleOffReason::TurnedOff)
        );
        // Empty matches Node's `getSetting(key, "0")` default.
        put(&f, TENANT, ASSEMBLY_TRACKING_KEY, "");
        assert_eq!(
            modules(&f)[4],
            off(Module::AssemblyTracking, ModuleOffReason::DefaultOff)
        );
    }

    #[test]
    fn reads_only_the_requested_tenant() {
        let f = Fixture::new();
        put(&f, "other", USER_KEY, r#"{"farm":true,"spoolman":true}"#);
        put(&f, "other", ASSEMBLY_TRACKING_KEY, "1");
        assert_eq!(modules(&f), all_off(ModuleOffReason::DefaultOff));
    }

    #[test]
    fn unreadable_setting_stays_off_and_echoes_nothing() {
        let f = Fixture::new();
        let canary = "pp-canary-sk-0123456789abcdef";
        put(&f, TENANT, USER_KEY, &format!(r#"{{"farm":"{canary}"}}"#));
        put(&f, TENANT, ASSEMBLY_TRACKING_KEY, canary);
        let read = modules(&f);
        assert_eq!(read[..4], all_off(ModuleOffReason::SettingUnreadable)[..4]);
        assert_eq!(read[4], on(Module::AssemblyTracking));
        let body = serde_json::to_string(&read).unwrap();
        assert!(!body.contains(canary), "{body}");
    }
}
