# Printer send queue storage (GRE-1208)

The Rust send queue (GRE-365) keeps its items in the `printer_send_queue`
table. Node keeps them as one JSON array in each tenant's `printer.send_queue`
setting. This page covers the storage half: the table, how Rust reads a queue
Node left behind, and when the items move. It is compiled only with the
default-off `desktop_rebuild` feature, and only the Rust-only launcher
(`PP_DESKTOP_REBUILD=1` or `true`) opens the writer that moves them. The
storage writes come first below; the send routes and the upload worker that
sends to printers (GRE-365) follow in [Routes](#routes-gre-365) and
[Upload worker](#upload-worker-gre-365-gre-1215).

## Schema 44

The 43 to 44 step installs the table and its index from
`pp-storage/src/send_queue/schema.sql`, validates them and stamps 44 in one
transaction, after a `pre-schema44.db` copy like every step through
`LAST_COPIED_STEP`. It alters no table Node uses. Preflight refuses the table
in a database below 44, every open checks its SQL against the reference, and
the Node schema-parity comparison omits it, as it omits the other Rust-only
tables.

| Column | Meaning |
| --- | --- |
| `tenant_id`, `id` | Primary key. The tenant and the item id are Node's, unchanged. |
| `position` | Stored order within the tenant, unique per tenant. |
| `filename`, `artifact_path`, `printer_id` | Required, as Node's `parseItem` requires them. `printer_id` is the fleet printer id string Node stored. |
| `profile_id` | The Build, null or a positive integer. |
| `match`, `wait_for_idle`, `start` | Node's send options. |
| `state` | `queued`, `sending`, `done`, `error` or `cancelled`. |
| `checkoff_units` | Null or a JSON array of `{part_id, unit_index, object_name?}`. |
| `upload_job_id`, `error`, `host_name` | Null or non-empty text. |
| `created_at`, `updated_at` | Node's ISO timestamps. |

A partial unique index, `printer_send_queue_one_sending`, allows at most one
`sending` item per tenant and printer, the rule Node's dispatch checks before
it claims an item.

## Reading a queue Node left

While a tenant's `printer.send_queue` setting row exists, every Rust reader
reads the setting exactly as before and the table holds nothing for that
tenant. Once the setting is gone they read the table, which
`send_queue::rows` returns in Node's item JSON shape, so each reader keeps its
own Node-compatible parsing:

- the farm reads (`GET /api/v2/printer-send-queue` and the match suggestion
  evidence), [Farm reads](farm-reads.md)
- Build History's send entries and the farm log's send entries
- the Build context's production counts
- Accept's production check and its Required-unit remap, which writes the
  remapped `checkoff_units` back to the table rows, or to the setting while
  the setting exists

## When items move

`WriterOwner::open` runs `send_queue::recover` after job recovery, in one
immediate transaction. The writer opens only after the ownership marker
decision has given the directory to Rust, and Node refuses a database at
schema 44, so no Node process writes the setting afterwards.

1. For each tenant with the setting, if the setting is a clean queue, every
   item is inserted in stored order and the setting row is deleted. Clean
   means a JSON array whose every item Node's `parseItem` keeps whole: a
   known state; a non-empty id, file name, artifact path and printer; no
   duplicate id; a `profile_id` that is null or a positive integer; and
   `checkoff_units` that are null or an array of whole coordinates. Text is
   trimmed and `error` cut to 500 characters, as `parseItem` does, and a
   missing time is stamped with the open time, as Node stamps it on load.
2. A queue that is not clean stays in the setting and keeps being read from
   there. Moving it would drop or change items that Node or Accept reads, and
   Accept still refuses such a queue as it does today. The only rewrite is the
   one Node's own restart makes: each `sending` item becomes `error` with
   the interrupted text below. A value that is not a JSON array, or holds no
   `sending` item, stays byte for byte.
3. A tenant whose table already holds items keeps both: the setting is read,
   and the rows are never merged with it.
4. Each tenant moves inside its own savepoint. If the table refuses an item
   (for example an id over 128 characters), that tenant's inserts roll back
   and its setting stays, as in item 2; startup and the other tenants are
   unaffected.
5. A `sending` item moves as `error`, and every `sending` row already in
   the table becomes `error`, with Node's
   `INTERRUPTED_PRINTER_SEND_ERROR` text and a new `updated_at`. The process
   that started the upload is gone, so the outcome is unknown; the item is
   never sent again automatically, and only the user may send it again.

A second open finds no setting and no `sending` item, and changes nothing.

An item dispatched by Rust (below) also has a Rust `printer-upload` job.
Job recovery runs first: an upload whose effect was admitted needs
reconciliation, and its item becomes `error` with the interrupted text. Then
every remaining `sending` item becomes `error`, and its upload job, which job
recovery queued again because nothing reached the printer yet, is failed. That
item says `Interrupted by process restart before the upload began. Nothing was
sent to the printer. Send now sends it.`, and the upload never starts on its
own.

## User actions (GRE-365)

`pp_storage::send_queue::writes` holds the queue's writes. Each request is one
writer transaction, authenticated like a job enqueue, and only an accepted
write commits.

| Request | Effect |
| --- | --- |
| `Add` (Add to queue, GRE-1227) | Writes a `queued` item last in stored order for a file the route staged, with a new UUID, Node's defaults (`match` `pinned`, `wait_for_idle` true, `start` false) and the normalized `checkoff_units`. Refused with 404 `Profile not found` when the Plan is not the user's, and with the Farm refusal below when Farm is off for that Plan. It creates no job and wakes no worker. |
| `Admit` | Authenticates only, so the route refuses a bad credential before it reads an upload. |
| `Dispatch` (Send now, and each item Send ready picks) | A `queued` or `error` item becomes `sending` and gets a new durable `printer-upload` job, with its file, printer, start choice, Build and Checkoff units, in the same transaction. Refused with 409 `Item is not dispatchable` for another state, 409 `Item changed concurrently` while another item is `sending` to the printer, and 409 `Another send to this printer is still unresolved` while any other job holds the printer. |
| `Reconcile` | The user's answer for an item whose last send has an unknown outcome; see below. Otherwise 409 `This item has no send with an unknown printer outcome`. |
| `Remove` | A `queued` or `error` item becomes `cancelled`; otherwise 409 `Item not cancellable`. |
| `Missing` | A `queued` or `error` item whose staged file is gone becomes `error` with Node's `Artifact file missing on disk`. |
| `Get`, `Ready` | The item, or up to 8 `queued` items in stored order (optionally for one printer), as Node's dispatch and drain read them. `Ready` answers the first item's Farm refusal instead, so the drain refuses as a whole. |

An unknown id is 404 `Queue item not found`. A tenant whose queue stayed in
the legacy setting gets 409 for every request; that queue is only read.

The job settles its item in the transaction that saves the job: a finished or
uploaded-only job makes it `done`; a failed job makes it `error` with the
job's reason (`Upload failed` when it has none); a cancelled one `Send
cancelled`; and a job whose printer outcome is unknown makes it `error` with
the unknown-outcome text. That includes an attempt whose lease expired while
the process kept running: only startup recovery uses the interrupted text. An
attempt whose lease expired before any effect was admitted is never queued
again: the job fails, and its item is `error` with `NOT_ADMITTED_ERROR`
("The send did not start because storage could not admit it. Nothing was sent
to the printer. Send now sends it."), so the upload never starts minutes later
without a user action.

## Unknown outcomes

A send's outcome is unknown when its Rust upload awaits reconciliation, or,
for a send Node started, when Node's restart marked it interrupted. Such a job
keeps its printer reserved, so nothing else is sent to that printer, and
nothing is resent automatically:

- `Dispatch` and `Missing` on the item are refused with 409 and problem code
  `send_outcome_unknown`.
- `Reconcile` names the send it answers for with `job_id`, the item's
  `upload_job_id`. An answer for any other send, or none, is refused with
  409 and problem code `send_changed`, so a late answer from a stale view
  never closes a newer send. With `printed: true` it makes the item `done`. The job succeeds,
  with the printer's receipt as its result, only when a kept receipt proves
  the send (the upload, and the start when the item starts the print), as
  the generic job reconciliation requires. Without that proof the job fails,
  and its reason says the user reported it printed and which receipt the
  printer never gave: the upload's, or only the start's. With
  `printed: false` it marks the job failed and leaves the item `error` with `You reconciled this send: the printer did not print it. Send
  now sends it again.` The answer is stored in
  `durable_job_reconciliations` as `UserPrinted` or `UserDidNotPrint`
  against the last effect, and the printer is free. Neither answer sends; a
  new send needs a new Send now.
- `Remove` never sends, so it marks the unresolved job failed with `Removed
  from the send queue after the printer outcome was unknown` and frees the
  printer.

A send Node started has no Rust job, so until it is reconciled or removed
its item reserves the printer: dispatching another item to that printer is
refused with `Another send to this printer is still unresolved`. Its answer
only changes the item.

If a queue-owned upload leaves reconciliation through the generic job
reconciliation instead, the job's new state still settles its `error` item:
a confirmed success makes it `done`, so it is never sent twice.

A receipt kept from an attempt whose authority was refused counts as
confirmed when the user says it printed. Otherwise the receipt is moved into
the reconciliation record, which keeps it as evidence.

## Routes (GRE-365)

`pp_api::send_queue` serves the user actions in the Rust-only runtime, under
`/api/v2`, behind `PP_DESKTOP_REBUILD` and the accepted gate (loopback, exact
Host and Origin, and a session or API key). `pp-core` mounts them with the
status routes.

| Route | Answer |
| --- | --- |
| `POST /printer-send-queue` (Add to queue) | 200 `{item}` with `state: "queued"`. A `multipart/form-data` upload; see [Add to queue](#add-to-queue-gre-1227). |
| `POST /printer-send-queue/{id}/dispatch` (Send now) | 200 `{item, job_id}`. Body `{"force": true}` skips the idle check. |
| `POST /printer-send-queue/drain` (Send ready) | 200 `{results: [{item_id, job_id} or {item_id, error}]}`, or one 403 `FEATURE_DISABLED` / 500 `FEATURE_TOGGLES_INVALID` when Farm refuses a ready item. Optional `printer_id` must be a non-empty string. |
| `POST /printer-send-queue/{id}/reconcile` | 200 `{item}`. Body `{"printed": true|false, "job_id": "<upload_job_id>"}`; `job_id` may be omitted or null only when the item has no `upload_job_id`. A `job_id` for another send is 409 `send_changed`; a missing or non-boolean `printed`, or a non-string `job_id`, is 400. |
| `DELETE /printer-send-queue/{id}` (Remove) | 200 `{item}`, and the item's staged directory is deleted. |

Send now checks storage first, so an unknown outcome is always 409
`send_outcome_unknown`. Storage also requires the Farm feature for the item's
Plan, or for the user when the Plan is gone, as Node's
`storedQueueAdmissionProblem` does: 403 `FEATURE_DISABLED` when it is off and
500 `FEATURE_TOGGLES_INVALID` when the stored toggles cannot be read. Send
ready checks Farm once over the items it would send, before it sends any, and
answers that one problem for the whole request instead of a 200 with per-item
errors, as Node's drain route does. Then Send now checks the printer without
contacting it:

- the file name must end in `.gcode`, `.bgcode` or `.gco` (400);
- the staged file must be under `exports` in a `printer-uploads` directory
  (400 `Invalid artifact path`). A missing file makes the item `error` with
  `Artifact file missing on disk` and answers 409;
- the printer must be in the fleet (404) and be linked to an enabled
  Moonraker or PrusaLink host (400);
- an item with `wait_for_idle` needs the status watcher's latest state to
  be idle or complete, unless the request forces it (409 `Printer is printing
  — wait for Idle or force dispatch`). Moonraker and PrusaLink hosts have
  watchers here (PrusaLink since GRE-1175); any other kind needs `force`.

Send ready does the same for each queued item it reads (up to 8), sends at
most one per printer, and skips an item refused with 409. That item stays as
it is, except an item whose staged file is gone, which becomes `error` as in
Send now. Any other refusal is listed as `{item_id, error}`.

## Add to queue (GRE-1227)

`POST /api/v2/printer-send-queue` is Node's `POST /printer-send-queue`. It only
stages a file and writes a `queued` row; it never sends. Send now and Send
ready stay separate user actions. The form is read under Node's limits: one
file part named `file` or `gcode` of up to 64 MiB (413 `Print file exceeds the
64 MiB upload limit`), up to eight fields cut at 64 KiB, and 10 requests a
minute. The fields are Node's: `printer_id`, `profile_id` or `plan_id`,
`start` (`1`, `true` or `yes`), `wait_for_idle` (off for `0`, `false` or `no`),
`match` (`pinned` or `compatible`) and `checkoff_units` (a JSON array; invalid
or repeated units are dropped). The route answers in this order:

1. 401 before it reads the body when the credential is not valid;
2. 415 for a body that is not `multipart/form-data`; 400 `Only one G-code
   file is allowed` for a second file part;
3. 400 `printer_id is required`, then `G-code file required`, then `Only
   .gcode / .bgcode / .gco files can be sent to a printer`, then `Pick a plan
   to bind this send (profile_id required)`;
4. the printer: 404 `Fleet printer not found`; 400 `Printer is not linked to
   a host. Link a Moonraker or PrusaLink host in Settings.`; 400 `Linked
   printer host was not found` for a missing or disabled host; and 400 `Queue
   is not supported for <type>` for any host the upload worker cannot send to
   (anything but Moonraker and PrusaLink);
5. the file is written to `exports/printer-uploads/<random>/<name>`, where
   `<name>` is Node's sanitized base name with `:` replaced by `_`, so a
   Windows drive prefix cannot move the file out of that directory;
6. storage's `Add`: 404 `Profile not found`, or the Farm refusal. The staged
   directory is deleted again unless the item was written or storage's
   outcome is unknown (commit unknown, or the reply lost after admission),
   since the item may then exist.

Known differences from Node, provisional (GRE-1227): Node checks the Plan
before the printer, so with both wrong Rust answers the printer refusal
first. Node answers a Bambu host with 403 `FEATURE_DISABLED` while the Bambu
Module is off and 400 when it is on; Rust always answers 400, since no Bambu
send exists. Node reads `profile_id` and unit numbers with `Number()`, so it
also takes forms such as `"0x10"`; Rust reads decimal numbers, and units only
as JSON numbers, so a unit written as `4.0` is dropped. A disabled linked host
is refused with `Linked printer host was not found`; Node queues for it. A
file name that is only spaces answers 400 `Could not enqueue item` before the
printer, Plan and Farm checks; Node answers it after them. Node's add route
leaves `@fastify/multipart` limit errors to Fastify's default handler, so a
second file part or a ninth field is 413 `reach files limit` or `reach fields
limit`, and an oversized file is probably 413 `request file too large`; Rust
answers the texts above, and 400 `That upload could not be read` for a ninth
field or a field named after an `Object.prototype` member.

## Upload-only preference (GRE-366)

The Rust-only send queue stores a tenant's opt-in upload-only preference as a
JSON boolean in `app_settings` under `printer.upload_only`. A missing setting
means disabled. It has no schema migration, timer or dispatcher of its own.

- `GET /api/v2/printer-send-queue/upload-only` returns
  `{"item":{"enabled":false}}` when the preference is absent.
- `PUT` on that same route requires `{"enabled":true}` or
  `{"enabled":false}`. Strings and numbers are refused. Both routes use the
  existing session/API-key admission and default-off rebuild gates.
- With the preference enabled, Add stores `start=false` regardless of the form,
  and Send now / Send ready force the queue row and its immutable job payload to
  `start=false`, including rows queued before enabling it. The worker can admit
  only an upload effect for that payload; combined-start and separate-start
  effects are refused. Moonraker receives no print-start request; PrusaLink
  receives `print-after-upload: ?0`.
- A mode change sends nothing, creates no job and wakes no worker. Dispatch is
  still an explicit user action. Disabling the preference never upgrades an
  already confirmed upload-only job to upload-and-start. Changing the preference
  does not rewrite a send already dispatched or running.
- Interrupted uploads keep the existing uncertain-outcome/reconciliation rules.
  Neither an idle worker tick, restart nor mode change resends them. A damaged
  stored preference refuses a dispatch instead of guessing a send mode.

Tests named `ticket_t_27` cover persistent defaults and tenant isolation,
HTTP admission, explicit drain/dispatch, forbidden start effects, idle timers,
and interruption/restart. Driver traffic uses only pp-testkit loopback printers;
request receipts assert no start command and PrusaLink's upload-only header.
The preference has no replacement-UI control in this backend slice.

## Upload worker (GRE-365, GRE-1215)

`pp_core::send_worker::SendWorker` is the only code that uploads to a
printer, and only `printer-upload` jobs that a dispatch enqueued. It starts
with the Rust-only runtime. When idle it waits for a dispatch to wake it, or
30 seconds for an attempt whose lease expired, so it does not poll storage.
Shutdown lets an upload in flight finish for up
to the mutation drain bound. For each job it:

1. checks the job's original authority, resolves the host, and reads the
   staged file (at most 64 MiB);
2. admits one effect (`printer_upload`, or `printer_upload_and_start`)
   before any request. Storage admits it only while Farm is still on, so a
   send whose Farm was turned off after Send now fails with `Feature farm is
   disabled.` and contacts no printer;
3. sends once through the Moonraker or PrusaLink driver. An upload-only send
   never calls `print/start`. A confirmed send finishes the job with the
   printer, host and remote path, creates the Build's Checkoff link, and
   deletes the staged directory, as Node does. A failed label read falls back
   to the printer id and `Printer`, and a link Node would not create is
   skipped, so a send the printer confirmed always finishes. When the file states the
   slicer's filament estimate (see below), the job result carries it as
   `filament_estimate_g` (and `filament_grams_per_mm` when the file states a
   length too), and the same transaction records both for the link in
   `print_link_grams` with basis `estimated`, covering the link's Checkoff units. A definite refusal, including
   a start the printer refused after the upload, denies the effect, so the
   job fails, the item is `error` with the reason, and the printer is free.
   An unknown outcome (for example a connection closed with no answer) leaves
   the job awaiting reconciliation. Nothing is retried.

A job that fails before the effect (unlinked printer, missing or foreign
file, an authority or effect admission that storage refuses) fails with its
reason at once and contacts no printer, so its item is `error` and the
printer is free. Shutdown stops claiming new uploads before the gateway
drains. A claimed upload that has not started when shutdown begins fails
with "Print Partner was stopping" and contacts no printer. The worker checks
before the effect is admitted and again after, denying the admitted effect.
Status checks before Send now never trim the queue's terminal history. A
host that is not Moonraker or PrusaLink fails with "Upload is not supported
for <type>".

## Slicer gram estimate (GRE-1238)

`pp_domain::slicer_grams::estimate` reads the estimate from the bytes the worker
already read for the upload, before the upload, and contacts nothing:

- text G-code: within the first and last MiB, `; total filament used [g] =`
  (PrusaSlicer, SuperSlicer), else `; total filament weight [g] :` (OrcaSlicer,
  BambuStudio), else `; filament used [g] =` with one value per extruder summed;
- binary G-code (`.bgcode`): the same keys in the uncompressed print metadata
  block, else the printer metadata block, before the first G-code block.

The length comes from `; total filament length [mm] :` (OrcaSlicer), else
`; filament used [mm] =` summed. Grams over length is the send's grams per
millimetre, which the status watcher multiplies by the length the printer
measures at finish. With several extruders this is one blended factor over
all filaments. A file with grams but no positive length records the
estimate with no factor, so the estimate stays after the print.

A file with no such value, a compressed metadata block, a value that is not a
positive number, or Cura's metres-only comment gives no estimate. The link then
has no grams row, and the printer's measured length at finish is not recorded
either, because there is no grams per millimetre to convert it with. A send without
Checkoff units creates no link and records nothing.

## Tests

```sh
cargo nextest run --locked --manifest-path rust/Cargo.toml -p pp-storage --features desktop_rebuild -E 'test(ticket_t_26) | test(remapped_queue_rows_match_literal_node_results)'
cargo nextest run --locked --manifest-path rust/Cargo.toml -p pp-api -p pp-core --features pp-api/desktop_rebuild,pp-core/desktop_rebuild -E 'test(ticket_t_26) | test(add_to_queue)'
cargo nextest run --locked --manifest-path rust/Cargo.toml -p pp-storage --features desktop_rebuild -E 'test(the_form)'
```
