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
new Source, so a second send replaces the first. One function,
`plainFormMayRender`, decides whether the plain form renders. It renders only
when the Build has no recorded send, in flight or unconfirmed, and a read since
the tab opened shows zero layers:

- The Build's layers were read after the form mounted. Leaving the Sources tab
  and coming back reads them again; cached layers never open it. Until that
  read succeeds the tab shows its status and no form.
- While a send for that Build is in flight, the tab shows "Setting the base
  Source." and no form, on every mount: after Overview and back, after leaving
  the Build, and after Back and Forward. When the send settles, the tab shows the
  base, the unconfirmed panel, or the form with the core's refusal. A press
  also checks the mutation key `["core", "setBuildBase", id]`, so two forms
  cannot both send.
- Each send is recorded at the press, before
  it leaves, in module state (`web/apps/ui/src/library/unconfirmedBase.ts`),
  not in the query client. Sign-in, account setup and sign-out clear or prune
  the query client, and the record survives each of them; only a page reload
  ends it.

A send leaves the record only when the core confirms it or refuses it. The
handler's only success is a 200 with the Build's layers, which then hold the
base layer it wrote, so confirmation is HTTP 200 exactly, with a body that
matches the contract and holds a `base` layer. A refusal is the core's own
problem: an
`application/problem+json` body valid against the generated `Problem` schema,
whose `status` matches the answer, with a status the set-base handler refuses
with: 400, 401, 403, 404 or 413. Each of those is decided before or inside the
one write transaction, so nothing of that request commits later. Every other
answer keeps the send: any other 4xx, such as a proxy's 408, 421, 425, 429 or
its own 400 or 403 page; a redirect, which the PUT refuses to follow; every
5xx, including the core's 503 "outcome unknown"; a dropped connection; any
other 2xx, such as a proxy's 202 or 204; and a 200 whose body fails the
contract or shows no base.

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
device, or from an MCP or API client. It cannot tell an intermediary's forged
problem document, valid and status-matched with one of the handler's refusal
statuses, from the core's own refusal, so a forged refusal reopens the form.
The browser may also re-send the same PUT below `fetch`, for example after a
421 or a connection reset before the headers arrive. Both copies carry the same
Source, so the base is the same. The real guard belongs in the core
(GRE-1866): a base PUT that refuses to replace an existing base unless the
caller names the base it expects.
`web/apps/ui/src/library/SetBaseSource.test.tsx` covers each answer above that
keeps a send, each cache reset (including History's Restore as draft, which
removes the Build's Plan query), Set a base anyway and its refusal, Cancel, a
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
