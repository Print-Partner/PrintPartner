# D19 publication client and writer admission, GRE-612 PR 3

With `desktop_rebuild`, `WriterOwner::publication_with_policy` validates the
deployment's `AuthPolicy` before constructing a `PublicationClient`.
`SingleAccountDefault` resolves sessions to tenant `default`; `AccountTenant`
resolves sessions to the account's tenant. Both retain the account as the receipt
actor. Invalid policy fails before queue admission or writes. The policy-less
`publication()` helper is available only in unit tests. `PublicationCommand`
trims keys with ECMAScript whitespace rules and admits 1 to 160 UTF-16 units.
`normalized_http` keeps the original admitted request separate from a request
normalized by a caller. The writer resolves the authenticated tenant and actor
inside the publication transaction. Request data cannot select either identity.

`PublicationClient::apply` uses the bounded writer queue. Before admission,
cancellation returns `required_units::AdmissionFailure::Cancelled`, a full queue
at its deadline returns `QueueFull`, and closed or poisoned admission returns
`Stopped`. After admission, the caller waits for the result even if cancelled;
a lost writer reply returns `OutcomeUnknown`. The writer uses its dequeue
transaction deadline and the shared bounded SQLite lock admission path. It
commits applied or replayed receipts and rolls back unsuccessful publication.
Its token allocator uses operating-system randomness.

The feature installs and validates the admission table and immutable triggers
at schema 37. Main never uses version 36: the reference's version 36 import
tables arrive with D14's version 38 step. Reopening checks schema text and
orphan admissions. Without the feature, storage remains at schema 35 and refuses
a newer database.

D13 save, GRE-608, can call `AuthenticatedPublicationContext::resolve` and
`publish_in_transaction` on its own transaction, using `PublicationRequests::new`
for admitted and execution requests. That caller owns commit or rollback. It
must not enqueue `PublicationClient::apply` while holding its transaction.
D13 integration remains GRE-608's caller work.

Eight literal fixtures run the actual Node Apply route and workspace service
against fresh SQLite databases. They cover missing, whitespace-only and
UTF-16-sized keys, embedded BOM preservation, identical replay receipts,
session actor attribution, tenant isolation and unexpected-error text. The
fixture supplies authenticated request context; authentication middleware
itself is covered by the auth tests. Rust compares these literals through its
client, queue and transaction executor with deterministic fixture tokens. A
separate owner test proves schema installation, random token generation,
writer dispatch, shutdown and replay after reopen.

GRE-665 item 1 applies because this public client can return errors. Invalid
keys now use Node's `Request is invalid`; unexpected publication errors use
Node's `Plan draft update failed`. The Node error fixture injects a SQLite
trigger failure through the real route. Internal transaction callers retain
the original diagnostic error. Authentication failures retain the auth module's
existing errors. Queue admission has no Node counterpart; direct Rust tests
check the supplied reference behavior, including cancellation after admission.

### D17 source scan: observation and settlement

D17 (a), GRE-613, ports `source_scan` document records, observations, and
settlement from `desktop/source-sync-local-current-20261008`. The
`desktop_rebuild` feature gates the module and the jobs commands
`ObserveLocalScan` and `SettleLocalScan`. Settlement rechecks the original
credential, Source reservation, configuration, activation, and path. It replaces
the document index and records a confirmed effect receipt in one transaction.
PDFs remain pending for extraction. Settlement does not finish the job or retire
its Source token. Callers must complete, reconcile, release or drop `source_work` promptly.

The three-PR slice plan assigns the complete D17 scope as follows:

| Slice | Work | Estimated added/changed lines |
| --- | --- | ---: |
| D17 (a) | Document types, observation/settlement, journal SQL, jobs commands/clients/outcomes, `classify_issued`/`proof` callers | 600 |
| D17 (a) | Settlement, authority and reservation tests; Node generator and golden; docs | 650–850 |
| D17 (b) | Complete, inspect, inspect-held and reconcile commands/clients/outcomes; both transaction decisions; `retire_issued`/`consume_retired` callers | 780 |
| D17 (b) | `revalidate_source_authority`, `commit_source_authority_refusal`, idempotent End/release, remaining reference catalog tests | 400–500 |
| D17 (c) | Schema 40, `backups/pre-schema40.db`, validation inside the migration transaction, migration tests and docs | 150–200 |

D14 (b), #553, supplies schema 39 and the Source configuration and document
provenance columns. D17 (c) installs the scan journal at schema 40 through the
production migration. The scan tests open `WriterOwner` and use that schema
without ad-hoc tables or column alterations. The default build stays at schema 35.
Source Sync remains D18, GRE-614.

`fixtures/parity/source-scan/generate.mjs` runs Node's
`services/source-docs-index.ts`, which calls `services/source-docs-scan.ts`.
Four cases cover empty input, markdown/PDF indexing, ignored directories and
Unicode paths. Rust tests assert the committed Node rows and counts. Node stores
24 hex characters of the content hash; the reference Rust journal uses full
SHA-256. The generator also records full Node crypto hashes for Rust input.
The probe sorts paths by JavaScript code units for the settlement contract and
records inventory/index digests from Node crypto. Durable settlement, Source
reservations and authority checks have no Node counterpart. The tests adapt the
observation/settlement portion of the reference catalog test
`repeated_local_scan_completion_adopts_the_exact_terminal_result` and its Source
lease lifetime checks. D17 (b) ports exact terminal-result adoption and release
acknowledgement. New tests exercise stale observations, revoked authority,
invalid documents and transaction rollback.

Regenerate or verify the Node results with:

```sh
node fixtures/parity/source-scan/generate.mjs
node fixtures/parity/source-scan/generate.mjs --check
```

D17 (b) adds `CompleteLocalScan`, `InspectLocalScan`, `InspectHeldLocalScan` and
`ReconcileLocalScan`. Completion commits the public result and journal digest
before retiring the Source token. A repeated completion adopts only the exact
terminal request and result. Held inspection recovers settlement evidence after
a lost reply. Reconciliation retains the effects and records the failure before
retiring the reservation. Release acknowledges already retired tokens and keeps
the proof if acknowledgement fails.

GRE-703 items 2–4 bind settlement to the journaled reservation incarnation and
token, test retired proofs and cancellation after observation, and replace the
raw UNIQUE error with `LocalScanFailure::AlreadyObserved`. D17 (c) addresses
GRE-703 item 1 by moving the test harness onto the real migration. It also removes
the client schema prerequisite notes for GRE-707 item 6.
`mark_local_failure`, `revalidate_source_authority` and
`commit_source_authority_refusal` retain their tests; this schema slice adds no
production callers for them. GRE-610 (D14 import Phase) calls the two authority
helpers. `mark_local_failure` keeps its `dead_code` expectation until GRE-614
(D18 source_sync) calls it. `commit_source_authority_refusal` reuses
`AuthorityRefusalReason::of` and saves one refusal.

The Node generator now also executes the local `syncProjectById` completion and
failure paths. Rust compares four terminal document-index results and two
failure-metadata cases against Node. Durable completion, inspection and
reconciliation have no Node counterpart. Tests port the remaining reference
completion and release checks, and add refusal, rollback and lost-reply cases.

### D17 source scan: schema 40

D17 (c) completes GRE-613 behind `desktop_rebuild`. The 39 to 40 step installs
`source_scan_executions` and its receipt index from `source_scan/schema.sql`.
`IF NOT EXISTS` preserves the reference catalog text and lets the step validate
an existing table and refuse drift without replacing it.

The upgrade keeps D14 (b)'s order:

1. Back up the input before any write. From 39, this is `backups/pre-schema40.db`.
2. Restore common objects and repair tenants without starter seeding.
3. Back up each pending step, install and validate it inside one transaction, then
   stamp and commit its version. The scan validator runs before the 40 stamp.
4. Validate the installed schema, then seed starters.

Preflight validates the scan journal for the stored version. A database below 40
with scan objects is refused. Reopening at 40 validates the journal without a new
backup. A fresh install creates no backups. The default build refuses schema 40.

Upgrade tests cover 35, 37, 38 and 39 to 40, logical backup contents from 39,
a blocked backup with unchanged input, and a drifted existing journal. The direct
initializer test bypasses preflight to prove that step validation rolls back the
new index and leaves version 39 intact. The same drift is also refused through
`WriterOwner` preflight. Logical comparisons include the SQLite catalog and rows,
because SQLite's backup API can rewrite header counters.

### D18 source sync: batch open and target claim

D18 (a), GRE-614, ports the first part of `source_sync` from
`desktop/source-sync-local-current-20261008` behind `desktop_rebuild`. A Sync
attempt opens a batch, then claims its targets one at a time. The default build
is unchanged and stays at schema 35.

| Slice | Work | Estimated added/changed lines |
| --- | --- | ---: |
| D18 (a) | Schema 41; `open`, basis validation, target claim with Source reservation, ordinary failures, `halt_job`; Cancel and worker Fail halts; bridge removal; tests, Node probe, docs | 1,600 |
| D18 (b) | Target validation, `settle`, `fail` with `mark_local_failure`, `acknowledge`, `adopt`, `halt_after_error` and `HaltSourceSync`; recovery of retained evidence; tests and docs | 1,450 |
| D18 (c) | `finish`, `inspect`, `inspect_retained`, the recovery read types, the journal-owner guards for generic Finish, effects and reconciliation, claim and advance refusals, the prune guard; tests and docs | 1,200 |

`open_source_sync` seals the batch the first time. An explicit selection keeps
payload order. A selection of all Sources orders them by `last_synced_at`, then
id, as the reference does; Node's `listProjectIds` uses table order. A later open
revalidates the sealed basis and returns claims from earlier attempts to pending.

`claim_next_source_sync_target` claims the first target that is not settled, in
one writer transaction. A missing Source fails with Node's `Source not found`. A
non-Local Source fails as `unsupported_kind`. Neither failure takes a reservation.
A Local target records the attempt, the Source observation and a new reservation
token. The token is activated after the commit, as claim-time reservation does.
The reference uses three round trips for this: next, `BeginSyncTarget` and bind.
A busy Source returns `SourceBusy` and leaves the target pending. A claim before
open is refused. The returned `SyncTargetLease` holds the Source until it is
released or dropped, so callers must not keep it longer than the target's work.

Cancel, a worker `Fail` and a revoked original credential halt the batch. Pending
targets become not-run. A claimed target is kept for reconciliation, and the Job
becomes `reconciliation_required`. Otherwise the Job is cancelled or failed. A
revoked credential records a Source-writer refusal. Until D18 (b), a claimed
target cannot be settled. GRE-854 rejects generic `Finish`, `FinishPublic`,
`BeginEffect` and `ConfirmEffect` for Sync before authority revalidation.
These calls leave the Job, batch, target, history and Source reservation unchanged.
Heartbeat and progress updates remain available, and worker `Fail` still halts
the batch. Non-Sync jobs and builds without `desktop_rebuild` keep their existing
generic worker behavior. GRE-775 stores one successful target and releases its
reservation. GRE-776 records one local read failure, the Source failure
observation and progress, and leaves that reservation unacknowledged so the
failure stays retained evidence. GRE-777 acknowledges that retained failure when
the presented observation matches the claim. GRE-778 adopts an already-
acknowledged successful result when the live Source observation still matches
the claim capture, and refuses mismatched provenance without writing.
GRE-779 ports `halt_after_error` and `HaltSourceSync` so a caller Source
operation error closes through the existing batch halt path. The detail is the
Job recovery message. Retained failures stay as recorded. A claimed target is
kept for reconciliation. A stale attempt is refused without writing.
GRE-780 ports `inspect` and `InspectSourceSyncTarget` so the authorized worker
reads one live Sync target without writing or changing reservation ownership.
Claimed targets report `Pending`; acknowledged successes and failures return
their stored evidence. A foreign tenant row or Job mismatch is refused without
writing.
GRE-781 ports recovery read types and `inspect_retained` through
`inspect_source_sync_recovery` so a credential-scoped caller validates retained
target/result/observation bindings and reads each disposition without adopting
or deleting evidence. A corrupt Source binding, incomplete success capture,
batch-phase mismatch or stale version is refused without writing.
GRE-782 ports `finish`, `validated_terminal_aggregate` and
`FinishSourceSync` so a Sync-owned finish succeeds only after every target has
a valid acknowledged terminal result. The aggregate is stored on the batch.
Exact retry returns the same aggregate without writing. An unsettled target is
refused without writing.

