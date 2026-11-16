use super::*;
use crate::import::import;
use crate::printer_setup::{integrations, printers};
use crate::read_model::farm::{CheckoffResultState, Farm, FarmTasks, SendState};
use crate::send_queue::{INTERRUPTED_ERROR, NOT_STARTED_ERROR};
use rusqlite::OptionalExtension;

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

fn put(folder: &Installation, key: &str, value: &serde_json::Value) {
    Connection::open(folder.database_path())
        .unwrap()
        .execute(
            "INSERT INTO app_settings (tenant_id, key, value) VALUES ('default', ?1, ?2)
             ON CONFLICT(tenant_id, key) DO UPDATE SET value = excluded.value",
            (key, value.to_string()),
        )
        .unwrap();
}

const SENTINEL: &str = "sentinel-not-in-report";
const BINDING_SECRET: &str = "binding-secret-not-in-report";
const OUTSIDE: &str = "/outside/sending.gcode";

/// GRE-412 part 4: one kept link whose `part_id` is text, a link Node drops,
/// 18 open prints, and a queue of `done`, `queued` and `sending`.
fn checkoff_source() -> Installation {
    let folder = node_folder();
    let root = folder.path().to_path_buf();
    let sentinel = SENTINEL;
    let binding_secret = BINDING_SECRET;
    put(
        &folder,
        "printer.checkoff_links",
        &serde_json::json!([
            {
                "id": "link-1",
                "profile_id": 1,
                "integration_id": "moon",
                "printer_id": "p-moon",
                "filename": "linked.gcode",
                "state": "watching",
                "created_at": "2026-11-05T12:00:00.000Z",
                "units": [{"part_id": "15", "unit_index": 2}],
                "note": sentinel
            },
            {"id": "dropped", "state": "bogus"}
        ]),
    );
    let prints: Vec<_> = (0..18)
        .map(|index| {
            serde_json::json!({
                "id": format!("p{index}"),
                "integration_id": "moon",
                "printer_id": "p-moon",
                "filename": if index == 0 { "fresh.gcode".to_owned() } else { format!("print-{index}.gcode") },
                "completed_at": "2026-11-05T12:00:00.000Z",
            })
        })
        .collect();
    put(
        &folder,
        "printer.unattributed_prints",
        &serde_json::Value::Array(prints),
    );
    put(
        &folder,
        "printer.plan_bindings",
        &serde_json::json!([{
            "integration_id": "moon",
            "profile_id": 1,
            "updated_at": "2026-11-05T00:00:00.000Z",
            "token": binding_secret
        }]),
    );
    let outside = OUTSIDE;
    put(
        &folder,
        "printer.send_queue",
        &serde_json::json!([
            {
                "id": "q-done",
                "filename": "done.gcode",
                "artifact_path": root.join("exports/done.gcode"),
                "printer_id": "p-moon",
                "state": "done",
                "checkoff_units": [{"part_id": 12, "unit_index": 0}],
                "created_at": "2026-11-05T12:00:00.000Z",
                "updated_at": "2026-11-05T12:00:00.000Z"
            },
            {
                "id": "q-queued",
                "filename": "fresh.gcode",
                "artifact_path": root.join("exports/queued.gcode"),
                "printer_id": "p-moon",
                "profile_id": "1",
                "state": "queued",
                "created_at": "2026-11-05T12:00:00.000Z",
                "updated_at": "2026-11-05T12:00:00.000Z"
            },
            {
                "id": "q-sending",
                "filename": "sending.gcode",
                "artifact_path": outside,
                "printer_id": "p-moon",
                "state": "sending",
                "created_at": "2026-11-05T12:00:00.000Z",
                "updated_at": "2026-11-05T12:00:00.000Z"
            }
        ]),
    );
    folder
}

