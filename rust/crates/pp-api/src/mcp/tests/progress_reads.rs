//! GRE-1297 read tools over streamable HTTP: `get_remaining` and
//! `get_printer_checkoff` answer Node's text for the caller's tenant only.
//! Each expected string is Node's tool output on this seed.

use super::reads::{call, session};
use super::*;

/// Units 1-4 printed; a fleet whose `int-b` is ambiguous; links, open,
/// dismissed and claimed unattributed prints for `default`, and a link and
/// fleet for `foreign` that must never show.
const SEED: &str = r#"
UPDATE print_progress SET completed=1 WHERE id IN (1,2,3,4);
INSERT INTO app_settings (tenant_id,key,value) VALUES
('default','printer.fleet','[{"id":"p1","name":"Prusa XL","integration_id":"int-a"},{"id":"p2","name":"Voron","integration_id":" int-b "},{"id":"p3","name":"Bambu","integration_id":"int-b"}]'),
('default','printer.checkoff_links','[
 {"id":"l1","profile_id":1,"integration_id":"int-a","printer_id":"p1","host_name":"Old name","filename":"plate1.gcode","units":[{"part_id":1,"unit_index":0,"object_name":"a"}],"state":"awaiting_verify","saw_active":true,"created_at":"2026-11-01T10:00:00.000Z","completed_at":"2026-11-01T12:00:00.000Z"},
 {"id":"l2","profile_id":1,"integration_id":"int-b","printer_id":"gone","host_name":"Keep me","filename":"plate2.gcode","units":[],"state":"watching","saw_active":false,"created_at":"2026-11-02T10:00:00.000Z"},
 {"id":"l3","profile_id":2,"integration_id":"int-c","printer_id":"x","host_name":"Elsewhere","filename":"plate3.gcode","units":[],"state":"verified","saw_active":true,"created_at":"2026-11-03T10:00:00.000Z"}]'),
('default','printer.unattributed_prints','[
 {"id":"u1","integration_id":"int-a","printer_id":"zzz","host_name":"H","filename":"a.gcode","completed_at":"2026-11-04T10:00:00.000Z","gcode_objects":["a"],"candidates":[]},
 {"id":"u2","integration_id":"int-a","printer_id":"p1","host_name":"H","filename":"b.gcode","completed_at":"2026-11-04T11:00:00.000Z","gcode_objects":[],"candidates":[],"dismissed":true,"dismissed_at":"2099-01-01T00:00:00.000Z"},
 {"id":"u3","integration_id":"int-z","printer_id":5,"filename":"c.gcode","completed_at":"2026-11-04T12:00:00.000Z","gcode_objects":[],"candidates":[],"claimed_at":"2099-01-01T00:00:00.000Z","claimed_profile_id":1}]'),
('foreign','printer.fleet','[{"id":"p1","name":"Foreign printer","integration_id":"int-a"}]'),
('foreign','printer.checkoff_links','[{"id":"f1","profile_id":2,"integration_id":"int-a","printer_id":"p1","host_name":"F","filename":"secret.gcode","units":[],"state":"awaiting_verify","saw_active":true,"created_at":"2026-11-01T10:00:00.000Z"}]');
"#;