GRE-857 preserves a Sync Job's completed Source-writer refusal evidence when
Cancel marks the Job for cancellation during reconciliation. The refusal
observation, target failure code and detail, recovery message, and generation
remain unchanged.
A second Cancel writes nothing. Repeating the same authority-refusal halt also
writes nothing, including when the first halt failed the Job without a claim.
The regression tests cancel twice and reopen storage to check the saved evidence.
GRE-855 applies the same Source-writer refusal and batch halt during
`open_source_sync`, heartbeat, progress and worker `Fail`. A recognized refusal
commits before returning its typed authority error. It fences the attempt even
before a batch exists. An existing batch halts with pending targets marked
not-run, or requires reconciliation when claims or retained effects remain.
Open-time refusal preserves a prior attempt's claim evidence instead of resetting
it to pending. Unrecognized errors still roll back. Tests reopen storage to check
that the refusal, batch and target evidence persist. Cancel preserves a completed
worker-advance halt and its refusal evidence.

The reference schema numbers Source Sync 42 because its 41 validates manifest
columns, which main does not have. Here the 40 to 41 step installs
`source_sync_batches` and `source_sync_targets` from `source_sync/schema.sql`,
following D17 (c): back up to `backups/pre-schema41.db`, install and validate in
one transaction, then stamp 41. Preflight refuses Sync objects below 41.

`fixtures/parity/source-sync/generate.mjs` runs Node's `InProcessJobRunner` Sync
job over a missing id and a Local Source. The Rust test asserts the target order
and Node's literal failure. Node has no durable batch, Source reservation or
authority check. The claim-before-open, restart-recovery and ordinal tests adapt
the reference `pp-core/tests/source_sync_local.rs` cases. The halt, refusal,
busy-Source and bridge tests are new. The generic-worker regression checks every
effect kind and both Finish APIs after a target claim, with the original session
valid and revoked. It compares database snapshots and verifies that the same
Source reservation still prevents other work.
The non-Sync regression confirms that generic effects and Finish still work.

```sh
node fixtures/parity/source-sync/generate.mjs --check
umask 022
cargo test --locked --manifest-path rust/Cargo.toml -p pp-storage --features desktop_rebuild --lib source_sync::
cargo test --locked --manifest-path rust/Cargo.toml -p pp-storage --features desktop_rebuild --test job_feature_admission
cargo test --locked --manifest-path rust/Cargo.toml -p pp-storage --test durable_jobs
cargo test --locked --manifest-path rust/Cargo.toml -p pp-storage --features desktop_rebuild --test durable_jobs
cargo check --locked --manifest-path rust/Cargo.toml -p pp-storage --all-targets
cargo check --locked --manifest-path rust/Cargo.toml -p pp-storage --all-targets --features desktop_rebuild
cargo test --locked --manifest-path rust/Cargo.toml -p pp-storage --features desktop_rebuild --lib uploads::tests
```

### D20 Checkoff progress

GRE-615 adds `pp-storage::checkoff_progress`, compiled only with
`desktop_rebuild`. `WriterOwner::checkoff_progress()` returns a client whose
`apply` runs one request in one immediate writer transaction. No routes call it
yet; D24 and D26 wire HTTP behind `PP_DESKTOP_REBUILD`.

The contract is `db/accepted-plan-progress.ts`, `setAcceptedPrintedCounts` in
`db/repository.ts`, the progress and assembly routes in `routes/parts.ts`, and
the import route in `routes/plan-drafts.ts`. `apply` returns the Node status
and JSON body for each outcome:

- Completion by Required-unit token marks that unit and every lower unit
  printed. Clearing it clears that unit and every higher unit, including their
  assembly.
- Assembly by token sets one unit. Assembling an unprinted unit changes nothing.
- Part coordinates (`/parts/:id/progress` and `/parts/:id/assembled` bodies)
  resolve to a token against the current accepted snapshot.
- Import sets each listed Part's printed prefix. It checks every row before it
  writes any, and reports how many Parts changed.

Every write first checks the expected accepted basis, then archival. Unit
completion and assembly then check that each Part has one progress row per
unit. Import skips that check, as Node does. Only a successful write commits,
so a refusal leaves the database unchanged, including API key usage. An import for a Plan the caller cannot see returns 404
`progress_target_not_found`. This differs from Node on purpose. Node throws a
plain error and returns 500, but its route already returns 404 for a Plan it
does not serve.

```sh
cargo test --locked --manifest-path rust/Cargo.toml -p pp-storage --features desktop_rebuild --test checkoff_progress
```

### Domain event log, GRE-1088

The 41 to 42 step installs `domain_events` and its index. Checkoff
writes, including GRE-420 verify, bursts and burst undo, and Accept append an
event in their own transaction when they change a row. The step writes no
step copy of its own; the copy taken at open covers it. See
[Domain events](../domain-events.md).

### Domain event stream, GRE-1162

`GET /api/v1/events` serves the log as Server-Sent Events from `read_router`,
so the Rust-only runtime mounts it. It resumes from `Last-Event-ID`, answers
401 without a credential, ends the stream when the session is revoked, and
stops feeding a client whose bounded queue stays full for 10 seconds; that
connection closes at the server's next write, or after 30 seconds in which the
client accepts no bytes. Shutdown draining ends open streams and does not wait
for them. See
[Domain events](../domain-events.md#http-stream).

```sh
cargo test --locked --manifest-path rust/Cargo.toml -p pp-api --features desktop_rebuild --lib events::
```

### Printer Checkoff verify and dismiss (GRE-420)

GRE-420 adds two `checkoff_progress::Request` variants, still compiled only with
`desktop_rebuild` and still called by no route. Each runs in the same immediate
writer transaction as the other Checkoff writes.

- `Request::Verify { body }` takes the `POST /printer-checkoff/verify` body:
  `link_id`, `decisions` and optional `additional_decisions`. It ports
  `verifyPrinterCheckoff`, `verifyAcceptedPrint` and
  `applyAcceptedUnitDecisionsInternal`. In one transaction it reads the link
  from the `printer.checkoff_links` setting, checks the decisions against the
  link's pending units and the current accepted snapshot, marks confirmed units
  printed, records the resolved units, imported-extra results, state and
  `units_marked` on the link, and appends one `print_jobs` row with one
  `print_job_parts` row per decision. It returns Node's status, `detail`, and
  `{ link, units_confirmed, units_rejected, outcomes }` body.
- `Request::Dismiss { body }` ports `dismissHostFailedLink`: a `host_failed`
  link becomes `dismissed`.

Links are parsed, normalized, moved to the end and trimmed to 200 exactly as
`services/printer-checkoff-store.ts` writes them, so the stored setting matches
Node byte for byte after a write, except for lone surrogates (below). Only an explicit authenticated request can
verify. A link that a status watcher still holds in `watching` refuses with 409,
and a repeated decision or a second link for an already printed unit cannot
count a unit twice: the first refuses because the unit is no longer pending, the
second records the outcome with `units_confirmed: 0`.

One correction differs from Node on purpose. Node writes each Part's confirmed
units before it checks the next Part's left-fill rule, then returns 400 and
commits the earlier Parts. Rust checks every Part first, so a refused burst
writes nothing. The parity fixture keeps this case as its last step and the test
asserts the one progress row Node left behind.

Text is cut at UTF-16 units like Node's `.slice`. When a cut splits a surrogate
pair, Node stores a lone surrogate escape. Rust strings cannot hold one, so
storage reads a lone surrogate in the stored links as U+FFFD and writes U+FFFD
where its own cut splits a pair. The rest of the setting still loads; before
this, one such string made every link in the tenant unreadable. The fixture
seeds a lone surrogate and cuts a note across a pair to cover both directions.

Not ported here: the route itself and its webhooks, unattributed-print claiming
after verify, the Spoolman deduction Node runs after the response (and the
storage-owned deduction intent GRE-411 adds), file assignments, remote-file
recheck, and the monitor that moves links to `awaiting_verify`. Those stay on
Node until their whole operation moves. Manual complete moved with GRE-1169
(below).

The Node fixture replays the services on a copy of
`rust/crates/pp-storage/tests/fixtures/required-units/unchanged.db` with
synthetic links; generated IDs and times are normalized.

```sh
cd web && node --conditions=development --import tsx ../fixtures/parity/printer-checkoff-verify/generate.mjs --check
cargo nextest run --locked --manifest-path rust/Cargo.toml -p pp-storage --features desktop_rebuild --no-tests=fail -E 'test(ticket_t_61_checkoff)'
```

### Manual complete (GRE-1169)

With `desktop_rebuild` and `PP_DESKTOP_REBUILD=1` or `true`, the Checkoff
router mounts Node's `POST /printer-checkoff/{id}/manual-complete` (and its
`/api/v1` and `/api/v2` aliases) behind the same loopback, Host, Origin and
credential gate as the bursts.
`pp-storage::checkoff_progress::verify::manual_complete` runs in one writer
transaction and matches Node:

- 404 `Tracked print not found` when the link id is unknown.
- 409 `This print is monitored by its printer host` unless the link's
  `integration_id` is `manual:<printer_id>`.
- 409 `Tracked print is no longer waiting to finish` unless the link is
  `watching`.
- Otherwise the link becomes `awaiting_verify` with `host_outcome: success`,
  `saw_active: true`, `last_progress: 100` and a fresh `completed_at`, moves
  to the end of the list before the terminal-history trim, and the response is
  `{ link }`. Nothing is checked off; the operator still verifies the print.

Node's handler has no side effects after the write, so this route owns the
whole operation. Single-link verify and dismiss stay on Node: they deduct
Spoolman stock, send webhooks and claim unattributed prints after the write.

```sh
cargo test --locked --manifest-path rust/Cargo.toml -p pp-api --features desktop_rebuild --test checkoff_single_http
```

### Checkoff bursts (GRE-1090)

With `desktop_rebuild` and `PP_DESKTOP_REBUILD=1` or `true`, the gateway mounts
`POST /printer-checkoff/verify-batch` and `POST /printer-checkoff/verify-batch/undo`
at `/` and `/api/v2` (`pp-api/src/checkoff.rs`, admission through
`accepted::Gate`: loopback, exact Host and Origin, session or API key). These
routes exist only in the Rust-owned runtime: with the flag on, Rust holds the
ownership marker and `.desktop-lease` for the data directory and standalone Node
is refused (see Writer admission above), so no Node verify, monitor or claim
writes `printer.checkoff_links`, progress or outcomes beside a burst. A burst
does not run Node's unattributed-print claim or Spoolman deduction after verify;
those are GRE-1169 and GRE-411 (deduction intent).
MCP `propose_verify_printer_checkoff` applies through the same burst
(GRE-1170, below).

A burst body is `{ "idempotency_key": "...", "beds": [{ "link_id", "decisions",
"additional_decisions"? }] }`: 1 to 200 characters of key and 1 to 50 beds, each
naming a different link and shaped like a `POST /printer-checkoff/verify` body.
`pp-storage::checkoff_progress` runs every bed through the GRE-420 verify in one
writer transaction. The left-fill rule counts units that any bed of the burst
confirms, so a Part's units spread over several beds may come in any bed order.
The first bed that refuses rolls the whole burst back and
returns its status with the detail prefixed by `beds[i]: `. Success returns
`undo_token` and one result per bed.

The receipt is written in the same transaction under the
`printer.checkoff_bursts` setting: the key, the SHA-256 of the request with
object keys sorted, the response, and the link, progress and outcome rows undo
needs. A repeat with the same key and request returns the stored response before
any other check and writes nothing, also after a restart. The same key with a
different request returns 409. The newest 100 receipts are kept. A dropped
receipt leaves the SHA-256 of its key in `printer.checkoff_bursts_retired` (the
newest 5,000), and replaying that key returns 409 instead of running again, so an
undone burst cannot be counted again by a late retry. A replay of an undone burst
that still has its receipt (GRE-1190) returns 409 with a detail that starts
`burst_undone:`, writes nothing and appends no event, so a client is never told
a burst applied when its progress was undone. The receipt stays as it was, apart
from the `undone_at` mark undo wrote. To apply the same beds again the client
sends a new idempotency key.

Undo takes `{ "undo_token" }` and is a compensating write: it restores each
link at its original position, the progress rows and the outcome rows the burst
wrote. It refuses with 409 and a reason when any of them changed since, when
the burst was already undone, and when anything that can depend on the burst's
units changed after it: the burst stores the SHA-256 of every progress row of
the Parts it decided, the outcome rows that name its units, and every stored link
whose units or resolved units name them, and undo compares it. A later verify of
another link that records a unit the burst printed, or a later unit that filled
from the burst's unit, therefore blocks undo. Unrelated activity, such as a new
print job, a new link, or a dismiss of a link without the burst's units, does not. A receipt
without that hash or its unit list (written before they existed) refuses with
409 instead of undoing on weaker checks. Undo on a Build archived since the burst refuses like
every other progress write (409 `Archived Plan Progress cannot be changed`). An unknown token is 404. No printer, webhook or
Spoolman call happens in either transaction.

#### Reconcile by operation id (GRE-1170)

`GET /printer-checkoff/verify-batch?idempotency_key=...` (also at `/api/v2`,
same admission) answers what a key settled to without resending the body. The
key is read like the burst's key (JS-trimmed, 1 to 200 characters), and it must
appear exactly once, otherwise the answer is 400. The read writes no Checkoff
state; as on every route, an API key's `last_used_at` may be recorded.

