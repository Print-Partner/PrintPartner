use super::*;
use crate::import::setup::{self, Converted, convert};
use crate::import::{Counted, CredentialAddress, SizeChange, import};
use crate::printer_setup::{DocumentFault, ServiceKind, integrations, printers};
use serde_json::json;

const STAMPS: &str =
    r#""created_at":"2026-10-01T00:00:00.000Z","updated_at":"2026-10-02T00:00:00.000Z""#;

fn fleet(stored: &str) -> (Vec<Value>, SetupReport) {
    let converted = convert(Some(stored), None).unwrap();
    let text = converted.fleet.unwrap();
    // Rust's own reader takes every converted document.
    printers::parse(&text).unwrap();
    (serde_json::from_str(&text).unwrap(), converted.report)
}

fn hosts(stored: &str) -> (Vec<Value>, String, SetupReport) {
    let converted = convert(None, Some(stored)).unwrap();
    let text = converted.integrations.unwrap();
    assert!(integrations::parse(&text).is_ok(), "Rust's reader refuses");
    (serde_json::from_str(&text).unwrap(), text, converted.report)
}

fn counted(found: &[Counted]) -> Vec<(&str, &str, u64)> {
    let mut all = Vec::new();
    for one in found {
        all.push((one.kind, one.name.as_str(), one.count));
    }
    all
}

#[test]
fn a_printer_gets_the_values_node_read_from_it() {
    let stored = json!([
        {"id": 7},
        {"id": "b", "name": "Synthetic B", "model": null, "bed_width_mm": "300",
         "bed_depth_mm": null, "margin_mm": 0, "max_filament_slots": 9,
         "loaded_filaments": [
            {"slot": "2", "filament_color_id": null, "label": "second"},
            {"filament_color_id": "red"}],
         "integration_id": "  host-1  ", "device_id": "", "preset_id": null,
         "preferred_slicer": " Orca "},
    ]);
    let (printers, report) = fleet(&stored.to_string());
    let empty = |slot: u8| json!({"slot": slot, "filament_color_id": null, "label": ""});
    assert_eq!(
        printers[0],
        json!({"id": "7", "name": "Printer", "model": "Printer", "bed_width_mm": 250,
               "bed_depth_mm": 210, "bed_height_mm": null, "margin_mm": 4,
               "max_filament_slots": 1, "loaded_filaments": [empty(1)]})
    );
    assert_eq!(
        printers[1],
        json!({"id": "b", "name": "Synthetic B", "model": "Synthetic B", "bed_width_mm": 300,
               "bed_depth_mm": 210, "bed_height_mm": null, "margin_mm": 0,
               "max_filament_slots": 4,
               "loaded_filaments": [
                   {"slot": 1, "filament_color_id": "red", "label": ""},
                   {"slot": 2, "filament_color_id": null, "label": "second"},
                   empty(3), empty(4)],
               "integration_id": "host-1", "preferred_slicer": "orca"})
    );
    // Nothing here is a change Node did not make on every read.
    assert_eq!((report.printers, report.sizes.len()), (2, 0));
    assert_eq!((&report.kept_keys, &report.refused), (&vec![], &vec![]));
}

#[test]
fn a_fractional_size_is_rounded_and_each_change_is_listed() {
    let stored = json!([
        {"id": "a", "bed_width_mm": 220.5, "bed_depth_mm": "210.9", "bed_height_mm": 250.01,
         "margin_mm": 3.2},
        {"id": "b", "bed_width_mm": 256, "bed_depth_mm": 256.0, "margin_mm": -0.5},
    ]);
    let (printers, report) = fleet(&stored.to_string());
    let sizes = |printer: &Value| {
        ["bed_width_mm", "bed_depth_mm", "margin_mm"].map(|field| printer[field].as_u64().unwrap())
    };
    // The bed rounds down and the margin up, so nothing is placed off the bed.
    assert_eq!(sizes(&printers[0]), [220, 210, 4]);
    assert_eq!(sizes(&printers[1]), [256, 256, 0]);
    assert_eq!(printers[0]["bed_height_mm"], 250);
    let change = |printer, field, stored: &str, converted| SizeChange {
        printer,
        field,
        stored: stored.to_owned(),
        converted,
    };
    assert_eq!(
        report.sizes,
        [
            change(1, "bed_width_mm", "220.5", 220),
            change(1, "bed_depth_mm", "210.9", 210),
            change(1, "bed_height_mm", "250.01", 250),
            change(1, "margin_mm", "3.2", 4),
            change(2, "margin_mm", "-0.5", 0),
        ]
    );
    let text = report.to_string();
    let line =
        "Bed sizes rounded to whole millimetres: 5\n  printer 1 bed_width_mm: 220.5 -> 220\n";
    assert!(text.contains(line), "{text}");
    assert_eq!(report.refused, []);
}

