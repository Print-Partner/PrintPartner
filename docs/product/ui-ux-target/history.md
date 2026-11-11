# History and farm log UI target

[UI and UX target](../ui-ux-target.md) owns the shared design rules.
This page owns this feature's contracts and delivery slices.

## Contracts

Status terms and the Rust-only rule are defined in the [contract overview](../ui-ux-target.md#contracts-the-ui-needs).

| Need | Contract | Status |
| --- | --- | --- |
| Farm log with undo | `GET /api/v2/farm-log`, `POST /api/v2/farm-log/{id}/undo` | Exists in Rust behind `desktop_rebuild` (GRE-1126); bursts and their undos are logged, sends never offer undo. See [Farm log](../../farm-log.md). The replacement UI shows it at `/farm/log`, linked from Farm, with Undo on each entry the core reports undoable and the reason on the rest (GRE-1101) |
| Build history | `GET /plans/:id/history` | Exists in Rust behind `desktop_rebuild` (GRE-1092); the generated client names it `readBuildHistory`. The replacement History tab lists it newest first with Show older entries (GRE-1101). It lists manual and printer checkoff bursts with their live undo state and undoes one with `POST /plans/:id/history/:entry_id/undo` (`undoBuildHistoryEntry`); sends show as not undoable with the reason (GRE-1259, UI8-03 done). Keep UI ticket GRE-1023 unchanged. See [Build history](../../build-history.md) |
| Restore as draft | `POST /plans/:id/snapshots/:sid/restore` in draft mode | Exists in Rust behind `desktop_rebuild` (GRE-1094), for snapshots Accept saved; see [Build history](../../build-history.md#restore-a-snapshot-as-a-draft). The History tab offers it, as a button and as `R`, on each snapshot whose history entry is `restorable`, and opens the Plan tab's Accept gate on the restored draft (GRE-1260) |

## Delivery slices

Numbers preserve the [product build order](../ui-ux-target.md#build-order); Linear owns execution state.

| # | Slice | Usable result |
| --- | --- | --- |
| 8 | History tab and farm log with undo | Fix a wrong change in two steps |
