#![cfg(feature = "desktop_rebuild")]
//! `GET /plans/:id/history` storage read over the Node schema-34 fixture.

use pp_storage::{
    Limits, WriterOwner,
    auth::{AuthPolicy, FirstUserTenant, RegistrationPolicy, Secret, SessionTenantPolicy},
    build_graph::PositiveId,
    read_model::{
        Credential,
        history::{Cursor, HistoryClient, Outcome, Page, Request},
    },
};
use rusqlite::{Connection, types::Value as SqlValue};
use serde_json::{Value, json};
use std::{
    path::PathBuf,
    sync::{Arc, atomic::AtomicBool},
    time::Duration,
};

#[derive(Debug, PartialEq)]
struct Stored {
    rows: Vec<(String, Vec<Vec<SqlValue>>)>,
    files: Vec<(PathBuf, Vec<u8>)>,
}

#[path = "common/scratch.rs"]
mod scratch;

struct Fixture {
    root: scratch::Scratch,
    owner: Option<WriterOwner>,
    client: HistoryClient,
}

impl Fixture {
    fn new() -> Self {
        let root = scratch::Scratch::new("pp-build-history-");
        pp_os::create_private_dir_all(&root.join("repos/1/revisions/accepted")).unwrap();
        std::fs::write(
            root.join("repos/1/revisions/accepted/manifest.yaml"),
            "parts: []\n",
        )
        .unwrap();
        std::fs::write(
            root.join("print-partner.db"),
            include_bytes!("fixtures/accepted-plan-node.db"),
        )
        .unwrap();
        seed(&Connection::open(root.join("print-partner.db")).unwrap());
        let (owner, _) = WriterOwner::open(&root, Limits::default()).unwrap();
        let client = owner
            .build_history(AuthPolicy {
                registration: RegistrationPolicy::Open,
                session_tenant: SessionTenantPolicy::AccountTenant,
                first_user: FirstUserTenant::NewUser,
            })
            .unwrap();
        Self {
            root,
            owner: Some(owner),
            client,
        }
    }

    fn sql(&self) -> Connection {
        let connection = Connection::open(self.root.join("print-partner.db")).unwrap();
        connection.busy_timeout(Duration::from_secs(3)).unwrap();
        connection
    }

    fn read(&self, build: u64, cursor: Option<&str>, limit: u16) -> Outcome {
        self.client
            .read(
                Credential::Session(Secret::new("read-fixture-secret".into())),
                Request {
                    build: Some(PositiveId::new(build).unwrap()),
                    cursor: cursor.map(|raw| Cursor::parse(raw).unwrap()),
                    limit,
                },
                Arc::new(AtomicBool::new(false)),
                Duration::from_secs(1),
            )
            .unwrap()
    }

    fn page(&self, cursor: Option<&str>, limit: u16) -> Page {
        match self.read(1, cursor, limit) {
            Outcome::Page(page) => page,
            Outcome::MissingBuild => panic!("Build 1 missing"),
        }
    }

    /// Every table row in rowid order, and every file under the data
    /// directory except the SQLite database and its WAL and shared memory.
    fn stored(&self) -> Stored {
        fn select(db: &Connection, sql: &str) -> Vec<Vec<SqlValue>> {
            let mut statement = db.prepare(sql).unwrap();
            let width = statement.column_count();
            let mut cursor = statement.query([]).unwrap();
            let mut found = Vec::new();
            while let Some(row) = cursor.next().unwrap() {
                found.push((0..width).map(|i| row.get(i).unwrap()).collect());
            }
            found
        }
        let db = self.sql();
        let rows = select(
            &db,
            "SELECT name FROM sqlite_master WHERE type='table' ORDER BY name",
        )
        .into_iter()
        .map(|name| {
            let SqlValue::Text(table) = &name[0] else {
                panic!("table name {name:?}")
            };
            let sql = format!(
                "SELECT * FROM \"{}\" ORDER BY rowid",
                table.replace('"', "\"\"")
            );
            (table.clone(), select(&db, &sql))
        })
        .collect();
        let mut files = Vec::new();
        let mut stack = vec![self.root.to_path_buf()];
        while let Some(directory) = stack.pop() {
            for path in std::fs::read_dir(directory)
                .unwrap()
                .map(|e| e.unwrap().path())
            {
                let name = path.strip_prefix(&*self.root).unwrap().to_owned();
                if path.is_dir() {
                    stack.push(path);
                } else if ![
                    "print-partner.db",
                    "print-partner.db-wal",
                    "print-partner.db-shm",
                ]
                .contains(&name.to_str().unwrap())
                {
                    files.push((name, std::fs::read(&path).unwrap()));
                }
            }
        }
        files.sort();
        Stored { rows, files }
    }
}

