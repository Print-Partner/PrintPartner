# Checkoff and unmatched prints UI target

[UI and UX target](../ui-ux-target.md) owns the shared design rules.
This page owns this feature's contracts and delivery slices.

## Clear the beds

Today shows one hero task: "Clear 2 beds and confirm what came off", with the reason ("frees Voron 2.4 and Trident") and the diff inline. One press confirms the batch.

Checkoff input model, taken from the free-synthesis candidate because it is the only one that specifies the mechanism:

- `P` marks a plate printed, showing what it will mark before it commits.
- `F` fills all remaining units of a part.
- `0` to `9` set a count.
- `Space` or click ticks one unit; `Shift` selects a run.
- One burst is one transaction with an idempotency key, so a phone retry cannot double-count, and an undo token.
- Per-unit ticking remains available. It is the fallback, not the main path.

Step counts, measured before and counted after:

| Flow | Before | After |
| --- | --- | --- |
| F1, Build 22, 100 ticks in 15 bursts | 102 | 15 to 16, if each burst is one plate |
| F2, Build 19, 47 ticks in 6 bursts | 50 | 7 to 8 |
| F3, 18 unmatched prints | 0 resolved | about 5 to 20 depending on suggestion accuracy |

The plate assumption must be validated against Build 22's accepted plates before these numbers are quoted as achieved. If a burst is not a plate, part fill carries the saving, and the number rises toward the count of distinct parts touched.

Checkoff is not a Plan change, so it does not pass the Accept gate. It is undoable as a burst.

## Handle prints the app could not match

A Match prints review lists the backlog with confidence in words (Exact, Likely, No suggestion), bulk select, and one press per confident group. Low-confidence rows start unchecked. Nothing is ever ticked automatically: verify-first is a product rule, and 18 unresolved prints are the evidence that matching is not reliable enough to skip confirmation.

## Contracts

Status terms and the Rust-only rule are defined in the [contract overview](../ui-ux-target.md#contracts-the-ui-needs).

| Need | Contract | Status |
| --- | --- | --- |
| Checkoff read | `GET /plans/{id}/checkoff`, `/parts/{id}/assembled` | Exists |
| Checkoff tab units and manual bursts | `GET /api/v2/plans/{id}/checkoff/units`, `POST /plans/{id}/checkoff/bursts` and `/bursts/undo` | Exists in Rust behind `desktop_rebuild` (GRE-1191); see [Manual Checkoff bursts](../../records/d19-publication-client.md#manual-checkoff-bursts-gre-1191) |
| Printer checkoff and unmatched | `GET /api/v2/printer-checkoff`, `/unattributed` | Exists in Rust behind `desktop_rebuild` (GRE-1125), read-only |
| Verify one result | `POST /printer-checkoff/verify`, `/reconcile`, `/dismiss` | Port |
| Verify several beds at once | `POST /printer-checkoff/verify-batch` with `undo_token` | New |
| Match suggestions | `GET /api/v2/printer-checkoff/unattributed/suggestions` | Exists in Rust behind `desktop_rebuild` (GRE-1091), read-only; never confirms. See [Farm reads](../../farm-reads.md#match-suggestions) |

## Delivery slices

Numbers preserve the [product build order](../ui-ux-target.md#build-order); Linear owns execution state.

| # | Slice | Usable result |
| --- | --- | --- |
| 5 | Clear beds in batches and review unmatched prints | F1 and F3 targets |