#[test]
fn a_key_rust_does_not_name_is_kept_with_its_value_and_counted() {
    let nested = r#"{"token":"synthetic-unread","list":[1,2.5,null,"é\n"]}"#;
    let stored = format!(
        r#"[{{"id":"a","nozzle_mm":0.4,"a key that is no name":{nested}}},{{"nozzle_mm":0.6,"id":"b"}}]"#
    );
    let converted = convert(Some(&stored), None).unwrap();
    let text = converted.fleet.unwrap();
    // The keys follow the named fields, each with its value as stored.
    let first = format!(r#""nozzle_mm":0.4,"a key that is no name":{nested}}},"#);
    assert!(text.contains(&first), "{text}");
    assert!(text.ends_with(r#""nozzle_mm":0.6}]"#), "{text}");
    let kept = [("printer", "?", 1), ("printer", "nozzle_mm", 2)];
    assert_eq!(counted(&converted.report.kept_keys), kept);

    let stored = format!(
        r#"[{{"id":"m","type":"moonraker","name":"Synthetic M","capabilities":{{"files":true}},"config":{{"base_url":"http://m.invalid","port":7125,"apiKey":"synthetic-second","api_key":"synthetic-first","legacy":{nested}}},{STAMPS}}},
            {{"id":"p","type":"prusalink","name":"Synthetic P","config":{{"base_url":"http://p.invalid","port":80}},{STAMPS}}}]"#
    );
    let (rows, text, report) = hosts(&stored);
    // A second spelling Node did not read is one more unread key.
    let config = format!(
        r#""config":{{"base_url":"http://m.invalid","enabled":true,"api_key":"synthetic-first","port":7125,"apiKey":"synthetic-second","legacy":{nested}}}"#
    );
    assert!(text.contains(&config), "{text}");
    assert_eq!(rows[0]["capabilities"], json!({"files": true}));
    assert_eq!(
        counted(&report.kept_keys),
        [
            ("moonraker", "capabilities", 1),
            ("moonraker", "config.apiKey", 1),
            ("moonraker", "config.legacy", 1),
            ("moonraker", "config.port", 1),
            ("prusalink", "config.port", 1),
        ]
    );
    assert_eq!(report.refused, []);
    let shown = format!(
        "{report}\n{report:?}\n{}",
        serde_json::to_string(&report).unwrap()
    );
    let lines =
        "keys kept unread (kind, key, rows that hold it): 5\n  moonraker capabilities = 1\n";
    assert!(shown.contains(lines), "{shown}");
    for value in ["synthetic", "7125", "invalid", "no name"] {
        assert!(!shown.contains(value), "{value} is in:\n{shown}");
    }
}

#[test]
fn a_value_rusts_shape_cannot_hold_is_counted_and_nothing_is_guessed() {
    let stored = json!([
        {"id": "ok"},
        {"id": "a", "name": 7, "bed_width_mm": true, "bed_height_mm": -4, "margin_mm": "wide",
         "max_filament_slots": 1.5, "integration_id": 12, "preferred_slicer": "cura",
         "loaded_filaments": [{"slot": 1, "spool": 3}, {"slot": 1}, {"slot": 3}, "text"]},
        {"id": "b", "name": {}, "loaded_filaments": {}},
        {"name": "Synthetic without an id"},
        {"id": "ok", "name": "Synthetic duplicate"},
        null,
    ]);
    let converted = convert(Some(&stored.to_string()), None).unwrap();
    assert_eq!(converted.report.printers, 1);
    assert_eq!(
        counted(&converted.report.refused),
        [
            ("printer", "(an entry is not an object)", 1),
            ("printer", "(an id is stored twice)", 1),
            ("printer", "bed_height_mm", 1),
            ("printer", "bed_width_mm", 1),
            ("printer", "id", 1),
            ("printer", "integration_id", 1),
            ("printer", "loaded_filaments", 2),
            ("printer", "loaded_filaments.slot", 1),
            ("printer", "loaded_filaments.spool", 1),
            ("printer", "margin_mm", 1),
            ("printer", "max_filament_slots", 1),
            ("printer", "name", 2),
            ("printer", "preferred_slicer", 1),
        ]
    );

    let stored = format!(
        r#"[{{"id":"m","type":"moonraker","name":"Synthetic M","config":{{"baseUrl":7,"enabled":"yes","api_key":5}},{STAMPS}}},
            {{"id":"n","type":"moonraker","config":{{}}}},
            {{"id":"m","type":"spoolman"}},
            {{"id":"x","type":"octoprint"}},
            {{"id":4,"type":"moonraker"}},
            {{"id":"s","type":"spoolman","name":"Synthetic S","config":"text",{STAMPS}}},
            "text"]"#
    );
    let converted = convert(None, Some(&stored)).unwrap();
    assert_eq!(converted.report.integrations, 0);
    assert_eq!(
        counted(&converted.report.refused),
        [
            ("integration", "(an entry is not an object)", 1),
            ("integration", "type", 1),
            ("moonraker", "config.api_key", 1),
            ("moonraker", "config.baseUrl", 1),
            ("moonraker", "config.base_url", 2),
            ("moonraker", "config.enabled", 1),
            ("moonraker", "created_at", 1),
            ("moonraker", "id", 1),
            ("moonraker", "name", 1),
            ("moonraker", "updated_at", 1),
            ("spoolman", "(an id is stored twice)", 1),
            ("spoolman", "config", 1),
        ]
    );
    let text = converted.report.to_string();
    let line = "cannot hold (kind, field, rows): 12. The import refuses this installation.\n";
    assert!(text.contains(line), "{text}");
    assert!(!text.contains("Synthetic"), "{text}");

    // Text that is no list is not read as an empty one.
    for text in ["{}", "not json", "7"] {
        let converted = convert(Some(text), Some(text)).unwrap();
        let whole = "(the setting is not a list)";
        let expected = [("integration", whole, 1), ("printer", whole, 1)];
        assert_eq!(counted(&converted.report.refused), expected);
    }
    // No setting, or an empty one, is no document: nothing is written.
    let converted = convert(None, Some("")).unwrap();
    assert!(converted.fleet.is_none() && converted.integrations.is_none());
    assert_eq!(converted.report, SetupReport::default());
}

#[test]
fn an_integration_gets_rusts_field_names_and_other_kinds_stay_as_stored() {
    let bambu = r#"{ "id":"b",  "type":"bambu", "config":{"host":"bambu.invalid","access_code":"synthetic-code","url":"http://u:p@bambu.invalid"} }"#;
    let stored = format!(
        r#"[{{"id":"m","type":"moonraker","name":"Synthetic M","config":{{"baseUrl":"http://m.invalid","base_url":null,"apiKey":"synthetic-key","enabled":false}},{STAMPS}}},
            {{"id":"p","type":"prusalink","name":"Synthetic P","config":{{"base_url":"http://synthetic:synthetic-pass@p.invalid","user":"maker","api_key":"synthetic-key","password":"synthetic-pass","storage_path":"/usb/"}},{STAMPS}}},
            {bambu},
            {{"id":"s","type":"spoolman","name":"","config":{{"base_url":"http://s.invalid"}},{STAMPS}}}]"#
    );
    let (rows, text, report) = hosts(&stored);
    let stamps: Value = serde_json::from_str(&format!("{{{STAMPS}}}")).unwrap();
    let row = |mut row: Value| {
        row.as_object_mut()
            .unwrap()
            .extend(stamps.as_object().unwrap().clone());
        row
    };
    assert_eq!(
        rows[0],
        row(
            json!({"id": "m", "type": "moonraker", "name": "Synthetic M",
            "config": {"base_url": "http://m.invalid", "enabled": false, "api_key": "synthetic-key"}})
        )
    );
    // The address is kept as stored, credentials and all, and so is a
    // credential the kind does not sign in with.
    assert_eq!(
        rows[1]["config"],
        json!({"base_url": "http://synthetic:synthetic-pass@p.invalid", "enabled": true,
               "username": "maker", "api_key": "synthetic-key", "password": "synthetic-pass",
               "storage": "/usb/"})
    );
    // A kind Rust does not create is stored byte for byte.
    assert!(text.contains(bambu), "{text}");
    assert_eq!(
        rows[3]["config"],
        json!({"base_url": "http://s.invalid", "enabled": true})
    );

    assert_eq!((report.integrations, &report.refused), (4, &vec![]));
    assert_eq!(report.kept_keys, []);
    let held = |entry, kind| CredentialAddress { entry, kind };
    assert_eq!(
        report.credential_addresses,
        [held(2, ServiceKind::Prusalink), held(3, ServiceKind::Bambu)]
    );
    let shown = format!("{report}\n{}", serde_json::to_string(&report).unwrap());
    assert!(shown.contains("kept as stored: 2"), "{shown}");
    for value in ["invalid", "synthetic", "Synthetic", "maker", "/usb/"] {
        assert!(!shown.contains(value), "{value} is in:\n{shown}");
    }
}