| State | Body | Meaning |
| --- | --- | --- |
| `applied` | `{idempotency_key, state, applied_at, undone_at: null, receipt}` | The burst committed; `receipt` is the stored response |
| `undone` | the same with `undone_at` | The burst committed and was undone |
| `expired` | `applied_at`, `undone_at` and `receipt` are null | The burst committed, its receipt was trimmed, and the key cannot run again |
| `unknown` | `applied_at`, `undone_at` and `receipt` are null | The key is in neither list, but the retired list is full (5,000 keys), so a key older than the newest 5,100 bursts may have been dropped. The lookup cannot tell whether it ran, and replay would treat it as new |
| 404 `burst_not_found` | problem body | The key is in neither list and no key was ever dropped, so no burst used it and nothing was applied. Resending the same request with the same key is safe |

#### MCP `propose_verify_printer_checkoff` (GRE-1170)

The Rust MCP (`/mcp`, `/api/v1/mcp`, API key always required) ports Node's
`propose_verify_printer_checkoff` with the same name and schema
(`{link_id, decisions}`). The propose step reads the link in a writer
transaction that changes no Checkoff state and refuses as Node does: `link_id
and decisions are required`, `Checkoff link not found`, or `Checkoff link is
not awaiting verification`. Nothing is written until `confirm_apply`.
`confirm_apply` runs the proposal as a one-bed burst with the idempotency key
`mcp:<action_id>`, so it shares this burst's receipt, replay, undo and farm log.
It answers `{ok: true, result: <burst receipt>}`, which carries `undo_token`.
Node answered with the single-verify body. A refused burst answers
`{ok: false, status, detail}`, writes nothing and stays pending for a retry or
dismiss.

The action id is the operation id. `confirm_apply` for an action this session
no longer holds (applied already, or proposed before a restart or in another
session) looks up `mcp:<action_id>`. If that burst exists, the call answers
`{ok, reconciled: true, action_id, detail, result, undone_at}` instead of
Node's unknown-action error: `ok` is true while the burst stands, and false
once it was undone or its receipt expired. Otherwise, including an `unknown`
lookup, the call answers Node's unknown-action error. If the lookup cannot run
(the writer is busy or past its deadline, or authentication fails), the call
answers `{"error": "Checkoff is unavailable"}` and the action is not reported
as unknown.

The replacement UI's Today screen confirms one bed, or units picked on several
beds as one burst (one request, one Undo), through the generated client (`verifyCheckoffBurst`, `undoCheckoffBurst`) and resends a
lost send with the same key; see `web/apps/ui/README.md` (Today).

`web/packages/contracts/src/checkoff-burst.ts` holds the request schema and the
`P` (`plateBed`) and `F` (`partFillBeds`) helpers, which both produce this
request shape. `verifyBatchRequest` merges beds for the same link, keeping both
unit decisions and imported-extra `additional_decisions`, and leaves out beds with
nothing to decide (a plate still `watching`), which the route would refuse.

```sh
cargo nextest run --locked --manifest-path rust/Cargo.toml --no-tests=fail --features pp-storage/desktop_rebuild,pp-core/desktop_rebuild,pp-api/desktop_rebuild -E 'test(verify_batch)'
npm --prefix web exec -w @print-partner/contracts -- vitest run src/checkoff-burst.test.ts
```

### Manual Checkoff bursts (GRE-1191)

The Build Checkoff tab records what was printed or assembled with no printer
link. With `desktop_rebuild` and `PP_DESKTOP_REBUILD=1` or `true`, the
Checkoff router mounts two writes at `/` and `/api/v2`, and the Accepted Plan
reads mount one read at `/api/v2` only. All three are Rust-only; Node has no
route at these paths, so the gateway manifests do not list them.

- `GET /api/v2/plans/{id}/checkoff/units` returns `{ profile_id, basis, parts }`:
  the included Parts in the order of `GET /plans/{id}/checkoff`, each with its
  quantity, printed and assembled counts and every Required unit's `token`,
  `unit_index`, `object_name`, `printed` and `assembled`. `basis` is the
  Accepted Plan basis a burst sends as `expected`, or `null` for a Build with
  no Accepted Plan (`parts` is then empty). A Build the caller cannot see is 404.
  Each unit also carries `plate`, `{ plate_id, ordinal, printer_name }` for the
  head Plate revision's Plate it is placed on, and the body carries that
  revision as `plate_revision_id` (GRE-1121, for the Checkoff tab's `P` key).
  `pp-storage::plates::head_plates` reads them in the same transaction as the
  units. Both are `null` when the head revision stamps an older Accepted Plan
  or there is none; a unit the head lists as unplaced has `plate: null`. A head
  revision whose layout fails its integrity checks fails the read with 500,
  as `GET /plans/{id}/plates` does.
- `POST /plans/{id}/checkoff/bursts` takes `{ idempotency_key, expected, units }`:
  a key of 1 to 200 characters after trimming, the basis, and 1 to 1,000
  `{ token, printed?, assembled? }` entries, each setting at least one flag and
  naming a different token. Unknown fields are refused. `expected` for another
  Build than the path is 404 `Plan not found`.
  `pp-storage::checkoff_progress::manual` applies the burst in one writer
  transaction. Printed units fill from the left, as the single-unit writes do:
  `printed: true` prints that unit and every lower unit of its Part, and
  `printed: false` clears that unit and every higher unit and their assembly. A
  burst that prints a unit at or above one it clears on the same Part is 400.
  Assembling a unit that is not printed after the burst is 400. A token that is
  not on an included Part of the Accepted Plan is 404, so counts never pass the
  Part's units. A stale basis is 409 `Accepted Plan changed; reload and retry`,
  and an archived Build is 409 like every progress write. The first refusal
  leaves the database unchanged.
- Success returns `{ idempotency_key, undo_token, profile_id, printed_added,
  printed_removed, assembled_added, assembled_removed, parts }`, where `parts`
  holds each named Part's quantity and new counts. The receipt is stored in the
  same transaction under the `checkoff.manual_bursts` setting with the request's
  SHA-256 (object keys sorted), the progress rows as they were before, and the
  SHA-256 of what can depend on the changed units (the same state hash as
  printer bursts). Retention and replay follow printer bursts: the same key and
  request returns the stored receipt and writes nothing, also after a restart;
  the same key with another request is 409; a key whose receipt was dropped
  (newest 100 kept) is 409 from `checkoff.manual_bursts_retired`. A replay of a
  burst that was undone is 409 `This Checkoff burst was undone; send the change
  again as a new burst`, so a late retry never counts it again.
- `POST /plans/{id}/checkoff/bursts/undo` takes `{ undo_token }` and restores
  the progress rows the burst changed. It refuses with 409 and a reason when the
  Accepted Plan basis changed since the burst, when any progress row of the
  touched Parts, an outcome naming a touched unit, or a printer Checkoff link
  naming one changed since (`Checkoff changed since this burst: a later change
  on its Parts depends on it`), and when it was already undone. A token from
  another Build's path, or an unknown one, is 404.

Each Part a burst or undo changes appends one `checkoff.progress_changed` event
in the same transaction ([Domain events](../domain-events.md)). Neither write
touches a draft, a Required-unit ID, a printer Checkoff link, a print job, the
send queue or Spoolman, so Checkoff never passes the Accept gate and never
deducts or reserves stock. The generated client names the three routes
`readBuildCheckoffUnits`, `recordManualCheckoffBurst` and
`undoManualCheckoffBurst`.

```sh
cargo test --locked --manifest-path rust/Cargo.toml -p pp-storage --features desktop_rebuild --test checkoff_manual_bursts
cargo test --locked --manifest-path rust/Cargo.toml -p pp-api --features desktop_rebuild --test checkoff_manual_bursts_http
```

### Print-file classification (GRE-1212)

`pp_domain::print_file::classify` (feature `desktop_rebuild`) is the Rust port
of Node's `classifyPrintFileBytes`. The file-assignment routes in GRE-1210
will use it to decide what a print file is from its bytes alone. It returns the
classification (`gcode`, `bgcode`, or `3mf` with `toolpath_package`,
`slicer_project`, `model_package` or `unsupported`), the SHA-256 and size of
the bytes, or one of Node's ten rejection reasons with Node's wording.
`next_action` and `is_print_ready` match `printFileNextAction` and the
contract's `isPrintReady`.

It reads files of up to 256 MiB. For 3MF it walks the ZIP central directory
only, with Node's caps (2,000 entries, 1,024-byte names, 64 MiB per entry,
256 MiB in total, depth 16), and refuses unsafe names and symlink entries. It
never inflates an entry or parses XML. Binary G-code is checked structurally
(header, version, checksum type, block chain) and never decoded.

