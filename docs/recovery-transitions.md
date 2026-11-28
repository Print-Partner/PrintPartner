# Recovery transition inventory (GRE-1426)

Production stays on Node. The replacement launcher selects the Rust runtime
only with the rebuild gate enabled and a Rust-owned data directory. The
production procedure is the [whole-runtime switch](adr/0010-whole-runtime-cutover.md)
to a separate [Rust-owned copy](adr/0016-rust-owns-its-data-formats.md). Do not
use the old per-operation receipt to turn off a Node worker: it proves that
Node can be fenced, not that a live Rust worker, equivalent routes and safe
data are available. A Rust-owned route with no registered handler in the old
gateway returns `rust_owner_unavailable`; it does not fall back to Node.

## What must move together

These are the durable-job and Checkoff surfaces in scope, not a release
receipt. The [compatibility ownership manifest](../rust/crates/pp-gateway/ownership-manifest.json)
lists the exact Node aliases and participants; the
[Node fence list](../web/apps/server/src/data/node-fenced-participants.json)
identifies its non-HTTP participants. The Rust side uses the
[product router and workers](../rust/crates/pp-core/src/lib.rs) and the
[public API inventory](api.md). Neither inventory alone proves that both
runtimes do the same work or that replacement routes exist.

### Source Sync

The Node `source.sync` group includes more than a manual sync:

| Entry point | Node participant or job | Rust replacement / remaining evidence |
| --- | --- | --- |
| `POST /jobs/sync` and `POST /api/v1/jobs/sync` | `job.sync` | Both aliases enqueue `Sync` from one jobs router; the launcher starts one `SyncWorker` over that owner. This slice verifies both aliases below. |
| `POST /jobs/check-source-updates` and its `/api/v1` twin | `job.check-source-updates` | Rust classifies its recovery policy, but these POSTs are not mounted and `SyncWorker` does not claim this job. |
| `POST /jobs/import-scan` and its `/api/v1` twin | `job.import-scan` | These POSTs are not mounted. Do not treat Rust supplied-file ingestion as a port of the legacy scan. |
| `POST /jobs/extract-source-docs` and its `/api/v1` twin | `job.extract-source-docs` | These POSTs are not mounted. Source documentation reads do not imply a replacement for that worker. |
| Source create, manifest update, upload-files and upload-zip, and Source docs and cover reads, with their listed aliases | Source routes and Source revision workspace | Check catalog writes, the upload journal worker, and revision activation/reads together. An HTTP route or a read-only projection is not evidence that every writer moved. |
| Assistant apply aliases, `assistant.propose_import_source_files`, MCP `start_sync`, watcher start and worker, legacy manifest-override migration | Non-manual entry points and startup writers | A manual sync receipt does not cover these participants. Check each caller, startup writer and its data before calling the family complete. |

Rust's manual worker reads GitHub but publishes only to the local revision
workspace. Its `Sync` job is classified `requeue_local` and uses the Source
Sync journal and reservation receipts. `CheckSourceUpdates` is a distinct
`requeue_until_effect` kind; do not infer its implementation from the manual
worker. See [Source sync storage](storage.md) for local recovery.

### Printer send

Compatibility groups the `/printer-send-queue` list/add/dispatch/drain/delete
routes, both `/jobs/printer-upload` aliases, `job.printer-upload`, startup and
missing-job reconciliation, shutdown interruption, and the legacy self-host
exports migration under `printer.send-queue`. The replacement queue is under
`/api/v2/printer-send-queue` and has its own reconciliation route. It uses
one `SendWorker` and the storage job policy
`never_resend_after_effect`. The [printer-send fault proof](../rust/crates/pp-core/README.md)
covers lost replies and interruption, not all legacy HTTP aliases or all
startup and shutdown participants. In particular, the two direct
`POST /jobs/printer-upload` forms are not mounted by the Rust jobs router.
Future transition checks must retain the durable unknown result and never
repeat a physical send to prove equivalence.

**Restart before effect admission (GRE-2023).** Stage two files through the
Rust launcher, then use the storage writer to leave one upload job queued
and the other leased with no effect receipt. Startup marks both jobs and
queue entries as not started. It keeps their original job IDs and staged
bytes, rejects reconciliation because the outcomes are known, and makes
no upload. The queue entries explain that nothing reached the printer;
the job reads return terminal failures with no upload result.

Send ready skips both recovered entries. This exercise starts with
Rust-owned data; it is not an import or a Node compatibility rehearsal.

Only a later Send now creates a fresh job. Each explicit retry makes one
upload-only request to the loopback fake. The original failure receipts
stay readable after both retries and restart. The test deliberately stops at
the durable handoff before the send worker admits an effect. It complements
the [shutdown test](send-queue.md), where the effect has already reached the
fake and the outcome can be unknown.

The rehearsal leaves the old attempts in storage. The queue points to a
new job only once the user sends again, so a successful retry does not
rewrite the history of the interrupted attempt. This does not exercise
revocation of the original session's authority.

```sh
cargo nextest run --locked --manifest-path rust/Cargo.toml -p pp-core \
  --test runtime_m0 -E 'test(send_before_effect::)'
```

### Spoolman deduction

