//! GRE-1550: an Accept that keeps every Required unit, each unit's Part
//! `included` and its STL keeps the Plate layout as a new Plate revision for
//! the new Accepted Plan. Any other Accept clears the layout.
use super::*;
use pp_storage::plates::{Request, UnitEdit};
use pp_storage::production::{
    layout::{
        Placement, PlateInput, PrinterGeometry, Spacing, UnitInput, layout_digest, validate_plates,
    },
    model::{DimensionUm, OffsetUm, PlateId, RequiredUnitToken},
};

const PLATE: &str = "plate_0000000000000000000000000000000a";

/// The units of the fixture's included Parts on one Plate, 20 mm apart: the
/// bracket's tokens 1 to 3 and the gear's 5 and 6. Unit 1 is moved and pinned.
fn units() -> Vec<(String, i64, Placement, bool)> {
    [1u8, 2, 3, 5, 6]
        .into_iter()
        .enumerate()
        .map(|(index, n)| {
            let moved = n == 1;
            let placement = if moved {
                Placement::Manual
            } else {
                Placement::Auto
            };
            (
                format!("ppu_{n:032}"),
                5_000 + 20_000 * index as i64,
                placement,
                moved,
            )
        })
        .collect()
}

/// The layout digest of [`units`] as a Plate write computes it.
fn digest() -> String {
    let size = |um| DimensionUm::parse(um).unwrap();
    let units: Vec<_> = units()
        .into_iter()
        .map(|(token, x, placement, pinned)| UnitInput {
            token: RequiredUnitToken::parse(token).unwrap(),
            x_um: OffsetUm::parse(x).unwrap(),
            y_um: OffsetUm::parse(5_000).unwrap(),
            width_um: size(10_000),
            depth_um: size(10_000),
            height_um: size(10_000),
            placement,
            pinned,
        })
        .collect();
    let expected = units.iter().map(|unit| unit.token.clone()).collect();
    let plate = PlateInput {
        plate_id: PlateId::parse(PLATE).unwrap(),
        printer_id: "sim-1".into(),
        printer_name: "Simulator".into(),
        printer_model: "sim".into(),
        printer: PrinterGeometry {
            bed_width_um: size(256_000),
            bed_depth_um: size(256_000),
            bed_height_um: size(256_000),
            margin_um: OffsetUm::parse(5_000).unwrap(),
        },
        units,
    };
    let validated = validate_plates(vec![plate], &expected, false, Spacing::Clearance).unwrap();
    layout_digest(&validated, 2).as_str().to_owned()
}

/// Plate revision `id` of Accepted revision 1 holding [`units`] under
/// `digest`, recording the revision an arrange replaced, and made the head.
fn plate_revision(id: i64, undo_from: Option<i64>, digest: &str) -> String {
    let undo = undo_from.map_or("NULL".into(), |id| id.to_string());
    let mut sql = format!(
        "DELETE FROM accepted_plate_heads;
INSERT INTO accepted_plate_revisions(id,tenant_id,profile_id,plan_revision_id,plan_version,plan_revision_digest,required_unit_mapping_digest,layout_digest,expected_plate_count,expected_unit_count,revision_number,undo_from_revision_id,created_at)
  SELECT {id},'default',1,1,1,r.snapshot_digest,s.mapping_digest,'{digest}',1,5,{id},{undo},'2026-08-21T00:00:00.000Z'
  FROM plan_revisions r JOIN plan_revision_required_unit_sets s ON s.revision_id=r.id WHERE r.id=1;
INSERT INTO accepted_plates(tenant_id,revision_id,plate_id,ordinal,printer_id,printer_name,printer_model,bed_width_um,bed_depth_um,bed_height_um,margin_um)
  VALUES('default',{id},'{PLATE}',1,'sim-1','Simulator','sim',256000,256000,256000,5000);
"
    );
    for (token, x, placement, pinned) in units() {
        let placement = if placement == Placement::Manual {
            "manual"
        } else {
            "auto"
        };
        sql.push_str(&format!(
            "INSERT INTO accepted_plate_units(tenant_id,revision_id,plate_id,required_unit_token,x_um,y_um,width_um,depth_um,height_um,placement,pinned)
  VALUES('default',{id},'{PLATE}','{token}',{x},5000,10000,10000,10000,'{placement}',{});\n",
            i64::from(pinned)
        ));
    }
    sql.push_str(&format!(
        "INSERT INTO accepted_plate_heads(tenant_id,profile_id,current_revision_id) VALUES('default',1,{id});\n"
    ));
    sql
}

