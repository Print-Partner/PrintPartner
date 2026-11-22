use super::*;
use serde_json::json;

const NOW: &str = "2026-11-21T12:00:00.000Z";

fn link(fields: Value) -> Link {
    let mut raw = json!({
        "id": "l1",
        "profile_id": 1,
        "integration_id": "moon-1",
        "printer_id": "p1",
        "filename": "plate-a.gcode",
        "remote_path": "pp/plate-a.gcode",
        "state": "watching",
        "created_at": "2026-11-21T10:00:00.000Z",
    });
    for (key, value) in fields.as_object().unwrap() {
        raw[key] = value.clone();
    }
    links::parse(&raw, NOW).unwrap()
}

fn report(state: HostState, filename: Option<&str>, progress: Option<f64>) -> HostReport {
    HostReport {
        state,
        filename: filename.map(str::to_owned),
        progress,
    }
}

fn now() -> OffsetDateTime {
    OffsetDateTime::parse(NOW, &Rfc3339).unwrap()
}

#[test]
fn gre_1897_a_finish_of_the_sent_file_awaits_verify() {
    use HostState::*;
    let fresh = link(json!({}));
    for name in [
        "plate-a.gcode",
        " PP/Plate-A.GCODE ",
        "C:\\x\\plate-a.gcode",
    ] {
        assert_eq!(
            decide(&fresh, &report(Complete, Some(name), None)),
            Decision::AwaitVerify,
            "{name}"
        );
    }
    assert_eq!(
        decide(&fresh, &report(Complete, Some("other.gcode"), None)),
        Decision::Noop,
        "another file finishing is not this send"
    );
    assert_eq!(
        decide(&fresh, &report(Complete, None, None)),
        Decision::Noop,
        "a cleared name counts only for a print seen running"
    );
    for seen in [json!({"saw_active": true}), json!({"started": true})] {
        assert_eq!(
            decide(&link(seen.clone()), &report(Complete, Some("  "), None)),
            Decision::AwaitVerify,
            "{seen}"
        );
    }
}

#[test]
fn gre_1897_errors_and_cancels_fail_the_link_and_other_states_wait() {
    use HostState::*;
    let fresh = link(json!({}));
    let seen = link(json!({"saw_active": true, "last_progress": 40}));
    let near_done = link(json!({"saw_active": true, "last_progress": 99}));
    let cases = [
        (
            &fresh,
            report(Error, Some("plate-a.gcode"), None),
            Decision::HostFailed("failed"),
        ),
        (
            &fresh,
            report(Error, Some("other.gcode"), None),
            Decision::Noop,
        ),
        (
            &seen,
            report(Error, None, None),
            Decision::HostFailed("failed"),
        ),
        (&fresh, report(Idle, None, None), Decision::Noop),
        (
            &seen,
            report(Idle, None, None),
            Decision::HostFailed("cancelled"),
        ),
        (&near_done, report(Idle, None, None), Decision::AwaitVerify),
        (&seen, report(Offline, None, None), Decision::Noop),
        (
            &seen,
            report(Unknown, Some("plate-a.gcode"), None),
            Decision::Noop,
        ),
        (
            &fresh,
            report(Printing, Some("other.gcode"), Some(5.0)),
            Decision::Noop,
        ),
        (
            &fresh,
            report(Printing, Some("plate-a.gcode"), Some(5.0)),
            Decision::MarkActive(Some(5.0)),
        ),
        (
            &fresh,
            report(Paused, Some("plate-a.gcode"), Some(f64::NAN)),
            Decision::MarkActive(None),
        ),
    ];
    for (link, report, expected) in cases {
        assert_eq!(decide(link, &report), expected, "{report:?}");
    }
}

#[test]
fn gre_1897_only_watching_links_move() {
    for state in ["awaiting_verify", "verified", "host_failed", "dismissed"] {
        let done = link(json!({ "state": state }));
        assert_eq!(
            decide(
                &done,
                &report(HostState::Complete, Some("plate-a.gcode"), None)
            ),
            Decision::Noop,
            "{state}"
        );
    }
}

#[test]
fn gre_1897_a_link_is_stale_after_48_hours_unless_its_print_ran() {
    let at = |created: &str, seen: bool| {
        stale(
            &link(json!({"created_at": created, "saw_active": seen})),
            now(),
        )
    };
    assert!(at("2026-11-19T11:59:59.000Z", false));
    assert!(!at("2026-11-19T12:00:00.000Z", false), "exactly 48 hours");
    assert!(!at("2026-11-19T11:00:00.000Z", true), "its print ran");
    assert!(
        !at("not a time", false),
        "an unreadable time is never stale"
    );
}
