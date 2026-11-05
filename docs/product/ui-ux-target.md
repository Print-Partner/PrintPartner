# UI and UX target

Status: accepted for implementation on 2026-11-02. Chosen by an arena of three independent designs judged by two reviewers. Supersedes the UI v11 screen sequence. See [ADR 0008](../adr/0008-replace-the-old-screens.md) for why the old screens are replaced rather than ported.

This page is the design the new UI is built against. Linear owns the tickets; this page owns the shape.

## Product promise

Open Print Partner and it shows the one thing that gets a printer working again, lets you finish it where you are, and lets you take it back.

## Why this shape

The measured behaviour, from eight days of redacted logs:

| Evidence | Number |
| --- | --- |
| Manual checkoff ticks | 386, peaking at 9 PM (143) and 8 PM (108) |
| Largest single session | Build 22, 100 ticks in 15 bursts |
| Finished prints the app could not match | 18 (Aug 29 to Oct 12), list loaded 130 times, 0 resolved |
| Builds-list loads | 533; 6 of 8 long sessions start there |
| Plan changes | 437 UI applies against 228 assistant decisions |
| Plates sent from Print Partner | 4 in 90 days, none in the last 30 |
| Snapshots ever created | 2, both for disasters |
| Printer links | none since Sep 8 |

Three facts follow and they govern every screen below.

1. **Recording what came off a printer is the dominant job, and it happens in bursts in the evening.** Every other flow is rarer.
2. **Sending plates is rare.** Build for it, but do not make the farm dispatch queue the centre of the product.
3. **Printer-driven data cannot be assumed.** No link has reported since Sep 8. Any screen that depends on printer data must degrade to manual checkoff without losing the job.

## Information architecture

Five destinations in a left rail, plus a command palette.

| Destination | Key | Holds |
| --- | --- | --- |
| Today | `G T` | Pulse line, hero task, ranked tasks, open Build cards |
| Builds | `G B` | Builds list, then the Build workspace |
| Farm | `G F` | Printers, queue, farm log |
| Library | `G L` | Library Sources, import, sync, revisions |
| Settings | `G ,` | Modules, integrations, sharing, devices, backups |

The pulse line (printer counts by state) is on every screen. `Cmd/Ctrl+K` opens the palette; `?` shows shortcuts.

A Build workspace has tabs: **Overview, Sources, Plan, Production, Checkoff, History**. The Machine tab is parked with STEP conversion and is not part of this delivery.

Domain words come from [CONTEXT.md](../../CONTEXT.md). Build is the container; Plan is the requirements at one accepted revision.

### Dropped from the old UI

| Old | Fate | Reason |
| --- | --- | --- |
| Four-stage spine as app navigation | Becomes Build-local tabs | The stages belong to one Build |
| Global Checkoff page | Build Checkoff tab plus Clear beds | One open page made 10,905 polling requests |
| All Production attention list | Today tasks and the Farm queue | It listed everything instead of ranking actions |
| Per-page polling | Live events | Same 10,905 requests |
| Backup as the only undo | Backups stay for disasters; History is the undo | Snapshots were a disaster tool used as undo |
| Stale flag on every Build | Shown only when open work needs a decision | 8 of 10 stale Builds were fully checked off |
| Community board | Dropped | Single-user product |
| Coloured stage dots | Dropped | Colour-only status |

## The three flows that matter

### Clear the beds

Today shows one hero task: "Clear 2 beds and confirm what came off", with the reason ("frees Voron 2.4 and Trident") and the diff inline. One press confirms the batch.

Checkoff input model, taken from the free-synthesis candidate because it is the only one that specifies the mechanism:

- `P` marks a plate printed, showing what it will mark before it commits.
- `F` fills all remaining units of a part.
- `0` to `9` set a count.
- `Space` or click ticks one unit; `Shift` selects a run.
- One burst is one transaction with an idempotency key, so a phone retry cannot double-count, and an undo token.
- Per-unit ticking remains available. It is the fallback, not the main path.

Step counts, measured before and counted after:

| Flow | Before | After |
| --- | --- | --- |
| F1, Build 22, 100 ticks in 15 bursts | 102 | 15 to 16, if each burst is one plate |
| F2, Build 19, 47 ticks in 6 bursts | 50 | 7 to 8 |
| F3, 18 unmatched prints | 0 resolved | about 5 to 20 depending on suggestion accuracy |

The plate assumption must be validated against Build 22's accepted plates before these numbers are quoted as achieved. If a burst is not a plate, part fill carries the saving, and the number rises toward the count of distinct parts touched.

Checkoff is not a Plan change, so it does not pass the Accept gate. It is undoable as a burst.

### Change a Build safely

Every Plan change, including rename, order, customer and due date, lands in a draft and goes through the Accept gate. The gate previews old against new, names printed units affected, plate changes, stock impact and duplicate orders, and states that a snapshot is saved before accepting.

