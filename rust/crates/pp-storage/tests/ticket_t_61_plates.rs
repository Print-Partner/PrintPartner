#![cfg(feature = "desktop_rebuild")]
//! T-61-plates (GRE-1189, GRE-1184): the Plate read and the Plate-revision
//! edits on a disposable copy of the Node-written `accepted-plate-node.db`, through the
//! public storage client, across a writer restart.
use pp_storage::{
    Limits, WriterOwner,
    auth::{AuthPolicy, FirstUserTenant, RegistrationPolicy, Secret, SessionTenantPolicy},
    plates::{PlatesClient, Request, Response, UnitEdit},
    read_model::Credential,
};
use rusqlite::Connection;
use serde_json::{Value, json};
use std::{sync::atomic::AtomicBool, time::Duration};

const TOKEN: &str = "ppu_574c8d5f92624f029e21705a8ebb8a1b";
const SECRET: &str = "read-fixture-secret";

struct Fixture {
    dir: tempfile::TempDir,
    owner: Option<WriterOwner>,
}

impl Fixture {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let accepted = dir.path().join("repos/1/revisions/accepted");
        pp_os::create_private_dir_all(&accepted).unwrap();
        let database = include_bytes!("fixtures/accepted-plate-node.db");
        std::fs::write(dir.path().join("print-partner.db"), database).unwrap();
        let mut fixture = Self { dir, owner: None };
        fixture.restart();
        fixture
    }

    fn restart(&mut self) {
        if let Some(owner) = self.owner.take() {
            owner.shutdown().unwrap();
        }
        self.owner = Some(
            WriterOwner::open(self.dir.path(), Limits::default())
                .unwrap()
                .0,
        );
    }

    fn client(&self) -> PlatesClient {
        let policy = AuthPolicy {
            registration: RegistrationPolicy::Open,
            session_tenant: SessionTenantPolicy::AccountTenant,
            first_user: FirstUserTenant::NewUser,
        };
        self.owner
            .as_ref()
            .unwrap()
            .accepted_plates(policy)
            .unwrap()
    }

    fn send(&self, secret: &str, request: Request) -> anyhow::Result<Response> {
        self.client().apply(
            Credential::Session(Secret::new(secret.into())),
            request,
            &AtomicBool::new(false),
            Duration::from_secs(5),
        )
    }

    fn apply(&self, request: Request) -> Response {
        self.send(SECRET, request).unwrap()
    }

    fn sql(&self) -> Connection {
        let connection = Connection::open(self.dir.path().join("print-partner.db")).unwrap();
        connection.busy_timeout(Duration::from_secs(3)).unwrap();
        connection
    }

    /// Every Plate row, the head and the Plate events, as one comparable text.
    fn history(&self) -> String {
        let sql = self.sql();
        [
            "SELECT * FROM accepted_plate_revisions ORDER BY id",
            "SELECT * FROM accepted_plates ORDER BY revision_id, ordinal",
            "SELECT * FROM accepted_plate_units ORDER BY revision_id, required_unit_token",
            "SELECT * FROM accepted_plate_heads",
            "SELECT event_type, aggregate_id, payload_json FROM domain_events ORDER BY seq",
        ]
        .iter()
        .map(|query| {
            let mut statement = sql.prepare(query).unwrap();
            let columns = statement.column_count();
            let mut rows = statement.query([]).unwrap();
            let mut text = String::new();
            while let Some(row) = rows.next().unwrap() {
                for index in 0..columns {
                    text.push_str(&format!("{:?}|", row.get_ref(index).unwrap()));
                }
                text.push('\n');
            }
            text
        })
        .collect()
    }

    fn events(&self) -> Vec<Value> {
        let sql = self.sql();
        let mut statement = sql
            .prepare("SELECT payload_json FROM domain_events WHERE event_type='plates.revision_changed' ORDER BY seq")
            .unwrap();
        statement
            .query_map([], |row| row.get::<_, String>(0))
            .unwrap()
            .map(|payload| serde_json::from_str(&payload.unwrap()).unwrap())
            .collect()
    }

    /// The basis that stamps the Node-written revision 1.
    fn basis(&self) -> Value {
        self.sql()
            .query_row(
                "SELECT profile_id, plan_version, plan_revision_id, plan_revision_digest, required_unit_mapping_digest FROM accepted_plate_revisions WHERE id=1",
                [],
                |row| {
                    Ok(json!({
                        "profile_id": row.get::<_, i64>(0)?,
                        "plan_version": row.get::<_, i64>(1)?,
                        "plan_revision_id": row.get::<_, i64>(2)?,
                        "plan_revision_digest": row.get::<_, String>(3)?,
                        "required_unit_mapping_digest": row.get::<_, String>(4)?,
                    }))
                },
            )
            .unwrap()
    }

    fn body(&self, revision: i64, extra: Value) -> Value {
        let mut body = json!({"expected": self.basis(), "expected_plate_revision_id": revision});
        body.as_object_mut()
            .unwrap()
            .extend(extra.as_object().unwrap().clone());
        body
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        if let Some(owner) = self.owner.take() {
            let _ = owner.shutdown();
        }
    }
}