/// Stops storage before `scratch` removes the directory.
impl Drop for Fixture {
    fn drop(&mut self) {
        self.owner
            .take()
            .map(WriterOwner::shutdown)
            .transpose()
            .unwrap();
    }
}

/// History rows for Build 1 around its fixture revision (accepted
/// 2026-10-02T21:06:51.938Z), plus rows another Build and another tenant own.
fn seed(db: &Connection) {
    db.execute_batch(
        "INSERT INTO build_profiles (id,tenant_id,name) VALUES (2,'default','Other Build');
         INSERT INTO plan_decisions (id,tenant_id,profile_id,created_at,actor,kind,label,summary) VALUES
           (1,'default',1,'2026-10-01T09:00:00.000Z','assistant','applied_action','Swap colour','Swapped the frame to PETG'),
           (2,'default',1,'2026-10-03T09:00:00.000Z','user','user_note','',''),
           (3,'default',2,'2026-10-03T09:00:00.000Z','assistant','choice','Other','Other Build decision'),
           (4,'foreign',1,'2026-10-03T09:00:00.000Z','assistant','choice','Foreign','Foreign tenant decision');
         INSERT INTO plan_snapshots (id,tenant_id,profile_id,name,created_at,source,payload_json) VALUES
           (1,'default',1,'Before PETG','2026-10-01T08:00:00.000Z','user','{}'),
           (2,'default',1,'','2026-10-02T21:06:50.000Z','pre_apply','{}'),
           (3,'default',1,'Assistant copy','2026-10-04T08:00:00.000Z','assistant','{}');
         INSERT INTO print_jobs (id,tenant_id,profile_id,filename,at,link_id) VALUES
           ('job-a','default',1,'plate_1.gcode','2026-10-05T20:00:00.000Z','link-a'),
           ('job-b','default',2,'other.gcode','2026-10-05T20:00:00.000Z','link-b');
         INSERT INTO print_job_parts (id,tenant_id,job_id,at,profile_id,part_id,unit_index,result) VALUES
           ('p1','default','job-a','2026-10-05T20:00:00.000Z',1,1,0,'confirmed'),
           ('p2','default','job-a','2026-10-05T20:00:00.000Z',1,2,0,'confirmed'),
           ('p3','default','job-a','2026-10-05T20:00:00.000Z',1,2,1,'rejected'),
           ('p4','default',NULL,'2026-09-30T10:00:00.000Z',1,1,1,'confirmed'),
           ('p5','default',NULL,'2026-09-30T10:00:00.000Z',1,2,2,'rejected'),
           ('p6','default',NULL,'2026-09-30T10:00:00.000Z',2,1,0,'confirmed');",
    )
    .unwrap();
    set_queue(
        db,
        &json!([
            {"id":"send-a","filename":"plate_1.gcode","printer_id":"mk4","host_name":"MK4","state":"done","profile_id":1,"created_at":"2026-10-05T19:00:00.000Z","updated_at":"2026-10-05T19:30:00.000Z"},
            {"id":"send-b","filename":"other.gcode","printer_id":"mk4","state":"done","profile_id":2,"created_at":"2026-10-05T19:00:00.000Z","updated_at":"2026-10-05T19:30:00.000Z"}
        ]),
    );
}

