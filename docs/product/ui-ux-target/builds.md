# Builds UI target

[UI and UX target](../ui-ux-target.md) owns the shared design rules.
This page owns this feature's contracts and delivery slices.

## Contracts

Status terms and the Rust-only rule are defined in the [contract overview](../ui-ux-target.md#contracts-the-ui-needs).

| Need | Contract | Status |
| --- | --- | --- |
| Builds list, detail, layers | `GET /api/v1/plans`, `/plans/{id}`, `/layers`, `/kit-manifest` | Exists. Under `desktop_rebuild` each summary also carries the header's `customer` and `due_date` from `build_profile_details` (GRE-1100). With the Orders module on, the Builds list sorts by due date (earliest first, none last), filters to overdue (unarchived, due before today and not fully printed; today is rechecked each minute) or no due date, and marks each unarchived Build whose non-blank order number another unarchived Build shares, compared after [normalization](../../draft-headers.md#order-authority-gre-1095) as the Accept preview counts duplicates, without merging them |
| First Source thumbnail | `listBuildLayers`, `listSourceFiles`, `GET /api/v1/sources/{id}/thumbnail?path=` | The Builds list shows the first STL of the Source on the lowest `layer_order`. That row requests the layers and the file list only once it is in the viewport, and a row off screen does not. The Sources tab lists each Source's files grouped by folder, with the same thumbnail beside each STL. A failed thumbnail says **No preview**. The PNG is not tinted |
| Build header edit and archive | `PATCH /plans/{id}`, `POST /plans/{id}/archive` (also at `/api/v1` and `/api/v2`) | Exists in Node; served from Rust behind `desktop_rebuild` (GRE-1231); see [Build header edit and archive in Rust](../../records/d19-publication-client.md#build-header-edit-and-archive-in-rust-gre-1231) |
| Variant dimensions and selection | `GET /plans/{id}/variant-dimensions`, `POST /plans/{id}/variant-selection` (also at `/api/v1` and `/api/v2`) | Exists in Node; served from Rust behind `desktop_rebuild` (GRE-1221); see [Variant dimensions and selection in Rust](../../records/d19-publication-client.md#variant-dimensions-and-selection-in-rust-gre-1221) |
| Build workflow projection | `GET /plans/:id/workflow` | Exists (GRE-1123) |
| Build planning phase | `GET /plans/:id/build-planning` | Exists in Rust behind `desktop_rebuild` (GRE-1119) |

## Delivery slices

Numbers preserve the [product build order](../ui-ux-target.md#build-order); Linear owns execution state.

| # | Slice | Usable result |
| --- | --- | --- |
| 2 | Builds list and Build Overview, read-only | Browse Builds |
| 7 | Order fields, duplicate protection and due-date ranking | One authoritative Build per order |