/// Copies revision `from` as the new head, the way an arrange would write it:
/// optionally under a generated Plate id (with the layout digest that id
/// needs) and recording the revision it replaced. Stored revisions are
/// immutable, so a test writes a new one rather than editing one.
fn copy_revision(f: &Fixture, from: i64, plate: Option<&str>, undo_from: Option<i64>) -> i64 {
    use pp_storage::production::{
        layout::{
            Placement, PlateInput, PrinterGeometry, UnitInput, ValidatedPlate, layout_digest,
        },
        model::{DimensionUm, OffsetUm, PlateId, RequiredUnitToken},
    };
    let digest = plate.map(|id| {
        let size = |um: i64| DimensionUm::parse(um).unwrap();
        let offset = |um: i64| OffsetUm::parse(um).unwrap();
        let plate = ValidatedPlate {
            ordinal: 1,
            input: PlateInput {
                plate_id: PlateId::parse(id).unwrap(),
                printer_id: "fixture-printer".into(),
                printer_name: "Fixture Printer".into(),
                printer_model: "Fixture Model".into(),
                printer: PrinterGeometry {
                    bed_width_um: size(250_000),
                    bed_depth_um: size(220_000),
                    bed_height_um: size(220_000),
                    margin_um: offset(5000),
                },
                units: vec![UnitInput {
                    token: RequiredUnitToken::parse(TOKEN).unwrap(),
                    x_um: offset(5000),
                    y_um: offset(5000),
                    width_um: size(50_000),
                    depth_um: size(40_000),
                    height_um: size(30_000),
                    placement: Placement::Auto,
                    pinned: false,
                }],
            },
        };
        layout_digest(&[plate], 2).as_str().to_owned()
    });
    let sql = f.sql();
    sql.execute(
        "INSERT INTO accepted_plate_revisions(tenant_id,profile_id,plan_revision_id,plan_version,plan_revision_digest,required_unit_mapping_digest,layout_digest,expected_plate_count,expected_unit_count,revision_number,undo_from_revision_id,created_at) SELECT tenant_id,profile_id,plan_revision_id,plan_version,plan_revision_digest,required_unit_mapping_digest,COALESCE(?2,layout_digest),expected_plate_count,expected_unit_count,(SELECT MAX(revision_number)+1 FROM accepted_plate_revisions),?3,created_at FROM accepted_plate_revisions WHERE id=?1",
        rusqlite::params![from, digest, undo_from],
    )
    .unwrap();
    let id = sql.last_insert_rowid();
    sql.execute(
        "INSERT INTO accepted_plates(tenant_id,revision_id,plate_id,ordinal,printer_id,printer_name,printer_model,bed_width_um,bed_depth_um,bed_height_um,margin_um) SELECT tenant_id,?2,COALESCE(?3,plate_id),ordinal,printer_id,printer_name,printer_model,bed_width_um,bed_depth_um,bed_height_um,margin_um FROM accepted_plates WHERE revision_id=?1",
        rusqlite::params![from, id, plate],
    )
    .unwrap();
    sql.execute(
        "INSERT INTO accepted_plate_units(tenant_id,revision_id,plate_id,required_unit_token,x_um,y_um,width_um,depth_um,height_um,placement,pinned) SELECT tenant_id,?2,COALESCE(?3,plate_id),required_unit_token,x_um,y_um,width_um,depth_um,height_um,placement,pinned FROM accepted_plate_units WHERE revision_id=?1",
        rusqlite::params![from, id, plate],
    )
    .unwrap();
    sql.execute(
        "UPDATE accepted_plate_heads SET current_revision_id=?1",
        [id],
    )
    .unwrap();
    id
}

