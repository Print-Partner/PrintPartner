//! Node's accepted-plate fixture: a disposable data dir holding a copy of
//! `accepted-plate-node.db`, and the `no_head` phase of the Plate initialize
//! golden, whose seed pins `repos/1/revisions/accepted/included.stl`.
use serde_json::Value;
use std::path::Path;

/// A disposable data dir holding a copy of the Node-written fixture.
pub fn data_dir() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join("repos/1/revisions/accepted")).unwrap();
    std::fs::write(
        dir.path().join("print-partner.db"),
        include_bytes!("../../../pp-storage/tests/fixtures/accepted-plate-node.db"),
    )
    .unwrap();
    dir
}

/// The `no_head` phase of Node's initialize golden on a fresh data dir: its
/// STL files, re-pinned digests and fleet, and its `creates_head` body.
pub fn initialize_fixture(root: &Path) -> Value {
    let golden: Value = serde_json::from_str(include_str!(
        "../../../../../fixtures/parity/plates-initialize/node.json"
    ))
    .unwrap();
    let phase = golden["phases"]
        .as_array()
        .unwrap()
        .iter()
        .find(|phase| phase["name"] == "no_head")
        .unwrap();
    for file in phase["files"].as_array().unwrap() {
        std::fs::write(
            root.join(file["path"].as_str().unwrap()),
            file["text"].as_str().unwrap(),
        )
        .unwrap();
    }
    let sql = rusqlite::Connection::open(root.join("print-partner.db")).unwrap();
    let triggers: Vec<String> = sql
        .prepare("SELECT name FROM sqlite_schema WHERE type='trigger'")
        .unwrap()
        .query_map([], |row| row.get(0))
        .unwrap()
        .map(Result::unwrap)
        .collect();
    for trigger in triggers {
        sql.execute_batch(&format!("DROP TRIGGER \"{trigger}\""))
            .unwrap();
    }
    sql.execute_batch(&format!(
        "PRAGMA foreign_keys=OFF; {}",
        phase["seed"].as_str().unwrap()
    ))
    .unwrap();
    phase["cases"]
        .as_array()
        .unwrap()
        .iter()
        .find(|case| case["id"] == "creates_head")
        .unwrap()["body"]
        .clone()
}
