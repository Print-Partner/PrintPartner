use super::*;
use serde_json::json;

fn log() -> Connection {
    let connection = Connection::open_in_memory().unwrap();
    connection
        .execute_batch(include_str!("schema.sql"))
        .unwrap();
    connection
}

/// A fresh log, its feed, and the position a client starts from.
fn opened() -> (Connection, Feed, Cursor) {
    let mut connection = log();
    let feed = Feed::new(0);
    let (_, start) = events(read_at(&mut connection, &feed, "default", None));
    (connection, feed, start)
}

fn stored(connection: &Connection) -> i64 {
    connection
        .query_row("SELECT count(*) FROM domain_events", [], |row| row.get(0))
        .unwrap()
}

fn change(id: i64) -> Change {
    Change {
        kind: PROGRESS_CHANGED,
        aggregate_type: "build",
        aggregate_id: id.to_string(),
        aggregate_version: None,
        payload: json!({ "part_id": id }),
    }
}

fn commit(connection: &mut Connection, tenant: &str, id: i64) {
    let tx = connection.transaction().unwrap();
    append(&tx, tenant, change(id)).unwrap();
    tx.commit().unwrap();
}

fn read_at(connection: &mut Connection, feed: &Feed, tenant: &str, after: Option<&Cursor>) -> Read {
    let tx = connection.transaction().unwrap();
    let result = read(&tx, feed, tenant, after).unwrap();
    tx.commit().unwrap();
    result
}

fn events(result: Read) -> (Vec<String>, Cursor) {
    match result {
        Read::Events { events, position } => (
            events.into_iter().map(|event| event.aggregate_id).collect(),
            position,
        ),
        other => panic!("expected events, got {other:?}"),
    }
}

fn resync(result: Read) -> ResyncReason {
    match result {
        Read::Resync { reason, .. } => reason,
        other => panic!("expected a resync, got {other:?}"),
    }
}

fn at(position: &Cursor, seq: i64) -> Cursor {
    Cursor::parse(&format!("{}.{seq}", position.generation)).unwrap()
}

#[test]
fn domain_events_cursor_round_trips_and_refuses_malformed_text() {
    let generation = "0123456789abcdef0123456789abcdef";
    for text in [format!("{generation}.0"), format!("{generation}.42")] {
        assert_eq!(Cursor::parse(&text).unwrap().to_string(), text);
    }
    for text in [
        String::new(),
        "42".to_owned(),
        format!("{generation}."),
        format!("{generation}.042"),
        format!("{generation}.-1"),
        format!("{generation}.1.2"),
        format!("{generation}.99999999999999999999"),
        format!("{}.1", generation.to_uppercase()),
        format!("{}.1", &generation[1..]),
    ] {
        assert_eq!(Cursor::parse(&text), None, "{text}");
    }
}

#[test]
fn domain_events_rolled_back_change_leaves_no_event() {
    let (mut connection, feed, start) = opened();
    let tx = connection.transaction().unwrap();
    append(&tx, "default", change(1)).unwrap();
    tx.rollback().unwrap();
    assert_eq!(
        events(read_at(&mut connection, &feed, "default", Some(&start))),
        (Vec::new(), start.clone())
    );
    commit(&mut connection, "default", 2);
    let (ids, position) = events(read_at(&mut connection, &feed, "default", Some(&start)));
    assert_eq!(ids, ["2"]);
    assert_eq!(position, at(&start, 1), "a rollback allocates no sequence");
}

#[test]
fn domain_events_replay_returns_missed_events_in_commit_order() {
    let mut connection = log();
    let feed = Feed::new(0);
    let (none, start) = events(read_at(&mut connection, &feed, "default", None));
    assert!(none.is_empty());
    for id in [7, 3, 9] {
        commit(&mut connection, "default", id);
    }
    let (ids, head) = events(read_at(&mut connection, &feed, "default", Some(&start)));
    assert_eq!(ids, ["7", "3", "9"]);
    assert_eq!(head, at(&start, 3));
    let (ids, _) = events(read_at(
        &mut connection,
        &feed,
        "default",
        Some(&at(&start, 2)),
    ));
    assert_eq!(ids, ["9"]);
    assert_eq!(
        events(read_at(&mut connection, &feed, "default", Some(&head))),
        (Vec::new(), head.clone())
    );
}

#[test]
fn domain_events_read_pages_at_the_limit() {
    let (mut connection, feed, start) = opened();
    for id in 0..(READ_LIMIT as i64 + 5) {
        commit(&mut connection, "default", id);
    }
    let (first, position) = events(read_at(&mut connection, &feed, "default", Some(&start)));
    assert_eq!(first.len(), READ_LIMIT);
    assert_eq!(position, at(&start, READ_LIMIT as i64));
    let (rest, head) = events(read_at(&mut connection, &feed, "default", Some(&position)));
    assert_eq!(rest.len(), 5);
    assert_eq!(head, at(&start, READ_LIMIT as i64 + 5));
}

#[test]
fn domain_events_cursor_past_retention_requires_resync() {
    let (mut connection, feed, start) = opened();
    for id in 0..(RETAINED_EVENTS + 3) {
        commit(&mut connection, "default", id);
    }
    assert_eq!(stored(&connection), RETAINED_EVENTS);
    for seq in [0, 2] {
        assert_eq!(
            resync(read_at(
                &mut connection,
                &feed,
                "default",
                Some(&at(&start, seq))
            )),
            ResyncReason::Expired
        );
    }
    let (ids, _) = events(read_at(
        &mut connection,
        &feed,
        "default",
        Some(&at(&start, 3)),
    ));
    assert_eq!(ids.len(), READ_LIMIT);
    assert_eq!(ids[0], "3");
}

