# Spoolman stock UI target

[UI and UX target](../ui-ux-target.md) owns the shared design rules.
This page owns this feature's contracts and delivery slices.

## Contracts

Status terms and the Rust-only rule are defined in the [contract overview](../ui-ux-target.md#contracts-the-ui-needs).

| Need | Contract | Status |
| --- | --- | --- |
| Spoolman stock | `GET /integrations/:id/spoolman/spools` | Port |
| Spoolman shortfalls | `GET /plans/:id/filament-shortfalls` (Rust-only) | Exists; grams needed reads the per-send grams table (schema 46): the slicer estimate recorded at send, replaced by the printer's measured length at finish (GRE-1165); basis is `measured`, `estimated` (with `measured_length_not_convertible` when a measurement could not be converted) or `unknown` with a reason. The Build Overview's Filament section reads it (GRE-1102); the workflow projection does not yet |

## Delivery slices

Numbers preserve the [product build order](../ui-ux-target.md#build-order); Linear owns execution state.

| # | Slice | Usable result |
| --- | --- | --- |
| 9 | Spoolman shortfall tasks | See what stock needs attention |

## Filament on the Build Overview

The Overview has a **Filament** section under its facts and above Layers (`web/apps/ui/src/builds/FilamentSection.tsx`). It loads on its own, so the rest of the Overview never waits for Spoolman.

- It reads `readFilamentShortfalls` when the Overview opens and when the owner presses **Refresh**, never on a timer, and shows when the numbers were read. Offline, Refresh is disabled with the offline reason and the last rows stay.
- `ready`: one row per filament with remaining units. **Needed** in grams with its basis ("measured over 3 prints", "slicer estimate") or "Not known yet" with the reason. **In Spoolman** in grams with the number of spools, or "Not known" with the reason. **Status**: "Enough", "Short by N g" in the `pp-tag` style, or "Not known". Under the row, its Parts with remaining units. A row with an empty label is named by its filament ID.
- `ready` with no rows: "Nothing left to print." `no_accepted_plan`: "No accepted Plan yet." `module_off`: the Module-off block with its Settings link.
- Unknown stock is never shown as 0 g, and a row with either side unknown never shows "Short".
- A failed read keeps the last rows, says so with the time they were read, and offers Retry.

Not yet: a shortfall task on Today, deductions on this section (GRE-1102 PR 2, after GRE-411), and a colour swatch.
