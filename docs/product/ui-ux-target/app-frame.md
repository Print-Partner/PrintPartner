# App frame and settings UI target

[UI and UX target](../ui-ux-target.md) owns the shared design rules.
This page owns this feature's contracts and delivery slices.

## Contracts

Status terms and the Rust-only rule are defined in the [contract overview](../ui-ux-target.md#contracts-the-ui-needs).

| Need | Contract | Status |
| --- | --- | --- |
| Jobs and progress | `GET /api/v1/jobs`, `/ws/jobs/{id}` | Exists |
| Health with module flags | `GET /health` `modules` list with off reason (Rust-only mode) | Exists. The replacement Settings screen (`/settings`, GRE-1336) lists each Module, on or off, with its off reason. Module changes use `PUT /api/v2/modules/{module}` with confirmation and refresh the shared core reads. The Printers section lists, adds and deletes printers. Integrations and Backups remain unavailable; Sharing and devices explains the missing Rust read contract. See [Settings Modules](../settings-modules.md) and [Settings Printers](../settings-printers.md) |
| Domain events | one authenticated, resumable stream | New |

## Delivery slices

Numbers preserve the [product build order](../ui-ux-target.md#build-order); Linear owns execution state.

| # | Slice | Usable result |
| --- | --- | --- |
| 0 | Serve the authenticated Rust product entry point behind the flag | The replacement UI can reach Rust without Node fallback |
| 1 | Tokens, type, status descriptors, app frame and command palette | A real frame in all three webviews |
| 11 | Phone bench and Share on network pairing | Confirm beds from a phone |
| 12 | Native menus, notifications and tray | See alerts outside the window |
| 13 | First run, help, empty states and copy | Set up a new install and create the first Build |
