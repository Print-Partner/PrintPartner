# @print-partner/ui

The replacement Print Partner UI. It talks only to the Rust core. The design it
builds toward is [UI and UX target](../../../docs/product/ui-ux-target.md);
[ADR 0008](../../../docs/adr/0008-replace-the-old-screens.md) says why it is a new
package instead of a port of `apps/web`.

Today it is a placeholder shell: an app frame, a core reachability line, a type
specimen at `/type`, and a Not found route. Product screens arrive in their own slices.

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

## Talking to the Rust core

- The dev server listens on `127.0.0.1:5174` and forwards `/api`, `/ws` and
  `/health` to `PP_UI_CORE_URL`, for example
  `PP_UI_CORE_URL=http://127.0.0.1:8080`. The value must be an http(s) origin
  with no path. There is no default: without it the dev server forwards nothing,
  so it cannot reach the Node server by accident. `core-proxy.ts` holds this
  logic and `test/core-proxy.test.ts` tests it.
- UI code calls the core through `fetchCore` in `src/core/client.ts`. It accepts
  same-origin paths only and sends same-origin credentials. In the desktop app
  and Docker the Rust core serves this UI, so the paths reach it directly.

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
- `src/typography.css` defines `--font-sans` and `--font-mono` and the `.num`
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
  uses. TanStack Router is compared against it in the Builds slice.
- **Server state:** TanStack Query 5 is the one cache. Routes and core calls
  must not keep a second copy.
- Both were already in `web/package-lock.json`, so this package added no new
  runtime library.

## Generated Rust client

The generated client package does not exist yet. When it lands:

1. Add its package name to `ALLOWED_WORKSPACE_PACKAGES` in
   `web/eslint-rules/ui-boundary.js` and to this package's dependencies.
2. Build typed calls on the generated client instead of on `fetchCore`. Keep
   `fetchCore`'s same-origin rule, or move it into the generated client's
   transport.
