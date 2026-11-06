//! Shared by the printer Checkoff HTTP tests: a disposable copy of a real
//! SQLite fixture with the synthetic links of the verify parity fixture, the
//! mounted Checkoff router, and loopback requests to it.
use axum::{Router, http::StatusCode};
use loopback::ORIGIN;
use pp_api::{accepted::AcceptedHttpConfig, checkoff::checkoff_router};
use pp_storage::{Limits, WriterOwner};
use serde_json::Value;
use std::path::PathBuf;

#[path = "loopback.rs"]
mod loopback;
const NODE: &str = include_str!("../../../../../fixtures/parity/printer-checkoff-verify/node.json");
const DATABASE: &[u8] =
    include_bytes!("../../../pp-storage/tests/fixtures/required-units/unchanged.db");

pub struct Fixture(PathBuf);

impl Fixture {
    pub fn new() -> Self {
        let path = tempfile::tempdir().unwrap().keep();
        std::fs::create_dir_all(path.join("repos/1/revisions/fixture")).unwrap();
        std::fs::write(path.join("print-partner.db"), DATABASE).unwrap();
        let fixture = Self(path);
        let sql = fixture.sql();
        for statement in crate::js_json::parse(NODE)["setup"].as_array().unwrap() {
            sql.execute_batch(statement.as_str().unwrap()).unwrap();
        }
        fixture
    }

    pub fn sql(&self) -> rusqlite::Connection {
        rusqlite::Connection::open(self.0.join("print-partner.db")).unwrap()
    }

    pub fn open(&self, flag: &str) -> (WriterOwner, Router) {
        let owner = WriterOwner::open(&self.0, Limits::default()).unwrap().0;
        let config = AcceptedHttpConfig::from_startup(ORIGIN, "default", Some(flag)).unwrap();
        let router = checkoff_router(config, owner.checkoff_progress().unwrap());
        (owner, router)
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

pub async fn post(router: &Router, path: &str, body: &str, cookie: bool) -> (StatusCode, Value) {
    send(router, "POST", path, body, cookie).await
}

pub async fn send(
    router: &Router,
    method: &str,
    path: &str,
    body: &str,
    cookie: bool,
) -> (StatusCode, Value) {
    let cookie = cookie.then_some("pp_session=required-unit-fixture-secret");
    loopback::send(router, method, path, body, cookie).await
}
