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
| Create a Build | `POST /api/v2/plans` (`createBuild`) with `name` and an optional `base_project_id` | Exists in Rust behind `desktop_rebuild`; in the generated client since GRE-1876. Builds' Create Build dialog sends it; see [Create Build](#create-build) and [Plan create](../../gre-792-builds-http.md#create-post-plans) |
| Set a Build's base Source | `PUT /api/v2/plans/{id}/layers/base` (`setBuildBase`) with `project_id` | Exists in Rust behind `desktop_rebuild`; in the generated client since GRE-1401. Adds the base layer, or moves an existing one to that Source, and answers the Build's layers. The Sources tab of a Build with no layers sends it; see [Set base Source](../empty-state-actions.md#set-base-source) |
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
**Base Source** from the Library. The first Source is preselected, and **No base
Source** is the other choice. With no Sources it links to Library. While the
Source list loads, **Create** waits. If the list fails, the dialog says so and
offers **Try again**, so Create never sends a "No base Source" the user did not
choose. **Create** is also disabled without a name, with a name over the core's
4,096-character limit, offline, and while a create or its check is running.

Create sends `createBuild` once. The create and its check live outside the
dialog (`web/apps/ui/src/builds/createBuild.ts`), so closing the dialog keeps
them. A reopened dialog shows the same name, says the create is running or
being checked, and cannot send a second one. An answer that arrives while no
dialog is open opens nothing; the next dialog shows the check's result.

Only an exact HTTP 200, not redirected, with a body that matches the contract,
opens the new Build's Overview. The core's own refusals keep the dialog open
with the reason: a problem document with status 400, 401, 403, 409 or 413. A
400 `Profile name is ...` reads "Build name is ...", a 400 `Project not found`
says the Source left the Library, and the 409 Build limit shows the core's
detail. A 400 `Profile already exists` reads the Builds list and links to the
Build that holds the name; when the list does not load or does not show it, the
dialog still says the name is in use.

Every other answer may hide a created Build: a 5xx, a dropped connection, a
proxy page, another 2xx such as 202, or a 200 that fails the contract. The
dialog never resends. As
[the repeat rule](../../gre-792-builds-http.md#client-contract-createbuild-gre-1876)
says, it reads the Builds list and looks for the exact name. It cancels any
list read already in flight, because that read may have started before the
create, and sends a new one:

- The name is there, and the list loaded before the press did not hold it: the
  Build was created, and the dialog links to it.
- The name is there, but no list had loaded before the press, or that list
  already held it: the dialog links to it and says it may be an earlier Build.
- The name is not there: the dialog says pressing Create again is safe, because
  the core refuses a second Build with the same name.
- The list did not load: the dialog says to check Builds before pressing Create
  again.

`web/apps/ui/src/builds/FirstBuild.test.tsx` covers each answer.