`fixtures/parity/print-file-classification/cases.json` holds 85 inputs and
the Node classifier's answers: the Node unit-test inputs plus truncated 3MF and
binary G-code, ZIP64 records, hostile ZIP entries (including names with a leading byte-order mark, which Node's `TextDecoder` drops) and G-code edges.

```sh
cd web && node --conditions=development --import tsx ../fixtures/parity/print-file-classification/generate.mjs --check
cargo test --locked --manifest-path rust/Cargo.toml -p pp-domain --features desktop_rebuild --test print_file_parity
```

### Print-file assignment storage (GRE-1217)

`Request::AssignFile` in `pp-storage::checkoff_progress` (feature
`desktop_rebuild`) is the create path of Node's
`materializeAcceptedPrinterLink`. It is what `POST
/printer-checkoff/file-assignments` records once the route has parsed the
request and read the file. GRE-1220 mounts it (next section). In one
writer transaction, against the Build's Accepted Plan:

- The plan revision must be the one the operator saw, or 409 `The Accepted
  Plan moved on; reload the Build and choose the file again`.
- Every confirmed unit must be an incomplete Required unit of an included
  Part, picked once, and every object mapping must point an unmatched or
  ambiguous object at a distinct confirmed unit. Otherwise 409 `That print
  file does not map to an incomplete Required unit in this Build`.
- No watching or awaiting-verify link may already hold the same file name
  (basename, any case) on that integration, or 409 `That print file is already
  assigned`.
- The link is created watching with the confirmed units (each with its
  Required-unit object name), the unmatched names as `unlabeled_names`, the
  remote path, identity and classification, and, with `retain_unmatched`, the
  unmatched objects (or the file itself) as pending imported extras. With
  `completed` it is created awaiting verify with Node's host-completed fields.
  Node applies that patch in a second transaction; here it is the same write.
- The response is `{ link, attribution }`, with Node's camelCase
  `attribution` object.

`checkoff_progress::attribution` is now the one port of
`resolveAcceptedPrinterAttribution` (per-name outcomes, the positive-only rule
that drops fuzzy file-name matches, the file-name fallback),
`confirmAcceptedPrinterUnits` and `applyConfirmedObjectMappings`. The
unattributed-print claim uses it too. `pp_domain::gcode_objects::match_object_name_exact`
is the matcher without its fuzzy step.

A Build that does not exist is a 500 for both claim and assignment, because
Node's operational read throws `Profile not found` and both routes answer that
with a plain 500. The claim answered 409 `Accepted Plan operational state is
not initialized` before.

`fixtures/parity/printer-file-assignment/node.json` replays 18 create commands
through Node's repository (and its completed patch) on a copy of the
required-units fixture: matches by Required-unit name and by file name,
confirmed and mapped units, every refusal the fixture can reach, kept extras, a
completed print and a missing Build. The Rust test replays them and compares
every response and stored row.

```sh
cd web && node --conditions=development --import tsx ../fixtures/parity/printer-file-assignment/generate.mjs --check
cargo test --locked --manifest-path rust/Cargo.toml -p pp-storage --features desktop_rebuild --test printer_file_assignment --test unattributed_claim
```

### Print-file assignment routes (GRE-1220, GRE-1255, GRE-1256)

`pp_api::file_assignments` serves `POST /printer-checkoff/file-assignments`,
`POST /printer-checkoff/file-assignments/preview` and
`POST /printer-checkoff/file-assignments/upload` at the root and under
`/api/v1`, where Node registers them, behind `desktop_rebuild` and
`PP_DESKTOP_REBUILD`. Each mount has Node's windows per route: 20 a minute
for the preview and assignment with their 1 MiB JSON limit, and 10 a minute
for the upload.

- Storage parses the body in Node's `parsePrintFileRequest` order and resolves
  the printer as `resolveAssignmentPrinter` does: a fleet printer by id, its
  host or `manual:<id>` with `tracking: "manual"`, or, with no `printer_id`,
  the unmanaged printer (`manual:unmanaged`). The assignment route then checks
  `plan_revision_id`, `unit_tokens` and `object_mappings`; the preview reads
  the Build's Accepted Plan and builds `suggestPrintFileAttribution`'s
  suggestion and match review. Every refusal keeps Node's status and detail.
- The route reads the file. A `snapshot_token` is trimmed and needs a
  `remote_path` and no `upload_token`, or 400 `A snapshot token requires its
  original printer file path`. It redeems the classification and identity
  captured for exactly that Build, printer, path and file name while the
  printer's linked integration is unchanged; anything else is 409 `This file
  preview expired or its source changed. Cancel and reopen the file.` A
  redeemed file is not re-observed for drift, and a successful assignment
  spends the token. Node redeems and records in one synchronous step; here the
  assignment reserves the token inside the storage call, so a concurrent
  assignment of the same token is refused, and an admitted write spends it
  even if the request is dropped. An outcome that is unknown after admission (a
  lost writer reply or `CommitUnknown`) spends it too, so the operator reopens
  the preview instead of recording the file twice. Any other answer, a refusal
  or a credential failure such as an expired session, wrote nothing and puts the
  token back. A reserved token keeps its place in the 128-token limit, and the
  writer transaction checks
  the printer's host fingerprint again, so a settings change after the route's
  read refuses with the same 409.
- The upload route authenticates the caller first, as Node's `onRequest`
  hooks do, so a bad credential is 401 before the rate window counts it or
  the body is read. It takes `multipart/form-data` (else 415 `Send the print
  file as multipart/form-data`) under Node's busboy limits: one file part of
  up to 64 MB (413 `That print file is larger than the 64 MB PrintPartner
  will take as an upload`, 400 `Upload one print file at a time` for a
  second), and four fields cut at 64 KiB (a fifth is 400 `That upload could
  not be read`). A part is a file when it has a file name or an
  `application/octet-stream` type, as busboy decides, and a part named after
  an `Object.prototype` member is 400 `That upload could not be read`, as
  `@fastify/multipart` refuses it: when the form ends, so a later files,
  fields or size limit answers first, and without that part's own size limit.
  Node has no cap on the whole body; here it
  is 65 MiB, and a body past it that is still inside the file part is the
  413 (the unreadable 400 when that part is named after an
  `Object.prototype` member), anywhere else the unreadable 400. Answers still differ from Node for
  requests the web client never sends: an empty multipart body (the
  unreadable 400 here, `profile_id is required` in Node), a field part typed
  `application/json` (read as text here, JSON-parsed in Node), a file name
  sent only as `filename*=` (multer does not decode it), and a malformed or
  oversized JSON body (415 here, Fastify's 400 or 413). It then checks
  `profile_id`, the `file` part, its file name (what follows the last `/` or `\`,
  unless that is `.` or `..`, trimmed) and extension in Node's order,
  reads the Build's Accepted Plan (a missing Build is a 500,
  as for the preview), classifies the bytes off the async workers (409 with
  Node's rejection detail), and holds the classification for 15 minutes under
  a new `upload_token`. The answer is the preview's body with that token.
  `print_file::UploadStore` is Node's `pendingUploads`: it keeps only the
  classification and identity (`size_bytes`, `sha256`), since the bytes
  classify the same way every time, but counts their size toward Node's
  128 MB cap, so a third full upload is 503 `PrintPartner is holding as many
  uploaded print files as it will; record one of them, then try again`.
- An `upload_token` is redeemed only for the Build it was uploaded for. An
  unknown, expired or foreign one answers Node's 409 `That upload is no longer
  available; upload the print file again`. The assignment reserves it as it
  reserves a snapshot: a successful or unknown outcome spends it, and a
  refusal releases it, so the operator does not upload the file again after
  a stale plan revision. Node reads the upload without reserving it; here a
  concurrent second assignment of the same token is refused, while a preview
  still reads it.
- A `remote_path` without a snapshot on a Moonraker or PrusaLink host is read
  off the printer as Node's `inspectRemoteFile` reads it (GRE-1256), through
  the same `PrinterHostPort` the printer-host reads use: the directory listing
  gives the provider identity (`size_bytes`, `modified_at`,
  `provider_revision`) when the host lists the file, and the bytes the host
  serves, up to 256 MiB, are classified off the async workers and add
  `size_bytes` and `sha256`. The host serves only a file its directory
  listing shows (the printer-host file read's guard against paths outside
  the storage tree), so unlike Node, where a failed listing still lets the
  bytes decide, a listing that fails or omits the file leaves it unread. A
  path that is not a safe storage path, a host that will not serve the file,
  or a failed read, including one cut off by the host's download deadline,
  leaves the file unread too. A reply whose declared `Content-Length` is past
  256 MiB is refused before any byte is read, as Node's
  `readBoundedResponseChunks` refuses it, and a body that grows past 256 MiB
  is refused at the frame that crosses it: both are 409
  `That print file is too large to inspect`. Bytes that are not a print file
  are 409 with Node's rejection detail, and a classifier that fails inside
  PrintPartner is 500. The route matches the listing entry and reads the file
  by its safe storage path, so `/gcodes/a.gcode` finds the entry for
  `gcodes/a.gcode` and a path with `..` is unread; the observe step and the
  stored link keep the raw `remote_path`, as Node does. A printer whose settings cannot
  be read in time is 503 `Printer settings are unavailable`. A printer tracked
  by hand, or a host type Node has no file reader for, records the print
  unread as Node does, and an unread 3MF is refused with Node's 409.
- After such a read passes the print-ready check, the assignment route
  re-observes the file before recording it, as Node's
  `observePrinterCheckoffRemoteFile` does, in its own writer transaction
  (`FileRoute::Observe`) that commits whatever the assignment then does.
  Every watching or awaiting-verify link on that integration and path gets
  `remote_drift` set to Node's `detectPrinterFileDrift` reason (`content`,
  `revision`, `size`, then `modified`, each only when both identities carry
  it) or cleared when the file matches again; a changed link moves to the end
  of the list as `updatePrinterCheckoffLink` moves it. A snapshot or an
  upload has no path to drift against, so nothing is observed for them. The
  drift Node's status poll records for every link is not ported here.
- The assignment itself is `Request::AssignFile` (previous section).

`checkoff_progress::print_file::SnapshotStore` is Node's
`printer-file-snapshots.ts` for one database, as Node keeps one per repository:
every `SettingsClient` of a writer returns the same store from
`print_file_snapshots()`, and both routers take it from there. One capture at a time, at most
128 tokens, 15 minutes each. The files/content route calls
`SnapshotStore::begin` with `print_file::file_source` read before and after
it asks the printer, `Capture::push` for each chunk it streams, and
`Capture::finish` at EOF, which classifies the bytes and makes the token
redeemable. A capture dropped before EOF releases the store and leaves no
token.

`GET /printers/{id}/files/content?profile_id=` captures (GRE-1214). As in Node:

- `profile_id` is read with `Number()` and must name a stored Build, or the
  route answers 400 `Choose an existing Build before opening a print file`
  before it checks the path or asks the printer.
- Only a 2xx reply for a file the host lists is captured. Any other reply is
  answered as without a Build.
- A refused capture (another capture running, 128 tokens held, or the host's
  fingerprint changed while the file opened) answers 502 `File inspection
  unavailable. Finish the current file, then reopen this file.` without the
  body.
- A captured reply carries `X-PrintPartner-Snapshot: <token>` and
  `Access-Control-Expose-Headers: X-PrintPartner-Snapshot` and drops
  `Content-Length`, so the client waits for the end of the body. A file
  past the inspection limit, or one that does not classify, ends the body with
  an error and no token.
- The listing's `size_bytes` and `modified_at` become the snapshot's
  identity, then the capture's `size_bytes` and `sha256` replace them.

```sh
cargo test --locked --manifest-path rust/Cargo.toml -p pp-api --features desktop_rebuild --test file_assignments_http
cargo test --locked --manifest-path rust/Cargo.toml -p pp-storage --features desktop_rebuild --lib drift_takes_the_strongest
```

### Accepted Plate workspace read (GRE-1189)

`pp-storage::plates` serves Node's `GET /plans/{id}/plates`
(`routes/accepted-plates.ts`, `readAcceptedPlateWorkspace`) in one deferred
read transaction. `pp-api::plates::plates_router` serves it at `/` and
`/api/v2` behind `accepted::Gate` when `PP_DESKTOP_REBUILD` is `1` or `true`.

`read_router` mounts it with every other Plate route; see
[Accepted Plate routes in Rust](#accepted-plate-routes-in-rust-gre-419).

The read answers as Node does. A Build the caller cannot see is 404
`profile_not_found`; an empty Accepted Plan, or one with no Required units, is
`{"kind": "empty_plan"}`; a dirty or uninitialized compatibility state is 409
`accepted_state_unavailable`. With no head, or a head revision stamped with
another Accepted Plan basis, the answer is `setup` with the Required units and
`expected_plate_revision_id` (`null` without a head). Otherwise it is `ready`:
the head revision's layout, read in either stored layout format and checked
against its counts, tenant and layout digest, with placed units per Plate,
`unplaced` units, and `unassigned` Required units. `printers` lists the
`printer.fleet` rows that Node's `acceptedPrinter` accepts (trimmed id, name
and model, a positive build height, margins smaller than half the bed),
sorted by UTF-8 id. Stored Plate data that contradicts its revision, and a
stored Plate id that is not a generated `plate_` id, which Node's contract
parser rejects, are 500 `internal_error`. A read writes nothing.

### Accepted Plate edits (GRE-1184)

`pp-storage::plates` also serves the Plate-revision edits from Node's
`routes/accepted-plates.ts`: `PATCH /plans/{id}/plates/{plateId}/units/{token}` (move),
`PATCH .../units/{token}/pin`, `POST .../units/{token}/unplace`,
`POST .../units/{token}/transfer` and `POST /plans/{id}/plates/restore`.
`pp-api::plates::plates_router` serves them at `/` and `/api/v2` behind
`accepted::Gate` when `PP_DESKTOP_REBUILD` is `1` or `true`.

`read_router` mounts them with every other Plate route; see
[Accepted Plate routes in Rust](#accepted-plate-routes-in-rust-gre-419).

Each edit is one immediate writer transaction. It checks, in Node's order, the
target Printer (transfer only, 422 `printer_not_found`), that the Build is
visible and its Accepted Plan matches the request's `expected` basis (409
`accepted_plan_changed`, or `accepted_state_unavailable` while compatibility is
dirty or uninitialized), that the Build is not archived (409 `plan_archived`),
and that the head is `expected_plate_revision_id` (409
`plate_revision_changed`). It then reads the head revision's layout in either
stored layout format and checks its counts, tenant and digest. A change is
validated with the current clearance rule (422 `outside_build_area`,
`overlapping_units` or `invalid_units`), written as a new immutable revision
with the next revision number, and the head moves by compare-and-set from the
expected revision. The same transaction appends `plates.revision_changed`. An
edit whose layout digest equals the head's returns the head and writes nothing.
Refusals write nothing. Stored Plate data that contradicts its revision is 500
`internal_error`, as Node's `AcceptedPlateIntegrityError`.

A move takes any trimmed Plate id of at most 200 characters; pin, unplace and
transfer take only generated `plate_` ids, as Node's routes do. A transfer
leaves the unit unplaced on the target Plate, or on a new random `plate_` Plate
for `target_printer_id`, and drops a source Plate it empties. Restore accepts
only the revision the head's arrange replaced (otherwise 422
`unit_not_found`), writes its layout as a new revision and returns the
workspace.

```sh
cargo nextest run --locked --manifest-path rust/Cargo.toml --no-tests=fail --features pp-storage/desktop_rebuild,pp-api/desktop_rebuild -p pp-storage -p pp-api -E 'test(ticket_t_61_plates) | binary(plates_http)'
```

### Accepted Plate arrange (GRE-1200)

`pp-storage::plates` serves Node's `POST /plans/{id}/plates/arrange`
(`arrangeAcceptedPlates` in `services/accepted-plate-workspace.ts`) as
`Request::Arrange`. The body is `expected`, `expected_plate_revision_id` and
`mode`, which is `unplaced` or `all`; anything else is 400
`invalid_request`. `read_router` mounts it with the other Plate routes.

Arrange is one immediate writer transaction. It checks, in Node's order, that
the Build is visible (404 `profile_not_found`), that it has an Accepted Plan
(an empty Plan, or one with no Required units, answers `{"kind": "empty_plan"}`),
that a head revision exists, that the request's basis is current (409
`accepted_plan_changed`) and that the head is `expected_plate_revision_id` (409
`plate_revision_changed`). It then packs the layout, checks archive state (409
`plan_archived`) and validates the result with the clearance rule.

- `unplaced` keeps each manual or pinned unit where it is and packs every other
  unit of that Plate around them. Units that overflow go to new Plates for the
  same Printer, with Node's deterministic `initial_plate_id`.
- `all` repacks every unit of each Printer, split into buckets by the Build's
  stored Production setup rules (`production_setup:<id>` in `app_settings`,
  read by `production::setup::stored_rules`). It reuses the Printer's existing
  Plate ids in order, clears manual placement and pins, and records the replaced
  revision as `undo_from` so restore can return to it.

A unit that cannot fit its Printer is 422 `unit_too_large` with `token` and
`printer_id`. A packed layout whose digest equals the head's returns the
workspace and writes nothing. Otherwise the new revision, the head move and
`plates.revision_changed` commit together, and the response is the workspace
with each Plate's units in packing order, as Node answers.

`fixtures/parity/plates-arrange/node.json` is Node's answer for 28 cases in 13
phases (refusals, pinned and manual units, an unplaced unit, archive, a missing
head, a foreign Plan, several Printers, grouping rules and keep-together rules).
Regenerate it with `node fixtures/parity/plates-arrange/generate.mjs` and check it
with `--check`. `ticket_t_61_plates_arrange_matches_node` replays every case and
compares responses, changed tables, events and final Plate rows.

### Accepted Plate initialize (GRE-1202)

`pp-storage::plates` serves Node's `POST /plans/{id}/plates/initialize`
(`initializeAcceptedPlates` and `publishAcceptedPlatesInternal`) as
`Request::Initialize`. The body is `expected`, `expected_plate_revision_id`
(`null` before any Plate revision) and `assignments`, a list of
`{token, printer_id}` where `printer_id` is `null` or trimmed text of 1 to 200
UTF-16 units. Anything else is 400 `invalid_request`.

Initialize is one immediate writer transaction. Node reads, measures and
publishes in three steps; Rust holds the writer while it reads the STL files,
so no other write can land between the checks and the publish. In Node's
order it checks:

1. The Build is visible and has an Accepted Plan (404 `profile_not_found`,
   `{"kind": "empty_plan"}`, or 409 `accepted_state_unavailable`), and the basis
   is current (409 `accepted_plan_changed`).
2. The expected head. A head on an older Accepted Plan counts as the setup
   head. A mismatch with no current layout is 409 `plate_revision_changed`.
3. The assignments: 422 `unknown_unit_token`, `duplicate_assignment`,
   `missing_assignment` (an empty list) or `unassigned_units`, each with
   `tokens` in UTF-8 order. Units left out of the list stay unassigned.
4. The Printers in the stored fleet: 422 `printer_not_found` or
   `missing_printer_geometry` with `printer_ids`. While the head does not match,
   each of these refusals is 409 `plate_revision_changed` instead.
5. A replay: when the head moved but the stored layout is exactly what these
   assignments pack to with the stored dimensions, the answer is the current
   workspace and nothing is written. Otherwise 409 `plate_revision_changed`.
6. Each assigned unit's STL, by `plates::artifacts`: found inside its Source
   snapshot one segment at a time, case-insensitively, without following
   links, and checked against its accepted SHA-256. A failure is 409 with
   Node's reason as the code (`missing`, `unsafe_path`, `ambiguous_case`,
   `symlink`, `not_file`, `empty`, `io_error`, `digest_mismatch`, `legacy` or
   `untracked_source`) and `token`, 409 `invalid_stl`, or 422
   `degenerate_geometry`. The route limits are 10,000 units, 256 MiB of
   distinct STL bytes and 5,000,000 triangles counted once per unit (413
   `limit_exceeded` with `limit`). An STL shared by several units is read once.
7. Packing: each Printer in UTF-8 id order packs its units in Plan order,
   split by the Production setup's grouping buckets, onto Plates with Node's
   deterministic ids (422 `unit_too_large` with `token` and `printer_id`).
8. The publish: 409 `plan_archived`, then the clearance rule. A layout equal
   to the head of the same Accepted Plan writes nothing. Otherwise it writes
   a new revision, creates or moves the head, and appends
   `plates.revision_changed`. The answer is the workspace with units in packing
   order and `arrange_undo_revision_id` `null`, as Node answers.

STL geometry (`production::geometry`) moved from `pp-core` to `pp-storage` for
this; `pp-core::production::geometry` re-exports it.

One case differs from Node on purpose: a replay that names a unit the stored
layout does not hold is 409 `plate_revision_changed`, where Node throws and
answers 500. A Printer whose bed is past the stored Plate range (more than
2,147,483,647 µm) cannot be packed in the Plate types, so its units are only
checked against that bed while every other Printer packs, and a unit that does
not fit, on that Printer or another, is still 422 `unit_too_large`. After that an archived Build is 409
`plan_archived` and any other Build is 422 `outside_build_area`, the codes
Node gives for the same requests.

`fixtures/parity/plates-initialize/node.json` is Node's answer for 44 cases in
18 phases. Each phase writes box STLs into the pinned Source snapshot and
seeds SQL that re-pins those Parts to the files' SHA-256 and restamps the
revision digest with Node's `digestPlanRevisionParts`, because the fixtures pin
digests whose files do not exist. Regenerate it with
`node fixtures/parity/plates-initialize/generate.mjs` and check it with `--check`.
`ticket_t_61_plates_initialize_matches_node` replays it. The `plates::artifacts`
unit tests cover the limits, a linked file, a linked snapshot directory, an
ambiguous case and paths outside the repositories. The walk starts from
`repos/<locator>` as stored, not the canonical snapshot path the accepted read
checks, so a snapshot directory replaced by a link is refused as Node refuses it.

### Accepted Plate routes in Rust (GRE-419)

`pp_core::rebuild_api::read_router` mounts `pp_api::plates::plates_router`, so
with `desktop_rebuild` and `PP_DESKTOP_REBUILD=1` or `true` the Rust-only start
(`rebuild_api::product_router`) serves every Plate route Node registers, at
`/` and `/api/v2` as Node does:

- `GET /plans/{id}/plates`
- `POST /plans/{id}/plates/initialize`, `.../arrange` and `.../restore`
- `PATCH /plans/{id}/plates/{plateId}/units/{token}` and `.../pin`
- `POST /plans/{id}/plates/{plateId}/units/{token}/unplace` and `.../transfer`

They move together because they all write the same `accepted_plate_*` tables.
A request is one storage transaction on the single writer. Nothing falls back
to Node, and no Plate route is left on Node beside a Rust one. Bodies follow
Node's limits: 8 MiB for initialize (`MAX_BULK_JSON_BODY_BYTES`) and 1 MiB for
the other writes (`MAX_JSON_BODY_BYTES`). A larger body is 413 `Request body too
large` before storage runs. The flag-off default still serves these routes
from Node, and the gateway manifests still name Node as their owner. Making
Rust the default is the gateway cutover (GRE-593).

`plates_http` drives initialize, its replay, arrange, the body limits and the
flag through the router.

```sh
cargo nextest run --locked --manifest-path rust/Cargo.toml --no-tests=fail --features pp-storage/desktop_rebuild,pp-api/desktop_rebuild -p pp-storage -p pp-api -E 'test(ticket_t_61_plates) | binary(plates_http)'
```

### Production setup in Rust (GRE-1209)

GRE-421's first part. `pp_storage::plates` serves Node's `GET` and
`PATCH /plans/{id}/production-setup` as `Request::ProductionSetup`, and
`pp_api::plates::plates_router` mounts both at `/` only, because Node
registers no `/api/v2` alias for them. With `desktop_rebuild` and
`PP_DESKTOP_REBUILD=1` or `true`, Rust alone runs the command that writes
`production_setup:<id>`. Rust already deletes the key with its Build. Node's
duplicate still copies it to the new Build's own key until the duplicate part
of GRE-421 moves, so the two runtimes never write the same key.

The route checks in Node's order:

1. The id is `Number(id)` as a positive integer (so `0x1` and `" 1 "` name
   Build 1), else 400 `invalid Build id`.
2. The Build must exist in the tenant, else 404 `Build not found`. An archived
   Build still saves, as in Node.
3. The PATCH body must be one `productionSetupCommandSchema` command, else 400
   `Invalid production setup` with Zod's `issues`: the same paths and
   messages, in Zod's order.
4. A `set_route` that changes a route already chosen is 409
   `production_method_locked` when a Printer Checkoff link of this Build that
   is not dismissed names, after the slicer's instance suffix, a Required unit
   the next package makes. The next package is the custom selection's available
   tokens, or every incomplete unit. When the Accepted Plan state cannot be
   read, it is 503.

A stored setup that fails `productionSetupSchema`, or names another Build, reads
as Node's default. A saved setup is Zod's output: trimmed text, unknown keys
dropped, and the schema's key order. Node retries its setting compare-and-set
and answers 409 after eight conflicts. Here the read, the lock check and the
write share one immediate transaction on the single writer, so no conflict can
occur. The body limit is Node's 8 MiB (`MAX_BULK_JSON_BODY_BYTES`), and a body
that is not JSON is 400 `Invalid JSON body`.

`fixtures/parity/production-setup/node.json` is Node's answer for 82 cases in
22 phases. `ticket_t_61_build_lifecycle_production_setup_matches_node` replays
the 78 that reach storage and compares the status, the body, the changed tables
and the stored text, then reopens the writer. The other four are the
`/api/v2` alias and Fastify's body parser, which `plates_http` covers at the
router. Regenerate the golden with
`node fixtures/parity/production-setup/generate.mjs`.

```sh
cargo nextest run --locked --manifest-path rust/Cargo.toml --no-tests=fail --features pp-storage/desktop_rebuild,pp-api/desktop_rebuild -p pp-storage -p pp-api -E 'test(ticket_t_61_build_lifecycle) | binary(plates_http)'
```

### Build header edit and archive in Rust (GRE-1231)

GRE-421's second part. `pp_api::builds::build_router` serves Node's
`PATCH /plans/{id}` and `POST /plans/{id}/archive` at `/`, `/api/v1` and
`/api/v2`, both as `BuildCommand::Update` in `pp_storage::build_graph::header`
(an archive is an update with only `archive` set). With `desktop_rebuild` and
`PP_DESKTOP_REBUILD=1` or `true`, Rust alone writes a Build's name, special
request, `archived_at` and `last_used_at` through these routes. Draft headers
and Accept's header copy stay where GRE-1207 put them.

A PATCH checks in Node's order:

1. `archived: false` is 400 `Cannot unarchive; duplicate the archived template
   instead`, before the Build is looked up.
2. The id is `Number(id)` as a positive integer (through `plates::build_id`),
   and the Build must be the tenant's, else 404 `Profile not found`.
3. On `/api/v1`, a Build whose accepted progress is unavailable gets the legacy
   refusal before anything is written.
4. A `special_request` key needs the `orders` feature: 403 `FEATURE_DISABLED`,
   or 500 `FEATURE_TOGGLES_INVALID` when the stored toggles do not read.
5. A string `name` is trimmed. Blank is 400 `Profile name is required`. A name
   another Build has is the Build rename rule Accept uses:
   409 `{"code":"duplicate_build_name","name":...}` (Node answered 400
   `Profile already exists`). The web client's `engineFetch` shows that 409 as
   `Profile already exists: <name>`, the message Node gave. A `name` that is not a string is ignored.
6. `special_request` is `String(value)` trimmed, and blank or `null` clears it.
   When `String()` throws (an object with its own `toString` member, or an
   array holding one), any rename stays saved and the answer is Node's: 500
   `Internal Server Error` beside an archive, else 400 `Cannot convert object
   to primitive value`.
7. `archived: true` archives as below, then `touch_last_used: true` sets
   `last_used_at`.

As in Node, each step writes as it goes: a rename saved before an archive
refusal stays saved. The answer is the Build summary on the mount's contract.

An archive keeps an archived Build as it is. Otherwise every Part of the
accepted Plan needs one progress row per unit (Node's `assertCompleteProgress`,
through `checkoff_progress::complete_rows`; a missing row is 500 `Accepted Plan
data is inconsistent`), and the Plan must have units and none remaining (else 400 `Archive only when print remaining is
0`). Compatibility-dirty or uninitialized progress is 409, and an integrity
failure is 500 `Accepted Plan data is inconsistent`.

`fixtures/parity/build-header/node.json` is Node's answer, with
`PP_DESKTOP_REBUILD=1`, for 58 requests in 14 phases on copies of
`accepted-plate-node.db`. `build_header_patch_and_archive_match_node` replays
it through `build_router`: 51 cases match Node's status, body and changed
tables, after the replay removes GRE-1100's `customer` and `due_date` from
each Rust summary (Node's summary has neither, and the replay requires both to
be present and null). Seven are declared corrections that write nothing: the two
duplicate-name renames (409 above), a blank name beside an archive (400, where
Node answered 500), a PATCH or archive body that is not JSON under
`Content-Type: application/json` and an empty archive body under that type
(400 `Request is invalid` without Fastify's parser codes), and a `null` PATCH
body (400 `Request is invalid`, where Node answered 500). An archive otherwise
ignores its body. Like the other Rust routes, it does not reproduce Fastify's
415 for a body under a content type Fastify has no parser for, so that request
archives. The web client always sends `{}` as JSON.

```sh
cargo nextest run --locked --manifest-path rust/Cargo.toml --features desktop_rebuild --no-tests=fail -p pp-api --test build_header_http
```

### Variant dimensions and selection in Rust (GRE-1221)

GRE-421's sixth part (GRE-1221). `pp_api::builds::build_router` serves Node's
`GET /plans/{id}/variant-dimensions` (with `HEAD`) and
`POST /plans/{id}/variant-selection` at `/`, `/api/v1` and `/api/v2`, as
`BuildCommand::Variants` in `pp_storage::build_graph::variants`. With
`desktop_rebuild` and `PP_DESKTOP_REBUILD=1` or `true`, Rust alone writes
`plan_variant_selection_<id>` and the base Source's import rules for a
selection. The import-rules write is `catalog::save_import_rules`, the same
code `PUT /sources/{id}/import-rules` runs, so both routes store the same
normalized rules and mark the same Plans modified.

The read takes the Build, its base Source (the lowest `layer_order` base
layer), that Source's `local_path` and the stored selection in one read
transaction. It then reads `print-partner.manifest.yaml` through the
production `SourceReads` adapter, outside any transaction, as the manifest
builder does. Dimensions are empty when the Build has no base Source, the
Source has no checkout, the manifest is missing or unreadable, or the manifest
does not load (Node's `loadManifestYaml` throws, for example on an empty
`selections` list, or a mapping with its own `toString` key where Node calls
`String()` on it). Otherwise each entry of `variant_dimensions` whose value is
a list keeps its strings and numbers in JavaScript property order; a list
`variant_dimensions` reads as index keys, a `__proto__` entry is dropped (Node's
assignment calls the prototype setter), and a non-finite number is `null` in
JSON. The stored selection is returned as `JSON.stringify(JSON.parse(text))`,
or `{}` when it is missing, empty or not JSON.

A selection checks in Node's order:

0. Under `Content-Type: application/json`, an empty body, a body that is not
   JSON, or one Fastify's parser refuses as prototype poisoning (a `__proto__`
   key, or a `constructor` key whose object has a `prototype` key) is 400
   `Request is invalid` before the id is read, as Fastify answers before Node's
   route runs. Like the other Rust routes, a body under another content type is
   read as JSON rather than refused with Fastify's 415.
1. The id is `Number(id)` as a positive integer (so `0x3`, `3e0` and `" 3 "`
   name Plan 3, through `plates::build_id`), else 404 `Profile not found`,
   before the body is read. The dimensions read does the same.
2. The body is `{ selection?: Record<string, string>, source_id?: positive safe
   integer }` (no body or `null` is `{}`), else 400 `Invalid variant
   selection`.
3. Without a base Source, 400 `No base source on plan`; a `source_id` other
   than the base Source, 400 `Source must match the plan's base source`.
4. Each selected dimension, in JavaScript property order, must be a dimension
   (400 `Unknown variant dimension: <name>`) and its value must equal
   `String(option)` for one option (400 `Unsupported option for variant
   dimension: <name>`). `String()` is JavaScript's, so `2.0` is `"2"` and
   `.nan` is `"NaN"`.

One immediate writer transaction then rechecks the Build and its base Source,
stores the selection as `JSON.stringify` writes it, and saves each non-empty
value plus `/` as an import rule. The stored selection and the response keep a
lone UTF-16 surrogate, but an import rule carries U+FFFD in its place, so
`imported_paths` stays readable by Rust's `serde_json` readers of that column;
Node stores the escape. The refusal detail for an unknown dimension does the
same. A read failure other than a missing or
unreadable file, such as an exhausted `PreparationLimits` budget (each
copied dimension name and text option, aliases included, is charged to it), is 503
`source_observation_failed`, as for the manifest builder; Node has no limits.

`fixtures/parity/variants-http/node.json` is Node's answer for 78 requests on
one database. `variants_flat_v1_v2_match_node_responses_and_changed_tables`
in `ticket_t_10_fixtures` replays every case through `build_router` over the
production `SourceReads`: 74 match Node's status, body and changed tables, and
four bodies Fastify's parser refuses (poisoned, not JSON, or empty under
`application/json`, including on a missing or non-numeric Plan) are declared
corrections (400 `Request is invalid` without Fastify's parser codes);
and after each of the 18 saves the stored selection text and the Source's
`imported_paths` equal Node's byte for byte. Regenerate the golden with
`node fixtures/parity/variants-http/generate.mjs`.

```sh
cargo nextest run --locked --manifest-path rust/Cargo.toml --features desktop_rebuild --no-tests=fail -p pp-api -E 'test(variants_flat)'
```

### Plan features and decision append in Rust (GRE-1243)

GRE-421's seventh part (GRE-1243). `pp_api::builds::build_router` serves Node's
`GET` and `PATCH /plans/{id}/features` and `POST /plans/{id}/decisions` at `/`,
`/api/v1` and `/api/v2`, as `BuildCommand::PlanFeatures` and
`BuildCommand::AppendDecision`. With `desktop_rebuild` and
`PP_DESKTOP_REBUILD=1` or `true`, Rust alone reads and writes
`feature_toggles:plan:<id>` through these routes and alone appends
`plan_decisions` rows. With the flag off the router mounts nothing and Node
keeps both. `GET /plans/{id}/decisions` stays in the planning router
(GRE-1119), and `/settings/features`, the tenant layer, stays Node's.

A features request checks in Node's order:

0. The credential is checked first, as Node's `onRequest` hook does. Then a
   PATCH body Fastify's parser refuses (empty, not JSON, or a `__proto__` or
   `constructor.prototype` key under `application/json`) is 400 `Request is
   invalid`. A decision append refuses those bodies at the same point.
