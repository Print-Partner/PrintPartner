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

The pulse line (printer counts by state) is on every screen. `Cmd/Ctrl+K` opens the palette; `?` opens Help, which explains how Plan changes save and lists the shortcuts.

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

See [Clear the beds](ui-ux-target/checkoff.md#clear-the-beds).

### Change a Build safely

See [Change a Build safely](ui-ux-target/plan.md#change-a-build-safely).

### Handle prints the app could not match

See [Handle prints the app could not match](ui-ux-target/checkoff.md#handle-prints-the-app-could-not-match).

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

Each feature page owns its contract rows and delivery slices. For a feature change,
edit only that feature's page. Update this index only when adding or removing a feature,
and keep cross-feature design rules here. Do not copy contract rows back into this page.

| Feature | Page | Delivery slices |
| --- | --- | --- |
| App frame and settings | [App frame and settings](ui-ux-target/app-frame.md) | 0, 1, 11, 12, 13 |
| Builds | [Builds](ui-ux-target/builds.md) | 2, 7 |
| Farm printers | [Farm printers](ui-ux-target/farm.md) | 3 |
| Today | [Today](ui-ux-target/today.md) | 4 |
| Checkoff and unmatched prints | [Checkoff and unmatched prints](ui-ux-target/checkoff.md) | 5 |
| Plan and Accept | [Plan and Accept](ui-ux-target/plan.md) | 6 |
| History and farm log | [History and farm log](ui-ux-target/history.md) | 8 |
| Spoolman stock | [Spoolman stock](ui-ux-target/stock.md) | 9 |
| Production and sends | [Production and sends](ui-ux-target/production.md) | 10 |
| Library and Sources | [Library and Sources](ui-ux-target/library.md) | 14 |

## Build order

Every slice is one PR under about 1,500 changed lines, ships behind `desktop_rebuild`, and leaves the product in a usable state. Linear owns issue IDs and execution state; this table defines product order.

Delivery slices retain their numbers, 0 through 14, on the feature pages linked above.
Read them in numeric order across pages; a page's later slice does not move ahead of
an earlier slice on another page. Change a slice's wording in its feature page.

### Tracker rules

Do not edit, comment on, change the status, labels, relations, assignee or milestone of any existing UI v11 screen ticket or protected per-route/cutover ticket. GRE-621 keeps its existing D26 scope. Create separate issues for replacement UI work and for any Rust-only product entry-point work that remains. Reuse a backend issue only when its current scope already matches and it is outside the protected set.

Track missing contract work separately: task feed, batch Checkoff, match suggestions, Build history, Accept preview, restore-as-draft, domain event stream, and health module flags. Use GRE-1014 for `/ws/printers` only if its existing contract fully covers the target. Do not create a duplicate contract ticket.

Machine and STEP conversion are parked. GRE-1007 stays untouched and outside this build order until Chad approves its re-entry.

### A live contradiction to fix

`web/apps/server/src/routes/workflow-guide.ts` tells users, under Plan: "There is no separate acceptance or publishing step. Changes save automatically." That describes the current Node behavior, not the replacement Accept gate. Add accurate copy to a new replacement-UI help issue; leave GRE-1012 and all other protected screen tickets unchanged. GRE-1140 added that copy to the replacement UI ([Change a Build safely](#change-a-build-safely)). The old Node guide is unchanged unless the owner asks. The Rust workflow projection's Open Plan action, which Today and MCP share, now says "Changes save to a draft as you make them, and the Build changes only when you accept it." (GRE-1355). Only the old Node copy (`web/packages/domain/src/build-workflow.ts` and the guide) still says "Changes save automatically."

### Replacement Help

The header Help button, command palette and `?` shortcut open the same dialog.
Help describes Today, Builds, Farm, Library and Settings from the shared destination
list. Its links close Help and open the chosen destination. Help explains where
keyboard shortcuts work and lets the user turn single-key shortcuts off.

The Plan changes section still reads `planCopy.ts`, shared with the Plan and History
tabs: saved drafts survive app closure, and only explicit Accept changes the Build.
GRE-1106 covers this Help slice and the replacement-UI copy scan. GRE-1384 carries
UI13-01 (first Source and Build on disposable data) and UI13-03 (every empty state
names the next useful action). Source import, Build creation, Settings and printer
discovery are outside this Help slice.

## Open questions

1. Is one burst one plate? Validate on Build 22 before quoting the step counts.
2. What is the match accuracy on the 18-print backlog? Hand-count it. Nothing auto-confirms until it is known.
