use super::*;
use crate::{
    Limits, SettingSnapshot,
    app_settings::{Key, Outcome as SettingOutcome, Write},
    auth::{FirstUserTenant, RegistrationPolicy, Secret, SessionTenantPolicy},
    printer_hosts,
    write::test_support::{first_session, seed_setting},
};
use serde_json::{Value, json};

const WAIT: Duration = Duration::from_secs(5);
static IDLE: AtomicBool = AtomicBool::new(false);
/// The desktop policy: the one account's session is admitted to `default`.
const POLICY: AuthPolicy = AuthPolicy {
    registration: RegistrationPolicy::FirstAccountOnly,
    first_user: FirstUserTenant::NewUser,
    session_tenant: SessionTenantPolicy::SingleAccountDefault,
};

type Changed = Result<IntegrationSummary, PrinterSetupError>;

struct Fixture {
    _directory: tempfile::TempDir,
    owner: WriterOwner,
    client: PrinterSetupClient,
    token: String,
}

impl Fixture {
    fn new() -> Self {
        let directory = tempfile::tempdir().unwrap();
        let (owner, _) = WriterOwner::open(directory.path(), Limits::default()).unwrap();
        let token = first_session(&owner);
        Self {
            client: owner.printer_setup(POLICY).unwrap(),
            token,
            owner,
            _directory: directory,
        }
    }

    fn session(&self) -> Credential {
        Credential::Session(Secret::new(self.token.clone()))
    }

    fn create(&self, new: NewIntegration) -> Result<IntegrationSummary, PrinterSetupError> {
        self.client
            .create_integration(self.session(), new, &IDLE, WAIT)
    }

    fn change(&self, id: &str, patch: IntegrationPatch) -> Changed {
        let (session, id) = (self.session(), id.into());
        self.client
            .update_integration(session, id, patch, &IDLE, WAIT)
    }

    fn delete(&self, id: &str) -> Result<IntegrationSummary, PrinterSetupError> {
        self.client
            .delete_integration(self.session(), id.into(), &IDLE, WAIT)
    }

    /// The stored document's text.
    fn stored(&self) -> Option<String> {
        let reader = self.owner.client().reader(WAIT).unwrap();
        match reader.snapshot("default", INTEGRATIONS_KEY).unwrap() {
            SettingSnapshot::Stored { value } => Some(value),
            SettingSnapshot::Missing => None,
        }
    }

    /// The text stored under another key; `None` when there is none.
    fn stored_at(&self, key: &str) -> Option<String> {
        let reader = self.owner.client().reader(WAIT).unwrap();
        reader.get_setting("default", key, None).unwrap()
    }

    fn seed(&self, text: &str) {
        seed_setting(&self.owner, INTEGRATIONS_KEY, text);
    }
}

fn new(kind: ServiceKind, base_url: &str) -> NewIntegration {
    NewIntegration {
        kind,
        name: " Voron ".into(),
        base_url: base_url.into(),
        enabled: true,
        username: None,
        api_key: None,
        password: None,
    }
}

/// The fields an `Invalid` refusal names, in order.
fn invalid_fields<T>(refused: Result<T, PrinterSetupError>, case: usize) -> Vec<(Field, Reason)> {
    let Err(PrinterSetupError::Invalid(invalid)) = refused else {
        panic!("case {case} was not refused as invalid")
    };
    invalid.iter().map(|one| (one.field, one.reason)).collect()
}

fn secret(value: &str) -> Option<Secret> {
    Some(Secret::new(value.into()))
}

#[test]
fn create_stores_rows_the_readers_accept_and_answers_without_secrets() {
    let fixture = Fixture::new();
    let moon = NewIntegration {
        api_key: secret("moon-key-canary"),
        ..new(ServiceKind::Moonraker, "http://moon.test:7125")
    };
    let moon = fixture.create(moon).unwrap();
    let expected = IntegrationSummary {
        id: moon.id.clone(),
        kind: ServiceKind::Moonraker,
        name: "Voron".into(),
        enabled: true,
        base_url: Some("http://moon.test:7125".into()),
        username: None,
        has_api_key: true,
        has_password: false,
    };
    assert_eq!(moon, expected);
    assert_eq!(moon.id.len(), 32);
    let prusa = NewIntegration {
        username: Some("maker".into()),
        password: secret("prusa-password-canary"),
        enabled: false,
        ..new(ServiceKind::Prusalink, "https://mk4.test")
    };
    let prusa = fixture.create(prusa).unwrap();
    assert!(prusa.has_password && !prusa.has_api_key && !prusa.enabled);

    let text = fixture.stored().unwrap();
    let rows: Value = serde_json::from_str(&text).unwrap();
    let stamp = rows[0]["created_at"].as_str().unwrap();
    assert!(stamp.len() == 24 && stamp.ends_with('Z'), "{stamp}");
    assert_eq!(
        rows[0],
        json!({"id": moon.id, "type": "moonraker", "name": "Voron",
            "config": {"base_url": "http://moon.test:7125", "enabled": true,
                "api_key": "moon-key-canary"},
            "created_at": stamp, "updated_at": stamp})
    );
    // The status watchers and the host lookup read what was written.
    let watched = printer_hosts::watched(Some(&text)).unwrap();
    let host = &watched.moonraker[0];
    let read = (host.id.as_str(), host.api_key.as_deref(), host.enabled);
    assert_eq!(read, (moon.id.as_str(), Some("moon-key-canary"), true));
    let host = &watched.prusalink[0];
    let read = (host.username.as_str(), host.password.as_deref());
    assert_eq!(read, ("maker", Some("prusa-password-canary")));
    assert_eq!((host.id.as_str(), host.enabled), (prusa.id.as_str(), false));
}