fn plates(server: &ReadServer, request: Request) -> pp_storage::plates::Response {
    server
        .owner
        .accepted_plates(policy())
        .unwrap()
        .apply(
            pp_storage::read_model::Credential::Session(pp_storage::auth::Secret::new(
                "required-unit-fixture-secret".into(),
            )),
            request,
            &AtomicBool::new(false),
            Duration::from_secs(5),
        )
        .unwrap()
}

fn read_plates(server: &ReadServer) -> Value {
    let response = plates(
        server,
        Request::Read {
            profile: "1".into(),
        },
    );
    assert_eq!(response.status, 200, "{}", response.body);
    response.body
}

/// Rows the Plate tables and event log hold, as counts.
fn counts(server: &ReadServer) -> [i64; 3] {
    let count = |sql: &str| server.sql().query_row(sql, [], |row| row.get(0)).unwrap();
    [
        count("SELECT COUNT(*) FROM accepted_plate_revisions"),
        count("SELECT COUNT(*) FROM domain_events WHERE event_type='plan.revision_accepted'"),
        count("SELECT COUNT(*) FROM domain_events WHERE event_type='plates.revision_changed'"),
    ]
}

/// The Plate and unit rows of Plate revision `id`, without the revision id.
fn rows(server: &ReadServer, id: i64) -> Vec<String> {
    let sql = server.sql();
    let mut rows = Vec::new();
    for query in [
        "SELECT layout_digest,expected_plate_count,expected_unit_count FROM accepted_plate_revisions WHERE id=?1",
        "SELECT plate_id,ordinal,printer_id,printer_name,printer_model,bed_width_um,bed_depth_um,bed_height_um,margin_um FROM accepted_plates WHERE revision_id=?1 ORDER BY ordinal",
        "SELECT plate_id,required_unit_token,x_um,y_um,width_um,depth_um,height_um,placement,pinned FROM accepted_plate_units WHERE revision_id=?1 ORDER BY required_unit_token",
    ] {
        let mut statement = sql.prepare(query).unwrap();
        let width = statement.column_count();
        rows.extend(
            statement
                .query_map([id], |row| {
                    (0..width)
                        .map(|index| row.get::<_, rusqlite::types::Value>(index))
                        .collect::<rusqlite::Result<Vec<_>>>()
                        .map(|values| format!("{values:?}"))
                })
                .unwrap()
                .map(Result::unwrap),
        );
    }
    rows
}

fn without_part_ids(value: &Value) -> Value {
    match value {
        Value::Array(items) => Value::Array(items.iter().map(without_part_ids).collect()),
        Value::Object(fields) => Value::Object(
            fields
                .iter()
                .filter(|(key, _)| *key != "part_id")
                .map(|(key, value)| (key.clone(), without_part_ids(value)))
                .collect(),
        ),
        other => other.clone(),
    }
}

fn applied(outcome: &pp_storage::plan_publication::Outcome) {
    assert!(
        matches!(
            outcome,
            pp_storage::plan_publication::Outcome::Applied { .. }
        ),
        "{outcome:?}"
    );
}

