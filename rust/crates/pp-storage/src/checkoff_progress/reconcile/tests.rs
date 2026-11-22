use super::*;
use Decision::*;
use HostState::*;
use serde_json::json;

const NOW: &str = "2026-11-21T12:00:00.000Z";

fn link(fields: Value) -> Link {
    let mut raw = json!({
        "id": "l1", "profile_id": 1, "integration_id": "moon-1", "printer_id": "p1",
        "filename": "plate-a.gcode", "remote_path": "pp/plate-a.gcode",
        "state": "watching", "created_at": "2026-11-21T10:00:00.000Z",
    });
    for (key, value) in fields.as_object().unwrap() {
        raw[key] = value.clone();
    }
    links::parse(&raw, NOW).unwrap()
}

/// Host `state` at 5% reporting `file` (blank for none).
fn report(state: HostState, file: &str) -> HostReport {
    let filename = Some(file.to_owned()).filter(|f| !f.is_empty());
    let progress = Some(5.0);
    HostReport {
        state,
        filename,
        progress,
        cancelled: false,
    }
}

fn decided(link: &Link, state: HostState, file: &str) -> Decision {
    decide(link, &report(state, file), &Taken::default())
}

#[test]
fn gre_1897_a_finish_of_the_sent_file_awaits_verify() {
    let fresh = link(json!({"started": true}));
    for name in [
        "plate-a.gcode",
        " PP/Plate-A.GCODE ",
        "C:\\x\\plate-a.gcode",
    ] {
        assert_eq!(decided(&fresh, Complete, name), AwaitVerify, "{name}");
    }
    assert_eq!(
        decided(&fresh, Complete, "other.gcode"),
        Noop,
        "another file"
    );
    for seen in [json!({"saw_active": true}), json!({"started": true})] {
        assert_eq!(
            decided(&link(seen.clone()), Complete, "  "),
            AwaitVerify,
            "{seen}"
        );
    }
    // An upload never seen printing and not started by its send never ran.
    let upload = link(json!({}));
    for (state, file) in [
        (Complete, "plate-a.gcode"),
        (Complete, ""),
        (Error, "plate-a.gcode"),
    ] {
        assert_eq!(decided(&upload, state, file), Noop, "{state:?} {file}");
    }
}

#[test]
fn gre_1897_errors_and_cancels_fail_the_link_and_other_states_wait() {
    let fresh = link(json!({"started": true}));
    let seen = link(json!({"saw_active": true, "last_progress": 40}));
    let near_done = link(json!({"saw_active": true, "last_progress": 99}));
    assert_eq!(
        decided(&fresh, Error, "plate-a.gcode"),
        HostFailed("failed")
    );
    assert_eq!(decided(&fresh, Error, "other.gcode"), Noop);
    assert_eq!(decided(&seen, Error, ""), HostFailed("failed"));
    assert_eq!(decided(&fresh, Idle, ""), Noop);
    assert_eq!(decided(&seen, Idle, ""), HostFailed("cancelled"));
    assert_eq!(
        decided(&near_done, Idle, ""),
        AwaitVerify,
        "a missed brief finish"
    );
    assert_eq!(decided(&seen, Offline, ""), Noop);
    assert_eq!(decided(&seen, Unknown, "plate-a.gcode"), Noop);
    assert_eq!(decided(&fresh, Printing, "other.gcode"), Noop);
    assert_eq!(
        decided(&fresh, Printing, "plate-a.gcode"),
        MarkActive(Some(5.0))
    );
    let cancel = HostReport {
        cancelled: true,
        ..report(Idle, "")
    };
    assert_eq!(
        decide(&near_done, &cancel, &Taken::default()),
        HostFailed("cancelled"),
        "explicit"
    );
    let paused = HostReport {
        progress: Some(f64::NAN),
        ..report(Paused, "plate-a.gcode")
    };
    assert_eq!(decide(&fresh, &paused, &Taken::default()), MarkActive(None));
}

#[test]
fn gre_1897_only_watching_links_move() {
    for state in ["awaiting_verify", "verified", "host_failed", "dismissed"] {
        let done = link(json!({ "state": state }));
        assert_eq!(decided(&done, Complete, "plate-a.gcode"), Noop, "{state}");
    }
}