fn set_queue(db: &Connection, items: &Value) {
    db.execute(
        "INSERT INTO app_settings (tenant_id,key,value) VALUES ('default','printer.send_queue',?1)
         ON CONFLICT(tenant_id,key) DO UPDATE SET value=excluded.value",
        [items.to_string()],
    )
    .unwrap();
}

fn json_entries(page: &Page) -> Vec<Value> {
    page.entries
        .iter()
        .map(|entry| serde_json::to_value(entry).unwrap())
        .collect()
}

#[test]
fn build_history_lists_every_source_newest_first() {
    let fixture = Fixture::new();
    let page = fixture.page(None, 50);
    assert_eq!(page.plan_id, 1);
    assert_eq!(page.next_cursor, None);
    assert_eq!(
        json_entries(&page),
        vec![
            json!({"id":"checkoff_burst:job-a","type":"checkoff_burst","at":"2026-10-05T20:00:00.000Z","actor":"user",
                "summary":"Checked off 2 units, rejected 1 from plate_1.gcode","link":{"kind":"checkoff_burst","id":"job-a"},
                "undo":{"undoable":false,"reason":"no_undo_token"}}),
            json!({"id":"send:send-a","type":"send","at":"2026-10-05T19:00:00.000Z","actor":"user",
                "summary":"Sent plate_1.gcode to MK4","link":{"kind":"send","id":"send-a"},
                "undo":{"undoable":false,"reason":"printer_command"}}),
            json!({"id":"snapshot:3","type":"snapshot","at":"2026-10-04T08:00:00.000Z","actor":"assistant",
                "summary":"Assistant copy","link":{"kind":"snapshot","id":"3"}}),
            json!({"id":"decision:2","type":"decision","at":"2026-10-03T09:00:00.000Z","actor":"user",
                "summary":"user_note","link":{"kind":"decision","id":"2"}}),
            json!({"id":"revision:1","type":"revision","at":"2026-10-02T21:06:51.938Z","actor":"user",
                "summary":"Accepted revision 1","link":{"kind":"revision","id":"1"}}),
            json!({"id":"snapshot:2","type":"snapshot","at":"2026-10-02T21:06:50.000Z","actor":"user",
                "summary":"Snapshot before apply","link":{"kind":"snapshot","id":"2"}}),
            json!({"id":"decision:1","type":"decision","at":"2026-10-01T09:00:00.000Z","actor":"assistant",
                "summary":"Swapped the frame to PETG","link":{"kind":"decision","id":"1"}}),
            json!({"id":"snapshot:1","type":"snapshot","at":"2026-10-01T08:00:00.000Z","actor":"user",
                "summary":"Before PETG","link":{"kind":"snapshot","id":"1"}}),
            json!({"id":"checkoff_burst:outcomes-p4","type":"checkoff_burst","at":"2026-09-30T10:00:00.000Z","actor":"user",
                "summary":"Checked off 1 unit, rejected 1","link":{"kind":"checkoff_burst","id":"outcomes-p4"},
                "undo":{"undoable":false,"reason":"no_undo_token"}}),
        ]
    );
}

