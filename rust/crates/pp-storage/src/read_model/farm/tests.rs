use super::*;
use serde_json::json;

fn farm(links: Value, unmatched: Value) -> Farm {
    let list = |value: Value| value.as_array().cloned().unwrap();
    Farm::parse(
        list(json!([
            {"id": "p-none", "name": "Annex K3", "max_filament_slots": 2,
             "loaded_filaments": [{"slot": 2, "filament_color_id": "pla-red", "label": "Red"}]},
            {"id": "p-gone", "name": "Gone", "integration_id": "missing"},
            {"id": "p-off", "name": "Off", "integration_id": "off"},
            {"id": "p-prusa", "name": "MK4", "integration_id": "prusa"},
            {"id": "p-moon", "name": "Micron", "model": "Voron Micron", "integration_id": "moon"},
            {"id": "p-spool", "name": "Spool", "integration_id": "spool"},
            {"name": "No id"}
        ])),
        list(json!([
            {"id": "off", "type": "moonraker", "config": {"enabled": false}},
            {"id": "prusa", "type": "prusalink", "config": {}},
            {"id": "moon", "type": "moonraker", "config": {"base_url": "http://printer.test"}},
            {"id": "spool", "type": "spoolman"}
        ])),
        list(links),
        list(unmatched),
        list(json!([
            {"id": "q1", "filename": "a.gcode", "artifact_path": "/data/exports/a.gcode",
             "printer_id": "p-moon", "state": "sending", "start": true, "profile_id": "7",
             "checkoff_units": [{"part_id": 1, "unit_index": 0}], "error": "  Upload failed  ",
             "upload_job_id": "job-1", "outcome_unknown": true,
             "created_at": "2026-11-05T01:00:00.000Z"},
            {"id": "q2", "filename": "b.gcode", "printer_id": "p-moon", "state": "queued"},
            {"id": "q3", "filename": "c.gcode", "artifact_path": "x", "printer_id": "p",
             "state": "bogus"}
        ])),
    )
}

fn link(state: &str, completed: Option<&str>, applied: Option<&str>) -> Value {
    json!({"id": format!("l-{state}"), "profile_id": 3, "printer_id": "p-moon",
        "integration_id": "moon", "filename": "dir/Plate.gcode", "state": state,
        "units": [{"part_id": 1, "unit_index": 0}, {"part_id": 1, "unit_index": 1}],
        "resolved_units": [{"part_id": 1}], "created_at": "2026-11-05T00:00:00.000Z",
        "completed_at": completed, "applied_at": applied})
}

fn seen(state: HostState) -> Observation {
    Observation::Seen {
        state,
        progress_percent: Some(42),
        filename: Some("dir/Plate.gcode".into()),
        last_heard_at: Some("2026-11-05T02:00:00.000Z".into()),
        state_since: Some("2026-11-05T01:00:00.000Z".into()),
    }
}

fn observe(moon: Observation) -> impl Fn(&PrinterIntegration) -> Observation {
    move |integration| match integration.id.as_str() {
        "moon" => moon.clone(),
        "prusa" => Observation::NotWatched,
        id => panic!("observed {id}"),
    }
}

#[test]
fn farm_printer_reasons_are_distinct_and_only_enabled_printers_are_observed() {
    let farm = farm(json!([]), json!([]));
    let rows: Vec<_> = farm
        .printers(observe(seen(HostState::Offline)))
        .printers
        .into_iter()
        .map(|p| {
            let value = serde_json::to_value(&p).unwrap();
            (
                p.id,
                value["state"]["execution"].clone(),
                value["reason"].clone(),
            )
        })
        .collect();
    let row =
        |id: &str, execution: &str, reason: &str| (id.into(), json!(execution), json!(reason));
    assert_eq!(
        rows,
        vec![
            row("p-none", "not_set_up", "not_set_up"),
            row("p-gone", "not_set_up", "integration_missing"),
            row("p-off", "manual", "monitoring_disabled"),
            row("p-prusa", "manual", "monitoring_not_supported"),
            row("p-moon", "unreachable", "unreachable"),
            row("p-spool", "not_set_up", "integration_missing"),
        ]
    );
    let unavailable = farm.printers(observe(Observation::Unavailable));
    assert_eq!(
        unavailable.printers[4].reason,
        Some(PrinterReason::AwaitingStatus)
    );
    let unwatched = Farm::parse(
        vec![json!({"id": "p-blank", "name": "Blank", "integration_id": "blank"})],
        vec![json!({"id": "blank", "type": "moonraker", "config": {"base_url": " "}})],
        vec![],
        vec![],
        vec![],
    )
    .printers(|_| panic!("a host no watcher polls is not observed"));
    assert_eq!(
        unwatched.printers[0].reason,
        Some(PrinterReason::MonitoringNotSupported),
        "an address-less Moonraker host never waits on a status"
    );
    let unsupported = farm.printers(observe(Observation::NotSupported));
    assert_eq!(
        unsupported.printers[4].reason,
        Some(PrinterReason::MonitoringNotSupported)
    );
    let printing = farm.printers(observe(seen(HostState::Printing)));
    let moon = serde_json::to_value(&printing.printers[4]).unwrap();
    assert_eq!(
        moon,
        json!({"id": "p-moon", "name": "Micron", "model": "Voron Micron",
            "integration": {"id": "moon", "kind": "moonraker"},
            "state": {"execution": "printing", "bed_clear": "needs_clearing", "display": "printing"},
            "reason": null, "last_heard_at": "2026-11-05T02:00:00.000Z", "progress_percent": 42,
            "loaded_filaments": [{"slot": 1, "filament_color_id": null, "label": ""}]})
    );
    let idle = farm.printers(observe(seen(HostState::Idle)));
    assert_eq!(idle.printers[4].progress_percent, None);
    assert_eq!(
        serde_json::to_value(&idle.printers[0].loaded_filaments).unwrap(),
        json!([{"slot": 1, "filament_color_id": null, "label": ""},
               {"slot": 2, "filament_color_id": "pla-red", "label": "Red"}])
    );
}

