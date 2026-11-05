use super::*;
use pp_storage::{
    printer_display_state::{BedClearState, PrinterExecutionState, PrinterStateView},
    read_model::farm::StreamState,
};

fn printer(id: &str, state: StreamState, heard: &str) -> StreamPrinter {
    StreamPrinter {
        id: id.into(),
        name: format!("Printer {id}"),
        state,
        display: PrinterStateView::new(PrinterExecutionState::Idle, BedClearState::Unknown),
        reason: None,
        result_id: None,
        progress_percent: None,
        bed_needs_clearing: None,
        filaments: vec![],
        last_heard_at: Some(heard.into()),
    }
}

fn parse(frames: &[Utf8Bytes]) -> Vec<Value> {
    frames
        .iter()
        .map(|text| serde_json::from_str(text.as_str()).unwrap())
        .collect()
}

#[test]
fn printer_stream_sends_a_snapshot_then_every_transition_in_order() {
    let stream = PrinterStream::new(HEARTBEAT);
    stream.publish(vec![printer("a", StreamState::Printing, "t0")]);
    let (first, _, last) = stream.open(None);
    let first = parse(&first);
    assert_eq!(first.len(), 1);
    assert_eq!(first[0]["type"], "snapshot");
    assert_eq!(first[0]["seq"], 1);
    assert_eq!(first[0]["heartbeat_seconds"], 15);
    assert_eq!(first[0]["printers"][0]["state"], "printing");
    assert_eq!(last, 1);
    let id = first[0]["stream"].as_str().unwrap().to_owned();
    assert_eq!(id.len(), 32);

    stream.publish(vec![printer("a", StreamState::Printing, "t1")]);
    stream.publish(vec![printer("a", StreamState::Error, "t2")]);
    stream.publish(vec![printer("a", StreamState::Idle, "t3")]);
    stream.publish(vec![]);
    let (events, _, last) = stream.open(Some((id.clone(), 1)));
    let events = parse(&events);
    let kinds: Vec<_> = events
        .iter()
        .map(|event| {
            (
                event["seq"].as_u64().unwrap(),
                event["type"].as_str().unwrap(),
            )
        })
        .collect();
    assert_eq!(
        kinds,
        [(2, "printer"), (3, "printer"), (4, "removed")],
        "a new last-heard time alone is not an event"
    );
    assert_eq!(events[0]["printer"]["state"], "error");
    assert_eq!(events[1]["printer"]["state"], "idle");
    assert_eq!(events[2]["id"], "a");
    assert_eq!(last, 4);
    assert!(stream.open(Some((id, 4))).0.is_empty(), "nothing missed");
}

#[test]
fn printer_stream_resume_falls_back_to_a_snapshot() {
    let stream = PrinterStream::new(HEARTBEAT);
    let (first, _, _) = stream.open(None);
    let id = parse(&first)[0]["stream"].as_str().unwrap().to_owned();
    for step in 0..=RETAINED {
        let state = if step % 2 == 0 {
            StreamState::Printing
        } else {
            StreamState::Paused
        };
        stream.publish(vec![printer("a", state, "t")]);
    }
    let snapshot = |resume| parse(&stream.open(resume).0)[0]["type"].clone();
    assert_eq!(snapshot(Some((id.clone(), 0))), "snapshot", "too old");
    assert_eq!(
        snapshot(Some((id.clone(), 9999))),
        "snapshot",
        "ahead of the stream"
    );
    assert_eq!(
        snapshot(Some(("other".into(), 5))),
        "snapshot",
        "another stream"
    );
    let resumed = parse(&stream.open(Some((id, RETAINED as u64))).0);
    assert_eq!(resumed.len(), 1);
    assert_eq!(resumed[0]["seq"], RETAINED as u64 + 1);
}

#[test]
fn printer_stream_resume_query_needs_both_parts() {
    assert_eq!(resume(None), Ok(None));
    assert_eq!(resume(Some("other=1")), Ok(None));
    assert_eq!(
        resume(Some("stream=ab%20c&since=7")),
        Ok(Some(("ab c".into(), 7)))
    );
    for bad in [
        "stream=a",
        "since=1",
        "stream=a&since=x",
        "stream=a&since=1&since=2",
    ] {
        assert!(resume(Some(bad)).is_err(), "{bad}");
    }
}

#[test]
fn printer_stream_heartbeat_names_the_last_sequence() {
    let stream = PrinterStream::new(HEARTBEAT);
    let beat: Value = serde_json::from_str(stream.heartbeat(3).as_str()).unwrap();
    assert_eq!(beat["type"], "heartbeat");
    assert_eq!(beat["seq"], 3);
    assert!(beat["at"].as_str().unwrap().ends_with('Z'));
}