#[test]
fn checkoff_prints_and_the_send_queue_convert() {
    let folder = checkoff_source();
    let report = run(folder.path()).unwrap();
    let text = report.to_string();
    assert_eq!(report.unmatched_prints, 18, "{text}");
    for line in [
        "printer.checkoff_links index 1: state is not a link state = 1",
        "printer.checkoff_links units.part_id stored as text = 1",
        "dropped printer.checkoff_links note = 1",
        "dropped printer.plan_bindings token = 1",
        "printer.send_queue state: queued -> error = 1",
        "printer.send_queue state: sending -> error = 1",
        "printer.send_queue profile_id stored as text = 1",
        "rebased printer.send_queue artifact_path = 2",
        "kept printer.send_queue artifact_path = 1",
    ] {
        assert!(text.contains(line), "{line} missing from:\n{text}");
    }
    assert!(report.unnamed.is_empty(), "{:?}", report.unnamed);
    assert!(!text.contains("would refuse"), "{text}");
    for hidden in [SENTINEL, BINDING_SECRET, OUTSIDE, "15"] {
        assert!(!text.contains(hidden), "{hidden} is in the report:\n{text}");
    }
}

#[test]
fn the_imported_queue_is_error_and_farm_reads_the_table() {
    let folder = checkoff_source();
    let parent = tempfile::tempdir().unwrap();
    let destination = parent.path().join("rust");
    let imported = import(folder.path(), &destination).unwrap();
    let database = destination.join("print-partner.db");
    let mut connection = Connection::open(&database).unwrap();
    let queue_setting: Option<String> = connection
        .query_row(
            "SELECT value FROM app_settings WHERE key = 'printer.send_queue'",
            [],
            |row| row.get(0),
        )
        .optional()
        .unwrap();
    assert_eq!(queue_setting, None);
    let uploads: i64 = connection
        .query_row(
            "SELECT COUNT(*) FROM durable_jobs WHERE kind = 'printer-upload'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(uploads, 0);
    let settings: i64 = connection
        .query_row("SELECT COUNT(*) FROM app_settings", [], |row| row.get(0))
        .unwrap();
    assert_eq!(imported.tables["app_settings"], settings as u64);
    let units: String = connection
        .query_row(
            "SELECT checkoff_units FROM printer_send_queue WHERE id = 'q-done'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(units, r#"[{"part_id":12,"unit_index":0}]"#);
    let stored: Vec<(String, String, Option<String>)> = connection
        .prepare(
            "SELECT state, artifact_path, error FROM printer_send_queue
             WHERE tenant_id = 'default' ORDER BY position",
        )
        .unwrap()
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))
        .unwrap()
        .map(|row| row.unwrap())
        .collect();
    let canonical = destination.canonicalize().unwrap();
    let path = |name: &str| canonical.join(name).to_string_lossy().into_owned();
    assert_eq!(
        stored,
        vec![
            ("done".into(), path("exports/done.gcode"), None),
            (
                "error".into(),
                path("exports/queued.gcode"),
                Some(NOT_STARTED_ERROR.into())
            ),
            (
                "error".into(),
                OUTSIDE.into(),
                Some(INTERRUPTED_ERROR.into())
            ),
        ]
    );
    let links = serde_json::from_str(&setting_at(&database, "printer.checkoff_links")).unwrap();
    let prints =
        serde_json::from_str(&setting_at(&database, "printer.unattributed_prints")).unwrap();
    let queue = crate::send_queue::rows(&connection, "default")
        .unwrap()
        .unwrap();
    let farm = Farm::parse(Vec::new(), Vec::new(), links, prints, queue);
    let results = farm.checkoff().results;
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].id, "link-1");
    assert_eq!(results[0].state, CheckoffResultState::Watching);
    assert_eq!(results[0].units[0].part_id, 15);
    assert_eq!(results[0].units[0].unit_index, 2);
    assert_eq!(farm.unmatched().prints.len(), 18);
    let queued = farm.send_queue().items;
    assert_eq!(
        queued.iter().map(|item| item.state).collect::<Vec<_>>(),
        [SendState::Done, SendState::Error, SendState::Error]
    );
    assert!(queued.iter().all(|item| item.upload_job_id.is_none()));
    let tx = connection.transaction().unwrap();
    let tasks = FarmTasks::read(&tx, "default").unwrap();
    assert_eq!(tasks.matches(1), 1);
    let bindings = setting_at(&database, "printer.plan_bindings");
    assert!(!bindings.contains(BINDING_SECRET), "{bindings}");
    assert!(bindings.contains("\"profile_id\":1"), "{bindings}");
}