Spoolman is not a generic `JobKind`. The Rust owner writes a deduction when a
Checkoff burst commits. The same runtime starts its `DeductionWorker` and
mounts the list, detail and explicit reconcile routes under
`/api/v2/spoolman/deductions`. Include Verify, Verify batch and undo (including
roll-up UI callers), any reconciliation wake, and worker startup/shutdown
when tracing ownership. Undo only cancels unsent intents; it cannot restore
stock after an acknowledged deduction. The [deduction proof](storage.md)
in GRE-1427 is separate from this manual Sync proof.

### Other durable work and app handoffs

**Accepted Plate export and local slicer handoff (GRE-2008).** The Rust
desktop launcher serves the managed Plate ZIP and publishes one verified
3MF on an explicit handoff to a configured local inbox. The published bytes
match the requested Plate in the ZIP. A retry after restart returns the
same filename with `already_delivered: true`; conflicting bytes are not
overwritten. A partial pending file is not a delivery and is replaced only
when the operator requests delivery. The Rust server still serves the ZIP
but does not mount the local picker or handoff.

Neither `POST /jobs/export-accepted-plate-3mf` nor
`POST /slicer-instances/:id/open-accepted-plates`, including their `/api/v1`
aliases, enqueues a job, delivers a file or falls back to Node on the Rust
launcher. The test runs with no Node executable and only a disposable
inbox. It does not launch a slicer, prove parity for the other exports, or
retire the original Node worker. See [Local slicer handoff](api.md#local-slicer-handoff)
for the replacement contract.

Run the launcher proof with:

```sh
cargo nextest run --locked --manifest-path rust/Cargo.toml -p pp-core \
  --test runtime_m0 -E 'test(plate_export_transition::)'
```

- The Node export groups are `export.accepted-plate-3mf`,
  `export.checklist-html`, `export.direct-3mf`, `export.kit-bundle`, and
  `export.stl-pack`. Each includes both job POST aliases and its job runner;
  accepted-plate 3MF also includes the two `open-accepted-plates` aliases.
  Local export jobs have `requeue_local` policy. A replacement file download
  is not proof of an equivalent legacy job or of a safely repeated app launch.
- `profile.sync` also pairs the `/ws/profile-sync` connection and managed
  profile sync worker. The old worker fence alone provides no Rust product
  parity. An explicit slicer or Bambu Connect handoff has a separate user
  action; do not count that action as a recoverable background export job.
- Supplied-file import uses a Rust-owned journal, an acquisition worker, and
  startup recovery. It is local staging; it does not run any of the three
  legacy Source jobs that the Rust product currently refuses.
- Webhook, Discord and Home Assistant ports need their own route, worker
  and provider proofs when implemented. Do not count an absent endpoint or a
  generic job policy as evidence of a supported Rust integration.

### Kit bundle compatibility (GRE-2031)

With the rebuild gate off, the desktop launcher routes both
`POST /jobs/export-kit-bundle` and `POST /api/v1/jobs/export-kit-bundle`
to Node. The proof creates a disposable Plan, sends an editable request
through one alias and a request for accepted progress through the other,
and follows each distinct job to completion. The Node worker publishes a
downloadable `.print-partner-kit.zip` containing only `kit.json`. The
download matches the published bytes. Restart leaves the files downloadable
and does not rerun the old in-memory jobs. The Plan has no accepted
revision; this does not prove parity for exporting accepted progress.

In a separate Rust-owned directory, the launcher runs with the rebuild
gate on and no Node executable. Both aliases refuse both request forms
for an existing Plan. No Rust job or export file appears before or
after restart. This is a compatibility boundary proof, not a Rust kit
bundle replacement, an import test, or a production ownership switch.

Run the launcher proof against the built desktop release assets:

```sh
cargo nextest run --locked --manifest-path rust/Cargo.toml -p pp-core \
  --test runtime_m0 -E 'test(kit_bundle_aliases_and_worker_remain_with_node)'
```

## Manual Source Sync receipt

The runtime proof starts the actual Rust-only launcher twice with the same
disposable directory and an intentionally nonexistent Node executable. The
Rust gateway reports `compat: not_used`. Both manual POST aliases reject a
desktop cookie without an account session, reject an empty Source list, and
accept the same valid request shape. The three other legacy Source job POST
pairs are refused and create no job.

Each accepted request uses a distinct nonexistent Source ID. This makes the
real background worker resolve a target and finish a durable per-Source
`Source not found` receipt without any GitHub request. Both job detail aliases
read the same result; the job list contains exactly the IDs returned by
successful enqueues. On restart, both original receipts are unchanged, the
same admissions still apply, and both aliases can send new jobs to the
launched worker. No old per-operation cutover record is written. The fake
GitHub transport tests in `pp-core::sync_worker` separately prove real
revision publication; this receipt does not claim watcher, import, MCP,
external fault, or Node contract parity.

Run the focused evidence locally:

```sh
cargo nextest run --locked --manifest-path rust/Cargo.toml -p pp-core \
  --features desktop_rebuild --no-tests=fail \
  -E 'test(ticket_t_28_recovery_source_sync_aliases) | test(sync_worker::tests)'
```

The rest of the transition evidence is tracked in
[GRE-1998](https://linear.app/greetz/issue/GRE-1998).
