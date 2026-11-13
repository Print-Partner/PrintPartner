# Plan and Accept UI target

[UI and UX target](../ui-ux-target.md) owns the shared design rules.
This page owns this feature's contracts and delivery slices.

## Change a Build safely

Every Plan change, including rename, order, customer and due date, lands in a draft and goes through the Accept gate. The gate previews old against new, names printed units affected, plate changes, stock impact and duplicate orders, and states that a snapshot is saved before accepting.

Before the gate can preview, the draft's Required-unit reconciliation must be settled: the units already printed for the accepted Plan are matched to the draft's Parts. When the preview answers `409 reconciliation_required`, the gate shows a **Match printed units** step in place of the changes, and Accept stays off with "Match the printed units first." If the draft's `reconciliation` lists no conflicts (no match selected yet, or a stale one), the gate sends the core's automatic match once (`PUT /api/v2/plans/{id}/drafts/{draft_id}/reconciliation` with `decisions: []`, the draft's `expected_snapshot_digest` and an `Idempotency-Key`). If it lists conflicts, the gate asks each in plain words with one choice per decision: for a Part that matches several accepted Parts, which one's printed units it keeps or new units (`select_exact_predecessor`, `replace`); for a Part that changed since it was accepted, keep the units printed for it or new units (`accept_prior_completion`, `replace`). Two Parts cannot pick the same printed units. A `predecessor_claimed` conflict has no choice of its own: the core judges each send afresh, so the gate offers **Start the match over** (`decisions: []`) to ask every question again, or the user changes the Parts. The match runs after any queued draft save and names the snapshot that save left. A lost send, or the next send of the same decisions after an unconfirmed outcome, reuses the key and body. The workspace it returns replaces the cached draft, so the next write names the new snapshot. If the answer still lists conflicts, focus moves to the step's heading. Otherwise the gate previews again and moves focus to "Printed units matched: N kept, N new, N no longer needed." The automatic match is sent only before any other send from the gate, so it never replaces the user's answers. If the preview still answers `reconciliation_required` after a match, the gate does not resend on its own; it says so and offers **Match printed units**. Rust's Accept runs the automatic match itself when no match is selected, so an Accept refusal `422 reconciliation_required` means the selection is unresolved or stale; the gate then previews again and shows the same step. A draft whose match is already settled sees no step.

Every Accept starts the new accepted Plan with no Plate layout, as Node does: the Build's Plate head is removed, so its arranged Plates are cleared even when every Required unit is kept (a rename included). The Plate revisions stay stored, but no screen shows them. The gate lists each cleared Plate and, under them, says: "A new accepted Plan starts with no Plate layout, so Accept clears these Plates even when every unit is kept. Laying Plates out again is not available in this UI yet." (GRE-1530). Without cleared Plates it says "No plates change." and nothing more. After Accept the Build's next action is unchanged by this: with no printer linked it is still "Check off printed units", because unplaced units can be checked off by hand.

The Accept gate names the holding Builds when a proposed order number is refused as a duplicate. The draft stays open; change its order number or archive the holding Build before trying again.

One order is one Build, enforced transactionally (Accept refuses `duplicate_order`; see [Order authority](../../draft-headers.md#order-authority-gre-1095)). A duplicate creates a detached experiment that records its parent, and promoting an alternative retires the previous authority atomically. Existing duplicate records are displayed, never silently merged.

History shows accepted revisions, snapshots, checkoff bursts and sends. Restore as draft (`R`) opens the gate.

The replacement UI says the same thing everywhere it explains a Plan change: Help's **Plan changes** section, the Plan tab's draft line, and History's empty state (GRE-1140). Each change saves to the draft: a checkbox such as Included at once, and a typed field (name, order number, customer, due date or quantity) when it loses focus or on Enter. A saved draft survives a restart. Only Accept changes the Build. Accept first saves a snapshot of the accepted Plan it replaces when the core can restore one, and the preview's snapshot note says whether it will. History can restore such a snapshot as a draft. The wording lives in `web/apps/ui/src/builds/planCopy.ts`, and `planCopy.test.ts` fails if any replacement UI source says changes save automatically or that there is no acceptance step. Restoring a plan never rewinds printed evidence or a printer command.

## Contracts

Status terms and the Rust-only rule are defined in the [contract overview](../ui-ux-target.md#contracts-the-ui-needs).

| Need | Contract | Status |
| --- | --- | --- |
| Drafts | `GET/PATCH /api/v2/plans/{id}/drafts*` (drafts are mounted under `/api/v2`, not `/api/v1`) | Exists |
| Assistant decisions | `GET /plans/:id/decisions`, `POST /plans/{id}/decisions` (also at `/api/v1` and `/api/v2`) | Exists in Rust behind `desktop_rebuild` (read GRE-1119, append GRE-1243); History lists them too |
| Plan feature overrides | `GET` and `PATCH /plans/{id}/features` (also at `/api/v1` and `/api/v2`) | Exists in Node; served from Rust behind `desktop_rebuild` (GRE-1243); see [Plan features and decision append in Rust](../../records/d19-publication-client.md#plan-features-and-decision-append-in-rust-gre-1243) |
| Accept | `POST /api/v2/plans/{id}/drafts/{draft_id}/apply` with an `Idempotency-Key` | Exists in Rust behind `desktop_rebuild`; the generated client's `acceptDraft` (GRE-1120). The Plan tab sends the previewed `revision`, so a stale Accept is `draft_changed`. Before the preview, the Plan tab settles Required-unit reconciliation through `PUT /api/v2/plans/{id}/drafts/{draft_id}/reconciliation` (`reconcileDraft`, GRE-1276) |
| Plan tab edits | `PUT /api/v2/plans/{id}/drafts/header`, `POST /api/v2/plans/{id}/drafts/save`, and `GET /api/v2/plans/{id}/drafts/header` for the Build header and Accepted base before any draft | Exists in Rust behind `desktop_rebuild` (GRE-1197, GRE-1120); see [Draft header fields](../../draft-headers.md) |
| Accept preview with checks | `GET /api/v2/plans/{id}/drafts/{draft_id}/accept-preview` | Exists in Rust behind `desktop_rebuild` (GRE-1207 added the header changes) |

## Delivery slices

Numbers preserve the [product build order](../ui-ux-target.md#build-order); Linear owns execution state.

| # | Slice | Usable result |
| --- | --- | --- |
| 6 | Plan tab and Accept gate with a snapshot on accept | Edit and accept in the replacement UI |
