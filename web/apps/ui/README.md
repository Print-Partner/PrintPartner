# @print-partner/ui

The replacement Print Partner UI. It talks only to the Rust core. The design it
builds toward is [UI and UX target](../../../docs/product/ui-ux-target.md);
[ADR 0008](../../../docs/adr/0008-replace-the-old-screens.md) says why it is a new
package instead of a port of `apps/web`.

Today it has the app frame (rail, pulse line slot, command palette, shortcut
list), the shared five-state screen template, the read-only Builds list and Build
Overview, an empty screen for each other destination, a type specimen at
`/type`, the screen states on sample data at `/states/:kind`, and a Not found
route. Other product data screens arrive in their own slices.

## Commands

Run from the repository root after `npm --prefix web ci`:

```sh
npm --prefix web run dev --workspace @print-partner/ui
npm --prefix web run build --workspace @print-partner/ui
npm --prefix web run typecheck --workspace @print-partner/ui
npm --prefix web run lint --workspace @print-partner/ui
npm --prefix web run test --workspace @print-partner/ui
```

`just ci --only ui-package` runs typecheck, lint, test, build and the
`test:browser` font request check. The legacy root
`build`, `typecheck` and `test` scripts do not include this package.

## App frame

`src/frame/destinations.ts` lists the five destinations once; the rail, the
shortcuts, the palette and the shortcut list read it. `/` redirects to `/today`.

- `G` then `T`, `B`, `F`, `L` or `,` within 1.5 s goes to a destination, and `?`
  opens the shortcut list; both are ignored in text fields and behind a dialog.
  `Cmd/Ctrl+K` toggles the palette from anywhere (`src/frame/useShortcuts.ts`).
- Single-key shortcuts (the `G` sequences and `?`) can be turned off, as WCAG
  2.1.4 requires: a checkbox in the shortcut list and a palette command. The
  choice is saved in `localStorage` (`pp.characterShortcuts`); if storage throws,
  they stay on. `Cmd/Ctrl+K` always works.
- Dialogs use Radix Dialog and the palette uses cmdk, as `docs/architecture.md`
  asks. Type to filter, Up and Down, Enter, Escape; keys that belong to an IME
  composition are left alone. Focus stays in the dialog and returns to the
  element that opened it.
- The "Printer pulse" header region says "Printer counts not loaded" until the
  Farm slice feeds it, never zeros.
- `src/frame/connection.tsx` polls `GET /health` every 30 s, or 5 s while it
  fails, and shares `checking`, `online` or `offline`. Each probe gives up after
  4 s, and it keeps running while the browser reports no network.

Every screen renders through `ScreenTemplate` (`src/screen/ScreenTemplate.tsx`).
Loading reserves its
rows and shows skeletons after 150 ms with no numbers; error keeps last-known
data with its time and a Retry; module off links to Settings; offline keeps
loaded data and says changes are off. `MutationButton` is the button for any
change: offline it keeps focus, does nothing, and shows the reason beside it as
its `aria-describedby`. `/states/<kind>` renders each state on sample data; any
other kind is Not found.

`test/browser/frame.browser.mjs`, part of `npm run test:browser`, runs axe at
WCAG 2.2 AA in WebKitGTK on every frame page, state and dialog in all three
themes, and fails on sideways scrolling at 200% text and 400% zoom. With
`PP_UI_EVIDENCE_DIR=<dir>` it saves a screenshot of each load there. Its themes
come from `THEMES` in `src/theme/tokenGate.ts`. `test/browser/harness.mjs` holds
the loopback server and WebKitGTK launcher that both browser checks use.

## Talking to the Rust core

- The dev server listens on `127.0.0.1:5174` and forwards `/api`, `/ws` and
  `/health` to `PP_UI_CORE_URL`, for example
  `PP_UI_CORE_URL=http://127.0.0.1:8080`. The value must be an http(s) origin
  with no path. There is no default: without it the dev server forwards nothing,
  so it cannot reach the Node server by accident. `core-proxy.ts` holds this
  logic and `test/core-proxy.test.ts` tests it.
