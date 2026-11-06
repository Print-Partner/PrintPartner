use super::*;
use crate::{Limits, WriterOwner, read_model::farm::Farm};
use std::path::Path;

fn open(directory: &Path) {
    let (owner, _) = WriterOwner::open(directory, Limits::default()).unwrap();
    owner.shutdown().unwrap();
}

fn database(directory: &Path) -> Connection {
    Connection::open(directory.join("print-partner.db")).unwrap()
}

fn store(directory: &Path, tenant: &str, value: &str) {
    database(directory)
        .execute(
            "INSERT INTO app_settings(tenant_id,key,value) VALUES(?1,?2,?3)",
            params![tenant, LEGACY_KEY, value],
        )
        .unwrap();
}

fn setting(directory: &Path, tenant: &str) -> Option<String> {
    database(directory)
        .query_row(
            "SELECT value FROM app_settings WHERE tenant_id=?1 AND key=?2",
            params![tenant, LEGACY_KEY],
            |row| row.get(0),
        )
        .optional()
        .unwrap()
}

fn table(directory: &Path) -> Vec<(String, i64, String, String)> {
    database(directory)
        .prepare("SELECT tenant_id,position,id,state FROM printer_send_queue ORDER BY tenant_id,position")
        .unwrap()
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)))
        .unwrap()
        .collect::<rusqlite::Result<_>>()
        .unwrap()
}

fn queue(directory: &Path, tenant: &str) -> Vec<Value> {
    let mut connection = database(directory);
    let tx = connection.transaction().unwrap();
    crate::read_model::farm::queue_list(&tx, tenant).unwrap()
}

fn sends(items: Vec<Value>) -> Value {
    serde_json::to_value(Farm::parse(vec![], vec![], vec![], vec![], items).send_queue()).unwrap()
}

fn legacy_queue() -> Value {
    json!([
        {"id": "q-done", "filename": "plate-1.gcode", "artifact_path": "/data/exports/printer-uploads/a/plate-1.gcode",
         "printer_id": "p-moon", "match": "pinned", "wait_for_idle": true, "start": true, "profile_id": 7,
         "checkoff_units": [{"part_id": 11, "unit_index": 0, "object_name": "bracket_ppu_1"}],
         "state": "done", "upload_job_id": "job-1", "host_name": "Micron",
         "created_at": "2026-11-05T01:00:00.000Z", "updated_at": "2026-11-05T01:05:00.000Z"},
        {"id": "q-sending", "filename": "plate-2.gcode", "artifact_path": "/data/exports/printer-uploads/b/plate-2.gcode",
         "printer_id": "p-moon", "wait_for_idle": false, "start": false, "profile_id": 7, "state": "sending",
         "upload_job_id": "job-2", "created_at": "2026-11-05T02:00:00.000Z", "updated_at": "2026-11-05T02:00:00.000Z"},
        {"id": " q-queued ", "filename": "plate-3.gcode", "artifact_path": "/data/exports/printer-uploads/c/plate-3.gcode",
         "printer_id": "p-prusa", "state": "queued", "error": "  old  ",
         "created_at": "2026-11-05T03:00:00.000Z", "updated_at": "2026-11-05T03:00:00.000Z"}
    ])
}

