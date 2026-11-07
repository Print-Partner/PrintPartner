#![cfg(feature = "desktop_rebuild")]
//! GRE-1217: the create path of `materializeAcceptedPrinterLink` for print-file
//! assignments. Replays `fixtures/parity/printer-file-assignment/node.json`
//! through the public checkoff progress client on a copy of the same fixture
//! and compares every receipt and stored row.
use pp_storage::{
    Limits, WriterOwner,
    auth::Secret,
    checkoff_progress::{
        AssignedLink, Assignment, Request,
        attribution::{Mapping, Unit},
    },
    read_model::Credential,
};
use serde_json::{Value, json};
use std::{path::PathBuf, sync::atomic::AtomicBool, time::Duration};

#[path = "common/js_json.rs"]
mod js_json;
#[path = "common/json_rows.rs"]
mod json_rows;
#[path = "common/normalizer.rs"]
mod normalizer;
use normalizer::Normalizer;

const NODE: &str = include_str!("../../../../fixtures/parity/printer-file-assignment/node.json");
const DATABASE: &[u8] = include_bytes!("fixtures/required-units/unchanged.db");

struct Fixture(PathBuf);

impl Fixture {
    fn new(node: &Value) -> Self {
        let path = std::env::temp_dir().join(format!(
            "pp-printer-file-assignment-{}",
            hex::encode(rand::random::<[u8; 16]>())
        ));
        std::fs::create_dir_all(path.join("repos/1/revisions/fixture")).unwrap();
        std::fs::write(path.join("print-partner.db"), DATABASE).unwrap();
        let fixture = Self(path);
        for sql in node["setup"].as_array().unwrap() {
            fixture.sql().execute_batch(sql.as_str().unwrap()).unwrap();
        }
        fixture
    }

    fn sql(&self) -> rusqlite::Connection {
        rusqlite::Connection::open(self.0.join("print-partner.db")).unwrap()
    }

    fn rows(&self, node: &Value, normalizer: &mut Normalizer) -> Value {
        let mut rows = json_rows::query_rows(&self.sql(), node["queries"].as_object().unwrap());
        for link in rows["links"].as_array_mut().unwrap() {
            link["value"] = js_json::parse(link["value"].as_str().unwrap());
        }
        normalizer.walk(rows)
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn unit(raw: &Value) -> Unit {
    Unit::at(
        raw["part_id"].as_i64().unwrap(),
        raw["unit_index"].as_i64().unwrap(),
    )
}

fn text(raw: &Value) -> Option<String> {
    raw.as_str().map(str::to_owned)
}

fn assignment(command: &Value) -> Assignment {
    let link = &command["link"];
    Assignment {
        profile_id: command["profileId"].as_i64().unwrap(),
        plan_revision_id: command["expectedPlanRevisionId"].as_i64().unwrap(),
        object_names: serde_json::from_value(command["objectNames"].clone()).unwrap(),
        units: command["confirmedUnits"]
            .as_array()
            .unwrap()
            .iter()
            .map(unit)
            .collect(),
        mappings: command["objectMappings"].as_array().map(|mappings| {
            mappings
                .iter()
                .map(|mapping| Mapping {
                    object_index: mapping["object_index"].as_u64().unwrap() as usize,
                    unit: unit(mapping),
                })
                .collect()
        }),
        retain_unmatched: command["retainUnmatched"] == true,
        completed: command["completed"] == true,
        link: AssignedLink {
            integration_id: text(&link["integrationId"]).unwrap(),
            printer_id: text(&link["printerId"]).unwrap(),
            host_name: text(&link["hostName"]).unwrap(),
            filename: text(&link["filename"]).unwrap(),
            remote_path: text(&link["remotePath"]),
            remote_identity: link.get("remoteIdentity").cloned(),
            classification: link.get("classification").cloned(),
        },
        snapshot_source: None,
    }
}

#[test]
fn printer_file_assignment_matches_node_for_every_step() {
    let node = js_json::parse(NODE);
    let fixture = Fixture::new(&node);
    let mut normalizer = Normalizer::default();
    assert_eq!(fixture.rows(&node, &mut normalizer), node["before"]);
    let owner = WriterOwner::open(&fixture.0, Limits::default()).unwrap().0;
    let steps = node["steps"].as_array().unwrap();
    assert!(steps.len() >= 18);
    for (index, step) in steps.iter().enumerate() {
        let response = owner
            .checkoff_progress()
            .unwrap()
            .apply(
                Credential::Session(Secret::new("required-unit-fixture-secret".into())),
                Request::AssignFile(Box::new(assignment(&step["command"]))),
                &AtomicBool::new(false),
                Duration::from_secs(5),
            )
            .unwrap();
        let receipt = normalizer.walk(serde_json::to_value(response).unwrap());
        assert_eq!(receipt, step["receipt"], "step {index} receipt");
        assert_eq!(
            fixture.rows(&node, &mut normalizer),
            step["rows"],
            "step {index} rows"
        );
    }
    let statuses: Vec<&Value> = steps.iter().map(|s| &s["receipt"]["status"]).collect();
    for status in [json!(200), json!(409), json!(500)] {
        assert!(statuses.contains(&&status), "Node corpus covers {status}");
    }
    owner.shutdown().unwrap();
}
