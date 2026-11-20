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
  it share the mutation key `["core", "setBuildBase", id]`, and a press checks
  it again, so two forms cannot both send.
- The Build has no unconfirmed send. Each send is recorded at the press, before
  it leaves, in module state (`web/apps/ui/src/library/unconfirmedBase.ts`),
  not in the query client. Sign-in, account setup and sign-out clear or prune
  the query client, and the record survives each of them; only a page reload
  ends it.

A send leaves the record only when the core confirms it (a 200 whose body
matches the contract) or refuses it with its own problem: an
`application/problem+json` body valid against the generated `Problem` schema,
whose `status` matches the answer, with a status the set-base handler refuses
with: 400, 401, 403, 404 or 413. Each of those is decided before or inside the
one write transaction, so nothing of that request commits later. Every other
answer keeps the send: any other 4xx, such as a proxy's 408, 421, 425, 429 or
its own 400 or 403 page; a redirect, which the PUT refuses to follow; every
5xx, including the core's 503 "outcome unknown"; a dropped connection; and a
200 whose body fails the contract.

While a Build has an unconfirmed send, the Sources tab shows no form. It says
the send could not be confirmed and that the core may still set that Source,
and offers **Read again**. A read that shows a base ends the record. A read of
zero layers does not, because the write may still be on its way. **Set a base
anyway** is the owner's way out when the first send never arrived. It explains
that the earlier send can still replace the base set now, and that the base set
now can replace one the earlier send already set. It needs the Source chosen
again, and it opens only after a read that succeeded since the Sources tab
opened. A refusal of that send ends only its own record, so the earlier send
still keeps the plain form closed.

The UI cannot see a send from before a page reload, from another window or
device, or from an MCP or API client. The real guard belongs in the core
(GRE-1866): a base PUT that refuses to replace an existing base unless the
caller names the base it expects.
`web/apps/ui/src/library/SetBaseSource.test.tsx` covers each answer above that
keeps a send, each cache reset, Set a base anyway and its refusal, Cancel, a
second Build, and leaving and coming back while the PUT or its re-read is held.

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