#[test]
fn ticket_t_26_open_moves_a_clean_legacy_queue_into_the_table() {
    let directory = tempfile::tempdir().unwrap();
    open(directory.path());
    let legacy = legacy_queue();
    store(directory.path(), "default", &legacy.to_string());
    let other = json!([{"id": "q-other", "filename": "x.gcode", "artifact_path": "/x/x.gcode",
        "printer_id": "p-moon", "state": "queued"}]);
    store(directory.path(), "acme", &other.to_string());
    let before = sends(queue(directory.path(), "default"));

    open(directory.path());

    assert_eq!(setting(directory.path(), "default"), None);
    assert_eq!(setting(directory.path(), "acme"), None);
    let row = |tenant: &str, position, id: &str, state: &str| {
        (tenant.to_owned(), position, id.to_owned(), state.to_owned())
    };
    assert_eq!(
        table(directory.path()),
        vec![
            row("acme", 0, "q-other", "queued"),
            row("default", 0, "q-done", "done"),
            row("default", 1, "q-sending", "error"),
            row("default", 2, "q-queued", "queued"),
        ],
        "each tenant keeps its own items, ids and order"
    );
    let items = queue(directory.path(), "default");
    assert_eq!(items[0], legacy[0], "a finished item moves unchanged");
    let interrupted = &items[1];
    assert_eq!(interrupted["state"], "error");
    assert_eq!(interrupted["error"], INTERRUPTED_ERROR);
    assert_ne!(interrupted["updated_at"], legacy[1]["updated_at"]);
    let mut expected = legacy[1].clone();
    for key in ["state", "error", "updated_at"] {
        expected[key] = interrupted[key].clone();
    }
    assert_eq!(interrupted, &expected, "only the interruption changed");
    assert_eq!(
        items[2],
        json!({"id": "q-queued", "filename": "plate-3.gcode",
            "artifact_path": "/data/exports/printer-uploads/c/plate-3.gcode", "printer_id": "p-prusa",
            "wait_for_idle": true, "start": false, "state": "queued", "error": "old",
            "created_at": "2026-11-05T03:00:00.000Z", "updated_at": "2026-11-05T03:00:00.000Z"}),
        "the item reads as Node's parseItem reads it"
    );
    let mut after = sends(items);
    after["items"][1]["state"] = before["items"][1]["state"].clone();
    after["items"][1]["updated_at"] = before["items"][1]["updated_at"].clone();
    assert_eq!(
        after, before,
        "the farm read is unchanged apart from the interruption"
    );

    let rows = table(directory.path());
    let read = queue(directory.path(), "default");
    open(directory.path());
    assert_eq!(
        (table(directory.path()), queue(directory.path(), "default")),
        (rows, read)
    );
}

#[test]
fn ticket_t_26_open_leaves_an_unclean_legacy_queue_untouched() {
    let item = r#""id":"q","filename":"a.gcode","artifact_path":"/a.gcode","printer_id":"p","state":"queued""#;
    for value in [
        "{}".to_owned(),
        "not json".to_owned(),
        String::new(),
        format!("[{{{item}}},{{{item}}}]"),
        r#"[{"id":"q","filename":"a.gcode","printer_id":"p","state":"queued"}]"#.to_owned(),
        format!(r#"[{{{item},"profile_id":"7"}}]"#),
        format!(r#"[{{{item},"profile_id":0}}]"#),
        format!(r#"[{{{item},"checkoff_units":{{}}}}]"#),
        format!(r#"[{{{item},"checkoff_units":[{{"part_id":"1","unit_index":0}}]}}]"#),
        format!(r#"[{{{item},"state":"sent"}}]"#),
    ] {
        let directory = tempfile::tempdir().unwrap();
        open(directory.path());
        store(directory.path(), "default", &value);
        let before = queue(directory.path(), "default");
        open(directory.path());
        assert_eq!(
            setting(directory.path(), "default").as_deref(),
            Some(value.as_str()),
            "{value}"
        );
        assert!(table(directory.path()).is_empty(), "{value}");
        assert_eq!(queue(directory.path(), "default"), before, "{value}");
    }
}

#[test]
fn ticket_t_26_a_legacy_setting_wins_over_table_rows() {
    let directory = tempfile::tempdir().unwrap();
    open(directory.path());
    let one = json!([{"id": "q1", "filename": "a.gcode", "artifact_path": "/a.gcode",
        "printer_id": "p", "state": "queued"}]);
    store(directory.path(), "default", &one.to_string());
    open(directory.path());
    let two = json!([{"id": "q2", "filename": "b.gcode", "artifact_path": "/b.gcode",
        "printer_id": "p", "state": "sending"}]);
    store(directory.path(), "default", &two.to_string());
    assert_eq!(rows(&database(directory.path()), "default").unwrap(), None);
    assert_eq!(
        queue(directory.path(), "default"),
        two.as_array().unwrap().clone()
    );

    open(directory.path());
    assert_eq!(
        setting(directory.path(), "default"),
        Some(two.to_string()),
        "items already in the table are never merged with a setting"
    );
    assert_eq!(
        table(directory.path()),
        vec![(
            "default".to_owned(),
            0,
            "q1".to_owned(),
            "queued".to_owned()
        )]
    );
}