1. The id must match `/^[1-9]\d*$/` and be a safe integer, and a PATCH body
   must be an object whose keys are all features (`farm`, `orders`, `bambu`,
   `spoolman`) and whose values are all booleans or `null`. Otherwise the
   answer is 400 `Invalid plan feature input` once the credential checks. So
   `0x1`, `01` and `1.0` are 400 here, unlike the `Number()` ids of the other
   Build routes.
2. The Plan must be the tenant's, else 404 `Plan not found`.
3. The tenant layer `feature_toggles` and the Plan layer must both be JSON
   objects whose feature keys hold booleans, else 500
   `FEATURE_TOGGLES_INVALID`.

A patch applies in registry order (`farm`, `orders`, `bambu`, `spoolman`):
`null` removes the key, a boolean replaces it in place or appends it. Layers
are read as Node's `parseFeatureLayer` reads them
(`pp_storage::source_scan::node_json::RawObject`): unknown keys, JavaScript
property order and each number's source text (`1.50`, `1E+2`, `-0`,
`12345678901234567890`) survive, and strings are re-escaped as
`JSON.stringify` writes them. One immediate writer transaction checks the
Plan, reads both layers and writes the Plan layer's text even when it is
unchanged or empty, so an emptied layer is `{}` and the row is never deleted.
The read and the write share that transaction, so Node's 409 compare-and-set
conflict cannot occur. `FeatureClient`'s Plan-scope `set` writes the same
way; its tenant-scope `set` keeps sorted keys and deletes an empty row, as
before, because Node owns that operation. The answer is Node's
`{profile_id, overrides, effective}`, where `effective` gives each feature's
`enabled` and its `source` (`plan`, `user` or `default`).

