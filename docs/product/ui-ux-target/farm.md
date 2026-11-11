# Farm printers UI target

[UI and UX target](../ui-ux-target.md) owns the shared design rules.
This page owns this feature's contracts and delivery slices.

## Contracts

Status terms and the Rust-only rule are defined in the [contract overview](../ui-ux-target.md#contracts-the-ui-needs).

| Need | Contract | Status |
| --- | --- | --- |
| Printers | `GET /api/v2/printers` | Exists in Rust behind `desktop_rebuild` (GRE-1125): Moonraker and PrusaLink observed; Bambu reads as Manual until it has a Rust status watcher (GRE-362). See [Farm reads](../../farm-reads.md) |
| Printer stream | `GET /ws/printers` | Rust behind `desktop_rebuild` (GRE-1014, [Printer stream](../../printer-stream.md)) |

## Delivery slices

Numbers preserve the [product build order](../ui-ux-target.md#build-order); Linear owns execution state.

| # | Slice | Usable result |
| --- | --- | --- |
| 3 | Farm printers with honest states and the live pulse line | See every printer, including Annex K3 and Micron |