#[test]
fn gre_1897_a_link_is_stale_after_48_hours_unless_its_print_ran() {
    let now = OffsetDateTime::parse(NOW, &Rfc3339).unwrap();
    let at = |created: &str, seen: bool| {
        stale(
            &link(json!({"created_at": created, "saw_active": seen})),
            now,
        )
    };
    assert!(at("2026-11-19T11:59:59.000Z", false));
    assert!(!at("2026-11-19T12:00:00.000Z", false), "exactly 48 hours");
    assert!(!at("2026-11-19T11:00:00.000Z", true), "its print ran");
    let started = link(json!({"created_at": "2026-11-19T11:00:00.000Z", "started": true}));
    assert!(!stale(&started, now), "its send started the print");
    assert!(
        !at("not a time", false),
        "an unreadable time is never stale"
    );
}

#[test]
fn gre_1897_a_status_moves_one_send_of_a_shared_file_name() {
    let picked = |links: Vec<Value>| {
        let links: Vec<Link> = links.into_iter().map(link).collect();
        let moving = (0..links.len()).map(|i| (i, AwaitVerify)).collect();
        one_send(&links, moving)
            .into_iter()
            .map(|(i, _)| i)
            .collect::<Vec<_>>()
    };
    let (started, uploaded) = (json!({"started": true}), json!({"started": false}));
    let seen = json!({"saw_active": true});
    assert_eq!(picked(vec![uploaded.clone()]), [0], "one send");
    assert_eq!(
        picked(vec![uploaded.clone(), started.clone()]),
        [1],
        "the started send"
    );
    assert_eq!(
        picked(vec![started.clone(), seen]),
        [1],
        "the send seen printing"
    );
    assert!(
        picked(vec![started.clone(), started]).is_empty(),
        "ambiguous"
    );
    assert!(
        picked(vec![uploaded.clone(), uploaded]).is_empty(),
        "ambiguous"
    );
}

/// Reconciles each of `reports` in turn against `links` stored for host
/// `moon-1`, and returns every stored link's `(id, state)`.
fn replayed(links: Vec<Value>, reports: &[HostReport]) -> Vec<(String, String)> {
    let mut connection = rusqlite::Connection::open_in_memory().unwrap();
    connection
        .execute_batch(
            "CREATE TABLE app_settings (tenant_id TEXT, key TEXT, value TEXT, PRIMARY KEY (tenant_id, key))",
        )
        .unwrap();
    connection
        .execute(
            "INSERT INTO app_settings VALUES ('default', 'printer.checkoff_links', ?1)",
            [Value::from(links).to_string()],
        )
        .unwrap();
    let now = OffsetDateTime::parse(NOW, &Rfc3339).unwrap();
    for (step, report) in reports.iter().enumerate() {
        let tx = connection.transaction().unwrap();
        reconcile(
            &tx,
            "default",
            "moon-1",
            report,
            now + time::Duration::minutes(step as i64),
        )
        .unwrap();
        tx.commit().unwrap();
    }
    let tx = connection.transaction().unwrap();
    let mut states: Vec<(String, String)> = links::load(&tx, "default", NOW)
        .unwrap()
        .iter()
        .map(|link| {
            (
                link["id"].as_str().unwrap().into(),
                link["state"].as_str().unwrap().into(),
            )
        })
        .collect();
    states.sort();
    states
}

#[test]
fn gre_1897_a_finish_replayed_after_offline_moves_no_second_started_link() {
    let send = |id: &str, saw_active: bool| {
        let mut raw = link(json!({"id": id, "started": true, "saw_active": saw_active}));
        raw.insert("units".into(), json!([{"part_id": 1, "unit_index": 0}]));
        Value::Object(raw)
    };
    let links = vec![send("older", false), send("current", true)];
    let finish = report(Complete, "plate-a.gcode");
    let states = replayed(links, &[finish.clone(), report(Offline, ""), finish]);
    assert_eq!(
        states,
        [
            ("current".to_owned(), "awaiting_verify".to_owned()),
            ("older".to_owned(), "watching".to_owned()),
        ],
        "the replayed finish is the one `current` consumed"
    );
}

