# Domain events

GRE-1088 adds one bounded change feed so the replacement UI can invalidate
queries instead of polling. This page covers the storage half,
`pp_storage::domain_events`, and the SSE route that serves it (GRE-1162),
both compiled only with `desktop_rebuild`. Printer telemetry stays on `/ws/printers` and
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
event. Refused Checkoff requests write none, and neither does a Checkoff
request that changes no row, such as assembling an unprinted unit or
repeating a completion.

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
task, never block or call storage. A waker that panics is unsubscribed and the
writer carries on. Subscribe before the first read so no commit between them
is missed.

## HTTP stream

`pp_api::events::events_router` serves `GET /api/v1/events` as Server-Sent
Events. `rebuild_api::read_router` mounts it, so the Rust-only runtime
(`product_router`) serves it when `PP_DESKTOP_REBUILD` is `1` or `true`.
There is no Node route and no fallback.

Admission is the Accepted Plan reads' gate: loopback Host and Origin checks,
then a session cookie or API key. The first read runs before the response
starts, so a missing or refused credential is a 401 problem (RFC 9457) and a
malformed cursor is a 400. Errors after the stream starts end it.

The resume cursor is the `Last-Event-ID` header, which a browser
`EventSource` sends on reconnect, or the `cursor` query parameter for a first
connect. `Last-Event-ID` wins when both are present; a repeated value is a 400.

| Event | `id` | `data` |
| --- | --- | --- |
| `ready` | the current position | `{"position"}`; sent first when the connect has no cursor. Load state after it. |
| `domain_event` | the event's cursor | the event: `cursor`, `type`, `schema_version`, `aggregate_*`, `operation_id`, `occurred_at`, `payload` |
| `resync_required` | the position to continue from | `{"reason", "position"}` with a reason from [Cursors and reads](#cursors-and-reads). Reload state; the stream continues. |

Each stream subscribes to storage wake-ups before its first read, re-reads
after every wake, and sends a `: keep-alive` comment every 15 seconds, which
also re-reads. A read that fails, such as after the session is revoked or
storage stops, ends the stream.

| Stream bound (`StreamLimits::default`) | Value |
| --- | --- |
| Concurrent streams per router | 8; another connect gets a 503 problem |
| Frames queued per client | 64, about 160 KiB at the payload limit |
| Slow-client deadline | 10 s with the queue full, then the stream ends |
| Heartbeat | 15 s |

A slow client never blocks storage: the writer only signals a stream's task,
and the task waits on the client's queue. When the queue stays full past the
deadline, the task releases its stream slot and subscription, and the response
body fails, so the server closes the connection without sending the queued
frames. A client that stops reading entirely holds only its socket buffers
and the queue until it reads again or disconnects.

## Tests

```sh
cargo nextest run --locked --manifest-path rust/Cargo.toml --no-tests=fail \
  --features pp-storage/desktop_rebuild,pp-core/desktop_rebuild -E 'test(domain_events)'
cargo test --locked --manifest-path rust/Cargo.toml -p pp-storage --features desktop_rebuild \
  --lib plan_publication::tests::transaction::transaction_replays_node_outcomes_receipts_and_persisted_rows
```

Unit tests live in `pp-storage/src/domain_events/tests.rs`; the Checkoff,
restart and wake-up tests are in `pp-storage/tests/checkoff_progress.rs`. The
SSE tests are in `pp-api/src/events/tests.rs`, and the mount test is
`rebuild_read_router_serves_the_domain_event_stream` in
`pp-core/tests/rebuild_read_http.rs`:

```sh
cargo test --locked --manifest-path rust/Cargo.toml -p pp-api --features desktop_rebuild --lib events::
cargo nextest run --locked --manifest-path rust/Cargo.toml -p pp-core --features desktop_rebuild --test rebuild_read_http
```

| Item | Tests |
| --- | --- |
| EVT-01 same transaction | `domain_events_rolled_back_change_leaves_no_event`, `domain_events_refused_or_failed_checkoff_writes_no_event`, `domain_events_checkoff_that_changes_no_row_writes_no_event`, `domain_events_checkoff_commits_one_event_with_its_change`, and the publication parity test (one event per applied Accept, none for replays and refusals) |
| EVT-02 replay or resync | `domain_events_replay_returns_missed_events_in_commit_order`, `domain_events_read_pages_at_the_limit`, `domain_events_cursor_past_retention_requires_resync`, `domain_events_other_generation_or_future_cursor_requires_resync`, `domain_events_restart_starts_a_new_generation` |
| EVT-03 authentication (storage side) | `domain_events_unauthenticated_read_is_refused`, `domain_events_reads_only_the_callers_tenant` |
| EVT-04 versioned events | `domain_events_unknown_version_requires_resync_instead_of_skipping`, `domain_events_append_refuses_oversized_payload_and_unknown_kind` |
| EVT-02 over HTTP | `domain_events_reconnect_replays_every_missed_event_in_order`, `domain_events_unreplayable_cursor_gets_resync_required` (other generation, ahead of the log, past the replay window), `domain_events_resume_cursor_prefers_last_event_id_and_refuses_malformed_values` |
| EVT-03 over HTTP | `domain_events_unauthenticated_connects_get_401`, `domain_events_revoked_session_closes_the_stream` |
| EVT-05 slow consumer | `domain_events_slow_consumer_is_disconnected_without_stalling_writers`, `domain_events_stream_count_is_bounded` |
| Wake-ups | `domain_events_feed_wakes_on_new_events_and_authority_changes_only`, `domain_events_panicking_waker_is_unsubscribed_and_others_still_wake`, `domain_events_subscriber_is_woken_by_commits_not_by_reads` |
