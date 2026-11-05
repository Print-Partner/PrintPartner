# Domain events

GRE-1088 adds one bounded change feed so the replacement UI can invalidate
queries instead of polling. This page covers the storage half,
`pp_storage::domain_events`, compiled only with `desktop_rebuild`. The HTTP
stream that serves it is GRE-1162. Printer telemetry stays on `/ws/printers` and
job progress on the job WebSockets.

## What is recorded

A family appends an event inside the same SQLite transaction as the change it
describes. If that transaction rolls back, the event is gone too, and if the
event cannot be written, the change rolls back. One writer thread assigns every
sequence number, so cursor order is commit order.

| Type | Version | Written by | Aggregate | Payload |
| --- | --- | --- | --- | --- |
| `checkoff.progress_changed` | 1 | `checkoff_progress`: unit completion, assembly, and count import | `build`, the Build ID | `{"part_id": N}` for a unit change, `{"import": true}` for an import |
| `plan.revision_accepted` | 1 | `plan_publication::publish_in_transaction`, used by Accept and draft save-and-accept | `build`, the Build ID, version = new accepted Plan version | `{"revision_id": N}` |

An idempotent replay of an Accept (`Existing` or `AlreadyApplied`) writes no
event. Refused Checkoff requests write none.

Each event carries a cursor, type, schema version, aggregate type, ID and
version (`null` when the aggregate has none), a random 32-hex operation ID,
the commit time in Unix milliseconds, and a JSON payload of at most 2,048
bytes. Payloads hold IDs only, never names, file paths or secrets.

This is a change feed, not event sourcing. The tables stay the state; an event
tells a client which queries to reload.

## Schema 42

The 41 to 42 step installs `domain_events` and its `(tenant, seq)` index from
`domain_events/schema.sql`, validates them and stamps 42 in one transaction. A
database below 42 with these objects is refused by preflight, before the
opening copy or any write. The step only adds tables, so it writes no
`pre-schema42.db` copy of its own: an upgrade from 41 keeps the copy taken at
open, and an upgrade from an older version keeps the copies of its earlier
steps. That keeps the interrupted-upgrade bound in `backup_retention.rs`
(`LAST_COPIED_STEP`) at 41.

An open from 41 skips backup retention. It may be the restart of an upgrade
from 37 that stopped after step 41 committed, with copies 38 to 41 already on
disk. Its opening `pre-schema42.db` would be a fifth name, and keeping three
older copies would delete `pre-schema38.db`, the untouched schema-37 input.
The compile-time assert beside `LAST_COPIED_STEP` stops a schema 43 step
from landing without a run marker that restores retention for these opens.

## Cursors and reads

A cursor is `<generation>.<sequence>`: 32 lowercase hex digits, a dot, and a
decimal sequence. Every storage open draws a new generation in memory and
treats events stored before the open as expired, so a cursor from before a
restart, or from a restored copy, is refused instead of matching unrelated
events. Opening writes nothing. Replay therefore covers dropped connections
within one run; after a restart the client reloads its state.

`WriterOwner::domain_events()` and `domain_events_with_policy(policy)` return
`DomainEventsClient`.
`read(credential, after, ...)` authenticates the credential on the writer
queue (an auth failure is an `Err`) and returns only that tenant's events:

- `after = None`: no events and the newest position. A client reads this
  first, then loads its state, so any change after that position arrives as an
  event.
- `after = Some(cursor)`: up to 256 events after it, in commit order, and the
  position to continue from.
- `Resync { reason, position }` when the cursor is from another generation
  (`generation_changed`), older than the retained events (`expired`), past
  the newest event (`ahead_of_log`), or when an event after it has a type or
  version this build does not know (`unknown_version`). The client reloads
  its state and continues from `position`. An unknown event is never skipped
  silently.

## Bounds

| Bound | Value |
| --- | --- |
| Events kept for replay | 4,096 (`RETAINED_EVENTS`); older events are deleted in the transaction that appends a new one |
| Payload | 2,048 bytes (`MAX_PAYLOAD_BYTES`), checked in Rust and by a table CHECK |
| Events per read | 256 (`READ_LIMIT`) |
| Live subscriptions | 64 per storage owner (`MAX_SUBSCRIPTIONS`) |

## Live wake-ups

`subscribe(&waker)` registers a non-blocking callback, held weakly, so dropping
the caller's `Waker` unsubscribes. The writer thread calls it after an envelope
that moved the newest sequence, after every auth change so a stream can recheck
a revoked session at once, and when storage stops. Reads never wake anyone.
Writers never wait on readers: a wake is a callback that must only signal a
task. Subscribe before the first read so no commit between them is missed.

## Tests

```sh
cargo nextest run --locked --manifest-path rust/Cargo.toml --no-tests=fail \
  --features pp-storage/desktop_rebuild,pp-core/desktop_rebuild -E 'test(domain_events)'
cargo test --locked --manifest-path rust/Cargo.toml -p pp-storage --features desktop_rebuild \
  --lib plan_publication::tests::transaction::transaction_replays_node_outcomes_receipts_and_persisted_rows
```

Unit tests live in `pp-storage/src/domain_events/tests.rs`; the Checkoff,
restart and wake-up tests are in `pp-storage/tests/checkoff_progress.rs`.

| Item | Tests |
| --- | --- |
| EVT-01 same transaction | `domain_events_rolled_back_change_leaves_no_event`, `domain_events_refused_or_failed_checkoff_writes_no_event`, `domain_events_checkoff_commits_one_event_with_its_change`, and the publication parity test (one event per applied Accept, none for replays and refusals) |
| EVT-02 replay or resync | `domain_events_replay_returns_missed_events_in_commit_order`, `domain_events_read_pages_at_the_limit`, `domain_events_cursor_past_retention_requires_resync`, `domain_events_other_generation_or_future_cursor_requires_resync`, `domain_events_restart_starts_a_new_generation` |
| EVT-03 authentication (storage side) | `domain_events_unauthenticated_read_is_refused`, `domain_events_reads_only_the_callers_tenant` |
| EVT-04 versioned events | `domain_events_unknown_version_requires_resync_instead_of_skipping`, `domain_events_append_refuses_oversized_payload_and_unknown_kind` |
| Wake-ups | `domain_events_feed_wakes_on_new_events_and_authority_changes_only`, `domain_events_subscriber_is_woken_by_commits_not_by_reads` |
