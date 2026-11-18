# Replacement UI empty-state actions

The replacement UI keeps an empty read distinct from loading, offline and failed
reads. Every empty state names the next useful action. A next action navigates,
changes a local filter, or opens a create dialog. It never accepts a Plan,
confirms printed units or sends a printer command.

## Empty states and their next action

| State | Next action | Effect |
| --- | --- | --- |
| Today has no ranked tasks | View Builds | Opens `/builds` to inspect existing Builds. |
| Builds has no Builds | Create Build, and the first-Build steps | Opens the Create Build dialog (GRE-1401). The steps are `FIRST_BUILD_STEPS` in `planCopy.ts`, the same list Help shows. |
| Create Build with an empty Library | Add a Source in Library | Opens `/library`. The dialog can still create a Build with no base Source. |
| No Builds match a due-date filter | Show all Builds | Selects All without changing any Build. |
| A Build has no layers (Sources tab) | Set base Source | Sends `setBuildBase` once with the chosen Library Source, then stays disabled until the Build has been read again, so a second send cannot replace the base the first set. A failed re-read offers Read again. With an empty Library it links to Library instead. |
| A Build has no layers (Overview) | Choose its base Source | Opens the Build's Sources tab. |
| Build History has no entries | Open Plan | Opens that Build's Plan tab. Shared `HISTORY_EMPTY` copy still explains snapshots and restore. |
| Library has no Sources | Add Source | Opens the Add Source dialog (GRE-1513). A non-GitHub row can upload a ZIP (GRE-1725). A GitHub Source starts one sync (GRE-1728). |
| Library has no Source activity, with a GitHub Source | Sync a GitHub Source from its row | Activity lists sync updates and failures; the row's Sync button starts one. |
| Library has no Source activity, with only ZIP Sources | Add Source, choosing GitHub repository | Activity records only GitHub syncs and update checks, and a ZIP Source has no Sync button, so the text names Add Source under the heading. |
| Farm has no printers | Add a printer in Settings | Opens `/settings#printers`, where Add a printer lives. |
| A Module is off | See Modules in Settings | `ModuleOff` links to `/settings#modules`. |
| Command palette has no match | Clear search | Says the search matched nothing, suggests a screen name or "help", and clears the search back to every command. |

Today renders its ranked-task empty message inside a successful checkoff read.
An empty task list does not imply that every Build is finished or ready to print.

No Build tab is deferred. `routes.tsx` maps every tab path in `BUILD_TABS` to its
screen through a typed record, so a tab added without a screen fails the
typecheck instead of showing a placeholder.

## First run

From an empty install: Builds is empty and names Create Build. With no Sources,
the Create Build dialog leads to Library. Add Source creates the Source; a ZIP
row can then upload files, or a GitHub Source syncs. Back on Builds, Create Build
preselects that Source as the base, and the new Build opens on its Overview with
the Source as its base layer. Help's **Your first Build** section lists the same
steps. `web/apps/ui/src/builds/FirstBuild.test.tsx` walks this path on a scripted
core with in-memory data.

## Owned elsewhere

Settings' own empty states (no Modules reported, no integrations) belong to
GRE-1122. Printer discovery and probe on first run belong to GRE-1127. When those
flows land, their empty states should lead to them; Farm's empty state already
points at Settings' Printers section.

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
