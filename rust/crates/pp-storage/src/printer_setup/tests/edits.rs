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
    let before = row(&fixture);

    // A new address and password in one write; the username is not sent.
    let moved = IntegrationPatch {
        base_url: Some("https://mk4.test:8443".into()),
        password: Some(secret("second-password-canary")),
        ..IntegrationPatch::default()
    };
    let moved = fixture.change(&created.id, moved).unwrap();
    let expected = IntegrationSummary {
        base_url: Some("https://mk4.test:8443".into()),
        ..created.clone()
    };
    assert_eq!(moved, expected);
    let after = row(&fixture);
    let config = json!({"base_url": "https://mk4.test:8443", "enabled": true,
        "username": "maker", "password": "second-password-canary"});
    assert_eq!(after["config"], config);
    assert_eq!(after["created_at"], before["created_at"]);
    assert!(after["updated_at"].as_str() >= before["updated_at"].as_str());
    // The status watchers read the new address and password.
    let watched = printer_hosts::watched(fixture.stored().as_deref()).unwrap();
    let host = &watched.prusalink[0];
    let read = (host.base_url.as_str(), host.password.as_deref());
    assert_eq!(
        read,
        ("https://mk4.test:8443", Some("second-password-canary"))
    );

    // An absent credential is kept; a null one is cleared.
    let renamed = IntegrationPatch {
        name: Some(" Big MK4 ".into()),
        enabled: Some(false),
        ..IntegrationPatch::default()
    };
    let renamed = fixture.change(&created.id, renamed).unwrap();
    let shown = (renamed.name.as_str(), renamed.enabled, renamed.has_password);
    assert_eq!(shown, ("Big MK4", false, true));
    let cleared = IntegrationPatch {
        username: Some(None),
        password: Some(None),
        ..IntegrationPatch::default()
    };
    let cleared = fixture.change(&created.id, cleared).unwrap();
    assert_eq!((cleared.username, cleared.has_password), (None, false));
    let config = json!({"base_url": "https://mk4.test:8443", "enabled": false});
    assert_eq!(row(&fixture)["config"], config);

    // An edit that sends nothing answers the stored integration.
    let same = fixture.change(&created.id, IntegrationPatch::default());
    assert_eq!(same.unwrap().name, "Big MK4");
}

#[test]
fn an_invalid_edit_names_its_fields_and_changes_nothing() {
    use Field::{ApiKey, BaseUrl, Name, Password};
    use Reason::{Empty, HasCredentials, NotHttpUrl, NotUsedByKind};
    let fixture = Fixture::new();
    let created = prusalink(&fixture);
    let stored = fixture.stored();
    let cases = [
        (
            IntegrationPatch {
                name: Some("  ".into()),
                base_url: Some("ftp://mk4.test".into()),
                ..IntegrationPatch::default()
            },
            vec![(Name, Empty), (BaseUrl, NotHttpUrl)],
        ),
        (
            IntegrationPatch {
                base_url: Some("http://maker:pw@mk4.test".into()),
                password: Some(secret("")),
                ..IntegrationPatch::default()
            },
            vec![(BaseUrl, HasCredentials), (Password, Empty)],
        ),
        (
            // A valid name beside a field the kind does not take.
            IntegrationPatch {
                name: Some("Renamed".into()),
                api_key: Some(secret("key")),
                ..IntegrationPatch::default()
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
    let old = json!([{"id": "moon-1", "type": "moonraker", "name": "Old",
        "config": {"base_url": "http://user:old-canary@moon.test", "enabled": true},
        "created_at": "2026-11-12T03:04:05.678Z", "updated_at": "2026-11-12T03:04:05.678Z"}]);
    fixture.seed(&old.to_string());
    let renamed = IntegrationPatch {
        name: Some("Voron".into()),
        ..IntegrationPatch::default()
    };
    assert_eq!(fixture.change("moon-1", renamed).unwrap().name, "Voron");
    let config = &row(&fixture)["config"];
    assert_eq!(config["base_url"], "http://user:old-canary@moon.test");
}

#[test]
fn a_kept_kind_an_unknown_id_and_a_refused_credential_change_nothing() {
    let fixture = Fixture::new();
    let bambu = r#"[{ "type":"bambu","id":"bambu-1", "config":{"host":"10.0.0.9"},"name":"H2D" }]"#;
    fixture.seed(bambu);
    let rename = || IntegrationPatch {
        name: Some("Renamed".into()),
        ..IntegrationPatch::default()
    };
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
