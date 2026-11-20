# Replacement UI empty-state actions

The replacement UI keeps an empty read distinct from loading, offline and failed
reads. A next action navigates to existing work or changes a local filter. It
never accepts a Plan, confirms printed units or sends a printer command.

## Current slice

| State | Next action | Effect |
| --- | --- | --- |
| Today has no ranked tasks | View Builds | Opens `/builds` to inspect existing Builds. |
| No Builds match a due-date filter | Show all Builds | Selects All without changing any Build. |
| A Build has no layers (Sources tab) | Set base Source | Sends `setBuildBase` once with the chosen Library Source (GRE-1864). After an unconfirmed send it offers Read again and Set a base anyway instead; see [Set base Source](#set-base-source). With an empty Library it links to Library instead. |
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
new Source, so a second send replaces the first. The plain form therefore opens
only when all three hold:

- The Build's layers were read after the form mounted. Leaving the Sources tab
  and coming back reads them again; cached layers never open it.
- No `setBuildBase` for that Build is in flight. The send and the re-read after
  it share the mutation key `["core", "setBuildBase", id]` in the query client,
  which outlives the tab.
- The Build has no unconfirmed send. From the press until the core confirms or
  refuses it, a send is recorded per Build in the query client
  (`web/apps/ui/src/library/unconfirmedBase.ts`). The record is never
  garbage-collected, so it outlives the tab and the send settling. A 5xx or lost
  answer leaves it in place.

A 200 answer is the Build's layers, and the UI writes them into the Build cache
at once. A 4xx is a refusal: nothing was written, and the form shows the reason.

While a Build has an unconfirmed send, the Sources tab shows no form. It says
the send could not be confirmed and that the core may still set that Source,
and offers **Read again**. A read that shows a base clears the record and shows
the base. A read of zero layers does not, because the write may still be on its
way. **Set a base anyway** is the owner's way out when the first send never
arrived. It explains that if the earlier send reaches the core later, whichever
of the two arrives last becomes the base, and it needs the Source chosen again.
It opens only after a read that succeeded since the panel appeared.

The UI cannot see a send from before a page reload, because the record lives in
memory. Another window or client can also send at the same time. The real guard
belongs in the core: a base PUT that refuses to replace an existing base unless
the caller names the base it expects.
`web/apps/ui/src/library/SetBaseSource.test.tsx` covers leaving and coming back
while the PUT or its re-read is held, a base set from another window, a 502,
500 or dropped answer whose write lands later, Set a base anyway, and a second
Build.

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