#[test]
fn farm_bed_is_clear_only_after_a_verified_result_with_nothing_newer() {
    let bed = |links: Value, unmatched: Value, host: HostState| {
        let printers = farm(links, unmatched).printers(observe(seen(host)));
        serde_json::to_value(printers.printers[4].state).unwrap()
    };
    let verified = link(
        "verified",
        Some("2026-11-05T01:00:00.000Z"),
        Some("2026-11-05T01:30:00.000Z"),
    );
    assert_eq!(
        bed(json!([verified]), json!([]), HostState::Complete),
        json!({"execution": "idle", "bed_clear": "clear", "display": "ready"})
    );
    let direct = Observation::Seen {
        state: HostState::Complete,
        progress_percent: None,
        filename: Some("direct.gcode".into()),
        last_heard_at: None,
        state_since: Some("2026-11-05T01:00:00.000Z".into()),
    };
    let direct = farm(json!([verified]), json!([])).printers(observe(direct));
    assert_eq!(
        direct.printers[4].state.bed_clear(),
        BedClearState::NeedsClearing,
        "a job run outside Print Partner is not covered by an older verification"
    );
    for since in [Some("2026-11-05T01:45:00.000Z"), None] {
        let reprint = Observation::Seen {
            state: HostState::Complete,
            progress_percent: None,
            filename: Some("Plate.gcode".into()),
            last_heard_at: None,
            state_since: since.map(Into::into),
        };
        let reprint = farm(json!([verified]), json!([])).printers(observe(reprint));
        assert_eq!(
            reprint.printers[4].state.bed_clear(),
            BedClearState::NeedsClearing,
            "the verified file completing again after its confirmation ({since:?}) is a new part"
        );
    }
    let awaiting = link("awaiting_verify", Some("2026-11-05T01:45:00.000Z"), None);
    assert_eq!(
        bed(json!([verified, awaiting]), json!([]), HostState::Idle)["display"],
        "bed_to_clear"
    );
    let later = json!([{"id": "u1", "integration_id": "moon", "printer_id": "p-moon",
        "filename": "other.gcode", "completed_at": "2026-11-05T01:40:00.000Z"}]);
    assert_eq!(
        bed(json!([verified]), later, HostState::Idle)["bed_clear"],
        "needs_clearing"
    );
    assert_eq!(
        bed(json!([]), json!([]), HostState::Complete)["bed_clear"],
        "needs_clearing"
    );
    assert_eq!(
        bed(json!([]), json!([]), HostState::Idle)["bed_clear"],
        "unknown"
    );
    assert_eq!(
        bed(json!([verified]), json!([]), HostState::Error),
        json!({"execution": "failed", "bed_clear": "needs_clearing", "display": "failed"})
    );
}