fn read(profile: &str) -> Request {
    Request::Read {
        profile: profile.into(),
    }
}

fn edit(f: &Fixture, edit: UnitEdit, plate: &str, revision: i64, extra: Value) -> Response {
    f.apply(Request::Unit {
        edit,
        profile: "1".into(),
        plate: plate.into(),
        token: TOKEN.into(),
        body: f.body(revision, extra),
    })
}

fn at(revision: i64) -> (u16, Value) {
    (
        200,
        json!({"plate_revision_id": revision, "plate_revision_number": revision}),
    )
}

fn revision(response: &Response) -> (u16, Value) {
    (response.status, response.body.clone())
}

fn refused(response: &Response, status: u16, code: &str) {
    assert_eq!(
        (response.status, response.body["code"].as_str()),
        (status, Some(code)),
        "{}",
        response.body
    );
}

const LEGACY: &str = "fixture-plate";
const FIRST: &str = "plate_abababababababababababababababab";

/// `printer.fleet` rows Node's `acceptedPrinter` accepts (`p2`, `h1`) and
/// refuses.
fn with_fleet(f: &Fixture) {
    let fleet = json!([
        {"id": "flat", "name": "Flat", "bed_width_mm": 200, "bed_depth_mm": 200},
        {"id": "p2", "name": " Second ", "model": "MK", "bed_width_mm": 200, "bed_depth_mm": "180", "bed_height_mm": 150, "margin_mm": 5},
        {"id": "a1", "name": "Tight", "bed_width_mm": 10, "bed_depth_mm": 10, "bed_height_mm": 10, "margin_mm": 5},
        {"id": "h1", "name": "Hex", "bed_width_mm": "0xC8", "bed_depth_mm": [180], "bed_height_mm": "0b1010", "margin_mm": []},
        {"id": "n1", "name": "Pair", "bed_width_mm": [1, 2], "bed_height_mm": 10},
    ]);
    f.sql()
        .execute(
            "INSERT INTO app_settings(tenant_id, key, value) VALUES('default', 'printer.fleet', ?1)",
            [fleet.to_string()],
        )
        .unwrap();
}

