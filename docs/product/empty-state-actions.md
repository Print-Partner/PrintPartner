# Replacement UI empty-state actions

The replacement UI keeps an empty read distinct from loading, offline and failed
reads. A next action navigates to existing work or changes a local filter. It
never accepts a Plan, confirms printed units or sends a printer command.

## Current slice

| State | Next action | Effect |
| --- | --- | --- |
| Today has no ranked tasks | View Builds | Opens `/builds` to inspect existing Builds. |
| No Builds match a due-date filter | Show all Builds | Selects All without changing any Build. |
| A Build has no layers (Sources tab) | Set base Source | Sends `setBuildBase` once with the chosen Library Source (GRE-1401 slice a). See [Set base Source](#set-base-source). With an empty Library it links to Library instead. |
| A Build has no layers (Overview) | Choose its base Source | Opens the Build's Sources tab. |
| Build History has no entries | Open Plan | Opens that Build's Plan tab. Shared `HISTORY_EMPTY` copy still explains snapshots and restore. |
| Farm Log has no entries | View Farm | Opens `/farm` to inspect the fleet. |
| Library has no Sources | Add Source | Opens the Add Source dialog (GRE-1513). Create adds a Source record. A non-GitHub row can upload a ZIP (GRE-1725). A GitHub Source starts one sync (GRE-1728). |

Today renders its ranked-task empty message inside a successful checkoff read.
An empty task list does not imply that every Build is finished or ready to print.

## Remaining audit

GRE-1401 holds the first-run walkthrough and the remaining UI13-03 audit.
Library's empty state leads to Add Source (GRE-1513), which creates a Source
record. A non-GitHub row can upload a ZIP (GRE-1725). A GitHub Source starts
one sync (GRE-1728). `/builds` is still
a read-only list: there is no Build creation screen in the replacement router,
so its empty state cannot yet lead to creating the first Build. The walkthrough
waits for that flow and for Source files; it must use disposable data when they
land.

The remaining audit includes Source activity, empty Source files,
deferred Build tabs, Farm and Settings-dependent states, and command-palette
no-match guidance. Coordinate Settings with GRE-1122 and discovery/probe with
GRE-1127. Keep the existing Help destinations and `planCopy.ts` wording.

## Set base Source

The core's `PUT /api/v2/plans/{id}/layers/base` moves an existing base to the
new Source, so a second send replaces the first. The form therefore opens only
when both hold:

- The Build's layers were read after the form mounted. Leaving the Sources tab
  and coming back reads them again; cached layers never open it.
- No `setBuildBase` for that Build is in flight. The send and the re-read after
  it share the mutation key `["core", "setBuildBase", id]` in the query client,
  which outlives the tab.

A 200 answer is the Build's layers, and the UI writes them into the Build cache
at once. A lost answer says the send could not be confirmed. A failed re-read
keeps the form closed and offers **Read again**.
`web/apps/ui/src/library/SetBaseSource.test.tsx` covers leaving and coming back
while the PUT or its re-read is held, and a base set from another window.

## Capture and checks

Run the Rust-only evidence harness under a private X display, before and after
editing the UI:

```sh
xvfb-run -a scripts/evidence/run.sh --empty-states --module farm --module orders \
  --shot /today --shot /builds --shot /builds/1000/history --shot /farm/log
```

`--empty-states` changes only the harness-generated synthetic seed before startup.
It archives the seeded undated Builds, adds archived Build 1000
without history, and removes waiting checkoff links, the Farm log and the send
queue. Existing Plan histories stay intact. The capture selects Overdue on
Builds to show an empty filter; it does not change real data or issue a send.

Run the UI package tests, evidence harness tests, `xvfb-run -a just ci --fast`
and `just report --base origin/main`. Inspect every PNG before publishing it.