#[test]
fn an_invalid_value_names_its_field_and_stores_nothing() {
    use Field::{ApiKey, BaseUrl, Kind, Name, Password, Username};
    use Reason::{Empty, HasCredentials, NotCreatable, NotHttpUrl, NotUsedByKind, TooLong};
    let fixture = Fixture::new();
    let moon = |base_url: &str| new(ServiceKind::Moonraker, base_url);
    let long = format!(
        "http://moon.test/{}",
        "a".repeat(integrations::MAX_BASE_URL_BYTES)
    );
    let cases = [
        (
            new(ServiceKind::Bambu, "http://h2d.test"),
            vec![(Kind, NotCreatable)],
        ),
        (
            NewIntegration {
                name: "  ".into(),
                ..moon("ftp://moon.test")
            },
            vec![(Name, Empty), (BaseUrl, NotHttpUrl)],
        ),
        (moon("moon.test:7125"), vec![(BaseUrl, NotHttpUrl)]),
        (moon(" http://moon.test"), vec![(BaseUrl, NotHttpUrl)]),
        (moon("http://moon\n.test"), vec![(BaseUrl, NotHttpUrl)]),
        (
            moon("http://user:pw@moon.test"),
            vec![(BaseUrl, HasCredentials)],
        ),
        (moon(&long), vec![(BaseUrl, TooLong)]),
        (
            NewIntegration {
                name: "n".repeat(integrations::MAX_NAME_BYTES + 1),
                api_key: secret(""),
                ..moon("http://moon.test")
            },
            vec![(Name, TooLong), (ApiKey, Empty)],
        ),
        (
            NewIntegration {
                username: Some("maker".into()),
                password: secret("pw"),
                ..moon("http://moon.test")
            },
            vec![(Username, NotUsedByKind), (Password, NotUsedByKind)],
        ),
        (
            NewIntegration {
                api_key: secret("key"),
                ..new(ServiceKind::Prusalink, "http://mk4.test")
            },
            vec![(ApiKey, NotUsedByKind)],
        ),
    ];
    for (index, (request, expected)) in cases.into_iter().enumerate() {
        let named = invalid_fields(fixture.create(request), index);
        assert_eq!(named, expected, "case {index}");
    }
    assert_eq!(fixture.stored(), None);
}

#[test]
fn a_document_that_is_not_the_typed_shape_refuses_every_write_untouched() {
    use DocumentFault::{DuplicateId, NotAList, UnknownKind, WrongShape};
    let fixture = Fixture::new();
    let valid = json!({"id": "moon-1", "type": "moonraker", "name": "Voron",
        "config": {"base_url": "http://moon.test", "enabled": true},
        "created_at": "2026-11-12T03:04:05.678Z", "updated_at": "2026-11-12T03:04:05.678Z"});
    let aliased = json!({"id": "moon-2", "type": "moonraker", "name": "Old",
        "config": {"baseUrl": "http://old.test", "apiKey": "alias-key-canary", "enabled": true},
        "created_at": "2026-11-12T03:04:05.678Z", "updated_at": "2026-11-12T03:04:05.678Z"});
    let cases = [
        (json!({"id": "moon-1"}), None, NotAList),
        (
            json!([valid, {"id": "odd-1", "type": "mystery"}]),
            Some(1),
            UnknownKind,
        ),
        (json!([valid, aliased]), Some(1), WrongShape),
        (json!([{"type": "bambu"}]), Some(0), WrongShape),
        (json!([{"id": "", "type": "spoolman"}]), Some(0), WrongShape),
        (json!([valid, valid]), Some(1), DuplicateId),
    ];
    for (document, entry, reason) in cases {
        let text = document.to_string();
        fixture.seed(&text);
        let fault = InvalidDocument {
            document: SetupDocument::Integrations,
            entry,
            reason,
        };
        for refused in [
            fixture.create(new(ServiceKind::Moonraker, "http://moon.test")),
            fixture.change("moon-1", IntegrationPatch::default()),
            fixture.delete("moon-1"),
        ] {
            // The refusal names the entry and holds none of its text.
            let shown = format!("{refused:?}");
            assert!(!shown.contains("canary") && !shown.contains("moon"));
            let Err(PrinterSetupError::StoredDocumentInvalid(found)) = refused else {
                panic!("{text} took a write: {shown}")
            };
            assert_eq!(found, fault, "{text}");
        }
        assert_eq!(fixture.stored().as_deref(), Some(text.as_str()));
    }
}

