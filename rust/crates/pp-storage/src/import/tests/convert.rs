use super::*;
use crate::import::import;
use crate::printer_setup::{integrations, printers};

fn setting_at(database: &Path, key: &str) -> String {
    let connection = Connection::open(database).unwrap();
    connection
        .query_row(
            "SELECT value FROM app_settings WHERE key = ?1",
            [key],
            |row| row.get(0),
        )
        .unwrap()
}

#[test]
fn printers_and_integrations_convert_and_a_bad_document_is_refused() {
    let folder = node_folder();
    let secret = "user-pass-not-a-real-secret";
    let fleet = r#"[{"id":1,"name":"Bench","bed_width_mm":"250.9","margin_mm":4.1,"nozzle_mm":0.4},{"id":"kept","name":"Other","bed_width_mm":250}]"#;
    let hosts = format!(
        r#"[{{"id":"moon-1","type":"moonraker","name":"Voron","config":{{"baseUrl":"http://{secret}@printer.invalid/printer","api_key":"{secret}","port":7125}},"created_at":"2026-10-02T00:00:00.000Z","updated_at":"2026-10-02T00:00:00.000Z","capabilities":{{"files":true}}}},{{"type":"bambu","id":"bambu-1","name":"H2D","config":{{"host":"http://user:kept@10.0.0.9","access_code":"{secret}"}}}}]"#
    );
    execute(
        &folder,
        &format!(
            "UPDATE app_settings SET value = '{fleet}' WHERE key = 'printer.fleet';
             UPDATE app_settings SET value = '{hosts}' WHERE key = 'integrations';"
        ),
    );
    let report = run(folder.path()).unwrap();
    let text = report.to_string();
    assert!(text.contains("printer.fleet id: 1 -> \"1\" = 1"), "{text}");
    assert!(
        text.contains("printer.fleet bed_width_mm: \"250.9\" -> 250 = 1"),
        "{text}"
    );
    assert!(
        text.contains("printer.fleet margin_mm: 4.1 -> 5 = 1"),
        "{text}"
    );
    assert!(text.contains("dropped printer nozzle_mm = 1"), "{text}");
    assert!(text.contains("dropped moonraker config.port = 1"), "{text}");
    assert!(
        text.contains("dropped moonraker capabilities = 1"),
        "{text}"
    );
    assert!(
        text.contains("moved moonraker config.baseUrl to base_url = 1"),
        "{text}"
    );
    assert!(
        text.contains("addresses with a username or password, kept as stored: 2"),
        "{text}"
    );
    for hidden in [secret, "0.4", "access_code"] {
        assert!(!text.contains(hidden), "{hidden} is in the report:\n{text}");
    }

    let parent = tempfile::tempdir().unwrap();
    let destination = parent.path().join("rust");
    import(folder.path(), &destination).unwrap();
    let database = destination.join("print-partner.db");
    let fleet = setting_at(&database, "printer.fleet");
    let printers = printers::parse(&fleet).unwrap();
    assert_eq!(printers[0].id, "1");
    assert_eq!(printers[0].bed_width_mm, 250);
    assert_eq!(printers[0].margin_mm, 5);
    assert_eq!(printers[0].bed_depth_mm, 210);
    assert!(!fleet.contains("nozzle"), "{fleet}");
    let hosts = setting_at(&database, "integrations");
    integrations::parse(&hosts).unwrap();
    assert!(hosts.contains(secret), "the kept credential was removed");
    assert!(
        hosts.contains("http://user-pass-not-a-real-secret@printer.invalid/printer"),
        "{hosts}"
    );
    assert!(!hosts.contains("\"port\""), "{hosts}");
    let bambu = format!(
        r#"{{"type":"bambu","id":"bambu-1","name":"H2D","config":{{"host":"http://user:kept@10.0.0.9","access_code":"{secret}"}}}}"#
    );
    assert!(
        hosts.contains(&bambu),
        "a kept kind was rewritten:\n{hosts}"
    );
    let saved = std::fs::read_to_string(destination.join(crate::import::REPORT_FILE)).unwrap();
    assert!(!saved.contains(secret), "{saved}");
}

#[test]
fn an_unknown_integration_type_refuses_the_import_and_the_dry_run_still_reports() {
    let folder = node_folder();
    execute(
        &folder,
        "UPDATE app_settings SET value = '[{\"id\":\"x\",\"type\":\"widget\"}]' WHERE key = 'integrations'",
    );
    let report = run(folder.path()).unwrap();
    let text = report.to_string();
    assert!(
        text.contains("integrations type is not one Rust keeps = 1"),
        "{text}"
    );
    assert!(
        text.contains("The import would refuse this installation."),
        "{text}"
    );
    assert!(
        text.contains("step 3 converted: setting integrations = 1"),
        "{text}"
    );
    assert!(!text.contains("widget"), "{text}");
    let parent = tempfile::tempdir().unwrap();
    let destination = parent.path().join("rust");
    let error = import(folder.path(), &destination).unwrap_err();
    assert!(matches!(error, ImportError::Settings(_)), "{error:?}");
    assert!(error.to_string().contains("not the typed shape"));
    assert!(!destination.join(ownership_marker::MARKER_FILE).exists());
}

#[test]
fn a_share_prefix_is_allowed_and_any_other_empty_segment_is_not() {
    assert!(verify::plain_or_share("//server/share/data/repos/kit"));
    assert!(verify::plain_or_share("/data/repos/kit"));
    assert!(!verify::plain_or_share("///server/share"));
    assert!(!verify::plain_or_share("//server//share/data"));
    assert!(!verify::plain_or_share("/data//repos/kit"));
}