/// The fixture's draft changes only the excluded Part's status, so Accept
/// keeps every unit. The head is an arrange of every unit (revision 2, which
/// replaced revision 1).
#[tokio::test]
async fn kept_accept_copies_the_layout_for_the_new_plan() {
    let digest = digest();
    let setup = format!(
        "{}{}",
        plate_revision(1, None, &digest),
        plate_revision(2, Some(1), &digest)
    );
    let server = ReadServer::start_limited(8 * 1024 * 1024, &setup).await;
    with_source_revision(&server);
    let before = read_plates(&server);
    assert_eq!(
        (&before["kind"], &before["arrange_undo_revision_id"]),
        (&json!("ready"), &json!(1))
    );
    let workspace = select_units(&server).await;
    let preview = json_reply(server.request(Method::GET, PREVIEW), 200).await;
    assert_eq!(preview["kind"], "changed");
    assert_eq!(preview["changes"]["plates"], json!({"cleared": []}));
    let [revisions, accepted, changed] = counts(&server);

    let revision = revision_of(&workspace);
    applied(&accept(&server, &revision, "kept", false));
    let after = read_plates(&server);
    assert_eq!(
        (
            &after["kind"],
            &after["plate_revision_id"],
            &after["plate_revision_number"],
            &after["arrange_undo_revision_id"],
            &after["basis"]["plan_revision_id"],
            &after["basis"]["plan_version"],
        ),
        (
            &json!("ready"),
            &json!(3),
            &json!(3),
            &Value::Null,
            &json!(2),
            &json!(2)
        )
    );
    // Accept writes new projection Parts, so only `part_id` may change.
    for key in ["printers", "plates", "unplaced", "unassigned"] {
        assert_eq!(
            without_part_ids(&after[key]),
            without_part_ids(&before[key]),
            "{key}"
        );
    }
    assert_eq!(rows(&server, 3), rows(&server, 2));
    assert_eq!(counts(&server), [revisions + 1, accepted + 1, changed + 1]);

    // A replay changes nothing.
    let replay = accept(&server, &revision, "kept", false);
    assert!(
        matches!(
            replay,
            pp_storage::plan_publication::Outcome::Existing { .. }
        ),
        "{replay:?}"
    );
    assert_eq!(counts(&server), [revisions + 1, accepted + 1, changed + 1]);

    let server = server.restart().await;
    assert_eq!(read_plates(&server), after);

    // The arrange before Accept cannot be undone across it.
    let basis = after["basis"].clone();
    let restore = plates(
        &server,
        Request::Restore {
            profile: "1".into(),
            body: json!({"expected": basis, "expected_plate_revision_id": 3,
                "restore_plate_revision_id": 1}),
        },
    );
    assert_eq!(
        (restore.status, &restore.body["code"]),
        (422, &json!("unit_not_found"))
    );
    assert_eq!(read_plates(&server), after);

    // Edits name the new Plan and Plate revision; the old ones are stale.
    let moved = |basis: &Value, revision: i64| {
        plates(
            &server,
            Request::Unit {
                edit: UnitEdit::Move,
                profile: "1".into(),
                plate: PLATE.into(),
                token: format!("ppu_{:032}", 2),
                body: json!({"expected": basis, "expected_plate_revision_id": revision,
                    "x_um": 30_000, "y_um": 40_000}),
            },
        )
    };
    let stale = moved(&before["basis"], 2);
    assert_eq!(
        (stale.status, &stale.body["code"]),
        (409, &json!("accepted_plan_changed"))
    );
    let edited = moved(&basis, 3);
    assert_eq!(
        (edited.status, edited.body),
        (
            200,
            json!({"plate_revision_id": 4, "plate_revision_number": 4})
        )
    );
    server.close().await;
}

/// The draft's edits that change a unit, and a head whose layout fails a
/// Plate read's check: Accept clears the layout, writes no Plate revision,
/// keeps the old rows and appends no Plate event.
#[tokio::test]
async fn accept_that_changes_a_unit_clears_the_layout() {
    let quantity =
        |value: i64| json!({"kind":"set_quantity_override","draft_part_ids":[4],"value":value});
    let exclude_gear = json!({"kind":"set_included","draft_part_ids":[6],"value":false});
    let cases = [
        ("adds a unit", Some(quantity(4)), digest()),
        ("removes a unit", Some(quantity(2)), digest()),
        ("excludes the gear", Some(exclude_gear), digest()),
        ("corrupt layout", None, "0".repeat(64)),
    ];
    for (case, decision, digest) in cases {
        let setup = plate_revision(1, None, &digest);
        let server = ReadServer::start_limited(8 * 1024 * 1024, &setup).await;
        with_source_revision(&server);
        let workspace = match decision {
            Some(decision) => {
                let body = json!({"expected_snapshot_digest": DIGEST, "decisions": [decision]});
                json_reply(
                    server
                        .request(Method::PATCH, "/api/v2/plans/1/drafts/2/parts")
                        .json(&body),
                    200,
                )
                .await
            }
            None => select_units(&server).await,
        };
        let preview = json_reply(server.request(Method::GET, PREVIEW), 200).await;
        assert_eq!(
            preview["changes"]["plates"],
            json!({"cleared": [{"plate_id": PLATE, "ordinal": 1, "printer_name": "Simulator",
                "unit_count": 5}]}),
            "{case}"
        );
        let before = rows(&server, 1);
        let [revisions, accepted, changed] = counts(&server);
        applied(&accept(&server, &revision_of(&workspace), case, false));
        let after = read_plates(&server);
        assert_eq!(
            (&after["kind"], &after["expected_plate_revision_id"]),
            (&json!("setup"), &Value::Null),
            "{case}"
        );
        assert_eq!(rows(&server, 1), before, "{case}");
        assert_eq!(
            counts(&server),
            [revisions, accepted + 1, changed],
            "{case}"
        );
        server.close().await;
    }
}

/// Without a Plate head, Accept writes no Plate revision.
#[tokio::test]
async fn accept_without_a_plate_head_writes_no_plate_revision() {
    let server = ReadServer::start_limited(8 * 1024 * 1024, "").await;
    with_source_revision(&server);
    let workspace = select_units(&server).await;
    let [revisions, accepted, changed] = counts(&server);
    assert_eq!(revisions, 0);
    applied(&accept(&server, &revision_of(&workspace), "no-head", false));
    assert_eq!(counts(&server), [0, accepted + 1, changed]);
    assert_eq!(read_plates(&server)["kind"], "setup");
    server.close().await;
}
