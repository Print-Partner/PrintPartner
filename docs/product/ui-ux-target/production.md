# Production and sends UI target

[UI and UX target](../ui-ux-target.md) owns the shared design rules.
This page owns this feature's contracts and delivery slices.

## Contracts

Status terms and the Rust-only rule are defined in the [contract overview](../ui-ux-target.md#contracts-the-ui-needs).

| Need | Contract | Status |
| --- | --- | --- |
| Plates workspace, initialize, arrange, edits and restore | `GET /plans/{id}/plates`, `POST /plans/{id}/plates/initialize`, `/arrange`, `/restore`, and the unit routes under `/plans/{id}/plates/{plateId}/units/{token}` (also at `/api/v2`) | Exists in Node; served from Rust behind `desktop_rebuild` (GRE-419); see [Accepted Plate routes in Rust](../../records/d19-publication-client.md#accepted-plate-routes-in-rust-gre-419) |
| Production setup | `GET` and `PATCH /plans/{id}/production-setup` (root only, as in Node) | Exists in Node; served from Rust behind `desktop_rebuild` (GRE-1209); see [Production setup in Rust](../../records/d19-publication-client.md#production-setup-in-rust-gre-1209) |
| Send queue | `GET /api/v2/printer-send-queue`; `POST /api/v2/printer-send-queue` (Add, a `multipart/form-data` upload); `POST .../{id}/dispatch`, `.../drain`, `.../{id}/reconcile`, `DELETE .../{id}` | Exists in Rust behind `desktop_rebuild`: the read (GRE-1125), Add (GRE-1227), and Send now, Send ready, Reconcile and Remove with the upload worker (GRE-365). Add, Send now, Reconcile and Remove are typed routes in the generated client (GRE-1460). Reconcile has no UI yet. See [Send queue](../../send-queue.md) |

## Delivery slices

Numbers preserve the [product build order](../ui-ux-target.md#build-order); Linear owns execution state.

| # | Slice | Usable result |
| --- | --- | --- |
| 10 | Production, send sheet and risk-scaled cancel confirmation | Send and cancel explicitly |

## Production tab (GRE-1486)

`/builds/:id/production` lists this Build's sends: queue items whose `profile_id` is the Build, newest first, without removed ones. Each row shows the file, the printer, Upload only or Upload and start print, the time, and a state: Not sent (`queued`), Sending, Sent (`done`), Failed with the stored error, or Outcome unknown. While a row is Sending the list reads the queue every 3 seconds. With the Farm Module off the tab shows the Module-off state and no sheet.

"Send a file" opens a sheet with the file (`.gcode`, `.gco` or `.bgcode`; anything else is refused before a request), a Moonraker or PrusaLink printer from `GET /api/v2/printers` with its current state, and Upload only or Upload and start print. Upload only is selected each time the sheet opens.

- **Upload only:** Send runs Add with `start` and `wait_for_idle` off, then Send now.
- **Upload and start print:** offered only when the printer's execution state is idle (Ready, or Bed to clear after a finished print), which is what the route accepts for a start. Otherwise it is disabled and says why. Send opens a confirmation naming the file and the printer, whose first line says when the bed needs clearing. Only its Start print runs Add with `start` and `wait_for_idle` on, then Send now.

The UI never sends `force`. Nothing is sent when the tab or sheet opens, when a file is chosen, on reload or on reconnect. A refusal from Send now leaves a Not sent row and shows the core's reason. A lost answer, or a 5xx from Add or Send now (the write may have committed), re-reads the queue and repeats nothing.
