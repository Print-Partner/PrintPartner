# Spoolman stock UI target

[UI and UX target](../ui-ux-target.md) owns the shared design rules.
This page owns this feature's contracts and delivery slices.

## Contracts

Status terms and the Rust-only rule are defined in the [contract overview](../ui-ux-target.md#contracts-the-ui-needs).

| Need | Contract | Status |
| --- | --- | --- |
| Spoolman stock | `GET /integrations/:id/spoolman/spools` | Port |
| Spoolman shortfalls | `GET /plans/:id/filament-shortfalls` (Rust-only) | Exists; grams needed reads the per-send grams table (schema 46): the slicer estimate recorded at send, replaced by the printer's measured length at finish (GRE-1165); basis is `measured`, `estimated` (with `measured_length_not_convertible` when a measurement could not be converted) or `unknown` with a reason. The workflow projection consumes it next |

## Delivery slices

Numbers preserve the [product build order](../ui-ux-target.md#build-order); Linear owns execution state.

| # | Slice | Usable result |
| --- | --- | --- |
| 9 | Spoolman shortfall tasks | See what stock needs attention |