#[test]
fn farm_lists_skip_unusable_rows_and_hide_paths() {
    let farm = farm(
        json!([link("applied", None, None), {"id": "bad", "state": "watching"}]),
        json!([
            {"id": "u-linked", "integration_id": "moon", "printer_id": "p-moon",
             "filename": "PLATE.gcode", "completed_at": "2026-11-05T01:00:00.000Z"},
            {"id": "u-open", "integration_id": "moon", "printer_id": "p-moon",
             "filename": "x.gcode", "completed_at": "2026-11-05T01:00:00.000Z",
             "gcode_objects": ["a", "b"], "candidates": [{}]},
            {"id": "u-claimed", "integration_id": "moon", "printer_id": "p-moon",
             "filename": "y.gcode", "completed_at": "c", "claimed_at": "d"},
            {"id": "u-dismissed", "integration_id": "moon", "printer_id": "p-moon",
             "filename": "z.gcode", "completed_at": "c", "dismissed": true}
        ]),
    );
    let results = serde_json::to_value(farm.checkoff()).unwrap();
    assert_eq!(
        results,
        json!({"results": [{"id": "l-applied", "profile_id": 3, "printer_id": "p-moon",
            "integration_id": "moon", "filename": "dir/Plate.gcode", "state": "verified",
            "host_outcome": "success", "unit_count": 2, "resolved_unit_count": 1,
            "units": [{"part_id": 1, "unit_index": 0, "part_filename": null},
                      {"part_id": 1, "unit_index": 1, "part_filename": null}],
            "resolved_units": [],
            "created_at": "2026-11-05T00:00:00.000Z", "completed_at": null, "applied_at": null}]})
    );
    assert_eq!(
        serde_json::to_value(farm.unmatched()).unwrap(),
        json!({"prints": [{"id": "u-open", "printer_id": "p-moon", "integration_id": "moon",
            "filename": "x.gcode", "completed_at": "2026-11-05T01:00:00.000Z",
            "object_count": 2, "candidate_count": 1}]})
    );
    let queue = serde_json::to_value(farm.send_queue()).unwrap();
    assert_eq!(
        queue,
        json!({"items": [{"id": "q1", "filename": "a.gcode", "printer_id": "p-moon",
            "profile_id": 7, "state": "sending", "start": true, "wait_for_idle": true,
            "unit_count": 1, "created_at": "2026-11-05T01:00:00.000Z", "updated_at": null,
            "error": "Upload failed", "upload_job_id": "job-1", "outcome_unknown": true}]})
    );
    let many: Vec<_> = (0..300)
        .map(|i| {
            json!({"id": format!("q{i}"), "filename": "a.gcode", "artifact_path": "x",
            "printer_id": "p", "state": "queued"})
        })
        .collect();
    let all = Farm::parse(vec![], vec![], vec![], vec![], many);
    assert_eq!(
        all.send_queue().items.len(),
        300,
        "active sends are never dropped"
    );
    let text = queue.to_string();
    assert!(!text.contains("/data/exports"));
}

#[test]
fn farm_results_fill_outcome_and_completion_as_node_does() {
    let farm = farm(
        json!([
            link("host_failed", None, Some("2026-11-05T03:00:00.000Z")),
            link("watching", None, None)
        ]),
        json!([]),
    );
    let results = serde_json::to_value(farm.checkoff()).unwrap();
    assert_eq!(results["results"][0]["host_outcome"], "failed");
    assert_eq!(
        results["results"][0]["completed_at"], "2026-11-05T03:00:00.000Z",
        "a missing completion falls back to applied_at"
    );
    assert_eq!(results["results"][1]["host_outcome"], Value::Null);
}

#[test]
fn farm_stream_printers_carry_known_fields_only() {
    let watching = json!({"id": "l-run", "profile_id": 3, "printer_id": "p-moon",
        "integration_id": "moon", "filename": "dir/Plate.gcode", "state": "watching",
        "units": [], "created_at": "2026-11-05T00:00:00.000Z"});
    let farm = farm(json!([watching]), json!([]));
    let printing = farm.stream_printers(observe(seen(HostState::Printing)));
    assert_eq!(
        serde_json::to_value(&printing[4]).unwrap(),
        json!({"id": "p-moon", "name": "Micron", "state": "printing",
            "display": {"execution": "printing", "bed_clear": "needs_clearing", "display": "printing"},
            "result_id": "l-run", "progress_percent": 42, "bed_needs_clearing": true,
            "filaments": [{"slot": 1}], "last_heard_at": "2026-11-05T02:00:00.000Z"})
    );
    let quiet = serde_json::to_value(&printing[0]).unwrap();
    assert_eq!(quiet["state"], "unknown");
    assert_eq!(
        quiet["filaments"][1],
        json!({"slot": 2, "color_id": "pla-red", "label": "Red"})
    );
    for absent in [
        "result_id",
        "progress_percent",
        "bed_needs_clearing",
        "last_heard_at",
    ] {
        assert!(quiet.get(absent).is_none(), "{absent}");
    }
    let mut later = printing[4].clone();
    later.last_heard_at = Some("2026-11-05T02:00:05.000Z".into());
    assert!(
        later.same_state(&printing[4]),
        "a new last-heard time is not a change"
    );
    let idle = farm.stream_printers(observe(seen(HostState::Idle)));
    assert!(!idle[4].same_state(&printing[4]));
    assert_eq!(idle[4].result_id, None);
}

