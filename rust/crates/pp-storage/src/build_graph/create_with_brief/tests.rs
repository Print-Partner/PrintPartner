use super::*;
use crate::{
    Limits, Setting, SettingCommand, SettingSnapshot, WriterOwner,
    auth::{
        AuthFailure, AuthPolicy, FirstUserTenant, Outcome, RegistrationPolicy, Request, Secret,
        SessionTenantPolicy,
    },
};
use std::sync::Arc;

const WAIT: Duration = Duration::from_secs(5);
static IDLE: AtomicBool = AtomicBool::new(false);
const POLICY: AuthPolicy = AuthPolicy {
    registration: RegistrationPolicy::FirstAccountOnly,
    first_user: FirstUserTenant::NewUser,
    session_tenant: SessionTenantPolicy::SingleAccountDefault,
};
const TENANT: &str = "default";

struct Fixture {
    _directory: tempfile::TempDir,
    owner: WriterOwner,
    client: BuildGraphClient,
    token: String,
}

impl Fixture {
    /// A fresh installation with the Orders Module on.
    fn new() -> Self {
        let directory = tempfile::tempdir().unwrap();
        let (owner, _) = WriterOwner::open(directory.path(), Limits::default()).unwrap();
        let register = Request::Register {
            email: "builds@example.com".into(),
            display_name: "Builds".into(),
            password: Secret::new("correct-horse".into()),
        };
        let auth = owner.auth(FirstUserTenant::NewUser);
        let registered = auth.submit(register, Arc::default(), WAIT).unwrap();
        let Outcome::Session { token, .. } = registered.recv().unwrap().unwrap() else {
            panic!("registration returns a session")
        };
        let fixture = Self {
            client: owner.build_graph_with_policy(POLICY).unwrap(),
            token: token.expose().to_owned(),
            owner,
            _directory: directory,
        };
        fixture.seed("feature_toggles", r#"{"orders":true}"#);
        fixture
    }

    fn session(&self) -> Credential {
        Credential::Session(Secret::new(self.token.clone()))
    }

    fn create(&self, new: NewBuild) -> Result<CreatedBuild, CreateBuildError> {
        self.client
            .create_with_brief(self.session(), new, &IDLE, WAIT)
    }

    fn check(&self, new: &NewBuild) -> Result<(), CreateBuildError> {
        self.client
            .check_create_with_brief(self.session(), new, WAIT)
    }

    fn seed(&self, key: &str, value: &str) {
        let setting = Setting {
            tenant: TENANT.into(),
            key: key.into(),
            value: value.into(),
        };
        let reply = self
            .owner
            .client()
            .submit(SettingCommand::Set(setting), &IDLE, WAIT);
        assert!(reply.unwrap().recv().unwrap().unwrap());
    }

    fn stored(&self, key: &str) -> Option<String> {
        let reader = self.owner.client().reader(WAIT).unwrap();
        match reader.snapshot(TENANT, key).unwrap() {
            SettingSnapshot::Stored { value } => Some(value),
            SettingSnapshot::Missing => None,
        }
    }

    /// Every Build's id, name and special request, by id.
    fn builds(&self) -> Vec<(i64, String, Option<String>)> {
        let reader = self.owner.client().reader(WAIT).unwrap();
        let connection = reader.connection.as_ref().unwrap();
        let mut statement = connection
            .prepare("SELECT id,name,special_request FROM build_profiles ORDER BY id")
            .unwrap();
        statement
            .query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))
            .unwrap()
            .collect::<rusqlite::Result<_>>()
            .unwrap()
    }

    /// Whether any brief or idempotency record is stored.
    fn planning_settings(&self) -> i64 {
        let reader = self.owner.client().reader(WAIT).unwrap();
        reader
            .connection
            .as_ref()
            .unwrap()
            .query_row(
                "SELECT COUNT(*) FROM app_settings WHERE key LIKE 'build_planning.%'",
                [],
                |row| row.get(0),
            )
            .unwrap()
    }
}

fn new_build(name: &str, request: &str, key: Option<&str>) -> NewBuild {
    NewBuild::parse(name, request, key).unwrap()
}

#[test]
fn parse_trims_and_refuses_blank_fields() {
    let new = new_build("  Voron for Sam ", "\n Build a Voron 2.4.\n", Some("  k1 "));
    assert_eq!(new.name(), "Voron for Sam");
    assert_eq!(new.request(), "Build a Voron 2.4.");
    assert_eq!(new.idempotency_key(), Some("k1"));
    assert_eq!(new_build("A", "r", Some("  ")).idempotency_key(), None);
    let refused = |name: &str, request: &str, key: Option<&str>| {
        NewBuild::parse(name, request, key).unwrap_err()
    };
    assert_eq!(refused(" \u{3000}", "r", None), InvalidNewBuild::BlankName);
    assert_eq!(refused("A", " \t\n", None), InvalidNewBuild::BlankRequest);
    assert_eq!(
        refused(&"n".repeat(4097), "r", None),
        InvalidNewBuild::NameTooLong
    );
    let long_key = "k".repeat(MAX_IDEMPOTENCY_KEY_CHARS + 1);
    assert_eq!(
        refused("A", "r", Some(&long_key)),
        InvalidNewBuild::IdempotencyKeyTooLong
    );
    assert!(NewBuild::parse("A", "r", Some(&long_key[1..])).is_ok());
}