#[test]
fn ticket_t_61_plates_read_lists_the_head_layout_and_complete_printers() {
    let mut f = Fixture::new();
    assert_eq!(copy_revision(&f, 1, Some(FIRST), None), 2);
    with_fleet(&f);
    let before = f.history();
    let ready = f.apply(read("1"));
    let body = &ready.body;
    assert_eq!(
        (ready.status, &body["kind"]),
        (200, &json!("ready")),
        "{body}"
    );
    assert_eq!(
        (
            &body["plate_revision_id"],
            &body["plate_revision_number"],
            &body["arrange_undo_revision_id"]
        ),
        (&json!(2), &json!(2), &Value::Null)
    );
    assert_eq!(body["basis"]["profile_id"], 1);
    let plate = &body["plates"][0];
    assert_eq!(
        (&plate["plate_id"], &plate["ordinal"]),
        (&json!(FIRST), &json!(1))
    );
    assert_eq!(plate["printer"]["bed_width_um"], 250_000);
    let unit = &plate["units"][0];
    assert_eq!(
        (
            &unit["token"],
            &unit["x_um"],
            &unit["placement"],
            &unit["pinned"]
        ),
        (&json!(TOKEN), &json!(5000), &json!("auto"), &json!(false))
    );
    assert_eq!(body["unplaced"], json!([]));
    assert_eq!(
        body["printers"],
        json!([
            {"id": "h1", "name": "Hex", "model": "Hex", "bed_width_um": 200_000, "bed_depth_um": 180_000, "bed_height_um": 10_000, "margin_um": 0},
            {"id": "p2", "name": "Second", "model": "MK", "bed_width_um": 200_000, "bed_depth_um": 180_000, "bed_height_um": 150_000, "margin_um": 5000},
        ]),
        "JavaScript Number() reads radix strings and arrays; printers without a build height, with margins filling the bed or with NaN sizes are not offered"
    );
    let unassigned = body["unassigned"].as_array().unwrap();
    assert!(unassigned.iter().all(|unit| unit["token"] != TOKEN));
    assert_eq!(f.history(), before, "a read writes nothing");

    f.restart();
    assert_eq!(
        f.apply(read("1")),
        ready,
        "the same workspace after a restart"
    );

    f.sql()
        .execute(
            "UPDATE app_settings SET value=?1 WHERE key='printer.fleet'",
            [json!([
                {"id": "p2", "name": "Second", "bed_height_mm": 150},
                {"id": "q", "bed_height_mm": 200, "loaded_filaments": [null]},
            ])
            .to_string()],
        )
        .unwrap();
    assert_eq!(
        f.apply(read("1")).body["printers"],
        json!([]),
        "a null loaded filament makes Node's fleet parse throw, which empties the fleet"
    );
}

#[test]
fn ticket_t_61_plates_read_refuses_without_writes() {
    let f = Fixture::new();
    let before = f.history();
    // Node's contract parser refuses the fixture's legacy Plate id on read.
    refused(&f.apply(read("1")), 500, "internal_error");
    refused(&f.apply(read("x")), 400, "invalid_request");
    refused(&f.apply(read("01x")), 400, "invalid_request");
    refused(&f.apply(read("2")), 404, "profile_not_found");
    assert!(f.send("wrong-secret", read("1")).is_err());
    assert_eq!(f.history(), before);
}

#[test]
fn ticket_t_61_plates_read_without_a_matching_head_is_setup() {
    let f = Fixture::new();
    f.sql()
        .execute("DELETE FROM accepted_plate_heads", [])
        .unwrap();
    let setup = f.apply(read("1"));
    let body = &setup.body;
    assert_eq!(
        (setup.status, &body["kind"]),
        (200, &json!("setup")),
        "{body}"
    );
    assert_eq!(body["expected_plate_revision_id"], Value::Null);
    assert_eq!(body["printers"], json!([]));
    let units = body["units"].as_array().unwrap();
    assert!(units.iter().any(|unit| unit["token"] == TOKEN));
    assert!(units.iter().all(|unit| unit["completed"].is_boolean()));
}

