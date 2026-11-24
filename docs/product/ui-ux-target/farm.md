# Farm printers UI target

[UI and UX target](../ui-ux-target.md) owns the shared design rules.
This page owns this feature's contracts and delivery slices.

## Contracts

Status terms and the Rust-only rule are defined in the [contract overview](../ui-ux-target.md#contracts-the-ui-needs).

| Need | Contract | Status |
| --- | --- | --- |
| Printers | `GET /api/v2/printers` | Exists in Rust behind `desktop_rebuild` (GRE-1125): Moonraker and PrusaLink observed; Bambu reads as Manual until it has a Rust status watcher (GRE-362). See [Farm reads](../../farm-reads.md) |
| Printer stream | `GET /ws/printers` | Rust behind `desktop_rebuild` (GRE-1014, [Printer stream](../../printer-stream.md)) |
| Send queue | `GET /api/v2/printer-send-queue` | Rust behind `desktop_rebuild` (GRE-1125, [Send queue](../../send-queue.md)) |

## Send queue (GRE-1948)

Farm shows sends from all Builds below the printer table while the Farm Module
is on. The queue lists each Build, file, printer and [send state](production.md#production-tab-gre-1486),
including Outcome unknown. Removed sends are hidden. An assigned Build links to
its Production tab, where the operator can act on the send. When a name is
unavailable, the queue shows the Build or printer ID. A send without a Build
has no Production link.

The queue reads again every three seconds while a send is Sending. It has its
own empty and read-error states so printer status stays visible.

## Delivery slices

Numbers preserve the [product build order](../ui-ux-target.md#build-order); Linear owns execution state.

| # | Slice | Usable result |
| --- | --- | --- |
| 3 | Farm printers with honest states and the live pulse line | See every printer, including Annex K3 and Micron |
