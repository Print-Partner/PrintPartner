use super::*;
use pp_storage::build_graph::PlanFreshness;
use serde_json::json;

fn build(id: i64, progress: AcceptedProgress, archived: bool) -> ProfileSummary {
    ProfileSummary {
        id,
        name: format!("B{id}"),
        order_number: None,
        special_request: None,
        part_count: 3,
        accepted_progress: progress,
        build_stale: false,
        freshness: PlanFreshness::Unavailable { code: "x".into() },
        archived_at: archived.then(|| "2026-11-01T00:00:00.000Z".into()),
        last_used_at: None,
        customer: None,
        due_date: None,
    }
}

fn args(value: Value) -> Map<String, Value> {
    value.as_object().unwrap().clone()
}

#[test]
fn hours_default_to_eight_and_must_be_a_number_in_range() {
    assert_eq!(Window::parse(&args(json!({}))).unwrap().hours, 8.0);
    assert_eq!(
        Window::parse(&args(json!({ "hours": null })))
            .unwrap()
            .hours,
        8.0
    );
    assert_eq!(
        Window::parse(&args(json!({ "hours": 24 }))).unwrap().hours,
        24.0
    );
    assert_eq!(
        Window::parse(&args(json!({ "hours": 0.5 }))).unwrap().hours,
        0.5
    );
    assert!(Window::parse(&args(json!({ "hours": 2160 }))).is_ok());
    for bad in [
        json!(0),
        json!(-1),
        json!(2160.5),
        json!("8"),
        json!(true),
        json!([8]),
    ] {
        let refused = Window::parse(&args(json!({ "hours": bad }))).unwrap_err();
        assert_eq!(refused.code, Code::InvalidHours, "{bad}");
    }
}

#[test]
fn stats_count_the_window_and_list_active_plans() {
    let now = SystemTime::UNIX_EPOCH + Duration::from_secs(1_800_000_000);
    let inputs = Inputs {
        jobs: vec![PrinterActivity {
            printer_id: Some("p-a".into()),
            sent: 2,
            completed: 1,
            failed: 1,
            filament_consumed_g: 119.5,
        }],
        results: &[],
        unmatched: &[],
        fleet: Fleet::new([("p-a".to_owned(), "Printer A".to_owned(), None)]),
        builds: Ok(vec![
            build(
                1,
                AcceptedProgress::Ready {
                    total_units: 10,
                    remaining_units: 4,
                },
                false,
            ),
            build(2, AcceptedProgress::Empty, false),
            build(3, AcceptedProgress::Empty, true),
        ]),
    };
    let window = Window::parse(&args(json!({}))).unwrap();
    let stats = serde_json::to_value(stats(window, now, inputs)).unwrap();
    assert_eq!(stats["window_hours"], json!(8));
    assert_eq!(
        stats["since"],
        iso_time(now - Duration::from_secs(8 * 3600))
    );
    assert_eq!(
        (
            &stats["plates_sent"],
            &stats["plates_completed"],
            &stats["plates_failed"]
        ),
        (&json!(2), &json!(1), &json!(1))
    );
    assert_eq!(stats["completion_rate"], json!(0.5));
    assert_eq!(stats["filament_consumed_g"], json!(119.5));
    assert_eq!(stats["empty_window"], json!(false));
    assert_eq!(stats["by_printer"][0]["printer_name"], "Printer A");
    assert_eq!(
        stats["active_plans"],
        json!({ "kind": "listed", "plans": [
            { "plan_id": 1, "plan_name": "B1", "part_count": 3, "accepted_progress":
              { "kind": "ready", "total_units": 10, "remaining_units": 4, "printed_units": 6 } },
            { "plan_id": 2, "plan_name": "B2", "part_count": 3, "accepted_progress": { "kind": "empty" } },
        ] })
    );
}

#[test]
fn an_unreadable_build_list_keeps_the_counts() {
    let inputs = Inputs {
        jobs: vec![PrinterActivity {
            printer_id: Some("p-a".into()),
            sent: 3,
            completed: 2,
            failed: 0,
            filament_consumed_g: 40.0,
        }],
        results: &[],
        unmatched: &[],
        fleet: Fleet::default(),
        builds: Err(PlansUnavailable::TooManyBuilds),
    };
    let window = Window::parse(&args(json!({}))).unwrap();
    let stats = serde_json::to_value(stats(window, SystemTime::now(), inputs)).unwrap();
    assert_eq!(stats["plates_sent"], json!(3));
    assert_eq!(stats["filament_consumed_g"], json!(40));
    assert_eq!(
        stats["active_plans"],
        json!({ "kind": "unavailable", "reason": "too_many_builds" })
    );
}

#[test]
fn an_empty_window_counts_zero() {
    let inputs = Inputs {
        jobs: Vec::new(),
        results: &[],
        unmatched: &[],
        fleet: Fleet::default(),
        builds: Ok(Vec::new()),
    };
    let window = Window::parse(&args(json!({ "hours": 1.5 }))).unwrap();
    let stats = serde_json::to_value(stats(window, SystemTime::now(), inputs)).unwrap();
    assert_eq!(stats["window_hours"], json!(1.5));
    assert_eq!(stats["empty_window"], json!(true));
    assert_eq!(stats["plates_sent"], json!(0));
    assert_eq!(stats["completion_rate"], Value::Null);
    assert_eq!(stats["prints_seen_on_printers"]["total"], json!(0));
}

#[test]
fn completion_rate_rounds_to_three_places() {
    assert_eq!(completion_rate(0, 0), None);
    assert_eq!(completion_rate(2, 1), Some(0.667));
    assert_eq!(completion_rate(1, 0), Some(1.0));
}

#[test]
fn a_report_resolves_to_the_only_fleet_printer_on_its_integration() {
    let fleet = Fleet::new([
        (
            "p-a".to_owned(),
            "Printer A".to_owned(),
            Some("moon-a".to_owned()),
        ),
        (
            "p-b".to_owned(),
            "Printer B".to_owned(),
            Some("shared".to_owned()),
        ),
        (
            "p-c".to_owned(),
            "Printer C".to_owned(),
            Some("shared".to_owned()),
        ),
    ]);
    let named = |id: &str, name: &str| (id.to_owned(), Some(name.to_owned()));
    assert_eq!(fleet.resolve("p-b", "shared"), named("p-b", "Printer B"));
    assert_eq!(
        fleet.resolve("default", " moon-a "),
        named("p-a", "Printer A")
    );
    assert_eq!(
        fleet.resolve("default", "shared"),
        ("default".to_owned(), None)
    );
    assert_eq!(
        fleet.resolve("default", "gone"),
        ("default".to_owned(), None)
    );
}
