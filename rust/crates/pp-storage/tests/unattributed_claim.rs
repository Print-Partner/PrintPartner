#![cfg(feature = "desktop_rebuild")]
//! GRE-1182: claiming an unattributed print creates its printer Checkoff link,
//! marks the print claimed and records the plan binding and a domain event in
//! one transaction; dismiss drops it. A refused claim writes nothing. Runs on
//! a copy of the required-units fixture with synthetic prints.
use checkoff_apply::apply;
use checkoff_fixture::Fixture;
use pp_storage::{WriterOwner, checkoff_progress::Request};
use rusqlite::params;
use serde_json::{Value, json};

#[path = "common/checkoff_apply.rs"]
mod checkoff_apply;
#[path = "common/checkoff_fixture.rs"]
mod checkoff_fixture;
#[path = "common/js_json.rs"]
mod js_json;

const TOKEN: &str = "ppu_0000000000000000000000000000000";

fn unit(n: u8, part: &str) -> String {
    format!("{part}__{TOKEN}{n}")
}

fn print(id: &str, filename: &str, objects: &[String]) -> Value {
    json!({
        "id": id,
        "integration_id": "sim-moonraker",
        "printer_id": "sim-1",
        "host_name": "Simulated Voron",
        "filename": filename,
        "completed_at": "2026-11-05T20:00:00.000Z",
        "gcode_objects": objects,
        "candidates": [],
    })
}

impl Fixture {
    fn with_prints(prints: Value) -> Self {
        let fixture = Self::new();
        fixture
            .sql()
            .execute(
                "INSERT INTO app_settings (tenant_id, key, value) VALUES ('default', 'printer.unattributed_prints', ?1)",
                params![prints.to_string()],
            )
            .unwrap();
        fixture
    }

    fn setting(&self, key: &str) -> Value {
        self.sql()
            .query_row(
                "SELECT value FROM app_settings WHERE tenant_id='default' AND key=?1",
                params![key],
                |r| r.get::<_, String>(0),
            )
            .map(|raw| js_json::parse(&raw))
            .unwrap_or(Value::Null)
    }

    fn events(&self) -> Vec<(String, String, Value)> {
        let sql = self.sql();
        let mut stmt = sql
            .prepare(
                "SELECT event_type, aggregate_id, payload_json FROM domain_events ORDER BY seq",
            )
            .unwrap();
        stmt.query_map([], |r| {
            Ok((
                r.get(0)?,
                r.get(1)?,
                serde_json::from_str(&r.get::<_, String>(2)?).unwrap(),
            ))
        })
        .unwrap()
        .collect::<rusqlite::Result<_>>()
        .unwrap()
    }

    /// Every row a claim or dismiss can write.
    fn state(&self) -> Value {
        json!({
            "prints": self.setting("printer.unattributed_prints"),
            "links": self.setting("printer.checkoff_links"),
            "bindings": self.setting("printer.plan_bindings"),
            "events": self.events().len(),
        })
    }
}

fn claim(owner: &WriterOwner, id: &str, body: Value) -> Value {
    apply(
        owner,
        Request::ClaimUnattributed {
            id: id.into(),
            body,
        },
    )
}

fn units(link: &Value) -> Vec<(i64, i64)> {
    link["units"]
        .as_array()
        .unwrap()
        .iter()
        .map(|u| {
            (
                u["part_id"].as_i64().unwrap(),
                u["unit_index"].as_i64().unwrap(),
            )
        })
        .collect()
}

#[test]
fn claim_binds_required_unit_names_and_claims_every_open_copy_of_the_file() {
    let names = [unit(2, "bracket"), unit(5, "gear"), unit(1, "bracket")];
    let fixture = Fixture::with_prints(json!([
        print("p1", "plate-z.gcode", &names),
        print("p1-again", "jobs/PLATE-Z.gcode", &names),
        print("other", "other.gcode", &names),
    ]));
    let owner = fixture.open();

    let claimed = claim(&owner, "p1", json!({ "profile_id": 1 }));
    assert_eq!(claimed["status"], 200, "{claimed}");
    let link = &claimed["body"]["link"];
    assert_eq!(claimed["body"]["ok"], true);
    assert_eq!(link["state"], "awaiting_verify");
    assert_eq!(link["host_outcome"], "success");
    assert_eq!(link["profile_id"], 1);
    assert_eq!(link["filename"], "plate-z.gcode");
    assert_eq!(link["completed_at"], "2026-11-05T20:00:00.000Z");
    // Unit 0 of the bracket is already completed, so its name stays unlabeled.
    assert_eq!(units(link), [(1, 1), (3, 0)]);
    assert_eq!(link["unlabeled_names"], json!([unit(1, "bracket")]));

    let stored = fixture.setting("printer.checkoff_links");
    assert!(
        stored
            .as_array()
            .unwrap()
            .iter()
            .any(|l| l["id"] == link["id"])
    );
    let prints = fixture.setting("printer.unattributed_prints");
    let claimed_profile = |id: &str| {
        prints
            .as_array()
            .unwrap()
            .iter()
            .find(|p| p["id"] == id)
            .unwrap()["claimed_profile_id"]
            .clone()
    };
    assert_eq!(claimed_profile("p1"), 1);
    assert_eq!(claimed_profile("p1-again"), 1);
    assert_eq!(claimed_profile("other"), Value::Null);
    let bindings = fixture.setting("printer.plan_bindings");
    assert_eq!(bindings[0]["integration_id"], "sim-moonraker");
    assert_eq!(bindings[0]["profile_id"], 1);
    assert_eq!(
        fixture.events(),
        [(
            "checkoff.unattributed_resolved".to_owned(),
            "p1".to_owned(),
            json!({ "profile_id": 1, "link_id": link["id"] }),
        )]
    );

    let again = claim(&owner, "p1-again", json!({ "profile_id": 1 }));
    assert_eq!(
        again,
        json!({ "status": 409, "body": { "detail": "Print already claimed" } })
    );
}