A decision append reads the id with `Number()`, so `0xc` and `12.0` name Plan
12. A missing, foreign or non-numeric Plan is 404 `Profile not found` before
the body is read. `kind` defaults to `user_note` when it is absent or `null`
and must be `applied_action`, `dismissed_action`, `user_note` or `choice`,
else 400 `Invalid kind`. `actor` is `assistant` only for exactly that string,
else `user`. `params` (default `{}`) and `result` (default `null`) are stored
as `JSON.stringify` writes them. Node hands `action_type`, `label`, `summary`
and `rationale` to better-sqlite3 unchecked, with only `null` falling back to
`""` or `null`, and Rust binds them the same way:

- A number is bound as a double that the TEXT column stores as text: `5` is
  `"5.0"`, `1e21` is `"1.0e+21"` and `-0` is `"0.0"`. A lone surrogate becomes
  U+FFFD.
- An array is spread into the parameter list, so `["x"]` stores `"x"`, and
  `[]` beside `["a","b"]` shifts the later values into the neighbouring
  columns as Node does. An object is a named-parameter bag that binds nothing.
- A boolean, or an array element that is not a string, number or `null`, is
  400 `SQLite3 can only bind numbers, strings, bigints, buffers, and null`. A
  second object is 400 `You cannot specify named parameters in two different
  objects`. Any count other than eleven parameters is 400 `Too few parameter
  values were provided` or `Too many parameter values were provided`;
  a twelfth bindable value stops binding there, before any later field is
  checked. A NOT NULL failure is 400 with SQLite's message.

The answer is the inserted row as Node's `mapPlanDecision` returns it. The
append writes only `plan_decisions`: no publication row or blocker.

Both routes follow Fastify's `text/plain` parser, which hands the route the
body as a string. A `text/plain` feature patch is therefore 400 `Invalid plan
feature input` whatever it says, and a `text/plain` decision has no fields, so
it appends the defaults, as `{}` does. Any other content type is read as JSON,
as on the other Rust Build routes.

`fixtures/parity/plan-features-decisions/node.json` is Node's answer for 114
requests on one database, run with `PP_DESKTOP_REBUILD=1` except four with it
unset. `plan_features_decisions_flat_v1_v2_match_node_responses_and_changed_tables`
in `ticket_t_10_fixtures` replays them through `build_router` merged with
`planning_router`, as `pp-core` composes them. 102 match Node's status, body
and changed tables. Eight bodies Fastify's parser refuses are declared
corrections (400 `Request is invalid` without Fastify's codes). The four
flag-off cases reach a router built with the flag off, which mounts nothing:
404 and nothing written, where Node answers its own 404 `Plan not found` for
features and still appends the decision. After each of the 34 writes, the
Plan's stored layer text or the decision rows it added equal Node's byte for
byte, and every 200 body also has Node's key order (error bodies come from the
shared problem builder, which writes `code` before `detail`). A malformed body
sent with an unknown session is 401 and writes nothing. Regenerate the golden with
`node fixtures/parity/plan-features-decisions/generate.mjs`.

```sh
cargo nextest run --locked --manifest-path rust/Cargo.toml --features desktop_rebuild --no-tests=fail -p pp-api -E 'test(plan_features_decisions) | test(plan_feature_ids)'
```

### D21 accepted read JSON bodies, GRE-616 PR 1

With `desktop_rebuild`, `AcceptedRead`, `Snapshot` and `Batch` each have an
`into_json_body` method. It returns the same bytes as the derived `Serialize`
output, except for Part manifest metadata. Each accepted `Part` now carries its
`PartManifestMetadata`, and the body writes `requirement` and `optionGroupId`
from those UTF-16 units. Text that is not valid Unicode keeps each unpaired
surrogate as a `\uXXXX` escape, as `JSON.stringify` does. The derived output
writes `null` for that text. No routes call these methods yet; D23 to D26 wire
HTTP behind `PP_DESKTOP_REBUILD`.

D22, GRE-618, lifts the temporary snapshot refusal for non-scalar manifest
text by moving Review to the same lossless carrier, as described below.

```sh
cargo test --locked --manifest-path rust/Cargo.toml -p pp-storage --features desktop_rebuild --lib read_model::json_body
```

### D21 published capture, GRE-616 PR 2

With `desktop_rebuild`, `read_model::capture_published` reads the Accepted Plan
and its Build context in one transaction after a committed Save. It fails when
the Build is missing, when the Accepted Plan is not ready, or when the snapshot
Plan version is older than the Save receipt. Integrity failures propagate
unchanged. The D13 Save port (GRE-608) is its first caller.

Two other reference differences are not ported, because main is already
correct:

- Accepted progress. The reference counts units inline. Main uses
  `views::progress`, which gives the same counts: units of included Parts, and
  the units not yet completed. A test pins these counts in both profile
  summaries.
- Naming folder rules. The reference omits `functional_class` when it is unset,
  which changes the effective naming digest. Node `digestEffectiveNaming` writes
  `"functional_class":null`, and main does the same. Porting the change would
  mark every Build with such a rule stale. A test pins the Node digest.

```sh
cargo test --locked --manifest-path rust/Cargo.toml -p pp-storage --features desktop_rebuild --lib read_model::capture_published_tests
cargo test --locked --manifest-path rust/Cargo.toml -p pp-storage --test ticket_t_61_read_model
```


### D22 Review JSON and workflow

GRE-618 completes the workflow and views delta from
`desktop/source-sync-local-current-20261008`. Workflow already matches that
reference, so its resolver stays unchanged. Main retains its catalog and
collation fallbacks instead of the reference's panic behavior.

