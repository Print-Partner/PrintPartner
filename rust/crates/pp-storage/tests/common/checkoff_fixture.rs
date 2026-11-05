use pp_storage::{Limits, WriterOwner};
use rusqlite::Connection;
use std::path::PathBuf;

pub const NODE: &str =
    include_str!("../../../../../fixtures/parity/printer-checkoff-verify/node.json");
const DATABASE: &[u8] = include_bytes!("../fixtures/required-units/unchanged.db");

/// A disposable copy of the required-units fixture with the synthetic links of
/// the printer Checkoff verify parity fixture.
pub struct Fixture {
    pub path: PathBuf,
}

impl Fixture {
    pub fn new() -> Self {
        let path = std::env::temp_dir().join(format!(
            "pp-printer-checkoff-{}",
            hex::encode(rand::random::<[u8; 16]>())
        ));
        std::fs::create_dir_all(path.join("repos/1/revisions/fixture")).unwrap();
        std::fs::write(path.join("print-partner.db"), DATABASE).unwrap();
        let fixture = Self { path };
        for sql in crate::js_json::parse(NODE)["setup"].as_array().unwrap() {
            fixture.sql().execute_batch(sql.as_str().unwrap()).unwrap();
        }
        fixture
    }

    pub fn sql(&self) -> Connection {
        Connection::open(self.path.join("print-partner.db")).unwrap()
    }

    pub fn open(&self) -> WriterOwner {
        WriterOwner::open(&self.path, Limits::default()).unwrap().0
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.path);
    }
}