#[test]
fn claim_matches_plain_file_names_and_honors_the_selected_plate_files() {
    let plain = [
        "gear.stl".to_owned(),
        "bracket.stl".to_owned(),
        "gear.stl".to_owned(),
    ];
    let fixture = Fixture::with_prints(json!([
        print("all", "mixed.gcode", &plain),
        print("gears", "gears.gcode", &plain),
    ]));
    let owner = fixture.open();

    let all = claim(&owner, "all", json!({ "profile_id": "1" }));
    assert_eq!(all["status"], 200, "{all}");
    assert_eq!(units(&all["body"]["link"]), [(3, 0), (1, 1), (3, 1)]);

    let fixture = Fixture::with_prints(json!([print("gears", "gears.gcode", &plain)]));
    let owner = fixture.open();
    let gears = claim(
        &owner,
        "gears",
        json!({ "profile_id": 1, "selected_stl_basenames": ["  GEAR.STL "] }),
    );
    assert_eq!(gears["status"], 200, "{gears}");
    assert_eq!(units(&gears["body"]["link"]), [(3, 0), (3, 1)]);
}

#[test]
fn a_refused_claim_answers_like_node_and_writes_nothing() {
    let mut dismissed = print("dismissed", "d.gcode", &[unit(2, "bracket")]);
    dismissed["dismissed"] = json!(true);
    let mut claimed = print("claimed", "c.gcode", &[unit(2, "bracket")]);
    claimed["claimed_at"] = json!("2999-01-01T00:00:00.000Z");
    let fixture = Fixture::with_prints(json!([
        print("open", "open.gcode", &[unit(2, "bracket")]),
        print("assigned", "plate-a.gcode", &[unit(2, "bracket")]),
        print("mystery", "mystery.gcode", &["mystery.stl".to_owned()]),
        dismissed,
        claimed,
    ]));
    fixture
        .sql()
        .execute(
            "INSERT INTO app_settings (tenant_id, key, value) VALUES ('default', 'printer.plan_bindings', '[]')",
            [],
        )
        .unwrap();
    let owner = fixture.open();
    let before = fixture.state();
    let refused = |id: &str, body: Value| {
        let response = claim(&owner, id, body);
        assert_eq!(fixture.state(), before, "{id} wrote");
        (
            response["status"].as_u64().unwrap(),
            response["body"]["detail"].clone(),
        )
    };
    let detail = |s: &str| Value::from(s);

    assert_eq!(
        refused("open", json!({})),
        (400, detail("profile_id is required"))
    );
    assert_eq!(
        refused("open", json!({ "profile_id": 1.5 })),
        (400, detail("profile_id is required"))
    );
    assert_eq!(
        refused("nope", json!({ "profile_id": 1 })),
        (404, detail("Unattributed print not found"))
    );
    assert_eq!(
        refused("claimed", json!({ "profile_id": 1 })),
        (409, detail("Print already claimed"))
    );
    assert_eq!(
        refused(
            "open",
            json!({ "profile_id": 1, "selected_stl_basenames": [" ", 3] })
        ),
        (400, detail("Select at least one plate file"))
    );
    assert_eq!(
        refused(
            "open",
            json!({ "profile_id": 1, "selected_stl_basenames": ["gear.stl"] })
        ),
        (400, detail("Selected files are not on this plate"))
    );
    assert_eq!(
        refused("dismissed", json!({ "profile_id": 1 })),
        (409, detail("Print changed or was already claimed"))
    );
    // Node's operational read throws "Profile not found" for a Build that does
    // not exist, and the claim route answers that with a plain 500.
    assert_eq!(
        refused("open", json!({ "profile_id": 99 })),
        (500, detail("Internal Server Error"))
    );
    assert_eq!(
        refused("mystery", json!({ "profile_id": 1 })),
        (
            409,
            detail("That print file does not map to an incomplete Required unit in this Build")
        )
    );
    assert_eq!(
        refused("assigned", json!({ "profile_id": 1 })),
        (409, detail("That print file is already assigned"))
    );

    fixture
        .sql()
        .execute(
            "UPDATE app_settings SET value='{}' WHERE tenant_id='default' AND key='printer.plan_bindings'",
            [],
        )
        .unwrap();
    let before = fixture.state();
    let failed = claim(&owner, "open", json!({ "profile_id": 1 }));
    assert_eq!(
        failed,
        json!({ "status": 500, "body": { "detail": "Internal Server Error" } })
    );
    assert_eq!(fixture.state(), before, "a failed claim rolled back");
}

#[test]
fn dismiss_marks_any_stored_print_and_refuses_an_unknown_one() {
    let fixture = Fixture::with_prints(json!([print("p1", "a.gcode", &[])]));
    let owner = fixture.open();
    let missing = apply(&owner, Request::DismissUnattributed { id: "nope".into() });
    assert_eq!(
        missing,
        json!({ "status": 404, "body": { "detail": "Unattributed print not found" } })
    );
    assert!(fixture.events().is_empty());

    let dismissed = apply(&owner, Request::DismissUnattributed { id: "p1".into() });
    assert_eq!(dismissed, json!({ "status": 200, "body": { "ok": true } }));
    let stored = &fixture.setting("printer.unattributed_prints")[0];
    assert_eq!(stored["dismissed"], true);
    assert!(stored["dismissed_at"].is_string());
    assert_eq!(
        fixture.events(),
        [(
            "checkoff.unattributed_resolved".to_owned(),
            "p1".to_owned(),
            json!({ "dismissed": true }),
        )]
    );
}
