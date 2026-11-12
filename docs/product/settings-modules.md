# Settings Modules

Replacement Settings reads each Module and its off reason from Rust `/health`.
A user can turn a readable Module on or off through the generated `setModule`
client (`PUT /api/v2/modules/{module}` with `{on: boolean}`). No Node setting is
written. The runtime's default-off rebuild gates remain unchanged.

Every change opens a confirmation dialog. Turning a Module off keeps saved
Builds and history, and does not stop a printer or undo a completed effect.
Existing Rust routes own worker enforcement and stored history; the UI does
not rewrite either. Settings remains reachable when a Module is off.

The row keeps its current state until the Rust response arrives. The response
updates the shared health cache, then core reads refresh so other screens see
the same state. A pending request blocks another write, including after navigating away and back. Closing its dialog
leaves a visible saving status. Failed or lost replies trigger reads, never an
automatic write retry. A user can reload Module state before trying again.

Offline controls stay focusable with an accessible reason and perform no writes.
A Module whose setting is unreadable also stays unavailable until the core can
read it again. Loading, empty, failed refresh with last-known data, and offline
screen states use the existing screen template. Native buttons and the shared
Radix dialog provide keyboard activation, focus trapping, Escape and focus return.
Controls reuse the existing phone-sized button styles and wrapping facts grid.

The Sharing and devices section is read-only availability guidance. Main has
no generated sharing/device/pairing route or health state for them, so the screen
reports that boundary rather than inventing device rows or a current sharing
state. Sharing remains off by default in the product contract. Actual read-only
views, pairing and revocation wait for their Rust contract.

GRE-1412 quotes remaining SET-02, SET-03, SET-04 and deferred SET-05 items.
Secret settings, integrations, filaments and installation backup/restore are
separate slices. The existing detailed Settings child tickets retain their context.

Verify with the Settings tests and full ui-package tests, then `just ci --fast`.
Focused cases cover confirmation, cancellation/focus return, canonical state,
pending writes, server refusal, lost replies, unreadable settings and offline
confirmation. Capture `/settings` on disposable data through the evidence harness.