With `desktop_rebuild`, `views::review_json` returns `ProjectReviewJsonBody`.
Its `into_bytes` preserves `requirement` and `option_group_id` from each Part's
UTF-16 manifest metadata, including unpaired surrogates as JSON escapes.
`summary_json` returns an optional `ProjectReviewSummaryJsonBody` with its own
`into_bytes`. Ready and empty reads have summaries; unavailable reads do not.
The summary uses scalar fields and does not serialize Part metadata.
Accepted snapshot reads now admit valid non-scalar metadata. Invalid PPJS
encodings, projection mismatches and digest failures remain refusals.

Default builds retain `views::review` and `views::summarize_review`. The fixture
binary selects the matching API at compile time. No HTTP routes or runtime
callers are added. Future product callers must check `PP_DESKTOP_REBUILD` and
send these JSON bytes directly, without parsing them through `serde_json::Value`,
which cannot hold unpaired surrogates.

```sh
cargo test --locked --manifest-path rust/Cargo.toml -p pp-storage
cargo test --locked --manifest-path rust/Cargo.toml -p pp-storage --features desktop_rebuild
```

### D23 auth HTTP wiring, GRE-617 PR 1

Main's `pp-api` `auth/http.rs` and its `auth_http` tests are newer than
`desktop/source-sync-local-current-20261008`. They keep the reset delivery
window, health `degraded_detail`, `hasPassword` and the no-panic rewrites. No
handler code is ported back from the reference.

`pp-api` gains a `desktop_rebuild` feature. `auth_router` accepts an
`AuthBackend`: the existing pooled `AuthClient`, or, with the feature, D6's
`RebuildAuthClient`. Existing callers pass `AuthClient` unchanged.

With `desktop_rebuild`, `pp_core::rebuild_api::router(rebuild, owner, routes)`
is the single place that registers Rust auth routes. It returns `None` before
creating any client unless the stored `DesktopRebuild` setting is on. When it
is on, it selects `rebuild_auth_with_policy` with the caller's lifetimes, as D6
requires. Default builds do not compile the module.

The desktop gateway does not mount this router. The desktop process holds the
storage lease that `WriterOwner::open` needs, and Node owns those writes until
cutover. Gateway mounting belongs to D26.

```sh
cargo test --locked --manifest-path rust/Cargo.toml -p pp-api -p pp-core
cargo test --locked --manifest-path rust/Cargo.toml -p pp-api -p pp-core --features desktop_rebuild
```

### Password-change response parity, GRE-873

`fixtures/parity/auth-password-change/` records Node's public
`POST /auth/change-password` status and body together with whether the old and
new passwords still authenticate. The corpus covers missing cookies, wrong
current passwords, valid changes, malformed fields, and nonempty invalid or
revoked cookies (including empty bodies). Rust resolves the session before
field validation so those stale cookies return
`401 {"detail":"Authentication required"}` instead of a 400 field error.
Password and session policy stay unchanged. The focused suite requires
`desktop_rebuild`.

```sh
node fixtures/parity/auth-password-change/generate.mjs --check
cargo test --locked --manifest-path rust/Cargo.toml -p pp-api --features desktop_rebuild --test auth_password_change_parity
```

### D24 drafts HTTP, GRE-619

`pp-api` `drafts/http.rs` compiles with `desktop_rebuild`.
`DraftHttpConfig::new` enables the router only when
`PP_DESKTOP_REBUILD` is exactly `1` or `true` at process start. Otherwise
`draft_router` and `admit_draft_routes` mount nothing and every draft path
answers 404.

Landed replacement slices for the superseded #635 monolith:

