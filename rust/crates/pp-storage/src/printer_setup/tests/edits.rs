//! `update_integration`: one write changes what the patch sends.

use super::*;

fn prusalink(fixture: &Fixture) -> IntegrationSummary {
    let prusa = NewIntegration {
        username: Some("maker".into()),
        password: secret("first-password-canary"),
        ..new(ServiceKind::Prusalink, "http://mk4.test")
    };
    fixture.create(prusa).unwrap()
}

/// A patch that sets only the name and the address it is given.
fn patch(name: Option<&str>, base_url: Option<&str>) -> IntegrationPatch {
    IntegrationPatch {
        name: name.map(Into::into),
        base_url: base_url.map(Into::into),
        ..IntegrationPatch::default()
    }
}

/// The one stored row.
fn row(fixture: &Fixture) -> Value {
    let rows: Value = serde_json::from_str(&fixture.stored().unwrap()).unwrap();
    assert_eq!(rows.as_array().unwrap().len(), 1);
    rows[0].clone()
}

#[test]
fn an_edit_changes_what_it_sends_and_keeps_the_rest() {
    let fixture = Fixture::new();
    let created = prusalink(&fixture);

    // A new address and password in one write; the username is not sent.
    let moved = IntegrationPatch {
        password: Some(secret("new-pw-canary")),
        ..patch(None, Some("https://mk4.test:8443"))
    };
    let moved = fixture.change(&created.id, moved).unwrap();
    assert_eq!(moved.base_url.as_deref(), Some("https://mk4.test:8443"));
    let after = row(&fixture);
    let config = json!({"base_url": "https://mk4.test:8443", "enabled": true,
        "username": "maker", "password": "new-pw-canary"});
    assert_eq!(after["config"], config);
    // The status watchers read the new address and password.
    let watched = printer_hosts::watched(fixture.stored().as_deref()).unwrap();
    let host = &watched.prusalink[0];
    let read = (host.base_url.as_str(), host.password.as_deref());
    assert_eq!(read, ("https://mk4.test:8443", Some("new-pw-canary")));

    // An absent credential is kept when the address sent is the stored one.
    let renamed = IntegrationPatch {
        enabled: Some(false),
        ..patch(Some(" Big MK4 "), Some("https://mk4.test:8443"))
    };
    let renamed = fixture.change(&created.id, renamed).unwrap();
    let shown = (renamed.name.as_str(), renamed.enabled, renamed.has_password);
    assert_eq!(shown, ("Big MK4", false, true));
    // A new address takes the stored password as null, which clears it.
    let cleared = IntegrationPatch {
        username: Some(None),
        password: Some(None),
        ..patch(None, Some("http://mk4.test"))
    };
    let cleared = fixture.change(&created.id, cleared).unwrap();
    assert_eq!((cleared.username, cleared.has_password), (None, false));
    let config = json!({"base_url": "http://mk4.test", "enabled": false});
    assert_eq!(row(&fixture)["config"], config);

    // With no stored secret, the address changes alone. An edit that sends
    // nothing answers the stored integration.
    let moved = patch(None, Some("http://moved.test"));
    let moved = fixture.change(&created.id, moved).unwrap();
    assert_eq!(moved.base_url.as_deref(), Some("http://moved.test"));
    let same = fixture.change(&created.id, IntegrationPatch::default());
    assert_eq!(same.unwrap(), moved);
}

#[test]
fn an_invalid_edit_names_its_fields_and_changes_nothing() {
    use Field::{ApiKey, BaseUrl, Name, Password};
    use Reason::{Empty, HasCredentials, NotHttpUrl, NotResent, NotUsedByKind};
    let fixture = Fixture::new();
    let created = prusalink(&fixture);
    let stored = fixture.stored();
    let cases = [
        (
            patch(Some("  "), Some("ftp://mk4.test")),
            vec![(Name, Empty), (BaseUrl, NotHttpUrl), (Password, NotResent)],
        ),
        (
            // A new address would send the stored password to another host.
            patch(None, Some("http://other.test")),
            vec![(Password, NotResent)],
        ),
        (
            IntegrationPatch {
                password: Some(secret("")),
                ..patch(None, Some("http://maker:pw@mk4.test"))
            },
            vec![(BaseUrl, HasCredentials), (Password, Empty)],
        ),
        (
            // A valid name beside a field the kind does not take.
            IntegrationPatch {
                api_key: Some(secret("key")),
                ..patch(Some("Renamed"), None)
            },
            vec![(ApiKey, NotUsedByKind)],
        ),
    ];
    for (index, (patch, expected)) in cases.into_iter().enumerate() {
        let named = invalid_fields(fixture.change(&created.id, patch), index);
        assert_eq!(named, expected, "case {index}");
    }
    assert_eq!(fixture.stored(), stored);
}

#[test]
fn an_edit_checks_only_what_it_sends() {
    let fixture = Fixture::new();
    // A row from before the checks: its address holds a password.
    let old = json!([{"id": "moon-1", "type": "moonraker", "name": "Old", "created_at": "",
        "config": {"base_url": "http://u:old-canary@moon.test", "enabled": true}, "updated_at": ""}]);
    fixture.seed(&old.to_string());
    let renamed = fixture.change("moon-1", patch(Some("Voron"), None));
    assert_eq!(renamed.unwrap().name, "Voron");
    assert!(fixture.stored().unwrap().contains("u:old-canary@moon.test"));
}

#[test]
fn a_kept_kind_an_unknown_id_and_a_refused_credential_change_nothing() {
    let fixture = Fixture::new();
    let bambu = r#"[{ "type":"bambu","id":"bambu-1", "config":{"host":"10.0.0.9"},"name":"H2D" }]"#;
    fixture.seed(bambu);
    let rename = || patch(Some("Renamed"), None);
    let kept = fixture.change("bambu-1", rename());
    assert!(matches!(kept, Err(PrinterSetupError::NotEditable)));
    let missing = fixture.change("moon-1", rename());
    assert!(matches!(missing, Err(PrinterSetupError::NotFound)));
    let anonymous = Credential::Session(Secret::new("not-a-session".into()));
    let refused =
        fixture
            .client
            .update_integration(anonymous, "bambu-1".into(), rename(), &IDLE, WAIT);
    let refused = format!("{refused:?}");
    assert_eq!(refused, "Err(Writer(Auth(SessionRequired)))");
    assert_eq!(fixture.stored().as_deref(), Some(bambu));
}