#[test]
fn a_completed_link_filters_only_plausible_replays() {
    const FILE: &str = "plate-a.gcode";
    const COMPLETED: &str = "2026-11-21T11:00:00.000Z";
    const LATER: &str = "2026-11-21T11:30:00.000Z";
    const AWAITING: &str = "awaiting_verify";
    const WATCHING: &str = "watching";

    let send = |id: &str, fields: Value| {
        let mut raw = json!({
            "id": id, "profile_id": 1, "integration_id": "moon-1",
            "filename": FILE, "state": WATCHING, "started": true,
            "created_at": "2026-11-21T10:00:00.000Z",
            "units": [{"part_id": 1, "unit_index": 0}],
        });
        for (key, value) in fields.as_object().unwrap() {
            raw[key] = value.clone();
        }
        raw
    };
    let took = |fields: Value| {
        let mut raw = json!({
            "state": AWAITING, "host_outcome": "success", "completed_at": COMPLETED,
        });
        for (key, value) in fields.as_object().unwrap() {
            raw[key] = value.clone();
        }
        send("taken", raw)
    };
    let finish = report(Complete, FILE);
    let blank = report(Complete, " ");
    let print = report(Printing, FILE);
    let cases = [
        (
            "restart",
            json!({}),
            json!({}),
            vec![finish.clone(), finish.clone()],
            WATCHING,
            AWAITING,
        ),
        (
            "reconnect with blank filename",
            json!({}),
            json!({}),
            vec![report(Offline, ""), blank.clone()],
            WATCHING,
            AWAITING,
        ),
        (
            "already verified",
            json!({"state": "verified"}),
            json!({}),
            vec![finish.clone()],
            WATCHING,
            "verified",
        ),
        (
            "new send",
            json!({}),
            json!({"created_at": LATER}),
            vec![finish.clone()],
            AWAITING,
            AWAITING,
        ),
        (
            "later printing observed",
            json!({}),
            json!({}),
            vec![print, finish.clone()],
            AWAITING,
            AWAITING,
        ),
        (
            "missing completion time",
            json!({"completed_at": null}),
            json!({}),
            vec![finish.clone()],
            AWAITING,
            AWAITING,
        ),
        (
            "different host",
            json!({"integration_id": "moon-2"}),
            json!({}),
            vec![finish.clone()],
            AWAITING,
            AWAITING,
        ),
        (
            "different file",
            json!({"filename": "other.gcode"}),
            json!({}),
            vec![finish.clone()],
            AWAITING,
            AWAITING,
        ),
        (
            "blank filename on the same host",
            json!({"filename": "other.gcode"}),
            json!({}),
            vec![blank],
            WATCHING,
            AWAITING,
        ),
        (
            "remote path matches",
            json!({"filename": "other.gcode", "remote_path": "PP/Plate-A.GCODE"}),
            json!({}),
            vec![finish.clone()],
            WATCHING,
            AWAITING,
        ),
        (
            "failed print",
            json!({"state": "host_failed", "host_outcome": "failed"}),
            json!({}),
            vec![finish.clone()],
            AWAITING,
            "host_failed",
        ),
        (
            "equal timestamps",
            json!({}),
            json!({"created_at": COMPLETED}),
            vec![finish.clone()],
            AWAITING,
            AWAITING,
        ),
        (
            "unreadable timestamp",
            json!({}),
            json!({"created_at": "not a time"}),
            vec![finish.clone()],
            AWAITING,
            AWAITING,
        ),
        (
            "idle after near-done progress",
            json!({}),
            json!({"saw_active": true, "last_progress": 99}),
            vec![report(Idle, "")],
            AWAITING,
            AWAITING,
        ),
    ];
    for (case, taken, candidate, reports, candidate_state, taken_state) in cases {
        assert_eq!(
            replayed(vec![took(taken), send("candidate", candidate)], &reports),
            [
                ("candidate".into(), candidate_state.into()),
                ("taken".into(), taken_state.into()),
            ],
            "{case}"
        );
    }
    assert_eq!(
        replayed(
            vec![
                took(json!({})),
                send("older", json!({})),
                send("newer", json!({"created_at": LATER})),
            ],
            &[finish],
        ),
        [
            ("newer".into(), AWAITING.into()),
            ("older".into(), WATCHING.into()),
            ("taken".into(), AWAITING.into()),
        ],
        "a replay candidate cannot make a new send ambiguous"
    );
}