#[test]
fn a_bad_plan_binding_or_a_repeated_send_id_refuses_the_import() {
    let folder = node_folder();
    put(
        &folder,
        "printer.plan_bindings",
        &serde_json::json!([{"integration_id": "  ", "profile_id": 1, "updated_at": "2026-11-05T00:00:00.000Z"}]),
    );
    let report = run(folder.path()).unwrap();
    let text = report.to_string();
    assert!(
        text.contains("printer.plan_bindings index 0: is not a binding = 1"),
        "{text}"
    );
    assert!(
        text.contains("The import would refuse this installation."),
        "{text}"
    );
    let parent = tempfile::tempdir().unwrap();
    let destination = parent.path().join("rust");
    let error = import(folder.path(), &destination).unwrap_err();
    assert!(matches!(error, ImportError::Settings(_)), "{error:?}");
    assert!(error.to_string().contains("not the typed shape"));
    assert!(!destination.join(ownership_marker::MARKER_FILE).exists());

    let folder = node_folder();
    let item = serde_json::json!({
        "id": "same",
        "filename": "a.gcode",
        "artifact_path": "/outside/a.gcode",
        "printer_id": "p-moon",
        "state": "done",
        "created_at": "2026-11-05T12:00:00.000Z",
        "updated_at": "2026-11-05T12:00:00.000Z"
    });
    put(
        &folder,
        "printer.send_queue",
        &serde_json::json!([item, item]),
    );
    let report = run(folder.path()).unwrap();
    assert!(
        report
            .to_string()
            .contains("printer.send_queue index 1: repeated id = 1"),
        "{report}"
    );
    let destination = parent.path().join("again");
    let error = import(folder.path(), &destination).unwrap_err();
    assert!(matches!(error, ImportError::Settings(_)), "{error:?}");
    assert!(!destination.join(ownership_marker::MARKER_FILE).exists());

    let folder = node_folder();
    let climbed = folder.path().join("exports/../secret.gcode");
    put(
        &folder,
        "printer.send_queue",
        &serde_json::json!([{
            "id": "climbs",
            "filename": "secret.gcode",
            "artifact_path": climbed,
            "printer_id": "p-moon",
            "state": "done",
            "created_at": "2026-11-05T12:00:00.000Z",
            "updated_at": "2026-11-05T12:00:00.000Z"
        }]),
    );
    let report = run(folder.path()).unwrap();
    assert!(
        report
            .to_string()
            .contains("artifact_path leaves the installation = 1"),
        "{report}"
    );
    assert!(!report.to_string().contains("secret.gcode"), "{report}");
    let destination = parent.path().join("climbed");
    let error = import(folder.path(), &destination).unwrap_err();
    assert!(matches!(error, ImportError::Settings(_)), "{error:?}");
    assert!(!destination.join(ownership_marker::MARKER_FILE).exists());
}