#[test]
fn ticket_t_61_plates_refused_edits_write_nothing() {
    let f = Fixture::new();
    let before = f.history();
    let at_6000 = json!({"x_um": 6000, "y_um": 6000});
    refused(
        &edit(&f, UnitEdit::Move, LEGACY, 2, at_6000.clone()),
        409,
        "plate_revision_changed",
    );
    refused(
        &edit(
            &f,
            UnitEdit::Move,
            LEGACY,
            1,
            json!({"x_um": -1, "y_um": 0}),
        ),
        400,
        "invalid_request",
    );
    refused(
        &edit(
            &f,
            UnitEdit::Move,
            LEGACY,
            1,
            json!({"x_um": 250_000, "y_um": 6000}),
        ),
        422,
        "outside_build_area",
    );
    refused(
        &edit(&f, UnitEdit::Move, "other-plate", 1, at_6000.clone()),
        422,
        "unit_not_found",
    );
    refused(
        &edit(&f, UnitEdit::Pin, LEGACY, 1, json!({"pinned": true})),
        400,
        "invalid_request",
    );
    for (field, value, status, code) in [
        ("plan_version", 2, 409, "accepted_plan_changed"),
        ("profile_id", 2, 400, "invalid_request"),
    ] {
        let mut body = f.body(1, at_6000.clone());
        body["expected"][field] = json!(value);
        let request = Request::Unit {
            edit: UnitEdit::Move,
            profile: "1".into(),
            plate: LEGACY.into(),
            token: TOKEN.into(),
            body,
        };
        refused(&f.apply(request), status, code);
    }
    let request = Request::Unit {
        edit: UnitEdit::Move,
        profile: "1".into(),
        plate: LEGACY.into(),
        token: TOKEN.into(),
        body: f.body(1, at_6000.clone()),
    };
    assert!(f.send("wrong-secret", request).is_err());
    f.sql()
        .execute(
            "UPDATE build_profiles SET archived_at='2026-11-01T00:00:00.000Z'",
            [],
        )
        .unwrap();
    refused(
        &edit(&f, UnitEdit::Move, LEGACY, 1, at_6000),
        409,
        "plan_archived",
    );
    assert_eq!(f.history(), before);
}