/// A row the import wrote with keys the shape does not name, on the row and
/// in its config. No answer shows them and an edit stores them back.
#[test]
fn keys_the_shape_does_not_name_are_never_shown_and_an_edit_keeps_them_byte_for_byte() {
    let fixture = Fixture::new();
    let in_config = r#""port":7125,"legacy":{"token":"unread-canary","list":[1,2.5,null]}"#;
    let on_row = r#""capabilities":{"files":true,"note":"unread-canary"}"#;
    let stamp = "2026-11-12T03:04:05.678Z";
    let stored = format!(
        r#"[{{"id":"moon-1","type":"moonraker","name":"Voron","config":{{"base_url":"http://moon.test","enabled":true,"api_key":"moon-key",{in_config}}},"created_at":"{stamp}","updated_at":"{stamp}",{on_row}}}]"#
    );
    fixture.seed(&stored);
    let patch = IntegrationPatch {
        name: Some("Renamed".into()),
        enabled: Some(false),
        ..IntegrationPatch::default()
    };
    let changed = fixture.change("moon-1", patch).unwrap();
    assert_eq!((changed.name.as_str(), changed.enabled), ("Renamed", false));
    let shown = format!("{changed:?}");
    for trace in ["canary", "port", "7125", "legacy", "capabilities"] {
        assert!(!shown.contains(trace), "{trace} is in {shown}");
    }
    // The stored row differs only in what the edit set and its time.
    let after = fixture.stored().unwrap();
    let expected = stored
        .replace("Voron", "Renamed")
        .replace(r#""enabled":true"#, r#""enabled":false"#);
    let updated = after.find(r#""updated_at":""#).unwrap();
    assert_eq!(after[..updated], expected[..updated]);
    assert!(after.ends_with(&format!(r#"",{on_row}}}]"#)), "{after}");
    // The hosts the watchers read are the typed fields alone.
    let hosts = format!("{:?}", printer_hosts::project(Some(&after)));
    assert!(
        hosts.contains("moon-key") && !hosts.contains("canary"),
        "{hosts}"
    );
}

#[test]
fn a_kept_row_is_written_back_byte_for_byte_and_delete_clears_what_named_it() {
    let fixture = Fixture::new();
    let bambu = r#"{ "type":"bambu","id":"bambu-1", "config":{"host":"10.0.0.9","access_code":"kept-canary" , "extra":[1, 2]},"name":"H2D" }"#;
    fixture.seed(&format!("[{bambu}]"));
    let spool = fixture
        .create(new(ServiceKind::Spoolman, "http://spool.test:7912"))
        .unwrap();
    assert!(fixture.stored().unwrap().starts_with(&format!("[{bambu},")));

    // A Spoolman default and a TLS pin that name integrations about to go.
    let settings = fixture.owner.app_settings_with_policy(POLICY).unwrap();
    let default = Write::SpoolmanDefault(spool.id.clone());
    let chosen = settings.write(fixture.session(), default, &IDLE, WAIT);
    assert!(matches!(chosen, Ok(SettingOutcome::Written)));
    let settings_client = fixture.owner.client();
    let pin = tls_pins::Pin {
        host: "10.0.0.9".into(),
        port: 8883,
        sha256: [7; 32],
        pinned_at: String::new(),
    };
    for id in ["bambu-1", "bambu-2"] {
        let pinned = settings_client.pin_tls(id, pin.clone(), &IDLE, WAIT);
        assert!(pinned.unwrap().recv().unwrap().unwrap());
    }

    let removed = fixture.delete("bambu-1").unwrap();
    assert_eq!(
        (removed.kind, removed.name.as_str(), removed.base_url),
        (ServiceKind::Bambu, "H2D", None)
    );
    // Only the deleted integration's pin goes, and the default stays until
    // the integration it names is deleted.
    let pinned = |id| {
        tls_pins::read(&settings_client, WAIT, id)
            .unwrap()
            .is_some()
    };
    assert_eq!((pinned("bambu-1"), pinned("bambu-2")), (false, true));
    let default = || settings.read(fixture.session(), Key::SpoolmanDefault, &IDLE, WAIT);
    assert!(matches!(default(), Ok(SettingOutcome::Value(Some(id))) if id == spool.id));
    assert_eq!(fixture.delete(&spool.id).unwrap(), spool);
    assert!(matches!(default(), Ok(SettingOutcome::Value(None))));
    assert_eq!(fixture.stored().as_deref(), Some("[]"));
    let gone = fixture.delete("bambu-1");
    assert!(matches!(gone, Err(PrinterSetupError::NotFound)));
}

#[test]
fn the_limit_and_a_refused_credential_store_nothing() {
    let fixture = Fixture::new();
    let anonymous = Credential::Session(Secret::new("not-a-session".into()));
    let moon = || new(ServiceKind::Moonraker, "http://moon.test");
    let refused = fixture
        .client
        .create_integration(anonymous, moon(), &IDLE, WAIT);
    let refused = format!("{refused:?}");
    assert_eq!(refused, "Err(Writer(Auth(SessionRequired)))");
    assert_eq!(fixture.stored(), None);

    let rows: Vec<Value> = (0..MAX_INTEGRATIONS)
        .map(|index| json!({"id": format!("ai-{index}"), "type": "ai_assistant"}))
        .collect();
    let full = Value::Array(rows).to_string();
    fixture.seed(&full);
    let over = fixture.create(moon());
    assert!(matches!(over, Err(PrinterSetupError::LimitReached)));
    assert_eq!(fixture.stored(), Some(full));
    fixture.delete("ai-0").unwrap();
    fixture.create(moon()).unwrap();
}

#[test]
fn the_list_names_every_integration_and_no_secret() {
    let fixture = Fixture::new();
    let list = |credential| fixture.client.list_integrations(credential, &IDLE, WAIT);
    assert_eq!(list(fixture.session()).unwrap(), []);
    let bambu = r#"{"type":"bambu","id":"bambu-1","name":"H2D","config":{"host":"10.0.0.9","access_code":"kept-canary"}}"#;
    fixture.seed(&format!("[{bambu}]"));
    let moon = NewIntegration {
        api_key: secret("api-canary"),
        ..new(ServiceKind::Moonraker, "http://moon.test")
    };
    let prusa = NewIntegration {
        username: Some("maker".into()),
        password: secret("password-canary"),
        ..new(ServiceKind::Prusalink, "http://prusa.test")
    };
    let created = [
        fixture.create(moon).unwrap(),
        fixture.create(prusa).unwrap(),
    ];

    let listed = list(fixture.session()).unwrap();
    assert_eq!(listed[1..], created);
    assert_eq!(
        (
            listed[0].kind,
            listed[0].id.as_str(),
            listed[0].base_url.as_deref()
        ),
        (ServiceKind::Bambu, "bambu-1", None)
    );
    let flags: Vec<_> = listed
        .iter()
        .map(|i| (i.has_api_key, i.has_password))
        .collect();
    assert_eq!(flags, [(false, false), (true, false), (false, true)]);
    // The summaries hold no secret value at all.
    let shown = format!("{listed:?}");
    for canary in ["kept-canary", "api-canary", "password-canary", "10.0.0.9"] {
        assert!(!shown.contains(canary), "{canary} in {shown}");
    }

    // A row another writer stored with credentials or a key in its address
    // is shown without them.
    fixture.seed(
        r#"[{"type":"moonraker","id":"node-1","name":"Old","config":{"base_url":"http://admin:hunter2@192.168.1.5:7125/?api_key=query-canary#x","enabled":true},"created_at":"","updated_at":""},
            {"type":"moonraker","id":"node-2","name":"Bad","config":{"base_url":"not a url","enabled":true},"created_at":"","updated_at":""}]"#,
    );
    let shown = list(fixture.session()).unwrap();
    let urls: Vec<_> = shown.iter().map(|i| i.base_url.as_deref()).collect();
    assert_eq!(urls, [Some("http://192.168.1.5:7125/"), None]);

    let anonymous = Credential::Session(Secret::new("not-a-session".into()));
    let refused = format!("{:?}", list(anonymous));
    assert_eq!(refused, "Err(Writer(Auth(SessionRequired)))");
    fixture.seed(r#"[{"type":"moonraker","id":"m","config":"not an object"}]"#);
    let unreadable = list(fixture.session());
    assert!(matches!(
        unreadable,
        Err(PrinterSetupError::StoredDocumentInvalid(_))
    ));
}

mod edits;
mod printers;
