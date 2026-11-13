# Settings Printers

Replacement Settings has a Printers section (GRE-1431; part A is GRE-1489, part
B is GRE-1514). It lists the saved printers from `GET /api/v2/printers`, the
same read Farm uses, with each printer's model and the kind of its connection.
It adds, edits and deletes printers through the generated Rust client.

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

The integration write can lose its answer too. So the form reads
`GET /api/v2/integrations` before it sends the create and keeps the ids it saw;
when that read fails, nothing is sent. After a create with no answer the form
keeps what it sent and locks the connection fields. The next "Save printer" or
"Discard" reads the integrations and the printer list first. An integration
that was not listed before, with the same kind, name and address, that no
printer links is the one this form saved: it is used, or deleted on "Discard",
and no second one is created. An integration that was already there is never
used or deleted, however alike. When two new ones match, the form stops, says
so and can be closed. Only when the list shows no new match is the create sent
again.

A failed printer write may still have applied: its answer can be lost to a
timeout after the core saved the printer. So both choices read the printer list
again first. When a printer already links the integration, nothing is sent or
deleted; the form closes and says the printer was saved after all. When the
list cannot be read, nothing is sent or deleted either; the form says to reopen
Settings and can then be closed.

## Edit

"Edit" opens a form with the printer's name and, for a Moonraker or PrusaLink
connection, its address and credentials. The connection is read from
`GET /api/v2/integrations` when the form opens. A key or password is write-only:
the form says whether one is set and never shows it. Leaving the field empty
keeps the stored one; "Remove the stored API key" (or password) clears it.
The address field starts from the stored address the list answers. The Rust
core already leaves out any user name, password, query or fragment, and answers
`null` for text that is not a URL, so the field starts empty then. The form
strips those parts again (`shownAddress` in `settings/connections.ts`) and its
notes about hidden sign-in details or an address that can't be shown appear
only if a core ever answers such text. Saving the address unchanged sends no
address, so the stored one stays.

Saving sends only what changed: `updateIntegration` first, then `updatePrinter`
for the name. A refused connection change therefore leaves the name as it was.
If the name write fails after the connection saved, the form says so, locks the
connection fields and sends only the name on the next save.

When the address changes and a key or password is stored, the core accepts the
change only if that secret is sent again or removed, so a stored secret never
goes to a new address (GRE-1481). The form asks for it and says why in one
sentence. It never sends a value it was not given: saving without it shows the
core's refusal, such as "API key must be sent again when the address changes",
and changes nothing.

When the printer has no connection, its connection is gone or of a kind this
form cannot edit, or the connection cannot be read, the form says so and only
the name can be changed.

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
- A request the core refuses as invalid shows each refused field with the
  core's reason for it. The UI shows that text and never matches on it.

Verify with `npm --prefix web run test --workspace @print-partner/ui`. Capture
`/settings` on a Rust-only core through the evidence harness; use a loopback
Moonraker simulator for the add flow, never a real printer.