- UI code calls the core through `fetchCore` in `src/core/client.ts`. It accepts
  same-origin paths only and sends same-origin credentials. In the desktop app
  and Docker the Rust core serves this UI, so the paths reach it directly. An
  error answer names the core's problem `detail` when it sends one.
- `readCore` and `writeCore` in `src/core/rust.ts` are the typed GET and write.
  `writeCore` checks the body against the generated request schema before it
  sends, and the answer against the response schema.

## Builds

`/builds` lists every Build `GET /api/v2/plans` returns, in the core's order,
with its name, order number, accepted progress and next action
(`src/builds/BuildsList.tsx`). `/builds/:id` opens the Build workspace: the tab
bar Overview, Sources, Plan, Production, Checkoff and History (the Machine tab is
parked), with Overview at the index route (`src/builds/BuildWorkspace.tsx`).
Overview reads `GET /api/v2/plans/{id}`, `/layers` and `/workflow` and shows
progress, part count, the Plan's freshness with its reasons in words, the next
action with its reason, the count of open decisions, the order number and customer
request, last use, and the layers in layer order. The other tabs say
what they will hold and that a later slice brings them. Nothing here changes data.

- **Order fields follow the Orders Module.** `useModule` (`src/core/modules.ts`)
  reads the Module list from `GET /health`. Orders on shows the Order column and
  the Overview's order fields; Orders off hides them and says so with a link to
  Settings. While the core has not said, neither shows. The Builds screens have
  no Module of their own, so the whole-screen Module-off state does not apply.
- **No customer name or due date.** No Rust route or table holds them yet, so
  the list has no columns for them.
- **Open tasks and next action** come from the shared Rust workflow projection
  (GRE-1123), the read Today and MCP use (`src/core/workflow.ts`). The list reads
  `GET /api/v2/tasks` once: each open Build has one task, its next action, shown
  with "Needs you" when the projection marks it for attention; a Build the feed
  lists as unavailable (its stored data failed a check) shows "Not available"
  with the reason; a Build missing from the feed shows "Nothing open". The list keeps the core's Build order and
  never re-ranks by the feed. Overview reads `GET /api/v2/plans/{id}/workflow`:
  the next action and reason, and the count of open decisions, the sum of the
  stages' `task_count` (a next action such as Prepare Production is not a
  decision, so the count can be 0 beside it). If the feed or the workflow read fails, the Builds stay
  readable and that part says it could not load, or could not refresh and
  keeps the last next action with its time, with a Retry.
- **States.** Both screens map their query through `queryScreenState`
  (`src/screen/queryState.ts`): loading, empty, error with the last-known data,
  its time and Retry, and offline with what was loaded. A Build ID that is not a
  positive safe integer, or that the core answers 404 for, is Not found.
- **Keyboard.** Up, Down, Home and End move focus between the Build links. Rows
  are keyed by Build ID, so a refetch keeps focus where it was.
- **Large lists.** Every row renders; rows off screen use
  `content-visibility: auto` so they skip layout and paint. This was chosen from
  `test/browser/builds.browser.mjs` on 10,000 Builds on a 4-CPU cloud host:
  WebKitGTK showed the first row 2.0 to 3.5 s after navigation (fetch, schema
  check and render of all rows) and moved focus to the next row by the next
  frame in a median of 17 to 38 ms (max 89 ms); headless Chromium took 1.2 to
  2.1 s and a median of 67 to 108 ms. There is no approved latency budget, so the check records the
  numbers and fails only on a missing row, next actions that never load, or
  misplaced focus. Revisit
  virtualization if a measurement says otherwise.

## Today

`/today` (`src/today/TodayScreen.tsx`) reads `GET /api/v2/printer-checkoff`,
`GET /api/v2/tasks` and, with the Farm module on, `GET /api/v2/printers` for
printer names and link state. It rereads the results, tasks and printers every 30 s,
so a print that ends or a link that drops shows up without a reload.

