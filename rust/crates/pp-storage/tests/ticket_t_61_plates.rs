#![cfg(feature = "desktop_rebuild")]
//! T-61-plates (GRE-1189): the Plate workspace read on a
//! disposable copy of the Node-written `accepted-plate-node.db`, through the
//! public storage client, across a writer restart.
use pp_storage::{
    Limits, WriterOwner,
    auth::{AuthPolicy, FirstUserTenant, RegistrationPolicy, Secret, SessionTenantPolicy},
    plates::{PlatesClient, Request, Response},
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
}

impl Drop for Fixture {
    fn drop(&mut self) {
        if let Some(owner) = self.owner.take() {
            let _ = owner.shutdown();
        }
    }
}

/// Copies revision `from` as the new head under the generated Plate id
/// `plate`, with the layout digest that id needs. Stored revisions are
/// immutable, so a test writes a new one rather than editing one.
fn copy_revision(f: &Fixture, from: i64, plate: &str) -> i64 {
    use pp_storage::production::{
        layout::{
            Placement, PlateInput, PrinterGeometry, UnitInput, ValidatedPlate, layout_digest,
        },
        model::{DimensionUm, OffsetUm, PlateId, RequiredUnitToken},
    };
    let digest = {
        let size = |um: i64| DimensionUm::parse(um).unwrap();
        let offset = |um: i64| OffsetUm::parse(um).unwrap();
        let plate = ValidatedPlate {
            ordinal: 1,
            input: PlateInput {
                plate_id: PlateId::parse(plate).unwrap(),
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
    };
    let sql = f.sql();
    sql.execute(
        "INSERT INTO accepted_plate_revisions(tenant_id,profile_id,plan_revision_id,plan_version,plan_revision_digest,required_unit_mapping_digest,layout_digest,expected_plate_count,expected_unit_count,revision_number,undo_from_revision_id,created_at) SELECT tenant_id,profile_id,plan_revision_id,plan_version,plan_revision_digest,required_unit_mapping_digest,?2,expected_plate_count,expected_unit_count,(SELECT MAX(revision_number)+1 FROM accepted_plate_revisions),NULL,created_at FROM accepted_plate_revisions WHERE id=?1",
        rusqlite::params![from, digest],
    )
    .unwrap();
    let id = sql.last_insert_rowid();
    sql.execute(
        "INSERT INTO accepted_plates(tenant_id,revision_id,plate_id,ordinal,printer_id,printer_name,printer_model,bed_width_um,bed_depth_um,bed_height_um,margin_um) SELECT tenant_id,?2,?3,ordinal,printer_id,printer_name,printer_model,bed_width_um,bed_depth_um,bed_height_um,margin_um FROM accepted_plates WHERE revision_id=?1",
        rusqlite::params![from, id, plate],
    )
    .unwrap();
    sql.execute(
        "INSERT INTO accepted_plate_units(tenant_id,revision_id,plate_id,required_unit_token,x_um,y_um,width_um,depth_um,height_um,placement,pinned) SELECT tenant_id,?2,?3,required_unit_token,x_um,y_um,width_um,depth_um,height_um,placement,pinned FROM accepted_plate_units WHERE revision_id=?1",
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

fn refused(response: &Response, status: u16, code: &str) {
    assert_eq!(
        (response.status, response.body["code"].as_str()),
        (status, Some(code)),
        "{}",
        response.body
    );
}

const FIRST: &str = "plate_abababababababababababababababab";

#[test]
fn ticket_t_61_plates_read_lists_the_head_layout_and_complete_printers() {
    let mut f = Fixture::new();
    assert_eq!(copy_revision(&f, 1, FIRST), 2);
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
