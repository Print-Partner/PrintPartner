#![cfg(feature = "desktop_rebuild")]
//! GRE-1091: match suggestions read the disposable accepted-plate fixture
//! on disk and write nothing.
use pp_storage::{
    Limits, WriterOwner,
    auth::{self, Secret},
    read_model::{
        Credential,
        farm::{MatchConfidence, MatchSuggestion},
    },
};
use rusqlite::{Connection, params};
use serde_json::{Value, json};
use std::{
    path::{Path, PathBuf},
    sync::{Arc, atomic::AtomicBool},
    time::Duration,
};

/// The fixture's one Plate holds one Required unit of Build 1's "included.stl".
const TOKEN: &str = "ppu_574c8d5f92624f029e21705a8ebb8a1b";

struct Fixture {
    root: PathBuf,
    owner: Option<WriterOwner>,
}

impl Fixture {
    fn new(prints: &Value) -> Self {
        let root = std::env::temp_dir().join(format!(
            "pp-match-suggestions-{}-{}",
            std::process::id(),
            rand::random::<u64>()
        ));
        pp_os::create_private_dir_all(&root).unwrap();
        std::fs::write(
            root.join("print-partner.db"),
            include_bytes!("fixtures/accepted-plate-node.db"),
        )
        .unwrap();
        let fixture = Self { root, owner: None };
        fixture
            .raw()
            .execute(
                "INSERT INTO app_settings(tenant_id,key,value) VALUES('default','printer.unattributed_prints',?1)",
                params![prints.to_string()],
            )
            .unwrap();
        fixture
    }

    fn raw(&self) -> Connection {
        let connection = Connection::open(self.root.join("print-partner.db")).unwrap();
        connection.busy_timeout(Duration::from_secs(3)).unwrap();
        connection
    }

    /// Opens storage, which migrates the fixture to the current schema.
    fn open(&mut self) -> &WriterOwner {
        let root: &Path = &self.root;
        self.owner.get_or_insert_with(|| {
            WriterOwner::open(
                root,
                Limits {
                    queued_writes: 2,
                    readers: 1,
                },
            )
            .unwrap()
            .0
        })
    }

    fn suggestions(&mut self) -> Vec<MatchSuggestion> {
        self.open()
            .farm_reads(auth::AuthPolicy {
                registration: auth::RegistrationPolicy::Open,
                session_tenant: auth::SessionTenantPolicy::AccountTenant,
                first_user: auth::FirstUserTenant::NewUser,
            })
            .unwrap()
            .read_for_suggestions(
                Credential::Session(Secret::new("read-fixture-secret".into())),
                Arc::new(AtomicBool::new(false)),
                Duration::from_secs(1),
            )
            .unwrap()
            .suggestions()
            .expect("read with the match library")
            .suggestions
    }

    /// SQLite's count of commits other connections made, seen from `watch`.
    fn commits(watch: &Connection) -> i64 {
        watch
            .pragma_query_value(None, "data_version", |row| row.get(0))
            .unwrap()
    }

    fn close(&mut self) {
        if let Some(owner) = self.owner.take() {
            owner.shutdown().unwrap();
        }
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        self.close();
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

fn print(id: &str, objects: &[&str]) -> Value {
    json!({"id": id, "integration_id": "moon", "printer_id": "p-moon", "host_name": "Micron",
        "filename": format!("{id}.gcode"), "completed_at": "2026-11-05T12:00:00.000Z",
        "gcode_objects": objects, "candidates": []})
}

fn prints() -> Value {
    json!([
        print("tokened", &[&format!("included__{TOKEN}.stl_id_0_copy_0")]),
        print("named", &["included.stl_id_0_copy_0"]),
        print("excluded", &["excluded.stl_id_0_copy_0"]),
        print("stranger", &[&format!("included__ppu_{}", "f".repeat(32))]),
        print("empty", &[]),
    ])
}

fn summary(rows: &[MatchSuggestion]) -> Vec<Value> {
    rows.iter()
        .map(|row| {
            json!([
                row.print_id,
                row.confidence,
                row.starts_checked,
                row.build.as_ref().map(|build| build.id),
                row.plate.as_ref().map(|plate| &plate.plate_id)
            ])
        })
        .collect()
}

#[test]
fn match_suggestions_on_disk_are_read_only_and_never_confirm() {
    let mut fixture = Fixture::new(&prints());
    fixture.open();
    let watch = fixture.raw();
    let before = Fixture::commits(&watch);
    let rows = fixture.suggestions();
    assert_eq!(
        summary(&rows),
        vec![
            json!(["tokened", "exact", true, 1, "fixture-plate"]),
            json!(["named", "likely", false, 1, "fixture-plate"]),
            json!(["excluded", "none", false, null, null]),
            json!(["stranger", "none", false, null, null]),
            json!(["empty", "none", false, null, null]),
        ]
    );
    assert_eq!(
        rows[0].build.as_ref().unwrap().name,
        "Tracked operational Build"
    );
    for _ in 0..3 {
        assert_eq!(fixture.suggestions(), rows, "a repeated read is stable");
    }
    assert_eq!(
        Fixture::commits(&watch),
        before,
        "suggestions committed a write"
    );
    fixture
        .raw()
        .execute(
            "INSERT INTO app_settings(tenant_id,key,value) VALUES('default','control','1')",
            [],
        )
        .unwrap();
    assert_ne!(
        Fixture::commits(&watch),
        before,
        "control: the check sees another connection's commit"
    );
    fixture.close();
    let stored: String = fixture
        .raw()
        .query_row(
            "SELECT value FROM app_settings WHERE tenant_id='default' AND key='printer.unattributed_prints'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    let stored: Value = serde_json::from_str(&stored).unwrap();
    assert!(
        stored
            .as_array()
            .unwrap()
            .iter()
            .all(|print| print.get("claimed_at").is_none()
                && print.get("claimed_profile_id").is_none()),
        "no print was claimed"
    );
}

#[test]
fn match_suggestions_plate_stems_keep_unicode_part_names() {
    let mut fixture = Fixture::new(&json!([print("named", &["café.stl_id_0_copy_0"])]));
    fixture.open();
    let raw = fixture.raw();
    let immutable: String = raw
        .query_row(
            "SELECT name FROM sqlite_master WHERE type='trigger' AND sql LIKE '%revision Parts are immutable%'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    raw.execute_batch(&format!(
        "DROP TRIGGER \"{immutable}\";
         UPDATE parts SET filename='café.stl' WHERE filename='included.stl';
         UPDATE plan_revision_parts SET filename='café.stl' WHERE filename='included.stl';"
    ))
    .unwrap();
    assert_eq!(
        summary(&fixture.suggestions()),
        vec![json!(["named", "likely", false, 1, "fixture-plate"])],
        "the exported object name is caf___ppu_..., the part file is café.stl"
    );
}

#[test]
fn match_suggestions_skip_archived_builds() {
    let mut fixture = Fixture::new(&prints());
    fixture
        .raw()
        .execute(
            "UPDATE build_profiles SET archived_at='2026-11-01T00:00:00.000Z' WHERE id=1",
            [],
        )
        .unwrap();
    let rows = fixture.suggestions();
    assert!(
        rows.iter()
            .all(|row| row.confidence == MatchConfidence::None && row.build.is_none()),
        "{rows:#?}"
    );
    assert!(rows[0].evidence.object_list.unit_build_ids.is_empty());
}