- **Hero task.** When printer results wait in `awaiting_verify` with pending
  units, the hero is "Clear N beds and confirm what came off", the reason names
  the printers it frees, and each bed lists what confirming it marks: one line
  per Part with its file name, the count and the unit numbers (`unit_index` + 1).
  With no bed waiting, the hero is the top task from the shared Rust ranking,
  with its reason and counts, and the ranked list holds the rest.
- **Confirm one bed.** "Confirm <printer> bed" sends one
  `POST /api/v2/printer-checkoff/verify-batch` with one bed and a new
  `crypto.randomUUID()` idempotency key, confirming every pending unit of that
  result.
- **Clear beds burst** (`src/today/ClearBeds.tsx`, `src/today/beds.ts`). Each
  bed lists its pending units as checkboxes. On a focused unit, `P` picks every
  unit on its bed (the plate), `F` every unit of its Part on every bed, and `0`
  to `9` that many of its Part, lowest unit first, dropping the rest of that
  Part. `Space` or a click toggles one unit; with `Shift`, every unit from the
  last one toggled takes the new state. The preview says how many units on how
  many beds will be marked, and what each Part gains, before anything is sent.
  "Confirm N picked units" sends every picked bed in one verify-batch request
  with one key, and the tally carries one Undo for the whole burst. Rejecting
  units stays in the Build Checkoff tab (GRE-1121).