One order is one Build, enforced transactionally. A duplicate creates a detached experiment that records its parent, and promoting an alternative retires the previous authority atomically. Existing duplicate records are displayed, never silently merged.

History shows accepted revisions, snapshots, checkoff bursts and sends. Restore as draft (`R`) opens the gate. Restoring a plan never rewinds printed evidence or a printer command.

### Handle prints the app could not match

A Match prints review lists the backlog with confidence in words (Exact, Likely, No suggestion), bulk select, and one press per confident group. Low-confidence rows start unchecked. Nothing is ever ticked automatically: verify-first is a product rule, and 18 unresolved prints are the evidence that matching is not reliable enough to skip confirmation.

## Screen states

Every screen designs five states, and the fifth is not optional:

1. **Loading.** Skeletons after 150 ms; reserves the eventual layout; never shows false zeros.
2. **Empty.** A successful read with no records, with the next useful action.
3. **Error.** Keeps last-known data with its timestamp and a Retry. Never a blank page.
4. **Module off.** Says which module is off, what would appear, and links to Settings. Module state comes from the core, not from guessing at a 404.
5. **Offline.** The core is unreachable: navigation of already-loaded records still works, mutations are disabled with the reason in text and in the screen-reader description.

`ScreenTemplate` in `web/apps/ui/src/screen` renders all five, and `MutationButton` there is the one way a screen offers a change, so offline disables it everywhere. See the [ui package README](../../web/apps/ui/README.md#app-frame).

Printer states are never blank. The vocabulary is Printing, Bed to clear, Ready, Paused, Failed, Unreachable, Not set up, Manual, Unknown. Execution state and bed-clear state are separate facts even when one row summarises both. An idle printer shows Ready only when its bed is confirmed clear; an unknown bed shows Bed to clear. `pp_domain::printer_display_state` holds the vocabulary and the summary rule.

## Design system

Graphite and cyan, IBM Plex, as decided. Dark is the default; light and a shop-floor theme exist.

- **Cyan means interaction only.** One primary button per screen, focus rings, selection. Never status, never filament.
- **Status is always an icon plus a word,** generated from one Rust enum so a new state cannot ship without its label, icon and colour.
- **Filament swatches are round chips with a border**, visually distinct from status pills.
- **Type.** IBM Plex Sans 400/500/600 for UI, IBM Plex Mono 400/500 with tabular figures for counts, ETAs, grams, IDs and order numbers. Bundled, no CDN.
- **Density.** Comfortable (40 px rows) and compact (32 px). Checkoff and phone targets are 44 to 48 px.
- **Motion.** 120 to 200 ms, ease-out, showing where a thing went. Reduced motion keeps the same information as text.
- **Accessibility.** WCAG 2.2 AA, focus appearance, 200% text, 400% reflow, forced colors, live regions for tallies and printer events. Drag has a menu and a keyboard equivalent. Every disabled control explains itself.
- **Platforms.** WKWebView, WebView2 and WebKitGTK are release targets. On WebKitGTK set `WEBKIT_DISABLE_DMABUF_RENDERER=1` (the repo's value, not `WEBKIT_DMABUF_RENDERER_FORCE_SHM`), and the plate preview must have a non-WebGL fallback. Nothing in the first release needs WebGL.

The tokens that implement this section, and the gate that checks them, are described in [Replacement UI tokens](ui-tokens.md).

## Contracts the UI needs

Rules: the UI never calls Node. A contract marked **Port** exists in Node today and must move to Rust before the screen uses it. **Exists** means a Rust route is registered in `rust/crates/pp-api`. **New** means no route exists anywhere.

| Need | Contract | Status |
| --- | --- | --- |
| Builds list, detail, layers | `GET /api/v1/plans`, `/plans/{id}`, `/layers`, `/kit-manifest` | Exists |
| Drafts | `GET/PATCH /api/v2/plans/{id}/drafts*` (drafts are mounted under `/api/v2`, not `/api/v1`) | Exists |
| Checkoff read | `GET /plans/{id}/checkoff`, `/parts/{id}/assembled` | Exists |
| Sources | `GET /sources`, `/sources/activity`, naming, import rules | Exists |
| Jobs and progress | `GET /api/v1/jobs`, `/ws/jobs/{id}` | Exists |
| Build workflow projection | `GET /plans/:id/workflow` | Exists (GRE-1123) |
| Build planning phase | `GET /plans/:id/build-planning` | Exists in Rust behind `desktop_rebuild` (GRE-1119) |
| Assistant decisions | `GET /plans/:id/decisions` | Exists in Rust behind `desktop_rebuild` (GRE-1119); History lists them too |
| Printers | `GET /api/v2/printers` | Exists in Rust behind `desktop_rebuild` (GRE-1125): Moonraker observed; PrusaLink and Bambu read as Manual until their drivers are composed (GRE-360, GRE-1044). See [Farm reads](../farm-reads.md) |
| Printer checkoff and unmatched | `GET /api/v2/printer-checkoff`, `/unattributed` | Exists in Rust behind `desktop_rebuild` (GRE-1125), read-only |
| Send queue | `GET /api/v2/printer-send-queue` | Exists in Rust behind `desktop_rebuild` (GRE-1125), read-only |
| Spoolman stock | `GET /integrations/:id/spoolman/spools` | Port |
| Spoolman shortfalls | `GET /plans/:id/filament-shortfalls` (Rust-only) | Exists; grams needed reads `unknown` (`grams_not_recorded`) until the GRE-1165 writer records grams per print (slicer estimate at send, measured at finish); basis is `measured`, `estimated` or `unknown` with a reason. The workflow projection consumes it next |
| Accept | `POST /plans/:id/drafts/:draftId/apply` | Port |
| Verify one result | `POST /printer-checkoff/verify`, `/reconcile`, `/dismiss` | Port |
| Health with module flags | `GET /health` `modules` list with off reason (Rust-only mode) | Exists |
| Ranked task feed | `GET /api/v1/tasks` | Exists (GRE-1123), a view over the workflow projection |
| Verify several beds at once | `POST /printer-checkoff/verify-batch` with `undo_token` | New |
| Farm log with undo | `GET /api/v2/farm-log`, `POST /api/v2/farm-log/{id}/undo` | Exists in Rust behind `desktop_rebuild` (GRE-1126); bursts and their undos are logged, sends never offer undo. See [Farm log](../farm-log.md) |
| Match suggestions | `GET /printer-checkoff/unattributed/suggestions` | New |
| Build history | `GET /plans/:id/history` | New contract issue; keep UI ticket GRE-1023 unchanged |
| Accept preview with checks | `POST /plans/:id/drafts/:draftId/preview` | New |
| Restore as draft | `POST /plans/:id/snapshots/:sid/restore` in draft mode | New variant |
| Printer stream | `GET /ws/printers` | Rust behind `desktop_rebuild` (GRE-1014, [Printer stream](../printer-stream.md)) |
| Domain events | one authenticated, resumable stream | New |

The task feed must be a read view over the ported Build Workflow projection (GRE-415), not a second ranking engine. Today, Build Overview and MCP must not be able to disagree about the next action.

## Build order

Every slice is one PR under about 1,500 changed lines, ships behind `desktop_rebuild`, and leaves the product in a usable state. Linear owns issue IDs and execution state; this table defines product order.

| # | Slice | Usable result |
| --- | --- | --- |
| 0 | Serve the authenticated Rust product entry point behind the flag | The replacement UI can reach Rust without Node fallback |
| 1 | Tokens, type, status descriptors, app frame and command palette | A real frame in all three webviews |
| 2 | Builds list and Build Overview, read-only | Browse Builds |
| 3 | Farm printers with honest states and the live pulse line | See every printer, including Annex K3 and Micron |
| 4 | Today task feed, single-bed confirmation and undo | The evening loop works |
| 5 | Clear beds in batches and review unmatched prints | F1 and F3 targets |
| 6 | Plan tab and Accept gate with a snapshot on accept | Edit and accept in the replacement UI |
| 7 | Order fields, duplicate protection and due-date ranking | One authoritative Build per order |
| 8 | History tab and farm log with undo | Fix a wrong change in two steps |
| 9 | Spoolman shortfall tasks | See what stock needs attention |
| 10 | Production, send sheet and risk-scaled cancel confirmation | Send and cancel explicitly |
| 11 | Phone bench and Share on network pairing | Confirm beds from a phone |
| 12 | Native menus, notifications and tray | See alerts outside the window |
| 13 | First run, help, empty states and copy | Set up a new install and create the first Build |
| 14 | Library and Sources tab | Manage Sources |

### Tracker rules

Do not edit, comment on, change the status, labels, relations, assignee or milestone of any existing UI v11 screen ticket or protected per-route/cutover ticket. GRE-621 keeps its existing D26 scope. Create separate issues for replacement UI work and for any Rust-only product entry-point work that remains. Reuse a backend issue only when its current scope already matches and it is outside the protected set.

Track missing contract work separately: task feed, batch Checkoff, match suggestions, Build history, Accept preview, restore-as-draft, domain event stream, and health module flags. Use GRE-1014 for `/ws/printers` only if its existing contract fully covers the target. Do not create a duplicate contract ticket.

Machine and STEP conversion are parked. GRE-1007 stays untouched and outside this build order until Chad approves its re-entry.

### A live contradiction to fix

`web/apps/server/src/routes/workflow-guide.ts` tells users, under Plan: "There is no separate acceptance or publishing step. Changes save automatically." That describes the current Node behavior, not the replacement Accept gate. Add accurate copy to a new replacement-UI help issue; leave GRE-1012 and all other protected screen tickets unchanged.

## Open questions

1. Is one burst one plate? Validate on Build 22 before quoting the step counts.
2. What is the match accuracy on the 18-print backlog? Hand-count it. Nothing auto-confirms until it is known.