/// A database with the two settings as `fleet` and `integrations` store them.
fn staged(fleet: &str, integrations: &str) -> Connection {
    let connection = Connection::open_in_memory().unwrap();
    connection
        .execute_batch("CREATE TABLE app_settings(tenant_id TEXT, key TEXT, value TEXT)")
        .unwrap();
    for (key, value) in [("printer.fleet", fleet), ("integrations", integrations)] {
        let sql = "INSERT INTO app_settings VALUES ('default', ?1, ?2)";
        connection.execute(sql, [key, value]).unwrap();
    }
    connection
}

#[test]
fn a_converted_document_that_fails_its_check_stops_the_import() {
    let stored = json!([{"id": "a", "nozzle_mm": 0.4}, {"id": "b"}]).to_string();
    let connection = staged(&stored, "[]");
    let planned = setup::plan(&connection).unwrap();
    setup::write(&connection, &planned).unwrap();
    let found = |key: &str| -> String {
        let sql = "SELECT value FROM app_settings WHERE key = ?1";
        connection.query_row(sql, [key], |row| row.get(0)).unwrap()
    };
    assert_eq!(Some(found("printer.fleet")), planned.fleet);
    assert_eq!(found("integrations"), "[]");

    let refused = |fleet: Value| {
        let connection = staged(&stored, "[]");
        let converted = Converted {
            fleet: Some(fleet.to_string()),
            integrations: None,
            report: SetupReport::default(),
        };
        match setup::write(&connection, &converted) {
            Err(ImportError::Setup { setting, fault }) => (setting, fault),
            other => panic!("{:?}", other.map_err(|error| error.to_string())),
        }
    };
    let printers: Vec<Value> = serde_json::from_str(planned.fleet.as_ref().unwrap()).unwrap();
    let (first, second) = (printers[0].clone(), printers[1].clone());
    // Not the shape Rust reads.
    let shape = Some(DocumentFault::WrongShape);
    let unconverted: Value = serde_json::from_str(&stored).unwrap();
    assert_eq!(refused(unconverted), ("printer.fleet", shape));
    let twice = Some(DocumentFault::DuplicateId);
    assert_eq!(refused(json!([first, first])), ("printer.fleet", twice));
    // An entry missing, one too many, another order, or a lost or changed key.
    let mut invented = second.clone();
    invented["id"] = json!("c");
    let (mut lost, mut changed) = (first.clone(), first.clone());
    lost.as_object_mut().unwrap().remove("nozzle_mm");
    changed["nozzle_mm"] = json!(0.6);
    for document in [
        json!([first]),
        json!([first, second, invented]),
        json!([second, first]),
        json!([lost, second]),
        json!([changed, second]),
    ] {
        assert_eq!(refused(document), ("printer.fleet", None));
    }
}