#[test]
fn ticket_t_61_plates_move_writes_one_immutable_revision_and_one_event() {
    let f = Fixture::new();
    let before = f.history();
    // Node's contract parser refuses the fixture's legacy Plate id on read.
    refused(&f.apply(read("1")), 500, "internal_error");
    let at_6000 = json!({"x_um": 6000, "y_um": 6000});
    let first = edit(&f, UnitEdit::Move, " fixture-plate ", 1, at_6000.clone());
    assert_eq!(revision(&first), at(2));
    let row: (String, i64, i64) = f
        .sql()
        .query_row(
            "SELECT placement, x_um, (SELECT current_revision_id FROM accepted_plate_heads) FROM accepted_plate_units WHERE revision_id=2",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .unwrap();
    assert_eq!(row, ("manual".into(), 6000, 2));
    let revision_one = &before[..before.find('\n').unwrap()];
    assert!(
        f.history().starts_with(revision_one),
        "revision 1 is unchanged"
    );
    assert_eq!(f.events(), vec![json!({"plate_revision_id": 2})]);

    let settled = f.history();
    assert_eq!(
        revision(&edit(&f, UnitEdit::Move, LEGACY, 2, at_6000.clone())),
        at(2)
    );
    refused(
        &edit(&f, UnitEdit::Move, LEGACY, 1, at_6000),
        409,
        "plate_revision_changed",
    );
    assert_eq!(
        f.history(),
        settled,
        "an unchanged or stale move writes nothing"
    );
}

#[test]
fn ticket_t_61_plates_read_and_transfer_to_a_new_printer_plate() {
    let f = Fixture::new();
    assert_eq!(copy_revision(&f, 1, Some(FIRST), None), 2);
    with_fleet(&f);
    let ready = f.apply(read("1"));
    assert_eq!(
        (ready.status, &ready.body["kind"]),
        (200, &json!("ready")),
        "{}",
        ready.body
    );
    let unit = &ready.body["plates"][0]["units"][0];
    assert_eq!(
        (&unit["token"], &unit["placement"]),
        (&json!(TOKEN), &json!("auto"))
    );
    let unassigned = ready.body["unassigned"].as_array().unwrap();
    assert!(unassigned.iter().all(|unit| unit["token"] != TOKEN));

    let before = f.history();
    refused(
        &edit(
            &f,
            UnitEdit::Transfer,
            FIRST,
            2,
            json!({"target_printer_id": "flat"}),
        ),
        422,
        "printer_not_found",
    );
    refused(
        &edit(
            &f,
            UnitEdit::Transfer,
            FIRST,
            2,
            json!({"target_printer_id": "p2", "target_plate_id": FIRST}),
        ),
        400,
        "invalid_request",
    );
    assert_eq!(
        revision(&edit(
            &f,
            UnitEdit::Transfer,
            FIRST,
            2,
            json!({"target_plate_id": FIRST})
        )),
        at(2)
    );
    assert_eq!(f.history(), before);

    assert_eq!(
        revision(&edit(
            &f,
            UnitEdit::Transfer,
            FIRST,
            2,
            json!({"target_printer_id": "p2"})
        )),
        at(3)
    );
    let moved = f.apply(read("1"));
    let plates = moved.body["plates"].as_array().unwrap();
    assert_eq!(plates.len(), 1, "the emptied Plate is gone");
    assert_ne!(plates[0]["plate_id"], FIRST);
    assert_eq!(
        (&plates[0]["printer"]["id"], &plates[0]["units"]),
        (&json!("p2"), &json!([]))
    );
    let unplaced = &moved.body["unplaced"][0];
    assert_eq!(
        (&unplaced["plate_id"], &unplaced["printer_id"]),
        (&plates[0]["plate_id"], &json!("p2"))
    );
}

#[test]
fn ticket_t_61_plates_pin_unplace_and_restore_survive_restart() {
    let mut f = Fixture::new();
    copy_revision(&f, 1, Some(FIRST), None);
    with_fleet(&f);
    edit(
        &f,
        UnitEdit::Transfer,
        FIRST,
        2,
        json!({"target_printer_id": "p2"}),
    );
    let second = f.apply(read("1")).body["plates"][0]["plate_id"]
        .as_str()
        .unwrap()
        .to_owned();
    // As in Node, a move compares coordinates only: an unplaced unit "moved"
    // to the coordinates it kept stays unplaced and writes nothing.
    assert_eq!(
        revision(&edit(
            &f,
            UnitEdit::Move,
            &second,
            3,
            json!({"x_um": 5000, "y_um": 5000})
        )),
        at(3)
    );
    assert_eq!(
        revision(&edit(
            &f,
            UnitEdit::Move,
            &second,
            3,
            json!({"x_um": 6000, "y_um": 5000})
        )),
        at(4)
    );
    assert_eq!(
        revision(&edit(
            &f,
            UnitEdit::Pin,
            &second,
            4,
            json!({"pinned": true})
        )),
        at(5)
    );
    assert_eq!(
        revision(&edit(
            &f,
            UnitEdit::Pin,
            &second,
            5,
            json!({"pinned": true})
        )),
        at(5)
    );
    assert_eq!(
        revision(&edit(&f, UnitEdit::Unplace, &second, 5, json!({}))),
        at(6)
    );

    // An arrange records the revision it replaced; restore brings it back.
    assert_eq!(copy_revision(&f, 6, None, Some(5)), 7);
    let restore = |f: &Fixture, id: i64| {
        f.apply(Request::Restore {
            profile: "1".into(),
            body: f.body(7, json!({"restore_plate_revision_id": id})),
        })
    };
    let settled = f.history();
    refused(&restore(&f, 4), 422, "unit_not_found");
    assert_eq!(f.history(), settled);
    let restored = restore(&f, 5);
    let body = &restored.body;
    assert_eq!(restored.status, 200, "{body}");
    assert_eq!(
        (
            &body["plate_revision_id"],
            &body["plate_revision_number"],
            &body["arrange_undo_revision_id"]
        ),
        (&json!(8), &json!(8), &Value::Null)
    );
    let placed = &body["plates"][0]["units"][0];
    assert_eq!(
        (&placed["x_um"], &placed["placement"], &placed["pinned"]),
        (&json!(6000), &json!("manual"), &json!(true))
    );
    let events = [3, 4, 5, 6, 8].map(|id| json!({"plate_revision_id": id}));
    assert_eq!(f.events(), events.to_vec());

    f.restart();
    assert_eq!(
        f.apply(read("1")),
        restored,
        "the restored workspace survives a restart"
    );
    let history = f.history();
    refused(
        &edit(&f, UnitEdit::Pin, &second, 7, json!({"pinned": false})),
        409,
        "plate_revision_changed",
    );
    assert_eq!(f.history(), history);
}