#[test]
fn domain_events_other_generation_or_future_cursor_requires_resync() {
    let (mut connection, feed, start) = opened();
    commit(&mut connection, "default", 1);
    assert_eq!(
        resync(read_at(
            &mut connection,
            &feed,
            "default",
            Some(&at(&start, 2))
        )),
        ResyncReason::AheadOfLog
    );
    let reopened = Feed::open(&connection).unwrap();
    assert_eq!(
        resync(read_at(&mut connection, &reopened, "default", Some(&start))),
        ResyncReason::GenerationChanged
    );
    let (_, position) = events(read_at(&mut connection, &reopened, "default", None));
    assert_ne!(position.generation, start.generation);
    assert_eq!(position.seq, 1);
    assert_eq!(
        resync(read_at(
            &mut connection,
            &reopened,
            "default",
            Some(&at(&position, 0))
        )),
        ResyncReason::Expired,
        "events stored before the open are not replayed"
    );
    commit(&mut connection, "default", 2);
    let (ids, _) = events(read_at(
        &mut connection,
        &reopened,
        "default",
        Some(&position),
    ));
    assert_eq!(ids, ["2"]);
}

#[test]
fn domain_events_unknown_version_requires_resync_instead_of_skipping() {
    for change in [
        "UPDATE domain_events SET schema_version=2",
        "UPDATE domain_events SET event_type='checkoff.future'",
    ] {
        let (mut connection, feed, start) = opened();
        commit(&mut connection, "default", 1);
        commit(&mut connection, "default", 2);
        connection
            .execute(&format!("{change} WHERE seq=2"), [])
            .unwrap();
        match read_at(&mut connection, &feed, "default", Some(&start)) {
            Read::Resync { reason, position } => {
                assert_eq!(reason, ResyncReason::UnknownVersion, "{change}");
                assert_eq!(position, at(&start, 2), "{change}");
            }
            other => panic!("{change}: {other:?}"),
        }
        let (ids, _) = events(read_at(
            &mut connection,
            &feed,
            "default",
            Some(&at(&start, 2)),
        ));
        assert!(ids.is_empty());
    }
}

#[test]
fn domain_events_reads_only_the_callers_tenant() {
    let mut connection = log();
    let feed = Feed::new(0);
    let (_, start) = events(read_at(&mut connection, &feed, "a", None));
    commit(&mut connection, "a", 1);
    commit(&mut connection, "b", 2);
    commit(&mut connection, "a", 3);
    let (ids, head) = events(read_at(&mut connection, &feed, "a", Some(&start)));
    assert_eq!(ids, ["1", "3"]);
    assert_eq!(head, at(&start, 3));
    let (ids, _) = events(read_at(&mut connection, &feed, "b", Some(&start)));
    assert_eq!(ids, ["2"]);
}

#[test]
fn domain_events_append_refuses_oversized_payload_and_unknown_kind() {
    let mut connection = log();
    let tx = connection.transaction().unwrap();
    let mut large = change(1);
    large.payload = json!({ "text": "x".repeat(MAX_PAYLOAD_BYTES) });
    assert!(append(&tx, "default", large).is_err());
    let mut unknown = change(1);
    unknown.kind = EventKind {
        name: "checkoff.progress_changed",
        version: 2,
    };
    assert!(append(&tx, "default", unknown).is_err());
    assert_eq!(stored(&tx), 0);
}

#[test]
fn domain_events_feed_wakes_on_new_events_and_authority_changes_only() {
    let mut connection = log();
    let feed = Feed::new(head(&connection).unwrap());
    let woken = Arc::new(AtomicI64::new(0));
    let counter = woken.clone();
    let waker: Waker = Arc::new(move || {
        counter.fetch_add(1, Ordering::SeqCst);
    });
    feed.subscribe(&waker).unwrap();
    feed.after_write(&connection, false);
    assert_eq!(woken.load(Ordering::SeqCst), 0, "no change, no wake");
    commit(&mut connection, "default", 1);
    feed.after_write(&connection, false);
    assert_eq!(woken.load(Ordering::SeqCst), 1);
    feed.after_write(&connection, false);
    assert_eq!(woken.load(Ordering::SeqCst), 1);
    feed.after_write(&connection, true);
    assert_eq!(
        woken.load(Ordering::SeqCst),
        2,
        "auth changes recheck streams"
    );
    drop(waker);
    feed.after_write(&connection, true);
    assert_eq!(
        woken.load(Ordering::SeqCst),
        2,
        "a dropped waker unsubscribes"
    );
    let held: Vec<Waker> = (0..MAX_SUBSCRIPTIONS)
        .map(|_| Arc::new(|| {}) as Waker)
        .collect();
    for waker in &held {
        feed.subscribe(waker).unwrap();
    }
    assert!(feed.subscribe(&(Arc::new(|| {}) as Waker)).is_err());
}

#[test]
fn domain_events_panicking_waker_is_unsubscribed_and_others_still_wake() {
    let feed = Feed::new(0);
    let woken = Arc::new(AtomicI64::new(0));
    let counter = woken.clone();
    let panics: Waker = Arc::new(|| panic!("waker probe"));
    let counts: Waker = Arc::new(move || {
        counter.fetch_add(1, Ordering::SeqCst);
    });
    feed.subscribe(&panics).unwrap();
    feed.subscribe(&counts).unwrap();
    feed.wake();
    feed.wake();
    assert_eq!(woken.load(Ordering::SeqCst), 2);
    assert_eq!(feed.wakers.lock().unwrap().len(), 1);
}