#[test]
fn an_import_stores_printers_and_integrations_as_rust_reads_them() {
    let folder = node_folder();
    let planned = run(folder.path()).unwrap();
    let parent = tempfile::tempdir().unwrap();
    let report = import(folder.path(), &parent.path().join("rust")).unwrap();
    // The dry run says what the import then does.
    assert_eq!(planned.setup, report.setup);
    let setup = &report.setup;
    assert_eq!(
        (report.printers, setup.integrations),
        (synthetic::PRINTERS, 2)
    );
    let sizes: Vec<(usize, &str, &str, u32)> = (setup.sizes.iter())
        .map(|size| {
            (
                size.printer,
                size.field,
                size.stored.as_str(),
                size.converted,
            )
        })
        .collect();
    let expected = [
        (2, "bed_width_mm", "220.5", 220),
        (2, "bed_height_mm", "250.9", 250),
        (2, "margin_mm", "3.2", 4),
    ];
    assert_eq!(sizes, expected);
    // The extra key of a fleet row and of an integration's config, by count.
    let kept = [
        ("moonraker", "config.poll_seconds", 1),
        ("printer", "nozzle_mm", 1),
    ];
    assert_eq!(counted(&setup.kept_keys), kept);
    assert_eq!(setup.refused, []);
    let moonraker = CredentialAddress {
        entry: 1,
        kind: ServiceKind::Moonraker,
    };
    assert_eq!(setup.credential_addresses, [moonraker]);

    let connection = Connection::open(parent.path().join("rust/print-partner.db")).unwrap();
    let stored = |key: &str| -> String {
        let sql = "SELECT value FROM app_settings WHERE tenant_id = 'default' AND key = ?1";
        connection.query_row(sql, [key], |row| row.get(0)).unwrap()
    };
    let fleet = stored("printer.fleet");
    let printers = printers::parse(&fleet).unwrap();
    let ids: Vec<&str> = printers.iter().map(|printer| printer.id.as_str()).collect();
    assert_eq!(ids, ["1", "synthetic-printer-2"]);
    let second = &printers[1];
    let sizes = (
        second.bed_width_mm,
        second.bed_depth_mm,
        second.bed_height_mm,
    );
    assert_eq!((sizes, second.margin_mm), ((220, 220, Some(250)), 4));
    assert_eq!(
        second.integration_id.as_deref(),
        Some("synthetic-integration")
    );
    assert_eq!(second.loaded_filaments[1].label, "Synthetic PLA");
    // The typed row shows nothing of the key it carries; the stored row holds it.
    assert!(!format!("{second:?}").contains("nozzle"), "{second:?}");
    assert!(fleet.ends_with(r#","nozzle_mm":0.4}]"#), "{fleet}");

    let text = stored("integrations");
    let entries = integrations::parse(&text).ok().unwrap();
    let ids: Vec<&str> = entries.iter().map(|entry| entry.id()).collect();
    assert_eq!(ids, ["synthetic-integration", "synthetic-bambu"]);
    let summary = format!("{:?}", entries[0].summary());
    assert!(!summary.contains("poll"), "{summary}");
    // The address, the key and the unread key arrive as stored.
    let rows: Vec<Value> = serde_json::from_str(&text).unwrap();
    let address = format!(
        "http://synthetic:{}@printer.invalid:7125",
        synthetic::ADDRESS_SECRET
    );
    assert_eq!(rows[0]["config"]["base_url"], address);
    assert_eq!(rows[0]["config"]["api_key"], synthetic::INTEGRATION_SECRET);
    assert_eq!(rows[0]["config"]["poll_seconds"], 5);
    assert_eq!(rows[1]["config"]["serial"], "SYNTHETIC0001");
}

#[test]
fn an_installation_with_a_value_that_does_not_fit_is_refused_and_nothing_is_written() {
    let folder = node_folder();
    execute(
        &folder,
        r#"UPDATE app_settings SET value = '[{"id":"a","bed_width_mm":"wide"}]' WHERE key = 'printer.fleet'"#,
    );
    let planned = run(folder.path()).unwrap();
    assert_eq!(
        counted(&planned.setup.refused),
        [("printer", "bed_width_mm", 1)]
    );
    let parent = tempfile::tempdir().unwrap();
    let destination = parent.path().join("rust");
    let refused = import(folder.path(), &destination).unwrap_err();
    let ImportError::SetupValues(found) = &refused else {
        panic!("{refused}")
    };
    assert_eq!(counted(found), [("printer", "bed_width_mm", 1)]);
    let text = refused.to_string();
    assert!(
        text.contains("printer bed_width_mm = 1") && !text.contains("wide"),
        "{text}"
    );
    // The refusal leaves a folder no Rust runtime opens.
    assert!(!destination.join(ownership_marker::MARKER_FILE).exists());
}
