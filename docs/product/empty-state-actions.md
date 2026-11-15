# Replacement UI empty-state actions

The replacement UI keeps an empty read distinct from loading, offline and failed
reads. A next action navigates to existing work or changes a local filter. It
never accepts a Plan, confirms printed units or sends a printer command.

## Current slice

| State | Next action | Effect |
| --- | --- | --- |
| Today has no ranked tasks | View Builds | Opens `/builds` to inspect existing Builds. |
| No Builds match a due-date filter | Show all Builds | Selects All without changing any Build. |
| Build History has no entries | Open Plan | Opens that Build's Plan tab. Shared `HISTORY_EMPTY` copy still explains snapshots and restore. |
| Farm Log has no entries | View Farm | Opens `/farm` to inspect the fleet. |
| Library has no Sources | Add Source | Opens the Add Source dialog (GRE-1513). Create adds a Source record only; uploading and sync come later. |

Today renders its ranked-task empty message inside a successful checkoff read.
An empty task list does not imply that every Build is finished or ready to print.

## Remaining audit

GRE-1401 holds the first-run walkthrough and the remaining UI13-03 audit.
Library's empty state leads to Add Source (GRE-1513), which creates a Source
record; uploading a ZIP and sync are later parts of GRE-1365. `/builds` is still
a read-only list: there is no Build creation screen in the replacement router,
so its empty state cannot yet lead to creating the first Build. The walkthrough
waits for that flow and for Source files; it must use disposable data when they
land.

The remaining audit includes Source activity, zero-layer Build Sources,
deferred Build tabs, Farm and Settings-dependent states, and command-palette
no-match guidance. Coordinate Settings with GRE-1122 and discovery/probe with
GRE-1127. Keep the existing Help destinations and `planCopy.ts` wording.

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