#[test]
fn create_stores_the_build_its_request_and_a_new_brief() {
    let fixture = Fixture::new();
    let new = new_build("Voron for Sam", "  Black and red, ABS.  ", Some("sam-1"));
    fixture.check(&new).unwrap();
    assert_eq!(fixture.planning_settings(), 0, "the check writes nothing");

    let created = fixture.create(new).unwrap();
    assert_eq!(created.name, "Voron for Sam");
    assert!(!created.replayed);
    let id = created.id.get() as i64;
    assert_eq!(
        fixture.builds(),
        vec![(
            id,
            "Voron for Sam".to_owned(),
            Some("Black and red, ABS.".to_owned())
        )]
    );

    let text = fixture.stored(&format!("build_planning.v1.{id}")).unwrap();
    let stored: serde_json::Value = serde_json::from_str(&text).unwrap();
    let fields = stored.as_object().unwrap();
    assert!(!fields.contains_key("draft_id"), "{text}");
    assert!(fields.values().all(|field| !field.is_null()), "{text}");
    let brief = BuildPlanningBrief::parse(&text, id).unwrap();
    assert_eq!(brief.special_request, "Black and red, ABS.");
    assert!(brief.requirements.is_empty() && brief.evidence.is_empty());
    assert_eq!(brief.created_at, brief.updated_at);
    assert_eq!(
        brief.stored_text().unwrap(),
        text,
        "one canonical stored form"
    );
    assert_eq!(
        fixture.stored("build_planning.create.sam-1"),
        Some(id.to_string())
    );
}

#[test]
fn a_recorded_key_replays_its_build_and_writes_nothing() {
    let fixture = Fixture::new();
    let first = fixture
        .create(new_build("Voron for Sam", "request", Some("k")))
        .unwrap();
    let again = new_build("Voron for Sam", "other request", Some("k"));
    fixture.check(&again).unwrap();
    let replay = fixture.create(again).unwrap();
    assert_eq!(replay.id, first.id);
    assert!(replay.replayed);
    assert_eq!(fixture.builds().len(), 1);
    let brief = fixture
        .stored(&format!("build_planning.v1.{}", first.id.get()))
        .unwrap();
    assert!(brief.contains("\"special_request\":\"request\""), "{brief}");
}

#[test]
fn a_key_whose_build_is_gone_creates_a_new_build_and_moves_the_key() {
    let fixture = Fixture::new();
    fixture.seed("build_planning.create.k", "999");
    let created = fixture.create(new_build("A", "r", Some("k"))).unwrap();
    assert!(!created.replayed);
    assert_ne!(created.id.get(), 999);
    assert_eq!(
        fixture.stored("build_planning.create.k"),
        Some(created.id.get().to_string())
    );
}

#[test]
fn refusals_write_nothing() {
    let fixture = Fixture::new();
    fixture.create(new_build("Taken", "r", None)).unwrap();
    let before = (fixture.builds(), fixture.planning_settings());
    let taken = new_build("Taken", "r", Some("k"));
    assert!(matches!(
        fixture.check(&taken),
        Err(CreateBuildError::NameTaken)
    ));
    assert!(matches!(
        fixture.create(taken),
        Err(CreateBuildError::NameTaken)
    ));

    fixture.seed("build_planning.create.bad", "not an id");
    let bad = new_build("Fresh", "r", Some("bad"));
    assert!(matches!(
        fixture.check(&bad),
        Err(CreateBuildError::IdempotencyRecordInvalid)
    ));
    assert!(matches!(
        fixture.create(bad),
        Err(CreateBuildError::IdempotencyRecordInvalid)
    ));
    assert_eq!(fixture.builds(), before.0);
    assert_eq!(fixture.planning_settings(), before.1 + 1);
}

#[test]
fn the_orders_module_must_be_on_even_for_a_replay() {
    let fixture = Fixture::new();
    fixture.create(new_build("A", "r", Some("k"))).unwrap();
    fixture.seed("feature_toggles", r#"{"orders":false}"#);
    for new in [new_build("A", "r", Some("k")), new_build("B", "r", None)] {
        assert!(matches!(
            fixture.check(&new),
            Err(CreateBuildError::OrdersOff)
        ));
        assert!(matches!(
            fixture.create(new),
            Err(CreateBuildError::OrdersOff)
        ));
    }
    fixture.seed("feature_toggles", r#"{"orders":"yes"}"#);
    assert!(matches!(
        fixture.create(new_build("B", "r", None)),
        Err(CreateBuildError::ModuleSettingsInvalid)
    ));
    assert_eq!(fixture.builds().len(), 1);
}

#[test]
fn a_refused_credential_writes_nothing() {
    let fixture = Fixture::new();
    let stranger = Credential::Session(Secret::new("x".repeat(64)));
    let refused =
        fixture
            .client
            .create_with_brief(stranger, new_build("A", "r", None), &IDLE, WAIT);
    assert!(
        matches!(
            refused,
            Err(CreateBuildError::Writer(WriterFailure::Auth(
                AuthFailure::SessionRequired
            )))
        ),
        "{refused:?}"
    );
    assert!(fixture.builds().is_empty());
}

#[test]
fn a_failure_after_the_insert_rolls_the_whole_create_back() {
    let fixture = Fixture::new();
    let database = fixture
        ._directory
        .path()
        .join(crate::archive::DATABASE_FILE);
    let connection = rusqlite::Connection::open(database).unwrap();
    connection.busy_timeout(WAIT).unwrap();
    connection
        .execute_batch(
            "CREATE TRIGGER refuse_brief BEFORE INSERT ON app_settings \
             WHEN NEW.key LIKE 'build_planning.v1.%' BEGIN SELECT RAISE(ABORT,'refused'); END;",
        )
        .unwrap();
    let failed = fixture.create(new_build("A", "r", Some("k")));
    assert!(
        matches!(
            failed,
            Err(CreateBuildError::Writer(WriterFailure::Internal(_)))
        ),
        "{failed:?}"
    );
    assert!(fixture.builds().is_empty());
    assert_eq!(fixture.planning_settings(), 0);
}