- **Progress fills from the left.** The core refuses a Part's unit while a lower
  unit of that Part is still pending (400 "Confirm must include lower incomplete
  units first"). When a pick or a bed's confirm skips a lower pending unit shown
  on another bed, Today names those units before the send, and "Pick them too"
  adds them to the burst. It never adds them on its own. A lower unit on a print
  still running (`watching`) cannot be picked, so Today says it is still printing
  and that the core refuses the confirm until it comes off. A lower unit on no
  loaded result is not known to the UI; the core's 400 detail shows instead.
- **Burst limit.** One verify-batch carries at most 50 beds. A bigger pick shows
  that limit in place of Confirm. A send refused by the client contract is never
  resent, and picks made while a confirm is in flight stay picked.
- **Retry.** A send that fails without an answer from the core (a network error
  or a lost answer) is resent up to twice with the same key, and the alert's
  Retry resends it again with the same key. The core answers a repeated key
  with the first receipt and writes nothing, so a retry never counts twice
  ([Checkoff bursts](../../../docs/desktop-rebuild.md#checkoff-bursts-gre-1090)).
  A refusal from the core (4xx) shows its detail and no Retry. Until a bed's
  confirm gets an answer, confirming that bed again reuses its key, so a write
  that landed while its answer was lost replays its receipt and Undo instead of
  being refused as already verified. A resent Undo whose first answer was lost
  gets 409 "already undone"; Today counts that as done and rereads.
- **Undo.** The tally, "Confirmed N units from <printers>.", sits in a polite
  `role="status"` region that is in the page before any confirm, beside an Undo
  button. Undo sends the receipt's `undo_token` to
  `POST /api/v2/printer-checkoff/verify-batch/undo`; the core restores the
  links, progress and outcomes, or refuses with 409 and a reason.
- **Printer links down.** Confirming a bed needs no printer. A bed whose printer
  the farm read shows unreachable, not set up, manual or unknown, or does not
  name, says to confirm by hand what came off. Results still `watching` on such
  a printer are listed under "Prints the app cannot see finish" with a link to
  the Build's Checkoff tab, so no print drops out of sight.
- With the core offline every confirm, Undo and Retry is disabled with the
  offline reason (`MutationButton`).
- **Tests.** `src/today/Today.test.tsx` runs against a fake core that applies
  verify-batch and undo like the Rust routes, including the replay of a
  repeated key, and counts the requests and marked units.

## Fonts and type

The UI bundles IBM Plex Sans 400/500/600 and IBM Plex Mono 400/500 as WOFF2 files
in `src/fonts/`, latin and latin-ext subsets, and makes no font request to any
other origin. The files are unmodified copies from `@fontsource/ibm-plex-sans`
and `@fontsource/ibm-plex-mono` 5.3.0 (OFL-1.1). `src/fonts/OFL.txt` and the
IBM Plex section of `THIRD_PARTY_NOTICES.md` carry the licence. To change a
file, copy it from a newer Fontsource release, update `fonts.css`, the version
here and the notices section, then run `npm run docs:licenses` from `web` so the
server's copy of the notices matches.

- `src/fonts/fonts.css` declares the faces. Sources are relative WOFF2 URLs only,
  with no `local()` and no remote URL; `test/fonts.test.ts` enforces that.
- `src/theme/tokens.css` defines `--font-sans` and `--font-mono`, naming the
  bundled families. `src/typography.css` holds the heading weights and the `.num`
  class: Plex Mono with `font-variant-numeric: tabular-nums`. Use `.num` for
  counts, ETAs, grams, IDs and order numbers.
- `/type` renders every face and figure role, for screenshot checks.
- `npm run test:browser` (after `build`) checks for zero requests to another
  origin in Chromium and WebKitGTK; see the
  [CI guide](../../../docs/development/ci.md#replacement-ui-package).

The build emits 10 font files, 176,284 bytes in total. A page in latin text
loads only the latin files it uses, at most 5 files and 100,620 bytes.

## Import boundary

The ESLint rule `print-partner/ui-boundary` (`web/eslint-rules/ui-boundary.js`)
applies to every file in this package. It fails:

- any relative import that resolves outside `web/apps/ui`, such as
  `../../web/src/App` or `../../../packages/contracts/src/index`;
- any `@print-partner/*` package that is not on its allowlist, including
  `@print-partner/contracts`, `@print-partner/domain`, `@print-partner/server`
  and `@print-partner/web`.

`test/boundary.test.ts` plants each of those imports and expects the rule to
fail it. Do not add `paths` or Vite aliases that point outside this package.

## Choices

- **Router:** React Router 7 (`react-router`), the library `apps/web` already
  uses. The Builds slice compared it with TanStack Router on paper and kept it:
  nested routes cover the Build tabs, the read-only screens have no search
  parameters that need typing, and TanStack Router is not in the lockfile, so
  it would add a runtime dependency for no current need.
- **Server state:** TanStack Query 5 is the one cache. Routes and core calls
  must not keep a second copy.
- Both were already in `web/package-lock.json`, so this package added no new
  runtime library.

## Generated Rust client

`@print-partner/rust-client` is the one workspace package on the boundary
allowlist. `readCore(name, params)` in `src/core/rust.ts` is the typed read: it
fills the generated route's path through `fetchCore`, so the same-origin rule
holds, and parses the body with the route's generated schema. A body that fails
the schema is a `ContractError`, shown as an error, never a partly typed value.
`CoreError.status` keeps the HTTP status.

## Printer status

`src/status/PrinterStatus.tsx` renders a printer state as an icon plus a word.
The states, labels, Lucide icon names and colour tokens come from
`printerDisplayDescriptors` in the generated client, which Rust's
`pp_domain::printer_display_state` produces. Do not add a state, label or
colour here; change the Rust enum and regenerate.

- The icon map is keyed by the generated icon names, so a new Rust icon fails
  `typecheck` until it has a component.
- `printerStatusPresentation` throws on an empty label instead of rendering an
  icon-only or blank status.
- Each descriptor names a `--status-*` token defined in `src/theme/tokens.css`
  for every theme (see `docs/product/ui-tokens.md`). Cyan
  is reserved for focus, selection and the primary button, so
  `PrinterStatus.test.tsx` fails if a status token is a cyan hue.
