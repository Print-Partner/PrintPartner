---
name: pp-send-once
description: Check that a printer send with an unknown outcome is not handed to the printer again until the user reconciles. Use when changing send-queue dispatch, the send worker's upload, or printer-upload job recovery.
---

# Printer send is not repeated while its outcome is unknown

A send whose printer effect was admitted, and whose outcome is still unknown, must not be handed to the printer again. Send now stays refused until the user reconciles. A second copy on the printer cannot be undone.

The check is a TLA+ model of one queue item, run with TLC 1.7.4 (TLC2 2.19). The question is whether the steps can happen in a bad order. The model is small enough to check every order, up to two uploads. Read every TLC result with `/statechart`. It states each broken invariant as the shortest path, in plain words.

## When to run

Run it when a change touches any of these:

- `rust/crates/pp-storage/src/send_queue/writes.rs` — `sendable`, `outcome_unknown`, `dispatch`, `reconcile`
- `rust/crates/pp-core/src/send_worker.rs` — the upload in `run`, `begin`, `classify`
- `rust/crates/pp-storage/src/jobs.rs` — `WorkerOperation::Fail` for an admitted effect, `recovery_decision`, `reconcile_printer_upload`

Last confirmed against commit `01a9700130f1b07504dce55eb0ce37458f1bf9ff`. Those three files at that commit are what the model describes. If a later commit changes any of them, the model may be stale. Read the diff. If the steps still match, move this commit forward. If a step changed, update `SendOnce.tla` so each action is still one indivisible step in the code, rerun both commands below, and only then move the commit forward.

## Run

From the repository root, Java 21 or newer:

```sh
rust/verify/printer-send-once/check.sh
rust/verify/printer-send-once/check.sh broken
```

The first command is the product. It must exit 0. The second removes the `sendable` guard (`faithful = FALSE` in `SendOnceBroken.cfg`), which is `outcome_unknown` no longer refusing Send now. It must exit non-zero and name `SendNowRefusesUnknown`. The script downloads `tla2tools-1.7.4.jar` into `~/.cache/print-partner/` on first use. `TLA2TOOLS_JAR` overrides that path.

A faithful failure is a finding. Say it as the path `/statechart` prints: which action ran, and what `item`, `unknown`, and `uploads` were. Do not drop an invariant or set `faithful` to false to make it pass. Do not change the send code unless the user asked.

A broken run that exits 0 means the model no longer encodes the guard. That is a modeling error. Repair `SendNow` so it refuses an unknown outcome when `faithful` is true, and allows it when `faithful` is false.

## What the model counts

`uploads` increases only on `Upload`, the one `HostSends::upload` call after `BeginEffect`. `FailUnknown` is `WorkerOperation::Fail` while the effect is admitted, and `recovery_decision` after a crash: the job becomes reconciliation-required and nothing is uploaded by that step. `ReconcileNotPrinted` clears the unknown flag and leaves the item in `error`, so a later `SendNow` is a new explicit send. `Bound` stops the check at two uploads. A third explicit send after another reconcile is the same cycle.

## What this does not cover

One item and one printer. It does not model Farm being off, a missing file, a second item on the same printer, removal, or the bytes of the upload. It does not prove the Rust functions. A pass means every order of these steps, up to two uploads, keeps the two invariants. It does not mean the whole send path is correct.