#[test]
fn a_drive_relative_artifact_path_refuses_the_import() {
    let folder = node_folder();
    let drive = folder.path().join("exports/C:secret-not-in-report");
    put(
        &folder,
        "printer.send_queue",
        &serde_json::json!([{
            "id": "drive",
            "filename": "drive.gcode",
            "artifact_path": drive,
            "printer_id": "p-moon",
            "state": "done",
            "created_at": "2026-11-05T12:00:00.000Z",
            "updated_at": "2026-11-05T12:00:00.000Z"
        }]),
    );
    let report = run(folder.path()).unwrap();
    assert!(
        report
            .to_string()
            .contains("artifact_path leaves the installation = 1"),
        "{report}"
    );
    assert!(
        !report.to_string().contains("secret-not-in-report"),
        "{report}"
    );
    let parent = tempfile::tempdir().unwrap();
    let destination = parent.path().join("drive");
    let error = import(folder.path(), &destination).unwrap_err();
    assert!(matches!(error, ImportError::Settings(_)), "{error:?}");
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

const VENDOR: &str = "do-not-print-vendor";
const EVENT_TEXT: &str = "do-not-print-event";
const TOKEN: &str = "do-not-print-token";
const FILAMENT_SECRET: &str = "do-not-print-filament";
const STORE_NOTE: &str = "do-not-print-note";

fn sources_source() -> Installation {
    let folder = node_folder();
    let connection = Connection::open(folder.database_path()).unwrap();
    connection
        .execute(
            "UPDATE projects SET metadata_json = ?1, imported_paths = ?2 WHERE id = 1",
            (
                format!(r#"{{"category":"  Printer kits  ","vendor":"{VENDOR}"}}"#),
                r#"["  parts/  ", 4, "   ", "\\stl\\part.stl"]"#,
            ),
        )
        .unwrap();
    let mods = serde_json::json!({"category": "\u{a0}Mods\u{a0}".to_owned()}).to_string();
    connection
        .execute(
            "UPDATE projects SET metadata_json = ?1, imported_paths = ?2 WHERE id = 2",
            (mods, r#"["  mods/  "]"#),
        )
        .unwrap();
    connection
        .execute(
            "INSERT INTO app_settings (tenant_id, key, value) VALUES ('default', 'source_categories', ?1)",
            [r#"["  Printer kits  ","Printer kits/Mods","Hardware/Screws",2]"#],
        )
        .unwrap();
    connection
        .execute(
            "INSERT INTO app_events (at, kind, payload_json) VALUES ('2026-11-01T00:00:00.000Z', 'source.updated', ?1)",
            [format!(
                r#"{{"source_id":1,"source_name":"Synthetic Source 01","error":"{EVENT_TEXT}","token":"{TOKEN}"}}"#
            )],
        )
        .unwrap();
    std::fs::write(
        folder.path().join("custom_filaments.json"),
        format!(
            r#"{{"filaments":[{{"id":"custom-1","display_name":"  Mine  ","product_line":" Custom ","hex":" #AABBCC ","combo_label":"Mine","secret":"{FILAMENT_SECRET}"}}],"note":"{STORE_NOTE}"}}"#
        ),
    )
    .unwrap();
    folder
}

#[test]
fn sources_metadata_rules_activity_and_filaments_convert() {
    let folder = sources_source();
    let report = run(folder.path()).unwrap();
    let text = report.to_string();
    for line in [
        "projects.metadata_json category trimmed = 2",
        "kept projects.metadata_json vendor = 1",
        "projects.imported_paths index 1: is not text = 1",
        "projects.imported_paths index 2: is blank = 1",
        "projects.imported_paths normalized = 3",
        "source_categories index 3: is not text = 1",
        "source_categories normalized = 1",
        "source_categories expanded = 1",
        "kept app_events token = 1",
        "kept app_events source = 1",
        "custom_filaments.json trimmed = 3",
        "kept custom_filaments.json secret = 1",
        "dropped custom_filaments.json note = 1",
    ] {
        assert!(text.contains(line), "{line} missing from:\n{text}");
    }
    assert!(!text.contains("would refuse"), "{text}");
    for hidden in [
        VENDOR,
        EVENT_TEXT,
        TOKEN,
        FILAMENT_SECRET,
        STORE_NOTE,
        "parts/",
    ] {
        assert!(!text.contains(hidden), "{hidden} is in the report:\n{text}");
    }

    let parent = tempfile::tempdir().unwrap();
    let destination = parent.path().join("rust");
    import(folder.path(), &destination).unwrap();
    let mut connection = Connection::open(destination.join("print-partner.db")).unwrap();
    let tx = connection.transaction().unwrap();
    let first = crate::catalog::get(&tx, "default", 1).unwrap().unwrap();
    let second = crate::catalog::get(&tx, "default", 2).unwrap().unwrap();
    assert_eq!(first.category.as_deref(), Some("Printer kits"));
    assert_eq!(second.category.as_deref(), Some("Mods"));
    let rules = |id: i64| -> String {
        tx.query_row(
            "SELECT imported_paths FROM projects WHERE id = ?1",
            [id],
            |row| row.get(0),
        )
        .unwrap()
    };
    assert_eq!(rules(1), r#"["parts/","stl/part.stl"]"#);
    assert_eq!(rules(2), r#"["mods/"]"#);
    let metadata: String = tx
        .query_row(
            "SELECT metadata_json FROM projects WHERE id = 1",
            [],
            |row| row.get(0),
        )
        .unwrap();
    let metadata: serde_json::Value = serde_json::from_str(&metadata).unwrap();
    assert_eq!(metadata["vendor"], VENDOR);
    assert_eq!(
        crate::catalog::source_categories(&tx, "default").unwrap(),
        [
            "Printer kits",
            "Printer kits/Mods",
            "Hardware",
            "Hardware/Screws",
        ]
    );
    let events = crate::catalog::source_activity(&tx, "default", 20).unwrap();
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].kind, "source.updated");
    assert_eq!(events[0].source_name, "Synthetic Source 01");
    assert_eq!(
        events[0]
            .source_id
            .as_ref()
            .map(ToString::to_string)
            .as_deref(),
        Some("1")
    );
    assert_eq!(events[0].detail.as_deref(), Some(EVENT_TEXT));
    drop(tx);
    let crate::admin::CustomFilaments::Body(body) = crate::admin::custom_filaments(&destination)
    else {
        panic!("filament store did not read");
    };
    let store: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(store["filaments"][0]["display_name"], "Mine");
    assert_eq!(store["filaments"][0]["product_line"], "Custom");
    assert_eq!(store["filaments"][0]["hex"], "#AABBCC");
    assert_eq!(store["filaments"][0]["id"], "custom-1");
    assert_eq!(store["filaments"][0]["secret"], FILAMENT_SECRET);
    assert!(store.get("note").is_none(), "{store}");
}

#[test]
fn a_filament_entry_that_is_not_an_object_refuses_the_import() {
    let folder = node_folder();
    std::fs::write(
        folder.path().join("custom_filaments.json"),
        r#"{"filaments":[1]}"#,
    )
    .unwrap();
    let report = run(folder.path()).unwrap();
    let text = report.to_string();
    assert!(
        text.contains("custom_filaments.json index 0: is not an object = 1"),
        "{text}"
    );
    assert!(
        text.contains("The import would refuse this installation."),
        "{text}"
    );
    let parent = tempfile::tempdir().unwrap();
    let destination = parent.path().join("rust");
    let error = import(folder.path(), &destination).unwrap_err();
    assert!(matches!(error, ImportError::Settings(_)), "{error:?}");
    assert!(!destination.join(ownership_marker::MARKER_FILE).exists());

    let folder = node_folder();
    std::fs::write(folder.path().join("custom_filaments.json"), "null").unwrap();
    let report = run(folder.path()).unwrap();
    assert!(
        report
            .to_string()
            .contains("custom_filaments.json is null = 1"),
        "{report}"
    );
    let destination = parent.path().join("null-store");
    let error = import(folder.path(), &destination).unwrap_err();
    assert!(matches!(error, ImportError::Settings(_)), "{error:?}");
    assert!(!destination.join(ownership_marker::MARKER_FILE).exists());
}

#[test]
fn metadata_that_is_not_an_object_is_kept_as_raw_and_not_printed() {
    let folder = node_folder();
    let raw = r#"["do-not-print-raw"]"#;
    execute(
        &folder,
        &format!("UPDATE projects SET metadata_json = '{raw}' WHERE id = 1"),
    );
    let report = run(folder.path()).unwrap();
    let text = report.to_string();
    assert!(
        text.contains("projects.metadata_json is not an object = 1"),
        "{text}"
    );
    assert!(!text.contains("do-not-print-raw"), "{text}");
    let parent = tempfile::tempdir().unwrap();
    let destination = parent.path().join("rust");
    import(folder.path(), &destination).unwrap();
    let connection = Connection::open(destination.join("print-partner.db")).unwrap();
    let metadata: String = connection
        .query_row(
            "SELECT metadata_json FROM projects WHERE id = 1",
            [],
            |row| row.get(0),
        )
        .unwrap();
    let metadata: serde_json::Value = serde_json::from_str(&metadata).unwrap();
    assert_eq!(metadata["raw"], raw);
}

const RAW_KEY: &str = "ppk_synthetic_not_a_real_key";
const ROLE_NOTE: &str = "hidden-role-note";
const MODULE_NOTE: &str = "hidden-module";
const SPOOL: &str = "4242";

fn builds_source() -> Installation {
    let folder = node_folder();
    let legacy = {
        use base64::Engine;
        base64::engine::general_purpose::STANDARD.encode(RAW_KEY)
    };
    let setup = serde_json::json!({
        "format": "production-setup-v1",
        "profile_id": 1,
        "preferred_slicer_instance_id": "bench-slicer",
        "selection": {"mode": "all_incomplete"},
        "printer_assignments": [],
        "route": "stl",
        "rules": [],
        "updated_at": "2026-01-01T00:00:00.000Z",
    });
    let role = format!(
        r#"{{"Primary":{{"filament_color_id":"filament-red-kept","spoolman_spool_id":{SPOOL},"note":"{ROLE_NOTE}"}}}}"#
    );
    let kit = format!(
        r#"{{"name":"Bench kit","selections":{{"group":"variant-a"}},"modules":"{MODULE_NOTE}"}}"#
    );
    let keys = serde_json::json!([{
        "id": "key_synthetic",
        "keyHash": legacy,
        "createdAt": "2026-10-02T00:00:00.000Z",
        "lastUsedAt": null,
        "expiresAt": null,
        "isActive": true,
    }]);
    execute(
        &folder,
        &format!(
            "UPDATE app_settings SET value = '{setup}' WHERE key = 'production_setup:1';
             UPDATE app_settings SET value = '{{\"farm\":true,\"not-a-module\":true}}' WHERE key = 'feature_toggles';
             INSERT INTO app_settings (tenant_id, key, value) VALUES ('default', 'role_filaments_1', '{role}');
             INSERT INTO app_settings (tenant_id, key, value) VALUES ('default', 'plan_variant_selection_1', '{{\"size\":3,\"color\":\"red\"}}');
             UPDATE app_settings SET value = '{kit}' WHERE key = 'kit_manifest_2';
             UPDATE plan_snapshots SET payload_json = '{{\"parts\":[],\"layers\":[{{\"source_name\":\"Synthetic Source 01\",\"layer_type\":\"base\",\"project_id\":1}}],\"accepted_plan_revision_id\":7}}' WHERE name = 'Synthetic snapshot';
             INSERT INTO app_settings (tenant_id, key, value) VALUES ('default', 'api_keys_v1', '{keys}');"
        ),
    );
    std::fs::write(
        folder
            .path()
            .join("repos/2")
            .join(".printpartner-source-snapshot.json"),
        r#"{"version":1,"upstreamRevisionKey":"synthetic-b","manifestDigest":"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa","selection":{"maxStlFiles":0,"maxDocumentationBytes":0,"omittedFiles":[]},"files":[]}"#,
    )
    .unwrap();
    folder
}

#[test]
fn builds_plans_toggles_and_api_keys_convert_and_read_back() {
    let folder = builds_source();
    let dry = run(folder.path()).unwrap();
    let text = dry.to_string();
    assert!(
        text.contains("dropped feature_toggles not-a-module = 1"),
        "{text}"
    );
    assert!(
        text.contains("role_filaments spool id stored as text = 1"),
        "{text}"
    );
    assert!(text.contains("dropped role_filaments note = 1"), "{text}");
    assert!(
        text.contains("plan_variant_selection value stored as text = 1"),
        "{text}"
    );
    assert!(text.contains("dropped kit_manifest modules = 1"), "{text}");
    assert!(text.contains("api_keys_v1 re-hashed = 1"), "{text}");
    assert!(
        text.contains("plan_snapshots payload stored as SnapshotPayload = 1"),
        "{text}"
    );
    assert!(!text.contains("would refuse"), "{text}");
    assert!(!text.contains(RAW_KEY), "{text}");
    assert!(!text.contains(ROLE_NOTE), "{text}");
    assert!(!text.contains(MODULE_NOTE), "{text}");
    assert!(!text.contains(SPOOL), "{text}");
    assert!(!text.contains("filament-red-kept"), "{text}");

    let parent = tempfile::tempdir().unwrap();
    let destination = parent.path().join("rust");
    let imported = import(folder.path(), &destination).unwrap();
    assert!(imported.complete, "{imported}");
    assert!(imported.rows.iter().all(|row| row.status != "pending"));
    assert_eq!(imported.rows.last().unwrap().status, "checked");
    imported_build_reads_back(&destination);
}

fn imported_build_reads_back(destination: &Path) {
    let database = destination.join("print-partner.db");
    let setup = setting_at(&database, "production_setup:1");
    let setup = crate::production::setup::stored_setup(Some(&setup), 1).unwrap();
    assert_eq!(setup["preferred_slicer_instance_id"], "bench-slicer");
    assert_eq!(setup["route"], "stl");
    assert_eq!(setup["profile_id"], 1);

    let roles: serde_json::Value =
        serde_json::from_str(&setting_at(&database, "role_filaments_1")).unwrap();
    assert_eq!(roles["Primary"]["filament_color_id"], "filament-red-kept");
    assert_eq!(roles["Primary"]["spoolman_spool_id"], SPOOL);
    assert!(roles["Primary"]["filament_custom_hex"].is_null());
    assert!(roles["Primary"].get("note").is_none(), "{roles}");

    let selection: serde_json::Value =
        serde_json::from_str(&setting_at(&database, "plan_variant_selection_1")).unwrap();
    assert_eq!(selection["size"], "3");
    assert_eq!(selection["color"], "red");

    let kit_raw = setting_at(&database, "kit_manifest_2");
    crate::manifest_options::KitManifest::from_stored(&kit_raw).unwrap();
    let kit: serde_json::Value = serde_json::from_str(&kit_raw).unwrap();
    assert_eq!(kit["name"], "Bench kit");
    assert_eq!(kit["selections"]["group"], "variant-a");
    assert!(kit.get("modules").is_none(), "{kit}");

    let toggles: pp_domain::features::Toggles =
        serde_json::from_str(&setting_at(&database, "feature_toggles")).unwrap();
    assert_eq!(toggles.get(pp_domain::features::Feature::Farm), Some(true));
    assert!(toggles.get(pp_domain::features::Feature::Orders).is_none());
    let plan: pp_domain::features::Toggles =
        serde_json::from_str(&setting_at(&database, "feature_toggles:plan:1")).unwrap();
    assert_eq!(plan.get(pp_domain::features::Feature::Bambu), Some(false));

    let connection = Connection::open(&database).unwrap();
    let payload: String = connection
        .query_row("SELECT payload_json FROM plan_snapshots", [], |row| {
            row.get(0)
        })
        .unwrap();
    assert_eq!(
        crate::working_drafts::restore::snapshot_revision(&payload),
        Some(7)
    );
    let payload: serde_json::Value = serde_json::from_str(&payload).unwrap();
    assert_eq!(payload["layers"][0]["source_name"], "Synthetic Source 01");
    assert_eq!(payload["layers"][0]["project_id"], 1);
    assert_eq!(payload["parts"], serde_json::json!([]));

    let keys = setting_at(&database, "api_keys_v1");
    assert!(crate::auth::collection_accepts(&keys, RAW_KEY).unwrap());
    assert!(!keys.contains(RAW_KEY), "{keys}");
    assert_eq!(
        setting_at(&database, "build_planning.create.synthetic-key-7"),
        "1"
    );
    let manifest = destination
        .join("repos/2")
        .join(".printpartner-source-snapshot.json");
    assert!(
        std::fs::read_to_string(manifest)
            .unwrap()
            .contains(r#""version":1"#)
    );
}

#[test]
fn a_build_document_the_typed_shape_cannot_store_is_refused() {
    let folder = node_folder();
    execute(
        &folder,
        "UPDATE app_settings SET value = '{\"farm\":\"yes-not-a-bool\"}' WHERE key = 'feature_toggles';
         INSERT INTO app_settings (tenant_id, key, value) VALUES ('default', 'build_planning.v1.1', '{\"secret\":\"brief-secret-not-real\"}');
         INSERT INTO app_settings (tenant_id, key, value) VALUES ('default', 'build_planning.create.create-key-token-not-real', 'create-value-not-a-build');",
    );
    std::fs::write(
        folder
            .path()
            .join("repos/1/revisions/synthetic-a")
            .join(".printpartner-source-snapshot.json"),
        r#"{"version":2,"token":"snapshot-secret-not-real"}"#,
    )
    .unwrap();
    let text = run(folder.path()).unwrap().to_string();
    for line in [
        "feature_toggles choice is not a boolean = 1",
        "build_planning.v1 did not pass the typed reader = 1",
        "build_planning.create is not a Build id = 1",
        "source snapshot manifest version is not 1 = 1",
        "The import would refuse this installation.",
    ] {
        assert!(text.contains(line), "{line} missing from:\n{text}");
    }
    for hidden in [
        "yes-not-a-bool",
        "brief-secret-not-real",
        "create-key-token-not-real",
        "create-value-not-a-build",
        "snapshot-secret-not-real",
    ] {
        assert!(!text.contains(hidden), "{hidden} is in the report:\n{text}");
    }
    let parent = tempfile::tempdir().unwrap();
    let destination = parent.path().join("rust");
    let error = import(folder.path(), &destination).unwrap_err();
    assert!(matches!(error, ImportError::Settings(_)), "{error:?}");
    assert!(!destination.join(ownership_marker::MARKER_FILE).exists());
}