#[test]
fn build_history_cursor_pages_stay_stable_while_entries_arrive() {
    let fixture = Fixture::new();
    let all: Vec<String> = fixture
        .page(None, 50)
        .entries
        .into_iter()
        .map(|entry| entry.id)
        .collect();
    let first = fixture.page(None, 3);
    let mut seen: Vec<String> = first.entries.iter().map(|e| e.id.clone()).collect();
    let mut cursor = first.next_cursor;
    let mut arrivals = 0;
    while let Some(raw) = cursor {
        // New entries land after each page: a later decision, one at the
        // same instant as the page's last entry, and a newly queued send.
        arrivals += 1;
        let last_at = fixture
            .page(None, 50)
            .entries
            .into_iter()
            .find(|entry| entry.id == *seen.last().unwrap())
            .unwrap()
            .at;
        fixture
            .sql()
            .execute(
                "INSERT INTO plan_decisions (tenant_id,profile_id,created_at,actor,kind,summary) VALUES
                 ('default',1,'2026-11-01T00:00:00.000Z','assistant','choice','Later'),
                 ('default',1,?1,'assistant','choice','Same instant')",
                [&last_at],
            )
            .unwrap();
        let queue: String = fixture
            .sql()
            .query_row(
                "SELECT value FROM app_settings WHERE key='printer.send_queue'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        let mut queue: Vec<Value> = serde_json::from_str(&queue).unwrap();
        queue.push(json!({"id":format!("send-new-{arrivals}"),"filename":"new.gcode","printer_id":"mk4",
            "state":"queued","profile_id":1,"created_at":"2026-11-02T00:00:00.000Z","updated_at":"2026-11-02T00:00:00.000Z"}));
        set_queue(&fixture.sql(), &Value::Array(queue));

        let page = fixture.page(Some(&raw), 3);
        seen.extend(page.entries.iter().map(|e| e.id.clone()));
        cursor = page.next_cursor;
    }
    assert!(
        arrivals >= 2,
        "nine entries take at least three pages of three"
    );
    let later: Vec<String> = {
        let db = fixture.sql();
        let mut statement = db
            .prepare("SELECT 'decision:' || id FROM plan_decisions WHERE summary='Later'")
            .unwrap();
        statement
            .query_map([], |row| row.get(0))
            .unwrap()
            .collect::<rusqlite::Result<_>>()
            .unwrap()
    };
    assert_eq!(later.len(), arrivals);
    assert!(
        seen.iter()
            .all(|id| !later.contains(id) && !id.starts_with("send:send-new")),
        "entries newer than the cursor never reach later pages: {seen:?}"
    );
    // A same-instant arrival may sort after the cursor; existing entries are
    // still neither skipped nor repeated.
    let existing: Vec<&String> = seen.iter().filter(|id| all.contains(id)).collect();
    assert_eq!(existing, all.iter().collect::<Vec<_>>());
    let mut unique = seen.clone();
    unique.sort();
    unique.dedup();
    assert_eq!(unique.len(), seen.len(), "no entry repeated: {seen:?}");
    let fresh = fixture.page(None, 50);
    assert_eq!(fresh.entries.len(), all.len() + 3 * arrivals);
    assert_eq!(fresh.entries[0].id, format!("send:send-new-{arrivals}"));
}

#[test]
fn build_history_reads_write_nothing() {
    let fixture = Fixture::new();
    let before = fixture.stored();
    assert!(
        before
            .files
            .iter()
            .any(|(path, _)| path.starts_with("repos"))
    );
    let mut cursor = None;
    loop {
        let page = fixture.page(cursor.as_deref(), 2);
        cursor = page.next_cursor;
        if cursor.is_none() {
            break;
        }
    }
    assert!(matches!(fixture.read(2, None, 50), Outcome::Page(_)));
    assert!(matches!(fixture.read(999, None, 50), Outcome::MissingBuild));
    assert_eq!(fixture.stored(), before);
}

#[test]
fn build_history_hides_other_tenants_and_keeps_old_rows_readable() {
    let fixture = Fixture::new();
    fixture
        .sql()
        .execute_batch(
            "UPDATE build_profiles SET tenant_id='foreign' WHERE id=2;
             UPDATE plan_decisions SET created_at='legacy text' WHERE id=1;
             UPDATE app_settings SET value='not json' WHERE key='printer.send_queue';",
        )
        .unwrap();
    assert!(matches!(fixture.read(2, None, 50), Outcome::MissingBuild));
    let page = fixture.page(None, 50);
    let ids: Vec<&str> = page.entries.iter().map(|e| e.id.as_str()).collect();
    assert!(
        !ids.contains(&"send:send-a"),
        "malformed queue reads as empty"
    );
    assert_eq!(
        ids.last(),
        Some(&"decision:1"),
        "unparseable time sorts oldest"
    );
    assert_eq!(page.entries.last().unwrap().at, "legacy text");
}