| Slice | Linear | Scope |
| --- | --- | --- |
| a1 | GRE-939 | Loopback Host/Origin admission and credential presentation ([#969](https://github.com/poitee/PrintPartner/pull/969)) |
| a2 | GRE-944 | Request target, body size, and body-read timeout bounds |
| b | GRE-940 | List and workspace reads |
| c | GRE-941 | API-key `ResolveKey` so `lastUsedAt` commits; foreign-tenant keys 401 |
| d | GRE-942 | Part edits with Node conflict shapes |
| e | GRE-988 | Abandon with storage lifecycle token |
| f | GRE-1093 | Accept preview through Accept's own checks |
| g | GRE-416 | Required-unit reconciliation (`PUT .../reconciliation`) |

With both gates on, `draft_router` serves these paths at the root and
`/api/v2`:

| Method | Path | Storage |
| --- | --- | --- |
| GET | `/plans/{id}/drafts` | `WorkingDraftClient` list |
| GET | `/plans/{id}/drafts/{draft_id}` | workspace read |
| GET | `/plans/{id}/drafts/{draft_id}/accept-preview` | read-only Accept preview |
| PATCH | `/plans/{id}/drafts/{draft_id}/parts` | `edit` with typed `EditRequest` |
| PUT | `/plans/{id}/drafts/{draft_id}/reconciliation` | `ReconciliationClient::reconcile` with typed `ReconciliationRequest` |
| POST | `/plans/{id}/drafts/{draft_id}/abandon` | `abandon` with typed `AbandonRequest` |
| POST | `/plans/{id}/drafts/save` | draft-only autosave, `WorkingDraftClient::autosave` (GRE-418) |
| GET | `/plans/{id}/drafts/header` | Build header and Accepted base before any draft (GRE-1120) |
| GET | `/plans/{id}/drafts/{draft_id}/header` | draft header beside the Build's header (GRE-1197) |
| PUT | `/plans/{id}/drafts/header` | draft header save, `WorkingDraftClient::save_header` (GRE-1197) |

`admit_draft_routes` buffers each body before routing, disables Axum's 2 MiB
default, and applies the guard's limits: 16 KiB request target, 1 MiB for
reads, 8 MiB for PATCH and PUT, 5 s body timeout, 64 admission permits. Loopback
transport is required. Host must match the configured listener; forwarded and
`x-forwarded-*` headers are refused. Session-cookie writes (PATCH/PUT/POST) need a matching
Origin; GETs and API-key writes may omit it, but any Origin sent must match.
A foreign or `null` Origin, or `sec-fetch-site: cross-site`, returns 403.
Ambiguous credentials return 401, or 403 on an Origin-less write. Exactly one
of session cookie, `Authorization: Bearer`, or `x-print-partner-api-key` is
accepted. API keys use the configured tenant, not a request header.

List returns `{ profile_id, drafts: [{ draft_id, state, lifecycle_version,
snapshot_digest, base }] }`. Workspace returns the storage workspace JSON.
Missing profile or draft returns 404 with Node `detail` and `code`
(`profile_not_found` / `draft_not_found`).

PATCH bodies are typed `EditRequest`. Invalid JSON, unknown fields, empty
decisions, zero part IDs, and out-of-range quantities return 400
`invalid_request` before any mutation. Successful edits return 200 with the
workspace. Conflicts match Node `plan-drafts.ts`: 409 is `{ code }` plus
`workspace` only when one exists (`draft_changed`, `base_changed`,
`not_open`, `accepted_baseline_required`, `idempotency_conflict`) with no
synthetic conflict `detail`; 422 `domain_error` is `{ code }` only. 404 and
503 keep their Node `detail`. Server failures use
`{ detail: "Plan draft update failed", code: "internal_error" }`.

`POST /plans/{id}/drafts/{draft_id}/abandon` takes
`{ expected_lifecycle_version }` (`AbandonRequest`). Success returns 200 with
the draft identity (`state: abandoned`, lifecycle advanced). A retry with the
same expected lifecycle version is idempotent (`Ready`). A stale token returns
409 `{ code: "draft_changed", workspace }` with no synthetic `detail`; a
consumed or already-closed draft that is not the idempotent retry returns
409 `{ code: "not_open", workspace }`. Invalid bodies return 400
`invalid_request` before mutation. Server failures use the same
`Plan draft update failed` detail as edit. The desktop gateway does not mount
this router; D26 does.

`PUT /plans/{id}/drafts/{draft_id}/reconciliation` (GRE-416) saves and selects
a Required-unit reconciliation, as Node's route of the same path does. The
`Idempotency-Key` header is required: a missing key, or one that is blank or
longer than 160 characters after trimming spaces and tabs, returns 400
`invalid_request` before the body is read. The body is
`{ expected_snapshot_digest, decisions }` (`ReconciliationRequest`):
`select_exact_predecessor`, `accept_prior_completion` or `replace` decisions,
at most 10,000, one per draft Part. A malformed body returns 400 before any
write. The route reuses `WorkingDraftClient`'s writer and auth policy through
`WorkingDraftClient::reconciliation`, so the selection runs in one immediate
transaction on the single writer and nothing is sent to Node.

- 200 returns the draft workspace with its `reconciliation` summary. The
  selection commits with one `plan.draft_changed` domain event
  ([Domain events](../domain-events.md)).
- A retry with the same actor, key and payload returns the stored selection and
  writes nothing, before the snapshot digest is checked. The one exception is
  Node's repair: an open v1 draft whose selection was detached gets that
  selection back, and the repair still writes no domain event. The same key with
  another payload returns 409 `{ code: "idempotency_conflict" }`.
- Edit's conflicts apply: 409 `draft_changed`, `base_changed`, `not_open` or
  `accepted_baseline_required`, 422 `domain_error`, and 404
  `profile_not_found` or `draft_not_found`. When another connection holds
  the SQLite write lock past the writer's admission deadline, the route answers
  503 `transaction_unavailable`, as edit does.
- A decision that does not apply to the draft makes Node's service throw, so
  both routes answer 500 `Plan draft update failed`. The transaction rolls
  back every reconciliation, decision and assignment row and writes no event.


### Draft recompute, rebase and Accept (GRE-417)

The draft router also serves Node's three remaining draft writes under the
same flag, at the root and under `/api/v2`. Each needs an `Idempotency-Key`
header (trimmed, 1 to 160 characters). A request without a usable key gets 400
`invalid_request` before its body is read. Each request runs on the
`WorkingDraftClient` writer, and nothing is sent to Node.

- `POST /plans/{id}/drafts/recompute` takes `{ apply_manifest: boolean }` and
  runs `WorkingDraftClient::recompute_and_select`. It abandons every other open
  draft, creates the new draft, and selects Required units as an edit does. It
  answers with the workspace. A replay with the same key returns the same
  draft and writes nothing. `no_layers`, `no_stls` and `would_wipe` are 422
  `{ code }`. `accepted_baseline_required`, `base_changed`, `inputs_changed` and
  `idempotency_conflict` are 409. A missing Plan is 404 `profile_not_found`.
- `POST /plans/{id}/drafts/{draft_id}/rebase` takes `RebaseRequest` and runs
  `WorkingDraftClient::rebase_and_select`. A rebased or existing draft is selected
  and answered as recompute's is. As in Node's `rebaseFailure`, a source whose
  lifecycle or digest changed is 409 `draft_changed` with the source's workspace,
  and a source whose state differs from `expected_source_state` is 409
  `not_open` with its workspace. `merge_conflicts`
  is 422 `{ code, conflicts }`. `base_unchanged` is 409, alongside recompute's
  409s and 422s.
- `POST /plans/{id}/drafts/{draft_id}/apply` (Accept) takes `ApplyRequest` and runs
  `PublicationClient::apply` through `WorkingDraftClient::publication`. 200 is
  the receipt `{ profile_id, draft_id, revision_id, plan_version,
  draft_lifecycle_version, revision_digest, required_unit_mapping_digest,
  applied_at }`, also for a replay with the same key or an already consumed
  draft. When Accept answers `reconciliation_required`, the route runs Node's
  `prepareForApply` and retries once under the same key with the prepared
  draft's revision. That means checking the draft against the request,
  answering 409 `not_open`, `draft_changed` or `base_changed` with its
  workspace, and otherwise selecting Required units. `draft_changed`,
  `base_changed` and `not_open` from Accept carry the draft's current workspace.
  `reconciliation_required` is 422 `{ code, reason }`. `production_active` is
  423 with `checkoff_link_count` and `send_queue_item_count`.
  `checkoff_remap_unsafe` is 422 with `unmappable`. `build_archived` and
  `token_allocation_failed` are 422 `{ code }`. Rust's `execution_conflict`,
  which Node has no counterpart for, is 409 `{ code, operations }`, and so is
  `duplicate_build_name` (`{ code, name }`), a draft header naming another
  Build ([Draft header fields](../draft-headers.md#accept)), and
  `duplicate_order` (`{ code, order_number, builds }`), a draft header giving
  the Build an order number another unarchived Build holds
  ([Order authority](../draft-headers.md#order-authority-gre-1095)). A missing Plan
  is 404 `profile_not_found` and a missing draft is 404 `draft_not_found`.
  Only Accept publishes. Recompute, rebase and autosave never write an
  Accepted Plan.
  Before it publishes, Accept saves the snapshot its preview promises: one
  `plan_snapshots` row, `source` `pre_apply`, named `Before accepting draft
  <draft_id>`, in the same transaction. It holds the Accepted Plan being
  replaced. Its `layers` come from that revision's `plan_revision_inputs`
  (type, Source id, current Source name and order), not from `profile_layers`:
  Accept publishes the working layers, so they already match the new Plan. The
  payload has Node's `capturePlanSnapshotPayload` shape plus
  `accepted_plan_revision_id` and `accepted_plan_version`. Accepted revisions
  record no kit manifest, Source tag or branch, so `kit_manifest`, `tag`,
  `branch` and `recipe_summary` are `null`, and restoring the row brings back
  the replaced Plan's Source layers while keeping the current kit manifest and
  Source refs. The first Accept, a replaced revision with no input set, one
  with an input whose Source row is gone, and one whose migrated input set
  holds `legacy:<id>` layers have nothing restore can place as recorded, so
  they save none, and the preview reports `saved_before_accept: false` for
  them. An empty input set is a Plan with no Source layers; its snapshot has
  `layers: []`, which restores that. A replay, an already consumed draft and
  every refusal save none either. Beside the snapshot, Accept saves the Build's
  header as it stood in `plan_snapshot_headers` (schema step 45), and a draft
  header row is copied to the Build
  ([Draft header fields](../draft-headers.md#accept)).

Contention past the writer's admission deadline is 503
`transaction_unavailable`. Any other storage failure is 500
`Plan draft update failed`, as it is for edit. Every draft write in these
operations appends `plan.draft_changed` in its own transaction
([Domain events](../domain-events.md)).

`POST /plans/{id}/progress/import` (Node's accepted Progress import) is mounted
on the Checkoff router (`pp_api::checkoff`). It takes `{ expected, rows }` and sets
each listed Part's printed count against the Accepted Plan the caller
observed, with the 8 MiB body limit Node's bulk routes use. A body for
another Plan is 404 `progress_target_not_found`. A stale basis is 409
`stale_accepted_plan`. Storage's other refusals keep their Node detail and code.

```sh
cargo nextest run --locked --manifest-path rust/Cargo.toml --no-tests=fail --features pp-storage/desktop_rebuild,pp-api/desktop_rebuild -p pp-storage -p pp-api -E 'test(ticket_t_61_publication)'
```

With `desktop_rebuild` and `PP_DESKTOP_REBUILD=1|true`, the Rust-only start
(`CoreRuntime::start_for_launcher`) serves recompute, rebase, Accept and
progress import through `rebuild_api::product_router`, which merges the draft
router into the read router that carries Checkoff. The flag-off default still
serves those four from Node. Making Rust the default is the gateway cutover (GRE-593).

### Draft-only autosave (GRE-418)

`POST /plans/{id}/drafts/save` writes Plan choices into the observed open
draft, or into a new final-state draft when none is open, with one
Required-unit reconciliation whose row is the durable receipt, all in one
immediate transaction. It needs an `Idempotency-Key` like the routes above,
replays before the stale-base check, refuses a changed payload under a reused
key, and never publishes. [Plan autosave](../plan-autosave.md) has the contract.

This is a separate operation from Node's `POST /plans/:id/save`, which
publishes. That path stays on Node with its publishing semantics until UI
slice 6 moves the browser to draft-then-Accept, and no Rust router mounts it,
so Node and Rust never write the same operation. Node has no route at
`/plans/{id}/drafts/save`, which is why the gateway manifests do not list it.

### Draft header fields (GRE-1197)

`PUT /plans/{id}/drafts/header` saves a Build's name, order number, customer
and due date into the observed open draft, or into a new draft when none is
open, in one immediate transaction. It needs an `Idempotency-Key`, writes
nothing to the Build, and Accept publishes it.
`GET /plans/{id}/drafts/{draft_id}/header` reads the draft's header beside the
Build's current one. [Draft header fields](../draft-headers.md) has the contract,
schema steps 43 and 45, the draft paths that carry the header forward, and the
Accept copy.

`GET /plans/{id}/drafts/{draft_id}/accept-preview` reports what accepting the
draft's current revision would change, and writes nothing. It runs in one
deferred read transaction on the writer. Storage owns the checks:
`plan_publication::check_draft` and `check_selection` are the same functions
Accept (`publish_in_transaction`) runs before it writes, in the same order.

- A draft whose Parts and inputs equal its accepted base returns 200
  `{ kind: "no_change", revision, snapshot }`, but only when Accept, sent
  without `remap_checkoff_links`, would pass every check. Otherwise the preview
  returns Accept's refusal, such as `reconciliation_required`,
  `production_active` or `execution_conflict`. A changed draft reports active
  production and running operations as checks in `changes` instead.
- Otherwise 200 `{ kind: "changed", revision, changes, snapshot }`. The diff
  and the `no_change` decision use the Parts Accept would publish: a Part with
  a base takes `filament_color_id`, `filament_custom_hex` and
  `spoolman_spool_id` from its live accepted Part, as Accept does, and a new
  Part keeps its own. `changes` holds `fields` (each changed Part field, `old`
  and `new`), `parts_added`,
  `parts_removed`, `inputs` (changed Source input fields; each value is
  `{ kind: "text" | "integer" | "flag", value }` or `null`), `required_units`
  (`added`, `removed`, and `printed_affected`: printed units that Accept removes
  or whose Part changes), `plates.cleared` (the accepted Plates Accept clears),
  `stock` (`off`, or per Spoolman spool the unprinted unit count before and
  after), `header` (the Build header fields the draft changes, `old` and
  `new`), `duplicate_orders` (other unarchived Plans with the order number
  the Build has after Accept),
  `production` (active checkoff links and queued sends, with any link Accept
  could not remap), and `blocked_by` (running operations Accept refuses on).
- `revision` is `{ draft_id, expected_snapshot_digest,
  expected_lifecycle_version, expected_base }`, the fields Accept's request
  expects. Accept refuses a stale one with `draft_changed` or `base_changed`.
- `snapshot` is `{ saved_before_accept, detail }`. `saved_before_accept` is
  true when Accept will save the `pre_apply` snapshot described above, that is
  when the accepted Plan's revision has an input set whose layers are all
  `base` or `addon` with an existing Source. It is false, with a detail
  saying no snapshot is saved, on a Build's first Accept, when the accepted
  revision has no input set (a `legacy` revision), when one of its inputs
  names a Source row that is gone, and when its input set was migrated by
  schema v18 and holds `legacy:<id>` layers, which Node's restore would turn
  into addons with no base.
- A refusal is the one Accept would return, as an RFC 9457 problem
  ([Problem responses](../problem-responses.md)): 404 `draft_not_found` for a
  missing draft, otherwise 409 with no `detail`, Accept's outcome kind as
  `code`, and its fields as extension members, for example
  `{ code: "already_applied", receipt }` for a draft Accept already consumed,
  `base_changed`, `inputs_changed`, `build_archived`, or
  `{ code: "reconciliation_required", reason }`. Accept's idempotency-key
  lookup has no preview counterpart, since a preview sends no key.
- Refusals come in Accept's order, through the same storage functions:
  `already_applied`; then the Plan (`build_archived`) before the draft
  (`draft_not_found`, `not_open`), so a missing draft on an archived Plan is
  `build_archived`; then `draft_changed`, `accepted_baseline_required`,
  `base_changed`, `inputs_changed`, the draft header's `duplicate_build_name`
  and `duplicate_order`, and `reconciliation_required`; then, for
  `no_change` only, `production_active`, `checkoff_remap_unsafe` and
  `execution_conflict`. Token allocation can still fail inside Accept's
  write; a preview cannot predict that.

Draft rows carry Parts and inputs only, so Plan name, order number, and other
Plan header fields never appear in `fields`.

```sh
cargo nextest run --locked --manifest-path rust/Cargo.toml --no-tests=fail --features pp-storage/desktop_rebuild,pp-core/desktop_rebuild -E 'test(accept_preview)'
```

```sh
cargo test --locked --manifest-path rust/Cargo.toml -p pp-api --lib
cargo test --locked --manifest-path rust/Cargo.toml -p pp-api --features desktop_rebuild --lib drafts::http::tests -- --test-threads=2
```

### D30 catalog HTTP reads, GRE-804

`pp-api` gains `catalog`, compiled only with `desktop_rebuild`.
`CatalogHttpConfig::from_startup` keeps the router unmounted unless
`PP_DESKTOP_REBUILD` is exactly `1` or `true`. While it is unmounted, `GET` and
`POST /sources` answer 404 and do not open the catalog.

With both gates on, `GET /sources` and `GET /sources/{id}` are served at the
root and `/api/v1`. List returns `{ "sources": [...] }`. Detail returns that
caller’s catalog `SourceSummary`. A missing or non-canonical id returns 404
`Source not found`. The session cookie or one API key is required. API keys use
the configured tenant, not a request header. The same loopback, host, and
origin checks used by draft admission apply. Create, patch, and delete stay
unregistered. The desktop gateway does not mount this router; D26 does.

```sh
cargo test --locked --manifest-path rust/Cargo.toml -p pp-api --test catalog_http
cargo test --locked --manifest-path rust/Cargo.toml -p pp-api --features desktop_rebuild --test catalog_http
```

### D26 runtime wiring (a), GRE-818

`pp_core::rebuild_api::clients(rebuild, owner, policy, lifetimes)` composes the
opted-in auth, draft, and Build storage clients from the same `WriterOwner` and
stored `DesktopRebuild` flag that D23's auth router already uses. It returns
`None` before creating any client unless the flag is on. Catalog is not in the
bundle: GRE-819 mounts it with `admit_catalog_routes(config, owner)`, which
builds a credentialed catalog client per request. `RebuildHttpClients::close`
drops the owned handles; storage shutdown stays with `WriterOwner`. This slice
does not mount routes or change production ownership.

```sh
cargo check --locked --manifest-path rust/Cargo.toml -p pp-core --no-default-features
cargo test --locked --manifest-path rust/Cargo.toml -p pp-core --features desktop_rebuild \
  --lib application_host_ -- --test-threads=2
```

See `docs/gre-818-rebuild-clients.md`.

### D26 runtime wiring (b), GRE-819

`pp_core::rebuild_api::read_router` mounts auth, Plan builds, and catalog from
one `WriterOwner` when the stored rebuild flag is on. It uses the GRE-818 client
bundle plus `admit_catalog_routes(config, owner)`. Aliases share that owner.
`RebuildReadOwner::admit` fails closed for unknown names. Flag off returns
`None` before clients or mounts. Draft HTTP is GRE-820 (below); cutover remains
GRE-593.

```sh
cargo check --locked --manifest-path rust/Cargo.toml -p pp-core --no-default-features
cargo test --locked --manifest-path rust/Cargo.toml -p pp-core --features desktop_rebuild \
  --test rebuild_read_http -- --exact --test-threads=2
```

Focused tests: `rebuild_read_aliases_use_one_owner`, `unknown_read_owner_is_refused_before_execution`.

### D26 runtime wiring (c), GRE-820

`pp_core::rebuild_api::draft_router` mounts completed draft list/read/edit/abandon
routes from one `WriterOwner` when the stored rebuild flag is on. It uses the
GRE-818 client bundle's auth and draft handles. Root and `/api/v2` aliases share
that executor. `RebuildDraftOwner::admit` fails closed for unknown names. Flag
off returns `None` before clients or mounts. Rust draft failures stay on the
Rust path with no Node fallback. Cutover remains GRE-593.

```sh
cargo check --locked --manifest-path rust/Cargo.toml -p pp-core --no-default-features
cargo test --locked --manifest-path rust/Cargo.toml -p pp-core --features desktop_rebuild \
  --test rebuild_draft_http -- --exact --test-threads=2
```

Focused tests: `draft_aliases_and_workers_share_executor`,
`rust_draft_failure_never_falls_back_to_node`.

See `docs/gre-820-draft-mutations.md`.

### D26 web app shell, GRE-621

The web app reads the rebuild flag from one place,
`desktopRebuildEnabled` in `web/apps/web/src/lib/desktopRebuild.ts`. It is true
only when health advertises `tenant_feature_toggles`, which Node does only for
`PP_DESKTOP_REBUILD` set to exactly `1` or `true`. `JobContext`, `planReview` and
`PlanWorkspaceContext` call it in place of their copies of the same check.

While the flag is on, `AppLayout` sets `data-desktop-rebuild` on `<html>` and
removes it when the flag turns off or the shell unmounts. Rebuilt UI styles
(GRE-1008 onward) scope under `:root[data-desktop-rebuild]`, which also covers
dialogs and other portals. With the flag off the root gets no attribute and the
shell markup is unchanged. This slice adds no tokens, screens or routes.

```sh
cd web/apps/web
npx vitest run src/lib/desktopRebuild.test.ts src/layout/AppLayout.desktopRebuild.test.tsx
```

