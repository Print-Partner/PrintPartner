//! Replays the shared Moonraker status corpus that the Node adapter test also reads.
use pp_api_types::printer_status::PrinterHostStatus;
use pp_drivers::moonraker::recorded_status;
use serde::Deserialize;
use serde_json::Value;
use std::{collections::BTreeSet, path::Path};

const FIXTURES: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../../fixtures/drivers/moonraker"
);

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Case {
    name: String,
    base_url: String,
    fixture: Option<String>,
    body: Option<Value>,
    expected: Value,
    rust_rejects: Option<String>,
}

#[derive(Deserialize)]
struct Corpus {
    cases: Vec<Case>,
}

fn body(case: &Case) -> Vec<u8> {
    match (&case.fixture, &case.body) {
        (Some(file), None) => std::fs::read(Path::new(FIXTURES).join(file)).unwrap(),
        (None, Some(body)) => serde_json::to_vec(body).unwrap(),
        _ => panic!("{}: set exactly one of fixture or body", case.name),
    }
}

#[test]
fn ticket_t_4_watcher_proof_status_parity_corpus() {
    let corpus: Corpus = serde_json::from_slice(
        &std::fs::read(Path::new(FIXTURES).join("status-parity.json")).unwrap(),
    )
    .unwrap();
    assert!(corpus.cases.len() >= 21);
    let mut mismatches = Vec::new();
    for case in &corpus.cases {
        // The poller trims the base the same way before building the query URL.
        let base = case.base_url.trim().trim_end_matches('/');
        let actual = recorded_status(&body(case), base);
        if let Some(reason) = &case.rust_rejects {
            if actual.is_ok() {
                mismatches.push(format!("{}: expected rejection ({reason})", case.name));
            }
            continue;
        }
        // Round-trip the Node expectation through the Rust type so 60 and 60.0 compare
        // equal, and check no expected field was dropped on the way.
        let expected: PrinterHostStatus = serde_json::from_value(case.expected.clone()).unwrap();
        let expected = serde_json::to_value(&expected).unwrap();
        assert_eq!(
            expected
                .as_object()
                .unwrap()
                .keys()
                .collect::<BTreeSet<_>>(),
            case.expected
                .as_object()
                .unwrap()
                .keys()
                .collect::<BTreeSet<_>>(),
            "{}: expected fields outside PrinterHostStatus",
            case.name
        );
        match actual.map(|status| serde_json::to_value(status).unwrap()) {
            Ok(actual) if actual == expected => {}
            other => mismatches.push(format!("{}: {other:?} != {expected}", case.name)),
        }
    }
    assert!(mismatches.is_empty(), "{}", mismatches.join("\n"));
    println!("{} Moonraker status parity cases", corpus.cases.len());
}

/// GRE-1897: a cancel still reads `idle` with Node's fields, and is flagged
/// apart from a host back at rest, so no cancel can pass for a finish.
#[test]
fn gre_1897_a_cancelled_print_reads_idle_and_flags_the_cancel() {
    let read = |state: &str| {
        let stats = format!(r#"{{"state":"{state}","filename":"part.gcode"}}"#);
        let body = format!(r#"{{"result":{{"status":{{"print_stats":{stats}}}}}}}"#);
        recorded_status(body.as_bytes(), "http://printer.test").unwrap()
    };
    let cancelled = read("cancelled");
    let json = serde_json::to_value(&cancelled).unwrap();
    assert_eq!(
        (json["state"].as_str(), json["message"].as_str()),
        (Some("idle"), Some("Idle"))
    );
    assert!(
        cancelled.cancelled && json.get("cancelled").is_none(),
        "{json}"
    );
    for state in ["standby", "complete", "printing"] {
        assert!(!read(state).cancelled, "{state}");
    }
}
