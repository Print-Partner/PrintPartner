# Settings Printers

Replacement Settings has a Printers section (GRE-1489, part A of GRE-1431). It
lists the saved printers from `GET /api/v2/printers`, the same read Farm uses,
with each printer's model and the kind of its connection. It adds and deletes
printers through the generated Rust client. Editing a saved printer is part B.

## Add

The form takes the kind (Moonraker or PrusaLink), the address, an optional API
key or a username and password, a name, a model, and the bed width, depth and
height in whole millimetres. There are no presets.

"Check address" calls `POST /api/v2/printers/probe` with the kind and address.
"Save printer" is offered only for the kind and address that were checked. When
the check fails, the form shows the core's reason and offers "Save anyway", for
a printer that is switched off. The probe does not take credentials, so a key or
password is checked when the printer first connects, and the form says so. A
saved key or password is never shown again.

Saving is two writes: `createIntegration` (the address and credentials), then
`createPrinter` with the new integration's id. If the printer write fails after
the integration saved, the form says so and offers two choices. "Save printer"
sends only the printer again. "Discard" deletes that integration. Escape and a
click outside the form do not close it in that state.

## Delete

Delete opens a confirmation that says what it removes. It sends `deletePrinter`,
then reads the printer list again, and sends `deleteIntegration` only when that
fresh list shows no other printer linking the integration. The core does not
refuse to delete an integration a printer still links, and the shown list can
be out of date, for example after a printer was added in another window. When
another printer links it, or the list cannot be read, the integration is kept
and the message says so. If the integration write fails, the dialog says the
printer is deleted and offers "Delete connection", which checks and sends only
that write.

## Rules

- No write is resent automatically. A failure shows the core's reason.
- Every dialog close reads the list again, because a write whose answer was
  lost may still have applied.
- Offline, the controls stay focusable and perform no write.
- The section's states are loading, empty, failed read, last list with a failed
  refresh, and ready.
- A request the core refuses as invalid shows the core's detail, which does not
  name the field yet (GRE-974).

Verify with `npm --prefix web run test --workspace @print-partner/ui`. Capture
`/settings` on a Rust-only core through the evidence harness; use a loopback
Moonraker simulator for the add flow, never a real printer.
