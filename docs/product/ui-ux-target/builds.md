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

Builds has a **Create Build** button in every state (`CreateBuildDialog.tsx`):
a name and a **Base Source**, the first Library Source by default or **No base
Source**; with no Sources it links to Library. **Create** waits while Sources
load, offers **Try again** if they fail, and is disabled without a valid name
(4,096 characters at most), offline, and while a create or its check runs.

The create lives outside the dialog (`createBuild.ts`), so a reopened dialog
shows its name and state and cannot send a second one. Only an exact,
unredirected HTTP 200 with a contract-valid body opens the new Build; the POST
uses `redirect: "manual"` through `fetchCore` so `fetch` never resends it, and
the post-create Builds list read refuses redirects so a redirected list never
confirms the Build. The core's own problem
(400, 401, 403, 409 or 413, not redirected) is shown in the UI's words, and a
401 opens sign-in. `Profile already exists` links to the Build holding the name.

Any other answer may hide a created Build. As
[the repeat rule](../../gre-792-builds-http.md#client-contract-createbuild-gre-1876)
says, the dialog never resends: it reads the Builds list and looks for the exact
name. It says the Build was **created** when the list loaded before the press
lacked it, **may be an earlier one** when there was no such list or it held the
name, that pressing Create again is safe when the name is missing, and to check
Builds when the list did not load. Every 200 reads the list too. That read
cancels older reads, survives leaving Builds, and never overwrites a newer list:
if the cache moved on, as after sign-out or sign-in, it answers with the cache's
list or not at all, and its 401 opens no sign-in. `FirstBuild.test.tsx` covers
each case.