#[test]
fn farm_results_list_units_and_decisions_and_skip_unreadable_ones() {
    let mut stored = link("awaiting_verify", None, None);
    stored["units"] = json!([
        {"part_id": 7, "unit_index": 0, "object_name": "a"},
        {"part_id": "8", "unit_index": 2},
        {"part_id": 0, "unit_index": 1},
        {"part_id": 7, "unit_index": -1},
        {"part_id": 7, "unit_index": 1.5},
        {"part_id": 7},
        "junk"
    ]);
    stored["resolved_units"] = json!([
        {"part_id": 7, "unit_index": 0, "result": "confirmed"},
        {"part_id": 8, "unit_index": 2, "result": "rejected", "reason": "spaghetti"},
        {"part_id": 8, "unit_index": 3, "result": "maybe"},
        {"part_id": 8, "unit_index": 4}
    ]);
    let results = farm(json!([stored]), json!([])).checkoff().results;
    let result = &results[0];
    assert_eq!((result.unit_count, result.resolved_unit_count), (7, 4));
    assert_eq!(
        serde_json::to_value(&result.units).unwrap(),
        json!([{"part_id": 7, "unit_index": 0, "part_filename": null},
               {"part_id": 8, "unit_index": 2, "part_filename": null}])
    );
    assert_eq!(
        serde_json::to_value(&result.resolved_units).unwrap(),
        json!([{"part_id": 7, "unit_index": 0, "result": "confirmed"},
               {"part_id": 8, "unit_index": 2, "result": "rejected"}])
    );
}

#[test]
fn farm_names_result_parts_from_the_tenants_rows_only() {
    let mut connection = Connection::open_in_memory().unwrap();
    connection
        .execute_batch(
            "CREATE TABLE parts (id INTEGER PRIMARY KEY, tenant_id TEXT NOT NULL, filename TEXT NOT NULL);
             INSERT INTO parts VALUES (7, 'default', 'bracket.stl'), (8, 'other', 'theirs.stl'),
                                      (9, 'default', '');",
        )
        .unwrap();
    let mut stored = link("awaiting_verify", None, None);
    stored["units"] = json!([
        {"part_id": 7, "unit_index": 0},
        {"part_id": 7, "unit_index": 1},
        {"part_id": 8, "unit_index": 0},
        {"part_id": 9, "unit_index": 0},
        {"part_id": 10, "unit_index": 0}
    ]);
    let mut read = farm(json!([stored]), json!([]));
    let tx = connection.transaction().unwrap();
    read.name_parts(&tx, "default").unwrap();
    let names: Vec<_> = read.checkoff().results[0]
        .units
        .iter()
        .map(|unit| unit.part_filename.clone())
        .collect();
    assert_eq!(
        names,
        [
            Some("bracket.stl".into()),
            Some("bracket.stl".into()),
            None,
            None,
            None
        ],
        "another tenant's Part, an empty file name and a missing Part stay null"
    );
}

#[test]
fn ticket_t_22_unpolled_prusalink_hosts_never_wait_on_a_status() {
    // A PrusaLink host past the watchers' cap, or without an address, is
    // never polled, so it reads as not supported instead of awaiting status.
    let mut integrations: Vec<Value> = (0..crate::printer_hosts::MAX_HOSTS - 1)
        .map(|i| json!({"id": format!("m{i}"), "type": "moonraker", "config": {"base_url": "http://m.test"}}))
        .collect();
    integrations.extend([
        json!({"id": "mk4", "type": "prusalink", "config": {"base_url": "http://p.test"}}),
        json!({"id": "late", "type": "prusalink", "config": {"base_url": "http://q.test"}}),
    ]);
    let farm = Farm::parse(
        vec![
            json!({"id": "p-mk4", "name": "MK4", "integration_id": "mk4"}),
            json!({"id": "p-late", "name": "Late", "integration_id": "late"}),
        ],
        integrations,
        vec![],
        vec![],
        vec![],
    );
    let printers = farm
        .printers(|integration| {
            assert_eq!(integration.id, "mk4", "only the polled host is observed");
            Observation::Unavailable
        })
        .printers;
    assert_eq!(printers[0].reason, Some(PrinterReason::AwaitingStatus));
    assert_eq!(
        printers[1].reason,
        Some(PrinterReason::MonitoringNotSupported)
    );
    let blank = Farm::parse(
        vec![json!({"id": "p-blank", "name": "Blank", "integration_id": "blank"})],
        vec![json!({"id": "blank", "type": "prusalink", "config": {"base_url": " "}})],
        vec![],
        vec![],
        vec![],
    )
    .printers(|_| panic!("a host no watcher polls is not observed"));
    assert_eq!(
        blank.printers[0].reason,
        Some(PrinterReason::MonitoringNotSupported)
    );
}
