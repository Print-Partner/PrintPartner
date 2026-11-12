# Library and Sources UI target

[UI and UX target](../ui-ux-target.md) owns the shared design rules.
This page owns this feature's contracts and delivery slices.

## Contracts

Status terms and the Rust-only rule are defined in the [contract overview](../ui-ux-target.md#contracts-the-ui-needs).

| Need | Contract | Status |
| --- | --- | --- |
| Sources | `GET /sources`, `/sources/activity`, naming, import rules | Exists in Rust behind `desktop_rebuild`. The generated client's `listSources` and `listSourceActivity` read `/api/v1/sources` and `/api/v1/sources/activity` (GRE-1364). Library (`/library`) lists each Source with its revision, sync state and newest activity, and the Build Sources tab shows each layer's Source, its current revision, and the revision the Accepted Plan used, read from the Build's Plan freshness. Both are read only |

## Delivery slices

Numbers preserve the [product build order](../ui-ux-target.md#build-order); Linear owns execution state.

| # | Slice | Usable result |
| --- | --- | --- |
| 14 | Library and Sources tab | Manage Sources |
