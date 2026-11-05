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
            {"id": "moon", "type": "moonraker"},
            {"id": "spool", "type": "spoolman"}
        ])),
        list(links),
        list(unmatched),
        list(json!([
            {"id": "q1", "filename": "a.gcode", "artifact_path": "/data/exports/a.gcode",
             "printer_id": "p-moon", "state": "sending", "start": true, "profile_id": "7",
             "checkoff_units": [{"part_id": 1, "unit_index": 0}], "error": "secret detail",
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
fn farm_lists_skip_unusable_rows_and_hide_paths_and_error_text() {
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
            "host_outcome": null, "unit_count": 2, "resolved_unit_count": 1,
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
            "unit_count": 1, "created_at": "2026-11-05T01:00:00.000Z", "updated_at": null}]})
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
    assert!(!text.contains("/data/exports") && !text.contains("secret detail"));
}
