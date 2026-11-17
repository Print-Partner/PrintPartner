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
| Create a Build | `POST /api/v2/plans` (`createBuild`) with `name` and an optional `base_project_id` | Exists in Rust behind `desktop_rebuild`; in the generated client since GRE-1401. The 200 body is the Build's summary plus its `layers`; the client keeps the summary. The core refuses a name already in use (400 `Profile already exists`) and an unknown base Source (400 `Project not found`) and writes nothing |
| Set a Build's base Source | `PUT /api/v2/plans/{id}/layers/base` (`setBuildBase`) with `project_id` | Exists in Rust behind `desktop_rebuild`; in the generated client since GRE-1401. Adds the base layer, or moves an existing one to that Source, and answers the Build's layers |
| Build workflow projection | `GET /plans/:id/workflow` | Exists (GRE-1123) |
| Build planning phase | `GET /plans/:id/build-planning` | Exists in Rust behind `desktop_rebuild` (GRE-1119) |

## Delivery slices

Numbers preserve the [product build order](../ui-ux-target.md#build-order); Linear owns execution state.

| # | Slice | Usable result |
| --- | --- | --- |
| 2 | Builds list and Build Overview, read-only | Browse Builds |
| 7 | Order fields, duplicate protection and due-date ranking | One authoritative Build per order |

## Create Build

Builds has a **Create Build** button under its heading in every state
(`web/apps/ui/src/builds/CreateBuildDialog.tsx`). The dialog takes a name and a
**Base Source** from the Library, preselected to the first Source, with
**No base Source** as the other choice. With no Sources it links to Library
instead. **Create** sends `createBuild` once, is disabled without a name and
offline, and opens the new Build's Overview. A 4xx keeps the dialog open with
the reason; a 5xx or lost answer says the create could not be confirmed and
reads the list again. The core refuses a second Build with the same name, so
pressing Create again cannot make a duplicate.

A Build with no layers shows **Set base Source** on its Sources tab, and its
Overview links there. It sends `setBuildBase` once and reads the Build again.
