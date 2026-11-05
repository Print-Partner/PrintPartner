# Printer stream

`GET /ws/printers` is the live printer stream (GRE-1014). It is part of the
replacement Rust API, which mounts only with the default-off `desktop_rebuild`
feature compiled and `PP_DESKTOP_REBUILD` set to `1` or `true`. With the flag
off the route answers 404. It is read-only: clients never send commands, and
the server ignores anything a client sends.

## Admission

The upgrade goes through the same `accepted::Gate` as the farm reads, and a
storage read proves the session or API key before the upgrade. Without one the
upgrade answers 401. The credential is checked again before each heartbeat; if
it is refused (401 or 403), the server closes with 1008. If storage cannot
answer that check (busy, stopping or failed), it closes with 1013 instead, and
the client resumes with `since`. Until the producer has read
the fleet once, and after the runtime starts shutting down, the upgrade answers
503, so a snapshot never claims an empty fleet that is only unread. Shutdown
closes every open socket with 1001 and waits for each to finish.

## Frames

Every frame is a JSON text frame with `type`, `stream` (a random id per server
process) and `seq`.

| `type` | Sent | Fields |
| --- | --- | --- |
| `snapshot` | First, on a fresh connect | `heartbeat_seconds` (15), `printers`: every configured printer. `seq` is the last event it covers. |
| `printer` | One per change | `printer`: the printer's new state. `seq` is one more than the previous event. |
| `removed` | A printer left the fleet | `id` |
| `heartbeat` | Every 15 seconds | `at` (RFC 3339) and `seq`, the last event sent to this client |

The producer advances its own copy of each host's status one watcher
transition at a time, so a state the printer passed through is its own event
even when the next one follows quickly: `error` then `idle` is two events. A
new last-heard time alone is not an event. Each watcher transition carries a
counter; the producer starts from a map snapshot read with that counter and,
if it falls more than 1,024 transitions behind, resyncs the same way, skipping
transitions the snapshot already holds so no older state follows a newer one.

## Printer

`StreamPrinter` fields, all from the farm reads' projection
([Farm reads](farm-reads.md)). Unknown fields are absent, never guessed, and no
field carries a host address, API key, file path or printer error text.

- `id`, `name`.
- `state`: `idle`, `printing`, `paused`, `complete`, `error`, `offline` or
  `unknown`. Moonraker's `standby` and `cancelled` read `idle`. A printer no
  watcher reads (not set up, manual, awaiting status) is `unknown`. A watched printer
  that stops answering becomes `offline` and keeps its `last_heard_at`.
- `display`: GRE-1002's `PrinterStateView` (`execution`, `bed_clear`, `display`).
- `reason`: the farm reads' `PrinterReason`, when there is one.
- `result_id`: the watching Checkoff result for the file the printer is
  running, while printing or paused.
- `progress_percent`: whole percent while printing or paused.
- `bed_needs_clearing`: from `bed_clear`; absent while it is `unknown`.
- `filaments`: `slot`, and `color_id` and `label` when stored.
- `last_heard_at`: the last status reply, RFC 3339.

Start and expected end time, material, Spoolman spool id, nozzle size and the
printer's own failure text are not observed yet, so they are absent.

## Resume

Reconnect with `?stream=<id>&since=<seq>` to get the events after `since`. The
server keeps the last 1,024 events. When `stream` names another process, `since`
is ahead of the stream, or the events after it are gone, the client gets a
fresh `snapshot` instead. Both parameters or neither; anything else answers 400.
A client more than 1,024 events behind is closed with 1013 and resumes the same
way.

## Proof

```sh
cd rust
cargo test -p pp-api --features desktop_rebuild --lib printer_stream
cargo test -p pp-drivers --lib printer_stream_transitions
cargo test -p pp-core --features desktop_rebuild --test runtime_m0 -- printer_stream
```

The launcher test replays a recorded Moonraker session (printing, error, idle,
then no answers) through the Rust-only runtime and checks the 401, the
snapshot of every configured printer, the ordered states, `offline` with a
last-heard time, a heartbeat, resume and the fresh-snapshot fallback.
