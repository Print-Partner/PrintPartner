use super::*;
use crate::{
    Limits, SettingSnapshot, WriterOwner,
    auth::{AuthPolicy, FirstUserTenant, RegistrationPolicy, Secret, SessionTenantPolicy},
    build_graph::NewBuild,
    write::test_support::{WAIT, first_session, seed_setting},
};
use serde_json::{Value, json};

static IDLE: AtomicBool = AtomicBool::new(false);
const POLICY: AuthPolicy = AuthPolicy {
    registration: RegistrationPolicy::FirstAccountOnly,
    first_user: FirstUserTenant::NewUser,
    session_tenant: SessionTenantPolicy::SingleAccountDefault,
};

struct Fixture {
    _directory: tempfile::TempDir,
    owner: WriterOwner,
    client: BuildGraphClient,
    token: String,
    build: PositiveId,
}

impl Fixture {
    /// A Build whose brief `create_with_brief` stored, then given evidence
    /// `e1` (Source 11) and `e2` (Source 12) with no roles and draft 5.
    fn new() -> Self {
        let directory = tempfile::tempdir().unwrap();
        let (owner, _) = WriterOwner::open(directory.path(), Limits::default()).unwrap();
        let token = first_session(&owner);
        seed_setting(&owner, "feature_toggles", r#"{"orders":true}"#);
        let client = owner.build_graph_with_policy(POLICY).unwrap();
        let session = Credential::Session(Secret::new(token.clone()));
        let new = NewBuild::parse("Voron", "A Voron 2.4", None).unwrap();
        let build = client
            .create_with_brief(session, new, &IDLE, WAIT)
            .unwrap()
            .id;
        let fixture = Self {
            _directory: directory,
            owner,
            client,
            token,
            build,
        };
        let mut brief = fixture.brief();
        brief["evidence"] = json!([evidence("e1", 11), evidence("e2", 12)]);
        brief["draft_id"] = json!(5);
        fixture.store(&brief);
        fixture
    }

    fn session(&self) -> Credential {
        Credential::Session(Secret::new(self.token.clone()))
    }

    fn key(&self) -> String {
        format!("build_planning.v1.{}", self.build.get())
    }

    fn brief(&self) -> Value {
        let reader = self.owner.client().reader(WAIT).unwrap();
        match reader.snapshot("default", &self.key()).unwrap() {
            SettingSnapshot::Stored { value } => serde_json::from_str(&value).unwrap(),
            SettingSnapshot::Missing => panic!("no brief"),
        }
    }

    fn store(&self, brief: &Value) {
        seed_setting(&self.owner, &self.key(), &brief.to_string());
    }

    fn set(&self, changes: &[(&str, SourceRole)]) -> Result<Vec<RoleSet>, SourceRolesError> {
        let changes = changes
            .iter()
            .map(|(id, role)| RoleChange::new(id, *role))
            .collect();
        self.client
            .set_source_roles(self.session(), self.build, changes, &IDLE, WAIT)
    }
}

fn evidence(id: &str, source: i64) -> Value {
    let url = format!("https://example.test/{id}");
    json!({ "id": id, "url": url, "normalized_url": url, "kind": "canonical_design", "source_id": source })
}

fn roles(brief: &Value) -> Vec<Value> {
    brief["evidence"]
        .as_array()
        .unwrap()
        .iter()
        .map(|item| item["source_role"].clone())
        .collect()
}

#[test]
fn set_source_roles_sets_each_role_and_clears_the_selected_draft() {
    let fixture = Fixture::new();
    let before = fixture.brief();
    fixture
        .client
        .check_source_roles(fixture.session(), fixture.build, WAIT)
        .unwrap();
    assert_eq!(fixture.brief(), before, "the check writes nothing");

    let set = fixture
        .set(&[
            ("e1", SourceRole::StructuralBase),
            (" e2 ", SourceRole::Overlay),
            ("e2", SourceRole::Addon),
        ])
        .unwrap();
    assert_eq!(
        set,
        [
            RoleSet {
                evidence_id: "e1".into(),
                source_id: Some(11),
                role: SourceRole::StructuralBase,
            },
            RoleSet {
                evidence_id: "e2".into(),
                source_id: Some(12),
                role: SourceRole::Addon,
            },
        ]
    );
    let after = fixture.brief();
    assert_eq!(roles(&after), [json!("structural_base"), json!("addon")]);
    assert!(after.get("draft_id").is_none(), "{after}");
    assert_ne!(after["updated_at"], before["updated_at"]);
    let mut unchanged = after.clone();
    for field in ["evidence", "updated_at"] {
        unchanged[field] = before[field].clone();
    }
    unchanged["draft_id"] = json!(5);
    assert_eq!(unchanged, before, "only roles, draft and stamp change");
}

#[test]
fn set_source_roles_refuses_and_stores_nothing() {
    let fixture = Fixture::new();
    let before = fixture.brief();
    assert!(matches!(
        fixture.set(&[("e1", SourceRole::Addon), ("e9", SourceRole::Addon)]),
        Err(SourceRolesError::EvidenceNotFound(id)) if id == "e9"
    ));
    assert!(matches!(
        fixture.set(&[
            ("e1", SourceRole::StructuralBase),
            ("e2", SourceRole::StructuralBase)
        ]),
        Err(SourceRolesError::MultipleStructuralBases)
    ));
    assert_eq!(fixture.brief(), before);

    // A base already in the brief counts too.
    fixture.set(&[("e1", SourceRole::StructuralBase)]).unwrap();
    let based = fixture.brief();
    assert!(matches!(
        fixture.set(&[("e2", SourceRole::StructuralBase)]),
        Err(SourceRolesError::MultipleStructuralBases)
    ));
    assert_eq!(fixture.brief(), based);
}

#[test]
fn source_roles_need_a_brief() {
    let fixture = Fixture::new();
    let other = PositiveId::new(fixture.build.get() + 1).unwrap();
    assert!(matches!(
        fixture
            .client
            .check_source_roles(fixture.session(), other, WAIT),
        Err(SourceRolesError::NoBrief)
    ));
    let changes = vec![RoleChange::new("e1", SourceRole::Addon)];
    assert!(matches!(
        fixture
            .client
            .set_source_roles(fixture.session(), other, changes, &IDLE, WAIT),
        Err(SourceRolesError::NoBrief)
    ));
    seed_setting(&fixture.owner, &fixture.key(), "{\"version\":1}");
    assert!(matches!(
        fixture
            .client
            .check_source_roles(fixture.session(), fixture.build, WAIT),
        Err(SourceRolesError::BriefInvalid)
    ));
}

#[test]
fn source_roles_need_a_valid_credential() {
    let fixture = Fixture::new();
    let before = fixture.brief();
    let stranger = || Credential::Session(Secret::new("not-a-session".into()));
    assert!(matches!(
        fixture
            .client
            .check_source_roles(stranger(), fixture.build, WAIT),
        Err(SourceRolesError::Writer(WriterFailure::Auth(_)))
    ));
    let changes = vec![RoleChange::new("e1", SourceRole::Addon)];
    assert!(matches!(
        fixture
            .client
            .set_source_roles(stranger(), fixture.build, changes, &IDLE, WAIT),
        Err(SourceRolesError::Writer(WriterFailure::Auth(_)))
    ));
    assert_eq!(fixture.brief(), before);
}