/// `(tool, arguments, Node's text, isError)`, recorded from Node on [`SEED`].
const CASES: [(&str, &str, &str, bool); 16] = [
    (
        "get_remaining",
        r#"{"plan_id":1}"#,
        r#"{"plan_id":1,"plan_name":"Fixture Build","archived_at":null,"summary":"1/2 parts fully printed · 3/5 units","printed_units":3,"total_units":5,"remaining_units":2,"percent":60,"can_archive":false,"part_count":2}"#,
        false,
    ),
    (
        "get_remaining",
        r#"{"plan_id":"1.9"}"#,
        r#"{"plan_id":1,"plan_name":"Fixture Build","archived_at":null,"summary":"1/2 parts fully printed · 3/5 units","printed_units":3,"total_units":5,"remaining_units":2,"percent":60,"can_archive":false,"part_count":2}"#,
        false,
    ),
    (
        "get_remaining",
        r#"{"plan_id":2}"#,
        r#"{"error":"Plan not found"}"#,
        true,
    ),
    (
        "get_remaining",
        r#"{"plan_id":99}"#,
        r#"{"error":"Plan not found"}"#,
        true,
    ),
    (
        "get_remaining",
        r#"{"plan_id":0}"#,
        r#"{"error":"Plan not found"}"#,
        true,
    ),
    (
        "get_remaining",
        r#"{"plan_id":-3}"#,
        r#"{"error":"Plan not found"}"#,
        true,
    ),
    (
        "get_remaining",
        r#"{"plan_id":"x"}"#,
        r#"{"error":"plan_id required"}"#,
        true,
    ),
    (
        "get_remaining",
        r#"{}"#,
        r#"{"error":"plan_id required"}"#,
        true,
    ),
    (
        "get_printer_checkoff",
        r#"{}"#,
        r#"{"links":[{"id":"l1","profile_id":1,"integration_id":"int-a","printer_id":"p1","host_name":"Prusa XL","filename":"plate1.gcode","units":[{"part_id":1,"unit_index":0,"object_name":"a"}],"state":"awaiting_verify","host_outcome":"success","saw_active":true,"started":false,"created_at":"2026-11-01T10:00:00.000Z","completed_at":"2026-11-01T12:00:00.000Z"},{"id":"l2","profile_id":1,"integration_id":"int-b","printer_id":"gone","host_name":"Keep me","filename":"plate2.gcode","units":[],"state":"watching","saw_active":false,"started":false,"created_at":"2026-11-02T10:00:00.000Z"},{"id":"l3","profile_id":2,"integration_id":"int-c","printer_id":"x","host_name":"Elsewhere","filename":"plate3.gcode","units":[],"state":"verified","host_outcome":"success","saw_active":true,"started":false,"created_at":"2026-11-03T10:00:00.000Z"}],"unattributed":[{"id":"u1","integration_id":"int-a","printer_id":"zzz","host_name":"Prusa XL","filename":"a.gcode","completed_at":"2026-11-04T10:00:00.000Z","gcode_objects":["a"],"candidates":[]},{"id":"u3","integration_id":"int-z","printer_id":5,"filename":"c.gcode","completed_at":"2026-11-04T12:00:00.000Z","gcode_objects":[],"candidates":[],"claimed_at":"2099-01-01T00:00:00.000Z","claimed_profile_id":1}],"pending_verification":1}"#,
        false,
    ),
    (
        "get_printer_checkoff",
        r#"{"plan_id":1}"#,
        r#"{"links":[{"id":"l1","profile_id":1,"integration_id":"int-a","printer_id":"p1","host_name":"Prusa XL","filename":"plate1.gcode","units":[{"part_id":1,"unit_index":0,"object_name":"a"}],"state":"awaiting_verify","host_outcome":"success","saw_active":true,"started":false,"created_at":"2026-11-01T10:00:00.000Z","completed_at":"2026-11-01T12:00:00.000Z"},{"id":"l2","profile_id":1,"integration_id":"int-b","printer_id":"gone","host_name":"Keep me","filename":"plate2.gcode","units":[],"state":"watching","saw_active":false,"started":false,"created_at":"2026-11-02T10:00:00.000Z"}],"unattributed":[{"id":"u3","integration_id":"int-z","printer_id":5,"filename":"c.gcode","completed_at":"2026-11-04T12:00:00.000Z","gcode_objects":[],"candidates":[],"claimed_at":"2099-01-01T00:00:00.000Z","claimed_profile_id":1}],"pending_verification":1}"#,
        false,
    ),
    (
        "get_printer_checkoff",
        r#"{"plan_id":"2"}"#,
        r#"{"links":[{"id":"l3","profile_id":2,"integration_id":"int-c","printer_id":"x","host_name":"Elsewhere","filename":"plate3.gcode","units":[],"state":"verified","host_outcome":"success","saw_active":true,"started":false,"created_at":"2026-11-03T10:00:00.000Z"}],"unattributed":[],"pending_verification":0}"#,
        false,
    ),
    (
        "get_printer_checkoff",
        r#"{"plan_id":1,"state":" awaiting_verify "}"#,
        r#"{"links":[{"id":"l1","profile_id":1,"integration_id":"int-a","printer_id":"p1","host_name":"Prusa XL","filename":"plate1.gcode","units":[{"part_id":1,"unit_index":0,"object_name":"a"}],"state":"awaiting_verify","host_outcome":"success","saw_active":true,"started":false,"created_at":"2026-11-01T10:00:00.000Z","completed_at":"2026-11-01T12:00:00.000Z"}],"unattributed":[{"id":"u3","integration_id":"int-z","printer_id":5,"filename":"c.gcode","completed_at":"2026-11-04T12:00:00.000Z","gcode_objects":[],"candidates":[],"claimed_at":"2099-01-01T00:00:00.000Z","claimed_profile_id":1}],"pending_verification":1}"#,
        false,
    ),
    (
        "get_printer_checkoff",
        r#"{"integration_id":"int-b"}"#,
        r#"{"links":[{"id":"l2","profile_id":1,"integration_id":"int-b","printer_id":"gone","host_name":"Keep me","filename":"plate2.gcode","units":[],"state":"watching","saw_active":false,"started":false,"created_at":"2026-11-02T10:00:00.000Z"}],"unattributed":[{"id":"u1","integration_id":"int-a","printer_id":"zzz","host_name":"Prusa XL","filename":"a.gcode","completed_at":"2026-11-04T10:00:00.000Z","gcode_objects":["a"],"candidates":[]},{"id":"u3","integration_id":"int-z","printer_id":5,"filename":"c.gcode","completed_at":"2026-11-04T12:00:00.000Z","gcode_objects":[],"candidates":[],"claimed_at":"2099-01-01T00:00:00.000Z","claimed_profile_id":1}],"pending_verification":0}"#,
        false,
    ),
    (
        "get_printer_checkoff",
        r#"{"state":"bogus"}"#,
        r#"{"error":"invalid checkoff state"}"#,
        true,
    ),
    (
        "get_printer_checkoff",
        r#"{"state":7,"integration_id":null}"#,
        r#"{"links":[{"id":"l1","profile_id":1,"integration_id":"int-a","printer_id":"p1","host_name":"Prusa XL","filename":"plate1.gcode","units":[{"part_id":1,"unit_index":0,"object_name":"a"}],"state":"awaiting_verify","host_outcome":"success","saw_active":true,"started":false,"created_at":"2026-11-01T10:00:00.000Z","completed_at":"2026-11-01T12:00:00.000Z"},{"id":"l2","profile_id":1,"integration_id":"int-b","printer_id":"gone","host_name":"Keep me","filename":"plate2.gcode","units":[],"state":"watching","saw_active":false,"started":false,"created_at":"2026-11-02T10:00:00.000Z"},{"id":"l3","profile_id":2,"integration_id":"int-c","printer_id":"x","host_name":"Elsewhere","filename":"plate3.gcode","units":[],"state":"verified","host_outcome":"success","saw_active":true,"started":false,"created_at":"2026-11-03T10:00:00.000Z"}],"unattributed":[{"id":"u1","integration_id":"int-a","printer_id":"zzz","host_name":"Prusa XL","filename":"a.gcode","completed_at":"2026-11-04T10:00:00.000Z","gcode_objects":["a"],"candidates":[]},{"id":"u3","integration_id":"int-z","printer_id":5,"filename":"c.gcode","completed_at":"2026-11-04T12:00:00.000Z","gcode_objects":[],"candidates":[],"claimed_at":"2099-01-01T00:00:00.000Z","claimed_profile_id":1}],"pending_verification":1}"#,
        false,
    ),
    (
        "get_printer_checkoff",
        r#"{"plan_id":99}"#,
        r#"{"links":[],"unattributed":[],"pending_verification":0}"#,
        false,
    ),
];

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn ticket_t_62_mcp_domain_progress_reads_answer_node_text_for_the_callers_tenant() {
    let server = McpServer::seeded(SEED).await;
    let (_, key) = server.create_key().await;
    let headers = session(&server, &format!("Bearer {key}")).await;
    for (tool, arguments, want, is_error) in CASES {
        let arguments: Value = serde_json::from_str(arguments).unwrap();
        let got = call(&server, &headers, tool, arguments.clone()).await;
        assert_eq!(got, (want.to_owned(), is_error), "{tool} {arguments}");
    }
}
